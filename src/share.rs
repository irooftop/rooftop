//! Public share links: `/s/<slug>` lets other people talk to a persona without
//! access to the owner's UI, the export, the notes or the LLM prompt.
//!
//! * A [`Share`] points at a *template* chat (export + persona + user + notes).
//! * Every visitor gets an own [`GuestSession`] backed by a separate chat that
//!   starts at the end of the export (`private` mode), or all visitors write
//!   into one chat (`room` mode).
//! * An optional PIN is stored as an Argon2id hash, and failed attempts are
//!   throttled per client IP and per link ([`Limiter`]).

use crate::state::now_string;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShareMode {
    /// Each visitor gets an own conversation.
    #[default]
    Private,
    /// All visitors share one conversation and see each other's messages.
    Room,
}

/// How much of the exported history visitors can read.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HistoryVisibility {
    /// Only the new conversation (the persona still knows the whole export).
    #[default]
    None,
    /// The last `history_recent` messages of the export.
    Recent,
    /// The whole export.
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestSession {
    /// Public id (shown to the owner, stored as message author in rooms).
    pub id: String,
    /// Secret bearer token, part of the visitor's URL `/s/<slug>/<token>`.
    pub token: String,
    /// Chat holding this conversation (the same for all sessions in a room).
    pub chat_id: String,
    /// Optional name the visitor typed in (shown to the owner and in rooms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub last_seen: String,
    /// Persona answers requested by this session.
    #[serde(default)]
    pub replies: u32,
    /// The address the visitor last came from (shown to the owner, used to block them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
}

/// An address the owner blocked from a link.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Blocked {
    pub ip: String,
    /// The visitor's name when they were blocked, to recognise the entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Share {
    pub id: String,
    /// Template chat.
    pub chat_id: String,
    /// URL part: `/s/<slug>`.
    pub slug: String,
    /// Name of the persona shown to visitors.
    pub display_name: String,
    /// Optional line on the landing page.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub greeting: String,
    /// Argon2id PHC-like string `argon2id$<salt hex>$<hash hex>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_hash: Option<String>,
    /// The PIN is digits only (visitors get a numeric keypad).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pin_numeric: bool,
    #[serde(default)]
    pub mode: ShareMode,
    #[serde(default)]
    pub history: HistoryVisibility,
    #[serde(default = "default_recent")]
    pub history_recent: usize,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Local time `YYYY-MM-DDTHH:MM:SS`, after which the link stops working afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// 0 = unlimited.
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,
    /// Persona answers per session (per room in room mode), 0 = unlimited.
    #[serde(default = "default_max_replies")]
    pub max_replies: u32,
    pub created_at: String,
    #[serde(default)]
    pub sessions: Vec<GuestSession>,
    /// Telegram group that receives visitor messages with **Reply** / **AI reply**
    /// buttons (see [`crate::tgbot`]). `None`: the AI answers directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telegram_chat_id: Option<i64>,
    /// With a Telegram group: seconds before the AI answers on its own, 0 = only on request.
    #[serde(default = "default_ai_delay")]
    pub ai_delay_secs: u32,
    /// Addresses that may not open this link.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked: Vec<Blocked>,
    /// Visitors may ask the persona to write first, without a message of their own.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_nudge: bool,
    /// Visitors may have the persona's last answer written again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_regenerate: bool,
    /// The shared conversation in room mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_chat_id: Option<String>,
}

fn yes() -> bool {
    true
}
fn default_recent() -> usize {
    30
}
fn default_max_sessions() -> usize {
    50
}
fn default_ai_delay() -> u32 {
    crate::tgbot::DEFAULT_AI_DELAY_SECS
}
fn default_max_replies() -> u32 {
    200
}

impl Share {
    pub fn is_active(&self) -> bool {
        self.enabled && self.expires_at.as_deref().is_none_or(|e| now_string().as_str() < e)
    }
    pub fn session_by_token(&self, token: &str) -> Option<&GuestSession> {
        // Tokens are random, but compare in constant time anyway.
        self.sessions.iter().find(|s| ct_eq(s.token.as_bytes(), token.as_bytes()))
    }
}

pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

const RESERVED: &[&str] = &["api", "s", "media", "new", "admin", "app", "static", "healthz"];

