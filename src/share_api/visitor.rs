//! The visitor's side, under `/s/<slug>` and `/api/s/<slug>`: the start page, PIN check,
//! the conversation and its media. Every request resolves the visitor's session token.

use super::{BLOCKED, GONE, GUEST_MAX_CHARS, S, not_found, too_many};
use crate::agent;
use crate::chat::{Chat, Event, GuestChat};
use crate::model::{ChatExport, MediaKind, Message};
use crate::server::{ApiError, ApiResult, ClientIp, PostMessage, PostReaction, bad, page_messages, view_messages_as};
use crate::share::{self, GuestSession, HistoryVisibility, IP_FAILS, LINK_FAILS, Share, ShareMode};
use crate::state::{AppState, now_string};
use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

async fn active_share(st: &AppState, slug: &str) -> Result<Share, ApiError> {
    let slug = slug.to_lowercase();
    st.db.read().await.shares.values().find(|s| s.slug == slug && s.is_active()).cloned().ok_or_else(|| not_found(GONE))
}

/// Resolves a visitor session. Unknown tokens count as failed attempts per IP.
async fn resolve(st: &AppState, slug: &str, token: &str, ip: &str) -> Result<(Share, GuestSession), ApiError> {
    let key = format!("token:{ip}");
    st.limiter.lock().expect("limiter").check_locked(&key).map_err(too_many)?;
    let share = active_share(st, slug).await?;
    if share.is_blocked(ip) {
        return Err(ApiError(StatusCode::FORBIDDEN, BLOCKED.into()));
    }
    match share.session_by_token(token).cloned() {
        Some(s) => Ok((share, s)),
        None => {
            st.limiter.lock().expect("limiter").fail(&key, (20, Duration::from_secs(60), Duration::from_secs(3600)));
            Err(not_found("This conversation was ended by the owner of the link, or the link is incomplete."))
        }
    }
}

fn persona_avatar(export: &ChatExport, persona: &str) -> Option<String> {
    export.participants.iter().find(|p| p.name == persona).and_then(|p| p.avatar.clone())
}

pub(super) async fn guest_info(State(st): S, Path(slug): Path<String>, Extension(ClientIp(ip)): Extension<ClientIp>) -> ApiResult {
    let share = active_share(&st, &slug).await?;
    if share.is_blocked(&ip) {
        return Err(ApiError(StatusCode::FORBIDDEN, BLOCKED.into()));
    }
    let db = st.db.read().await;
    let chat = db.chats.get(&share.chat_id).ok_or_else(|| not_found(GONE))?;
    let has_avatar = db.exports.get(&chat.export_id).and_then(|r| persona_avatar(&r.export, &chat.persona)).is_some();
    let full = share.max_sessions > 0 && share.mode == ShareMode::Private && share.sessions.len() >= share.max_sessions;
    Ok(Json(json!({
        "slug": share.slug,
        "display_name": share.display_name,
        "greeting": share.greeting,
        "pin": share.pin_hash.is_some(),
        "pin_numeric": share.pin_numeric,
        "mode": share.mode,
        // The avatar is public only for links without a PIN.
        "avatar": has_avatar && share.pin_hash.is_none(),
        "full": full,
    })))
}

pub(super) async fn guest_avatar(State(st): S, Path(slug): Path<String>, headers: HeaderMap) -> Result<Response, ApiError> {
    let share = active_share(&st, &slug).await?;
    if share.pin_hash.is_some() {
        return Err(not_found("not found"));
    }
    let (rec, chat) = st.snapshot(&share.chat_id).await.map_err(|_| not_found(GONE))?;
    let path = persona_avatar(&rec.export, &chat.persona).ok_or_else(|| not_found("not found"))?;
    crate::server::serve_media(&st, &rec.id, &path, headers).await
}

#[derive(Deserialize, Default)]
pub(super) struct StartReq {
    pin: Option<String>,
    name: Option<String>,
}

