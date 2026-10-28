#!/usr/bin/env python3
"""Writes the splash page in every language, plus sitemap.xml, from the English page.

deploy/landing/index.html is the English page and the template for the others. Between the marker
pairs <!-- i18n:head -->, <!-- i18n:faq --> and <!-- i18n:langs --> the generator writes its own
markup: hreflang and Open Graph tags, the questions (with FAQPage data) and the language list.
Every other language replaces the English text with scripts/landing-i18n/<dir>.json, keyed by the
exact English string; a language file must cover every key, so nothing is left in English by mistake.

Output: deploy/landing/index.html (English), deploy/landing/<dir>/index.html, deploy/landing/sitemap.xml.
Run after changing the page or a translation (after scripts/landing-demo.py, which it preserves):
    python3 scripts/landing-i18n.py
"""
import html
import json
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
LANDING = ROOT / "deploy" / "landing"
I18N = ROOT / "scripts" / "landing-i18n"
SITE = "https://iroof.top"

# (directory, hreflang, og:locale, direction, name in the language itself). The first entry is the default page, "/".
LANGS = [
    ("", "en", "en_US", "ltr", "English"),
    ("es", "es", "es_ES", "ltr", "Español"),
    ("zh-hans", "zh-Hans", "zh_CN", "ltr", "简体中文"),
    ("hi", "hi", "hi_IN", "ltr", "हिन्दी"),
    ("ar", "ar", "ar_AR", "rtl", "العربية"),
    ("fr", "fr", "fr_FR", "ltr", "Français"),
    ("bn", "bn", "bn_BD", "ltr", "বাংলা"),
    ("pt-br", "pt-BR", "pt_BR", "ltr", "Português (Brasil)"),
    ("ru", "ru", "ru_RU", "ltr", "Русский"),
    ("ur", "ur", "ur_PK", "rtl", "اردو"),
    ("id", "id", "id_ID", "ltr", "Bahasa Indonesia"),
    ("de", "de", "de_DE", "ltr", "Deutsch"),
    ("ja", "ja", "ja_JP", "ltr", "日本語"),
    ("tr", "tr", "tr_TR", "ltr", "Türkçe"),
    ("ko", "ko", "ko_KR", "ltr", "한국어"),
    ("vi", "vi", "vi_VN", "ltr", "Tiếng Việt"),
    ("it", "it", "it_IT", "ltr", "Italiano"),
    ("pl", "pl", "pl_PL", "ltr", "Polski"),
    ("uk", "uk", "uk_UA", "ltr", "Українська"),
    ("nl", "nl", "nl_NL", "ltr", "Nederlands"),
    ("fa", "fa", "fa_IR", "rtl", "فارسی"),
]

# Every visible English string that the translations replace (exact text as it appears in index.html).
TEXTS = [
    "iRoof.top: talk to the people you miss",
    "Export a Telegram or WhatsApp chat, and an LLM continues it in the style of the person you miss. Open source, runs on your computer, offline if you like.",
    "Talk to the people you miss: an LLM continues your Telegram or WhatsApp chat in their style.",
    "Talk to the people you miss.",
    "Export a Telegram or WhatsApp chat, and an AI continues it in their style. On your computer, offline if you like.",
    "Download",
    "Docs",
    "What it looks like",
    "Someone sits on a sloped roof by the chimney, hugging their knees and looking up at the moon and the stars",
    "A chat continues in the style of a real person, then a friend opens the shared link on a phone",
    "Apple Silicon · .dmg",
    "Intel .dmg",
    "10 and 11 · .exe",
    ".msi package",
    "AppImage · x86-64",
    "Ubuntu .deb",
    "Fedora .rpm",
    "As a server",
    "all options",
    "Click to select, then copy",
    "The apps aren't signed yet. On a Mac, open the app once, then",
    "System Settings → Privacy &amp; Security → Open Anyway",
    "On Windows,",
    "More info → Run anyway",
    "Step by step",
    "Documentation",
    "Privacy",
    "MIT",
]

