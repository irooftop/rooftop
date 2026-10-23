// ---------- share links (owner) ----------
const SH = { shares: [], pins: {}, editing: null, tg: { enabled: false, groups: [] }, net: { available: false }, rename: null };
const tgTitle = id => SH.tg.groups.find(g => g.id === id)?.title || "Telegram group";
const isLocalHost = () => ["localhost", "127.0.0.1", "[::1]", "::1"].includes(location.hostname) || location.hostname.endsWith(".localhost");
// Addresses where the links work, best first: the internet relay, the local network, this page's own origin.
function linkBases() {
  const n = SH.net, out = [];
  if (n.available && n.internet?.enabled && n.internet.url) out.push({ kind: "internet", label: "Internet", base: n.internet.url });
  if (n.available && n.lan?.enabled && n.lan.url) out.push({ kind: "lan", label: "Wi‑Fi", base: n.lan.url });
  out.push({ kind: "here", label: isLocalHost() ? "Computer" : location.host, base: location.origin });
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
  $("#shareDlg")._dirty = false; // a fresh page of the dialog: nothing typed yet
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
        <div class="grow"><div class="t">${esc(g.name || `Visitor ${g.id.slice(0, 4)}`)}</div><div class="muted small">${g.messages} message${g.messages === 1 ? "" : "s"} · ${g.replies} answer${g.replies === 1 ? "" : "s"} · ${esc(fmtWhen(g.last_seen || g.created_at))}${g.ip ? ` · <span class="mono ip" title="The address they last wrote from">${esc(g.ip)}</span>` : ""}</div></div>
        <a class="btn sm" href="#/chat/${esc(g.chat_id)}" data-open-session>${icon("eye")}<span>Read</span></a>
        <button class="btn sm danger" data-end="${esc(g.id)}" data-share="${esc(sh.id)}" title="End this conversation and delete it" aria-label="End conversation">${icon("trash")}</button>
      </div>`).join("")}</details>` : `<div class="muted small">No visitors yet.</div>`;
    const blocked = sh.blocked?.length ? `<details class="sessions blocked"><summary>${sh.blocked.length} blocked address${sh.blocked.length > 1 ? "es" : ""}</summary>
      ${sh.blocked.map(x => `<div class="session-row">
        <span class="se-badge sm">${icon("lock")}</span>
        <div class="grow"><div class="t mono">${esc(x.ip)}</div><div class="muted small">${x.name ? `${esc(x.name)} · ` : ""}blocked ${esc(fmtWhen(x.at))}</div></div>
        <button class="btn sm" data-unblock="${esc(x.ip)}" data-share="${esc(sh.id)}">Unblock</button>
      </div>`).join("")}</details>` : "";
    return `<div class="share-card ${sh.active ? "" : "inactive"}" data-id="${esc(sh.id)}">
      <div class="share-top">
        <div class="share-url"><span class="u">${esc(url.replace(/^https?:\/\//, ""))}</span><span class="muted small">as <b>${esc(sh.display_name)}</b></span></div>
        <div class="share-actions">
          <button class="btn sm" data-copy="${esc(sh.id)}" title="Copy link">${icon("copy")}<span>Copy</span></button>
          <button class="btn sm" data-qr="${esc(sh.id)}" title="QR code to open the link on a phone" aria-label="QR code to open the link on a phone" aria-haspopup="dialog">${icon("qr")}</button>
          ${navigator.share ? `<button class="btn sm" data-native="${esc(sh.id)}" title="Share…" aria-label="Share via apps">${icon("share")}</button>` : ""}
          <button class="btn sm" data-edit="${esc(sh.id)}" title="Edit link">${icon("edit")}<span>Edit</span></button>
          <button class="btn sm danger" data-del="${esc(sh.id)}" title="Delete link" aria-label="Delete link /s/${esc(sh.slug)}">${icon("trash")}</button>
        </div>
      </div>
      ${SH.pins[sh.id] ? `<div class="pin-once">${icon("lock")}<span>PIN <b class="mono">${esc(SH.pins[sh.id])}</b> (shown only now)</span><button class="btn sm" data-invite="${esc(sh.id)}">${icon("copy")}<span>Copy invite with PIN</span></button></div>` : ""}
      <div class="chips">${chips}</div>
      ${sessions}${blocked}
    </div>`;
  }).join("") + shareTip() : `<div class="share-empty"><span class="se-badge">${icon("link")}</span><b>No links yet</b><span>Create a link so friends can talk to ${esc(S.chat?.chat.persona || "this persona")}. You decide who can enter, what they see and how many answers they get.</span></div>`;
  for (const c of $$("#shList .share-card")) {
    const d = c.querySelector("details.sessions"), was = opened.get(c.dataset.id);
    if (d && was != null) d.open = was;
  }
}
// Presets in the fragment let a friend skip the start page: /s/name#name=Mia&pin=1234.
function shareTip() {
  const sh = SH.shares[0], pin = SH.shares.some(x => x.has_pin);
  const example = `${hostOf(shareUrl(sh))}#name=Mia${pin ? "&pin=1234" : ""}`;
  return `<p class="share-tip">${icon("sparkle")}<span>Nothing to type for your friend: add <code>#name=Mia</code>${pin ? " and <code>&amp;pin=…</code>" : ""} to a link, e.g. <code class="mono">${esc(example)}</code>, and they go straight into the chat. The part after <code>#</code> never reaches the server.</span></p>`;
}
// The QR code of a link, in its own small dialog over the list, with a switch between addresses.
const QR = { sh: null, kind: null, url: "" };
function openQr(sh) {
  QR.sh = sh; QR.kind = linkBases()[0].kind;
  renderQr();
  $("#qrDlg").showModal();
}
function renderQr() {
  const sh = QR.sh; if (!sh) return;
  const bases = linkBases();
  const pick = bases.find(b => b.kind === QR.kind) || bases[0];
  QR.kind = pick.kind;
  const pin = SH.pins[sh.id];
  const plain = shareUrl(sh, pick.base);
  // A fresh PIN rides along in the fragment, like in the copied invite.
  QR.url = plain + (pin ? `#pin=${encodeURIComponent(pin)}` : "");
  $("#qrWho").innerHTML = `Chat with <b>${esc(sh.display_name)}</b>${sh.active ? "" : ` · <span class="err">${sh.enabled ? "expired" : "paused"}</span>`}`;
  $("#qrTabs").classList.toggle("hidden", bases.length < 2);
  $("#qrTabs").innerHTML = bases.map(b => `<label><input type="radio" name="qrKind" value="${b.kind}" ${b === pick ? "checked" : ""}><span>${icon(b.kind === "internet" ? "globe" : b.kind === "lan" ? "wifi" : "user")}${esc(b.label)}</span></label>`).join("");
  const img = $("#qrImg"), src = qrSrc(QR.url);
  if (img.getAttribute("src") !== src) img.src = src;
  img.alt = `QR code for ${hostOf(plain)}`;
  $("#qrUrl").textContent = hostOf(plain);
  const warn = pick.kind === "here" && isLocalHost();
  $("#qrNote").className = `qr-note${warn ? " warn" : ""}`;
  $("#qrNote").innerHTML = `${icon(warn ? "alert" : pick.kind === "internet" ? "globe" : pick.kind === "lan" ? "wifi" : "info")}<span>${esc(QR_NOTE[pick.kind])}${pin ? " The PIN is included, so the phone opens the chat right away." : ""}</span>`;
  $("#qrOpen").href = QR.url;
}
$("#qrTabs").addEventListener("change", e => { QR.kind = e.target.value; renderQr(); });
$("#qrCopy").onclick = e => copyWithFeedback(e.currentTarget, QR.url, "Copied");
$("#qrDlg").addEventListener("close", () => { QR.sh = null; });

