//! Normalized, transport-independent representation of a Telegram chat.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Photo,
    Video,
    RoundVideo,
    Voice,
    Audio,
    File,
    Sticker,
    Gif,
    Location,
    Contact,
    Poll,
    Call,
    Other,
}

impl MediaKind {
    pub fn id_prefix(self) -> &'static str {
        match self {
            MediaKind::Photo => "pho",
            MediaKind::Video => "vid",
            MediaKind::RoundVideo => "rnd",
            MediaKind::Voice => "voc",
            MediaKind::Audio => "aud",
            MediaKind::File => "fil",
            MediaKind::Sticker => "stk",
            MediaKind::Gif => "gif",
            _ => "med",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MediaKind::Photo => "photo",
            MediaKind::Video => "video",
            MediaKind::RoundVideo => "round video message",
            MediaKind::Voice => "voice message",
            MediaKind::Audio => "audio file",
            MediaKind::File => "file",
            MediaKind::Sticker => "sticker",
            MediaKind::Gif => "GIF",
            MediaKind::Location => "location",
            MediaKind::Contact => "contact",
            MediaKind::Poll => "poll",
            MediaKind::Call => "call",
            MediaKind::Other => "media",
        }
    }

    /// Media that can be re-sent by the persona through `send_media`.
    pub fn is_resendable(self) -> bool {
        matches!(self, MediaKind::Photo | MediaKind::Video | MediaKind::RoundVideo | MediaKind::Voice | MediaKind::Audio | MediaKind::File)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Media {
    pub kind: Option<MediaKind>,
    /// Stable id in the export's media library (`stk_…`, `gif_…`, `pho_…`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_id: Option<String>,
    /// Path relative to the export root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumb: Option<String>,
    /// External URL (e.g. a GIF found through an MCP server).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Duration or size string as shown by Telegram ("0:05", "1.6 KB").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// What the item shows or says, for media created in a live chat
    /// (transcript of a recorded voice message, prompt of a generated photo).
    /// Export media keep their description in the library instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Pixel size of media created in a live chat; export media take it from the library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[u32; 2]>,
}

impl Media {
    pub fn kind(&self) -> MediaKind {
        self.kind.unwrap_or(MediaKind::Other)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LinkPreview {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Preview image path relative to the export root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub photo: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Reaction {
    pub emoji: String,
    pub count: u32,
    /// Names of reactors when the export lists them (usually empty for HTML exports).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub from: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: i64,
    /// Sender display name. Empty for service messages.
    pub from: String,
    /// Local timestamp, `YYYY-MM-DDTHH:MM:SS`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub service: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub edited: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<i64>,
    /// Partial quote of the replied message (Telegram "quote" replies).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forwarded_from: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<Media>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_preview: Option<LinkPreview>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<Reaction>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Message {
    /// One-line preview used in reply hints and search results.
    pub fn preview(&self, max: usize) -> String {
        let mut s = if !self.text.trim().is_empty() {
            // Collapse line breaks and runs of spaces: "Schema.\n\nMy DB." → "Schema. My DB."
            self.text.split_whitespace().collect::<Vec<_>>().join(" ")
        } else if let Some(m) = &self.media {
            match m.title.as_deref().filter(|t| *t != "GIF") {
                Some(t) => format!("[{} \"{t}\"]", m.kind().label()),
                None => format!("[{}]", m.kind().label()),
            }
        } else if let Some(lp) = &self.link_preview {
            lp.url.clone()
        } else {
            String::new()
        };
        if s.chars().count() > max {
            s = s.chars().take(max).collect::<String>() + "…";
        }
        s
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LibraryItem {
    pub id: String,
    pub kind: MediaKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumb: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// File exists in the export (some exports skip large media).
    pub available: bool,
    /// Short visual description (optionally produced by a vision model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Pixel size (width, height) of the photo, or of the poster of a video or GIF, for the
    /// chat to reserve its box ([`crate::imgsize`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[u32; 2]>,
    /// Message ids where this item was used.
    pub uses: Vec<i64>,
    /// Usage count per sender.
    pub senders: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Participant {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    pub messages: u32,
}

/// A parsed Telegram export.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatExport {
    pub title: String,
    pub participants: Vec<Participant>,
    pub messages: Vec<Message>,
    pub library: Vec<LibraryItem>,
    /// Not imported: started from a name and notes only (New chat → From notes). It has
    /// no messages, belongs to its one chat, and goes when that chat is deleted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub blank: bool,
}

impl ChatExport {
    pub fn library_item(&self, id: &str) -> Option<&LibraryItem> {
        self.library.iter().find(|i| i.id == id)
    }

    /// Best guess at who created the export: in a personal chat the export is
    /// titled with the *other* person's name.
    pub fn exporter(&self) -> Option<&str> {
        let others: Vec<&Participant> = self.participants.iter().filter(|p| p.name != self.title).collect();
        if others.len() == 1 && others.len() < self.participants.len() { Some(&others[0].name) } else { None }
    }
}
