//! What happens in a chat: the user's messages, voice messages and reactions, the
//! persona's turns (answer, rewrite) and the exact prompt it gets.

use super::chats::{timeline_messages, view_messages};
use super::uploads::multipart_error;
use super::{ApiError, ApiResult, S, bad};
use crate::agent;
use crate::chat::{Chat, Event};
use crate::model::{ChatExport, MediaKind, Message};
use crate::prompt::{self, PromptOptions};
use crate::state::{AppState, now_string};
use crate::tools;
use anyhow::anyhow;
use axum::Json;
use axum::extract::{Multipart, Path, State};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Deserialize)]
pub(crate) struct PostMessage {
    text: Option<String>,
    reply_to: Option<i64>,
    quote: Option<String>,
    sticker_id: Option<String>,
    gif_id: Option<String>,
}

pub(super) async fn post_message(State(st): S, Path(id): Path<String>, Json(req): Json<PostMessage>) -> ApiResult {
    let msg = add_message(&st, &id, req, 8000, None).await?;
    let (rec, chat) = st.snapshot(&id).await?;
    let all = timeline_messages(&rec.export, &chat);
    let mine: Vec<&Message> = all.iter().copied().filter(|m| m.id == msg).collect();
    let view = view_messages(&rec.export, &chat, &mine);
    Ok(Json(json!({"messages": view})))
}

/// Appends a message of the human side and returns its id. `author` is the
/// visitor session id in share rooms.
pub(crate) async fn add_message(
    st: &Arc<AppState>,
    id: &str,
    req: PostMessage,
    max_chars: usize,
    author: Option<(&str, &str)>,
) -> Result<i64, ApiError> {
    let (rec, chat) = st.snapshot(id).await?;
    let export = &rec.export;
    let text = req.text.unwrap_or_default().trim().to_string();
    if text.chars().count() > max_chars {
        return Err(bad("message is too long"));
    }
    let media = match (req.sticker_id.as_deref(), req.gif_id.as_deref()) {
        (Some(sid), _) | (None, Some(sid)) => {
            let item = export
                .library_item(sid)
                .filter(|i| matches!(i.kind, MediaKind::Sticker | MediaKind::Gif))
                .ok_or_else(|| bad("unknown sticker/GIF id"))?;
            Some(crate::model::Media {
                kind: Some(item.kind),
                library_id: Some(item.id.clone()),
                file: item.file.clone(),
                thumb: item.thumb.clone(),
                ..Default::default()
            })
        }
        _ => None,
    };
    if text.is_empty() && media.is_none() {
        return Err(bad("empty message"));
    }
    let reply_to = match req.reply_to {
        Some(r) if crate::chat::find_message(export, &chat.events, r).is_none() => return Err(bad("reply target not found")),
        other => other,
    };
    let quote = req.quote.map(|q| q.trim().chars().take(1000).collect::<String>()).filter(|q| !q.is_empty() && reply_to.is_some());
    let msg_id = st.alloc_ids(id, 1).await?;
    // In a room each visitor writes under their own name (and session, for "mine").
    let from = author.map_or_else(|| chat.user.clone(), |(_, name)| name.to_string());
    let m = Message { id: msg_id, from, date: Some(now_string()), reply_to, quote, text, media, ..Default::default() };
    let ev = Event::Message(m);
    st.append_events(id, std::slice::from_ref(&ev)).await?;
    if let Some(a) = author {
        let mut db = st.db.write().await;
        if let Some(c) = db.chats.get_mut(id)
            && let Some(g) = c.guest.as_mut()
        {
            g.authors.insert(msg_id, a.0.to_string());
            st.paths.save_chat(c)?;
        }
    }
    Ok(msg_id)
}

pub(super) async fn post_voice(State(st): S, Path(id): Path<String>, mp: Multipart) -> ApiResult {
    let msg = add_voice(&st, &id, mp, 20 * 1024 * 1024, None).await?;
    let (rec, chat) = st.snapshot(&id).await?;
    let all = timeline_messages(&rec.export, &chat);
    let mine: Vec<&Message> = all.iter().copied().filter(|m| m.id == msg).collect();
    let view = view_messages(&rec.export, &chat, &mine);
    Ok(Json(json!({"messages": view})))
}

