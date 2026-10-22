//! Where share links can be opened, for an app whose owner UI is only on this
//! device (the desktop app, or the server bound to localhost):
//!
//! * **Local network**: a second listener on all interfaces that serves only
//!   the visitor routes, so phones on the same Wi-Fi can open the links. The
//!   owner UI stays on localhost.
//! * **Internet**: the relay client ([`crate::relay_client`]) publishes the links
//!   at `https://<name>.iroof.top` while the app runs.
//!
//! Also the owner's QR code endpoint, so a phone can open a link by scanning it.

use crate::relay_client::{self, ClientParams, RelayStatus};
use crate::server::{ApiError, ApiResult, bad};
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// Ports tried for the local network listener, so links and QR codes stay the same across restarts.
const LAN_PORTS: std::ops::RangeInclusive<u16> = 8737..=8746;

type S = State<Arc<AppState>>;

struct Lan {
    port: u16,
    task: tokio::task::JoinHandle<()>,
}

pub struct Sharing {
    /// Loopback address of the owner UI.
    local: SocketAddr,
    lan: Mutex<Option<Lan>>,
    lan_error: Mutex<Option<String>>,
    relay: Mutex<Option<CancellationToken>>,
    relay_status: Arc<Mutex<RelayStatus>>,
    http: reqwest::Client,
}

/// The IPv4 address other devices on the local network can open: Wi-Fi /
/// Ethernet before VPN tunnels and virtual bridges, private ranges first.
pub fn lan_ip() -> Option<std::net::Ipv4Addr> {
    const VIRTUAL: &[&str] = &[
        "utun",
        "tun",
        "tap",
        "wg",
        "ppp",
        "ipsec",
        "tailscale",
        "zt",
        "docker",
        "br-",
        "veth",
        "vmnet",
        "vboxnet",
        "virbr",
        "bridge",
        "awdl",
        "llw",
        "anpi",
        "gif",
        "stf",
        "lxc",
        "cni",
        "flannel",
    ];
    if_addrs::get_if_addrs()
        .ok()?
        .into_iter()
        .filter(|i| !i.is_loopback() && !i.is_link_local() && i.oper_status != if_addrs::IfOperStatus::Down)
        .filter_map(|i| {
            let std::net::IpAddr::V4(ip) = i.ip() else { return None };
            let name = i.name.to_lowercase();
            let virtual_if = i.is_p2p() || VIRTUAL.iter().any(|p| name.starts_with(p)) || name.contains("virtual") || name.contains("vpn");
            Some((u8::from(virtual_if) * 2 + u8::from(!ip.is_private()), ip))
        })
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, ip)| ip)
}

fn sharing(st: &AppState) -> Option<Arc<Sharing>> {
    st.sharing.get().cloned()
}

/// Enables local network / internet sharing for an app served on `local`
/// (a loopback address) and applies the saved settings.
pub async fn init(st: &Arc<AppState>, local: SocketAddr) {
    let s = Arc::new(Sharing {
        local,
        lan: Default::default(),
        lan_error: Default::default(),
        relay: Default::default(),
        relay_status: Arc::new(Mutex::new(RelayStatus { state: "off", ..Default::default() })),
        http: reqwest::Client::new(),
    });
    if st.sharing.set(s).is_err() {
        return;
    }
    let _g = st.control.lock().await;
    apply(st).await;
}

/// Starts or stops the listener and the relay client to match the settings.
async fn apply(st: &Arc<AppState>) {
    let Some(s) = sharing(st) else { return };
    let cfg = st.db.read().await.settings.sharing.clone();
    // Local network.
    let running = s.lan.lock().expect("lan").is_some();
    if cfg.lan && !running {
        match start_lan(st, cfg.lan_port).await {
            Ok(lan) => {
                if lan.port != cfg.lan_port {
                    let mut db = st.db.write().await;
                    db.settings.sharing.lan_port = lan.port;
                    if let Err(e) = st.paths.save_settings(&db.settings) {
                        tracing::warn!("saving settings: {e:#}");
                    }
                }
                tracing::info!("share links on the local network: port {}", lan.port);
                *s.lan_error.lock().expect("lan") = None;
                *s.lan.lock().expect("lan") = Some(lan);
            }
            Err(e) => *s.lan_error.lock().expect("lan") = Some(format!("{e:#}")),
        }
    } else if !cfg.lan
        && let Some(lan) = s.lan.lock().expect("lan").take()
    {
        lan.task.abort();
    }
    // Internet.
    let relay_running = s.relay.lock().expect("relay").is_some();
    if cfg.internet && !relay_running {
        match ensure_account(st, false).await {
            Ok((base, secret, name)) => {
                let cancel = CancellationToken::new();
                {
                    let mut status = s.relay_status.lock().expect("relay status");
                    *status = RelayStatus { state: "connecting", error: None, url: status.url.clone() };
                }
                let params = ClientParams {
                    base,
                    secret,
                    name,
                    guest: crate::server::guest_router(st.clone(), true),
                    status: s.relay_status.clone(),
                    cancel: cancel.clone(),
                };
                tokio::spawn(relay_client::run(params));
                *s.relay.lock().expect("relay") = Some(cancel);
            }
            Err(e) => *s.relay_status.lock().expect("relay status") = RelayStatus { state: "error", error: Some(e.message), url: None },
        }
    } else if !cfg.internet {
        if let Some(c) = s.relay.lock().expect("relay").take() {
            c.cancel();
        }
        let mut status = s.relay_status.lock().expect("relay status");
        status.state = "off";
        status.error = None;
    }
}

