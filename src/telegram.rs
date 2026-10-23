//! Parser for Telegram Desktop "Export chat history" HTML output
//! (`messages.html`, `messages2.html`, …).
//!
//! Pages are parsed independently and in parallel. What depends on the pages
//! before (the sender of a "joined" message, ids of messages without one, the
//! first avatar of each sender) is resolved afterwards in a single ordered pass.

use crate::import::{Progress, Report, Stage, par_map};
use crate::model::{LinkPreview, Media, MediaKind, Message, Reaction};
use anyhow::{Context, Result, bail};
use chrono::NaiveDateTime;
use scraper::{ElementRef, Html, Node, Selector};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Default)]
pub struct ParsedHtml {
    pub title: String,
    pub messages: Vec<Message>,
    /// (sender name, avatar path relative to export root)
    pub avatars: Vec<(String, String)>,
}

/// Selectors are parsed once: building one per element dominated the parse time.
struct Selectors {
    title_name: Selector,
    title_bold: Selector,
    title_tag: Selector,
    messages: Selector,
    body: Selector,
    userpic: Selector,
    link: Selector,
    quote: Selector,
    reaction: Selector,
    emoji: Selector,
    img: Selector,
    count: Selector,
    titled: Selector,
    title: Selector,
    status: Selector,
    site: Selector,
    web_title: Selector,
    web_description: Selector,
}

static SEL: LazyLock<Selectors> = LazyLock::new(|| {
    let s = |css: &str| Selector::parse(css).expect("static selector");
    Selectors {
        // macOS and Linux use `.name`, older Windows builds `.content .text.bold`,
        // and the generic `<title>` ("Exported Data") never names the chat.
        title_name: s(".page_header .name"),
        title_bold: s(".page_header .content .text.bold"),
        title_tag: s("title"),
        messages: s(".history > .message"),
        body: s(".body"),
        userpic: s(".userpic_wrap img.userpic"),
        link: s("a"),
        quote: s(".quote, blockquote"),
        reaction: s(".reaction"),
        emoji: s(".emoji"),
        img: s("img"),
        count: s(".count"),
        titled: s("[title]"),
        title: s(".title"),
        status: s(".status, .video_duration"),
        site: s(".webpage_site"),
        web_title: s(".webpage_title"),
        web_description: s(".webpage_description"),
    }
});

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

/// Parses every page of the export in `dir`, reporting after each finished page.
pub fn parse_export_dir(dir: &Path, report: Report) -> Result<ParsedHtml> {
    let paths = message_pages(dir)?;
    let (done, found) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let pages = par_map(&paths, |path| -> Result<Page> {
        let html = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let page = parse_page(&html);
        let messages = found.fetch_add(page.messages.len(), Ordering::Relaxed) + page.messages.len();
        let done = done.fetch_add(1, Ordering::Relaxed) + 1;
        report(Progress { stage: Stage::Parsing, done, total: paths.len(), messages });
        Ok(page)
    });
    Ok(merge(pages.into_iter().collect::<Result<Vec<_>>>()?))
}

/// Parses a single page (tests and small inputs).
pub fn parse_html(html: &str) -> ParsedHtml {
    merge(vec![parse_page(html)])
}

fn has_class(el: &ElementRef, class: &str) -> bool {
    el.value().classes().any(|c| c == class)
}

fn child_elements<'a>(el: &ElementRef<'a>) -> impl Iterator<Item = ElementRef<'a>> + 'a {
    el.children().filter_map(ElementRef::wrap)
}

fn first_desc<'a>(el: &ElementRef<'a>, selector: &Selector) -> Option<ElementRef<'a>> {
    el.select(selector).next()
}

