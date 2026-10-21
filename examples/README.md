# Example exports

Two made-up conversations, ready to import (drop the folder onto the page, or `rooftop --import <folder>`).
All people, photos and voices are fictional. Every character has a habit that a good persona should copy,
so you can check at a glance whether the model picked up the style.

## `telegram/`: Jonas and Priya cook grandma's lasagna

A Telegram Desktop HTML export (`messages.html`), 76 messages over two days (Friday 16 and Saturday
17 October 2026). Jonas invited his parents for dinner and promised his grandma's lasagna. Priya plans the
farmers market trip, takes over his flour-covered kitchen and gets him through the evening.

| Person | Habit to imitate |
|---|---|
| **Jonas Weber** (40 messages) | All lowercase, bursts of short bubbles, stretched words ("yesss", "nooo"). He never uses a question mark and asks with "or nah" instead. Calls Priya "boss", likes 🍝 and answers with GIFs. |
| **Priya Nair** (36 messages) | Full punctuation, plans as numbered lists, one-word replies "Noted." and "Right.", ETAs, dry humour. She sends voice messages when her hands are full. |

It contains every kind of content a Telegram export can have, in the same folders and file names:

* `photos/`: the recipe card, the market stall, the kitchen after the bread disaster, the finished green lasagna.
* `stickers/`: a video sticker (`.webm`), an animated sticker (`.tgs`) and a static one (`.webp`), each with the
  unreferenced thumbnail Telegram writes to `thumbs/`.
* `gifs/`: three GIFs. `voice_messages/`: Priya at the pasta stand. `round_video_messages/`: Priya at the market.
* `video_files/`: a 15-second video of the day. `files/`: Priya's plan as a PDF and Jonas's dinner music (`.mp3`).
* `webpage_thumbs/`: the picture of a link preview card.
* A forwarded channel post with bold text and a link, an edited message, replies and 18 reactions.

## `whatsapp/`: Sofia and Daniel, a weekend in love

An Android WhatsApp export folder (`WhatsApp Chat with Sofia Moreno.txt` plus the attached files), 39 messages
over two days (Saturday 18 and Sunday 19 October 2026): coffee by the window, sunset on the pier, plans for Wednesday.

| Person | Habit to imitate |
|---|---|
| **Sofia Moreno** (24 messages) | Lowercase, "hehe", calls him "sunshine", and ends almost every message with a heart whose colour matches her mood: 💛 happy, 💙 sleepy, 💜 missing him, ❤️ in love. |
| **Daniel Hart** (15 messages) | Proper sentences full of sea and lighthouse metaphors, calls her "Sof", and signs every message with "x". |

Media: 2 photos, a voice note, an animated sticker. The export also shows the usual WhatsApp details the
importer handles: the encryption notice, an edited message and a deleted one.

## How they were made

The photos were generated with `meta/muse-image` and the voice messages with `qwen/qwen-audio-3.0-tts-flash`
(voice `longanhuan_v3.6`). The GIFs and stickers are SVG animations rendered frame by frame, except the `.tgs`
sticker, which is a hand-written Lottie file. The video is an `ffmpeg` slideshow of the four Telegram photos, the
round video pans across the market photo, and the PDF was printed from HTML. The dinner music is an original jingle
synthesized for this example. The recipe blog, the channel and the band are fictional.

`tests/examples.rs` checks that both exports import completely and that every habit above is really in the text.
