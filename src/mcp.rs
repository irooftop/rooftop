//! Tools-only Model Context Protocol client.
//!
//! Supported revisions:
//! * `2026-07-28` ("modern"): stateless, no `initialize`, and every request carries
//!   `_meta` (`io.modelcontextprotocol/protocolVersion`, `clientInfo`,
//!   `clientCapabilities`) and, on HTTP, `MCP-Protocol-Version`, `Mcp-Method`,
//!   `Mcp-Name` and `Mcp-Param-*` (`x-mcp-header`) headers. Era detection via
//!   `server/discover` with `UnsupportedProtocolVersionError` (-32022) handling.
//! * `2025-11-25`, `2025-06-18`, `2025-03-26` ("legacy" Streamable HTTP):
//!   `initialize` handshake, `Mcp-Session-Id`, `MCP-Protocol-Version`,
//!   JSON or SSE responses, re-initialize on 404.
//! * `2024-11-05`: deprecated HTTP+SSE transport (GET stream + `endpoint` event).
//! * stdio transport for all of the above (newline-delimited JSON-RPC).

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, oneshot};

pub const MODERN_VERSIONS: &[&str] = &["2026-07-28"];
pub const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
const META_CLIENT_CAPS: &str = "io.modelcontextprotocol/clientCapabilities";
const ERR_HEADER_MISMATCH: i64 = -32020;
const ERR_MISSING_CAPABILITY: i64 = -32021;
const ERR_UNSUPPORTED_VERSION: i64 = -32022;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// Same shape as the widespread `mcpServers` JSON (Claude Desktop, Cursor…).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ServerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Optional transport hint: "http" | "sse" | "stdio".
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    /// Working directory of a stdio server (set by the manager, not configurable):
    /// file outputs of the server are picked up only from inside it.
    #[serde(skip)]
    pub workdir: Option<std::path::PathBuf>,
}

/// Replaces `${NAME}` with the environment variable `NAME` (so secrets such as
/// `${OPENAI_API_KEY}` stay in the server environment / `.env`, not in settings).
/// Unknown variables are left as they are.
pub fn expand_env(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) if !after[..end].is_empty() && after[..end].chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(v) => out.push_str(&v),
                    Err(_) => {
                        tracing::warn!("MCP config references unset environment variable `{name}`");
                        out.push_str(&rest[start..start + 2 + end + 1]);
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push_str("${");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

impl ServerConfig {
    /// Copy with `${VAR}` references resolved in url, headers, command, args and env.
    pub fn resolved(&self) -> Self {
        let map = |m: &BTreeMap<String, String>| m.iter().map(|(k, v)| (k.clone(), expand_env(v))).collect();
        Self {
            url: self.url.as_deref().map(expand_env),
            headers: map(&self.headers),
            command: self.command.as_deref().map(expand_env),
            args: self.args.iter().map(|a| expand_env(a)).collect(),
            env: map(&self.env),
            ..self.clone()
        }
    }
}

/// LLM function names of media-creating MCP tools.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaTools {
    pub image: Option<String>,
    pub speech: Option<String>,
}

pub type McpConfig = BTreeMap<String, ServerConfig>;

fn client_info() -> Value {
    json!({"name": "iroof.top", "title": "iRoof.top", "version": env!("CARGO_PKG_VERSION")})
}

fn id_key(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[derive(Debug)]
pub enum RpcError {
    /// JSON-RPC error object.
    Rpc {
        code: i64,
        message: String,
        data: Option<Value>,
        http: Option<u16>,
    },
    /// Non-JSON-RPC HTTP failure.
    Http {
        status: u16,
        body: String,
    },
    Other(anyhow::Error),
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::Rpc { code, message, .. } => write!(f, "MCP error {code}: {message}"),
            RpcError::Http { status, body } => write!(f, "MCP HTTP {status}: {}", crate::llm::truncate(body, 300)),
            RpcError::Other(e) => write!(f, "{e:#}"),
        }
    }
}

impl From<anyhow::Error> for RpcError {
    fn from(e: anyhow::Error) -> Self {
        RpcError::Other(e)
    }
}

fn response_to_result(msg: Value, http: Option<u16>) -> Result<Value, RpcError> {
    if let Some(err) = msg.get("error") {
        return Err(RpcError::Rpc {
            code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: err.get("message").and_then(Value::as_str).unwrap_or_default().to_string(),
            data: err.get("data").cloned(),
            http,
        });
    }
    Ok(msg.get("result").cloned().unwrap_or(Value::Null))
}

/// Correlates responses with requests on a full-duplex channel (stdio, legacy SSE).
#[derive(Default)]
struct Mux {
    pending: Mutex<HashMap<String, oneshot::Sender<Value>>>,
}

impl Mux {
    /// Returns a reply for server-initiated requests.
    async fn dispatch(&self, msg: Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).map(str::to_string);
        match (id, method) {
            (Some(id), None) => {
                if let Some(tx) = self.pending.lock().await.remove(&id_key(&id)) {
                    let _ = tx.send(msg);
                }
                None
            }
            (Some(id), Some(method)) => Some(server_request_reply(&id, &method)),
            _ => None, // notification
        }
    }
}

