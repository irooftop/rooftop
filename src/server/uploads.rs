//! Getting exports in: uploads in batches (web UI), one-shot uploads, imports by path
//! (desktop app), and the background import jobs they start.

use super::exports::export_summary;
use super::{ApiError, ApiResult, Desktop, Pick, S, bad};
use crate::import::safe_rel;
use crate::state::AppState;
use crate::store::ExportRecord;
use anyhow::anyhow;
use axum::body::Body;
use axum::extract::{Multipart, Path, Request, State};
use axum::http::{StatusCode, header};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

/// Imports an export directory the caller owns (it is copied, never moved), without progress.
/// Returns (record, newly_created).
pub async fn import_into_store(st: &Arc<AppState>, src: PathBuf) -> anyhow::Result<(Arc<ExportRecord>, bool)> {
    crate::imports::run(st, src, crate::import::Placement::Copy, Arc::new(|_| {})).await
}

/// Streams the uploaded files to a temporary directory, then starts a background
/// import job and answers `202 {"id": …}` at once: progress is `GET /api/imports/{id}`.
///
/// One request carries the whole export. The web UI uses [upload sessions](create_upload)
/// instead, so a big export travels in small batches that can be retried one by one.
pub(super) async fn upload_export(State(st): S, mut mp: Multipart) -> ApiResult<(StatusCode, Json<Value>)> {
    let tmp = st.paths.tmp().join(uuid::Uuid::new_v4().simple().to_string());
    let received = receive_files(&mut mp, &tmp.join("files")).await;
    match received {
        Ok(r) if r.files > 0 => {}
        Ok(_) => {
            let _ = tokio::fs::remove_dir_all(&tmp).await;
            return Err(bad("no files uploaded"));
        }
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&tmp).await;
            return Err(e);
        }
    }
    Ok((StatusCode::ACCEPTED, Json(json!({"id": start_import_job(&st, tmp)}))))
}

/// `/api/local/*`, desktop app only: the native picker and imports by path.
pub(super) fn local_routes(desktop: Desktop) -> Router<Arc<AppState>> {
    #[derive(Deserialize)]
    struct PickReq {
        kind: Pick,
    }
    #[derive(Deserialize)]
    struct ImportReq {
        paths: Vec<PathBuf>,
    }
    let pick = desktop.pick.clone();
    Router::new()
        // `Json` bodies only: a cross-site page can't send those without a CORS preflight,
        // which this server never answers.
        .route(
            "/api/local/pick",
            post(move |Json(req): Json<PickReq>| async move {
                let path = pick(req.kind).await;
                Json(json!({"path": path.map(|p| p.display().to_string())}))
            }),
        )
        .route(
            "/api/local/import",
            post(|State(st): S, Json(req): Json<ImportReq>| async move {
                let paths = req.paths;
                let src = tokio::task::spawn_blocking(move || crate::imports::resolve_local(&paths))
                    .await
                    .map_err(anyhow::Error::from)?
                    .map_err(|e| bad(format!("{e:#}")))?;
                let job = st.imports.create();
                let id = job.id();
                let name = src.file_name().map(|n| n.to_string_lossy().into_owned());
                tokio::spawn(crate::imports::run_local(st.clone(), job, src, UNZIP_LIMIT));
                ApiResult::<(StatusCode, Json<Value>)>::Ok((StatusCode::ACCEPTED, Json(json!({"id": id, "name": name}))))
            }),
        )
}

/// Starts the background import of the upload directory `tmp` (its `files/`), which the job removes.
fn start_import_job(st: &Arc<AppState>, tmp: PathBuf) -> String {
    let job = st.imports.create();
    let id = job.id();
    tokio::spawn(crate::imports::run_upload(st.clone(), job, tmp, UNZIP_LIMIT));
    id
}

/// Largest total size an uploaded zip archive may unpack to.
const UNZIP_LIMIT: u64 = 64 * 1024 * 1024 * 1024;

/// Upload sessions live in `tmp/` (emptied at start-up) under this prefix.
const UPLOAD_PREFIX: &str = "up-";

/// Sessions nobody touched for this long are abandoned (a closed tab) and removed.
const UPLOAD_MAX_IDLE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Directory of upload session `id`, if the id is well formed and the session exists.
async fn upload_dir(st: &AppState, id: &str) -> Result<PathBuf, ApiError> {
    let valid = id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit());
    let dir = st.paths.tmp().join(format!("{UPLOAD_PREFIX}{id}"));
    if !valid || !tokio::fs::try_exists(dir.join("files")).await.unwrap_or(false) {
        return Err(ApiError(StatusCode::NOT_FOUND, "upload session not found: start the upload again".into()));
    }
    Ok(dir)
}

