use crate::chat::{Chat, Event, Memory, Usage};
use crate::llm::{LlmClient, LlmConfig};
use crate::mcp::McpManager;
use crate::store::{Db, ExportRecord, Paths};
use anyhow::{Result, anyhow};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

pub struct AppState {
    pub paths: Paths,
    pub db: RwLock<Db>,
    pub llm: LlmClient,
    /// Defaults from the environment (`OPENAI_BASE_URL`, `OPENAI_API_KEY`, `OPENAI_MODEL`).
    pub env_llm: LlmConfig,
    pub mcp: Mutex<McpManager>,
    gen_locks: std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Serializes imports (they are disk heavy).
    pub import_lock: Mutex<()>,
    /// Imports started by uploads, for progress polling.
    pub imports: crate::imports::Jobs,
    /// PIN brute-force protection and guest rate limits.
    pub limiter: std::sync::Mutex<crate::share::Limiter>,
    /// Telegram relay for share links, while a bot is configured.
    pub telegram: std::sync::RwLock<Option<Arc<crate::tgbot::Bot>>>,
    /// Bot from `TELEGRAM_BOT_TOKEN`: wins over the bot chosen in Settings.
    pub telegram_env: std::sync::OnceLock<crate::tgbot::BotConfig>,
    /// Local network / internet sharing, when the owner UI runs on this device.
    pub sharing: std::sync::OnceLock<Arc<crate::sharing::Sharing>>,
    /// Serializes reconfiguration of the bot and of sharing.
    /// Offline mode (effective): shared with the LLM client.
    offline: Arc<std::sync::atomic::AtomicBool>,
    /// Offline mode forced by `--offline` / `ROOFTOP_OFFLINE`: Settings can't turn it off.
    offline_forced: std::sync::atomic::AtomicBool,
    pub control: Mutex<()>,
}

pub fn now_local() -> chrono::NaiveDateTime {
    chrono::Local::now().naive_local()
}

pub fn now_string() -> String {
    now_local().format("%Y-%m-%dT%H:%M:%S").to_string()
}

impl AppState {
    pub fn new(paths: Paths, db: Db, env_llm: LlmConfig) -> Arc<Self> {
        let llm = LlmClient::new();
        let forced = crate::llm::offline_from_env();
        let offline = llm.offline_flag();
        offline.store(forced || db.settings.offline, std::sync::atomic::Ordering::Relaxed);
        let mcp = McpManager::new(db.settings.mcp_servers.clone()).with_workspace(paths.root.join("mcp")).with_offline(offline.clone());
        Arc::new(Self {
            paths,
            db: RwLock::new(db),
            llm,
            env_llm,
            mcp: Mutex::new(mcp),
            gen_locks: Default::default(),
            import_lock: Mutex::new(()),
            imports: Default::default(),
            limiter: Default::default(),
            telegram: Default::default(),
            telegram_env: Default::default(),
            sharing: Default::default(),
            control: Mutex::new(()),
            offline,
            offline_forced: forced.into(),
        })
    }

    /// A fresh MCP manager for `config` (stdio servers run in the data dir; remote ones obey Offline mode).
    pub fn mcp_manager(&self, config: crate::mcp::McpConfig) -> McpManager {
        McpManager::new(config).with_workspace(self.paths.root.join("mcp")).with_offline(self.offline.clone())
    }

