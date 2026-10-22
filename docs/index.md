---
layout: home

hero:
  name: iRoof.top
  text: Talk to the people you miss
  tagline: Export a Telegram or WhatsApp chat. An LLM continues it in the style of one person, with their stickers, GIFs, reactions and voice.
  image:
    light: /img/logo_light.svg
    dark: /img/logo_dark.svg
    alt: iRoof.top
  actions:
    - theme: brand
      text: Get started
      link: /guide/getting-started
    - theme: alt
      text: How it works
      link: /features/how-it-works
    - theme: alt
      text: GitHub
      link: https://github.com/irooftop/rooftop

features:
  - icon: 💬
    title: Their style, not just their words
    details: A style profile from the whole export, and every sticker, GIF, reply and reaction replayed as a tool call.
    link: /features/how-it-works
  - icon: 🔗
    title: Share links
    details: Let a friend talk to the persona at /s/name, with a PIN and limits. On the Wi‑Fi or at name.iroof.top.
    link: /features/share-links
  - icon: ✈️
    title: Answer from Telegram
    details: Visitor messages land in your Telegram group. Reply yourself, or let the AI answer after a countdown.
    link: /features/telegram
  - icon: 🎙️
    title: Photos and voice
    details: Describes stickers and photos, transcribes voice messages, and can send new ones through MCP tools.
    link: /features/media
  - icon: 🧩
    title: Any model, any MCP server
    details: Any OpenAI-compatible endpoint. MCP over Streamable HTTP or stdio, every revision from 2024-11-05 to 2026-07-28.
    link: /features/mcp
  - icon: 📦
    title: One small binary
    details: Rust server with an embedded web UI, a desktop app for macOS, Windows and Linux, and an Alpine Docker image.
    link: /guide/getting-started
---

<div class="wide">

![A Telegram or WhatsApp export goes into iRoof.top, an LLM writes the answers, and the chat goes on](/how-it-works.svg)

</div>

## In one minute

```sh
brew install irooftop/tap/rooftop
export OPENAI_API_KEY=…            # OpenRouter by default, any OpenAI-compatible API works
rooftop                            # then open http://127.0.0.1:8080 and drop an export onto the page
```

::: tip A note on care
This is meant for staying close to people who are far away. The persona stays in character but answers honestly if
you sincerely ask whether you are talking to an AI, and responds with care if you seem to be in distress. Chats stay
in your data directory, and message content goes only to the LLM endpoint you configure.
:::
