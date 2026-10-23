//! App state for the UI, settings, the model connection test and MCP servers.

use super::chats::chat_summary;
use super::exports::export_summary;
use super::{ApiError, ApiResult, S, bad};
use crate::mcp::McpConfig;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) async fn api_state(State(st): S) -> ApiResult {
    let db = st.db.read().await;
    let mut chats: Vec<Value> = db
        .chats
        .values()
        .filter(|c| c.guest.is_none())
        .map(|c| chat_summary(c, db.exports.get(&c.export_id).map(|r| r.as_ref())))
        .collect();
    chats.sort_by(|a, b| b["updated_at"].as_str().cmp(&a["updated_at"].as_str()));
    let exports: Vec<Value> = db.exports.values().map(|r| export_summary(r)).collect();
    let shares: Vec<Value> = db.shares.values().map(crate::share_api::share_summary).collect();
    drop(db);
    let cfg = st.llm_config().await;
    // Whether someone chose the endpoint (Settings or OPENAI_BASE_URL), rather than the built-in default.
    let endpoint_set =
        !st.db.read().await.settings.llm.base_url.trim().is_empty() || std::env::var("OPENAI_BASE_URL").is_ok_and(|v| !v.trim().is_empty());
    Ok(Json(json!({
        "exports": exports,
        "chats": chats,
        "shares": shares,
        "llm": {
            "base_url": cfg.base_url, "model": cfg.model, "has_api_key": !cfg.api_key.is_empty(),
            "host": crate::llm::host_of(&cfg.base_url),
            "endpoint_set": endpoint_set,
            // Nothing can answer until a key is set (local endpoints usually need none).
            "needs_key": cfg.api_key.trim().is_empty() && !crate::llm::is_local(&cfg.base_url),
        },
        "offline": {"on": st.is_offline(), "forced": st.offline_forced()},
        "version": env!("CARGO_PKG_VERSION"),
    })))
}

pub(super) async fn get_settings(State(st): S) -> ApiResult {
    let s = st.db.read().await.settings.clone();
    // Model endpoints that Offline mode would refuse, so the UI can say so up front.
    let mut remote: Vec<String> = Vec::new();
    for cfg in [st.llm_config().await, st.media_llm_config().await] {
        let origin = crate::llm::origin_of(&cfg.base_url);
        if !crate::llm::is_local(&cfg.base_url) && !remote.contains(&origin) {
            remote.push(origin);
        }
    }
    Ok(Json(json!({
        "offline": {"on": st.is_offline(), "setting": s.offline, "forced": st.offline_forced(), "remote": remote},
        "base_url": s.llm.base_url,
        "model": s.llm.model,
        "temperature": s.llm.temperature,
        "has_api_key": !s.llm.api_key.is_empty(),
        "env": {"base_url": st.env_llm.base_url, "model": st.env_llm.model, "has_api_key": !st.env_llm.api_key.is_empty()},
        "history_budget_chars": s.history_budget_chars,
        "max_tool_steps": s.max_tool_steps,
        "media_model": s.media_model,
        "env_media_model": std::env::var("OPENAI_MEDIA_MODEL").unwrap_or_default(),
        "mcpServers": s.mcp_servers,
        "telegram": {
            "mode": s.telegram.mode,
            "has_token": !s.telegram.token.trim().is_empty(),
            // TELEGRAM_BOT_TOKEN is set: Settings can't change the bot.
            "env": st.telegram_env.get().is_some(),
            "relay_host": crate::relay_client::relay_host(&crate::relay_client::relay_base(&s.sharing.relay_url)),
        },
    })))
}

