//! One persona turn: prompt → LLM → tool calls → chat events.

use crate::chat::Memory;
use crate::chat::{Chat, Event, find_message};
use crate::model::{ChatExport, Media, MediaKind, Message};
use crate::prompt::{self, PromptOptions};
use crate::state::{AppState, now_string};
use crate::tools;
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::sync::Arc;

pub struct TurnOutcome {
    pub events: Vec<Event>,
    pub error: Option<String>,
    pub usage: Option<crate::chat::Usage>,
}

struct Ctx<'a> {
    state: &'a AppState,
    export: &'a ChatExport,
    chat: Chat,
    events: Vec<Event>,
    /// Media created by tools in this turn (persisted with the chat).
    generated: Vec<(String, Media)>,
    media_tools: crate::mcp::MediaTools,
}

/// MCP arguments without `null` / empty-string values: some models fill every
/// optional field with placeholders, which servers then reject.
fn clean_args(args: &Value) -> Value {
    match args {
        Value::Object(m) => {
            Value::Object(m.iter().filter(|(_, v)| !v.is_null() && v.as_str() != Some("")).map(|(k, v)| (k.clone(), v.clone())).collect())
        }
        other => other.clone(),
    }
}

fn arg_str<'v>(args: &'v Value, key: &str) -> Option<&'v str> {
    args.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

fn arg_id(args: &Value, key: &str) -> Option<i64> {
    match args.get(key) {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().trim_start_matches('#').parse().ok(),
        _ => None,
    }
}

