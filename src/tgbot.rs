//! Telegram relay for share links: the owner watches visitor conversations in
//! a Telegram group and decides who answers.
//!
//! * The bot token comes from the configuration (`TELEGRAM_BOT_TOKEN`). Updates
//!   are fetched by long polling, so no public URL or webhook is needed.
//! * A share link can point at a group the bot is a member of
//!   ([`crate::share::Share::telegram_chat_id`]). Every visitor message is posted
//!   there with two inline buttons: **✍️ Reply** and **🤖 AI reply (10 s)**.
//!   Nobody clicks → the persona (LLM) answers when the countdown ends. **AI
//!   reply** answers right away. **Reply** stops the countdown and asks for a
//!   reply (ForceReply). The owner's text then reaches the visitor as a
//!   message of the persona, indistinguishable from an AI answer.
//! * Bots in groups run in privacy mode by default: they still receive button
//!   presses, replies to their own messages, commands and membership changes,
//!   which is all this relay needs. Any reply to a relayed message (or to an
//!   AI answer) is delivered to that visitor.
//! * Only members of the selected group can act: callbacks and replies are
//!   accepted only from the group the share is linked to.

use crate::chat::Event;
use crate::model::Message;
use crate::share::{GuestSession, Share};
use crate::state::{AppState, now_string};
use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Default countdown before the AI answers on its own.
pub const DEFAULT_AI_DELAY_SECS: u32 = 10;
/// Relayed messages remembered for routing replies.
const MAX_THREADS: usize = 5000;
/// After **Reply**, the presser's plain (non-reply) messages go to that visitor for this long…
const ANSWER_WINDOW: Duration = Duration::from_secs(15 * 60);
/// …and after each answer for this long, so an answer can span several messages.
const FOLLOW_UP: Duration = Duration::from_secs(2 * 60);
/// Telegram's message length limit is 4096 characters.
const MAX_TEXT: usize = 3500;

#[derive(Debug, Clone)]
pub struct BotConfig {
    pub token: String,
    /// Bot API base URL, `https://api.telegram.org` (or a local Bot API server).
    pub api_base: String,
    /// Groups the relay may use. Empty: every group the bot is added to.
    pub allowed_chats: Vec<i64>,
}

/// A relayed Telegram message and the visitor conversation it belongs to.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Thread {
    tg_chat: i64,
    tg_msg: i64,
    share_id: String,
    chat_id: String,
    /// The visitor message this Telegram message shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    msg_id: Option<i64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Persisted {
    /// Groups the bot is a member of: id → title.
    #[serde(default)]
    groups: BTreeMap<i64, String>,
    #[serde(default)]
    threads: Vec<Thread>,
}

/// A visitor message waiting for an answer.
struct Pending {
    seq: u64,
    share_id: String,
    tg_chat: i64,
    /// Message carrying the buttons, if it was delivered.
    tg_msg: Option<i64>,
    /// ForceReply prompt sent after **Reply**.
    prompt_msg: Option<i64>,
    /// The owner pressed **Reply**: no countdown until the next visitor message.
    human: bool,
    timer: Option<tokio::task::AbortHandle>,
    guest: String,
}

/// Whose plain messages in a group are answers to a visitor (see [`ANSWER_WINDOW`]).
struct Answering {
    chat_id: String,
    share_id: String,
    until: std::time::Instant,
}

pub struct Bot {
    cfg: BotConfig,
    http: reqwest::Client,
    path: PathBuf,
    data: std::sync::Mutex<Persisted>,
    pending: std::sync::Mutex<HashMap<String, Pending>>,
    /// Chats whose AI answer is being generated.
    busy: std::sync::Mutex<HashSet<String>>,
    seq: AtomicU64,
    me: std::sync::Mutex<Option<(i64, String)>>,
    /// Privacy mode is off: the bot receives every group message, not only replies to it.
    reads_all: std::sync::atomic::AtomicBool,
    /// (group, user) → the visitor their next plain message answers.
    answering: std::sync::Mutex<HashMap<(i64, i64), Answering>>,
    /// Serializes relaying of visitor messages (keeps their order in the group).
    relay_lock: tokio::sync::Mutex<()>,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() > max { s.chars().take(max).collect::<String>() + "…" } else { s.to_string() }
}