fn server_request_reply(id: &Value, method: &str) -> Value {
    if method == "ping" {
        json!({"jsonrpc": "2.0", "id": id, "result": {}})
    } else {
        json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("client does not support {method}")}})
    }
}

struct Stdio {
    stdin: Mutex<tokio::process::ChildStdin>,
    mux: Arc<Mux>,
    _child: Mutex<tokio::process::Child>,
}

impl Stdio {
    async fn spawn(name: &str, cfg: &ServerConfig) -> Result<Arc<Self>> {
        let cmd = cfg.command.as_deref().ok_or_else(|| anyhow!("missing command"))?;
        let mut command = tokio::process::Command::new(cmd);
        if let Some(dir) = &cfg.workdir {
            std::fs::create_dir_all(dir).with_context(|| format!("creating MCP workspace {}", dir.display()))?;
            command.current_dir(dir);
        }
        let mut child = command
            .args(&cfg.args)
            .envs(&cfg.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawning MCP server `{cmd}`"))?;
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let stderr = child.stderr.take().expect("stderr");
        let me = Arc::new(Self { stdin: Mutex::new(stdin), mux: Arc::new(Mux::default()), _child: Mutex::new(child) });
        let weak = Arc::downgrade(&me);
        let mux = me.mux.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else { continue };
                if let Some(reply) = mux.dispatch(msg).await
                    && let Some(me) = weak.upgrade()
                {
                    let _ = me.write(&reply).await;
                }
            }
        });
        let label = name.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(server = %label, "mcp stderr: {line}");
            }
        });
        Ok(me)
    }

    async fn write(&self, msg: &Value) -> Result<()> {
        let mut line = serde_json::to_vec(msg)?;
        line.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(&line).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn request(&self, msg: Value, timeout: Duration) -> Result<Value, RpcError> {
        let (tx, rx) = oneshot::channel();
        let key = id_key(&msg["id"]);
        self.mux.pending.lock().await.insert(key.clone(), tx);
        self.write(&msg).await?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(resp)) => response_to_result(resp, None),
            Ok(Err(_)) => Err(anyhow!("MCP server closed the connection").into()),
            Err(_) => {
                self.mux.pending.lock().await.remove(&key);
                Err(anyhow!("MCP request timed out").into())
            }
        }
    }
}

/// Deprecated 2024-11-05 HTTP+SSE transport.
struct LegacySse {
    endpoint: String,
    mux: Arc<Mux>,
    _reader: tokio::task::JoinHandle<()>,
}

struct Http {
    client: reqwest::Client,
    url: String,
    headers: BTreeMap<String, String>,
    session: Mutex<Option<String>>,
    sse: Mutex<Option<Arc<LegacySse>>>,
}

fn header_safe(v: &str) -> bool {
    !v.is_empty() && v == v.trim() && v.bytes().all(|b| (0x20..=0x7e).contains(&b) || b == b'\t')
}

fn encode_header_value(v: &str) -> String {
    if header_safe(v) { v.to_string() } else { format!("=?base64?{}?=", base64::engine::general_purpose::STANDARD.encode(v.as_bytes())) }
}

/// Parses an SSE body, returning the JSON-RPC response with `want` id.
async fn read_sse_response(resp: reqwest::Response, want: &str, mut on_request: impl FnMut(Value)) -> Result<Value> {
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    while let Some(chunk) = stream.next().await {
        buf.push_str(&String::from_utf8_lossy(&chunk?).replace("\r\n", "\n"));
        while let Some(pos) = buf.find("\n\n") {
            let event: String = buf.drain(..pos + 2).collect();
            let data: Vec<&str> = event.lines().filter_map(|l| l.strip_prefix("data:")).map(|d| d.strip_prefix(' ').unwrap_or(d)).collect();
            if data.is_empty() {
                continue;
            }
            let Ok(msg) = serde_json::from_str::<Value>(&data.join("\n")) else { continue };
            for m in match msg {
                Value::Array(a) => a,
                one => vec![one],
            } {
                let is_response = m.get("method").is_none();
                match m.get("id") {
                    Some(id) if is_response && id_key(id) == want => return Ok(m),
                    Some(_) if !is_response => on_request(m),
                    _ => {}
                }
            }
        }
    }
    bail!("SSE stream ended without a response")
}

