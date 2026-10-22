# Docker & servers

## Docker

```sh
docker run -d --name rooftop -p 8080:8080 \
  -e OPENAI_API_KEY=… -e OPENAI_MODEL=… \
  -e ROOFTOP_PASSWORD=change-me -e TZ=Europe/Berlin \
  -v rooftop-data:/data \
  ghcr.io/irooftop/rooftop:latest
```

| | |
|---|---|
| Image | `ghcr.io/irooftop/rooftop`, linux/amd64 and linux/arm64 |
| Tags | `latest`, `X.Y.Z`, `X.Y` |
| Data | `/data` (exports, chats, settings) |
| Import on start | mount a folder and set `ROOFTOP_IMPORT=/import/chat` |

::: tip Node.js and ffmpeg
The image has neither. For stdio MCP servers started with `npx`, extend it:

```dockerfile
FROM ghcr.io/irooftop/rooftop
RUN apk add nodejs npm ffmpeg   # ffmpeg lets the AI see GIFs and videos frame by frame
```

Or use a remote (HTTP) MCP server instead.
:::

## A server with HTTPS

[`deploy/`](https://github.com/irooftop/rooftop/tree/main/deploy) runs iRoof.top behind [Caddy](https://caddyserver.com),
which gets and renews the certificate:

```sh
cd deploy
sed -i 's/example.com/your.domain/g' Caddyfile        # the domain must already point at the server
printf 'ROOFTOP_PASSWORD=%s\n' "$(openssl rand -base64 24)" > iroof.env && chmod 600 iroof.env
docker compose up -d
```

```
  visitor ──https──▶ Caddy :443 ──▶ iroof:8080
                       │               ├─ /s/…, /api/s/…, assets   public
                       │               └─ everything else          password
                       └─ TLS certificate, renewed automatically
```

* Share links use your domain: `https://your.domain/s/<name>`.
* The container runs read-only, without capabilities and with a memory limit.
* `ROOFTOP_TRUST_PROXY` is on, because Caddy passes the visitor's IP.
* Update with `docker compose pull && docker compose up -d`. Back up the `iroof-data` volume.

::: danger Set a password on a public server
`ROOFTOP_PASSWORD` protects the owner UI and API. Without it, anyone who finds the address can read your chats.
:::
