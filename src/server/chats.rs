//! Chats: creating them (from an export or from notes), reading pages of the timeline,
//! renaming and deleting, and how messages are presented to the UI.

use super::exports::export_summary;
use super::{ApiError, ApiResult, S, bad};
use crate::chat::{Chat, Event, Item, timeline};
use crate::model::{ChatExport, MediaKind, Message};
use crate::state::{AppState, now_string};
use crate::store::ExportRecord;
use crate::style::infer_reactors;
use anyhow::anyhow;
use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

pub(crate) fn chat_summary(c: &Chat, export: Option<&ExportRecord>) -> Value {
    let last = c
        .events
        .iter()
        .rev()
        .find_map(|e| match e {
            Event::Message(m) => Some(m),
            _ => None,
        })
        .or_else(|| export.and_then(|r| r.export.messages.iter().rev().find(|m| !m.service)));
    json!({
        "id": c.id,
        "export_id": c.export_id,
        "title": c.title,
        "persona": c.persona,
        "user": c.user,
        "updated_at": c.updated_at,
        "persona_avatar": export.and_then(|r| r.export.participants.iter().find(|p| p.name == c.persona).and_then(|p| p.avatar.clone())),
        "last": last.map(|m| json!({"from": m.from, "preview": m.preview(80), "date": m.date})),
        "guest": c.guest.as_ref().map(|g| json!({"share_id": g.share_id})),
    })
}

#[derive(Deserialize)]
pub(super) struct NewChat {
    /// Empty: a chat without an imported history, from `persona`, `user` and `notes` only.
    #[serde(default)]
    export_id: String,
    persona: String,
    user: Option<String>,
    title: Option<String>,
    notes: Option<String>,
}

pub fn make_chat(rec: &ExportRecord, persona: &str, user: Option<&str>, title: Option<&str>, notes: &str) -> Result<Chat, ApiError> {
    let e = &rec.export;
    if !e.participants.iter().any(|p| p.name == persona) {
        return Err(bad(format!("`{persona}` is not a participant of this export")));
    }
    let user = user
        .map(str::to_string)
        .filter(|u| !u.trim().is_empty())
        .or_else(|| e.participants.iter().find(|p| p.name != persona).map(|p| p.name.clone()))
        .unwrap_or_else(|| "Me".into());
    if user == persona {
        return Err(bad("persona and user must be different people"));
    }
    let max_id = e.messages.iter().map(|m| m.id).max().unwrap_or(0);
    let now = now_string();
    Ok(Chat {
        id: uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
        export_id: rec.id.clone(),
        title: title.map(str::trim).filter(|t| !t.is_empty()).map(String::from).unwrap_or_else(|| persona.to_string()),
        persona: persona.to_string(),
        user,
        notes: notes.to_string(),
        voice: String::new(),
        created_at: now.clone(),
        updated_at: now,
        events: Vec::new(),
        next_id: max_id + 1,
        memory: None,
        last_usage: None,
        guest: None,
        generated: Default::default(),
    })
}

pub(super) async fn create_chat(State(st): S, Json(req): Json<NewChat>) -> ApiResult {
    let rec = if req.export_id.is_empty() {
        blank_export(&st, &req).await?
    } else {
        st.db.read().await.exports.get(&req.export_id).cloned().ok_or_else(|| anyhow!("export not found"))?
    };
    // A blank export holds the cleaned-up names: the person first, then the user.
    let (persona, user) = match &rec.export.participants[..] {
        [p, u] if rec.export.blank => (p.name.as_str(), Some(u.name.as_str())),
        _ => (req.persona.as_str(), req.user.as_deref()),
    };
    let chat = make_chat(&rec, persona, user, req.title.as_deref(), req.notes.as_deref().unwrap_or_default())?;
    st.paths.save_chat(&chat)?;
    let summary = chat_summary(&chat, Some(&rec));
    st.db.write().await.chats.insert(chat.id.clone(), chat);
    Ok(Json(summary))
}

