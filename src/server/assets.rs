//! The web UI's files: `index.html` (with the inlined walkthrough), `app.js`, `app.css`
//! and icons, and the share-link pages that start as a loading skeleton.

use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};

pub(super) async fn guest_root() -> Response {
    let html = "<!doctype html><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>iRoof.top</title>\
<body style=\"font:16px/1.5 system-ui,sans-serif;display:grid;place-items:center;min-height:90vh;text-align:center\"><main><h1>iRoof.top</h1>\
<p>Open the share link you were sent: it looks like <code>/s/name</code>.</p></main></body>";
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

fn asset(body: &'static str, ctype: &'static str) -> Response {
    ([(header::CONTENT_TYPE, ctype), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

/// `index.html` with the Telegram export walkthrough inlined at `<!-- telegram-export.svg -->`.
/// The SVG is also a standalone file (used by the README), so it has a single source.
static INDEX_HTML: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    let svg = include_str!("../../web/telegram-export.svg");
    let svg = svg.find("<svg").map_or(svg, |i| &svg[i..]); // drop the XML prolog and comments
    include_str!("../../web/index.html").replacen("<!-- telegram-export.svg -->", &untitled(svg.trim_end()), 1)
});

/// Moves the `<title>` of an inline SVG into `aria-label`: browsers show a `<title>` as a
/// tooltip wherever the pointer rests on the drawing, screen readers read either.
fn untitled(svg: &str) -> String {
    let (Some(start), Some(end)) = (svg.find("<title"), svg.find("</title>")) else { return svg.to_string() };
    let Some(open_end) = svg[start..end].find('>').map(|i| start + i + 1) else { return svg.to_string() };
    let text = svg[open_end..end].trim().replace('"', "&quot;");
    let id = svg[start..open_end].split("id=\"").nth(1).and_then(|r| r.split('"').next()).unwrap_or_default();
    let mut out = format!("{}{}", &svg[..start], svg[end + "</title>".len()..].trim_start_matches([' ', '\n']));
    let labelled = format!("aria-labelledby=\"{id}\"");
    if !id.is_empty() && out.contains(&labelled) {
        out = out.replacen(&labelled, &format!("aria-label=\"{text}\""), 1);
    }
    out
}

pub(crate) async fn index() -> Response {
    page(INDEX_HTML.as_str())
}

/// A share link's pages: marked as a visitor's page and as still loading in the HTML
/// itself, so the first paint is a skeleton of the start page (`gate`) or of the chat, not
/// the owner's layout that app.js would replace a moment later.
fn guest_html(view: &str) -> String {
    let html = INDEX_HTML.replacen("<html lang=\"en\">", &format!("<html lang=\"en\" class=\"guest booting boot-{view}\">"), 1);
    html.replacen("<div class=\"app\" id=\"app\">", "<div class=\"app guest-mode\" id=\"app\">", 1)
}

static GUEST_GATE_HTML: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| guest_html("gate"));

static GUEST_CHAT_HTML: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| guest_html("chat"));

/// `/s/{slug}`: the start page of a share link.
pub(crate) async fn guest_gate_index() -> Response {
    page(GUEST_GATE_HTML.as_str())
}

/// `/s/{slug}/{token}`: a visitor's conversation.
pub(crate) async fn guest_chat_index() -> Response {
    page(GUEST_CHAT_HTML.as_str())
}

pub(super) fn page(html: &'static str) -> Response {
    let mut r = asset(html, "text/html; charset=utf-8");
    let h = r.headers_mut();
    // Share URLs carry a secret session token: never leak it via Referer.
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert("X-Frame-Options", HeaderValue::from_static("DENY"));
    h.insert("X-Content-Type-Options", HeaderValue::from_static("nosniff"));
    r
}

/// The UI's script: the parts in `web/js/`, in order, as one classic script (one request,
/// one scope; no build step). Each part is one area of the UI.
const APP_JS: &str = concat!(
    include_str!("../../web/js/01-core.js"),
    include_str!("../../web/js/02-utils.js"),
    include_str!("../../web/js/03-theme.js"),
    include_str!("../../web/js/04-sidebar.js"),
    include_str!("../../web/js/05-import.js"),
    include_str!("../../web/js/06-new-chat.js"),
    include_str!("../../web/js/07-chat-render.js"),
    include_str!("../../web/js/08-interactions.js"),
    include_str!("../../web/js/09-profile.js"),
    include_str!("../../web/js/10-picker.js"),
    include_str!("../../web/js/11-settings.js"),
    include_str!("../../web/js/12-media-library.js"),
    include_str!("../../web/js/13-lightbox.js"),
    include_str!("../../web/js/14-voice.js"),
    include_str!("../../web/js/15-clipboard.js"),
    include_str!("../../web/js/16-visitor.js"),
    include_str!("../../web/js/17-shares.js"),
    include_str!("../../web/js/18-home.js"),
    include_str!("../../web/js/19-boot.js"),
    include_str!("../../web/js/20-keyboard.js"),
);

pub(super) async fn app_js() -> Response {
    asset(APP_JS, "text/javascript; charset=utf-8")
}

pub(super) async fn app_css() -> Response {
    asset(include_str!("../../web/app.css"), "text/css; charset=utf-8")
}

/// Follows the light / dark color scheme of the browser.
pub(super) async fn favicon_svg() -> Response {
    asset(include_str!("../../web/favicon.svg"), "image/svg+xml")
}

/// 16, 32 and 48 px, for browsers and tools that only read `/favicon.ico`.
pub(super) async fn favicon_ico() -> Response {
    static ICO: &[u8] = include_bytes!("../../web/favicon.ico");
    ([(header::CONTENT_TYPE, "image/x-icon"), (header::CACHE_CONTROL, "no-cache")], ICO).into_response()
}
