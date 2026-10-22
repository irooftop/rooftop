//! Turns a Telegram or WhatsApp export into a [`ChatExport`] with a deduplicated
//! media library and puts the referenced media files into the data store.
//!
//! An import is a pipeline of separately callable steps, so the caller can look
//! at the result between them (the server dedupes by [`fingerprint`] before any
//! media is hashed or moved):
//!
//! 1. [`parse`] reads the messages,
//! 2. [`Staged::link_media`] builds the library (hashing the media files),
//! 3. [`place_media`] copies or moves the referenced files.
//!
//! The steps report their [`Progress`] and never touch the network.

use crate::model::{ChatExport, LibraryItem, MediaKind, Participant};
use crate::{telegram, telegram_json, whatsapp};
use anyhow::{Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Import stages in the order they run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Unpacking,
    Reading,
    Parsing,
    Hashing,
    Placing,
    Saving,
}

/// Where an import is: `done` of `total` units of the stage's work (pages,
/// files). `messages` counts the messages found so far while parsing, else 0.
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub stage: Stage,
    pub done: usize,
    pub total: usize,
    pub messages: usize,
}

/// Receives progress from any worker thread.
pub type Report<'a> = &'a (dyn Fn(Progress) + Sync);

/// Maps `items` on all cores and returns the results in item order.
pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let workers = std::thread::available_parallelism().map_or(1, usize::from).min(items.len()).max(1);
    let next = AtomicUsize::new(0);
    let worker = || {
        let mut out = Vec::new();
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(item) = items.get(i) else { break };
            out.push((i, f(item)));
        }
        out
    };
    let mut done: Vec<(usize, R)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers).map(|_| s.spawn(worker)).collect();
        handles.into_iter().flat_map(|h| h.join().expect("import worker panicked")).collect()
    });
    done.sort_unstable_by_key(|(i, _)| *i);
    done.into_iter().map(|(_, r)| r).collect()
}

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
    let mut buf = vec![0u8; 256 * 1024];
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

/// How [`place_media`] puts files into the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// The source stays as it is (a folder the user owns).
    Copy,
    /// The source is ours to consume (an upload): renamed, copied across file systems.
    Move,
}

/// Messages and participants of an export, whose media are not looked at yet.
pub struct Staged {
    /// Directory the media paths in the messages are relative to.
    pub root: PathBuf,
    pub export: ChatExport,
    /// (sender name, avatar path relative to the root)
    avatars: Vec<(String, String)>,
}

/// Parses the export at `src` (a directory or a WhatsApp `.txt` file).
pub fn parse(src: &Path, report: Report) -> Result<Staged> {
    let (root, kind) = detect_export(src).context("no messages.html, result.json or WhatsApp .txt found in the upload")?;
    let parsed = match kind {
        ExportKind::TelegramHtml => telegram::parse_export_dir(&root, report)?,
        ExportKind::TelegramJson => telegram_json::parse_export_dir(&root, report)?,
        ExportKind::WhatsApp(txt) => whatsapp::parse_file(&txt)?,
    };
    let participants = participants(&parsed.messages);
    let title = match parsed.title {
        t if !t.is_empty() => t,
        _ => participants.first().map_or_else(|| "Chat".into(), |p| p.name.clone()),
    };
    let export = ChatExport { title, participants, messages: parsed.messages, library: Vec::new() };
    Ok(Staged { root, export, avatars: parsed.avatars })
}

/// Senders in order of first appearance with their message counts.
fn participants(messages: &[crate::model::Message]) -> Vec<Participant> {
    let mut index: HashMap<&str, usize> = HashMap::new();
    let mut list: Vec<Participant> = Vec::new();
    for m in messages.iter().filter(|m| !m.service && !m.from.is_empty()) {
        match index.get(m.from.as_str()) {
            Some(&i) => list[i].messages += 1,
            None => {
                index.insert(&m.from, list.len());
                list.push(Participant { name: m.from.clone(), avatar: None, messages: 1 });
            }
        }
    }
    list
}

/// Identifies the content of an export, to skip importing it twice.
pub fn fingerprint(e: &ChatExport) -> String {
    let mut h = Sha256::new();
    h.update(e.title.as_bytes());
    for m in &e.messages {
        h.update(m.id.to_le_bytes());
        h.update(m.date.as_deref().unwrap_or_default().as_bytes());
        h.update(m.text.as_bytes());
    }
    hex::encode(h.finalize())[..24].to_string()
}

/// A media message that goes into the library, with the file it points to.
struct Linked {
    index: usize,
    rel: String,
    path: Option<PathBuf>,
}

