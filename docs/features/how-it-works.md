# How it works

<div class="wide">

![How iRoof.top works](/how-it-works.svg)

</div>

Every export becomes the same normalized messages. Then every action of the persona is replayed to the model as a
**tool call**, so it learns *how* the person interacts, not just what they write, and answers the same way.

## Actions as tool calls

| What the persona did | Tool call in the history and in live answers |
|---|---|
| text bubble, maybe a reply or partial quote | `send_message {text, reply_to?, quote?}` |
| sticker | `send_sticker {sticker_id, reply_to?}` |
| GIF from the library, or a URL from an MCP tool | `send_gif {gif_id \| url, caption?}` |
| photo, video, voice, audio, file | `send_media {media_id, caption?}` |
| forwarded post | `forward_message {message_id}` |
| reaction | `react {message_id, emoji}` |
| *(recall older history)* | `search_history {query}` |

Several calls in one turn become several bubbles. The other side's messages become compact lines the ids refer to:

```text
#52108 19:08 ↩ reply to #52107 (your: "grandmas lasagna 🍝"): You have made lasagna exactly zero times.
<sticker stk_156dbb381a>: a cat in a chef's hat, stirring a pot
reacted 😂 to #52107 ("grandmas lasagna 🍝")
```

## The system prompt

| Part | |
|---|---|
| Who to be | estimate the persona's knowledge and style, and never know more than they would |
| Style profile | from the *whole* export: message length, capitalisation, punctuation, emoji and sticker use, reply and reaction habits, bubbles per turn, verbatim samples |
| Library | stickers, GIFs and media with how they were used |
| Notes | what you wrote about the person |

The prompt is **static** for a chat: no clock, no counters. The model reads the time from the `— day —` separators
and the `HH:MM` stamps, and each request is a byte-identical prefix of the next one, so provider prompt caches keep
hitting.

## Long chats

```
 history budget ───────────────────────────────────────────────▶
 ┌──────────────────────┬────────────────────────────────────────┐
 │ memory (≤ 350 words) │ newest turns, cut to 60% of the budget │
 └──────────────────────┴────────────────────────────────────────┘
   relationship, facts,    the cut point stays put for a long
   topics, running jokes   stretch, so the cached prefix does too
```

When the replayed history outgrows the budget, the next reply first folds the oldest turns (and the previous memory)
into a short summary. Nothing is spent on import or when you open a chat. The style profile still comes from the whole
export, and `search_history` reaches every old message. Token `usage` (prompt, cached, completion) is shown in `{ }`.

## Media library

The chat model reads text, so a sticker is just `<sticker stk_…>` until it is described. **Describe with AI** sends
the images of the missing items to the media model, which must support vision.

* GIFs, video stickers and videos go as **4 frames in order**, so the model sees the motion. This needs
  [ffmpeg](https://ffmpeg.org) (`brew install ffmpeg`, `apt install ffmpeg`, `winget install ffmpeg`), otherwise
  the thumbnail is used.
* **Redo** describes one item again. Any description can be edited by hand.
* Descriptions belong to the export and serve all its chats.

See it yourself with the [CLI](../reference/cli): `rooftop convert <export> --persona "Jonas Weber"`.
