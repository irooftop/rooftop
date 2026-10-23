//! HTTP handlers for share links: owner management (behind the owner auth)
//! and the public visitor API under `/s/<slug>` and `/api/s/<slug>`.

use crate::agent;
use crate::chat::{Chat, Event, GuestChat};
use crate::model::{ChatExport, MediaKind, Message};
use crate::server::{ApiError, ApiResult, ClientIp, PostMessage, PostReaction, bad, page_messages, view_messages};
use crate::share::{self, GuestSession, HistoryVisibility, IP_FAILS, LINK_FAILS, Share, ShareMode};
use crate::state::{AppState, now_string};
use crate::store::Db;
use anyhow::anyhow;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

type S = State<Arc<AppState>>;

const GONE: &str = "This link doesn't exist or is no longer active.";
const GUEST_MAX_CHARS: usize = 4000;

fn not_found(msg: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, msg.into())
}

fn too_many(wait: Duration) -> ApiError {
    let secs = wait.as_secs().max(1);
    let human = if secs >= 120 { format!("{} min", secs.div_ceil(60)) } else { format!("{secs} s") };
    ApiError(StatusCode::TOO_MANY_REQUESTS, format!("Too many attempts. Try again in {human}."))
}

pub fn guest_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/s/{slug}", get(crate::server::guest_gate_index))
        .route("/s/{slug}/{token}", get(crate::server::guest_chat_index))
        .route("/api/s/{slug}", get(guest_info))
        .route("/api/s/{slug}/avatar", get(guest_avatar))
        .route("/api/s/{slug}/sessions", post(guest_start))
        .route("/api/s/{slug}/c/{token}", get(guest_chat))
        .route("/api/s/{slug}/c/{token}/messages", post(guest_message))
        .route("/api/s/{slug}/c/{token}/react", post(guest_react))
        .route("/api/s/{slug}/c/{token}/voice", post(guest_voice))
        .route("/api/s/{slug}/c/{token}/respond", post(guest_respond))
        .route("/api/s/{slug}/c/{token}/regenerate", post(guest_regenerate))
        .route("/api/s/{slug}/c/{token}/media/{*path}", get(guest_media))
}

// ---------------------------------------------------------------- owner side

pub fn share_summary(s: &Share) -> Value {
    json!({
        "id": s.id,
        "chat_id": s.chat_id,
        "slug": s.slug,
        "display_name": s.display_name,
        "greeting": s.greeting,
        "has_pin": s.pin_hash.is_some(),
        "mode": s.mode,
        "history": s.history,
        "history_recent": s.history_recent,
        "enabled": s.enabled,
        "active": s.is_active(),
        "expires_at": s.expires_at,
        "max_sessions": s.max_sessions,
        "max_replies": s.max_replies,
        "telegram_chat_id": s.telegram_chat_id,
        "ai_delay_secs": s.ai_delay_secs,
        "allow_nudge": s.allow_nudge,
        "allow_regenerate": s.allow_regenerate,
        "created_at": s.created_at,
        "sessions": s.sessions.iter().map(|g| json!({
            "id": g.id, "name": g.name, "chat_id": g.chat_id,
            "created_at": g.created_at, "last_seen": g.last_seen, "replies": g.replies,
        })).collect::<Vec<_>>(),
    })
}

fn share_detail(s: &Share, db: &Db) -> Value {
    let mut v = share_summary(s);
    if let Some(list) = v["sessions"].as_array_mut() {
        for g in list {
            let chat = g["chat_id"].as_str().and_then(|c| db.chats.get(c));
            let n = chat.map_or(0, |c| c.events.iter().filter(|e| matches!(e, Event::Message(_))).count());
            g["messages"] = json!(n);
        }
    }
    v
}

pub async fn list_shares(State(st): S, Path(chat_id): Path<String>) -> ApiResult {
    let db = st.db.read().await;
    if !db.chats.contains_key(&chat_id) {
        return Err(anyhow!("chat not found").into());
    }
    let shares: Vec<Value> = db.shares.values().filter(|s| s.chat_id == chat_id).map(|s| share_detail(s, &db)).collect();
    Ok(Json(json!({"shares": shares})))
}

