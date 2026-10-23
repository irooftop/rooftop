# Relay hub

A server can be the relay that apps use for **Internet** sharing, as iroof.top is.

```
 visitor ──▶ https://alex.your.domain/s/… ──▶ hub ══ WebSocket ══▶ Alex's app
                                              │
 app ── POST /relay/v1/register {name} ──────▶│  names: first come, first served
 app ── GET  /relay/v1/connect ══════════════▶│  one outgoing connection per app
```

| Side | Setting |
|---|---|
| Hub | `ROOFTOP_RELAY_DOMAIN=your.domain` (optional shared bot: `ROOFTOP_RELAY_BOT_TOKEN`) |
| Apps | `ROOFTOP_RELAY_URL=https://your.domain`: links at `https://<name>.your.domain` |

## Requirements

* A wildcard DNS record `*.your.domain` pointing at the server.
* A wildcard certificate. With Caddy that needs the DNS-01 challenge and a
  [DNS provider module](https://caddyserver.com/docs/modules/) (`xcaddy build --with github.com/caddy-dns/<provider>`).
* A reverse proxy that passes `Host` through (Caddy does by default):

```nginx
*.your.domain {
	tls {
		dns <provider> …
	}
	reverse_proxy iroof:8080
}
```

## A splash page on the main domain

On the main domain the hub serves the relay API, its own share links and the owner's app (behind `ROOFTOP_PASSWORD`).
iroof.top shows a small splash page at `/` instead, with links to the docs and GitHub:
[`deploy/landing/index.html`](https://github.com/irooftop/rooftop/blob/main/deploy/landing/index.html), a single
self-contained file (no scripts, fonts or third-party requests). Unknown `<name>.your.domain` links land there too.
Mount the folder into Caddy (`./landing:/srv/landing:ro`) and route only `/`:

```nginx
your.domain {
	# Visitors: the splash page. A browser that already sends the owner's password gets the app.
	@landing {
		method GET HEAD
		path / /index.html
		not header Authorization *
	}
	handle @landing {
		root * /srv/landing
		rewrite * /index.html
		header Content-Security-Policy "default-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
		file_server
	}
	# The owner signs in at /owner: the browser asks for the password, keeps sending it, and / opens the app.
	handle /owner {
		@anon not header Authorization *
		handle @anon {
			header WWW-Authenticate "Basic realm=\"iRoof.top\""
			respond "Sign in to open the app" 401
		}
		handle {
			redir * / 302
		}
	}
	handle {
		reverse_proxy iroof:8080
	}
}
```

The app still checks the password on every owner request; the matcher only decides who sees the splash page.

## Rules

| | |
|---|---|
| Hub API | `/relay/v1/…` on the main domain |
| `<name>.your.domain` | always goes to the app that owns the name, never to the hub's owner UI |
| Names | 3–32 lowercase letters, digits and hyphens; a few (`www`, `api`, `admin`…) reserved |
| Release | after 180 days without a connection |
| New names | rate-limited per IP |
| Ownership | a random secret in the app's `settings.json`; the hub stores only its SHA-256 |
| State | `relay.json` and `relay-telegram.json` in the data directory |
