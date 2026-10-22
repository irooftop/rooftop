//! Import formats beyond the macOS HTML export: Windows-style HTML, Telegram JSON
//! and WhatsApp text exports. All fixtures are synthetic and built in temp dirs.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use rooftop::import::{find_export_root, import_dir};
use rooftop::llm::LlmConfig;
use rooftop::model::{ChatExport, MediaKind};
use rooftop::server::{self, ServerOptions};
use rooftop::state::AppState;
use rooftop::store::Paths;
use serde_json::json;
use std::path::Path;
use tower::ServiceExt;

fn write(dir: &Path, rel: &str, content: impl AsRef<[u8]>) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

fn kinds(e: &ChatExport) -> Vec<MediaKind> {
    e.messages.iter().filter_map(|m| m.media.as_ref().map(|m| m.kind())).collect()
}

// ---------------------------------------------------------------- Telegram

fn html_message(id: i64, who: &str, title: &str, extra: &str) -> String {
    format!(
        "<div class=\"message default clearfix\" id=\"message{id}\"><div class=\"body\">\
         <div class=\"pull_right date details\" title=\"{title}\">10:00</div>\
         <div class=\"from_name\">{who}</div>{extra}</div></div>"
    )
}

fn windows_html() -> String {
    let msgs = [
        html_message(11, "Kate", "21.10.2026 18:06:02 GMT+03:00", "<div class=\"text\">hello there</div>"),
        html_message(
            12,
            "Sam",
            "21.10.2026 18:07:40 GMT+03:00",
            "<div class=\"reply_to details\">In reply to <a href=\"#go_to_message11\" onclick=\"return GoToMessage(11)\">this message</a></div>\
             <div class=\"text\">hi <a href=\"https://example.com/p\">page</a></div>",
        ),
        html_message(
            13,
            "Kate",
            "21.10.2026 18:10:35 GMT+03:00",
            "<div class=\"media_wrap clearfix\"><a class=\"photo_wrap clearfix pull_left\" href=\"photos/p1.jpg\">\
             <img class=\"photo\" src=\"photos/p1_thumb.jpg\"></a></div>",
        ),
    ];
    format!(
        "<html><head><title>Exported Data</title></head><body><div class=\"page_wrap\">\
         <div class=\"page_header\"><div class=\"content\"><div class=\"text bold\">\nSynthetic Chat \n</div></div></div>\
         <div class=\"page_body chat_page\"><div class=\"history\">{}</div></div></div></body></html>",
        msgs.concat()
    )
}

#[test]
fn windows_html_gmt_dates_and_old_header_title() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "messages.html", windows_html());
    write(tmp.path(), "photos/p1.jpg", b"jpg");
    let e = import_dir(tmp.path(), None).unwrap();
    assert_eq!(e.title, "Synthetic Chat");
    let dates: Vec<_> = e.messages.iter().map(|m| m.date.as_deref().unwrap()).collect();
    assert_eq!(dates, ["2026-10-21T18:06:02", "2026-10-21T18:07:40", "2026-10-21T18:10:35"]);
    assert_eq!(e.messages[1].reply_to, Some(11));
    assert_eq!(kinds(&e), [MediaKind::Photo]);
}

#[test]
fn generic_title_is_never_used() {
    let tmp = tempfile::tempdir().unwrap();
    let html = windows_html().replace("<div class=\"text bold\">\nSynthetic Chat \n</div>", "");
    write(tmp.path(), "messages.html", html);
    let e = import_dir(tmp.path(), None).unwrap();
    // Falls back to the first participant instead of "Exported Data".
    assert_eq!(e.title, "Kate");
}

