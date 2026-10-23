//! Converts an export (+ live continuation) into an OpenAI-compatible
//! `messages` array.
//!
//! * Messages of the persona become `assistant` turns made of tool calls
//!   (`send_message`, `send_sticker`, `send_gif`, `send_media`,
//!   `forward_message`, `react`), each followed by its `tool` result.
//! * Everything else becomes `user` turns: one line-oriented block per run of
//!   consecutive messages, with ids, times, replies, quotes, forwards, media
//!   and reactions spelled out so the model can reply to / react on them.

use crate::chat::{Chat, Item, find_message, timeline};
use crate::model::{ChatExport, LibraryItem, Media, MediaKind, Message};
use crate::style;
use crate::tools;
use chrono::{NaiveDate, NaiveDateTime};
use serde_json::{Value, json};
use std::fmt::Write;

/// The kept history is compacted down to this share of the budget, so the next
/// compaction (and the next cache miss) only happens after ~40% of the budget
/// of new conversation.
const LOW_WATERMARK: f32 = 0.6;
/// Max characters of transcript sent to the summarizer at once. On the first
/// compaction of a huge export only the most recent part is summarized, and older
/// details stay reachable through `search_history` and the style profile.
const MAX_SUMMARY_INPUT_CHARS: usize = 60_000;

#[derive(Default)]
pub struct PromptOptions {
    /// Approximate size limit (characters) of the replayed history.
    pub budget_chars: usize,
    /// Extra instructions describing connected MCP tools.
    pub extra_tools_note: Option<String>,
    /// Tools that create media, used to keep a persona's photo / voice habit.
    pub media_tools: crate::mcp::MediaTools,
}

pub struct Prompt {
    pub messages: Vec<Value>,
    /// History turns replaced by the summary (or trimmed).
    pub dropped_turns: usize,
    pub total_turns: usize,
    /// Set when the kept history exceeds the budget: the caller should
    /// summarize `transcript`, store [`crate::chat::Memory`] with `turns = to`
    /// and rebuild. Until then the prompt already trims at `to`.
    pub compaction: Option<Compaction>,
}

#[derive(Debug, Clone)]
pub struct Compaction {
    pub from: usize,
    pub to: usize,
    /// Readable transcript of turns `from..to` (tail-capped).
    pub transcript: String,
}

enum Turn {
    User(Vec<String>),
    Assistant(Vec<(String, String, Value, Value)>), // (call id, name, args, result)
}

impl Turn {
    fn to_values(&self) -> Vec<Value> {
        match self {
            Turn::User(lines) => vec![json!({"role": "user", "content": lines.join("\n")})],
            Turn::Assistant(calls) => {
                let mut v = vec![json!({
                    "role": "assistant",
                    "content": null,
                    "tool_calls": calls.iter().map(|(id, name, args, _)| json!({
                        "id": id, "type": "function",
                        "function": {"name": name, "arguments": args.to_string()}
                    })).collect::<Vec<_>>()
                })];
                for (id, _, _, result) in calls {
                    v.push(json!({"role": "tool", "tool_call_id": id, "content": result.to_string()}));
                }
                v
            }
        }
    }
}

fn parse_date(d: &Option<String>) -> Option<NaiveDateTime> {
    d.as_deref().and_then(|s| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok())
}

pub fn media_desc(media: &Media, export: &ChatExport) -> String {
    let kind = media.kind();
    let item: Option<&LibraryItem> = media.library_id.as_deref().and_then(|id| export.library_item(id));
    let mut s = format!("<{}", kind.label());
    if let Some(id) = &media.library_id {
        let _ = write!(s, " {id}");
    }
    if let Some(url) = &media.url {
        let _ = write!(s, " {url}");
    }
    let title = media.title.as_deref().filter(|t| *t != "GIF" && !matches!(kind, MediaKind::Voice | MediaKind::Video));
    if let Some(t) = title {
        let _ = write!(s, " \"{t}\"");
    }
    if let Some(st) = &media.status {
        let _ = write!(s, " {st}");
    }
    if let Some(d) = item.and_then(|i| i.description.as_deref()).or(media.description.as_deref()) {
        if matches!(kind, MediaKind::Voice | MediaKind::RoundVideo) {
            // Voice descriptions are transcripts: what was said.
            let _ = write!(s, ": «{d}»");
        } else {
            let _ = write!(s, ": {d}");
        }
    }
    if item.is_some_and(|i| !i.available) {
        s.push_str(" (file not in export)");
    }
    s.push('>');
    s
}