/// Text of a message for the group: its text, or a media label plus the
/// transcript / description when there is one.
fn describe(m: &Message) -> String {
    let base = m.preview(MAX_TEXT);
    match m.media.as_ref().and_then(|md| md.description.as_deref()).filter(|d| !d.trim().is_empty()) {
        Some(d) if m.text.trim().is_empty() => format!("{base} {}", clip(d.trim(), MAX_TEXT)),
        _ => base,
    }
}

impl Bot {
    pub fn new(cfg: BotConfig, data_dir: &std::path::Path) -> Arc<Self> {
        crate::llm::ensure_crypto_provider();
        let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(20)).build().expect("http client");
        let path = data_dir.join("telegram.json");
        let data = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Persisted>(&b).ok()).unwrap_or_default();
        Arc::new(Self {
            cfg,
            http,
            path,
            data: std::sync::Mutex::new(data),
            pending: Default::default(),
            busy: Default::default(),
            seq: AtomicU64::new(1),
            me: Default::default(),
            reads_all: Default::default(),
            answering: Default::default(),
            relay_lock: Default::default(),
        })
    }

    fn save(&self) {
        let bytes = {
            let d = self.data.lock().expect("tg data");
            serde_json::to_vec(&*d)
        };
        let res = bytes.map_err(anyhow::Error::from).and_then(|b| {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = self.path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
            std::fs::write(&tmp, b)?;
            std::fs::rename(&tmp, &self.path)?;
            Ok(())
        });
        if let Err(e) = res {
            tracing::warn!("saving telegram.json: {e:#}");
        }
    }

    /// Calls a Bot API method. Errors never contain the URL (it holds the token).
    async fn call(&self, method: &str, body: Value) -> Result<Value> {
        let url = format!("{}/bot{}/{method}", self.cfg.api_base.trim_end_matches('/'), self.cfg.token);
        let timeout = if method == "getUpdates" { 70 } else { 30 };
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .timeout(Duration::from_secs(timeout))
            .send()
            .await
            .map_err(|e| anyhow!("telegram {method}: {}", e.without_url()))?;
        let v: Value = resp.json().await.map_err(|e| anyhow!("telegram {method}: {}", e.without_url()))?;
        if v["ok"] != json!(true) {
            bail!("telegram {method}: {}", v["description"].as_str().unwrap_or("request failed"));
        }
        Ok(v["result"].clone())
    }

    async fn send(&self, chat: i64, text: &str, reply_to: Option<i64>, markup: Option<Value>) -> Result<i64> {
        let mut body = json!({"chat_id": chat, "text": text, "parse_mode": "HTML", "link_preview_options": {"is_disabled": true}});
        if let Some(r) = reply_to {
            body["reply_parameters"] = json!({"message_id": r, "allow_sending_without_reply": true});
        }
        if let Some(m) = markup {
            body["reply_markup"] = m;
        }
        let r = self.call("sendMessage", body).await?;
        r["message_id"].as_i64().ok_or_else(|| anyhow!("telegram sendMessage: no message_id"))
    }

    async fn set_keyboard(&self, chat: i64, msg: i64, keyboard: Value) {
        let body = json!({"chat_id": chat, "message_id": msg, "reply_markup": keyboard});
        if let Err(e) = self.call("editMessageReplyMarkup", body).await
            && !e.to_string().contains("not modified")
        {
            tracing::debug!("{e:#}");
        }
    }

    async fn strip_keyboard(&self, chat: i64, msg: i64) {
        self.set_keyboard(chat, msg, json!({"inline_keyboard": []})).await;
    }

    async fn answer_callback(&self, id: &str, text: &str) {
        if let Err(e) = self.call("answerCallbackQuery", json!({"callback_query_id": id, "text": text})).await {
            tracing::debug!("{e:#}");
        }
    }

    pub fn username(&self) -> Option<String> {
        self.me.lock().expect("tg me").as_ref().map(|(_, u)| u.clone())
    }

    fn bot_id(&self) -> Option<i64> {
        self.me.lock().expect("tg me").as_ref().map(|(id, _)| *id)
    }

    fn allowed(&self, chat: i64) -> bool {
        self.cfg.allowed_chats.is_empty() || self.cfg.allowed_chats.contains(&chat)
    }

    /// Groups that can be picked for a share link.
    pub fn groups(&self) -> Vec<(i64, String)> {
        let d = self.data.lock().expect("tg data");
        d.groups.iter().filter(|(id, _)| self.allowed(**id)).map(|(id, t)| (*id, t.clone())).collect()
    }

    pub fn has_group(&self, chat: i64) -> bool {
        self.allowed(chat) && self.data.lock().expect("tg data").groups.contains_key(&chat)
    }

    /// Remembers a group. Returns whether it is new.
    fn register(&self, chat: i64, title: &str) -> bool {
        if !self.allowed(chat) {
            return false;
        }
        let changed = {
            let mut d = self.data.lock().expect("tg data");
            d.groups.insert(chat, title.to_string()).as_deref() != Some(title)
        };
        if changed {
            self.save();
        }
        changed
    }

    fn unregister(&self, chat: i64) {
        if self.data.lock().expect("tg data").groups.remove(&chat).is_some() {
            self.save();
        }
    }

    fn remember(&self, t: Thread) {
        {
            let mut d = self.data.lock().expect("tg data");
            d.threads.push(t);
            let n = d.threads.len();
            if n > MAX_THREADS {
                d.threads.drain(..n - MAX_THREADS);
            }
        }
        self.save();
    }

    fn thread(&self, tg_chat: i64, tg_msg: i64) -> Option<Thread> {
        self.data.lock().expect("tg data").threads.iter().rev().find(|t| t.tg_chat == tg_chat && t.tg_msg == tg_msg).cloned()
    }

    /// From now on, plain messages of `user` in `tg_chat` answer the visitor of `chat_id`.
    fn start_answering(&self, tg_chat: i64, user: i64, chat_id: &str, share_id: &str, window: Duration) {
        let a = Answering { chat_id: chat_id.to_string(), share_id: share_id.to_string(), until: std::time::Instant::now() + window };
        let mut all = self.answering.lock().expect("tg answering");
        all.retain(|_, a| a.until > std::time::Instant::now());
        all.insert((tg_chat, user), a);
    }

    fn answering_for(&self, tg_chat: i64, user: i64) -> Option<(String, String)> {
        let all = self.answering.lock().expect("tg answering");
        all.get(&(tg_chat, user)).filter(|a| a.until > std::time::Instant::now()).map(|a| (a.chat_id.clone(), a.share_id.clone()))
    }

    /// The AI took over this visitor: plain messages no longer go there.
    fn stop_answering(&self, chat_id: &str) {
        self.answering.lock().expect("tg answering").retain(|_, a| a.chat_id != chat_id);
    }

    /// Whether the bot receives plain (non-reply) messages in `tg_chat`:
    /// privacy mode is off, or the bot is an admin there.
    async fn sees_all(&self, tg_chat: i64) -> bool {
        if self.reads_all.load(Ordering::Relaxed) {
            return true;
        }
        let Some(me) = self.bot_id() else { return false };
        match self.call("getChatMember", json!({"chat_id": tg_chat, "user_id": me})).await {
            Ok(m) => matches!(m["status"].as_str(), Some("administrator" | "creator")),
            Err(_) => false,
        }
    }

    /// Whether the visitor of `chat_id` should see "typing…": an answer is
    /// being generated, the owner is writing, or the countdown runs.
    pub fn is_typing(&self, chat_id: &str) -> bool {
        self.busy.lock().expect("tg busy").contains(chat_id)
            || self.pending.lock().expect("tg pending").get(chat_id).is_some_and(|p| p.human || p.timer.is_some())
    }

    /// Removes the pending answer of `chat_id` (only if it is `seq`, when given).
    fn take_pending(&self, chat_id: &str, seq: Option<u64>) -> Option<Pending> {
        let mut p = self.pending.lock().expect("tg pending");
        if seq.is_some_and(|s| p.get(chat_id).is_none_or(|x| x.seq != s)) {
            return None;
        }
        p.remove(chat_id)
    }
}

