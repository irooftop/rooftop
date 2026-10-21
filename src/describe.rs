//! Optional vision / audio pass: short descriptions of stickers, GIFs, videos
//! and photos, and transcripts of voice messages, so the text-only history
//! tells the model what each item shows or says. GIFs and videos are sent as a
//! few frames in order (see [`crate::frames`]), else as their thumbnail.

use crate::import::safe_rel;
use crate::llm::{LlmClient, LlmConfig};
use crate::model::{ChatExport, LibraryItem, MediaKind};
use base64::Engine;
use futures::StreamExt;
use serde_json::json;
use std::path::Path;

const MAX_IMAGE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_AUDIO_BYTES: u64 = 20 * 1024 * 1024;

/// A voice message file the media model can transcribe: `(input_audio format, path)`.
fn audio_source(dir: &Path, file: Option<&str>) -> Option<(&'static str, std::path::PathBuf)> {
    let file = file?;
    let format = crate::genmedia::audio_format(Path::new(file).extension()?.to_str()?)?;
    let path = dir.join(safe_rel(file)?);
    let meta = std::fs::metadata(&path).ok()?;
    (meta.is_file() && meta.len() <= MAX_AUDIO_BYTES).then_some((format, path))
}

const TRANSCRIBE_PROMPT: &str = "This is a voice message from a private chat. Transcribe it verbatim in its original language, keeping the speaker's wording, fillers and slang. Reply with the transcript only. If there is no speech, describe the sound in a few words in [brackets].";

/// Transcribes a voice message with an audio-capable model (`input_audio`).
pub async fn transcribe(llm: &LlmClient, cfg: &LlmConfig, bytes: &[u8], format: &str) -> anyhow::Result<String> {
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    let messages = vec![json!({"role": "user", "content": [
        {"type": "text", "text": TRANSCRIBE_PROMPT},
        {"type": "input_audio", "input_audio": {"data": data, "format": format}}
    ]})];
    let text = ask_media(llm, cfg, &messages, TRANSCRIBE_PROMPT.len()).await?;
    Ok(crate::llm::truncate(&text.split_whitespace().collect::<Vec<_>>().join(" "), 1000))
}

/// Attempts per item: providers behind an aggregator are picked per request, so a retry
/// usually lands on one that handles the media.
const MEDIA_ATTEMPTS: usize = 3;

/// Sends a request with an image or audio part and checks that the media really arrived.
///
/// Some providers behind an aggregator (seen on OpenRouter) silently drop the whole multimodal
/// message. The model then answers an empty prompt with unrelated text (a math proof, a product
/// description), which must never be saved as a description. Such a reply reports fewer prompt
/// tokens than the instruction text alone, so it is rejected and the request retried, as is an
/// empty reply. Endpoints that report no usage are trusted as before.
async fn ask_media(llm: &LlmClient, cfg: &LlmConfig, messages: &[serde_json::Value], prompt_chars: usize) -> anyhow::Result<String> {
    // The instruction alone is at least ~chars/4 tokens: half of that is a safe floor.
    let floor = (prompt_chars / 8).max(8) as u64;
    let mut last = anyhow::anyhow!("no reply");
    for _ in 0..MEDIA_ATTEMPTS {
        let c = llm.complete(cfg, messages, &[]).await?;
        if let Some(u) = c.usage.as_ref().filter(|u| u.prompt_tokens < floor) {
            last = anyhow::anyhow!(
                "the provider of {} ignored the media ({} prompt tokens) and the answer was discarded. Try again, or pick another media model in Settings",
                cfg.model,
                u.prompt_tokens
            );
            continue;
        }
        let text = c.content.unwrap_or_default().trim().trim_matches('"').to_string();
        if text.is_empty() {
            last = anyhow::anyhow!("{} returned an empty answer", cfg.model);
            continue;
        }
        return Ok(text);
    }
    Err(last)
}

fn image_source(dir: &Path, file: Option<&str>, thumb: Option<&str>) -> Option<(String, std::path::PathBuf)> {
    let is_img = |p: &str| {
        let l = p.to_lowercase();
        [".jpg", ".jpeg", ".png", ".webp", ".gif"].iter().any(|e| l.ends_with(e))
    };
    let pick = file.filter(|f| is_img(f)).or(thumb.filter(|t| is_img(t)))?;
    let path = dir.join(safe_rel(pick)?);
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let mime = mime_guess::from_path(&path).first_or_octet_stream().to_string();
    Some((mime, path))
}

fn visual(kind: MediaKind) -> bool {
    matches!(kind, MediaKind::Sticker | MediaKind::Gif | MediaKind::Photo | MediaKind::Video | MediaKind::RoundVideo)
}

/// An animated file (GIF as `.mp4`, video sticker, video) to take frames from, when ffmpeg is installed.
fn frames_source(dir: &Path, file: Option<&str>) -> Option<std::path::PathBuf> {
    let file = file.filter(|f| crate::frames::is_animated_file(f))?;
    crate::frames::ffmpeg()?;
    let path = dir.join(safe_rel(file)?);
    path.is_file().then_some(path)
}