/// Normalizes and validates a custom slug: 3–40 of `a-z 0-9 - _`.
pub fn normalize_slug(s: &str) -> Result<String> {
    let s = s.trim().to_lowercase();
    if !(3..=40).contains(&s.chars().count()) {
        bail!("the link name must be 3–40 characters");
    }
    if !s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        bail!("the link name may contain only latin letters, digits, `-` and `_`");
    }
    if s.starts_with(['-', '_']) || s.ends_with(['-', '_']) {
        bail!("the link name must start and end with a letter or digit");
    }
    if RESERVED.contains(&s.as_str()) {
        bail!("`{s}` is reserved");
    }
    Ok(s)
}

/// Random lowercase alphanumeric string (≈ 5.2 bits per char), from the OS CSPRNG via uuid v4.
pub fn random_id(len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let mut out = String::with_capacity(len);
    while out.len() < len {
        for b in uuid::Uuid::new_v4().as_bytes() {
            if out.len() == len {
                break;
            }
            out.push(ALPHABET[(*b as usize) % ALPHABET.len()] as char);
        }
    }
    out
}

/// Validates a PIN / passphrase chosen by the owner.
pub fn validate_pin(pin: &str) -> Result<()> {
    let n = pin.chars().count();
    if !(4..=64).contains(&n) {
        bail!("the PIN must be 4–64 characters");
    }
    Ok(())
}

pub fn hash_pin(pin: &str) -> Result<String> {
    let salt = uuid::Uuid::new_v4();
    let mut out = [0u8; 32];
    argon2::Argon2::default()
        .hash_password_into(pin.as_bytes(), salt.as_bytes(), &mut out)
        .map_err(|e| anyhow::anyhow!("hashing PIN: {e}"))?;
    Ok(format!("argon2id${}${}", hex::encode(salt.as_bytes()), hex::encode(out)))
}

