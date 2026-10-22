//! Large Telegram HTML exports through the real HTTP import path: every message
//! survives in order, sender names carry across page boundaries, and the import
//! finishes in a sane time. The 120,000-message case is `#[ignore]`d in debug CI:
//! `cargo test --release --test scale -- --ignored --nocapture`.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use rooftop::llm::LlmConfig;
use rooftop::server::{self, ServerOptions};
use rooftop::state::AppState;
use rooftop::store::Paths;
use serde_json::Value;
use std::fmt::Write;
use std::time::{Duration, Instant};
use tower::ServiceExt;

const PER_PAGE: usize = 1000;

/// Sender of message `n` (1-based): runs of 7 messages, so pages start in the
/// middle of a run (joined message, no `from_name`) and at run boundaries.
fn sender(n: usize) -> &'static str {
    if (n / 7).is_multiple_of(2) { "Anna" } else { "Boris" }
}

/// One `messagesN.html` holding messages `first..first + count`.
fn page(first: usize, count: usize) -> String {
    let mut h = String::with_capacity(count * 420);
    h.push_str("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Exported Data</title></head><body><div class=\"page_wrap\">");
    h.push_str("<div class=\"page_header\"><div class=\"content\"><div class=\"text bold\">Anna</div></div></div><div class=\"page_body chat_page\"><div class=\"history\">");
    for n in first..first + count {
        let joined = n > 1 && sender(n) == sender(n - 1);
        let _ = write!(h, "<div class=\"message default clearfix{}\" id=\"message{n}\"><div class=\"body\">", if joined { " joined" } else { "" });
        let _ = write!(h, "<div class=\"pull_right date details\" title=\"01.02.2025 10:{:02}:{:02} UTC+03:00\">10:00</div>", (n / 60) % 60, n % 60);
        if !joined {
            let _ = write!(h, "<div class=\"from_name\">{}</div>", sender(n));
        }
        if n % 500 == 0 {
            let file = format!("photos/photo_{}.jpg", (n / 500) % 5);
            let _ = write!(h, "<div class=\"media_wrap clearfix\"><a class=\"photo_wrap clearfix pull_left\" href=\"{file}\"><img class=\"photo\" src=\"{file}\"></a></div>");
        } else {
            let _ = write!(h, "<div class=\"text\">message number {n} with some words, an emoji \u{1F600} and a &amp; entity</div>");
        }
        h.push_str("</div></div>");
    }
    h.push_str("</div></div></div></body></html>");
    h
}

fn multipart(files: &[(String, Vec<u8>)]) -> (String, Vec<u8>) {
    let boundary = "XBOUNDARYX";
    let mut body = Vec::new();
    for (name, data) in files {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

async fn call(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

async fn run(messages: usize, budget: Duration) {
    let pages = messages.div_ceil(PER_PAGE);
    let mut files: Vec<(String, Vec<u8>)> = (0..pages)
        .map(|p| {
            let name = if p == 0 { "export/messages.html".to_string() } else { format!("export/messages{}.html", p + 1) };
            (name, page(p * PER_PAGE + 1, PER_PAGE.min(messages - p * PER_PAGE)).into_bytes())
        })
        .collect();
    for i in 0..5 {
        files.push((format!("export/photos/photo_{i}.jpg"), format!("jpeg bytes {i}").into_bytes()));
    }
    let size: usize = files.iter().map(|f| f.1.len()).sum();

    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(tmp.path().to_path_buf());
    let db = paths.load().unwrap();
    let llm = LlmConfig { base_url: "http://127.0.0.1:9/v1".into(), api_key: String::new(), model: "test".into(), temperature: None };
    let state = AppState::new(paths, db, llm);
    let app = server::router(state.clone(), ServerOptions::default());

    let started = Instant::now();
    let (ct, body) = multipart(&files);
    let req = Request::builder().method("POST").uri("/api/exports").header(header::CONTENT_TYPE, ct).body(Body::from(body)).unwrap();
    let (status, v) = call(&app, req).await;
    let total = started.elapsed();
    assert_eq!(status, StatusCode::OK, "{v}");
    println!("{messages} messages, {pages} pages, {:.1} MB: import took {total:.2?}", size as f64 / 1e6);
    assert!(total < budget, "import took {total:?}, budget {budget:?}");
    assert_eq!(v["export"]["messages"], messages);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "scale benchmark: run with --release -- --ignored"]
async fn imports_120k_messages() {
    run(120_000, Duration::from_secs(600)).await;
}
