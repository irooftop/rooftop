use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rooftop::llm::LlmConfig;
use rooftop::prompt::{self, PromptOptions};
use rooftop::server::{self, ServerOptions};
use rooftop::state::AppState;
use rooftop::store::Paths;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "rooftop",
    version,
    about = "iRoof.top — chat with people dear to you, in their own style, from a Telegram or WhatsApp export"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    serve: ServeArgs,
}

#[derive(clap::Args, Clone)]
struct ServeArgs {
    /// Listen address.
    #[arg(long, env = "ROOFTOP_ADDR", default_value = "127.0.0.1:8080")]
    addr: String,
    /// Data directory (imported exports, chats, settings).
    #[arg(long, env = "ROOFTOP_DATA", default_value = "./data")]
    data: PathBuf,
    /// Telegram or WhatsApp exports (a folder, or a WhatsApp .txt) to import on start (idempotent). Separate several with ':' in the env var.
    #[arg(long = "import", env = "ROOFTOP_IMPORT", value_delimiter = ':')]
    import: Vec<PathBuf>,
    /// Require HTTP Basic auth with this password.
    #[arg(long, env = "ROOFTOP_PASSWORD", hide_env_values = true)]
    password: Option<String>,
    /// Maximum upload size in MiB.
    #[arg(long, env = "ROOFTOP_MAX_UPLOAD_MB", default_value_t = 2048)]
    max_upload_mb: usize,
    /// Trust `X-Forwarded-For` / `X-Real-IP` for the client IP (PIN throttling). Only behind a reverse proxy.
    #[arg(long, env = "ROOFTOP_TRUST_PROXY", default_value_t = false, action = clap::ArgAction::Set, num_args = 0..=1, default_missing_value = "true")]
    trust_proxy: bool,
    /// Telegram bot token: share links can relay visitor messages to a Telegram
    /// group, where you answer yourself or let the AI answer.
    #[arg(long, env = "TELEGRAM_BOT_TOKEN", hide_env_values = true)]
    telegram_token: Option<String>,
    /// Telegram group id(s) the relay may use, comma-separated.
    /// Default: every group the bot is added to.
    #[arg(long, env = "TELEGRAM_CHAT_ID", value_delimiter = ',', allow_negative_numbers = true)]
    telegram_chat: Vec<i64>,
    /// Telegram Bot API base URL (e.g. a self-hosted Bot API server).
    #[arg(long, env = "TELEGRAM_API_URL", default_value = "https://api.telegram.org")]
    telegram_api: String,
}

#[derive(Subcommand)]
enum Command {
    /// Run the web server (default).
    Serve(ServeArgs),
    /// Print the parsed, normalized export as JSON.
    Parse { dir: PathBuf },
    /// Print the OpenAI `messages` array (+ tools) for an export.
    Convert {
        dir: PathBuf,
        /// Participant the LLM should speak as (default: the chat title / other person).
        #[arg(long)]
        persona: Option<String>,
        /// Participant the human plays (default: the other participant).
        #[arg(long)]
        user: Option<String>,
        /// History budget in characters.
        #[arg(long, default_value_t = 120_000)]
        budget: usize,
    },
}

fn env_llm() -> LlmConfig {
    LlmConfig {
        base_url: std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://openrouter.ai/api/v1".into()),
        api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
        model: std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "openai/gpt-6-luna".into()),
        temperature: std::env::var("OPENAI_TEMPERATURE").ok().and_then(|t| t.parse().ok()),
    }
}

fn default_persona(e: &rooftop::model::ChatExport) -> Option<String> {
    e.participants
        .iter()
        .find(|p| p.name == e.title)
        .or_else(|| e.participants.iter().find(|p| Some(p.name.as_str()) != e.exporter()))
        .map(|p| p.name.clone())
}

