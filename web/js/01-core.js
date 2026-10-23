// The web UI of iRoof.top. The files in web/js/ are one script, served as /app.js in the
// order of their numbers (src/server/assets.rs): later parts use what earlier ones define.
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

