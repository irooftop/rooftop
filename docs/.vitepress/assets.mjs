// Copies the images shared with the app and the README into docs/public/img (git-ignored),
// so there is a single source for each of them.
import { copyFileSync, mkdirSync } from "node:fs";

const root = new URL("../../", import.meta.url);
const out = new URL("../public/img/", import.meta.url);
mkdirSync(out, { recursive: true });
for (const f of ["demo.svg", "web/telegram-export.svg", "web/favicon.svg", "web/icon_light.svg", "web/icon_dark.svg", "web/logo_light.svg", "web/logo_dark.svg"]) {
  copyFileSync(new URL(f, root), new URL(f.split("/").pop(), out));
}
