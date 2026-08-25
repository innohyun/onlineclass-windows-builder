use super::{normalize_id_segment, normalize_tenant_id, AppState, SqliteStore};
use rusqlite::params;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

fn load_view(store: &SqliteStore, raw_tenant_id: String, raw_page_id: String) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(raw_tenant_id)));
    let page_id = normalize_id_segment(Some(&Value::String(raw_page_id)), 180);
    if tenant_id.is_empty() { return Err("tenant_id_required".to_string()); }
    if page_id.is_empty() { return Err("page_id_required".to_string()); }
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    let mut statement = conn.prepare(
        "SELECT page_id,parent_id,title,emoji,position,properties_json,document_json,markdown,created_at_ms,updated_at_ms FROM work_note_pages WHERE tenant_id=?1 ORDER BY position,page_id",
    ).map_err(|error| format!("work_note_reader_prepare_failed:{error}"))?;
    let rows = statement.query_map(params![tenant_id], |row| {
        let properties = row.get::<_, String>(5)?;
        let blocks = row.get::<_, String>(6)?;
        Ok(json!({
            "pageId": row.get::<_, String>(0)?, "parentId": row.get::<_, Option<String>>(1)?,
            "title": row.get::<_, String>(2)?, "emoji": row.get::<_, String>(3)?, "position": row.get::<_, i64>(4)?,
            "properties": serde_json::from_str::<Value>(&properties).unwrap_or_else(|_| json!({})),
            "blocks": serde_json::from_str::<Value>(&blocks).unwrap_or_else(|_| json!([])),
            "markdown": row.get::<_, String>(7)?, "createdAtMs": row.get::<_, i64>(8)?, "updatedAtMs": row.get::<_, i64>(9)?,
        }))
    }).map_err(|error| format!("work_note_reader_query_failed:{error}"))?;
    let mut pages = Vec::new();
    for row in rows { pages.push(row.map_err(|error| format!("work_note_reader_row_failed:{error}"))?); }
    drop(statement);
    let parent_by_id = pages.iter().filter_map(|page| page.get("pageId").and_then(Value::as_str).map(|id| {
        (id.to_string(), page.get("parentId").and_then(Value::as_str).map(str::to_string))
    })).collect::<HashMap<_, _>>();
    if !parent_by_id.contains_key(&page_id) { return Err("work_note_reader_page_not_found".to_string()); }
    let mut root_page_id = page_id.clone();
    let mut seen = HashSet::new();
    while let Some(Some(parent_id)) = parent_by_id.get(&root_page_id) {
        if !seen.insert(root_page_id.clone()) { return Err("work_note_reader_tree_invalid".to_string()); }
        root_page_id = parent_id.clone();
    }
    let mut included = HashSet::from([root_page_id.clone()]);
    loop {
        let before = included.len();
        for (id, parent) in &parent_by_id {
            if parent.as_ref().is_some_and(|value| included.contains(value)) { included.insert(id.clone()); }
        }
        if included.len() == before { break; }
    }
    pages.retain(|page| page.get("pageId").and_then(Value::as_str).is_some_and(|id| included.contains(id)));
    let mut attachment_statement = conn.prepare(
        "SELECT attachment_id,page_id,block_id,file_name,content_type,byte_size,sha256,updated_at_ms FROM work_note_attachments WHERE tenant_id=?1 ORDER BY page_id,attachment_id",
    ).map_err(|error| format!("work_note_reader_attachment_prepare_failed:{error}"))?;
    let attachments = attachment_statement.query_map(params![tenant_id], |row| Ok(json!({
        "attachmentId": row.get::<_, String>(0)?, "mediaId": row.get::<_, String>(0)?, "pageId": row.get::<_, String>(1)?,
        "blockId": row.get::<_, String>(2)?, "fileName": row.get::<_, String>(3)?, "contentType": row.get::<_, String>(4)?,
        "size": row.get::<_, i64>(5)?, "sha256": row.get::<_, String>(6)?, "updatedAtMs": row.get::<_, i64>(7)?,
    }))).map_err(|error| format!("work_note_reader_attachment_query_failed:{error}"))?;
    let mut filtered_attachments = Vec::new();
    for row in attachments {
        let value = row.map_err(|error| format!("work_note_reader_attachment_row_failed:{error}"))?;
        if value.get("pageId").and_then(Value::as_str).is_some_and(|id| included.contains(id)) { filtered_attachments.push(value); }
    }
    Ok(json!({ "ok": true, "rootPageId": root_page_id, "selectedPageId": page_id,
        "pages": pages, "attachments": filtered_attachments }))
}

#[tauri::command]
pub(crate) fn get_local_work_note_view(state: tauri::State<'_, AppState>, tenant_id: String, page_id: String) -> Value {
    let result = state.store.lock().ok().and_then(|store| store.clone())
        .ok_or_else(|| "local_store_unavailable".to_string())
        .and_then(|store| load_view(&store, tenant_id, page_id));
    result.unwrap_or_else(|error| json!({ "ok": false, "error": error }))
}
