// ---------- photo viewer (lightbox gallery) ----------
const LB = { list: [], i: 0, zoom: 1, x: 0, y: 0, opener: null };
function galleryPhotos() {
  return (S.chat?.messages || []).filter(m => m.media?.kind === "photo" && m.media.available !== false && m.media.file);
}
function openLightbox(id) {
  LB.list = galleryPhotos(); LB.i = Math.max(0, LB.list.findIndex(m => m.id === id));
  if (!LB.list.length) return;
  LB.opener = document.activeElement;
  $("#lbStrip").innerHTML = LB.list.length > 1 ? LB.list.map((m, k) => `<button data-k="${k}" aria-label="Photo ${k + 1}"><img src="${mediaUrl(m.media.file)}" alt="" loading="lazy"></button>`).join("") : "";
  const lb = $("#lightbox"); lb.classList.remove("hidden"); document.body.classList.add("lb-open");
  showLb(); lb.focus();
}
function closeLightbox() {
  $("#lightbox").classList.add("hidden"); document.body.classList.remove("lb-open");
  $("#lbImg").removeAttribute("src"); LB.opener?.focus?.();
}
function setZoom(z, cx = 0, cy = 0) {
  LB.zoom = z; if (z === 1) { LB.x = 0; LB.y = 0; } else { LB.x = -cx * (z - 1); LB.y = -cy * (z - 1); }
  applyZoom();
}
function applyZoom() {
  const img = $("#lbImg");
  img.style.transform = `translate(${LB.x}px, ${LB.y}px) scale(${LB.zoom})`;
  $("#lightbox").classList.toggle("zoomed", LB.zoom > 1);
}
function showLb() {
  const m = LB.list[LB.i]; if (!m) return;
  const src = mediaUrl(m.media.file), n = LB.list.length;
  const img = $("#lbImg"); img.classList.add("loading"); img.onload = () => img.classList.remove("loading");
  img.src = src; img.alt = m.media.description || m.text || "Photo";
  setZoom(1);
  $("#lbName").textContent = m.mine ? "You" : m.from;
  $("#lbDate").textContent = m.date ? `${fmtDay(m.date)}, ${fmtTime(m.date)}` : "";
  $("#lbCount").textContent = n > 1 ? `${LB.i + 1} / ${n}` : "";
  $("#lbCaption").textContent = m.text || "";
  $("#lbCaption").classList.toggle("hidden", !m.text);
  $("#lbOpen").href = src; $("#lbDownload").href = src;
  $("#lbPrev").disabled = LB.i === 0; $("#lbNext").disabled = LB.i === n - 1;
  $$("#lbStrip button").forEach((b, k) => { b.classList.toggle("on", k === LB.i); if (k === LB.i) b.scrollIntoView({ inline: "center", block: "nearest" }); });
  // Preload neighbours for instant swipes.
  for (const k of [LB.i - 1, LB.i + 1]) if (LB.list[k]) new Image().src = mediaUrl(LB.list[k].media.file);
}
const lbGo = d => { const k = LB.i + d; if (k >= 0 && k < LB.list.length) { LB.i = k; showLb(); } };
$("#lbPrev").onclick = e => { e.stopPropagation(); lbGo(-1); };
$("#lbNext").onclick = e => { e.stopPropagation(); lbGo(1); };
$("#lbClose").onclick = closeLightbox;
$("#lbLocate").onclick = () => {
  const id = LB.list[LB.i]?.id; closeLightbox();
  const el = $(`#m-${id}`); if (el) { el.scrollIntoView({ block: "center" }); el.classList.remove("flash"); void el.offsetWidth; el.classList.add("flash"); }
};
$("#lbStrip").onclick = e => { const b = e.target.closest("[data-k]"); if (b) { LB.i = Number(b.dataset.k); showLb(); } };
$("#lightbox").addEventListener("keydown", e => {
  if (e.key === "Escape") { e.preventDefault(); LB.zoom > 1 ? setZoom(1) : closeLightbox(); }
  else if (e.key === "ArrowLeft") lbGo(-1);
  else if (e.key === "ArrowRight") lbGo(1);
  else if (e.key === "Tab") { // keep focus inside the viewer
    const f = $$("#lightbox button:not(:disabled), #lightbox a[href]").filter(x => x.offsetParent);
    if (!f.length) return;
    const first = f[0], last = f[f.length - 1];
    if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
    else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
  }
});
// Viewer gestures: click on the dark area closes; double-click / double-tap zooms; drag pans; swipe navigates or closes.
(() => {
  const stage = $("#lbStage"), img = $("#lbImg");
  let start = null, moved = false, lastTap = 0;
  const zoomAt = (cx, cy) => {
    if (LB.zoom > 1) return setZoom(1);
    const r = img.getBoundingClientRect();
    setZoom(2.5, cx - (r.left + r.width / 2), cy - (r.top + r.height / 2));
  };
  stage.addEventListener("pointerdown", e => {
    if (e.target.closest(".lb-nav")) return;
    start = { x: e.clientX, y: e.clientY, ox: LB.x, oy: LB.y, t: Date.now() }; moved = false;
    stage.setPointerCapture(e.pointerId);
  });
  stage.addEventListener("pointermove", e => {
    if (!start) return;
    const dx = e.clientX - start.x, dy = e.clientY - start.y;
    if (Math.abs(dx) + Math.abs(dy) > 6) moved = true;
    if (LB.zoom > 1) { LB.x = start.ox + dx; LB.y = start.oy + dy; applyZoom(); }
    else if (moved) { img.style.transform = `translate(${dx}px, ${Math.max(0, dy) * .6}px)`; img.style.opacity = String(1 - Math.min(.5, Math.max(0, dy) / 400)); }
  });
  stage.addEventListener("pointerup", e => {
    if (!start) return;
    const dx = e.clientX - start.x, dy = e.clientY - start.y, fast = Date.now() - start.t < 600;
    const onImg = e.target === img; start = null; img.style.opacity = "";
    if (LB.zoom === 1) img.style.transform = "";
    if (moved) {
      if (LB.zoom > 1) return;
      if (Math.abs(dx) > 60 && Math.abs(dx) > Math.abs(dy)) return lbGo(dx < 0 ? 1 : -1);
      if (dy > 110 && fast) return closeLightbox();
      return;
    }
    const now = Date.now();
    if (onImg && now - lastTap < 300) { lastTap = 0; zoomAt(e.clientX, e.clientY); return; }
    lastTap = onImg ? now : 0;
    if (!onImg && LB.zoom === 1) closeLightbox();
  });
  stage.addEventListener("wheel", e => {
    if (!e.ctrlKey && LB.zoom === 1) return; // trackpad pinch arrives as ctrl+wheel
    e.preventDefault();
    const z = Math.min(5, Math.max(1, LB.zoom * (e.deltaY < 0 ? 1.15 : 1 / 1.15)));
    if (z === 1) setZoom(1); else { LB.zoom = z; applyZoom(); }
  }, { passive: false });
})();