fn keyboard(chat_id: &str, seq: u64, countdown: Option<u32>) -> Value {
    let ai = match countdown {
        Some(s) if s > 0 => format!("🤖 AI reply ({s} s)"),
        _ => "🤖 AI reply".to_string(),
    };
    json!({"inline_keyboard": [[
        {"text": "✍️ Reply", "callback_data": format!("r:{chat_id}:{seq}")},
        {"text": ai, "callback_data": format!("a:{chat_id}:{seq}")},
    ]]})
}

fn bot(st: &AppState) -> Option<&Arc<Bot>> {
    st.telegram.get()
}

/// Whether visitor messages of `share` go to Telegram (instead of the AI answering directly).
pub fn relay_active(st: &AppState, share: &Share) -> bool {
    share.telegram_chat_id.is_some() && bot(st).is_some()
}

pub fn is_typing(st: &AppState, chat_id: &str) -> bool {
    bot(st).is_some_and(|b| b.is_typing(chat_id))
}

fn guest_name(session: &GuestSession) -> String {
    session.name.clone().unwrap_or_else(|| format!("Guest {}", &session.id[..4.min(session.id.len())]))
}

/// Posts a new visitor message to the share's group with the answer buttons
/// and starts the AI countdown. Errors are logged: the visitor's message is
/// already stored, and the AI still answers if Telegram is unreachable.
pub async fn relay_guest_message(st: &Arc<AppState>, share: &Share, session: &GuestSession, msg_id: i64) {
    let (Some(bot), Some(tg_chat)) = (bot(st).cloned(), share.telegram_chat_id) else { return };
    let _order = bot.relay_lock.lock().await;
    let chat_id = session.chat_id.clone();
    let Ok((rec, chat)) = st.snapshot(&chat_id).await else { return };
    let Some(m) = crate::chat::find_message(&rec.export, &chat.events, msg_id).cloned() else { return };
    let guest = guest_name(session);
    let mut text = format!("💬 <b>{}</b> <i>· /s/{}</i>\n", esc(&guest), esc(&share.slug));
    if let Some(r) = m.reply_to.and_then(|r| crate::chat::find_message(&rec.export, &chat.events, r)) {
        text.push_str(&format!("<i>↩️ {}</i>\n", esc(&r.preview(80))));
    }
    text.push_str(&esc(&describe(&m)));

    let seq = bot.seq.fetch_add(1, Ordering::Relaxed);
    let previous = bot.take_pending(&chat_id, None);
    let human = previous.as_ref().is_some_and(|p| p.human);
    if let Some(p) = &previous {
        if let Some(t) = &p.timer {
            t.abort();
        }
        if let Some(msg) = p.tg_msg {
            bot.strip_keyboard(p.tg_chat, msg).await;
        }
    }
    let delay = share.ai_delay_secs;
    let countdown = (!human).then_some(delay);
    let tg_msg = match bot.send(tg_chat, &text, None, Some(keyboard(&chat_id, seq, countdown))).await {
        Ok(id) => {
            bot.remember(Thread { tg_chat, tg_msg: id, share_id: share.id.clone(), chat_id: chat_id.clone(), msg_id: Some(msg_id) });
            Some(id)
        }
        Err(e) => {
            tracing::warn!(slug = %share.slug, "relaying a visitor message failed: {e:#}");
            None
        }
    };
    // Nobody can press a button that wasn't delivered: the AI answers anyway.
    let delay = if tg_msg.is_none() { delay.max(DEFAULT_AI_DELAY_SECS) } else { delay };
    let timer = (!human && delay > 0).then(|| {
        let st = st.clone();
        let chat_id = chat_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(delay.into())).await;
            if let Some(p) = bot_take(&st, &chat_id, Some(seq)) {
                ai_answer(st, chat_id, p).await;
            }
        })
        .abort_handle()
    });
    let pending = Pending {
        seq,
        share_id: share.id.clone(),
        tg_chat,
        tg_msg,
        prompt_msg: previous.as_ref().and_then(|p| p.prompt_msg).filter(|_| human),
        human,
        timer,
        guest,
    };
    bot.pending.lock().expect("tg pending").insert(chat_id, pending);
}

