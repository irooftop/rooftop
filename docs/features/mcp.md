# MCP tools

**Settings → MCP servers** takes the common `mcpServers` JSON:

```json
{
  "giphy":  { "command": "npx", "args": ["-y", "mcp-server-giphy"], "env": { "GIPHY_API_KEY": "…" } },
  "remote": { "url": "https://example.com/mcp", "headers": { "Authorization": "Bearer …" } },
  "legacy": { "url": "https://old.example.com/sse", "type": "sse" }
}
```

* Tools reach the model as `mcp_<server>_<tool>`. A GIF found by Giphy is sent with `send_gif {url}`.
* `env`, `headers`, `args`, `command` and `url` may use `${NAME}` from the environment or `.env`, so secrets stay out
  of `settings.json`.
* Stdio servers run in their own directory, `data/mcp/<server>/`.
* **Reconnect MCP** in Settings restarts them; the status lists each server's protocol and tools.

## Protocol revisions

| Revision | Transport | |
|---|---|---|
| `2026-07-28` | Streamable HTTP, stateless | probed first with `server/discover` |
| `2025-11-25`, `2025-06-18`, `2025-03-26` | Streamable HTTP or stdio | `initialize` handshake, sessions |
| `2024-11-05` | HTTP+SSE (deprecated) | last fallback |

```
 server/discover (2026-07-28)
   ├─ ok ─────────────────────────────▶ stateless
   ├─ -32022 unsupported version ─────▶ switch version
   └─ anything else ─▶ initialize ─┬──▶ session (re-initialize on 404)
                                   └──▶ legacy HTTP+SSE
```

Elicitation and sampling requests (`input_required`) are reported to the model as errors.
