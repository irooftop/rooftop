//! HTTP API + embedded web UI.

use crate::agent;
use crate::chat::{Chat, Event, Item, timeline};
use crate::import::safe_rel;
use crate::mcp::McpConfig;
use crate::model::{ChatExport, MediaKind, Message};
use crate::prompt::{self, PromptOptions};
use crate::state::{AppState, now_string};
use crate::store::ExportRecord;
use crate::style::infer_reactors;
use crate::tools;
use anyhow::anyhow;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tower::ServiceExt;

type S = State<Arc<AppState>>;

pub struct ApiError(pub StatusCode, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        let msg = format!("{e:#}");
        let code = if msg.contains("not found") { StatusCode::NOT_FOUND } else { StatusCode::INTERNAL_SERVER_ERROR };
        ApiError(code, msg)
    }
}

pub(crate) fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

pub(crate) type ApiResult<T = Json<Value>> = Result<T, ApiError>;

pub struct ServerOptions {
    pub password: Option<String>,
    /// Largest request body the owner routes accept; 0 means no limit (the default:
    /// exports are many GB, and the owner uploads from their own computer).
    pub max_upload_bytes: usize,
    /// Take the client IP for PIN throttling from `X-Forwarded-For` / `X-Real-IP`
    /// (only behind a reverse proxy that sets them).
    pub trust_proxy: bool,
    /// Relay hub ([`crate::relay_hub`]): serves `<name>.<domain>` for iRoof.top apps.
    pub relay_hub: Option<Arc<crate::relay_hub::Hub>>,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self { password: None, max_upload_bytes: 0, trust_proxy: false, relay_hub: None }
    }
}

/// Largest request a visitor can send (voice messages are ≤ 5 MiB).
const GUEST_MAX_BODY: usize = 16 * 1024 * 1024;

/// What visitors need: the share pages and their API, and the static assets.
fn public_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/app.js", get(app_js))
        .route("/app.css", get(app_css))
        .route("/favicon.svg", get(favicon_svg))
        .route("/favicon.ico", get(favicon_ico))
        .merge(crate::share_api::guest_routes())
}

fn with_client_ip(router: Router<Arc<AppState>>, trust_proxy: bool) -> Router<Arc<AppState>> {
    router.layer(middleware::from_fn(move |mut req: Request, next: Next| async move {
        let ip = client_ip(&req, trust_proxy);
        req.extensions_mut().insert(ClientIp(ip));
        next.run(req).await
    }))
}

/// Only the visitor side (share links), for other devices on the local network
/// and for the internet relay: the owner UI and API are not part of it.
pub fn guest_router(state: Arc<AppState>, trust_proxy: bool) -> Router {
    with_client_ip(public_routes(), trust_proxy)
        .route("/", get(guest_root))
        .route("/healthz", get(|| async { "ok" }))
        .fallback(|| async { (StatusCode::NOT_FOUND, "not found") })
        .layer(DefaultBodyLimit::max(GUEST_MAX_BODY))
        .layer(tower_http::compression::CompressionLayer::new())
        .with_state(state)
}

async fn guest_root() -> Response {
    let html = "<!doctype html><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>iRoof.top</title>\
<body style=\"font:16px/1.5 system-ui,sans-serif;display:grid;place-items:center;min-height:90vh;text-align:center\"><main><h1>iRoof.top</h1>\
<p>Open the share link you were sent: it looks like <code>/s/name</code>.</p></main></body>";
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

pub fn router(state: Arc<AppState>, opts: ServerOptions) -> Router {
    let password = opts.password.filter(|p| !p.is_empty()).map(Arc::new);
    let trust_proxy = opts.trust_proxy;
    let app = Router::new()
        .route("/", get(index))
        .route("/api/state", get(api_state))
        .route("/api/exports", post(upload_export))
        .route("/api/imports/{id}", get(get_import))
        .route("/api/exports/{id}", get(get_export).delete(delete_export))
        .route("/api/exports/{id}/describe", post(describe_export))
        .route("/api/exports/{id}/library/{item}", patch(patch_library_item))
        .route("/api/chats", post(create_chat))
        .route("/api/chats/{id}", get(get_chat).patch(update_chat).delete(delete_chat))
        .route("/api/chats/{id}/messages", post(post_message))
        .route("/api/chats/{id}/react", post(post_reaction))
        .route("/api/chats/{id}/voice", post(post_voice))
        .route("/api/chats/{id}/respond", post(respond))
        .route("/api/chats/{id}/regenerate", post(regenerate))
        .route("/api/chats/{id}/prompt", get(get_prompt))
        .route("/api/chats/{id}/shares", get(crate::share_api::list_shares).post(crate::share_api::create_share))
        .route("/api/shares/{id}", patch(crate::share_api::update_share).delete(crate::share_api::delete_share))
        .route("/api/shares/{id}/sessions/{session}", axum::routing::delete(crate::share_api::delete_session))
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/llm/test", post(test_llm))
        .route("/api/mcp", get(mcp_status))
        .route("/api/mcp/reconnect", post(mcp_reconnect))
        .route("/api/telegram", get(crate::tgbot::status))
        .route("/api/telegram/link", post(crate::tgbot::add_link))
        .merge(crate::sharing::owner_routes())
        .route("/media/{export_id}/{*path}", get(media))
        .layer(middleware::from_fn(move |req: Request, next: Next| {
            let password = password.clone();
            async move { auth(password, req, next).await }
        }))
        // Public: static assets and share links (they have their own PIN check).
        .merge(public_routes());
    let app = with_client_ip(app, trust_proxy)
        .route("/healthz", get(|| async { "ok" }))
        .layer(match opts.max_upload_bytes {
            0 => DefaultBodyLimit::disable(),
            n => DefaultBodyLimit::max(n),
        })
        .layer(tower_http::compression::CompressionLayer::new())
        .with_state(state);
    match opts.relay_hub {
        // Outermost: `<name>.<domain>` never reaches the owner's routes.
        Some(hub) => app.layer(middleware::from_fn_with_state(hub, crate::relay_hub::middleware)),
        None => app,
    }
}

/// Optional HTTP Basic auth (any user name, `ROOFTOP_PASSWORD` as password).
async fn auth(password: Option<Arc<String>>, req: Request, next: Next) -> Response {
    let Some(pw) = password else { return next.run(req).await };
    let ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
        .and_then(|b| {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.decode(b.trim()).ok()
        })
        .and_then(|d| String::from_utf8(d).ok())
        .and_then(|s| s.split_once(':').map(|(_, p)| p.to_string()))
        .is_some_and(|given| {
            // Constant-time comparison of digests.
            let a = Sha256::digest(given.as_bytes());
            let b = Sha256::digest(pw.as_bytes());
            a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
        });
    if ok {
        next.run(req).await
    } else {
        let mut r = (StatusCode::UNAUTHORIZED, "authentication required").into_response();
        r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"iRoof.top\""));
        r
    }
}

