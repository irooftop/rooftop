//! Sharing beyond this device, end to end on localhost: the relay hub
//! (`<name>.<domain>` → the app over a WebSocket), the local network listener,
//! and the hub's shared Telegram bot (a mock Bot API).

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode, Uri, header};
use axum::routing::post;
use rooftop::llm::LlmConfig;
use rooftop::relay_hub::{Hub, HubConfig};
use rooftop::relay_tg::SharedBotConfig;
use rooftop::server::{self, ServerOptions};
use rooftop::state::AppState;
use rooftop::store::{Paths, TelegramMode};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tower::ServiceExt;

const DOMAIN: &str = "relay.test";
const GROUP: i64 = -1005550001;

async fn serve(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });
    format!("http://{addr}")
}

struct App {
    _tmp: tempfile::TempDir,
    state: Arc<AppState>,
    router: Router,
}

/// An app with the sample export and a share link `/s/joe`, pointed at the hub.
async fn make_app(hub_url: &str, llm_url: &str) -> Option<App> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/telegram");
    if !src.join("messages.html").exists() {
        eprintln!("examples/telegram missing, skipping");
        return None;
    }
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(tmp.path().to_path_buf());
    let mut db = paths.load().unwrap();
    db.settings.sharing.relay_url = hub_url.to_string();
    let llm = LlmConfig { base_url: format!("{llm_url}/v1"), api_key: String::new(), model: "mock".into(), temperature: None };
    let state = AppState::new(paths, db, llm);
    let (rec, _) = server::import_into_store(&state, src).await.unwrap();
    let chat = server::make_chat(&rec, "Jonas Weber", None, None, "owner notes").map_err(|_| ()).unwrap();
    let chat_id = chat.id.clone();
    state.paths.save_chat(&chat).unwrap();
    state.db.write().await.chats.insert(chat.id.clone(), chat);
    let router = server::router(state.clone(), ServerOptions::default());
    let (st, v) = call(&router, "POST", &format!("/api/chats/{chat_id}/shares"), Some(json!({"slug": "joe", "display_name": "Joe"}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    Some(App { _tmp: tmp, state, router })
}

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

/// A request to the hub as if it came for `host`.
async fn visit(
    hub_url: &str,
    host: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, String, reqwest::header::HeaderMap) {
    let http = reqwest::Client::new();
    let mut r = http.request(method.parse().unwrap(), format!("{hub_url}{path}")).header("host", host);
    if let Some(b) = body {
        r = r.json(&b);
    }
    let resp = r.send().await.unwrap();
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap();
    let headers = resp.headers().clone();
    (status, resp.text().await.unwrap(), headers)
}

/// A hub: a password-protected rooftop server with the relay for `relay.test`.
async fn hub(bot: Option<SharedBotConfig>) -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(tmp.path().to_path_buf());
    let db = paths.load().unwrap();
    let state = AppState::new(paths, db, LlmConfig::default());
    let hub = Hub::new(HubConfig { domain: DOMAIN.into(), data_dir: tmp.path().to_path_buf(), trust_proxy: false, max_accounts: 100, bot })
        .unwrap();
    hub.start();
    let router = server::router(state, ServerOptions { password: Some("owner-pw".into()), relay_hub: Some(hub), ..Default::default() });
    (tmp, serve(router).await)
}

async fn sharing(app: &App, patch: Value) -> Value {
    let (st, v) = call(&app.router, "PUT", "/api/sharing", Some(patch)).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    v
}

#[tokio::test]
async fn links_reach_visitors_through_the_relay_and_the_local_network() {
    let (_hub_dir, hub_url) = hub(None).await;
    let Some(app) = make_app(&hub_url, "http://127.0.0.1:9").await else { return };

    // Not the owner UI on this device: sharing can't be turned on.
    let (st, _) = call(&app.router, "PUT", "/api/sharing", Some(json!({"internet": true}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    rooftop::sharing::init(&app.state, "127.0.0.1:1".parse().unwrap()).await;
    let v = call(&app.router, "GET", "/api/sharing", None).await.1;
    assert_eq!(v["available"], true);
    assert_eq!(v["internet"]["enabled"], false);

    // Bad and reserved names never reach the hub.
    let (st, v) = call(&app.router, "PUT", "/api/sharing", Some(json!({"name": "www"}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");

    let v = sharing(&app, json!({"name": "alice", "internet": true})).await;
    assert_eq!(v["internet"]["name"], "alice");
    let mut status = v;
    for _ in 0..100 {
        if status["internet"]["state"] == "connected" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        status = call(&app.router, "GET", "/api/sharing", None).await.1;
    }
    assert_eq!(status["internet"]["state"], "connected", "{status}");
    assert_eq!(status["internet"]["url"], "https://alice.relay.test");
    let settings = call(&app.router, "GET", "/api/settings", None).await.1;
    assert!(!settings.to_string().contains(&app.state.db.read().await.settings.sharing.secret), "the secret stays on the server");

    // A visitor at alice.relay.test reaches the share, with no owner password.
    let host = "alice.relay.test";
    let (st, body, _) = visit(&hub_url, host, "GET", "/api/s/joe", None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["display_name"], "Joe");
    let (st, body, headers) = visit(&hub_url, host, "GET", "/s/joe", None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(body.contains("<html"), "the visitor page");
    assert_eq!(headers["x-frame-options"], "DENY");
    let (st, body, _) = visit(&hub_url, host, "POST", "/api/s/joe/sessions", Some(json!({"name": "Mia"}))).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let token = serde_json::from_str::<Value>(&body).unwrap()["token"].as_str().unwrap().to_string();
    let (st, body, _) = visit(&hub_url, host, "GET", &format!("/api/s/joe/c/{token}"), None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert!(!body.contains("owner notes"));
    // Assets of the visitor page are served too (gzip passes through untouched).
    let (st, body, _) = visit(&hub_url, host, "GET", "/app.js", None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(body.len() > 10_000);

    // The owner API of the app is never reachable through the relay.
    for path in ["/api/state", "/api/settings", "/api/sharing", "/media/x/y"] {
        let (st, _, _) = visit(&hub_url, host, "GET", path, None).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{path}");
    }
    // The hub's own owner UI still needs its password, and unknown names are 404.
    let (st, _, _) = visit(&hub_url, DOMAIN, "GET", "/api/state", None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _, _) = visit(&hub_url, "nobody.relay.test", "GET", "/s/joe", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Another app can't take the name.
    let Some(other) = make_app(&hub_url, "http://127.0.0.1:9").await else { return };
    rooftop::sharing::init(&other.state, "127.0.0.1:2".parse().unwrap()).await;
    let (st, v) = call(&other.router, "PUT", "/api/sharing", Some(json!({"name": "alice"}))).await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");

    // Renaming moves the links.
    sharing(&app, json!({"name": "alice2"})).await;
    for _ in 0..100 {
        if visit(&hub_url, "alice2.relay.test", "GET", "/api/s/joe", None).await.0 == StatusCode::OK {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(visit(&hub_url, "alice2.relay.test", "GET", "/api/s/joe", None).await.0, StatusCode::OK);
    assert_eq!(visit(&hub_url, host, "GET", "/api/s/joe", None).await.0, StatusCode::NOT_FOUND);

    // Turned off: the name stays, visitors see that the app is offline.
    sharing(&app, json!({"internet": false})).await;
    let mut st = StatusCode::OK;
    for _ in 0..100 {
        st = visit(&hub_url, "alice2.relay.test", "GET", "/api/s/joe", None).await.0;
        if st == StatusCode::SERVICE_UNAVAILABLE {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);

    // Local network: visitor routes on another port, the owner API isn't there.
    let v = sharing(&app, json!({"lan": true})).await;
    assert_eq!(v["lan"]["enabled"], true);
    let port = app.state.db.read().await.settings.sharing.lan_port;
    assert_ne!(port, 0);
    let lan = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::new();
    let r = http.get(format!("{lan}/api/s/joe")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    for path in ["/api/state", "/api/settings", "/api/sharing", "/api/qr?text=x"] {
        assert_eq!(http.get(format!("{lan}{path}")).send().await.unwrap().status(), 404, "{path}");
    }
    sharing(&app, json!({"lan": false})).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(http.get(format!("{lan}/api/s/joe")).send().await.is_err(), "the listener is closed");

    // QR codes for the owner.
    let req = Request::builder().uri("/api/qr?text=http%3A%2F%2F192.168.1.5%3A8737%2Fs%2Fjoe").body(Body::empty()).unwrap();
    let resp = app.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::CONTENT_TYPE], "image/svg+xml");
}

// ------------------------------------------------------------ shared bot

#[derive(Default)]
struct Tg {
    calls: Vec<(String, Value)>,
    updates: Vec<Value>,
    next_update: i64,
    next_msg: i64,
}

type TgState = Arc<Mutex<Tg>>;

impl Tg {
    fn push(&mut self, mut u: Value) {
        self.next_update += 1;
        u["update_id"] = json!(self.next_update);
        self.updates.push(u);
    }
}

async fn tg_api(State(tg): State<TgState>, uri: Uri, body: axum::body::Bytes) -> axum::Json<Value> {
    assert!(uri.path().starts_with("/botREAL:TOKEN/"), "only the hub talks to Telegram: {}", uri.path());
    let method = uri.path().rsplit('/').next().unwrap_or_default().to_string();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if method == "getUpdates" {
        let offset = body["offset"].as_i64().unwrap_or(0);
        for _ in 0..20 {
            let ready: Vec<Value> =
                tg.lock().unwrap().updates.iter().filter(|u| u["update_id"].as_i64().unwrap() >= offset).cloned().collect();
            if !ready.is_empty() {
                return axum::Json(json!({"ok": true, "result": ready}));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        return axum::Json(json!({"ok": true, "result": []}));
    }
    let mut t = tg.lock().unwrap();
    t.calls.push((method.clone(), body.clone()));
    let result = match method.as_str() {
        "getMe" => json!({"id": 777, "is_bot": true, "username": "shared_test_bot", "can_read_all_group_messages": false}),
        "sendMessage" => {
            t.next_msg += 1;
            json!({"message_id": t.next_msg, "chat": {"id": body["chat_id"]}, "text": body["text"]})
        }
        _ => json!(true),
    };
    axum::Json(json!({"ok": true, "result": result}))
}

fn sent_to(tg: &TgState, chat: i64) -> Vec<String> {
    tg.lock()
        .unwrap()
        .calls
        .iter()
        .filter(|(m, b)| m == "sendMessage" && b["chat_id"] == json!(chat))
        .map(|(_, b)| b["text"].as_str().unwrap_or_default().to_string())
        .collect()
}

async fn eventually(what: &str, mut f: impl AsyncFnMut() -> bool) {
    for _ in 0..200 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn shared_bot_links_a_group_to_one_app_only() {
    let tg: TgState = Default::default();
    let tg_url = serve(Router::new().fallback(post(tg_api)).with_state(tg.clone())).await;
    let (_hub_dir, hub_url) = hub(Some(SharedBotConfig { token: "REAL:TOKEN".into(), api_base: tg_url })).await;
    let Some(app) = make_app(&hub_url, "http://127.0.0.1:9").await else { return };

    // Settings → Telegram → Shared bot: the app gets a relay account and connects through the hub.
    let (st, v) = call(&app.router, "PUT", "/api/settings", Some(json!({"telegram": {"mode": "shared"}}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(app.state.db.read().await.settings.telegram.mode, TelegramMode::Shared);
    eventually("the shared bot", async || call(&app.router, "GET", "/api/telegram", None).await.1["username"] == "shared_test_bot").await;
    let status = call(&app.router, "GET", "/api/telegram", None).await.1;
    assert_eq!(status["source"], "shared");

    // Add to a group: a one-time code in a t.me link.
    let (st, link) = call(&app.router, "POST", "/api/telegram/link", None).await;
    assert_eq!(st, StatusCode::OK, "{link}");
    let url = link["url"].as_str().unwrap();
    let code = url.strip_prefix("https://t.me/shared_test_bot?startgroup=").expect(url).to_string();

    let group = json!({"id": GROUP, "type": "supergroup", "title": "Friends"});
    // A wrong code doesn't link the group.
    tg.lock().unwrap().push(
        json!({"message": {"message_id": 1, "chat": group, "from": {"id": 5, "is_bot": false}, "text": "/start@shared_test_bot nope"}}),
    );
    eventually("the expired-code notice", async || sent_to(&tg, GROUP).iter().any(|t| t.contains("expired"))).await;
    tg.lock().unwrap().push(json!({"message": {"message_id": 2, "chat": group, "from": {"id": 5, "is_bot": false}, "text": format!("/start@shared_test_bot {code}")}}));
    eventually("the group in the app", async || {
        call(&app.router, "GET", "/api/telegram", None).await.1["groups"] == json!([{"id": GROUP, "title": "Friends"}])
    })
    .await;
    // The app greets the group through the hub.
    eventually("the greeting", async || sent_to(&tg, GROUP).iter().any(|t| t.starts_with("👋 Ready"))).await;

    // Visitor messages are relayed to the group with the buttons.
    let template = app.state.db.read().await.chats.keys().next().unwrap().clone();
    let (st, share) = call(
        &app.router,
        "POST",
        &format!("/api/chats/{template}/shares"),
        Some(json!({"slug": "tgjoe", "display_name": "Joe", "telegram_chat_id": GROUP, "ai_delay_secs": 0})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{share}");
    let (_, s) = call(&app.router, "POST", "/api/s/tgjoe/sessions", Some(json!({"name": "Mia"}))).await;
    let token = s["token"].as_str().unwrap();
    let (st, _) = call(&app.router, "POST", &format!("/api/s/tgjoe/c/{token}/messages"), Some(json!({"text": "hello there"}))).await;
    assert_eq!(st, StatusCode::OK);
    eventually("the relayed message", async || sent_to(&tg, GROUP).iter().any(|t| t.contains("hello there"))).await;

    // A button press in the group reaches the app, and the app may answer it.
    let ai_button = {
        let t = tg.lock().unwrap();
        let (_, b) = t.calls.iter().rev().find(|(m, b)| m == "sendMessage" && b["reply_markup"]["inline_keyboard"].is_array()).unwrap();
        b["reply_markup"]["inline_keyboard"][0][1]["callback_data"].as_str().unwrap().to_string()
    };
    tg.lock().unwrap().push(json!({"callback_query": {
        "id": "cb-1", "from": {"id": 5, "first_name": "Owner"}, "data": ai_button,
        "message": {"message_id": 3, "chat": group},
    }}));
    eventually("the answered button", async || {
        tg.lock().unwrap().calls.iter().any(|(m, b)| m == "answerCallbackQuery" && b["callback_query_id"] == "cb-1")
    })
    .await;

    // Another app of the same hub can't use that group, nor answer its buttons.
    let secret = "MalloryMalloryMalloryMalloryMallory0123".to_string();
    let http = reqwest::Client::new();
    let r: Value = http
        .post(format!("{hub_url}/relay/v1/register"))
        .bearer_auth(&secret)
        .json(&json!({"name": "mallory"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["url"], "https://mallory.relay.test");
    let tg_proxy = format!("{hub_url}/relay/v1/tg/bot{secret}");
    let r = http.post(format!("{tg_proxy}/sendMessage")).json(&json!({"chat_id": GROUP, "text": "phish"})).send().await.unwrap();
    assert_eq!(r.status(), 403);
    let r = http.post(format!("{tg_proxy}/answerCallbackQuery")).json(&json!({"callback_query_id": "x"})).send().await.unwrap();
    assert_eq!(r.status(), 403);
    let r = http.post(format!("{tg_proxy}/deleteWebhook")).json(&json!({})).send().await.unwrap();
    assert_eq!(r.status(), 403, "only the relay's methods");
    let r = http.post(format!("{hub_url}/relay/v1/tg/botWRONGSECRETWRONGSECRETWRONGSECRET1/getMe")).send().await.unwrap();
    assert_eq!(r.status(), 401);
    assert!(sent_to(&tg, GROUP).iter().all(|t| t != "phish"));
}
