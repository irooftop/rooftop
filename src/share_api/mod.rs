//! HTTP handlers for share links: owner management (behind the owner auth)
//! and the public visitor API under `/s/<slug>` and `/api/s/<slug>`.

mod owner;
mod visitor;

pub(crate) use owner::remove_share;
pub use owner::{create_share, delete_session, delete_share, list_shares, share_summary, update_share};

use crate::server::ApiError;
use crate::state::AppState;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use std::sync::Arc;
use std::time::Duration;

type S = State<Arc<AppState>>;

const GONE: &str = "This link doesn't exist or is no longer active.";
/// For an address the owner blocked: no detail on why.
const BLOCKED: &str = "This link isn't available to you.";
const GUEST_MAX_CHARS: usize = 4000;

fn not_found(msg: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, msg.into())
}

fn too_many(wait: Duration) -> ApiError {
    let secs = wait.as_secs().max(1);
    let human = if secs >= 120 { format!("{} min", secs.div_ceil(60)) } else { format!("{secs} s") };
    ApiError(StatusCode::TOO_MANY_REQUESTS, format!("Too many attempts. Try again in {human}."))
}

pub fn guest_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/s/{slug}", get(crate::server::guest_gate_index))
        .route("/s/{slug}/{token}", get(crate::server::guest_chat_index))
        .route("/api/s/{slug}", get(visitor::guest_info))
        .route("/api/s/{slug}/avatar", get(visitor::guest_avatar))
        .route("/api/s/{slug}/sessions", post(visitor::guest_start))
        .route("/api/s/{slug}/c/{token}", get(visitor::guest_chat))
        .route("/api/s/{slug}/c/{token}/messages", post(visitor::guest_message))
        .route("/api/s/{slug}/c/{token}/react", post(visitor::guest_react))
        .route("/api/s/{slug}/c/{token}/voice", post(visitor::guest_voice))
        .route("/api/s/{slug}/c/{token}/respond", post(visitor::guest_respond))
        .route("/api/s/{slug}/c/{token}/regenerate", post(visitor::guest_regenerate))
        .route("/api/s/{slug}/c/{token}/media/{*path}", get(visitor::guest_media))
}
