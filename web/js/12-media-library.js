// ---------- media library (descriptions for the model) ----------
const VISUAL = ["sticker", "gif", "photo", "video", "round_video", "voice"];
const MD = { exportId: null, items: [], filter: "all", query: "", busy: new Set(), shown: 0, audio: null, io: null,
  sort: localStorage.getItem("iroof:md-sort") || "uses", minUses: Number(localStorage.getItem("iroof:md-min-uses")) || 1 };
const MD_PAGE = 60; // cards rendered per step: big libraries stay fast
const MD_FILTERS = [["all", "All"], ["missing", "Missing"], ["described", "Described"], ["sticker", "Stickers"], ["gif", "GIFs"], ["photo", "Photos"], ["video", "Videos"], ["voice", "Voice"]];
const MD_KIND = { sticker: ["sticker", "Sticker"], gif: ["film", "GIF"], photo: ["image", "Photo"], video: ["film", "Video"], round_video: ["record", "Video message"], voice: ["mic", "Voice"] };
async function openMedia(exportId) {
  try {
    const ex = await api(`/api/exports/${exportId}`);
    MD.exportId = exportId; MD.items = ex.library_items.filter(i => VISUAL.includes(i.kind)); MD.ffmpeg = ex.ffmpeg !== false;
    for (const i of MD.items) {
      i.firstUse = i.uses.reduce((m, u) => Math.min(m, u), Infinity);
      i.lastUse = i.uses.reduce((m, u) => Math.max(m, u), -Infinity);
    }
    if (MD.lastRun?.exportId !== exportId) MD.lastRun = null;
    MD.filter = "all"; MD.query = ""; $("#mdSearch").value = "";
    $("#mdSort").value = MD.sort; $("#mdMinUses").value = String(MD.minUses);
    $("#mdH").textContent = `Media library · ${ex.title}`;
    renderMedia();
    if (!$("#mediaDlg").open) {
      $("#mediaDlg").showModal();
      // Search is the natural first step with a mouse. On touch screens, don't pop up the keyboard.
      if (matchMedia("(pointer: fine)").matches) $("#mdSearch").focus(); else $("#mediaDlg .dlg").focus({ preventScroll: true });
    }
    $("#mdGrid").scrollTop = 0;
  } catch (e) { toast(e.message, { error: true }); }
}
$("#mediaDlg").addEventListener("close", () => { mdStopAudio(); MD.io?.disconnect(); $("#mdGrid").innerHTML = ""; });
const mdItem = id => MD.items.find(i => i.id === id);
const mdIsVideo = p => /\.(mp4|webm|mov|m4v)$/i.test(p || "");
function mdThumb(i) {
  const u = p => esc(mediaUrl(p, MD.exportId));
  if (!i.available) return `<div class="md-ph" title="Not included in the export">${icon(MD_KIND[i.kind]?.[0] || "image")}</div>`;
  if (i.kind === "voice") {
    const on = MD.audio?.dataset.src === mediaUrl(i.file, MD.exportId) && !MD.audio.paused;
    return `<button type="button" class="md-play ${on ? "on" : ""}" data-play="${u(i.file)}" aria-label="${on ? "Pause" : "Play"} voice message">${icon(on ? "pause" : "play")}</button>`;
  }
  // Videos (and video stickers) show a still first frame and play only while hovered or focused: see mdObserve().
  let media;
  if (i.kind === "photo" || (i.kind !== "video" && i.file && !mdIsVideo(i.file) && !/\.tgs$/i.test(i.file))) media = `<img src="${u(i.file)}" alt="" loading="lazy" decoding="async">`;
  else if (i.thumb) media = `<img src="${u(i.thumb)}" alt="" loading="lazy" decoding="async">`;
  else if (mdIsVideo(i.file)) media = `<video data-src="${u(i.file)}" muted loop playsinline preload="none"></video><span class="md-anim" aria-hidden="true">${icon("play")}</span>`;
  else return `<div class="md-ph">${icon("image")}</div>`;
  return `<a class="md-open" href="${u(i.file)}" target="_blank" rel="noopener" title="Open the original">${media}</a>`;
}
function mdStatus(i) {
  if (MD.busy.has(i.id)) return `<span class="md-badge busy"><span class="spin" aria-hidden="true"></span>${i.kind === "voice" ? "Transcribing" : "Describing"}</span>`;
  if (i.saved) return `<span class="md-badge ok">${icon("check")}Saved</span>`;
  if (!i.description) return `<span class="md-badge missing">Missing</span>`;
  return "";
}
function mdCard(i) {
  const [ic, label] = MD_KIND[i.kind] || ["image", i.kind];
  const busy = MD.busy.has(i.id), voice = i.kind === "voice";
  const what = voice ? "Transcript" : "Description";
  const ai = i.describable
    ? `<button type="button" class="md-ai" data-ai="${esc(i.id)}" title="${i.description ? `${voice ? "Transcribe" : "Describe"} again with AI` : `${voice ? "Transcribe" : "Describe"} with AI`}" aria-label="${i.description ? "Redo" : "Write"} the ${what.toLowerCase()} of ${esc(i.id)} with AI" ${busy ? "disabled" : ""}>${icon(i.description ? "refresh" : "sparkle")}</button>`
    : "";
  const placeholder = voice ? (i.describable ? "Not transcribed yet: use ✦ or type what is said" : "Type what is said") : i.describable ? "Not described yet: use ✦ or type what it shows"
    : !MD.ffmpeg && /\.(mp4|webm|gif|mov|m4v|mkv)$/i.test(i.file || "") ? "Install ffmpeg to describe it with ✦, or type what it shows" : "No preview: type what it shows";
  return `<div class="md-card ${busy ? "busy" : ""} ${i.description ? "" : "empty"}" data-id="${esc(i.id)}" role="listitem">
    <div class="md-side"><div class="md-thumb">${mdThumb(i)}</div><code class="md-id" title="${esc(i.id)}">${esc(i.id)}</code></div>
    <div class="md-body">
      <div class="md-meta"><span class="md-kind">${icon(ic)}${esc(label)}</span><span class="md-uses" title="Times it appears in the chat">${i.uses.length}×</span><span class="md-status">${mdStatus(i)}</span>${ai}</div>
      <textarea class="md-desc" rows="2" maxlength="1000" aria-label="${what} of ${esc(i.id)}" placeholder="${placeholder}" ${busy ? "disabled" : ""}>${esc(i.description || "")}</textarea>
    </div></div>`;
}
// Message ids grow with time, so the first and last use date an item.
const MD_SORTS = {
  uses: (a, b) => b.uses.length - a.uses.length,
  rare: (a, b) => a.uses.length - b.uses.length,
  recent: (a, b) => b.lastUse - a.lastUse,
  first: (a, b) => a.firstUse - b.firstUse,
};
const mdKindOk = (i, k) => k === "all" ? true : k === "missing" ? !i.description : k === "described" ? !!i.description
  : k === "video" ? i.kind === "video" || i.kind === "round_video" : i.kind === k;
