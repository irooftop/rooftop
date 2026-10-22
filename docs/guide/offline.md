# Offline & privacy

iRoof.top keeps your chats on your computer. With a model that runs on your computer too, **nothing has to leave it**.
**Offline mode** makes that a rule the app enforces, not a habit.

## What happens locally

| Step | Where it runs | Network |
|---|---|---|
| Import, parsing, the style profile | your computer | none |
| Storage | JSON files and media in the data directory | none |
| Search in the history, share-link PINs, the media library | your computer | none |
| Describing photos, stickers, GIFs, videos | the **media model**, only when you press **Describe with AI** | one request per item to the model endpoint |
| Transcribing voice | the media model, when you press **Describe with AI** on a voice message, or record one | one request to the model endpoint |
| A reply | the **chat model**, when you send a message (or a visitor does) | requests to the model endpoint |
| Compacting a long history | the chat model, as part of a reply, when the history exceeds the budget | one extra request to the model endpoint |

Importing never calls a model. The first call happens when you send a message, press **Describe** or record a voice
message. A reply sends the model your exported history (up to the [history budget](../reference/configuration#settings-only)),
the style profile and the media descriptions. So **where the model runs decides where your chat goes**: a cloud
endpoint receives it, a model on your computer doesn't.

## Run exclusively offline

1. Install [Ollama](https://ollama.com) or [LM Studio](https://lmstudio.ai), or run llama.cpp's `llama-server`.
2. Download a **chat model that supports tool calling**, and a **vision model** if you want photos and stickers described.
3. In **Settings** pick the endpoint and the model, then switch **Offline** on:

| Server | Base URL |
|---|---|
| Ollama | `http://localhost:11434/v1` |
| LM Studio | `http://localhost:1234/v1` |
| llama.cpp `llama-server` | `http://localhost:8081/v1` (its default port `8080` is iRoof.top's: start it with `llama-server --port 8081`) |

4. Press **Test**. It sends a tiny request and shows the model and host that answered. You can switch the network off
   and chat on.

The **desktop app** is set up in Settings. It doesn't read your shell or a `.env` file (see [Desktop app](./desktop)),
though it does honour `ROOFTOP_OFFLINE` and `OPENAI_*` if they are in the environment it was started with. A server
reads the environment (and `.env`):

::: code-group

```sh [Homebrew / cargo server]
# --offline is the same as ROOFTOP_OFFLINE=1
OPENAI_BASE_URL=http://localhost:11434/v1 OPENAI_MODEL=llama3.1 rooftop --offline
```

```sh [Docker]
docker run -p 8080:8080 -v rooftop-data:/data \
  --add-host=host.docker.internal:host-gateway \
  -e ROOFTOP_OFFLINE=1 \
  -e OPENAI_BASE_URL=http://host.docker.internal:11434/v1 -e OPENAI_MODEL=llama3.1 \
  ghcr.io/irooftop/rooftop
```

:::

In Docker, `localhost` is the container itself, so use `host.docker.internal` (the `--add-host` flag is needed on Linux
only). If Ollama refuses the connection, make it listen on all interfaces (`OLLAMA_HOST=0.0.0.0`).

### What Offline mode enforces

| | Offline mode |
|---|---|
| Chat, history summary, media description, voice transcription | refused unless the endpoint is [local](#local-means) |
| MCP servers reached over HTTP | refused unless local, shown as an error in Settings |
| Internet sharing (`<name>.iroof.top`) | off: the relay client doesn't start, and the switch is locked |
| Telegram bot, own or shared | not started |
| GIFs by URL | the AI can't send a GIF from a URL that isn't local |
| Wi‑Fi share links | work |

A refused request fails before any connection is made, and the message says why: `Offline mode: https://openrouter.ai
is not a local endpoint`. A redirect from a local endpoint to another host is refused too. A small **Offline**
badge sits in the sidebar header while the mode is on. The setting is saved. `--offline` / `ROOFTOP_OFFLINE=1` turn it
on regardless of Settings, and Settings can't switch that off.

#### Local means

`localhost`, loopback (`127.0.0.0/8`, `::1`), private network addresses (`10/8`, `172.16/12`, `192.168/16`, `fc00::/7`),
link-local addresses, and the names `*.local` and `*.internal` (such as `host.docker.internal`). A model on another
computer in your home network counts as local, because it never leaves your network. Names are judged by how they are
written, they are not looked up.

### What Offline mode can't check

* **stdio MCP servers** are programs on your computer. iRoof.top starts them but can't see what they connect to, so
  only add ones you trust to stay local.
* **Voice transcription** uses the chat API (an `input_audio` part), not the Whisper `/audio/transcriptions` API.
  It needs a local server and model that accept audio there. If yours doesn't, the voice message is still saved and
  sent, only its text is missing.
* **Image and speech generation** exist only as [MCP tools](../features/mcp). Offline, use a local MCP server for them.
* The people you share with open your Wi‑Fi links in their own browsers. Offline mode governs this app, not their devices.

## Share with friends safely

| Link | What happens |
|---|---|
| **Wi‑Fi** `http://<your IP>:8737/s/<name>` | never leaves your local network |
| **Internet** `https://<name>.iroof.top/s/<name>` | the app opens one outgoing WebSocket to the hub, which forwards the visitor's HTTP requests and your app's responses over it |

With an Internet link, your export, your chats and the model stay on your computer, and the hub stores none of them.
The hub keeps only an account record: your name, a hash of your secret and when it last saw you.

Be clear about what that does *not* give you. The hub terminates TLS, so, like any reverse proxy, it technically sees
the traffic passing through: what the visitor types, the persona's replies, and whatever history the link lets
visitors read (**Visitors can read the export**, default **nothing**). It forwards only the visitor pages (`/s/…`,
`/api/s/…`, page assets) and drops cookies in both directions, and your owner UI and settings are never reachable
through it. If that is too much trust, use Wi‑Fi links, or [run your own relay hub](../reference/relay-hub).
[More on share links](../features/share-links).

With **Offline mode** the iroof.top relay is off. Wi‑Fi links keep working.

## The Telegram bot

[Answer from Telegram](../features/telegram) posts to your group:

* new **visitor messages** (name, link name, text, or a media label with its description or transcript, and a short
  preview of the message they replied to), and the AI's **replies**;
* never the exported history. Old messages are not transmitted.

Bots have no end-to-end encryption: the text passes through Telegram in plain text. With the **shared bot** it also
passes through the hub, which keeps only which group belongs to which account. An **own bot** talks straight to
Telegram, without the hub. Offline mode turns both off.

## Where your data goes

| Data | Where it goes |
|---|---|
| Exports, chats, settings, API key | the data directory on your computer |
| Import and style profile | nowhere |
| History, style profile, your messages | the model endpoint you chose, on each reply |
| Photos and voice | the media model endpoint, on **Describe** or when you record a voice message |
| Visitor messages on a Wi‑Fi link | your computer and your model endpoint |
| Visitor messages on an Internet link | the hub (in transit), your computer, your model endpoint |
| Visitor messages with Telegram | additionally Telegram, and the hub for the shared bot |
| Offline mode | local model endpoint only, Wi‑Fi links only, no Telegram |
