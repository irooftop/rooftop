//! Minimal OpenAI-compatible `/chat/completions` client (OpenAI, OpenRouter,
//! Ollama, vLLM, LM Studio, …).

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LlmConfig {
    pub base_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

impl LlmConfig {
    /// Defaults from the environment: `OPENAI_BASE_URL`, `OPENAI_API_KEY`, `OPENAI_MODEL`, `OPENAI_TEMPERATURE`.
    pub fn from_env() -> Self {
        Self {
            base_url: std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://openrouter.ai/api/v1".into()),
            api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
            model: std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "openai/gpt-6-luna".into()),
            temperature: std::env::var("OPENAI_TEMPERATURE").ok().and_then(|t| t.parse().ok()),
        }
    }
}

/// Whether `ROOFTOP_OFFLINE` asks for Offline mode (`1`, `true`, `yes`, `on`).
pub fn offline_from_env() -> bool {
    std::env::var("ROOFTOP_OFFLINE").is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

#[derive(Debug, Clone, Default)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Default)]
pub struct Completion {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    /// The raw assistant message, to append back into the conversation.
    pub raw_message: Value,
    /// `usage` of the response, when the provider reports it.
    pub usage: Option<crate::chat::Usage>,
}

/// Reads OpenAI-style `usage`, including cached prompt tokens
/// (`prompt_tokens_details.cached_tokens`, some providers use `cache_read_input_tokens`).
pub fn parse_usage(v: &Value) -> Option<crate::chat::Usage> {
    let u = v.get("usage")?;
    let n = |p: &str| u.pointer(p).and_then(Value::as_u64);
    Some(crate::chat::Usage {
        prompt_tokens: n("/prompt_tokens")?,
        completion_tokens: n("/completion_tokens").unwrap_or(0),
        cached_tokens: n("/prompt_tokens_details/cached_tokens").or_else(|| n("/cache_read_input_tokens")).unwrap_or(0),
        prompt_chars: 0,
    })
}

#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    /// Offline mode: only local endpoints are contacted (shared with the app state).
    offline: Arc<AtomicBool>,
}

impl Default for LlmClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Installs the process-wide rustls crypto provider (ring) once.
pub fn ensure_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

impl LlmClient {
    pub fn new() -> Self {
        ensure_crypto_provider();
        let offline = Arc::new(AtomicBool::new(false));
        // In Offline mode a redirect must not lead away from the local network either.
        let flag = offline.clone();
        let redirects = reqwest::redirect::Policy::custom(move |a| {
            if a.previous().len() >= 10 {
                a.error("too many redirects")
            } else if flag.load(Ordering::Relaxed) && !is_local(a.url().as_str()) {
                let msg = format!("Offline mode: redirected to {}, which is not a local endpoint", origin_of(a.url().as_str()));
                a.error(msg)
            } else {
                a.follow()
            }
        });
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(180))
            .connect_timeout(Duration::from_secs(20))
            .redirect(redirects)
            .build()
            .expect("http client");
        Self { http, offline }
    }

    /// The Offline mode switch, shared with whoever owns the setting.
    pub fn offline_flag(&self) -> Arc<AtomicBool> {
        self.offline.clone()
    }

    pub async fn complete(&self, cfg: &LlmConfig, messages: &[Value], tools: &[Value]) -> Result<Completion> {
        if cfg.base_url.trim().is_empty() || cfg.model.trim().is_empty() {
            bail!("LLM is not configured: set the base URL and model in Settings (or OPENAI_BASE_URL / OPENAI_MODEL)");
        }
        if self.offline.load(Ordering::Relaxed) {
            offline_check(&cfg.base_url)?;
        }
        let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
        let mut body = json!({"model": cfg.model, "messages": messages});
        if !tools.is_empty() {
            body["tools"] = json!(tools);
            body["tool_choice"] = json!("auto");
        }
        if let Some(t) = cfg.temperature {
            body["temperature"] = json!(t);
        }
        let host = host_of(&cfg.base_url);
        if cfg.api_key.trim().is_empty() && !is_local(&cfg.base_url) {
            bail!("No API key is set for {host}. Add your key in Settings → API key (or set OPENAI_API_KEY before starting).");
        }
        let mut req = self.http.post(&url).header("X-Title", "iRoof.top").header("HTTP-Referer", "https://iroof.top").json(&body);
        if !cfg.api_key.is_empty() {
            req = req.bearer_auth(&cfg.api_key);
        }
        let resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                anyhow!("{host} took too long to answer. Try again, or pick a faster model in Settings.")
            } else if e.is_connect() {
                anyhow!("Can't reach {host}. Check the base URL in Settings and your internet connection. ({e})")
            } else {
                anyhow!("Request to {host} failed: {e}")
            }
        })?;
        let status = resp.status();
        let text = resp.text().await.context("reading the model's response")?;
        if !status.is_success() {
            bail!("{}", explain_failure(status.as_u16(), &host, &cfg.model, &provider_message(&text)));
        }
        let v: Value =
            serde_json::from_str(&text).with_context(|| format!("{host} returned something that is not JSON: {}", truncate(&text, 300)))?;
        if let Some(err) = v.get("error") {
            let code = err.get("code").and_then(Value::as_u64).unwrap_or(0) as u16;
            bail!("{}", explain_failure(code, &host, &cfg.model, &provider_message(&v.to_string())));
        }
        let msg =
            v.pointer("/choices/0/message").cloned().ok_or_else(|| anyhow!("LLM response has no choices: {}", truncate(&text, 300)))?;
        let content = msg.get("content").and_then(Value::as_str).map(str::to_string).filter(|s| !s.trim().is_empty());
        let tool_calls = msg
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .enumerate()
                    .map(|(i, c)| ToolCall {
                        id: c.get("id").and_then(Value::as_str).map(String::from).unwrap_or_else(|| format!("call_{i}")),
                        name: c.pointer("/function/name").and_then(Value::as_str).unwrap_or_default().to_string(),
                        arguments: match c.pointer("/function/arguments") {
                            Some(Value::String(s)) => s.clone(),
                            Some(other) => other.to_string(),
                            None => "{}".into(),
                        },
                    })
                    .collect()
            })
            .unwrap_or_default();
        let usage = parse_usage(&v).map(|mut u| {
            u.prompt_chars = messages.iter().map(|m| m.to_string().len() as u64).sum();
            u
        });
        Ok(Completion { content, tool_calls, raw_message: msg, usage })
    }
}

