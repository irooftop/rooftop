//! Parser for WhatsApp "Export chat" text files (Android and iOS).
//!
//! Supported line headers (the marks U+200E and U+200F may precede them):
//! - Android 12h: `10/21/26, 6:17<U+202F>PM - Name: text`
//! - Android 24h: `21/10/2026, 18:06 - Name: text` and `21.10.26, 18:06 - Name: text`
//! - iOS: `[21/10/2026, 18:06:02] Name: text` and `[10/21/26, 6:17:02 AM] Name: text`
//!
//! Lines without a header continue the previous message. The day/month order
//! is detected per file from the data.
//! Messages that carry no information for a persona are dropped:
//! the end-to-end encryption notice, deleted messages and a lone `null`
//! (view-once media). Media that were not exported (`<Media omitted>`) become
//! media without a file.

use crate::import::safe_rel;
use crate::model::{Media, MediaKind, Message};
use crate::telegram::ParsedHtml;
use anyhow::{Context, Result};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use std::path::{Path, PathBuf};

const MARKS: &[char] = &['\u{200e}', '\u{200f}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{feff}'];
const EDITED: &str = "<This message was edited>";

fn clean(s: &str) -> &str {
    s.trim_matches(|c: char| MARKS.contains(&c) || c.is_whitespace())
}

#[derive(Debug, Clone, Copy)]
struct Header<'a> {
    a: u32,
    b: u32,
    c: u32,
    /// Year first (`2026-10-21`), so the day/month order is fixed.
    ymd: bool,
    hour: u32,
    min: u32,
    sec: u32,
    pm: Option<bool>,
    ios: bool,
    rest: &'a str,
}

fn numbers(s: &str, sep: &[char]) -> Option<Vec<u32>> {
    s.split(sep)
        .map(|p| {
            let p = p.trim();
            (!p.is_empty() && p.len() <= 4 && p.chars().all(|c| c.is_ascii_digit())).then(|| p.parse().ok()).flatten()
        })
        .collect()
}

fn parse_header(line: &str) -> Option<Header<'_>> {
    let line = line.trim_start_matches(MARKS);
    let (stamp, rest, ios) = if let Some(r) = line.strip_prefix('[') {
        let end = r.find(']')?;
        (&r[..end], r[end + 1..].trim_start(), true)
    } else {
        let end = [line.find(" - "), line.find(" \u{2013} ")].into_iter().flatten().min()?;
        let sep = line[end..].chars().nth(1)?.len_utf8();
        (&line[..end], &line[end + 2 + sep..], false)
    };
    if stamp.len() > 48 {
        return None;
    }
    let stamp: String = stamp.chars().map(|c| if c == '\u{202f}' || c == '\u{a0}' { ' ' } else { c }).collect();
    let stamp = stamp.trim();
    let (date, time) = match stamp.split_once(',') {
        Some((d, t)) => (d, t),
        None => stamp.split_once(' ')?,
    };
    let d = numbers(date, &['/', '.', '-'])?;
    if d.len() != 3 {
        return None;
    }
    let time = time.to_lowercase().replace(['.', ' '], "");
    let (time, pm) = if let Some(t) = time.strip_suffix("pm") {
        (t.to_string(), Some(true))
    } else if let Some(t) = time.strip_suffix("am") {
        (t.to_string(), Some(false))
    } else {
        (time, None)
    };
    let t = numbers(&time, &[':'])?;
    if !(2..=3).contains(&t.len()) {
        return None;
    }
    let (hour, min, sec) = (t[0], t[1], t.get(2).copied().unwrap_or(0));
    let hour_ok = if pm.is_some() { hour <= 12 } else { hour <= 23 };
    if !hour_ok || min > 59 || sec > 59 {
        return None;
    }
    let ymd = date.trim().split(['/', '.', '-']).next().is_some_and(|p| p.trim().len() == 4);
    Some(Header { a: d[0], b: d[1], c: d[2], ymd, hour, min, sec, pm, ios, rest })
}