/// Client address used for PIN throttling.
#[derive(Clone, Debug)]
pub struct ClientIp(pub String);

pub(crate) fn client_ip(req: &Request, trust_proxy: bool) -> String {
    if trust_proxy {
        let h = req.headers();
        let fwd = h
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .or_else(|| h.get("x-real-ip").and_then(|v| v.to_str().ok()))
            .map(str::trim)
            .filter(|v| !v.is_empty());
        if let Some(ip) = fwd {
            return ip.to_string();
        }
    }
    req.extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn asset(body: &'static str, ctype: &'static str) -> Response {
    ([(header::CONTENT_TYPE, ctype), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

/// `index.html` with the Telegram export walkthrough inlined at `<!-- telegram-export.svg -->`.
/// The SVG is also a standalone file (used by the README), so it has a single source.
static INDEX_HTML: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    let svg = include_str!("../web/telegram-export.svg");
    let svg = svg.find("<svg").map_or(svg, |i| &svg[i..]); // drop the XML prolog and comments
    include_str!("../web/index.html").replacen("<!-- telegram-export.svg -->", svg.trim_end(), 1)
});

pub(crate) async fn index() -> Response {
    let mut r = asset(INDEX_HTML.as_str(), "text/html; charset=utf-8");
    let h = r.headers_mut();
    // Share URLs carry a secret session token: never leak it via Referer.
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert("X-Frame-Options", HeaderValue::from_static("DENY"));
    h.insert("X-Content-Type-Options", HeaderValue::from_static("nosniff"));
    r
}
async fn app_js() -> Response {
    asset(include_str!("../web/app.js"), "text/javascript; charset=utf-8")
}
async fn app_css() -> Response {
    asset(include_str!("../web/app.css"), "text/css; charset=utf-8")
}
/// Follows the light / dark color scheme of the browser.
async fn favicon_svg() -> Response {
    asset(include_str!("../web/favicon.svg"), "image/svg+xml")
}
/// 16, 32 and 48 px, for browsers and tools that only read `/favicon.ico`.
async fn favicon_ico() -> Response {
    static ICO: &[u8] = include_bytes!("../web/favicon.ico");
    ([(header::CONTENT_TYPE, "image/x-icon"), (header::CACHE_CONTROL, "no-cache")], ICO).into_response()
}

pub(crate) fn export_summary(rec: &ExportRecord) -> Value {
    let e = &rec.export;
    json!({
        "id": rec.id,
        "title": e.title,
        "imported_at": rec.imported_at,
        "participants": e.participants,
        "exporter": e.exporter(),
        "messages": e.messages.len(),
        "library": e.library.len(),
        "described": e.library.iter().filter(|i| i.description.is_some()).count(),
    })
}

pub(crate) fn chat_summary(c: &Chat, export: Option<&ExportRecord>) -> Value {
    let last = c
        .events
        .iter()
        .rev()
        .find_map(|e| match e {
            Event::Message(m) => Some(m),
            _ => None,
        })
        .or_else(|| export.and_then(|r| r.export.messages.iter().rev().find(|m| !m.service)));
    json!({
        "id": c.id,
        "export_id": c.export_id,
        "title": c.title,
        "persona": c.persona,
        "user": c.user,
        "updated_at": c.updated_at,
        "persona_avatar": export.and_then(|r| r.export.participants.iter().find(|p| p.name == c.persona).and_then(|p| p.avatar.clone())),
        "last": last.map(|m| json!({"from": m.from, "preview": m.preview(80), "date": m.date})),
        "guest": c.guest.as_ref().map(|g| json!({"share_id": g.share_id})),
    })
}

async fn api_state(State(st): S) -> ApiResult {
    let db = st.db.read().await;
    let mut chats: Vec<Value> = db
        .chats
        .values()
        .filter(|c| c.guest.is_none())
        .map(|c| chat_summary(c, db.exports.get(&c.export_id).map(|r| r.as_ref())))
        .collect();
    chats.sort_by(|a, b| b["updated_at"].as_str().cmp(&a["updated_at"].as_str()));
    let exports: Vec<Value> = db.exports.values().map(|r| export_summary(r)).collect();
    let shares: Vec<Value> = db.shares.values().map(crate::share_api::share_summary).collect();
    drop(db);
    let cfg = st.llm_config().await;
    // Whether someone chose the endpoint (Settings or OPENAI_BASE_URL), rather than the built-in default.
    let endpoint_set =
        !st.db.read().await.settings.llm.base_url.trim().is_empty() || std::env::var("OPENAI_BASE_URL").is_ok_and(|v| !v.trim().is_empty());
    Ok(Json(json!({
        "exports": exports,
        "chats": chats,
        "shares": shares,
        "llm": {
            "base_url": cfg.base_url, "model": cfg.model, "has_api_key": !cfg.api_key.is_empty(),
            "host": crate::llm::host_of(&cfg.base_url),
            "endpoint_set": endpoint_set,
            // Nothing can answer until a key is set (local endpoints usually need none).
            "needs_key": cfg.api_key.trim().is_empty() && !crate::llm::is_local(&cfg.base_url),
        },
        "offline": {"on": st.is_offline(), "forced": st.offline_forced()},
        "version": env!("CARGO_PKG_VERSION"),
    })))
}

/// Imports an export directory the caller owns (it is copied, never moved), without progress.
/// Returns (record, newly_created).
pub async fn import_into_store(st: &Arc<AppState>, src: PathBuf) -> anyhow::Result<(Arc<ExportRecord>, bool)> {
    crate::imports::run(st, src, crate::import::Placement::Copy, Arc::new(|_| {})).await
}

/// Streams the uploaded files to a temporary directory, then starts a background
/// import job and answers `202 {"id": …}` at once: progress is `GET /api/imports/{id}`.
async fn upload_export(State(st): S, mut mp: Multipart) -> ApiResult<(StatusCode, Json<Value>)> {
    let tmp = st.paths.tmp().join(uuid::Uuid::new_v4().simple().to_string());
    let received = receive_files(&mut mp, &tmp).await;
    if let Err(e) = received {
        let _ = tokio::fs::remove_dir_all(&tmp).await;
        return Err(e);
    }
    let job = st.imports.create();
    let id = job.id();
    tokio::spawn(crate::imports::run_upload(st.clone(), job, tmp, UNZIP_LIMIT));
    Ok((StatusCode::ACCEPTED, Json(json!({"id": id}))))
}

/// Largest total size an uploaded zip archive may unpack to.
const UNZIP_LIMIT: u64 = 64 * 1024 * 1024 * 1024;

async fn receive_files(mp: &mut Multipart, tmp: &std::path::Path) -> Result<(), ApiError> {
    let io = |e: std::io::Error| ApiError::from(anyhow::Error::from(e));
    let mut count = 0usize;
    while let Some(mut field) = mp.next_field().await.map_err(|e| bad(format!("upload failed: {e}")))? {
        let Some(name) = field.file_name().map(|s| s.replace('\\', "/")) else { continue };
        let rel = safe_rel(&name).ok_or_else(|| bad(format!("invalid file path `{name}`")))?;
        let dest = tmp.join("files").join(&rel);
        if let Some(p) = dest.parent() {
            tokio::fs::create_dir_all(p).await.map_err(io)?;
        }
        let mut f = tokio::io::BufWriter::new(tokio::fs::File::create(&dest).await.map_err(io)?);
        while let Some(chunk) = field.chunk().await.map_err(|e| bad(format!("upload failed: {e}")))? {
            f.write_all(&chunk).await.map_err(io)?;
        }
        f.flush().await.map_err(io)?;
        count += 1;
    }
    if count == 0 {
        return Err(bad("no files uploaded"));
    }
    Ok(())
}

async fn get_import(State(st): S, Path(id): Path<String>) -> ApiResult {
    let snap = st.imports.get(&id).ok_or_else(|| anyhow!("import not found"))?;
    Ok(Json(json!(snap)))
}

async fn get_export(State(st): S, Path(id): Path<String>) -> ApiResult {
    let rec = st.db.read().await.exports.get(&id).cloned().ok_or_else(|| anyhow!("export not found"))?;
    let mut v = export_summary(&rec);
    // Without ffmpeg, GIFs and videos can be described only from a thumbnail.
    v["ffmpeg"] = json!(crate::frames::ffmpeg().is_some());
    let dir = st.paths.export_files(&id);
    v["library_items"] = tokio::task::spawn_blocking(move || {
        let items: Vec<Value> = rec
            .export
            .library
            .iter()
            .map(|i| {
                let mut item = json!(i);
                item["describable"] = json!(crate::describe::can_describe(&dir, i));
                item
            })
            .collect();
        json!(items)
    })
    .await
    .map_err(anyhow::Error::from)?;
    Ok(Json(v))
}

async fn delete_export(State(st): S, Path(id): Path<String>) -> ApiResult {
    let mut db = st.db.write().await;
    db.exports.remove(&id).ok_or_else(|| anyhow!("export not found"))?;
    let chats: Vec<String> = db.chats.values().filter(|c| c.export_id == id).map(|c| c.id.clone()).collect();
    for c in &chats {
        db.chats.remove(c);
        st.paths.delete_chat(c)?;
    }
    let shares: Vec<String> = db.shares.values().filter(|s| chats.contains(&s.chat_id)).map(|s| s.id.clone()).collect();
    for s in &shares {
        db.shares.remove(s);
        st.paths.delete_share(s)?;
    }
    st.paths.delete_export(&id)?;
    Ok(Json(json!({"ok": true, "deleted_chats": chats})))
}

#[derive(Deserialize, Default)]
struct DescribeReq {
    #[serde(default)]
    ids: Vec<String>,
    #[serde(default)]
    force: bool,
}

/// Replaces library descriptions of an export and persists it.
async fn set_descriptions(st: &AppState, id: &str, updates: &[(String, Option<String>)]) -> Result<(), ApiError> {
    // Merge into the current record (it may have changed meanwhile).
    let mut db = st.db.write().await;
    let cur = db.exports.get(id).cloned().ok_or_else(|| anyhow!("export not found"))?;
    let mut new = (*cur).clone();
    for item in new.export.library.iter_mut() {
        if let Some((_, d)) = updates.iter().find(|(i, _)| *i == item.id) {
            item.description = d.clone();
        }
    }
    // Serializing a big export takes a while: keep it off the async workers.
    let (paths, new) = (st.paths.clone(), Arc::new(new));
    let saved = new.clone();
    tokio::task::spawn_blocking(move || paths.save_export(&saved)).await.map_err(anyhow::Error::from)??;
    db.exports.insert(id.to_string(), new);
    Ok(())
}

async fn describe_export(State(st): S, Path(id): Path<String>, body: Option<Json<DescribeReq>>) -> ApiResult {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let rec = st.db.read().await.exports.get(&id).cloned().ok_or_else(|| anyhow!("export not found"))?;
    let cfg = st.media_llm_config().await;
    let sel = crate::describe::Selection { ids: req.ids, force: req.force };
    let (described, failed) = crate::describe::describe_library(&st.llm, &cfg, &st.paths.export_files(&id), &rec.export, &sel).await;
    if !described.is_empty() {
        let updates: Vec<(String, Option<String>)> = described.iter().map(|(i, d)| (i.clone(), Some(d.clone()))).collect();
        set_descriptions(&st, &id, &updates).await?;
    }
    let items: serde_json::Map<String, Value> = described.iter().map(|(i, d)| (i.clone(), json!(d))).collect();
    Ok(Json(json!({"described": described.len(), "items": items, "failed": failed})))
}

#[derive(Deserialize)]
struct DescriptionPatch {
    description: Option<String>,
}

/// Manual edit of one library description (empty clears it).
async fn patch_library_item(State(st): S, Path((id, item)): Path<(String, String)>, Json(p): Json<DescriptionPatch>) -> ApiResult {
    let exists = st.db.read().await.exports.get(&id).is_some_and(|r| r.export.library.iter().any(|i| i.id == item));
    if !exists {
        return Err(bad("unknown library item"));
    }
    let d = p.description.map(|d| d.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|d| !d.is_empty());
    if d.as_ref().is_some_and(|d| d.chars().count() > 1000) {
        return Err(bad("description is longer than 1000 characters"));
    }
    set_descriptions(&st, &id, &[(item.clone(), d.clone())]).await?;
    Ok(Json(json!({"id": item, "description": d})))
}

#[derive(Deserialize)]
struct NewChat {
    export_id: String,
    persona: String,
    user: Option<String>,
    title: Option<String>,
    notes: Option<String>,
}

pub fn make_chat(rec: &ExportRecord, persona: &str, user: Option<&str>, title: Option<&str>, notes: &str) -> Result<Chat, ApiError> {
    let e = &rec.export;
    if !e.participants.iter().any(|p| p.name == persona) {
        return Err(bad(format!("`{persona}` is not a participant of this export")));
    }
    let user = user
        .map(str::to_string)
        .filter(|u| !u.trim().is_empty())
        .or_else(|| e.participants.iter().find(|p| p.name != persona).map(|p| p.name.clone()))
        .unwrap_or_else(|| "Me".into());
    if user == persona {
        return Err(bad("persona and user must be different people"));
    }
    let max_id = e.messages.iter().map(|m| m.id).max().unwrap_or(0);
    let now = now_string();
    Ok(Chat {
        id: uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
        export_id: rec.id.clone(),
        title: title.map(str::trim).filter(|t| !t.is_empty()).map(String::from).unwrap_or_else(|| persona.to_string()),
        persona: persona.to_string(),
        user,
        notes: notes.to_string(),
        voice: String::new(),
        created_at: now.clone(),
        updated_at: now,
        events: Vec::new(),
        next_id: max_id + 1,
        memory: None,
        last_usage: None,
        guest: None,
        generated: Default::default(),
    })
}

async fn create_chat(State(st): S, Json(req): Json<NewChat>) -> ApiResult {
    let rec = st.db.read().await.exports.get(&req.export_id).cloned().ok_or_else(|| anyhow!("export not found"))?;
    let chat = make_chat(&rec, &req.persona, req.user.as_deref(), req.title.as_deref(), req.notes.as_deref().unwrap_or_default())?;
    st.paths.save_chat(&chat)?;
    let summary = chat_summary(&chat, Some(&rec));
    st.db.write().await.chats.insert(chat.id.clone(), chat);
    Ok(Json(summary))
}

/// Messages of the merged export + live timeline in display order.
pub(crate) fn timeline_messages<'a>(export: &'a ChatExport, chat: &'a Chat) -> Vec<&'a Message> {
    timeline(export, &chat.events)
        .into_iter()
        .filter_map(|item| match item {
            Item::Msg(m) => Some(m),
            _ => None,
        })
        .collect()
}

/// Window of the last `limit` messages positioned strictly before message
/// `before` (or the end of the timeline), plus whether older ones exist.
pub(crate) fn page_messages<'a, 'b>(
    all: &'b [&'a Message],
    before: Option<i64>,
    limit: usize,
) -> Result<(&'b [&'a Message], bool), ApiError> {
    let end = match before {
        Some(id) => all.iter().position(|m| m.id == id).ok_or_else(|| bad("unknown message id in `before`"))?,
        None => all.len(),
    };
    let start = end.saturating_sub(limit.clamp(1, MAX_PAGE));
    Ok((&all[start..end], start > 0))
}

