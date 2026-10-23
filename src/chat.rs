//! A conversation continued on top of an imported export.

use crate::model::{ChatExport, Message};
use crate::style::infer_reactors;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chat {
    pub id: String,
    pub export_id: String,
    pub title: String,
    /// Participant the LLM speaks as.
    pub persona: String,
    /// Participant the human plays.
    pub user: String,
    #[serde(default)]
    pub notes: String,
    /// TTS voice for the persona's generated voice messages (e.g. `Puck`), empty
    /// uses the speech server's default.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub voice: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub events: Vec<Event>,
    pub next_id: i64,
    /// Rolling summary of the history turns that no longer fit the budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<Memory>,
    /// Token usage reported by the provider for the last LLM request of this chat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_usage: Option<Usage>,
    /// Set for conversations of share-link visitors (hidden from the owner's chat list).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest: Option<GuestChat>,
    /// Media created by tools during this chat (`gen_…` ids, e.g. a generated
    /// photo or voice message), which the persona can send with `send_media`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub generated: std::collections::BTreeMap<String, crate::model::Media>,
}

/// Link between a visitor conversation and its share.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GuestChat {
    pub share_id: String,
    /// The owner's name in the template chat: whom the export history and the notes are
    /// about. Empty for conversations made before 0.5.6, where `Chat::user` is the owner.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
    /// Message id -> author session id (room mode: who wrote which message).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub authors: std::collections::BTreeMap<i64, String>,
}

/// History compaction state. The first `turns` history turns are replaced by
/// `summary`. It only moves forward in large steps, so the prompt prefix
/// (system prompt + summary + kept turns) stays byte-identical between
/// requests and provider prompt caches keep hitting.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Memory {
    pub turns: usize,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub updated_at: String,
}

/// `usage` of an OpenAI-compatible response: the exact context size in tokens.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    #[serde(default)]
    pub cached_tokens: u64,
    /// Characters of the serialized request `messages`, to calibrate chars per token.
    #[serde(default)]
    pub prompt_chars: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)] // stored once per chat event, so boxing buys nothing here
pub enum Event {
    Message(Message),
    Reaction {
        target: i64,
        emoji: String,
        from: String,
        date: String,
        /// `true` removes a previously set reaction.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        removed: bool,
    },
}

impl Event {
    pub fn actor(&self) -> &str {
        match self {
            Event::Message(m) => &m.from,
            Event::Reaction { from, .. } => from,
        }
    }
}

/// One entry of the merged export + live timeline.
#[derive(Debug, Clone)]
pub enum Item<'a> {
    Msg(&'a Message),
    React { target: i64, emoji: String, from: String, removed: bool },
}

impl Item<'_> {
    pub fn actor(&self) -> &str {
        match self {
            Item::Msg(m) => &m.from,
            Item::React { from, .. } => from,
        }
    }
}

/// Builds the chronological timeline. Export reactions have no timestamp, so
/// each one is placed right before the reactor's next action after the target
/// message (or at the end of the export).
pub fn timeline<'a>(export: &'a ChatExport, events: &'a [Event]) -> Vec<Item<'a>> {
    let names: Vec<String> = export.participants.iter().map(|p| p.name.clone()).collect();
    let mut out = Vec::with_capacity(export.messages.len() + events.len());
    let mut pending: Vec<Item<'a>> = Vec::new();
    for m in &export.messages {
        if !m.service {
            let (now, later): (Vec<Item>, Vec<Item>) = pending.drain(..).partition(|p| p.actor() == m.from);
            out.extend(now);
            pending = later;
        }
        out.push(Item::Msg(m));
        for r in &m.reactions {
            for who in infer_reactors(m, r.count, &r.from, &names) {
                pending.push(Item::React { target: m.id, emoji: r.emoji.clone(), from: who, removed: false });
            }
        }
    }
    out.extend(pending);
    for e in events {
        out.push(match e {
            Event::Message(m) => Item::Msg(m),
            Event::Reaction { target, emoji, from, removed, .. } => {
                Item::React { target: *target, emoji: emoji.clone(), from: from.clone(), removed: *removed }
            }
        });
    }
    out
}

pub fn find_message<'a>(export: &'a ChatExport, events: &'a [Event], id: i64) -> Option<&'a Message> {
    events
        .iter()
        .rev()
        .find_map(|e| match e {
            Event::Message(m) if m.id == id => Some(m),
            _ => None,
        })
        .or_else(|| export.messages.iter().find(|m| m.id == id))
}