fn year(y: u32) -> i32 {
    match y {
        0..=69 => 2000 + y as i32,
        70..=99 => 1900 + y as i32,
        _ => y as i32,
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Order {
    DayFirst,
    MonthFirst,
}

impl Header<'_> {
    fn datetime(&self, order: Order) -> Option<NaiveDateTime> {
        let (y, m, d) = if self.ymd {
            (self.a as i32, self.b, self.c)
        } else if order == Order::DayFirst {
            (year(self.c), self.b, self.a)
        } else {
            (year(self.c), self.a, self.b)
        };
        let hour = match self.pm {
            Some(pm) => self.hour % 12 + if pm { 12 } else { 0 },
            None => self.hour,
        };
        Some(NaiveDate::from_ymd_opt(y, m, d)?.and_time(NaiveTime::from_hms_opt(hour, self.min, self.sec)?))
    }
}

/// Day/month order for the whole file. A value above 12 decides, otherwise
/// month first for 12h clocks and day first for 24h clocks.
fn detect_order(headers: &[Header]) -> Order {
    let plain: Vec<&Header> = headers.iter().filter(|h| !h.ymd).collect();
    let all = |o: Order| plain.iter().all(|h| h.datetime(o).is_some());
    match (all(Order::DayFirst), all(Order::MonthFirst)) {
        (true, false) => Order::DayFirst,
        (false, true) => Order::MonthFirst,
        _ if plain.iter().any(|h| h.pm.is_some()) => Order::MonthFirst,
        _ => Order::DayFirst,
    }
}

/// Splits `Name: text`, returning `None` for system lines.
fn split_sender(rest: &str) -> Option<(&str, &str)> {
    let (name, text) = match rest.find(": ") {
        Some(i) => (&rest[..i], &rest[i + 2..]),
        None => (rest.strip_suffix(':')?, ""),
    };
    let name = clean(name);
    let plausible = !name.is_empty()
        && name.chars().count() <= 60
        && name.split_whitespace().count() <= 6
        && !name.contains(['"', '\u{201c}', '\u{201d}', '\u{ab}'])
        && !name.starts_with("Welcome to")
        && !name.contains("end-to-end");
    plausible.then_some((name, text))
}

fn sender_name(n: &str) -> String {
    // iOS marks contacts that are not saved with a leading tilde.
    n.trim_start_matches('~').trim().to_string()
}

/// Kind from the WhatsApp attachment naming scheme, falling back to the extension.
pub fn kind_from_name(name: &str) -> MediaKind {
    let n = name.rsplit('/').next().unwrap_or(name).to_lowercase();
    let ext = n.rsplit_once('.').map(|(_, e)| e).unwrap_or_default();
    let android = |p: &str| n.starts_with(p) && n.contains("-wa");
    if android("img-") {
        MediaKind::Photo
    } else if android("vid-") {
        MediaKind::Video
    } else if android("ptt-") {
        MediaKind::Voice
    } else if android("aud-") {
        MediaKind::Audio
    } else if android("stk-") {
        MediaKind::Sticker
    } else if android("doc-") {
        MediaKind::File
    } else if android("gif-") || n.contains("-gif-") {
        MediaKind::Gif
    } else if n.contains("-photo-") {
        MediaKind::Photo
    } else if n.contains("-video-") {
        MediaKind::Video
    } else if n.contains("-audio-") {
        if ext == "opus" { MediaKind::Voice } else { MediaKind::Audio }
    } else if n.contains("-sticker-") {
        MediaKind::Sticker
    } else {
        match ext {
            "jpg" | "jpeg" | "png" | "heic" | "heif" | "bmp" => MediaKind::Photo,
            "webp" => MediaKind::Sticker,
            "gif" => MediaKind::Gif,
            "mp4" | "mov" | "3gp" | "mkv" | "webm" | "avi" => MediaKind::Video,
            "opus" | "ogg" | "oga" => MediaKind::Voice,
            "mp3" | "m4a" | "aac" | "wav" | "amr" | "flac" => MediaKind::Audio,
            _ => MediaKind::File,
        }
    }
}

fn omitted_kind(line: &str) -> Option<MediaKind> {
    let l = clean(line).trim_end_matches('.').trim_matches(['<', '>']).to_lowercase();
    Some(match l.as_str() {
        "media omitted" => MediaKind::Other,
        "image omitted" => MediaKind::Photo,
        "video omitted" => MediaKind::Video,
        "sticker omitted" => MediaKind::Sticker,
        "audio omitted" => MediaKind::Audio,
        "gif omitted" => MediaKind::Gif,
        "document omitted" => MediaKind::File,
        "contact card omitted" => MediaKind::Contact,
        _ => return None,
    })
}

fn attachment(name: &str, exists: &dyn Fn(&str) -> bool) -> Media {
    let kind = kind_from_name(name);
    let rel = safe_rel(name).map(|p| p.to_string_lossy().replace('\\', "/")).filter(|r| exists(r));
    let title = (kind == MediaKind::File).then(|| name.to_string());
    Media { kind: Some(kind), file: rel, title, ..Default::default() }
}

enum Item {
    Skip,
    Service(String),
    Msg { text: String, edited: bool, media: Option<Media> },
}

fn is_e2e_notice(text: &str) -> bool {
    let l = clean(text).to_lowercase();
    l.starts_with("messages and calls are") && l.contains("end-to-end")
}

fn is_deleted(text: &str) -> bool {
    let l = clean(text).trim_end_matches('.').to_lowercase();
    l == "this message was deleted" || l == "you deleted this message" || l == "null"
}

fn classify(lines: &[&str], ios: bool, exists: &dyn Fn(&str) -> bool) -> Item {
    let lead_mark = lines.first().is_some_and(|l| l.starts_with('\u{200e}'));
    let joined = lines.join("\n");
    let mut body = clean(&joined).to_string();
    if body.contains(EDITED) {
        body = clean(&body.replace(EDITED, "")).to_string();
        return match classify_body(&body, ios, lead_mark, exists) {
            Item::Msg { text, media, .. } => Item::Msg { text, edited: true, media },
            other => other,
        };
    }
    classify_body(&body, ios, lead_mark, exists)
}

fn classify_body(body: &str, ios: bool, lead_mark: bool, exists: &dyn Fn(&str) -> bool) -> Item {
    if body.is_empty() || is_e2e_notice(body) || is_deleted(body) {
        return Item::Skip;
    }
    let lines: Vec<&str> = body.lines().collect();
    let mut media = None;
    let mut caption: Vec<String> = Vec::new();
    if ios && let Some(i) = lines.iter().position(|l| l.contains("<attached:")) {
        let line = lines[i];
        let start = line.find("<attached:").unwrap_or(0);
        let after = &line[start + "<attached:".len()..];
        let end = after.find('>').unwrap_or(after.len());
        let name = clean(&after[..end]);
        if !name.is_empty() {
            media = Some(attachment(name, exists));
            let rest = format!("{}{}", &line[..start], after.get(end + 1..).unwrap_or_default());
            caption = lines.iter().enumerate().map(|(j, l)| if j == i { rest.clone() } else { (*l).to_string() }).collect();
        }
    }
    if media.is_none()
        && !ios
        && let Some(name) = clean(lines[0]).strip_suffix("(file attached)").map(clean).filter(|n| !n.is_empty())
    {
        media = Some(attachment(name, exists));
        caption = lines[1..].iter().map(|l| (*l).to_string()).collect();
    }
    if media.is_none()
        && let Some(kind) = omitted_kind(lines[0])
    {
        media = Some(Media { kind: Some(kind), ..Default::default() });
        caption = lines[1..].iter().map(|l| (*l).to_string()).collect();
    }
    if media.is_none() {
        if ios && lead_mark {
            return Item::Service(body.to_string());
        }
        return Item::Msg { text: body.to_string(), edited: false, media: None };
    }
    let text = clean(&caption.join("\n")).to_string();
    Item::Msg { text, edited: false, media }
}

struct Raw<'a> {
    header: Header<'a>,
    cont: Vec<&'a str>,
}