fn render_other(m: &Message, export: &ChatExport, chat: &Chat, multi: bool) -> String {
    if m.service {
        return format!("[service #{}] {}", m.id, m.text);
    }
    let mut head = format!("#{}", m.id);
    if multi {
        let _ = write!(head, " {}", m.from);
    }
    if let Some(d) = parse_date(&m.date) {
        let _ = write!(head, " {}", d.format("%H:%M"));
    }
    if m.edited {
        head.push_str(" (edited)");
    }
    if let Some(r) = m.reply_to {
        let _ = write!(head, " ↩ reply to #{r}");
        if let Some(t) = find_message(export, &chat.events, r) {
            let whose = if t.from == chat.persona { "your".to_string() } else { format!("{}'s", t.from) };
            let _ = write!(head, " ({whose}: \"{}\")", t.preview(60));
        }
    }
    if let Some(q) = &m.quote {
        let _ = write!(head, " quoting \"{q}\"");
    }
    if let Some(f) = &m.forwarded_from {
        if f.is_empty() {
            head.push_str(" [forwarded]");
        } else {
            let _ = write!(head, " [forwarded from {f}]");
        }
    }
    let mut body = Vec::new();
    if let Some(media) = &m.media {
        body.push(media_desc(media, export));
    }
    if !m.text.is_empty() {
        body.push(m.text.clone());
    }
    if let Some(lp) = &m.link_preview {
        let parts: Vec<&str> = [lp.site.as_deref(), lp.title.as_deref(), lp.description.as_deref()].into_iter().flatten().collect();
        body.push(format!("<link preview: {}>", parts.join(" — ")));
    }
    format!("{head}: {}", body.join("\n"))
}

fn persona_call(m: &Message, export: &ChatExport, voice_tool: bool) -> (String, Value, Value) {
    let ok = json!({"ok": true, "message_id": m.id});
    let mut args = serde_json::Map::new();
    let reply = |args: &mut serde_json::Map<String, Value>| {
        if let Some(r) = m.reply_to {
            args.insert("reply_to".into(), json!(r));
        }
    };
    if let Some(f) = &m.forwarded_from {
        let mut res = json!({"ok": true, "message_id": m.id, "preview": m.preview(80)});
        if !f.is_empty() {
            res["forwarded_from"] = json!(f);
        }
        return (tools::FORWARD_MESSAGE.into(), json!({"message_id": m.id}), res);
    }
    if let Some(media) = &m.media {
        let kind = media.kind();
        let caption = |args: &mut serde_json::Map<String, Value>| {
            if !m.text.is_empty() {
                args.insert("caption".into(), json!(m.text));
            }
        };
        // With a speech tool, the persona's transcribed voice messages replay as
        // `send_voice {text}`: the model learns the habit in the form it can repeat.
        let transcript = media
            .library_id
            .as_deref()
            .and_then(|id| export.library_item(id))
            .and_then(|i| i.description.clone())
            .or(media.description.clone());
        if voice_tool
            && kind == MediaKind::Voice
            && let Some(t) = transcript
        {
            args.insert("text".into(), json!(t));
            reply(&mut args);
            return (tools::SEND_VOICE.into(), Value::Object(args), ok);
        }
        match (kind, &media.library_id) {
            (MediaKind::Sticker, Some(id)) => {
                args.insert("sticker_id".into(), json!(id));
                reply(&mut args);
                return (tools::SEND_STICKER.into(), Value::Object(args), ok);
            }
            (MediaKind::Gif, id) => {
                match (id, &media.url) {
                    (Some(id), _) => args.insert("gif_id".into(), json!(id)),
                    (None, Some(url)) => args.insert("url".into(), json!(url)),
                    _ => None,
                };
                caption(&mut args);
                reply(&mut args);
                return (tools::SEND_GIF.into(), Value::Object(args), ok);
            }
            (k, Some(id)) if k.is_resendable() || id.starts_with("gen_") => {
                args.insert("media_id".into(), json!(id));
                caption(&mut args);
                reply(&mut args);
                // Media created in this chat are not in the library list of the
                // system prompt: the result says what was sent.
                let mut ok = ok;
                if media.description.is_some() {
                    ok["sent"] = json!(media_desc(media, export));
                }
                return (tools::SEND_MEDIA.into(), Value::Object(args), ok);
            }
            _ => {
                let mut text = media_desc(media, export);
                if !m.text.is_empty() {
                    text = format!("{text}\n{}", m.text);
                }
                args.insert("text".into(), json!(text));
                reply(&mut args);
                return (tools::SEND_MESSAGE.into(), Value::Object(args), ok);
            }
        }
    }
    args.insert("text".into(), json!(m.text));
    reply(&mut args);
    if let Some(q) = &m.quote {
        args.insert("quote".into(), json!(q));
    }
    (tools::SEND_MESSAGE.into(), Value::Object(args), ok)
}

