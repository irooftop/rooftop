# Export a chat

<div class="wide">

![Telegram Desktop: open the chat, click ⋯ in the top right corner, choose Export Chat History…, then Export](/img/telegram-export.svg)

</div>

## Telegram

In **Telegram Desktop** (macOS, Windows, Linux): open the chat → **⋯** → **Export Chat History…** → **Export**.

* Keep photos, videos, voice messages, stickers and GIFs switched on: the persona uses them.
* Both formats work: **HTML** (`messages.html`) and machine-readable **JSON** (`result.json`).
* Drop the export folder, or a `.zip` of it, onto the page.

## WhatsApp

On the phone: open the chat → **⋮ → More → Export chat** (iPhone: tap the contact name → **Export Chat**) →
**Include media**. Send the `.zip` to your computer and drop it onto the page.

A chat exported without media is a single `.txt`, and that works too.

## What is understood

| Source | Files |
|---|---|
| Telegram HTML | `messages.html`, `messages2.html`, … |
| Telegram JSON | `result.json` (used when a folder has no `messages.html`) |
| WhatsApp | `WhatsApp Chat with <name>.txt` (Android) or `_chat.txt` (iOS), in a folder, a `.zip` or alone |

* **Telegram**: text with line breaks, links, quotes and code; replies and partial quotes; forwards; edits; service
  messages; reactions; link previews; photo, video, round video, voice, audio, file, sticker (`.webp`, `.webm`,
  `.tgs`), GIF, location, contact, poll and call.
* **WhatsApp**: Android and iOS layouts, 12- and 24-hour clocks and both day/month orders, detected per file.
  Multi-line messages, edits and attachments are kept. Deleted messages and the encryption notice are skipped,
  and `<Media omitted>` is marked as unavailable.

Media files are hashed, so a sticker sent five times becomes one library item (`stk_…`, `gif_…`, `pho_…`).

::: info Reactions in HTML exports
HTML exports don't say who reacted. In one-to-one chats a reaction is attributed to the other participant, and
to both when the count is 2.
:::
