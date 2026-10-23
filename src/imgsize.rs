//! Pixel sizes of photos, videos and GIFs, read from the file headers (JPEG, PNG, GIF,
//! WebP; the track header of MP4/MOV), else from the poster. The chat reserves each media's box from them before it
//! loads, so the timeline doesn't jump while it is scrolled.

use crate::model::{ChatExport, LibraryItem, MediaKind};
use crate::state::AppState;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

/// How much of a file is read to find its size: a JPEG's EXIF block (with its own preview)
/// comes before the frame header and may take up to 64 KiB.
const HEAD: u64 = 256 * 1024;

/// (width, height) of the image or video at `path`, as displayed (JPEG EXIF and MP4 track
/// rotation applied).
pub fn read(path: &Path) -> Option<(u32, u32)> {
    if is_video(&path.to_string_lossy()) {
        return mp4(&mut std::fs::File::open(path).ok()?);
    }
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(HEAD).read_to_end(&mut buf).ok()?;
    parse(&buf)
}

/// (width, height) of an image from its first bytes.
pub fn parse(b: &[u8]) -> Option<(u32, u32)> {
    let size = if b.starts_with(&[0xFF, 0xD8]) {
        jpeg(b)
    } else if b.starts_with(b"\x89PNG\r\n\x1a\n") && b.len() >= 24 {
        Some((be32(b, 16)?, be32(b, 20)?))
    } else if b.starts_with(b"GIF8") && b.len() >= 10 {
        Some((u16::from_le_bytes([b[6], b[7]]) as u32, u16::from_le_bytes([b[8], b[9]]) as u32))
    } else if b.len() >= 30 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        webp(b)
    } else {
        None
    };
    size.filter(|&(w, h)| w > 0 && h > 0)
}

fn be16(b: &[u8], i: usize) -> Option<u32> {
    Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32)
}

fn be32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

fn le24(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, 0]))
}

fn jpeg(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    let mut rotated = false;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let marker = b[i + 1];
        if marker == 0xFF {
            i += 1; // fill byte
            continue;
        }
        if matches!(marker, 0x01 | 0xD0..=0xD9) {
            i += 2;
            continue;
        }
        let len = be16(b, i + 2)? as usize;
        let seg = b.get(i + 4..(i + 2 + len).min(b.len()))?;
        match marker {
            0xE1 if seg.starts_with(b"Exif\0\0") => rotated = exif_rotated(&seg[6..]).unwrap_or(false),
            // Start of frame (not DHT 0xC4, JPG 0xC8, DAC 0xCC).
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                let (h, w) = (be16(seg, 1)?, be16(seg, 3)?);
                return Some(if rotated { (h, w) } else { (w, h) });
            }
            _ => {}
        }
        i += 2 + len;
    }
    None
}

/// Whether the EXIF orientation turns the image by 90° (values 5 to 8).
fn exif_rotated(t: &[u8]) -> Option<bool> {
    let le = match t.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |i: usize| -> Option<u32> {
        let x = [*t.get(i)?, *t.get(i + 1)?];
        Some(if le { u16::from_le_bytes(x) } else { u16::from_be_bytes(x) } as u32)
    };
    let u32_at = |i: usize| -> Option<u32> {
        let x: [u8; 4] = t.get(i..i + 4)?.try_into().ok()?;
        Some(if le { u32::from_le_bytes(x) } else { u32::from_be_bytes(x) })
    };
    let ifd = u32_at(4)? as usize;
    for k in 0..u16_at(ifd)? as usize {
        let e = ifd + 2 + k * 12;
        if u16_at(e)? == 0x0112 {
            return Some(matches!(u16_at(e + 8)?, 5..=8));
        }
    }
    Some(false)
}

/// Size of the first video track of an MP4/MOV: `moov/trak/tkhd`, found by seeking from
/// box header to box header (the `moov` may sit at the end of the file).
fn mp4<R: Read + Seek>(f: &mut R) -> Option<(u32, u32)> {
    let end = f.seek(SeekFrom::End(0)).ok()?;
    let moov = child(f, 0, end, b"moov")?;
    let mut at = moov.0;
    while let Some((start, stop)) = child(f, at, moov.1, b"trak") {
        at = stop;
        let Some((t0, t1)) = child(f, start, stop, b"tkhd") else { continue };
        let mut tkhd = vec![0; (t1 - t0).min(128) as usize];
        f.seek(SeekFrom::Start(t0)).ok()?;
        f.read_exact(&mut tkhd).ok()?;
        // Version 0: 32-bit times (84-byte body), version 1: 64-bit (96); the matrix sits
        // 40 bytes before the 16.16 width and height that end the box.
        let n = if tkhd.first() == Some(&1) { 96 } else { 84 };
        // (Display sizes may be fractional, e.g. 117.97: round them.)
        let px = |v: u32| ((v as u64 + 0x8000) >> 16) as u32;
        let (w, h) = (px(be32(&tkhd, n - 8)?), px(be32(&tkhd, n - 4)?));
        if w == 0 || h == 0 {
            continue; // an audio track
        }
        let (a, b) = (be32(&tkhd, n - 44)? as i32, be32(&tkhd, n - 40)? as i32);
        return Some(if a == 0 && b != 0 { (h, w) } else { (w, h) });
    }
    None
}

