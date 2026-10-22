//! Large Telegram HTML exports through the real HTTP import path (upload, background
//! job, polling): every message survives in order, sender names carry across page
//! boundaries, media is moved into the store, a repeated upload is recognised, and
//! nothing is sent to the LLM. The 120,000-message case is `#[ignore]`d:
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
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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
        let _ = write!(
            h,
            "<div class=\"message default clearfix{}\" id=\"message{n}\"><div class=\"body\">",
            if joined { " joined" } else { "" }
        );
        let _ = write!(
            h,
            "<div class=\"pull_right date details\" title=\"01.02.2025 10:{:02}:{:02} UTC+03:00\">10:00</div>",
            (n / 60) % 60,
            n % 60
        );
        if !joined {
            let _ = write!(h, "<div class=\"from_name\">{}</div>", sender(n));
        }
        if n % 500 == 0 {
            let file = format!("photos/photo_{}.jpg", (n / 500) % 5);
            let _ = write!(
                h,
                "<div class=\"media_wrap clearfix\"><a class=\"photo_wrap clearfix pull_left\" href=\"{file}\"><img class=\"photo\" src=\"{file}\"></a></div>"
            );
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

struct Env {
    _tmp: tempfile::TempDir,
    state: Arc<AppState>,
    app: Router,
    paths: Paths,
}

fn env(llm_url: &str) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(tmp.path().to_path_buf());
    let db = paths.load().unwrap();
    let llm = LlmConfig { base_url: llm_url.into(), api_key: String::new(), model: "test".into(), temperature: None };
    let state = AppState::new(paths.clone(), db, llm);
    let app = server::router(state.clone(), ServerOptions::default());
    Env { _tmp: tmp, state, app, paths }
}

fn export_files(messages: usize) -> Vec<(String, Vec<u8>)> {
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
    files
}

/// Uploads the files and polls the job until it ends. Returns the final job
/// and how long the upload request itself took to answer.
async fn import(app: &Router, files: &[(String, Vec<u8>)]) -> (Value, Duration) {
    let (ct, body) = multipart(files);
    let req = Request::builder().method("POST").uri("/api/exports").header(header::CONTENT_TYPE, ct).body(Body::from(body)).unwrap();
    let started = Instant::now();
    let (status, v) = call(app, req).await;
    let answered = started.elapsed();
    assert_eq!(status, StatusCode::ACCEPTED, "{v}");
    let id = v["id"].as_str().expect("job id").to_string();
    let mut last_stage = String::new();
    loop {
        let req = Request::builder().uri(format!("/api/imports/{id}")).body(Body::empty()).unwrap();
        let (status, job) = call(app, req).await;
        assert_eq!(status, StatusCode::OK, "{job}");
        let stage = job["stage"].as_str().unwrap_or_default().to_string();
        if stage != last_stage {
            println!("  [{:.2?}] {} {}/{} ({} messages)", started.elapsed(), stage, job["done"], job["total"], job["messages"]);
            last_stage = stage;
        }
        match job["state"].as_str().unwrap() {
            "done" | "failed" => return (job, answered),
            _ => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
}

async fn run(messages: usize, budget: Duration) {
    let e = env("http://127.0.0.1:9/v1");
    let files = export_files(messages);
    let size: usize = files.iter().map(|f| f.1.len()).sum();

    let started = Instant::now();
    let (job, answered) = import(&e.app, &files).await;
    let total = started.elapsed();
    println!(
        "{messages} messages, {} pages, {:.1} MB: upload answered in {answered:.2?}, import took {total:.2?}",
        files.len() - 5,
        size as f64 / 1e6
    );
    assert_eq!(job["state"], "done", "{job}");
    assert!(total < budget, "import took {total:?}, budget {budget:?}");
    assert_eq!(job["created"], true);
    assert_eq!(job["export"]["messages"], messages);
    assert_eq!(job["export"]["library"], 5);

    let id = job["export"]["id"].as_str().unwrap().to_string();
    let db = e.state.db.read().await;
    let export = &db.exports[&id].export;
    assert_eq!(export.messages.len(), messages);
    for (i, m) in export.messages.iter().enumerate() {
        let n = i + 1;
        assert_eq!(m.id, n as i64, "order at {n}");
        assert_eq!(m.from, sender(n), "sender of message {n} (pages start mid-run)");
    }
    let uses: usize = export.library.iter().map(|i| i.uses.len()).sum();
    assert_eq!(uses, messages / 500);
    let files_dir = e.paths.export_files(&id);
    for i in 0..5 {
        assert!(files_dir.join(format!("photos/photo_{i}.jpg")).is_file(), "photo_{i}.jpg placed");
    }
    drop(db);
    assert_eq!(std::fs::read_dir(e.paths.tmp()).unwrap().count(), 0, "upload directory is removed (media moved, not left behind)");

    // The same export again is recognised, not stored twice.
    let (again, _) = import(&e.app, &files).await;
    assert_eq!(again["state"], "done", "{again}");
    assert_eq!(again["created"], false);
    assert_eq!(again["export"]["id"], id.as_str());
    assert_eq!(e.state.db.read().await.exports.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn imports_5k_messages_across_pages() {
    run(5_000, Duration::from_secs(60)).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "scale benchmark: run with --release -- --ignored"]
async fn imports_120k_messages() {
    run(120_000, Duration::from_secs(600)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn import_makes_no_llm_calls() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        while listener.accept().await.is_ok() {
            counter.fetch_add(1, Ordering::Relaxed);
        }
    });
    let e = env(&url);
    let (job, _) = import(&e.app, &export_files(2_000)).await;
    assert_eq!(job["state"], "done", "{job}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(hits.load(Ordering::Relaxed), 0, "importing must not contact the LLM endpoint");
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_without_an_export_fails_with_a_clear_message() {
    let e = env("http://127.0.0.1:9/v1");
    let (job, _) = import(&e.app, &[("x/readme.txt".into(), b"hello".to_vec())]).await;
    assert_eq!(job["state"], "failed");
    assert!(job["error"].as_str().unwrap().contains("no messages.html, result.json or WhatsApp .txt found"), "{job}");
}
