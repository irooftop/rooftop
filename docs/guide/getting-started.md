# Getting started

## Install

| | Command |
|---|---|
| **Desktop app**, macOS | `brew install --cask irooftop/tap/irooftop` |
| **Desktop app**, Windows | [Installer (.exe)](https://github.com/irooftop/rooftop/releases/latest/download/iRoof.top_x64-setup.exe) or [.msi](https://github.com/irooftop/rooftop/releases/latest/download/iRoof.top_x64_en-US.msi) |
| **Desktop app**, Linux | [AppImage](https://github.com/irooftop/rooftop/releases/latest/download/iRoof.top_amd64.AppImage), [.deb](https://github.com/irooftop/rooftop/releases/latest/download/iRoof.top_amd64.deb) or [.rpm](https://github.com/irooftop/rooftop/releases/latest/download/iRoof.top.x86_64.rpm) |
| **Server**, macOS and Linux | `brew install irooftop/tap/rooftop` (then `brew services start rooftop`) |
| **Server**, anywhere with Rust ≥ 1.88 | `cargo install rooftop` |
| **Server**, container | `docker pull ghcr.io/irooftop/rooftop` ([Docker](./docker)) |

The desktop app needs no terminal: see [Desktop app](./desktop).

## Run

```sh
export OPENAI_BASE_URL=https://openrouter.ai/api/v1   # the default; any OpenAI-compatible API works
export OPENAI_API_KEY=…
export OPENAI_MODEL=openai/gpt-6-luna                 # must support tool calling
rooftop                                               # http://127.0.0.1:8080
```

The variables can also live in a `.env` file next to where you start `rooftop`, or be set later in **Settings**.
**Test** in Settings sends a tiny request and tells you in plain words what is wrong, for example
"openrouter.ai rejected the API key (401)".

Want nothing to leave your computer? Use a local model and [Offline mode](./offline).

## Your first chat

```
 ┌──────────────┐     ┌────────────────────┐     ┌──────────────────────┐
 │ 1. Export    │ ──▶ │ 2. Drop it onto    │ ──▶ │ 3. New chat: pick    │
 │ the chat     │     │    the page        │     │ who the AI plays     │
 └──────────────┘     └────────────────────┘     └──────────────────────┘
```

1. [Export a chat](./export) from Telegram Desktop or WhatsApp.
2. Drop the folder, `.zip` or WhatsApp `.txt` onto the page (or start with `rooftop --import <folder>`).
3. Press **New chat** and choose the persona: the other person, or yourself ("chat with myself in their style").

::: tip Try it without your own chats
The repository has two fictional exports in [`examples/`](https://github.com/irooftop/rooftop/tree/main/examples):
Jonas and Priya cook a lasagna on Telegram, Sofia and Daniel spend a weekend on WhatsApp.

```sh
git clone https://github.com/irooftop/rooftop && cd rooftop
rooftop --import ./examples/telegram
```
:::

## What's in the window

| Button | What it does |
|---|---|
| **Share** | [Links](../features/share-links) for friends to talk to the persona |
| **Media library** | Every sticker, GIF, photo and voice message with what the AI knows about it |
| **Notes** | Things about the person the export doesn't say. The persona reads them, visitors never do |
| **`{ }`** | The exact JSON sent to the model, and the tokens used |
| **Rewrite** | Drops the persona's last answer and writes it again |
| **Let them write first** | The persona starts the conversation |
