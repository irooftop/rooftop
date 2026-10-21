//! Photo / voice habits end to end, without network: a voice-only export, a mock
//! OpenAI-compatible LLM and a mock stdio MCP server that "generates" speech
//! (a WAV written to its working directory, returned as a `resource_link`) and
//! images (inline base64 PNG).

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::routing::post;
use axum::{Json, routing::get};
use rooftop::llm::LlmConfig;
use rooftop::mcp::ServerConfig;
use rooftop::model::MediaKind;
use rooftop::server::{self, ServerOptions};
use rooftop::state::AppState;
use rooftop::store::Paths;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;
use tower::ServiceExt;

/// 1 second of 16 kHz mono silence.
fn wav() -> Vec<u8> {
    let data = vec![0u8; 32_000];
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    for v in [16u32.to_le_bytes().to_vec(), 1u16.to_le_bytes().to_vec(), 1u16.to_le_bytes().to_vec()] {
        b.extend_from_slice(&v);
    }
    b.extend_from_slice(&16_000u32.to_le_bytes());
    b.extend_from_slice(&32_000u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    b.extend_from_slice(&data);
    b
}

/// A Telegram HTML export in which both friends only send voice messages.
fn voice_export(dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join("voice_messages")).unwrap();
    let mut html = String::from(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Sam</title></head><body><div class=\"page_wrap\"><div class=\"page_body chat_page\"><div class=\"history\">\
         <div class=\"message service\"><div class=\"body details\">8 October 2026</div></div>",
    );
    for i in 1..=8 {
        let who = if i % 2 == 1 { "Kate" } else { "Sam" };
        let id = 500 + i;
        let file = format!("voice_messages/{id}_file_{id}_0.ogg");
        std::fs::write(dir.join(&file), wav()).unwrap();
        html.push_str(&format!(
            "<div class=\"message default clearfix\" id=\"message{id}\"><div class=\"body\">\
             <div class=\"pull_right date details\" title=\"8 October 2026, 18:0{i}:00\">18:0{i}</div>\
             <div class=\"from_name\">{who}</div>\
             <div class=\"media_wrap clearfix\"><a class=\"media clearfix pull_left block_link media_voice_message\" href=\"{file}\">\
             <div class=\"fill pull_left\"></div><div class=\"body\"><div class=\"title bold\">Voice message</div><div class=\"status details\">0:01</div></div></a></div>\
             </div></div>"
        ));
    }
    html.push_str("</div></div></div></body></html>");
    std::fs::write(dir.join("messages.html"), html).unwrap();
}

const MCP_SERVER: &str = r#"
import sys, json, os, base64
PNG = base64.b64encode(b"\x89PNG\r\n\x1a\n" + b"\x00" * 64).decode()
WAV = open(os.environ["WAV_SAMPLE"], "rb").read()
tools = [
  {"name": "generate_speech", "description": "Text to speech", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}, "output": {"type": "string"}, "voice": {"type": "string"}}}},
  {"name": "generate_image", "description": "Generate an image", "inputSchema": {"type": "object", "properties": {"prompt": {"type": "string"}, "output": {"type": "string"}}}},
]
calls = open("calls.jsonl", "a")
for line in sys.stdin:
    m = json.loads(line); mid = m.get("id"); meth = m.get("method")
    if mid is None: continue
    if meth == "initialize":
        r = {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}}, "serverInfo": {"name": "media", "version": "1"}}
    elif meth == "tools/list":
        r = {"tools": tools}
    elif meth == "tools/call":
        p = m["params"]; a = p["arguments"]; calls.write(json.dumps(p) + "\n"); calls.flush()
        if p["name"] == "generate_speech":
            out = os.path.abspath(a["output"]); open(out, "wb").write(WAV)
            r = {"content": [{"type": "resource_link", "uri": "file://" + out, "name": a["output"], "mimeType": "audio/wav"}, {"type": "text", "text": "saved"}]}
        else:
            r = {"content": [{"type": "image", "mimeType": "image/png", "data": PNG}]}
    else:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "nope"}}) + "\n"); sys.stdout.flush(); continue
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": r}) + "\n"); sys.stdout.flush()
"#;

