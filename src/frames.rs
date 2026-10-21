//! Frames of animated media for the vision model. Telegram and WhatsApp export
//! GIFs as `.mp4` (video stickers as `.webm`), which no vision endpoint accepts.
//! A few evenly spaced frames, sent in order, let the model see the motion.
//!
//! Uses `ffmpeg` when it is installed (optional, like Node.js for MCP servers).
//! Without it, items fall back to their thumbnail, if the export has one.

use anyhow::{Context, Result, anyhow, bail};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

/// Frames per item: enough to follow a short loop, few enough for a cheap request.
pub const FRAMES: usize = 4;
/// Longest side of a frame in pixels.
const MAX_SIDE: u32 = 512;
const TIMEOUT: Duration = Duration::from_secs(30);

/// Files whose motion matters: GIFs (as `.mp4` or real `.gif`), video stickers and videos.
pub fn is_animated_file(file: &str) -> bool {
    let l = file.to_lowercase();
    [".mp4", ".webm", ".gif", ".mov", ".m4v", ".mkv"].iter().any(|e| l.ends_with(e))
}

fn works(bin: &Path) -> bool {
    std::process::Command::new(bin)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The `ffmpeg` binary, if installed: `ROOFTOP_FFMPEG`, then `PATH`, then the usual
/// install locations (apps started from the macOS Finder don't get the shell's `PATH`).
pub fn ffmpeg() -> Option<&'static Path> {
    static BIN: OnceLock<Option<PathBuf>> = OnceLock::new();
    BIN.get_or_init(|| {
        if let Some(p) = std::env::var_os("ROOFTOP_FFMPEG").filter(|p| !p.is_empty()) {
            let p = PathBuf::from(p);
            return works(&p).then_some(p);
        }
        let exe = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
        let dirs = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default();
        let fallback = [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/opt/homebrew/opt/ffmpeg/bin",
            "/opt/homebrew/opt/ffmpeg-full/bin",
            "/usr/bin",
            "/snap/bin",
            "/opt/local/bin",
        ];
        dirs.into_iter().chain(fallback.iter().map(PathBuf::from)).map(|d| d.join(exe)).find(|p| p.is_file() && works(p))
    })
    .as_deref()
}

/// `Duration: 00:00:02.04` from ffmpeg's stream info, in seconds.
fn parse_duration(info: &str) -> Option<f64> {
    let rest = &info[info.find("Duration: ")? + 10..];
    let hms = rest.split(',').next()?.trim();
    let mut parts = hms.split(':');
    let (h, m, s) = (parts.next()?.parse::<f64>().ok()?, parts.next()?.parse::<f64>().ok()?, parts.next()?.parse::<f64>().ok()?);
    Some(h * 3600.0 + m * 60.0 + s).filter(|d| *d > 0.0)
}

/// Arguments that open `path` as the only input. Only local files are allowed:
/// a crafted "video" (e.g. an HLS playlist) must not make ffmpeg fetch URLs.
fn input_args(path: &Path, extra: &[&str]) -> Vec<OsString> {
    let mut a: Vec<OsString> = ["-hide_banner", "-nostdin", "-protocol_whitelist", "file"].into_iter().map(OsString::from).collect();
    a.extend(extra.iter().map(OsString::from));
    a.push("-i".into());
    a.push(path.into());
    a
}

async fn run(bin: &Path, args: &[OsString]) -> Result<std::process::Output> {
    let child = tokio::process::Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("starting ffmpeg")?;
    tokio::time::timeout(TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| anyhow!("ffmpeg took longer than {} s", TIMEOUT.as_secs()))?
        .context("running ffmpeg")
}

/// Up to `n` evenly spaced JPEG frames of `path`, in order.
pub async fn extract(path: &Path, n: usize) -> Result<Vec<Vec<u8>>> {
    let bin = ffmpeg().ok_or_else(|| anyhow!("install ffmpeg to describe GIFs and videos"))?;
    // Without an output ffmpeg prints the stream info (and exits with an error, which is expected).
    let info = run(bin, &input_args(path, &[])).await?;
    let duration = parse_duration(&String::from_utf8_lossy(&info.stderr));
    let rate = match duration {
        Some(d) => format!("{n}/{d:.3}"),
        None => "2".to_string(),
    };
    let filter = format!("fps={rate},scale='min({MAX_SIDE},iw)':'min({MAX_SIDE},ih)':force_original_aspect_ratio=decrease");
    let dir = std::env::temp_dir().join(format!("rooftop-frames-{}", uuid::Uuid::new_v4().simple()));
    tokio::fs::create_dir_all(&dir).await?;
    let pattern = dir.join("f%02d.jpg");
    let frames = n.to_string();
    let mut args = input_args(path, &["-loglevel", "error"]);
    args.extend(["-vf", filter.as_str(), "-frames:v", frames.as_str(), "-q:v", "4", "-y"].map(OsString::from));
    args.push(pattern.into());
    let result = async {
        let out = run(bin, &args).await?;
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        files.sort();
        if files.is_empty() {
            let err = String::from_utf8_lossy(&out.stderr);
            bail!("ffmpeg found no frames: {}", crate::llm::truncate(err.trim(), 300));
        }
        files.iter().map(|f| std::fs::read(f).map_err(anyhow::Error::from)).collect::<Result<Vec<_>>>()
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_kinds() {
        let info = "Input #0, mov,mp4: \n  Duration: 00:01:02.50, start: 0.000000, bitrate: 98 kb/s";
        assert_eq!(parse_duration(info), Some(62.5));
        assert_eq!(parse_duration("  Duration: N/A, bitrate: N/A"), None);
        assert!(is_animated_file("gifs/52155_dance.MP4") && is_animated_file("stickers/s.webm"));
        assert!(!is_animated_file("stickers/s.webp") && !is_animated_file("stickers/a.tgs"));
    }
}
