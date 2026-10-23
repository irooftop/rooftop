//! Chats without an imported history: started from a name and notes only.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use rooftop::llm::LlmConfig;
use rooftop::server::{self, ServerOptions};
use rooftop::state::AppState;
use rooftop::store::Paths;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn call(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(uri);
    if body.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    let resp = app.clone().oneshot(b.body(body.map_or_else(Body::empty, |v| Body::from(v.to_string()))).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn a_chat_from_notes_needs_no_import_and_goes_with_its_chat() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(tmp.path().to_path_buf());
    let db = paths.load().unwrap();
    let llm = LlmConfig { base_url: "http://127.0.0.1:9/v1".into(), api_key: String::new(), model: "test".into(), temperature: None };
    let app = server::router(AppState::new(paths.clone(), db, llm), ServerOptions::default());

    let notes = "my grandma Rosa, lives in Naples, calls me tesoro, writes in short Italian messages";
    let (st, chat) = call(&app, "POST", "/api/chats", Some(json!({"persona": "  Rosa  ", "user": "Luca", "notes": notes}))).await;
    assert_eq!(st, StatusCode::OK, "{chat}");
    assert_eq!(chat["title"], "Rosa");
    let id = chat["id"].as_str().unwrap().to_string();
    let (_, state) = call(&app, "GET", "/api/state", None).await;
    let ex = &state["exports"][0];
    assert_eq!((ex["blank"].clone(), ex["messages"].clone()), (json!(true), json!(0)), "{ex}");

    // It chats like any other: a message, then the prompt the model would get.
    let (st, v) = call(&app, "POST", &format!("/api/chats/{id}/messages"), Some(json!({"text": "ciao nonna!"}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, p) = call(&app, "GET", &format!("/api/chats/{id}/prompt"), None).await;
    assert_eq!(st, StatusCode::OK, "{p}");
    let system = p["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains(notes) && system.contains("no earlier chat history"), "{system}");
    assert!(!system.contains("Style profile"), "no profile without messages: {system}");
    assert!(p.to_string().contains("ciao nonna!"), "{p}");

    // Bad input is refused, and nothing is left behind.
    for body in [json!({"persona": " "}), json!({"persona": "Rosa", "user": "Rosa"}), json!({"persona": "x".repeat(65)})] {
        let (st, v) = call(&app, "POST", "/api/chats", Some(body.clone())).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body} → {v}");
    }
    let (_, state) = call(&app, "GET", "/api/state", None).await;
    assert_eq!(state["exports"].as_array().unwrap().len(), 1);

    // Deleting the chat deletes its blank export (in memory and on disk).
    let (st, _) = call(&app, "DELETE", &format!("/api/chats/{id}"), None).await;
    assert_eq!(st, StatusCode::OK);
    let (_, state) = call(&app, "GET", "/api/state", None).await;
    assert!(state["exports"].as_array().unwrap().is_empty(), "{state}");
    assert!(std::fs::read_dir(tmp.path().join("exports")).unwrap().next().is_none());
}