fn chat_json() -> serde_json::Value {
    json!({
        "name": "Synthetic Chat", "type": "personal_chat", "id": 1,
        "messages": [
            {"id": 11, "type": "message", "date": "2026-10-21T18:06:02", "from": "Kate", "from_id": "user1", "text": "hello there", "text_entities": []},
            {"id": 12, "type": "message", "date": "2026-10-21T18:07:40", "edited": "2026-10-21T18:08:00", "from": "Sam", "from_id": "user2",
             "reply_to_message_id": 11, "text": ["hi ", {"type": "text_link", "text": "page", "href": "https://example.com/p"}, " ",
             {"type": "code", "text": "x=1"}], "text_entities": []},
            {"id": 13, "type": "message", "date": "2026-10-21T18:10:35", "from": "Kate", "from_id": "user1", "photo": "photos/p1.jpg", "text": ""},
            {"id": 14, "type": "message", "date": "2026-10-21T18:11:00", "from": "Sam", "from_id": "user2", "file": "stickers/s.webp",
             "media_type": "sticker", "thumbnail": "stickers/s.webp_thumb.jpg", "text": ""},
            {"id": 15, "type": "message", "date": "2026-10-21T18:12:00", "from": "Kate", "from_id": "user1", "file": "video_files/v.mp4",
             "media_type": "video_file", "duration_seconds": 65, "text": "", "reactions": [{"type": "emoji", "count": 2, "emoji": "👍",
             "recent": [{"from": "Sam", "from_id": "user2", "date": "2026-10-21T18:13:00"}]}]},
            {"id": 16, "type": "message", "date": "2026-10-21T18:13:00", "from": "Sam", "from_id": "user2", "file": "voice_messages/a.ogg",
             "media_type": "voice_message", "text": ""},
            {"id": 17, "type": "message", "date": "2026-10-21T18:14:00", "from": "Kate", "from_id": "user1", "file": "files/doc.pdf",
             "file_name": "doc.pdf", "text": ""},
            {"id": 18, "type": "message", "date": "2026-10-21T18:15:00", "from": "Sam", "from_id": "user2", "file": "video_files/gif.mp4",
             "media_type": "animation", "text": ""},
            {"id": 19, "type": "message", "date": "2026-10-21T18:16:00", "from": "Kate", "from_id": "user1", "file": "round_video_messages/r.mp4",
             "media_type": "video_message", "text": ""},
            {"id": 20, "type": "message", "date": "2026-10-21T18:17:00", "from": "Sam", "from_id": "user2", "forwarded_from": "Some Channel",
             "location_information": {"latitude": 50.45, "longitude": 30.52}, "text": ""},
            {"id": 21, "type": "service", "date": "2026-10-21T18:18:00", "actor": "Kate", "actor_id": "user1", "action": "pin_message",
             "message_id": 11, "text": ""}
        ]
    })
}

#[test]
fn json_only_export() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "result.json", chat_json().to_string());
    write(tmp.path(), "photos/p1.jpg", b"jpg");
    write(tmp.path(), "stickers/s.webp", b"webp");
    let e = import_dir(tmp.path(), None).unwrap();
    assert_eq!(e.title, "Synthetic Chat");
    assert_eq!(e.messages.len(), 11);
    let m = &e.messages;
    assert_eq!(m[0].date.as_deref(), Some("2026-10-21T18:06:02"));
    assert_eq!(m[1].text, "hi page (https://example.com/p) `x=1`");
    assert!(m[1].edited);
    assert_eq!(m[1].reply_to, Some(11));
    assert_eq!(
        kinds(&e),
        [
            MediaKind::Photo,
            MediaKind::Sticker,
            MediaKind::Video,
            MediaKind::Voice,
            MediaKind::File,
            MediaKind::Gif,
            MediaKind::RoundVideo,
            MediaKind::Location
        ]
    );
    assert_eq!(m[4].media.as_ref().unwrap().status.as_deref(), Some("1:05"));
    assert_eq!(m[4].reactions[0].emoji, "👍");
    assert_eq!(m[4].reactions[0].from, ["Sam"]);
    assert_eq!(m[9].forwarded_from.as_deref(), Some("Some Channel"));
    assert!(m[10].service);
    assert_eq!(m[10].text, "Kate pinned a message");
    let names: Vec<_> = e.participants.iter().map(|p| (p.name.as_str(), p.messages)).collect();
    assert_eq!(names, [("Kate", 5), ("Sam", 5)]);
    // Existing files are in the library, missing ones are listed as unavailable.
    assert!(e.library.iter().any(|i| i.kind == MediaKind::Photo && i.available));
    assert!(e.library.iter().any(|i| i.kind == MediaKind::Video && !i.available));
}

