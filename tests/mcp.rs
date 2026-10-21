//! MCP client against mock servers of every supported protocol era.

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use rooftop::mcp::{McpClient, McpManager, ServerConfig};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::{Mutex, mpsc};

async fn spawn(app: Router) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    format!("http://{addr}")
}

fn h<'a>(headers: &'a HeaderMap, k: &str) -> Option<&'a str> {
    headers.get(k).and_then(|v| v.to_str().ok())
}

fn rpc_err(id: &Value, code: i64, msg: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}})
}

fn gif_tools() -> Value {
    json!([
        {"name": "search_gifs", "description": "Search GIFs", "inputSchema": {"type": "object", "properties": {
            "query": {"type": "string"}, "region": {"type": "string", "x-mcp-header": "Region"}}, "required": ["query"]}},
        {"name": "broken", "inputSchema": {"type": "object", "properties": {"n": {"type": "number", "x-mcp-header": "N"}}}}
    ])
}

// ---------- 2026-07-28 (stateless) ----------
async fn modern(headers: HeaderMap, body: axum::Json<Value>) -> Response {
    let msg = body.0;
    let id = msg["id"].clone();
    let method = msg["method"].as_str().unwrap_or_default();
    let meta = &msg["params"]["_meta"];
    if method == "initialize" {
        return (StatusCode::BAD_REQUEST, axum::Json(rpc_err(&id, -32601, "use 2026-07-28"))).into_response();
    }
    let version = h(&headers, "mcp-protocol-version").unwrap_or_default();
    if version != "2026-07-28" || meta["io.modelcontextprotocol/protocolVersion"] != "2026-07-28" {
        let mut e = rpc_err(&id, -32022, "Unsupported protocol version");
        e["error"]["data"] = json!({"supported": ["2026-07-28"], "requested": version});
        return (StatusCode::BAD_REQUEST, axum::Json(e)).into_response();
    }
    assert_eq!(h(&headers, "mcp-method"), Some(method), "Mcp-Method header");
    assert!(meta["io.modelcontextprotocol/clientInfo"]["name"].is_string());
    assert!(meta["io.modelcontextprotocol/clientCapabilities"].is_object());
    assert!(h(&headers, "accept").unwrap().contains("text/event-stream"));
    assert!(h(&headers, "mcp-session-id").is_none());
    let result = match method {
        "server/discover" => {
            json!({"resultType": "complete", "supportedVersions": ["2026-07-28"], "capabilities": {"tools": {}}, "instructions": "GIF search"})
        }
        "tools/list" => {
            if msg["params"]["cursor"].is_null() {
                json!({"resultType": "complete", "tools": gif_tools(), "nextCursor": "p2"})
            } else {
                json!({"resultType": "complete", "tools": [{"name": "trending", "inputSchema": {"type": "object"}}]})
            }
        }
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap();
            assert_eq!(h(&headers, "mcp-name"), Some(name));
            let region = msg["params"]["arguments"]["region"].as_str();
            assert_eq!(h(&headers, "mcp-param-region"), region, "x-mcp-header mirror");
            // Answer through SSE to exercise stream parsing.
            let resp = json!({"jsonrpc": "2.0", "id": id, "result": {"resultType": "complete",
                "content": [{"type": "text", "text": "https://media.giphy.com/media/abc/giphy.gif"}], "structuredContent": {"count": 1}}});
            let note = json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {"progress": 1}});
            let body = format!("event: message\ndata: {note}\n\nevent: message\ndata: {resp}\n\n");
            return ([("content-type", "text/event-stream")], body).into_response();
        }
        _ => return (StatusCode::NOT_FOUND, axum::Json(rpc_err(&id, -32601, "unknown"))).into_response(),
    };
    axum::Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

#[tokio::test]
async fn modern_2026_stateless() {
    let url = spawn(Router::new().route("/mcp", post(modern))).await;
    let cfg = ServerConfig { url: Some(format!("{url}/mcp")), ..Default::default() };
    let c = McpClient::connect("giphy", &cfg).await.unwrap();
    assert_eq!(c.protocol_version, "2026-07-28");
    assert_eq!(c.instructions.as_deref(), Some("GIF search"));
    let names: Vec<&str> = c.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["search_gifs", "trending"], "invalid x-mcp-header tool excluded, pagination followed");
    let r = c.call_tool("search_gifs", json!({"query": "cat", "region": "eu"})).await.unwrap();
    let text = rooftop::mcp::result_to_text(&r);
    assert!(text.contains("giphy.gif") && text.contains("\"count\":1"));
}

