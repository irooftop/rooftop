//! Shared Telegram bot of a relay hub: one bot (e.g. @iRoofTopBot) serves the
//! Telegram relay of many iRoof.top apps that don't have a bot of their own.
//!
//! * The hub alone polls Telegram (a bot token allows one poller). Each update
//!   goes to the app whose group it comes from.
//! * Apps talk to a Bot API-compatible proxy: `<hub>/relay/v1/tg/bot<secret>/<method>`.
//!   So the app's [`crate::tgbot`] works unchanged, with the account secret in
//!   place of a bot token. The proxy only allows the methods the relay needs,
//!   and only for the app's own groups (and its own button presses).
//! * A group is linked to an app with a one-time code: the app shows
//!   `https://t.me/<bot>?startgroup=<code>`, Telegram adds the bot to the chosen
//!   group and posts `/start <code>` there.
//! * Messages pass through the hub in plain text (Telegram bots have no
//!   end-to-end encryption). Whoever runs the hub could read them.

use crate::relay_hub::{Hub, account_id};
use crate::share::Limiter;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

const CODE_TTL: Duration = Duration::from_secs(3600);
const QUEUE_MAX: usize = 1000;
const MAX_POLL: u64 = 50;

#[derive(Debug, Clone)]
pub struct SharedBotConfig {
    pub token: String,
    pub api_base: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Links {
    /// Telegram group id → account id.
    #[serde(default)]
    chats: BTreeMap<i64, String>,
}

struct Queue {
    next: i64,
    items: VecDeque<(i64, Value)>,
    notify: Arc<Notify>,
}

impl Queue {
    fn new() -> Self {
        // Ids keep growing across hub restarts, so an app's offset never skips new updates.
        Self { next: chrono::Utc::now().timestamp_millis(), items: VecDeque::new(), notify: Arc::new(Notify::new()) }
    }
}

pub struct SharedBot {
    cfg: SharedBotConfig,
    http: reqwest::Client,
    path: PathBuf,
    links: std::sync::Mutex<Links>,
    me: std::sync::Mutex<Option<Value>>,
    queues: std::sync::Mutex<HashMap<String, Queue>>,
    /// One-time group link codes → (account, created).
    codes: std::sync::Mutex<HashMap<String, (String, Instant)>>,
    /// Button presses → (account, received): only that app may answer them.
    callbacks: std::sync::Mutex<HashMap<String, (String, Instant)>>,
    limiter: std::sync::Mutex<Limiter>,
}

fn tg_error(code: u16, description: &str) -> Response {
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST);
    (status, axum::Json(json!({"ok": false, "error_code": code, "description": description}))).into_response()
}

fn ok(result: Value) -> Response {
    axum::Json(json!({"ok": true, "result": result})).into_response()
}