/// The body (start, end) of the first `kind` box among the boxes in `from..to`.
fn child<R: Read + Seek>(f: &mut R, mut from: u64, to: u64, kind: &[u8; 4]) -> Option<(u64, u64)> {
    while from + 8 <= to {
        let mut head = [0u8; 16];
        f.seek(SeekFrom::Start(from)).ok()?;
        f.read_exact(&mut head[..8]).ok()?;
        let (mut size, mut body) = (u32::from_be_bytes(head[..4].try_into().ok()?) as u64, from + 8);
        if size == 1 {
            f.read_exact(&mut head[8..]).ok()?;
            size = u64::from_be_bytes(head[8..].try_into().ok()?);
            body += 8;
        } else if size == 0 {
            size = to - from; // to the end
        }
        if size < body - from || from + size > to {
            return None;
        }
        if &head[4..8] == kind {
            return Some((body, from + size));
        }
        from += size;
    }
    None
}

fn webp(b: &[u8]) -> Option<(u32, u32)> {
    match &b[12..16] {
        b"VP8 " => Some((be16_le(b, 26)? & 0x3FFF, be16_le(b, 28)? & 0x3FFF)),
        b"VP8L" if b.get(20) == Some(&0x2F) => {
            let (b0, b1, b2, b3) = (b[21] as u32, b[22] as u32, b[23] as u32, b[24] as u32);
            Some((1 + (((b1 & 0x3F) << 8) | b0), 1 + (((b3 & 0x0F) << 10) | (b2 << 2) | ((b1 & 0xC0) >> 6))))
        }
        b"VP8X" => Some((1 + le24(b, 24)?, 1 + le24(b, 27)?)),
        _ => None,
    }
}

fn be16_le(b: &[u8], i: usize) -> Option<u32> {
    Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32)
}

/// Kinds whose box the chat sizes from the media (stickers and round videos have a fixed one).
fn sized(kind: MediaKind) -> bool {
    matches!(kind, MediaKind::Photo | MediaKind::Video | MediaKind::Gif)
}

fn is_video(rel: &str) -> bool {
    let ext = rel.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    matches!(ext.as_str(), "mp4" | "mov" | "m4v")
}

fn is_image(rel: &str) -> bool {
    let ext = rel.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "gif" | "webp")
}

/// Size of a library item under `root`: the file itself, else its poster.
pub fn of_item(root: &Path, item: &LibraryItem) -> Option<[u32; 2]> {
    let file = item.file.as_deref().filter(|f| item.available && (is_image(f) || is_video(f)));
    file.into_iter().chain(item.thumb.as_deref()).find_map(|rel| read(&root.join(rel))).map(|(w, h)| [w, h])
}

/// Measures the library items of `export` that have no size yet; returns how many it found.
pub fn fill(root: &Path, export: &mut ChatExport) -> usize {
    let mut n = 0;
    for item in export.library.iter_mut().filter(|i| sized(i.kind) && i.size.is_none()) {
        item.size = of_item(root, item);
        n += item.size.is_some() as usize;
    }
    n
}