// ---------- 2025-06-18 Streamable HTTP with sessions ----------
#[derive(Default)]
struct Legacy {
    expire_once: AtomicBool,
    inits: AtomicUsize,
    pinged: AtomicBool,
}

async fn legacy(State(st): State<Arc<Legacy>>, headers: HeaderMap, body: axum::Json<Value>) -> Response {
    let msg = body.0;
    let id = msg["id"].clone();
    let method = msg["method"].as_str().unwrap_or_default();
    match method {
        "initialize" => {
            // First handshake offers our newest legacy version, and re-inits reuse the negotiated one.
            let offered = msg["params"]["protocolVersion"].as_str().unwrap_or_default();
            let expected = if st.inits.load(Ordering::SeqCst) == 0 { "2025-11-25" } else { "2025-06-18" };
            assert_eq!(offered, expected);
            let n = st.inits.fetch_add(1, Ordering::SeqCst) + 1;
            let r = json!({"jsonrpc": "2.0", "id": id, "result": {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "legacy", "version": "1"}}});
            return ([("mcp-session-id", format!("sess-{n}"))], axum::Json(r)).into_response();
        }
        "notifications/initialized" => return StatusCode::ACCEPTED.into_response(),
        "server/discover" => {
            return (StatusCode::BAD_REQUEST, axum::Json(rpc_err(&id, -32000, "Bad Request: Server not initialized"))).into_response();
        }
        _ => {}
    }
    if msg.get("method").is_none() && msg.get("result").is_some() {
        // Client's reply to our ping.
        st.pinged.store(true, Ordering::SeqCst);
        return StatusCode::ACCEPTED.into_response();
    }
    let expected = format!("sess-{}", st.inits.load(Ordering::SeqCst));
    if h(&headers, "mcp-session-id") != Some(expected.as_str()) {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    }
    assert_eq!(h(&headers, "mcp-protocol-version"), Some("2025-06-18"));
    match method {
        "tools/list" => axum::Json(json!({"jsonrpc": "2.0", "id": id, "result": {"tools": gif_tools()}})).into_response(),
        "tools/call" => {
            if st.expire_once.swap(false, Ordering::SeqCst) {
                return (StatusCode::NOT_FOUND, "expired").into_response();
            }
            let ping = json!({"jsonrpc": "2.0", "id": "srv-1", "method": "ping"});
            let resp =
                json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "legacy ok"}], "isError": false}});
            ([("content-type", "text/event-stream")], format!("data: {ping}\n\ndata: {resp}\n\n")).into_response()
        }
        _ => axum::Json(rpc_err(&id, -32601, "unknown")).into_response(),
    }
}

#[tokio::test]
async fn legacy_streamable_http_sessions() {
    let st = Arc::new(Legacy { expire_once: AtomicBool::new(true), ..Default::default() });
    let url = spawn(Router::new().route("/mcp", post(legacy)).with_state(st.clone())).await;
    let cfg = ServerConfig { url: Some(format!("{url}/mcp")), ..Default::default() };
    let c = McpClient::connect("legacy", &cfg).await.unwrap();
    assert_eq!(c.protocol_version, "2025-06-18");
    // Legacy era ignores x-mcp-header, so both tools are kept.
    assert_eq!(c.tools.len(), 2);
    let r = c.call_tool("search_gifs", json!({"query": "x"})).await.unwrap();
    assert!(rooftop::mcp::result_to_text(&r).contains("legacy ok"));
    assert_eq!(st.inits.load(Ordering::SeqCst), 2, "re-initialized after 404");
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(st.pinged.load(Ordering::SeqCst), "answered server ping");
}

// ---------- 2024-11-05 HTTP+SSE ----------
#[derive(Clone)]
struct Old {
    tx: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,
}

