# Configuration

Environment variables, a `.env` file in the working directory, or the matching `rooftop` flags
([CLI](./cli)). Values set in **Settings** win over the environment for the LLM endpoint, model, key, temperature,
history budget, tool steps, MCP servers and the Telegram bot. `TELEGRAM_BOT_TOKEN` wins over Settings.

## Model

| Variable | Default | |
|---|---|---|
| `OPENAI_BASE_URL` | `https://openrouter.ai/api/v1` | any OpenAI-compatible API: OpenAI, OpenRouter, Gemini, Mistral, DeepSeek, Groq, Ollama, LM Studio, vLLM… |
| `OPENAI_API_KEY` | – | bearer token |
| `OPENAI_MODEL` | `openai/gpt-6-luna` | must support tool calling |
| `OPENAI_TEMPERATURE` | provider default | |
| `OPENAI_MEDIA_MODEL` | `OPENAI_MODEL` | describes images and transcribes voice, e.g. `xiaomi/mimo-v2.6-flash` |

## Server

| Variable | Default | |
|---|---|---|
| `ROOFTOP_ADDR` | `127.0.0.1:8080` (`0.0.0.0:8080` in Docker) | with `0.0.0.0:<port>` the log shows the LAN URL |
| `ROOFTOP_DATA` | `./data` (`/data` in Docker) | exports, chats, settings |
| `ROOFTOP_IMPORT` | – | exports to import on start, `:`-separated, idempotent |
| `ROOFTOP_PASSWORD` | – | HTTP Basic auth for the owner UI (any user name). **Set it on a public server** |
| `ROOFTOP_MAX_UPLOAD_MB` | `2048` | upload limit |
| `ROOFTOP_TRUST_PROXY` | `false` | client IP from `X-Forwarded-For` / `X-Real-IP`, only behind a reverse proxy |
| `ROOFTOP_FFMPEG` | `ffmpeg` on `PATH`, then Homebrew, `/usr/local`, `/usr/bin` | frames of GIFs and videos (optional) |
| `TZ` | UTC | time zone of live messages |

## Telegram

| Variable | Default | |
|---|---|---|
| `TELEGRAM_BOT_TOKEN` | – | your bot from [@BotFather](https://t.me/BotFather) ([Answer from Telegram](../features/telegram)) |
| `TELEGRAM_CHAT_ID` | any group the bot joins | allowed group ids, comma-separated |
| `TELEGRAM_API_URL` | `https://api.telegram.org` | e.g. a self-hosted Bot API server |

## Relay

| Variable | Default | |
|---|---|---|
| `ROOFTOP_RELAY_URL` | `https://iroof.top` | relay for **Internet** sharing and the shared bot |
| `ROOFTOP_RELAY_DOMAIN` | – | run a [relay hub](./relay-hub) for this domain |
| `ROOFTOP_RELAY_BOT_TOKEN` | – | the hub's shared bot; must differ from `TELEGRAM_BOT_TOKEN` |
| `ROOFTOP_RELAY_MAX_ACCOUNTS` | `5000` | most names a hub keeps |

## Settings only

| Setting | Default | |
|---|---|---|
| History budget | `120000` characters | how much history is replayed before [memory](../features/how-it-works#long-chats) kicks in |
| Max tool steps | `6` | LLM rounds per answer (search, MCP tool, reply…) |