const MAX_PAGE: usize = 2000;

/// UI view of the given timeline messages with merged reactions.
pub(crate) fn view_messages(export: &ChatExport, chat: &Chat, msgs: &[&Message]) -> Vec<Value> {
    let names: Vec<String> = export.participants.iter().map(|p| p.name.clone()).collect();
    // message id -> list of (reactor, emoji), one reaction per reactor.
    let mut reactions: HashMap<i64, Vec<(String, String)>> = HashMap::new();
    let mut unknown: HashMap<i64, Vec<(String, u32)>> = HashMap::new();
    let wanted: HashSet<i64> = msgs.iter().map(|m| m.id).collect();
    for m in msgs {
        for r in &m.reactions {
            let who = infer_reactors(m, r.count, &r.from, &names);
            if who.is_empty() {
                unknown.entry(m.id).or_default().push((r.emoji.clone(), r.count));
            }
            for w in who {
                reactions.entry(m.id).or_default().push((w, r.emoji.clone()));
            }
        }
    }
    // Live reactions: one per reactor and message, and a new one replaces the old.
    for e in &chat.events {
        if let Event::Reaction { target, emoji, from, removed, .. } = e
            && wanted.contains(target)
        {
            let list = reactions.entry(*target).or_default();
            list.retain(|(f, _)| f != from);
            if !*removed {
                list.push((from.clone(), emoji.clone()));
            }
        }
    }
    let max_export_id = export.messages.iter().map(|m| m.id).max().unwrap_or(i64::MIN);
    msgs.iter()
        .map(|m| {
            let mut agg: Vec<(String, u32, bool)> = Vec::new();
            for (from, emoji) in reactions.get(&m.id).cloned().unwrap_or_default() {
                match agg.iter_mut().find(|(e, _, _)| *e == emoji) {
                    Some(a) => {
                        a.1 += 1;
                        a.2 |= from == chat.user;
                    }
                    None => agg.push((emoji, 1, from == chat.user)),
                }
            }
            for (emoji, count) in unknown.get(&m.id).cloned().unwrap_or_default() {
                agg.push((emoji, count, false));
            }
            let mut v = serde_json::to_value(m).unwrap_or_default();
            v["reactions"] = json!(agg.iter().map(|(e, c, mine)| json!({"emoji": e, "count": c, "mine": mine})).collect::<Vec<_>>());
            v["mine"] = json!(m.from == chat.user);
            v["persona"] = json!(m.from == chat.persona);
            v["live"] = json!(m.id > max_export_id);
            if let Some(lid) = m.media.as_ref().and_then(|x| x.library_id.as_deref())
                && let Some(item) = export.library_item(lid)
            {
                v["media"]["available"] = json!(item.available);
                if let Some(d) = &item.description {
                    v["media"]["description"] = json!(d);
                }
            }
            v
        })
        .collect()
}