#[derive(Deserialize, Default)]
pub struct ShareInput {
    slug: Option<String>,
    display_name: Option<String>,
    greeting: Option<String>,
    /// `None` keeps the PIN, `""` removes it.
    pin: Option<String>,
    mode: Option<ShareMode>,
    history: Option<HistoryVisibility>,
    history_recent: Option<usize>,
    enabled: Option<bool>,
    /// `null` / `""` removes the expiry.
    #[serde(default, deserialize_with = "double_option")]
    expires_at: Option<Option<String>>,
    max_sessions: Option<usize>,
    max_replies: Option<u32>,
    /// Telegram group for visitor messages, `null` turns the relay off.
    #[serde(default, deserialize_with = "double_option")]
    telegram_chat_id: Option<Option<i64>>,
    ai_delay_secs: Option<u32>,
    allow_nudge: Option<bool>,
    allow_regenerate: Option<bool>,
    /// Ends all visitor conversations (deletes them).
    #[serde(default)]
    revoke_sessions: bool,
}

fn double_option<'de, T: Deserialize<'de>, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
    Ok(Some(Option::<T>::deserialize(d)?))
}

fn normalize_expiry(v: &str) -> Result<Option<String>, ApiError> {
    let v = v.trim();
    if v.is_empty() {
        return Ok(None);
    }
    let dt = chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(v, "%Y-%m-%dT%H:%M"))
        .map_err(|_| bad("expiry must be a date and time like 2026-12-31T23:59"))?;
    Ok(Some(dt.format("%Y-%m-%dT%H:%M:%S").to_string()))
}

/// Applies `input` to `share` (validation included). Returns whether sessions must be revoked.
async fn apply_input(st: &AppState, share: &mut Share, input: ShareInput, db: &Db) -> Result<bool, ApiError> {
    if let Some(slug) = input.slug.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let slug = share::normalize_slug(slug).map_err(|e| bad(e.to_string()))?;
        if db.shares.values().any(|s| s.slug == slug && s.id != share.id) {
            return Err(ApiError(StatusCode::CONFLICT, format!("the link name `{slug}` is already taken")));
        }
        share.slug = slug;
    }
    if let Some(n) = input.display_name {
        let n = n.split_whitespace().collect::<Vec<_>>().join(" ");
        if n.chars().count() > 64 {
            return Err(bad("the name is longer than 64 characters"));
        }
        if !n.is_empty() {
            share.display_name = n;
        }
    }
    if let Some(g) = input.greeting {
        let g = g.trim().to_string();
        if g.chars().count() > 300 {
            return Err(bad("the greeting is longer than 300 characters"));
        }
        share.greeting = g;
    }
    if let Some(pin) = input.pin {
        let pin = pin.trim().to_string();
        share.pin_numeric = !pin.is_empty() && pin.chars().all(|c| c.is_ascii_digit());
        share.pin_hash = if pin.is_empty() {
            None
        } else {
            share::validate_pin(&pin).map_err(|e| bad(e.to_string()))?;
            Some(tokio::task::spawn_blocking(move || share::hash_pin(&pin)).await.map_err(anyhow::Error::from)??)
        };
    }
    if let Some(m) = input.mode
        && m != share.mode
    {
        if !share.sessions.is_empty() && !input.revoke_sessions {
            return Err(bad("changing the mode ends the existing conversations, end them first"));
        }
        share.mode = m;
    }
    if let Some(h) = input.history {
        share.history = h;
    }
    if let Some(n) = input.history_recent {
        share.history_recent = n.clamp(1, 1000);
    }
    if let Some(e) = input.enabled {
        share.enabled = e;
    }
    if let Some(e) = input.expires_at {
        share.expires_at = normalize_expiry(e.as_deref().unwrap_or_default())?;
    }
    if let Some(n) = input.max_sessions {
        share.max_sessions = n.min(100_000);
    }
    if let Some(n) = input.max_replies {
        share.max_replies = n.min(1_000_000);
    }
    if let Some(tg) = input.telegram_chat_id {
        if let Some(id) = tg
            && share.telegram_chat_id != Some(id)
        {
            let bot = st.telegram.read().expect("tg bot").clone().ok_or_else(|| bad("no Telegram bot is set up (Settings → Telegram)"))?;
            if !bot.has_group(id) {
                return Err(bad("the bot is not a member of this Telegram group, add it to the group first"));
            }
        }
        share.telegram_chat_id = tg;
    }
    if let Some(n) = input.ai_delay_secs {
        share.ai_delay_secs = n.min(3600);
    }
    if let Some(b) = input.allow_nudge {
        share.allow_nudge = b;
    }
    if let Some(b) = input.allow_regenerate {
        share.allow_regenerate = b;
    }
    Ok(input.revoke_sessions)
}

