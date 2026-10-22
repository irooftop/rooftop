# Photos & voice

Some people mostly send photos, others mostly talk in voice messages. iRoof.top keeps both habits.

## Understanding them

In the media library, **Describe with AI** describes stickers, GIFs and photos and **transcribes voice messages** with
the *media model*, which must accept images and audio. Set it in Settings or with `OPENAI_MEDIA_MODEL`, for example
`google/gemini-3.5-flash-lite` on OpenRouter.

```text
<voice message 0:05: «what was said»>
```

When one kind dominates, the style profile says so, e.g. "talks mostly in voice messages (10 of 10)".

## Your voice messages

With an empty composer the send button becomes a **microphone**, as in Telegram. The recording is converted to WAV in
the browser, uploaded and transcribed. Visitors of share links can record too.

::: info
Browsers allow recording only on `https://` or `localhost`.
:::

## The persona's photos and voice messages

Connect an MCP server that makes images and one that speaks:

```json
{
  "img": {"command": "npx", "args": ["-y", "@dtst/img"],
          "env": {"OPENAI_BASE_URL": "https://openrouter.ai/api/v1", "OPENAI_API_KEY": "${OPENAI_API_KEY}", "OPENAI_MODEL": "meta/muse-image"}},
  "tts": {"command": "npx", "args": ["-y", "@dtst/tts"],
          "env": {"OPENAI_BASE_URL": "https://openrouter.ai/api/v1", "OPENAI_API_KEY": "${OPENAI_API_KEY}",
                  "OPENAI_MODEL": "google/gemini-3.8-flash-lite-tts", "OPENAI_VOICE": "Laomedeia"}}
}
```

The persona then gets two more actions:

| Action | What it does |
|---|---|
| `send_voice {text}` | calls the speech tool (WAV, retried as mp3) and sends a voice message |
| `send_photo {prompt, caption?}` | calls the image tool and sends a photo |

* With a speech tool connected, the persona's own transcribed voice messages are replayed as `send_voice`, so the
  model sees the habit in the form it can continue.
* **Voice** in *Chat details* (for example `Puck`) gives the persona their own voice.
* Any other image or audio an MCP tool returns becomes chat media `gen_…` (checked by content, up to 25 MB, deleted
  with the chat).

::: tip Costs
Each photo or voice message is an extra paid call: about $0.01 per image with `meta/muse-image`, and a few seconds of TTS.
:::