pub(super) async fn guest_start(
    State(st): S,
    Path(slug): Path<String>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    body: Option<Json<StartReq>>,
) -> ApiResult {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let share = active_share(&st, &slug).await?;
    if share.is_blocked(&ip) {
        return Err(ApiError(StatusCode::FORBIDDEN, BLOCKED.into()));
    }
    if let Some(hash) = share.pin_hash.clone() {
        let ip_key = format!("pin:{}:{ip}", share.id);
        let link_key = format!("pin:{}", share.id);
        {
            let l = st.limiter.lock().expect("limiter");
            l.check_locked(&ip_key).map_err(too_many)?;
            l.check_locked(&link_key).map_err(too_many)?;
        }
        let pin = req.pin.clone().unwrap_or_default().trim().to_string();
        let ok = !pin.is_empty() && tokio::task::spawn_blocking(move || share::verify_pin(&hash, &pin)).await.unwrap_or(false);
        let mut l = st.limiter.lock().expect("limiter");
        if !ok {
            let lock = [l.fail(&ip_key, IP_FAILS), l.fail(&link_key, LINK_FAILS)].into_iter().flatten().max();
            tracing::warn!(slug = %share.slug, %ip, "wrong share PIN");
            return Err(match lock {
                Some(d) => too_many(d),
                None => ApiError(StatusCode::FORBIDDEN, "Wrong PIN.".into()),
            });
        }
        l.success(&ip_key);
    }
    st.limiter.lock().expect("limiter").hit(&format!("start:{ip}"), 20, Duration::from_secs(3600)).map_err(too_many)?;
    let name =
        req.name.map(|n| n.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(40).collect::<String>()).filter(|n| !n.is_empty());

    let mut db = st.db.write().await;
    // Re-read under the write lock: the share may have changed meanwhile.
    let mut share = db.shares.get(&share.id).cloned().ok_or_else(|| not_found(GONE))?;
    if share.max_sessions > 0 && share.sessions.len() >= share.max_sessions {
        return Err(ApiError(StatusCode::FORBIDDEN, "This link has reached its limit of conversations.".into()));
    }
    let template = db.chats.get(&share.chat_id).cloned().ok_or_else(|| not_found(GONE))?;
    let rec = db.exports.get(&template.export_id).cloned().ok_or_else(|| not_found(GONE))?;
    // A name the persona could take for someone else (itself, the owner, another visitor
    // of the room) gets a suffix: the conversation must tell people apart.
    let name = name.map(|n| {
        let people = [template.persona.as_str(), share.display_name.as_str(), template.user.as_str()];
        let others: Vec<String> = share.sessions.iter().filter(|_| share.mode == ShareMode::Room).map(|s| share.visitor_name(s)).collect();
        share::distinct_name(n, &people, &others.iter().map(String::as_str).collect::<Vec<_>>())
    });
    let visitor = name.clone().unwrap_or_else(|| share::GUEST_NAME.to_string());
    let chat_id = match (share.mode, share.room_chat_id.clone()) {
        (ShareMode::Room, Some(id)) if db.chats.contains_key(&id) => id,
        _ => {
            let chat = guest_chat_from(&template, &rec.export, &share, &visitor);
            st.paths.save_chat(&chat)?;
            let id = chat.id.clone();
            db.chats.insert(id.clone(), chat);
            if share.mode == ShareMode::Room {
                share.room_chat_id = Some(id.clone());
            }
            id
        }
    };
    let session = share::new_session(&chat_id, name, &ip);
    let token = session.token.clone();
    share.sessions.push(session);
    st.paths.save_share(&share)?;
    db.shares.insert(share.id.clone(), share);
    Ok(Json(json!({"token": token})))
}

