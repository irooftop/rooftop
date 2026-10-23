# Development

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

| Tests cover | |
|---|---|
| Parser and conversion | against `examples/telegram` and `examples/whatsapp` |
| Share links | visitor isolation, PIN lockout, media allow-list, rooms, names, blocking, auth bypass |
| MCP client | mock servers of every protocol era (HTTP, legacy SSE, stdio) |
| Relay | hub, names, forwarding |

The desktop crate is separate: `cd desktop && cargo check`.

## Layout

```
src/        server (axum), parsers, LLM agent, MCP client, share links, relay, Telegram
  server/     the owner's HTTP API, one file per area: assets, uploads, exports, chats,
              messages, settings, media; mod.rs builds the routers
  share_api/  share links: owner.rs (managing links and visitors), visitor.rs (/s/…)
web/        the UI: index.html, app.css and js/ (served as one app.js), embedded into the binary
  js/         one file per area of the UI, numbered in load order (one script, one scope)
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
 bump Cargo.toml ─▶ tag vX.Y.Z ─┬─▶ Release: crates.io + ghcr.io image (amd64 + arm64, native runners)
                                └─▶ desktop: installers ─▶ publish release (latest) ─▶ Homebrew: formula + cask
```

1. Bump `version` in `Cargo.toml` (and `desktop/`), commit, push a tag `vX.Y.Z`. The tag must be numeric for the
   Windows MSI. Everything below then runs without further clicks.
2. *Release* publishes the crate (secret `CARGO_REGISTRY_TOKEN`, environment `crates-io`) and the Docker image:
   one job per architecture on a native runner (`ubuntu-latest`, `ubuntu-24.04-arm`) pushes by digest, and a
   `merge` job creates the multi-arch tags. Run it by hand with `dry_run` to check crate publishing.
3. *desktop* builds installers for macOS (arm64, x64), Windows and Linux into a draft release. When all of them
   are attached, its `publish` job fills in generated release notes, publishes the release and marks it latest.
4. Its `homebrew` job then calls *Homebrew*, which pushes `Formula/rooftop.rb` and `Casks/irooftop.rb` to
   [`irooftop/homebrew-tap`](https://github.com/irooftop/homebrew-tap). It needs the secret `HOMEBREW_TAP_TOKEN`
   (fine-grained token, `irooftop/homebrew-tap`, Contents: read and write) or `HOMEBREW_TAP_DEPLOY_KEY`; without
   one it skips with a notice. (A release published by `GITHUB_TOKEN` does not fire `release: published`, which is
   why the call is explicit; publishing a release by hand still triggers Homebrew through that event.)

Rerun the tap update for an existing tag, for example after adding the secret:

```sh
gh workflow run homebrew.yml -f tag=v0.3.0
```

`scripts/homebrew-tap.sh vX.Y.Z <tap checkout>` renders the same files locally. If the installers of a tag
failed, rerun the failed *desktop* jobs: `publish` and `homebrew` follow once they pass.
