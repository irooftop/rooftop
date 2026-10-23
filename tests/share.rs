//! Share links against the sample export: isolation of visitors, PIN
//! throttling, media allow-list, rooms and the owner's view.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use rooftop::llm::LlmConfig;
use rooftop::server::{self, ServerOptions};
use rooftop::state::AppState;
use rooftop::store::Paths;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use tower::ServiceExt;

const DIR: &str = "examples/telegram";

struct Env {
    _tmp: tempfile::TempDir,
    state: Arc<AppState>,
    app: Router,
    chat_id: String,
}

async fn setup(password: Option<&str>) -> Option<Env> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join(DIR);
    if !src.join("messages.html").exists() {
        eprintln!("examples/telegram missing, skipping");
        return None;
    }
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(tmp.path().to_path_buf());
    let db = paths.load().unwrap();
    // Unreachable endpoint: these tests never ask the persona to answer.
    let llm = LlmConfig { base_url: "http://127.0.0.1:9/v1".into(), api_key: String::new(), model: "test".into(), temperature: None };
    let state = AppState::new(paths, db, llm);
    let (rec, _) = server::import_into_store(&state, src).await.unwrap();
    let chat = server::make_chat(&rec, "Jonas Weber", None, None, "secret owner notes").map_err(|_| ()).unwrap();
    let chat_id = chat.id.clone();
    state.paths.save_chat(&chat).unwrap();
    state.db.write().await.chats.insert(chat.id.clone(), chat);
    let app = server::router(state.clone(), ServerOptions { password: password.map(String::from), ..Default::default() });
    Some(Env { _tmp: tmp, state, app, chat_id })
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>, auth: Option<&str>) -> (StatusCode, Value, String) {
    let mut b = Request::builder().method(method).uri(uri);
    if body.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    if let Some(pw) = auth {
        use base64::Engine;
        b = b.header(header::AUTHORIZATION, format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("u:{pw}"))));
    }
    let req = b.body(body.map_or_else(Body::empty, |v| Body::from(v.to_string()))).unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8_lossy(&bytes).to_string();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null), text)
}

async fn create_share(env: &Env, body: Value) -> Value {
    let (st, v, text) = call(&env.app, "POST", &format!("/api/chats/{}/shares", env.chat_id), Some(body), None).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    v
}

async fn start(env: &Env, slug: &str, body: Value) -> (StatusCode, Value) {
    let (st, v, _) = call(&env.app, "POST", &format!("/api/s/{slug}/sessions"), Some(body), None).await;
    (st, v)
}

