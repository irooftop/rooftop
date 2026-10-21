//! Chat-action tools. Every Telegram action of the persona (a text bubble, a
//! sticker, a GIF, a reaction, a reply, a forward) is a tool call, both in the
//! replayed history and in live answers, so the model learns *how* the person
//! interacts and not only what they write.

use serde_json::{Value, json};

pub const SEND_MESSAGE: &str = "send_message";
pub const SEND_STICKER: &str = "send_sticker";
pub const SEND_GIF: &str = "send_gif";
pub const SEND_MEDIA: &str = "send_media";
pub const FORWARD_MESSAGE: &str = "forward_message";
pub const REACT: &str = "react";
pub const SEARCH_HISTORY: &str = "search_history";
pub const SEND_VOICE: &str = "send_voice";
pub const SEND_PHOTO: &str = "send_photo";

/// Tools whose only effect is a visible chat action, and after them the turn ends.
pub fn is_action(name: &str) -> bool {
    matches!(name, SEND_MESSAGE | SEND_STICKER | SEND_GIF | SEND_MEDIA | FORWARD_MESSAGE | REACT | SEND_VOICE | SEND_PHOTO)
}

fn tool(name: &str, description: &str, parameters: Value) -> Value {
    json!({"type": "function", "function": {"name": name, "description": description, "parameters": parameters}})
}

/// `send_voice` / `send_photo`, offered when an MCP server can create speech /
/// images. They wrap that tool with fixed, valid arguments, so a voice message
/// or photo is a single action like any other.
pub fn media_definitions(media: &crate::mcp::MediaTools) -> Vec<Value> {
    let reply_to = json!({"type": "integer", "description": "Optional id (#number) of the message this replies to, only when the person would explicitly reply."});
    let mut out = Vec::new();
    if media.speech.is_some() {
        out.push(tool(
            SEND_VOICE,
            "Record and send a voice message: `text` is spoken in the person's voice. Write exactly what they would say out loud (their words, language, fillers, tone), not a description.",
            json!({"type": "object", "properties": {
                "text": {"type": "string", "description": "The words to say."},
                "reply_to": reply_to
            }, "required": ["text"]}),
        ));
    }
    if media.image.is_some() {
        out.push(tool(
            SEND_PHOTO,
            "Take and send a new photo: `prompt` describes the picture (a casual smartphone photo from the person's life that fits the conversation). Optional caption.",
            json!({"type": "object", "properties": {
                "prompt": {"type": "string", "description": "What the photo shows: subject, place, light, framing."},
                "caption": {"type": "string"},
                "reply_to": reply_to
            }, "required": ["prompt"]}),
        ));
    }
    out
}

pub fn definitions() -> Vec<Value> {
    let reply_to = json!({"type": "integer", "description": "Optional id (#number) of the message this replies to. Use only when the person would explicitly reply, typically to an older message or one of several unanswered ones, not by default to the latest message."});
    vec![
        tool(
            SEND_MESSAGE,
            "Send one chat bubble. Call several times in one turn to send several consecutive bubbles, exactly as the person splits thoughts.",
            json!({"type": "object", "properties": {
                "text": {"type": "string", "description": "Message text, written exactly in the person's style."},
                "reply_to": reply_to,
                "quote": {"type": "string", "description": "Optional exact fragment of the replied message to quote (partial quote). Never the whole message, and omit it for a plain reply."}
            }, "required": ["text"]}),
        ),
        tool(
            SEND_STICKER,
            "Send a sticker from the sticker library (ids listed in the system prompt).",
            json!({"type": "object", "properties": {
                "sticker_id": {"type": "string"},
                "reply_to": reply_to
            }, "required": ["sticker_id"]}),
        ),
        tool(
            SEND_GIF,
            "Send a GIF: either a library gif_id, or a direct https URL of a GIF/MP4 obtained from another tool (e.g. a GIF search MCP server).",
            json!({"type": "object", "properties": {
                "gif_id": {"type": "string"},
                "url": {"type": "string", "description": "Direct media URL (.gif/.mp4/.webp)."},
                "caption": {"type": "string"},
                "reply_to": reply_to
            }}),
        ),
        tool(
            SEND_MEDIA,
            "Re-send a photo, video, voice message, audio or file that was already shared in this chat (media library ids).",
            json!({"type": "object", "properties": {
                "media_id": {"type": "string"},
                "caption": {"type": "string"},
                "reply_to": reply_to
            }, "required": ["media_id"]}),
        ),
        tool(
            FORWARD_MESSAGE,
            "Forward (re-share) an existing message or post from this chat by id.",
            json!({"type": "object", "properties": {
                "message_id": {"type": "integer"},
                "comment": {"type": "string", "description": "Optional text bubble sent right after the forward."}
            }, "required": ["message_id"]}),
        ),
        tool(
            REACT,
            "Put an emoji reaction on a message (like a Telegram reaction / like).",
            json!({"type": "object", "properties": {
                "message_id": {"type": "integer"},
                "emoji": {"type": "string", "description": "Single emoji, e.g. ❤ 👍 😁 🔥"}
            }, "required": ["message_id", "emoji"]}),
        ),
        tool(
            SEARCH_HISTORY,
            "Search the whole chat history (including parts not shown) to recall facts, shared memories or how the person phrased things. Returns matching messages, not visible to the other person.",
            json!({"type": "object", "properties": {
                "query": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 30}
            }, "required": ["query"]}),
        ),
    ]
}
