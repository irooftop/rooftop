//! App side of the relay ([`crate::relay_proto`]): keeps a WebSocket to the hub
//! and answers visitor requests from the guest-only router, so share links work
//! at `https://<name>.<domain>` while the app runs.

use crate::relay_proto::{self, Frame};
use anyhow::{Result, anyhow};
use axum::Router;
use axum::body::Body;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

pub const DEFAULT_RELAY: &str = "https://iroof.top";

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RelayStatus {
    /// `off`, `connecting`, `connected` or `error`.
    pub state: &'static str,
    pub error: Option<String>,
    /// Public base URL, e.g. `https://alice.iroof.top`.
    pub url: Option<String>,
}

/// Hub base URL: the setting, then `ROOFTOP_RELAY_URL`, then iroof.top.
pub fn relay_base(setting: &str) -> String {
    let s = setting.trim();
    let base = if s.is_empty() { std::env::var("ROOFTOP_RELAY_URL").unwrap_or_default() } else { s.to_string() };
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() { DEFAULT_RELAY.to_string() } else { base.to_string() }
}

/// Host part of the hub URL, for display (`iroof.top`).
pub fn relay_host(base: &str) -> String {
    base.split("://").nth(1).unwrap_or(base).split('/').next().unwrap_or_default().to_string()
}

/// Why a registration failed: the hub's message, and whether the name is the problem.
#[derive(Debug)]
pub struct RegisterError {
    pub name_problem: bool,
    pub message: String,
}

impl std::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Claims `name` at the hub for the account of `secret`. Returns the public URL.
pub async fn register(http: &reqwest::Client, base: &str, secret: &str, name: &str) -> Result<String, RegisterError> {
    let url = format!("{base}{}", relay_proto::REGISTER_PATH);
    let resp =
        http.post(&url).bearer_auth(secret).json(&json!({"name": name})).timeout(Duration::from_secs(20)).send().await.map_err(|e| {
            RegisterError { name_problem: false, message: format!("Can't reach {}: {}", relay_host(base), e.without_url()) }
        })?;
    let status = resp.status();
    let v: Value = resp.json().await.unwrap_or_default();
    if status.is_success() {
        return v["url"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| RegisterError { name_problem: false, message: "unexpected relay reply".into() });
    }
    let message = v["error"].as_str().map_or_else(|| format!("relay error {status}"), String::from);
    Err(RegisterError { name_problem: matches!(status.as_u16(), 400 | 409), message })
}

fn ws_url(base: &str) -> String {
    let ws = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{ws}{}", relay_proto::CONNECT_PATH)
}

pub struct ClientParams {
    pub base: String,
    pub secret: String,
    pub name: String,
    /// Guest-only router (trusts `X-Forwarded-For`, which the hub sets).
    pub guest: Router,
    pub status: Arc<Mutex<RelayStatus>>,
    pub cancel: CancellationToken,
}

fn set(status: &Mutex<RelayStatus>, state: &'static str, error: Option<String>) {
    let mut s = status.lock().expect("relay status");
    s.state = state;
    s.error = error;
}

