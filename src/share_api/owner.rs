//! The owner's side of share links: creating, editing and deleting them, and the
//! visitor conversations and blocked addresses of each.

use super::S;
use crate::chat::Event;
use crate::server::{ApiError, ApiResult, bad};
use crate::share::{self, GuestSession, HistoryVisibility, Share, ShareMode};
use crate::state::{AppState, now_string};
use crate::store::Db;
use anyhow::anyhow;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

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
        "blocked": s.blocked,
        "created_at": s.created_at,
        "sessions": s.sessions.iter().map(|g| json!({
            "id": g.id, "name": g.name, "chat_id": g.chat_id,
            "created_at": g.created_at, "last_seen": g.last_seen, "replies": g.replies, "ip": g.ip,
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
    /// Addresses to take off the block list.
    #[serde(default)]
    unblock: Vec<String>,
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
    share.blocked.retain(|b| !input.unblock.contains(&b.ip));
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
        blocked: Vec::new(),
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
#[derive(Deserialize, Default)]
pub struct EndSession {
    /// Also keep the visitor's address from opening the link again.
    #[serde(default)]
    block: bool,
}

/// `DELETE /api/shares/{id}/sessions/{session}[?block=true]`: ends a visitor conversation
/// (deleting it), and optionally blocks the address it came from.
pub async fn delete_session(State(st): S, Path((id, session)): Path<(String, String)>, Query(q): Query<EndSession>) -> ApiResult {
    let mut db = st.db.write().await;
    let mut share = db.shares.get(&id).cloned().ok_or_else(|| anyhow!("share not found"))?;
    let pos = share.sessions.iter().position(|s| s.id == session).ok_or_else(|| anyhow!("session not found"))?;
    let removed = share.sessions.remove(pos);
    if q.block {
        let ip =
            removed.ip.clone().ok_or_else(|| bad("this visitor's address isn't known (they came before 0.5.7), so it can't be blocked"))?;
        if !share.is_blocked(&ip) {
            share.blocked.push(share::Blocked { ip: ip.clone(), name: removed.name.clone(), at: now_string() });
        }
        // Everyone else from that address goes too: a block that leaves them chatting isn't one.
        let gone: Vec<GuestSession> = share.sessions.iter().filter(|s| s.ip.as_deref() == Some(ip.as_str())).cloned().collect();
        share.sessions.retain(|s| s.ip.as_deref() != Some(ip.as_str()));
        for g in gone {
            drop_session_chat(&st, &mut db, &mut share, &g)?;
        }
    }
    drop_session_chat(&st, &mut db, &mut share, &removed)?;
    st.paths.save_share(&share)?;
    let v = share_detail(&share, &db);
    db.shares.insert(id, share);
    Ok(Json(v))
}

/// Deletes the conversation of a removed session, unless others still use it (a room).
fn drop_session_chat(st: &AppState, db: &mut Db, share: &mut Share, removed: &GuestSession) -> Result<(), ApiError> {
    if !share.sessions.iter().any(|s| s.chat_id == removed.chat_id) {
        db.chats.remove(&removed.chat_id);
        st.paths.delete_chat(&removed.chat_id)?;
        if share.room_chat_id.as_deref() == Some(removed.chat_id.as_str()) {
            share.room_chat_id = None;
        }
    }
    Ok(())
}