/// Number of history turns (a memory with fewer turns than an export alone
/// produces stays valid for a new chat on that export).
pub fn history_turn_count(export: &ChatExport, chat: &Chat) -> usize {
    history_turns_inner(export, chat, false).len()
}

/// History turns with the calendar day that is current at the start of each turn.
fn history_turns(export: &ChatExport, chat: &Chat, voice_tool: bool) -> Vec<(Turn, Option<NaiveDate>)> {
    let turns = history_turns_inner(export, chat, voice_tool);
    let mut day = None;
    turns
        .into_iter()
        .map(|(t, d)| {
            let start = day.or(d);
            if let Some(d) = d {
                day = Some(d);
            }
            (t, start)
        })
        .collect()
}

/// Turns plus the last day separator emitted inside each turn.
fn history_turns_inner(export: &ChatExport, chat: &Chat, voice_tool: bool) -> Vec<(Turn, Option<NaiveDate>)> {
    // Names on the other side's lines in group exports, and in share-link conversations:
    // the host in the export history, then the visitor(s).
    let multi = export.participants.len() > 2 || !matches!(audience(chat), Audience::Owner);
    let mut turns: Vec<(Turn, Option<NaiveDate>)> = Vec::new();
    let mut last_day: Option<chrono::NaiveDate> = None;
    let mut react_n = 0usize;
    for item in timeline(export, &chat.events) {
        let persona = item.actor() == chat.persona && !matches!(item, Item::Msg(m) if m.service);
        if let Item::Msg(m) = &item
            && let Some(d) = parse_date(&m.date)
            && last_day != Some(d.date())
        {
            last_day = Some(d.date());
            match turns.last_mut() {
                Some((Turn::User(lines), day)) => {
                    lines.push(day_sep(d.date()));
                    *day = Some(d.date());
                }
                _ => turns.push((Turn::User(vec![day_sep(d.date())]), Some(d.date()))),
            }
        }
        if persona {
            let (id, name, args, result) = match &item {
                Item::Msg(m) => {
                    let (name, args, result) = persona_call(m, export, voice_tool);
                    (format!("call_{}", m.id), name, args, result)
                }
                Item::React { target, emoji, removed, .. } => {
                    react_n += 1;
                    let emoji = if *removed { String::new() } else { emoji.clone() };
                    (
                        format!("call_r{target}_{react_n}"),
                        tools::REACT.to_string(),
                        json!({"message_id": target, "emoji": emoji}),
                        json!({"ok": true}),
                    )
                }
            };
            match turns.last_mut() {
                Some((Turn::Assistant(calls), _)) => calls.push((id, name, args, result)),
                _ => turns.push((Turn::Assistant(vec![(id, name, args, result)]), None)),
            }
        } else {
            let line = match &item {
                Item::Msg(m) => render_other(m, export, chat, multi),
                Item::React { target, emoji, from, removed } => {
                    let whose =
                        find_message(export, &chat.events, *target).map(|t| format!(" (\"{}\")", t.preview(40))).unwrap_or_default();
                    let who = if multi { format!("{from} ") } else { String::new() };
                    if *removed {
                        format!("{who}removed reaction {emoji} from #{target}{whose}")
                    } else {
                        format!("{who}reacted {emoji} to #{target}{whose}")
                    }
                }
            };
            match turns.last_mut() {
                Some((Turn::User(lines), _)) => lines.push(line),
                _ => turns.push((Turn::User(vec![line]), None)),
            }
        }
    }
    turns
}

fn day_sep(d: NaiveDate) -> String {
    format!("— {} —", d.format("%A, %-d %B %Y"))
}

