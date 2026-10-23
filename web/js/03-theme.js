// ---------- theme ----------
const THEMES = ["auto", "light", "dark"];
function applyTheme() {
  const pref = localStorage.getItem("theme") || "auto";
  const dark = pref === "dark" || (pref === "auto" && matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
  // Native window chrome (traffic lights, resize shadows) follows the app's theme.
  if (SHELL) tauriInvoke("plugin:window|set_theme", { label: "main", value: pref === "auto" ? null : pref });
  const b = $("#themeBtn"); b.innerHTML = icon(pref === "auto" ? "auto" : dark ? "moon" : "sun"); b.title = `Theme: ${pref}`; b.setAttribute("aria-label", `Theme: ${pref}. Click to change`);
}
$("#themeBtn").onclick = () => {
  const cur = localStorage.getItem("theme") || "auto";
  localStorage.setItem("theme", THEMES[(THEMES.indexOf(cur) + 1) % 3]); applyTheme();
};
matchMedia("(prefers-color-scheme: dark)").addEventListener("change", applyTheme);
applyTheme();