/// Whole-timeline counters for the chat header/statistics, without serializing messages.
fn timeline_stats(export: &ChatExport, msgs: &[&Message]) -> Value {
    let max_export_id = export.messages.iter().map(|m| m.id).max().unwrap_or(i64::MIN);
    let (mut count, mut live, mut links) = (0usize, 0usize, 0usize);
    let mut by: BTreeMap<&str, usize> = BTreeMap::new();
    let mut media: BTreeMap<&str, usize> = BTreeMap::new();
    let (mut first, mut last) = (None::<&str>, None::<&str>);
    for m in msgs {
        if m.id > max_export_id {
            live += 1;
        }
        if m.service {
            continue;
        }
        count += 1;
        *by.entry(m.from.as_str()).or_default() += 1;
        if let Some(d) = m.date.as_deref() {
            first = first.or(Some(d));
            last = Some(d);
        }
        if m.link_preview.is_some() || m.text.contains("http://") || m.text.contains("https://") {
            links += 1;
        }
        if let Some(k) = m.media.as_ref().and_then(|x| x.kind) {
            let key = match k {
                MediaKind::Photo => "photo",
                MediaKind::Video => "video",
                MediaKind::RoundVideo => "round_video",
                MediaKind::Voice => "voice",
                MediaKind::Audio => "audio",
                MediaKind::File => "file",
                MediaKind::Sticker => "sticker",
                MediaKind::Gif => "gif",
                _ => continue,
            };
            *media.entry(key).or_default() += 1;
        }
    }
    json!({"messages": count, "live": live, "by": by, "media": media, "links": links, "first_date": first, "last_date": last})
}

