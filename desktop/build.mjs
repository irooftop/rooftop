// `npm run build` = `tauri build` that also produces the DMG in a plain terminal on macOS.
// The DMG step normally asks Finder (via AppleScript) to arrange the window, which
// needs System Settings > Privacy & Security > Automation permission for the terminal.
// With CI set, Tauri's bundler passes `--skip-jenkins` and skips that cosmetic step.
// Set ROOFTOP_DMG_FINDER=1 to keep the Finder layout (requires the permission).
import { spawnSync } from "node:child_process";

const env = { ...process.env };
if (!env.ROOFTOP_DMG_FINDER && !env.CI) env.CI = "true";
const args = process.argv.slice(2);
// tauri.macos.conf.json limits a plain `npx tauri build` to the .app (it always
// works), but here we also ask for the DMG unless bundles were chosen explicitly.
if (process.platform === "darwin" && !args.some(a => a === "--bundles" || a === "-b" || a.startsWith("--bundles=") || a === "--no-bundle")) {
  args.push("--bundles", "app,dmg");
}
const r = spawnSync("npx", ["tauri", "build", ...args], { stdio: "inherit", env, shell: process.platform === "win32" });
process.exit(r.status ?? 1);