    /// Whether Offline mode is on: only local models and this network, no relay, no Telegram.
    pub fn is_offline(&self) -> bool {
        self.offline.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Whether Offline mode comes from `--offline` / `ROOFTOP_OFFLINE` (Settings can't turn it off).
    pub fn offline_forced(&self) -> bool {
        self.offline_forced.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Turns Offline mode on regardless of Settings (`--offline`).
    pub fn force_offline(&self) {
        self.offline_forced.store(true, std::sync::atomic::Ordering::Relaxed);
        self.offline.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Applies the saved setting (and the forced switch) to the effective state.
    pub fn set_offline_setting(&self, on: bool) {
        self.offline.store(on || self.offline_forced(), std::sync::atomic::Ordering::Relaxed);
    }

    pub async fn llm_config(&self) -> LlmConfig {
        let s = self.db.read().await.settings.llm.clone();
        let pick = |a: String, b: &String| if a.trim().is_empty() { b.clone() } else { a };
        LlmConfig {
            base_url: pick(s.base_url, &self.env_llm.base_url),
            api_key: pick(s.api_key, &self.env_llm.api_key),
            model: pick(s.model, &self.env_llm.model),
            temperature: s.temperature.or(self.env_llm.temperature),
        }
    }

    /// LLM config for describing images and transcribing voice messages.
    pub async fn media_llm_config(&self) -> LlmConfig {
        let mut cfg = self.llm_config().await;
        let configured = self.db.read().await.settings.media_model.trim().to_string();
        let model = Some(configured)
            .filter(|m| !m.is_empty())
            .or_else(|| std::env::var("OPENAI_MEDIA_MODEL").ok().map(|m| m.trim().to_string()).filter(|m| !m.is_empty()));
        if let Some(m) = model {
            cfg.model = m;
        }
        cfg
    }

    pub fn gen_lock(&self, chat_id: &str) -> Arc<Mutex<()>> {
        self.gen_locks.lock().expect("lock").entry(chat_id.to_string()).or_default().clone()
    }

    pub async fn snapshot(&self, chat_id: &str) -> Result<(Arc<ExportRecord>, Chat)> {
        let db = self.db.read().await;
        let chat = db.chats.get(chat_id).cloned().ok_or_else(|| anyhow!("chat not found"))?;
        let export = db.exports.get(&chat.export_id).cloned().ok_or_else(|| anyhow!("export of this chat was deleted"))?;
        Ok((export, chat))
    }

    /// Reserves `n` message ids in a chat.
    pub async fn alloc_ids(&self, chat_id: &str, n: i64) -> Result<i64> {
        let mut db = self.db.write().await;
        let chat = db.chats.get_mut(chat_id).ok_or_else(|| anyhow!("chat not found"))?;
        let first = chat.next_id;
        chat.next_id += n;
        Ok(first)
    }

    /// Stores the history compaction state of a chat.
    pub async fn set_memory(&self, chat_id: &str, memory: Memory) -> Result<()> {
        let mut db = self.db.write().await;
        let chat = db.chats.get_mut(chat_id).ok_or_else(|| anyhow!("chat not found"))?;
        chat.memory = Some(memory);
        self.paths.save_chat(chat)
    }

    /// Remembers the provider-reported token usage of the last request.
    pub async fn set_usage(&self, chat_id: &str, usage: Usage) -> Result<()> {
        let mut db = self.db.write().await;
        let chat = db.chats.get_mut(chat_id).ok_or_else(|| anyhow!("chat not found"))?;
        chat.last_usage = Some(usage);
        self.paths.save_chat(chat)
    }

    /// Registers media created by tools (`gen_…`) in a chat.
    pub async fn add_generated(&self, chat_id: &str, items: Vec<(String, crate::model::Media)>) -> Result<()> {
        let mut db = self.db.write().await;
        let chat = db.chats.get_mut(chat_id).ok_or_else(|| anyhow!("chat not found"))?;
        chat.generated.extend(items);
        self.paths.save_chat(chat)
    }

    /// Appends events and persists the chat.
    pub async fn append_events(&self, chat_id: &str, events: &[Event]) -> Result<()> {
        let mut db = self.db.write().await;
        let chat = db.chats.get_mut(chat_id).ok_or_else(|| anyhow!("chat not found"))?;
        chat.events.extend_from_slice(events);
        for e in events {
            if let Event::Message(m) = e {
                chat.next_id = chat.next_id.max(m.id + 1);
            }
        }
        chat.updated_at = now_string();
        self.paths.save_chat(chat)
    }
}