/// A fresh visitor conversation on the template's export: no live events, the
/// template's notes, and its memory summary when that only covers the export. The persona
/// talks to `visitor` (a private link) or to everyone in the room; the template's user is
/// the host, whom the export history and the notes are about.
fn guest_chat_from(template: &Chat, export: &ChatExport, share: &Share, visitor: &str) -> Chat {
    let now = now_string();
    let mut chat = Chat {
        id: uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
        export_id: template.export_id.clone(),
        title: share.display_name.clone(),
        persona: template.persona.clone(),
        // Room messages carry each visitor's name; `user` is only who "you" are in the owner's view.
        user: if share.mode == ShareMode::Room { share::ROOM_USER.to_string() } else { visitor.to_string() },
        notes: template.notes.clone(),
        voice: template.voice.clone(),
        created_at: now.clone(),
        updated_at: now,
        events: Vec::new(),
        next_id: export.messages.iter().map(|m| m.id).max().unwrap_or(0) + 1,
        memory: None,
        last_usage: None,
        guest: Some(GuestChat { share_id: share.id.clone(), host: template.user.clone(), authors: Default::default() }),
        generated: Default::default(),
    };
    if let Some(m) = &template.memory
        && m.turns < crate::prompt::history_turn_count(export, &chat)
    {
        chat.memory = Some(m.clone());
    }
    chat
}

/// Ids of export messages visitors may read.
fn visible_export_ids(export: &ChatExport, share: &Share) -> Option<HashSet<i64>> {
    match share.history {
        HistoryVisibility::Full => None,
        HistoryVisibility::None => Some(HashSet::new()),
        HistoryVisibility::Recent => {
            let mut ids: Vec<i64> = export.messages.iter().rev().filter(|m| !m.service).take(share.history_recent).map(|m| m.id).collect();
            // Keep day separators / service lines inside that range too.
            if let Some(&first) = ids.last() {
                ids.extend(export.messages.iter().filter(|m| m.service && m.id > first).map(|m| m.id));
            }
            Some(ids.into_iter().collect())
        }
    }
}

/// Timeline messages a visitor may see: filtered history plus live messages.
fn guest_visible<'a>(export: &'a ChatExport, chat: &'a Chat, share: &Share) -> Vec<&'a Message> {
    let max_export_id = export.messages.iter().map(|m| m.id).max().unwrap_or(i64::MIN);
    let visible = visible_export_ids(export, share);
    crate::server::timeline_messages(export, chat)
        .into_iter()
        .filter(|m| m.id > max_export_id || visible.as_ref().is_none_or(|ids| ids.contains(&m.id)))
        .collect()
}

/// What a visitor sees of `msgs` (hidden ones are dropped): the persona under
/// its display name, no media descriptions, and room authors.
fn guest_view(export: &ChatExport, chat: &Chat, share: &Share, session: &GuestSession, msgs: &[&Message]) -> Vec<Value> {
    let max_export_id = export.messages.iter().map(|m| m.id).max().unwrap_or(i64::MIN);
    let visible = visible_export_ids(export, share);
    let allowed: Vec<&Message> =
        msgs.iter().copied().filter(|m| m.id > max_export_id || visible.as_ref().is_none_or(|ids| ids.contains(&m.id))).collect();
    let room = share.mode == ShareMode::Room;
    let authors = chat.guest.as_ref().map(|g| &g.authors);
    let names: std::collections::HashMap<&str, String> = share.sessions.iter().map(|s| (s.id.as_str(), share.visitor_name(s))).collect();
    // Whose reactions are "mine": in a room each visitor reacts under their own name
    // (rooms from before 0.5.6 under the chat's user, shared by all).
    let viewer =
        if room && chat.guest.as_ref().is_some_and(|g| !g.host.is_empty()) { share.visitor_name(session) } else { chat.user.clone() };
    view_messages_as(export, chat, &viewer, &allowed)
        .into_iter()
        .map(|mut v| {
            let id = v["id"].as_i64().unwrap_or_default();
            let persona = v["from"] == json!(chat.persona);
            if persona {
                v["from"] = json!(share.display_name);
            }
            if let Some(md) = v.get_mut("media").and_then(Value::as_object_mut) {
                md.remove("description");
            }
            // Room messages are mine by their author session, whatever the names say.
            if room && id > max_export_id && !persona {
                let author = authors.and_then(|a| a.get(&id));
                let mine = author.is_some_and(|a| *a == session.id);
                v["mine"] = json!(mine);
                if !mine && let Some(n) = author.and_then(|a| names.get(a.as_str())) {
                    v["from"] = json!(n);
                }
            }
            v
        })
        .collect()
}