impl Ctx<'_> {
    fn find(&self, id: i64) -> Option<&Message> {
        find_message(self.export, &self.chat.events, id)
    }

    async fn new_message(&mut self, args: &Value) -> Result<Message> {
        let id = self.state.alloc_ids(&self.chat.id, 1).await?;
        let reply_to = arg_id(args, "reply_to").filter(|r| self.find(*r).is_some());
        // A quote must be a real fragment of the replied message, quoting the whole
        // text is just a plain reply (Telegram shows no quote block for it).
        let quote = arg_str(args, "quote")
            .filter(|q| reply_to.and_then(|r| self.find(r)).is_some_and(|t| t.text.contains(q) && t.text.trim() != *q))
            .map(String::from);
        Ok(Message { id, from: self.chat.persona.clone(), date: Some(now_string()), reply_to, quote, ..Default::default() })
    }

    fn push(&mut self, e: Event) {
        self.chat.events.push(e.clone());
        self.events.push(e);
    }

    /// Stores a file produced by a tool as a `gen_…` media item of this chat.
    async fn store_generated(&mut self, cap: crate::genmedia::Captured, description: Option<String>) -> Result<(String, MediaKind)> {
        let id = format!("gen_{}", crate::share::random_id(10));
        let rel = format!("generated/{}/{id}.{}", self.chat.id, cap.ext);
        let path = self.state.paths.export_files(&self.chat.export_id).join(&rel);
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        tokio::fs::write(&path, &cap.bytes).await?;
        let media = Media {
            kind: Some(cap.kind),
            library_id: Some(id.clone()),
            file: Some(rel),
            status: crate::genmedia::wav_duration(&cap.bytes),
            description,
            size: crate::imgsize::parse(&cap.bytes).map(|(w, h)| [w, h]),
            ..Default::default()
        };
        self.chat.generated.insert(id.clone(), media.clone());
        self.generated.push((id.clone(), media));
        Ok((id, cap.kind))
    }

    fn library_media(&self, id: &str, kinds: &[MediaKind]) -> Result<Media> {
        if let Some(m) = self.chat.generated.get(id) {
            return Ok(m.clone());
        }
        let item = self
            .export
            .library_item(id)
            .filter(|i| kinds.contains(&i.kind))
            .ok_or_else(|| anyhow!("unknown id `{id}` for this tool, use one of the ids listed in the system prompt"))?;
        Ok(Media {
            kind: Some(item.kind),
            library_id: Some(item.id.clone()),
            file: item.file.clone(),
            thumb: item.thumb.clone(),
            url: None,
            title: item.title.clone(),
            status: None,
            description: None,
            size: item.size,
        })
    }

    /// Runs the MCP tool behind `send_voice` / `send_photo` and stores its output.
    async fn create_media(&mut self, fname: Option<String>, args: Value, want: &[MediaKind], description: String) -> Result<Media> {
        let fname = fname.ok_or_else(|| anyhow!("no MCP server for this is connected"))?;
        let (client, tool) = self.state.mcp.lock().await.resolve(&fname).ok_or_else(|| anyhow!("the media tool is not available"))?;
        tracing::info!(chat = %self.chat.id, server = %client.name, tool = %tool, args = %crate::llm::truncate(&args.to_string(), 300), "media tool call");
        let r = client.call_tool(&tool, args).await?;
        if r.get("isError").and_then(Value::as_bool) == Some(true) {
            return Err(anyhow!("{}", crate::mcp::result_to_text(&r)));
        }
        let cap = crate::genmedia::collect(&r, client.workspace.as_deref())
            .into_iter()
            .find(|c| want.contains(&c.kind))
            .ok_or_else(|| anyhow!("the tool returned no usable file: {}", crate::llm::truncate(&crate::mcp::result_to_text(&r), 400)))?;
        let (id, _) = self.store_generated(cap, Some(description)).await?;
        Ok(self.chat.generated.get(&id).cloned().expect("just stored"))
    }

    async fn action(&mut self, name: &str, args: &Value) -> Result<Value> {
        match name {
            tools::SEND_VOICE => {
                let text = arg_str(args, "text").ok_or_else(|| anyhow!("`text` is required"))?.to_string();
                let file = format!("voice-{}", crate::share::random_id(8));
                let mut call = json!({"text": text, "output": format!("{file}.wav")});
                if !self.chat.voice.trim().is_empty() {
                    call["voice"] = json!(self.chat.voice.trim());
                }
                let fname = self.media_tools.speech.clone();
                let media = match self.create_media(fname.clone(), call.clone(), &[MediaKind::Voice], text.clone()).await {
                    Ok(m) => m,
                    // Some providers only produce mp3.
                    Err(e) => {
                        tracing::warn!(chat = %self.chat.id, "wav speech failed, retrying as mp3: {e:#}");
                        call["output"] = json!(format!("{file}.mp3"));
                        self.create_media(fname, call, &[MediaKind::Voice], text).await?
                    }
                };
                let mut m = self.new_message(args).await?;
                m.media = Some(media);
                let id = m.id;
                self.push(Event::Message(m));
                Ok(json!({"ok": true, "message_id": id}))
            }
            tools::SEND_PHOTO => {
                let prompt = arg_str(args, "prompt").ok_or_else(|| anyhow!("`prompt` is required"))?.to_string();
                let call = json!({"prompt": prompt, "output": format!("photo-{}.jpg", crate::share::random_id(8))});
                let media = self.create_media(self.media_tools.image.clone(), call, &[MediaKind::Photo, MediaKind::Gif], prompt).await?;
                let mut m = self.new_message(args).await?;
                m.media = Some(media);
                m.text = arg_str(args, "caption").unwrap_or_default().to_string();
                let id = m.id;
                self.push(Event::Message(m));
                Ok(json!({"ok": true, "message_id": id}))
            }
            tools::SEND_MESSAGE => {
                let text = arg_str(args, "text").ok_or_else(|| anyhow!("`text` is required"))?.to_string();
                let mut m = self.new_message(args).await?;
                m.text = text;
                let id = m.id;
                self.push(Event::Message(m));
                Ok(json!({"ok": true, "message_id": id}))
            }
            tools::SEND_STICKER => {
                let sid = arg_str(args, "sticker_id").ok_or_else(|| anyhow!("`sticker_id` is required"))?;
                let media = self.library_media(sid, &[MediaKind::Sticker])?;
                let mut m = self.new_message(args).await?;
                m.media = Some(media);
                let id = m.id;
                self.push(Event::Message(m));
                Ok(json!({"ok": true, "message_id": id}))
            }
            tools::SEND_GIF => {
                let media = if let Some(gid) = arg_str(args, "gif_id") {
                    self.library_media(gid, &[MediaKind::Gif])?
                } else if let Some(url) = arg_str(args, "url") {
                    let u = reqwest::Url::parse(url).map_err(|_| anyhow!("`url` is not a valid URL"))?;
                    if !matches!(u.scheme(), "https" | "http") {
                        return Err(anyhow!("`url` must be http(s)"));
                    }
                    // The browser would fetch it: not in Offline mode, unless it is on this network.
                    if self.state.is_offline() {
                        crate::llm::offline_check(u.as_str())?;
                    }
                    Media { kind: Some(MediaKind::Gif), url: Some(u.to_string()), ..Default::default() }
                } else {
                    return Err(anyhow!("provide `gif_id` or `url`"));
                };
                let mut m = self.new_message(args).await?;
                m.media = Some(media);
                m.text = arg_str(args, "caption").unwrap_or_default().to_string();
                let id = m.id;
                self.push(Event::Message(m));
                Ok(json!({"ok": true, "message_id": id}))
            }
            tools::SEND_MEDIA => {
                let mid = arg_str(args, "media_id").ok_or_else(|| anyhow!("`media_id` is required"))?;
                let media = self.library_media(
                    mid,
                    &[MediaKind::Photo, MediaKind::Video, MediaKind::RoundVideo, MediaKind::Voice, MediaKind::Audio, MediaKind::File],
                )?;
                let mut m = self.new_message(args).await?;
                m.media = Some(media);
                m.text = arg_str(args, "caption").unwrap_or_default().to_string();
                let id = m.id;
                self.push(Event::Message(m));
                Ok(json!({"ok": true, "message_id": id}))
            }
            tools::FORWARD_MESSAGE => {
                let src_id = arg_id(args, "message_id").ok_or_else(|| anyhow!("`message_id` is required"))?;
                let src = self.find(src_id).cloned().ok_or_else(|| anyhow!("message #{src_id} not found"))?;
                if src.service {
                    return Err(anyhow!("service messages cannot be forwarded"));
                }
                let mut m = self.new_message(&json!({})).await?;
                m.forwarded_from = Some(src.forwarded_from.clone().filter(|f| !f.is_empty()).unwrap_or(src.from.clone()));
                m.text = src.text.clone();
                m.media = src.media.clone();
                m.link_preview = src.link_preview.clone();
                let id = m.id;
                self.push(Event::Message(m));
                let mut res = json!({"ok": true, "message_id": id});
                if let Some(c) = arg_str(args, "comment") {
                    let mut cm = self.new_message(&json!({})).await?;
                    cm.text = c.to_string();
                    res["comment_message_id"] = json!(cm.id);
                    self.push(Event::Message(cm));
                }
                Ok(res)
            }
            tools::REACT => {
                let target = arg_id(args, "message_id").ok_or_else(|| anyhow!("`message_id` is required"))?;
                if self.find(target).is_none() {
                    return Err(anyhow!("message #{target} not found"));
                }
                let emoji = args.get("emoji").and_then(Value::as_str).unwrap_or_default().trim().to_string();
                if emoji.chars().count() > 8 {
                    return Err(anyhow!("`emoji` must be a single emoji"));
                }
                self.push(Event::Reaction {
                    target,
                    removed: emoji.is_empty(),
                    emoji,
                    from: self.chat.persona.clone(),
                    date: now_string(),
                });
                Ok(json!({"ok": true}))
            }
            other => Err(anyhow!("unknown tool `{other}`")),
        }
    }
}

