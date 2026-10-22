//! Relay hub: publishes the share links of iRoof.top apps at
//! `https://<name>.<domain>` (e.g. `alice.iroof.top`), and optionally runs a
//! shared Telegram bot for them ([`crate::relay_tg`]).
//!
//! * An app claims a name with a random secret it keeps (`POST /relay/v1/register`).
//!   The hub stores only the SHA-256 of the secret, which is the account id.
//! * The app keeps a WebSocket open (`/relay/v1/connect`). Visitor requests to
//!   its subdomain are forwarded over it ([`crate::relay_proto`]).
//! * Only visitor paths are forwarded (`/s/…`, `/api/s/…` and the static
//!   assets the visitor page loads). Cookies and credentials never cross, and
//!   `Set-Cookie` from an app is dropped, so one subdomain can't affect another
//!   or the main site.
//! * The hub needs a wildcard DNS record and certificate for `*.<domain>`,
//!   with the reverse proxy passing the `Host` header through.

use crate::relay_proto::{self, Frame};
use crate::share::Limiter;
use anyhow::{Context, Result};
use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

/// Requests one app may have in flight.
const MAX_IN_FLIGHT: usize = 32;
/// Connected apps at once.
const MAX_CONNECTIONS: usize = 2000;
/// How long the hub waits for the app to start answering (the AI may be slow).
const HEAD_TIMEOUT: Duration = Duration::from_secs(300);
/// Longest pause between two chunks of a response body.
const CHUNK_TIMEOUT: Duration = Duration::from_secs(60);
/// The app is considered gone after this long without any message (pings included).
const IDLE_TIMEOUT: Duration = Duration::from_secs(75);
const PING_EVERY: Duration = Duration::from_secs(25);
/// Accounts not seen for this long lose their name.
const ACCOUNT_TTL_SECS: i64 = 180 * 86_400;

