//! Parser for Telegram Desktop "Machine-readable JSON" exports (`result.json`).
//! Produces the same structures as the HTML parser.
//!
//! The file is streamed: one message at a time is turned into a [`Message`], so
//! a file with hundreds of thousands of messages never exists as one JSON tree.

use crate::import::{Progress, Report, Stage};
use crate::model::{Media, MediaKind, Message, Reaction};
use crate::telegram::ParsedHtml;
use anyhow::{Context, Result, bail};
use chrono::NaiveDateTime;
use serde::Deserializer as _;
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::cell::Cell;
use std::fmt;
use std::io::{BufReader, Read};
use std::path::Path;
use std::rc::Rc;

pub const FILE_NAME: &str = "result.json";

/// Elements between two progress reports.
const REPORT_EVERY: usize = 2000;

/// Cheap check that `path` is a single-chat export (the `messages` key sits near the top).
pub fn looks_like_chat_json(path: &Path) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut buf = vec![0u8; 4096];
    let n = f.read(&mut buf).unwrap_or(0);
    let head = String::from_utf8_lossy(&buf[..n]);
    head.trim_start().starts_with('{') && head.contains("\"messages\"")
}

/// Reads `result.json` from `dir`, reporting the share of the file read so far.
pub fn parse_export_dir(dir: &Path, report: Report) -> Result<ParsedHtml> {
    let path = dir.join(FILE_NAME);
    let file = std::fs::File::open(&path).with_context(|| format!("reading {}", path.display()))?;
    let total = file.metadata().map_or(0, |m| m.len() as usize);
    let read = Rc::new(Cell::new(0usize));
    let reader = Counting { inner: BufReader::with_capacity(1 << 20, file), read: read.clone() };
    parse_reader(reader, &|messages| report(Progress { stage: Stage::Parsing, done: read.get().min(total), total, messages }))
}

/// Counts the bytes handed out by the reader.
struct Counting<R> {
    inner: R,
    read: Rc<Cell<usize>>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read.set(self.read.get() + n);
        Ok(n)
    }
}

/// Parses an export from `reader`, calling `progress` with the message count every few thousand messages.
fn parse_reader(reader: impl Read, progress: &dyn Fn(usize)) -> Result<ParsedHtml> {
    let mut out = ParsedHtml::default();
    let mut de = serde_json::Deserializer::from_reader(reader);
    let has_messages = de.deserialize_map(Root { out: &mut out, progress }).context("invalid JSON export")?;
    if !has_messages {
        bail!("the JSON export has no `messages` list (export a single chat)");
    }
    progress(out.messages.len());
    Ok(out)
}

/// The top-level object: `name` and the `messages` array, in any order.
struct Root<'a> {
    out: &'a mut ParsedHtml,
    progress: &'a dyn Fn(usize),
}

impl<'de> Visitor<'de> for Root<'_> {
    type Value = bool;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a chat export object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<bool, A::Error> {
        let mut has_messages = false;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "name" => self.out.title = map.next_value::<Option<String>>()?.unwrap_or_default().trim().to_string(),
                "messages" => {
                    map.next_value_seed(Messages { out: &mut *self.out, progress: self.progress })?;
                    has_messages = true;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(has_messages)
    }
}

/// The `messages` array, converted element by element.
struct Messages<'a> {
    out: &'a mut ParsedHtml,
    progress: &'a dyn Fn(usize),
}

impl<'de> DeserializeSeed<'de> for Messages<'_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for Messages<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a list of messages")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        let mut seen = 0usize;
        while let Some(m) = seq.next_element::<Value>()? {
            self.out.messages.extend(parse_message(&m));
            seen += 1;
            if seen % REPORT_EVERY == 0 {
                (self.progress)(self.out.messages.len());
            }
        }
        Ok(())
    }
}

fn str_of<'a>(m: &'a Value, key: &str) -> Option<&'a str> {
    m.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn date_of(m: &Value, key: &str) -> Option<String> {
    let s = str_of(m, key)?;
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok().map(|d| d.format("%Y-%m-%dT%H:%M:%S").to_string())
}