/// The export behind a chat started from notes only: the two people, no messages.
async fn blank_export(st: &AppState, req: &NewChat) -> Result<Arc<ExportRecord>, ApiError> {
    let clean = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let persona = clean(&req.persona);
    let user = req.user.as_deref().map(clean).filter(|u| !u.is_empty()).unwrap_or_else(|| "Me".into());
    if persona.is_empty() {
        return Err(bad("enter the name of the person"));
    }
    if persona.chars().count() > 64 || user.chars().count() > 64 {
        return Err(bad("names can be at most 64 characters"));
    }
    if persona == user {
        return Err(bad("the person and you need different names"));
    }
    if req.notes.as_deref().unwrap_or_default().chars().count() > 20_000 {
        return Err(bad("the notes can be at most 20,000 characters"));
    }
    let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
    let rec = ExportRecord {
        id: id.clone(),
        imported_at: now_string(),
        // Never equal to an import's, so no import is ever taken for this one.
        fingerprint: format!("blank:{id}"),
        export: ChatExport {
            title: persona.clone(),
            participants: [persona, user].map(|name| crate::model::Participant { name, avatar: None, messages: 0 }).into(),
            blank: true,
            ..Default::default()
        },
    };
    st.paths.save_export(&rec)?;
    let rec = Arc::new(rec);
    st.db.write().await.exports.insert(id, rec.clone());
    Ok(rec)
}

/// Messages of the merged export + live timeline in display order.
pub(crate) fn timeline_messages<'a>(export: &'a ChatExport, chat: &'a Chat) -> Vec<&'a Message> {
    timeline(export, &chat.events)
        .into_iter()
        .filter_map(|item| match item {
            Item::Msg(m) => Some(m),
            _ => None,
        })
        .collect()
}

/// Window of the last `limit` messages positioned strictly before message
/// `before` (or the end of the timeline), plus whether older ones exist.
pub(crate) fn page_messages<'a, 'b>(
    all: &'b [&'a Message],
    before: Option<i64>,
    limit: usize,
) -> Result<(&'b [&'a Message], bool), ApiError> {
    let end = match before {
        Some(id) => all.iter().position(|m| m.id == id).ok_or_else(|| bad("unknown message id in `before`"))?,
        None => all.len(),
    };
    let start = end.saturating_sub(limit.clamp(1, MAX_PAGE));
    Ok((&all[start..end], start > 0))
}

const MAX_PAGE: usize = 2000;

/// UI view of the given timeline messages with merged reactions.
pub(crate) fn view_messages(export: &ChatExport, chat: &Chat, msgs: &[&Message]) -> Vec<Value> {
    view_messages_as(export, chat, &chat.user, msgs)
}

