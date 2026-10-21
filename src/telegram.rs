//! Parser for Telegram Desktop "Export chat history" HTML output
//! (`messages.html`, `messages2.html`, …).

use crate::model::{LinkPreview, Media, MediaKind, Message, Reaction};
use anyhow::{Context, Result, bail};
use chrono::NaiveDateTime;
use scraper::{ElementRef, Html, Node, Selector};
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct ParsedHtml {
    pub title: String,
    pub messages: Vec<Message>,
    /// (sender name, avatar path relative to export root)
    pub avatars: Vec<(String, String)>,
}

/// Returns `messages.html`, `messages2.html`, … in page order.
pub fn message_pages(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut pages: Vec<(u32, PathBuf)> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let n = name.strip_prefix("messages")?.strip_suffix(".html")?;
            let idx = if n.is_empty() { 1 } else { n.parse().ok()? };
            Some((idx, e.path()))
        })
        .collect();
    if pages.is_empty() {
        bail!("no messages.html found in {}", dir.display());
    }
    pages.sort();
    Ok(pages.into_iter().map(|(_, p)| p).collect())
}

pub fn parse_export_dir(dir: &Path) -> Result<ParsedHtml> {
    let mut out = ParsedHtml::default();
    let mut last_sender = String::new();
    let mut synthetic_id = -1i64;
    for page in message_pages(dir)? {
        let html = std::fs::read_to_string(&page).with_context(|| format!("reading {}", page.display()))?;
        parse_page(&html, &mut out, &mut last_sender, &mut synthetic_id);
    }
    Ok(out)
}

pub fn parse_html(html: &str) -> ParsedHtml {
    let mut out = ParsedHtml::default();
    parse_page(html, &mut out, &mut String::new(), &mut -1);
    out
}

fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("static selector")
}

fn has_class(el: &ElementRef, class: &str) -> bool {
    el.value().classes().any(|c| c == class)
}

fn child_elements<'a>(el: &ElementRef<'a>) -> impl Iterator<Item = ElementRef<'a>> + 'a {
    el.children().filter_map(ElementRef::wrap)
}

fn first_desc<'a>(el: &ElementRef<'a>, selector: &str) -> Option<ElementRef<'a>> {
    el.select(&sel(selector)).next()
}