#[derive(Debug, Clone)]
pub struct HubConfig {
    /// Apex domain, e.g. `iroof.top`. Apps get `<name>.<domain>`.
    pub domain: String,
    pub data_dir: PathBuf,
    /// Client IPs from `X-Forwarded-For` (behind a reverse proxy).
    pub trust_proxy: bool,
    pub max_accounts: usize,
    /// Shared Telegram bot for the apps (`ROOFTOP_RELAY_BOT_TOKEN`).
    pub bot: Option<crate::relay_tg::SharedBotConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Account {
    name: String,
    created_at: i64,
    last_seen: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    /// Account id (hex SHA-256 of the secret) → account.
    #[serde(default)]
    accounts: BTreeMap<String, Account>,
}

/// One connected app.
struct Conn {
    serial: u64,
    tx: mpsc::Sender<Message>,
    pending: std::sync::Mutex<HashMap<u64, mpsc::Sender<Part>>>,
    next_id: AtomicU64,
    slots: Arc<Semaphore>,
    closed: CancellationToken,
}

enum Part {
    Head(u16, Vec<(String, String)>),
    Data(Bytes),
    End,
    Fail(String),
}

pub struct Hub {
    pub cfg: HubConfig,
    path: PathBuf,
    reg: std::sync::Mutex<Registry>,
    /// name → account id.
    names: std::sync::Mutex<HashMap<String, String>>,
    conns: std::sync::Mutex<HashMap<String, Arc<Conn>>>,
    limiter: std::sync::Mutex<Limiter>,
    serial: AtomicU64,
    pub tg: Option<Arc<crate::relay_tg::SharedBot>>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Account id of a secret, `None` if the secret isn't well-formed.
pub fn account_id(secret: &str) -> Option<String> {
    let ok = (32..=128).contains(&secret.len()) && secret.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    ok.then(|| hex::encode(Sha256::digest(secret.as_bytes())))
}

fn bearer(h: &HeaderMap) -> Option<&str> {
    h.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ").map(str::trim)
}

fn error(status: StatusCode, msg: &str) -> Response {
    (status, axum::Json(json!({"error": msg}))).into_response()
}

fn page(status: StatusCode, title: &str, text: &str) -> Response {
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{title} · iRoof.top</title><style>body{{font:16px/1.5 system-ui,sans-serif;display:grid;place-items:center;min-height:100vh;margin:0;\
         background:#0f1115;color:#e8e8ea}}main{{max-width:28rem;padding:2rem;text-align:center}}h1{{font-size:1.4rem}}p{{color:#a8abb3}}a{{color:#7aa7ff}}</style>\
         </head><body><main><h1>{title}</h1><p>{text}</p></main></body></html>"
    );
    let mut r = (status, [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], html).into_response();
    if status == StatusCode::SERVICE_UNAVAILABLE {
        r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("30"));
    }
    r
}

impl Hub {
    pub fn new(cfg: HubConfig) -> Result<Arc<Self>> {
        let domain = cfg.domain.trim().trim_matches('.').to_ascii_lowercase();
        anyhow::ensure!(domain.contains('.'), "relay domain `{domain}` must be a domain name like iroof.top");
        let cfg = HubConfig { domain, ..cfg };
        std::fs::create_dir_all(&cfg.data_dir).with_context(|| format!("creating {}", cfg.data_dir.display()))?;
        let path = cfg.data_dir.join("relay.json");
        let mut reg: Registry = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b).with_context(|| format!("parsing {}", path.display()))?,
            Err(_) => Registry::default(),
        };
        let cutoff = now() - ACCOUNT_TTL_SECS;
        reg.accounts.retain(|_, a| a.last_seen >= cutoff);
        let names = reg.accounts.iter().map(|(id, a)| (a.name.clone(), id.clone())).collect();
        let tg = cfg.bot.clone().map(|b| crate::relay_tg::SharedBot::new(b, &cfg.data_dir));
        let hub = Arc::new(Self {
            cfg,
            path,
            reg: std::sync::Mutex::new(reg),
            names: std::sync::Mutex::new(names),
            conns: Default::default(),
            limiter: Default::default(),
            serial: AtomicU64::new(1),
            tg,
        });
        hub.save();
        Ok(hub)
    }

    /// Starts background work (the shared bot's polling).
    pub fn start(self: &Arc<Self>) {
        if let Some(tg) = &self.tg {
            crate::relay_tg::start(self.clone(), tg.clone());
        }
    }

    fn save(&self) {
        let bytes = serde_json::to_vec_pretty(&*self.reg.lock().expect("relay reg"));
        let res = bytes.map_err(anyhow::Error::from).and_then(|b| {
            let tmp = self.path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
            std::fs::write(&tmp, b)?;
            std::fs::rename(&tmp, &self.path)?;
            Ok(())
        });
        if let Err(e) = res {
            tracing::warn!("saving relay.json: {e:#}");
        }
    }

    pub fn url_of(&self, name: &str) -> String {
        format!("https://{name}.{}", self.cfg.domain)
    }

    /// Whether the account exists. Returns its name.
    pub fn account_name(&self, id: &str) -> Option<String> {
        self.reg.lock().expect("relay reg").accounts.get(id).map(|a| a.name.clone())
    }