fn bot_take(st: &AppState, chat_id: &str, seq: Option<u64>) -> Option<Pending> {
    bot(st)?.take_pending(chat_id, seq)
}

/// Runs the persona for a taken pending answer and posts what it said.
async fn ai_answer(st: Arc<AppState>, chat_id: String, p: Pending) {
    let Some(bot) = bot(&st).cloned() else { return };
    bot.stop_answering(&chat_id);
    if let Some(msg) = p.tg_msg {
        bot.strip_keyboard(p.tg_chat, msg).await;
    }
    if let Some(msg) = p.prompt_msg {
        bot.strip_keyboard(p.tg_chat, msg).await;
    }
    let share = st.db.read().await.shares.get(&p.share_id).cloned();
    let Some(share) = share.filter(|s| s.is_active()) else { return };
    // Same accounting as the web flow: AI answers count against the limit.
    {
        let mut db = st.db.write().await;
        let Some(s) = db.shares.get_mut(&p.share_id) else { return };
        let used: u32 = s.sessions.iter().filter(|g| g.chat_id == chat_id).map(|g| g.replies).sum();
        if s.max_replies > 0 && used >= s.max_replies {
            drop(db);
            let _ =
                bot.send(p.tg_chat, "⛔ This link reached its limit of AI answers. Reply yourself to keep talking.", p.tg_msg, None).await;
            return;
        }
        if let Some(g) = s.sessions.iter_mut().filter(|g| g.chat_id == chat_id).max_by(|a, b| a.last_seen.cmp(&b.last_seen)) {
            g.replies += 1;
        }
        if let Err(e) = st.paths.save_share(s) {
            tracing::warn!("saving share: {e:#}");
        }
    }
    bot.busy.lock().expect("tg busy").insert(chat_id.clone());
    let outcome = crate::agent::respond(&st, &chat_id).await;
    bot.busy.lock().expect("tg busy").remove(&chat_id);
    let (events, error) = match outcome {
        Ok(o) => (o.events, o.error),
        Err(e) => (Vec::new(), Some(format!("{e:#}"))),
    };
    let mut lines = Vec::new();
    for e in &events {
        match e {
            Event::Message(m) => lines.push(esc(&describe(m))),
            Event::Reaction { emoji, removed: false, .. } => lines.push(format!("<i>reacted {}</i>", esc(emoji))),
            Event::Reaction { .. } => {}
        }
    }
    let text = match (lines.is_empty(), error) {
        (_, Some(err)) if lines.is_empty() => format!("⚠️ The AI couldn't answer {}: {}", esc(&p.guest), esc(&clip(&err, 300))),
        (true, None) => format!("🤖 The AI chose not to answer {}.", esc(&p.guest)),
        _ => format!("🤖 <b>AI</b> → {}:\n{}", esc(&p.guest), clip(&lines.join("\n"), MAX_TEXT)),
    };
    match bot.send(p.tg_chat, &text, p.tg_msg, None).await {
        Ok(id) => bot.remember(Thread { tg_chat: p.tg_chat, tg_msg: id, share_id: share.id.clone(), chat_id, msg_id: None }),
        Err(e) => tracing::warn!("posting the AI answer failed: {e:#}"),
    }
}