impl Staged {
    /// Builds the media library (identical files share one item) and keeps only
    /// references to files that exist. Returns the files to [`place_media`], sorted.
    pub fn link_media(&mut self, report: Report) -> Result<Vec<String>> {
        let root = self.root.as_path();
        let mut to_place: Vec<String> = Vec::new();
        let avatars: HashMap<&str, &str> = self.avatars.iter().map(|(n, a)| (n.as_str(), a.as_str())).collect();
        for p in self.export.participants.iter_mut() {
            p.avatar = avatars.get(p.name.as_str()).filter(|a| existing(root, a).is_some()).map(|a| a.to_string());
            to_place.extend(p.avatar.clone());
        }
        for msg in self.export.messages.iter_mut() {
            if let Some(lp) = msg.link_preview.as_mut() {
                lp.photo = lp.photo.take().filter(|p| existing(root, p).is_some());
                to_place.extend(lp.photo.clone());
            }
        }
        let linked = self.linked(root);
        let digests = hash_files(&linked, report)?;
        let mut library: BTreeMap<String, LibraryItem> = BTreeMap::new();
        for Linked { index, rel, path } in linked {
            let msg = &mut self.export.messages[index];
            let media = msg.media.as_mut().expect("linked messages have media");
            let kind = media.kind();
            let digest = match &path {
                Some(p) => digests[p].clone(),
                None => hex::encode(Sha256::digest(rel.as_bytes())),
            };
            let id = format!("{}_{}", kind.id_prefix(), &digest[..10]);
            let thumb = media.thumb.clone().filter(|t| existing(root, t).is_some()).or_else(|| {
                let t = format!("thumbs/thumb_{}_0.jpg", msg.id);
                existing(root, &t).map(|_| t)
            });
            media.thumb = thumb.clone();
            media.library_id = Some(id.clone());
            if path.is_some() {
                to_place.push(rel.clone());
            }
            to_place.extend(thumb.clone());
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
        self.export.library = library.into_values().collect();
        self.export.library.sort_by(|a, b| b.uses.len().cmp(&a.uses.len()).then(a.id.cmp(&b.id)));
        to_place.sort();
        to_place.dedup();
        Ok(to_place)
    }

    /// Media messages that belong into the library (the rest are cards: location, poll, …).
    fn linked(&self, root: &Path) -> Vec<Linked> {
        let in_library = |k: MediaKind| k.is_resendable() || matches!(k, MediaKind::Sticker | MediaKind::Gif);
        self.export
            .messages
            .iter()
            .enumerate()
            .filter_map(|(index, m)| {
                let media = m.media.as_ref().filter(|x| in_library(x.kind()))?;
                let rel = media.file.clone()?;
                let path = existing(root, &rel);
                Some(Linked { index, rel, path })
            })
            .collect()
    }
}

/// SHA-256 of every distinct existing file, hashed on all cores.
fn hash_files(linked: &[Linked], report: Report) -> Result<HashMap<PathBuf, String>> {
    let mut seen = HashSet::new();
    let paths: Vec<&PathBuf> = linked.iter().filter_map(|l| l.path.as_ref()).filter(|p| seen.insert(*p)).collect();
    let done = AtomicUsize::new(0);
    let hashed = par_map(&paths, |p| {
        let digest = hash_file(p).with_context(|| format!("hashing {}", p.display()));
        let done = done.fetch_add(1, Ordering::Relaxed) + 1;
        if done.is_multiple_of(16) || done == paths.len() {
            report(Progress { stage: Stage::Hashing, done, total: paths.len(), messages: 0 });
        }
        digest
    });
    paths.into_iter().zip(hashed).map(|(p, d)| Ok((p.clone(), d?))).collect()
}

/// Puts `rels` (paths relative to `root`, see [`Staged::link_media`]) into `dest`
/// under the same relative paths.
pub fn place_media(root: &Path, dest: &Path, rels: &[String], how: Placement, report: Report) -> Result<()> {
    let mut made: HashSet<PathBuf> = HashSet::new();
    for (i, rel) in rels.iter().enumerate() {
        let (Some(from), Some(safe)) = (existing(root, rel), safe_rel(rel)) else { continue };
        let to = dest.join(safe);
        if let Some(parent) = to.parent().filter(|p| !made.contains(*p)) {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
            made.insert(parent.to_path_buf());
        }
        match how {
            Placement::Copy => copy(&from, &to)?,
            // Renaming fails across file systems (a data directory on another disk): copy then.
            Placement::Move => std::fs::rename(&from, &to).or_else(|_| copy(&from, &to).and_then(|()| Ok(std::fs::remove_file(&from)?)))?,
        }
        if (i + 1) % 64 == 0 || i + 1 == rels.len() {
            report(Progress { stage: Stage::Placing, done: i + 1, total: rels.len(), messages: 0 });
        }
    }
    Ok(())
}

fn copy(from: &Path, to: &Path) -> Result<()> {
    std::fs::copy(from, to).with_context(|| format!("copying {}", from.display()))?;
    Ok(())
}

/// Parses `src` (an export directory or a WhatsApp `.txt` file) and, when `dest`
/// is given, copies every referenced media file (preserving relative paths) into it.
pub fn import_dir(src: &Path, dest: Option<&Path>) -> Result<ChatExport> {
    let quiet = |_: Progress| {};
    let mut staged = parse(src, &quiet)?;
    let rels = staged.link_media(&quiet)?;
    if let Some(dest) = dest {
        place_media(&staged.root, dest, &rels, Placement::Copy, &quiet)?;
    }
    Ok(staged.export)
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

    #[test]
    fn par_map_keeps_order() {
        let items: Vec<u32> = (0..1000).collect();
        let out = par_map(&items, |n| n * 2);
        assert_eq!(out, items.iter().map(|n| n * 2).collect::<Vec<_>>());
        assert!(par_map(&Vec::<u32>::new(), |n| *n).is_empty());
    }
}