/// Stores a voice message recorded in the UI (multipart: `file`, optional
/// `duration` in seconds and `reply_to`), transcribes it with the media model
/// so the persona knows what was said, and appends it as the user's message.
pub(crate) async fn add_voice(
    st: &Arc<AppState>,
    id: &str,
    mut mp: Multipart,
    max_bytes: usize,
    author: Option<(&str, &str)>,
) -> Result<i64, ApiError> {
    let (rec, chat) = st.snapshot(id).await?;
    let (mut bytes, mut duration, mut reply_to) = (Vec::new(), None::<f64>, None::<i64>);
    while let Some(mut field) = mp.next_field().await.map_err(|e| multipart_error(e, None))? {
        match field.name().unwrap_or_default() {
            "file" => {
                while let Some(chunk) = field.chunk().await.map_err(|e| multipart_error(e, None))? {
                    if bytes.len() + chunk.len() > max_bytes {
                        return Err(bad("the voice message is too long"));
                    }
                    bytes.extend_from_slice(&chunk);
                }
            }
            "duration" => duration = field.text().await.ok().and_then(|t| t.trim().parse().ok()),
            "reply_to" => reply_to = field.text().await.ok().and_then(|t| t.trim().parse().ok()),
            _ => {}
        }
    }
    let ext = match crate::genmedia::sniff(&bytes) {
        Some((MediaKind::Voice, ext)) => ext,
        _ => return Err(bad("not a supported audio recording (wav, ogg, mp3, m4a, webm)")),
    };
    if reply_to.is_some_and(|r| crate::chat::find_message(&rec.export, &chat.events, r).is_none()) {
        return Err(bad("reply target not found"));
    }
    let rel = format!("generated/{id}/usr_{}.{ext}", crate::share::random_id(10));
    let path = st.paths.export_files(&rec.id).join(&rel);
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await.map_err(anyhow::Error::from)?;
    }
    tokio::fs::write(&path, &bytes).await.map_err(anyhow::Error::from)?;
    let status = crate::genmedia::wav_duration(&bytes)
        .or_else(|| duration.filter(|d| d.is_finite() && *d >= 0.0).map(|d| crate::genmedia::fmt_duration(d.round() as u64)));
    let cfg = st.media_llm_config().await;
    let format = crate::genmedia::audio_format(ext).unwrap_or("wav");
    let transcript =
        match tokio::time::timeout(std::time::Duration::from_secs(90), crate::describe::transcribe(&st.llm, &cfg, &bytes, format)).await {
            Ok(Ok(t)) => Some(t),
            Ok(Err(e)) => {
                tracing::warn!(chat = id, model = %cfg.model, "voice transcription failed (set a media model that accepts audio): {e:#}");
                None
            }
            Err(_) => {
                tracing::warn!(chat = id, "voice transcription timed out");
                None
            }
        };
    let media =
        crate::model::Media { kind: Some(MediaKind::Voice), file: Some(rel), status, description: transcript, ..Default::default() };
    let msg_id = st.alloc_ids(id, 1).await?;
    let from = author.map_or_else(|| chat.user.clone(), |(_, name)| name.to_string());
    let m = Message { id: msg_id, from, date: Some(now_string()), reply_to, media: Some(media), ..Default::default() };
    st.append_events(id, &[Event::Message(m)]).await?;
    if let Some(a) = author {
        let mut db = st.db.write().await;
        if let Some(c) = db.chats.get_mut(id)
            && let Some(g) = c.guest.as_mut()
        {
            g.authors.insert(msg_id, a.0.to_string());
            st.paths.save_chat(c)?;
        }
    }
    Ok(msg_id)
}

#[derive(Deserialize)]
pub(crate) struct PostReaction {
    pub message_id: i64,
    pub emoji: String,
}

pub(super) async fn post_reaction(State(st): S, Path(id): Path<String>, Json(req): Json<PostReaction>) -> ApiResult {
    add_reaction(&st, &id, req, None).await?;
    Ok(Json(json!({"ok": true})))
}

/// `from`: who reacts, when not the chat's user (a visitor in a room).
pub(crate) async fn add_reaction(st: &Arc<AppState>, id: &str, req: PostReaction, from: Option<&str>) -> Result<(), ApiError> {
    let (rec, chat) = st.snapshot(id).await?;
    if crate::chat::find_message(&rec.export, &chat.events, req.message_id).is_none() {
        return Err(bad("message not found"));
    }
    let emoji = req.emoji.trim().to_string();
    if emoji.chars().count() > 8 {
        return Err(bad("invalid emoji"));
    }
    let from = from.map_or_else(|| chat.user.clone(), str::to_string);
    let ev = Event::Reaction { target: req.message_id, removed: emoji.is_empty(), emoji, from, date: now_string() };
    st.append_events(id, &[ev]).await?;
    Ok(())
}

