//! The example exports in `examples/` stay importable, and each character keeps the habit
//! the persona is supposed to pick up.

use rooftop::import::import_dir;
use rooftop::model::{ChatExport, MediaKind};
use std::collections::BTreeSet;
use std::path::Path;

fn load(rel: &str) -> Option<ChatExport> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples").join(rel);
    if !p.exists() {
        eprintln!("{rel} missing, skipping");
        return None;
    }
    Some(import_dir(&p, None).expect("import"))
}

fn texts<'a>(e: &'a ChatExport, who: &str) -> Vec<&'a str> {
    e.messages.iter().filter(|m| m.from == who && !m.service && !m.text.is_empty()).map(|m| m.text.as_str()).collect()
}

fn kinds(e: &ChatExport) -> Vec<MediaKind> {
    e.messages.iter().filter_map(|m| m.media.as_ref().map(|x| x.kind())).collect()
}

fn days(e: &ChatExport) -> BTreeSet<String> {
    e.messages.iter().filter_map(|m| m.date.as_deref().map(|d| d[..10].to_string())).collect()
}

#[test]
fn telegram_example() {
    let Some(e) = load("telegram") else { return };
    assert_eq!(e.title, "Jonas Weber");
    let count = |n: &str| e.participants.iter().find(|p| p.name == n).map(|p| p.messages).unwrap_or(0);
    assert!((30..=40).contains(&count("Jonas Weber")) && (30..=40).contains(&count("Priya Nair")), "{:?}", e.participants);
    assert_eq!(days(&e).len(), 2, "the chat spans two days");
    assert!(e.messages.iter().all(|m| m.date.is_some()));
    let k = kinds(&e);
    assert_eq!(k.iter().filter(|k| **k == MediaKind::Photo).count(), 4);
    assert_eq!(k.iter().filter(|k| **k == MediaKind::Gif).count(), 3);
    assert!(k.contains(&MediaKind::Voice) && k.contains(&MediaKind::Video));
    assert!(k.contains(&MediaKind::RoundVideo) && k.contains(&MediaKind::File));
    let preview = e.messages.iter().find_map(|m| m.link_preview.as_ref()).expect("a link preview");
    assert!(preview.photo.as_deref().is_some_and(|p| p.starts_with("webpage_thumbs/")), "{preview:?}");
    assert!(e.messages.iter().filter(|m| !m.reactions.is_empty()).count() >= 10);
    assert!(e.library.iter().all(|i| i.available), "every media file is included");

    // Jonas: lowercase bursts, stretched words, never a question mark.
    let jonas = texts(&e, "Jonas Weber");
    for t in &jonas {
        assert!(!t.contains('?'), "Jonas asks without question marks: {t:?}");
    }
    assert!(jonas.iter().filter(|t| t.contains("yesss") || t.contains("nooo")).count() >= 3);
    assert!(jonas.iter().filter(|t| **t == t.to_lowercase()).count() >= jonas.len() - 2, "Jonas writes in lowercase");
    // Priya: full sentences, numbered plans, "Noted."
    let priya = texts(&e, "Priya Nair");
    assert!(priya.iter().all(|t| t.trim_end().ends_with(['.', '?', '!'])), "Priya always punctuates");
    assert!(priya.iter().filter(|t| t.contains("\n1. ")).count() >= 3, "Priya plans in numbered lists");
    assert!(priya.iter().filter(|t| t.starts_with("Noted.")).count() >= 2);
}

#[test]
fn whatsapp_example() {
    let Some(e) = load("whatsapp") else { return };
    assert_eq!(e.title, "Sofia Moreno");
    assert_eq!(days(&e).len(), 2);
    let k = kinds(&e);
    assert_eq!(k.iter().filter(|k| **k == MediaKind::Photo).count(), 2);
    assert!(k.contains(&MediaKind::Voice) && k.contains(&MediaKind::Sticker));
    assert!(e.library.iter().all(|i| i.available));
    assert!(e.messages.iter().any(|m| m.edited));
    assert!(!e.messages.iter().any(|m| m.text.contains("deleted") || m.text.contains("end-to-end")));

    // Daniel signs every message with "x". Sofia writes in lowercase and ends with a heart.
    let daniel = texts(&e, "Daniel Hart");
    assert!(daniel.iter().all(|t| t.ends_with(" x")), "{daniel:?}");
    let sofia = texts(&e, "Sofia Moreno");
    assert!(sofia.iter().all(|t| *t == t.to_lowercase() || t.chars().all(|c| !c.is_lowercase())), "{sofia:?}");
    let hearts = ['💛', '💙', '💜', '❤'];
    assert!(sofia.iter().filter(|t| t.chars().any(|c| hearts.contains(&c))).count() * 4 >= sofia.len() * 3, "mostly ends with a heart");
}