pub async fn create_share(State(st): S, Path(chat_id): Path<String>, Json(input): Json<ShareInput>) -> ApiResult {
    let mut db = st.db.write().await;
    let chat = db.chats.get(&chat_id).ok_or_else(|| anyhow!("chat not found"))?;
    if chat.guest.is_some() {
        return Err(bad("a visitor conversation cannot be shared"));
    }
    let mut share = Share {
        id: share::random_id(12),
        chat_id: chat_id.clone(),
        slug: String::new(),
        display_name: chat.persona.clone(),
        greeting: String::new(),
        pin_hash: None,
        pin_numeric: false,
        mode: ShareMode::Private,
        history: HistoryVisibility::None,
        history_recent: 30,
        enabled: true,
        expires_at: None,
        max_sessions: 50,
        max_replies: 200,
        telegram_chat_id: None,
        ai_delay_secs: crate::tgbot::DEFAULT_AI_DELAY_SECS,
        created_at: now_string(),
        sessions: Vec::new(),
        allow_nudge: false,
        allow_regenerate: false,
        room_chat_id: None,
    };
    apply_input(&st, &mut share, input, &db).await?;
    if share.slug.is_empty() {
        share.slug = loop {
            let s = share::random_id(10);
            if !db.shares.values().any(|x| x.slug == s) {
                break s;
            }
        };
    }
    st.paths.save_share(&share)?;
    let v = share_detail(&share, &db);
    db.shares.insert(share.id.clone(), share);
    Ok(Json(v))
}

pub async fn update_share(State(st): S, Path(id): Path<String>, Json(input): Json<ShareInput>) -> ApiResult {
    let mut db = st.db.write().await;
    let mut share = db.shares.get(&id).cloned().ok_or_else(|| anyhow!("share not found"))?;
    if apply_input(&st, &mut share, input, &db).await? {
        revoke_all(&st, &mut db, &mut share)?;
    }
    st.paths.save_share(&share)?;
    let v = share_detail(&share, &db);
    db.shares.insert(id, share);
    Ok(Json(v))
}

/// Deletes all visitor conversations of a share.
fn revoke_all(st: &AppState, db: &mut Db, share: &mut Share) -> anyhow::Result<()> {
    let chats: Vec<String> =
        db.chats.values().filter(|c| c.guest.as_ref().is_some_and(|g| g.share_id == share.id)).map(|c| c.id.clone()).collect();
    for c in chats {
        db.chats.remove(&c);
        st.paths.delete_chat(&c)?;
    }
    share.sessions.clear();
    share.room_chat_id = None;
    Ok(())
}

/// Removes a share with its visitor conversations.
pub fn remove_share(st: &AppState, db: &mut Db, id: &str) -> anyhow::Result<()> {
    if let Some(mut share) = db.shares.remove(id) {
        revoke_all(st, db, &mut share)?;
        st.paths.delete_share(id)?;
    }
    Ok(())
}

pub async fn delete_share(State(st): S, Path(id): Path<String>) -> ApiResult {
    let mut db = st.db.write().await;
    if !db.shares.contains_key(&id) {
        return Err(anyhow!("share not found").into());
    }
    remove_share(&st, &mut db, &id)?;
    Ok(Json(json!({"ok": true})))
}

/// Ends one visitor session. Its conversation is deleted unless other room members still use it.
pub async fn delete_session(State(st): S, Path((id, session)): Path<(String, String)>) -> ApiResult {
    let mut db = st.db.write().await;
    let mut share = db.shares.get(&id).cloned().ok_or_else(|| anyhow!("share not found"))?;
    let pos = share.sessions.iter().position(|s| s.id == session).ok_or_else(|| anyhow!("session not found"))?;
    let removed = share.sessions.remove(pos);
    if !share.sessions.iter().any(|s| s.chat_id == removed.chat_id) {
        db.chats.remove(&removed.chat_id);
        st.paths.delete_chat(&removed.chat_id)?;
        if share.room_chat_id.as_deref() == Some(removed.chat_id.as_str()) {
            share.room_chat_id = None;
        }
    }
    st.paths.save_share(&share)?;
    let v = share_detail(&share, &db);
    db.shares.insert(id, share);
    Ok(Json(v))
}