/// `chat_id` as a number (Telegram also accepts it as a string).
fn chat_of(body: &Value) -> Option<i64> {
    match &body["chat_id"] {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// The chat an update belongs to.
fn update_chat(u: &Value) -> Option<&Value> {
    ["message", "edited_message", "my_chat_member"]
        .iter()
        .find_map(|k| u.get(*k).map(|m| &m["chat"]))
        .or_else(|| u.get("callback_query").map(|c| &c["message"]["chat"]))
        .filter(|c| c.is_object())
}

impl SharedBot {
    pub fn new(cfg: SharedBotConfig, data_dir: &std::path::Path) -> Arc<Self> {
        crate::llm::ensure_crypto_provider();
        let path = data_dir.join("relay-telegram.json");
        let links = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Arc::new(Self {
            cfg,
            http: reqwest::Client::builder().connect_timeout(Duration::from_secs(20)).build().expect("http client"),
            path,
            links: std::sync::Mutex::new(links),
            me: Default::default(),
            queues: Default::default(),
            codes: Default::default(),
            callbacks: Default::default(),
            limiter: Default::default(),
        })
    }

    pub fn username(&self) -> Option<String> {
        self.me.lock().expect("tg me").as_ref().and_then(|m| m["username"].as_str().map(String::from))
    }

    fn bot_id(&self) -> Option<i64> {
        self.me.lock().expect("tg me").as_ref().and_then(|m| m["id"].as_i64())
    }

    fn save(&self) {
        let bytes = serde_json::to_vec_pretty(&*self.links.lock().expect("tg links"));
        let res = bytes.map_err(anyhow::Error::from).and_then(|b| {
            let tmp = self.path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
            std::fs::write(&tmp, b)?;
            std::fs::rename(&tmp, &self.path)?;
            Ok(())
        });
        if let Err(e) = res {
            tracing::warn!("saving relay-telegram.json: {e:#}");
        }
    }

    fn owner(&self, chat: i64) -> Option<String> {
        self.links.lock().expect("tg links").chats.get(&chat).cloned()
    }

    /// Calls the real Bot API and returns its JSON reply (`ok` true or false).
    async fn raw(&self, method: &str, body: &Value) -> anyhow::Result<Value> {
        let url = format!("{}/bot{}/{method}", self.cfg.api_base.trim_end_matches('/'), self.cfg.token);
        let timeout = if method == "getUpdates" { 70 } else { 30 };
        let resp = self
            .http
            .post(&url)
            .json(body)
            .timeout(Duration::from_secs(timeout))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("telegram {method}: {}", e.without_url()))?;
        resp.json().await.map_err(|e| anyhow::anyhow!("telegram {method}: {}", e.without_url()))
    }

    async fn say(&self, chat: i64, text: &str) {
        let body = json!({"chat_id": chat, "text": text, "parse_mode": "HTML", "link_preview_options": {"is_disabled": true}});
        if let Err(e) = self.raw("sendMessage", &body).await {
            tracing::debug!("{e:#}");
        }
    }

    fn push(&self, account: &str, mut update: Value) {
        let mut queues = self.queues.lock().expect("tg queues");
        let q = queues.entry(account.to_string()).or_insert_with(Queue::new);
        let id = q.next;
        q.next += 1;
        update["update_id"] = json!(id);
        q.items.push_back((id, update));
        while q.items.len() > QUEUE_MAX {
            q.items.pop_front();
        }
        q.notify.notify_waiters();
    }

    /// A fresh one-time code that links a group to `account`.
    fn new_code(&self, account: &str) -> String {
        use rand_like::token;
        let mut codes = self.codes.lock().expect("tg codes");
        codes.retain(|_, (_, t)| t.elapsed() < CODE_TTL);
        let code = token(16);
        codes.insert(code.clone(), (account.to_string(), Instant::now()));
        code
    }

    fn link(&self, chat: &Value, code: &str) -> Option<(String, Option<String>)> {
        let account = {
            let mut codes = self.codes.lock().expect("tg codes");
            match codes.remove(code) {
                Some((a, t)) if t.elapsed() < CODE_TTL => a,
                _ => return None,
            }
        };
        let id = chat["id"].as_i64()?;
        let previous = self.links.lock().expect("tg links").chats.insert(id, account.clone()).filter(|p| *p != account);
        self.save();
        Some((account, previous))
    }

    fn member_update(&self, chat: &Value, status: &str) -> Value {
        let me = self.me.lock().expect("tg me").clone().unwrap_or_default();
        json!({"my_chat_member": {"chat": chat, "date": chrono::Utc::now().timestamp(), "new_chat_member": {"status": status, "user": me}}})
    }

    /// Routes one update from Telegram.
    async fn route(&self, hub: &Hub, u: Value) {
        let Some(chat) = update_chat(&u).cloned() else { return };
        let Some(chat_id) = chat["id"].as_i64() else { return };
        let group = matches!(chat["type"].as_str(), Some("group" | "supergroup"));
        if let Some(m) = u.get("message") {
            let text = m["text"].as_str().unwrap_or_default().trim();
            let mut words = text.split_whitespace();
            let first = words.next().unwrap_or_default();
            let (cmd, mention) = first.split_once('@').map_or((first, None), |(c, b)| (c, Some(b)));
            let for_me = mention.is_none_or(|b| self.username().is_some_and(|u| u.eq_ignore_ascii_case(b)));
            if cmd == "/start"
                && for_me
                && let Some(code) = words.next()
            {
                if !group {
                    self.say(chat_id, "Add me to a group from iRoof.top: Settings → Telegram → <b>Add to a group</b>.").await;
                    return;
                }
                match self.link(&chat, code) {
                    Some((account, previous)) => {
                        if let Some(prev) = previous {
                            self.push(&prev, self.member_update(&chat, "left"));
                        }
                        let name = hub.account_name(&account).unwrap_or_default();
                        tracing::info!(name, chat_id, "relay bot: group linked");
                        // The app registers the group and greets it, as when its own bot is added.
                        self.push(&account, self.member_update(&chat, "member"));
                    }
                    None => {
                        self.say(chat_id, "⚠️ This invite expired or was already used. In iRoof.top press <b>Add to a group</b> again.")
                            .await;
                    }
                }
                return;
            }
            if let Some(new_id) = m["migrate_to_chat_id"].as_i64() {
                let moved = self.links.lock().expect("tg links").chats.remove(&chat_id);
                if let Some(a) = &moved {
                    self.links.lock().expect("tg links").chats.insert(new_id, a.clone());
                    self.save();
                }
            }
        }
        let owner = self.owner(chat_id);
        if let Some(cq) = u.get("callback_query")
            && let (Some(id), Some(owner)) = (cq["id"].as_str(), owner.as_ref())
        {
            let mut cbs = self.callbacks.lock().expect("tg callbacks");
            cbs.retain(|_, (_, t)| t.elapsed() < Duration::from_secs(900));
            cbs.insert(id.to_string(), (owner.clone(), Instant::now()));
        }
        let left = u["my_chat_member"]["new_chat_member"]["status"].as_str().is_some_and(|s| matches!(s, "left" | "kicked"));
        match owner {
            Some(owner) => {
                self.push(&owner, u);
                if left {
                    self.links.lock().expect("tg links").chats.remove(&chat_id);
                    self.save();
                }
            }
            None => {
                let joined =
                    u["my_chat_member"]["new_chat_member"]["status"].as_str().is_some_and(|s| matches!(s, "member" | "administrator"));
                let asked = u["message"]["text"].as_str().is_some_and(|t| {
                    let c = t.split_whitespace().next().unwrap_or_default().split('@').next().unwrap_or_default();
                    matches!(c, "/start" | "/help")
                });
                if (group && joined) || asked {
                    let text = "👋 I relay iRoof.top share links. To connect a group, open iRoof.top → Settings → Telegram → <b>Shared bot</b> → <b>Add to a group</b> and pick the group there.";
                    self.say(chat_id, text).await;
                }
            }
        }
    }
}

/// Polls the shared bot and routes updates to the apps.
pub fn start(hub: Arc<Hub>, tg: Arc<SharedBot>) {
    tokio::spawn(async move {
        loop {
            match tg.raw("getMe", &json!({})).await {
                Ok(v) if v["ok"] == json!(true) => {
                    tracing::info!("relay: shared Telegram bot @{} connected", v["result"]["username"].as_str().unwrap_or("?"));
                    *tg.me.lock().expect("tg me") = Some(v["result"].clone());
                    break;
                }
                Ok(v) => tracing::warn!("relay bot getMe: {}", v["description"].as_str().unwrap_or("failed")),
                Err(e) => tracing::warn!("relay bot: {e:#}"),
            }
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
        let mut offset: i64 = 0;
        loop {
            let body = json!({"offset": offset, "timeout": 30, "allowed_updates": ["message", "callback_query", "my_chat_member"]});
            match tg.raw("getUpdates", &body).await {
                Ok(v) if v["ok"] == json!(true) => {
                    for u in v["result"].as_array().cloned().unwrap_or_default() {
                        offset = offset.max(u["update_id"].as_i64().unwrap_or(0) + 1);
                        tg.route(&hub, u).await;
                    }
                }
                Ok(v) => {
                    tracing::warn!("relay bot getUpdates: {}", v["description"].as_str().unwrap_or("failed"));
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
                Err(e) => {
                    tracing::warn!("relay bot: {e:#}");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    });
}

/// `POST <TG_PATH>/bot<secret>/<method>`: the Bot API, limited to the app's groups.
pub async fn handle(hub: &Hub, tg: &SharedBot, rest: &str, req: Request) -> Response {
    let Some((token, method)) = rest.split_once('/') else { return tg_error(404, "Not Found") };
    let Some(account) = token.strip_prefix("bot").and_then(account_id).filter(|a| hub.account_name(a).is_some()) else {
        return tg_error(401, "Unauthorized");
    };
    let body: Value = match axum::body::to_bytes(req.into_body(), 64 * 1024).await {
        Ok(b) if b.is_empty() => json!({}),
        Ok(b) => match serde_json::from_slice(&b) {
            Ok(v) => v,
            Err(_) => return tg_error(400, "Bad Request: invalid JSON"),
        },
        Err(_) => return tg_error(413, "Request Entity Too Large"),
    };
    if !matches!(method, "getUpdates" | "getMe")
        && tg.limiter.lock().expect("tg limiter").hit(&account, 120, Duration::from_secs(60)).is_err()
    {
        return tg_error(429, "Too Many Requests: retry after 30");
    }
    let Some(me) = tg.me.lock().expect("tg me").clone() else { return tg_error(503, "Service Unavailable: the shared bot is starting") };
    match method {
        "getMe" => ok(me),
        "getUpdates" => ok(poll(tg, &account, &body).await),
        "iroofLink" => {
            let code = tg.new_code(&account);
            let username = tg.username().unwrap_or_default();
            ok(json!({"username": username, "code": code, "url": format!("https://t.me/{username}?startgroup={code}")}))
        }
        "sendMessage" | "editMessageReplyMarkup" | "editMessageText" | "getChat" | "getChatMember" | "setMessageReaction" => {
            let Some(chat) = chat_of(&body) else { return tg_error(400, "Bad Request: chat_id is required") };
            if tg.owner(chat).as_deref() != Some(account.as_str()) {
                return tg_error(403, "Forbidden: this group isn't linked to your app");
            }
            if method == "getChatMember" && body["user_id"].as_i64() != tg.bot_id() {
                return tg_error(403, "Forbidden: only the bot's own membership can be read");
            }
            forward(tg, method, &body).await
        }
        "answerCallbackQuery" => {
            let id = body["callback_query_id"].as_str().unwrap_or_default();
            let mine = tg.callbacks.lock().expect("tg callbacks").get(id).is_some_and(|(a, _)| *a == account);
            if !mine {
                return tg_error(403, "Forbidden: unknown callback query");
            }
            forward(tg, method, &body).await
        }
        _ => tg_error(403, "Forbidden: this method isn't available through the shared bot"),
    }
}

async fn forward(tg: &SharedBot, method: &str, body: &Value) -> Response {
    match tg.raw(method, body).await {
        Ok(v) => axum::Json(v).into_response(),
        Err(e) => tg_error(502, &format!("Bad Gateway: {e}")),
    }
}

/// Long poll of an app's queue, like `getUpdates` (`offset`, `timeout`, `limit`).
async fn poll(tg: &SharedBot, account: &str, body: &Value) -> Value {
    let offset = body["offset"].as_i64().unwrap_or(0);
    let limit = body["limit"].as_u64().unwrap_or(100).clamp(1, 100) as usize;
    let wait = Duration::from_secs(body["timeout"].as_u64().unwrap_or(0).min(MAX_POLL));
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let notify = {
            let mut queues = tg.queues.lock().expect("tg queues");
            let q = queues.entry(account.to_string()).or_insert_with(Queue::new);
            while q.items.front().is_some_and(|(id, _)| *id < offset) {
                q.items.pop_front();
            }
            if !q.items.is_empty() {
                return Value::Array(q.items.iter().take(limit).map(|(_, u)| u.clone()).collect());
            }
            q.notify.clone()
        };
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        // Re-check: an update may have arrived between the lock and `enable`.
        if tg.queues.lock().expect("tg queues").get(account).is_some_and(|q| q.items.iter().any(|(id, _)| *id >= offset)) {
            continue;
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            return json!([]);
        }
    }
}

/// Random URL-safe tokens from the OS random source.
pub(crate) mod rand_like {
    pub fn token(len: usize) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        let mut out = String::with_capacity(len);
        let mut buf = [0u8; 64];
        while out.len() < len {
            getrandom::fill(&mut buf).expect("OS random source");
            // 62 * 4 = 248: rejecting 248..=255 keeps the choice uniform.
            for b in buf.iter().copied().filter(|b| *b < 248) {
                if out.len() < len {
                    out.push(ALPHABET[(b % 62) as usize] as char);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_ids_and_update_chats() {
        assert_eq!(chat_of(&json!({"chat_id": -100123})), Some(-100123));
        assert_eq!(chat_of(&json!({"chat_id": "-5"})), Some(-5));
        assert_eq!(chat_of(&json!({})), None);
        let cq = json!({"callback_query": {"id": "1", "message": {"chat": {"id": -7}}}});
        assert_eq!(update_chat(&cq).unwrap()["id"], -7);
        assert!(update_chat(&json!({"poll": {}})).is_none());
    }

    #[test]
    fn tokens_are_url_safe() {
        let t = rand_like::token(40);
        assert_eq!(t.len(), 40);
        assert!(t.bytes().all(|b| b.is_ascii_alphanumeric()));
        assert_ne!(rand_like::token(16), rand_like::token(16));
    }
}