EN_UI = {"faq": "Questions", "language": "Language"}

EN_FAQ = [
    ("What is iRoof.top?",
     "Talk to the people you miss. Export a Telegram or WhatsApp chat, and an AI continues it in the style of one person: "
     "their words, bubbles, stickers, GIFs, reactions and voice. It is open source under the MIT license."),
    ("How do I export a chat?",
     "In Telegram Desktop, open the chat, choose ⋯, then Export Chat History. In WhatsApp, open the chat, choose Export chat "
     "and include media. Drop the folder, the .zip or the .txt file onto iRoof.top."),
    ("Does my chat leave my computer?",
     "Importing never touches the network. With a local model (Ollama, LM Studio, llama.cpp) and Offline mode turned on, "
     "nothing leaves your computer."),
    ("Which AI does it use?",
     "Any OpenAI-compatible service: OpenAI, OpenRouter, or a model on your own computer or server, such as Ollama, "
     "LM Studio or vLLM. The software is free; a cloud model is billed by its provider."),
    ("Can someone else talk to the chat?",
     "Yes. A share link (iroof.top/s/name) lets someone else continue the conversation, with a PIN and limits that you set. "
     "It works on your Wi-Fi or at name.iroof.top, and it can show a QR code."),
]

MARK_RE = re.compile(r"<!-- i18n:(head|faq|langs) -->.*?<!-- /i18n:\1 -->", re.S)


def fail(message):
    sys.exit(f"landing-i18n: {message}")


def esc(text):
    return html.escape(text, quote=False)


def url_of(directory):
    return f"/{directory}/" if directory else "/"


def template(master):
    """The page with every generated block emptied to a placeholder; the visible text is still English."""
    return MARK_RE.sub(lambda m: f"<!-- i18n:{m.group(1)} -->@@{m.group(1)}@@<!-- /i18n:{m.group(1)} -->", master)


def load_language(directory, hreflang):
    path = I18N / f"{directory}.json"
    if not path.exists():
        fail(f"missing translation file {path.relative_to(ROOT)}")
    data = json.loads(path.read_text(encoding="utf-8"))
    texts = data.get("texts", {})
    missing = [t for t in TEXTS if t not in texts]
    extra = [t for t in texts if t not in TEXTS]
    if missing or extra:
        fail(f"{hreflang}: missing {missing!r}, unknown {extra!r}")
    for source, value in texts.items():
        if not value.strip() or '"' in value or "<" in value or ">" in value:
            fail(f"{hreflang}: bad translation for {source!r}: {value!r}")
    faq = data.get("faq", [])
    if len(faq) != len(EN_FAQ) or any(len(pair) != 2 or not all(pair) for pair in faq):
        fail(f"{hreflang}: faq must have {len(EN_FAQ)} [question, answer] pairs")
    ui = data.get("ui", {})
    if set(ui) != set(EN_UI) or not all(ui.values()):
        fail(f"{hreflang}: ui must have keys {sorted(EN_UI)}")
    return texts, [tuple(pair) for pair in faq], ui


def translate(text, table):
    if not table:
        return text
    pattern = re.compile("|".join(re.escape(key) for key in sorted(table, key=len, reverse=True)))
    return pattern.sub(lambda m: table[m.group(0)], text)


def head_block(directory, hreflang, og_locale):
    lines = [f'<link rel="canonical" href="{SITE}{url_of(directory)}">']
    for d, h, _, _, _ in LANGS:
        lines.append(f'<link rel="alternate" hreflang="{h}" href="{SITE}{url_of(d)}">')
    lines.append(f'<link rel="alternate" hreflang="x-default" href="{SITE}/">')
    lines.append('<meta property="og:type" content="website">')
    lines.append('<meta property="og:site_name" content="iRoof.top">')
    lines.append(f'<meta property="og:url" content="{SITE}{url_of(directory)}">')
    lines.append(f'<meta property="og:locale" content="{og_locale}">')
    for d, _, other_locale, _, _ in LANGS:
        if other_locale != og_locale:
            lines.append(f'<meta property="og:locale:alternate" content="{other_locale}">')
    return "\n  ".join(lines)