    fn touch(&self, id: &str) {
        let changed = {
            let mut reg = self.reg.lock().expect("relay reg");
            match reg.accounts.get_mut(id) {
                // Saved at most hourly per account: last_seen only drives the 180-day expiry.
                Some(a) if now() - a.last_seen > 3600 => {
                    a.last_seen = now();
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.save();
        }
    }

    fn hit(&self, key: &str, max: u32, window: Duration) -> Result<(), Box<Response>> {
        self.limiter.lock().expect("relay limiter").hit(key, max, window).map_err(|wait| {
            let mut r = error(StatusCode::TOO_MANY_REQUESTS, "Too many requests, try again later.");
            if let Ok(v) = HeaderValue::from_str(&wait.as_secs().max(1).to_string()) {
                r.headers_mut().insert(header::RETRY_AFTER, v);
            }
            Box::new(r)
        })
    }

    /// Claims `name` for the account of `secret` (creating the account), or renames it.
    fn register(&self, secret: &str, name: &str, ip: &str) -> Result<String, Box<Response>> {
        let id = account_id(secret).ok_or_else(|| Box::new(error(StatusCode::UNAUTHORIZED, "invalid account secret")))?;
        relay_proto::validate_name(name).map_err(|e| Box::new(error(StatusCode::BAD_REQUEST, &e.to_string())))?;
        self.hit(&format!("register:{ip}"), 30, Duration::from_secs(3600))?;
        let mut reg = self.reg.lock().expect("relay reg");
        let mut names = self.names.lock().expect("relay names");
        if names.get(name).is_some_and(|owner| *owner != id) {
            return Err(Box::new(error(StatusCode::CONFLICT, "This name is taken, pick another one.")));
        }
        match reg.accounts.get_mut(&id) {
            Some(a) => {
                if a.name != name {
                    names.remove(&a.name);
                    a.name = name.to_string();
                }
                a.last_seen = now();
            }
            None => {
                if reg.accounts.len() >= self.cfg.max_accounts {
                    return Err(Box::new(error(StatusCode::SERVICE_UNAVAILABLE, "The relay is full, try again later.")));
                }
                drop((reg, names));
                self.hit(&format!("new-account:{ip}"), 10, Duration::from_secs(86_400))?;
                reg = self.reg.lock().expect("relay reg");
                names = self.names.lock().expect("relay names");
                if names.contains_key(name) {
                    return Err(Box::new(error(StatusCode::CONFLICT, "This name is taken, pick another one.")));
                }
                reg.accounts.insert(id.clone(), Account { name: name.to_string(), created_at: now(), last_seen: now() });
                tracing::info!(name, "relay: new account");
            }
        }
        names.insert(name.to_string(), id.clone());
        drop((reg, names));
        self.save();
        // A connected app keeps its WebSocket: the next visitor request uses the new name.
        Ok(self.url_of(name))
    }

    fn host(&self, req: &Request) -> Option<String> {
        let raw = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).or_else(|| req.uri().host())?;
        let host = raw.rsplit_once(':').filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit())).map_or(raw, |(h, _)| h);
        Some(host.trim_end_matches('.').to_ascii_lowercase())
    }

    fn client_ip(&self, req: &Request) -> String {
        crate::server::client_ip(req, self.cfg.trust_proxy)
    }
}

/// Middleware in front of the whole app: every `<name>.<domain>` request, and
/// the hub's API (`/relay/v1/…`) on other host names. Everything else passes through.
pub async fn middleware(State(hub): State<Arc<Hub>>, req: Request, next: Next) -> Response {
    let host = hub.host(&req).unwrap_or_default();
    match host.strip_suffix(&hub.cfg.domain).and_then(|s| s.strip_suffix('.')) {
        Some(label) if !label.contains('.') && !label.is_empty() => proxy(&hub, label.to_string(), req).await,
        // The hub's API answers on the apex (or any other name of this server), never on app subdomains.
        _ if req.uri().path().starts_with("/relay/v1/") => api(hub, req).await,
        _ => next.run(req).await,
    }
}

#[derive(Deserialize)]
struct RegisterReq {
    name: String,
}

