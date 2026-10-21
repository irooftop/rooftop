//! JSON-file persistence under the data directory:
//!
//! ```text
//! data/settings.json
//! data/exports/<id>/export.json   parsed export + media library
//! data/exports/<id>/files/…       referenced media (original relative paths)
//! data/chats/<id>.json            chat metadata + live events
//! data/shares/<id>.json           share links + visitor sessions
//! ```

use crate::chat::Chat;
use crate::llm::LlmConfig;
use crate::mcp::McpConfig;
use crate::model::ChatExport;
use crate::share::Share;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportRecord {
    pub id: String,
    pub imported_at: String,
    /// Content fingerprint used to skip duplicate imports.
    #[serde(default)]
    pub fingerprint: String,
    pub export: ChatExport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Overrides for the env-provided LLM config, and empty fields fall back to env.
    pub llm: LlmConfig,
    pub history_budget_chars: usize,
    pub max_tool_steps: usize,
    /// Model for image descriptions and voice transcripts (must accept images /
    /// audio), empty uses `OPENAI_MEDIA_MODEL`, then the chat model.
    pub media_model: String,
    #[serde(rename = "mcpServers")]
    pub mcp_servers: McpConfig,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            llm: LlmConfig::default(),
            history_budget_chars: 120_000,
            max_tool_steps: 6,
            media_model: String::new(),
            mcp_servers: McpConfig::new(),
        }
    }
}

#[derive(Default)]
pub struct Db {
    pub exports: BTreeMap<String, Arc<ExportRecord>>,
    pub chats: BTreeMap<String, Chat>,
    pub shares: BTreeMap<String, Share>,
    pub settings: Settings,
}

pub struct Paths {
    pub root: PathBuf,
}

fn write_json_atomic(path: &Path, value: &impl Serialize, pretty: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    let bytes = if pretty { serde_json::to_vec_pretty(value)? } else { serde_json::to_vec(value)? };
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

impl Paths {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    pub fn settings(&self) -> PathBuf {
        self.root.join("settings.json")
    }
    pub fn export_dir(&self, id: &str) -> PathBuf {
        self.root.join("exports").join(id)
    }
    pub fn export_files(&self, id: &str) -> PathBuf {
        self.export_dir(id).join("files")
    }
    pub fn chat(&self, id: &str) -> PathBuf {
        self.root.join("chats").join(format!("{id}.json"))
    }
    pub fn share(&self, id: &str) -> PathBuf {
        self.root.join("shares").join(format!("{id}.json"))
    }
    pub fn tmp(&self) -> PathBuf {
        self.root.join("tmp")
    }

    pub fn load(&self) -> Result<Db> {
        std::fs::create_dir_all(self.root.join("exports"))?;
        std::fs::create_dir_all(self.root.join("chats"))?;
        std::fs::create_dir_all(self.root.join("shares"))?;
        let _ = std::fs::remove_dir_all(self.tmp());
        let mut db = Db::default();
        if let Ok(bytes) = std::fs::read(self.settings()) {
            db.settings = serde_json::from_slice(&bytes).context("parsing settings.json")?;
        }
        for e in std::fs::read_dir(self.root.join("exports"))?.flatten() {
            let p = e.path().join("export.json");
            match std::fs::read(&p).map_err(anyhow::Error::from).and_then(|b| Ok(serde_json::from_slice::<ExportRecord>(&b)?)) {
                Ok(rec) => {
                    db.exports.insert(rec.id.clone(), Arc::new(rec));
                }
                Err(err) => tracing::warn!("skipping {}: {err:#}", p.display()),
            }
        }
        for e in std::fs::read_dir(self.root.join("chats"))?.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            match std::fs::read(&p).map_err(anyhow::Error::from).and_then(|b| Ok(serde_json::from_slice::<Chat>(&b)?)) {
                Ok(c) => {
                    db.chats.insert(c.id.clone(), c);
                }
                Err(err) => tracing::warn!("skipping {}: {err:#}", p.display()),
            }
        }
        for e in std::fs::read_dir(self.root.join("shares"))?.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            match std::fs::read(&p).map_err(anyhow::Error::from).and_then(|b| Ok(serde_json::from_slice::<Share>(&b)?)) {
                Ok(s) => {
                    db.shares.insert(s.id.clone(), s);
                }
                Err(err) => tracing::warn!("skipping {}: {err:#}", p.display()),
            }
        }
        Ok(db)
    }

    pub fn save_settings(&self, s: &Settings) -> Result<()> {
        write_json_atomic(&self.settings(), s, true)
    }
    pub fn save_export(&self, rec: &ExportRecord) -> Result<()> {
        write_json_atomic(&self.export_dir(&rec.id).join("export.json"), rec, false)
    }
    pub fn save_chat(&self, chat: &Chat) -> Result<()> {
        write_json_atomic(&self.chat(&chat.id), chat, false)
    }
    /// Deletes a chat file and the media created in that chat (`files/generated/<id>/`).
    pub fn delete_chat(&self, id: &str) -> Result<()> {
        let p = self.chat(id);
        if p.exists() {
            std::fs::remove_file(p)?;
        }
        if !id.is_empty() && !id.contains(['/', '\\', '.']) {
            for e in std::fs::read_dir(self.root.join("exports")).into_iter().flatten().flatten() {
                let dir = e.path().join("files").join("generated").join(id);
                if dir.is_dir() {
                    std::fs::remove_dir_all(dir)?;
                }
            }
        }
        Ok(())
    }
    pub fn save_share(&self, share: &Share) -> Result<()> {
        write_json_atomic(&self.share(&share.id), share, true)
    }
    pub fn delete_share(&self, id: &str) -> Result<()> {
        let p = self.share(id);
        if p.exists() {
            std::fs::remove_file(p)?;
        }
        Ok(())
    }
    pub fn delete_export(&self, id: &str) -> Result<()> {
        let p = self.export_dir(id);
        if p.exists() {
            std::fs::remove_dir_all(p)?;
        }
        Ok(())
    }
}
