"use strict";
// iRoof.top web client: no build step, no dependencies.

const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => [...el.querySelectorAll(s)];
const QUICK = ["❤", "👍", "😁", "😮", "😢", "🔥", "👏", "🎉", "🙏", "🤔"];
// Inline SVG icons live in the sprite at the top of index.html.
const icon = (name, cls = "") => `<svg class="i${cls ? " " + cls : ""}" aria-hidden="true" focusable="false"><use href="#i-${name}"></use></svg>`;
const RESPOND_DELAY = 1600; // wait for more bubbles before the persona answers

// Visitor mode: /s/<slug> (start page) or /s/<slug>/<token> (a conversation).
const GM = location.pathname.match(/^\/s\/([^/]+)(?:\/([^/]+))?\/?$/);
const GUEST = GM ? { slug: decodeURIComponent(GM[1]), token: GM[2] ? decodeURIComponent(GM[2]) : null, info: null } : null;
const enc = encodeURIComponent;
const guestBase = () => `/api/s/${enc(GUEST.slug)}`;
// API path of the open chat (owner: /api/chats/<id>, visitor: /api/s/<slug>/c/<token>).
const chatPath = (p = "", id = S.chatId) => (GUEST ? `${guestBase()}/c/${enc(GUEST.token)}` : `/api/chats/${id}`) + (p ? `/${p}` : "");

// Desktop app (Tauri) on macOS: the window has no title bar, and the traffic lights
// float over our headers, which act as the drag region.
const SHELL = window.__IROOF_SHELL__ || null;
if (SHELL?.titlebar === "overlay") document.documentElement.classList.add("mac-titlebar");
const tauriInvoke = (cmd, args) => window.__TAURI_INTERNALS__?.invoke(cmd, args)?.catch?.(() => { });

const PAGE = 200; // messages per page (older pages load as you scroll up)
const S = {
  state: null, chatId: null, chat: null, byId: new Map(),
  reply: null, pickerTab: "sticker", respondTimer: null, busy: false, limit: PAGE,
  rendered: [], loadingOlder: false,
};