/// Connects, serves and reconnects with backoff until cancelled.
pub async fn run(p: ClientParams) {
    crate::llm::ensure_crypto_provider();
    let http = reqwest::Client::new();
    let mut backoff = Duration::from_secs(2);
    loop {
        if p.cancel.is_cancelled() {
            break;
        }
        // Keep the last error visible while reconnecting.
        p.status.lock().expect("relay status").state = "connecting";
        let started = std::time::Instant::now();
        match connect(&p).await {
            Ok(ws) => {
                set(&p.status, "connected", None);
                backoff = Duration::from_secs(2);
                let res = serve(ws, &p).await;
                if p.cancel.is_cancelled() {
                    break;
                }
                tracing::info!("relay connection closed{}", res.err().map(|e| format!(": {e:#}")).unwrap_or_default());
                set(&p.status, "connecting", None);
                if started.elapsed() < Duration::from_secs(30) {
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
            }
            Err(ConnectError::Unauthorized) => {
                // The hub doesn't know the account (e.g. its data was reset): claim the name again.
                match register(&http, &p.base, &p.secret, &p.name).await {
                    Ok(url) => {
                        p.status.lock().expect("relay status").url = Some(url);
                        continue;
                    }
                    Err(e) if e.name_problem => {
                        set(&p.status, "error", Some(e.message));
                        p.cancel.cancelled().await;
                        break;
                    }
                    Err(e) => set(&p.status, "error", Some(e.message)),
                }
            }
            Err(ConnectError::Other(e)) => {
                tracing::debug!("relay connect: {e:#}");
                set(&p.status, "error", Some(format!("Can't reach {}. Retrying…", relay_host(&p.base))));
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = p.cancel.cancelled() => break,
        }
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
    set(&p.status, "off", None);
}

enum ConnectError {
    Unauthorized,
    Other(anyhow::Error),
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(p: &ClientParams) -> Result<Ws, ConnectError> {
    use tungstenite::client::IntoClientRequest;
    let mut req = ws_url(&p.base).into_client_request().map_err(|e| ConnectError::Other(e.into()))?;
    let auth = format!("Bearer {}", p.secret).parse().map_err(|_| ConnectError::Other(anyhow!("invalid secret")))?;
    req.headers_mut().insert("authorization", auth);
    let mut cfg = tungstenite::protocol::WebSocketConfig::default();
    cfg.max_message_size = Some(relay_proto::MAX_MESSAGE);
    cfg.max_frame_size = Some(relay_proto::MAX_MESSAGE);
    let fut = tokio_tungstenite::connect_async_with_config(req, Some(cfg), false);
    match tokio::time::timeout(Duration::from_secs(20), fut).await {
        Ok(Ok((ws, _))) => Ok(ws),
        Ok(Err(tungstenite::Error::Http(resp))) if resp.status() == 401 => Err(ConnectError::Unauthorized),
        Ok(Err(e)) => Err(ConnectError::Other(e.into())),
        Err(_) => Err(ConnectError::Other(anyhow!("connect timed out"))),
    }
}

async fn serve(ws: Ws, p: &ClientParams) -> Result<()> {
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::channel::<Message>(64);
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if sink.send(m).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let tasks: Arc<Mutex<HashMap<u64, tokio::task::AbortHandle>>> = Default::default();
    let result = loop {
        let next = tokio::select! {
            n = tokio::time::timeout(Duration::from_secs(75), stream.next()) => n,
            _ = p.cancel.cancelled() => break Ok(()),
        };
        let msg = match next {
            Ok(Some(Ok(m))) => m,
            Ok(Some(Err(e))) => break Err(e.into()),
            Ok(None) => break Ok(()),
            Err(_) => break Err(anyhow!("the relay stopped responding")),
        };
        let Message::Binary(bytes) = msg else { continue };
        let Ok((frame, payload)) = relay_proto::decode(&bytes) else { continue };
        match frame {
            Frame::Hello { url, .. } => p.status.lock().expect("relay status").url = Some(url),
            Frame::Request { id, method, uri, headers } => {
                let body = payload.to_vec();
                let guest = p.guest.clone();
                let tx = tx.clone();
                let done = tasks.clone();
                let task = tokio::spawn(async move {
                    answer(guest, &tx, id, method, uri, headers, body).await;
                    done.lock().expect("relay tasks").remove(&id);
                });
                tasks.lock().expect("relay tasks").insert(id, task.abort_handle());
            }
            Frame::Cancel { id } => {
                if let Some(t) = tasks.lock().expect("relay tasks").remove(&id) {
                    t.abort();
                }
            }
            _ => {}
        }
    };
    for (_, t) in tasks.lock().expect("relay tasks").drain() {
        t.abort();
    }
    drop(tx);
    let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
    result
}

async fn send(tx: &mpsc::Sender<Message>, frame: &Frame, payload: &[u8]) -> bool {
    tx.send(Message::Binary(relay_proto::encode(frame, payload).into())).await.is_ok()
}

/// Runs one visitor request through the guest router and streams the response back.
async fn answer(
    guest: Router,
    tx: &mpsc::Sender<Message>,
    id: u64,
    method: String,
    uri: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
) {
    let mut b = axum::http::Request::builder().method(method.as_str()).uri(uri.as_str());
    for (k, v) in &headers {
        if !relay_proto::is_dropped_header(k) {
            b = b.header(k.as_str(), v.as_str());
        }
    }
    let req = match b.body(Body::from(body)) {
        Ok(r) => r,
        Err(e) => {
            send(tx, &Frame::Fail { id, message: format!("bad request: {e}") }, &[]).await;
            return;
        }
    };
    let resp = match guest.oneshot(req).await {
        Ok(r) => r,
        Err(e) => {
            send(tx, &Frame::Fail { id, message: e.to_string() }, &[]).await;
            return;
        }
    };
    let head = Frame::Head { id, status: resp.status().as_u16(), headers: relay_proto::header_pairs(resp.headers()) };
    if !send(tx, &head, &[]).await {
        return;
    }
    let mut data = resp.into_body().into_data_stream();
    while let Some(chunk) = data.next().await {
        match chunk {
            Ok(bytes) => {
                for part in bytes.chunks(relay_proto::CHUNK) {
                    if !send(tx, &Frame::Data { id }, part).await {
                        return;
                    }
                }
            }
            Err(e) => {
                send(tx, &Frame::Fail { id, message: e.to_string() }, &[]).await;
                return;
            }
        }
    }
    send(tx, &Frame::End { id }, &[]).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(ws_url("https://iroof.top"), "wss://iroof.top/relay/v1/connect");
        assert_eq!(ws_url("http://127.0.0.1:9"), "ws://127.0.0.1:9/relay/v1/connect");
        assert_eq!(relay_host("https://iroof.top"), "iroof.top");
        assert_eq!(relay_host("http://127.0.0.1:9/x"), "127.0.0.1:9");
        assert_eq!(relay_base(" https://example.org/ "), "https://example.org");
    }
}
