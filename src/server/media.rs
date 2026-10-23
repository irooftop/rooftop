//! Serving an export's media files.

use super::{ApiError, S, bad};
use crate::import::safe_rel;
use crate::state::AppState;
use anyhow::anyhow;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use tower::ServiceExt;

pub(super) async fn media(State(st): S, Path((export_id, path)): Path<(String, String)>, headers: HeaderMap) -> Result<Response, ApiError> {
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