/// The IPv4 address other devices on the local network can open: Wi-Fi /
/// Ethernet before VPN tunnels and virtual bridges, private ranges first.
fn lan_ip() -> Option<std::net::Ipv4Addr> {
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

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    rooftop::llm::ensure_crypto_provider();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,rooftop=info".into()))
        .init();
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Parse { dir }) => {
            let e = rooftop::import::import_dir(&dir, None)?;
            println!("{}", serde_json::to_string_pretty(&e)?);
            Ok(())
        }
        Some(Command::Convert { dir, persona, user, budget }) => {
            let e = rooftop::import::import_dir(&dir, None)?;
            let persona = persona.or_else(|| default_persona(&e)).context("cannot determine persona, pass --persona")?;
            let rec = rooftop::store::ExportRecord { id: "cli".into(), imported_at: String::new(), fingerprint: String::new(), export: e };
            let chat =
                server::make_chat(&rec, &persona, user.as_deref(), None, "").map_err(|_| anyhow::anyhow!("invalid --persona/--user"))?;
            let p = prompt::build(&rec.export, &chat, &PromptOptions { budget_chars: budget, ..Default::default() });
            let out = serde_json::json!({"model": env_llm().model, "messages": p.messages, "tools": rooftop::tools::definitions()});
            println!("{}", serde_json::to_string_pretty(&out)?);
            Ok(())
        }
        Some(Command::Serve(args)) => serve(args).await,
        None => serve(cli.serve).await,
    }
}

async fn serve(args: ServeArgs) -> Result<()> {
    let paths = Paths::new(args.data.clone());
    let db = paths.load().with_context(|| format!("loading data dir {}", args.data.display()))?;
    let state = AppState::new(paths, db, env_llm());
    if let Some(token) = args.telegram_token.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        let cfg = rooftop::tgbot::BotConfig {
            token: token.to_string(),
            api_base: args.telegram_api.clone(),
            allowed_chats: args.telegram_chat.clone(),
        };
        rooftop::tgbot::start(&state, cfg)?;
    }
    for dir in args.import.iter().filter(|d| !d.as_os_str().is_empty()) {
        match server::import_into_store(&state, dir.clone()).await {
            Ok((rec, created)) => {
                let has_chat = state.db.read().await.chats.values().any(|c| c.export_id == rec.id);
                if !has_chat
                    && let Some(persona) = default_persona(&rec.export)
                    && let Ok(chat) = server::make_chat(&rec, &persona, None, None, "")
                {
                    state.paths.save_chat(&chat)?;
                    state.db.write().await.chats.insert(chat.id.clone(), chat);
                }
                tracing::info!(
                    "{} export `{}` from {}",
                    if created { "imported" } else { "already have" },
                    rec.export.title,
                    dir.display()
                );
            }
            Err(e) => tracing::error!("import of {} failed: {e:#}", dir.display()),
        }
    }
    let app = server::router(
        state,
        ServerOptions {
            password: args.password,
            max_upload_bytes: args.max_upload_mb.saturating_mul(1024 * 1024),
            trust_proxy: args.trust_proxy,
        },
    );
    let listener = tokio::net::TcpListener::bind(&args.addr).await.with_context(|| format!("binding {}", args.addr))?;
    let bound = listener.local_addr().with_context(|| format!("binding {}", args.addr))?;
    if bound.ip().is_unspecified() {
        // All interfaces: show the address other devices on the network (Wi-Fi/LAN) can open.
        let ip = lan_ip().map_or(std::net::IpAddr::from([127, 0, 0, 1]), std::net::IpAddr::V4);
        tracing::info!("iRoof.top listening on http://{}", std::net::SocketAddr::new(ip, bound.port()));
    } else {
        tracing::info!("iRoof.top listening on http://{bound}");
    }
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .with_graceful_shutdown(async {
            let ctrl_c = tokio::signal::ctrl_c();
            #[cfg(unix)]
            {
                let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal");
                tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
            }
            #[cfg(not(unix))]
            let _ = ctrl_c.await;
        })
        .await?;
    Ok(())
}