async fn old_sse(State(st): State<Old>) -> impl IntoResponse {
    let (tx, rx) = mpsc::unbounded_channel::<String>();
    tx.send("__endpoint__".into()).unwrap();
    *st.tx.lock().await = Some(tx);
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        let m = rx.recv().await?;
        let ev = if m == "__endpoint__" {
            SseEvent::default().event("endpoint").data("/messages?session=1")
        } else {
            SseEvent::default().event("message").data(m)
        };
        Some((Ok::<_, std::convert::Infallible>(ev), rx))
    });
    Sse::new(stream)
}

async fn old_post(State(st): State<Old>, body: axum::Json<Value>) -> StatusCode {
    let msg = body.0;
    let id = msg["id"].clone();
    let result = match msg["method"].as_str().unwrap_or_default() {
        "initialize" => {
            json!({"protocolVersion": "2024-11-05", "capabilities": {"tools": {}}, "serverInfo": {"name": "old", "version": "0"}})
        }
        "tools/list" => json!({"tools": [{"name": "random_gif", "inputSchema": {"type": "object"}}]}),
        "tools/call" => json!({"content": [{"type": "text", "text": "old ok"}]}),
        _ => return StatusCode::ACCEPTED,
    };
    if let Some(tx) = st.tx.lock().await.as_ref() {
        tx.send(json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()).unwrap();
    }
    StatusCode::ACCEPTED
}

#[tokio::test]
async fn legacy_http_sse_2024() {
    let st = Old { tx: Arc::new(Mutex::new(None)) };
    let app = Router::new()
        .route("/sse", get(old_sse).post(|| async { StatusCode::METHOD_NOT_ALLOWED }))
        .route("/messages", post(old_post))
        .with_state(st);
    let url = spawn(app).await;
    let cfg = ServerConfig { url: Some(format!("{url}/sse")), ..Default::default() };
    let c = McpClient::connect("old", &cfg).await.unwrap();
    assert_eq!(c.protocol_version, "2024-11-05");
    let r = c.call_tool("random_gif", json!({})).await.unwrap();
    assert!(rooftop::mcp::result_to_text(&r).contains("old ok"));
}

// ---------- stdio (legacy server, discovery probe falls back) ----------
const STDIO_SERVER: &str = r#"
import sys, json
for line in sys.stdin:
    m = json.loads(line)
    mid = m.get("id"); meth = m.get("method")
    if mid is None: continue
    if meth == "server/discover":
        r = {"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "Method not found"}}
    elif meth == "initialize":
        r = {"jsonrpc": "2.0", "id": mid, "result": {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}}, "serverInfo": {"name": "py", "version": "1"}}}
    elif meth == "tools/list":
        r = {"jsonrpc": "2.0", "id": mid, "result": {"tools": [{"name": "echo", "inputSchema": {"type": "object", "properties": {"s": {"type": "string"}}}}]}}
    elif meth == "tools/call":
        r = {"jsonrpc": "2.0", "id": mid, "result": {"content": [{"type": "text", "text": "echo:" + m["params"]["arguments"]["s"]}]}}
    else:
        r = {"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "nope"}}
    sys.stdout.write(json.dumps(r) + "\n"); sys.stdout.flush()
"#;

#[tokio::test]
async fn stdio_with_discovery_fallback() {
    if std::process::Command::new("python3").arg("--version").output().is_err() {
        eprintln!("python3 missing, skipping");
        return;
    }
    let cfg =
        ServerConfig { command: Some("python3".into()), args: vec!["-I".into(), "-c".into(), STDIO_SERVER.into()], ..Default::default() };
    let mut cfgs = rooftop::mcp::McpConfig::new();
    cfgs.insert("py echo".into(), cfg);
    let mut m = McpManager::new(cfgs);
    m.ensure_connected().await;
    let st = m.status();
    assert!(st[0].connected, "{:?}", st[0].error);
    assert_eq!(st[0].protocol_version.as_deref(), Some("2025-11-25"));
    let defs = m.tool_definitions();
    let fname = defs[0]["function"]["name"].as_str().unwrap().to_string();
    assert_eq!(fname, "mcp_py_echo_echo");
    let (client, tool) = m.resolve(&fname).unwrap();
    let r = client.call_tool(&tool, json!({"s": "hi"})).await.unwrap();
    assert!(rooftop::mcp::result_to_text(&r).contains("echo:hi"));
}