#[derive(Default)]
struct Llm {
    /// Tool calls to answer with, one per persona request.
    script: Vec<Value>,
    requests: Vec<Value>,
}

async fn completions(State(llm): State<Arc<Mutex<Llm>>>, Json(body): Json<Value>) -> Json<Value> {
    let mut l = llm.lock().await;
    l.requests.push(body.clone());
    let audio = body.to_string().contains("input_audio");
    let message = if audio {
        json!({"role": "assistant", "content": "are you on your way?"})
    } else if l.script.is_empty() {
        json!({"role": "assistant", "content": null, "tool_calls": []})
    } else {
        let call = l.script.remove(0);
        json!({"role": "assistant", "content": null, "tool_calls": [{"id": "c1", "type": "function", "function": {"name": call["name"], "arguments": call["arguments"].to_string()}}]})
    };
    Json(json!({"choices": [{"message": message, "finish_reason": "stop"}]}))
}

#[tokio::test]
async fn voice_and_photo_habits_with_media_tools() {
    if std::process::Command::new("python3").arg("--version").output().is_err() {
        eprintln!("python3 missing, skipping");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let export_dir = tmp.path().join("ChatExport_Sam");
    voice_export(&export_dir);
    let sample = tmp.path().join("sample.wav");
    std::fs::write(&sample, wav()).unwrap();

    let llm = Arc::new(Mutex::new(Llm {
        script: vec![
            json!({"name": "send_voice", "arguments": {"text": "yeah, on my way"}}),
            json!({"name": "send_photo", "arguments": {"prompt": "trailhead parking lot at dawn", "caption": "here"}}),
        ],
        ..Default::default()
    }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", l.local_addr().unwrap());
    let app = Router::new().route("/v1/chat/completions", post(completions)).route("/", get(|| async { "" })).with_state(llm.clone());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });

    let paths = Paths::new(tmp.path().join("data"));
    let mut db = paths.load().unwrap();
    let mut env = std::collections::BTreeMap::new();
    env.insert("WAV_SAMPLE".to_string(), sample.display().to_string());
    db.settings.mcp_servers.insert(
        "media".into(),
        ServerConfig {
            command: Some("python3".into()),
            args: vec!["-I".into(), "-c".into(), MCP_SERVER.into()],
            env,
            ..Default::default()
        },
    );
    let cfg = LlmConfig { base_url: base, api_key: String::new(), model: "mock".into(), temperature: None };
    let state = AppState::new(paths, db, cfg);
    let (rec, _) = server::import_into_store(&state, export_dir).await.unwrap();
    assert!(rec.export.library.iter().all(|i| i.kind == MediaKind::Voice));

    // Transcribe the export (mock audio model) like "Describe with AI" does.
    let router = server::router(state.clone(), ServerOptions::default());
    let resp =
        router.clone().oneshot(Request::post(format!("/api/exports/{}/describe", rec.id)).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let rec = state.db.read().await.exports.get(&rec.id).cloned().unwrap();
    assert!(rec.export.library.iter().all(|i| i.description.as_deref() == Some("are you on your way?")));

    let chat = server::make_chat(&rec, "Sam", Some("Kate"), None, "").map_err(|_| ()).unwrap();
    let chat_id = chat.id.clone();
    let mut chat = chat;
    chat.voice = "Puck".into();
    state.paths.save_chat(&chat).unwrap();
    state.db.write().await.chats.insert(chat_id.clone(), chat);

    // Kate records a voice message in the UI and it is transcribed for the persona.
    let boundary = "XBOUNDARY";
    let mut body =
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"voice.wav\"\r\nContent-Type: audio/wav\r\n\r\n")
            .into_bytes();
    body.extend_from_slice(&wav());
    body.extend_from_slice(
        format!("\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"duration\"\r\n\r\n1\r\n--{boundary}--\r\n").as_bytes(),
    );
    let resp = router
        .clone()
        .oneshot(
            Request::post(format!("/api/chats/{chat_id}/voice"))
                .header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: Value = serde_json::from_slice(&axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(v["messages"][0]["media"]["kind"], "voice");
    assert_eq!(v["messages"][0]["media"]["description"], "are you on your way?");
    assert_eq!(v["messages"][0]["media"]["status"], "0:01");

    // The persona answers with a voice message.
    let out = rooftop::agent::respond(&state, &chat_id).await.unwrap();
    assert!(out.error.is_none(), "{:?}", out.error);
    let req = llm.lock().await.requests.last().cloned().unwrap();
    let tools: Vec<&str> = req["tools"].as_array().unwrap().iter().filter_map(|t| t["function"]["name"].as_str()).collect();
    assert!(tools.contains(&"send_voice") && tools.contains(&"send_photo"), "{tools:?}");
    let system = req["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("Voice habit") && system.contains("send_voice"), "{system}");
    assert!(system.contains("talks mostly in voice messages"));
    let history_calls: Vec<&str> = req["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["tool_calls"].as_array())
        .flatten()
        .filter_map(|c| c["function"]["name"].as_str())
        .collect();
    assert!(!history_calls.is_empty() && history_calls.iter().all(|n| *n == "send_voice"), "{history_calls:?}");
    assert!(req.to_string().contains("<voice message 0:01: «are you on your way?»>"), "user voice shown with transcript");

    let msg = match &out.events[..] {
        [rooftop::chat::Event::Message(m)] => m.clone(),
        other => panic!("expected one message, got {other:?}"),
    };
    let media = msg.media.unwrap();
    assert_eq!(media.kind(), MediaKind::Voice);
    assert_eq!(media.description.as_deref(), Some("yeah, on my way"));
    assert_eq!(media.status.as_deref(), Some("0:01"));
    let file = media.file.unwrap();
    assert!(file.starts_with(&format!("generated/{chat_id}/gen_")) && file.ends_with(".wav"), "{file}");
    assert!(state.paths.export_files(&rec.id).join(&file).is_file());
    let calls = std::fs::read_to_string(tmp.path().join("data/mcp/media/calls.jsonl")).unwrap();
    let first: Value = serde_json::from_str(calls.lines().next().unwrap()).unwrap();
    assert_eq!(first["arguments"]["voice"], "Puck", "the chat's voice is passed");
    assert_eq!(first["arguments"]["text"], "yeah, on my way");

    // And with a photo.
    let out = rooftop::agent::respond(&state, &chat_id).await.unwrap();
    assert!(out.error.is_none(), "{:?}", out.error);
    let rooftop::chat::Event::Message(photo) = &out.events[0] else { panic!("expected a message") };
    let pm = photo.media.clone().unwrap();
    assert_eq!(pm.kind(), MediaKind::Photo);
    assert_eq!(photo.text, "here");
    assert!(pm.file.unwrap().ends_with(".png"));

    // The next request replays both: the voice as send_voice, the photo with what it showed.
    let _ = rooftop::agent::respond(&state, &chat_id).await.unwrap();
    let req = llm.lock().await.requests.last().cloned().unwrap().to_string();
    assert!(req.contains(r#"{\"text\":\"yeah, on my way\"}"#), "{req}");
    assert!(req.contains("trailhead parking lot at dawn"));

    // Generated media are served to the owner and deleted with the chat.
    let gen_dir = state.paths.export_files(&rec.id).join("generated").join(&chat_id);
    assert!(gen_dir.is_dir());
    let resp = router.clone().oneshot(Request::delete(format!("/api/chats/{chat_id}")).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(!gen_dir.exists());
}
