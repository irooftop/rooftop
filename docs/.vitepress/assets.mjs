// Copies the images shared with the app and the README into docs/public/img, and the app's
// scene into theme/scene.svg (both git-ignored), so there is a single source for each of them.
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";

const root = new URL("../../", import.meta.url);
const out = new URL("../public/img/", import.meta.url);
mkdirSync(out, { recursive: true });
for (const f of ["demo.svg", "web/telegram-export.svg", "web/favicon.svg", "web/icon_light.svg", "web/icon_dark.svg", "web/logo_light.svg", "web/logo_dark.svg"]) {
  copyFileSync(new URL(f, root), new URL(f.split("/").pop(), out));
}

// The scene of the app's home screen (and of iroof.top) for the docs' home page.
const html = readFileSync(new URL("web/index.html", root), "utf8");
const scene = html.match(/<svg class="scene"[\s\S]*?<\/svg>/)?.[0];
if (!scene) throw new Error("web/index.html: no <svg class=\"scene\"> found");
writeFileSync(new URL("theme/scene.svg", import.meta.url), scene.replace('class="scene"', 'class="scene" aria-hidden="true" focusable="false"') + "\n");
