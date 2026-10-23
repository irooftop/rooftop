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
  // The box is reserved from the pixel size the server measured, before anything loads,
  // so the timeline doesn't jump while it is scrolled (see .media.sized in app.css).
  const [w, h] = Array.isArray(md.size) ? md.size : [];
  const sized = w > 0 && h > 0 ? ` sized" style="--w:${w};--h:${h}` : "";
  switch (md.kind) {
    case "sticker": {
      if (unavailable) return `<div class="sticker placeholder">${icon("sticker")}</div>`;
      if (isVid(md.file)) return `<div class="sticker"><video src="${src}" autoplay loop muted playsinline poster="${thumb}" title="${alt}"></video></div>`;
      if (/\.tgs$/i.test(md.file)) return thumb ? `<div class="sticker"><img src="${thumb}" alt="${alt}" title="${alt}"></div>` : `<div class="sticker placeholder">${icon("sticker")}</div>`;
      return `<div class="sticker"><img src="${src}" alt="${alt}" title="${alt}"></div>`;
    }
    case "gif":
      if (unavailable) return `<div class="unavail">GIF unavailable</div>`;
      return `<div class="media gif${sized}">${isVid(md.file || md.url) ? `<video src="${src}" autoplay loop muted playsinline title="${alt}"></video>` : `<img src="${src}" alt="${alt}" referrerpolicy="no-referrer">`}</div>`;
    case "photo":
      if (unavailable) return `<div class="unavail">${icon("image")}Photo not included in export</div>`;
      return `<a class="media zoomable${sized}" href="${src}" target="_blank" rel="noopener" data-lb="${m.id}" aria-label="Open photo"><img src="${src}" alt="${alt}" loading="lazy"></a>`;
    case "video":
      if (unavailable) return `<div class="unavail">${icon("film")}Video not included in export ${esc(md.status || "")}</div>`;
      return `<div class="media video${sized}"><video src="${src}" controls preload="metadata" playsinline ${thumb ? `poster="${thumb}"` : ""}></video></div>`;
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
  const withMedia = !bare && !mediaOnly && ["photo", "video", "gif"].includes(k) && m.media.available !== false && !!(m.media.file || m.media.url);
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
  const meta = `<span class="meta">${m.edited ? "edited " : ""}${fmtTime(m.date)}</span>`;
  const reactions = m.reactions?.length ? `<div class="reactions">${m.reactions.map(r => `<button class="reaction ${r.mine ? "mine" : ""}" data-react="${esc(r.emoji)}" data-id="${m.id}">${esc(r.emoji)}${r.count > 1 ? `<span class="c">${r.count}</span>` : ""}</button>`).join("")}</div>` : "";
  // Like Telegram: reactions on the left and the time on the right of one line. Stickers
  // and lone photos keep the time on the picture, with the reactions under it.
  if (reactions && !bare && !mediaOnly) inner += `<div class="foot">${reactions}${meta}</div>`;
  else inner += meta + reactions;
  const person = S.chat.export.participants.find(p => p.name === m.from);
  return `<div class="row ${mine ? "mine" : ""} ${startGroup ? "group-start" : ""} ${endGroup ? "group-end" : ""}" data-row="${m.id}">
    ${mine ? "" : profileAvatar(m.from, person?.avatar)}
    <div class="bubble ${bare ? "bare" : ""} ${mediaOnly ? "media-only" : ""} ${withMedia ? "with-media" : ""}" id="m-${m.id}">${inner}</div>
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
    ? `<div class="empty-chat">${avatarHtml(S.chat.chat.persona, S.chat.chat.persona_avatar, "")}<b>${esc(S.chat.chat.persona)}</b>${S.chat.share?.greeting ? `<span>${esc(S.chat.share.greeting)}</span>` : ""}<span>${canNudge() ? `Say hello, or tap the speech bubble above to let ${esc(S.chat.chat.persona)} write first.` : `Say hello: ${esc(S.chat.chat.persona)} will answer.`}</span></div>`
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
  // The typing bubble stays the last row through a re-render (it has its own timers).
  const typing = $("#typingRow"); typing?.remove();
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
  if (typing && TYPING.on) box.appendChild(typing);
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
// "typing…" the way people type: a few seconds of typing, a short pause, typing again, for
// as long as the persona is writing. A pause only fades the bubble (its row keeps its
// place, so the chat doesn't jump) and the header goes back to its usual line.
// `pause`: start with one, e.g. right after a bubble arrived.
const TYPING = { on: false, timer: null };
const between = (a, b) => a + Math.random() * (b - a);
function setTyping(on, { pause = 0 } = {}) {
  clearTimeout(TYPING.timer);
  TYPING.on = on;
  $("#typingRow")?.remove();
  if (!on) return typingVisible(false);
  const box = $("#messages");
  const p = S.chat.export.participants.find(p => p.name === S.chat.chat.persona);
  box.insertAdjacentHTML("beforeend", `<div class="row typing-row group-start group-end" id="typingRow">${profileAvatar(S.chat.chat.persona, p?.avatar)}<div class="bubble" role="status" aria-label="${esc(S.chat.chat.persona)} is typing"><span class="dots"><i></i><i></i><i></i></span></div></div>`);
  box.scrollTop = box.scrollHeight;
  const burst = () => { typingVisible(true); TYPING.timer = setTimeout(rest, between(1800, 4500)); };
  const rest = (ms = between(500, 1500)) => { typingVisible(false); TYPING.timer = setTimeout(burst, ms); };
  if (pause > 0) rest(pause); else burst();
}
function typingVisible(on) {
  const sub = $("#chatSub"), row = $("#typingRow");
  row?.classList.toggle("paused", !on);
  if (on) { sub.textContent = "typing…"; sub.classList.add("typing"); }
  else { sub.classList.remove("typing"); sub.textContent = subtitle(); }
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
  if (ex.blank) return `AI as ${c.persona}, from your notes · you are ${c.user}`;
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
    // From notes: no export, so no media library to look at.
    $("#mediaBtn").classList.toggle("hidden", !!data.export.blank);
    // Visitors get "write first" and "rewrite" only where the link allows them; with neither,
    // the header keeps just the persona (the owner's other actions are hidden for visitors).
    $("#nudgeBtn").classList.toggle("hidden", !canNudge());
    $("#regenBtn").classList.toggle("hidden", !canRegenerate());
    $("#chatView .head-actions").classList.toggle("hidden", !!GUEST && !canNudge() && !canRegenerate());
    if (GUEST) document.title = `${data.chat.title} · iRoof.top`;
    $("#chatName").textContent = data.chat.title;
    $("#chatSub").textContent = subtitle();
    renderMessages({ keepScroll });
    renderSidebar();
    bootDone();
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