impl Http {
    fn new(cfg: &ServerConfig) -> Result<Self> {
        let url = cfg.url.clone().ok_or_else(|| anyhow!("missing url"))?;
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            bail!("MCP url must be http(s)");
        }
        crate::llm::ensure_crypto_provider();
        Ok(Self {
            client: reqwest::Client::builder().connect_timeout(Duration::from_secs(15)).build()?,
            url,
            headers: cfg.headers.clone(),
            session: Mutex::new(None),
            sse: Mutex::new(None),
        })
    }

    /// POST one JSON-RPC message. Returns the response (for requests) or Null (notifications).
    async fn post(self: &Arc<Self>, msg: &Value, extra: &[(String, String)], timeout: Duration) -> Result<Value, RpcError> {
        if let Some(sse) = self.sse.lock().await.clone() {
            return self.post_legacy_sse(&sse, msg, timeout).await;
        }
        let mut req = self
            .client
            .post(&self.url)
            .timeout(timeout)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json");
        for (k, v) in self.headers.iter() {
            req = req.header(k, v);
        }
        for (k, v) in extra {
            req = req.header(k, v);
        }
        if let Some(s) = self.session.lock().await.as_ref() {
            req = req.header("Mcp-Session-Id", s);
        }
        let resp = req.json(msg).send().await.map_err(|e| RpcError::Other(e.into()))?;
        let status = resp.status().as_u16();
        if let Some(sid) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            *self.session.lock().await = Some(sid.to_string());
        }
        let is_request = msg.get("id").is_some();
        if status == 202 || (!is_request && resp.status().is_success()) {
            return Ok(Value::Null);
        }
        let ctype = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or_default().to_lowercase();
        if ctype.starts_with("text/event-stream") && resp.status().is_success() {
            let want = id_key(&msg["id"]);
            // Answer server-initiated requests (e.g. ping) of pre-2026 servers.
            let mut reply_headers: Vec<(String, String)> = self.headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            reply_headers.extend(extra.iter().filter(|(k, _)| k.eq_ignore_ascii_case("MCP-Protocol-Version")).cloned());
            if let Some(sid) = self.session.lock().await.clone() {
                reply_headers.push(("Mcp-Session-Id".into(), sid));
            }
            let (client, url) = (self.client.clone(), self.url.clone());
            let found = tokio::time::timeout(
                timeout,
                read_sse_response(resp, &want, move |req| {
                    let (client, url, headers) = (client.clone(), url.clone(), reply_headers.clone());
                    tokio::spawn(async move {
                        let (id, method) = (req["id"].clone(), req["method"].as_str().unwrap_or_default().to_string());
                        let mut r = client
                            .post(url)
                            .header("Accept", "application/json, text/event-stream")
                            .json(&server_request_reply(&id, &method));
                        for (k, v) in headers {
                            r = r.header(k, v);
                        }
                        let _ = r.send().await;
                    });
                }),
            )
            .await
            .map_err(|_| RpcError::Other(anyhow!("MCP request timed out")))??;
            return response_to_result(found, Some(status));
        }
        let body = resp.text().await.map_err(|e| RpcError::Other(e.into()))?;
        let parsed: Option<Value> = serde_json::from_str(&body).ok();
        let parsed = match parsed {
            Some(Value::Array(a)) => a.into_iter().find(|m| m.get("id") == msg.get("id")),
            other => other,
        };
        match parsed {
            Some(v) if v.get("error").is_some() || (v.get("result").is_some() && (200..300).contains(&status)) => {
                response_to_result(v, Some(status))
            }
            _ => Err(RpcError::Http { status, body }),
        }
    }

    async fn open_legacy_sse(self: &Arc<Self>) -> Result<()> {
        let mut req = self.client.get(&self.url).header("Accept", "text/event-stream");
        for (k, v) in self.headers.iter() {
            req = req.header(k, v);
        }
        let resp = req.send().await?.error_for_status()?;
        let base = reqwest::Url::parse(&self.url)?;
        let mux = Arc::new(Mux::default());
        let (ep_tx, ep_rx) = oneshot::channel::<String>();
        let mux2 = mux.clone();
        let me = Arc::downgrade(self);
        let reader = tokio::spawn(async move {
            let mut ep_tx = Some(ep_tx);
            let mut stream = resp.bytes_stream();
            let mut buf = String::new();
            let mut endpoint: Option<String> = None;
            while let Some(Ok(chunk)) = stream.next().await {
                buf.push_str(&String::from_utf8_lossy(&chunk).replace("\r\n", "\n"));
                while let Some(pos) = buf.find("\n\n") {
                    let ev: String = buf.drain(..pos + 2).collect();
                    let name = ev.lines().find_map(|l| l.strip_prefix("event:")).map(str::trim).unwrap_or("message");
                    let data: Vec<&str> = ev.lines().filter_map(|l| l.strip_prefix("data:")).map(str::trim).collect();
                    let data = data.join("\n");
                    if name == "endpoint" {
                        if let (Some(tx), Ok(u)) = (ep_tx.take(), base.join(&data)) {
                            endpoint = Some(u.to_string());
                            let _ = tx.send(u.to_string());
                        }
                    } else if let Ok(msg) = serde_json::from_str::<Value>(&data)
                        && let Some(reply) = mux2.dispatch(msg).await
                        && let (Some(me), Some(ep)) = (me.upgrade(), endpoint.clone())
                    {
                        let _ = me.client.post(ep).json(&reply).send().await;
                    }
                }
            }
        });
        let endpoint = tokio::time::timeout(PROBE_TIMEOUT, ep_rx).await.map_err(|_| anyhow!("legacy SSE: no endpoint event"))??;
        *self.sse.lock().await = Some(Arc::new(LegacySse { endpoint, mux, _reader: reader }));
        Ok(())
    }

    async fn post_legacy_sse(&self, sse: &LegacySse, msg: &Value, timeout: Duration) -> Result<Value, RpcError> {
        let rx = if let Some(id) = msg.get("id") {
            let (tx, rx) = oneshot::channel();
            sse.mux.pending.lock().await.insert(id_key(id), tx);
            Some(rx)
        } else {
            None
        };
        let mut req = self.client.post(&sse.endpoint).json(msg);
        for (k, v) in self.headers.iter() {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(|e| RpcError::Other(e.into()))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            return Err(RpcError::Http { status, body: resp.text().await.unwrap_or_default() });
        }
        match rx {
            None => Ok(Value::Null),
            Some(rx) => match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(v)) => response_to_result(v, None),
                _ => Err(anyhow!("MCP request timed out").into()),
            },
        }
    }
}

