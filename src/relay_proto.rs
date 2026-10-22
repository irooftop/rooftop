//! Wire format between a relay hub (e.g. iroof.top) and an iRoof.top app that
//! publishes its share links at `https://<name>.<domain>`.
//!
//! The app keeps one outgoing WebSocket to the hub. Visitor requests travel
//! over it and the app answers them from its guest-only router, so nothing on
//! the app's computer listens on the internet and the owner UI never leaves it.
//!
//! Every WebSocket message is binary: a 4-byte big-endian header length, the
//! JSON [`Frame`] header, then the raw payload (request body or response chunk).

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// Hub endpoint for the app's WebSocket (`Authorization: Bearer <secret>`).
pub const CONNECT_PATH: &str = "/relay/v1/connect";
/// Hub endpoint that claims (or renames) the account's subdomain.
pub const REGISTER_PATH: &str = "/relay/v1/register";
/// Hub info (domain, shared bot): `GET`, public.
pub const INFO_PATH: &str = "/relay/v1/info";
/// Bot API proxy of the shared bot: `<TG_PATH>/bot<secret>/<method>`.
pub const TG_PATH: &str = "/relay/v1/tg";

/// Largest visitor request body the hub forwards (voice messages are ≤ 5 MiB).
pub const MAX_REQUEST_BODY: usize = 8 * 1024 * 1024;
/// Response bodies are sent in chunks of at most this size.
pub const CHUNK: usize = 64 * 1024;
/// Largest WebSocket message either side accepts.
pub const MAX_MESSAGE: usize = MAX_REQUEST_BODY + 64 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Frame {
    /// Hub → app, once after connecting: where the links are published.
    Hello { name: String, url: String },
    /// Hub → app: a visitor request, the payload is its whole body.
    Request { id: u64, method: String, uri: String, headers: Vec<(String, String)> },
    /// Hub → app: the visitor went away, stop working on `id`.
    Cancel { id: u64 },
    /// App → hub: status and headers of the response to `id`.
    Head { id: u64, status: u16, headers: Vec<(String, String)> },
    /// App → hub: a chunk of the response body (the payload).
    Data { id: u64 },
    /// App → hub: the response body is complete.
    End { id: u64 },
    /// App → hub: the request failed before a response head was sent.
    Fail { id: u64, message: String },
}

pub fn encode(frame: &Frame, payload: &[u8]) -> Vec<u8> {
    let header = serde_json::to_vec(frame).expect("frame serializes");
    let mut out = Vec::with_capacity(4 + header.len() + payload.len());
    out.extend_from_slice(&(header.len() as u32).to_be_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(payload);
    out
}

pub fn decode(msg: &[u8]) -> Result<(Frame, &[u8])> {
    if msg.len() < 4 {
        bail!("relay frame too short");
    }
    let len = u32::from_be_bytes([msg[0], msg[1], msg[2], msg[3]]) as usize;
    if len > 64 * 1024 || msg.len() < 4 + len {
        bail!("relay frame header length is invalid");
    }
    let frame = serde_json::from_slice(&msg[4..4 + len])?;
    Ok((frame, &msg[4 + len..]))
}

/// Hop-by-hop and credential headers that never cross the relay.
pub fn is_dropped_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "authorization"
            | "cookie"
            | "set-cookie"
            | "content-length"
    )
}

/// Headers as text pairs, without dropped ones and values that aren't text.
pub fn header_pairs(h: &axum::http::HeaderMap) -> Vec<(String, String)> {
    h.iter()
        .filter(|(k, _)| !is_dropped_header(k.as_str()))
        .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), v.to_string())))
        .collect()
}

/// Subdomain names an app may claim: 3–32 of `a-z 0-9 -`, starting and ending
/// with a letter or digit, no `--` (keeps punycode `xn--` out), not reserved.
pub fn validate_name(name: &str) -> Result<()> {
    const RESERVED: &[&str] = &[
        "www",
        "api",
        "app",
        "admin",
        "root",
        "mail",
        "smtp",
        "imap",
        "pop",
        "ftp",
        "ns",
        "ns1",
        "ns2",
        "dns",
        "mx",
        "relay",
        "static",
        "assets",
        "cdn",
        "status",
        "docs",
        "doc",
        "blog",
        "help",
        "support",
        "dev",
        "test",
        "staging",
        "prod",
        "beta",
        "iroof",
        "rooftop",
        "telegram",
        "bot",
        "login",
        "account",
        "accounts",
        "billing",
        "security",
        "abuse",
        "postmaster",
        "hostmaster",
        "webmaster",
        "localhost",
        "share",
        "s",
        "download",
        "downloads",
        "update",
        "updates",
        "aggrega",
        "reseller",
    ];
    let ok_chars = name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    let edges = name.bytes().next().is_some_and(|b| b != b'-') && name.bytes().last().is_some_and(|b| b != b'-');
    if !(3..=32).contains(&name.len()) || !ok_chars || !edges || name.contains("--") {
        bail!("Use 3–32 lowercase letters, digits or single hyphens, starting and ending with a letter or digit.");
    }
    if RESERVED.contains(&name) {
        bail!("This name is reserved, pick another one.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_with_payload() {
        let f = Frame::Request { id: 7, method: "POST".into(), uri: "/api/s/x".into(), headers: vec![("a".into(), "b".into())] };
        let bytes = encode(&f, b"hello");
        let (back, payload) = decode(&bytes).unwrap();
        assert_eq!(back, f);
        assert_eq!(payload, b"hello");
        assert!(decode(&[0, 0, 0, 9, b'{']).is_err());
        assert!(decode(&[1]).is_err());
    }

    #[test]
    fn names_are_validated() {
        for ok in ["alice", "a-b-c", "x99", "roof-3k2j9a"] {
            validate_name(ok).unwrap();
        }
        for bad in ["ab", "-ab", "ab-", "a--b", "xn--abc", "Alice", "a_b", "www", "admin", &"a".repeat(33)] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn credentials_and_hop_headers_are_dropped() {
        assert!(is_dropped_header("Cookie"));
        assert!(is_dropped_header("set-cookie"));
        assert!(is_dropped_header("Authorization"));
        assert!(!is_dropped_header("content-type"));
    }
}
