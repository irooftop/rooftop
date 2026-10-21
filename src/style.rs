//! Deterministic communication-style statistics computed from the whole export.
//! They anchor the LLM's imitation even when old history is truncated.

use crate::model::{ChatExport, MediaKind, Message};
use std::collections::HashMap;
use std::fmt::Write;

pub fn is_emoji(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2300..=0x23FF | 0x2B00..=0x2BFF | 0x3030 | 0x303D | 0x3297 | 0x3299)
}

#[derive(Debug, Default)]
pub struct Profile {
    pub name: String,
    pub messages: usize,
    pub text_messages: usize,
    pub avg_chars: f32,
    pub median_words: usize,
    pub emoji_share: f32,
    pub top_emojis: Vec<(String, usize)>,
    pub stickers: usize,
    pub gifs: usize,
    pub photos: usize,
    pub videos: usize,
    pub voice: usize,
    /// Files and audio tracks.
    pub other_media: usize,
    pub forwards: usize,
    pub replies: usize,
    pub edited: usize,
    pub lowercase_start: f32,
    pub ends_with_period: f32,
    pub ends_with_question: f32,
    pub ends_with_exclaim: f32,
    pub no_final_punct: f32,
    pub multiline: f32,
    pub avg_bubbles_per_turn: f32,
    pub reactions_given: Vec<(String, usize)>,
    pub samples: Vec<String>,
    /// Transcripts of own voice messages (how they talk), when transcribed.
    pub voice_samples: Vec<String>,
}

/// Who most likely left a reaction. HTML exports usually omit reactors. In a
/// one-to-one chat people rarely react to their own messages, so the reaction
/// is attributed to the other participant.
pub fn infer_reactors(msg: &Message, count: u32, explicit: &[String], participants: &[String]) -> Vec<String> {
    if !explicit.is_empty() {
        return explicit.to_vec();
    }
    if participants.len() == 2 {
        let other = participants.iter().find(|p| **p != msg.from).cloned();
        let mut v: Vec<String> = other.into_iter().collect();
        if count >= 2 {
            v.push(msg.from.clone());
        }
        return v;
    }
    Vec::new()
}

pub fn profile(export: &ChatExport, name: &str) -> Profile {
    let participants: Vec<String> = export.participants.iter().map(|p| p.name.clone()).collect();
    let mine: Vec<&Message> = export.messages.iter().filter(|m| !m.service && m.from == name).collect();
    let mut p = Profile { name: name.to_string(), messages: mine.len(), ..Default::default() };
    let texts: Vec<&str> = mine.iter().filter(|m| m.forwarded_from.is_none()).map(|m| m.text.trim()).filter(|t| !t.is_empty()).collect();
    p.text_messages = texts.len();
    let mut emoji_counts: HashMap<String, usize> = HashMap::new();
    let mut words: Vec<usize> = Vec::new();
    let (mut chars, mut with_emoji, mut lower, mut period, mut question, mut exclaim, mut none, mut multi) =
        (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    for t in &texts {
        chars += t.chars().count();
        words.push(t.split_whitespace().count());
        let mut any = false;
        for c in t.chars().filter(|c| is_emoji(*c)) {
            any = true;
            *emoji_counts.entry(c.to_string()).or_default() += 1;
        }
        if any {
            with_emoji += 1;
        }
        if let Some(first) = t.chars().find(|c| c.is_alphabetic())
            && first.is_lowercase()
        {
            lower += 1;
        }
        match t.chars().rev().find(|c| !c.is_whitespace() && !is_emoji(*c) && *c != '\u{FE0F}') {
            Some('.') => period += 1,
            Some('?') => question += 1,
            Some('!') => exclaim += 1,
            _ => none += 1,
        }
        if t.contains('\n') {
            multi += 1;
        }
    }
    let n = texts.len().max(1) as f32;
    p.avg_chars = chars as f32 / n;
    words.sort_unstable();
    p.median_words = words.get(words.len() / 2).copied().unwrap_or(0);
    p.emoji_share = with_emoji as f32 / n;
    p.lowercase_start = lower as f32 / n;
    p.ends_with_period = period as f32 / n;
    p.ends_with_question = question as f32 / n;
    p.ends_with_exclaim = exclaim as f32 / n;
    p.no_final_punct = none as f32 / n;
    p.multiline = multi as f32 / n;
    let mut top: Vec<(String, usize)> = emoji_counts.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    top.truncate(8);
    p.top_emojis = top;
    for m in &mine {
        if m.forwarded_from.is_some() {
            p.forwards += 1;
        }
        if m.reply_to.is_some() {
            p.replies += 1;
        }
        if m.edited {
            p.edited += 1;
        }
        match m.media.as_ref().map(|m| m.kind()) {
            Some(MediaKind::Sticker) => p.stickers += 1,
            Some(MediaKind::Gif) => p.gifs += 1,
            Some(MediaKind::Photo) => p.photos += 1,
            Some(MediaKind::Video | MediaKind::RoundVideo) => p.videos += 1,
            Some(MediaKind::Voice) => p.voice += 1,
            Some(_) => p.other_media += 1,
            None => {}
        }
    }
    // Consecutive messages by the same sender form one "turn".
    let mut turns = 0usize;
    let mut prev: Option<&str> = None;
    for m in export.messages.iter().filter(|m| !m.service) {
        if m.from == name && prev != Some(name) {
            turns += 1;
        }
        prev = Some(m.from.as_str());
    }
    p.avg_bubbles_per_turn = mine.len() as f32 / turns.max(1) as f32;
    let mut given: HashMap<String, usize> = HashMap::new();
    for m in &export.messages {
        for r in &m.reactions {
            if infer_reactors(m, r.count, &r.from, &participants).iter().any(|x| x == name) {
                *given.entry(r.emoji.clone()).or_default() += 1;
            }
        }
    }
    let mut given: Vec<(String, usize)> = given.into_iter().collect();
    given.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    p.reactions_given = given;
    // Evenly spread own-words samples across the whole history.
    let pool: Vec<&str> = texts.iter().copied().filter(|t| t.chars().count() <= 280).collect();
    let want = 24usize.min(pool.len());
    if want > 0 {
        let step = pool.len() as f32 / want as f32;
        p.samples = (0..want)
            .map(|i| pool[(i as f32 * step) as usize].lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" / "))
            .collect();
        p.samples.dedup();
    }
    let spoken: Vec<&str> = export
        .messages
        .iter()
        .filter(|m| m.from == name && m.media.as_ref().is_some_and(|x| x.kind() == MediaKind::Voice))
        .filter_map(|m| {
            let media = m.media.as_ref()?;
            media
                .library_id
                .as_deref()
                .and_then(|id| export.library_item(id))
                .and_then(|i| i.description.as_deref())
                .or(media.description.as_deref())
        })
        .filter(|t| !t.starts_with('['))
        .collect();
    let want = 12usize.min(spoken.len());
    if want > 0 {
        let step = spoken.len() as f32 / want as f32;
        p.voice_samples = (0..want).map(|i| crate::llm::truncate(spoken[(i as f32 * step) as usize], 280)).collect();
    }
    p
}

