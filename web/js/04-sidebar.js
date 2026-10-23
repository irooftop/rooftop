// ---------- sidebar ----------
async function loadState() {
  if (GUEST) return null;
  S.state = await api("/api/state");
  OFFLINE = !!S.state.offline?.on; renderOfflineBadge();
  renderSidebar();
  return S.state;
}
function exportById(id) { return S.state?.exports.find(e => e.id === id); }
// Real imports; chats started from notes have a blank export of their own, which isn't listed.
const imported = () => (S.state?.exports || []).filter(e => !e.blank);
function renderSidebar() {
  if (!S.state) return;
  renderSetupNote();
  const list = $("#chatList");
  const { chats } = S.state, exports = imported();
  if (!chats.length) list.innerHTML = `<div class="empty-list">${exports.length ? "No chats yet. Start one with <b>New chat</b>." : "Import a Telegram or WhatsApp chat, or start one from notes with <b>New chat</b>."}</div>`;
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

