// ---------- visitor mode (/s/<slug>) ----------
const tokenKey = () => `iroof:s:${GUEST.slug}`;
function forgetGuestToken() { if (localStorage.getItem(tokenKey()) === GUEST.token) localStorage.removeItem(tokenKey()); GUEST.token = null; }
function gateError(msg) { const e = $("#gateErr"); e.textContent = msg || ""; e.classList.toggle("hidden", !msg); }
async function showGate(error) {
  stopRoomPoll();
  $("#chatView").classList.add("hidden"); $("#welcome").classList.add("hidden"); $("#app").classList.remove("in-chat");
  history.replaceState(null, "", `/s/${enc(GUEST.slug)}`);
  // Still loading (a saved conversation that's gone): the skeleton turns into the start page's.
  if (document.documentElement.classList.contains("booting")) document.documentElement.classList.replace("boot-chat", "boot-gate");
  try {
    GUEST.info = await api(guestBase());
  } catch (e) {
    bootDone();
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
  bootDone();
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
      if (typing) setTyping(true, { pause: between(350, 900) });
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
// The skeleton the server sent with the page (see .booting in app.css) gives way to the real view.
function bootDone() { document.documentElement.classList.remove("booting", "boot-gate", "boot-chat"); }
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