// ---------------------------------------------------------------- visitor side

async fn active_share(st: &AppState, slug: &str) -> Result<Share, ApiError> {
    let slug = slug.to_lowercase();
    st.db.read().await.shares.values().find(|s| s.slug == slug && s.is_active()).cloned().ok_or_else(|| not_found(GONE))
}

/// Resolves a visitor session. Unknown tokens count as failed attempts per IP.
async fn resolve(st: &AppState, slug: &str, token: &str, ip: &str) -> Result<(Share, GuestSession), ApiError> {
    let key = format!("token:{ip}");
    st.limiter.lock().expect("limiter").check_locked(&key).map_err(too_many)?;
    let share = active_share(st, slug).await?;
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

async fn guest_info(State(st): S, Path(slug): Path<String>) -> ApiResult {
    let share = active_share(&st, &slug).await?;
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

async fn guest_avatar(State(st): S, Path(slug): Path<String>, headers: HeaderMap) -> Result<Response, ApiError> {
    let share = active_share(&st, &slug).await?;
    if share.pin_hash.is_some() {
        return Err(not_found("not found"));
    }
    let (rec, chat) = st.snapshot(&share.chat_id).await.map_err(|_| not_found(GONE))?;
    let path = persona_avatar(&rec.export, &chat.persona).ok_or_else(|| not_found("not found"))?;
    crate::server::serve_media(&st, &rec.id, &path, headers).await
}

#[derive(Deserialize, Default)]
struct StartReq {
    pin: Option<String>,
    name: Option<String>,
}

async fn guest_start(
    State(st): S,
    Path(slug): Path<String>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    body: Option<Json<StartReq>>,
) -> ApiResult {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let share = active_share(&st, &slug).await?;
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
    let chat_id = match (share.mode, share.room_chat_id.clone()) {
        (ShareMode::Room, Some(id)) if db.chats.contains_key(&id) => id,
        _ => {
            let chat = guest_chat_from(&template, &rec.export, &share);
            st.paths.save_chat(&chat)?;
            let id = chat.id.clone();
            db.chats.insert(id.clone(), chat);
            if share.mode == ShareMode::Room {
                share.room_chat_id = Some(id.clone());
            }
            id
        }
    };
    let session = share::new_session(&chat_id, name);
    let token = session.token.clone();
    share.sessions.push(session);
    st.paths.save_share(&share)?;
    db.shares.insert(share.id.clone(), share);
    Ok(Json(json!({"token": token})))
}

/// A fresh visitor conversation on the template's export: no live events, the
/// template's notes, and its memory summary when that only covers the export.
fn guest_chat_from(template: &Chat, export: &ChatExport, share: &Share) -> Chat {
    let now = now_string();
    let mut chat = Chat {
        id: uuid::Uuid::new_v4().simple().to_string()[..12].to_string(),
        export_id: template.export_id.clone(),
        title: share.display_name.clone(),
        persona: template.persona.clone(),
        user: template.user.clone(),
        notes: template.notes.clone(),
        voice: template.voice.clone(),
        created_at: now.clone(),
        updated_at: now,
        events: Vec::new(),
        next_id: export.messages.iter().map(|m| m.id).max().unwrap_or(0) + 1,
        memory: None,
        last_usage: None,
        guest: Some(GuestChat { share_id: share.id.clone(), authors: Default::default() }),
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
    let names: std::collections::HashMap<&str, &str> =
        share.sessions.iter().map(|s| (s.id.as_str(), s.name.as_deref().unwrap_or("Guest"))).collect();
    view_messages(export, chat, &allowed)
        .into_iter()
        .map(|mut v| {
            let id = v["id"].as_i64().unwrap_or_default();
            if v["from"] == json!(chat.persona) {
                v["from"] = json!(share.display_name);
            }
            if let Some(md) = v.get_mut("media").and_then(Value::as_object_mut) {
                md.remove("description");
            }
            if room && id > max_export_id && v["mine"] == json!(true) {
                let author = authors.and_then(|a| a.get(&id));
                let mine = author.is_some_and(|a| *a == session.id);
                v["mine"] = json!(mine);
                if !mine {
                    v["from"] = json!(author.and_then(|a| names.get(a.as_str()).copied()).unwrap_or("Guest"));
                }
            }
            v
        })
        .collect()
}

#[derive(Deserialize)]
struct LimitQuery {
    limit: Option<usize>,
    before: Option<i64>,
}

async fn guest_chat(
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
    let multi = share.mode == ShareMode::Room || rec.export.participants.len() > 2;
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
async fn touch(st: &AppState, share_id: &str, session_id: &str, add_reply: bool) -> Result<(), ApiError> {
    let mut db = st.db.write().await;
    let share = db.shares.get_mut(share_id).ok_or_else(|| not_found(GONE))?;
    if let Some(s) = share.sessions.iter_mut().find(|s| s.id == session_id) {
        s.last_seen = now_string();
        if add_reply {
            s.replies += 1;
        }
    }
    st.paths.save_share(share)?;
    Ok(())
}

async fn guest_message(
    State(st): S,
    Path((slug, token)): Path<(String, String)>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    Json(req): Json<PostMessage>,
) -> ApiResult {
    let (share, session) = resolve(&st, &slug, &token, &ip).await?;
    rate(&st, format!("msg:{}", session.id), 30, Duration::from_secs(60))?;
    let author = (share.mode == ShareMode::Room).then_some(session.id.as_str());
    let id = crate::server::add_message(&st, &session.chat_id, req, GUEST_MAX_CHARS, author).await?;
    touch(&st, &share.id, &session.id, false).await?;
    crate::tgbot::relay_guest_message(&st, &share, &session, id).await;
    let (rec, chat) = st.snapshot(&session.chat_id).await?;
    let mine: Vec<&Message> = guest_visible(&rec.export, &chat, &share).into_iter().filter(|m| m.id == id).collect();
    let view = guest_view(&rec.export, &chat, &share, &session, &mine);
    Ok(Json(json!({"messages": view})))
}

async fn guest_voice(
    State(st): S,
    Path((slug, token)): Path<(String, String)>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    mp: axum::extract::Multipart,
) -> ApiResult {
    let (share, session) = resolve(&st, &slug, &token, &ip).await?;
    // Each voice message costs a transcription call.
    rate(&st, format!("voice:{}", session.id), 10, Duration::from_secs(60))?;
    let author = (share.mode == ShareMode::Room).then_some(session.id.as_str());
    let id = crate::server::add_voice(&st, &session.chat_id, mp, 5 * 1024 * 1024, author).await?;
    touch(&st, &share.id, &session.id, false).await?;
    crate::tgbot::relay_guest_message(&st, &share, &session, id).await;
    let (rec, chat) = st.snapshot(&session.chat_id).await?;
    let mine: Vec<&Message> = guest_visible(&rec.export, &chat, &share).into_iter().filter(|m| m.id == id).collect();
    let view = guest_view(&rec.export, &chat, &share, &session, &mine);
    Ok(Json(json!({"messages": view})))
}

async fn guest_react(
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
    crate::server::add_reaction(&st, &session.chat_id, req).await?;
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
    // An answer needs something to answer: a visitor's message after the persona's last one.
    // Without one this is "write first", which the owner may not allow.
    if !regenerate && !share.allow_nudge {
        let (_, chat) = st.snapshot(&session.chat_id).await.map_err(|_| not_found(GONE))?;
        let last = chat.events.iter().rev().find_map(|e| match e {
            Event::Message(m) if !m.service => Some(m.from == chat.persona),
            _ => None,
        });
        if last.unwrap_or(true) {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                format!("{} answers your messages here: write something first.", share.display_name),
            ));
        }
    }
    rate(&st, format!("turn:{}", session.id), 10, Duration::from_secs(60))?;
    rate(&st, format!("turn-link:{}", share.id), 120, Duration::from_secs(60))?;
    touch(&st, &share.id, &session.id, true).await?;
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

async fn guest_respond(
    State(st): S,
    Path((slug, token)): Path<(String, String)>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
) -> ApiResult {
    guest_turn(st, slug, token, ip, false).await
}

async fn guest_regenerate(
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

async fn guest_media(
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