/// `POST /api/uploads`: starts an upload session. The files then come in any number of
/// `POST /api/uploads/{id}/files` batches, `POST /api/uploads/{id}/check` looks at the
/// chat before the media, and `POST /api/uploads/{id}/import` imports them.
pub(super) async fn create_upload(State(st): S) -> ApiResult {
    let tmp = st.paths.tmp();
    // Forget sessions that were left behind.
    let stale = tmp.clone();
    let _ = tokio::task::spawn_blocking(move || {
        for e in std::fs::read_dir(&stale).into_iter().flatten().flatten() {
            let idle = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
            if e.file_name().to_string_lossy().starts_with(UPLOAD_PREFIX) && idle.is_some_and(|d| d > UPLOAD_MAX_IDLE) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    })
    .await;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let dir = tmp.join(format!("{UPLOAD_PREFIX}{id}")).join("files");
    tokio::fs::create_dir_all(&dir).await.map_err(anyhow::Error::from)?;
    Ok(Json(json!({"id": id})))
}

/// `POST /api/uploads/{id}/files`: one multipart batch of files with their relative paths.
/// Sending a batch again replaces its files, so a failed batch can simply be retried.
pub(super) async fn upload_batch(State(st): S, Path(id): Path<String>, req: Request) -> ApiResult {
    use axum::extract::FromRequest;
    use futures::StreamExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    let dir = upload_dir(&st, &id).await?;
    // Count what arrives: when a batch breaks, received vs. declared tells whether the
    // browser sent a short body or the connection was cut.
    let declared = req.headers().get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
    let received = Arc::new(AtomicU64::new(0));
    let (parts, body) = req.into_parts();
    let counter = received.clone();
    let counted = body.into_data_stream().inspect(move |r| {
        if let Ok(b) = r {
            counter.fetch_add(b.len() as u64, Ordering::Relaxed);
        }
    });
    let req = Request::from_parts(parts, Body::from_stream(counted));
    let mut mp = Multipart::from_request(req, &()).await.map_err(|e| bad(format!("upload failed: {}", e.body_text())))?;
    let r = receive_files(&mut mp, &dir.join("files")).await.map_err(|mut e| {
        let got = received.load(Ordering::Relaxed);
        let of = declared.map(|d| format!(" of {d}")).unwrap_or_default();
        tracing::warn!(upload = %id, received = got, declared = ?declared, "upload batch failed: {}", e.1);
        e.1 = format!("{} [the server received {got}{of} bytes]", e.1);
        e
    })?;
    // Mark the session as in use (see UPLOAD_MAX_IDLE).
    let _ = std::fs::File::open(&dir).and_then(|f| f.set_modified(std::time::SystemTime::now()));
    Ok(Json(json!({"files": r.files, "bytes": r.bytes})))
}

#[derive(Deserialize, Default)]
pub(super) struct CheckReq {
    /// Bytes still to upload, for the disk space check.
    #[serde(default)]
    bytes: u64,
}

/// `POST /api/uploads/{id}/check`, after the chat itself (pages, `result.json` or the
/// WhatsApp `.txt`) is uploaded and before the media: reads the messages, says whether
/// this export is already imported, and whether the rest fits on the disk. A big upload
/// then fails (or turns out to be unnecessary) in seconds instead of after many gigabytes.
pub(super) async fn check_upload(State(st): S, Path(id): Path<String>, body: Option<Json<CheckReq>>) -> ApiResult {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let dir = upload_dir(&st, &id).await?;
    let files = dir.join("files");
    let parsed = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let staged = crate::import::parse(&files, &|_| {})?;
        Ok((crate::import::fingerprint(&staged.export), staged.export.title.clone(), staged.export.messages.len()))
    })
    .await
    .map_err(anyhow::Error::from)?;
    let (fingerprint, title, messages) = parsed.map_err(|e| bad(format!("{e:#}")))?;
    let existing = st.db.read().await.exports.values().find(|r| r.fingerprint == fingerprint).map(|r| export_summary(r));
    let free = free_space(&dir);
    // The import moves the files from tmp/ into the data directory: without a copy when both
    // are on the same disk. Keep a margin for the export's JSON and the system.
    let need = req.bytes + req.bytes / 20 + 512 * 1024 * 1024;
    let fits = existing.is_some() || free.is_none_or(|f| f >= need);
    Ok(Json(json!({
        "title": title, "messages": messages, "existing": existing,
        "free_bytes": free, "needed_bytes": need, "fits": fits,
        "data_dir": st.paths.tmp().parent().map(|p| p.display().to_string()),
    })))
}

/// Free space for an unprivileged user on the file system of `path`, where known.
#[cfg(unix)]
fn free_space(path: &std::path::Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `s` a writable statvfs.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)] // the field types differ between platforms
    Some(s.f_bavail as u64 * s.f_frsize as u64)
}

