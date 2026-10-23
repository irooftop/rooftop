//! Imported exports: details, deletion and the media library's descriptions.

use super::{ApiError, ApiResult, S, bad};
use crate::state::AppState;
use crate::store::ExportRecord;
use anyhow::anyhow;
use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) fn export_summary(rec: &ExportRecord) -> Value {
    let e = &rec.export;
    json!({
        "id": rec.id,
        "title": e.title,
        "imported_at": rec.imported_at,
        "participants": e.participants,
        "exporter": e.exporter(),
        "messages": e.messages.len(),
        "library": e.library.len(),
        "described": e.library.iter().filter(|i| i.description.is_some()).count(),
        "blank": e.blank,
    })
}

pub(super) async fn get_export(State(st): S, Path(id): Path<String>) -> ApiResult {
    let rec = st.db.read().await.exports.get(&id).cloned().ok_or_else(|| anyhow!("export not found"))?;
    let mut v = export_summary(&rec);
    // Without ffmpeg, GIFs and videos can be described only from a thumbnail.
    v["ffmpeg"] = json!(crate::frames::ffmpeg().is_some());
    let dir = st.paths.export_files(&id);
    v["library_items"] = tokio::task::spawn_blocking(move || {
        let items: Vec<Value> = rec
            .export
            .library
            .iter()
            .map(|i| {
                let mut item = json!(i);
                item["describable"] = json!(crate::describe::can_describe(&dir, i));
                item
            })
            .collect();
        json!(items)
    })
    .await
    .map_err(anyhow::Error::from)?;
    Ok(Json(v))
}

pub(super) async fn delete_export(State(st): S, Path(id): Path<String>) -> ApiResult {
    let mut db = st.db.write().await;
    db.exports.remove(&id).ok_or_else(|| anyhow!("export not found"))?;
    let chats: Vec<String> = db.chats.values().filter(|c| c.export_id == id).map(|c| c.id.clone()).collect();
    for c in &chats {
        db.chats.remove(c);
        st.paths.delete_chat(c)?;
    }
    let shares: Vec<String> = db.shares.values().filter(|s| chats.contains(&s.chat_id)).map(|s| s.id.clone()).collect();
    for s in &shares {
        db.shares.remove(s);
        st.paths.delete_share(s)?;
    }
    st.paths.delete_export(&id)?;
    Ok(Json(json!({"ok": true, "deleted_chats": chats})))
}

#[derive(Deserialize, Default)]
pub(super) struct DescribeReq {
    #[serde(default)]
    ids: Vec<String>,
    #[serde(default)]
    force: bool,
}

/// Replaces library descriptions of an export and persists it.
async fn set_descriptions(st: &AppState, id: &str, updates: &[(String, Option<String>)]) -> Result<(), ApiError> {
    // Merge into the current record (it may have changed meanwhile).
    let mut db = st.db.write().await;
    let cur = db.exports.get(id).cloned().ok_or_else(|| anyhow!("export not found"))?;
    let mut new = (*cur).clone();
    for item in new.export.library.iter_mut() {
        if let Some((_, d)) = updates.iter().find(|(i, _)| *i == item.id) {
            item.description = d.clone();
        }
    }
    // Serializing a big export takes a while: keep it off the async workers.
    let (paths, new) = (st.paths.clone(), Arc::new(new));
    let saved = new.clone();
    tokio::task::spawn_blocking(move || paths.save_export(&saved)).await.map_err(anyhow::Error::from)??;
    db.exports.insert(id.to_string(), new);
    Ok(())
}

pub(super) async fn describe_export(State(st): S, Path(id): Path<String>, body: Option<Json<DescribeReq>>) -> ApiResult {
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let rec = st.db.read().await.exports.get(&id).cloned().ok_or_else(|| anyhow!("export not found"))?;
    let cfg = st.media_llm_config().await;
    let sel = crate::describe::Selection { ids: req.ids, force: req.force };
    let (described, failed) = crate::describe::describe_library(&st.llm, &cfg, &st.paths.export_files(&id), &rec.export, &sel).await;
    if !described.is_empty() {
        let updates: Vec<(String, Option<String>)> = described.iter().map(|(i, d)| (i.clone(), Some(d.clone()))).collect();
        set_descriptions(&st, &id, &updates).await?;
    }
    let items: serde_json::Map<String, Value> = described.iter().map(|(i, d)| (i.clone(), json!(d))).collect();
    Ok(Json(json!({"described": described.len(), "items": items, "failed": failed})))
}

#[derive(Deserialize)]
pub(super) struct DescriptionPatch {
    description: Option<String>,
}

/// Manual edit of one library description (empty clears it).
pub(super) async fn patch_library_item(
    State(st): S,
    Path((id, item)): Path<(String, String)>,
    Json(p): Json<DescriptionPatch>,
) -> ApiResult {
    let exists = st.db.read().await.exports.get(&id).is_some_and(|r| r.export.library.iter().any(|i| i.id == item));
    if !exists {
        return Err(bad("unknown library item"));
    }
    let d = p.description.map(|d| d.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|d| !d.is_empty());
    if d.as_ref().is_some_and(|d| d.chars().count() > 1000) {
        return Err(bad("description is longer than 1000 characters"));
    }
    set_descriptions(&st, &id, &[(item.clone(), d.clone())]).await?;
    Ok(Json(json!({"id": item, "description": d})))
}
