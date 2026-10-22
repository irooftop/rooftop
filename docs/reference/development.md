# Development

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

| Tests cover | |
|---|---|
| Parser and conversion | against `examples/telegram` and `examples/whatsapp` |
| Share links | visitor isolation, PIN lockout, media allow-list, rooms, auth bypass |
| MCP client | mock servers of every protocol era (HTTP, legacy SSE, stdio) |
| Relay | hub, names, forwarding |

The desktop crate is separate: `cd desktop && cargo check`.

## Layout

```
src/        server (axum), parsers, LLM agent, MCP client, share links, relay, Telegram
web/        the UI: index.html, app.js, app.css, embedded into the binary
desktop/    Tauri app around the same server
docs/       this site (VitePress)
deploy/     Caddy + Docker Compose
examples/   two fictional exports
```

## Docs

```sh
cd docs
npm ci
npm run dev     # http://localhost:5173/rooftop/
npm run build   # docs/.vitepress/dist
```

The site deploys to [irooftop.github.io/rooftop](https://irooftop.github.io/rooftop) on every push to `main` that
touches `docs/`. `demo.svg` is built by `python3 scripts/demo-svg.py` (plain SVG and CSS, so GitHub renders it).

## Releasing

```
 bump Cargo.toml ─▶ tag vX.Y.Z ─┬─▶ Release: crates.io + ghcr.io image
                                └─▶ desktop: installers ─▶ draft GitHub Release
                                                              │ publish
                                                              ▼
                                                  Homebrew: formula + cask
```

1. Bump `version` in `Cargo.toml` (and `desktop/`), commit, push a tag `vX.Y.Z`. The tag must be numeric for the
   Windows MSI.
2. *Release* publishes the crate (secret `CARGO_REGISTRY_TOKEN`, environment `crates-io`) and the Docker image.
   Run it by hand with `dry_run` to check publishing.
3. *desktop* builds installers for macOS (arm64, x64), Windows and Linux into a **draft** release.
4. Publishing the release runs *Homebrew*, which pushes `Formula/rooftop.rb` and `Casks/irooftop.rb` to
   [`irooftop/homebrew-tap`](https://github.com/irooftop/homebrew-tap) (secret `HOMEBREW_TAP_TOKEN` or
   `HOMEBREW_TAP_DEPLOY_KEY`; without one it skips). `scripts/homebrew-tap.sh vX.Y.Z <tap checkout>` does the same by hand.
