//! Imports as background jobs. An import of a big export takes a while, so the
//! upload request only hands the files over: a [`Job`] then parses, dedupes,
//! hashes and places them while the UI polls its [`Snapshot`].
//!
//! The pipeline ([`run`]) is shared with the `--import` command line option.
//! It never makes a network request: describing media is a separate, explicit action.

use crate::import::{self, Placement, Progress, Stage};
use crate::state::{AppState, now_string};
use crate::store::{ExportRecord, Paths};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Where a job is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Waiting for the import before it (imports run one at a time).
    Queued,
    Running,
    Done,
    Failed,
}

/// What `GET /api/imports/{id}` returns.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub id: String,
    pub state: State,
    pub stage: Stage,
    pub done: usize,
    pub total: usize,
    /// Messages found so far.
    pub messages: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Set when done: false when the export was imported before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<bool>,
    /// Set when done: summary of the (existing or new) export.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub export: Option<Value>,
}

pub struct Job {
    state: Mutex<Snapshot>,
}

impl Job {
    fn new() -> Self {
        let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        let state = Snapshot {
            id,
            state: State::Queued,
            stage: Stage::Reading,
            done: 0,
            total: 0,
            messages: 0,
            error: None,
            created: None,
            export: None,
        };
        Self { state: Mutex::new(state) }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Snapshot) -> R) -> R {
        f(&mut self.state.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn id(&self) -> String {
        self.with(|s| s.id.clone())
    }

    pub fn snapshot(&self) -> Snapshot {
        self.with(|s| s.clone())
    }

    /// Records progress from the pipeline.
    pub fn report(&self, p: Progress) {
        self.with(|s| {
            s.state = State::Running;
            s.stage = p.stage;
            (s.done, s.total) = (p.done, p.total);
            if p.stage == Stage::Parsing {
                s.messages = p.messages;
            }
        });
    }

    fn finish(&self, result: Result<(Arc<ExportRecord>, bool)>) {
        self.with(|s| match result {
            Ok((rec, created)) => {
                s.state = State::Done;
                s.created = Some(created);
                s.export = Some(crate::server::export_summary(&rec));
            }
            Err(e) => {
                s.state = State::Failed;
                s.error = Some(format!("{e:#}"));
            }
        });
    }
}

/// Finished jobs kept for late pollers.
const KEEP_FINISHED: usize = 16;

/// The jobs of this process, newest last.
#[derive(Default)]
pub struct Jobs {
    list: Mutex<Vec<Arc<Job>>>,
}

impl Jobs {
    pub fn create(&self) -> Arc<Job> {
        let job = Arc::new(Job::new());
        let mut list = self.list.lock().unwrap_or_else(|e| e.into_inner());
        list.push(job.clone());
        let finished = |j: &Arc<Job>| matches!(j.snapshot().state, State::Done | State::Failed);
        while list.iter().filter(|j| finished(j)).count() > KEEP_FINISHED {
            let oldest = list.iter().position(finished).expect("a finished job exists");
            list.remove(oldest);
        }
        job
    }

    pub fn get(&self, id: &str) -> Option<Snapshot> {
        let list = self.list.lock().unwrap_or_else(|e| e.into_inner());
        list.iter().map(|j| j.snapshot()).find(|s| s.id == id)
    }

    pub fn all(&self) -> Vec<Snapshot> {
        self.list.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|j| j.snapshot()).collect()
    }
}

/// Receives progress from blocking worker threads.
pub type Reporter = Arc<dyn Fn(Progress) + Send + Sync>;

/// A reporter that logs the start of every stage and then every quarter of it.
pub fn log_progress() -> Reporter {
    let last = AtomicUsize::new(usize::MAX);
    Arc::new(move |p: Progress| {
        let quarter = (p.done * 4).checked_div(p.total).unwrap_or(0);
        let key = (p.stage as usize) * 8 + quarter;
        if last.swap(key, Ordering::Relaxed) != key {
            tracing::info!("import: {:?} {}/{} ({} messages)", p.stage, p.done, p.total, p.messages);
        }
    })
}