/// Paths of media that were skipped by the export read `(File not included. …)`.
fn path_of(m: &Value, key: &str) -> Option<String> {
    str_of(m, key).filter(|s| !s.starts_with('(')).map(String::from)
}

fn parse_message(m: &Value) -> Option<Message> {
    let id = m.get("id").and_then(Value::as_i64)?;
    let date = date_of(m, "date");
    let text = text_of(m.get("text"));
    if m.get("type").and_then(Value::as_str) == Some("service") {
        let actor = str_of(m, "actor").unwrap_or_default();
        if str_of(m, "action") == Some("phone_call") {
            let status = m.get("duration_seconds").and_then(Value::as_u64).map(duration);
            let media = Media { kind: Some(MediaKind::Call), status, ..Default::default() };
            return Some(Message { id, from: actor.into(), date, media: Some(media), ..Default::default() });
        }
        let text = service_text(m, actor, &text);
        return (!text.is_empty()).then(|| Message { id, date, service: true, text, ..Default::default() });
    }
    let mut msg = Message { id, date, text, ..Default::default() };
    msg.from = str_of(m, "from").unwrap_or("Deleted Account").to_string();
    msg.edited = m.get("edited").is_some_and(|e| !e.is_null());
    msg.reply_to = m.get("reply_to_message_id").and_then(Value::as_i64);
    msg.forwarded_from = str_of(m, "forwarded_from").map(String::from);
    msg.media = media_of(m);
    if let Some(rs) = m.get("reactions").and_then(Value::as_array) {
        for r in rs {
            let emoji = str_of(r, "emoji").unwrap_or_default().to_string();
            let from: Vec<String> = r
                .get("recent")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|x| str_of(x, "from").map(String::from))
                .collect();
            let count = r.get("count").and_then(Value::as_u64).unwrap_or(1) as u32;
            if !emoji.is_empty() {
                msg.reactions.push(Reaction { emoji, count, from });
            }
        }
    }
    Some(msg)
}

