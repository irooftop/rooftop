// ---------- home: the roof at night ----------
const HOME_LINES = [
  "Some people are far away.",
  "Their words stay with you.",
  "Up here, the conversation goes on.",
  "Come up to the roof.",
];
const HOME = { timer: null, run: 0 };
function storyType(run, i = 0) {
  const el = $("#homeStory");
  if (run !== HOME.run) return;
  const line = HOME_LINES[i];
  const reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;
  el.classList.remove("fade");
  if (reduce) { el.textContent = HOME_LINES[HOME_LINES.length - 1]; return; }
  let n = 0;
  const step = () => {
    if (run !== HOME.run) return;
    el.innerHTML = `${esc(line.slice(0, n))}<span class="caret" aria-hidden="true"></span>`;
    if (n++ < line.length) { HOME.timer = setTimeout(step, 42 + Math.random() * 40); return; }
    if (i === HOME_LINES.length - 1) { HOME.timer = setTimeout(() => { el.textContent = line; }, 2400); return; }
    HOME.timer = setTimeout(() => { el.classList.add("fade"); HOME.timer = setTimeout(() => storyType(run, i + 1), 650); }, 2200);
  };
  step();
}
function showHome() {
  const home = $("#home");
  HOME.run++; clearTimeout(HOME.timer);
  home.classList.remove("hidden", "leaving");
  // Restart the drawing animation.
  const scene = home.querySelector(".scene"); scene.replaceWith(scene.cloneNode(true));
  $("#homeStory").textContent = "";
  HOME.timer = setTimeout(() => storyType(HOME.run), 1700);
  document.title = "iRoof.top";
}
function leaveHome() {
  const home = $("#home");
  if (home.classList.contains("hidden") || home.classList.contains("leaving")) return;
  HOME.run++; clearTimeout(HOME.timer);
  sessionStorage.setItem("iroof:climbed", "1");
  home.classList.add("leaving");
  const done = () => { home.classList.add("hidden"); home.classList.remove("leaving"); };
  if (matchMedia("(prefers-reduced-motion: reduce)").matches) done(); else setTimeout(done, 680);
  if (matchMedia("(pointer: fine)").matches) setTimeout(() => (S.chatId ? $("#input") : $("#newChatBtn"))?.focus(), 700);
}
$("#homeGo").onclick = leaveHome;
$("#brandBtn").onclick = showHome;
document.addEventListener("keydown", e => {
  if ($("#home").classList.contains("hidden")) return;
  if (e.key === "Escape" || (e.key === "Enter" && e.target === document.body)) { e.preventDefault(); leaveHome(); }
});
// The roof greets you when the app opens (once per tab session), unless a link points somewhere specific.
if (!GUEST && !location.hash && !sessionStorage.getItem("iroof:climbed")) showHome();

