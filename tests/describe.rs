//! "Describe with AI" must not save what a provider answers when it silently dropped the media
//! (seen on OpenRouter: an empty prompt, then a math proof saved as a voice transcript).

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use rooftop::describe::{Selection, describe_library};
use rooftop::llm::{LlmClient, LlmConfig};
use rooftop::model::{ChatExport, LibraryItem, MediaKind};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Replies in order, then repeats the last one. Counts the requests.
#[derive(Default)]
struct Script {
    replies: VecDeque<Value>,
    calls: usize,
}

fn reply(text: &str, prompt_tokens: u64) -> Value {
    json!({"choices": [{"message": {"role": "assistant", "content": text}}],
           "usage": {"prompt_tokens": prompt_tokens, "completion_tokens": 20}})
}

async fn completions(State(s): State<Arc<Mutex<Script>>>, Json(_): Json<Value>) -> Json<Value> {
    let mut s = s.lock().unwrap();
    s.calls += 1;
    let r = if s.replies.len() > 1 { s.replies.pop_front().unwrap() } else { s.replies[0].clone() };
    Json(r)
}

async fn run(replies: Vec<Value>) -> (Vec<(String, String)>, Vec<String>, usize) {
    let script = Arc::new(Mutex::new(Script { replies: replies.into(), calls: 0 }));
    let app = Router::new().route("/v1/chat/completions", post(completions)).with_state(script.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("voice_messages")).unwrap();
    std::fs::write(dir.path().join("voice_messages/v.ogg"), b"OggS fake opus bytes").unwrap();
    let export = ChatExport {
        title: "t".into(),
        library: vec![LibraryItem {
            id: "voc_1".into(),
            kind: MediaKind::Voice,
            file: Some("voice_messages/v.ogg".into()),
            thumb: None,
            title: None,
            available: true,
            description: None,
            size: None,
            uses: vec![1],
            senders: Default::default(),
        }],
        ..Default::default()
    };
    let cfg = LlmConfig { base_url: base, api_key: String::new(), model: "audio-model".into(), temperature: None };
    let (ok, failed) = describe_library(&LlmClient::new(), &cfg, dir.path(), &export, &Selection::default()).await;
    let calls = script.lock().unwrap().calls;
    (ok, failed, calls)
}

#[tokio::test]
async fn dropped_media_is_retried_and_never_saved() {
    let (ok, failed, calls) = run(vec![
        // The provider lost the message: 7 prompt tokens and an unrelated answer.
        reply("Looking at this problem, I need to find the minimum of …", 7),
        // Another provider received it but answered nothing.
        reply("", 96),
        reply("Okay, I'm at the pasta stand.", 96),
    ])
    .await;
    assert_eq!(calls, 3);
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(ok, vec![("voc_1".to_string(), "Okay, I'm at the pasta stand.".to_string())]);
}

#[tokio::test]
async fn a_provider_that_always_drops_media_fails_cleanly() {
    let (ok, failed, calls) = run(vec![reply("Hi! It looks like your message might have been empty.", 7)]).await;
    assert!(ok.is_empty(), "nothing is saved: {ok:?}");
    assert_eq!(calls, 3, "a bounded number of attempts");
    assert_eq!(failed.len(), 1);
    assert!(failed[0].contains("ignored the media"), "{}", failed[0]);
}

async fn record(State(s): State<Arc<Mutex<Vec<Value>>>>, Json(body): Json<Value>) -> Json<Value> {
    s.lock().unwrap().push(body);
    Json(reply("a dancing color test pattern, upbeat", 900))
}

/// Telegram exports GIFs as `.mp4` without a thumbnail: they are described from
/// frames taken in order (needs ffmpeg, skipped without it).
#[tokio::test]
async fn gif_mp4_is_described_from_frames() {
    let Some(ffmpeg) = rooftop::frames::ffmpeg() else {
        eprintln!("ffmpeg missing, skipping");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("gifs")).unwrap();
    let gif = dir.path().join("gifs/dance.mp4");
    let made = std::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=duration=2:size=320x240:rate=10", "-pix_fmt", "yuv420p"])
        .arg(&gif)
        .status()
        .unwrap();
    assert!(made.success());
    let item = LibraryItem {
        id: "gif_1".into(),
        kind: MediaKind::Gif,
        file: Some("gifs/dance.mp4".into()),
        thumb: None,
        title: None,
        available: true,
        description: None,
        size: None,
        uses: vec![1],
        senders: Default::default(),
    };
    assert!(rooftop::describe::can_describe(dir.path(), &item), "describable without a thumbnail");
    let export = ChatExport { title: "t".into(), library: vec![item], ..Default::default() };

    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new().route("/v1/chat/completions", post(record)).with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let cfg = LlmConfig { base_url: base, api_key: String::new(), model: "vision".into(), temperature: None };
    let (ok, failed) = describe_library(&LlmClient::new(), &cfg, dir.path(), &export, &Selection::default()).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(ok, vec![("gif_1".to_string(), "a dancing color test pattern, upbeat".to_string())]);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "one request for all frames");
    let content = seen[0]["messages"][0]["content"].as_array().unwrap();
    let text = content[0]["text"].as_str().unwrap();
    assert!(text.contains("frames, in order") && text.contains("GIF"), "{text}");
    let frames: Vec<&str> = content[1..].iter().map(|p| p["image_url"]["url"].as_str().unwrap()).collect();
    assert_eq!(frames.len(), rooftop::frames::FRAMES);
    assert!(frames.iter().all(|u| u.starts_with("data:image/jpeg;base64,/9j/")), "JPEG frames");
    assert!(frames.windows(2).all(|w| w[0] != w[1]), "different moments of the animation");
}