// Everything but the kind tab: what the tab counts are counted over.
function mdScoped() {
  const q = MD.query.trim().toLowerCase();
  return MD.items.filter(i => i.uses.length >= MD.minUses && (!q || i.id.toLowerCase().includes(q) || (i.description || "").toLowerCase().includes(q)));
}
// The list as shown, in the chosen order (stable: ties keep the server's most-used order).
function mdVisible() {
  const list = mdScoped().filter(i => mdKindOk(i, MD.filter));
  return MD.sort === "uses" ? list : list.sort(MD_SORTS[MD.sort] || MD_SORTS.uses);
}
// What "Describe with AI" would send: the shown items that need it, top to bottom.
const mdTodo = () => mdVisible().filter(i => !i.description && i.describable);
// Header: filters, progress and the bulk button. Cheap, and it never touches the cards.
function renderMediaHeader() {
  const described = MD.items.filter(i => i.description).length, total = MD.items.length;
  const manualOnly = MD.items.filter(i => !i.description && !i.describable).length;
  const scoped = mdScoped();
  $("#mdFilters").innerHTML = MD_FILTERS.map(([k, label]) => {
    const n = scoped.filter(i => mdKindOk(i, k)).length;
    return n || k === "all" || k === MD.filter ? `<button role="tab" aria-selected="${MD.filter === k}" data-filter="${k}">${label} <span>${fmtNum(n)}</span></button>` : "";
  }).join("");
  const run = MD.run, meter = $("#mdMeter");
  // While describing, the bar follows the run; otherwise the whole library.
  const [now, max] = run ? [run.done, run.total] : [described, total];
  meter.style.setProperty("--p", String(max ? (now / max) * 100 : 0));
  meter.setAttribute("aria-label", run ? "Describing" : "Described");
  meter.setAttribute("aria-valuemax", String(max)); meter.setAttribute("aria-valuenow", String(now));
  meter.classList.toggle("done", !run && total > 0 && described === total);
  meter.classList.toggle("running", !!run);
  $("#mdProgress").classList.toggle("running", !!run);
  const todo = mdTodo().length;
  // In the header: a ring and a few words ("12/13 described", "Describing 40/120 · 2 min");
  // the full sentence is the tooltip and what screen readers hear.
  let short, full;
  if (run) {
    const failed = run.failed.length;
    short = `<span class="w">${run.stop ? "Stopping" : "Describing"} </span>${fmtNum(Math.min(run.done + run.active, run.total))}/${fmtNum(run.total)}${mdEta(run) ? `<span class="w"> · ${mdEta(run)}</span>` : ""}${failed ? ` <span class="bad">· ${fmtNum(failed)} failed</span>` : ""}`;
    full = mdRunText(run);
  } else {
    const last = MD.lastRun;
    const failed = last?.failed || 0;
    short = `${fmtNum(described)}/${fmtNum(total)}<span class="w"> described</span>${failed ? ` <span class="bad">· ${fmtNum(failed)} failed</span>` : ""}`;
    full = `${fmtNum(described)} of ${fmtNum(total)} described` + (manualOnly ? ` · ${fmtNum(manualOnly)} to type by hand` : "")
      + (last ? ` · last run: ${fmtNum(last.ok)} described${last.failed ? `, ${fmtNum(last.failed)} failed` : ""}${last.left ? `, stopped with ${fmtNum(last.left)} left` : ""}` : "");
  }
  $("#mdSummary").innerHTML = short;
  $("#mdProgress").title = full;
  $("#mdSummary").setAttribute("aria-label", full);
  const btn = $("#mdDescribe");
  btn.disabled = run ? run.stop : !todo;
  btn.classList.toggle("primary", !run);
  btn.title = run ? "Stop after the descriptions in progress" : todo ? "Describes the shown items that have no description yet, top to bottom" : "";
  btn.querySelector("span").textContent = run ? (run.stop ? "Stopping…" : `Stop · ${fmtNum(run.done)}/${fmtNum(run.total)}`) : todo ? `Describe ${fmtNum(todo)} with AI` : "Nothing to describe here";
}
const fmtDur = s => s >= 5400 ? `${Math.round(s / 3600)} h` : s >= 90 ? `${Math.round(s / 60)} min` : `${Math.max(1, Math.round(s))} s`;
// "6 min left" once the rate is known.
function mdEta(run) {
  const secs = (performance.now() - run.started) / 1000, rate = run.done / secs;
  return run.stop || run.done < 3 || !(rate > 0) ? "" : `${fmtDur((run.total - run.done) / rate)} left`;
}
// "Describing 120 of 1,234 · 2 failed · 3.1/s · about 6 min left"
function mdRunText(run) {
  if (run.stop) return `Stopping: finishing ${run.active} in progress…`;
  const secs = (performance.now() - run.started) / 1000, rate = run.done / secs;
  const left = run.total - run.done;
  let t = `Describing ${fmtNum(Math.min(run.done + run.active, run.total))} of ${fmtNum(run.total)}`;
  if (run.failed.length) t += ` · ${fmtNum(run.failed.length)} failed`;
  if (run.done >= 3 && rate > 0) t += ` · ${rate >= 1 ? rate.toFixed(1) + "/s" : Math.round(rate * 60) + "/min"} · about ${fmtDur(left / rate)} left`;
  return t;
}
function renderMedia() {
  renderMediaHeader();
  const list = mdVisible();
  MD.shown = Math.min(list.length, Math.max(MD_PAGE, MD.shown));
  const more = list.length - MD.shown;
  $("#mdGrid").innerHTML = list.length
    ? list.slice(0, MD.shown).map(mdCard).join("") + (more > 0 ? `<button type="button" class="btn md-more" data-md-more>Show ${Math.min(more, MD_PAGE)} more of ${more}</button>` : "")
    : `<div class="md-empty muted">${MD.query ? `Nothing matches “${esc(MD.query)}”.` : MD.filter === "missing" ? "Everything here is described." : MD.minUses > 1 ? `Nothing used ${MD.minUses}+ times here.` : "Nothing here."}</div>`;
  mdObserve();
}
// Replaces one card in place (keeps scroll, focus elsewhere and other videos).
function updateMediaCard(id) {
  const el = $(`.md-card[data-id="${CSS.escape(id)}"]`), it = mdItem(id); if (!el || !it) return;
  const t = document.createElement("template"); t.innerHTML = mdCard(it);
  const card = t.content.firstElementChild; el.replaceWith(card); mdObserve(card);
}
function mdObserve(root = $("#mdGrid")) {
  // Load a video only near the viewport, as a still frame (#t= makes Safari paint it too).
  MD.io ??= new IntersectionObserver(entries => {
    for (const { target: v, isIntersecting } of entries) {
      if (isIntersecting) { if (!v.src) { v.preload = "metadata"; v.src = `${v.dataset.src}#t=0.1`; } }
      else v.pause();
    }
  }, { rootMargin: "120px" }); // viewport root: clipping by the scrolling grid is taken into account
  root.querySelectorAll("video[data-src]").forEach(v => MD.io.observe(v));
}
// One request per item, a few at a time: each card fills in (and is saved) as soon as its
// answer arrives, and a stopped or failing run keeps everything finished so far.
const MD_PARALLEL = 4;
function runDescribe(ids, force) {
  const fresh = ids.filter(id => !MD.busy.has(id));
  if (!fresh.length) return;
  const run = MD.run ??= { queue: [], total: 0, done: 0, ok: 0, failed: [], active: 0, stop: false, started: performance.now(), exportId: MD.exportId };
  for (const id of fresh) { MD.busy.add(id); run.queue.push({ id, force }); }
  run.total += fresh.length;
  renderMediaHeader(); fresh.forEach(updateMediaCard);
  while (run.active < MD_PARALLEL && run.queue.length) mdWorker(run);
}
async function mdWorker(run) {
  run.active++;
  while (run.queue.length && !run.stop) {
    const { id, force } = run.queue.shift();
    try {
      const r = await api(`/api/exports/${run.exportId}/describe`, { method: "POST", body: { ids: [id], force } });
      const d = r.items?.[id];
      if (d != null) { const it = MD.exportId === run.exportId && mdItem(id); if (it) it.description = d; run.ok++; }
      else if (r.failed?.length) run.failed.push(r.failed[0].replace(/^[\w-]+: /, ""));
    } catch (e) { run.failed.push(e.message); }
    run.done++; MD.busy.delete(id);
    // Nothing works (no key, wrong model…): stop instead of paying for the rest.
    if (!run.ok && run.failed.length >= 3) mdStopRun(run);
    if ($("#mediaDlg").open) { renderMediaHeader(); updateMediaCard(id); }
    refreshChatLibrary();
  }
  if (--run.active === 0) mdFinishRun(run);
}
function mdStopRun(run) {
  run.stop = true;
  const left = run.queue.splice(0).map(q => q.id);
  left.forEach(id => MD.busy.delete(id));
  if ($("#mediaDlg").open) { renderMediaHeader(); left.forEach(updateMediaCard); }
}
function mdFinishRun(run) {
  if (MD.run === run) MD.run = null;
  MD.lastRun = { ok: run.ok, failed: run.failed.length, left: run.total - run.done, exportId: run.exportId };
  if ($("#mediaDlg").open) renderMediaHeader();
  const skipped = run.total - run.done;
  const what = `Described ${run.ok} of ${run.total}`;
  if (run.failed.length) errorToast(`${what}, ${run.failed.length} failed${skipped ? `, ${skipped} skipped` : ""}. ${run.failed[0]}`);
  else if (skipped) toast(`${what}, stopped. Describe again to continue with the rest.`);
  else if (run.total > 1 || !run.ok) toast(run.ok ? what : "Nothing to describe");
  loadState().catch(() => { });
}
function refreshChatLibrary() {
  if (S.chat?.export.id !== MD.exportId || !S.chat.library) return;
  for (const it of MD.items) { const l = S.chat.library.find(x => x.id === it.id); if (l) l.description = it.description; }
}
// No autoplay: a video plays while the pointer is over it or its card link has focus.
const mdVideoOf = el => el?.closest?.(".md-open")?.querySelector("video");
function mdPreview(v, on) {
  if (!v) return;
  if (on && !matchMedia("(prefers-reduced-motion: reduce)").matches) { if (!v.src) v.src = v.dataset.src; v.play().catch(() => { }); }
  else v.pause();
  v.closest(".md-open").classList.toggle("playing", on);
}
$("#mdGrid").addEventListener("pointerover", e => { if (e.pointerType === "mouse") mdPreview(mdVideoOf(e.target), true); });
$("#mdGrid").addEventListener("pointerout", e => { const a = e.target.closest?.(".md-open"); if (a && !a.contains(e.relatedTarget)) mdPreview(mdVideoOf(a), false); });
$("#mdGrid").addEventListener("focusin", e => mdPreview(mdVideoOf(e.target), true));
$("#mdGrid").addEventListener("focusout", e => mdPreview(mdVideoOf(e.target), false));
$("#mdSort").onchange = e => { MD.sort = e.target.value; localStorage.setItem("iroof:md-sort", MD.sort); MD.shown = 0; renderMedia(); $("#mdGrid").scrollTop = 0; };
$("#mdMinUses").onchange = e => { MD.minUses = Number(e.target.value) || 1; localStorage.setItem("iroof:md-min-uses", String(MD.minUses)); MD.shown = 0; renderMedia(); $("#mdGrid").scrollTop = 0; };
$("#mdFilters").onclick = e => { const b = e.target.closest("[data-filter]"); if (b) { MD.filter = b.dataset.filter; MD.shown = 0; renderMedia(); $("#mdGrid").scrollTop = 0; } };
let mdSearchTimer;
$("#mdSearch").addEventListener("input", e => { clearTimeout(mdSearchTimer); mdSearchTimer = setTimeout(() => { MD.query = e.target.value; MD.shown = 0; renderMedia(); }, 150); });
$("#mdDescribe").onclick = async () => {
  if (MD.run) { mdStopRun(MD.run); return; }
  const ids = mdTodo().map(i => i.id);
  if (!ids.length) return;
  if (!await confirmDialog({
    title: `Describe ${fmtNum(ids.length)} item${ids.length > 1 ? "s" : ""} with AI?`,
    text: `They go in the order shown (${$("#mdSort").selectedOptions[0].text.toLowerCase()}), ${MD_PARALLEL} at a time, to the media model from Settings: one request each, which costs tokens. Each description is saved as soon as it arrives, so you can stop at any time and continue later with the rest. Descriptions are used by all chats of this export and can be edited.`,
    ok: "Describe", icon: "sparkle",
  })) return;
  runDescribe(ids, false);
};
$("#mdGrid").addEventListener("click", e => {
  if (e.target.closest("[data-md-more]")) { MD.shown += MD_PAGE; renderMedia(); return; }
  const b = e.target.closest("[data-ai]"); if (b) runDescribe([b.dataset.ai], true);
});
// Cmd/Ctrl+Enter saves the description right away.
$("#mdGrid").addEventListener("keydown", e => { if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && e.target.matches(".md-desc")) { e.preventDefault(); e.target.blur(); } });
$("#mdGrid").addEventListener("change", async e => {
  const ta = e.target.closest(".md-desc"); if (!ta) return;
  const card = ta.closest(".md-card"), id = card.dataset.id, it = mdItem(id);
  try {
    const r = await api(`/api/exports/${MD.exportId}/library/${encodeURIComponent(id)}`, { method: "PATCH", body: { description: ta.value } });
    if (!it) return;
    it.description = r.description; it.saved = true;
    // Update the badge and state without re-rendering: the user may already be typing in the next card.
    card.classList.toggle("empty", !it.description);
    card.querySelector(".md-status").innerHTML = mdStatus(it);
    renderMediaHeader(); refreshChatLibrary();
    setTimeout(() => { it.saved = false; const st = $(`.md-card[data-id="${CSS.escape(id)}"] .md-status`); if (st) st.innerHTML = mdStatus(it); }, 1800);
  } catch (err) { toast(err.message, { error: true }); }
});
function mdStopAudio() {
  const a = MD.audio; MD.audio = null; a?.pause();
  $$(".md-play.on").forEach(b => { b.classList.remove("on"); b.innerHTML = icon("play"); b.setAttribute("aria-label", "Play voice message"); });
}
// Voice items play in place, one at a time.
$("#mdGrid").addEventListener("click", e => {
  const b = e.target.closest("[data-play]"); if (!b) return;
  const same = MD.audio?.dataset.src === b.dataset.play;
  mdStopAudio();
  if (same) return;
  const a = new Audio(b.dataset.play); a.dataset.src = b.dataset.play; MD.audio = a;
  a.onended = mdStopAudio;
  a.play().then(() => { b.classList.add("on"); b.innerHTML = icon("pause"); b.setAttribute("aria-label", "Pause voice message"); }).catch(() => { MD.audio = null; toast("This voice message can't be played here.", { error: true }); });
});

