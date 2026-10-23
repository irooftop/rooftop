# Share links

**Share** in the chat header creates links so other people can talk to the persona, for example:

* a friend judges how well the AI writes **in your style**;
* a friend talks to **a copy of themselves**.

A link is `https://<your host>/s/<name>`. Visitors see only the chat: no sidebar, settings, notes, media
descriptions, prompt, token usage or other chats.

## Options

| Option | Default | |
|---|---|---|
| Who can open it | this computer | **Wi‑Fi** and **Internet** switches, right in the editor ([below](#open-links-on-other-devices)) |
| Link | random | pick a name such as `alex` |
| Display name | persona name | the real names from the export are not shown |
| PIN | none | 4–64 characters, stored as an Argon2id hash. ✨ generates 6 digits |
| Conversations | private | **Private**: one conversation per visitor. **Shared room**: everyone in one conversation |
| Visitors can read the export | nothing | nothing, the last *N* messages, or everything |
| Limits | 50 visitors, 200 answers each | every answer costs tokens on your key. `0` = unlimited |
| Visitors can let them write first | off | shows the 💬 button: the persona sends a message without one from the visitor |
| Visitors can rewrite an answer | off | shows the ↻ button: the persona's last answer is written again. Both use an answer of the limit, and the server refuses them while they are off |
| Expires / active | never / on | pause or expire a link without deleting it |
| Telegram group | off | [answer from Telegram](./telegram) |

You can **Read** every visitor conversation in the Share dialog, and **End** one to revoke it. Each visitor shows
the address they last wrote from; when ending a conversation, **Also block** keeps that address out of the link
(the start page, new conversations and ones already open) until you **Unblock** it. On a shared network one
address may be several people. Addresses are stored with the link on your computer only.

## Who the persona talks to

A visitor is not you. When Mia opens your link, the persona knows that **Mia** is writing, that the link
came from you, and that the history and your notes are about you and them, not about Mia: it meets her
as someone new. Without a name she is just a guest.

| | The persona sees |
|---|---|
| Private link | one conversation with the visitor, under the name they gave (or "Guest") |
| Shared room | a group chat: every message carries its sender's name, and people can join at any time |
| Your export history | your messages under your name, then the visitor's, so they're never mixed up |

The system prompt is fixed for each conversation, so providers keep caching it: the visitor's name is set
when the conversation starts, and in a room the names travel with the messages. A visitor who calls themselves
like the persona or like you becomes e.g. "Alex (guest)", a second "Ann" in a room "Ann 2".

## Invites

| Link | Opens |
|---|---|
| `/s/alex` | the start page |
| `/s/alex#pin=2468` | the chat right away. The PIN is after `#`, so it never reaches the server or logs |
| `/s/alex#name=Greg&pin=2468` | the same, with the visitor's name filled in (`?name=Greg` works too) |

The **QR** button of a link opens a code to scan with a phone, for every address that works. Right after saving,
**Copy invite with PIN** copies the link with the PIN.

## Open links on other devices

On a server, links simply use the server's address. The desktop app (and `rooftop` on `127.0.0.1`) runs only on your
computer, so the Share dialog and the link editor have two switches. Each says whether it is **Live**,
**Connecting** or **Not working**, and the editor shows the address the link will have right away.

```
                    ┌──────────── your computer ────────────┐
 phone on Wi‑Fi ───▶│ :8737  visitor pages only             │
                    │                                       │
 anyone ──▶ name.iroof.top ══ one outgoing WebSocket ══▶ app │
                    │                                       │
                    │ 127.0.0.1  owner UI, settings, API    │
                    └───────────────────────────────────────┘
```

| | Wi‑Fi and local network | Internet |
|---|---|---|
| Address | `http://<your IP>:8737/s/<name>` | `https://<name>.iroof.top/s/<name>` |
| Who | devices on the same network | anyone, while the app runs |
| How | a second listener that serves only visitor pages | one outgoing WebSocket to the relay, nothing listens on the internet |
| Your data | stays on your computer | stays on your computer, but visitor messages pass through the relay |

* The Wi‑Fi port stays the same across restarts, so printed QR codes keep working. On macOS, allow incoming
  connections if the firewall asks.
* The Internet name is yours to pick and change (**Change** next to the address). The relay forwards only
  `/s/…`, `/api/s/…` and page assets, drops cookies both ways, and shows an "offline" page while the app is closed.
* Want your own relay instead of iroof.top? See [Relay hub](../reference/relay-hub).
* **Offline mode** turns the Internet switch off and locks it, Wi‑Fi keeps working. What the relay can see:
  [Offline & privacy](../guide/offline#share-with-friends-safely).

## Protection

| Guard | Limit |
|---|---|
| Wrong PINs from one IP | after 5: locked 1 min, then 2, 4… up to 1 h |
| Wrong PINs on a link, all IPs | 20 in 15 min: locked 5 min, doubling up to 24 h |
| Messages per visitor | 30 per minute, 10 answers per minute |
| New conversations | 20 per hour per IP |

Counters live in memory. A 6-digit PIN can't be guessed in any reasonable time. Behind a reverse proxy set
`ROOFTOP_TRUST_PROXY=true`, so the counters see real client IPs.

Visitor sessions use a random 26-character token in `/s/<name>/<token>`, remembered by the browser and kept from
leaking by `Referrer-Policy: no-referrer`. Visitors can load only the media they may see.