// Where the links open: Wi-Fi and Internet switches with a clear state (off, connecting, live, error).
// The same panel sits above the list and inside the link editor, so a new link gets the right address
// before it is created. Both copies render from SH.net.
function reachHtml() {
  const n = SH.net, lan = n.lan || {}, net = n.internet || {}, relay = esc(n.relay_host || "iroof.top");
  const lanState = !lan.enabled ? "off" : lan.error ? "error" : lan.url ? "live" : "connecting";
  const netState = !net.enabled ? "off" : net.state === "connected" ? "live" : net.state === "error" ? "error" : "connecting";
  const PILL = { live: ["ok", "Live"], connecting: ["wait", "Connecting"], error: ["err", "Not working"] };
  const pill = st => st === "off" ? "" : `<span class="pill ${PILL[st][0]}"><span class="dot"></span>${PILL[st][1]}</span>`;
  const row = (kind, ic, title, st, desc, extra = "") => `<div class="reach-item" data-state="${st}">
    <label class="switch-row reach-row"><span class="reach-ic">${icon(ic)}</span>
      <span class="sw-text"><span class="reach-title"><b>${title}</b>${pill(st)}</span><span class="muted small${st === "error" ? " err" : ""}">${desc}</span></span>
      <input type="checkbox" class="switch" data-reach-toggle="${kind}" ${st === "off" ? "" : "checked"} aria-label="${title}">
    </label>${extra}</div>`;
  const lanDesc = { off: "Phones and computers on the same network", live: "Phones on the same Wi‑Fi can open the links", connecting: "Starting…", error: esc(lan.error || "") }[lanState];
  const netDesc = {
    off: `Anyone, anywhere, at your own address on ${esc(net.domain || n.relay_host || "iroof.top")}`,
    live: `Anyone with a link can open it while iRoof.top runs here. Visits pass through ${relay}.`,
    connecting: `Connecting to ${relay}…`,
    error: esc(net.error || "Can't connect"),
  }[netState];
  const offline = !!n.offline;
  const lanAddr = lanState === "live" ? `<div class="reach-addr"><span class="addr mono">${esc(hostOf(lan.url))}</span></div>` : "";
  let netAddr = "";
  if (net.enabled) {
    const suffix = `.${esc(net.domain || n.relay_host || "iroof.top")}`;
    netAddr = SH.rename
      ? `<div class="reach-addr editing"><div class="slug-row"><input class="reach-name-input" value="${esc(SH.rename.draft)}" maxlength="32" autocomplete="off" autocapitalize="off" spellcheck="false" aria-label="Your address"><span class="slug-prefix suffix">${suffix}</span></div>
          <button type="button" class="btn sm primary" data-reach-save ${reachNameOk(SH.rename.draft) ? "" : "disabled"}>Save</button><button type="button" class="btn sm" data-reach-cancel>Cancel</button></div>
          <div class="reach-hint muted small">3–32 lowercase letters, digits or hyphens. Links you already sent stop working at the old address.</div>`
      : net.url ? `<div class="reach-addr"><span class="addr mono${netState === "live" ? "" : " dim"}">${esc(hostOf(net.url))}</span><button type="button" class="btn sm ghost" data-reach-rename title="Change your address">${icon("edit")}<span>Change</span></button></div>` : "";
  }
  const netRow = offline
    ? row("internet", "globe", "Internet", "off", "Offline mode is on: nothing is shared over the internet. Turn it off in Settings.", "").replace('data-reach-toggle="internet"', 'data-reach-toggle="internet" disabled')
    : row("internet", "globe", "Internet", netState, netDesc, netAddr);
  return row("lan", "wifi", "Wi‑Fi and local network", lanState, lanDesc, lanAddr) + netRow;
}
const reachNameOk = v => /^[a-z0-9](?:[a-z0-9-]{1,30}[a-z0-9])$/.test(v) && v !== (SH.net.internet?.name || "");
function renderReach() {
  const n = SH.net;
  $("#shReach").classList.toggle("hidden", !n.available);
  $("#seReachField").classList.toggle("hidden", !n.available);
  if (n.available) {
    const html = reachHtml();
    for (const box of $$("[data-reach]")) {
      // Don't pull the address field away from under the cursor while the owner types.
      if (box.contains(document.activeElement) && document.activeElement.matches(".reach-name-input")) continue;
      if (box._html !== html) { box.innerHTML = html; box._html = html; }
    }
  }
  $("#shLocal").classList.toggle("hidden", reachable() || (!isLocalHost() && !n.available));
  $("#shLocalText").innerHTML = n.available
    ? "For now these links only work on this computer. Turn on <b>Wi‑Fi</b> or <b>Internet</b> above so friends can open them."
    : `This page is open on <b>${esc(location.host)}</b>, so these links only work on this device. Open iRoof.top via an address your friends can reach to share it.`;
  updateLinkPreview();
  if ($("#qrDlg").open) renderQr();
}
// The editor shows the address the new link will really have, and who can open it.
function updateLinkPreview() {
  if (!shareEditing()) return;
  const n = SH.net, best = linkBases()[0], net = n.internet || {};
  $("#seOrigin").textContent = `${hostOf(best.base)}/s/`;
  $("#seOrigin").title = `${best.base}/s/`;
  const where = $("#seWhere");
  let cls = "ok", ic = "check", text;
  if (best.kind === "internet") {
    text = net.state === "connected" ? "Works anywhere" : net.state === "error" ? "Internet sharing has a problem, see above" : `Works anywhere once connected to ${n.relay_host || "iroof.top"}`;
    if (net.state !== "connected") { cls = net.state === "error" ? "warn" : "wait"; ic = net.state === "error" ? "alert" : "clock"; }
    if (n.lan?.enabled && n.lan.url) text += ` · on the same Wi‑Fi also at ${hostOf(n.lan.url)}`;
  } else if (best.kind === "lan") text = "Works for phones and computers on the same Wi‑Fi";
  else if (isLocalHost()) { cls = "warn"; ic = "alert"; text = n.available ? "Only works on this computer: turn on Wi‑Fi or Internet above" : "Only works on this computer"; }
  else text = `Works for everyone who can reach ${location.host}`;
  where.className = `field-note se-where ${cls}`;
  where.innerHTML = `${icon(ic)}<span>${esc(text)}</span>`;
}
async function putSharing(body, input) {
  if (input) input.disabled = true;
  let ok = false;
  try { SH.net = await api("/api/sharing", { method: "PUT", body }); ok = true; }
  catch (e) { toast(e.message, { error: true }); try { SH.net = await api("/api/sharing"); } catch { } }
  finally { if (input) input.disabled = false; }
  $$("[data-reach]").forEach(box => { box._html = null; }); // the switch the owner flipped may not match SH.net
  renderReach(); if (!shareEditing()) renderShares();
  if (ok && SH.net.internet?.enabled && SH.net.internet.state !== "connected") watchReach();
  return ok;
}
// Right after Internet is switched on, follow the connection closely so "Live" shows up without waiting for the slow poll.
let reachWatch = null;
function watchReach(tries = 20) {
  clearTimeout(reachWatch);
  reachWatch = setTimeout(async () => {
    if (!$("#shareDlg").open) return;
    try {
      const net = await api("/api/sharing");
      if (JSON.stringify(net) !== JSON.stringify(SH.net)) { SH.net = net; renderReach(); if (!shareEditing()) renderShares(); }
    } catch { }
    const st = SH.net.internet;
    if (tries > 1 && st?.enabled && st.state !== "connected" && st.state !== "error") watchReach(tries - 1);
  }, 800);
}
$("#shareDlg").addEventListener("change", e => {
  const t = e.target.closest("[data-reach-toggle]"); if (!t) return;
  e.stopPropagation();
  putSharing({ [t.dataset.reachToggle]: t.checked }, t);
}, true);
$("#shareDlg").addEventListener("click", e => {
  const b = e.target.closest("[data-reach-rename],[data-reach-cancel],[data-reach-save]"); if (!b) return;
  if (b.matches("[data-reach-rename]")) {
    SH.rename = { draft: SH.net.internet?.name || "" }; renderReach();
    const box = b.closest("[data-reach]") || document;
    const inp = $(".reach-name-input", box) || $(".reach-name-input"); if (inp) { inp.focus(); inp.setSelectionRange(inp.value.length, inp.value.length); }
  } else if (b.matches("[data-reach-cancel]")) { SH.rename = null; renderReach(); }
  else saveReachName(b);
});
$("#shareDlg").addEventListener("input", e => {
  const inp = e.target.closest(".reach-name-input"); if (!inp || !SH.rename) return;
  e.stopPropagation();
  SH.rename.draft = inp.value.trim().toLowerCase();
  const save = inp.closest(".reach-addr").querySelector("[data-reach-save]");
  save.disabled = !reachNameOk(SH.rename.draft);
}, true);
$("#shareDlg").addEventListener("keydown", e => {
  const inp = e.target.closest?.(".reach-name-input"); if (!inp) return;
  // Enter saves the address (and never submits the link editor around it); Escape only leaves the field.
  if (e.key === "Enter") { e.preventDefault(); saveReachName(inp.closest(".reach-addr").querySelector("[data-reach-save]")); }
  else if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); SH.rename = null; inp.blur(); renderReach(); }
});
async function saveReachName(btn) {
  const name = SH.rename?.draft || "";
  if (!reachNameOk(name)) return;
  document.activeElement?.blur();
  if (await putSharing({ name }, btn) && SH.net.internet?.name === name) {
    SH.rename = null; renderReach();
    toast(`Links now live at ${hostOf(SH.net.internet.url || "")}`);
  }
}
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
  const sh = SH.shares.find(x => x.id === (b.dataset.copy || b.dataset.qr || b.dataset.native || b.dataset.edit || b.dataset.del || b.dataset.invite || b.dataset.share));
  if (!sh && !b.hasAttribute("data-open-session")) return;
  if (b.dataset.qr) openQr(sh);
  else if (b.dataset.copy) copyWithFeedback(b, shareUrl(sh), "Copied");
  else if (b.dataset.invite) copyWithFeedback(b, inviteText(sh, true), "Invite copied");
  else if (b.dataset.native) navigator.share({ title: sh.display_name, text: inviteText(sh, false), url: shareUrl(sh) }).catch(() => { });
  else if (b.dataset.edit) openShareEdit(sh);
  else if (b.dataset.del) deleteShare(sh);
  else if (b.dataset.end) {
    const g = sh.sessions.find(x => x.id === b.dataset.end);
    const answer = await confirmDialog({
      title: `End the conversation with ${g?.name || "this visitor"}?`,
      text: "Their conversation is deleted and their link stops working. Unless you block them, they can start a new one while the share link is active.",
      ok: "End", danger: true,
      // Older visitors have no recorded address, so there is nothing to block.
      check: g?.ip ? `Also block ${g.ip}: nobody from this address can open the link again (on a shared network, that may be several people)` : null,
    });
    if (!answer) return;
    const block = answer.checked ? "?block=true" : "";
    try { const r = await api(`/api/shares/${sh.id}/sessions/${b.dataset.end}${block}`, { method: "DELETE" }); Object.assign(sh, r); renderShares(); loadState().catch(() => { }); if (block) toast(`${g.ip} blocked`); }
    catch (err) { toast(err.message, { error: true }); }
  } else if (b.dataset.unblock) {
    try { const r = await api(`/api/shares/${sh.id}`, { method: "PATCH", body: { unblock: [b.dataset.unblock] } }); Object.assign(sh, r); renderShares(); toast(`${b.dataset.unblock} unblocked`); }
    catch (err) { toast(err.message, { error: true }); }
  } else if (b.hasAttribute("data-open-session")) $("#shareDlg").close();
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
    ...($("#seNudge").checked || $("#seRegen").checked ? [["nudge", `Visitors can ${[$("#seNudge").checked && "let the persona write first", $("#seRegen").checked && "rewrite the last answer"].filter(Boolean).join(" and ")}: each uses an answer`]] : []),
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
  $("#seNudge").checked = !!sh?.allow_nudge; $("#seRegen").checked = !!sh?.allow_regenerate;
  const groups = [...SH.tg.groups];
  if (sh?.telegram_chat_id && !groups.some(g => g.id === sh.telegram_chat_id)) groups.push({ id: sh.telegram_chat_id, title: `${sh.telegram_chat_id} (bot not in group)` });
  $("#seTgRow").classList.toggle("hidden", !SH.tg.enabled && !sh?.telegram_chat_id);
  $("#seTg").innerHTML = `<option value="">Off: the AI answers directly</option>` + groups.map(g => `<option value="${g.id}">${esc(g.title)}</option>`).join("");
  $("#seTg").value = sh?.telegram_chat_id ? String(sh.telegram_chat_id) : "";
  $("#seAiDelay").value = sh ? sh.ai_delay_secs ?? 10 : 10;
  syncShareForm();
  sharePage(true);
  updateLinkPreview();
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
    allow_nudge: $("#seNudge").checked, allow_regenerate: $("#seRegen").checked,
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
$("#seDelete").onclick = () => SH.editing && deleteShare(SH.editing);
// From the list or the editor: the link stops working and its visitor conversations go.
async function deleteShare(sh) {
  if (!await confirmDialog({ title: `Delete the link /s/${sh.slug}?`, text: `The link stops working${sh.sessions.length ? ` and ${sh.sessions.length} visitor conversation${sh.sessions.length > 1 ? "s are" : " is"} deleted` : ""}.`, ok: "Delete link", danger: true })) return;
  try {
    await api(`/api/shares/${sh.id}`, { method: "DELETE" });
    SH.shares = SH.shares.filter(x => x.id !== sh.id); delete SH.pins[sh.id];
    renderShares(); sharePage(false); loadState().catch(() => { });
    toast(`Link /s/${sh.slug} deleted`);
  } catch (err) { toast(err.message, { error: true }); }
}

