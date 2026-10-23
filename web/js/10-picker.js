// ---------- sticker / GIF picker ----------
function renderPicker() {
  $$(".picker-tabs button").forEach(b => b.classList.toggle("active", b.dataset.tab === S.pickerTab));
  const items = (S.chat?.library || []).filter(i => i.kind === S.pickerTab);
  $("#pickerGrid").innerHTML = items.length ? items.map(i => {
    const vid = /\.(webm|mp4)$/i.test(i.file);
    const src = /\.tgs$/i.test(i.file) ? mediaUrl(i.thumb) : mediaUrl(i.file);
    return `<button data-pick-id="${i.id}" title="${esc(i.description || i.id)}">${vid ? `<video src="${src}" muted loop autoplay playsinline></video>` : `<img src="${src}" alt="" loading="lazy">`}</button>`;
  }).join("") : `<div class="picker-empty">No ${S.pickerTab === "gif" ? "GIFs" : "stickers"} in this export.</div>`;
}
$("#pickerBtn").onclick = e => { e.stopPropagation(); const open = $("#picker").classList.toggle("hidden") === false; $("#pickerBtn").setAttribute("aria-expanded", open); renderPicker(); };
$$(".picker-tabs button").forEach(b => b.onclick = () => { S.pickerTab = b.dataset.tab; renderPicker(); });
$("#pickerGrid").onclick = e => {
  const b = e.target.closest("[data-pick-id]"); if (!b) return;
  $("#picker").classList.add("hidden"); $("#pickerBtn").setAttribute("aria-expanded", "false");
  send(S.pickerTab === "gif" ? { gif_id: b.dataset.pickId } : { sticker_id: b.dataset.pickId });
};
document.addEventListener("click", e => { if (!e.target.closest("#picker") && !e.target.closest("#pickerBtn")) $("#picker").classList.add("hidden"); });