/// Exports imported before sizes were recorded get them once, in the background.
pub async fn backfill(st: Arc<AppState>) {
    let ids: Vec<String> = st
        .db
        .read()
        .await
        .exports
        .values()
        .filter(|r| r.export.library.iter().any(|i| sized(i.kind) && i.size.is_none() && (i.available || i.thumb.is_some())))
        .map(|r| r.id.clone())
        .collect();
    for id in ids {
        let Some(rec) = st.db.read().await.exports.get(&id).cloned() else { continue };
        let root = st.paths.export_files(&id);
        // Measure without holding the lock, then merge into the current record.
        let found = tokio::task::spawn_blocking(move || {
            rec.export
                .library
                .iter()
                .filter(|i| sized(i.kind) && i.size.is_none())
                .filter_map(|i| of_item(&root, i).map(|s| (i.id.clone(), s)))
                .collect::<std::collections::HashMap<_, _>>()
        })
        .await
        .unwrap_or_default();
        if found.is_empty() {
            continue;
        }
        let mut db = st.db.write().await;
        let Some(cur) = db.exports.get(&id).cloned() else { continue };
        let mut new = (*cur).clone();
        for item in new.export.library.iter_mut() {
            if let Some(s) = found.get(&item.id) {
                item.size = Some(*s);
            }
        }
        let (paths, new) = (st.paths.clone(), Arc::new(new));
        let saved = new.clone();
        match tokio::task::spawn_blocking(move || paths.save_export(&saved)).await {
            Ok(Ok(())) => {
                db.exports.insert(id.clone(), new);
                tracing::info!(export = %id, media = found.len(), "recorded media sizes");
            }
            Ok(Err(e)) => tracing::warn!(export = %id, "saving media sizes: {e:#}"),
            Err(e) => tracing::warn!(export = %id, "measuring media: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_common_formats() {
        let png = [b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".as_slice(), &640u32.to_be_bytes(), &480u32.to_be_bytes()].concat();
        assert_eq!(parse(&png), Some((640, 480)));
        assert_eq!(parse(b"GIF89a\x40\x01\xf0\x00"), Some((320, 240)));
        let mut vp8x = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\0\0\0\0".to_vec();
        vp8x.extend_from_slice(&[0x3F, 0x01, 0, 0xEF, 0, 0]); // 320 × 240, stored minus one
        assert_eq!(parse(&vp8x), Some((320, 240)));
        assert_eq!(parse(b"not an image at all, really"), None);
    }

    fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        [&((body.len() + 8) as u32).to_be_bytes(), kind.as_slice(), body].concat()
    }

    fn tkhd(w: u32, h: u32, rotated: bool) -> Vec<u8> {
        let mut b = vec![0u8; 84]; // version 0
        let (a, bb) = if rotated { (0u32, 0x0001_0000u32) } else { (0x0001_0000, 0) };
        b[40..44].copy_from_slice(&a.to_be_bytes());
        b[44..48].copy_from_slice(&bb.to_be_bytes());
        b[76..80].copy_from_slice(&(w << 16).to_be_bytes());
        b[80..84].copy_from_slice(&(h << 16).to_be_bytes());
        mp4_box(b"tkhd", &b)
    }

    /// The video track's size, after an audio track, with `moov` after `mdat` and rotation.
    #[test]
    fn mp4_reads_the_video_track() {
        for (rotated, want) in [(false, (1280, 720)), (true, (720, 1280))] {
            let audio = mp4_box(b"trak", &tkhd(0, 0, false));
            let video = mp4_box(b"trak", &[tkhd(1280, 720, rotated), mp4_box(b"mdia", &[0; 12])].concat());
            let file =
                [mp4_box(b"ftyp", b"isom\0\0\0\0"), mp4_box(b"mdat", &[7; 100]), mp4_box(b"moov", &[audio, video].concat())].concat();
            assert_eq!(mp4(&mut std::io::Cursor::new(file)), Some(want));
        }
        assert_eq!(mp4(&mut std::io::Cursor::new(b"garbage, not a box".to_vec())), None);
    }

    /// A JPEG with an APP1 EXIF block that says "rotate 90°" (orientation 6), then a frame of 400 × 300.
    #[test]
    fn jpeg_applies_the_exif_rotation() {
        let tiff = [b"MM\0\x2a\0\0\0\x08\0\x01".as_slice(), &[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0], &[0, 0, 0, 0]].concat();
        let app1 = [b"Exif\0\0".as_slice(), &tiff].concat();
        let sof = [8u8, 0x01, 0x2C, 0x01, 0x90, 3]; // precision, height 300, width 400, components
        let jpeg = |exif: bool| {
            let mut j = vec![0xFF, 0xD8];
            if exif {
                j.extend_from_slice(&[0xFF, 0xE1]);
                j.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
                j.extend_from_slice(&app1);
            }
            j.extend_from_slice(&[0xFF, 0xC0, 0, (sof.len() + 2) as u8]);
            j.extend_from_slice(&sof);
            j
        };
        assert_eq!(parse(&jpeg(false)), Some((400, 300)));
        assert_eq!(parse(&jpeg(true)), Some((300, 400)));
    }
}
