//! HTTP API + embedded web UI.
//!
//! [`router`] assembles the owner's API and pages, [`guest_router`] the visitor side; the
//! handlers live in one submodule per area. Paths like `crate::server::make_chat` stay
//! as they were: the submodules' shared items are re-exported here.

mod assets;
mod chats;
mod exports;
mod media;
mod messages;
mod settings;
mod uploads;

pub(crate) use assets::{guest_chat_index, guest_gate_index};
pub use chats::make_chat;
pub(crate) use chats::{page_messages, timeline_messages, view_messages_as};
pub(crate) use exports::export_summary;
pub(crate) use media::serve_media;
pub(crate) use messages::{PostMessage, PostReaction, add_message, add_reaction, add_voice, drop_last_persona_turn, turn_view};
pub use uploads::import_into_store;

use crate::state::AppState;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;

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

#[derive(Default)]
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
    /// The desktop app's native file access (`/api/local/*`); `None` on a server.
    pub desktop: Option<Desktop>,
}

/// What the desktop app lends the server. Its webview can't always read the files the
/// user chooses (on macOS, WebKit's sandboxed networking process is refused some of them,
/// so an upload arrives empty or fails with `NotReadableError`): the app shows the native
/// picker, or hands over dropped paths, and the server imports from the paths directly.
#[derive(Clone)]
pub struct Desktop {
    /// Shows the native picker; `None` when it was cancelled.
    pub pick: Arc<dyn Fn(Pick) -> futures::future::BoxFuture<'static, Option<PathBuf>> + Send + Sync>,
}

/// What the native picker asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pick {
    /// An export folder.
    Folder,
    /// A `.zip` or a WhatsApp `.txt`.
    File,
}

/// Largest request a visitor can send (voice messages are ≤ 5 MiB).
const GUEST_MAX_BODY: usize = 16 * 1024 * 1024;

/// What visitors need: the share pages and their API, and the static assets.
fn public_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/app.js", get(assets::app_js))
        .route("/app.css", get(assets::app_css))
        .route("/favicon.svg", get(assets::favicon_svg))
        .route("/favicon.ico", get(assets::favicon_ico))
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
        .route("/", get(assets::guest_root))
        .route("/healthz", get(|| async { "ok" }))
        .fallback(|| async { (StatusCode::NOT_FOUND, "not found") })
        .layer(DefaultBodyLimit::max(GUEST_MAX_BODY))
        .layer(tower_http::compression::CompressionLayer::new())
        .with_state(state)
}

pub fn router(state: Arc<AppState>, opts: ServerOptions) -> Router {
    let password = opts.password.filter(|p| !p.is_empty()).map(Arc::new);
    let trust_proxy = opts.trust_proxy;
    let app = Router::new()
        .route("/", get(assets::index))
        .route("/api/state", get(settings::api_state))
        .route("/api/exports", post(uploads::upload_export))
        .route("/api/uploads", post(uploads::create_upload))
        .route("/api/uploads/{id}", axum::routing::delete(uploads::cancel_upload))
        .route("/api/uploads/{id}/files", post(uploads::upload_batch))
        .route("/api/uploads/{id}/check", post(uploads::check_upload))
        .route("/api/uploads/{id}/import", post(uploads::finish_upload))
        .route("/api/imports/{id}", get(uploads::get_import))
        .route("/api/exports/{id}", get(exports::get_export).delete(exports::delete_export))
        .route("/api/exports/{id}/describe", post(exports::describe_export))
        .route("/api/exports/{id}/library/{item}", patch(exports::patch_library_item))
        .route("/api/chats", post(chats::create_chat))
        .route("/api/chats/{id}", get(chats::get_chat).patch(chats::update_chat).delete(chats::delete_chat))
        .route("/api/chats/{id}/messages", post(messages::post_message))
        .route("/api/chats/{id}/react", post(messages::post_reaction))
        .route("/api/chats/{id}/voice", post(messages::post_voice))
        .route("/api/chats/{id}/respond", post(messages::respond))
        .route("/api/chats/{id}/regenerate", post(messages::regenerate))
        .route("/api/chats/{id}/prompt", get(messages::get_prompt))
        .route("/api/chats/{id}/shares", get(crate::share_api::list_shares).post(crate::share_api::create_share))
        .route("/api/shares/{id}", patch(crate::share_api::update_share).delete(crate::share_api::delete_share))
        .route("/api/shares/{id}/sessions/{session}", axum::routing::delete(crate::share_api::delete_session))
        .route("/api/settings", get(settings::get_settings).put(settings::put_settings))
        .route("/api/llm/test", post(settings::test_llm))
        .route("/api/mcp", get(settings::mcp_status))
        .route("/api/mcp/reconnect", post(settings::mcp_reconnect))
        .route("/api/telegram", get(crate::tgbot::status))
        .route("/api/telegram/link", post(crate::tgbot::add_link))
        .merge(crate::sharing::owner_routes())
        .merge(opts.desktop.map(uploads::local_routes).unwrap_or_default())
        .route("/media/{export_id}/{*path}", get(media::media))
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
