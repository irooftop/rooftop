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
// `check`: an extra checkbox (e.g. "Also block…"); then it resolves to `{ checked }` or false.
function confirmDialog({ title, text = "", ok = "OK", danger = false, icon: ic = danger ? "alert" : null, remember = null, check = null }) {
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
  $("#cfCheckRow").classList.toggle("hidden", !check);
  $("#cfCheck").checked = false;
  if (check) $("#cfCheckText").textContent = check;
  dlg.returnValue = "";
  hideTip();
  dlg.showModal();
  (danger ? $("#cfCancel") : $("#cfOk")).focus(); // destructive actions focus the safe choice
  return new Promise(res => dlg.addEventListener("close", () => {
    const yes = dlg.returnValue === "ok";
    if (yes && remember && $("#cfRemember").checked) localStorage.setItem(skipKey(remember), "1");
    res(yes && check ? { checked: $("#cfCheck").checked } : yes);
  }, { once: true }));
}
// Light-dismiss for dialogs: a click on the backdrop closes them, but only when the press also started there
// (selecting text and releasing over the backdrop doesn't count), and never over unsaved edits: then the dialog
// gives a short nudge instead, and Cancel, Esc or the close button still discard them.
let pressedDialog = null;
document.addEventListener("pointerdown", e => { pressedDialog = e.target instanceof HTMLDialogElement ? e.target : null; }, true);
document.addEventListener("click", e => {
  const d = e.target;
  if (!(d instanceof HTMLDialogElement) || !d.open || pressedDialog !== d) return;
  if (!d._dirty) return d.close("cancel");
  d.classList.remove("nudge"); void d.offsetWidth; d.classList.add("nudge");
});
document.addEventListener("animationend", e => { if (e.animationName === "dlg-nudge") e.target.closest("dialog")?.classList.remove("nudge"); });
// Typing into a form of a dialog counts as an edit (search boxes and the sharing switches apply right away).
document.addEventListener("input", e => {
  const t = e.target; if (!t.closest?.("dialog form") || t.type === "search" || t.closest("[data-reach]")) return;
  t.closest("dialog")._dirty = true;
}, true);
document.addEventListener("close", e => { if (e.target instanceof HTMLDialogElement) e.target._dirty = false; }, true);
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