async fn api(hub: Arc<Hub>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    let ip = hub.client_ip(&req);
    if path == relay_proto::INFO_PATH {
        let bot = hub.tg.as_ref().and_then(|t| t.username());
        return axum::Json(json!({"domain": hub.cfg.domain, "bot": bot, "version": env!("CARGO_PKG_VERSION")})).into_response();
    }
    if path == relay_proto::REGISTER_PATH && req.method() == axum::http::Method::POST {
        let Some(secret) = bearer(req.headers()).map(str::to_string) else {
            return error(StatusCode::UNAUTHORIZED, "missing account secret");
        };
        let body = match axum::body::to_bytes(req.into_body(), 4096).await {
            Ok(b) => b,
            Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE, "request too large"),
        };
        let Ok(r) = serde_json::from_slice::<RegisterReq>(&body) else { return error(StatusCode::BAD_REQUEST, "expected {\"name\": …}") };
        let name = r.name.trim().to_ascii_lowercase();
        return match hub.register(&secret, &name, &ip) {
            Ok(url) => axum::Json(json!({"name": name, "url": url})).into_response(),
            Err(r) => *r,
        };
    }
    if path == relay_proto::CONNECT_PATH {
        if let Err(r) = hub.hit(&format!("connect:{ip}"), 240, Duration::from_secs(3600)) {
            return *r;
        }
        let Some(id) = bearer(req.headers()).and_then(account_id) else {
            return error(StatusCode::UNAUTHORIZED, "missing account secret");
        };
        let Some(name) = hub.account_name(&id) else {
            return error(StatusCode::UNAUTHORIZED, "unknown account: register a name first");
        };
        if hub.conns.lock().expect("relay conns").len() >= MAX_CONNECTIONS {
            return error(StatusCode::SERVICE_UNAVAILABLE, "The relay is full, try again later.");
        }
        let (mut parts, _) = req.into_parts();
        let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
            Ok(ws) => ws,
            Err(e) => return e.into_response(),
        };
        hub.touch(&id);
        return ws.max_message_size(relay_proto::MAX_MESSAGE).on_upgrade(move |socket| serve_app(hub, id, name, socket));
    }
    if let Some(rest) = path.strip_prefix(relay_proto::TG_PATH) {
        let Some(tg) = hub.tg.clone() else {
            let body = json!({"ok": false, "error_code": 501, "description": "Not Implemented: this relay has no shared bot"});
            return (StatusCode::NOT_IMPLEMENTED, axum::Json(body)).into_response();
        };
        let rest = rest.trim_start_matches('/').to_string();
        return crate::relay_tg::handle(&hub, &tg, &rest, req).await;
    }
    error(StatusCode::NOT_FOUND, "not found")
}

/// Runs one app connection until it closes, is replaced or goes idle.
async fn serve_app(hub: Arc<Hub>, id: String, name: String, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<Message>(64);
    let conn = Arc::new(Conn {
        serial: hub.serial.fetch_add(1, Ordering::Relaxed),
        tx,
        pending: Default::default(),
        next_id: AtomicU64::new(1),
        slots: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
        closed: CancellationToken::new(),
    });
    // A reconnecting app (new network, wake from sleep) replaces its old connection.
    if let Some(old) = hub.conns.lock().expect("relay conns").insert(id.clone(), conn.clone()) {
        old.closed.cancel();
    }
    tracing::info!(name, "relay: app connected");
    let hello = Frame::Hello { name: name.clone(), url: hub.url_of(&name) };
    let _ = conn.tx.send(Message::Binary(relay_proto::encode(&hello, &[]).into())).await;

    let closed = conn.closed.clone();
    let writer = tokio::spawn(async move {
        let mut ping = tokio::time::interval(PING_EVERY);
        loop {
            tokio::select! {
                m = rx.recv() => match m {
                    Some(m) => if sink.send(m).await.is_err() { break },
                    None => break,
                },
                _ = ping.tick() => if sink.send(Message::Ping(Bytes::new())).await.is_err() { break },
                _ = closed.cancelled() => break,
            }
        }
        let _ = sink.send(Message::Close(None)).await;
    });
    loop {
        let next = tokio::select! {
            n = tokio::time::timeout(IDLE_TIMEOUT, stream.next()) => n,
            _ = conn.closed.cancelled() => break,
        };
        let msg = match next {
            Ok(Some(Ok(m))) => m,
            _ => break,
        };
        let Message::Binary(bytes) = msg else { continue };
        let Ok((frame, payload)) = relay_proto::decode(&bytes) else {
            tracing::debug!(name, "relay: bad frame");
            continue;
        };
        let (id, part) = match frame {
            Frame::Head { id, status, headers } => (id, Part::Head(status, headers)),
            Frame::Data { id } => (id, Part::Data(Bytes::copy_from_slice(payload))),
            Frame::End { id } => (id, Part::End),
            Frame::Fail { id, message } => (id, Part::Fail(message)),
            _ => continue,
        };
        let last = matches!(part, Part::End | Part::Fail(_));
        let target = {
            let mut p = conn.pending.lock().expect("relay pending");
            if last { p.remove(&id) } else { p.get(&id).cloned() }
        };
        let Some(target) = target else { continue };
        // Back-pressure from a slow visitor: wait a little, then give up on that request.
        if tokio::time::timeout(Duration::from_secs(30), target.send(part)).await.is_err() {
            conn.pending.lock().expect("relay pending").remove(&id);
            let _ = conn.tx.try_send(Message::Binary(relay_proto::encode(&Frame::Cancel { id }, &[]).into()));
        }
    }
    conn.closed.cancel();
    writer.abort();
    conn.pending.lock().expect("relay pending").clear();
    {
        let mut conns = hub.conns.lock().expect("relay conns");
        if conns.get(&id).is_some_and(|c| c.serial == conn.serial) {
            conns.remove(&id);
        }
    }
    tracing::info!(name, "relay: app disconnected");
}