pub fn verify_pin(stored: &str, pin: &str) -> bool {
    let mut parts = stored.split('$');
    let (Some("argon2id"), Some(salt), Some(hash), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let (Ok(salt), Ok(hash)) = (hex::decode(salt), hex::decode(hash)) else { return false };
    let mut out = vec![0u8; hash.len()];
    argon2::Argon2::default().hash_password_into(pin.as_bytes(), &salt, &mut out).is_ok() && ct_eq(&out, &hash)
}

/// What the persona calls a visitor who gave no name.
pub const GUEST_NAME: &str = "Guest";
/// The `user` of a room conversation (its visitors write under their own names).
pub const ROOM_USER: &str = "Visitors";

impl Share {
    /// The name a visitor goes by in the conversation: what they typed in, else "Guest"
    /// (in a room "Guest 3", so several unnamed visitors stay apart).
    pub fn visitor_name(&self, session: &GuestSession) -> String {
        match &session.name {
            Some(n) => n.clone(),
            None if self.mode == ShareMode::Room => {
                let n = self.sessions.iter().position(|s| s.id == session.id).unwrap_or(self.sessions.len()) + 1;
                format!("{GUEST_NAME} {n}")
            }
            None => GUEST_NAME.to_string(),
        }
    }
}

/// `name`, changed where it would be mistaken for someone else in the conversation: the
/// persona's or the owner's name gets "(guest)" ("Alex (guest)"), another visitor's in a
/// room a number ("Ann 2").
pub fn distinct_name(name: String, people: &[&str], visitors: &[&str]) -> String {
    let clash = |n: &str| people.iter().chain(visitors).any(|t| t.eq_ignore_ascii_case(n));
    let base = if people.iter().any(|t| t.eq_ignore_ascii_case(&name)) { format!("{name} (guest)") } else { name };
    if !clash(&base) {
        return base;
    }
    (2..).map(|i| format!("{base} {i}")).find(|n| !clash(n)).expect("a free name")
}

impl Share {
    pub fn is_blocked(&self, ip: &str) -> bool {
        self.blocked.iter().any(|b| b.ip == ip)
    }
}

/// `ip` when it is a real address (the server may not know it: "unknown"), so that one
/// block never covers every visitor whose address is missing.
pub fn known_ip(ip: &str) -> Option<String> {
    ip.parse::<std::net::IpAddr>().ok().map(|a| a.to_string())
}

pub fn new_session(chat_id: &str, name: Option<String>, ip: &str) -> GuestSession {
    let now = now_string();
    GuestSession {
        id: random_id(8),
        token: random_id(26),
        chat_id: chat_id.to_string(),
        name,
        created_at: now.clone(),
        last_seen: now,
        replies: 0,
        ip: known_ip(ip),
    }
}

/// In-memory throttling: failed PIN attempts with escalating lockouts, plus
/// fixed-window rate limits for guest actions.
#[derive(Default)]
pub struct Limiter {
    fails: HashMap<String, Fails>,
    windows: HashMap<String, (Instant, u32)>,
}

struct Fails {
    count: u32,
    since: Instant,
    locked_until: Option<Instant>,
    /// Number of lockouts so far, each one doubles the next.
    strikes: u32,
}

/// Per client IP and link: 5 wrong PINs → 1 min, doubling up to 1 h.
pub const IP_FAILS: (u32, Duration, Duration) = (5, Duration::from_secs(60), Duration::from_secs(3600));
/// Per link, all clients together (distributed guessing): 20 wrong PINs within
/// 15 min → 5 min, doubling up to 24 h.
pub const LINK_FAILS: (u32, Duration, Duration) = (20, Duration::from_secs(300), Duration::from_secs(86_400));
const FAIL_WINDOW: Duration = Duration::from_secs(900);

impl Limiter {
    fn gc(&mut self, now: Instant) {
        if self.fails.len() > 10_000 {
            self.fails.retain(|_, f| f.locked_until.is_some_and(|l| l > now) || now.duration_since(f.since) < FAIL_WINDOW || f.strikes > 0);
        }
        if self.windows.len() > 10_000 {
            self.windows.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(3600));
        }
    }

    /// `Err(retry_after)` while `key` is locked out.
    pub fn check_locked(&self, key: &str) -> Result<(), Duration> {
        let now = Instant::now();
        match self.fails.get(key).and_then(|f| f.locked_until) {
            Some(until) if until > now => Err(until - now),
            _ => Ok(()),
        }
    }

    /// Records a failed attempt and returns the lockout if this one triggered it.
    pub fn fail(&mut self, key: &str, (max, base, cap): (u32, Duration, Duration)) -> Option<Duration> {
        let now = Instant::now();
        self.gc(now);
        let f = self.fails.entry(key.to_string()).or_insert(Fails { count: 0, since: now, locked_until: None, strikes: 0 });
        if now.duration_since(f.since) > FAIL_WINDOW {
            f.count = 0;
            f.since = now;
        }
        f.count += 1;
        if f.count >= max {
            let d = base.saturating_mul(1u32 << f.strikes.min(16)).min(cap);
            f.strikes += 1;
            f.count = 0;
            f.since = now;
            f.locked_until = Some(now + d);
            return Some(d);
        }
        None
    }

    pub fn success(&mut self, key: &str) {
        self.fails.remove(key);
    }

    /// Fixed window rate limit: at most `max` hits per `window`.
    pub fn hit(&mut self, key: &str, max: u32, window: Duration) -> Result<(), Duration> {
        let now = Instant::now();
        self.gc(now);
        let e = self.windows.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(e.0) >= window {
            *e = (now, 0);
        }
        if e.1 >= max {
            return Err(window.saturating_sub(now.duration_since(e.0)));
        }
        e.1 += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_roundtrip() {
        let h = hash_pin("4821").unwrap();
        assert!(verify_pin(&h, "4821"));
        assert!(!verify_pin(&h, "4822"));
        assert!(!verify_pin("garbage", "4821"));
        assert_ne!(hash_pin("4821").unwrap(), h, "salted");
    }

    #[test]
    fn slugs() {
        assert_eq!(normalize_slug(" Jonas-Weber ").unwrap(), "jonas-weber");
        assert!(normalize_slug("ab").is_err());
        assert!(normalize_slug("api").is_err());
        assert!(normalize_slug("a/b/c").is_err());
        assert!(normalize_slug("-abc").is_err());
        assert!(normalize_slug("привет").is_err());
    }

    #[test]
    fn lockout_escalates() {
        let mut l = Limiter::default();
        let cfg = (3, Duration::from_secs(60), Duration::from_secs(3600));
        assert!(l.fail("k", cfg).is_none());
        assert!(l.fail("k", cfg).is_none());
        assert_eq!(l.fail("k", cfg), Some(Duration::from_secs(60)));
        assert!(l.check_locked("k").is_err());
        assert!(l.check_locked("other").is_ok());
        for _ in 0..2 {
            l.fail("k", cfg);
        }
        assert_eq!(l.fail("k", cfg), Some(Duration::from_secs(120)));
        l.success("k");
        assert!(l.check_locked("k").is_ok());
    }

    #[test]
    fn rate_window() {
        let mut l = Limiter::default();
        for _ in 0..3 {
            assert!(l.hit("x", 3, Duration::from_secs(60)).is_ok());
        }
        assert!(l.hit("x", 3, Duration::from_secs(60)).is_err());
    }

    #[test]
    fn random_ids() {
        let a = random_id(26);
        assert_eq!(a.len(), 26);
        assert_ne!(a, random_id(26));
    }
}
