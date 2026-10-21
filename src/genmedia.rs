//! Media created during a live chat: files produced by MCP tools (image
//! generation, text-to-speech…) and voice messages recorded in the UI.
//!
//! Files are stored next to the export's media, under
//! `files/generated/<chat id>/`, so the existing media routes (and the share
//! allow-list, which follows the chat's messages) serve them.

use crate::model::MediaKind;
use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

/// Largest file taken from a tool result or an upload.
pub const MAX_BYTES: usize = 25 * 1024 * 1024;
/// At most this many media items are taken from one tool result.
const MAX_ITEMS: usize = 4;

#[derive(Debug, Clone)]
pub struct Captured {
    pub kind: MediaKind,
    pub ext: &'static str,
    pub bytes: Vec<u8>,
}

/// Detects the real type from the content (never trusts the declared MIME type),
/// so only genuine images, audio and video are stored and served.
pub fn sniff(b: &[u8]) -> Option<(MediaKind, &'static str)> {
    let at = |off: usize, sig: &[u8]| b.len() >= off + sig.len() && &b[off..off + sig.len()] == sig;
    Some(if at(0, b"\xFF\xD8\xFF") {
        (MediaKind::Photo, "jpg")
    } else if at(0, b"\x89PNG\r\n\x1a\n") {
        (MediaKind::Photo, "png")
    } else if at(0, b"RIFF") && at(8, b"WEBP") {
        (MediaKind::Photo, "webp")
    } else if at(0, b"GIF87a") || at(0, b"GIF89a") {
        (MediaKind::Gif, "gif")
    } else if at(0, b"RIFF") && at(8, b"WAVE") {
        (MediaKind::Voice, "wav")
    } else if at(0, b"OggS") {
        (MediaKind::Voice, "ogg")
    } else if at(0, b"ID3") || (b.len() > 2 && b[0] == 0xFF && (b[1] & 0xE0) == 0xE0 && (b[1] & 0x06) != 0) {
        (MediaKind::Voice, "mp3")
    } else if at(0, b"fLaC") {
        (MediaKind::Voice, "flac")
    } else if at(4, b"ftypM4A") {
        (MediaKind::Voice, "m4a")
    } else if at(4, b"ftyp") {
        (MediaKind::Video, "mp4")
    } else if at(0, b"\x1A\x45\xDF\xA3") {
        // WebM: a browser recording is audio-only, so treat it as a voice message.
        (MediaKind::Voice, "webm")
    } else {
        return None;
    })
}

/// `format` value of an OpenAI-compatible `input_audio` part for a file extension.
pub fn audio_format(ext: &str) -> Option<&'static str> {
    Some(match ext.to_ascii_lowercase().as_str() {
        "wav" => "wav",
        "mp3" => "mp3",
        "ogg" | "oga" | "opus" => "ogg",
        "flac" => "flac",
        "m4a" => "m4a",
        "aac" => "aac",
        "webm" => "webm",
        _ => return None,
    })
}

/// Duration of a PCM WAV file as Telegram shows it (`0:05`).
pub fn wav_duration(b: &[u8]) -> Option<String> {
    if b.len() < 44 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let mut i = 12;
    let mut byte_rate = 0u32;
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let size = u32::from_le_bytes(b[i + 4..i + 8].try_into().ok()?) as usize;
        if id == b"fmt " && i + 20 <= b.len() {
            byte_rate = u32::from_le_bytes(b[i + 16..i + 20].try_into().ok()?);
        } else if id == b"data" && byte_rate > 0 {
            // Streaming encoders write 0 or 0xFFFFFFFF, so fall back to the file size.
            let len = if size == 0 || size == u32::MAX as usize { b.len() - i - 8 } else { size.min(b.len() - i - 8) };
            return Some(fmt_duration((len as f64 / byte_rate as f64).round() as u64));
        }
        i += 8 + size + (size & 1);
    }
    None
}