#[derive(Deserialize)]
pub(super) struct TelegramPatch {
    mode: Option<crate::store::TelegramMode>,
    /// `Some("")` clears the stored token, `None` keeps it.
    token: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct SettingsPatch {
    base_url: Option<String>,
    model: Option<String>,
    /// `Some("")` clears the stored key, `None` keeps it.
    api_key: Option<String>,
    temperature: Option<Option<f32>>,
    history_budget_chars: Option<usize>,
    max_tool_steps: Option<usize>,
    media_model: Option<String>,
    #[serde(rename = "mcpServers")]
    mcp_servers: Option<Value>,
    telegram: Option<TelegramPatch>,
    /// Offline mode: only local models, nothing leaves this computer.
    offline: Option<bool>,
}

pub(super) async fn put_settings(State(st): S, Json(p): Json<SettingsPatch>) -> ApiResult {
    let mcp: Option<McpConfig> = match p.mcp_servers {
        None => None,
        // Accept either the bare map or `{"mcpServers": {...}}`.
        Some(v) => {
            let v = v.get("mcpServers").cloned().unwrap_or(v);
            let cfg: McpConfig = serde_json::from_value(v).map_err(|e| bad(format!("invalid MCP config: {e}")))?;
            for (name, c) in &cfg {
                if c.url.is_none() && c.command.is_none() {
                    return Err(bad(format!("MCP server `{name}` needs `url` or `command`")));
                }
            }
            Some(cfg)
        }
    };
    if let Some(t) = p.telegram.as_ref().and_then(|t| t.token.as_deref()).map(str::trim).filter(|t| !t.is_empty()) {
        let well_formed = t.split_once(':').is_some_and(|(id, rest)| {
            !id.is_empty()
                && id.bytes().all(|b| b.is_ascii_digit())
                && rest.len() >= 30
                && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        });
        if !well_formed {
            return Err(bad("That doesn't look like a bot token. @BotFather shows it as 123456789:AA…"));
        }
    }
    let mut db = st.db.write().await;
    let s = &mut db.settings;
    let was_offline = st.is_offline();
    let telegram_changed = p.telegram.is_some();
    if let Some(v) = p.offline {
        s.offline = v;
    }
    if let Some(t) = p.telegram {
        if let Some(m) = t.mode {
            s.telegram.mode = m;
        }
        if let Some(token) = t.token {
            s.telegram.token = token.trim().to_string();
        }
    }
    if let Some(v) = p.base_url {
        s.llm.base_url = v.trim().to_string();
    }
    if let Some(v) = p.model {
        s.llm.model = v.trim().to_string();
    }
    if let Some(v) = p.api_key {
        s.llm.api_key = v.trim().to_string();
    }
    if let Some(t) = p.temperature {
        s.llm.temperature = t.map(|x| x.clamp(0.0, 2.0));
    }
    if let Some(b) = p.history_budget_chars {
        s.history_budget_chars = b.clamp(4_000, 2_000_000);
    }
    if let Some(m) = p.max_tool_steps {
        s.max_tool_steps = m.clamp(1, 20);
    }
    if let Some(m) = p.media_model {
        s.media_model = m.trim().to_string();
    }
    let changed_mcp = mcp.is_some_and(|m| {
        let changed = m != s.mcp_servers;
        s.mcp_servers = m;
        changed
    });
    st.paths.save_settings(s)?;
    st.set_offline_setting(s.offline);
    let offline_changed = st.is_offline() != was_offline;
    if changed_mcp || offline_changed {
        *st.mcp.lock().await = st.mcp_manager(s.mcp_servers.clone());
    }
    drop(db);
    if offline_changed {
        crate::sharing::reapply(&st).await;
    }
    if telegram_changed || offline_changed {
        let _g = st.control.lock().await;
        crate::tgbot::apply(&st).await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("Telegram: {e:#}")))?;
    }
    Ok(Json(json!({"ok": true})))
}

/// A tiny request with the saved settings, so a wrong key or model shows up in
/// Settings instead of in the middle of a conversation.
pub(super) async fn test_llm(State(st): S) -> ApiResult {
    let cfg = st.llm_config().await;
    let started = std::time::Instant::now();
    let msgs = [json!({"role": "user", "content": "Reply with the single word: ok"})];
    Ok(Json(match st.llm.complete(&cfg, &msgs, &[]).await {
        Ok(c) => json!({
            "ok": true, "model": cfg.model, "host": crate::llm::host_of(&cfg.base_url),
            "ms": started.elapsed().as_millis() as u64,
            "reply": crate::llm::truncate(c.content.as_deref().unwrap_or_default().trim(), 80),
        }),
        Err(e) => json!({
            "ok": false, "model": cfg.model, "host": crate::llm::host_of(&cfg.base_url),
            "ms": started.elapsed().as_millis() as u64, "error": format!("{e:#}"),
        }),
    }))
}

pub(super) async fn mcp_status(State(st): S) -> ApiResult {
    let mut m = st.mcp.lock().await;
    m.ensure_connected().await;
    Ok(Json(json!({"servers": m.status()})))
}

pub(super) async fn mcp_reconnect(State(st): S) -> ApiResult {
    let cfg = st.mcp.lock().await.config().clone();
    let mut fresh = st.mcp_manager(cfg);
    fresh.ensure_connected().await;
    let status = fresh.status();
    *st.mcp.lock().await = fresh;
    Ok(Json(json!({"servers": status})))
}