#[test]
fn html_and_json_agree_on_the_essentials() {
    let html_dir = tempfile::tempdir().unwrap();
    let json_dir = tempfile::tempdir().unwrap();
    let json = json!({
        "name": "Synthetic Chat", "type": "personal_chat", "id": 1,
        "messages": [
            {"id": 11, "type": "message", "date": "2026-10-21T18:06:02", "from": "Kate", "text": "hello there"},
            {"id": 12, "type": "message", "date": "2026-10-21T18:07:40", "from": "Sam", "reply_to_message_id": 11, "text": "hi page"},
            {"id": 13, "type": "message", "date": "2026-10-21T18:10:35", "from": "Kate", "photo": "photos/p1.jpg", "text": ""}
        ]
    });
    write(json_dir.path(), "result.json", json.to_string());
    // Plain text in the second message keeps the comparison independent of link rendering.
    let html = windows_html().replace("hi <a href=\"https://example.com/p\">page</a>", "hi page");
    write(html_dir.path(), "messages.html", html);
    let a = import_dir(html_dir.path(), None).unwrap();
    let b = import_dir(json_dir.path(), None).unwrap();
    assert_eq!(a.title, b.title);
    assert_eq!(a.messages.len(), b.messages.len());
    for (x, y) in a.messages.iter().zip(&b.messages) {
        assert_eq!((x.id, &x.from, &x.date, &x.text, x.reply_to, x.edited), (y.id, &y.from, &y.date, &y.text, y.reply_to, y.edited));
        assert_eq!(x.media.as_ref().map(|m| (m.kind(), m.file.clone())), y.media.as_ref().map(|m| (m.kind(), m.file.clone())));
    }
}

#[test]
fn html_is_preferred_when_both_exist() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "messages.html", windows_html());
    write(tmp.path(), "result.json", chat_json().to_string());
    assert_eq!(import_dir(tmp.path(), None).unwrap().messages.len(), 3);
}

// ---------------------------------------------------------------- WhatsApp

const ANDROID_12H: &str = "10/21/26, 6:17\u{202f}PM - Messages and calls are end-to-end encrypted. Only people in this chat can read them.\n\
10/21/26, 6:17\u{202f}PM - Welcome to the group: General\n\
10/21/26, 6:17\u{202f}PM - Kate created group \"General\"\n\
10/21/26, 6:18\u{202f}PM - Kate: hey\n\
10/21/26, 6:19\u{202f}PM - Sam: first line\n\
second line\n\
\n\
fourth line\n\
10/21/26, 6:20\u{202f}PM - Kate: fixed text <This message was edited>\n\
10/21/26, 6:21\u{202f}PM - Sam: VID-20261021-WA0000.mp4 (file attached)\n\
look at this\n\
10/21/26, 6:22\u{202f}PM - Kate: STK-20261021-WA0001.webp (file attached)\n\
10/21/26, 6:23\u{202f}PM - Sam: <Media omitted>\n\
10/21/26, 6:24\u{202f}PM - Kate: image omitted\n\
10/21/26, 6:25\u{202f}PM - Sam: This message was deleted\n\
10/21/26, 6:26\u{202f}PM - Kate: null\n\
10/21/26, 6:27\u{202f}PM - Sam: IMG-20261021-WA0009.jpg (file attached)\n\
10/21/26, 6:28\u{202f}PM - Sam joined using this group's invite link\n\
10/21/26, 6:29\u{202f}PM - Kate: bye\n";