fn plain(el: &ElementRef) -> String {
    el.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_page(html: &str, out: &mut ParsedHtml, last_sender: &mut String, synthetic_id: &mut i64) {
    let doc = Html::parse_document(html);
    if out.title.is_empty() {
        // macOS and Linux use `.name`, older Windows builds `.content .text.bold`,
        // and the generic `<title>` ("Exported Data") never names the chat.
        out.title = [".page_header .name", ".page_header .content .text.bold", "title"]
            .iter()
            .filter_map(|s| doc.select(&sel(s)).next().map(|t| plain(&t)))
            .find(|t| !t.is_empty() && !t.eq_ignore_ascii_case("Exported Data"))
            .unwrap_or_default();
    }
    for el in doc.select(&sel(".history > .message")) {
        let id = el.value().id().and_then(|s| s.strip_prefix("message")).and_then(|s| s.parse::<i64>().ok()).unwrap_or_else(|| {
            *synthetic_id -= 1;
            *synthetic_id
        });
        if has_class(&el, "service") {
            let text = first_desc(&el, ".body").map(|b| plain(&b)).unwrap_or_default();
            // Day separators carry no information beyond message timestamps.
            if text.is_empty() || parse_day(&text).is_some() {
                continue;
            }
            out.messages.push(Message { id, service: true, text, ..Default::default() });
            continue;
        }
        let Some(body) = child_elements(&el).find(|c| has_class(c, "body")) else {
            continue;
        };
        let mut msg = Message { id, ..Default::default() };
        parse_body(&body, &mut msg, true);
        if msg.from.is_empty() {
            msg.from = last_sender.clone();
        } else {
            *last_sender = msg.from.clone();
        }
        if let Some(src) = first_desc(&el, ".userpic_wrap img.userpic").and_then(|i| i.attr("src").map(String::from))
            && !out.avatars.iter().any(|(n, _)| n == &msg.from)
        {
            out.avatars.push((msg.from.clone(), src));
        }
        out.messages.push(msg);
    }
}

/// `top` is false while parsing a nested `forwarded body`: there `from_name`
/// names the original author rather than the sender.
fn parse_body(body: &ElementRef, msg: &mut Message, top: bool) {
    for c in child_elements(body) {
        if has_class(&c, "date") && has_class(&c, "details") {
            if top {
                let title = c.attr("title").unwrap_or_default();
                msg.date = parse_timestamp(title).map(|d| d.format("%Y-%m-%dT%H:%M:%S").to_string());
                let shown = plain(&c).to_lowercase();
                msg.edited = shown.contains("edited") || title.to_lowercase().contains("edited");
            }
        } else if has_class(&c, "from_name") {
            // Strip the nested date span forwarded headers carry.
            let name: String = c
                .children()
                .filter_map(|n| match n.value() {
                    Node::Text(t) => Some(t.to_string()),
                    _ => None,
                })
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let name = if name.is_empty() { plain(&c) } else { name };
            if top {
                msg.from = name;
            } else if msg.forwarded_from.is_none() {
                msg.forwarded_from = Some(name);
            }
        } else if has_class(&c, "reply_to") {
            msg.reply_to = first_desc(&c, "a").and_then(|a| {
                let target = a.attr("onclick").or(a.attr("href")).unwrap_or_default();
                let digits: String =
                    target.rsplit(|ch: char| !ch.is_ascii_digit() && ch != '-').find(|s| !s.is_empty()).unwrap_or_default().to_string();
                digits.parse().ok()
            });
            if let Some(q) = first_desc(&c, ".quote, blockquote") {
                msg.quote = Some(plain(&q)).filter(|s| !s.is_empty());
            }
        } else if has_class(&c, "forwarded_from") {
            let t = plain(&c);
            msg.forwarded_from = Some(t.strip_prefix("Forwarded from").unwrap_or(&t).trim().to_string());
        } else if has_class(&c, "forwarded") && has_class(&c, "body") {
            parse_body(&c, msg, false);
            if msg.forwarded_from.is_none() {
                msg.forwarded_from = Some(String::new());
            }
        } else if has_class(&c, "text") {
            let t = rich_text(&c);
            if !t.is_empty() {
                if !msg.text.is_empty() {
                    msg.text.push_str("\n\n");
                }
                msg.text.push_str(&t);
            }
        } else if has_class(&c, "reactions") {
            for r in c.select(&sel(".reaction")) {
                let emoji = first_desc(&r, ".emoji")
                    .map(|e| {
                        let t = plain(&e);
                        if t.is_empty() {
                            first_desc(&e, "img").and_then(|i| i.attr("alt").map(String::from)).unwrap_or_default()
                        } else {
                            t
                        }
                    })
                    .unwrap_or_default();
                let count = first_desc(&r, ".count").and_then(|e| plain(&e).parse().ok()).unwrap_or(1);
                let from = r
                    .select(&sel("[title]"))
                    .filter_map(|e| e.attr("title").map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect();
                if !emoji.is_empty() {
                    msg.reactions.push(Reaction { emoji, count, from });
                }
            }
        } else if has_class(&c, "media_wrap") {
            for m in child_elements(&c) {
                parse_media_el(&m, msg);
            }
        } else {
            parse_media_el(&c, msg);
        }
    }
}

fn parse_media_el(el: &ElementRef, msg: &mut Message) {
    let href = el.attr("href").map(String::from);
    let img_src = first_desc(el, "img").and_then(|i| i.attr("src").map(String::from));
    let title = first_desc(el, ".title").map(|t| plain(&t)).filter(|s| !s.is_empty());
    let status = first_desc(el, ".status, .video_duration").map(|t| plain(&t)).filter(|s| !s.is_empty());
    let kind = if has_class(el, "webpage_preview") {
        let url = href.clone().unwrap_or_default();
        msg.link_preview = Some(LinkPreview {
            url,
            site: first_desc(el, ".webpage_site").map(|t| plain(&t)),
            title: first_desc(el, ".webpage_title").map(|t| plain(&t)),
            description: first_desc(el, ".webpage_description").map(|t| plain(&t)),
            photo: img_src.filter(|s| !s.starts_with("http")),
        });
        return;
    } else if has_class(el, "photo_wrap") || has_class(el, "media_photo") {
        MediaKind::Photo
    } else if has_class(el, "sticker_wrap") {
        MediaKind::Sticker
    } else if has_class(el, "animated_wrap") {
        MediaKind::Gif
    } else if has_class(el, "video_file_wrap") {
        let round = has_class(el, "round") || href.as_deref().is_some_and(|h| h.starts_with("round_video_messages/"));
        if round { MediaKind::RoundVideo } else { MediaKind::Video }
    } else if has_class(el, "media_video") {
        let gif = href.as_deref().is_some_and(|h| h.starts_with("gifs/")) || title.as_deref() == Some("GIF");
        if gif { MediaKind::Gif } else { MediaKind::Video }
    } else if has_class(el, "media_voice_message") {
        MediaKind::Voice
    } else if has_class(el, "media_audio_file") {
        MediaKind::Audio
    } else if has_class(el, "media_file") {
        MediaKind::File
    } else if has_class(el, "media_location") || has_class(el, "media_live_location") {
        MediaKind::Location
    } else if has_class(el, "media_contact") {
        MediaKind::Contact
    } else if has_class(el, "media_poll") || has_class(el, "poll") {
        MediaKind::Poll
    } else if has_class(el, "media_call") {
        MediaKind::Call
    } else if has_class(el, "media") {
        MediaKind::Other
    } else {
        return;
    };
    let external = |h: &str| h.starts_with("http://") || h.starts_with("https://");
    let (file, url) = match href {
        Some(h) if external(&h) => (None, Some(h)),
        Some(h) if h.starts_with('#') || h.starts_with("javascript:") => (None, None),
        Some(h) => (Some(h), None),
        None => (None, None),
    };
    let thumb = img_src.filter(|s| Some(s) != file.as_ref() && !external(s));
    let mut detail = status;
    if matches!(kind, MediaKind::Location | MediaKind::Contact | MediaKind::Poll | MediaKind::Call) {
        // Keep the human-readable card content.
        let t = plain(el);
        if !t.is_empty() {
            detail = Some(t);
        }
    }
    msg.media = Some(Media { kind: Some(kind), library_id: None, file, thumb, url, title, status: detail, description: None });
}

/// Converts a `.text` element into plain text, keeping line breaks, quotes and links.
pub fn rich_text(el: &ElementRef) -> String {
    let mut s = String::new();
    walk(el, &mut s);
    s.lines().map(str::trim).collect::<Vec<_>>().join("\n").trim().to_string()
}

fn walk(el: &ElementRef, out: &mut String) {
    for node in el.children() {
        match node.value() {
            Node::Text(t) => out.push_str(&t.replace('\n', " ")),
            Node::Element(e) => {
                let child = ElementRef::wrap(node).expect("element");
                match e.name() {
                    "br" => out.push('\n'),
                    "img" => {
                        if let Some(alt) = e.attr("alt") {
                            out.push_str(alt);
                        }
                    }
                    "a" => {
                        let mut inner = String::new();
                        walk(&child, &mut inner);
                        let href = e.attr("href").unwrap_or_default();
                        let shown = inner.trim();
                        out.push_str(&inner);
                        let is_web = href.starts_with("http://") || href.starts_with("https://");
                        if is_web && !shown.is_empty() && !href.to_lowercase().contains(&shown.to_lowercase()) && !shown.starts_with('@') {
                            out.push_str(&format!(" ({href})"));
                        }
                    }
                    "blockquote" => {
                        let mut inner = String::new();
                        walk(&child, &mut inner);
                        out.push('\n');
                        for line in inner.trim().lines() {
                            out.push_str("> ");
                            out.push_str(line.trim());
                            out.push('\n');
                        }
                    }
                    "pre" | "code" => {
                        let fence = if e.name() == "pre" { "```" } else { "`" };
                        out.push_str(fence);
                        out.push_str(&child.text().collect::<String>());
                        out.push_str(fence);
                    }
                    _ => walk(&child, out),
                }
            }
            _ => {}
        }
    }
}

pub fn parse_timestamp(title: &str) -> Option<NaiveDateTime> {
    // "15 September 2026, 16:41:15", "15.09.2026 16:41:15 UTC+03:00" or
    // "15.09.2026 16:41:15 GMT+03:00", possibly followed by an "Edited: …" line.
    // The local wall-clock time is kept as is, the offset is dropped.
    let first = title.lines().next().unwrap_or_default().trim();
    let base = first.split(" UTC").next().unwrap_or(first).split(" GMT").next().unwrap_or(first).trim();
    ["%d %B %Y, %H:%M:%S", "%d.%m.%Y %H:%M:%S", "%d %B %Y %H:%M:%S", "%Y-%m-%d %H:%M:%S"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(base, f).ok())
}

fn parse_day(s: &str) -> Option<chrono::NaiveDate> {
    ["%d %B %Y", "%B %d, %Y", "%d.%m.%Y"].iter().find_map(|f| chrono::NaiveDate::parse_from_str(s.trim(), f).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        assert!(parse_timestamp("15 September 2026, 16:41:15").is_some());
        assert!(parse_timestamp("15.09.2026 16:41:15 UTC+03:00").is_some());
        let gmt = parse_timestamp("21.10.2026 18:06:02 GMT+03:00").unwrap();
        assert_eq!(gmt.format("%Y-%m-%dT%H:%M:%S").to_string(), "2026-10-21T18:06:02");
        assert!(parse_timestamp("nope").is_none());
    }

    #[test]
    fn forwarded_body_and_quote() {
        let html = r##"<div class="history">
<div class="message default clearfix" id="message10"><div class="body">
<div class="pull_right date details" title="01.02.2025 10:00:00 UTC+03:00">10:00</div>
<div class="from_name">Bob</div>
<div class="reply_to details">In reply to <a href="#go_to_message9" onclick="return GoToMessage(9)">this message</a><div class="quote">partial bit</div></div>
<div class="forwarded body"><div class="from_name">Channel X <span class="date details" title="x"> 01.01.2025</span></div>
<div class="text">Post <a href="https://example.com/a">link</a></div></div>
</div></div></div>"##;
        let p = parse_html(html);
        let m = &p.messages[0];
        assert_eq!(m.from, "Bob");
        assert_eq!(m.reply_to, Some(9));
        assert_eq!(m.quote.as_deref(), Some("partial bit"));
        assert_eq!(m.forwarded_from.as_deref(), Some("Channel X"));
        assert_eq!(m.text, "Post link (https://example.com/a)");
    }
}