fn library_section(export: &ChatExport, chat: &Chat) -> String {
    let mut s = String::new();
    let fmt_item = |i: &LibraryItem| {
        let by: Vec<String> =
            i.senders.iter().map(|(n, c)| if *n == chat.persona { format!("you×{c}") } else { format!("{n}×{c}") }).collect();
        let mut line = format!("- {} ({})", i.id, by.join(", "));
        if let Some(t) = &i.title {
            let _ = write!(line, " \"{t}\"");
        }
        if let Some(d) = &i.description {
            let _ = write!(line, ": {d}");
        } else if let Some(first) = i.uses.first().and_then(|id| find_message(export, &chat.events, *id)) {
            // Without a visual description, the surrounding context hints at meaning.
            let ctx = export.messages.iter().position(|m| m.id == first.id).and_then(|p| p.checked_sub(1)).map(|p| &export.messages[p]);
            if let Some(prev) = ctx {
                let _ = write!(line, " — first sent after \"{}\"", prev.preview(40));
            }
            if let Some(r) = first.reactions.first() {
                let _ = write!(line, ", got {} reaction", r.emoji);
            }
        }
        if !i.available {
            line.push_str(" [file missing, shown as placeholder]");
        }
        line
    };
    let sections =
        [("Sticker library (send_sticker)", MediaKind::Sticker as u8, 60usize), ("GIF library (send_gif)", MediaKind::Gif as u8, 40)];
    for (title, kind, cap) in sections {
        let items: Vec<&LibraryItem> = export.library.iter().filter(|i| i.kind as u8 == kind).take(cap).collect();
        if !items.is_empty() {
            let _ = writeln!(s, "\n{title}:");
            for i in items {
                let _ = writeln!(s, "{}", fmt_item(i));
            }
        }
    }
    let media: Vec<&LibraryItem> =
        export.library.iter().filter(|i| i.kind.is_resendable() && i.senders.contains_key(&chat.persona)).take(40).collect();
    if !media.is_empty() {
        let _ = writeln!(s, "\nYour previously shared media (send_media, only re-send when it truly fits):");
        for i in media {
            let _ = writeln!(s, "{} [{}]", fmt_item(i), i.kind.label());
        }
    }
    s
}

/// The system prompt. It is deliberately *static* for a chat: no clock, no
/// counters, nothing derived from live messages, so the provider can cache it
/// together with the history prefix. The model knows the date and time from
/// the `— day —` separators and `HH:MM` stamps in the history.
/// Whom the persona is talking to. Share-link conversations are fixed when they start
/// (the visitor's name, or a room), so the system prompt stays static and cacheable;
/// who wrote what in a room is in the history lines, not here.
enum Audience<'a> {
    /// The owner's own chat: `chat.user` is the person the history is with.
    Owner,
    /// A visitor of a private link (`None`: they gave no name). `host` is the owner.
    Visitor { host: &'a str, name: Option<&'a str> },
    /// Everyone in a link's shared room.
    Room { host: &'a str },
}

fn audience(chat: &Chat) -> Audience<'_> {
    match &chat.guest {
        // Conversations from before 0.5.6 don't know their host: they keep the owner framing.
        Some(g) if !g.host.is_empty() => {
            if chat.user == crate::share::ROOM_USER {
                Audience::Room { host: &g.host }
            } else {
                Audience::Visitor { host: &g.host, name: (chat.user != crate::share::GUEST_NAME).then_some(chat.user.as_str()) }
            }
        }
        _ => Audience::Owner,
    }
}

/// Opening lines for a share-link visitor: who they are, and that the history and notes
/// are about the host, not about them. `history`: there is an export history to learn from.
fn visitor_framing(p: &str, aud: &Audience, history: bool) -> String {
    let past = if history {
        "The chat history before this comes from {p}'s real conversation with {host}: learn from it how {p} writes and what {p} knows and cares about, and you may remember it."
    } else {
        "Who {p} is and how {p} writes comes from {host}'s notes below."
    };
    match aud {
        Audience::Owner => String::new(),
        Audience::Visitor { host, name } => {
            let who = name.map_or_else(|| "Someone (they gave no name)".to_string(), str::to_string);
            let them = name.unwrap_or("they");
            format!(
                "You are {p}. {who} opened a link that {host} shared and is now texting with you on Telegram. Write exactly the way {p} writes. {}\n\
                 {them} is not {host}, unless they say so. Don't assume you know them: be {p} talking to someone new, the way {p} would (curious, wary, warm…), and let them tell you who they are.",
                past.replace("{p}", p).replace("{host}", host)
            )
        }
        Audience::Room { host } => format!(
            "You are {p}, in a Telegram group chat with people who opened a link that {host} shared. Several people may write, new ones may join at any time, and each of their messages shows the sender's name. Write exactly the way {p} writes. {}\n\
             They are not {host}, unless they say so. Don't assume you know them: be {p} meeting new people, the way {p} would, and use their names when {p} would.",
            past.replace("{p}", p).replace("{host}", host)
        ),
    }
}

