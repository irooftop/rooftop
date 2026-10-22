//! Offline mode: remote LLM and MCP endpoints are refused without any network
//! request, and the relay and the Telegram bot are not started.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use rooftop::llm::LlmConfig;
use rooftop::server::{self, ServerOptions};
use rooftop::state::AppState;
use rooftop::store::{Paths, TelegramMode};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tower::ServiceExt;

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(uri);
    if body.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    let req = b.body(body.map_or_else(Body::empty, |v| Body::from(v.to_string()))).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// A TCP listener standing in for the relay hub that counts connections.
async fn spy() -> (String, Arc<AtomicUsize>) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    tokio::spawn(async move {
        loop {
            if l.accept().await.is_ok() {
                h.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    (url, hits)
}

const TOKEN: &str = "123456789:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

#[tokio::test]
async fn offline_refuses_remote_models_and_keeps_relay_and_telegram_off() {
    let (hub, hub_hits) = spy().await;
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(tmp.path().to_path_buf());
    let mut db = paths.load().unwrap();
    db.settings.offline = true;
    db.settings.sharing.internet = true;
    db.settings.sharing.relay_url = hub.clone();
    db.settings.telegram.mode = TelegramMode::Own;
    db.settings.telegram.token = TOKEN.into();
    let llm = LlmConfig { base_url: "https://openrouter.ai/api/v1".into(), api_key: "key".into(), model: "m".into(), temperature: None };
    let state = AppState::new(paths, db, llm);
    assert!(state.is_offline());
    let router = server::router(state.clone(), ServerOptions::default());

    // A remote model: refused with the reason, before any DNS lookup or connection.
    let (st, v) = call(&router, "POST", "/api/llm/test", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["ok"], false);
    let err = v["error"].as_str().unwrap();
    assert!(err.starts_with("Offline mode: https://openrouter.ai is not a local endpoint"), "{err}");

    // The relay and the Telegram bot are not started, even though both are switched on.
    rooftop::sharing::init(&state, "127.0.0.1:1".parse().unwrap()).await;
    rooftop::tgbot::apply(&state).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(hub_hits.load(Ordering::SeqCst), 0, "the relay was contacted");
    assert!(state.telegram.read().unwrap().is_none(), "the Telegram bot was started");
    let (_, s) = call(&router, "GET", "/api/sharing", None).await;
    assert_eq!((s["offline"].as_bool(), s["internet"]["state"].as_str()), (Some(true), Some("off")), "{s}");

    // Remote MCP servers are refused, local ones are not (this one just isn't running).
    let cfg = json!({"mcpServers": {
        "remote": {"url": "https://mcp.example.com/mcp"},
        "local": {"url": "http://127.0.0.1:9/mcp"},
    }});
    let (st, _) = call(&router, "PUT", "/api/settings", Some(json!({"mcpServers": cfg}))).await;
    assert_eq!(st, StatusCode::OK);
    let (_, m) = call(&router, "GET", "/api/mcp", None).await;
    let servers = m["servers"].as_array().unwrap();
    let error_of = |n: &str| servers.iter().find(|s| s["name"] == n).and_then(|s| s["error"].as_str()).unwrap_or_default().to_string();
    assert!(error_of("remote").starts_with("Offline mode: https://mcp.example.com is not a local endpoint"), "{m}");
    assert!(!error_of("local").starts_with("Offline mode"), "{m}");

    // Settings report the state; switching it off lets remote endpoints through to the usual checks again.
    let (_, s) = call(&router, "GET", "/api/settings", None).await;
    assert_eq!(s["offline"]["on"], true);
    let (st, _) = call(&router, "PUT", "/api/settings", Some(json!({"offline": false}))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(!state.is_offline());
}

#[tokio::test]
async fn internet_sharing_cannot_be_switched_on_while_offline() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(tmp.path().to_path_buf());
    let mut db = paths.load().unwrap();
    db.settings.offline = true;
    let state = AppState::new(paths, db, LlmConfig::default());
    rooftop::sharing::init(&state, "127.0.0.1:1".parse().unwrap()).await;
    let router = server::router(state, ServerOptions::default());
    let (st, v) = call(&router, "PUT", "/api/sharing", Some(json!({"internet": true}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["error"].as_str().unwrap_or_default().contains("Offline mode"), "{v}");
}
