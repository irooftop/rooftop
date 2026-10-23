//! Paged chat views over a synthetic export (no sample data needed): window and
//! `has_more` semantics, `before` validation, minimal write responses and the
//! guest view never exposing hidden history.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use rooftop::llm::LlmConfig;
use rooftop::model::{ChatExport, Message, Participant};
use rooftop::server::{self, ServerOptions};
use rooftop::state::AppState;
use rooftop::store::{ExportRecord, Paths};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

const EXPORT_MSGS: i64 = 40;

struct Env {
    _tmp: tempfile::TempDir,
    app: Router,
    chat_id: String,
}

async fn setup() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(tmp.path().to_path_buf());
    let db = paths.load().unwrap();
    let llm = LlmConfig { base_url: "http://127.0.0.1:9/v1".into(), api_key: String::new(), model: "test".into(), temperature: None };
    let state = AppState::new(paths, db, llm);
    let messages = (1..=EXPORT_MSGS)
        .map(|i| Message {
            id: i * 10,
            from: if i % 2 == 0 { "Ann".into() } else { "Bob".into() },
            date: Some(format!("2026-01-{:02}T10:00:00", i.min(28))),
            text: if i == 3 { "see https://example.com".into() } else { format!("export message {i}") },
            ..Default::default()
        })
        .collect();
    let export = ChatExport {
        title: "Ann".into(),
        participants: vec![
            Participant { name: "Ann".into(), ..Default::default() },
            Participant { name: "Bob".into(), ..Default::default() },
        ],
        messages,
        library: Vec::new(),
        blank: false,
    };
    let rec = Arc::new(ExportRecord { id: "exp1".into(), imported_at: "now".into(), fingerprint: "fp".into(), export });
    state.paths.save_export(&rec).unwrap();
    state.db.write().await.exports.insert(rec.id.clone(), rec.clone());
    let chat = server::make_chat(&rec, "Ann", Some("Bob"), None, "secret owner notes").map_err(|_| ()).unwrap();
    let chat_id = chat.id.clone();
    state.paths.save_chat(&chat).unwrap();
    state.db.write().await.chats.insert(chat.id.clone(), chat);
    let app = server::router(state, ServerOptions { password: None, ..Default::default() });
    Env { _tmp: tmp, app, chat_id }
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value, String) {
    let mut b = Request::builder().method(method).uri(uri);
    if body.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    let req = b.body(body.map_or_else(Body::empty, |v| Body::from(v.to_string()))).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8_lossy(&bytes).to_string();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null), text)
}

fn ids(v: &Value) -> Vec<i64> {
    v["messages"].as_array().unwrap().iter().map(|m| m["id"].as_i64().unwrap()).collect()
}

async fn post_live(env: &Env, n: usize) -> Vec<i64> {
    let mut out = Vec::new();
    for i in 0..n {
        let (st, v, text) =
            call(&env.app, "POST", &format!("/api/chats/{}/messages", env.chat_id), Some(json!({"text": format!("live {i}")}))).await;
        assert_eq!(st, StatusCode::OK, "{text}");
        assert_eq!(v["messages"].as_array().unwrap().len(), 1, "only the new message is returned");
        out.extend(ids(&v));
    }
    out
}