/// Paths of the visitor page and its API: nothing else is forwarded.
fn visitor_path(path: &str) -> bool {
    path.starts_with("/s/") || path.starts_with("/api/s/") || matches!(path, "/app.js" | "/app.css" | "/favicon.svg" | "/favicon.ico")
}

/// Cancels the app's work when the visitor goes away before the response ends.
struct CancelGuard {
    conn: Arc<Conn>,
    id: u64,
    done: bool,
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if !self.done && self.conn.pending.lock().expect("relay pending").remove(&self.id).is_some() {
            let _ = self.conn.tx.try_send(Message::Binary(relay_proto::encode(&Frame::Cancel { id: self.id }, &[]).into()));
        }
    }
}

async fn proxy(hub: &Arc<Hub>, name: String, req: Request) -> Response {
    let path = req.uri().path().to_string();
    if path == "/" {
        return (StatusCode::FOUND, [(header::LOCATION, format!("https://{}/", hub.cfg.domain))]).into_response();
    }
    let account = hub.names.lock().expect("relay names").get(&name).cloned();
    let Some(account) = account else {
        return page(StatusCode::NOT_FOUND, "Nothing here", "This address isn't used by any iRoof.top app.");
    };
    if !visitor_path(&path) {
        return page(StatusCode::NOT_FOUND, "Not found", "Only share links are available at this address.");
    }
    let ip = hub.client_ip(&req);
    if let Err(r) = hub.hit(&format!("visit:{ip}"), 1200, Duration::from_secs(600)) {
        return *r;
    }
    let conn = hub.conns.lock().expect("relay conns").get(&account).cloned();
    let Some(conn) = conn else {
        return page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Offline right now",
            "The iRoof.top app behind this link isn't running at the moment. Try again later.",
        );
    };
    let Ok(permit) = conn.slots.clone().try_acquire_owned() else {
        return page(StatusCode::SERVICE_UNAVAILABLE, "Busy", "Too many visitors at once. Try again in a moment.");
    };
    let method = req.method().to_string();
    let uri = req.uri().path_and_query().map_or(path.clone(), |p| p.to_string());
    let mut headers = relay_proto::header_pairs(req.headers());
    headers.retain(|(k, _)| !k.starts_with("x-forwarded-") && k != "x-real-ip" && k != "forwarded" && k != "host");
    let host = format!("{name}.{}", hub.cfg.domain);
    headers.extend([
        ("host".to_string(), host.clone()),
        ("x-forwarded-for".to_string(), ip),
        ("x-forwarded-proto".to_string(), "https".to_string()),
        ("x-forwarded-host".to_string(), host),
    ]);
    let body = match axum::body::to_bytes(req.into_body(), relay_proto::MAX_REQUEST_BODY).await {
        Ok(b) => b,
        Err(_) => return error(StatusCode::PAYLOAD_TOO_LARGE, "request too large"),
    };
    let id = conn.next_id.fetch_add(1, Ordering::Relaxed);
    let (ptx, mut prx) = mpsc::channel::<Part>(32);
    conn.pending.lock().expect("relay pending").insert(id, ptx);
    let mut guard = CancelGuard { conn: conn.clone(), id, done: false };
    let frame = relay_proto::encode(&Frame::Request { id, method, uri, headers }, &body);
    if conn.tx.send(Message::Binary(frame.into())).await.is_err() {
        guard.done = true;
        return page(StatusCode::BAD_GATEWAY, "Connection lost", "The app went offline. Try again.");
    }
    let (status, headers) = match tokio::time::timeout(HEAD_TIMEOUT, prx.recv()).await {
        Ok(Some(Part::Head(s, h))) => (s, h),
        Ok(Some(Part::Fail(m))) => {
            guard.done = true;
            tracing::debug!(name, "relay: app failed a request: {m}");
            return page(StatusCode::BAD_GATEWAY, "Something went wrong", "The app couldn't answer this request.");
        }
        Ok(_) => return page(StatusCode::BAD_GATEWAY, "Connection lost", "The app went offline. Try again."),
        Err(_) => return page(StatusCode::GATEWAY_TIMEOUT, "No answer", "The app took too long to answer."),
    };
    let mut resp = Response::new(Body::from_stream(body_stream(prx, guard, permit)));
    *resp.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    let h = resp.headers_mut();
    for (k, v) in headers {
        if relay_proto::is_dropped_header(&k) {
            continue;
        }
        if let (Ok(k), Ok(v)) = (HeaderName::try_from(k), HeaderValue::try_from(v)) {
            h.append(k, v);
        }
    }
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    resp
}