#[derive(Deserialize)]
pub(super) struct LimitQuery {
    limit: Option<usize>,
    before: Option<i64>,
}

pub(super) async fn guest_chat(
    State(st): S,
    Path((slug, token)): Path<(String, String)>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    Query(q): Query<LimitQuery>,
) -> ApiResult {
    let (share, session) = resolve(&st, &slug, &token, &ip).await?;
    let (rec, chat) = st.snapshot(&session.chat_id).await.map_err(|_| not_found(GONE))?;
    let all = guest_visible(&rec.export, &chat, &share);
    let total = all.len();
    let (page, has_more) = page_messages(&all, q.before, q.limit.unwrap_or(500))?;
    let messages = guest_view(&rec.export, &chat, &share, &session, page);
    if q.before.is_some() {
        return Ok(Json(json!({"messages": messages, "total": total, "has_more": has_more})));
    }
    let library: Vec<Value> = rec
        .export
        .library
        .iter()
        .filter(|i| matches!(i.kind, MediaKind::Sticker | MediaKind::Gif) && i.available)
        .map(|i| json!({"id": i.id, "kind": i.kind, "file": i.file, "thumb": i.thumb}))
        .collect();
    let avatar = persona_avatar(&rec.export, &chat.persona);
    let mut participants = vec![json!({"name": share.display_name, "avatar": avatar})];
    // Names on the bubbles wherever more than the visitor and the persona appear: a room,
    // a group export, or visible history (the host's messages are not the visitor's).
    let multi = share.mode == ShareMode::Room || rec.export.participants.len() > 2 || share.history != HistoryVisibility::None;
    if share.history != HistoryVisibility::None {
        participants
            .extend(rec.export.participants.iter().filter(|p| p.name != chat.persona).map(|p| json!({"name": p.name, "avatar": p.avatar})));
    }
    let used = replies_used(&share, &session);
    Ok(Json(json!({
        "chat": {"id": token, "title": share.display_name, "persona": share.display_name, "user": "You", "persona_avatar": avatar, "updated_at": chat.updated_at},
        "notes": "",
        "export": {"id": "guest", "title": share.display_name, "exporter": null, "participants": participants, "multi": multi},
        "messages": messages,
        "total": total,
        "has_more": has_more,
        "library": library,
        "share": {
            "slug": share.slug, "mode": share.mode, "greeting": share.greeting,
            "replies_left": (share.max_replies > 0).then(|| share.max_replies.saturating_sub(used)),
            "session_name": session.name,
            // Answers come from the owner's Telegram group (or the AI after a countdown):
            // the client waits for them instead of asking the persona to respond.
            "relay": crate::tgbot::relay_active(&st, &share),
            // What the owner allows besides answering: "Let them write first", "Rewrite".
            "nudge": share.allow_nudge,
            "regenerate": share.allow_regenerate,
        },
        "typing": crate::tgbot::is_typing(&st, &session.chat_id),
    })))
}

fn replies_used(share: &Share, session: &GuestSession) -> u32 {
    match share.mode {
        ShareMode::Private => session.replies,
        ShareMode::Room => share.sessions.iter().filter(|s| s.chat_id == session.chat_id).map(|s| s.replies).sum(),
    }
}

fn rate(st: &AppState, key: String, max: u32, window: Duration) -> Result<(), ApiError> {
    st.limiter
        .lock()
        .expect("limiter")
        .hit(&key, max, window)
        .map_err(|d| ApiError(StatusCode::TOO_MANY_REQUESTS, format!("Slow down a little. Try again in {} s.", d.as_secs().max(1))))
}

/// Updates `last_seen` (and the reply counter) of a session.
/// Marks a visitor as active now (from `ip`), counting an answer when `add_reply`.
async fn touch(st: &AppState, share_id: &str, session_id: &str, ip: &str, add_reply: bool) -> Result<(), ApiError> {
    let mut db = st.db.write().await;
    let share = db.shares.get_mut(share_id).ok_or_else(|| not_found(GONE))?;
    if let Some(s) = share.sessions.iter_mut().find(|s| s.id == session_id) {
        s.last_seen = now_string();
        if let Some(ip) = share::known_ip(ip) {
            s.ip = Some(ip);
        }
        if add_reply {
            s.replies += 1;
        }
    }
    st.paths.save_share(share)?;
    Ok(())
}