fn plain(el: &ElementRef) -> String {
    el.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A message of one page, before the cross-page fix-ups of [`merge`].
struct PageMessage {
    msg: Message,
    avatar: Option<String>,
    /// 1-based position among the page's elements without a usable id.
    synthetic: Option<usize>,
}

struct Page {
    title: String,
    messages: Vec<PageMessage>,
    /// Elements without a usable id, kept or not: later pages continue the numbering after them.
    synthetic: usize,
}

fn parse_page(html: &str) -> Page {
    let doc = Html::parse_document(html);
    let title = [&SEL.title_name, &SEL.title_bold, &SEL.title_tag]
        .iter()
        .filter_map(|s| doc.select(s).next().map(|t| plain(&t)))
        .find(|t| !t.is_empty() && !t.eq_ignore_ascii_case("Exported Data"))
        .unwrap_or_default();
    let mut page = Page { title, messages: Vec::new(), synthetic: 0 };
    for el in doc.select(&SEL.messages) {
        let parsed_id = el.value().id().and_then(|s| s.strip_prefix("message")).and_then(|s| s.parse::<i64>().ok());
        let synthetic = parsed_id.is_none().then(|| {
            page.synthetic += 1;
            page.synthetic
        });
        let mut msg = Message { id: parsed_id.unwrap_or_default(), ..Default::default() };
        if has_class(&el, "service") {
            msg.text = first_desc(&el, &SEL.body).map(|b| plain(&b)).unwrap_or_default();
            // Day separators carry no information beyond message timestamps.
            if msg.text.is_empty() || parse_day(&msg.text).is_some() {
                continue;
            }
            msg.service = true;
            page.messages.push(PageMessage { msg, avatar: None, synthetic });
            continue;
        }
        let Some(body) = child_elements(&el).find(|c| has_class(c, "body")) else {
            continue;
        };
        parse_body(&body, &mut msg, true);
        let avatar = first_desc(&el, &SEL.userpic).and_then(|i| i.attr("src").map(String::from));
        page.messages.push(PageMessage { msg, avatar, synthetic });
    }
    page
}

/// Joins the pages in order and resolves what depends on earlier messages: a joined
/// message has no `from_name` and inherits the previous sender (also across pages),
/// ids missing from the markup count down from -2, and each sender keeps the first avatar seen.
fn merge(pages: Vec<Page>) -> ParsedHtml {
    let mut out = ParsedHtml { messages: Vec::with_capacity(pages.iter().map(|p| p.messages.len()).sum()), ..Default::default() };
    let mut last_sender = String::new();
    let mut synthetic_before = 0usize;
    let mut with_avatar: HashSet<String> = HashSet::new();
    for page in pages {
        if out.title.is_empty() {
            out.title = page.title;
        }
        for PageMessage { mut msg, avatar, synthetic } in page.messages {
            if let Some(k) = synthetic {
                msg.id = -((synthetic_before + k + 1) as i64);
            }
            if !msg.service {
                if msg.from.is_empty() {
                    msg.from = last_sender.clone();
                } else {
                    last_sender = msg.from.clone();
                }
                if let Some(src) = avatar
                    && with_avatar.insert(msg.from.clone())
                {
                    out.avatars.push((msg.from.clone(), src));
                }
            }
            out.messages.push(msg);
        }
        synthetic_before += page.synthetic;
    }
    out
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
            msg.reply_to = first_desc(&c, &SEL.link).and_then(|a| {
                let target = a.attr("onclick").or(a.attr("href")).unwrap_or_default();
                let digits: String =
                    target.rsplit(|ch: char| !ch.is_ascii_digit() && ch != '-').find(|s| !s.is_empty()).unwrap_or_default().to_string();
                digits.parse().ok()
            });
            if let Some(q) = first_desc(&c, &SEL.quote) {
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
            for r in c.select(&SEL.reaction) {
                let emoji = first_desc(&r, &SEL.emoji)
                    .map(|e| {
                        let t = plain(&e);
                        if t.is_empty() {
                            first_desc(&e, &SEL.img).and_then(|i| i.attr("alt").map(String::from)).unwrap_or_default()
                        } else {
                            t
                        }
                    })
                    .unwrap_or_default();
                let count = first_desc(&r, &SEL.count).and_then(|e| plain(&e).parse().ok()).unwrap_or(1);
                let from =
                    r.select(&SEL.titled).filter_map(|e| e.attr("title").map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect();
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
    let img_src = first_desc(el, &SEL.img).and_then(|i| i.attr("src").map(String::from));
    let title = first_desc(el, &SEL.title).map(|t| plain(&t)).filter(|s| !s.is_empty());
    let status = first_desc(el, &SEL.status).map(|t| plain(&t)).filter(|s| !s.is_empty());
    let kind = if has_class(el, "webpage_preview") {
        let url = href.clone().unwrap_or_default();
        msg.link_preview = Some(LinkPreview {
            url,
            site: first_desc(el, &SEL.site).map(|t| plain(&t)),
            title: first_desc(el, &SEL.web_title).map(|t| plain(&t)),
            description: first_desc(el, &SEL.web_description).map(|t| plain(&t)),
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
    msg.media = Some(Media { kind: Some(kind), library_id: None, file, thumb, url, title, status: detail, description: None, size: None });
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
