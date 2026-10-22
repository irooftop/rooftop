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

## Large exports

Exports of 100,000+ messages and many gigabytes work the same way: drop the folder. The page shows what is happening:

1. **Reading folder…** counts the files (the browser can take a moment to list tens of thousands).
2. **Uploading 1.2 GB of 3.4 GB (35%)**: the files are copied to the app's data folder. There is no size limit when you run it on your own computer.
3. **Reading messages: page 37 of 121 · 36,000 messages**: all pages are read once, in parallel.
4. **Checking media** and **Moving media**: files are hashed to find duplicates and moved (not copied twice) into the data folder.
5. **Saving**, then the *New chat* dialog opens.

Only one import runs at a time; an error stays visible in the drop zone and you can choose the folder again. Importing the same export twice is detected and does nothing.

Nothing leaves your computer during an import and no model is called. Media is described only when you press **Describe** in the media library.

With `rooftop --import <folder>` the folder is read in place and its files are copied, never moved; progress is logged.

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