fn service_text(m: &Value, actor: &str, text: &str) -> String {
    let members = || {
        m.get("members")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
            .unwrap_or_default()
    };
    let title = str_of(m, "title").unwrap_or_default();
    let who = if actor.is_empty() { String::new() } else { format!("{actor} ") };
    let s = match str_of(m, "action").unwrap_or_default() {
        "create_group" | "create_channel" => format!("{who}created «{title}»"),
        "invite_members" => format!("{who}invited {}", members()),
        "remove_members" => format!("{who}removed {}", members()),
        "join_group_by_link" => format!("{who}joined the group via invite link"),
        "pin_message" => format!("{who}pinned a message"),
        "edit_group_title" => format!("{who}changed the group title to «{title}»"),
        "edit_group_photo" => format!("{who}changed the group photo"),
        "" => format!("{who}{text}"),
        other => {
            let name = other.replace('_', " ");
            let extra = if title.is_empty() { text.to_string() } else { format!("«{title}»") };
            format!("{who}{name} {extra}")
        }
    };
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn duration(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn size_label(bytes: u64) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

fn media_of(m: &Value) -> Option<Media> {
    let status_of_file = || {
        m.get("duration_seconds")
            .and_then(Value::as_u64)
            .map(duration)
            .or_else(|| m.get("file_size").and_then(Value::as_u64).map(size_label))
    };
    if m.get("photo").is_some() {
        return Some(Media { kind: Some(MediaKind::Photo), file: path_of(m, "photo"), ..Default::default() });
    }
    if m.get("file").is_some() {
        let kind = match str_of(m, "media_type") {
            Some("sticker") => MediaKind::Sticker,
            Some("animation") => MediaKind::Gif,
            Some("video_file") => MediaKind::Video,
            Some("voice_message") => MediaKind::Voice,
            Some("audio_file") => MediaKind::Audio,
            Some("video_message") => MediaKind::RoundVideo,
            _ => MediaKind::File,
        };
        let title = match kind {
            MediaKind::Gif => Some("GIF".to_string()),
            MediaKind::File | MediaKind::Video => str_of(m, "file_name").map(String::from),
            MediaKind::Audio => {
                let t = str_of(m, "title").or(str_of(m, "file_name"));
                match (str_of(m, "performer"), t) {
                    (Some(p), Some(t)) => Some(format!("{p} - {t}")),
                    (_, t) => t.map(String::from),
                }
            }
            _ => None,
        };
        return Some(Media {
            kind: Some(kind),
            file: path_of(m, "file"),
            thumb: path_of(m, "thumbnail"),
            title,
            status: status_of_file(),
            ..Default::default()
        });
    }
    if let Some(loc) = m.get("location_information") {
        let coord = |k: &str| loc.get(k).and_then(Value::as_f64).map(|x| format!("{x:.6}")).unwrap_or_default();
        let status = format!("{}, {}", coord("latitude"), coord("longitude"));
        return Some(Media { kind: Some(MediaKind::Location), status: Some(status), ..Default::default() });
    }
    if let Some(c) = m.get("contact_information") {
        let part = |k: &str| str_of(c, k).unwrap_or_default().to_string();
        let status = [format!("{} {}", part("first_name"), part("last_name")), part("phone_number")]
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        return Some(Media { kind: Some(MediaKind::Contact), status: Some(status).filter(|s| !s.is_empty()), ..Default::default() });
    }
    if let Some(p) = m.get("poll") {
        let answers: Vec<&str> =
            p.get("answers").and_then(Value::as_array).into_iter().flatten().filter_map(|a| str_of(a, "text")).collect();
        let status = format!("{} {}", str_of(p, "question").unwrap_or_default(), answers.join(" / ")).trim().to_string();
        return Some(Media { kind: Some(MediaKind::Poll), status: Some(status).filter(|s| !s.is_empty()), ..Default::default() });
    }
    None
}

/// Plain text of a `text` field: a string or an array of strings and entities.
fn text_of(v: Option<&Value>) -> String {
    let mut s = String::new();
    match v {
        Some(Value::String(t)) => s.push_str(t),
        Some(Value::Array(parts)) => {
            for p in parts {
                match p {
                    Value::String(t) => s.push_str(t),
                    Value::Object(_) => entity(p, &mut s),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    // Same normalization as the HTML path.
    s.lines().map(str::trim).collect::<Vec<_>>().join("\n").trim().to_string()
}

fn entity(p: &Value, out: &mut String) {
    let t = str_of(p, "text").unwrap_or_default();
    match str_of(p, "type").unwrap_or_default() {
        "text_link" => {
            let href = str_of(p, "href").unwrap_or_default();
            out.push_str(t);
            let shown = t.trim();
            let is_web = href.starts_with("http://") || href.starts_with("https://");
            if is_web && !shown.is_empty() && !href.to_lowercase().contains(&shown.to_lowercase()) && !shown.starts_with('@') {
                out.push_str(&format!(" ({href})"));
            }
        }
        "code" => out.push_str(&format!("`{t}`")),
        "pre" => out.push_str(&format!("```{t}```")),
        "blockquote" | "blockquote_expandable" => {
            out.push('\n');
            for line in t.trim().lines() {
                out.push_str("> ");
                out.push_str(line.trim());
                out.push('\n');
            }
        }
        _ => out.push_str(t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entities_to_plain_text() {
        let v: Value = serde_json::from_str(
            r#"["see ", {"type":"text_link","text":"docs","href":"https://example.com/d"}, " and ", {"type":"link","text":"https://x.org"},
            " ", {"type":"code","text":"a=1"}, {"type":"bold","text":"!"}]"#,
        )
        .unwrap();
        assert_eq!(text_of(Some(&v)), "see docs (https://example.com/d) and https://x.org `a=1`!");
    }

    #[test]
    fn skipped_files_have_no_path() {
        let v: Value = serde_json::from_str(r#"{"photo":"(File not included. Change data exporting settings to download.)"}"#).unwrap();
        assert_eq!(media_of(&v).unwrap().file, None);
    }
}