/// Parses the text of a chat file. `title_hint` comes from the file name and
/// `exists` tells whether an attachment file is present in the export.
pub fn parse_text(text: &str, title_hint: &str, exists: &dyn Fn(&str) -> bool) -> ParsedHtml {
    let mut raws: Vec<Raw> = Vec::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        // A header needs a date valid in at least one order, otherwise it is just text.
        let header =
            parse_header(line).filter(|h| h.ymd || h.datetime(Order::DayFirst).is_some() || h.datetime(Order::MonthFirst).is_some());
        match (header, raws.last_mut()) {
            (Some(header), _) => raws.push(Raw { header, cont: Vec::new() }),
            (None, Some(last)) => last.cont.push(line),
            (None, None) => {}
        }
    }
    let headers: Vec<Header> = raws.iter().map(|r| r.header).collect();
    let order = detect_order(&headers);
    let other = if order == Order::DayFirst { Order::MonthFirst } else { Order::DayFirst };

    let mut out = ParsedHtml::default();
    let mut counts: Vec<(String, usize)> = Vec::new();
    for raw in &raws {
        let h = &raw.header;
        let date = h.datetime(order).or_else(|| h.datetime(other)).map(|d| d.format("%Y-%m-%dT%H:%M:%S").to_string());
        let sender = split_sender(h.rest);
        let first = sender.map_or(h.rest, |(_, t)| t);
        let mut lines = vec![first];
        lines.extend(raw.cont.iter().copied());
        let id = out.messages.len() as i64 + 1;
        match (classify(&lines, h.ios, exists), sender) {
            (Item::Skip, _) => {}
            (Item::Service(text), _) | (Item::Msg { text, .. }, None) => {
                let text = if sender.is_none() { clean(&text).to_string() } else { text };
                out.messages.push(Message { id, date, service: true, text, ..Default::default() });
            }
            (Item::Msg { text, edited, media }, Some((name, _))) => {
                let from = sender_name(name);
                match counts.iter_mut().find(|(n, _)| *n == from) {
                    Some((_, c)) => *c += 1,
                    None => counts.push((from.clone(), 1)),
                }
                out.messages.push(Message { id, from, date, edited, text, media, ..Default::default() });
            }
        }
    }
    out.title = if !title_hint.is_empty() {
        title_hint.to_string()
    } else {
        // The exporting user is not recorded, so the busiest sender stands in for the chat.
        counts.iter().rev().max_by_key(|c| c.1).map_or_else(|| "WhatsApp chat".into(), |c| c.0.clone())
    };
    out
}