#[tokio::test]
async fn pages_walk_the_timeline_backwards() {
    let env = setup().await;
    let live = post_live(&env, 3).await;
    let total = EXPORT_MSGS as usize + 3;
    let uri = format!("/api/chats/{}", env.chat_id);

    let (st, first, text) = call(&env.app, "GET", &format!("{uri}?limit=10"), None).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    assert_eq!(first["total"], total);
    assert_eq!(first["has_more"], true);
    assert!(first.get("chat").is_some() && first.get("library").is_some());
    let page = ids(&first);
    assert_eq!(page.len(), 10);
    assert_eq!(&page[7..], live.as_slice(), "live messages come last");
    assert_eq!(page[6], EXPORT_MSGS * 10);
    assert_eq!(first["stats"]["live"], 3);
    assert_eq!(first["stats"]["messages"], total);
    assert_eq!(first["stats"]["by"]["Bob"], 20 + 3);
    assert_eq!(first["stats"]["links"], 1);

    // Walk back to the start with lightweight pages.
    let mut all = page.clone();
    let mut cursor = page[0];
    loop {
        let (st, v, text) = call(&env.app, "GET", &format!("{uri}?limit=15&before={cursor}"), None).await;
        assert_eq!(st, StatusCode::OK, "{text}");
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys.len(), 3, "older pages are lightweight: {keys:?}");
        assert_eq!(v["total"], total);
        let older = ids(&v);
        assert!(!older.is_empty() && older.len() <= 15);
        all.splice(0..0, older.clone());
        cursor = older[0];
        if !v["has_more"].as_bool().unwrap() {
            break;
        }
    }
    let expected: Vec<i64> = (1..=EXPORT_MSGS).map(|i| i * 10).chain(live).collect();
    assert_eq!(all, expected);

    // Everything in one window: nothing older.
    let (_, v, _) = call(&env.app, "GET", &format!("{uri}?limit=5000"), None).await;
    assert_eq!(ids(&v).len(), total);
    assert_eq!(v["has_more"], false);
    // limit=0 is clamped to 1.
    let (_, v, _) = call(&env.app, "GET", &format!("{uri}?limit=0"), None).await;
    assert_eq!(ids(&v).len(), 1);
}

#[tokio::test]
async fn unknown_before_is_a_bad_request() {
    let env = setup().await;
    let (st, v, _) = call(&env.app, "GET", &format!("/api/chats/{}?before=999999", env.chat_id), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(v["error"].is_string());
}

#[tokio::test]
async fn guest_paging_never_leaks_hidden_history() {
    let env = setup().await;
    let (st, _, text) = call(
        &env.app,
        "POST",
        &format!("/api/chats/{}/shares", env.chat_id),
        Some(json!({"slug": "pager", "display_name": "Anna", "history": "recent", "history_recent": 5})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{text}");
    let (_, s, _) = call(&env.app, "POST", "/api/s/pager/sessions", Some(json!({}))).await;
    let token = s["token"].as_str().unwrap().to_string();
    let uri = format!("/api/s/pager/c/{token}");
    let (st, _, _) = call(&env.app, "POST", &format!("{uri}/messages"), Some(json!({"text": "hello"}))).await;
    assert_eq!(st, StatusCode::OK);

    let (st, v, text) = call(&env.app, "GET", &format!("{uri}?limit=3"), None).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    assert_eq!(v["total"], 6, "5 visible export messages + 1 live");
    assert_eq!(v["has_more"], true);
    let page = ids(&v);
    assert_eq!(page.len(), 3);
    let first_visible = (EXPORT_MSGS - 4) * 10;
    let (st, older, text) = call(&env.app, "GET", &format!("{uri}?limit=50&before={}", page[0]), None).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    assert_eq!(older["has_more"], false, "history stops at the visible window");
    assert_eq!(older.as_object().unwrap().len(), 3);
    assert_eq!(ids(&older).first(), Some(&first_visible));
    assert_eq!(ids(&older).len() + page.len(), 6);
    for m in older["messages"].as_array().unwrap().iter().chain(v["messages"].as_array().unwrap()) {
        assert!(m["id"].as_i64().unwrap() >= first_visible);
        assert_ne!(m["from"], "Ann", "persona is renamed");
    }

    // A hidden export message cannot be used as cursor-free entry to older history.
    let (st, _, _) = call(&env.app, "GET", &format!("{uri}?before={}", (EXPORT_MSGS - 10) * 10), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _, _) = call(&env.app, "GET", &format!("{uri}?before=123456"), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}