/// Host part of a base URL, for messages ("openrouter.ai").
pub fn host_of(base_url: &str) -> String {
    reqwest::Url::parse(base_url.trim()).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_else(|| base_url.trim().to_string())
}

/// Whether an endpoint is on this computer or this network: no key is needed, and Offline mode allows it.
///
/// Local: `localhost`, loopback (127.0.0.0/8, ::1), private LAN ranges (10/8, 172.16/12, 192.168/16, fc00::/7),
/// link-local (169.254/16, fe80::/10), `*.local` (mDNS) and `*.internal` (e.g. `host.docker.internal`).
/// Names are judged by their spelling, they are not resolved.
pub fn is_local(base_url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(base_url.trim()) else { return false };
    let Some(host) = u.host_str() else { return false };
    match host.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified(),
        Ok(std::net::IpAddr::V6(ip)) => {
            let seg = ip.segments()[0];
            ip.is_loopback() || ip.is_unspecified() || (seg & 0xfe00) == 0xfc00 || (seg & 0xffc0) == 0xfe80
        }
        Err(_) => {
            let host = host.to_ascii_lowercase();
            host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") || host.ends_with(".internal")
        }
    }
}

/// `scheme://host[:port]` of a URL, for messages.
pub fn origin_of(url: &str) -> String {
    match reqwest::Url::parse(url.trim()) {
        Ok(u) => u.origin().ascii_serialization(),
        Err(_) => url.trim().to_string(),
    }
}

/// Offline mode: `Err` with the reason unless the endpoint is local.
pub fn offline_check(url: &str) -> Result<()> {
    if is_local(url) {
        return Ok(());
    }
    bail!(
        "Offline mode: {} is not a local endpoint. Use a model on this computer or network (Ollama, LM Studio), or turn Offline mode off in Settings.",
        origin_of(url)
    )
}

/// The provider's own error text (`error.message` of an OpenAI-style body), else the raw body.
fn provider_message(body: &str) -> String {
    let v: Option<Value> = serde_json::from_str(body).ok();
    let msg = v
        .as_ref()
        .and_then(|v| v.pointer("/error/message").or_else(|| v.get("message")).or_else(|| v.get("error")))
        .map(|m| m.as_str().map(String::from).unwrap_or_else(|| m.to_string()))
        .unwrap_or_else(|| body.trim().to_string());
    truncate(&msg.split_whitespace().collect::<Vec<_>>().join(" "), 300)
}

