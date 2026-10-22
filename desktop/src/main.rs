//! iRoof.top desktop shell: starts the embedded `rooftop` HTTP server on a random
//! localhost port and shows it in a Tauri (wry) webview window. The UI only uses
//! `fetch` against that server, so no Tauri IPC / capabilities are required.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::{Context, Result};
use rooftop::llm::LlmConfig;
use rooftop::server::{self, ServerOptions};
use rooftop::state::AppState;
use rooftop::store::Paths;
use std::net::SocketAddr;
use tauri::ipc::CapabilityBuilder;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

const MAX_UPLOAD_BYTES: usize = 2048 * 1024 * 1024;

/// Load the data dir, bind 127.0.0.1:0 and serve in the background. Returns the bound address.
fn start_server(data_dir: std::path::PathBuf) -> Result<SocketAddr> {
    std::fs::create_dir_all(&data_dir).with_context(|| format!("creating {}", data_dir.display()))?;
    let paths = Paths::new(data_dir.clone());
    let db = paths.load().with_context(|| format!("loading data dir {}", data_dir.display()))?;
    rooftop::llm::ensure_crypto_provider();
    let state = AppState::new(paths, db, LlmConfig::from_env());
    let app = server::router(state.clone(), ServerOptions { password: None, max_upload_bytes: MAX_UPLOAD_BYTES, ..Default::default() });
    tauri::async_runtime::block_on(async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.context("binding 127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        tauri::async_runtime::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                eprintln!("server error: {e}");
            }
        });
        // Share links on the Wi-Fi and at <name>.iroof.top (if turned on), and the Telegram bot.
        rooftop::sharing::init(&state, addr).await;
        tauri::async_runtime::spawn(async move {
            let _g = state.control.lock().await;
            if let Err(e) = rooftop::tgbot::apply(&state).await {
                eprintln!("Telegram: {e:#}");
            }
        });
        Ok(addr)
    })
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
                .initialization_script("window.__IROOF_SHELL__ = { platform: 'macos', titlebar: 'overlay' };");
            builder.build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running iRoof.top");
}
