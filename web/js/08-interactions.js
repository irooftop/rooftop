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
    // A reaction to their message may get an answer, as in a real chat.
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
        // A bubble arrives, then a breath before they type the next one.
        setTyping(false); upsertMessages([v]); renderMessages(); setTyping(true, { pause: between(350, 900) });
      } else {
        await sleep(450);
        const v = views.get(a.target); if (v) { upsertMessages([v]); renderMessages({ keepScroll: true }); setTyping(true); $(`#m-${a.target} .reaction:not(.mine)`)?.classList.add("pop"); }
      }
    }
    if (GUEST && S.chat.share && S.chat.share.replies_left != null) { S.chat.share.replies_left = Math.max(0, S.chat.share.replies_left - 1); }
    if (r.error) errorToast(r.error, () => respond());
    else if (!r.actions.length) toast(`${S.chat.chat.persona} didn't answer this time.`, canNudge() ? { action: () => respond(), actionLabel: "Nudge" } : {});
  } catch (e) {
    errorToast(e.message, () => respond(path));
  } finally {
    S.busy = false;
    if (S.chatId === chatId) { setTyping(false); loadState().catch(() => { }); }
    // More messages came in meanwhile; for visitors only if something is still unanswered.
    if (S.pending) { S.pending = false; if (canNudge() || awaitingAnswer()) respond(); }
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
// What a visitor may do besides sending messages (the owner may do everything).
const canNudge = () => !GUEST || !!(S.chat?.share?.nudge && !S.chat.share.relay);
const canRegenerate = () => !GUEST || !!(S.chat?.share?.regenerate && !S.chat.share.relay);
// Whether the persona has something to answer: a message after their last one, or a
// reaction of ours on it (the server checks the same).
const awaitingAnswer = () => {
  const last = [...(S.chat?.messages || [])].reverse().find(m => !m.service);
  return !!last && (!last.persona || !!last.reactions?.some(r => r.mine));
};
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
    $("#promptInfo").innerHTML = promptInfo(lastPrompt);
    // Big prompts are cheap to copy or download but slow to lay out: show only the beginning.
    const json = JSON.stringify({ model: lastPrompt.model, messages: lastPrompt.messages, tools: lastPrompt.tools }, null, 2);
    lastPrompt.json = json;
    const MAX = 200_000;
    $("#promptPre").textContent = json.length > MAX ? `${json.slice(0, MAX)}\n\n… ${Math.round((json.length - MAX) / 1024)} KB more. Use Copy or Download for the full JSON.` : json;
    $("#promptDlg").showModal();
  } catch (e) { toast(e.message, { error: true }); }
};
// What is sent and why: the history is cut by size (Settings → History budget, in characters),
// not by a message count, because the model's context and the price are counted in tokens.
function promptInfo(p) {
  const chars = p.messages.reduce((n, m) => n + JSON.stringify(m).length, 0);
  const tokens = n => `≈${fmtNum(Math.round(n / 3.5 / 1000))}k tokens`;
  const kept = p.total_turns - p.dropped_turns;
  let s = `${fmtNum(p.messages.length)} messages, ${p.tools.length} tools, model ${esc(p.model)} · ${fmtNum(chars)} chars (${tokens(chars)})`;
  if (p.history_budget_chars) s += `<br>History: ${p.dropped_turns ? `the latest ${fmtNum(kept)} of ${fmtNum(p.total_turns)} turns` : `all ${fmtNum(p.total_turns)} turns`} fit the history budget of ${fmtNum(p.history_budget_chars)} chars (${tokens(p.history_budget_chars)}).`;
  if (p.dropped_turns) s += ` Older turns are ${p.memory?.summary ? "summarized in the memory" : "not replayed"}; the persona finds exact old messages with <code>search_history</code>.`;
  if (p.pending_compaction) s += " The next reply first folds the oldest kept turns into the memory.";
  return s;
}
$("#promptBudget").onclick = async () => {
  $("#promptDlg").close();
  await $("#settingsBtn").onclick();
  const b = $("#sBudget"); b.focus(); b.select?.();
};
$("#promptCopy").onclick = async () => toast(await copyText(lastPrompt?.json || "") ? "Copied" : "Copy failed", {});
$("#promptDownload").onclick = () => {
  const a = document.createElement("a");
  const url = URL.createObjectURL(new Blob([lastPrompt?.json || ""], { type: "application/json" }));
  a.href = url; a.download = `${S.chat.chat.title.replace(/[^\w.-]+/g, "_")}.messages.json`; a.click();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
};