fn pct(x: f32) -> String {
    format!("{:.0}%", x * 100.0)
}

impl Profile {
    pub fn render(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "- messages in export: {} ({} with own text)", self.messages, self.text_messages);
        if self.text_messages == 0 {
            let _ = writeln!(s, "- never types text messages");
        }
        if self.text_messages > 0 {
            let _ = writeln!(
                s,
                "- typical length: ~{:.0} characters, median {} words; multi-line messages {}",
                self.avg_chars,
                self.median_words,
                pct(self.multiline)
            );
            let _ = writeln!(s, "- splits a thought into ~{:.1} consecutive messages per turn", self.avg_bubbles_per_turn);
            let _ = writeln!(
                s,
                "- starts lowercase {}; ends with '.' {}, '?' {}, '!' {}, no punctuation {}",
                pct(self.lowercase_start),
                pct(self.ends_with_period),
                pct(self.ends_with_question),
                pct(self.ends_with_exclaim),
                pct(self.no_final_punct)
            );
            let emojis: Vec<String> = self.top_emojis.iter().map(|(e, c)| format!("{e}×{c}")).collect();
            let _ = writeln!(
                s,
                "- messages with emoji {}{}",
                pct(self.emoji_share),
                if emojis.is_empty() { String::new() } else { format!("; favourites: {}", emojis.join(" ")) }
            );
        }
        let m = self.messages.max(1) as f32;
        let _ = writeln!(
            s,
            "- stickers {} ({}), GIFs {} ({}), photos {} ({}), videos {} ({}), voice messages {} ({}), files/audio {} ({}), forwards {} ({})",
            self.stickers,
            pct(self.stickers as f32 / m),
            self.gifs,
            pct(self.gifs as f32 / m),
            self.photos,
            pct(self.photos as f32 / m),
            self.videos,
            pct(self.videos as f32 / m),
            self.voice,
            pct(self.voice as f32 / m),
            self.other_media,
            pct(self.other_media as f32 / m),
            self.forwards,
            pct(self.forwards as f32 / m)
        );
        // Make dominant media habits impossible to miss.
        if self.messages >= 3 && self.voice as f32 / m >= 0.4 {
            let _ = writeln!(s, "- talks mostly in voice messages ({} of {} messages)", self.voice, self.messages);
        }
        if self.messages >= 3 && self.photos as f32 / m >= 0.25 {
            let _ = writeln!(
                s,
                "- shares photos a lot ({} of {} messages), often instead of describing things in words",
                self.photos, self.messages
            );
        }
        let _ = writeln!(s, "- uses explicit replies for {} of messages", pct(self.replies as f32 / m));
        if !self.reactions_given.is_empty() {
            let r: Vec<String> = self.reactions_given.iter().map(|(e, c)| format!("{e}×{c}")).collect();
            let _ = writeln!(s, "- reactions left on the other person's messages: {}", r.join(" "));
        } else {
            let _ = writeln!(s, "- rarely or never leaves reactions");
        }
        if !self.samples.is_empty() {
            let _ = writeln!(s, "- verbatim samples: {}", self.samples.iter().map(|x| format!("«{x}»")).collect::<Vec<_>>().join(" "));
        }
        if !self.voice_samples.is_empty() {
            let _ = writeln!(
                s,
                "- how they talk in voice messages (transcripts): {}",
                self.voice_samples.iter().map(|x| format!("«{x}»")).collect::<Vec<_>>().join(" ")
            );
        }
        s
    }
}