pub async fn respond(state: &Arc<AppState>, chat_id: &str) -> Result<TurnOutcome> {
    let lock = state.gen_lock(chat_id);
    let _guard = lock.lock().await;
    let (record, chat) = state.snapshot(chat_id).await?;
    let export = &record.export;
    let cfg = state.llm_config().await;
    let (budget, max_steps) = {
        let db = state.db.read().await;
        (db.settings.history_budget_chars.max(4_000), db.settings.max_tool_steps.clamp(1, 20))
    };
    let (mcp_defs, mcp_note, media_tools) = {
        let mut m = state.mcp.lock().await;
        m.ensure_connected().await;
        (m.tool_definitions(), m.instructions_note(), m.media_tools())
    };
    let mut tool_defs = tools::definitions();
    tool_defs.extend(tools::media_definitions(&media_tools));
    tool_defs.extend(mcp_defs);
    let opts = PromptOptions { budget_chars: budget, extra_tools_note: mcp_note, media_tools: media_tools.clone() };
    let mut chat = chat;
    let mut built = prompt::build(export, &chat, &opts);
    // Compaction runs lazily, only when a reply is actually requested and the
    // kept history outgrew the budget (never at import time).
    if let Some(c) = built.compaction.take() {
        let previous = chat.memory.as_ref().map(|m| m.summary.clone());
        let summary = match state.llm.complete(&cfg, &prompt::summary_request(&chat, previous.as_deref(), &c.transcript), &[]).await {
            Ok(r) => r.content.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()),
            Err(e) => {
                tracing::warn!(chat = chat_id, "history summary failed, trimming without it: {e:#}");
                None
            }
        };
        if summary.is_none() {
            tracing::warn!(chat = chat_id, "history summary is empty, keeping the previous one");
        }
        // Even without a new summary the cut point is persisted, so the
        // prompt prefix stays stable instead of sliding by one turn per request.
        let memory = Memory { turns: c.to, summary: summary.or(previous).unwrap_or_default(), updated_at: now_string() };
        tracing::info!(chat = chat_id, from = c.from, to = c.to, chars = memory.summary.len(), "compacted history");
        state.set_memory(chat_id, memory.clone()).await?;
        chat.memory = Some(memory);
        built = prompt::build(export, &chat, &opts);
    }
    let mut messages = built.messages;
    let mut ctx = Ctx { state, export, chat, events: Vec::new(), generated: Vec::new(), media_tools };
    let mut error = None;
    let mut usage = None;

    for step in 0..max_steps {
        let completion = match state.llm.complete(&cfg, &messages, &tool_defs).await {
            Ok(c) => c,
            Err(e) => {
                error = Some(format!("{e:#}"));
                break;
            }
        };
        if let Some(u) = completion.usage {
            tracing::info!(
                chat = chat_id,
                prompt_tokens = u.prompt_tokens,
                cached_tokens = u.cached_tokens,
                completion_tokens = u.completion_tokens,
                "llm usage"
            );
            usage = Some(u);
        }
        if completion.tool_calls.is_empty() {
            // Models without tool support answer in plain text: deliver it.
            if (step == 0 || ctx.events.is_empty())
                && let Some(text) = completion.content
                && let Err(e) = ctx.action(tools::SEND_MESSAGE, &json!({"text": text})).await
            {
                error = Some(format!("{e:#}"));
            }
            break;
        }
        let mut assistant = json!({
            "role": "assistant",
            "content": completion.content,
            "tool_calls": completion.tool_calls.iter().map(|c| json!({
                "id": c.id, "type": "function", "function": {"name": c.name, "arguments": c.arguments}
            })).collect::<Vec<_>>()
        });
        if let Some(r) = completion.raw_message.get("reasoning_details") {
            assistant["reasoning_details"] = r.clone();
        }
        messages.push(assistant);
        let mut follow_up = false;
        for call in &completion.tool_calls {
            let args: Value = match serde_json::from_str(if call.arguments.trim().is_empty() { "{}" } else { &call.arguments }) {
                Ok(v) => v,
                Err(e) => {
                    follow_up = true;
                    messages.push(json!({"role": "tool", "tool_call_id": call.id, "content": json!({"ok": false, "error": format!("arguments are not valid JSON: {e}")}).to_string()}));
                    continue;
                }
            };
            let content = if tools::is_action(&call.name) {
                match ctx.action(&call.name, &args).await {
                    Ok(v) => v.to_string(),
                    Err(e) => {
                        follow_up = true;
                        json!({"ok": false, "error": format!("{e:#}")}).to_string()
                    }
                }
            } else if call.name == tools::SEARCH_HISTORY {
                follow_up = true;
                let q = arg_str(&args, "query").unwrap_or_default();
                let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(12).clamp(1, 30) as usize;
                let hits = prompt::search(export, &ctx.chat, q, limit);
                if hits.is_empty() { "no matches".to_string() } else { hits.join("\n") }
            } else {
                follow_up = true;
                let resolved = state.mcp.lock().await.resolve(&call.name);
                match resolved {
                    Some((client, tool)) => {
                        let clean = clean_args(&args);
                        tracing::info!(chat = chat_id, server = %client.name, tool = %tool, args = %crate::llm::truncate(&clean.to_string(), 300), "MCP tool call");
                        let result = client.call_tool(&tool, clean).await;
                        match result {
                            Ok(r) => {
                                let mut text = crate::mcp::result_to_text(&r);
                                // Images / audio the tool produced become sendable chat media.
                                let description = crate::genmedia::description_from_args(&args);
                                for cap in crate::genmedia::collect(&r, client.workspace.as_deref()) {
                                    match ctx.store_generated(cap, description.clone()).await {
                                        Ok((id, kind)) => text.push_str(&format!(
                                            "\n[saved as chat media `{id}` ({}). To send it: send_media {{\"media_id\": \"{id}\"}}]",
                                            kind.label()
                                        )),
                                        Err(e) => tracing::warn!(chat = chat_id, "storing tool media failed: {e:#}"),
                                    }
                                }
                                text
                            }
                            Err(e) => format!("ERROR: {e:#}"),
                        }
                    }
                    None => format!("ERROR: unknown tool `{}`", call.name),
                }
            };
            messages.push(json!({"role": "tool", "tool_call_id": call.id, "content": content}));
        }
        if !follow_up {
            break;
        }
    }

    if !ctx.generated.is_empty() {
        state.add_generated(chat_id, std::mem::take(&mut ctx.generated)).await?;
    }
    if !ctx.events.is_empty() {
        state.append_events(chat_id, &ctx.events).await?;
    }
    if let Some(u) = usage {
        state.set_usage(chat_id, u).await?;
    }
    Ok(TurnOutcome { events: ctx.events, error, usage })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_args() {
        assert_eq!(arg_id(&json!({"a": 5}), "a"), Some(5));
        assert_eq!(arg_id(&json!({"a": "#7"}), "a"), Some(7));
        assert_eq!(arg_id(&json!({}), "a"), None);
    }
}
