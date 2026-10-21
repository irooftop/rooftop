//! Telegram relay end to end, without network: a mock Bot API (long polling,
//! buttons, ForceReply) and a mock OpenAI-compatible LLM.

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode, Uri, header};
use axum::routing::post;
use rooftop::llm::LlmConfig;
use rooftop::server::{self, ServerOptions};
use rooftop::state::AppState;
use rooftop::store::Paths;
use rooftop::tgbot::{self, BotConfig};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tower::ServiceExt;

const GROUP: i64 = -1001234567890;
const OTHER_GROUP: i64 = -1009999999999;
const BOT_ID: i64 = 4242;

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
    let method = uri.path().rsplit('/').next().unwrap_or_default().to_string();
    assert!(uri.path().starts_with("/botTEST:TOKEN/"), "token in path: {}", uri.path());
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if method == "getUpdates" {
        let offset = body["offset"].as_i64().unwrap_or(0);
        for _ in 0..20 {
            let ready: Vec<Value> = {
                let t = tg.lock().unwrap();
                t.updates.iter().filter(|u| u["update_id"].as_i64().unwrap() >= offset).cloned().collect()
            };
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
        "getMe" => json!({"id": BOT_ID, "is_bot": true, "username": "rooftop_test_bot", "can_read_all_group_messages": false}),
        "getChat" => json!({"id": body["chat_id"], "type": "supergroup", "title": "Owners"}),
        "sendMessage" => {
            t.next_msg += 1;
            json!({"message_id": t.next_msg, "chat": {"id": body["chat_id"]}, "text": body["text"]})
        }
        _ => json!(true),
    };
    axum::Json(json!({"ok": true, "result": result}))
}

async fn llm(axum::Json(_): axum::Json<Value>) -> axum::Json<Value> {
    axum::Json(json!({"choices": [{"message": {"role": "assistant", "content": "ai says hi"}, "finish_reason": "stop"}]}))
}