// ---------- utils ----------
async function api(path, opts = {}) {
  const init = { ...opts, headers: { ...(opts.body && !(opts.body instanceof FormData) ? { "Content-Type": "application/json" } : {}), ...(opts.headers || {}) } };
  if (init.body && typeof init.body === "object" && !(init.body instanceof FormData)) init.body = JSON.stringify(init.body);
  const r = await fetch(path, init);
  const text = await r.text();
  let data; try { data = text ? JSON.parse(text) : {}; } catch { data = { error: text }; }
  if (!r.ok) throw new Error(data.error || `HTTP ${r.status}`);
  return data;
}
const esc = s => String(s ?? "").replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
function linkify(text) {
  return esc(text).replace(/\bhttps?:\/\/[^\s<>"']+/g, u => `<a href="${u}" target="_blank" rel="noopener noreferrer">${u}</a>`);
}
function mediaUrl(path, exportId = S.chat?.export.id) {
  if (!path) return "";
  if (/^https?:\/\//.test(path)) return path;
  if (GUEST) return `${chatPath("media")}/${path.split("/").map(encodeURIComponent).join("/")}`;
  return `/media/${encodeURIComponent(exportId)}/${path.split("/").map(encodeURIComponent).join("/")}`;
}
const COLORS = ["#e17076", "#7bc862", "#65aadd", "#a695e7", "#ee7aae", "#6ec9cb", "#faa774"];
function avatarHtml(name, avatar, cls = "", exportId, tag = "div") {
  const initials = (name || "?").split(/\s+/).filter(Boolean).slice(0, 2).map(w => [...w][0]).join("").toUpperCase();
  let h = 0; for (const c of name || "") h = (h * 31 + c.charCodeAt(0)) >>> 0;
  if (avatar) return `<${tag} class="avatar ${cls}" style="background-image:url('${esc(mediaUrl(avatar, exportId))}')"></${tag}>`;
  return `<${tag} class="avatar ${cls}" style="background:${COLORS[h % COLORS.length]}">${esc(initials)}</${tag}>`;
}
// A message avatar that opens the Info dialog of that person.
const profileAvatar = (name, avatar) => avatarHtml(name, avatar, "sm", undefined, "button")
  .replace('class="avatar ', `type="button" data-profile="${esc(name)}" title="${esc(name)}" aria-label="Info: ${esc(name)}" class="avatar `);
function fmtTime(d) { return d ? d.slice(11, 16) : ""; }
function fmtDay(d) {
  const dt = new Date(d.slice(0, 10) + "T00:00:00");
  const today = new Date(); today.setHours(0, 0, 0, 0);
  const diff = Math.round((today - dt) / 86400000);
  if (diff === 0) return "Today";
  if (diff === 1) return "Yesterday";
  return dt.toLocaleDateString(undefined, { day: "numeric", month: "long", year: dt.getFullYear() === today.getFullYear() ? undefined : "numeric" });
}
function fmtListTime(d) {
  if (!d) return "";
  const today = new Date().toISOString().slice(0, 10);
  if (d.slice(0, 10) === today) return d.slice(11, 16);
  return new Date(d.slice(0, 10)).toLocaleDateString(undefined, { day: "numeric", month: "short" });
}
let toastTimer;
function toast(msg, { error = false, action, actionLabel = "Retry", ms = 5000 } = {}) {
  const t = $("#toast");
  t.className = "toast" + (error ? " error" : "");
  t.innerHTML = `<span>${esc(msg)}</span>`;
  if (action) { const b = document.createElement("button"); b.textContent = actionLabel; b.onclick = () => { t.classList.add("hidden"); action(); }; t.append(b); }
  clearTimeout(toastTimer); toastTimer = setTimeout(() => t.classList.add("hidden"), action ? ms * 2 : ms);
}
const sleep = ms => new Promise(r => setTimeout(r, ms));

// Styled replacement for window.confirm(): resolves true/false.
// `remember` (a key) offers "Don't ask again" for frequent, non-destructive actions.
const skipKey = k => `iroof:noconfirm:${k}`;
function confirmDialog({ title, text = "", ok = "OK", danger = false, icon: ic = danger ? "alert" : null, remember = null }) {
  if (remember && localStorage.getItem(skipKey(remember)) === "1") return Promise.resolve(true);
  const dlg = $("#confirmDlg");
  $("#cfH").textContent = title; $("#cfText").textContent = text;
  $("#cfText").classList.toggle("hidden", !text);
  $("#cfOk").textContent = ok;
  $("#cfOk").className = `btn ${danger ? "danger-fill" : "primary"}`;
  $("#cfIcon").classList.toggle("hidden", !ic);
  $("#cfIcon").classList.toggle("accent", !danger);
  if (ic) $("#cfIcon").innerHTML = icon(ic);
  $("#cfRememberRow").classList.toggle("hidden", !remember);
  $("#cfRemember").checked = false;
  dlg.returnValue = "";
  hideTip();
  dlg.showModal();
  (danger ? $("#cfCancel") : $("#cfOk")).focus(); // destructive actions focus the safe choice
  return new Promise(res => dlg.addEventListener("close", () => {
    const yes = dlg.returnValue === "ok";
    if (yes && remember && $("#cfRemember").checked) localStorage.setItem(skipKey(remember), "1");
    res(yes);
  }, { once: true }));
}
// Light-dismiss for dialogs: a click on the backdrop closes them.
document.addEventListener("click", e => { if (e.target instanceof HTMLDialogElement && e.target.open) e.target.close("cancel"); });
// Info buttons show their help text in a floating tooltip: nothing in the layout moves.
// Click/tap pins it. On devices with a mouse, hovering previews it.
const TIP = { btn: null, pinned: false, timer: 0 };
function showTip(btn, pinned) {
  const src = document.getElementById(btn.getAttribute("aria-controls")); if (!src) return;
  const tip = $("#tip");
  if (TIP.btn && TIP.btn !== btn) resetTipBtn(TIP.btn);
  TIP.btn = btn; TIP.pinned = pinned;
  tip.innerHTML = src.innerHTML;
  // Inside a modal <dialog> only the top layer is visible: the tooltip must live in it.
  const host = btn.closest("dialog") || document.body;
  if (tip.parentElement !== host) host.append(tip);
  tip.classList.remove("hidden");
  btn.setAttribute("aria-expanded", "true"); btn.setAttribute("aria-describedby", "tip"); btn.classList.add("on");
  placeTip();
}
function placeTip() {
  const tip = $("#tip"), btn = TIP.btn; if (!btn || tip.classList.contains("hidden")) return;
  const r = btn.getBoundingClientRect(), m = 8;
  tip.style.left = "0px"; tip.style.top = "0px";
  const w = tip.offsetWidth, h = tip.offsetHeight;
  const left = Math.max(m, Math.min(innerWidth - w - m, r.left + r.width / 2 - w / 2));
  const below = r.bottom + 10 + h <= innerHeight - m || r.top - 10 - h < m;
  tip.style.left = `${left}px`;
  tip.style.top = `${below ? r.bottom + 10 : r.top - 10 - h}px`;
  tip.style.setProperty("--ax", `${Math.max(12, Math.min(w - 12, r.left + r.width / 2 - left))}px`);
  tip.classList.toggle("below", below); tip.classList.toggle("above", !below);
}
function resetTipBtn(b) { b.setAttribute("aria-expanded", "false"); b.removeAttribute("aria-describedby"); b.classList.remove("on"); }
function hideTip() {
  clearTimeout(TIP.timer);
  if (TIP.btn) resetTipBtn(TIP.btn);
  TIP.btn = null; TIP.pinned = false;
  $("#tip").classList.add("hidden");
}
document.addEventListener("click", e => {
  const b = e.target.closest(".info-btn");
  if (b) { e.preventDefault(); return TIP.btn === b && TIP.pinned ? hideTip() : showTip(b, true); }
  if (TIP.btn && !e.target.closest("#tip")) hideTip();
});
document.addEventListener("pointerover", e => {
  if (e.pointerType !== "mouse") return;
  const b = e.target.closest?.(".info-btn");
  if (b && !TIP.pinned) { clearTimeout(TIP.timer); TIP.timer = setTimeout(() => showTip(b, false), 250); }
});
document.addEventListener("pointerout", e => {
  if (e.pointerType !== "mouse" || !e.target.closest?.(".info-btn") || TIP.pinned) return;
  clearTimeout(TIP.timer); TIP.timer = setTimeout(() => { if (!TIP.pinned) hideTip(); }, 120);
});
document.addEventListener("keydown", e => { if (e.key === "Escape" && TIP.btn) { const pinned = TIP.btn; hideTip(); pinned.focus(); e.preventDefault(); e.stopPropagation(); } }, true);
addEventListener("resize", placeTip);
document.addEventListener("scroll", () => { if (TIP.btn) placeTip(); }, true);
document.addEventListener("close", hideTip, true); // a dialog closed
const isEmojiOnly = t => t && t.length <= 12 && /^(\p{Extended_Pictographic}|\p{Emoji_Component}|‍|️|\s)+$/u.test(t) && !/^[\d#*\s]+$/.test(t);

// ---------- theme ----------
const THEMES = ["auto", "light", "dark"];
function applyTheme() {
  const pref = localStorage.getItem("theme") || "auto";
  const dark = pref === "dark" || (pref === "auto" && matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
  // Native window chrome (traffic lights, resize shadows) follows the app's theme.
  if (SHELL) tauriInvoke("plugin:window|set_theme", { label: "main", value: pref === "auto" ? null : pref });
  const b = $("#themeBtn"); b.innerHTML = icon(pref === "auto" ? "auto" : dark ? "moon" : "sun"); b.title = `Theme: ${pref}`; b.setAttribute("aria-label", `Theme: ${pref}. Click to change`);
}
$("#themeBtn").onclick = () => {
  const cur = localStorage.getItem("theme") || "auto";
  localStorage.setItem("theme", THEMES[(THEMES.indexOf(cur) + 1) % 3]); applyTheme();
};
matchMedia("(prefers-color-scheme: dark)").addEventListener("change", applyTheme);
applyTheme();

// ---------- sidebar ----------
async function loadState() {
  if (GUEST) return null;
  S.state = await api("/api/state");
  renderSidebar();
  return S.state;
}
function exportById(id) { return S.state?.exports.find(e => e.id === id); }
function renderSidebar() {
  if (!S.state) return;
  renderSetupNote();
  const list = $("#chatList");
  const { chats, exports } = S.state;
  if (!chats.length) list.innerHTML = `<div class="empty-list">${exports.length ? "No chats yet. Start one with <b>New chat</b>." : "Import a Telegram or WhatsApp chat to begin."}</div>`;
  else list.innerHTML = chats.map(c => {
    const ex = exportById(c.export_id);
    const last = c.last ? `${c.last.from === c.user ? "You: " : ""}${c.last.preview}` : "";
    const shares = (S.state.shares || []).filter(x => x.chat_id === c.id && x.active);
    const visitors = shares.reduce((n, x) => n + x.sessions.length, 0);
    return `<a class="chat-item ${c.id === S.chatId ? "active" : ""}" href="#/chat/${c.id}" data-id="${c.id}">
      ${avatarHtml(c.persona, c.persona_avatar, "", c.export_id)}
      <div class="ci-main"><div class="ci-title">${esc(c.title)}${c.title !== c.persona ? "" : ""}<span class="ci-badge">${c.persona === ex?.exporter ? "as you" : "AI"}</span>${shares.length ? `<span class="ci-badge shared" title="Shared by link${visitors ? ` · ${visitors} visitor conversation${visitors > 1 ? "s" : ""}` : ""}">${icon("link")}${visitors || ""}</span>` : ""}</div>
      <div class="ci-sub">${esc(last)}</div></div>
      <div class="ci-time">${fmtListTime(c.updated_at)}</div></a>`;
  }).join("");
  $("#exportList").innerHTML = exports.length ? exports.map(e => `<div class="export-row">
      <div class="grow"><div class="t">${esc(e.title)}</div><div class="muted small">${e.messages} messages · ${e.library} media${e.described ? ` · ${e.described} described` : ""}</div></div>
      <button class="btn" data-media="${e.id}" title="Stickers, GIFs and photos, and what the AI knows about them" aria-label="Media library">${icon("image")}<span>Media</span></button>
      <button class="btn danger" data-del-export="${e.id}" title="Delete export and its chats" aria-label="Delete export and its chats">${icon("trash")}</button></div>`).join("")
    : `<div class="muted small">None yet.</div>`;
  $("#exportCount").textContent = exports.length || "";
}
$("#exportList").addEventListener("click", async ev => {
  const d = ev.target.closest("[data-media]"), x = ev.target.closest("[data-del-export]");
  if (d) openMedia(d.dataset.media);
  else if (x) {
    const e = exportById(x.dataset.delExport);
    const n = S.state.chats.filter(c => c.export_id === e.id).length;
    if (!await confirmDialog({ title: `Delete «${e.title}»?`, text: `The imported export, its media${n ? ` and ${n} chat${n > 1 ? "s" : ""} based on it` : ""} will be removed from this server. This cannot be undone.`, ok: "Delete", danger: true })) return;
    try { await api(`/api/exports/${e.id}`, { method: "DELETE" }); if (S.chat?.export.id === e.id) location.hash = ""; await loadState(); route(); }
    catch (err) { toast(err.message, { error: true }); }
  }
});

// ---------- import ----------
function setProgress(frac, indeterminate = false) {
  for (const p of [$("#uploadProgress"), $("#uploadProgress2")]) {
    p.classList.toggle("hidden", frac == null);
    p.classList.toggle("indeterminate", indeterminate);
    p.firstElementChild.style.width = indeterminate ? "" : `${Math.round((frac || 0) * 100)}%`;
  }
}
const SKIP = /(^|\/)(\.DS_Store|__MACOSX|Thumbs\.db)(\/|$)|(^|\/)(css|js|images)\//;
function upload(files) {
  files = files.filter(f => !SKIP.test(f.path));
  if (!files.length) return toast("Nothing to upload", { error: true });
  const fd = new FormData();
  for (const f of files) fd.append("files", f.file, f.path);
  return new Promise(resolve => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", "/api/exports");
    xhr.upload.onprogress = e => { if (e.lengthComputable) setProgress(e.loaded / e.total, e.loaded === e.total); };
    xhr.upload.onload = () => setProgress(1, true);
    xhr.onerror = () => { setProgress(null); toast("Upload failed", { error: true }); resolve(); };
    xhr.onload = async () => {
      setProgress(null);
      let data = {}; try { data = JSON.parse(xhr.responseText); } catch { }
      if (xhr.status >= 300) { toast(data.error || `Upload failed (${xhr.status})`, { error: true }); return resolve(); }
      $("#importDlg").close();
      await loadState();
      toast(data.created ? `Imported «${data.export.title}»: ${data.export.messages} messages` : `«${data.export.title}» was already imported`);
      openNewChat(data.export.id);
      resolve();
    };
    setProgress(0);
    xhr.send(fd);
  });
}
async function entriesToFiles(items) {
  const out = [];
  async function walk(entry, prefix) {
    if (entry.isFile) {
      const file = await new Promise((res, rej) => entry.file(res, rej));
      out.push({ file, path: prefix + entry.name });
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
    const items = [...e.dataTransfer.items].filter(i => i.kind === "file");
    let files = await entriesToFiles(items);
    if (!files.length) files = [...e.dataTransfer.files].map(f => ({ file: f, path: f.name }));
    upload(files);
  });
}
bindDropzone($("#dropzone")); bindDropzone($("#dropzone2"));
$("#pickFolder").onclick = () => $("#folderInput").click();
$("#pickZip").onclick = () => $("#zipInput").click();
$$("[data-pick]").forEach(b => b.onclick = () => (b.dataset.pick === "zip" ? $("#zipInput") : $("#folderInput")).click());
$("#folderInput").onchange = e => { upload([...e.target.files].map(f => ({ file: f, path: f.webkitRelativePath || f.name }))); e.target.value = ""; };
$("#zipInput").onchange = e => { upload([...e.target.files].map(f => ({ file: f, path: f.name }))); e.target.value = ""; };
// The import dialog shows the same walkthrough as the welcome screen: a copy of that SVG
// with its own ids (gradient and clip ids must stay unique in the document).
function openImport() {
  const box = $("#imTut");
  if (!box.firstElementChild) box.innerHTML = $("#tutorial").outerHTML.replace(/(id="|url\(#|href="#)(tut-[\w-]+|tutorial)/g, "$1$2-im").replace('aria-labelledby="tut-title-im"', "");
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

// ---------- new chat ----------
function openNewChat(exportId) {
  const { exports } = S.state;
  if (!exports.length) { openImport(); return; }
  const sel = $("#ncExport");
  sel.innerHTML = exports.map(e => `<option value="${e.id}">${esc(e.title)} — ${e.messages} messages</option>`).join("");
  if (exportId) sel.value = exportId;
  const renderPersonas = () => {
    const ex = exportById(sel.value);
    const people = ex.participants;
    const def = people.find(p => p.name === ex.title) || people.find(p => p.name !== ex.exporter) || people[0];
    $("#ncPersona").innerHTML = people.map(p => {
      const me = p.name === ex.exporter;
      const other = people.find(x => x.name !== p.name)?.name || "the others";
      return `<label class="persona"><input type="radio" name="persona" value="${esc(p.name)}" ${p === def ? "checked" : ""}>
        ${avatarHtml(p.name, p.avatar, "sm", ex.id)}
        <div><div class="pt">${esc(p.name)}${me ? " (probably you)" : ""}</div>
        <div class="ps">${me ? `Chat with yourself: the AI writes like you, you play ${esc(other)}` : `Talk to ${esc(p.name)}: the AI writes like them`} · ${p.messages} messages</div></div></label>`;
    }).join("");
  };
  sel.onchange = renderPersonas; renderPersonas();
  $("#ncTitle").value = ""; $("#ncNotes").value = "";
  $("#newChatDlg").showModal();
}
$("#newChatBtn").onclick = () => openNewChat();
$("#newChatForm").addEventListener("submit", async e => {
  if (e.submitter?.value !== "ok") return;
  e.preventDefault();
  const persona = $("input[name=persona]:checked")?.value;
  if (!persona) return;
  try {
    const c = await api("/api/chats", { method: "POST", body: { export_id: $("#ncExport").value, persona, title: $("#ncTitle").value, notes: $("#ncNotes").value } });
    $("#newChatDlg").close();
    await loadState();
    location.hash = `#/chat/${c.id}`;
  } catch (err) { toast(err.message, { error: true }); }
});

// ---------- chat rendering ----------
function previewOf(m) {
  if (!m) return "Message";
  if (m.text) return m.text.replace(/\s+/g, " ").slice(0, 90);
  const k = m.media?.kind;
  return { sticker: "Sticker", gif: "GIF", photo: "Photo", video: "Video", round_video: "Video message", voice: "Voice message", audio: m.media?.title || "Audio", file: m.media?.title || "File" }[k] || "Message";
}
function mediaHtml(m) {
  const md = m.media; if (!md) return "";
  const src = mediaUrl(md.file || md.url);
  const thumb = mediaUrl(md.thumb);
  const unavailable = md.available === false || (!md.file && !md.url);
  const alt = esc(md.description || md.kind);
  const isVid = p => /\.(mp4|webm|mov|m4v)(\?|$)/i.test(p || "");
  switch (md.kind) {
    case "sticker": {
      if (unavailable) return `<div class="sticker placeholder">${icon("sticker")}</div>`;
      if (isVid(md.file)) return `<div class="sticker"><video src="${src}" autoplay loop muted playsinline poster="${thumb}" title="${alt}"></video></div>`;
      if (/\.tgs$/i.test(md.file)) return thumb ? `<div class="sticker"><img src="${thumb}" alt="${alt}" title="${alt}"></div>` : `<div class="sticker placeholder">${icon("sticker")}</div>`;
      return `<div class="sticker"><img src="${src}" alt="${alt}" title="${alt}"></div>`;
    }
    case "gif":
      if (unavailable) return `<div class="unavail">GIF unavailable</div>`;
      return `<div class="media gif">${isVid(md.file || md.url) ? `<video src="${src}" autoplay loop muted playsinline title="${alt}"></video>` : `<img src="${src}" alt="${alt}" referrerpolicy="no-referrer">`}</div>`;
    case "photo":
      if (unavailable) return `<div class="unavail">${icon("image")}Photo not included in export</div>`;
      return `<a class="media zoomable" href="${src}" target="_blank" rel="noopener" data-lb="${m.id}" aria-label="Open photo"><img src="${src}" alt="${alt}" loading="lazy"></a>`;
    case "video":
      if (unavailable) return `<div class="unavail">${icon("film")}Video not included in export ${esc(md.status || "")}</div>`;
      return `<div class="media"><video src="${src}" controls preload="metadata" playsinline ${thumb ? `poster="${thumb}"` : ""}></video></div>`;
    case "round_video":
      if (unavailable) return `<div class="unavail">${icon("record")}Video message ${esc(md.status || "")}</div>`;
      return `<div class="round"><video src="${src}" preload="metadata" playsinline ${thumb ? `poster="${thumb}"` : ""} onclick="this.paused?this.play():this.pause()"></video></div>`;
    case "voice": case "audio":
      return `${md.kind === "audio" && md.title ? `<div class="doc"><div class="ic">${icon("music")}</div><div><div class="dt">${esc(md.title)}</div><div class="ds">${esc(md.status || "")}</div></div></div>` : ""}
        ${unavailable ? `<div class="unavail">${icon("mic")}Voice message ${esc(md.status || "")}</div>` : `<audio src="${src}" controls preload="${md.kind === "voice" ? "metadata" : "none"}"></audio>`}
        ${md.kind === "voice" && md.description ? `<details class="transcript"><summary>Transcript</summary><div>${esc(md.description)}</div></details>` : ""}`;
    case "file":
      return `<a class="doc" ${unavailable ? "" : `href="${src}" download`}><div class="ic">${icon("file")}</div><div><div class="dt">${esc(md.title || (md.file || "").split("/").pop())}</div><div class="ds">${esc(md.status || "")}</div></div></a>`;
    default:
      return `<div class="unavail">${esc(md.status || md.title || md.kind)}</div>`;
  }
}
function bubbleHtml(m, prev, next) {
  if (m.service) return `<div class="service" id="m-${m.id}">${esc(m.text)}</div>`;
  const mine = m.mine;
  const startGroup = !prev || prev.service || prev.from !== m.from || dayOf(prev) !== dayOf(m);
  const endGroup = !next || next.service || next.from !== m.from || dayOf(next) !== dayOf(m);
  const k = m.media?.kind;
  const bare = (k === "sticker" || k === "round_video") && !m.text;
  const mediaOnly = !bare && m.media && ["photo", "video", "gif"].includes(k) && !m.text && !m.forwarded_from;
  const jumbo = !m.media && isEmojiOnly(m.text);
  const multi = S.chat?.export.multi ?? (S.chat?.export.participants.length || 0) > 2;
  let inner = "";
  if (!mine && multi && startGroup && !bare) inner += `<div class="from">${esc(m.from)}</div>`;
  if (m.forwarded_from != null) inner += `<div class="fwd">Forwarded from <b>${esc(m.forwarded_from || "unknown")}</b></div>`;
  if (m.reply_to != null) {
    const t = S.byId.get(m.reply_to);
    inner += `<div class="reply" data-jump="${m.reply_to}"><div class="rt">${esc(t ? (t.mine ? "You" : t.from) : "Message")}</div>
      <div class="rx">${m.quote ? `<span class="rq">“${esc(m.quote)}”</span>` : esc(previewOf(t))}</div></div>`;
  }
  inner += mediaHtml(m);
  if (m.text) inner += `<div class="text${jumbo ? " jumbo" : ""}">${linkify(m.text)}</div>`;
  if (m.link_preview) {
    const lp = m.link_preview;
    inner += `<a class="lp" href="${esc(lp.url)}" target="_blank" rel="noopener noreferrer">${lp.site ? `<div class="s">${esc(lp.site)}</div>` : ""}${lp.title ? `<div class="t">${esc(lp.title)}</div>` : ""}${lp.description ? `<div class="d">${esc(lp.description)}</div>` : ""}${lp.photo ? `<img src="${mediaUrl(lp.photo)}" alt="" loading="lazy">` : ""}</a>`;
  }
  inner += `<span class="meta">${m.edited ? "edited " : ""}${fmtTime(m.date)}</span>`;
  if (m.reactions?.length) inner += `<div class="reactions">${m.reactions.map(r => `<button class="reaction ${r.mine ? "mine" : ""}" data-react="${esc(r.emoji)}" data-id="${m.id}">${esc(r.emoji)}${r.count > 1 ? `<span class="c">${r.count}</span>` : ""}</button>`).join("")}</div>`;
  const person = S.chat.export.participants.find(p => p.name === m.from);
  return `<div class="row ${mine ? "mine" : ""} ${startGroup ? "group-start" : ""} ${endGroup ? "group-end" : ""}" data-row="${m.id}">
    ${mine ? "" : profileAvatar(m.from, person?.avatar)}
    <div class="bubble ${bare ? "bare" : ""} ${mediaOnly ? "media-only" : ""}" id="m-${m.id}">${inner}</div>
    <div class="tools"><button data-act="reply" data-id="${m.id}" title="Reply (select text first to quote)" aria-label="Reply">${icon("reply")}</button><button data-act="react" data-id="${m.id}" title="React" aria-label="React">${icon("smile")}</button></div>
  </div>`;
}
const dayOf = m => (m.date || "").slice(0, 10);
// Keep the view pinned to the newest message while media loads, unless the user scrolled up.
S.pinned = true;
$("#messages").addEventListener("scroll", () => {
  const b = $("#messages"); S.pinned = b.scrollHeight - b.scrollTop - b.clientHeight < 120;
  $("#toBottom").classList.toggle("hidden", S.pinned || b.scrollHeight - b.clientHeight < 400);
  if (b.scrollTop < 600 && S.chat?.has_more && !S.loadingOlder) loadEarlier();
}, { passive: true });
$("#toBottom").onclick = () => { const b = $("#messages"); b.scrollTo({ top: b.scrollHeight, behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" }); };
$("#messages").addEventListener("load", () => { if (S.pinned) { const b = $("#messages"); b.scrollTop = b.scrollHeight; } }, true);
$("#messages").addEventListener("loadedmetadata", () => { if (S.pinned) { const b = $("#messages"); b.scrollTop = b.scrollHeight; } }, true);
// Rendering is keyed and incremental: only the rows whose HTML changed are replaced,
// so new bubbles, reactions and older pages don't rebuild (and restart) the whole history.
function messageItems() {
  const msgs = S.chat.messages, items = [];
  if (S.chat.has_more) items.push(["more", `<div class="load-more" id="loadMore">${S.loadingOlder ? `<span class="spin" aria-hidden="true"></span><span>Loading earlier messages…</span>` : `<button class="btn sm" data-load-more>Show earlier messages</button>`}</div>`]);
  if (!msgs.length) items.push(["empty", GUEST
    ? `<div class="empty-chat">${avatarHtml(S.chat.chat.persona, S.chat.chat.persona_avatar, "")}<b>${esc(S.chat.chat.persona)}</b>${S.chat.share?.greeting ? `<span>${esc(S.chat.share.greeting)}</span>` : ""}<span>Say hello, or tap the speech bubble above to let ${esc(S.chat.chat.persona)} write first.</span></div>`
    : `<div class="empty-chat">${icon("chat")}<b>No messages yet</b><span>Say hello, or use the speech bubble above to let ${esc(S.chat.chat.persona)} start.</span></div>`]);
  let lastDay = null;
  msgs.forEach((m, i) => {
    const d = dayOf(m);
    if (d && d !== lastDay) { items.push([`d${d}`, `<div class="day">${esc(fmtDay(m.date))}</div>`]); lastDay = d; }
    items.push([`m${m.id}`, bubbleHtml(m, msgs[i - 1], msgs[i + 1])]);
  });
  return items;
}
function renderMessages({ keepScroll = false } = {}) {
  const box = $("#messages");
  const atBottom = S.pinned;
  $("#typingRow")?.remove();
  const next = messageItems(), prev = S.rendered;
  const same = (a, b) => a[0] === b[0] && a[1] === b[1];
  if (S.renderedFor !== S.chatId || box.children.length !== prev.length) {
    box.innerHTML = next.map(x => x[1]).join("");
  } else {
    let p = 0; while (p < prev.length && p < next.length && same(prev[p], next[p])) p++;
    let q = 0; while (q < prev.length - p && q < next.length - p && same(prev[prev.length - 1 - q], next[next.length - 1 - q])) q++;
    const kids = [...box.children];
    for (const el of kids.slice(p, kids.length - q)) el.remove();
    const mid = next.slice(p, next.length - q);
    if (mid.length) {
      const t = document.createElement("template"); t.innerHTML = mid.map(x => x[1]).join("");
      box.insertBefore(t.content, kids[kids.length - q] || null);
    }
  }
  S.rendered = next; S.renderedFor = S.chatId;
  if (!keepScroll || atBottom) { box.scrollTop = box.scrollHeight; S.pinned = true; }
}
// Older history comes in pages, and the viewport stays where it was.
async function loadEarlier() {
  if (S.loadingOlder || !S.chat?.has_more || !S.chat.messages.length) return false;
  const chatId = S.chatId, first = S.chat.messages[0].id;
  S.loadingOlder = true; renderMessages({ keepScroll: true });
  try {
    const r = await api(`${chatPath()}?before=${enc(first)}&limit=${PAGE}`);
    if (S.chatId !== chatId) return false;
    const known = new Set(S.chat.messages.map(m => m.id));
    const older = r.messages.filter(m => !known.has(m.id));
    S.chat.messages = [...older, ...S.chat.messages];
    for (const m of older) S.byId.set(m.id, m);
    S.chat.total = r.total ?? S.chat.total;
    S.chat.has_more = !!r.has_more && older.length > 0;
    S.limit = Math.min(2000, S.chat.messages.length);
    return older.length > 0;
  } catch (e) { toast(e.message, { error: true }); return false; }
  finally {
    S.loadingOlder = false;
    if (S.chatId === chatId) {
      const box = $("#messages"), h = box.scrollHeight, t = box.scrollTop;
      renderMessages({ keepScroll: true });
      box.scrollTop = t + (box.scrollHeight - h);
    }
  }
}
// Scroll to a message, fetching older pages if it isn't loaded yet.
async function jumpTo(id) {
  for (let i = 0; i < 50 && !$(`#m-${id}`) && S.chat?.has_more; i++) if (!await loadEarlier()) break;
  const el = $(`#m-${id}`);
  if (!el) return toast("That message isn't available any more.");
  el.scrollIntoView({ block: "center", behavior: "smooth" }); el.classList.remove("flash"); void el.offsetWidth; el.classList.add("flash");
}
function upsertMessages(list) {
  for (const v of list) {
    const i = S.chat.messages.findIndex(m => m.id === v.id);
    if (i >= 0) S.chat.messages[i] = v; else { S.chat.messages.push(v); S.chat.total++; }
    S.byId.set(v.id, v);
  }
}
function setTyping(on) {
  const sub = $("#chatSub");
  if (on) { sub.textContent = "typing…"; sub.classList.add("typing"); }
  else { sub.classList.remove("typing"); sub.textContent = subtitle(); }
  $("#typingRow")?.remove();
  if (on) {
    const box = $("#messages");
    const p = S.chat.export.participants.find(p => p.name === S.chat.chat.persona);
    box.insertAdjacentHTML("beforeend", `<div class="row typing-row group-start group-end" id="typingRow">${profileAvatar(S.chat.chat.persona, p?.avatar)}<div class="bubble" role="status" aria-label="${esc(S.chat.chat.persona)} is typing"><span class="dots"><i></i><i></i><i></i></span></div></div>`);
    box.scrollTop = box.scrollHeight;
  }
}
function subtitle() {
  const c = S.chat.chat;
  const ex = S.chat.export;
  if (GUEST) {
    const left = S.chat.share?.replies_left;
    const base = S.chat.share?.mode === "room" ? "shared room" : "online";
    return left != null && left <= 20 ? `${base} · ${left} ${left === 1 ? "answer" : "answers"} left` : base;
  }
  if (c.guest) {
    const sh = S.state?.shares?.find(x => x.id === c.guest.share_id);
    const who = sh?.sessions.filter(x => x.chat_id === c.id).map(x => x.name || "visitor").join(", ");
    return `Visitor conversation${who ? ` · ${who}` : ""}${sh ? ` · /s/${sh.slug}` : ""}`;
  }
  return c.persona === ex.exporter ? `AI writes as you (${c.persona}) · you play ${c.user}` : `AI in the style of ${c.persona} · you are ${c.user}`;
}

async function openChat(id, { keepScroll = false } = {}) {
  try {
    const data = await api(`${chatPath("", id)}?limit=${S.limit}`);
    if (S.chatId !== id) { S.reply = null; renderReply(); $("#input").value = ""; autosize(); S.renderedFor = null; }
    data.has_more ??= data.total > data.messages.length; // older servers don't send has_more
    S.chatId = id; S.chat = data;
    S.byId = new Map(data.messages.map(m => [m.id, m]));
    $("#welcome").classList.add("hidden"); $("#guestGate").classList.add("hidden"); $("#chatView").classList.remove("hidden"); $("#app").classList.add("in-chat");
    const p = data.export.participants.find(p => p.name === data.chat.persona);
    $("#chatAvatar").outerHTML = avatarHtml(data.chat.persona, p?.avatar || data.chat.persona_avatar, "", undefined, "span").replace('class="avatar ', 'id="chatAvatar" class="avatar ');
    $("#peerBtn").setAttribute("aria-label", `${data.chat.title}: profile`);
    $("#shareBtn").classList.toggle("hidden", !!(GUEST || data.chat.guest));
    for (const b of ["#nudgeBtn", "#regenBtn"]) $(b).classList.toggle("hidden", !!(GUEST && data.share?.relay));
    if (GUEST) document.title = `${data.chat.title} · iRoof.top`;
    $("#chatName").textContent = data.chat.title;
    $("#chatSub").textContent = subtitle();
    renderMessages({ keepScroll });
    renderSidebar();
    if (matchMedia("(pointer: fine)").matches) $("#input").focus();
    return true;
  } catch (e) {
    if (GUEST) { forgetGuestToken(); showGate(e.message); return false; }
    toast(e.message, { error: true }); location.hash = "";
    return false;
  }
}
function showWelcome() {
  S.chatId = null; S.chat = null;
  $("#welcome").classList.remove("hidden"); $("#chatView").classList.add("hidden"); $("#app").classList.remove("in-chat");
  renderSidebar();
}
function route() {
  if (GUEST) return;
  const m = location.hash.match(/^#\/chat\/(\w+)/);
  if (m) { S.limit = PAGE; openChat(m[1]); } else showWelcome();
}
window.addEventListener("hashchange", route);
$("#backBtn").onclick = () => { location.hash = ""; };

// ---------- interactions ----------
function renderReply() {
  const bar = $("#replyBar");
  if (!S.reply) return bar.classList.add("hidden");
  const t = S.byId.get(S.reply.id);
  $("#replyTitle").textContent = `Reply to ${t?.mine ? "yourself" : t?.from || ""}`;
  $("#replyText").textContent = S.reply.quote ? `“${S.reply.quote}”` : previewOf(t);
  bar.classList.remove("hidden");
}
$("#replyCancel").onclick = () => { S.reply = null; renderReply(); };

function showReactPop(anchor, id) {
  const pop = $("#reactPop");
  pop.innerHTML = QUICK.map(e => `<button data-emoji="${e}">${e}</button>`).join("");
  pop.classList.remove("hidden");
  const r = anchor.getBoundingClientRect();
  const w = pop.offsetWidth;
  pop.style.left = Math.max(8, Math.min(innerWidth - w - 8, r.left + r.width / 2 - w / 2)) + "px";
  pop.style.top = Math.max(8, r.top - 52) + "px";
  pop.onclick = e => { const b = e.target.closest("[data-emoji]"); if (b) { pop.classList.add("hidden"); react(id, b.dataset.emoji); } };
}
document.addEventListener("click", e => { if (!e.target.closest("#reactPop") && !e.target.closest("[data-act=react]")) $("#reactPop").classList.add("hidden"); });

async function react(id, emoji) {
  const m = S.byId.get(id);
  const already = m?.reactions?.find(r => r.mine);
  const value = already && already.emoji === emoji ? "" : emoji;
  try {
    await api(chatPath("react"), { method: "POST", body: { message_id: id, emoji: value } });
    // Optimistic local update: one reaction per person.
    if (m) {
      m.reactions = (m.reactions || []).map(r => r.mine ? { ...r, count: r.count - 1, mine: false } : r).filter(r => r.count > 0);
      if (value) {
        const ex = m.reactions.find(r => r.emoji === value);
        if (ex) { ex.count++; ex.mine = true; } else m.reactions.push({ emoji: value, count: 1, mine: true });
      }
      renderMessages({ keepScroll: true });
      $(`#m-${id} .reaction.mine`)?.classList.add("pop");
    }
    if (value && !m?.mine) scheduleRespond();
  } catch (e) { toast(e.message, { error: true }); }
}

$("#messages").addEventListener("click", e => {
  const lb = e.target.closest("[data-lb]");
  if (lb && !e.metaKey && !e.ctrlKey && !e.shiftKey && e.button === 0) { e.preventDefault(); openLightbox(Number(lb.dataset.lb)); return; }
  if (e.target.closest("[data-load-more]")) { loadEarlier(); return; }
  const who = e.target.closest("[data-profile]");
  if (who) { openProfile(who.dataset.profile); return; }
  const jump = e.target.closest("[data-jump]");
  if (jump) { jumpTo(Number(jump.dataset.jump)); return; }
  const rb = e.target.closest("[data-react]");
  if (rb) return react(Number(rb.dataset.id), rb.dataset.react);
  const act = e.target.closest("[data-act]");
  if (act) {
    const id = Number(act.dataset.id);
    if (act.dataset.act === "reply") {
      const sel = getSelection();
      let quote = null;
      const bubble = $(`#m-${id}`);
      if (sel && !sel.isCollapsed && bubble?.contains(sel.anchorNode)) quote = sel.toString().trim().slice(0, 300) || null;
      S.reply = { id, quote }; renderReply(); $("#input").focus();
    } else showReactPop(act, id);
    return;
  }
  // Touch devices: tap a bubble to reveal its tools.
  const row = e.target.closest(".row");
  if (row && !matchMedia("(hover: hover)").matches) { $$(".row.touched").forEach(r => r !== row && r.classList.remove("touched")); row.classList.toggle("touched"); }
});
$("#messages").addEventListener("dblclick", e => {
  const row = e.target.closest("[data-row]");
  if (row && !e.target.closest("a,video,audio,button")) { getSelection()?.removeAllRanges(); react(Number(row.dataset.row), "❤"); }
});

const input = $("#input");
const CAN_RECORD = !!(navigator.mediaDevices?.getUserMedia && window.MediaRecorder && (window.AudioContext || window.webkitAudioContext));
function autosize() {
  input.style.height = "auto"; input.style.height = Math.min(180, input.scrollHeight) + "px";
  const empty = !input.value.trim();
  $("#sendBtn").disabled = empty;
  // Telegram-style: an empty composer offers the microphone instead of Send.
  $("#sendBtn").classList.toggle("hidden", empty && CAN_RECORD);
  $("#micBtn").classList.toggle("hidden", !(empty && CAN_RECORD));
}
input.addEventListener("input", () => { autosize(); if (S.respondTimer) scheduleRespond(); });
input.addEventListener("keydown", e => {
  if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); send(); }
  if (e.key === "Escape") { S.reply = null; renderReply(); $("#picker").classList.add("hidden"); }
});
$("#sendBtn").onclick = () => send();

async function send(extra = {}) {
  const text = input.value.trim();
  if (!text && !extra.sticker_id && !extra.gif_id) return;
  const body = { text, ...extra };
  if (S.reply) { body.reply_to = S.reply.id; if (S.reply.quote) body.quote = S.reply.quote; }
  input.value = ""; autosize();
  S.reply = null; renderReply();
  try {
    const r = await api(chatPath("messages"), { method: "POST", body });
    upsertMessages(r.messages);
    renderMessages();
    scheduleRespond();
  } catch (e) { input.value = text; autosize(); toast(e.message, { error: true }); }
}

function scheduleRespond(delay = RESPOND_DELAY) {
  // Relayed links: the owner (or the AI after a countdown) answers from Telegram, so just watch for it.
  if (GUEST && S.chat?.share?.relay) { setTimeout(pollGuest, 600); return; }
  clearTimeout(S.respondTimer);
  // Hold back while the user is still typing.
  S.respondTimer = setTimeout(() => {
    if (input.value.trim()) return scheduleRespond(delay);
    S.respondTimer = null; respond();
  }, delay);
}

async function respond(path = "respond") {
  if (S.busy) { S.pending = true; return; }
  S.busy = true;
  const chatId = S.chatId;
  setTyping(true);
  try {
    const r = await api(chatPath(path, chatId), { method: "POST" });
    if (S.chatId !== chatId) return;
    if (path === "regenerate") { await openChat(chatId, { keepScroll: true }); setTyping(true); }
    const views = new Map([...r.messages, ...r.updated].map(v => [v.id, v]));
    for (const a of r.actions) {
      if (a.type === "message") {
        const v = views.get(a.id); if (!v) continue;
        const len = (v.text || "").length;
        await sleep(v.media ? 700 : Math.min(2600, 450 + len * 38));
        if (S.chatId !== chatId) return;
        setTyping(false); upsertMessages([v]); renderMessages(); setTyping(true);
      } else {
        await sleep(450);
        const v = views.get(a.target); if (v) { upsertMessages([v]); renderMessages({ keepScroll: true }); setTyping(true); $(`#m-${a.target} .reaction:not(.mine)`)?.classList.add("pop"); }
      }
    }
    if (GUEST && S.chat.share && S.chat.share.replies_left != null) { S.chat.share.replies_left = Math.max(0, S.chat.share.replies_left - 1); }
    if (r.error) errorToast(r.error, () => respond());
    else if (!r.actions.length) toast(`${S.chat.chat.persona} didn't answer this time.`, { action: () => respond(), actionLabel: "Nudge" });
  } catch (e) {
    errorToast(e.message, () => respond(path));
  } finally {
    S.busy = false;
    if (S.chatId === chatId) { setTyping(false); loadState().catch(() => { }); }
    if (S.pending) { S.pending = false; respond(); }
  }
}
// Mobile overflow menu for the secondary chat actions (on desktop the menu is inline).
const moreMenu = $("#moreMenu"), moreBtn = $("#moreBtn");
const closeMore = () => { moreMenu.classList.remove("open"); moreBtn.setAttribute("aria-expanded", "false"); };
moreBtn.onclick = (e) => {
  e.stopPropagation();
  const open = moreMenu.classList.toggle("open");
  moreBtn.setAttribute("aria-expanded", String(open));
  if (open) moreMenu.querySelector("button")?.focus();
};
moreMenu.addEventListener("click", closeMore);
document.addEventListener("click", (e) => { if (!e.target.closest("#moreMenu")) closeMore(); });
document.addEventListener("keydown", (e) => { if (e.key === "Escape" && moreMenu.classList.contains("open")) { closeMore(); moreBtn.focus(); } });
// Actions that call the model get a short confirmation that says exactly what will happen.
const personaName = () => S.chat?.chat.persona || "They";
function answersLeftNote() {
  const left = S.chat?.share?.replies_left;
  return GUEST && left != null ? ` This uses one of the ${left} answer${left === 1 ? "" : "s"} left on this link.` : "";
}
$("#nudgeBtn").onclick = async () => {
  if (S.busy) return toast(`${personaName()} is already writing…`);
  if (!await confirmDialog({
    title: `Let ${personaName()} write first?`,
    text: `${personaName()} will send a new message on their own, without a new message from you: they may continue the topic, ask something or start a new one.${GUEST ? answersLeftNote() : " The model is called once with the current chat."}`,
    ok: "Let them write", icon: "nudge", remember: "nudge",
  })) return;
  respond();
};
$("#regenBtn").onclick = async () => {
  if (S.busy) return toast(`${personaName()} is already writing…`);
  const msgs = S.chat?.messages || [];
  const last = [...msgs].reverse().find(m => !m.service);
  if (!last?.live || !last.persona) return toast(`There is no new answer from ${personaName()} to rewrite yet.`);
  if (!await confirmDialog({
    title: "Rewrite the last answer?",
    text: `${personaName()}'s last answer (every bubble and reaction since your last message) will be deleted and written again. Your own messages stay as they are.`,
    ok: "Rewrite", icon: "refresh", remember: "regenerate",
  })) return;
  respond("regenerate");
};
$("#deleteChatBtn").onclick = async () => {
  if (!await confirmDialog({ title: `Delete chat «${S.chat.chat.title}»?`, text: "Your messages and the AI's replies in this chat will be deleted. The imported export stays, so you can start a new chat from it.", ok: "Delete chat", danger: true })) return;
  try { await api(`/api/chats/${S.chatId}`, { method: "DELETE" }); location.hash = ""; await loadState(); toast("Chat deleted"); }
  catch (e) { toast(e.message, { error: true }); }
};
$("#mediaBtn").onclick = () => openMedia(S.chat.export.id);
$("#notesBtn").onclick = () => {
  $("#nTitle").value = S.chat.chat.title; $("#nNotes").value = S.chat.notes || ""; $("#nVoice").value = S.chat.voice || "";
  $("#notesDlg").showModal();
};
$("#notesDlg form").addEventListener("submit", async e => {
  if (e.submitter?.value !== "ok") return;
  try {
    await api(`/api/chats/${S.chatId}`, { method: "PATCH", body: { title: $("#nTitle").value, notes: $("#nNotes").value, voice: $("#nVoice").value } });
    await loadState(); await openChat(S.chatId, { keepScroll: true });
  } catch (err) { toast(err.message, { error: true }); }
});
let lastPrompt = null;
$("#promptBtn").onclick = async () => {
  try {
    lastPrompt = await api(`/api/chats/${S.chatId}/prompt`);
    $("#promptInfo").textContent = `${lastPrompt.messages.length} messages, ${lastPrompt.tools.length} tools, model ${lastPrompt.model}${lastPrompt.dropped_turns ? ` · ${lastPrompt.dropped_turns} of ${lastPrompt.total_turns} oldest turns trimmed by the history budget` : ""}`;
    // Big prompts are cheap to copy or download but slow to lay out: show only the beginning.
    const json = JSON.stringify({ model: lastPrompt.model, messages: lastPrompt.messages, tools: lastPrompt.tools }, null, 2);
    lastPrompt.json = json;
    const MAX = 200_000;
    $("#promptPre").textContent = json.length > MAX ? `${json.slice(0, MAX)}\n\n… ${Math.round((json.length - MAX) / 1024)} KB more. Use Copy or Download for the full JSON.` : json;
    $("#promptDlg").showModal();
  } catch (e) { toast(e.message, { error: true }); }
};
$("#promptCopy").onclick = async () => toast(await copyText(lastPrompt?.json || "") ? "Copied" : "Copy failed", {});
$("#promptDownload").onclick = () => {
  const a = document.createElement("a");
  const url = URL.createObjectURL(new Blob([lastPrompt?.json || ""], { type: "application/json" }));
  a.href = url; a.download = `${S.chat.chat.title.replace(/[^\w.-]+/g, "_")}.messages.json`; a.click();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
};

// ---------- profile (tap the avatar or name in the header, like Telegram) ----------
const MEDIA_STATS = [["photo", "image", "photos"], ["video", "film", "videos"], ["round_video", "record", "circles"], ["voice", "mic", "voice"], ["audio", "music", "audio"], ["file", "file", "files"], ["sticker", "sticker", "stickers"], ["gif", "film", "GIFs"]];
const fmtDate = d => d ? new Date(d.slice(0, 10) + "T00:00:00").toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" }) : "";
function openProfile(name) {
  if (!S.chat) return;
  const { chat: c, export: ex } = S.chat;
  if (name && name !== c.persona) return openPersonInfo(name);
  const stats = S.chat.stats;
  const person = ex.participants.find(p => p.name === c.persona);
  const owner = !GUEST, template = owner && !c.guest;
  const row = (ic, v, k, extra = "") => `<div class="pf-row">${icon(ic)}<div class="grow">${v}${k ? `<div class="k">${esc(k)}</div>` : ""}${extra}</div></div>`;
  let h = `<div class="pf-hero">${avatarHtml(c.persona, person?.avatar || c.persona_avatar, "")}<div class="pf-name" id="pfName">${esc(c.title)}</div><div class="pf-sub">${esc(subtitle())}</div></div>`;
  if (owner) {
    const acts = [
      template && ["share", "share", "Share"],
      ["media", "image", "Media"],
      template && ["notes", "edit", "Edit"],
      ["prompt", "braces", "Prompt"],
    ].filter(Boolean);
    h += `<div class="pf-actions">${acts.map(([k, ic, label]) => `<button type="button" data-pf="${k}">${icon(ic)}<span>${label}</span></button>`).join("")}</div>`;
  }
  const rows = [];
  if (GUEST) {
    if (S.chat.share?.greeting) rows.push(row("chat", `<div class="v">${esc(S.chat.share.greeting)}</div>`, "Greeting"));
    rows.push(row(S.chat.share?.mode === "room" ? "users" : "user", `<div class="v">${S.chat.share?.mode === "room" ? "Shared room" : "Private conversation"}</div>`, S.chat.share?.mode === "room" ? "Everyone with the link sees these messages" : "Only you and the owner of the link can read it"));
    const left = S.chat.share?.replies_left;
    if (left != null) rows.push(row("clock", `<div class="v">${left.toLocaleString()}</div>`, `answer${left === 1 ? "" : "s"} left`));
  } else {
    const notes = S.chat.notes?.trim();
    rows.push(notes
      ? row("note", `<div class="v clamp" id="pfNotes">${esc(notes)}</div>`, "Notes for the AI", notes.length > 220 ? `<button type="button" class="more" data-pf="expand">Show more</button>` : "")
      : row("note", `<div class="v muted">No notes yet</div>`, "Notes help the AI: who they are, how you know each other", template ? `<button type="button" class="more" data-pf="notes">Add notes</button>` : ""));
    rows.push(row("user", `<div class="v">${esc(c.user)}</div>`, c.persona === ex.exporter ? "You play this side, and the AI writes as you" : "Your side of the conversation"));
    rows.push(row("archive", `<div class="v">${esc(ex.title)}</div>`, `Export${ex.imported_at ? ` · imported ${fmtDate(ex.imported_at)}` : ""}${person ? ` · ${person.messages.toLocaleString()} messages from ${c.persona}` : ""}`));
    if (stats?.first_date) rows.push(row("calendar", `<div class="v">${esc(fmtDate(stats.first_date))} – ${esc(fmtDate(stats.last_date))}</div>`, "First and last message"));
    if (S.chat.voice) rows.push(row("mic", `<div class="v">${esc(S.chat.voice)}</div>`, "Voice for voice messages"));
  }
  h += `<div class="pf-list">${rows.join("")}</div>`;
  if (stats) {
    const cells = [["chat", stats.messages, "messages"], ["sparkle", stats.live, "new"],
      ...MEDIA_STATS.map(([k, ic, label]) => [ic, stats.media?.[k] || 0, label]), ["link", stats.links, "links"]]
      .filter(([, n], i) => i === 0 || n);
    h += `<div class="pf-stats">${cells.map(([ic, n, label]) => `<div class="pf-stat">${icon(ic)}<div><b>${Number(n || 0).toLocaleString()}</b><span>${esc(label)}</span></div></div>`).join("")}</div>`;
  }
  const photos = galleryPhotos().slice(-8).reverse();
  if (photos.length) h += `<div class="pf-section">Recent photos</div><div class="pf-photos">${photos.map(m => `<button type="button" data-pf-photo="${m.id}" aria-label="Open photo">${`<img src="${esc(mediaUrl(m.media.thumb || m.media.file))}" alt="" loading="lazy">`}</button>`).join("")}</div>`;
  if (owner) h += `<button type="button" class="pf-danger" data-pf="delete">${icon("trash")}<span>${c.guest ? "Delete this visitor conversation" : "Delete chat"}</span></button>`;
  $("#pfBody").innerHTML = h;
  $("#profileDlg").showModal();
}
$("#peerBtn").onclick = () => openProfile();
// Info of someone other than the persona: another participant of a group export, or a visitor in a shared room.
function openPersonInfo(name) {
  const { chat: c, export: ex } = S.chat;
  const person = ex.participants.find(p => p.name === name);
  const row = (ic, v, k) => `<div class="pf-row">${icon(ic)}<div class="grow"><div class="v">${v}</div>${k ? `<div class="k">${esc(k)}</div>` : ""}</div></div>`;
  const sub = person ? (name === c.user ? "Your side of the conversation" : "Participant of the chat") : "Visitor in this room";
  const rows = [];
  if (person?.messages != null) rows.push(row("chat", person.messages.toLocaleString(), `message${person.messages === 1 ? "" : "s"} in the export`));
  if (person && name === ex.exporter) rows.push(row("archive", esc(ex.title), "Exported this chat"));
  if (!person) rows.push(row("users", "Shared room", "Writes in the same conversation as you"));
  $("#pfBody").innerHTML = `<div class="pf-hero">${avatarHtml(name, person?.avatar, "")}<div class="pf-name" id="pfName">${esc(name)}</div><div class="pf-sub">${esc(sub)}</div></div>`
    + (rows.length ? `<div class="pf-list">${rows.join("")}</div>` : "");
  $("#profileDlg").showModal();
}
$("#pfBody").addEventListener("click", e => {
  const ph = e.target.closest("[data-pf-photo]");
  if (ph) { $("#profileDlg").close(); openLightbox(Number(ph.dataset.pfPhoto)); return; }
  const b = e.target.closest("[data-pf]"); if (!b) return;
  const k = b.dataset.pf;
  if (k === "expand") { $("#pfNotes").classList.remove("clamp"); b.remove(); return; }
  // Open the next screen instead of stacking a second modal on top of this one.
  $("#profileDlg").close();
  ({ share: () => openShares(), media: () => openMedia(S.chat.export.id), notes: () => $("#notesBtn").click(), prompt: () => $("#promptBtn").click(), delete: () => $("#deleteChatBtn").click() })[k]?.();
});

// ---------- sticker / GIF picker ----------
function renderPicker() {
  $$(".picker-tabs button").forEach(b => b.classList.toggle("active", b.dataset.tab === S.pickerTab));
  const items = (S.chat?.library || []).filter(i => i.kind === S.pickerTab);
  $("#pickerGrid").innerHTML = items.length ? items.map(i => {
    const vid = /\.(webm|mp4)$/i.test(i.file);
    const src = /\.tgs$/i.test(i.file) ? mediaUrl(i.thumb) : mediaUrl(i.file);
    return `<button data-pick-id="${i.id}" title="${esc(i.description || i.id)}">${vid ? `<video src="${src}" muted loop autoplay playsinline></video>` : `<img src="${src}" alt="" loading="lazy">`}</button>`;
  }).join("") : `<div class="picker-empty">No ${S.pickerTab === "gif" ? "GIFs" : "stickers"} in this export.</div>`;
}
$("#pickerBtn").onclick = e => { e.stopPropagation(); const open = $("#picker").classList.toggle("hidden") === false; $("#pickerBtn").setAttribute("aria-expanded", open); renderPicker(); };
$$(".picker-tabs button").forEach(b => b.onclick = () => { S.pickerTab = b.dataset.tab; renderPicker(); });
$("#pickerGrid").onclick = e => {
  const b = e.target.closest("[data-pick-id]"); if (!b) return;
  $("#picker").classList.add("hidden"); $("#pickerBtn").setAttribute("aria-expanded", "false");
  send(S.pickerTab === "gif" ? { gif_id: b.dataset.pickId } : { sticker_id: b.dataset.pickId });
};
document.addEventListener("click", e => { if (!e.target.closest("#picker") && !e.target.closest("#pickerBtn")) $("#picker").classList.add("hidden"); });

// ---------- settings ----------
async function renderMcpStatus(servers) {
  $("#mcpStatus").innerHTML = servers.length ? servers.map(s => `<div class="mcp-row"><span class="dot ${s.connected ? "ok" : s.error ? "err" : ""}"></span>
    <div><b>${esc(s.name)}</b> ${s.connected ? `· MCP ${esc(s.protocol_version)} · ${s.tools.length} tools <span class="muted">${esc(s.tools.slice(0, 8).join(", "))}${s.tools.length > 8 ? "…" : ""}</span>` : `<span class="muted">${esc(s.error || "not connected")}</span>`}</div></div>`).join("")
    : `<div class="muted small">No MCP servers configured.</div>`;
}
$("#settingsBtn").onclick = async () => {
  try {
    const s = await api("/api/settings");
    $("#sBase").value = s.base_url; $("#sBase").placeholder = s.env.base_url || "https://openrouter.ai/api/v1";
    $("#sModel").value = s.model; $("#sModel").placeholder = s.env.model || "openai/gpt-6-luna";
    $("#sKey").value = ""; $("#sKey").placeholder = s.has_api_key ? "•••••• (saved — type to replace, “-” to clear)" : s.env.has_api_key ? "using OPENAI_API_KEY from environment" : "sk-…";
    $("#sTemp").value = s.temperature ?? "";
    $("#sBudget").value = s.history_budget_chars; $("#sSteps").value = s.max_tool_steps;
    $("#sMediaModel").value = s.media_model || ""; $("#sMediaModel").placeholder = s.env_media_model || "same as Model";
    $("#sMcp").value = JSON.stringify(s.mcpServers || {}, null, 2);
    openTgSettings(s.telegram || {});
    hideTip();
    $("#mcpStatus").innerHTML = `<div class="muted small">Checking MCP servers…</div>`;
    $("#sTestResult").className = "test-result hidden";
    $("#settingsDlg").showModal();
    if (S.state?.llm?.needs_key && !s.has_api_key) $(S.state.llm.endpoint_set ? "#sKey" : "#sBase").focus();
    api("/api/mcp").then(r => renderMcpStatus(r.servers)).catch(e => { $("#mcpStatus").textContent = e.message; });
  } catch (e) { toast(e.message, { error: true }); }
};
// Telegram: the mode applies right away (no Save needed), and the status follows the bot.
const TG = { env: false, mode: "off", relayHost: "iroof.top", link: null, poll: null, busy: false };
function openTgSettings(t) {
  TG.env = !!t.env; TG.mode = t.mode || "off"; TG.relayHost = t.relay_host || "iroof.top"; TG.link = null;
  $$(".tg-relay-host").forEach(e => { e.textContent = TG.relayHost; });
  setRadio("sTgMode", TG.mode);
  $$("#sTgModes input").forEach(i => { i.disabled = TG.env; });
  $("#sTgToken").value = ""; $("#sTgToken").placeholder = t.has_token ? "•••••• (saved, paste a new token to replace it)" : "Bot token from @BotFather, 123456789:AA…";
  $("#sTgTokenRow").classList.toggle("hidden", TG.mode !== "own" || TG.env);
  $("#sTgStatus").innerHTML = TG.mode === "off" && !TG.env ? "" : `<div class="muted small">Checking…</div>`;
  refreshTg();
  clearInterval(TG.poll);
  TG.poll = setInterval(() => { if (!document.hidden && (TG.mode !== "off" || TG.env)) refreshTg(); }, 3000);
}
$("#settingsDlg").addEventListener("close", () => { clearInterval(TG.poll); TG.poll = null; });
async function refreshTg() {
  try { renderTgStatus(await api("/api/telegram")); } catch { }
}
function renderTgStatus(t) {
  const out = $("#sTgStatus");
  if (!t.enabled) {
    out.innerHTML = TG.mode === "own" ? `<div class="muted small">Paste the token of your bot and press <b>Connect</b>.</div>`
      : TG.mode === "shared" ? `<div class="tg-line err small">${esc(t.error || "The shared bot isn't available right now.")}</div>`
        : `<div class="muted small">Off: visitors of your links talk to the AI only.</div>`;
    return;
  }
  const who = t.username ? `<b>@${esc(t.username)}</b>` : "the bot";
  const state = t.error ? `<span class="dot err"></span><span class="err">${esc(t.error)}</span>`
    : t.username ? `<span class="dot ok"></span><span>Connected as ${who}${TG.env ? " (set by TELEGRAM_BOT_TOKEN)" : ""}</span>`
      : `<span class="dot"></span><span>Connecting…</span>`;
  const groups = t.groups?.length ? `Groups: ${t.groups.map(g => `<b>${esc(g.title)}</b>`).join(", ")}. Pick one when you edit a share link.` : "No groups yet: add the bot to a group, then pick that group in a share link.";
  out.innerHTML = `<div class="tg-line small">${state}</div>
    ${t.username ? `<div class="muted small">${groups}${t.source === "shared" ? ` Messages pass through ${esc(TG.relayHost)}.` : ""}</div>
    <div class="tg-actions"><button type="button" class="btn sm" id="sTgAdd">${icon("plus")}<span>Add to a group</span></button></div>` : ""}
    ${TG.link ? `<div class="qr-panel"><img src="${qrSrc(TG.link)}" width="148" height="148" alt="QR code to add the bot to a group"><div class="qr-side"><div class="small">Telegram opened in your browser. Or scan this with your phone: it opens Telegram to pick the group${t.source === "shared" ? " (valid for 1 hour, once)" : ""}.</div><a class="small" href="${esc(TG.link)}" target="_blank" rel="noopener noreferrer">${esc(TG.link)}</a></div></div>` : ""}`;
  const add = $("#sTgAdd");
  if (add) add.onclick = async () => {
    add.disabled = true;
    try {
      const r = await api("/api/telegram/link", { method: "POST" });
      TG.link = r.url; window.open(r.url, "_blank", "noopener");
      renderTgStatus(t);
    } catch (e) { toast(e.message, { error: true }); add.disabled = false; }
  };
}
async function setTg(body) {
  if (TG.busy) return;
  TG.busy = true; $("#sTgConnect").disabled = true;
  $("#sTgStatus").innerHTML = `<div class="muted small">${body.mode === "off" ? "Turning off…" : "Connecting…"}</div>`;
  try {
    await api("/api/settings", { method: "PUT", body: { telegram: body } });
    TG.mode = body.mode; TG.link = null;
    if (body.token) { $("#sTgToken").value = ""; $("#sTgToken").placeholder = "•••••• (saved, paste a new token to replace it)"; }
    await refreshTg();
  } catch (e) {
    $("#sTgStatus").innerHTML = `<div class="tg-line err small">${esc(e.message)}</div>`;
  } finally { TG.busy = false; $("#sTgConnect").disabled = false; }
}
$("#sTgModes").addEventListener("change", () => {
  const mode = radioValue("sTgMode");
  $("#sTgTokenRow").classList.toggle("hidden", mode !== "own");
  // An own bot needs its token first (unless one is saved): Connect applies it.
  if (mode === "own" && !/saved/.test($("#sTgToken").placeholder)) {
    TG.mode = "own"; $("#sTgStatus").innerHTML = `<div class="muted small">Paste the token of your bot and press <b>Connect</b>.</div>`; $("#sTgToken").focus(); return;
  }
  setTg({ mode });
});
$("#sTgConnect").onclick = () => {
  const token = $("#sTgToken").value.trim();
  if (!token && !/saved/.test($("#sTgToken").placeholder)) { $("#sTgToken").focus(); return; }
  setTg(token ? { mode: "own", token } : { mode: "own" });
};
$("#sTgToken").addEventListener("keydown", e => { if (e.key === "Enter") { e.preventDefault(); $("#sTgConnect").click(); } });
$("#mcpReconnect").onclick = async () => {
  $("#mcpStatus").innerHTML = `<div class="muted small">Reconnecting…</div>`;
  try { renderMcpStatus((await api("/api/mcp/reconnect", { method: "POST" })).servers); } catch (e) { $("#mcpStatus").textContent = e.message; }
};
function settingsBody() {
  let mcp;
  try { mcp = JSON.parse($("#sMcp").value.trim() || "{}"); } catch (err) { throw new Error(`MCP JSON: ${err.message}`); }
  const body = {
    base_url: $("#sBase").value, model: $("#sModel").value,
    temperature: $("#sTemp").value === "" ? null : Number($("#sTemp").value),
    history_budget_chars: Number($("#sBudget").value) || undefined, max_tool_steps: Number($("#sSteps").value) || undefined,
    media_model: $("#sMediaModel").value,
    mcpServers: mcp,
  };
  const key = $("#sKey").value.trim();
  if (key) body.api_key = key === "-" ? "" : key;
  return body;
}
$("#settingsForm").addEventListener("submit", async e => {
  if (e.submitter?.value !== "ok") return;
  e.preventDefault();
  try {
    await api("/api/settings", { method: "PUT", body: settingsBody() });
    $("#settingsDlg").close(); toast("Settings saved"); loadState();
  } catch (err) { toast(err.message, { error: true }); }
});
$("#sTest").onclick = async () => {
  const out = $("#sTestResult"), btn = $("#sTest");
  btn.disabled = true; out.className = "test-result"; out.textContent = "Saving and asking the model…";
  try {
    await api("/api/settings", { method: "PUT", body: settingsBody() });
    if ($("#sKey").value.trim()) $("#sKey").placeholder = $("#sKey").value.trim() === "-" ? "sk-…" : "•••••• (saved — type to replace, “-” to clear)";
    $("#sKey").value = ""; loadState().catch(() => { });
    const r = await api("/api/llm/test", { method: "POST" });
    out.className = `test-result ${r.ok ? "ok" : "err"}`;
    out.textContent = r.ok ? `Connected: ${r.model} on ${r.host} answered in ${(r.ms / 1000).toFixed(1)} s.` : r.error;
  } catch (err) { out.className = "test-result err"; out.textContent = err.message; }
  finally { btn.disabled = false; }
};
// Errors that a setting can fix get a button to open Settings (owner only).
function errorToast(msg, retry) {
  if (!GUEST && /Settings/.test(msg)) toast(msg, { error: true, action: () => $("#settingsBtn").click(), actionLabel: "Open Settings", ms: 9000 });
  else toast(msg, { error: true, action: retry, ms: 7000 });
}
// No model can answer yet: a one-line hint. Until an endpoint is chosen it stays provider-neutral.
function renderSetupNote() {
  const llm = S.state?.llm;
  $("#setupNote").classList.toggle("hidden", !llm?.needs_key);
  if (!llm?.needs_key) return;
  $("#setupNote").querySelector("b").textContent = llm.endpoint_set ? "Add an API key" : "Connect a model";
  $("#setupNoteText").textContent = llm.endpoint_set ? `${llm.host} needs a key to answer` : "OpenAI-compatible API or local model";
  $("#setupNote").title = llm.endpoint_set ? `Endpoint: ${llm.base_url}` : "OpenAI, OpenRouter, Gemini, Mistral, Ollama, LM Studio…";
}
$("#setupNote").onclick = () => $("#settingsBtn").click();

// ---------- media library (descriptions for the model) ----------
const VISUAL = ["sticker", "gif", "photo", "video", "round_video", "voice"];
const MD = { exportId: null, items: [], filter: "all", query: "", busy: new Set(), shown: 0, audio: null, io: null };
const MD_PAGE = 60; // cards rendered per step: big libraries stay fast
const MD_FILTERS = [["all", "All"], ["missing", "Missing"], ["sticker", "Stickers"], ["gif", "GIFs"], ["photo", "Photos"], ["video", "Videos"], ["voice", "Voice"]];
const MD_KIND = { sticker: ["sticker", "Sticker"], gif: ["film", "GIF"], photo: ["image", "Photo"], video: ["film", "Video"], round_video: ["record", "Video message"], voice: ["mic", "Voice"] };
async function openMedia(exportId) {
  try {
    const ex = await api(`/api/exports/${exportId}`);
    MD.exportId = exportId; MD.items = ex.library_items.filter(i => VISUAL.includes(i.kind)); MD.ffmpeg = ex.ffmpeg !== false;
    MD.filter = "all"; MD.query = ""; $("#mdSearch").value = "";
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
function mdVisible() {
  const q = MD.query.trim().toLowerCase();
  return MD.items.filter(i => (MD.filter === "all" ? true : MD.filter === "missing" ? !i.description : MD.filter === "video" ? i.kind === "video" || i.kind === "round_video" : i.kind === MD.filter)
    && (!q || i.id.toLowerCase().includes(q) || (i.description || "").toLowerCase().includes(q)));
}
// Header: filters, progress and the bulk button. Cheap, and it never touches the cards.
function renderMediaHeader() {
  const missing = MD.items.filter(i => !i.description && i.describable).length;
  const described = MD.items.filter(i => i.description).length, total = MD.items.length;
  const manualOnly = MD.items.filter(i => !i.description && !i.describable).length;
  const count = k => k === "all" ? total : k === "missing" ? total - described : MD.items.filter(i => k === "video" ? i.kind === "video" || i.kind === "round_video" : i.kind === k).length;
  $("#mdFilters").innerHTML = MD_FILTERS.map(([k, label]) => {
    const n = count(k);
    return n || k === "all" || k === MD.filter ? `<button role="tab" aria-selected="${MD.filter === k}" data-filter="${k}">${label} <span>${n}</span></button>` : "";
  }).join("");
  const meter = $("#mdMeter");
  meter.firstElementChild.style.width = `${total ? (described / total) * 100 : 0}%`;
  meter.setAttribute("aria-valuemax", String(total)); meter.setAttribute("aria-valuenow", String(described));
  meter.classList.toggle("done", total > 0 && described === total);
  $("#mdSummary").textContent = `${described} of ${total} described` + (manualOnly ? ` · ${manualOnly} to type by hand` : "");
  const btn = $("#mdDescribe"), run = MD.run;
  btn.disabled = run ? run.stop : !missing;
  btn.classList.toggle("primary", !run);
  btn.querySelector("span").textContent = run ? (run.stop ? "Stopping…" : `Stop · ${run.done}/${run.total}`) : missing ? `Describe ${missing} with AI` : "All described";
}
function renderMedia() {
  renderMediaHeader();
  const list = mdVisible();
  MD.shown = Math.min(list.length, Math.max(MD_PAGE, MD.shown));
  const more = list.length - MD.shown;
  $("#mdGrid").innerHTML = list.length
    ? list.slice(0, MD.shown).map(mdCard).join("") + (more > 0 ? `<button type="button" class="btn md-more" data-md-more>Show ${Math.min(more, MD_PAGE)} more of ${more}</button>` : "")
    : `<div class="md-empty muted">${MD.query ? `Nothing matches “${esc(MD.query)}”.` : MD.filter === "missing" ? "Everything is described." : "Nothing here."}</div>`;
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
  const run = MD.run ??= { queue: [], total: 0, done: 0, ok: 0, failed: [], active: 0, stop: false };
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
      const r = await api(`/api/exports/${MD.exportId}/describe`, { method: "POST", body: { ids: [id], force } });
      const d = r.items?.[id];
      if (d != null) { const it = mdItem(id); if (it) it.description = d; run.ok++; }
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
  if ($("#mediaDlg").open) renderMediaHeader();
  const skipped = run.total - run.done;
  const what = `Described ${run.ok} of ${run.total}`;
  if (run.failed.length) errorToast(`${what}, ${run.failed.length} failed${skipped ? `, ${skipped} skipped` : ""}. ${run.failed[0]}`);
  else if (skipped) toast(`${what}, stopped`);
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
$("#mdFilters").onclick = e => { const b = e.target.closest("[data-filter]"); if (b) { MD.filter = b.dataset.filter; MD.shown = 0; renderMedia(); $("#mdGrid").scrollTop = 0; } };
let mdSearchTimer;
$("#mdSearch").addEventListener("input", e => { clearTimeout(mdSearchTimer); mdSearchTimer = setTimeout(() => { MD.query = e.target.value; MD.shown = 0; renderMedia(); }, 150); });
$("#mdDescribe").onclick = async () => {
  if (MD.run) { mdStopRun(MD.run); return; }
  const ids = MD.items.filter(i => !i.description && i.describable).map(i => i.id);
  if (!ids.length) return;
  if (!await confirmDialog({
    title: `Describe ${ids.length} item${ids.length > 1 ? "s" : ""} with AI?`,
    text: `The images and voice messages are sent to the media model from Settings, one request each, which costs tokens. The descriptions are saved with this export and used by all its chats. You can edit them afterwards.`,
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

// ---------- photo viewer (lightbox gallery) ----------
const LB = { list: [], i: 0, zoom: 1, x: 0, y: 0, opener: null };
function galleryPhotos() {
  return (S.chat?.messages || []).filter(m => m.media?.kind === "photo" && m.media.available !== false && m.media.file);
}
function openLightbox(id) {
  LB.list = galleryPhotos(); LB.i = Math.max(0, LB.list.findIndex(m => m.id === id));
  if (!LB.list.length) return;
  LB.opener = document.activeElement;
  $("#lbStrip").innerHTML = LB.list.length > 1 ? LB.list.map((m, k) => `<button data-k="${k}" aria-label="Photo ${k + 1}"><img src="${mediaUrl(m.media.file)}" alt="" loading="lazy"></button>`).join("") : "";
  const lb = $("#lightbox"); lb.classList.remove("hidden"); document.body.classList.add("lb-open");
  showLb(); lb.focus();
}
function closeLightbox() {
  $("#lightbox").classList.add("hidden"); document.body.classList.remove("lb-open");
  $("#lbImg").removeAttribute("src"); LB.opener?.focus?.();
}
function setZoom(z, cx = 0, cy = 0) {
  LB.zoom = z; if (z === 1) { LB.x = 0; LB.y = 0; } else { LB.x = -cx * (z - 1); LB.y = -cy * (z - 1); }
  applyZoom();
}
function applyZoom() {
  const img = $("#lbImg");
  img.style.transform = `translate(${LB.x}px, ${LB.y}px) scale(${LB.zoom})`;
  $("#lightbox").classList.toggle("zoomed", LB.zoom > 1);
}
function showLb() {
  const m = LB.list[LB.i]; if (!m) return;
  const src = mediaUrl(m.media.file), n = LB.list.length;
  const img = $("#lbImg"); img.classList.add("loading"); img.onload = () => img.classList.remove("loading");
  img.src = src; img.alt = m.media.description || m.text || "Photo";
  setZoom(1);
  $("#lbName").textContent = m.mine ? "You" : m.from;
  $("#lbDate").textContent = m.date ? `${fmtDay(m.date)}, ${fmtTime(m.date)}` : "";
  $("#lbCount").textContent = n > 1 ? `${LB.i + 1} / ${n}` : "";
  $("#lbCaption").textContent = m.text || "";
  $("#lbCaption").classList.toggle("hidden", !m.text);
  $("#lbOpen").href = src; $("#lbDownload").href = src;
  $("#lbPrev").disabled = LB.i === 0; $("#lbNext").disabled = LB.i === n - 1;
  $$("#lbStrip button").forEach((b, k) => { b.classList.toggle("on", k === LB.i); if (k === LB.i) b.scrollIntoView({ inline: "center", block: "nearest" }); });
  // Preload neighbours for instant swipes.
  for (const k of [LB.i - 1, LB.i + 1]) if (LB.list[k]) new Image().src = mediaUrl(LB.list[k].media.file);
}
const lbGo = d => { const k = LB.i + d; if (k >= 0 && k < LB.list.length) { LB.i = k; showLb(); } };
$("#lbPrev").onclick = e => { e.stopPropagation(); lbGo(-1); };
$("#lbNext").onclick = e => { e.stopPropagation(); lbGo(1); };
$("#lbClose").onclick = closeLightbox;
$("#lbLocate").onclick = () => {
  const id = LB.list[LB.i]?.id; closeLightbox();
  const el = $(`#m-${id}`); if (el) { el.scrollIntoView({ block: "center" }); el.classList.remove("flash"); void el.offsetWidth; el.classList.add("flash"); }
};
$("#lbStrip").onclick = e => { const b = e.target.closest("[data-k]"); if (b) { LB.i = Number(b.dataset.k); showLb(); } };
$("#lightbox").addEventListener("keydown", e => {
  if (e.key === "Escape") { e.preventDefault(); LB.zoom > 1 ? setZoom(1) : closeLightbox(); }
  else if (e.key === "ArrowLeft") lbGo(-1);
  else if (e.key === "ArrowRight") lbGo(1);
  else if (e.key === "Tab") { // keep focus inside the viewer
    const f = $$("#lightbox button:not(:disabled), #lightbox a[href]").filter(x => x.offsetParent);
    if (!f.length) return;
    const first = f[0], last = f[f.length - 1];
    if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
    else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
  }
});
// Viewer gestures: click on the dark area closes; double-click / double-tap zooms; drag pans; swipe navigates or closes.
(() => {
  const stage = $("#lbStage"), img = $("#lbImg");
  let start = null, moved = false, lastTap = 0;
  const zoomAt = (cx, cy) => {
    if (LB.zoom > 1) return setZoom(1);
    const r = img.getBoundingClientRect();
    setZoom(2.5, cx - (r.left + r.width / 2), cy - (r.top + r.height / 2));
  };
  stage.addEventListener("pointerdown", e => {
    if (e.target.closest(".lb-nav")) return;
    start = { x: e.clientX, y: e.clientY, ox: LB.x, oy: LB.y, t: Date.now() }; moved = false;
    stage.setPointerCapture(e.pointerId);
  });
  stage.addEventListener("pointermove", e => {
    if (!start) return;
    const dx = e.clientX - start.x, dy = e.clientY - start.y;
    if (Math.abs(dx) + Math.abs(dy) > 6) moved = true;
    if (LB.zoom > 1) { LB.x = start.ox + dx; LB.y = start.oy + dy; applyZoom(); }
    else if (moved) { img.style.transform = `translate(${dx}px, ${Math.max(0, dy) * .6}px)`; img.style.opacity = String(1 - Math.min(.5, Math.max(0, dy) / 400)); }
  });
  stage.addEventListener("pointerup", e => {
    if (!start) return;
    const dx = e.clientX - start.x, dy = e.clientY - start.y, fast = Date.now() - start.t < 600;
    const onImg = e.target === img; start = null; img.style.opacity = "";
    if (LB.zoom === 1) img.style.transform = "";
    if (moved) {
      if (LB.zoom > 1) return;
      if (Math.abs(dx) > 60 && Math.abs(dx) > Math.abs(dy)) return lbGo(dx < 0 ? 1 : -1);
      if (dy > 110 && fast) return closeLightbox();
      return;
    }
    const now = Date.now();
    if (onImg && now - lastTap < 300) { lastTap = 0; zoomAt(e.clientX, e.clientY); return; }
    lastTap = onImg ? now : 0;
    if (!onImg && LB.zoom === 1) closeLightbox();
  });
  stage.addEventListener("wheel", e => {
    if (!e.ctrlKey && LB.zoom === 1) return; // trackpad pinch arrives as ctrl+wheel
    e.preventDefault();
    const z = Math.min(5, Math.max(1, LB.zoom * (e.deltaY < 0 ? 1.15 : 1 / 1.15)));
    if (z === 1) setZoom(1); else { LB.zoom = z; applyZoom(); }
  }, { passive: false });
})();


// ---------- voice messages (record → 16 kHz mono WAV → upload, then the server transcribes) ----------
const REC = { recorder: null, stream: null, chunks: [], start: 0, timer: null, send: false };
const fmtSecs = s => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
async function startRecording() {
  if (REC.recorder) return;
  try {
    REC.stream = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true } });
  } catch (e) {
    return toast(e.name === "NotAllowedError" ? "Microphone access was denied." : `Microphone unavailable: ${e.message}`, { error: true });
  }
  REC.chunks = []; REC.send = false;
  REC.recorder = new MediaRecorder(REC.stream);
  REC.recorder.ondataavailable = e => { if (e.data.size) REC.chunks.push(e.data); };
  REC.recorder.onstop = finishRecording;
  REC.recorder.start(250);
  REC.start = Date.now();
  $(".compose-row").classList.add("hidden"); $("#recBar").classList.remove("hidden");
  $("#recHint").textContent = "Recording…"; $("#recTime").textContent = "0:00";
  REC.timer = setInterval(() => {
    const s = (Date.now() - REC.start) / 1000;
    $("#recTime").textContent = fmtSecs(s);
    if (s >= 300) stopRecording(true); // 5 minutes max
  }, 250);
  $("#recSend").focus();
}
function stopRecording(send) {
  if (!REC.recorder) return;
  REC.send = send;
  clearInterval(REC.timer);
  if (REC.recorder.state !== "inactive") REC.recorder.stop();
}
async function finishRecording() {
  const duration = (Date.now() - REC.start) / 1000;
  REC.stream?.getTracks().forEach(t => t.stop());
  const blob = new Blob(REC.chunks, { type: REC.recorder.mimeType || "audio/webm" });
  REC.recorder = null; REC.stream = null;
  $("#recBar").classList.add("hidden"); $(".compose-row").classList.remove("hidden");
  if (!REC.send) return;
  if (duration < 0.6) return toast("Hold on a bit longer: the message was too short.");
  try {
    const wav = await toWav(blob);
    const fd = new FormData();
    fd.append("file", wav, "voice.wav");
    fd.append("duration", String(Math.round(duration)));
    if (S.reply) fd.append("reply_to", String(S.reply.id));
    S.reply = null; renderReply();
    $("#micBtn").disabled = true;
    const r = await api(chatPath("voice"), { method: "POST", body: fd });
    upsertMessages(r.messages); renderMessages(); scheduleRespond();
  } catch (e) { toast(`Voice message not sent: ${e.message}`, { error: true }); }
  finally { $("#micBtn").disabled = false; }
}
// Any browser recording (webm/opus, mp4/aac, ogg) → 16 kHz mono PCM WAV: small, plays everywhere, transcribable.
async function toWav(blob) {
  const AC = window.AudioContext || window.webkitAudioContext;
  const ctx = new AC();
  let buf;
  try { buf = await ctx.decodeAudioData(await blob.arrayBuffer()); } finally { ctx.close(); }
  const rate = 16000, frames = Math.max(1, Math.ceil(buf.duration * rate));
  const off = new OfflineAudioContext(1, frames, rate);
  const src = off.createBufferSource(); src.buffer = buf; src.connect(off.destination); src.start();
  const pcm = (await off.startRendering()).getChannelData(0);
  const out = new DataView(new ArrayBuffer(44 + pcm.length * 2));
  const str = (o, t) => { for (let i = 0; i < t.length; i++) out.setUint8(o + i, t.charCodeAt(i)); };
  str(0, "RIFF"); out.setUint32(4, 36 + pcm.length * 2, true); str(8, "WAVEfmt ");
  out.setUint32(16, 16, true); out.setUint16(20, 1, true); out.setUint16(22, 1, true);
  out.setUint32(24, rate, true); out.setUint32(28, rate * 2, true); out.setUint16(32, 2, true); out.setUint16(34, 16, true);
  str(36, "data"); out.setUint32(40, pcm.length * 2, true);
  for (let i = 0; i < pcm.length; i++) out.setInt16(44 + i * 2, Math.max(-1, Math.min(1, pcm[i])) * 0x7fff, true);
  return new Blob([out], { type: "audio/wav" });
}
$("#micBtn").onclick = () => startRecording();
$("#recSend").onclick = () => stopRecording(true);
$("#recCancel").onclick = () => stopRecording(false);
document.addEventListener("keydown", e => { if (e.key === "Escape" && REC.recorder) stopRecording(false); });
autosize();

// ---------- clipboard (navigator.clipboard needs a secure context) ----------
// Copy buttons confirm in place: a check and "Copied" for a moment, then the label returns.
async function copyWithFeedback(btn, text, done) {
  if (!await copyText(text)) return toast("Copy failed: select the text and copy it by hand", { error: true });
  clearTimeout(btn._copyTimer);
  if (btn._label == null) { btn._label = btn.innerHTML; btn.style.minWidth = `${btn.offsetWidth}px`; }
  btn.innerHTML = `${icon("check")}<span aria-live="polite">${esc(done)}</span>`;
  btn.classList.add("copied");
  btn._copyTimer = setTimeout(() => { btn.innerHTML = btn._label; btn._label = null; btn.style.minWidth = ""; btn.classList.remove("copied"); }, 1800);
}
async function copyText(text) {
  try { if (navigator.clipboard && isSecureContext) { await navigator.clipboard.writeText(text); return true; } } catch { }
  const ta = document.createElement("textarea");
  ta.value = text; ta.setAttribute("readonly", ""); ta.style.cssText = "position:fixed;top:0;left:0;opacity:0";
  (document.querySelector("dialog[open]") || document.body).append(ta);
  ta.select(); ta.setSelectionRange(0, text.length);
  let ok = false; try { ok = document.execCommand("copy"); } catch { }
  ta.remove();
  return ok;
}

// ---------- visitor mode (/s/<slug>) ----------
const tokenKey = () => `iroof:s:${GUEST.slug}`;
function forgetGuestToken() { if (localStorage.getItem(tokenKey()) === GUEST.token) localStorage.removeItem(tokenKey()); GUEST.token = null; }
function gateError(msg) { const e = $("#gateErr"); e.textContent = msg || ""; e.classList.toggle("hidden", !msg); }
async function showGate(error) {
  stopRoomPoll();
  $("#chatView").classList.add("hidden"); $("#welcome").classList.add("hidden"); $("#app").classList.remove("in-chat");
  history.replaceState(null, "", `/s/${enc(GUEST.slug)}`);
  try {
    GUEST.info = await api(guestBase());
  } catch (e) {
    $("#guestGate").classList.remove("hidden");
    $("#gateAvatar").outerHTML = `<div class="avatar xl gone" id="gateAvatar">${icon("link")}</div>`;
    $("#gateTitle").textContent = "Link not available";
    $("#gateGreeting").textContent = e.message;
    $("#gateFields").classList.add("hidden"); $("#gateResume").classList.add("hidden"); $(".gate-note").classList.add("hidden");
    return;
  }
  const info = GUEST.info;
  document.title = `${info.display_name} · iRoof.top`;
  $("#gateAvatar").outerHTML = (info.avatar
    ? `<div class="avatar xl" style="background-image:url('${guestBase()}/avatar')"></div>`
    : avatarHtml(info.display_name, null, "xl")).replace('class="avatar ', 'id="gateAvatar" class="avatar ');
  $("#gateTitle").textContent = info.display_name;
  $("#gateGreeting").textContent = info.greeting || (info.mode === "room" ? "A shared room: everyone with this link sees the same conversation." : "");
  $("#gateGreeting").classList.toggle("hidden", !$("#gateGreeting").textContent);
  const saved = localStorage.getItem(tokenKey());
  $("#gateResume").classList.toggle("hidden", !saved);
  $("#gatePinRow").classList.toggle("hidden", !info.pin);
  $("#gatePin").inputMode = info.pin_numeric ? "numeric" : "text";
  $("#gateStart span").textContent = saved ? "Start a new conversation" : "Start chatting";
  $("#gateStart").classList.toggle("primary", !saved);
  $("#gateFields").classList.toggle("hidden", !!info.full && !saved);
  if (info.full) gateError("This link has reached its limit of conversations.");
  else gateError(error);
  $("#guestGate").classList.remove("hidden");
  const focus = !saved && matchMedia("(pointer: fine)").matches;
  if (focus) (info.pin ? $("#gatePin") : $("#gateYou")).focus();
}
async function enterGuestChat(token) {
  GUEST.token = token;
  localStorage.setItem(tokenKey(), token);
  history.replaceState(null, "", `/s/${enc(GUEST.slug)}/${enc(token)}`);
  S.chatId = token; S.limit = PAGE;
  if (await openChat(token)) startRoomPoll();
}
$("#gateContinue").onclick = () => enterGuestChat(localStorage.getItem(tokenKey()));
$("#gateForm").addEventListener("submit", async e => {
  e.preventDefault();
  const btn = $("#gateStart");
  if (btn.disabled) return;
  const pin = $("#gatePin").value.trim();
  if (GUEST.info?.pin && !pin) { gateError("Enter the PIN you got with the link."); $("#gatePin").focus(); return; }
  btn.disabled = true; gateError("");
  try {
    const r = await api(`${guestBase()}/sessions`, { method: "POST", body: { pin: pin || undefined, name: $("#gateYou").value.trim() || undefined } });
    $("#gatePin").value = "";
    await enterGuestChat(r.token);
  } catch (err) {
    gateError(err.message);
    if (GUEST.info?.pin) { $("#gatePin").select(); $("#gatePin").focus(); }
  } finally { btn.disabled = false; }
});
// Rooms: pick up other visitors' messages. Relayed links: pick up the answers and the typing state.
let roomTimer = null, roomSig = "", relayTyping = false;
const roomSignature = msgs => JSON.stringify(msgs.slice(-30).map(m => [m.id, m.reactions]));
function stopRoomPoll() { clearInterval(roomTimer); roomTimer = null; relayTyping = false; }
async function pollGuest() {
  if (S.busy || !GUEST?.token) return;
  try {
    const data = await api(`${chatPath()}?limit=${S.limit}`);
    if (S.busy) return;
    const sig = roomSignature(data.messages);
    const typing = !!(data.share?.relay && data.typing);
    if (sig !== roomSig) {
      roomSig = sig;
      data.has_more ??= data.total > data.messages.length;
      S.chat = data; S.byId = new Map(data.messages.map(m => [m.id, m]));
      renderMessages({ keepScroll: true });
      if (typing) setTyping(true);
    } else S.chat.share = data.share;
    if (typing !== relayTyping) { relayTyping = typing; setTyping(typing); }
  } catch { }
}
function startRoomPoll() {
  stopRoomPoll();
  const relay = !!S.chat?.share?.relay;
  if (S.chat?.share?.mode !== "room" && !relay) return;
  roomSig = roomSignature(S.chat.messages);
  if (relay && S.chat.typing) { relayTyping = true; setTyping(true); }
  roomTimer = setInterval(() => { if (!document.hidden) pollGuest(); }, relay ? 2000 : 4000);
}
// Presets in the link open the chat right away: /s/<slug>#name=Greg&pin=123456.
// The PIN is read only from the fragment, which browsers never send to the server
// (no access or proxy logs); the name may also come as ?name=. Both leave the
// address bar as soon as the gate shows.
function bootGuest() {
  document.documentElement.classList.add("guest");
  $("#app").classList.add("guest-mode");
  const hash = new URLSearchParams(location.hash.slice(1)), query = new URLSearchParams(location.search);
  const pin = (hash.get("pin") || "").trim().slice(0, 64);
  const name = (hash.get("name") || query.get("name") || "").split(/\s+/).filter(Boolean).join(" ").slice(0, 40);
  if (pin) $("#gatePin").value = pin;
  if (name) $("#gateYou").value = name;
  if (GUEST.token) enterGuestChat(GUEST.token);
  else showGate().then(() => { if (pin || name) autoEnter(); });
}
async function autoEnter() {
  const info = GUEST.info;
  if (!info) return;
  // Coming back with the same invite resumes the conversation instead of using up another slot.
  const saved = localStorage.getItem(tokenKey());
  if (saved) { $("#gatePin").value = ""; return enterGuestChat(saved); }
  if (info.full) return;
  if (info.pin && !$("#gatePin").value.trim()) { $("#gatePin").focus(); return; }
  $("#gateForm").requestSubmit();
}

// ---------- share links (owner) ----------
const SH = { shares: [], pins: {}, editing: null, tg: { enabled: false, groups: [] }, net: { available: false }, qr: {} };
const tgTitle = id => SH.tg.groups.find(g => g.id === id)?.title || "Telegram group";
const isLocalHost = () => ["localhost", "127.0.0.1", "[::1]", "::1"].includes(location.hostname) || location.hostname.endsWith(".localhost");
// Addresses where the links work, best first: the internet relay, the local network, this page's own origin.
function linkBases() {
  const n = SH.net, out = [];
  if (n.available && n.internet?.enabled && n.internet.url) out.push({ kind: "internet", label: "Internet", base: n.internet.url });
  if (n.available && n.lan?.enabled && n.lan.url) out.push({ kind: "lan", label: "Wi‑Fi", base: n.lan.url });
  out.push({ kind: "here", label: isLocalHost() ? "This computer" : location.host, base: location.origin });
  return out;
}
const reachable = () => linkBases().some(b => b.kind !== "here") || !isLocalHost();
const shareUrl = (sh, base = linkBases()[0].base) => `${base}/s/${enc(sh.slug)}`;
const hostOf = u => u.replace(/^https?:\/\//, "");
const QR_NOTE = {
  internet: "Scan with a phone camera. Works anywhere while iRoof.top runs on this computer.",
  lan: "Scan with a phone camera. The phone must be on the same Wi‑Fi as this computer.",
  here: "This address only works on this computer. Turn on Wi‑Fi or Internet sharing to open it on a phone.",
};
const qrSrc = text => `/api/qr?text=${encodeURIComponent(text)}`;
const HIST = { none: "history hidden", recent: "recent history", full: "full history" };
function fmtWhen(d) { return d ? `${fmtListTime(d)}${d.slice(0, 10) === new Date().toISOString().slice(0, 10) ? "" : ` ${d.slice(11, 16)}`}` : ""; }
// One dialog, two pages (list ↔ editor), like Telegram's nested settings: no modal on top of a modal.
function sharePage(edit) {
  hideTip();
  $("#shListPage").classList.toggle("hidden", edit);
  $("#shareEditForm").classList.toggle("hidden", !edit);
  $("#shareDlg").setAttribute("aria-labelledby", edit ? "seH" : "shH");
  $("#shareDlg").scrollTop = 0;
}
const shareEditing = () => !$("#shareEditForm").classList.contains("hidden");
async function openShares({ edit } = {}) {
  try {
    [SH.shares, SH.tg, SH.net] = await Promise.all([
      api(`/api/chats/${S.chatId}/shares`).then(r => r.shares),
      api("/api/telegram").catch(() => ({ enabled: false, groups: [] })),
      api("/api/sharing").catch(() => ({ available: false })),
    ]);
    $("#shH").textContent = `Share «${S.chat.chat.title}»`;
    renderReach();
    renderShares();
    if (edit !== undefined || !SH.shares.length) openShareEdit(edit || null);
    else sharePage(false);
    if (!$("#shareDlg").open) $("#shareDlg").showModal();
    startSharePoll();
  } catch (e) { toast(e.message, { error: true }); }
}
// Visitors arrive while the dialog is open: refresh the list in the background
// (re-rendered only when something changed, so copy feedback and open lists stay).
let sharePoll = null;
const shareSig = shares => JSON.stringify(shares.map(sh => [sh.id, sh.active, sh.sessions.map(g => [g.id, g.messages, g.replies, g.last_seen])]));
async function refreshShares({ manual = false } = {}) {
  if (!$("#shareDlg").open || !S.chatId) return;
  const btn = $("#shRefresh");
  if (manual) btn.classList.add("spinning");
  try {
    const [shares, net] = await Promise.all([api(`/api/chats/${S.chatId}/shares`).then(r => r.shares), api("/api/sharing").catch(() => SH.net)]);
    if (JSON.stringify(net) !== JSON.stringify(SH.net)) {
      SH.net = net; renderReach();
      if (!shareEditing()) renderShares();
    }
    if (shareSig(shares) !== shareSig(SH.shares)) {
      SH.shares = shares;
      if (!shareEditing()) renderShares();
      loadState().catch(() => { });
    }
  } catch (e) { if (manual) toast(e.message, { error: true }); }
  finally { if (manual) setTimeout(() => btn.classList.remove("spinning"), 400); }
}
function startSharePoll() {
  clearInterval(sharePoll);
  sharePoll = setInterval(() => { if (!document.hidden) refreshShares(); }, 4000);
}
$("#shareDlg").addEventListener("close", () => { clearInterval(sharePoll); sharePoll = null; });
$("#shRefresh").onclick = () => refreshShares({ manual: true });
function renderShares() {
  // Keep visitor lists the owner opened or closed as they are.
  const opened = new Map([...$$("#shList .share-card")].map(c => [c.dataset.id, c.querySelector("details.sessions")?.open]));
  $("#shList").innerHTML = SH.shares.length ? SH.shares.map(sh => {
    const url = shareUrl(sh);
    const qrOpen = SH.qr[sh.id];
    const chips = [
      sh.active ? "" : `<span class="chip off">${sh.enabled ? "expired" : "paused"}</span>`,
      sh.has_pin ? `<span class="chip">${icon("lock")}PIN</span>` : `<span class="chip warn">${icon("alert")}no PIN</span>`,
      `<span class="chip">${icon(sh.mode === "room" ? "users" : "user")}${sh.mode === "room" ? "shared room" : "private conversations"}</span>`,
      `<span class="chip">${icon("eye")}${HIST[sh.history]}${sh.history === "recent" ? ` (${sh.history_recent})` : ""}</span>`,
      sh.telegram_chat_id ? `<span class="chip">${icon("send")}${esc(tgTitle(sh.telegram_chat_id))} · AI ${sh.ai_delay_secs ? `after ${sh.ai_delay_secs} s` : "on request"}</span>` : "",
      sh.expires_at ? `<span class="chip">${icon("calendar")}until ${esc(sh.expires_at.slice(0, 16).replace("T", " "))}</span>` : "",
    ].join("");
    const sessions = sh.sessions.length ? `<details class="sessions" ${sh.sessions.length <= 4 ? "open" : ""}><summary>${sh.sessions.length} visitor${sh.sessions.length > 1 ? "s" : ""}${sh.max_sessions ? ` of ${sh.max_sessions}` : ""}</summary>
      ${sh.sessions.slice().reverse().map(g => `<div class="session-row">
        ${avatarHtml(g.name || "?", null, "sm")}
        <div class="grow"><div class="t">${esc(g.name || `Visitor ${g.id.slice(0, 4)}`)}</div><div class="muted small">${g.messages} message${g.messages === 1 ? "" : "s"} · ${g.replies} answer${g.replies === 1 ? "" : "s"} · ${esc(fmtWhen(g.last_seen || g.created_at))}</div></div>
        <a class="btn sm" href="#/chat/${esc(g.chat_id)}" data-open-session>${icon("eye")}<span>Read</span></a>
        <button class="btn sm danger" data-end="${esc(g.id)}" data-share="${esc(sh.id)}" title="End this conversation and delete it" aria-label="End conversation">${icon("trash")}</button>
      </div>`).join("")}</details>` : `<div class="muted small">No visitors yet.</div>`;
    return `<div class="share-card ${sh.active ? "" : "inactive"}" data-id="${esc(sh.id)}">
      <div class="share-top">
        <div class="share-url"><span class="u">${esc(url.replace(/^https?:\/\//, ""))}</span><span class="muted small">as <b>${esc(sh.display_name)}</b></span></div>
        <div class="share-actions">
          <button class="btn sm" data-copy="${esc(sh.id)}" title="Copy link">${icon("copy")}<span>Copy</span></button>
          <button class="btn sm ${qrOpen ? "on" : ""}" data-qr="${esc(sh.id)}" title="QR code to open the link on a phone" aria-label="QR code" aria-expanded="${qrOpen ? "true" : "false"}">${icon("qr")}</button>
          ${navigator.share ? `<button class="btn sm" data-native="${esc(sh.id)}" title="Share…" aria-label="Share via apps">${icon("share")}</button>` : ""}
          <button class="btn sm" data-edit="${esc(sh.id)}" title="Edit link">${icon("edit")}<span>Edit</span></button>
        </div>
      </div>
      ${SH.pins[sh.id] ? `<div class="pin-once">${icon("lock")}<span>PIN <b class="mono">${esc(SH.pins[sh.id])}</b> (shown only now)</span><button class="btn sm" data-invite="${esc(sh.id)}">${icon("copy")}<span>Copy invite with PIN</span></button></div>` : ""}
      ${qrOpen ? qrPanel(sh) : ""}
      <div class="chips">${chips}</div>
      ${sessions}
    </div>`;
  }).join("") : `<div class="empty-chat share-empty">${icon("link")}<b>No links yet</b><span>Create a link so friends can talk to ${esc(S.chat?.chat.persona || "this persona")}. You decide who can enter, what they see and how many answers they get.</span></div>`;
  for (const c of $$("#shList .share-card")) {
    const d = c.querySelector("details.sessions"), was = opened.get(c.dataset.id);
    if (d && was != null) d.open = was;
  }
}
// The QR code of a link, inline in its card (no dialog over the dialog), with a switch between addresses.
function qrPanel(sh) {
  const bases = linkBases();
  const pick = bases.find(b => b.kind === SH.qr[sh.id]) || bases[0];
  const pin = SH.pins[sh.id];
  // A fresh PIN rides along in the fragment, like in the copied invite.
  const url = shareUrl(sh, pick.base) + (pin ? `#pin=${encodeURIComponent(pin)}` : "");
  const tabs = bases.length > 1 ? `<div class="seg qr-tabs" role="radiogroup" aria-label="Address">${bases.map(b => `<label><input type="radio" name="qr-${esc(sh.id)}" value="${b.kind}" data-qr-kind="${b.kind}" data-share="${esc(sh.id)}" ${b === pick ? "checked" : ""}><span>${icon(b.kind === "internet" ? "globe" : b.kind === "lan" ? "wifi" : "user")}${esc(b.label)}</span></label>`).join("")}</div>` : "";
  return `<div class="qr-panel">
    <img src="${qrSrc(url)}" width="168" height="168" alt="QR code for ${esc(hostOf(url))}">
    <div class="qr-side">${tabs}
      <div class="mono small qr-url">${esc(hostOf(shareUrl(sh, pick.base)))}</div>
      <div class="muted small">${esc(QR_NOTE[pick.kind])}${pin ? " The PIN is included." : ""}</div>
    </div>
  </div>`;
}
function renderReach() {
  const n = SH.net, local = isLocalHost();
  $("#shReach").classList.toggle("hidden", !n.available);
  if (n.available) {
    const lan = n.lan || {}, net = n.internet || {};
    $("#rLan").checked = !!lan.enabled; $("#rNet").checked = !!net.enabled;
    const lanText = $("#rLanText");
    lanText.classList.toggle("err", !!(lan.enabled && lan.error));
    lanText.textContent = lan.enabled ? (lan.error || `Phones on the same Wi‑Fi open the links at ${hostOf(lan.url || "")}`) : "Phones and computers on the same network can open the links";
    const netText = $("#rNetText");
    netText.classList.toggle("err", net.enabled && net.state === "error");
    netText.textContent = !net.enabled ? `Links work anywhere at your own address on ${net.domain || n.relay_host}, while iRoof.top runs. Visits pass through ${n.relay_host}.`
      : net.state === "connected" ? `Live at ${hostOf(net.url || "")}`
        : net.state === "error" ? (net.error || "Can't connect")
          : `Connecting to ${n.relay_host}…`;
    $("#rNameForm").classList.toggle("hidden", !net.enabled);
    $("#rNameSuffix").textContent = `.${net.domain || n.relay_host}`;
    if (document.activeElement !== $("#rName")) { $("#rName").value = net.name || ""; $("#rNameSave").disabled = true; }
  }
  $("#shLocal").classList.toggle("hidden", reachable());
  $("#shLocalText").innerHTML = n.available
    ? "For now these links only work on this computer. Turn on <b>Wi‑Fi</b> or <b>Internet</b> above so friends can open them."
    : `This page is open on <b>${esc(location.host)}</b>, so these links only work on this device. Open iRoof.top via an address your friends can reach to share it.`;
  if (!local && !n.available) $("#shLocal").classList.add("hidden");
}
async function putSharing(body, input) {
  if (input) input.disabled = true;
  try { SH.net = await api("/api/sharing", { method: "PUT", body }); }
  catch (e) { toast(e.message, { error: true }); try { SH.net = await api("/api/sharing"); } catch { } }
  finally { if (input) input.disabled = false; }
  renderReach(); if (!shareEditing()) renderShares();
}
$("#rLan").onchange = e => putSharing({ lan: e.target.checked }, e.target);
$("#rNet").onchange = e => putSharing({ internet: e.target.checked }, e.target);
$("#rName").oninput = () => {
  const v = $("#rName").value.trim().toLowerCase();
  $("#rNameSave").disabled = !v || v === (SH.net.internet?.name || "");
};
$("#rNameForm").addEventListener("submit", async e => {
  e.preventDefault();
  const name = $("#rName").value.trim().toLowerCase();
  $("#rName").blur();
  await putSharing({ name }, $("#rNameSave"));
  if (SH.net.internet?.name === name) toast(`Links now live at ${hostOf(SH.net.internet.url || "")}`);
});
function inviteText(sh, withPin) {
  const pin = SH.pins[sh.id];
  // The PIN rides in the fragment (#pin=…): one tap opens the chat, and it never reaches server logs.
  // The PIN line stays for people who type the address by hand.
  return withPin && pin
    ? `Chat with ${sh.display_name}: ${shareUrl(sh)}#pin=${encodeURIComponent(pin)}\nPIN: ${pin}`
    : `Chat with ${sh.display_name}: ${shareUrl(sh)}`;
}
$("#shList").addEventListener("click", async e => {
  const b = e.target.closest("button,a"); if (!b) return;
  const sh = SH.shares.find(x => x.id === (b.dataset.copy || b.dataset.qr || b.dataset.native || b.dataset.edit || b.dataset.invite || b.dataset.share));
  if (b.dataset.qr) { SH.qr[sh.id] = SH.qr[sh.id] ? undefined : (linkBases()[0].kind); renderShares(); }
  else if (b.dataset.copy) copyWithFeedback(b, shareUrl(sh), "Copied");
  else if (b.dataset.invite) copyWithFeedback(b, inviteText(sh, true), "Invite copied");
  else if (b.dataset.native) navigator.share({ title: sh.display_name, text: inviteText(sh, false), url: shareUrl(sh) }).catch(() => { });
  else if (b.dataset.edit) openShareEdit(sh);
  else if (b.dataset.end) {
    if (!await confirmDialog({ title: "End this conversation?", text: "The visitor's conversation is deleted and their link stops working. They can start a new one if the share link is still active.", ok: "End", danger: true })) return;
    try { const r = await api(`/api/shares/${sh.id}/sessions/${b.dataset.end}`, { method: "DELETE" }); Object.assign(sh, r); renderShares(); loadState().catch(() => { }); }
    catch (err) { toast(err.message, { error: true }); }
  } else if (b.hasAttribute("data-open-session")) $("#shareDlg").close();
});
$("#shList").addEventListener("change", e => {
  const r = e.target.closest("[data-qr-kind]"); if (!r) return;
  SH.qr[r.dataset.share] = r.dataset.qrKind; renderShares();
});
$("#shareBtn").onclick = () => openShares();
$("#shNew").onclick = () => openShareEdit(null);
const radioValue = name => $(`input[name=${name}]:checked`)?.value;
const setRadio = (name, v) => $$(`input[name=${name}]`).forEach(r => { r.checked = r.value === v; });
const MODE_NOTE = {
  private: "Everyone starts fresh and sees only their own messages. Best for “does it sound like me?”.",
  room: "All visitors write into the same conversation and see each other's messages.",
};
const HIST_NOTE = { none: "Visitors only see the new conversation.", full: "Visitors can scroll through the whole export." };
function syncShareForm() {
  const mode = radioValue("seMode"), hist = radioValue("seHistory");
  $("#seModeNote").textContent = MODE_NOTE[mode];
  $("#seHistNote").textContent = HIST_NOTE[hist] || "";
  $("#seHistNote").classList.toggle("hidden", hist === "recent");
  $("#seRecentRow").classList.toggle("hidden", hist !== "recent");
  $("label[for=seMaxReplies]").textContent = mode === "room" ? "Answers in total" : "Answers each";
  $("#sePin").disabled = $("#sePinClear").checked;
  $("#seAiDelay").disabled = !$("#seTg").value;
  renderShareSummary();
}
function renderShareSummary() {
  const sh = SH.editing, mode = radioValue("seMode"), hist = radioValue("seHistory");
  const pinNow = $("#sePin").value.trim();
  const hasPin = $("#sePinClear").checked ? false : !!pinNow || !!sh?.has_pin;
  const maxS = Math.max(0, Number($("#seMaxSessions").value) || 0), maxR = Math.max(0, Number($("#seMaxReplies").value) || 0);
  const exp = $("#seExpires").value, n = Number($("#seRecent").value) || 30;
  const visitors = maxS ? `Up to ${maxS.toLocaleString()} visitor${maxS > 1 ? "s" : ""}` : "Unlimited visitors";
  const cap = mode === "room" ? maxR : maxS && maxR ? maxS * maxR : 0;
  const answers = mode === "room" ? (maxR ? `${maxR.toLocaleString()} answers in the room` : "unlimited answers") : (maxR ? `${maxR.toLocaleString()} answers each` : "unlimited answers each");
  const items = [
    hasPin ? ["lock", `Visitors need the PIN to enter${sh?.has_pin && !pinNow ? " (unchanged)" : ""}`] : ["alert", "Anyone who has the link can start chatting (no PIN)", "warn"],
    mode === "room" ? ["users", "One shared conversation for everyone"] : ["user", "A private conversation per visitor, and you can read them all"],
    ["eye", hist === "none" ? "The export stays hidden" : hist === "recent" ? `The last ${n.toLocaleString()} messages of the export are visible` : "The whole export is visible"],
    [cap ? "chat" : "alert", `${visitors}, ${answers}${cap ? ` · at most ${cap.toLocaleString()} paid answers` : " · no cap on your API costs"}`, cap ? "" : "warn"],
    ...($("#seTg").value ? [["send", `Messages go to «${tgTitle(Number($("#seTg").value))}»: you answer there, or the AI ${Number($("#seAiDelay").value) > 0 ? `answers after ${Number($("#seAiDelay").value)} s` : "answers when you press AI reply"}`]] : []),
    ["calendar", exp ? `Expires ${new Date(exp).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })}` : "Never expires"],
    ...($("#seEnabled").checked ? [] : [["clock", "Paused: visitors see “not available” until you turn it on", "warn"]]),
    ["info", "The persona still knows the whole export and your notes, but visitors never see them.", "dim"],
  ];
  $("#seSummary").innerHTML = items.map(([ic, text, cls = ""]) => `<li class="${cls}">${icon(ic)}<span>${esc(text)}</span></li>`).join("");
}
$("#shareEditForm").addEventListener("input", renderShareSummary);
$("#shareEditForm").addEventListener("change", syncShareForm);
$("#sePinGen").onclick = () => {
  const a = new Uint32Array(1); crypto.getRandomValues(a);
  $("#sePin").value = String(a[0] % 1000000).padStart(6, "0"); $("#sePinClear").checked = false; syncShareForm();
};
function openShareEdit(sh) {
  SH.editing = sh;
  $("#seH").textContent = sh ? `Edit /s/${sh.slug}` : "New link";
  $("#seSave").textContent = sh ? "Save" : "Create link";
  $("#seDelete").classList.toggle("hidden", !sh);
  $("#seOrigin").textContent = `${hostOf(linkBases()[0].base)}/s/`;
  $("#seSlug").value = sh?.slug || "";
  $("#seName").value = sh?.display_name || S.chat.chat.persona;
  $("#sePin").value = ""; $("#sePin").placeholder = sh?.has_pin ? "set · type to change" : "none";
  $("#sePinClear").checked = false; $("#sePinClearRow").classList.toggle("hidden", !sh?.has_pin);
  $("#seGreeting").value = sh?.greeting || "";
  setRadio("seMode", sh?.mode || "private");
  setRadio("seHistory", sh?.history || "none"); $("#seRecent").value = sh?.history_recent || 30;
  $("#seMaxSessions").value = sh ? sh.max_sessions : 50; $("#seMaxReplies").value = sh ? sh.max_replies : 200;
  $("#seExpires").value = sh?.expires_at ? sh.expires_at.slice(0, 16) : "";
  $("#seEnabled").checked = sh ? sh.enabled : true;
  const groups = [...SH.tg.groups];
  if (sh?.telegram_chat_id && !groups.some(g => g.id === sh.telegram_chat_id)) groups.push({ id: sh.telegram_chat_id, title: `${sh.telegram_chat_id} (bot not in group)` });
  $("#seTgRow").classList.toggle("hidden", !SH.tg.enabled && !sh?.telegram_chat_id);
  $("#seTg").innerHTML = `<option value="">Off: the AI answers directly</option>` + groups.map(g => `<option value="${g.id}">${esc(g.title)}</option>`).join("");
  $("#seTg").value = sh?.telegram_chat_id ? String(sh.telegram_chat_id) : "";
  $("#seAiDelay").value = sh ? sh.ai_delay_secs ?? 10 : 10;
  syncShareForm();
  sharePage(true);
  if (!$("#shareDlg").open) $("#shareDlg").showModal();
  if (matchMedia("(pointer: fine)").matches) $("#seSlug").focus();
}
// Cancel/Escape in the editor: back to the list, or close when there is nothing to go back to.
function leaveShareEdit() { if (SH.shares.length) sharePage(false); else $("#shareDlg").close(); }
$("#seBack").onclick = () => sharePage(false);
$("#seCancel").onclick = leaveShareEdit;
$("#shareDlg").addEventListener("cancel", e => { if (shareEditing() && SH.shares.length) { e.preventDefault(); sharePage(false); } });
$("#shareEditForm").addEventListener("submit", async e => {
  e.preventDefault();
  const sh = SH.editing;
  const mode = radioValue("seMode");
  const body = {
    slug: $("#seSlug").value.trim() || undefined, display_name: $("#seName").value.trim(), greeting: $("#seGreeting").value,
    mode, history: radioValue("seHistory"), history_recent: Number($("#seRecent").value) || 30,
    max_sessions: Number($("#seMaxSessions").value) || 0, max_replies: Number($("#seMaxReplies").value) || 0,
    expires_at: $("#seExpires").value || null, enabled: $("#seEnabled").checked,
  };
  if (!$("#seTgRow").classList.contains("hidden")) {
    body.telegram_chat_id = $("#seTg").value ? Number($("#seTg").value) : null;
    body.ai_delay_secs = Math.max(0, Math.min(3600, Number($("#seAiDelay").value) || 0));
  }
  const pin = $("#sePin").value.trim();
  if ($("#sePinClear").checked) body.pin = ""; else if (pin) body.pin = pin;
  if (sh && mode !== sh.mode && sh.sessions.length) {
    if (!await confirmDialog({ title: "Change the conversation mode?", text: `This ends and deletes the ${sh.sessions.length} existing visitor conversation${sh.sessions.length > 1 ? "s" : ""} of this link.`, ok: "Change and end them", danger: true })) return;
    body.revoke_sessions = true;
  }
  const btn = $("#seSave"); btn.disabled = true;
  try {
    const r = await api(sh ? `/api/shares/${sh.id}` : `/api/chats/${S.chatId}/shares`, { method: sh ? "PATCH" : "POST", body });
    if (body.pin) SH.pins[r.id] = body.pin; else if (body.pin === "") delete SH.pins[r.id];
    const i = SH.shares.findIndex(x => x.id === r.id);
    if (i >= 0) SH.shares[i] = r; else SH.shares.push(r);
    renderShares(); sharePage(false); loadState().catch(() => { });
    if (!sh) toast(await copyText(inviteText(r, false)) ? "Link created and copied" : "Link created");
    else toast("Link saved");
  } catch (err) { toast(err.message, { error: true }); }
  finally { btn.disabled = false; }
});
$("#seDelete").onclick = async () => {
  const sh = SH.editing; if (!sh) return;
  if (!await confirmDialog({ title: `Delete the link /s/${sh.slug}?`, text: `The link stops working${sh.sessions.length ? ` and ${sh.sessions.length} visitor conversation${sh.sessions.length > 1 ? "s are" : " is"} deleted` : ""}.`, ok: "Delete link", danger: true })) return;
  try {
    await api(`/api/shares/${sh.id}`, { method: "DELETE" });
    SH.shares = SH.shares.filter(x => x.id !== sh.id); delete SH.pins[sh.id];
    renderShares(); sharePage(false); loadState().catch(() => { });
  } catch (err) { toast(err.message, { error: true }); }
};

// ---------- home: the roof at night ----------
const HOME_LINES = [
  "Some people are far away.",
  "Their words stay with you.",
  "Up here, the conversation goes on.",
  "Come up to the roof.",
];
const HOME = { timer: null, run: 0 };
function storyType(run, i = 0) {
  const el = $("#homeStory");
  if (run !== HOME.run) return;
  const line = HOME_LINES[i];
  const reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;
  el.classList.remove("fade");
  if (reduce) { el.textContent = HOME_LINES[HOME_LINES.length - 1]; return; }
  let n = 0;
  const step = () => {
    if (run !== HOME.run) return;
    el.innerHTML = `${esc(line.slice(0, n))}<span class="caret" aria-hidden="true"></span>`;
    if (n++ < line.length) { HOME.timer = setTimeout(step, 42 + Math.random() * 40); return; }
    if (i === HOME_LINES.length - 1) { HOME.timer = setTimeout(() => { el.textContent = line; }, 2400); return; }
    HOME.timer = setTimeout(() => { el.classList.add("fade"); HOME.timer = setTimeout(() => storyType(run, i + 1), 650); }, 2200);
  };
  step();
}
function showHome() {
  const home = $("#home");
  HOME.run++; clearTimeout(HOME.timer);
  home.classList.remove("hidden", "leaving");
  // Restart the drawing animation.
  const scene = home.querySelector(".scene"); scene.replaceWith(scene.cloneNode(true));
  $("#homeStory").textContent = "";
  HOME.timer = setTimeout(() => storyType(HOME.run), 1700);
  document.title = "iRoof.top";
}
function leaveHome() {
  const home = $("#home");
  if (home.classList.contains("hidden") || home.classList.contains("leaving")) return;
  HOME.run++; clearTimeout(HOME.timer);
  sessionStorage.setItem("iroof:climbed", "1");
  home.classList.add("leaving");
  const done = () => { home.classList.add("hidden"); home.classList.remove("leaving"); };
  if (matchMedia("(prefers-reduced-motion: reduce)").matches) done(); else setTimeout(done, 680);
  if (matchMedia("(pointer: fine)").matches) setTimeout(() => (S.chatId ? $("#input") : $("#newChatBtn"))?.focus(), 700);
}
$("#homeGo").onclick = leaveHome;
$("#brandBtn").onclick = showHome;
document.addEventListener("keydown", e => {
  if ($("#home").classList.contains("hidden")) return;
  if (e.key === "Escape" || (e.key === "Enter" && e.target === document.body)) { e.preventDefault(); leaveHome(); }
});
// The roof greets you when the app opens (once per tab session), unless a link points somewhere specific.
if (!GUEST && !location.hash && !sessionStorage.getItem("iroof:climbed")) showHome();

// ---------- boot ----------
if (GUEST) bootGuest();
else loadState().then(route).catch(e => toast(e.message, { error: true }));

// ---------- mobile: keep the composer above the on-screen keyboard (iOS ignores interactive-widget) ----------
if (window.visualViewport) {
  const vv = visualViewport, root = document.documentElement;
  const fit = () => {
    root.style.setProperty("--app-h", `${Math.round(vv.height)}px`);
    if (vv.offsetTop) window.scrollTo(0, 0);
    if (S.pinned) { const b = $("#messages"); b.scrollTop = b.scrollHeight; }
  };
  vv.addEventListener("resize", fit); vv.addEventListener("scroll", fit);
}