/// Imports the export found in `src` into the data store, unless an export with
/// the same content exists. Returns the record and whether it is new.
///
/// With [`Placement::Move`] the media files are taken out of `src`.
pub async fn run(st: &Arc<AppState>, src: PathBuf, how: Placement, report: Reporter) -> Result<(Arc<ExportRecord>, bool)> {
    let _one_at_a_time = st.import_lock.lock().await;
    report(Progress { stage: Stage::Reading, done: 0, total: 0, messages: 0 });
    let r = report.clone();
    let (staged, fingerprint) = tokio::task::spawn_blocking(move || -> Result<_> {
        let staged = import::parse(&src, &*r)?;
        let fingerprint = import::fingerprint(&staged.export);
        Ok((staged, fingerprint))
    })
    .await??;
    let messages = staged.export.messages.len();
    anyhow::ensure!(messages > 0, "the export contains no messages");
    report(Progress { stage: Stage::Parsing, done: 1, total: 1, messages });
    if let Some(existing) = st.db.read().await.exports.values().find(|r| r.fingerprint == fingerprint).cloned() {
        return Ok((existing, false));
    }
    let (paths, r) = (st.paths.clone(), report.clone());
    let rec = tokio::task::spawn_blocking(move || store_export(&paths, fingerprint, staged, how, &*r)).await??;
    let rec = Arc::new(rec);
    st.db.write().await.exports.insert(rec.id.clone(), rec.clone());
    tracing::info!(title = %rec.export.title, messages, "imported export");
    Ok((rec, true))
}

/// Builds the library, places the media and saves the export, removing what it
/// wrote when a step fails.
fn store_export(
    paths: &Paths,
    fingerprint: String,
    mut staged: import::Staged,
    how: Placement,
    report: import::Report,
) -> Result<ExportRecord> {
    let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
    let result = (|| {
        let rels = staged.link_media(report)?;
        let dest = paths.export_files(&id);
        std::fs::create_dir_all(&dest).with_context(|| format!("creating {}", dest.display()))?;
        import::place_media(&staged.root, &dest, &rels, how, report)?;
        report(Progress { stage: Stage::Saving, done: 0, total: 1, messages: 0 });
        let rec = ExportRecord { id: id.clone(), imported_at: now_string(), fingerprint, export: staged.export };
        paths.save_export(&rec)?;
        Ok(rec)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(paths.export_dir(&id));
    }
    result
}

/// What to import for paths the user picked or dropped in the desktop app: an export
/// folder, a `.zip` or a WhatsApp `.txt` as it is; a file inside an export (`messages.html`),
/// or several files next to each other, stand for the folder they're in when that folder is
/// an export.
pub fn resolve_local(paths: &[PathBuf]) -> Result<PathBuf> {
    let paths: Vec<&PathBuf> = paths.iter().filter(|p| !p.as_os_str().is_empty()).collect();
    anyhow::ensure!(!paths.is_empty(), "no file or folder given");
    for p in &paths {
        anyhow::ensure!(p.is_absolute(), "not an absolute path: {}", p.display());
        anyhow::ensure!(p.exists(), "“{}” doesn't exist (moved or renamed?)", p.display());
    }
    let is_root = |d: &Path| import::find_export_root(d).is_some_and(|r| r == d);
    let ext = |p: &Path, x: &str| p.extension().is_some_and(|e| e.eq_ignore_ascii_case(x));
    if let [p] = paths[..]
        && (p.is_dir() || ext(p, "zip") || (ext(p, "txt") && import::detect_export(p).is_some()))
    {
        return Ok(p.clone());
    }
    let parent = paths[0].parent().filter(|d| paths.iter().all(|p| p.parent() == Some(d)));
    match parent {
        Some(d) if is_root(d) => Ok(d.to_path_buf()),
        _ if paths.len() > 1 => anyhow::bail!("drop one export at a time: its folder, .zip or WhatsApp .txt"),
        _ => anyhow::bail!("“{}” isn't a chat export: choose its folder, .zip or WhatsApp .txt", paths[0].display()),
    }
}

/// Imports an export from this computer (see [`resolve_local`]) as a job. The source is
/// copied and stays as it is; a `.zip` is unpacked into `tmp/` first.
pub async fn run_local(st: Arc<AppState>, job: Arc<Job>, src: PathBuf, unzip_limit: u64) {
    let work = st.paths.tmp().join(uuid::Uuid::new_v4().simple().to_string());
    let result = async {
        if src.is_file() && src.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
            let (zip, out, j) = (src.clone(), work.join("unzipped"), job.clone());
            let out2 = out.clone();
            tokio::task::spawn_blocking(move || unzip(&zip, &out2, unzip_limit, &|p| j.report(p))).await??;
            let j = job.clone();
            return run(&st, out, Placement::Move, Arc::new(move |p| j.report(p))).await;
        }
        let j = job.clone();
        run(&st, src.clone(), Placement::Copy, Arc::new(move |p| j.report(p))).await
    }
    .await;
    let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(work)).await;
    if let Err(e) = &result {
        tracing::warn!(src = %src.display(), "import failed: {e:#}");
    }
    job.finish(result);
}

