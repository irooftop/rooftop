//! iRoof.top desktop shell: starts the embedded `rooftop` HTTP server on a random
//! localhost port and shows it in a Tauri (wry) webview window. The UI talks to that
//! server with `fetch`; the only Tauri IPC it uses is a few window permissions.
//!
//! Exports are imported from their paths, not uploaded by the webview: on macOS WebKit's
//! sandboxed networking process is refused some of the files the user picks, so a
//! webview upload arrives empty or fails with `NotReadableError`. The shell shows the
//! native picker (for `/api/local/pick`) and hands dropped paths to the page, which
//! imports them with `/api/local/import`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::{Context, Result};
use rooftop::llm::LlmConfig;
use rooftop::server::{self, Desktop, Pick, ServerOptions};
use rooftop::state::AppState;
use rooftop::store::Paths;
use std::net::SocketAddr;
use tauri::ipc::CapabilityBuilder;
use tauri::{DragDropEvent, Manager, WebviewEvent, WebviewUrl, WebviewWindowBuilder};

/// Load the data dir, bind 127.0.0.1:0 and serve in the background. Returns the bound address.
fn start_server(data_dir: std::path::PathBuf) -> Result<SocketAddr> {
    std::fs::create_dir_all(&data_dir).with_context(|| format!("creating {}", data_dir.display()))?;
    let paths = Paths::new(data_dir.clone());
    let db = paths.load().with_context(|| format!("loading data dir {}", data_dir.display()))?;
    rooftop::llm::ensure_crypto_provider();
    let state = AppState::new(paths, db, LlmConfig::from_env());
    let desktop = Desktop { pick: std::sync::Arc::new(|kind| Box::pin(pick(kind))) };
    let app = server::router(state.clone(), ServerOptions { password: None, desktop: Some(desktop), ..Default::default() });
    tauri::async_runtime::block_on(async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.context("binding 127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        tauri::async_runtime::spawn(async move {
            if let Err(e) = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await {
                eprintln!("server error: {e}");
            }
        });
        // Share links on the Wi-Fi and at <name>.iroof.top (if turned on), and the Telegram bot.
        rooftop::sharing::init(&state, addr).await;
        // Media sizes for exports imported before 0.5.3, so the chat doesn't jump.
        tauri::async_runtime::spawn(rooftop::imgsize::backfill(state.clone()));
        tauri::async_runtime::spawn(async move {
            let _g = state.control.lock().await;
            if let Err(e) = rooftop::tgbot::apply(&state).await {
                eprintln!("Telegram: {e:#}");
            }
        });
        Ok(addr)
    })
}

/// The native picker for an export: its folder, or a `.zip` / WhatsApp `.txt`.
async fn pick(kind: Pick) -> Option<std::path::PathBuf> {
    let downloads = std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join("Downloads")).filter(|d| d.is_dir());
    let mut dialog = rfd::AsyncFileDialog::new();
    if let Some(d) = downloads {
        dialog = dialog.set_directory(d);
    }
    let picked = match kind {
        Pick::Folder => dialog.set_title("Choose the export folder").pick_folder().await,
        Pick::File => dialog.set_title("Choose the export").add_filter("Chat export", &["zip", "txt"]).pick_file().await,
    };
    picked.map(|h| h.path().to_path_buf())
}

/// Native file drags, for the page's `window.__IROOF_DROP__(type, x, y, paths)`: the
/// webview's own drop events carry files it may not be able to read, and no paths.
fn forward_drag(webview: &tauri::WebviewWindow, event: &DragDropEvent) {
    let (kind, pos, paths) = match event {
        DragDropEvent::Enter { paths, position } => ("enter", Some(*position), Some(paths)),
        DragDropEvent::Over { position } => ("over", Some(*position), None),
        DragDropEvent::Drop { paths, position } => ("drop", Some(*position), Some(paths)),
        DragDropEvent::Leave => ("leave", None, None),
        _ => return,
    };
    // Physical pixels from the webview's top left corner; the page converts to CSS pixels.
    let (x, y) = pos.map_or((-1.0, -1.0), |p| (p.x, p.y));
    let paths: Vec<String> = paths.into_iter().flatten().map(|p| p.display().to_string()).collect();
    let paths = paths.iter().map(|p| json_string(p)).collect::<Vec<_>>().join(",");
    let _ = webview.eval(format!("window.__IROOF_DROP__?.(\"{kind}\", {x}, {y}, [{paths}])"));
}

/// `s` as a JavaScript string literal.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Opens a link in the default browser (or Telegram, for `tg:` links).
fn open_external(url: &str) {
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).spawn();
    if let Err(e) = r {
        eprintln!("opening {url}: {e}");
    }
}

fn main() {
    tauri::Builder::default()
        .runtime(tauri_runtime_wry::Wry::default())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let addr = start_server(dir.clone())?;
            eprintln!("iRoof.top listening on http://{addr}/ (data: {})", dir.display());
            let url = format!("http://{addr}/").parse().map_err(|e| anyhow::anyhow!("{e}"))?;
            // The UI is served from this local origin, so let it (and only it) drag the
            // window, zoom on double click and match the window theme to the app's.
            app.add_capability(
                CapabilityBuilder::new("local-ui")
                    .remote(format!("http://{addr}/*"))
                    .window("main")
                    .permission("core:window:allow-start-dragging")
                    .permission("core:window:allow-internal-toggle-maximize")
                    .permission("core:window:allow-set-theme"),
            )?;
            let origin = format!("http://{addr}/");
            let builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                // Links to other sites (t.me, share links on the network) open in the browser.
                .on_new_window(move |url, _| {
                    let external = matches!(url.scheme(), "http" | "https" | "tg") && !url.as_str().starts_with(&origin);
                    if external {
                        open_external(url.as_str());
                        tauri::webview::NewWindowResponse::Deny
                    } else {
                        tauri::webview::NewWindowResponse::Allow
                    }
                })
                .title("iRoof.top")
                .inner_size(1280.0, 840.0)
                .min_inner_size(640.0, 480.0);
            // macOS: no title bar, the app's own headers sit under the traffic lights.
            #[cfg(target_os = "macos")]
            let builder = builder
                .title_bar_style(tauri::TitleBarStyle::Overlay)
                .hidden_title(true)
                .traffic_light_position(tauri::LogicalPosition::new(18.0, 24.0))
                .initialization_script("window.__IROOF_SHELL__ = { platform: 'macos', titlebar: 'overlay', files: 'native' };");
            #[cfg(not(target_os = "macos"))]
            let builder = builder.initialization_script("window.__IROOF_SHELL__ = { files: 'native' };");
            let window = builder.build()?;
            let webview = window.clone();
            window.on_webview_event(move |e| {
                if let WebviewEvent::DragDrop(d) = e {
                    forward_drag(&webview, d);
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running iRoof.top");
}