def faq_block(faq, ui, hreflang):
    items = "\n".join(
        f"    <details>\n      <summary>{esc(q)}</summary>\n      <p>{esc(a)}</p>\n    </details>" for q, a in faq
    )
    data = {
        "@context": "https://schema.org",
        "@type": "FAQPage",
        "inLanguage": hreflang,
        "mainEntity": [
            {"@type": "Question", "name": q, "acceptedAnswer": {"@type": "Answer", "text": a}} for q, a in faq
        ],
    }
    json_ld = json.dumps(data, ensure_ascii=False, indent=2).replace("</", "<\\/")
    return (
        f'<section class="faq" aria-labelledby="faqH">\n'
        f'    <h2 class="label" id="faqH">{esc(ui["faq"])}</h2>\n'
        f"{items}\n"
        f"  </section>\n"
        f'  <script type="application/ld+json">\n{json_ld}\n  </script>'
    )


def langs_block(current, ui):
    parts = []
    for d, h, _, _, name in LANGS:
        if d == current:
            parts.append(f'<span lang="{h}" aria-current="true">{esc(name)}</span>')
        else:
            parts.append(f'<a href="{url_of(d)}" lang="{h}" hreflang="{h}">{esc(name)}</a>')
    return f'<nav class="langs" aria-label="{esc(ui["language"])}">' + "".join(parts) + "</nav>"


def build(base, directory, hreflang, og_locale, direction, table, faq, ui):
    text = translate(base, table)
    for source, value in table.items():
        if source not in value and source in text:  # a translation may contain the English word (Downloaden)
            fail(f"{hreflang}: English text still on the page: {source!r}")
    dir_attr = ' dir="rtl"' if direction == "rtl" else ""
    text = re.sub(r'<html lang="[^"]*">', f'<html lang="{hreflang}"{dir_attr}>', text, count=1)
    text = text.replace("@@head@@", head_block(directory, hreflang, og_locale))
    text = text.replace("@@faq@@", faq_block(faq, ui, hreflang))
    text = text.replace("@@langs@@", langs_block(directory, ui))
    if "@@" in text:
        fail(f"{hreflang}: unfilled placeholder")
    return text


def sitemap():
    out = ['<?xml version="1.0" encoding="UTF-8"?>',
           '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9" xmlns:xhtml="http://www.w3.org/1999/xhtml">']
    alternates = "".join(
        f'<xhtml:link rel="alternate" hreflang="{h}" href="{SITE}{url_of(d)}"/>' for d, h, _, _, _ in LANGS
    ) + f'<xhtml:link rel="alternate" hreflang="x-default" href="{SITE}/"/>'
    for d, _, _, _, _ in LANGS:
        out.append(f"  <url><loc>{SITE}{url_of(d)}</loc>{alternates}</url>")
    out.append("</urlset>")
    return "\n".join(out) + "\n"


def main():
    master = (LANDING / "index.html").read_text(encoding="utf-8")
    base = template(master)
    for source in TEXTS:
        if base.count(source) == 0:
            fail(f"English text not found on the page: {source!r}")

    pages = {}
    for directory, hreflang, og_locale, direction, _ in LANGS:
        if hreflang == "en":
            table, faq, ui = {}, EN_FAQ, EN_UI
        else:
            texts, faq, ui = load_language(directory, hreflang)
            table = {source: texts[source] for source in TEXTS}
        pages[directory] = build(base, directory, hreflang, og_locale, direction, table, faq, ui)

    for directory, content in pages.items():
        path = LANDING / directory / "index.html" if directory else LANDING / "index.html"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
    (LANDING / "sitemap.xml").write_text(sitemap(), encoding="utf-8")
    print(f"wrote {len(pages)} pages and sitemap.xml")


if __name__ == "__main__":
    main()
