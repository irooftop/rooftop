# Desktop app

The desktop app ([Tauri](https://v3.tauri.app) `3.0.0-alpha.5`) runs the same server as `rooftop serve` on a random
`127.0.0.1` port and shows it in a native window.

| Platform | Install |
|---|---|
| macOS | `brew install --cask irooftop/tap/irooftop`, or the `.dmg` for [Apple Silicon](https://github.com/irooftop/rooftop/releases/latest/download/iRoof.top_aarch64.dmg) or [Intel](https://github.com/irooftop/rooftop/releases/latest/download/iRoof.top_x64.dmg) |
| Windows 10 and 11 | [Installer (.exe)](https://github.com/irooftop/rooftop/releases/latest/download/iRoof.top_x64-setup.exe) or [.msi](https://github.com/irooftop/rooftop/releases/latest/download/iRoof.top_x64_en-US.msi) |
| Linux (x86-64) | [AppImage](https://github.com/irooftop/rooftop/releases/latest/download/iRoof.top_amd64.AppImage) (any distribution), [.deb](https://github.com/irooftop/rooftop/releases/latest/download/iRoof.top_amd64.deb) (Debian, Ubuntu) or [.rpm](https://github.com/irooftop/rooftop/releases/latest/download/iRoof.top.x86_64.rpm) (Fedora) |

The download links always point to the newest release.

::: warning The builds are not signed yet
On macOS, open the app once, then **System Settings → Privacy & Security → Open Anyway**. On Windows, choose
**More info → Run anyway** in SmartScreen. Step by step, with an animation: [Open an unsigned app](./unsigned-apps).
:::

## First launch

The app doesn't read your shell or `.env`, so it greets you with **Connect a model**: open **Settings**, enter the
endpoint, key and model, and press **Test**. If `OPENAI_BASE_URL`, `OPENAI_API_KEY`, `OPENAI_MODEL` or
`OPENAI_TEMPERATURE` are set, they become the initial defaults.

| | |
|---|---|
| Data | the OS app-data directory, e.g. `~/Library/Application Support/top.iroof.desktop` on macOS |
| Share links | on the same Wi‑Fi or over the internet: [Open links on other devices](../features/share-links#open-links-on-other-devices) |
| Other sites | links such as `t.me` open in your browser |
| macOS window | no system title bar: the app's headers continue under the traffic lights and drag the window |
| Importing | the app reads the export itself: **Choose folder…** opens the system picker, and a dropped folder, `.zip` or `.txt` is imported from where it is. Nothing is uploaded, the original stays untouched, and on APFS the media are cloned rather than copied, so even a 13 GB export takes almost no extra space |

## Build it yourself

You need Rust ≥ 1.95 and Node.js, and on Linux the [Tauri prerequisites](https://v3.tauri.app/start/prerequisites/).

```sh
cd desktop
npm ci
npm run build                          # installers in desktop/target/release/bundle/
npm run build -- --debug --no-bundle   # just the binary
```

On macOS, `npm run build` also makes the `.dmg`, and skips the Finder AppleScript that only arranges the DMG window
(it fails without the *Automation* permission). Set `ROOFTOP_DMG_FINDER=1` to keep the Finder layout.