pub fn fmt_duration(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Media of an MCP `CallToolResult`: inline `image`/`audio` content (base64),
/// `resource` blobs, and `resource_link`s / embedded resources pointing at
/// `file://` paths **inside `workspace`** (the server's working directory).
/// Unknown or unsafe items are skipped and duplicates are dropped.
pub fn collect(result: &Value, workspace: Option<&Path>) -> Vec<Captured> {
    let mut out: Vec<Captured> = Vec::new();
    let mut seen: Vec<[u8; 32]> = Vec::new();
    let mut push = |bytes: Vec<u8>| {
        if bytes.is_empty() || bytes.len() > MAX_BYTES {
            return;
        }
        let Some((kind, ext)) = sniff(&bytes) else { return };
        let h: [u8; 32] = Sha256::digest(&bytes).into();
        if !seen.contains(&h) {
            seen.push(h);
            out.push(Captured { kind, ext, bytes });
        }
    };
    let b64 = |s: &str| base64::engine::general_purpose::STANDARD.decode(s.trim()).ok();
    for c in result.get("content").and_then(Value::as_array).into_iter().flatten() {
        match c.get("type").and_then(Value::as_str).unwrap_or_default() {
            "image" | "audio" => {
                if let Some(bytes) = c.get("data").and_then(Value::as_str).and_then(b64) {
                    push(bytes);
                }
            }
            "resource_link" => {
                if let Some(bytes) = c.get("uri").and_then(Value::as_str).and_then(|u| read_workspace_file(u, workspace)) {
                    push(bytes);
                }
            }
            "resource" => {
                let r = &c["resource"];
                if let Some(bytes) = r.get("blob").and_then(Value::as_str).and_then(b64) {
                    push(bytes);
                } else if let Some(bytes) = r.get("uri").and_then(Value::as_str).and_then(|u| read_workspace_file(u, workspace)) {
                    push(bytes);
                }
            }
            _ => {}
        }
    }
    out.truncate(MAX_ITEMS);
    out
}

/// Reads a `file://` URI only if it resolves to a regular file inside `workspace`.
fn read_workspace_file(uri: &str, workspace: Option<&Path>) -> Option<Vec<u8>> {
    let ws = workspace?;
    let url = reqwest::Url::parse(uri).ok()?;
    if url.scheme() != "file" {
        return None;
    }
    let path = url.to_file_path().ok()?.canonicalize().ok()?;
    if !path.starts_with(ws) {
        tracing::warn!(path = %path.display(), "ignoring MCP file outside the server workspace");
        return None;
    }
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() as usize > MAX_BYTES {
        return None;
    }
    std::fs::read(&path).ok()
}

/// A short, readable description from the arguments of the tool call that
/// produced the media: the spoken text of a TTS call, the prompt of an image.
pub fn description_from_args(args: &Value) -> Option<String> {
    ["text", "input", "prompt", "description", "caption"]
        .iter()
        .find_map(|k| args.get(*k).and_then(Value::as_str))
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty())
        .map(|s| crate::llm::truncate(&s, 1000))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wav(secs: u32) -> Vec<u8> {
        let rate = 16_000u32;
        let data = vec![0u8; (rate * 2 * secs) as usize];
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(&data);
        b
    }

    #[test]
    fn sniffs_and_measures() {
        assert_eq!(sniff(b"\xFF\xD8\xFF\xE0rest").unwrap().1, "jpg");
        assert_eq!(sniff(&wav(1)).unwrap(), (MediaKind::Voice, "wav"));
        assert!(sniff(b"<html>").is_none());
        assert_eq!(wav_duration(&wav(5)).as_deref(), Some("0:05"));
    }

    #[test]
    fn collects_inline_and_workspace_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let ws = ws.canonicalize().unwrap();
        std::fs::write(ws.join("v.wav"), wav(2)).unwrap();
        std::fs::write(dir.path().join("secret.wav"), wav(3)).unwrap();
        std::fs::write(ws.join("page.html"), b"<html>").unwrap();
        let jpg = b"\xFF\xD8\xFF\xE0jpegdata".to_vec();
        let url = |p: &Path| reqwest::Url::from_file_path(p).unwrap().to_string();
        let r = json!({"content": [
            {"type": "image", "mimeType": "image/jpeg", "data": base64::engine::general_purpose::STANDARD.encode(&jpg)},
            {"type": "resource_link", "uri": url(&ws.join("v.wav")), "mimeType": "audio/wav"},
            {"type": "resource_link", "uri": url(&ws.join("v.wav")), "mimeType": "audio/wav"},
            {"type": "resource_link", "uri": url(&dir.path().join("secret.wav"))},
            {"type": "resource_link", "uri": url(&ws.join("../secret.wav"))},
            {"type": "resource_link", "uri": url(&ws.join("page.html"))},
            {"type": "text", "text": "saved"}
        ]});
        let got = collect(&r, Some(&ws));
        assert_eq!(got.len(), 2, "inline image + one workspace wav, while outside files, duplicates and non-media are skipped");
        assert_eq!(got[0].kind, MediaKind::Photo);
        assert_eq!(got[1].kind, MediaKind::Voice);
        assert!(collect(&r, None).len() == 1, "without a workspace only inline data is taken");
    }

    #[test]
    fn describes_from_args() {
        assert_eq!(description_from_args(&json!({"text": "hi  there\n"})).as_deref(), Some("hi there"));
        assert_eq!(description_from_args(&json!({"prompt": "a cat"})).as_deref(), Some("a cat"));
        assert!(description_from_args(&json!({"n": 1})).is_none());
    }
}