/// A human explanation of a failed request: what happened and what to do, plus the provider's words.
pub fn explain_failure(status: u16, host: &str, model: &str, provider: &str) -> String {
    let lower = provider.to_lowercase();
    let what = match status {
        401 => format!("{host} rejected the API key (401). Check the key in Settings → API key."),
        402 => format!("Your {host} account is out of credits (402). Top up there, or choose a cheaper model in Settings."),
        403 => format!("{host} refused access (403): the key may not be allowed to use the model `{model}`. Check Settings."),
        404 => format!("{host} doesn't know the model `{model}` (404). Check Model and Base URL in Settings."),
        408 | 504 | 524 => format!("{host} timed out (HTTP {status}). Try again in a moment."),
        413 => format!("The request is too large for `{model}` (413). Lower the history budget in Settings."),
        429 => format!("Too many requests to {host}, or a usage limit was reached (429). Wait a moment and try again."),
        400 if lower.contains("tool") => {
            format!("The model `{model}` can't use tools (400), which the persona needs. Choose another model in Settings.")
        }
        400 if lower.contains("context") || lower.contains("too long") || lower.contains("maximum") => {
            format!("The conversation is too long for `{model}` (400). Lower the history budget in Settings.")
        }
        400 => format!("{host} rejected the request (400)."),
        500..=599 => format!("{host} is having trouble (HTTP {status}). Try again in a moment."),
        0 => format!("{host} returned an error."),
        s => format!("{host} answered with HTTP {s}."),
    };
    if provider.is_empty() { what } else { format!("{what} {host} says: “{provider}”") }
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { s.chars().take(max).collect::<String>() + "…" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explains_failures() {
        let m = explain_failure(
            401,
            "openrouter.ai",
            "x",
            &provider_message(r#"{"error":{"message":"No cookie auth credentials found","code":401}}"#),
        );
        assert!(m.starts_with("openrouter.ai rejected the API key (401). Check the key in Settings"), "{m}");
        assert!(m.contains("“No cookie auth credentials found”"));
        assert!(explain_failure(404, "h", "m/x", "").contains("doesn't know the model `m/x`"));
        assert!(explain_failure(400, "h", "m", "tools are not supported").contains("can't use tools"));
        assert!(is_local("http://localhost:11434/v1") && is_local("http://192.168.1.5:8000/v1") && is_local("http://127.0.0.1:1234"));
        assert!(!is_local("https://openrouter.ai/api/v1"));
        assert_eq!(host_of("https://openrouter.ai/api/v1"), "openrouter.ai");
    }

    #[tokio::test]
    async fn missing_key_fails_fast_with_guidance() {
        let cfg =
            LlmConfig { base_url: "https://openrouter.ai/api/v1".into(), api_key: String::new(), model: "m".into(), temperature: None };
        let e = LlmClient::new().complete(&cfg, &[json!({"role": "user", "content": "hi"})], &[]).await.unwrap_err().to_string();
        assert!(e.starts_with("No API key is set for openrouter.ai. Add your key in Settings"), "{e}");
    }

    #[test]
    fn local_hosts() {
        for u in [
            "http://localhost:11434/v1",
            "http://LocalHost/v1",
            "http://127.0.0.1:1234/v1",
            "http://127.9.9.9/v1",
            "http://[::1]:8080/v1",
            "http://10.0.0.7/v1",
            "http://172.16.0.1/v1",
            "http://172.31.255.1/v1",
            "http://192.168.1.5:8000/v1",
            "http://169.254.1.1/v1",
            "http://[fd12:3456::1]/v1",
            "http://[fe80::1]/v1",
            "http://studio.local:11434/v1",
            "http://host.docker.internal:11434/v1",
        ] {
            assert!(is_local(u), "{u} should be local");
        }
        for u in [
            "https://openrouter.ai/api/v1",
            "https://api.openai.com/v1",
            "http://8.8.8.8/v1",
            "http://172.32.0.1/v1",
            "http://172.15.0.1/v1",
            "http://[2606:4700::1111]/v1",
            "http://localhost.example.com/v1",
            "http://notlocal/v1",
            "not a url",
            "",
        ] {
            assert!(!is_local(u), "{u} should not be local");
        }
    }

    #[tokio::test]
    async fn offline_refuses_remote_endpoints_without_a_request() {
        let client = LlmClient::new();
        client.offline_flag().store(true, Ordering::Relaxed);
        let cfg = LlmConfig { base_url: "https://openrouter.ai/api/v1".into(), api_key: "k".into(), model: "m".into(), temperature: None };
        let e = client.complete(&cfg, &[json!({"role": "user", "content": "hi"})], &[]).await.unwrap_err().to_string();
        assert!(e.starts_with("Offline mode: https://openrouter.ai is not a local endpoint"), "{e}");
    }

    #[test]
    fn usage_with_cache() {
        let v = json!({"usage": {"prompt_tokens": 1200, "completion_tokens": 30, "prompt_tokens_details": {"cached_tokens": 1024}}});
        let u = parse_usage(&v).unwrap();
        assert_eq!((u.prompt_tokens, u.completion_tokens, u.cached_tokens), (1200, 30, 1024));
        assert!(parse_usage(&json!({})).is_none());
    }
}