fn whatsapp_dir(dir: &Path, with_media: bool) {
    write(dir, "WhatsApp Chat with General.txt", ANDROID_12H);
    if with_media {
        write(dir, "VID-20261021-WA0000.mp4", b"video-bytes");
        write(dir, "STK-20261021-WA0001.webp", b"sticker-bytes");
    }
}

#[test]
fn whatsapp_android_12h_folder_import_copies_media() {
    let src = tempfile::tempdir().unwrap();
    whatsapp_dir(src.path(), true);
    let dest = tempfile::tempdir().unwrap();
    let e = import_dir(src.path(), Some(dest.path())).unwrap();
    assert_eq!(e.title, "General");
    let text: Vec<_> = e.messages.iter().map(|m| (m.id, m.from.as_str(), m.text.as_str())).collect();
    assert_eq!(text[0], (1, "", "Welcome to the group: General"));
    assert_eq!(text[1], (2, "", "Kate created group \"General\""));
    assert_eq!(text[3], (4, "Sam", "first line\nsecond line\n\nfourth line"));
    assert_eq!(text[4], (5, "Kate", "fixed text"));
    assert!(e.messages[4].edited);
    assert!(e.messages[0].service && e.messages[1].service);
    assert_eq!(e.messages[0].date.as_deref(), Some("2026-10-21T18:17:00"));
    // Attachment with a caption keeps the caption.
    assert_eq!(e.messages[5].text, "look at this");
    assert_eq!(e.messages[5].media.as_ref().unwrap().file.as_deref(), Some("VID-20261021-WA0000.mp4"));
    // Omitted media have a kind but no file, deleted messages and "null" are dropped.
    let omitted: Vec<_> = e.messages.iter().filter(|m| m.media.as_ref().is_some_and(|m| m.file.is_none())).collect();
    assert_eq!(
        omitted.iter().map(|m| m.media.as_ref().unwrap().kind()).collect::<Vec<_>>(),
        [MediaKind::Other, MediaKind::Photo, MediaKind::Photo]
    );
    assert!(e.messages.iter().all(|m| !m.text.contains("deleted") && m.text != "null"));
    assert!(e.messages.iter().any(|m| m.service && m.text.starts_with("Sam joined")));
    assert!(!e.messages.iter().any(|m| m.text.contains("end-to-end")));
    assert!(e.messages.iter().enumerate().all(|(i, m)| m.id == i as i64 + 1));
    // Participants and library.
    let names: Vec<_> = e.participants.iter().map(|p| (p.name.as_str(), p.messages)).collect();
    assert_eq!(names, [("Kate", 5), ("Sam", 4)]);
    assert!(e.participants.iter().all(|p| p.avatar.is_none()));
    let lib: Vec<_> = e.library.iter().map(|i| (i.kind, i.available)).collect();
    assert!(lib.contains(&(MediaKind::Sticker, true)) && lib.contains(&(MediaKind::Video, true)));
    assert_eq!(e.library.len(), 2);
    assert_eq!(std::fs::read(dest.path().join("VID-20261021-WA0000.mp4")).unwrap(), b"video-bytes");
    assert_eq!(std::fs::read(dest.path().join("STK-20261021-WA0001.webp")).unwrap(), b"sticker-bytes");
    assert!(!dest.path().join("WhatsApp Chat with General.txt").exists());
}

#[test]
fn whatsapp_without_media_files_marks_attachments_missing() {
    let src = tempfile::tempdir().unwrap();
    whatsapp_dir(src.path(), false);
    let e = import_dir(src.path(), None).unwrap();
    let video = e.messages.iter().find_map(|m| m.media.as_ref().filter(|m| m.kind() == MediaKind::Video)).unwrap();
    // Files that are not in the export are never referenced, so they have no library entry.
    assert_eq!(video.file, None);
    assert!(e.library.is_empty());
}

