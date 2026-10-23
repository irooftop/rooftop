// ---------- mobile: keep the composer above the on-screen keyboard (iOS ignores interactive-widget) ----------
if (window.visualViewport) {
  const vv = visualViewport, root = document.documentElement;
  const fit = () => {
    root.style.setProperty("--app-h", `${Math.round(vv.height)}px`);
    if (vv.offsetTop) window.scrollTo(0, 0);
    if (S.pinned) { const b = $("#messages"); b.scrollTop = b.scrollHeight; }
  };
  vv.addEventListener("resize", fit); vv.addEventListener("scroll", fit);
}