#[tokio::test]
async fn visitor_sees_only_own_conversation_without_private_data() {
    let Some(env) = setup(None).await else { return };
    let share = create_share(&env, json!({"slug": "Joe", "display_name": "Joe"})).await;
    assert_eq!(share["slug"], "joe");
    assert_eq!(share["has_pin"], false);

    // Landing info: no real names, no notes.
    let (st, info, text) = call(&env.app, "GET", "/api/s/JOE", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(info["display_name"], "Joe");
    assert_eq!(info["pin"], false);
    assert!(!text.contains("Jonas Weber") && !text.contains("Priya Nair"), "{text}");

    let (st, a) = start(&env, "joe", json!({"name": "Bob"})).await;
    assert_eq!(st, StatusCode::OK);
    let ta = a["token"].as_str().unwrap().to_string();
    let (_, b) = start(&env, "joe", json!({})).await;
    let tb = b["token"].as_str().unwrap().to_string();
    assert_ne!(ta, tb);

    // History hidden: nothing from the export, persona under its display name.
    let (st, view, text) = call(&env.app, "GET", &format!("/api/s/joe/c/{ta}"), None, None).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    assert_eq!(view["messages"].as_array().unwrap().len(), 0);
    assert_eq!(view["chat"]["persona"], "Joe");
    for leak in ["Jonas Weber", "Priya Nair", "secret owner notes", "description"] {
        assert!(!text.contains(leak), "visitor view leaks `{leak}`: {text}");
    }
    assert!(!view["library"].as_array().unwrap().is_empty(), "stickers for the picker");

    // A visitor message is visible to that visitor only.
    let (st, m, text) = call(&env.app, "POST", &format!("/api/s/joe/c/{ta}/messages"), Some(json!({"text": "hi from bob"})), None).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    assert_eq!(m["messages"][0]["mine"], true);
    let (_, va, _) = call(&env.app, "GET", &format!("/api/s/joe/c/{ta}"), None, None).await;
    let (_, vb, _) = call(&env.app, "GET", &format!("/api/s/joe/c/{tb}"), None, None).await;
    assert_eq!(va["messages"].as_array().unwrap().len(), 1);
    assert_eq!(vb["messages"].as_array().unwrap().len(), 0);

    // Reacting to an invisible export message is refused.
    let (st, _, _) =
        call(&env.app, "POST", &format!("/api/s/joe/c/{tb}/react"), Some(json!({"message_id": 52122, "emoji": "🔥"})), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Media allow-list: stickers yes, export photos no (history hidden).
    let sticker = view["library"][0]["file"].as_str().unwrap();
    let (st, _, _) = call(&env.app, "GET", &format!("/api/s/joe/c/{ta}/media/{sticker}"), None, None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _, _) = call(&env.app, "GET", &format!("/api/s/joe/c/{ta}/media/photos/photo_52122_0.jpg"), None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _, _) = call(&env.app, "GET", &format!("/api/s/joe/c/{ta}/media/../export.json"), None, None).await;
    assert!(st == StatusCode::BAD_REQUEST || st == StatusCode::NOT_FOUND, "{st}");

    // Unknown tokens are rejected.
    let (st, _, _) = call(&env.app, "GET", "/api/s/joe/c/not-a-token", None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Owner: visitor chats are hidden from the list but listed under the share.
    let (_, state, _) = call(&env.app, "GET", "/api/state", None, None).await;
    assert_eq!(state["chats"].as_array().unwrap().len(), 1);
    assert_eq!(state["shares"][0]["sessions"].as_array().unwrap().len(), 2);
    let (_, list, _) = call(&env.app, "GET", &format!("/api/chats/{}/shares", env.chat_id), None, None).await;
    let sessions = list["shares"][0]["sessions"].as_array().unwrap();
    let bob = sessions.iter().find(|s| s["name"] == "Bob").unwrap();
    assert_eq!(bob["messages"], 1);
    assert!(!list.to_string().contains(&ta), "owner listing must not expose visitor tokens");
    // The owner can read the visitor conversation.
    let (st, owner_view, _) = call(&env.app, "GET", &format!("/api/chats/{}", bob["chat_id"].as_str().unwrap()), None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(owner_view.to_string().contains("hi from bob"));
    assert_eq!(owner_view["notes"], "secret owner notes", "visitor chats inherit the persona notes");

    // Ending a session revokes its token.
    let (st, _, _) = call(
        &env.app,
        "DELETE",
        &format!("/api/shares/{}/sessions/{}", share["id"].as_str().unwrap(), bob["id"].as_str().unwrap()),
        None,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _, _) = call(&env.app, "GET", &format!("/api/s/joe/c/{ta}"), None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Disabled link: gone for everyone.
    let (st, _, _) =
        call(&env.app, "PATCH", &format!("/api/shares/{}", share["id"].as_str().unwrap()), Some(json!({"enabled": false})), None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _, _) = call(&env.app, "GET", &format!("/api/s/joe/c/{tb}"), None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _, _) = call(&env.app, "GET", "/api/s/joe", None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Deleting the template chat removes the share and its visitor chats.
    let (st, _, _) = call(&env.app, "DELETE", &format!("/api/chats/{}", env.chat_id), None, None).await;
    assert_eq!(st, StatusCode::OK);
    let db = env.state.db.read().await;
    assert!(db.shares.is_empty());
    assert!(db.chats.is_empty());
}

#[tokio::test]
async fn pin_is_required_and_brute_force_is_throttled() {
    let Some(env) = setup(None).await else { return };
    let share = create_share(&env, json!({"slug": "pin-test", "pin": "482913"})).await;
    assert_eq!(share["has_pin"], true);
    let raw = std::fs::read_to_string(env.state.paths.share(share["id"].as_str().unwrap())).unwrap();
    assert!(!raw.contains("482913"), "the PIN is stored hashed");

    let (_, info, _) = call(&env.app, "GET", "/api/s/pin-test", None, None).await;
    assert_eq!(info["pin"], true);
    assert_eq!(info["pin_numeric"], true);

    let (st, _) = start(&env, "pin-test", json!({})).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    for _ in 0..3 {
        let (st, _) = start(&env, "pin-test", json!({"pin": "000000"})).await;
        assert_eq!(st, StatusCode::FORBIDDEN);
    }
    // A correct PIN before the lockout works and resets the per-client counter.
    let (st, v) = start(&env, "pin-test", json!({"pin": "482913"})).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    for i in 0..5 {
        let (st, _) = start(&env, "pin-test", json!({"pin": "111111"})).await;
        assert_eq!(st, if i < 4 { StatusCode::FORBIDDEN } else { StatusCode::TOO_MANY_REQUESTS });
    }
    // Locked: even the right PIN is refused now.
    let (st, v) = start(&env, "pin-test", json!({"pin": "482913"})).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    assert!(v["error"].as_str().unwrap().contains("Try again"));
}

#[tokio::test]
async fn room_visitors_share_one_conversation() {
    let Some(env) = setup(None).await else { return };
    create_share(&env, json!({"slug": "room", "mode": "room", "history": "recent", "history_recent": 5})).await;
    let (_, a) = start(&env, "room", json!({"name": "Ann"})).await;
    let (_, b) = start(&env, "room", json!({"name": "Ben"})).await;
    let (ta, tb) = (a["token"].as_str().unwrap(), b["token"].as_str().unwrap());
    let (st, _, text) = call(&env.app, "POST", &format!("/api/s/room/c/{ta}/messages"), Some(json!({"text": "ann here"})), None).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    let (_, vb, _) = call(&env.app, "GET", &format!("/api/s/room/c/{tb}"), None, None).await;
    let msgs = vb["messages"].as_array().unwrap();
    let ann = msgs.iter().find(|m| m["text"] == "ann here").expect("Ben sees Ann's message");
    assert_eq!(ann["mine"], false);
    assert_eq!(ann["from"], "Ann");
    // Recent history: 5 export messages plus the live one.
    assert_eq!(msgs.iter().filter(|m| m["service"] != true).count(), 6);
    let (_, va, _) = call(&env.app, "GET", &format!("/api/s/room/c/{ta}"), None, None).await;
    assert_eq!(va["messages"].as_array().unwrap().iter().find(|m| m["text"] == "ann here").unwrap()["mine"], true);
    assert_eq!(env.state.db.read().await.chats.values().filter(|c| c.guest.is_some()).count(), 1);
}

#[tokio::test]
async fn share_links_bypass_owner_auth_but_nothing_else_does() {
    let Some(env) = setup(Some("owner-pw")).await else { return };
    let (st, _, _) = call(&env.app, "GET", "/api/state", None, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, v, text) =
        call(&env.app, "POST", &format!("/api/chats/{}/shares", env.chat_id), Some(json!({"slug": "pub"})), Some("owner-pw")).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    assert_eq!(v["slug"], "pub");
    for uri in ["/s/pub", "/app.js", "/app.css", "/api/s/pub"] {
        let (st, _, _) = call(&env.app, "GET", uri, None, None).await;
        assert_eq!(st, StatusCode::OK, "{uri}");
    }
    let (st, s) = start(&env, "pub", json!({})).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _, _) = call(&env.app, "GET", &format!("/api/s/pub/c/{}", s["token"].as_str().unwrap()), None, None).await;
    assert_eq!(st, StatusCode::OK);
    for uri in ["/", "/api/settings", &format!("/api/chats/{}/prompt", env.chat_id), "/media/x/y"] {
        let (st, _, _) = call(&env.app, "GET", uri, None, None).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{uri}");
    }
}

#[tokio::test]
async fn slugs_are_validated_and_unique() {
    let Some(env) = setup(None).await else { return };
    let auto = create_share(&env, json!({})).await;
    assert_eq!(auto["slug"].as_str().unwrap().len(), 10);
    create_share(&env, json!({"slug": "taken"})).await;
    let url = format!("/api/chats/{}/shares", env.chat_id);
    let (st, _, _) = call(&env.app, "POST", &url, Some(json!({"slug": "Taken"})), None).await;
    assert_eq!(st, StatusCode::CONFLICT);
    for bad in ["a", "api", "with space", "../x"] {
        let (st, _, _) = call(&env.app, "POST", &url, Some(json!({"slug": bad})), None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (st, _, _) = call(&env.app, "POST", &url, Some(json!({"pin": "12"})), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _, _) = call(&env.app, "POST", &url, Some(json!({"expires_at": "2000-01-01T00:00"})), None).await;
    assert_eq!(st, StatusCode::OK);
    let expired = env.state.db.read().await.shares.values().find(|s| s.expires_at.is_some()).unwrap().slug.clone();
    let (st, _, _) = call(&env.app, "GET", &format!("/api/s/{expired}"), None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn favicons_are_public() {
    let Some(env) = setup(Some("pw")).await else { return };
    for (uri, ctype, magic) in [("/favicon.svg", "image/svg+xml", &b"<svg"[..]), ("/favicon.ico", "image/x-icon", &[0, 0, 1, 0][..])] {
        let resp = env.app.clone().oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{uri} without a password");
        assert_eq!(resp.headers()[header::CONTENT_TYPE], ctype);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert!(bytes.starts_with(magic), "{uri} body");
    }
    let (_, _, html) = call(&env.app, "GET", "/", None, Some("pw")).await;
    assert!(html.contains(r#"href="/favicon.svg""#) && html.contains(r#"href="/favicon.ico""#));
}

/// "Let them write first" and "Rewrite the last answer" are off unless the owner allows them:
/// the buttons are hidden and the server refuses them, while answering a message still works.
#[tokio::test]
async fn nudge_and_regenerate_are_opt_in() {
    let Some(env) = setup(None).await else { return };
    let share = create_share(&env, json!({"slug": "opt", "display_name": "Jonas"})).await;
    assert_eq!(share["allow_nudge"], false);
    assert_eq!(share["allow_regenerate"], false);
    let (_, s) = start(&env, "opt", json!({})).await;
    let t = s["token"].as_str().unwrap().to_string();
    let (_, chat, _) = call(&env.app, "GET", &format!("/api/s/opt/c/{t}"), None, None).await;
    assert_eq!(chat["share"]["nudge"], false);
    assert_eq!(chat["share"]["regenerate"], false);
    let turn = |path: &'static str| {
        let (app, t) = (env.app.clone(), t.clone());
        async move { call(&app, "POST", &format!("/api/s/opt/c/{t}/{path}"), None, None).await.0 }
    };
    assert_eq!(turn("respond").await, StatusCode::FORBIDDEN, "no visitor message yet: that's a nudge");
    assert_eq!(turn("regenerate").await, StatusCode::FORBIDDEN);
    let (st, _, text) = call(&env.app, "POST", &format!("/api/s/opt/c/{t}/messages"), Some(json!({"text": "hi"})), None).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    assert_ne!(turn("respond").await, StatusCode::FORBIDDEN, "answering a message is always allowed");

    let (st, v, text) = call(
        &env.app,
        "PATCH",
        &format!("/api/shares/{}", share["id"].as_str().unwrap()),
        Some(json!({"allow_nudge": true, "allow_regenerate": true})),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{text}");
    assert_eq!(v["allow_nudge"], true, "{v}");
    let (_, s2) = start(&env, "opt", json!({})).await;
    let t2 = s2["token"].as_str().unwrap();
    let (_, chat, _) = call(&env.app, "GET", &format!("/api/s/opt/c/{t2}"), None, None).await;
    assert_eq!((chat["share"]["nudge"].clone(), chat["share"]["regenerate"].clone()), (json!(true), json!(true)));
    let (st, _, _) = call(&env.app, "POST", &format!("/api/s/opt/c/{t2}/respond"), None, None).await;
    assert_ne!(st, StatusCode::FORBIDDEN);
}

async fn guest_prompt(env: &Env, chat_id: &str) -> String {
    let (st, p, text) = call(&env.app, "GET", &format!("/api/chats/{chat_id}/prompt"), None, None).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    p.to_string()
}

/// The persona talks to the visitor under the visitor's name, and knows that the export
/// history and the notes are about the owner, who is someone else.
#[tokio::test]
async fn visitors_are_themselves_not_the_owner() {
    let Some(env) = setup(None).await else { return };
    // The template chat: the AI is Jonas Weber, the owner plays Priya Nair.
    let share = create_share(&env, json!({"slug": "who", "display_name": "Jonas", "history": "full"})).await;
    let (_, s) = start(&env, "who", json!({"name": "Mia"})).await;
    let t = s["token"].as_str().unwrap().to_string();
    let (st, _, text) = call(&env.app, "POST", &format!("/api/s/who/c/{t}/messages"), Some(json!({"text": "hi, I'm Mia"})), None).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    let (_, list, _) = call(&env.app, "GET", &format!("/api/chats/{}/shares", env.chat_id), None, None).await;
    let chat_id = list["shares"][0]["sessions"][0]["chat_id"].as_str().unwrap().to_string();
    let prompt = guest_prompt(&env, &chat_id).await;
    assert!(prompt.contains("Mia opened a link that Priya Nair shared"), "{prompt}");
    assert!(prompt.contains("Mia is not Priya Nair"), "{prompt}");
    assert!(!prompt.contains("continuing a real Telegram conversation with"), "the owner framing is for the owner's chat: {prompt}");
    assert!(prompt.contains("Priya Nair 21:") || prompt.contains("Priya Nair 2"), "export lines carry the host's name: {prompt}");
    assert!(prompt.contains(" Mia ") && prompt.contains("hi, I'm Mia"), "{prompt}");
    // The visitor sees the owner's export messages as someone else's, with names on the bubbles.
    let (_, chat, _) = call(&env.app, "GET", &format!("/api/s/who/c/{t}"), None, None).await;
    assert_eq!(chat["export"]["multi"], true);
    let msgs = chat["messages"].as_array().unwrap();
    assert!(msgs.iter().any(|m| m["from"] == "Priya Nair" && m["mine"] == false), "the host's messages aren't the visitor's");
    assert!(msgs.iter().any(|m| m["text"] == "hi, I'm Mia" && m["mine"] == true));

    // A visitor who calls themselves like the owner is told apart.
    let (_, s2) = start(&env, "who", json!({"name": "priya nair"})).await;
    let (_, list, _) = call(&env.app, "GET", &format!("/api/chats/{}/shares", env.chat_id), None, None).await;
    assert_eq!(list["shares"][0]["id"], share["id"]);
    let names: Vec<&str> = list["shares"][0]["sessions"].as_array().unwrap().iter().filter_map(|g| g["name"].as_str()).collect();
    assert!(names.contains(&"priya nair (guest)"), "{list}");
    assert!(s2["token"].is_string());
}

/// In a room everyone writes and reacts under their own name; each sees their own as theirs.
#[tokio::test]
async fn room_visitors_keep_their_names() {
    let Some(env) = setup(None).await else { return };
    create_share(&env, json!({"slug": "club", "display_name": "Jonas", "mode": "room"})).await;
    let (_, a) = start(&env, "club", json!({"name": "Ann"})).await;
    let (_, b) = start(&env, "club", json!({"name": "Ann"})).await; // same name: kept apart
    let (ta, tb) = (a["token"].as_str().unwrap().to_string(), b["token"].as_str().unwrap().to_string());
    let (_, ma, _) = call(&env.app, "POST", &format!("/api/s/club/c/{ta}/messages"), Some(json!({"text": "hello from ann"})), None).await;
    let (_, mb, _) =
        call(&env.app, "POST", &format!("/api/s/club/c/{tb}/messages"), Some(json!({"text": "and from the other ann"})), None).await;
    let bob_msg = mb["messages"][0]["id"].as_i64().unwrap();
    let (st, _, text) =
        call(&env.app, "POST", &format!("/api/s/club/c/{ta}/react"), Some(json!({"message_id": bob_msg, "emoji": "👍"})), None).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    assert!(ma["messages"][0]["id"].is_i64());

    let (_, va, _) = call(&env.app, "GET", &format!("/api/s/club/c/{ta}"), None, None).await;
    let (_, vb, _) = call(&env.app, "GET", &format!("/api/s/club/c/{tb}"), None, None).await;
    let find = |v: &Value, text: &str| v["messages"].as_array().unwrap().iter().find(|m| m["text"] == text).cloned().unwrap();
    assert_eq!(find(&va, "hello from ann")["mine"], true);
    let other = find(&va, "and from the other ann");
    assert_eq!((other["mine"].clone(), other["from"].clone()), (json!(false), json!("Ann 2")), "{other}");
    assert_eq!(other["reactions"][0]["mine"], true, "Ann's own reaction: {other}");
    let theirs = find(&vb, "and from the other ann");
    assert_eq!((theirs["mine"].clone(), theirs["reactions"][0]["mine"].clone()), (json!(true), json!(false)), "{theirs}");

    let (_, list, _) = call(&env.app, "GET", &format!("/api/chats/{}/shares", env.chat_id), None, None).await;
    let room = list["shares"][0]["sessions"][0]["chat_id"].as_str().unwrap().to_string();
    let prompt = guest_prompt(&env, &room).await;
    assert!(prompt.contains("group chat with people who opened a link"), "{prompt}");
    assert!(prompt.contains(" Ann ") && prompt.contains(" Ann 2 "), "each line names its sender: {prompt}");
    assert!(prompt.contains("Ann 2 reacted 👍") || prompt.contains("Ann reacted 👍"), "{prompt}");
}

/// Like `call`, from the client address `ip`.
async fn call_from(app: &Router, ip: &str, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(uri);
    if body.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    let mut req = b.body(body.map_or_else(Body::empty, |v| Body::from(v.to_string()))).unwrap();
    let addr = std::net::SocketAddr::new(ip.parse().unwrap(), 50000);
    req.extensions_mut().insert(axum::extract::ConnectInfo(addr));
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// A visitor's ❤ on the persona's message is something to answer, even where "write
/// first" is off; nothing at all is not.
#[tokio::test]
async fn a_reaction_may_get_an_answer() {
    let Some(env) = setup(None).await else { return };
    create_share(&env, json!({"slug": "heart", "display_name": "Jonas", "history": "full"})).await;
    let (_, s) = start(&env, "heart", json!({"name": "Mia"})).await;
    let t = s["token"].as_str().unwrap().to_string();
    let respond_uri = format!("/api/s/heart/c/{t}/respond");
    let respond = || call(&env.app, "POST", &respond_uri, None, None);
    assert_eq!(respond().await.0, StatusCode::FORBIDDEN, "nothing to answer yet");
    let (_, chat, _) = call(&env.app, "GET", &format!("/api/s/heart/c/{t}"), None, None).await;
    let persona_msg = chat["messages"].as_array().unwrap().iter().rev().find(|m| m["from"] == "Jonas").unwrap()["id"].as_i64().unwrap();
    let (st, _, text) =
        call(&env.app, "POST", &format!("/api/s/heart/c/{t}/react"), Some(json!({"message_id": persona_msg, "emoji": "❤️"})), None).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    assert_ne!(respond().await.0, StatusCode::FORBIDDEN, "a reaction may be answered");
}

/// The owner sees where visitors write from, and can end a conversation and block that
/// address: it can't open the link, start or continue a conversation, until unblocked.
#[tokio::test]
async fn visitors_can_be_blocked_by_address() {
    let Some(env) = setup(None).await else { return };
    let share = create_share(&env, json!({"slug": "door", "display_name": "Jonas"})).await;
    let sid = share["id"].as_str().unwrap().to_string();
    let (bad_ip, good_ip) = ("203.0.113.7", "198.51.100.2");
    let (st, s) = call_from(&env.app, bad_ip, "POST", "/api/s/door/sessions", Some(json!({"name": "Bob"}))).await;
    assert_eq!(st, StatusCode::OK, "{s}");
    let (_, s2) = call_from(&env.app, bad_ip, "POST", "/api/s/door/sessions", Some(json!({"name": "Bob again"}))).await;
    let (_, keep) = call_from(&env.app, good_ip, "POST", "/api/s/door/sessions", Some(json!({"name": "Ann"}))).await;
    let (_, list, _) = call(&env.app, "GET", &format!("/api/chats/{}/shares", env.chat_id), None, None).await;
    let sessions = list["shares"][0]["sessions"].as_array().unwrap().clone();
    let bob = sessions.iter().find(|g| g["name"] == "Bob").unwrap();
    assert_eq!(bob["ip"], bad_ip);

    let (st, v, text) =
        call(&env.app, "DELETE", &format!("/api/shares/{sid}/sessions/{}?block=true", bob["id"].as_str().unwrap()), None, None).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    assert_eq!(v["blocked"][0]["ip"], bad_ip);
    let names: Vec<&str> = v["sessions"].as_array().unwrap().iter().filter_map(|g| g["name"].as_str()).collect();
    assert_eq!(names, ["Ann"], "everyone from the blocked address is gone");

    for (method, uri) in [("GET", "/api/s/door".to_string()), ("POST", "/api/s/door/sessions".to_string())] {
        let (st, v) = call_from(&env.app, bad_ip, method, &uri, (method == "POST").then(|| json!({}))).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{method} {uri}: {v}");
    }
    let t2 = s2["token"].as_str().unwrap();
    let (st, _) = call_from(&env.app, bad_ip, "GET", &format!("/api/s/door/c/{t2}"), None).await;
    assert_ne!(st, StatusCode::OK, "the blocked address can't continue either");
    let (st, _) = call_from(&env.app, good_ip, "GET", &format!("/api/s/door/c/{}", keep["token"].as_str().unwrap()), None).await;
    assert_eq!(st, StatusCode::OK, "others carry on");

    let (st, v, text) = call(&env.app, "PATCH", &format!("/api/shares/{sid}"), Some(json!({"unblock": [bad_ip]})), None).await;
    assert_eq!(st, StatusCode::OK, "{text}");
    assert!(v["blocked"].as_array().unwrap().is_empty(), "{v}");
    let (st, _) = call_from(&env.app, bad_ip, "POST", "/api/s/door/sessions", Some(json!({}))).await;
    assert_eq!(st, StatusCode::OK);
    assert!(s["token"].is_string());
}
