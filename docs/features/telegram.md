# Answer from Telegram

A share link can post its visitors' messages to a Telegram group. You answer there yourself, or the AI answers when
a countdown ends. Visitors see the same "typing…" either way and can't tell who answered: a "human or AI?" game, or a
support desk where the AI handles the obvious questions.

```
 visitor ──▶ share link ──▶ 🤖 bot ──▶ your Telegram group
                                        ├─ ✍️ Reply         you answer
                                        └─ 🤖 AI reply (10 s) the AI answers now, or when the countdown ends
```

## Set up

**Settings → Telegram** shows the bot, whether it is connected, and every group it is in.

| Mode | Setup | Messages go |
|---|---|---|
| **Shared bot** | none | through the relay (iroof.top), in plain text like any bot |
| **Own bot** | create one with [@BotFather](https://t.me/BotFather), paste the token, **Connect** | straight between your app and Telegram |
| `TELEGRAM_BOT_TOKEN` | on a server; `TELEGRAM_CHAT_ID` limits the groups | straight; wins over Settings |

Then press **Add to a group**:

1. Open the link in the **Telegram app**, in **Telegram Web**, or scan the QR code with your phone. Both links can
   be copied.
2. Pick a group (or create one) and confirm.
3. The dialog turns green, and the group appears in Settings and in every share link.

With the shared bot the link carries a one-time code, valid for an hour, that ties the group to your app only.

Finally pick the group under **Telegram group** in a share link, and set after how many seconds the AI answers
(`0` = only when you press **AI reply**).

::: tip Privacy
Only new visitor messages and the AI's answers are posted to the group, never the exported history. Offline mode turns
the bot off. Details: [Offline & privacy](../guide/offline#the-telegram-bot).
:::

::: tip No public address needed
Both kinds of bot use long polling, so this works from a laptop. A bot token works in one app at a time.
:::

## Three ways to answer

1. Reply to the bot's "✍️ … reply to this message" prompt.
2. Reply to the visitor's message (or an AI answer) at any time, without pressing **Reply**.
3. Press **Reply**, then just write. For 15 minutes your messages go to that visitor, and for 2 more minutes after each
   answer, so an answer can span several messages.

Way 3 needs the bot to see plain group messages: turn privacy mode off (@BotFather → `/setprivacy` → Disable, then
re-add the bot) or make it an admin. Ways 1 and 2 always work.

::: warning Use a group you trust
Anyone in the group can answer. Only text answers are relayed, and AI answers count against the link's limit.
:::