fn body_stream(
    rx: mpsc::Receiver<Part>,
    guard: CancelGuard,
    permit: OwnedSemaphorePermit,
) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> + Send {
    futures::stream::unfold(Some((rx, guard, permit)), |state| async move {
        let (mut rx, mut guard, permit) = state?;
        match tokio::time::timeout(CHUNK_TIMEOUT, rx.recv()).await {
            Ok(Some(Part::Data(b))) => Some((Ok(b), Some((rx, guard, permit)))),
            Ok(Some(Part::End)) => {
                guard.done = true;
                None
            }
            Ok(Some(Part::Fail(m))) => {
                guard.done = true;
                Some((Err(std::io::Error::other(m)), None))
            }
            Ok(Some(Part::Head(..))) => Some((Err(std::io::Error::other("unexpected response head")), None)),
            Ok(None) => Some((Err(std::io::Error::other("the app went offline")), None)),
            Err(_) => Some((Err(std::io::Error::other("the app stopped sending")), None)),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_ids_need_long_random_secrets() {
        assert!(account_id("short").is_none());
        assert!(account_id(&"a".repeat(32)).is_some());
        assert!(account_id(&format!("{}!", "a".repeat(40))).is_none());
        assert_eq!(account_id(&"a".repeat(40)).unwrap().len(), 64);
    }

    #[test]
    fn only_visitor_paths_are_forwarded() {
        for ok in ["/s/joe", "/s/joe/tok", "/api/s/joe/sessions", "/app.js", "/favicon.ico"] {
            assert!(visitor_path(ok), "{ok}");
        }
        for bad in ["/api/state", "/api/settings", "/media/x/y", "/healthz", "/relay/v1/connect", "/sx"] {
            assert!(!visitor_path(bad), "{bad}");
        }
    }
}
