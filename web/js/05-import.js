// ---------- import ----------
// One import at a time. The status line and bar of both drop zones follow it from
// the first click: reading the folder, uploading, then the server-side job.
const IMP = { busy: false };
const fmtNum = n => Number(n).toLocaleString("en-US");
const fmtBytes = n => n >= 1e9 ? `${(n / 1e9).toFixed(1)} GB` : n >= 1e6 ? `${Math.round(n / 1e6)} MB` : `${Math.max(1, Math.round(n / 1e3))} KB`;
function setImportStatus(text, { frac = null, indeterminate = false, error = false } = {}) {
  for (const [st, bar] of [["#importStatus", "#uploadProgress"], ["#importStatus2", "#uploadProgress2"]]) {
    const el = $(st), p = $(bar);
    el.textContent = text || "";
    el.hidden = !text;
    el.classList.toggle("error", error);
    p.classList.toggle("hidden", frac == null && !indeterminate);
    p.classList.toggle("indeterminate", indeterminate);
    p.firstElementChild.style.width = indeterminate ? "" : `${Math.round((frac || 0) * 100)}%`;
  }
}
function setImportBusy(busy) {
  IMP.busy = busy;
  for (const dz of [$("#dropzone"), $("#dropzone2")]) {
    dz.classList.toggle("busy", busy);
    dz.setAttribute("aria-busy", String(busy));
    dz.querySelectorAll("button").forEach(b => { b.disabled = busy; });
  }
  $("#folderInput").disabled = $("#zipInput").disabled = busy;
}
function importFailed(message) {
  setImportBusy(false);
  setImportStatus(`${message} Choose the folder or .zip again to retry.`, { error: true });
  toast(message, { error: true });
}
// What the server-side job is doing, from GET /api/imports/{id}.
function jobText(j) {
  const of = (what) => j.total ? `${what} ${fmtNum(j.done)} of ${fmtNum(j.total)}` : `${what}…`;
  switch (j.stage) {
    case "unpacking": return of("Unpacking archive: file");
    case "parsing": return j.total > 1 ? `Reading messages: page ${fmtNum(j.done)} of ${fmtNum(j.total)} · ${fmtNum(j.messages)} messages` : `Reading messages · ${fmtNum(j.messages)} messages`;
    case "hashing": return of("Checking media");
    case "placing": return of("Moving media");
    case "saving": return "Saving…";
    default: return j.state === "queued" ? "Waiting for another import to finish…" : "Reading the export…";
  }
}
async function pollImport(id) {
  let failures = 0;
  for (;;) {
    await new Promise(r => setTimeout(r, 500));
    let j;
    try { j = await api(`/api/imports/${id}`); failures = 0; }
    catch (e) { if (++failures > 10) return importFailed(`Lost contact with the import: ${e.message}`); continue; }
    if (j.state === "failed") return importFailed(j.error || "Import failed.");
    if (j.state === "done") return importDone(j);
    setImportStatus(jobText(j), j.total > 1 ? { frac: j.done / j.total } : { indeterminate: true });
  }
}
async function importDone(j) {
  setImportStatus("");
  setImportBusy(false);
  $("#importDlg").close();
  await loadState();
  const skipped = IMP.skipped || []; IMP.skipped = null;
  const done = j.created ? `Imported «${j.export.title}»: ${fmtNum(j.export.messages)} messages` : `«${j.export.title}» was already imported`;
  if (skipped.length) toast(`${done}. ${fmtNum(skipped.length)} media file${skipped.length > 1 ? "s" : ""} couldn't be uploaded and show as missing, e.g. “${skipped[0].path}” (${skipped[0].why}). The browser console lists them.`, { ms: 15000 });
  else toast(done);
  openNewChat(j.export.id);
}
const SKIP = /(^|\/)(\.DS_Store|__MACOSX|Thumbs\.db)(\/|$)|(^|\/)(css|js|images)\//;
const paint = () => new Promise(r => requestAnimationFrame(() => setTimeout(r)));
// Big exports (tens of thousands of files, many GB) go up in small batches, one after the
// other: each request stays short, a broken one is retried on its own, and the browser
// never has to build one giant request. The server imports once the last batch is in.
const BATCH_FILES = 300, BATCH_BYTES = 64e6, BATCH_TRIES = 3;
// Files the import can't do without: the chat itself (HTML/JSON pages, WhatsApp .txt).
// Telegram pages and result.json, or a WhatsApp .txt, at most one folder down; documents
// deeper in the export (files/…) are media like any other.
const ESSENTIAL = { test: p => p.split("/").length <= 2 && /(^|\/)(messages\d*\.html|result\.json|[^/]+\.txt)$/i.test(p) };
function* batchesOf(files) {
  let batch = [], bytes = 0;
  for (const f of files) {
    if (batch.length && (batch.length >= BATCH_FILES || bytes + f.file.size > BATCH_BYTES)) { yield batch; batch = []; bytes = 0; }
    batch.push(f); bytes += f.file.size;
  }
  if (batch.length) yield batch;
}
// One batch as multipart; `onBytes(n)` follows the bytes sent. Rejects with the server's error.
function sendBatch(session, batch, onBytes) {
  return new Promise((resolve, reject) => {
    const fd = new FormData();
    for (const f of batch) fd.append("files", f.file, f.path);
    const xhr = new XMLHttpRequest();
    xhr.open("POST", `/api/uploads/${session}/files`);
    xhr.upload.onprogress = e => { if (e.lengthComputable) onBytes(e.loaded / e.total); };
    xhr.onerror = () => reject(Object.assign(new Error("the connection to the server broke off"), { network: true }));
    xhr.onabort = () => reject(Object.assign(new Error("cancelled"), { aborted: true }));
    xhr.onload = () => {
      let data = {}; try { data = JSON.parse(xhr.responseText); } catch { }
      if (xhr.status >= 300) return reject(Object.assign(new Error(data.error || `HTTP ${xhr.status}`), { status: xhr.status }));
      resolve(data);
    };
    xhr.send(fd);
  });
}
// Files of a batch the browser can no longer read (moved, renamed, changed since they were
// chosen, or cloud placeholders): a single one makes the browser abort the whole request.
async function unreadable(batch) {
  const bad = [];
  for (const f of batch) {
    try { await f.file.slice(0, 1).arrayBuffer(); } catch (e) { bad.push({ ...f, why: e.name || e.message }); }
  }
  return bad;
}
// Sends `batch`, retrying a broken request. A batch that keeps failing although every file
// reads fine is split in halves until the one file that breaks it is found: that file is
// skipped (media) or named (the chat itself). Returns the files it had to skip.
async function sendReliably(session, batch, onBytes, depth = 0) {
  const size = batch.reduce((n, f) => n + f.file.size, 0);
  let error;
  for (let attempt = 1; attempt <= (depth ? 2 : BATCH_TRIES); attempt++) {
    try { await sendBatch(session, batch, frac => onBytes(frac * size)); return []; }
    catch (e) {
      error = e;
      if (e.status === 404 || e.status === 413 || e.status === 507) throw e; // retrying can't help
      const bad = await unreadable(batch);
      if (bad.length) {
        const essential = bad.find(f => ESSENTIAL.test(f.path));
        if (essential) throw new Error(`can't read “${essential.path}” (${essential.why}): it was moved, changed or isn't downloaded yet (iCloud/OneDrive)`);
        console.warn("iRoof.top import: skipping unreadable files", bad.map(f => `${f.path} (${f.why})`));
        const gone = new Set(bad.map(f => f.path));
        const rest = batch.filter(f => !gone.has(f.path));
        return bad.map(f => ({ path: f.path, why: f.why })).concat(rest.length ? await sendReliably(session, rest, onBytes, depth) : []);
      }
      if (attempt < (depth ? 2 : BATCH_TRIES)) {
        setImportStatus(`Upload interrupted (${e.message}), retrying…`, { indeterminate: true });
        await sleep(1000 * attempt);
      }
    }
  }
  if (batch.length === 1) {
    const f = batch[0];
    console.error("iRoof.top import: this file breaks the upload", f.path, f.file.size, "bytes:", error.message);
    if (ESSENTIAL.test(f.path)) throw new Error(`“${f.path}” can't be sent: ${error.message}`);
    return [{ path: f.path, why: error.message }];
  }
  // Same failure every time: find the file that causes it.
  setImportStatus(`A batch keeps failing: narrowing it down (${fmtNum(batch.length)} files)…`, { indeterminate: true });
  const half = Math.ceil(batch.length / 2);
  let done = 0;
  const first = await sendReliably(session, batch.slice(0, half), n => onBytes(n), depth + 1);
  done = batch.slice(0, half).reduce((n, f) => n + f.file.size, 0);
  const second = await sendReliably(session, batch.slice(half), n => onBytes(done + n), depth + 1);
  return first.concat(second);
}
async function upload(files) {
  if (IMP.busy) return;
  IMP.skipped = null;
  setImportBusy(true);
  setImportStatus(`Reading folder… ${fmtNum(files.length)} files`, { indeterminate: true });
  await paint();
  files = files.filter(f => !SKIP.test(f.path));
  if (!files.length) return importFailed("Nothing to upload: the folder has no files.");
  let session;
  try { session = (await api("/api/uploads", { method: "POST" })).id; }
  catch (e) { return importFailed(`Upload failed: ${e.message}.`); }
  const giveUp = message => { api(`/api/uploads/${session}`, { method: "DELETE" }).catch(() => { }); importFailed(message); };
  // The chat itself first (pages, result.json, the WhatsApp .txt), unless it's one archive.
  const zipped = files.length === 1 && /\.zip$/i.test(files[0].path);
  const chat = zipped ? [] : files.filter(f => ESSENTIAL.test(f.path));
  const media = zipped ? files : files.filter(f => !ESSENTIAL.test(f.path));
  const total = files.reduce((n, f) => n + f.file.size, 0);
  const skipped = [];
  let sentBytes = 0, sentFiles = 0;
  const show = (inFlight = 0) => {
    const done = sentBytes + inFlight;
    setImportStatus(`Uploading ${fmtBytes(done)} of ${fmtBytes(total)} (${Math.floor(total ? done / total * 100 : 100)}%) · ${fmtNum(sentFiles)} of ${fmtNum(files.length)} files`, { frac: total ? done / total : 0 });
  };
  const sendAll = async list => {
    for (const batch of batchesOf(list)) {
      skipped.push(...await sendReliably(session, batch, show));
      sentBytes += batch.reduce((n, f) => n + f.file.size, 0); sentFiles += batch.length;
      show();
    }
  };
  try {
    show();
    if (chat.length) {
      await sendAll(chat);
      setImportStatus("Reading the messages…", { indeterminate: true });
      const c = await api(`/api/uploads/${session}/check`, { method: "POST", body: { bytes: media.reduce((n, f) => n + f.file.size, 0) } });
      if (c.existing) {
        api(`/api/uploads/${session}`, { method: "DELETE" }).catch(() => { });
        return importDone({ created: false, export: c.existing });
      }
      if (!c.fits) return giveUp(`Not enough disk space: the media need ${fmtBytes(c.needed_bytes)}, but only ${fmtBytes(c.free_bytes)} is free on the disk of ${c.data_dir || "the data directory"}. Free up space, or import the folder on the server with --import (it copies without an upload).`);
      setImportStatus(`«${c.title}»: ${fmtNum(c.messages)} messages. Uploading the media…`, { frac: sentBytes / total });
    }
    await sendAll(media);
  } catch (e) {
    return giveUp(`Upload failed: ${e.message}.`);
  }
  setImportStatus("Starting the import…", { indeterminate: true });
  let job;
  try { job = await api(`/api/uploads/${session}/import`, { method: "POST" }); }
  catch (e) { return giveUp(`Import failed: ${e.message}.`); }
  // Said when the import is done (a toast now would be replaced by that one).
  IMP.skipped = skipped;
  if (skipped.length) console.warn("iRoof.top import: skipped files", skipped);
  setImportStatus("Reading the export…", { indeterminate: true });
  pollImport(job.id);
}
async function entriesToFiles(items) {
  const out = [];
  async function walk(entry, prefix) {
    if (entry.isFile) {
      const file = await new Promise((res, rej) => entry.file(res, rej));
      out.push({ file, path: prefix + entry.name });
      if (out.length % 200 === 0) setImportStatus(`Reading folder… ${fmtNum(out.length)} files`, { indeterminate: true });
    } else if (entry.isDirectory) {
      const reader = entry.createReader();
      let batch;
      do {
        batch = await new Promise((res, rej) => reader.readEntries(res, rej));
        for (const e of batch) await walk(e, prefix + entry.name + "/");
      } while (batch.length);
    }
  }
  const entries = items.map(i => i.webkitGetAsEntry?.()).filter(Boolean);
  for (const e of entries) await walk(e, "");
  return out;
}
function bindDropzone(el) {
  el.addEventListener("dragover", e => { e.preventDefault(); el.classList.add("drag"); });
  el.addEventListener("dragleave", () => el.classList.remove("drag"));
  el.addEventListener("drop", async e => {
    e.preventDefault(); el.classList.remove("drag");
    if (IMP.busy) return;
    setImportBusy(true);
    setImportStatus("Reading folder…", { indeterminate: true });
    const items = [...e.dataTransfer.items].filter(i => i.kind === "file");
    let files = await entriesToFiles(items);
    if (!files.length) files = [...e.dataTransfer.files].map(f => ({ file: f, path: f.name }));
    setImportBusy(false);
    upload(files);
  });
}
bindDropzone($("#dropzone")); bindDropzone($("#dropzone2"));
// Desktop app: the export is imported from its path. The webview can't be trusted to read
// the files (on macOS, WebKit's sandboxed networking process is refused some of them, and
// uploads arrive empty), so the app shows the native picker and passes on dropped paths.
const NATIVE_FILES = SHELL?.files === "native";
async function importLocal(paths) {
  if (IMP.busy || !paths.length) return;
  IMP.skipped = null;
  setImportBusy(true);
  setImportStatus("Reading the export…", { indeterminate: true });
  let job;
  try { job = await api("/api/local/import", { method: "POST", body: { paths } }); }
  catch (e) { return importFailed(`Import failed: ${e.message}.`); }
  pollImport(job.id);
}
async function pickNative(kind) {
  if (IMP.busy) return;
  setImportStatus(kind === "folder" ? "Waiting for the folder…" : "Waiting for the file…", { indeterminate: true });
  let r;
  try { r = await api("/api/local/pick", { method: "POST", body: { kind } }); }
  catch (e) { return importFailed(`Can't show the file picker: ${e.message}.`); }
  if (r.path) importLocal([r.path]); else if (!IMP.busy) setImportStatus("");
}
// Native drags (see desktop/src/main.rs): a drop on a drop zone, or anywhere over the open
// import dialog, imports. Coordinates are physical pixels from the top left of the page.
const dropTarget = (x, y) => {
  const el = x >= 0 && document.elementFromPoint(x / devicePixelRatio, y / devicePixelRatio);
  if (!el) return null;
  const dlg = $("#importDlg");
  return el.closest(".dropzone") || (dlg.open && dlg.contains(el) ? $("#dropzone2") : null);
};
window.__IROOF_DROP__ = (type, x, y, paths) => {
  const target = type === "leave" || IMP.busy ? null : dropTarget(x, y);
  for (const dz of [$("#dropzone"), $("#dropzone2")]) dz.classList.toggle("drag", type !== "drop" && dz === target);
  if (type === "drop" && target) importLocal(paths);
};
// The folder picker can take long to hand over tens of thousands of files: say so at once.
const pickFolder = () => {
  if (NATIVE_FILES) return pickNative("folder");
  setImportStatus("Waiting for the folder…", { indeterminate: true }); $("#folderInput").click();
};
const pickZip = () => (NATIVE_FILES ? pickNative("file") : $("#zipInput").click());
$("#folderInput").addEventListener("cancel", () => { if (!IMP.busy) setImportStatus(""); });
$("#pickFolder").onclick = pickFolder;
$("#pickZip").onclick = pickZip;
$$("[data-pick]").forEach(b => b.onclick = () => (b.dataset.pick === "zip" ? pickZip() : pickFolder()));
$("#folderInput").onchange = e => {
  const files = [...e.target.files].map(f => ({ file: f, path: f.webkitRelativePath || f.name }));
  e.target.value = "";
  files.length ? upload(files) : setImportStatus("");
};
$("#zipInput").onchange = e => { upload([...e.target.files].map(f => ({ file: f, path: f.name }))); e.target.value = ""; };
// The import dialog shows the same walkthrough as the welcome screen: a copy of that SVG
// with its own ids (gradient and clip ids must stay unique in the document).
function openImport() {
  const box = $("#imTut");
  if (!box.firstElementChild) box.innerHTML = $("#tutorial").outerHTML.replace(/(id="|url\(#|href="#)(tut-[\w-]+|tutorial)/g, "$1$2-im");
  setImportSource(localStorage.getItem("iroof:import-source") || "telegram");
  $("#importDlg").showModal();
}
// Telegram and WhatsApp share the dialog: the same drop zone, different instructions.
function setImportSource(src) {
  const dlg = $("#importDlg");
  dlg.dataset.source = src;
  $$("input[name=imSource]").forEach(r => { r.checked = r.value === src; });
  $$("#importDlg .im-pane").forEach(p => { p.hidden = p.dataset.pane !== src; });
  $("#imDropTitle").textContent = src === "whatsapp" ? "Drop the folder, .zip or .txt here" : "Drop the folder or .zip here";
  $("#imZipLabel").textContent = src === "whatsapp" ? "Choose .zip or .txt…" : "Choose .zip…";
}
$$("input[name=imSource]").forEach(r => r.onchange = () => { localStorage.setItem("iroof:import-source", r.value); setImportSource(r.value); });
$("#importBtn").onclick = openImport;
$$("[data-close]").forEach(b => b.onclick = () => b.closest("dialog").close());