#[cfg(not(unix))]
fn free_space(_path: &std::path::Path) -> Option<u64> {
    None
}

/// `POST /api/uploads/{id}/import`: imports the files of the session in the background,
/// like `POST /api/exports`.
pub(super) async fn finish_upload(State(st): S, Path(id): Path<String>) -> ApiResult<(StatusCode, Json<Value>)> {
    let dir = upload_dir(&st, &id).await?;
    // The job owns the files from now on: take them out of the session namespace first,
    // so a repeated request can't start a second import of the same directory.
    let job_dir = st.paths.tmp().join(uuid::Uuid::new_v4().simple().to_string());
    tokio::fs::rename(&dir, &job_dir).await.map_err(|_| ApiError(StatusCode::CONFLICT, "this upload is already being imported".into()))?;
    let files = job_dir.join("files");
    let empty =
        tokio::task::spawn_blocking(move || std::fs::read_dir(files).map(|mut d| d.next().is_none()).unwrap_or(true)).await.unwrap_or(true);
    if empty {
        let _ = tokio::fs::remove_dir_all(&job_dir).await;
        return Err(bad("no files uploaded"));
    }
    Ok((StatusCode::ACCEPTED, Json(json!({"id": start_import_job(&st, job_dir)}))))
}

/// `DELETE /api/uploads/{id}`: drops the files of an upload that won't be imported.
pub(super) async fn cancel_upload(State(st): S, Path(id): Path<String>) -> ApiResult {
    let dir = upload_dir(&st, &id).await?;
    tokio::fs::remove_dir_all(&dir).await.map_err(anyhow::Error::from)?;
    Ok(Json(json!({"ok": true})))
}

/// What [`receive_files`] wrote.
struct Received {
    files: usize,
    bytes: u64,
}

/// A broken multipart body: name the real cause (axum's own message is only "Error parsing
/// `multipart/form-data` request") and log it, so the owner can tell what happened.
pub(crate) fn multipart_error(e: axum::extract::multipart::MultipartError, file: Option<&str>) -> ApiError {
    let cause = e.body_text();
    let hint = if cause.contains("incomplete") || cause.contains("stream") || cause.contains("connection") {
        " (the browser stopped sending: a file could not be read, or the connection dropped)"
    } else {
        ""
    };
    let at = file.map(|f| format!(" at `{f}`")).unwrap_or_default();
    tracing::warn!(status = %e.status(), "upload failed{at}: {cause}");
    ApiError(e.status(), format!("upload failed{at}: {cause}{hint}"))
}

/// Writes the file fields of `mp` under `dest`. Each file goes to a `.part` file that is
/// renamed when complete, so a broken request never leaves a truncated file behind.
async fn receive_files(mp: &mut Multipart, dest: &std::path::Path) -> Result<Received, ApiError> {
    let io = |e: std::io::Error| ApiError::from(anyhow::Error::from(e));
    let mut out = Received { files: 0, bytes: 0 };
    let mut last: Option<String> = None;
    loop {
        let field = match mp.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => return Err(multipart_error(e, last.as_deref())),
        };
        let mut field = field;
        let Some(name) = field.file_name().map(|s| s.replace('\\', "/")) else { continue };
        let rel = safe_rel(&name).ok_or_else(|| bad(format!("invalid file path `{name}`")))?;
        let path = dest.join(&rel);
        if let Some(p) = path.parent() {
            tokio::fs::create_dir_all(p).await.map_err(io)?;
        }
        let mut part_name = path.file_name().unwrap_or_default().to_os_string();
        part_name.push(".part");
        let part = path.with_file_name(part_name);
        let written: Result<u64, ApiError> = async {
            let mut f = tokio::io::BufWriter::new(tokio::fs::File::create(&part).await.map_err(io)?);
            let mut n = 0u64;
            loop {
                match field.chunk().await {
                    Ok(Some(chunk)) => {
                        n += chunk.len() as u64;
                        f.write_all(&chunk).await.map_err(io)?;
                    }
                    Ok(None) => break,
                    Err(e) => return Err(multipart_error(e, Some(&name))),
                }
            }
            f.flush().await.map_err(io)?;
            Ok(n)
        }
        .await;
        match written {
            Ok(n) => {
                tokio::fs::rename(&part, &path).await.map_err(io)?;
                out.files += 1;
                out.bytes += n;
                last = Some(name);
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(e);
            }
        }
    }
    Ok(out)
}

pub(super) async fn get_import(State(st): S, Path(id): Path<String>) -> ApiResult {
    let snap = st.imports.get(&id).ok_or_else(|| anyhow!("import not found"))?;
    Ok(Json(json!(snap)))
}
