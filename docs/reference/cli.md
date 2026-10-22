# CLI

```text
rooftop [serve] [OPTIONS]        run the web server (the default)
rooftop parse <DIR>              print the normalized export as JSON
rooftop convert [OPTIONS] <DIR>  print the {model, messages, tools} for an export
```

## `serve`

| Flag | Variable | Default |
|---|---|---|
| `--addr` | `ROOFTOP_ADDR` | `127.0.0.1:8080` |
| `--data` | `ROOFTOP_DATA` | `./data` |
| `--import` | `ROOFTOP_IMPORT` | – |
| `--password` | `ROOFTOP_PASSWORD` | – |
| `--max-upload-mb` | `ROOFTOP_MAX_UPLOAD_MB` | `2048` |
| `--trust-proxy` | `ROOFTOP_TRUST_PROXY` | `false` |
| `--telegram-token` | `TELEGRAM_BOT_TOKEN` | – |
| `--telegram-chat` | `TELEGRAM_CHAT_ID` | every group the bot joins |
| `--telegram-api` | `TELEGRAM_API_URL` | `https://api.telegram.org` |
| `--relay-domain` | `ROOFTOP_RELAY_DOMAIN` | – |
| `--relay-bot-token` | `ROOFTOP_RELAY_BOT_TOKEN` | – |
| `--relay-max-accounts` | `ROOFTOP_RELAY_MAX_ACCOUNTS` | `5000` |

What each one does: [Configuration](./configuration).

## `parse` and `convert`

```sh
rooftop parse   ./examples/telegram                          # normalized JSON
rooftop convert ./examples/telegram --persona "Jonas Weber"  # what the model receives
```

| `convert` flag | Default | |
|---|---|---|
| `--persona` | the chat title / other person | who the LLM speaks as |
| `--user` | the other participant | who the human plays |
| `--budget` | `120000` | history budget in characters |

In the app, the `{ }` button shows the same JSON for the current chat.