pub(super) async fn guest_message(
    State(st): S,
    Path((slug, token)): Path<(String, String)>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    Json(req): Json<PostMessage>,
) -> ApiResult {
    let (share, session) = resolve(&st, &slug, &token, &ip).await?;
    rate(&st, format!("msg:{}", session.id), 30, Duration::from_secs(60))?;
    let name = share.visitor_name(&session);
    let author = (share.mode == ShareMode::Room).then_some((session.id.as_str(), name.as_str()));
    let id = crate::server::add_message(&st, &session.chat_id, req, GUEST_MAX_CHARS, author).await?;
    touch(&st, &share.id, &session.id, &ip, false).await?;
    crate::tgbot::relay_guest_message(&st, &share, &session, id).await;
    let (rec, chat) = st.snapshot(&session.chat_id).await?;
    let mine: Vec<&Message> = guest_visible(&rec.export, &chat, &share).into_iter().filter(|m| m.id == id).collect();
    let view = guest_view(&rec.export, &chat, &share, &session, &mine);
    Ok(Json(json!({"messages": view})))
}

pub(super) async fn guest_voice(
    State(st): S,
    Path((slug, token)): Path<(String, String)>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    mp: axum::extract::Multipart,
) -> ApiResult {
    let (share, session) = resolve(&st, &slug, &token, &ip).await?;
    // Each voice message costs a transcription call.
    rate(&st, format!("voice:{}", session.id), 10, Duration::from_secs(60))?;
    let name = share.visitor_name(&session);
    let author = (share.mode == ShareMode::Room).then_some((session.id.as_str(), name.as_str()));
    let id = crate::server::add_voice(&st, &session.chat_id, mp, 5 * 1024 * 1024, author).await?;
    touch(&st, &share.id, &session.id, &ip, false).await?;
    crate::tgbot::relay_guest_message(&st, &share, &session, id).await;
    let (rec, chat) = st.snapshot(&session.chat_id).await?;
    let mine: Vec<&Message> = guest_visible(&rec.export, &chat, &share).into_iter().filter(|m| m.id == id).collect();
    let view = guest_view(&rec.export, &chat, &share, &session, &mine);
    Ok(Json(json!({"messages": view})))
}

pub(super) async fn guest_react(
    State(st): S,
    Path((slug, token)): Path<(String, String)>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    Json(req): Json<PostReaction>,
) -> ApiResult {
    let (share, session) = resolve(&st, &slug, &token, &ip).await?;
    rate(&st, format!("msg:{}", session.id), 30, Duration::from_secs(60))?;
    let (rec, chat) = st.snapshot(&session.chat_id).await?;
    // Visitors can only react to what they can see.
    if !guest_visible(&rec.export, &chat, &share).iter().any(|m| m.id == req.message_id) {
        return Err(bad("message not found"));
    }
    let from = (share.mode == ShareMode::Room).then(|| share.visitor_name(&session));
    crate::server::add_reaction(&st, &session.chat_id, req, from.as_deref()).await?;
    Ok(Json(json!({"ok": true})))
}