pub fn system_prompt(export: &ChatExport, chat: &Chat, opts: &PromptOptions) -> String {
    if export.blank {
        return blank_system_prompt(export, chat, opts);
    }
    let p = &chat.persona;
    let aud = audience(chat);
    // The owner's chat names its other person; a visitor's chat the visitor (or "someone").
    let (u, host) = match &aud {
        Audience::Owner => (chat.user.as_str(), chat.user.as_str()),
        Audience::Visitor { host, name } => (name.unwrap_or("someone"), *host),
        Audience::Room { host } => ("someone", *host),
    };
    let mut s = String::new();
    if matches!(aud, Audience::Owner) {
        let _ = writeln!(
            s,
            "You are {p}. You are continuing a real Telegram conversation with {u}, writing exactly the way {p} writes. \
             The person on the other side may be far away from {p} and wants to keep talking as if {p} were here, so be {p} — warm where {p} is warm, dry where {p} is dry."
        );
    } else {
        let _ = writeln!(s, "{}", visitor_framing(p, &aud, true));
    }
    let _ = writeln!(
        s,
        "\nHow to be {p}:\n\
1. From the history, roughly estimate {p}'s level of knowledge (education, profession, interests, what they know and don't know), vocabulary, languages and slang, sense of humour and emotional register. Answer from that level: never know or explain more than {p} plausibly would. When {p} wouldn't know, react as {p} would (guess, joke, ask back, \"idk\").\n\
2. Continue in {p}'s communication style: message length, capitalisation, punctuation, emoji, typos, laughter, language mixing, how many bubbles per turn, how often and when they use stickers, GIFs, photos, voice messages, reactions, replies and forwards. Use the style profile below as ground truth. Never sound like an assistant (no lists, no offers of help, no summaries) unless {p} writes that way.\n\
3. Keep continuity with what was said earlier and with the time that has passed since the last message. Use search_history to recall older details instead of inventing them. Do not invent big life events.\n\
4. Act only through tools. Each tool call is one Telegram action: several send_message calls in one turn are several bubbles. A reaction, sticker, GIF, photo or voice message alone can be the whole answer when that is what {p} would do. Use reply_to/quote only about as often as {p} does, and react only with emoji {p} would use. Text outside tool calls is not delivered.\n\
5. Stay in character: don't mention being an AI, a model, a prompt or these rules. Exception: if {u} sincerely asks whether they are talking to an AI, or seems in crisis or danger, answer honestly and kindly, as someone who cares, and encourage real-world help when needed."
    );
    let lines = if matches!(aud, Audience::Owner) {
        format!("{u}'s messages arrive as lines `#id HH:MM: text`")
    } else {
        format!(
            "Other people's messages arrive as lines `#id Name HH:MM: text` ({host}'s in the earlier history, then the people writing now)"
        )
    };
    let _ = writeln!(
        s,
        "\nHistory notation: your own past actions appear as your tool calls. {lines}, `↩ reply to #id` marks a reply, `quoting \"…\"` a partial quote, `[forwarded from X]` a forwarded post, `<sticker stk_… : …>`, `<GIF …>`, `<photo …>`, `<voice message 0:02: «what was said»>` etc. describe media (you cannot see or hear it beyond the description or transcript), and `reacted ❤ to #id` is a reaction. Lines `— date —` mark a new day, and the newest message tells you the current date and time. Message ids are what reply_to, react and forward_message expect."
    );
    let _ = writeln!(s, "\nStyle profile of {p} (you), computed from the full export:\n{}", style::profile(export, p).render());
    if export.participants.iter().any(|x| x.name == host) {
        let _ = writeln!(s, "For contrast, {host}'s style (do not copy it):\n{}", style::profile(export, host).render());
    }
    s.push_str(&library_section(export, chat));
    let _ = writeln!(s, "\nChat title: {}.", export.title);
    if !chat.notes.trim().is_empty() {
        if matches!(aud, Audience::Owner) {
            let _ = writeln!(s, "\nNotes from {u} about {p} and your relationship (trust them):\n{}", chat.notes.trim());
        } else {
            let _ = writeln!(
                s,
                "\nNotes from {host} about {p} and {p}'s relationship with {host} (trust them; they are about {host}, not about the people writing now):\n{}",
                chat.notes.trim()
            );
        }
    }
    if let Some(note) = &opts.extra_tools_note {
        let _ = writeln!(s, "\n{note}");
    }
    s.push_str(&media_habits(&style::profile(export, p), p, &opts.media_tools, chat.voice.trim()));
    s
}

