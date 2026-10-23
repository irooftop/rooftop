// ---------- clipboard (navigator.clipboard needs a secure context) ----------
// Copy buttons confirm in place: a check and "Copied" for a moment, then the label returns.
async function copyWithFeedback(btn, text, done) {
  if (!await copyText(text)) return toast("Copy failed: select the text and copy it by hand", { error: true });
  clearTimeout(btn._copyTimer);
  if (btn._label == null) { btn._label = btn.innerHTML; btn.style.minWidth = `${btn.offsetWidth}px`; }
  btn.innerHTML = `${icon("check")}<span aria-live="polite">${esc(done)}</span>`;
  btn.classList.add("copied");
  btn._copyTimer = setTimeout(() => { btn.innerHTML = btn._label; btn._label = null; btn.style.minWidth = ""; btn.classList.remove("copied"); }, 1800);
}
async function copyText(text) {
  try { if (navigator.clipboard && isSecureContext) { await navigator.clipboard.writeText(text); return true; } } catch { }
  const ta = document.createElement("textarea");
  ta.value = text; ta.setAttribute("readonly", ""); ta.style.cssText = "position:fixed;top:0;left:0;opacity:0";
  (document.querySelector("dialog[open]") || document.body).append(ta);
  ta.select(); ta.setSelectionRange(0, text.length);
  let ok = false; try { ok = document.execCommand("copy"); } catch { }
  ta.remove();
  return ok;
}