/// Whether a vision model can describe this item: a visual kind with frames
/// (needs ffmpeg), an image file or a thumbnail.
pub fn can_describe(files_dir: &Path, item: &LibraryItem) -> bool {
    (visual(item.kind)
        && (frames_source(files_dir, item.file.as_deref()).is_some()
            || image_source(files_dir, item.file.as_deref(), item.thumb.as_deref()).is_some()))
        || (item.kind == MediaKind::Voice && audio_source(files_dir, item.file.as_deref()).is_some())
}

enum Job {
    Image(MediaKind, String, std::path::PathBuf),
    /// Frames of an animated file, with the thumbnail as a fallback.
    Frames(MediaKind, std::path::PathBuf, Option<(String, std::path::PathBuf)>),
    Audio(&'static str, std::path::PathBuf),
}

fn data_url(mime: &str, bytes: &[u8]) -> serde_json::Value {
    json!({"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))}})
}

/// Describes one image, or the frames of an animated item in order.
async fn describe_visual(
    llm: &LlmClient,
    cfg: &LlmConfig,
    kind: MediaKind,
    images: Vec<serde_json::Value>,
    frames: bool,
) -> anyhow::Result<String> {
    let prompt = if frames {
        format!(
            "These {} images are frames, in order, of a {} sent in a private chat. In at most 15 words, describe what happens in it and the emotion or reaction it conveys. Write in English, even if the image contains text in another language. Reply with the description only, no quotes.",
            images.len(),
            kind.label()
        )
    } else {
        format!(
            "This image is a {} sent in a private chat{}. In at most 15 words, describe what it shows and the emotion or reaction it conveys. Write in English, even if the image contains text in another language. Reply with the description only, no quotes.",
            kind.label(),
            if matches!(kind, MediaKind::Video | MediaKind::RoundVideo | MediaKind::Gif) { " (this is a frame/thumbnail)" } else { "" }
        )
    };
    let mut content = vec![json!({"type": "text", "text": prompt})];
    content.extend(images);
    let messages = vec![json!({"role": "user", "content": content})];
    let text = ask_media(llm, cfg, &messages, prompt.len()).await?.replace('\n', " ");
    Ok(crate::llm::truncate(&text, 160))
}

/// What to describe.
#[derive(Debug, Default, Clone)]
pub struct Selection {
    /// Only these ids (all items when empty).
    pub ids: Vec<String>,
    /// Re-describe items that already have a description.
    pub force: bool,
}

/// Describes library items (by default those lacking a description) and
/// returns `(id, description)` pairs plus `id: error` strings for failures.
pub async fn describe_library(
    llm: &LlmClient,
    cfg: &LlmConfig,
    files_dir: &Path,
    export: &ChatExport,
    sel: &Selection,
) -> (Vec<(String, String)>, Vec<String>) {
    let jobs: Vec<(usize, Job)> = export
        .library
        .iter()
        .enumerate()
        .filter(|(_, i)| (sel.force || i.description.is_none()) && (sel.ids.is_empty() || sel.ids.contains(&i.id)))
        .filter_map(|(idx, i)| {
            if visual(i.kind) {
                let thumb = image_source(files_dir, i.file.as_deref(), i.thumb.as_deref());
                match frames_source(files_dir, i.file.as_deref()) {
                    Some(p) => Some((idx, Job::Frames(i.kind, p, thumb))),
                    None => thumb.map(|(m, p)| (idx, Job::Image(i.kind, m, p))),
                }
            } else if i.kind == MediaKind::Voice {
                audio_source(files_dir, i.file.as_deref()).map(|(f, p)| (idx, Job::Audio(f, p)))
            } else {
                None
            }
        })
        .collect();
    let results: Vec<(usize, anyhow::Result<String>)> = futures::stream::iter(jobs)
        .map(|(idx, job)| async move {
            let r = match job {
                Job::Audio(format, path) => async { transcribe(llm, cfg, &tokio::fs::read(&path).await?, format).await }.await,
                Job::Image(kind, mime, path) => {
                    async {
                        let bytes = tokio::fs::read(&path).await?;
                        describe_visual(llm, cfg, kind, vec![data_url(&mime, &bytes)], false).await
                    }
                    .await
                }
                Job::Frames(kind, path, thumb) => match crate::frames::extract(&path, crate::frames::FRAMES).await {
                    Ok(frames) => {
                        let images = frames.iter().map(|f| data_url("image/jpeg", f)).collect();
                        describe_visual(llm, cfg, kind, images, true).await
                    }
                    // A file ffmpeg can't read still has its thumbnail, if the export has one.
                    Err(e) => match thumb {
                        Some((mime, p)) => {
                            async {
                                tracing::debug!("frames of {}: {e:#}, using the thumbnail", path.display());
                                let bytes = tokio::fs::read(&p).await?;
                                describe_visual(llm, cfg, kind, vec![data_url(&mime, &bytes)], false).await
                            }
                            .await
                        }
                        None => Err(e),
                    },
                },
            };
            (idx, r)
        })
        .buffer_unordered(4)
        .collect()
        .await;
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    for (idx, r) in results {
        let id = export.library[idx].id.clone();
        match r {
            Ok(d) => ok.push((id, d)),
            Err(e) => failed.push(format!("{id}: {e:#}")),
        }
    }
    (ok, failed)
}
