# HTTP API

JSON over HTTP. With `ROOFTOP_PASSWORD` set, the owner API needs HTTP Basic auth.

## Owner

| Method | Path | |
|---|---|---|
| GET | `/api/state` | exports, chats, share links, LLM info |
| POST | `/api/exports` | multipart upload: folder files with relative names, or a `.zip`. Answers `202 {"id"}` at once and imports in the background |
| POST | `/api/uploads` | start a batched upload (what the web UI uses): `{"id"}` |
| POST | `/api/uploads/{id}/files` | multipart batch of files with relative names; resending a batch replaces its files: `{"files", "bytes"}` |
| POST | `/api/uploads/{id}/check` | after the chat files, before the media: `{"bytes"}` still to send → `{title, messages, existing, fits, free_bytes, needed_bytes}` (already imported? enough disk space?) |
| POST | `/api/uploads/{id}/import` | import everything uploaded so far: `202 {"id"}` of the import job |
| DELETE | `/api/uploads/{id}` | drop an unfinished upload (abandoned ones are removed after 24 h and on restart) |
| POST | `/api/local/pick` | desktop app only: show the native picker, `{"kind": "folder" \| "file"}` → `{"path"}` (`null` when cancelled) |
| POST | `/api/local/import` | desktop app only: import from this computer, `{"paths": […]}` (a folder, `.zip` or WhatsApp `.txt`; files inside one export stand for its folder) → `202 {"id", "name"}` of the import job; the source is copied, never changed |
| GET | `/api/imports/{id}` | progress of an import job, see below |
| GET · DELETE | `/api/exports/{id}` | details with the media library · delete with its chats |
| POST | `/api/exports/{id}/describe` | describe the media library |
| POST | `/api/chats` | `{export_id, persona, user?, title?, notes?}` |
| GET · PATCH · DELETE | `/api/chats/{id}` | timeline (`?limit=`) · title and notes · delete |
| POST | `/api/chats/{id}/messages` | `{text?, reply_to?, quote?, sticker_id?, gif_id?}` |
| POST | `/api/chats/{id}/react` | `{message_id, emoji}` (`""` removes) |
| POST | `/api/chats/{id}/voice` | multipart `file` (wav, ogg, mp3, m4a, webm), `duration?`, `reply_to?` |
| POST | `/api/chats/{id}/respond` | let the persona act; returns messages and ordered actions |
| POST | `/api/chats/{id}/regenerate` | drop the persona's last turn and respond again |
| GET | `/api/chats/{id}/prompt` | the exact `{model, messages, tools}` |
| GET · POST | `/api/chats/{id}/shares` | list · create share links |
| PATCH · DELETE | `/api/shares/{id}` | edit (`pin: ""` removes it, `revoke_sessions`) · delete |
| DELETE | `/api/shares/{id}/sessions/{session}` | end one visitor conversation |
| GET · PUT | `/api/settings` | LLM, MCP and Telegram (`telegram: {mode: off\|shared\|own, token?}`) |
| GET · POST | `/api/mcp` · `/api/mcp/reconnect` | MCP status · reconnect |
| GET · POST | `/api/telegram` · `/api/telegram/link` | bot status and groups · a `t.me` link that adds the bot to a group |
| GET · PUT | `/api/sharing` | Wi‑Fi and Internet sharing `{lan?, internet?, name?}` (owner UI on localhost only) |
| GET | `/api/qr?text=…` | a QR code (SVG) |

### Import jobs

`GET /api/imports/{id}` returns

```json
{"id": "…", "state": "running", "stage": "parsing", "done": 37, "total": 121, "messages": 36000}
```

* `state`: `queued` (another import is running; they run one at a time), `running`, `done` or `failed`.
* `stage`: `unpacking`, `reading`, `parsing` (`done` of `total` pages), `hashing` and `placing` (files), `saving`.
* `messages`: messages found so far.
* `failed` adds `error`, for example `no messages.html, result.json or WhatsApp .txt found in the upload`.
* `done` adds `created` (`false` when the same export was imported before) and `export`, the summary also found in `/api/state`.

The last 16 finished jobs are kept; an unknown id answers 404.

## Public

No password, for share-link visitors:

| Method | Path | |
|---|---|---|
| GET | `/s/{name}[/{token}]` | the visitor page |
| GET | `/api/s/{name}` | start page info |
| POST | `/api/s/{name}/sessions` | `{pin?, name?}` → `{token}` |
| GET | `/api/s/{name}/c/{token}` | timeline |
| POST | `/api/s/{name}/c/{token}/messages` · `react` · `voice` · `respond` · `regenerate` | as for the owner; `regenerate`, and `respond` without a new visitor message ("write first"), only where the link has `allow_regenerate` / `allow_nudge` (else `403`) |
| GET | `/api/s/{name}/c/{token}/media/{path}` | media the visitor may see |

## Relay hub

| Method | Path | |
|---|---|---|
| POST | `/relay/v1/register` | `{name}`, claim or rename (`Authorization: Bearer <secret>`) |
| GET | `/relay/v1/connect` | the app's WebSocket |
| GET | `/relay/v1/info` | hub info |
| * | `/relay/v1/tg/bot<secret>/<method>` | the shared bot's Bot API proxy |