/// Starts the bot: resolves its identity and polls for updates in the background.
pub fn start(st: &Arc<AppState>, cfg: BotConfig) -> Result<()> {
    if cfg.token.trim().is_empty() {
        bail!("empty Telegram bot token");
    }
    let bot = Bot::new(cfg, &st.paths.root);
    st.telegram.set(bot).map_err(|_| anyhow!("the Telegram bot is already running"))?;
    let st = st.clone();
    tokio::spawn(async move { run(st).await });
    Ok(())
}

async fn run(st: Arc<AppState>) {
    let Some(bot) = bot(&st).cloned() else { return };
    loop {
        match bot.call("getMe", json!({})).await {
            Ok(me) => {
                let id = me["id"].as_i64().unwrap_or_default();
                let name = me["username"].as_str().unwrap_or_default().to_string();
                tracing::info!("Telegram bot @{name} connected");
                // Privacy mode off: an answer can also be a plain message after pressing Reply.
                bot.reads_all.store(me["can_read_all_group_messages"] == json!(true), Ordering::Relaxed);
                *bot.me.lock().expect("tg me") = Some((id, name));
                break;
            }
            Err(e) => {
                tracing::warn!("Telegram bot: {e:#}, retrying in 15 s");
                tokio::time::sleep(Duration::from_secs(15)).await;
            }
        }
    }
    // Configured groups get their title (and show up before anyone writes there).
    for chat in bot.cfg.allowed_chats.clone() {
        match bot.call("getChat", json!({"chat_id": chat})).await {
            Ok(c) => {
                bot.register(chat, c["title"].as_str().unwrap_or("Telegram group"));
            }
            Err(e) => tracing::warn!("Telegram group {chat}: {e:#}"),
        }
    }
    let mut offset: i64 = 0;
    loop {
        let body = json!({"offset": offset, "timeout": 30, "allowed_updates": ["message", "callback_query", "my_chat_member"]});
        match bot.call("getUpdates", body).await {
            Ok(Value::Array(updates)) => {
                for u in updates {
                    offset = offset.max(u["update_id"].as_i64().unwrap_or(0) + 1);
                    if let Err(e) = handle_update(&st, &bot, &u).await {
                        tracing::warn!("Telegram update: {e:#}");
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                let conflict = e.to_string().contains("Conflict");
                tracing::warn!(
                    "{e:#}{}",
                    if conflict { " (another process polls this bot, or a webhook is set: remove it to use the relay)" } else { "" }
                );
                tokio::time::sleep(Duration::from_secs(if conflict { 30 } else { 5 })).await;
            }
        }
    }
}

async fn handle_update(st: &Arc<AppState>, bot: &Arc<Bot>, u: &Value) -> Result<()> {
    if let Some(cq) = u.get("callback_query") {
        return on_callback(st, bot, cq).await;
    }
    if let Some(m) = u.get("my_chat_member") {
        let chat = &m["chat"];
        let id = chat["id"].as_i64().unwrap_or_default();
        if !matches!(chat["type"].as_str(), Some("group" | "supergroup")) {
            return Ok(());
        }
        match m["new_chat_member"]["status"].as_str() {
            Some("member" | "administrator") => {
                let title = chat["title"].as_str().unwrap_or("Telegram group");
                if bot.register(id, title) {
                    let text = format!(
                        "👋 Ready. In iRoof.top open a chat → <b>Share</b> → pick <b>{}</b> under Telegram. Visitor messages will appear here with <b>Reply</b> and <b>AI reply</b> buttons.",
                        esc(title)
                    );
                    let _ = bot.send(id, &text, None, None).await;
                } else if !bot.allowed(id) {
                    tracing::info!("Telegram group {id} is not in TELEGRAM_CHAT_ID, ignoring it");
                }
            }
            Some("left" | "kicked") => bot.unregister(id),
            _ => {}
        }
        return Ok(());
    }
    if let Some(m) = u.get("message") {
        return on_message(st, bot, m).await;
    }
    Ok(())
}

/// The share of a visitor conversation, if it is linked to `tg_chat`.
async fn linked_share(st: &AppState, chat_id: &str, tg_chat: i64) -> Option<Share> {
    let db = st.db.read().await;
    let share_id = db.chats.get(chat_id)?.guest.as_ref()?.share_id.clone();
    db.shares.get(&share_id).filter(|s| s.telegram_chat_id == Some(tg_chat)).cloned()
}

async fn on_callback(st: &Arc<AppState>, bot: &Arc<Bot>, cq: &Value) -> Result<()> {
    let id = cq["id"].as_str().unwrap_or_default();
    let tg_chat = cq["message"]["chat"]["id"].as_i64().unwrap_or_default();
    let tg_msg = cq["message"]["message_id"].as_i64();
    let data = cq["data"].as_str().unwrap_or_default();
    let mut parts = data.splitn(3, ':');
    let (Some(kind), Some(chat_id), Some(seq)) = (parts.next(), parts.next(), parts.next().and_then(|s| s.parse::<u64>().ok())) else {
        bot.answer_callback(id, "Unknown button.").await;
        return Ok(());
    };
    let Some(share) = linked_share(st, chat_id, tg_chat).await else {
        bot.answer_callback(id, "This conversation is no longer linked to this group.").await;
        if let Some(m) = tg_msg {
            bot.strip_keyboard(tg_chat, m).await;
        }
        return Ok(());
    };
    let stale = || async {
        bot.answer_callback(id, "Already answered.").await;
        if let Some(m) = tg_msg {
            bot.strip_keyboard(tg_chat, m).await;
        }
    };
    match kind {
        "a" => {
            let Some(mut p) = bot.take_pending(chat_id, Some(seq)) else {
                stale().await;
                return Ok(());
            };
            if let Some(t) = p.timer.take() {
                t.abort();
            }
            bot.answer_callback(id, "🤖 The AI is answering…").await;
            tokio::spawn(ai_answer(st.clone(), chat_id.to_string(), p));
        }
        "r" => {
            let taken = {
                let mut all = bot.pending.lock().expect("tg pending");
                match all.get_mut(chat_id) {
                    Some(p) if p.seq == seq => {
                        if let Some(t) = p.timer.take() {
                            t.abort();
                        }
                        p.human = true;
                        Some((p.tg_msg, p.guest.clone()))
                    }
                    _ => None,
                }
            };
            let Some((msg, guest)) = taken else {
                stale().await;
                return Ok(());
            };
            let who = &cq["from"];
            let user = who["id"].as_i64().unwrap_or_default();
            bot.start_answering(tg_chat, user, chat_id, &share.id, ANSWER_WINDOW);
            let plain = bot.sees_all(tg_chat).await;
            bot.answer_callback(id, if plain { "Write your answer." } else { "Write your answer as a reply." }).await;
            if let Some(m) = msg {
                bot.set_keyboard(
                    tg_chat,
                    m,
                    json!({"inline_keyboard": [[{"text": "🤖 Let the AI answer", "callback_data": format!("a:{chat_id}:{seq}")}]]}),
                )
                .await;
            }
            let mention = format!("<a href=\"tg://user?id={user}\">{}</a>", esc(who["first_name"].as_str().unwrap_or("You")));
            let text = if plain {
                format!("✍️ {mention}, write your answer to <b>{}</b>: just send it, or reply to this message.", esc(&guest))
            } else {
                format!("✍️ {mention}, reply to this message with your answer to <b>{}</b>.", esc(&guest))
            };
            let markup = json!({"force_reply": true, "selective": true, "input_field_placeholder": clip(&format!("Answer {guest}"), 60)});
            match bot.send(tg_chat, &text, msg, Some(markup)).await {
                Ok(prompt) => {
                    bot.remember(Thread {
                        tg_chat,
                        tg_msg: prompt,
                        share_id: share.id.clone(),
                        chat_id: chat_id.to_string(),
                        msg_id: None,
                    });
                    if let Some(p) = bot.pending.lock().expect("tg pending").get_mut(chat_id).filter(|p| p.seq == seq) {
                        p.prompt_msg = Some(prompt);
                    }
                }
                Err(e) => tracing::warn!("{e:#}"),
            }
        }
        _ => bot.answer_callback(id, "Unknown button.").await,
    }
    Ok(())
}

async fn on_message(st: &Arc<AppState>, bot: &Arc<Bot>, m: &Value) -> Result<()> {
    let chat = &m["chat"];
    let tg_chat = chat["id"].as_i64().unwrap_or_default();
    let group = matches!(chat["type"].as_str(), Some("group" | "supergroup"));
    if group {
        bot.register(tg_chat, chat["title"].as_str().unwrap_or("Telegram group"));
    }
    // A group upgraded to a supergroup gets a new id: move the links along.
    if let Some(new_id) = m["migrate_to_chat_id"].as_i64() {
        migrate(st, bot, tg_chat, new_id).await;
        return Ok(());
    }
    let text = m["text"].as_str().or_else(|| m["caption"].as_str()).unwrap_or_default().trim().to_string();
    let command = text.split_whitespace().next().unwrap_or_default().split('@').next().unwrap_or_default();
    if matches!(command, "/start" | "/chatid" | "/help") {
        let reply = if group && bot.allowed(tg_chat) {
            format!(
                "This group is ready (id <code>{tg_chat}</code>). In iRoof.top open a chat → <b>Share</b> → pick it under Telegram. Visitor messages then appear here: press <b>Reply</b> and answer, or let the AI answer when the countdown ends."
            )
        } else if group {
            format!("This group (id <code>{tg_chat}</code>) isn't in TELEGRAM_CHAT_ID of the iRoof.top server.")
        } else {
            "Add me to a group, then pick that group when you share a chat in iRoof.top.".to_string()
        };
        let _ = bot.send(tg_chat, &reply, m["message_id"].as_i64(), None).await;
        return Ok(());
    }
    if m["from"]["is_bot"] == json!(true) {
        return Ok(());
    }
    let user = m["from"]["id"].as_i64();
    let replied = &m["reply_to_message"];
    // In forum groups every message "replies" to its topic's first message.
    let topic_root = m["is_topic_message"] == json!(true) && replied["message_id"] == m["message_thread_id"];
    let thread = if replied.is_object() && !topic_root {
        // An explicit reply counts only when it replies to the relay: the visitor
        // message, the ✍️ prompt or an AI answer.
        let (Some(rt_id), Some(bot_id)) = (replied["message_id"].as_i64(), bot.bot_id()) else { return Ok(()) };
        if replied["from"]["id"].as_i64() != Some(bot_id) {
            return Ok(());
        }
        let Some(thread) = bot.thread(tg_chat, rt_id) else { return Ok(()) };
        thread
    } else {
        // A plain message counts only from someone who pressed Reply or just answered
        // (the bot receives these only with privacy mode off or as an admin).
        let Some((chat_id, share_id)) = user.and_then(|u| bot.answering_for(tg_chat, u)) else { return Ok(()) };
        Thread { tg_chat, tg_msg: 0, share_id, chat_id, msg_id: None }
    };
    let Some(share) = linked_share(st, &thread.chat_id, tg_chat).await else {
        let _ = bot.send(tg_chat, "This conversation is no longer linked to this group.", m["message_id"].as_i64(), None).await;
        return Ok(());
    };
    if text.is_empty() {
        let _ = bot.send(tg_chat, "Only text answers reach the visitor.", m["message_id"].as_i64(), None).await;
        return Ok(());
    }
    if text.chars().count() > 4096 {
        return Ok(());
    }
    owner_reply(st, &thread, &text).await?;
    if let Some(u) = user {
        bot.start_answering(tg_chat, u, &thread.chat_id, &share.id, FOLLOW_UP);
    }
    // The owner answered: the countdown and the buttons are no longer needed.
    if let Some(mut p) = bot.take_pending(&thread.chat_id, None) {
        if let Some(t) = p.timer.take() {
            t.abort();
        }
        for msg in [p.tg_msg, p.prompt_msg].into_iter().flatten() {
            bot.strip_keyboard(p.tg_chat, msg).await;
        }
    }
    if let Some(mid) = m["message_id"].as_i64() {
        let body = json!({"chat_id": tg_chat, "message_id": mid, "reaction": [{"type": "emoji", "emoji": "👍"}]});
        if let Err(e) = bot.call("setMessageReaction", body).await {
            tracing::debug!("{e:#}");
        }
    }
    tracing::info!(slug = %share.slug, "owner answered a visitor from Telegram");
    Ok(())
}

/// Appends the owner's Telegram answer as a message of the persona.
async fn owner_reply(st: &Arc<AppState>, thread: &Thread, text: &str) -> Result<()> {
    let (rec, chat) = st.snapshot(&thread.chat_id).await?;
    // Answering an older visitor message shows as a reply to it, answering the latest one doesn't.
    let last_user = chat.events.iter().rev().find_map(|e| match e {
        Event::Message(m) if m.from == chat.user => Some(m.id),
        _ => None,
    });
    let reply_to = thread.msg_id.filter(|id| Some(*id) != last_user && crate::chat::find_message(&rec.export, &chat.events, *id).is_some());
    let id = st.alloc_ids(&thread.chat_id, 1).await?;
    let msg = Message { id, from: chat.persona.clone(), date: Some(now_string()), reply_to, text: text.to_string(), ..Default::default() };
    st.append_events(&thread.chat_id, &[Event::Message(msg)]).await
}

async fn migrate(st: &Arc<AppState>, bot: &Arc<Bot>, old: i64, new: i64) {
    let title = bot.data.lock().expect("tg data").groups.get(&old).cloned();
    bot.unregister(old);
    if let Some(t) = title {
        bot.register(new, &t);
    }
    {
        let mut d = bot.data.lock().expect("tg data");
        for t in d.threads.iter_mut().filter(|t| t.tg_chat == old) {
            t.tg_chat = new;
        }
    }
    bot.save();
    let mut db = st.db.write().await;
    for s in db.shares.values_mut().filter(|s| s.telegram_chat_id == Some(old)) {
        s.telegram_chat_id = Some(new);
        if let Err(e) = st.paths.save_share(s) {
            tracing::warn!("saving share: {e:#}");
        }
    }
    tracing::info!("Telegram group {old} became supergroup {new}");
}

/// `GET /api/telegram` (owner): whether the bot runs and which groups can be picked.
pub async fn status(axum::extract::State(st): axum::extract::State<Arc<AppState>>) -> axum::Json<Value> {
    let Some(bot) = bot(&st) else {
        return axum::Json(json!({"enabled": false, "groups": []}));
    };
    let groups: Vec<Value> = bot.groups().into_iter().map(|(id, title)| json!({"id": id, "title": title})).collect();
    axum::Json(json!({"enabled": true, "username": bot.username(), "groups": groups}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_html_and_clips() {
        assert_eq!(esc("<b>&</b>"), "&lt;b&gt;&amp;&lt;/b&gt;");
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(clip("abc", 3), "abc");
    }

    #[test]
    fn callback_data_fits_telegram_limit() {
        let k = keyboard("0123456789ab", u64::MAX, Some(10));
        for b in k["inline_keyboard"][0].as_array().unwrap() {
            assert!(b["callback_data"].as_str().unwrap().len() <= 64);
        }
        assert_eq!(k["inline_keyboard"][0][1]["text"], "🤖 AI reply (10 s)");
        assert_eq!(keyboard("c", 1, Some(0))["inline_keyboard"][0][1]["text"], "🤖 AI reply");
    }
}