/// Chat title from `WhatsApp Chat with <name>.txt`, empty for other names.
pub fn title_from_file_name(file: &str) -> String {
    let stem = file.strip_suffix(".txt").or_else(|| file.strip_suffix(".TXT")).unwrap_or(file);
    ["WhatsApp Chat with ", "WhatsApp Chat - ", "WhatsApp Chat "]
        .iter()
        .find_map(|p| stem.strip_prefix(p))
        .map(|t| t.trim().to_string())
        .unwrap_or_default()
}

/// Whether the first lines of the file look like a WhatsApp chat.
pub fn is_whatsapp_txt(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut buf = vec![0u8; 16 * 1024];
    let n = f.read(&mut buf).unwrap_or(0);
    let text = String::from_utf8_lossy(&buf[..n]);
    text.trim_start_matches('\u{feff}').lines().take(40).any(|l| parse_header(l).is_some())
}

/// Picks the chat file inside `dir` (`_chat.txt`, `WhatsApp Chat …`, then any `.txt` that looks like a chat).
pub fn find_chat_file(dir: &Path) -> Option<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("txt"))
                && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })
        .collect();
    let rank = |p: &PathBuf| {
        let n = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if n == "_chat.txt" {
            0
        } else if n.starts_with("WhatsApp Chat") {
            1
        } else {
            2
        }
    };
    files.sort_by_key(|p| (rank(p), p.clone()));
    files.into_iter().find(|p| is_whatsapp_txt(p))
}

/// Parses a chat file, attachments are looked up next to it.
pub fn parse_file(txt: &Path) -> Result<ParsedHtml> {
    let bytes = std::fs::read(txt).with_context(|| format!("reading {}", txt.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let root = txt.parent().unwrap_or(Path::new("."));
    let name = txt.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let exists = |rel: &str| safe_rel(rel).is_some_and(|r| root.join(r).is_file());
    Ok(parse_text(&text, &title_from_file_name(&name), &exists))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> ParsedHtml {
        parse_text(text, "", &|_| false)
    }

    #[test]
    fn android_12h_with_narrow_space() {
        let p = parse("10/21/26, 6:17\u{202f}PM - Ann: hi\n10/21/26, 12:05\u{202f}AM - Bob: night\n");
        assert_eq!(p.messages[0].date.as_deref(), Some("2026-10-21T18:17:00"));
        assert_eq!(p.messages[1].date.as_deref(), Some("2026-10-21T00:05:00"));
    }

    #[test]
    fn order_detection() {
        // 24h defaults to day first, a value above 12 decides.
        let p = parse("03/04/2025, 10:00 - Ann: a\n");
        assert_eq!(p.messages[0].date.as_deref(), Some("2025-04-03T10:00:00"));
        let p = parse("03/04/2025, 10:00 - Ann: a\n04/13/2025, 10:00 - Ann: b\n");
        assert_eq!(p.messages[0].date.as_deref(), Some("2025-03-04T10:00:00"));
        // 12h defaults to month first.
        let p = parse("03/04/25, 10:00 AM - Ann: a\n");
        assert_eq!(p.messages[0].date.as_deref(), Some("2025-03-04T10:00:00"));
    }

    #[test]
    fn kinds() {
        assert_eq!(kind_from_name("IMG-20240101-WA0001.jpg"), MediaKind::Photo);
        assert_eq!(kind_from_name("00000012-PHOTO-2024-01-01-10-00-00.jpg"), MediaKind::Photo);
        assert_eq!(kind_from_name("00000013-AUDIO-2024-01-01-10-00-00.opus"), MediaKind::Voice);
        assert_eq!(kind_from_name("PTT-20240101-WA0002.opus"), MediaKind::Voice);
        assert_eq!(kind_from_name("report.pdf"), MediaKind::File);
    }

    #[test]
    fn title_from_name() {
        assert_eq!(title_from_file_name("WhatsApp Chat with General.txt"), "General");
        assert_eq!(title_from_file_name("_chat.txt"), "");
    }
}