#[test]
fn whatsapp_lone_txt_path_and_nested_zip_layout() {
    let src = tempfile::tempdir().unwrap();
    write(src.path(), "__MACOSX/._WhatsApp Chat with General.txt", "garbage");
    whatsapp_dir(&src.path().join("export"), true);
    // A folder one level down is found, resource-fork files are ignored.
    assert_eq!(find_export_root(src.path()).unwrap(), src.path().join("export"));
    let lone = tempfile::tempdir().unwrap();
    write(lone.path(), "WhatsApp Chat with Kate.txt", "21/10/2026, 18:06 - Kate: hi\n21/10/2026, 18:07 - Me: yo\n");
    let e = import_dir(&lone.path().join("WhatsApp Chat with Kate.txt"), None).unwrap();
    assert_eq!(e.title, "Kate");
    assert_eq!(e.messages.len(), 2);
    assert_eq!(e.exporter(), Some("Me"));
}

#[test]
fn whatsapp_24h_day_first_formats() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "chat.txt", "03/04/2025, 18:06 - Ann: one\n13.04.2025, 09:30 - Bob: two\n");
    let e = import_dir(tmp.path(), None).unwrap();
    assert_eq!(e.messages[0].date.as_deref(), Some("2025-04-03T18:06:00"));
    assert_eq!(e.messages[1].date.as_deref(), Some("2025-04-13T09:30:00"));
    // No file name hint: the busiest sender names the chat.
    assert_eq!(e.title, "Ann");

    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "chat.txt", "21.10.26, 18:06 - Ann: one\n22.10.26, 09:30 - Bob: two\n");
    let e = import_dir(tmp.path(), None).unwrap();
    assert_eq!(e.messages[1].date.as_deref(), Some("2026-10-22T09:30:00"));
}

#[test]
fn whatsapp_ios_bracket_format() {
    let tmp = tempfile::tempdir().unwrap();
    let chat = "\u{200e}[21/10/2026, 18:06:02] Kate: \u{200e}Messages and calls are end-to-end encrypted. No one outside of this chat can read them.\n\
\u{200e}[21/10/2026, 18:06:03] Kate: \u{200e}You created group \"Team\"\n\
[21/10/2026, 18:07:10] Kate: hello\nworld\n\
[21/10/2026, 18:08:00] Sam: \u{200e}<attached: 00000012-PHOTO-2026-10-21-18-08-00.jpg>\n\
[21/10/2026, 18:09:00] Sam: \u{200e}<attached: 00000013-AUDIO-2026-10-21-18-09-00.opus>\n\
[21/10/2026, 18:10:00] Sam: \u{200e}<attached: ../../etc/passwd>\n\
[21/10/2026, 18:11:00] Kate: \u{200e}sticker omitted\n\
[21/10/2026, 18:12:00] Sam: edited \u{200e}<This message was edited>\n\
[10/21/26, 6:13:00 AM] Sam: ignored mixed style but still a header\n";
    write(tmp.path(), "_chat.txt", chat);
    write(tmp.path(), "00000012-PHOTO-2026-10-21-18-08-00.jpg", b"jpg");
    write(tmp.path(), "00000013-AUDIO-2026-10-21-18-09-00.opus", b"opus");
    let e = import_dir(tmp.path(), None).unwrap();
    let m = &e.messages;
    assert!(m[0].service && m[0].text == "You created group \"Team\"");
    assert_eq!((m[1].from.as_str(), m[1].text.as_str()), ("Kate", "hello\nworld"));
    assert_eq!(m[1].date.as_deref(), Some("2026-10-21T18:07:10"));
    let photo = m[2].media.as_ref().unwrap();
    assert_eq!((photo.kind(), photo.file.as_deref()), (MediaKind::Photo, Some("00000012-PHOTO-2026-10-21-18-08-00.jpg")));
    let voice = m[3].media.as_ref().unwrap();
    assert_eq!(voice.kind(), MediaKind::Voice);
    // Paths outside the export are never referenced.
    assert_eq!(m[4].media.as_ref().unwrap().file, None);
    assert_eq!(m[5].media.as_ref().unwrap().kind(), MediaKind::Sticker);
    assert!(m[6].edited && m[6].text == "edited");
    assert_eq!(e.title, "Sam");
}