#[derive(Deserialize)]
struct ChatQuery {
    limit: Option<usize>,
    before: Option<i64>,
}

async fn get_chat(State(st): S, Path(id): Path<String>, Query(q): Query<ChatQuery>) -> ApiResult {
    let (rec, chat) = st.snapshot(&id).await?;
    let all = timeline_messages(&rec.export, &chat);
    let (page, has_more) = page_messages(&all, q.before, q.limit.unwrap_or(500))?;
    let messages = view_messages(&rec.export, &chat, page);
    let total = all.len();
    if q.before.is_some() {
        return Ok(Json(json!({"messages": messages, "total": total, "has_more": has_more})));
    }
    let library: Vec<Value> = rec
        .export
        .library
        .iter()
        .filter(|i| matches!(i.kind, MediaKind::Sticker | MediaKind::Gif) && i.available)
        .map(|i| json!({"id": i.id, "kind": i.kind, "file": i.file, "thumb": i.thumb, "description": i.description, "uses": i.uses.len()}))
        .collect();
    Ok(Json(json!({
        "chat": chat_summary(&chat, Some(&rec)),
        "notes": chat.notes,
        "voice": chat.voice,
        "export": export_summary(&rec),
        "messages": messages,
        "total": total,
        "has_more": has_more,
        "stats": timeline_stats(&rec.export, &all),
        "library": library,
    })))
}

#[derive(Deserialize)]
struct ChatPatch {
    title: Option<String>,
    notes: Option<String>,
    voice: Option<String>,
}

async fn update_chat(State(st): S, Path(id): Path<String>, Json(p): Json<ChatPatch>) -> ApiResult {
    let mut db = st.db.write().await;
    let chat = db.chats.get_mut(&id).ok_or_else(|| anyhow!("chat not found"))?;
    if let Some(t) = p.title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
        chat.title = t;
    }
    let notes = p.notes.clone();
    if let Some(n) = p.notes {
        chat.notes = n;
    }
    let voice = p.voice.map(|v| v.trim().chars().take(64).collect::<String>());
    if let Some(v) = &voice {
        chat.voice = v.clone();
    }
    chat.updated_at = now_string();
    st.paths.save_chat(chat)?;
    // Visitor conversations of this chat's share links follow the notes.
    if notes.is_some() || voice.is_some() {
        let shares: Vec<String> = db.shares.values().filter(|s| s.chat_id == id).map(|s| s.id.clone()).collect();
        for c in db.chats.values_mut().filter(|c| c.guest.as_ref().is_some_and(|g| shares.contains(&g.share_id))) {
            if let Some(n) = &notes {
                c.notes = n.clone();
            }
            if let Some(v) = &voice {
                c.voice = v.clone();
            }
            st.paths.save_chat(c)?;
        }
    }
    Ok(Json(json!({"ok": true})))
}