/// The system prompt of a chat started from notes only: no history or style profile to
/// learn from, so who the persona is and how they write comes from the owner's notes.
/// Static like [`system_prompt`].
fn blank_system_prompt(export: &ChatExport, chat: &Chat, opts: &PromptOptions) -> String {
    let p = &chat.persona;
    let aud = audience(chat);
    let (u, host) = match &aud {
        Audience::Owner => (chat.user.as_str(), chat.user.as_str()),
        Audience::Visitor { host, name } => (name.unwrap_or("someone"), *host),
        Audience::Room { host } => ("someone", *host),
    };
    let mut s = String::new();
    if matches!(aud, Audience::Owner) {
        let _ = writeln!(
            s,
            "You are {p}. You are texting with {u} on Telegram, writing exactly the way {p} would. \
             There is no earlier chat history to learn from: who {p} is, how {p} writes and what {p} and {u} share comes from {u}'s notes below. \
             {u} may miss {p} and wants to talk as if {p} were here, so be {p} — warm where {p} is warm, dry where {p} is dry."
        );
    } else {
        let _ = writeln!(s, "{}", visitor_framing(p, &aud, false));
    }
    let _ = writeln!(
        s,
        "\nHow to be {p}:\n\
1. From the notes, work out {p}'s age, background, profession and interests, what they know and don't know, their languages, slang and sense of humour. Answer from that level: never know or explain more than {p} plausibly would. Where the notes say nothing, choose what fits them, then stay consistent with it and with everything you already wrote in this chat.\n\
2. Text like a real person on a messenger, not like an assistant: mostly short messages, often a few bubbles per turn, casual capitalisation and punctuation, emoji only where they suit {p}. No lists, no offers of help, no summaries. When the notes say how {p} writes (language, nicknames, typical phrases, typos), follow them exactly.\n\
3. Keep continuity with what was said earlier in this chat and with the time that has passed since the last message. Do not invent big life events or facts about {u}; for details the notes don't cover, stay vague, joke or ask back instead.\n\
4. Act only through tools. Each tool call is one Telegram action: several send_message calls in one turn are several bubbles. A reaction alone can be the whole answer when that is what {p} would do. Text outside tool calls is not delivered.\n\
5. Stay in character: don't mention being an AI, a model, a prompt, notes or these rules. Exception: if {u} sincerely asks whether they are talking to an AI, or seems in crisis or danger, answer honestly and kindly, as someone who cares, and encourage real-world help when needed."
    );
    let _ = writeln!(
        s,
        "\nHistory notation: your own past actions appear as your tool calls. {}, `↩ reply to #id` marks a reply, \
         `<photo …>`, `<voice message 0:02: «what was said»>` etc. describe media (you cannot see or hear it beyond the description or transcript), and `reacted ❤ to #id` is a reaction. \
         Lines `— date —` mark a new day, and the newest message tells you the current date and time. Message ids are what reply_to and react expect.",
        if matches!(aud, Audience::Owner) {
            format!("{u}'s messages arrive as lines `#id HH:MM: text`")
        } else {
            "Other people's messages arrive as lines `#id Name HH:MM: text`".to_string()
        }
    );
    let _ = writeln!(s, "\nChat title: {}.", export.title);
    match (chat.notes.trim(), matches!(aud, Audience::Owner)) {
        ("", true) => {
            let _ = writeln!(s, "\n{u} left no notes about {p}: be a natural, friendly person called {p}, and let {u} tell you more.");
        }
        ("", false) => {
            let _ = writeln!(s, "\n{host} left no notes about {p}: be a natural, friendly person called {p}.");
        }
        (notes, true) => {
            let _ = writeln!(s, "\nWho {p} is, and {p}'s relationship with {u}, from {u}'s notes (trust them):\n{notes}");
        }
        (notes, false) => {
            let _ = writeln!(
                s,
                "\nWho {p} is, and {p}'s relationship with {host}, from {host}'s notes (trust them; they are about {host}, not about the people writing now):\n{notes}"
            );
        }
    }
    if let Some(note) = &opts.extra_tools_note {
        let _ = writeln!(s, "\n{note}");
    }
    s
}

/// When the persona mostly talks in voice messages or shares many photos and a
/// tool can create them, say so explicitly: otherwise models fall back to text.
fn media_habits(profile: &style::Profile, p: &str, tools: &crate::mcp::MediaTools, voice: &str) -> String {
    let n = profile.messages.max(1) as f32;
    let mut s = String::new();
    if tools.speech.is_some() && profile.messages >= 3 && profile.voice as f32 / n >= 0.4 {
        let _ = writeln!(
            s,
            "\nVoice habit: {p} talks in voice messages ({} of {} messages). Keep doing that: answer with send_voice {{text}} \
             (exactly what {p} would say out loud: their words, language, fillers and tone, one message per call) instead of typing. \
             Use send_message only where {p} would really type{}.",
            profile.voice,
            profile.messages,
            if voice.is_empty() { String::new() } else { format!(". Voice messages use this chat's voice `{voice}` automatically") }
        );
    }
    if tools.image.is_some() && profile.messages >= 3 && profile.photos as f32 / n >= 0.25 {
        let _ = writeln!(
            s,
            "\nPhoto habit: {p} shares photos a lot ({} of {} messages), often showing instead of telling. Keep doing that: when {p} would show something \
             (what they eat, where they are, the view or weather, something they bought or are doing), use send_photo {{prompt, caption?}} \
             with a prompt for a casual smartphone photo from {p}'s real life that fits the conversation and the earlier photos (no close-up faces, no text overlays), \
             and a short caption only if {p} would add one. Do not resend old photos as if they were new.",
            profile.photos, profile.messages
        );
    }
    s
}