async fn start_lan(st: &Arc<AppState>, saved: u16) -> anyhow::Result<Lan> {
    let mut candidates: Vec<u16> = Vec::new();
    if saved != 0 {
        candidates.push(saved);
    }
    candidates.extend(LAN_PORTS.filter(|p| *p != saved));
    candidates.push(0);
    let mut last = None;
    for port in candidates {
        match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
            Ok(listener) => {
                let port = listener.local_addr()?.port();
                let app = crate::server::guest_router(st.clone(), false);
                let task = tokio::spawn(async move {
                    if let Err(e) = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await {
                        tracing::warn!("local network listener: {e}");
                    }
                });
                return Ok(Lan { port, task });
            }
            Err(e) => last = Some(e),
        }
    }
    Err(anyhow::anyhow!("can't open a port for the local network: {}", last.map(|e| e.to_string()).unwrap_or_default()))
}

/// The relay account (base URL, secret, name), created and registered on first use.
/// `force` registers again (the hub may have lost the account).
pub async fn ensure_account(st: &AppState, force: bool) -> Result<(String, String, String), relay_client::RegisterError> {
    let cfg = st.db.read().await.settings.sharing.clone();
    let base = relay_client::relay_base(&cfg.relay_url);
    if !force && !cfg.secret.is_empty() && !cfg.name.is_empty() {
        return Ok((base, cfg.secret, cfg.name));
    }
    let secret = if cfg.secret.is_empty() { crate::relay_tg::rand_like::token(48) } else { cfg.secret.clone() };
    let http = reqwest::Client::new();
    let mut name = cfg.name.clone();
    let mut tries = 0;
    loop {
        let candidate = if name.is_empty() { format!("roof-{}", crate::share::random_id(6)) } else { name.clone() };
        match relay_client::register(&http, &base, &secret, &candidate).await {
            Ok(_) => {
                name = candidate;
                break;
            }
            // A generated name that happens to be taken: try another one.
            Err(e) if e.name_problem && cfg.name.is_empty() && tries < 5 => tries += 1,
            Err(e) => return Err(e),
        }
    }
    let mut db = st.db.write().await;
    db.settings.sharing.secret = secret.clone();
    db.settings.sharing.name = name.clone();
    st.paths.save_settings(&db.settings).map_err(|e| relay_client::RegisterError { name_problem: false, message: format!("{e:#}") })?;
    Ok((base, secret, name))
}

pub fn owner_routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/sharing", get(get_sharing).put(put_sharing)).route("/api/qr", get(qr))
}

async fn status_json(st: &AppState) -> Value {
    let cfg = st.db.read().await.settings.sharing.clone();
    let base = relay_client::relay_base(&cfg.relay_url);
    let Some(s) = sharing(st) else {
        return json!({"available": false, "relay_host": relay_client::relay_host(&base)});
    };
    let lan_port = s.lan.lock().expect("lan").as_ref().map(|l| l.port);
    let lan_ip = lan_ip();
    let lan_url = lan_port.zip(lan_ip).map(|(p, ip)| format!("http://{ip}:{p}"));
    let lan_error = s.lan_error.lock().expect("lan").clone().or_else(|| {
        (lan_port.is_some() && lan_ip.is_none())
            .then(|| "This computer isn't connected to a local network (Wi-Fi or Ethernet).".to_string())
    });
    let relay = s.relay_status.lock().expect("relay status").clone();
    let host = relay_client::relay_host(&base);
    let url = relay.url.clone().or_else(|| (!cfg.name.is_empty()).then(|| default_public_url(&base, &cfg.name)));
    // The links' domain (`iroof.top`): the public URL without the name.
    let domain = url.as_deref().map(relay_client::relay_host).and_then(|h| h.split_once('.').map(|(_, d)| d.to_string()));
    json!({
        "available": true,
        "local": format!("http://{}", s.local),
        "lan": {"enabled": cfg.lan, "url": lan_url, "error": lan_error},
        "internet": {
            "enabled": cfg.internet,
            "name": cfg.name,
            "url": url,
            "state": relay.state,
            "error": relay.error,
            "domain": domain.unwrap_or_else(|| host.clone()),
        },
        "relay_host": host,
    })
}