async fn delete_chat(State(st): S, Path(id): Path<String>) -> ApiResult {
    let mut db = st.db.write().await;
    let chat = db.chats.remove(&id).ok_or_else(|| anyhow!("chat not found"))?;
    st.paths.delete_chat(&id)?;
    if let Some(g) = chat.guest {
        // A visitor conversation: forget the sessions that pointed at it.
        if let Some(share) = db.shares.get_mut(&g.share_id) {
            share.sessions.retain(|s| s.chat_id != id);
            if share.room_chat_id.as_deref() == Some(id.as_str()) {
                share.room_chat_id = None;
            }
            st.paths.save_share(share)?;
        }
    } else {
        // A template chat: its share links and their conversations go too.
        let shares: Vec<String> = db.shares.values().filter(|s| s.chat_id == id).map(|s| s.id.clone()).collect();
        for sid in &shares {
            crate::share_api::remove_share(&st, &mut db, sid)?;
        }
    }
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
pub(crate) struct PostMessage {
    text: Option<String>,
    reply_to: Option<i64>,
    quote: Option<String>,
    sticker_id: Option<String>,
    gif_id: Option<String>,
}

async fn post_message(State(st): S, Path(id): Path<String>, Json(req): Json<PostMessage>) -> ApiResult {
    let msg = add_message(&st, &id, req, 8000, None).await?;
    let (rec, chat) = st.snapshot(&id).await?;
    let all = timeline_messages(&rec.export, &chat);
    let mine: Vec<&Message> = all.iter().copied().filter(|m| m.id == msg).collect();
    let view = view_messages(&rec.export, &chat, &mine);
    Ok(Json(json!({"messages": view})))
}

/// Appends a message of the human side and returns its id. `author` is the
/// visitor session id in share rooms.
pub(crate) async fn add_message(
    st: &Arc<AppState>,
    id: &str,
    req: PostMessage,
    max_chars: usize,
    author: Option<&str>,
) -> Result<i64, ApiError> {
    let (rec, chat) = st.snapshot(id).await?;
    let export = &rec.export;
    let text = req.text.unwrap_or_default().trim().to_string();
    if text.chars().count() > max_chars {
        return Err(bad("message is too long"));
    }
    let media = match (req.sticker_id.as_deref(), req.gif_id.as_deref()) {
        (Some(sid), _) | (None, Some(sid)) => {
            let item = export
                .library_item(sid)
                .filter(|i| matches!(i.kind, MediaKind::Sticker | MediaKind::Gif))
                .ok_or_else(|| bad("unknown sticker/GIF id"))?;
            Some(crate::model::Media {
                kind: Some(item.kind),
                library_id: Some(item.id.clone()),
                file: item.file.clone(),
                thumb: item.thumb.clone(),
                ..Default::default()
            })
        }
        _ => None,
    };
    if text.is_empty() && media.is_none() {
        return Err(bad("empty message"));
    }
    let reply_to = match req.reply_to {
        Some(r) if crate::chat::find_message(export, &chat.events, r).is_none() => return Err(bad("reply target not found")),
        other => other,
    };
    let quote = req.quote.map(|q| q.trim().chars().take(1000).collect::<String>()).filter(|q| !q.is_empty() && reply_to.is_some());
    let msg_id = st.alloc_ids(id, 1).await?;
    let m = Message { id: msg_id, from: chat.user.clone(), date: Some(now_string()), reply_to, quote, text, media, ..Default::default() };
    let ev = Event::Message(m);
    st.append_events(id, std::slice::from_ref(&ev)).await?;
    if let Some(a) = author {
        let mut db = st.db.write().await;
        if let Some(c) = db.chats.get_mut(id)
            && let Some(g) = c.guest.as_mut()
        {
            g.authors.insert(msg_id, a.to_string());
            st.paths.save_chat(c)?;
        }
    }
    Ok(msg_id)
}

async fn post_voice(State(st): S, Path(id): Path<String>, mp: Multipart) -> ApiResult {
    let msg = add_voice(&st, &id, mp, 20 * 1024 * 1024, None).await?;
    let (rec, chat) = st.snapshot(&id).await?;
    let all = timeline_messages(&rec.export, &chat);
    let mine: Vec<&Message> = all.iter().copied().filter(|m| m.id == msg).collect();
    let view = view_messages(&rec.export, &chat, &mine);
    Ok(Json(json!({"messages": view})))
}

/// Stores a voice message recorded in the UI (multipart: `file`, optional
/// `duration` in seconds and `reply_to`), transcribes it with the media model
/// so the persona knows what was said, and appends it as the user's message.
pub(crate) async fn add_voice(
    st: &Arc<AppState>,
    id: &str,
    mut mp: Multipart,
    max_bytes: usize,
    author: Option<&str>,
) -> Result<i64, ApiError> {
    let (rec, chat) = st.snapshot(id).await?;
    let (mut bytes, mut duration, mut reply_to) = (Vec::new(), None::<f64>, None::<i64>);
    while let Some(mut field) = mp.next_field().await.map_err(|e| bad(format!("upload failed: {e}")))? {
        match field.name().unwrap_or_default() {
            "file" => {
                while let Some(chunk) = field.chunk().await.map_err(|e| bad(format!("upload failed: {e}")))? {
                    if bytes.len() + chunk.len() > max_bytes {
                        return Err(bad("the voice message is too long"));
                    }
                    bytes.extend_from_slice(&chunk);
                }
            }
            "duration" => duration = field.text().await.ok().and_then(|t| t.trim().parse().ok()),
            "reply_to" => reply_to = field.text().await.ok().and_then(|t| t.trim().parse().ok()),
            _ => {}
        }
    }
    let ext = match crate::genmedia::sniff(&bytes) {
        Some((MediaKind::Voice, ext)) => ext,
        _ => return Err(bad("not a supported audio recording (wav, ogg, mp3, m4a, webm)")),
    };
    if reply_to.is_some_and(|r| crate::chat::find_message(&rec.export, &chat.events, r).is_none()) {
        return Err(bad("reply target not found"));
    }
    let rel = format!("generated/{id}/usr_{}.{ext}", crate::share::random_id(10));
    let path = st.paths.export_files(&rec.id).join(&rel);
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await.map_err(anyhow::Error::from)?;
    }
    tokio::fs::write(&path, &bytes).await.map_err(anyhow::Error::from)?;
    let status = crate::genmedia::wav_duration(&bytes)
        .or_else(|| duration.filter(|d| d.is_finite() && *d >= 0.0).map(|d| crate::genmedia::fmt_duration(d.round() as u64)));
    let cfg = st.media_llm_config().await;
    let format = crate::genmedia::audio_format(ext).unwrap_or("wav");
    let transcript =
        match tokio::time::timeout(std::time::Duration::from_secs(90), crate::describe::transcribe(&st.llm, &cfg, &bytes, format)).await {
            Ok(Ok(t)) => Some(t),
            Ok(Err(e)) => {
                tracing::warn!(chat = id, model = %cfg.model, "voice transcription failed (set a media model that accepts audio): {e:#}");
                None
            }
            Err(_) => {
                tracing::warn!(chat = id, "voice transcription timed out");
                None
            }
        };
    let media =
        crate::model::Media { kind: Some(MediaKind::Voice), file: Some(rel), status, description: transcript, ..Default::default() };
    let msg_id = st.alloc_ids(id, 1).await?;
    let m = Message { id: msg_id, from: chat.user.clone(), date: Some(now_string()), reply_to, media: Some(media), ..Default::default() };
    st.append_events(id, &[Event::Message(m)]).await?;
    if let Some(a) = author {
        let mut db = st.db.write().await;
        if let Some(c) = db.chats.get_mut(id)
            && let Some(g) = c.guest.as_mut()
        {
            g.authors.insert(msg_id, a.to_string());
            st.paths.save_chat(c)?;
        }
    }
    Ok(msg_id)
}

#[derive(Deserialize)]
pub(crate) struct PostReaction {
    pub message_id: i64,
    pub emoji: String,
}