async fn serve(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
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

async fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

fn calls(tg: &TgState, method: &str) -> Vec<Value> {
    tg.lock().unwrap().calls.iter().filter(|(m, _)| m == method).map(|(_, b)| b.clone()).collect()
}

async fn persona_texts(state: &AppState, chat_id: &str) -> Vec<String> {
    let db = state.db.read().await;
    let chat = &db.chats[chat_id];
    chat.events
        .iter()
        .filter_map(|e| match e {
            rooftop::chat::Event::Message(m) if m.from == chat.persona => Some(m.text.clone()),
            _ => None,
        })
        .collect()
}

async fn persona_count(state: &AppState, chat_id: &str) -> usize {
    persona_texts(state, chat_id).await.len()
}

/// The last relayed visitor message: its Telegram id and button callback data.
fn last_relay(tg: &TgState) -> (i64, String, String, String) {
    let t = tg.lock().unwrap();
    let mut id = 0;
    let mut found = None;
    for (m, b) in &t.calls {
        if m == "sendMessage" {
            id += 1;
            if b["reply_markup"]["inline_keyboard"].is_array() {
                let row = &b["reply_markup"]["inline_keyboard"][0];
                found = Some((
                    id,
                    row[0]["callback_data"].as_str().unwrap().to_string(),
                    row[1]["callback_data"].as_str().unwrap().to_string(),
                    row[1]["text"].as_str().unwrap().to_string(),
                ));
            }
        }
    }
    found.expect("a relayed message with buttons")
}

#[tokio::test]
async fn visitor_messages_relay_to_telegram_with_reply_and_ai_buttons() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/telegram");
    if !src.join("messages.html").exists() {
        eprintln!("examples/telegram missing, skipping");
        return;
    }
    let tg: TgState = Default::default();
    let tg_url = serve(Router::new().fallback(post(tg_api)).with_state(tg.clone())).await;
    let llm_url = serve(Router::new().route("/v1/chat/completions", post(llm))).await;

    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(tmp.path().to_path_buf());
    let db = paths.load().unwrap();
    let cfg = LlmConfig { base_url: format!("{llm_url}/v1"), api_key: String::new(), model: "mock".into(), temperature: None };
    let state = AppState::new(paths, db, cfg);
    let (rec, _) = server::import_into_store(&state, src).await.unwrap();
    let chat = server::make_chat(&rec, "Jonas Weber", None, None, "").map_err(|_| ()).unwrap();
    let template = chat.id.clone();
    state.paths.save_chat(&chat).unwrap();
    state.db.write().await.chats.insert(chat.id.clone(), chat);
    let app = server::router(state.clone(), ServerOptions::default());

    // Without a bot, a share can't be linked to Telegram.
    let (st, _) = call(&app, "POST", &format!("/api/chats/{template}/shares"), Some(json!({"telegram_chat_id": GROUP}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    tgbot::start(&state, BotConfig { token: "TEST:TOKEN".into(), api_base: tg_url, allowed_chats: vec![GROUP] }).unwrap();
    let mut groups = json!(null);
    for _ in 0..200 {
        groups = call(&app, "GET", "/api/telegram", None).await.1;
        if groups["groups"].as_array().is_some_and(|g| !g.is_empty()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(groups["enabled"], true);
    assert_eq!(groups["username"], "rooftop_test_bot");
    assert_eq!(groups["groups"], json!([{"id": GROUP, "title": "Owners"}]));

    // Groups outside the allow-list are rejected.
    let (st, _) = call(&app, "POST", &format!("/api/chats/{template}/shares"), Some(json!({"telegram_chat_id": OTHER_GROUP}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, share) = call(
        &app,
        "POST",
        &format!("/api/chats/{template}/shares"),
        Some(json!({"slug": "jonas", "display_name": "Jonas", "telegram_chat_id": GROUP, "ai_delay_secs": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{share}");
    assert_eq!(share["telegram_chat_id"], GROUP);
    assert_eq!(share["ai_delay_secs"], 1);
    let share_id = share["id"].as_str().unwrap().to_string();

    let (_, s) = call(&app, "POST", "/api/s/jonas/sessions", Some(json!({"name": "Mia"}))).await;
    let token = s["token"].as_str().unwrap().to_string();
    let base = format!("/api/s/jonas/c/{token}");
    let chat_id = state.db.read().await.shares[&share_id].sessions[0].chat_id.clone();

    // 1. Nobody presses anything: the AI answers when the countdown ends.
    let (st, _) = call(&app, "POST", &format!("{base}/messages"), Some(json!({"text": "are you <there>?"}))).await;
    assert_eq!(st, StatusCode::OK);
    let sent = calls(&tg, "sendMessage");
    let relayed = sent.last().unwrap();
    assert_eq!(relayed["chat_id"], GROUP);
    let text = relayed["text"].as_str().unwrap();
    assert!(text.contains("<b>Mia</b>") && text.contains("are you &lt;there&gt;?"), "{text}");
    let (_, reply_cb, ai_cb, ai_label) = last_relay(&tg);
    assert!(reply_cb.starts_with("r:") && ai_cb.starts_with("a:"));
    assert_eq!(ai_label, "🤖 AI reply (1 s)");
    let (_, view) = call(&app, "GET", &base, None).await;
    assert_eq!(view["share"]["relay"], true);
    assert_eq!(view["typing"], true, "the countdown shows as typing");
    // The visitor can't summon the AI directly on a relayed link.
    let (st, _) = call(&app, "POST", &format!("{base}/respond"), None).await;
    assert_eq!(st, StatusCode::CONFLICT);
    for _ in 0..200 {
        if persona_count(&state, &chat_id).await == 1
            && calls(&tg, "sendMessage").iter().any(|b| b["text"].as_str().unwrap().contains("ai says hi"))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(persona_texts(&state, &chat_id).await, vec!["ai says hi"]);
    let echo = calls(&tg, "sendMessage").into_iter().find(|b| b["text"].as_str().unwrap().contains("ai says hi")).unwrap();
    assert!(echo["text"].as_str().unwrap().starts_with("🤖 <b>AI</b> → Mia"));
    assert!(calls(&tg, "editMessageReplyMarkup").iter().any(|b| b["reply_markup"]["inline_keyboard"] == json!([])), "buttons removed");
    assert_eq!(state.db.read().await.shares[&share_id].sessions[0].replies, 1, "AI answers count against the limit");
    let (_, view) = call(&app, "GET", &base, None).await;
    assert_eq!(view["typing"], false);

    // 2. "Reply": the countdown stops, a ForceReply prompt asks for the answer,
    //    and the owner's reply reaches the visitor as the persona.
    call(&app, "POST", &format!("{base}/messages"), Some(json!({"text": "who is this really"}))).await;
    let (relay_msg, reply_cb, _, _) = last_relay(&tg);
    tg.lock().unwrap().push(json!({"callback_query": {
        "id": "cq1", "data": reply_cb, "from": {"id": 7, "first_name": "Owner"},
        "message": {"message_id": relay_msg, "chat": {"id": GROUP, "type": "supergroup"}},
    }}));
    wait_for("the ForceReply prompt", || calls(&tg, "sendMessage").iter().any(|b| b["reply_markup"]["force_reply"] == true)).await;
    let prompt = calls(&tg, "sendMessage").into_iter().find(|b| b["reply_markup"]["force_reply"] == true).unwrap();
    assert_eq!(prompt["reply_parameters"]["message_id"], relay_msg);
    assert!(calls(&tg, "answerCallbackQuery").iter().any(|b| b["callback_query_id"] == "cq1"));
    let prompt_id = calls(&tg, "sendMessage").iter().position(|b| b["reply_markup"]["force_reply"] == true).unwrap() as i64 + 1;
    let (_, view) = call(&app, "GET", &base, None).await;
    assert_eq!(view["typing"], true, "the owner writing shows as typing too");
    tokio::time::sleep(Duration::from_millis(1600)).await;
    assert_eq!(persona_count(&state, &chat_id).await, 1, "no AI answer after Reply was pressed");

    // A reply from another group doesn't count, even to a known message id.
    tg.lock().unwrap().push(json!({"message": {
        "message_id": 900, "chat": {"id": OTHER_GROUP, "type": "supergroup", "title": "Strangers"},
        "from": {"id": 8, "first_name": "Eve", "is_bot": false}, "text": "injected",
        "reply_to_message": {"message_id": prompt_id, "from": {"id": BOT_ID, "is_bot": true}},
    }}));
    tg.lock().unwrap().push(json!({"message": {
        "message_id": 901, "chat": {"id": GROUP, "type": "supergroup", "title": "Owners"},
        "from": {"id": 7, "first_name": "Owner", "is_bot": false}, "text": "it's really me, promise",
        "reply_to_message": {"message_id": prompt_id, "from": {"id": BOT_ID, "is_bot": true}},
    }}));
    wait_for("the owner's reaction", || calls(&tg, "setMessageReaction").iter().any(|b| b["message_id"] == 901)).await;
    assert_eq!(persona_texts(&state, &chat_id).await, vec!["ai says hi", "it's really me, promise"]);
    let (_, view) = call(&app, "GET", &base, None).await;
    let last = view["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["text"], "it's really me, promise");
    assert_eq!(last["from"], "Jonas", "shown under the display name, like AI answers");
    assert_eq!(view["typing"], false);
    assert_eq!(state.db.read().await.shares[&share_id].sessions[0].replies, 1, "human answers are free");
    let prompt_text = prompt["text"].as_str().unwrap();
    assert!(prompt_text.contains("reply to this message") && prompt_text.contains("<b>Mia</b>"), "{prompt_text}");

    // The second way: a plain follow-up from whoever just answered (no reply needed),
    // while other people's plain messages stay in the group.
    let msg = |id: i64, from: i64, text: &str, reply_to: Option<i64>| {
        let mut m = json!({"message_id": id, "chat": {"id": GROUP, "type": "supergroup", "title": "Owners"},
            "from": {"id": from, "first_name": "U", "is_bot": false}, "text": text});
        if let Some(r) = reply_to {
            m["reply_to_message"] = json!({"message_id": r, "from": {"id": BOT_ID, "is_bot": true}});
        }
        json!({"message": m})
    };
    tg.lock().unwrap().push(msg(902, 8, "lol who is mia", None));
    tg.lock().unwrap().push(msg(903, 7, "and the lasagna is gone", None));
    wait_for("the follow-up", || calls(&tg, "setMessageReaction").iter().any(|b| b["message_id"] == 903)).await;
    assert!(!calls(&tg, "setMessageReaction").iter().any(|b| b["message_id"] == 902), "chatter of others is ignored");
    // The third way: a reply to the visitor's own relayed message, by anyone in the group.
    tg.lock().unwrap().push(msg(904, 9, "(someone else from the team)", Some(relay_msg)));
    wait_for("the reply to the visitor message", || calls(&tg, "setMessageReaction").iter().any(|b| b["message_id"] == 904)).await;
    let answered = vec!["ai says hi", "it's really me, promise", "and the lasagna is gone", "(someone else from the team)"];
    assert_eq!(persona_texts(&state, &chat_id).await, answered);

    // 3. "AI reply" answers right away; a stale button press is rejected.
    let (st, _) = call(&app, "PATCH", &format!("/api/shares/{share_id}"), Some(json!({"ai_delay_secs": 600}))).await;
    assert_eq!(st, StatusCode::OK);
    call(&app, "POST", &format!("{base}/messages"), Some(json!({"text": "ok then"}))).await;
    let (relay_msg, _, ai_cb, ai_label) = last_relay(&tg);
    assert_eq!(ai_label, "🤖 AI reply (600 s)");
    // Pressing it from a group the share isn't linked to does nothing.
    tg.lock().unwrap().push(json!({"callback_query": {
        "id": "cq-x", "data": ai_cb, "from": {"id": 8, "first_name": "Eve"},
        "message": {"message_id": relay_msg, "chat": {"id": OTHER_GROUP, "type": "supergroup"}},
    }}));
    tg.lock().unwrap().push(json!({"callback_query": {
        "id": "cq2", "data": ai_cb, "from": {"id": 7, "first_name": "Owner"},
        "message": {"message_id": relay_msg, "chat": {"id": GROUP, "type": "supergroup"}},
    }}));
    tg.lock().unwrap().push(json!({"callback_query": {
        "id": "cq3", "data": ai_cb, "from": {"id": 7, "first_name": "Owner"},
        "message": {"message_id": relay_msg, "chat": {"id": GROUP, "type": "supergroup"}},
    }}));
    wait_for("the stale answer", || calls(&tg, "answerCallbackQuery").iter().any(|b| b["callback_query_id"] == "cq3")).await;
    for _ in 0..200 {
        if persona_count(&state, &chat_id).await == answered.len() + 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut expected = answered.clone();
    expected.push("ai says hi");
    assert_eq!(persona_texts(&state, &chat_id).await, expected, "exactly one AI answer");
    // Once the AI took over, plain messages no longer go to the visitor.
    tg.lock().unwrap().push(msg(905, 7, "just chatting now", None));
    tg.lock().unwrap().push(msg(906, 7, "/help", None));
    wait_for("the /help answer", || calls(&tg, "sendMessage").iter().any(|b| b["reply_parameters"]["message_id"] == 906)).await;
    assert_eq!(persona_texts(&state, &chat_id).await, expected, "no plain message after the AI answered");
    let answers = calls(&tg, "answerCallbackQuery");
    let text_of = |id: &str| answers.iter().find(|b| b["callback_query_id"] == id).unwrap()["text"].as_str().unwrap().to_string();
    assert!(text_of("cq-x").contains("no longer linked"));
    assert!(text_of("cq2").contains("AI is answering"));
    assert_eq!(text_of("cq3"), "Already answered.");

    // Turning the relay off brings back the direct AI flow.
    let (st, v) = call(&app, "PATCH", &format!("/api/shares/{share_id}"), Some(json!({"telegram_chat_id": null}))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["telegram_chat_id"], Value::Null);
    let (_, view) = call(&app, "GET", &base, None).await;
    assert_eq!(view["share"]["relay"], false);
    let before = calls(&tg, "sendMessage").len();
    call(&app, "POST", &format!("{base}/messages"), Some(json!({"text": "plain again"}))).await;
    assert_eq!(calls(&tg, "sendMessage").len(), before, "nothing relayed");
}