/// [`view_messages`] for `viewer`: their messages and reactions are the `mine` ones.
pub(crate) fn view_messages_as(export: &ChatExport, chat: &Chat, viewer: &str, msgs: &[&Message]) -> Vec<Value> {
    let names: Vec<String> = export.participants.iter().map(|p| p.name.clone()).collect();
    // message id -> list of (reactor, emoji), one reaction per reactor.
    let mut reactions: HashMap<i64, Vec<(String, String)>> = HashMap::new();
    let mut unknown: HashMap<i64, Vec<(String, u32)>> = HashMap::new();
    let wanted: HashSet<i64> = msgs.iter().map(|m| m.id).collect();
    for m in msgs {
        for r in &m.reactions {
            let who = infer_reactors(m, r.count, &r.from, &names);
            if who.is_empty() {
                unknown.entry(m.id).or_default().push((r.emoji.clone(), r.count));
            }
            for w in who {
                reactions.entry(m.id).or_default().push((w, r.emoji.clone()));
            }
        }
    }
    // Live reactions: one per reactor and message, and a new one replaces the old.
    for e in &chat.events {
        if let Event::Reaction { target, emoji, from, removed, .. } = e
            && wanted.contains(target)
        {
            let list = reactions.entry(*target).or_default();
            list.retain(|(f, _)| f != from);
            if !*removed {
                list.push((from.clone(), emoji.clone()));
            }
        }
    }
    let max_export_id = export.messages.iter().map(|m| m.id).max().unwrap_or(i64::MIN);
    msgs.iter()
        .map(|m| {
            let mut agg: Vec<(String, u32, bool)> = Vec::new();
            for (from, emoji) in reactions.get(&m.id).cloned().unwrap_or_default() {
                match agg.iter_mut().find(|(e, _, _)| *e == emoji) {
                    Some(a) => {
                        a.1 += 1;
                        a.2 |= from == viewer;
                    }
                    None => agg.push((emoji, 1, from == viewer)),
                }
            }
            for (emoji, count) in unknown.get(&m.id).cloned().unwrap_or_default() {
                agg.push((emoji, count, false));
            }
            let mut v = serde_json::to_value(m).unwrap_or_default();
            v["reactions"] = json!(agg.iter().map(|(e, c, mine)| json!({"emoji": e, "count": c, "mine": mine})).collect::<Vec<_>>());
            v["mine"] = json!(m.from == viewer);
            v["persona"] = json!(m.from == chat.persona);
            v["live"] = json!(m.id > max_export_id);
            if let Some(lid) = m.media.as_ref().and_then(|x| x.library_id.as_deref())
                && let Some(item) = export.library_item(lid)
            {
                v["media"]["available"] = json!(item.available);
                if let (Some(s), None) = (item.size, m.media.as_ref().and_then(|x| x.size)) {
                    v["media"]["size"] = json!(s);
                }
                if let Some(d) = &item.description {
                    v["media"]["description"] = json!(d);
                }
            }
            v
        })
        .collect()
}

/// Whole-timeline counters for the chat header/statistics, without serializing messages.
fn timeline_stats(export: &ChatExport, msgs: &[&Message]) -> Value {
    let max_export_id = export.messages.iter().map(|m| m.id).max().unwrap_or(i64::MIN);
    let (mut count, mut live, mut links) = (0usize, 0usize, 0usize);
    let mut by: BTreeMap<&str, usize> = BTreeMap::new();
    let mut media: BTreeMap<&str, usize> = BTreeMap::new();
    let (mut first, mut last) = (None::<&str>, None::<&str>);
    for m in msgs {
        if m.id > max_export_id {
            live += 1;
        }
        if m.service {
            continue;
        }
        count += 1;
        *by.entry(m.from.as_str()).or_default() += 1;
        if let Some(d) = m.date.as_deref() {
            first = first.or(Some(d));
            last = Some(d);
        }
        if m.link_preview.is_some() || m.text.contains("http://") || m.text.contains("https://") {
            links += 1;
        }
        if let Some(k) = m.media.as_ref().and_then(|x| x.kind) {
            let key = match k {
                MediaKind::Photo => "photo",
                MediaKind::Video => "video",
                MediaKind::RoundVideo => "round_video",
                MediaKind::Voice => "voice",
                MediaKind::Audio => "audio",
                MediaKind::File => "file",
                MediaKind::Sticker => "sticker",
                MediaKind::Gif => "gif",
                _ => continue,
            };
            *media.entry(key).or_default() += 1;
        }
    }
    json!({"messages": count, "live": live, "by": by, "media": media, "links": links, "first_date": first, "last_date": last})
}

#[derive(Deserialize)]
pub(super) struct ChatQuery {
    limit: Option<usize>,
    before: Option<i64>,
}