async fn post_reaction(State(st): S, Path(id): Path<String>, Json(req): Json<PostReaction>) -> ApiResult {
    add_reaction(&st, &id, req).await?;
    Ok(Json(json!({"ok": true})))
}

pub(crate) async fn add_reaction(st: &Arc<AppState>, id: &str, req: PostReaction) -> Result<(), ApiError> {
    let (rec, chat) = st.snapshot(id).await?;
    if crate::chat::find_message(&rec.export, &chat.events, req.message_id).is_none() {
        return Err(bad("message not found"));
    }
    let emoji = req.emoji.trim().to_string();
    if emoji.chars().count() > 8 {
        return Err(bad("invalid emoji"));
    }
    let ev = Event::Reaction { target: req.message_id, removed: emoji.is_empty(), emoji, from: chat.user.clone(), date: now_string() };
    st.append_events(id, &[ev]).await?;
    Ok(())
}

async fn turn_response(st: &Arc<AppState>, id: &str, outcome: agent::TurnOutcome) -> ApiResult {
    turn_view(st, id, outcome, view_messages).await
}

/// Messages and ordered actions of a persona turn, rendered by `view`.
pub(crate) async fn turn_view(
    st: &Arc<AppState>,
    id: &str,
    outcome: agent::TurnOutcome,
    view: impl FnOnce(&ChatExport, &Chat, &[&Message]) -> Vec<Value>,
) -> ApiResult {
    let (rec, chat) = st.snapshot(id).await?;
    let ids: Vec<i64> = outcome
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Message(m) => Some(m.id),
            _ => None,
        })
        .collect();
    let reacted: Vec<i64> = outcome
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Reaction { target, .. } => Some(*target),
            _ => None,
        })
        .collect();
    let wanted: Vec<&Message> =
        timeline_messages(&rec.export, &chat).into_iter().filter(|m| ids.contains(&m.id) || reacted.contains(&m.id)).collect();
    let views = view(&rec.export, &chat, &wanted);
    let messages: Vec<&Value> = views.iter().filter(|v| v["id"].as_i64().is_some_and(|i| ids.contains(&i))).collect();
    let updated: Vec<&Value> = views.iter().filter(|v| v["id"].as_i64().is_some_and(|i| reacted.contains(&i))).collect();
    // Order of actions, so the UI can replay them with typing delays.
    let actions: Vec<Value> = outcome
        .events
        .iter()
        .map(|e| match e {
            Event::Message(m) => json!({"type": "message", "id": m.id}),
            Event::Reaction { target, emoji, .. } => json!({"type": "reaction", "target": target, "emoji": emoji}),
        })
        .collect();
    Ok(Json(json!({"messages": messages, "updated": updated, "actions": actions, "error": outcome.error, "usage": outcome.usage})))
}

async fn respond(State(st): S, Path(id): Path<String>) -> ApiResult {
    let outcome = agent::respond(&st, &id).await?;
    turn_response(&st, &id, outcome).await
}

async fn regenerate(State(st): S, Path(id): Path<String>) -> ApiResult {
    drop_last_persona_turn(&st, &id).await?;
    let outcome = agent::respond(&st, &id).await?;
    turn_response(&st, &id, outcome).await
}

pub(crate) async fn drop_last_persona_turn(st: &Arc<AppState>, id: &str) -> Result<(), ApiError> {
    let lock = st.gen_lock(id);
    let _g = lock.lock().await;
    let mut db = st.db.write().await;
    let chat = db.chats.get_mut(id).ok_or_else(|| anyhow!("chat not found"))?;
    let persona = chat.persona.clone();
    while chat.events.last().is_some_and(|e| e.actor() == persona) {
        chat.events.pop();
    }
    st.paths.save_chat(chat)?;
    Ok(())
}

async fn get_prompt(State(st): S, Path(id): Path<String>) -> ApiResult {
    let (rec, chat) = st.snapshot(&id).await?;
    let budget = st.db.read().await.settings.history_budget_chars;
    let (mcp_defs, note, media_tools) = {
        let m = st.mcp.lock().await;
        (m.tool_definitions(), m.instructions_note(), m.media_tools())
    };
    let p = prompt::build(
        &rec.export,
        &chat,
        &PromptOptions { budget_chars: budget, extra_tools_note: note, media_tools: media_tools.clone() },
    );
    let mut tool_defs = tools::definitions();
    tool_defs.extend(tools::media_definitions(&media_tools));
    tool_defs.extend(mcp_defs);
    let cfg = st.llm_config().await;
    Ok(Json(json!({
        "model": cfg.model,
        "messages": p.messages,
        "tools": tool_defs,
        "dropped_turns": p.dropped_turns,
        "total_turns": p.total_turns,
        // The next reply will first summarize these turns into the memory.
        "pending_compaction": p.compaction.map(|c| json!({"from": c.from, "to": c.to})),
        "memory": chat.memory,
        "last_usage": chat.last_usage,
    })))
}

async fn get_settings(State(st): S) -> ApiResult {
    let s = st.db.read().await.settings.clone();
    Ok(Json(json!({
        "offline": {"on": st.is_offline(), "setting": s.offline, "forced": st.offline_forced()},
        "base_url": s.llm.base_url,
        "model": s.llm.model,
        "temperature": s.llm.temperature,
        "has_api_key": !s.llm.api_key.is_empty(),
        "env": {"base_url": st.env_llm.base_url, "model": st.env_llm.model, "has_api_key": !st.env_llm.api_key.is_empty()},
        "history_budget_chars": s.history_budget_chars,
        "max_tool_steps": s.max_tool_steps,
        "media_model": s.media_model,
        "env_media_model": std::env::var("OPENAI_MEDIA_MODEL").unwrap_or_default(),
        "mcpServers": s.mcp_servers,
        "telegram": {
            "mode": s.telegram.mode,
            "has_token": !s.telegram.token.trim().is_empty(),
            // TELEGRAM_BOT_TOKEN is set: Settings can't change the bot.
            "env": st.telegram_env.get().is_some(),
            "relay_host": crate::relay_client::relay_host(&crate::relay_client::relay_base(&s.sharing.relay_url)),
        },
    })))
}

#[derive(Deserialize)]
struct TelegramPatch {
    mode: Option<crate::store::TelegramMode>,
    /// `Some("")` clears the stored token, `None` keeps it.
    token: Option<String>,
}