fn turn_sizes(rendered: &[Vec<Value>]) -> Vec<usize> {
    rendered.iter().map(|v| v.iter().map(|m| m.to_string().len()).sum()).collect()
}

/// Where the kept history starts after compacting `from..`: the first turn
/// at which the rest fits the low watermark, moved forward to a `user` turn
/// so the kept history (and the summary attached to it) starts with `user`.
fn compaction_point(turns: &[(Turn, Option<NaiveDate>)], sizes: &[usize], from: usize, budget: usize) -> usize {
    let low = (budget as f32 * LOW_WATERMARK) as usize;
    let last = sizes.len().saturating_sub(1);
    let mut to = from;
    let mut sum: usize = sizes[from..].iter().sum();
    while sum > low && to < last {
        sum -= sizes[to];
        to += 1;
    }
    let mut k = to;
    while k < last && !matches!(turns[k].0, Turn::User(_)) {
        k += 1;
    }
    if matches!(turns[k].0, Turn::User(_)) { k } else { to }
}

/// Compact, human-readable transcript of turns for the summarizer.
fn transcript(turns: &[(Turn, Option<NaiveDate>)], chat: &Chat) -> String {
    let p = &chat.persona;
    let mut out = String::new();
    for (turn, _) in turns {
        match turn {
            Turn::User(lines) => {
                for l in lines {
                    let _ = writeln!(out, "{l}");
                }
            }
            Turn::Assistant(calls) => {
                for (_, name, args, result) in calls {
                    let a = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                    let reply = args.get("reply_to").and_then(Value::as_i64).map(|r| format!(" ↩ #{r}")).unwrap_or_default();
                    let id = result.get("message_id").and_then(Value::as_i64).map(|i| format!("#{i} ")).unwrap_or_default();
                    let body = match name.as_str() {
                        tools::SEND_MESSAGE => a("text"),
                        tools::SEND_STICKER => format!("<sticker {}>", a("sticker_id")),
                        tools::SEND_GIF => format!("<GIF {}{}> {}", a("gif_id"), a("url"), a("caption")),
                        tools::SEND_MEDIA => format!("<{}> {}", a("media_id"), a("caption")),
                        tools::SEND_VOICE => format!("<voice message: «{}»>", a("text")),
                        tools::SEND_PHOTO => format!("<photo: {}> {}", a("prompt"), a("caption")),
                        tools::FORWARD_MESSAGE => {
                            format!("[forwarded] {}", result.get("preview").and_then(Value::as_str).unwrap_or_default())
                        }
                        tools::REACT => {
                            let t = args.get("message_id").and_then(Value::as_i64).unwrap_or_default();
                            let _ = writeln!(out, "{p} reacted {} to #{t}", a("emoji"));
                            continue;
                        }
                        other => format!("<{other}>"),
                    };
                    let _ = writeln!(out, "{id}{p}{reply}: {}", body.trim());
                }
            }
        }
    }
    if out.len() > MAX_SUMMARY_INPUT_CHARS {
        let mut cut = out.len() - MAX_SUMMARY_INPUT_CHARS;
        while !out.is_char_boundary(cut) {
            cut += 1;
        }
        let nl = out[cut..].find('\n').map_or(cut, |i| cut + i + 1);
        out = format!("[… earlier part omitted …]\n{}", &out[nl..]);
    }
    out
}