#[test]
fn unrelated_text_files_are_not_chats() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "notes.txt", "just some notes\nnothing to see");
    assert!(find_export_root(tmp.path()).is_none());
    let err = import_dir(tmp.path(), None).unwrap_err().to_string();
    assert!(err.contains("WhatsApp") && err.contains("messages.html") && err.contains("result.json"), "{err}");
}

// ---------------------------------------------------------------- upload

fn multipart(files: &[(&str, &[u8])]) -> (String, Vec<u8>) {
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

async fn upload(files: &[(&str, &[u8])]) -> (StatusCode, serde_json::Value, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(tmp.path().to_path_buf());
    let db = paths.load().unwrap();
    let llm = LlmConfig { base_url: "http://127.0.0.1:9/v1".into(), api_key: String::new(), model: "test".into(), temperature: None };
    let app = server::router(AppState::new(paths, db, llm), ServerOptions::default());
    let (ct, body) = multipart(files);
    let req = Request::builder().method("POST").uri("/api/exports").header(header::CONTENT_TYPE, ct).body(Body::from(body)).unwrap();
    let (status, started) = send(&app, req).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    // The import runs in the background: poll its job to the end.
    let uri = format!("/api/imports/{}", started["id"].as_str().unwrap());
    loop {
        let (_, job) = send(&app, Request::builder().uri(&uri).body(Body::empty()).unwrap()).await;
        match job["state"].as_str() {
            Some("done") => return (StatusCode::OK, job, tmp),
            Some("failed") => return (StatusCode::BAD_REQUEST, job, tmp),
            _ => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
        }
    }
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

#[tokio::test]
async fn upload_accepts_a_lone_txt() {
    let (st, v, _tmp) = upload(&[("WhatsApp Chat with Kate.txt", b"21/10/2026, 18:06 - Kate: hi\n21/10/2026, 18:07 - Me: yo\n")]).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["created"], true);
    assert_eq!(v["export"]["title"], "Kate");
}

#[tokio::test]
async fn upload_accepts_a_zip_with_the_txt() {
    use std::io::Write;
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        zw.start_file("WhatsApp Chat with Kate.txt", opts).unwrap();
        zw.write_all(b"10/21/26, 6:17\xe2\x80\xafPM - Kate: hi\n").unwrap();
        zw.start_file("IMG-20261021-WA0001.jpg", opts).unwrap();
        zw.write_all(b"jpg").unwrap();
        zw.finish().unwrap();
    }
    let (st, v, tmp) = upload(&[("chat.zip", buf.get_ref())]).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["export"]["title"], "Kate");
    drop(tmp);
}

#[tokio::test]
async fn upload_rejects_unrelated_text_with_a_helpful_error() {
    let (st, v, _tmp) = upload(&[("notes.txt", b"nothing here")]).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(v.to_string().contains("WhatsApp"), "{v}");
}

fn test_app() -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::new(tmp.path().to_path_buf());
    let db = paths.load().unwrap();
    let llm = LlmConfig { base_url: "http://127.0.0.1:9/v1".into(), api_key: String::new(), model: "test".into(), temperature: None };
    (server::router(AppState::new(paths, db, llm), ServerOptions::default()), tmp)
}

fn post_files(uri: &str, files: &[(&str, &[u8])]) -> Request<Body> {
    let (ct, body) = multipart(files);
    Request::builder().method("POST").uri(uri).header(header::CONTENT_TYPE, ct).body(Body::from(body)).unwrap()
}

