//! Turns a Telegram or WhatsApp export into a [`ChatExport`] with a deduplicated
//! media library, optionally copying referenced media into the data store.

use crate::model::{ChatExport, LibraryItem, MediaKind, Participant};
use crate::{telegram, telegram_json, whatsapp};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// Rejects absolute paths and `..`, returns a normalized relative path.
pub fn safe_rel(rel: &str) -> Option<PathBuf> {
    let p = Path::new(rel);
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn hash_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

fn existing(src: &Path, rel: &str) -> Option<PathBuf> {
    let p = src.join(safe_rel(rel)?);
    p.is_file().then_some(p)
}

/// Which chat application and file format an export directory holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportKind {
    TelegramHtml,
    TelegramJson,
    /// The chat `.txt` file (attachments sit next to it).
    WhatsApp(PathBuf),
}

fn kind_in(d: &Path) -> Option<ExportKind> {
    if d.join("messages.html").is_file() {
        Some(ExportKind::TelegramHtml)
    } else if telegram_json::looks_like_chat_json(&d.join(telegram_json::FILE_NAME)) {
        Some(ExportKind::TelegramJson)
    } else {
        whatsapp::find_chat_file(d).map(ExportKind::WhatsApp)
    }
}

/// Locates the export root (directory with `messages.html`, `result.json` or a
/// WhatsApp chat `.txt`) inside `dir`, searching a few levels deep.
/// A path to a WhatsApp `.txt` file yields its directory.
pub fn detect_export(dir: &Path) -> Option<(PathBuf, ExportKind)> {
    fn rec(d: &Path, depth: u32) -> Option<(PathBuf, ExportKind)> {
        if let Some(kind) = kind_in(d) {
            return Some((d.to_path_buf(), kind));
        }
        if depth == 0 {
            return None;
        }
        let mut subdirs: Vec<PathBuf> = std::fs::read_dir(d)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir() && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("__MACOSX")))
            .collect();
        subdirs.sort();
        subdirs.iter().find_map(|s| rec(s, depth - 1))
    }
    if dir.is_file() {
        let is_txt = dir.extension().is_some_and(|e| e.eq_ignore_ascii_case("txt"));
        return (is_txt && whatsapp::is_whatsapp_txt(dir))
            .then(|| (dir.parent().unwrap_or(Path::new(".")).to_path_buf(), ExportKind::WhatsApp(dir.to_path_buf())));
    }
    rec(dir, 3)
}

/// Locates the export root inside `dir`, see [`detect_export`].
pub fn find_export_root(dir: &Path) -> Option<PathBuf> {
    detect_export(dir).map(|(root, _)| root)
}

/// Parses `src` (an export directory or a WhatsApp `.txt` file) and, when `dest`
/// is given, copies every referenced media file (preserving relative paths) into it.
pub fn import_dir(src: &Path, dest: Option<&Path>) -> Result<ChatExport> {
    let (src, kind) = detect_export(src).with_context(|| {
        format!(
            "no supported chat export found in {} (expected Telegram messages.html, Telegram result.json or a WhatsApp chat .txt)",
            src.display()
        )
    })?;
    let src = src.as_path();
    let parsed = match kind {
        ExportKind::TelegramHtml => telegram::parse_export_dir(src)?,
        ExportKind::TelegramJson => telegram_json::parse_export_dir(src)?,
        ExportKind::WhatsApp(txt) => whatsapp::parse_file(&txt)?,
    };
    let mut export = ChatExport { title: parsed.title.clone(), messages: parsed.messages, ..Default::default() };

    let mut counts: Vec<(String, u32)> = Vec::new();
    for m in export.messages.iter().filter(|m| !m.service && !m.from.is_empty()) {
        match counts.iter_mut().find(|(n, _)| n == &m.from) {
            Some((_, c)) => *c += 1,
            None => counts.push((m.from.clone(), 1)),
        }
    }
    let mut to_copy: Vec<String> = Vec::new();
    export.participants = counts
        .into_iter()
        .map(|(name, messages)| {
            let avatar = parsed.avatars.iter().find(|(n, _)| n == &name).map(|(_, a)| a.clone()).filter(|a| existing(src, a).is_some());
            if let Some(a) = &avatar {
                to_copy.push(a.clone());
            }
            Participant { name, avatar, messages }
        })
        .collect();
    if export.title.is_empty() {
        export.title = export.participants.first().map(|p| p.name.clone()).unwrap_or_else(|| "Chat".into());
    }

    for msg in export.messages.iter_mut() {
        if let Some(lp) = msg.link_preview.as_mut() {
            lp.photo = lp.photo.take().filter(|p| existing(src, p).is_some());
            if let Some(p) = &lp.photo {
                to_copy.push(p.clone());
            }
        }
    }
    let mut library: BTreeMap<String, LibraryItem> = BTreeMap::new();
    let mut hash_cache: HashMap<PathBuf, String> = HashMap::new();
    for msg in export.messages.iter_mut() {
        let Some(media) = msg.media.as_mut() else { continue };
        let kind = media.kind();
        if !(kind.is_resendable() || matches!(kind, MediaKind::Sticker | MediaKind::Gif)) {
            continue;
        }
        let Some(rel) = media.file.clone() else { continue };
        let path = existing(src, &rel);
        let digest = match &path {
            Some(p) => match hash_cache.get(p) {
                Some(h) => h.clone(),
                None => {
                    let h = hash_file(p).with_context(|| format!("hashing {}", p.display()))?;
                    hash_cache.insert(p.clone(), h.clone());
                    h
                }
            },
            None => hex::encode(Sha256::digest(rel.as_bytes())),
        };
        let id = format!("{}_{}", kind.id_prefix(), &digest[..10]);
        let thumb = media.thumb.clone().filter(|t| existing(src, t).is_some()).or_else(|| {
            let t = format!("thumbs/thumb_{}_0.jpg", msg.id);
            existing(src, &t).map(|_| t)
        });
        media.thumb = thumb.clone();
        media.library_id = Some(id.clone());
        if path.is_some() {
            to_copy.push(rel.clone());
        }
        if let Some(t) = &thumb {
            to_copy.push(t.clone());
        }
        let item = library.entry(id.clone()).or_insert_with(|| LibraryItem {
            id,
            kind,
            file: Some(rel.clone()),
            thumb: thumb.clone(),
            title: media.title.clone().filter(|t| t != "GIF"),
            available: path.is_some(),
            description: None,
            uses: Vec::new(),
            senders: BTreeMap::new(),
        });
        if item.thumb.is_none() {
            item.thumb = thumb;
        }
        item.uses.push(msg.id);
        *item.senders.entry(msg.from.clone()).or_default() += 1;
    }
    export.library = library.into_values().collect();
    export.library.sort_by(|a, b| b.uses.len().cmp(&a.uses.len()).then(a.id.cmp(&b.id)));

    if let Some(dest) = dest {
        to_copy.sort();
        to_copy.dedup();
        for rel in to_copy {
            let (Some(from), Some(safe)) = (existing(src, &rel), safe_rel(&rel)) else { continue };
            let to = dest.join(safe);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&from, &to).with_context(|| format!("copying {}", from.display()))?;
        }
    }
    Ok(export)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal() {
        assert!(safe_rel("../etc/passwd").is_none());
        assert!(safe_rel("/etc/passwd").is_none());
        assert_eq!(safe_rel("./a/b.jpg"), Some(PathBuf::from("a/b.jpg")));
    }
}