enum Conn {
    Http(Arc<Http>),
    Stdio(Arc<Stdio>),
}

#[derive(Debug, Clone, PartialEq)]
enum Era {
    Modern(String),
    Legacy(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct McpTool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub description: String,
    pub input_schema: Value,
    /// `x-mcp-header` mirrors: (path of property keys, header suffix).
    #[serde(skip)]
    header_params: Vec<(Vec<String>, String)>,
}

pub struct McpClient {
    pub name: String,
    conn: Conn,
    era: Mutex<Era>,
    next_id: AtomicU64,
    pub tools: Vec<McpTool>,
    pub instructions: Option<String>,
    pub protocol_version: String,
    /// Canonical working directory of a stdio server (see [`ServerConfig::workdir`]).
    pub workspace: Option<std::path::PathBuf>,
}

fn pick_modern(supported: &[String]) -> Option<String> {
    MODERN_VERSIONS.iter().find(|v| supported.iter().any(|s| s == *v)).map(|s| s.to_string())
}

fn pick_legacy(supported: &[String]) -> Option<String> {
    LEGACY_VERSIONS.iter().find(|v| supported.iter().any(|s| s == *v)).map(|s| s.to_string())
}

fn supported_from_error(data: &Option<Value>) -> Vec<String> {
    data.as_ref()
        .and_then(|d| d.get("supported"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

fn is_modern_error(code: i64) -> bool {
    matches!(code, ERR_HEADER_MISMATCH | ERR_MISSING_CAPABILITY | ERR_UNSUPPORTED_VERSION)
}

/// Validates `x-mcp-header` annotations. `Err` means the tool must be excluded.
fn header_params(schema: &Value) -> Result<Vec<(Vec<String>, String)>, String> {
    fn tchar(c: char) -> bool {
        c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c)
    }
    fn walk(schema: &Value, path: &mut Vec<String>, out: &mut Vec<(Vec<String>, String)>) -> Result<(), String> {
        let Some(obj) = schema.as_object() else { return Ok(()) };
        if let Some(h) = obj.get("x-mcp-header") {
            if path.is_empty() {
                return Err("x-mcp-header on a non statically reachable property".into());
            }
            let h = h.as_str().ok_or("x-mcp-header must be a string")?;
            if h.is_empty() || !h.chars().all(tchar) {
                return Err(format!("invalid x-mcp-header `{h}`"));
            }
            let ty = obj.get("type").and_then(Value::as_str).unwrap_or_default();
            if !matches!(ty, "string" | "integer" | "boolean") {
                return Err(format!("x-mcp-header `{h}` on non-primitive type `{ty}`"));
            }
            if out.iter().any(|(_, x)| x.eq_ignore_ascii_case(h)) {
                return Err(format!("duplicate x-mcp-header `{h}`"));
            }
            out.push((path.clone(), h.to_string()));
        }
        for (key, sub) in obj {
            match key.as_str() {
                "properties" => {
                    if let Some(props) = sub.as_object() {
                        for (name, s) in props {
                            path.push(name.clone());
                            walk(s, path, out)?;
                            path.pop();
                        }
                    }
                }
                "x-mcp-header" => {}
                _ => {
                    // Anything outside a pure `properties` chain must not carry annotations.
                    if contains_header(sub) {
                        return Err(format!("x-mcp-header under `{key}` is not statically reachable"));
                    }
                }
            }
        }
        Ok(())
    }
    fn contains_header(v: &Value) -> bool {
        match v {
            Value::Object(o) => o.contains_key("x-mcp-header") || o.values().any(contains_header),
            Value::Array(a) => a.iter().any(contains_header),
            _ => false,
        }
    }
    let mut out = Vec::new();
    walk(schema, &mut Vec::new(), &mut out)?;
    Ok(out)
}

impl McpClient {
    pub async fn connect(name: &str, cfg: &ServerConfig) -> Result<Self> {
        let stdio = cfg.command.is_some() && cfg.kind.as_deref() != Some("http") && cfg.kind.as_deref() != Some("sse");
        let conn = if stdio { Conn::Stdio(Stdio::spawn(name, cfg).await?) } else { Conn::Http(Arc::new(Http::new(cfg)?)) };
        let mut client = Self {
            name: name.to_string(),
            conn,
            era: Mutex::new(Era::Modern(MODERN_VERSIONS[0].into())),
            next_id: AtomicU64::new(1),
            tools: Vec::new(),
            instructions: None,
            protocol_version: String::new(),
            workspace: if stdio { cfg.workdir.as_ref().and_then(|d| d.canonicalize().ok()) } else { None },
        };
        if cfg.kind.as_deref() == Some("sse") {
            if let Conn::Http(h) = &client.conn {
                h.open_legacy_sse().await?;
            }
            client.initialize(&["2024-11-05".to_string()]).await?;
        } else {
            client.negotiate().await?;
        }
        client.protocol_version = match &*client.era.lock().await {
            Era::Modern(v) | Era::Legacy(v) => v.clone(),
        };
        client.tools = client.list_tools().await?;
        Ok(client)
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Era detection: probe with `server/discover`, fall back to `initialize`.
    async fn negotiate(&mut self) -> Result<()> {
        let preferred = MODERN_VERSIONS[0].to_string();
        match self.modern_request(&preferred, "server/discover", json!({}), None, PROBE_TIMEOUT).await {
            Ok(result) => {
                let supported: Vec<String> = result
                    .get("supportedVersions")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                self.instructions = result.get("instructions").and_then(Value::as_str).map(String::from);
                if let Some(v) = pick_modern(&supported).or_else(|| supported.is_empty().then(|| preferred.clone())) {
                    *self.era.lock().await = Era::Modern(v);
                    return Ok(());
                }
                self.initialize(&supported).await
            }
            Err(RpcError::Rpc { code: ERR_UNSUPPORTED_VERSION, data, .. }) => {
                let supported = supported_from_error(&data);
                if let Some(v) = pick_modern(&supported) {
                    *self.era.lock().await = Era::Modern(v);
                    Ok(())
                } else if pick_legacy(&supported).is_some() {
                    self.initialize(&supported).await
                } else {
                    bail!("no mutually supported MCP version (server supports {supported:?})")
                }
            }
            Err(RpcError::Rpc { code, message, .. }) if is_modern_error(code) => {
                bail!("MCP server rejected discovery: {message}")
            }
            Err(e) => {
                tracing::debug!(server = %self.name, "server/discover failed ({e}), falling back to initialize");
                let attempt = self.initialize(&[]).await;
                if attempt.is_err()
                    && let Conn::Http(h) = &self.conn
                {
                    tracing::debug!(server = %self.name, "falling back to deprecated HTTP+SSE transport");
                    h.open_legacy_sse().await?;
                    return self.initialize(&["2024-11-05".to_string()]).await;
                }
                attempt
            }
        }
    }

    /// Legacy handshake (2025-11-25 … 2024-11-05).
    async fn initialize(&self, hint: &[String]) -> Result<()> {
        let requested = pick_legacy(hint).unwrap_or_else(|| LEGACY_VERSIONS[0].to_string());
        let msg = json!({
            "jsonrpc": "2.0", "id": self.next_id(), "method": "initialize",
            "params": {"protocolVersion": requested, "capabilities": {}, "clientInfo": client_info()}
        });
        let result = self.send_raw(&msg, &[], REQUEST_TIMEOUT).await.map_err(|e| anyhow!("initialize failed: {e}"))?;
        let version = result.get("protocolVersion").and_then(Value::as_str).unwrap_or_default().to_string();
        if !LEGACY_VERSIONS.contains(&version.as_str()) {
            bail!("server chose unsupported MCP version `{version}`");
        }
        *self.era.lock().await = Era::Legacy(version.clone());
        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        self.send_raw(&note, &self.legacy_headers(&version), REQUEST_TIMEOUT).await.map_err(|e| anyhow!("{e}"))?;
        Ok(())
    }

    fn legacy_headers(&self, version: &str) -> Vec<(String, String)> {
        if version == "2024-11-05" { vec![] } else { vec![("MCP-Protocol-Version".into(), version.into())] }
    }

    async fn send_raw(&self, msg: &Value, headers: &[(String, String)], timeout: Duration) -> Result<Value, RpcError> {
        match &self.conn {
            Conn::Http(h) => h.post(msg, headers, timeout).await,
            Conn::Stdio(s) => {
                if msg.get("id").is_some() {
                    s.request(msg.clone(), timeout).await
                } else {
                    s.write(msg).await?;
                    Ok(Value::Null)
                }
            }
        }
    }

    async fn modern_request(
        &self,
        version: &str,
        method: &str,
        mut params: Value,
        tool: Option<&McpTool>,
        timeout: Duration,
    ) -> Result<Value, RpcError> {
        let meta = params.as_object_mut().expect("params object").entry("_meta").or_insert_with(|| json!({}));
        meta[META_VERSION] = json!(version);
        meta[META_CLIENT_INFO] = client_info();
        meta[META_CLIENT_CAPS] = json!({});
        let mut headers = vec![("MCP-Protocol-Version".to_string(), version.to_string()), ("Mcp-Method".to_string(), method.to_string())];
        if let Some(name) = params.get("name").or(params.get("uri")).and_then(Value::as_str) {
            headers.push(("Mcp-Name".into(), encode_header_value(name)));
        }
        if let Some(tool) = tool {
            for (path, header) in &tool.header_params {
                let mut v = &params["arguments"];
                for k in path {
                    v = &v[k.as_str()];
                }
                let s = match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Bool(b) => Some(b.to_string()),
                    Value::Number(n) if n.is_i64() || n.is_u64() => Some(n.to_string()),
                    _ => None,
                };
                if let Some(s) = s {
                    headers.push((format!("Mcp-Param-{header}"), encode_header_value(&s)));
                }
            }
        }
        let msg = json!({"jsonrpc": "2.0", "id": self.next_id(), "method": method, "params": params});
        let result = self.send_raw(&msg, &headers, timeout).await?;
        if result.get("resultType").and_then(Value::as_str) == Some("input_required") {
            return Err(anyhow!("MCP server requires interactive input (elicitation/sampling), which this client does not provide").into());
        }
        Ok(result)
    }

    async fn request(&self, method: &str, params: Value, tool: Option<&McpTool>) -> Result<Value, RpcError> {
        let era = self.era.lock().await.clone();
        match era {
            Era::Modern(v) => match self.modern_request(&v, method, params.clone(), tool, REQUEST_TIMEOUT).await {
                Err(RpcError::Rpc { code: ERR_UNSUPPORTED_VERSION, data, .. }) => {
                    let v2 = pick_modern(&supported_from_error(&data))
                        .ok_or_else(|| anyhow!("MCP server dropped support for our protocol versions"))?;
                    *self.era.lock().await = Era::Modern(v2.clone());
                    self.modern_request(&v2, method, params, tool, REQUEST_TIMEOUT).await
                }
                other => other,
            },
            Era::Legacy(v) => {
                let msg = json!({"jsonrpc": "2.0", "id": self.next_id(), "method": method, "params": params});
                match self.send_raw(&msg, &self.legacy_headers(&v), REQUEST_TIMEOUT).await {
                    // Session expired: re-initialize once and retry.
                    Err(RpcError::Http { status: 404, .. }) if matches!(&self.conn, Conn::Http(h) if h.session.lock().await.is_some()) => {
                        if let Conn::Http(h) = &self.conn {
                            *h.session.lock().await = None;
                        }
                        self.initialize(std::slice::from_ref(&v)).await?;
                        let msg = json!({"jsonrpc": "2.0", "id": self.next_id(), "method": method, "params": msg["params"]});
                        self.send_raw(&msg, &self.legacy_headers(&v), REQUEST_TIMEOUT).await
                    }
                    other => other,
                }
            }
        }
    }

    async fn list_tools(&self) -> Result<Vec<McpTool>> {
        let modern_http = matches!(&*self.era.lock().await, Era::Modern(_)) && matches!(self.conn, Conn::Http(_));
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..50 {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let result = self.request("tools/list", params, None).await.map_err(|e| anyhow!("tools/list: {e}"))?;
            for t in result.get("tools").and_then(Value::as_array).cloned().unwrap_or_default() {
                let name = t.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
                if name.is_empty() {
                    continue;
                }
                let schema = t.get("inputSchema").cloned().unwrap_or_else(|| json!({"type": "object"}));
                let hp = if modern_http {
                    match header_params(&schema) {
                        Ok(h) => h,
                        Err(reason) => {
                            tracing::warn!(server = %self.name, tool = %name, "excluding MCP tool: {reason}");
                            continue;
                        }
                    }
                } else {
                    Vec::new()
                };
                tools.push(McpTool {
                    title: t.get("title").and_then(Value::as_str).map(String::from),
                    description: t.get("description").and_then(Value::as_str).unwrap_or_default().to_string(),
                    input_schema: schema,
                    header_params: hp,
                    name,
                });
            }
            cursor = result.get("nextCursor").and_then(Value::as_str).map(String::from);
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value> {
        let tool = self.tools.iter().find(|t| t.name == name);
        self.request("tools/call", json!({"name": name, "arguments": arguments}), tool).await.map_err(|e| anyhow!("{e}"))
    }
}

/// Renders a `CallToolResult` for the LLM (text, links, structured content).
pub fn result_to_text(result: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        parts.push("ERROR:".into());
    }
    for c in result.get("content").and_then(Value::as_array).cloned().unwrap_or_default() {
        match c.get("type").and_then(Value::as_str).unwrap_or_default() {
            "text" => parts.push(c.get("text").and_then(Value::as_str).unwrap_or_default().to_string()),
            "image" | "audio" => parts.push(format!(
                "[{} {}]",
                c["type"].as_str().unwrap_or_default(),
                c.get("mimeType").and_then(Value::as_str).unwrap_or_default()
            )),
            "resource_link" => parts.push(format!(
                "[resource_link {} {} {}]",
                c.get("name").and_then(Value::as_str).unwrap_or_default(),
                c.get("uri").and_then(Value::as_str).unwrap_or_default(),
                c.get("mimeType").and_then(Value::as_str).unwrap_or_default()
            )),
            "resource" => {
                let r = &c["resource"];
                let text = r.get("text").and_then(Value::as_str).map(|t| crate::llm::truncate(t, 4000)).unwrap_or_default();
                parts.push(format!("[resource {}] {text}", r.get("uri").and_then(Value::as_str).unwrap_or_default()));
            }
            _ => parts.push(c.to_string()),
        }
    }
    if let Some(sc) = result.get("structuredContent")
        && !sc.is_null()
    {
        parts.push(format!("structuredContent: {sc}"));
    }
    crate::llm::truncate(&parts.join("\n"), 12000)
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerStatus {
    pub name: String,
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub tools: Vec<String>,
}

/// Lazily connects configured servers and exposes their tools to the LLM as
/// `mcp_<server>_<tool>` functions.
#[derive(Default)]
pub struct McpManager {
    config: McpConfig,
    clients: BTreeMap<String, Arc<McpClient>>,
    errors: BTreeMap<String, String>,
    /// OpenAI function name -> (server, tool)
    names: HashMap<String, (String, String)>,
    workspace_root: Option<std::path::PathBuf>,
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
}

impl McpManager {
    pub fn new(config: McpConfig) -> Self {
        Self { config, ..Default::default() }
    }

    /// Stdio servers run in `<root>/<server name>` (created on demand).
    pub fn with_workspace(mut self, root: std::path::PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }

    pub fn workspace_root(&self) -> Option<&std::path::Path> {
        self.workspace_root.as_deref()
    }

    pub fn config(&self) -> &McpConfig {
        &self.config
    }

    pub async fn ensure_connected(&mut self) {
        let todo: Vec<(String, ServerConfig)> = self
            .config
            .iter()
            .filter(|(n, c)| !c.disabled && !self.clients.contains_key(*n) && !self.errors.contains_key(*n))
            .map(|(n, c)| {
                let mut c = c.resolved();
                c.workdir = self.workspace_root.as_ref().map(|r| r.join(sanitize(n)));
                (n.clone(), c)
            })
            .collect();
        let results = futures::future::join_all(todo.into_iter().map(|(n, c)| async move {
            let r = tokio::time::timeout(Duration::from_secs(45), McpClient::connect(&n, &c)).await;
            (n, r)
        }))
        .await;
        for (name, r) in results {
            match r {
                Ok(Ok(c)) => {
                    tracing::info!(server = %name, version = %c.protocol_version, tools = c.tools.len(), "MCP server connected");
                    self.clients.insert(name, Arc::new(c));
                }
                Ok(Err(e)) => {
                    tracing::warn!(server = %name, "MCP connect failed: {e:#}");
                    self.errors.insert(name, format!("{e:#}"));
                }
                Err(_) => {
                    self.errors.insert(name, "connection timed out".into());
                }
            }
        }
        self.names.clear();
        for (server, c) in &self.clients {
            for t in &c.tools {
                let mut fname: String = format!("mcp_{}_{}", sanitize(server), sanitize(&t.name)).chars().take(64).collect();
                let mut n = 2;
                while self.names.contains_key(&fname) {
                    fname = format!("{}_{n}", fname.chars().take(60).collect::<String>());
                    n += 1;
                }
                self.names.insert(fname, (server.clone(), t.name.clone()));
            }
        }
    }

    /// Clears connection errors so the next turn retries.
    pub fn retry_failed(&mut self) {
        self.errors.clear();
    }

    pub fn tool_definitions(&self) -> Vec<Value> {
        let mut defs: Vec<(String, Value)> = self
            .names
            .iter()
            .filter_map(|(fname, (server, tool))| {
                let t = self.clients.get(server)?.tools.iter().find(|t| &t.name == tool)?;
                let mut params = t.input_schema.clone();
                if params.get("type").is_none() {
                    params["type"] = json!("object");
                }
                if params.get("properties").is_none() {
                    params["properties"] = json!({});
                }
                let desc = format!("[MCP server `{server}`] {}", if t.description.is_empty() { t.title.clone().unwrap_or_default() } else { t.description.clone() });
                Some((fname.clone(), json!({"type": "function", "function": {"name": fname, "description": crate::llm::truncate(&desc, 1000), "parameters": params}})))
            })
            .collect();
        defs.sort_by(|a, b| a.0.cmp(&b.0));
        defs.into_iter().map(|(_, d)| d).collect()
    }

    pub fn instructions_note(&self) -> Option<String> {
        if self.names.is_empty() {
            return None;
        }
        let mut s = String::from(
            "Extra tools from MCP servers are available (names start with `mcp_`). Their results are only visible to you. \
             If one returns a GIF/sticker/image URL you want to send, pass a direct media URL to send_gif(url=…). Use them only when the person would naturally do so.",
        );
        if self.has_media_tools() {
            s.push_str(
                "\nSome of these tools create new images or audio. Every image or audio file a tool returns is saved as chat media and the result names its id \
                 (`gen_…`), and nothing reaches the chat until you send it with send_media {\"media_id\": \"gen_…\", \"caption\"?: …}. An image arrives as a photo, \
                 audio as a voice message. Use this to share media the way the person does, judging by the history and the style profile: \
                 if they often send photos, send a fresh photo when they would (write the prompt as a casual phone photo of their real life: \
                 places, food, pets, things you talk about, with no text in the image unless natural). If they talk in voice messages, answer with a voice message: \
                 generate speech of exactly what they would say, in their words, language and tone, and send that instead of typing it. \
                 Give output files a short relative name (e.g. `photo.jpg`, `voice.wav`, and prefer wav or mp3 for speech) and generate one item per tool call. \
                 For an ordinary voice message or photo, prefer the built-in send_voice / send_photo tools: they do all of this in one step.",
            );
        }
        for (name, c) in &self.clients {
            if let Some(i) = &c.instructions {
                s.push_str(&format!("\nServer `{name}` notes: {}", crate::llm::truncate(i, 600)));
            }
        }
        Some(s)
    }

    /// The function names (as exposed to the LLM) of tools that create a photo
    /// and speech, if a connected server offers them.
    pub fn media_tools(&self) -> MediaTools {
        let find = |pred: &dyn Fn(&str) -> bool| {
            let mut names: Vec<&String> = self.names.iter().filter(|(_, (_, tool))| pred(&tool.to_lowercase())).map(|(f, _)| f).collect();
            names.sort();
            names.first().map(|s| s.to_string())
        };
        MediaTools {
            image: find(&|t| t.contains("image") && ["generate", "create", "draw", "make"].iter().any(|v| t.contains(v))),
            speech: find(&|t| ["speech", "tts", "speak", "voice"].iter().any(|v| t.contains(v)) && !t.starts_with("list")),
        }
    }

    /// Whether a connected server offers tools that create images or audio.
    pub fn has_media_tools(&self) -> bool {
        const HINTS: [&str; 7] = ["image", "speech", "tts", "audio", "voice", "photo", "picture"];
        self.clients.values().flat_map(|c| c.tools.iter()).any(|t| {
            let text = format!("{} {}", t.name, t.description).to_lowercase();
            HINTS.iter().any(|h| text.contains(h))
        })
    }

    pub fn resolve(&self, fname: &str) -> Option<(Arc<McpClient>, String)> {
        let (server, tool) = self.names.get(fname)?;
        Some((self.clients.get(server)?.clone(), tool.clone()))
    }

    pub fn status(&self) -> Vec<ServerStatus> {
        self.config
            .iter()
            .map(|(name, cfg)| {
                let c = self.clients.get(name);
                ServerStatus {
                    name: name.clone(),
                    connected: c.is_some(),
                    protocol_version: c.map(|c| c.protocol_version.clone()),
                    error: if cfg.disabled { Some("disabled".into()) } else { self.errors.get(name).cloned() },
                    tools: c.map(|c| c.tools.iter().map(|t| t.name.clone()).collect()).unwrap_or_default(),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn expands_env_references() {
        // SAFETY: test-only variable name, set before use.
        unsafe { std::env::set_var("ROOFTOP_TEST_SECRET", "s3cret") };
        assert_eq!(expand_env("Bearer ${ROOFTOP_TEST_SECRET}"), "Bearer s3cret");
        assert_eq!(expand_env("${ROOFTOP_TEST_UNSET_VAR}"), "${ROOFTOP_TEST_UNSET_VAR}");
        assert_eq!(expand_env("a ${ b} $${x"), "a ${ b} $${x");
        let mut cfg = ServerConfig::default();
        cfg.env.insert("KEY".into(), "${ROOFTOP_TEST_SECRET}".into());
        cfg.args = vec!["--k=${ROOFTOP_TEST_SECRET}".into()];
        let r = cfg.resolved();
        assert_eq!(r.env["KEY"], "s3cret");
        assert_eq!(r.args[0], "--k=s3cret");
        assert_eq!(cfg.env["KEY"], "${ROOFTOP_TEST_SECRET}", "the stored config keeps the reference");
    }

    use super::*;

    #[test]
    fn header_param_validation() {
        let ok = json!({"type": "object", "properties": {"region": {"type": "string", "x-mcp-header": "Region"}, "q": {"type": "string"}}});
        assert_eq!(header_params(&ok).unwrap(), vec![(vec!["region".to_string()], "Region".to_string())]);
        let bad_type = json!({"type": "object", "properties": {"n": {"type": "number", "x-mcp-header": "N"}}});
        assert!(header_params(&bad_type).is_err());
        let in_items = json!({"type": "object", "properties": {"a": {"type": "array", "items": {"type": "string", "x-mcp-header": "A"}}}});
        assert!(header_params(&in_items).is_err());
        let dup = json!({"type": "object", "properties": {"a": {"type": "string", "x-mcp-header": "X"}, "b": {"type": "string", "x-mcp-header": "x"}}});
        assert!(header_params(&dup).is_err());
    }

    #[test]
    fn header_encoding() {
        assert_eq!(encode_header_value("us-west1"), "us-west1");
        assert_eq!(encode_header_value("héllo"), format!("=?base64?{}?=", base64::engine::general_purpose::STANDARD.encode("héllo")));
        assert!(encode_header_value(" padded").starts_with("=?base64?"));
    }

    #[test]
    fn tool_result_rendering() {
        let r = json!({"content": [{"type": "text", "text": "hi"}, {"type": "resource_link", "uri": "https://x/y.gif", "name": "y", "mimeType": "image/gif"}], "structuredContent": {"a": 1}});
        let t = result_to_text(&r);
        assert!(t.contains("hi") && t.contains("https://x/y.gif") && t.contains("\"a\":1"));
    }
}