/// Runs an upload as a job: unpacks a zip if needed, imports and removes the upload directory.
pub async fn run_upload(st: Arc<AppState>, job: Arc<Job>, upload: PathBuf, unzip_limit: u64) {
    let result = import_upload(&st, &job, &upload, unzip_limit).await;
    let dir = upload.clone();
    let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(dir)).await;
    if let Err(e) = &result {
        tracing::warn!("import failed: {e:#}");
    }
    job.finish(result);
}

async fn import_upload(st: &Arc<AppState>, job: &Arc<Job>, upload: &Path, unzip_limit: u64) -> Result<(Arc<ExportRecord>, bool)> {
    let mut src = upload.join("files");
    let probe = src.clone();
    let has_export = tokio::task::spawn_blocking(move || import::find_export_root(&probe).is_some()).await?;
    if !has_export {
        let probe = src.clone();
        if let Some(zip) = tokio::task::spawn_blocking(move || find_zip(&probe, 3)).await? {
            let out = upload.join("unzipped");
            let (out2, j) = (out.clone(), job.clone());
            tokio::task::spawn_blocking(move || unzip(&zip, &out2, unzip_limit, &|p| j.report(p))).await??;
            src = out;
        }
    }
    let j = job.clone();
    run(st, src, Placement::Move, Arc::new(move |p| j.report(p))).await
}

/// The first `.zip` file under `dir`, at most `depth` directories down.
fn find_zip(dir: &Path, depth: u32) -> Option<PathBuf> {
    let mut subdirs = Vec::new();
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            subdirs.push(p);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("zip")) {
            return Some(p);
        }
    }
    (depth > 0).then(|| subdirs.iter().find_map(|d| find_zip(d, depth - 1))).flatten()
}

/// Extracts `zip` into `out`, refusing archives that unpack to more than `limit` bytes.
fn unzip(zip: &Path, out: &Path, limit: u64, report: &dyn Fn(Progress)) -> Result<()> {
    let f = std::fs::File::open(zip)?;
    let mut archive = zip::ZipArchive::new(f).context("not a valid zip archive")?;
    let total = archive.len();
    let mut written = 0u64;
    for i in 0..total {
        let mut entry = archive.by_index(i)?;
        let Some(rel) = entry.enclosed_name() else { continue };
        let dest = out.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&dest)?;
            continue;
        }
        written += entry.size();
        anyhow::ensure!(written <= limit, "zip archive is too large when extracted");
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::io::copy(&mut entry, &mut std::fs::File::create(&dest)?)?;
        if (i + 1) % 64 == 0 || i + 1 == total {
            report(Progress { stage: Stage::Unpacking, done: i + 1, total, messages: 0 });
        }
    }
    Ok(())
}