/// `https://<name>.<hub host>` before the hub said otherwise.
fn default_public_url(base: &str, name: &str) -> String {
    let host = relay_client::relay_host(base);
    let scheme = if base.starts_with("http://") { "http" } else { "https" };
    format!("{scheme}://{name}.{host}")
}

async fn get_sharing(State(st): S) -> ApiResult {
    Ok(Json(status_json(&st).await))
}

#[derive(Deserialize)]
struct SharingPatch {
    lan: Option<bool>,
    internet: Option<bool>,
    name: Option<String>,
}

async fn put_sharing(State(st): S, Json(p): Json<SharingPatch>) -> ApiResult {
    if sharing(&st).is_none() {
        return Err(bad("Sharing to the local network or the internet is only available when iRoof.top runs on this computer."));
    }
    let _g = st.control.lock().await;
    if let Some(name) = p.name.map(|n| n.trim().to_ascii_lowercase()) {
        crate::relay_proto::validate_name(&name).map_err(|e| bad(e.to_string()))?;
        let cfg = st.db.read().await.settings.sharing.clone();
        if name != cfg.name {
            let base = relay_client::relay_base(&cfg.relay_url);
            let secret = if cfg.secret.is_empty() { crate::relay_tg::rand_like::token(48) } else { cfg.secret.clone() };
            let s = sharing(&st).expect("checked");
            let url = relay_client::register(&s.http, &base, &secret, &name)
                .await
                .map_err(|e| ApiError(if e.name_problem { StatusCode::CONFLICT } else { StatusCode::BAD_GATEWAY }, e.message))?;
            let mut db = st.db.write().await;
            db.settings.sharing.name = name;
            db.settings.sharing.secret = secret;
            st.paths.save_settings(&db.settings)?;
            drop(db);
            s.relay_status.lock().expect("relay status").url = Some(url);
            // A running client reconnects with the new name (it re-registers by name if the hub forgets it).
            if let Some(c) = s.relay.lock().expect("relay").take() {
                c.cancel();
            }
        }
    }
    {
        let mut db = st.db.write().await;
        if let Some(v) = p.lan {
            db.settings.sharing.lan = v;
        }
        if let Some(v) = p.internet {
            db.settings.sharing.internet = v;
        }
        st.paths.save_settings(&db.settings)?;
    }
    apply(&st).await;
    // Give a fresh relay connection a moment, so the answer usually shows it connected.
    if p.internet == Some(true) {
        for _ in 0..30 {
            let state = sharing(&st).map(|s| s.relay_status.lock().expect("relay status").state);
            if state != Some("connecting") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
    Ok(Json(status_json(&st).await))
}

#[derive(Deserialize)]
struct QrQuery {
    text: String,
}

/// `GET /api/qr?text=…`: the text as a QR code (SVG).
async fn qr(Query(q): Query<QrQuery>) -> Result<Response, ApiError> {
    if q.text.is_empty() || q.text.len() > 1000 {
        return Err(bad("text must be 1–1000 bytes"));
    }
    Ok(([(header::CONTENT_TYPE, "image/svg+xml"), (header::CACHE_CONTROL, "private, max-age=3600")], qr_svg(&q.text)?).into_response())
}

pub fn qr_svg(text: &str) -> Result<String, ApiError> {
    use qrcode::render::svg;
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M).map_err(|e| bad(e.to_string()))?;
    Ok(code
        .render::<svg::Color>()
        .min_dimensions(256, 256)
        .quiet_zone(true)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_codes_render_as_svg() {
        let svg = qr_svg("http://192.168.1.20:8737/s/joe").map_err(|e| e.1).unwrap();
        assert!(svg.starts_with("<?xml") || svg.starts_with("<svg"));
        assert!(svg.contains("<svg"));
    }

    #[test]
    fn public_urls() {
        assert_eq!(default_public_url("https://iroof.top", "alice"), "https://alice.iroof.top");
        assert_eq!(default_public_url("http://127.0.0.1:9", "bob"), "http://bob.127.0.0.1:9");
    }
}