async fn turn_response(st: &Arc<AppState>, id: &str, outcome: agent::TurnOutcome) -> ApiResult {
    turn_view(st, id, outcome, view_messages).await
}

/// Messages and ordered actions of a persona turn, rendered by `view`.
pub(crate) async fn turn_view(
    st: &Arc<AppState>,
    id: &str,
    outcome: agent::TurnOutcome,
    view: impl FnOnce(&ChatExport, &Chat, &[&Message]) -> Vec<Value>,
) -> ApiResult {
    let (rec, chat) = st.snapshot(id).await?;
    let ids: Vec<i64> = outcome
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Message(m) => Some(m.id),
            _ => None,
        })
        .collect();
    let reacted: Vec<i64> = outcome
        .events
        .iter()
        .filter_map(|e| match e {
            Event::Reaction { target, .. } => Some(*target),
            _ => None,
        })
        .collect();
    let wanted: Vec<&Message> =
        timeline_messages(&rec.export, &chat).into_iter().filter(|m| ids.contains(&m.id) || reacted.contains(&m.id)).collect();
    let views = view(&rec.export, &chat, &wanted);
    let messages: Vec<&Value> = views.iter().filter(|v| v["id"].as_i64().is_some_and(|i| ids.contains(&i))).collect();
    let updated: Vec<&Value> = views.iter().filter(|v| v["id"].as_i64().is_some_and(|i| reacted.contains(&i))).collect();
    // Order of actions, so the UI can replay them with typing delays.
    let actions: Vec<Value> = outcome
        .events
        .iter()
        .map(|e| match e {
            Event::Message(m) => json!({"type": "message", "id": m.id}),
            Event::Reaction { target, emoji, .. } => json!({"type": "reaction", "target": target, "emoji": emoji}),
        })
        .collect();
    Ok(Json(json!({"messages": messages, "updated": updated, "actions": actions, "error": outcome.error, "usage": outcome.usage})))
}

pub(super) async fn respond(State(st): S, Path(id): Path<String>) -> ApiResult {
    let outcome = agent::respond(&st, &id).await?;
    turn_response(&st, &id, outcome).await
}

pub(super) async fn regenerate(State(st): S, Path(id): Path<String>) -> ApiResult {
    drop_last_persona_turn(&st, &id).await?;
    let outcome = agent::respond(&st, &id).await?;
    turn_response(&st, &id, outcome).await
}

pub(crate) async fn drop_last_persona_turn(st: &Arc<AppState>, id: &str) -> Result<(), ApiError> {
    let lock = st.gen_lock(id);
    let _g = lock.lock().await;
    let mut db = st.db.write().await;
    let chat = db.chats.get_mut(id).ok_or_else(|| anyhow!("chat not found"))?;
    let persona = chat.persona.clone();
    while chat.events.last().is_some_and(|e| e.actor() == persona) {
        chat.events.pop();
    }
    st.paths.save_chat(chat)?;
    Ok(())
}

pub(super) async fn get_prompt(State(st): S, Path(id): Path<String>) -> ApiResult {
    let (rec, chat) = st.snapshot(&id).await?;
    let budget = st.db.read().await.settings.history_budget_chars;
    let (mcp_defs, note, media_tools) = {
        let m = st.mcp.lock().await;
        (m.tool_definitions(), m.instructions_note(), m.media_tools())
    };
    let p = prompt::build(
        &rec.export,
        &chat,
        &PromptOptions { budget_chars: budget, extra_tools_note: note, media_tools: media_tools.clone() },
    );
    let mut tool_defs = tools::definitions();
    tool_defs.extend(tools::media_definitions(&media_tools));
    tool_defs.extend(mcp_defs);
    let cfg = st.llm_config().await;
    Ok(Json(json!({
        "model": cfg.model,
        "messages": p.messages,
        "tools": tool_defs,
        "dropped_turns": p.dropped_turns,
        "total_turns": p.total_turns,
        // Settings → History budget: how much of the chat (in characters) is replayed.
        "history_budget_chars": budget,
        // The next reply will first summarize these turns into the memory.
        "pending_compaction": p.compaction.map(|c| json!({"from": c.from, "to": c.to})),
        "memory": chat.memory,
        "last_usage": chat.last_usage,
    })))
}
