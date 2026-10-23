// ---------- new chat ----------
function openNewChat(exportId, { source } = {}) {
  const exports = imported();
  // Without an import, "From an export" has nothing to offer: start from notes.
  setNewChatSource(source || (exports.length ? "export" : "notes"));
  $("#ncName").value = ""; $("#ncYou").value = "";
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
  sel.onchange = renderPersonas; if (exports.length) renderPersonas(); else $("#ncPersona").innerHTML = "";
  $("#ncTitle").value = ""; $("#ncNotes").value = "";
  $("#newChatDlg").showModal();
  if (radioValue("ncSource") === "notes") $("#ncName").focus();
}
function setNewChatSource(src) {
  const notes = src === "notes", none = !imported().length;
  setRadio("ncSource", src);
  $$("#newChatDlg .nc-pane").forEach(p => { p.hidden = p.dataset.pane !== src; });
  $("#ncExport").required = !notes; $("#ncName").required = notes;
  // Without imports the export side is a pointer to the import dialog.
  $("#newChatDlg .nc-src input[value=export]").closest("label").title = none ? "Import a Telegram or WhatsApp chat first" : "";
  $("#ncNotesHint").textContent = notes ? "who they are, how they write" : "optional, helps the AI";
  $("#ncNotes").rows = notes ? 6 : 3;
  $("#ncNotes").placeholder = notes
    ? "e.g. my grandma Rosa, 82, lives in Naples. Calls me “tesoro”, writes short messages in Italian with lots of ❤️, asks if I ate, loves her garden and the radio"
    : "e.g. my grandma, lives in Kyiv, loves gardening, calls me “sunshine”";
}
$$("input[name=ncSource]").forEach(r => r.onchange = () => {
  if (r.value === "export" && !imported().length) { $("#newChatDlg").close(); openImport(); return; }
  setNewChatSource(r.value);
  if (r.value === "notes") $("#ncName").focus();
});
$("#fromNotesBtn").onclick = () => openNewChat(null, { source: "notes" });
$("#newChatBtn").onclick = () => openNewChat();
$("#newChatForm").addEventListener("submit", async e => {
  if (e.submitter?.value !== "ok") return;
  e.preventDefault();
  const fromNotes = radioValue("ncSource") === "notes";
  const persona = fromNotes ? $("#ncName").value.trim() : $("input[name=persona]:checked")?.value;
  if (!persona) { if (fromNotes) $("#ncName").focus(); return; }
  const body = fromNotes
    ? { persona, user: $("#ncYou").value.trim() || undefined, notes: $("#ncNotes").value }
    : { export_id: $("#ncExport").value, persona, title: $("#ncTitle").value, notes: $("#ncNotes").value };
  try {
    const c = await api("/api/chats", { method: "POST", body });
    $("#newChatDlg").close();
    await loadState();
    location.hash = `#/chat/${c.id}`;
  } catch (err) { toast(err.message, { error: true }); }
});