async fn guest_turn(st: Arc<AppState>, slug: String, token: String, ip: String, regenerate: bool) -> ApiResult {
    let (share, session) = resolve(&st, &slug, &token, &ip).await?;
    if crate::tgbot::relay_active(&st, &share) {
        return Err(ApiError(StatusCode::CONFLICT, format!("{} answers on their own here.", share.display_name)));
    }
    if share.max_replies > 0 && replies_used(&share, &session) >= share.max_replies {
        return Err(ApiError(StatusCode::FORBIDDEN, format!("{} has to go now. This conversation reached its limit.", share.display_name)));
    }
    if regenerate && !share.allow_regenerate {
        return Err(ApiError(StatusCode::FORBIDDEN, "Answers can't be rewritten on this link.".into()));
    }
    // An answer needs something to answer: a visitor's message or reaction after the
    // persona's last message. Without one this is "write first", which the owner may not allow.
    if !regenerate && !share.allow_nudge {
        let (_, chat) = st.snapshot(&session.chat_id).await.map_err(|_| not_found(GONE))?;
        if !awaits_answer(&chat) {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                format!("{} answers your messages here: write something first.", share.display_name),
            ));
        }
    }
    rate(&st, format!("turn:{}", session.id), 10, Duration::from_secs(60))?;
    rate(&st, format!("turn-link:{}", share.id), 120, Duration::from_secs(60))?;
    touch(&st, &share.id, &session.id, &ip, true).await?;
    if regenerate {
        crate::server::drop_last_persona_turn(&st, &session.chat_id).await?;
    }
    let outcome = agent::respond(&st, &session.chat_id).await?;
    // Provider errors may contain endpoint details, so visitors get a generic message.
    let outcome = agent::TurnOutcome {
        error: outcome.error.map(|e| {
            tracing::warn!(slug = %share.slug, "visitor turn failed: {e}");
            format!("{} can't answer right now. Try again in a moment.", share.display_name)
        }),
        usage: None,
        ..outcome
    };
    let chat_id = session.chat_id.clone();
    crate::server::turn_view(&st, &chat_id, outcome, move |e, c, m| guest_view(e, c, &share, &session, m)).await
}

/// Whether the visitor did something the persona can answer since its last message: wrote,
/// or reacted (a ❤ on its message may get a reply, as in a real chat).
fn awaits_answer(chat: &Chat) -> bool {
    chat.events
        .iter()
        .rev()
        .find_map(|e| match e {
            Event::Message(m) if !m.service => Some(m.from != chat.persona),
            Event::Reaction { from, removed: false, .. } if *from != chat.persona => Some(true),
            _ => None,
        })
        .unwrap_or(false)
}

pub(super) async fn guest_respond(
    State(st): S,
    Path((slug, token)): Path<(String, String)>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
) -> ApiResult {
    guest_turn(st, slug, token, ip, false).await
}

pub(super) async fn guest_regenerate(
    State(st): S,
    Path((slug, token)): Path<(String, String)>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
) -> ApiResult {
    guest_turn(st, slug, token, ip, true).await
}

/// Whether a visitor may load `path` of the export: stickers/GIFs (picker),
/// the persona avatar, media of live messages, and media of visible history.
fn guest_media_allowed(export: &ChatExport, chat: &Chat, share: &Share, path: &str) -> bool {
    if share.history == HistoryVisibility::Full {
        return true;
    }
    let is = |p: &Option<String>| p.as_deref() == Some(path);
    if export.library.iter().any(|i| matches!(i.kind, MediaKind::Sticker | MediaKind::Gif) && (is(&i.file) || is(&i.thumb))) {
        return true;
    }
    if export.participants.iter().any(|p| p.name == chat.persona && is(&p.avatar)) {
        return true;
    }
    let msg_has = |m: &crate::model::Message| {
        m.media.as_ref().is_some_and(|md| is(&md.file) || is(&md.thumb)) || m.link_preview.as_ref().is_some_and(|lp| is(&lp.photo))
    };
    if chat.events.iter().any(|e| matches!(e, Event::Message(m) if msg_has(m))) {
        return true;
    }
    if share.history == HistoryVisibility::Recent {
        return export.messages.iter().rev().filter(|m| !m.service).take(share.history_recent).any(msg_has);
    }
    false
}

pub(super) async fn guest_media(
    State(st): S,
    Path((slug, token, path)): Path<(String, String, String)>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (share, session) = resolve(&st, &slug, &token, &ip).await?;
    let (rec, chat) = st.snapshot(&session.chat_id).await.map_err(|_| not_found(GONE))?;
    if !guest_media_allowed(&rec.export, &chat, &share, &path) {
        return Err(not_found("not found"));
    }
    crate::server::serve_media(&st, &rec.id, &path, headers).await
}