pub(super) async fn get_chat(State(st): S, Path(id): Path<String>, Query(q): Query<ChatQuery>) -> ApiResult {
    let (rec, chat) = st.snapshot(&id).await?;
    let all = timeline_messages(&rec.export, &chat);
    let (page, has_more) = page_messages(&all, q.before, q.limit.unwrap_or(500))?;
    let messages = view_messages(&rec.export, &chat, page);
    let total = all.len();
    if q.before.is_some() {
        return Ok(Json(json!({"messages": messages, "total": total, "has_more": has_more})));
    }
    let library: Vec<Value> = rec
        .export
        .library
        .iter()
        .filter(|i| matches!(i.kind, MediaKind::Sticker | MediaKind::Gif) && i.available)
        .map(|i| json!({"id": i.id, "kind": i.kind, "file": i.file, "thumb": i.thumb, "description": i.description, "uses": i.uses.len()}))
        .collect();
    // Visitor conversations: the owner, the persona and visitors, so names on the bubbles.
    let mut export_view = export_summary(&rec);
    export_view["multi"] = json!(rec.export.participants.len() > 2 || chat.guest.is_some());
    Ok(Json(json!({
        "chat": chat_summary(&chat, Some(&rec)),
        "notes": chat.notes,
        "voice": chat.voice,
        "export": export_view,
        "messages": messages,
        "total": total,
        "has_more": has_more,
        "stats": timeline_stats(&rec.export, &all),
        "library": library,
    })))
}

#[derive(Deserialize)]
pub(super) struct ChatPatch {
    title: Option<String>,
    notes: Option<String>,
    voice: Option<String>,
}

pub(super) async fn update_chat(State(st): S, Path(id): Path<String>, Json(p): Json<ChatPatch>) -> ApiResult {
    let mut db = st.db.write().await;
    let chat = db.chats.get_mut(&id).ok_or_else(|| anyhow!("chat not found"))?;
    if let Some(t) = p.title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
        chat.title = t;
    }
    let notes = p.notes.clone();
    if let Some(n) = p.notes {
        chat.notes = n;
    }
    let voice = p.voice.map(|v| v.trim().chars().take(64).collect::<String>());
    if let Some(v) = &voice {
        chat.voice = v.clone();
    }
    chat.updated_at = now_string();
    st.paths.save_chat(chat)?;
    // Visitor conversations of this chat's share links follow the notes.
    if notes.is_some() || voice.is_some() {
        let shares: Vec<String> = db.shares.values().filter(|s| s.chat_id == id).map(|s| s.id.clone()).collect();
        for c in db.chats.values_mut().filter(|c| c.guest.as_ref().is_some_and(|g| shares.contains(&g.share_id))) {
            if let Some(n) = &notes {
                c.notes = n.clone();
            }
            if let Some(v) = &voice {
                c.voice = v.clone();
            }
            st.paths.save_chat(c)?;
        }
    }
    Ok(Json(json!({"ok": true})))
}

pub(super) async fn delete_chat(State(st): S, Path(id): Path<String>) -> ApiResult {
    let mut db = st.db.write().await;
    let chat = db.chats.remove(&id).ok_or_else(|| anyhow!("chat not found"))?;
    st.paths.delete_chat(&id)?;
    if let Some(g) = chat.guest {
        // A visitor conversation: forget the sessions that pointed at it.
        if let Some(share) = db.shares.get_mut(&g.share_id) {
            share.sessions.retain(|s| s.chat_id != id);
            if share.room_chat_id.as_deref() == Some(id.as_str()) {
                share.room_chat_id = None;
            }
            st.paths.save_share(share)?;
        }
    } else {
        // A template chat: its share links and their conversations go too.
        let shares: Vec<String> = db.shares.values().filter(|s| s.chat_id == id).map(|s| s.id.clone()).collect();
        for sid in &shares {
            crate::share_api::remove_share(&st, &mut db, sid)?;
        }
        // A chat started from notes owns its blank export: nothing else can use it.
        let blank = db.exports.get(&chat.export_id).is_some_and(|r| r.export.blank);
        if blank && !db.chats.values().any(|c| c.export_id == chat.export_id) {
            db.exports.remove(&chat.export_id);
            st.paths.delete_export(&chat.export_id)?;
        }
    }
    Ok(Json(json!({"ok": true})))
}