#[derive(Deserialize)]
struct SettingsPatch {
    base_url: Option<String>,
    model: Option<String>,
    /// `Some("")` clears the stored key, `None` keeps it.
    api_key: Option<String>,
    temperature: Option<Option<f32>>,
    history_budget_chars: Option<usize>,
    max_tool_steps: Option<usize>,
    media_model: Option<String>,
    #[serde(rename = "mcpServers")]
    mcp_servers: Option<Value>,
    telegram: Option<TelegramPatch>,
    /// Offline mode: only local models, nothing leaves this computer.
    offline: Option<bool>,
}

async fn put_settings(State(st): S, Json(p): Json<SettingsPatch>) -> ApiResult {
    let mcp: Option<McpConfig> = match p.mcp_servers {
        None => None,
        // Accept either the bare map or `{"mcpServers": {...}}`.
        Some(v) => {
            let v = v.get("mcpServers").cloned().unwrap_or(v);
            let cfg: McpConfig = serde_json::from_value(v).map_err(|e| bad(format!("invalid MCP config: {e}")))?;
            for (name, c) in &cfg {
                if c.url.is_none() && c.command.is_none() {
                    return Err(bad(format!("MCP server `{name}` needs `url` or `command`")));
                }
            }
            Some(cfg)
        }
    };
    if let Some(t) = p.telegram.as_ref().and_then(|t| t.token.as_deref()).map(str::trim).filter(|t| !t.is_empty()) {
        let well_formed = t.split_once(':').is_some_and(|(id, rest)| {
            !id.is_empty()
                && id.bytes().all(|b| b.is_ascii_digit())
                && rest.len() >= 30
                && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        });
        if !well_formed {
            return Err(bad("That doesn't look like a bot token. @BotFather shows it as 123456789:AA…"));
        }
    }
    let mut db = st.db.write().await;
    let s = &mut db.settings;
    let was_offline = st.is_offline();
    let telegram_changed = p.telegram.is_some();
    if let Some(v) = p.offline {
        s.offline = v;
    }
    if let Some(t) = p.telegram {
        if let Some(m) = t.mode {
            s.telegram.mode = m;
        }
        if let Some(token) = t.token {
            s.telegram.token = token.trim().to_string();
        }
    }
    if let Some(v) = p.base_url {
        s.llm.base_url = v.trim().to_string();
    }
    if let Some(v) = p.model {
        s.llm.model = v.trim().to_string();
    }
    if let Some(v) = p.api_key {
        s.llm.api_key = v.trim().to_string();
    }
    if let Some(t) = p.temperature {
        s.llm.temperature = t.map(|x| x.clamp(0.0, 2.0));
    }
    if let Some(b) = p.history_budget_chars {
        s.history_budget_chars = b.clamp(4_000, 2_000_000);
    }
    if let Some(m) = p.max_tool_steps {
        s.max_tool_steps = m.clamp(1, 20);
    }
    if let Some(m) = p.media_model {
        s.media_model = m.trim().to_string();
    }
    let changed_mcp = mcp.is_some_and(|m| {
        let changed = m != s.mcp_servers;
        s.mcp_servers = m;
        changed
    });
    st.paths.save_settings(s)?;
    st.set_offline_setting(s.offline);
    let offline_changed = st.is_offline() != was_offline;
    if changed_mcp || offline_changed {
        *st.mcp.lock().await = st.mcp_manager(s.mcp_servers.clone());
    }
    drop(db);
    if offline_changed {
        crate::sharing::reapply(&st).await;
    }
    if telegram_changed || offline_changed {
        let _g = st.control.lock().await;
        crate::tgbot::apply(&st).await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("Telegram: {e:#}")))?;
    }
    Ok(Json(json!({"ok": true})))
}

/// A tiny request with the saved settings, so a wrong key or model shows up in
/// Settings instead of in the middle of a conversation.
async fn test_llm(State(st): S) -> ApiResult {
    let cfg = st.llm_config().await;
    let started = std::time::Instant::now();
    let msgs = [json!({"role": "user", "content": "Reply with the single word: ok"})];
    Ok(Json(match st.llm.complete(&cfg, &msgs, &[]).await {
        Ok(c) => json!({
            "ok": true, "model": cfg.model, "host": crate::llm::host_of(&cfg.base_url),
            "ms": started.elapsed().as_millis() as u64,
            "reply": crate::llm::truncate(c.content.as_deref().unwrap_or_default().trim(), 80),
        }),
        Err(e) => json!({"ok": false, "model": cfg.model, "error": format!("{e:#}")}),
    }))
}

async fn mcp_status(State(st): S) -> ApiResult {
    let mut m = st.mcp.lock().await;
    m.ensure_connected().await;
    Ok(Json(json!({"servers": m.status()})))
}

async fn mcp_reconnect(State(st): S) -> ApiResult {
    let cfg = st.mcp.lock().await.config().clone();
    let mut fresh = st.mcp_manager(cfg);
    fresh.ensure_connected().await;
    let status = fresh.status();
    *st.mcp.lock().await = fresh;
    Ok(Json(json!({"servers": status})))
}

async fn media(State(st): S, Path((export_id, path)): Path<(String, String)>, headers: HeaderMap) -> Result<Response, ApiError> {
    if !st.db.read().await.exports.contains_key(&export_id) {
        return Err(ApiError(StatusCode::NOT_FOUND, "not found".into()));
    }
    serve_media(&st, &export_id, &path, headers).await
}

pub(crate) async fn serve_media(st: &AppState, export_id: &str, path: &str, headers: HeaderMap) -> Result<Response, ApiError> {
    let rel = safe_rel(path).ok_or_else(|| bad("invalid path"))?;
    let full = st.paths.export_files(export_id).join(rel);
    let mut req = Request::new(Body::empty());
    *req.headers_mut() = headers;
    let mut resp = tower_http::services::ServeFile::new(&full).oneshot(req).await.map_err(|e| anyhow!("{e}"))?.map(Body::new);
    // Telegram stores animated stickers as gzip'd Lottie JSON.
    if full.extension().is_some_and(|e| e == "tgs") {
        resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/x-tgsticker"));
    }
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400"));
    resp.headers_mut().insert("X-Content-Type-Options", HeaderValue::from_static("nosniff"));
    Ok(resp)
}