/// Builds the request. Layout (stable prefix first, for prompt caching):
/// 1. static system prompt,
/// 2. the first kept `user` turn, prefixed with the memory summary and the
///    current day, when older turns are compacted,
/// 3. the kept history turns, verbatim.
pub fn build(export: &ChatExport, chat: &Chat, opts: &PromptOptions) -> Prompt {
    let turns = history_turns(export, chat, opts.media_tools.speech.is_some());
    let total = turns.len();
    let rendered: Vec<Vec<Value>> = turns.iter().map(|(t, _)| t.to_values()).collect();
    let sizes = turn_sizes(&rendered);
    // Compaction state survives regenerations/deletions only while it still
    // leaves at least one turn.
    let memory = chat.memory.as_ref().filter(|m| m.turns > 0 && m.turns < total);
    let mut start = memory.map_or(0, |m| m.turns);
    let mut compaction = None;
    if total > 0 && sizes[start..].iter().sum::<usize>() > opts.budget_chars {
        let to = compaction_point(&turns, &sizes, start, opts.budget_chars);
        if to > start {
            compaction = Some(Compaction { from: start, to, transcript: transcript(&turns[start..to], chat) });
            start = to;
        }
    }
    let mut messages = vec![json!({"role": "system", "content": system_prompt(export, chat, opts)})];
    if start > 0 {
        let summary = memory.map(|m| m.summary.trim()).filter(|s| !s.is_empty());
        let mut head = String::from("[Context, not a chat message: the conversation started earlier than shown here.");
        // A newer compaction without a summary yet: the stored summary only covers older turns.
        let stale = compaction.is_some();
        match summary {
            Some(sum) => {
                let _ = write!(
                    head,
                    " Summary of the {} part, from your ({}) point of view:\n{sum}\n",
                    if stale { "oldest" } else { "earlier" },
                    chat.persona
                );
                if stale {
                    head.push_str("Some turns after that are not shown. ");
                }
            }
            None => head.push(' '),
        }
        head.push_str("Use search_history for exact older messages.]");
        let day = turns[start].1;
        let mut first = rendered[start].clone();
        match first.first_mut() {
            Some(m) if m["role"] == "user" => {
                let content = m["content"].as_str().unwrap_or_default().to_string();
                let sep = day.filter(|_| !content.starts_with("— ")).map(|d| format!("{}\n", day_sep(d))).unwrap_or_default();
                m["content"] = json!(format!("{head}\n\n{sep}{content}"));
            }
            _ => {
                let sep = day.map(|d| format!("\n\n{}", day_sep(d))).unwrap_or_default();
                messages.push(json!({"role": "user", "content": format!("{head}{sep}")}));
            }
        }
        messages.extend(first);
        for turn in &rendered[start + 1..] {
            messages.extend(turn.iter().cloned());
        }
    } else {
        for turn in &rendered {
            messages.extend(turn.iter().cloned());
        }
    }
    Prompt { messages, dropped_turns: start, total_turns: total, compaction }
}

/// Messages for the summarizer call that updates the memory.
pub fn summary_request(chat: &Chat, previous: Option<&str>, transcript: &str) -> Vec<Value> {
    let p = &chat.persona;
    let u = &chat.user;
    let sys = format!(
        "You maintain the long-term memory of a Telegram chat between {p} and {u}. {p} will be role-played by a model \
         that only sees this memory plus the most recent messages. Write the updated memory: merge the previous memory \
         with the new transcript part, keep everything still relevant, drop what no longer matters.\n\
         Cover: who they are to each other and how they treat each other; facts about both (work, places, people, pets, \
         preferences); ongoing topics, plans, promises, open questions and how they ended; running jokes, nicknames and \
         phrases they reuse; notable stickers/media and what they meant; the emotional state at the end; dates of key \
         events (use the `— day —` lines). Write from {p}'s point of view (\"you\" = {p}), as terse notes, \
         keeping quotes, names and slang in their original language. Do not describe writing style statistics. \
         Do not invent anything. At most ~350 words. Output only the memory text."
    );
    let mut user = String::new();
    if let Some(prev) = previous.map(str::trim).filter(|s| !s.is_empty()) {
        let _ = write!(user, "Previous memory:\n{prev}\n\n");
    }
    let _ = write!(user, "New transcript part (lines `#id HH:MM: text` are {u}, `{p}: …` lines are you):\n{transcript}");
    vec![json!({"role": "system", "content": sys}), json!({"role": "user", "content": user})]
}

/// Plain-text search across export and live messages for `search_history`.
pub fn search(export: &ChatExport, chat: &Chat, query: &str, limit: usize) -> Vec<String> {
    let q = query.to_lowercase();
    let terms: Vec<&str> = q.split_whitespace().collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for item in timeline(export, &chat.events) {
        let Item::Msg(m) = item else { continue };
        let mut hay = m.text.to_lowercase();
        if let Some(media) = &m.media {
            hay.push(' ');
            hay.push_str(&media_desc(media, export).to_lowercase());
        }
        if let Some(lp) = &m.link_preview {
            hay.push_str(
                &format!(" {} {}", lp.title.clone().unwrap_or_default(), lp.description.clone().unwrap_or_default()).to_lowercase(),
            );
        }
        if terms.iter().all(|t| hay.contains(t)) {
            let who = if m.from == chat.persona { "you".to_string() } else { m.from.clone() };
            let date = parse_date(&m.date).map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
            hits.push(format!("#{} {date} {who}: {}", m.id, m.preview(300)));
        }
    }
    let skip = hits.len().saturating_sub(limit);
    hits.split_off(skip)
}
