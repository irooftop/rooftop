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