async fn wait_for_job(app: &axum::Router, id: &str) -> serde_json::Value {
    let uri = format!("/api/imports/{id}");
    loop {
        let (_, job) = send(app, Request::builder().uri(&uri).body(Body::empty()).unwrap()).await;
        if matches!(job["state"].as_str(), Some("done" | "failed")) {
            return job;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// The web UI uploads big exports in batches: a session, several (retried) batches, then the import.
#[tokio::test]
async fn upload_session_imports_files_sent_in_batches() {
    let (app, tmp) = test_app();
    let (st, v) = send(&app, Request::builder().method("POST").uri("/api/uploads").body(Body::empty()).unwrap()).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    let chat = b"21/10/2026, 18:06 - Kate: hi\n21/10/2026, 18:07 - Me: IMG-20261021-WA0001.jpg (file attached)\n";
    let files = format!("/api/uploads/{id}/files");
    let (st, v) = send(&app, post_files(&files, &[("Chat/WhatsApp Chat with Kate.txt", chat)])).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["files"], 1);
    // A batch sent twice (a retry) replaces its files.
    for _ in 0..2 {
        let (st, v) = send(&app, post_files(&files, &[("Chat/IMG-20261021-WA0001.jpg", b"jpg")])).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["bytes"], 3);
    }
    let import = format!("/api/uploads/{id}/import");
    let (st, v) = send(&app, Request::builder().method("POST").uri(&import).body(Body::empty()).unwrap()).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    let job = wait_for_job(&app, v["id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "done", "{job}");
    assert_eq!(job["export"]["title"], "Kate");
    // The session is used up: no second import, no more batches.
    let (st, _) = send(&app, Request::builder().method("POST").uri(&import).body(Body::empty()).unwrap()).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = send(&app, post_files(&files, &[("x.jpg", b"x")])).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // Nothing is left in tmp once the job is done.
    let left: Vec<_> = std::fs::read_dir(tmp.path().join("tmp")).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
}

#[tokio::test]
async fn upload_session_rejects_bad_ids_and_empty_imports() {
    let (app, _tmp) = test_app();
    for bad in ["../../etc", "zz", "0123456789abcdef0123456789abcdeg"] {
        let (st, _) = send(&app, post_files(&format!("/api/uploads/{bad}/files"), &[("a.txt", b"a")])).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{bad}");
    }
    let (_, v) = send(&app, Request::builder().method("POST").uri("/api/uploads").body(Body::empty()).unwrap()).await;
    let id = v["id"].as_str().unwrap();
    let (st, v) = send(&app, Request::builder().method("POST").uri(format!("/api/uploads/{id}/import")).body(Body::empty()).unwrap()).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    // Cancelling drops the session.
    let (_, v) = send(&app, Request::builder().method("POST").uri("/api/uploads").body(Body::empty()).unwrap()).await;
    let id = v["id"].as_str().unwrap();
    let (st, _) = send(&app, Request::builder().method("DELETE").uri(format!("/api/uploads/{id}")).body(Body::empty()).unwrap()).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = send(&app, post_files(&format!("/api/uploads/{id}/files"), &[("a.txt", b"a")])).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

/// A body that breaks off mid-file names the cause and the file instead of axum's generic message.
#[tokio::test]
async fn broken_upload_names_the_cause_and_leaves_no_partial_file() {
    let (app, tmp) = test_app();
    let (_, v) = send(&app, Request::builder().method("POST").uri("/api/uploads").body(Body::empty()).unwrap()).await;
    let id = v["id"].as_str().unwrap();
    let (ct, body) = multipart(&[("Chat/ok.jpg", b"fine"), ("Chat/big.mp4", &[7u8; 4096])]);
    let cut = body.len() - 1000; // inside the second file
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/uploads/{id}/files"))
        .header(header::CONTENT_TYPE, ct)
        .body(Body::from(body[..cut].to_vec()))
        .unwrap();
    let (st, v) = send(&app, req).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let err = v["error"].as_str().unwrap();
    assert!(err.contains("Chat/big.mp4") && !err.contains("Error parsing") && err.contains("the server received"), "{err}");
    let dir = tmp.path().join("tmp").join(format!("up-{id}")).join("files").join("Chat");
    assert!(dir.join("ok.jpg").exists());
    assert!(!dir.join("big.mp4").exists() && !dir.join("big.mp4.part").exists());
}

/// Before the media: the check reads the chat, and finds an export that is already imported.
#[tokio::test]
async fn upload_check_reads_the_chat_and_spots_a_repeat() {
    let (app, _tmp) = test_app();
    let chat: &[u8] = b"21/10/2026, 18:06 - Kate: hi\n21/10/2026, 18:07 - Me: yo\n";
    let mut job_export = None;
    for round in 0..2 {
        let (_, v) = send(&app, Request::builder().method("POST").uri("/api/uploads").body(Body::empty()).unwrap()).await;
        let id = v["id"].as_str().unwrap().to_string();
        let (st, v) = send(&app, post_files(&format!("/api/uploads/{id}/files"), &[("WhatsApp Chat with Kate.txt", chat)])).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        let check = Request::builder()
            .method("POST")
            .uri(format!("/api/uploads/{id}/check"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"bytes": 1000}"#))
            .unwrap();
        let (st, c) = send(&app, check).await;
        assert_eq!(st, StatusCode::OK, "{c}");
        assert_eq!(c["title"], "Kate");
        assert_eq!(c["messages"], 2);
        if round == 0 {
            assert!(c["existing"].is_null(), "{c}");
            assert_eq!(c["fits"], true, "{c}");
            let (_, v) =
                send(&app, Request::builder().method("POST").uri(format!("/api/uploads/{id}/import")).body(Body::empty()).unwrap()).await;
            let job = wait_for_job(&app, v["id"].as_str().unwrap()).await;
            job_export = Some(job["export"]["id"].clone());
        } else {
            assert_eq!(Some(c["existing"]["id"].clone()), job_export, "{c}");
        }
    }
    // Way more than any disk: refused before the media are sent.
    let (_, v) = send(&app, Request::builder().method("POST").uri("/api/uploads").body(Body::empty()).unwrap()).await;
    let id = v["id"].as_str().unwrap().to_string();
    send(&app, post_files(&format!("/api/uploads/{id}/files"), &[("WhatsApp Chat with Bob.txt", b"21/10/2026, 18:06 - Bob: hi\n")])).await;
    let check = Request::builder()
        .method("POST")
        .uri(format!("/api/uploads/{id}/check"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!(r#"{{"bytes": {}}}"#, u64::MAX / 4)))
        .unwrap();
    let (_, c) = send(&app, check).await;
    if !c["free_bytes"].is_null() {
        assert_eq!(c["fits"], false, "{c}");
    }
    // Something that isn't a chat fails at the check, not after the upload.
    let (_, v) = send(&app, Request::builder().method("POST").uri("/api/uploads").body(Body::empty()).unwrap()).await;
    let id = v["id"].as_str().unwrap().to_string();
    send(&app, post_files(&format!("/api/uploads/{id}/files"), &[("notes.txt", b"nothing here")])).await;
    let (st, _) = send(&app, Request::builder().method("POST").uri(format!("/api/uploads/{id}/check")).body(Body::empty()).unwrap()).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

/// Inline SVGs carry their description in aria-label: a <title> would pop up as a tooltip.
#[tokio::test]
async fn index_svgs_have_no_tooltip_titles() {
    let (app, _tmp) = test_app();
    let resp = app.clone().oneshot(Request::builder().uri("/").body(Body::empty()).unwrap()).await.unwrap();
    let html = String::from_utf8(axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap();
    assert_eq!(html.matches("<title").count(), 1, "only the page title");
    assert!(html.contains(r#"id="tutorial""#) && html.contains(r#"aria-label="How to export a chat in Telegram Desktop"#));
    assert!(!html.contains("aria-labelledby=\"tut-title\"") && !html.contains("sceneTitle"));
}
