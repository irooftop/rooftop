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
    renderEnvNote(s);
    $("#sMcp").value = JSON.stringify(s.mcpServers || {}, null, 2);
    renderOffline(s.offline);
    openTgSettings(s.telegram || {});
    hideTip();
    $("#mcpStatus").innerHTML = `<div class="muted small">Checking MCP servers…</div>`;
    $("#sTestResult").className = "test-result hidden";
    $("#settingsDlg").showModal();
    if (S.state?.llm?.needs_key && !s.has_api_key) $(S.state.llm.endpoint_set ? "#sKey" : "#sBase").focus();
    api("/api/mcp").then(r => renderMcpStatus(r.servers)).catch(e => { $("#mcpStatus").textContent = e.message; });
  } catch (e) { toast(e.message, { error: true }); }
};
// Values from .env (or the environment) are fallbacks: they show as the grey placeholder of an
// empty field and are never copied into the saved settings, so editing .env keeps working.
function renderEnvNote(s) {
  const env = s.env || {}, used = [];
  if (env.base_url) used.push(["OPENAI_BASE_URL", env.base_url, !s.base_url]);
  if (env.model) used.push(["OPENAI_MODEL", env.model, !s.model]);
  if (s.env_media_model) used.push(["OPENAI_MEDIA_MODEL", s.env_media_model, !s.media_model]);
  const note = $("#sEnvNote");
  note.classList.toggle("hidden", !used.length && !env.has_api_key);
  const list = used.map(([k, v, active]) => `<code>${k}</code> = ${esc(v)}${active ? "" : " <i>(overridden here)</i>"}`).join(" · ");
  const key = env.has_api_key ? "<code>OPENAI_API_KEY</code> is set." : s.has_api_key ? "" : "<code>OPENAI_API_KEY</code> is empty: paste a key above, or put it in <code>.env</code> and restart.";
  note.innerHTML = `${icon("info")}<span><b>From .env</b> (shown in grey; empty fields use them, typed values override them): ${list}${list && key ? " · " : ""}${key}</span>`;
}
// Telegram: the mode applies right away (no Save needed), and the status follows the bot.
const TG = { env: false, mode: "off", relayHost: "iroof.top", poll: null, busy: false, last: null, sig: null };
const TG_TOKEN_HINT = `Create a bot with <a href="https://t.me/BotFather" target="_blank" rel="noopener noreferrer">@BotFather</a>, paste its token above and press <b>Connect</b>.`;
// Offline mode applies right away: the model, sharing and Telegram follow it.
function renderOffline(o) {
  OFFLINE = !!o?.on; OFFLINE_FORCED = !!o?.forced;
  $("#sOffline").checked = OFFLINE;
  $("#sOffline").disabled = OFFLINE_FORCED;
  $("#sOfflineWhy").textContent = o?.forced ? "Set by --offline / ROOFTOP_OFFLINE: restart without it to turn it off." : "Only local models (this computer or network). No internet sharing, no Telegram.";
  $("#sTgModes").closest(".tg-settings").classList.toggle("is-offline", OFFLINE);
  $$("#sTgModes input").forEach(i => { i.disabled = OFFLINE || TG.env; });
  $("#sTgTokenRow").classList.toggle("hidden", OFFLINE || TG.mode !== "own" || TG.env);
  if (OFFLINE) tgNote(`<div class="tg-note">${icon("info")}<span>Offline mode: the Telegram bot is off.</span></div>`);
  else if (TG.mode === "off" && !TG.env) tgNote("");
  renderOfflineBadge();
}
function renderOfflineBadge() {
  $("#offlineBadge").classList.toggle("hidden", !OFFLINE);
  $("#sidebar .side-head").classList.toggle("offline", OFFLINE);
}
let OFFLINE = false, OFFLINE_FORCED = false;
$("#offlineBadge").onclick = () => $("#settingsBtn").click();
$("#sOffline").addEventListener("change", async () => {
  const box = $("#sOffline"), want = box.checked;
  box.disabled = true;
  try {
    await api("/api/settings", { method: "PUT", body: { offline: want } });
    const s = await api("/api/settings");
    renderOffline(s.offline);
    if (!s.offline.on) openTgSettings(s.telegram || {});
    loadState().catch(() => { });
    api("/api/mcp").then(r => renderMcpStatus(r.servers)).catch(() => { });
    const remote = s.offline.on ? s.offline.remote || [] : [];
    toast(remote.length
      ? `Offline mode is on, but ${remote.join(" and ")} ${remote.length > 1 ? "are not local endpoints" : "is not a local endpoint"}: pick a model on this computer (e.g. Ollama) below, or replies will be refused.`
      : s.offline.on ? "Offline mode is on" : "Offline mode is off", { error: remote.length > 0, ms: remote.length ? 10000 : 5000 });
  } catch (e) { box.checked = !want; toast(e.message, { error: true }); }
  finally { box.disabled = OFFLINE_FORCED; }
});
function openTgSettings(t) {
  TG.env = !!t.env; TG.mode = t.mode || "off"; TG.relayHost = t.relay_host || "iroof.top"; TG.sig = null; TG.last = null;
  $$(".tg-relay-host").forEach(e => { e.textContent = TG.relayHost; });
  setRadio("sTgMode", TG.mode);
  $$("#sTgModes input").forEach(i => { i.disabled = TG.env; });
  $("#sTgToken").value = ""; $("#sTgToken").placeholder = t.has_token ? "•••••• (saved, paste a new token to replace it)" : "Bot token from @BotFather, 123456789:AA…";
  $("#sTgTokenRow").classList.toggle("hidden", TG.mode !== "own" || TG.env);
  tgNote(TG.mode === "off" && !TG.env ? "" : tgPending("Checking the bot…"));
  refreshTg();
  clearInterval(TG.poll);
  TG.poll = setInterval(() => { if (!document.hidden && !OFFLINE && (TG.mode !== "off" || TG.env)) refreshTg(); }, 3000);
  if (OFFLINE) renderOffline({ on: true, forced: OFFLINE_FORCED });
}
$("#settingsDlg").addEventListener("close", () => { clearInterval(TG.poll); TG.poll = null; if ($("#tgAddDlg").open) $("#tgAddDlg").close(); });
const tgPending = text => `<div class="tg-note"><span class="spin" aria-hidden="true"></span><span>${text}</span></div>`;
function tgNote(html) { TG.sig = null; $("#sTgStatus").innerHTML = html; }
async function refreshTg() {
  try { renderTgStatus(await api("/api/telegram")); } catch { }
}
// How many share links (of all chats) post to a group.
const tgUses = id => (S.state?.shares || []).filter(x => x.telegram_chat_id === id).length;
function renderTgStatus(t) {
  TG.last = t;
  tgAddProgress(t);
  // The poll runs every few seconds: rebuild only on a change, so hover and focus stay put.
  const sig = JSON.stringify([t, TG.mode, TG.env, (S.state?.shares || []).map(x => x.telegram_chat_id)]);
  if (sig === TG.sig) return;
  TG.sig = sig;
  const out = $("#sTgStatus");
  if (!t.enabled) {
    out.innerHTML = TG.mode === "own" ? `<div class="tg-note">${icon("info")}<span>${TG_TOKEN_HINT}</span></div>`
      : TG.mode === "shared" ? `<div class="tg-note err">${icon("alert")}<span>${esc(t.error || "The shared bot isn't available right now.")}</span></div>`
        : `<div class="tg-note">${icon("info")}<span>Off: visitors of your links talk to the AI only.</span></div>`;
    return;
  }
  const shared = t.source === "shared";
  const [cls, label] = t.error ? ["err", "Error"] : t.username ? ["ok", "Connected"] : ["wait", "Connecting"];
  const sub = shared ? `Shared iRoof.top bot · messages pass through ${esc(TG.relayHost)}`
    : TG.env ? "Your own bot · set by TELEGRAM_BOT_TOKEN" : "Your own bot · messages go straight to Telegram";
  const groups = t.groups || [];
  const list = groups.length
    ? `<ul class="tg-chats">${groups.map(g => {
      const n = tgUses(g.id);
      return `<li class="tg-chat">${avatarHtml(g.title, null, "sm")}<div class="grow"><div class="t">${esc(g.title)}</div><div class="muted small">${n ? `Used by ${n} share link${n > 1 ? "s" : ""}` : "Telegram group · ready for share links"}</div></div><span class="tg-chat-ok" title="Connected" aria-label="Connected">${icon("check")}</span></li>`;
    }).join("")}</ul>`
    : `<div class="tg-empty">${icon("users")}<div><b>No group yet</b><span>Add the bot to a Telegram group. Visitor messages arrive there, and you answer them right in the chat.</span></div></div>`;
  out.innerHTML = `<div class="tg-card">
    <div class="tg-bot">
      <span class="tg-logo" aria-hidden="true">${icon("send")}</span>
      <div class="tg-bot-text"><b>${t.username ? `@${esc(t.username)}` : "Telegram bot"}</b><span class="muted small">${sub}</span></div>
      <span class="pill ${cls}"><span class="dot"></span>${label}</span>
    </div>
    ${t.error ? `<div class="tg-note err">${icon("alert")}<span>${esc(t.error)}</span></div>` : ""}
    ${t.username ? `<div class="tg-section">
      <div class="tg-section-head"><span class="legend">Chats</span>${groups.length ? `<span class="count">${groups.length}</span>` : ""}</div>
      ${list}
      <button type="button" class="btn ${groups.length ? "" : "primary"} tg-add-btn" id="sTgAdd">${icon("plus")}<span>${groups.length ? "Add to another group" : "Add to a group"}</span></button>
    </div>` : ""}
  </div>`;
  const add = $("#sTgAdd");
  if (add) add.onclick = () => openTgAdd(add);
}
// "Add to a group": the t.me link (app), the same link for Telegram Web, a QR code for the phone,
// and a live check that turns green once the bot is in a new group.
const TA = { app: "", web: "", known: new Set() };
// Telegram Web opens tg:// addresses passed as `tgaddr`, so the start parameter (the one-time code) survives.
function tgWebUrl(u) {
  try {
    const x = new URL(u);
    const domain = x.pathname.replace(/^\/+/, "").split("/")[0];
    if (x.hostname !== "t.me" || !domain) return "https://web.telegram.org/k/";
    const tg = `tg://resolve?domain=${domain}${x.search ? `&${x.search.slice(1)}` : ""}`;
    return `https://web.telegram.org/k/#?tgaddr=${encodeURIComponent(tg)}`;
  } catch { return "https://web.telegram.org/k/"; }
}
async function openTgAdd(btn) {
  btn.disabled = true;
  try {
    const r = await api("/api/telegram/link", { method: "POST" });
    const shared = TG.last?.source === "shared";
    TA.app = r.url; TA.web = tgWebUrl(r.url);
    TA.known = new Set((TG.last?.groups || []).map(g => g.id));
    $("#taBot").textContent = r.username ? `@${r.username}` : "Telegram bot";
    $("#taBotSub").textContent = shared ? "Shared iRoof.top bot" : "Your own bot";
    $("#taQr").src = qrSrc(TA.app);
    $("#taOpenApp").href = TA.app; $("#taOpenWeb").href = TA.web;
    $("#taAppUrl").textContent = hostOf(TA.app); $("#taAppUrl").title = TA.app;
    $("#taWebUrl").textContent = hostOf(TA.web); $("#taWebUrl").title = TA.web;
    $("#taWait").className = "tg-wait";
    $("#taWait").innerHTML = `<span class="spin" aria-hidden="true"></span><span>Waiting for the bot to join a group…${shared ? " The link works once, for 1 hour." : ""}</span>`;
    $("#tgAddDlg").showModal();
  } catch (e) { toast(e.message, { error: true }); }
  finally { btn.disabled = false; }
}
function tgAddProgress(t) {
  if (!$("#tgAddDlg").open) return;
  const added = (t.groups || []).filter(g => !TA.known.has(g.id));
  if (!added.length) return;
  added.forEach(g => TA.known.add(g.id));
  $("#taWait").className = "tg-wait ok";
  $("#taWait").innerHTML = `${icon("check")}<span>Added to <b>${added.map(g => esc(g.title)).join("</b>, <b>")}</b>. Pick it under <b>Telegram group</b> in a share link.</span>`;
}
$("#tgAddDlg").addEventListener("click", e => {
  const b = e.target.closest("[data-ta-copy]");
  if (b) copyWithFeedback(b, b.dataset.taCopy === "web" ? TA.web : TA.app, "Copied");
});
async function setTg(body) {
  if (TG.busy) return;
  TG.busy = true; $("#sTgConnect").disabled = true;
  tgNote(tgPending(body.mode === "off" ? "Turning off…" : "Connecting…"));
  try {
    await api("/api/settings", { method: "PUT", body: { telegram: body } });
    TG.mode = body.mode;
    if (body.token) { $("#sTgToken").value = ""; $("#sTgToken").placeholder = "•••••• (saved, paste a new token to replace it)"; }
    await refreshTg();
  } catch (e) {
    tgNote(`<div class="tg-note err">${icon("alert")}<span>${esc(e.message)}</span></div>`);
  } finally { TG.busy = false; $("#sTgConnect").disabled = false; }
}
$("#sTgModes").addEventListener("change", () => {
  const mode = radioValue("sTgMode");
  $("#sTgTokenRow").classList.toggle("hidden", mode !== "own");
  // An own bot needs its token first (unless one is saved): Connect applies it.
  if (mode === "own" && !/saved/.test($("#sTgToken").placeholder)) {
    TG.mode = "own"; tgNote(`<div class="tg-note">${icon("info")}<span>${TG_TOKEN_HINT}</span></div>`); $("#sTgToken").focus(); return;
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
// Test: saves, then one tiny request. The card says what was checked, how it went and,
// on failure, what to change; the raw message stays one click away.
function renderTest(state, r = {}) {
  const out = $("#sTestResult");
  const rows = items => `<dl class="tr-rows">${items.filter(([, v]) => v).map(([k, v]) => `<dt>${k}</dt><dd>${v}</dd>`).join("")}</dl>`;
  const where = [["Endpoint", esc(r.host || "")], ["Model", r.model ? `<code>${esc(r.model)}</code>` : ""]];
  // An instant refusal (no key, offline) was never sent: no time to show.
  const secs = r.ms >= 100 ? `${(r.ms / 1000).toFixed(1)} s` : "";
  out.className = `test-result ${state}`;
  if (state === "busy") out.innerHTML = `<div class="tr-head"><span class="spin" aria-hidden="true"></span><b>Saving and asking the model…</b></div>`;
  else if (state === "ok") out.innerHTML = `<div class="tr-head">${icon("check")}<b>Connected</b><span class="tr-time">${secs}</span></div>`
    + rows([...where, ["Reply", r.reply ? `“${esc(r.reply)}”` : "<i>empty</i>"]]);
  else {
    const [first, ...rest] = String(r.error || "Unknown error").split(/(?<=\.)\s+(?=[A-Z(])/);
    out.innerHTML = `<div class="tr-head">${icon("alert")}<b>Not connected</b>${secs ? `<span class="tr-time">after ${secs}</span>` : ""}</div>`
      + `<p class="tr-msg">${esc(first)}${rest.length ? ` ${esc(rest.join(" "))}` : ""}</p>` + rows(where);
  }
  out.scrollIntoView({ block: "nearest", behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
}
$("#sTest").onclick = async () => {
  const btn = $("#sTest");
  btn.disabled = true; btn.setAttribute("aria-busy", "true");
  renderTest("busy");
  try {
    await api("/api/settings", { method: "PUT", body: settingsBody() });
    if ($("#sKey").value.trim()) $("#sKey").placeholder = $("#sKey").value.trim() === "-" ? "sk-…" : "•••••• (saved — type to replace, “-” to clear)";
    $("#sKey").value = ""; loadState().catch(() => { });
    const r = await api("/api/llm/test", { method: "POST" });
    renderTest(r.ok ? "ok" : "err", r);
  } catch (err) { renderTest("err", { error: err.message }); }
  finally { btn.disabled = false; btn.removeAttribute("aria-busy"); }
};
// A result describes the values it tested: editing them makes it stale.
["#sBase", "#sModel", "#sKey"].forEach(sel => $(sel).addEventListener("input", () => { const o = $("#sTestResult"); if (!o.classList.contains("busy")) o.className = "test-result hidden"; }));
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

