use crate::work_note_documents as documents;
use crate::{AppState, SqliteStore};
use base64::Engine;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::io::Read;
use std::sync::Arc;

fn store(state: &tauri::State<'_, AppState>) -> Result<Arc<SqliteStore>, String> {
    state
        .store
        .lock()
        .map_err(|_| "local_store_unavailable")?
        .clone()
        .ok_or("local_store_unavailable".into())
}
fn response(result: Result<Value, String>) -> Value {
    result.unwrap_or_else(|error| json!({"ok":false,"error":error}))
}
fn tenant(value: String) -> Result<String, String> {
    let id = crate::normalize_tenant_id(Some(&json!(value)));
    if id.is_empty() {
        Err("tenant_id_required".into())
    } else {
        Ok(id)
    }
}

#[tauri::command]
pub(crate) fn get_local_work_note_document(
    state: tauri::State<'_, AppState>,
    tenant_id: String,
    page_id: String,
) -> Value {
    response((|| {
        let store = store(&state)?;
        let tenant = tenant(tenant_id)?;
        let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
        let page = documents::read(&conn, &tenant, &page_id)?.ok_or("work_note_not_found")?;
        documents::assert_local_editable(&conn, &tenant, &page)?;
        if documents::is_trashed(&page) {
            return Err("work_note_trashed".into());
        }
        let bound = crate::lesson_plan_bindings::stored_page_structure(&conn, &tenant, &page_id)?.is_some();
        Ok(json!({"ok":true,"revision":documents::revision(&page),"page":page,
            "editPolicy":{"titleEditable":!bound,"structureEditable":!bound}}))
    })())
}
#[tauri::command]
pub(crate) fn save_local_work_note_document(
    state: tauri::State<'_, AppState>,
    input: Value,
) -> Value {
    response(store(&state).and_then(|store| documents::save(&store, input)))
}
#[tauri::command]
pub(crate) fn mutate_local_work_note_document(
    state: tauri::State<'_, AppState>,
    input: Value,
) -> Value {
    response(store(&state).and_then(|store| documents::mutate(&store, input)))
}
#[tauri::command]
pub(crate) fn list_local_work_note_documents(
    state: tauri::State<'_, AppState>,
    tenant_id: String,
    query: Option<String>,
) -> Value {
    response(
        store(&state)
            .and_then(|store| {
                documents::list_editable(&store, tenant_id, query.unwrap_or_default())
            })
            .map(|items| json!({"ok":true,"items":items})),
    )
}
#[tauri::command]
pub(crate) fn list_local_work_note_history(
    state: tauri::State<'_, AppState>,
    tenant_id: String,
    page_id: String,
    limit: Option<i64>,
    cursor: Option<crate::work_note_history::HistoryCursor>,
) -> Value {
    response((|| {
        let store = store(&state)?;
        let tenant = tenant(tenant_id)?;
        let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
        crate::work_note_history::list(&conn, &tenant, &page_id, limit, cursor)
    })())
}
#[tauri::command]
pub(crate) fn get_local_work_note_version(
    state: tauri::State<'_, AppState>,
    tenant_id: String,
    page_id: String,
    version_id: String,
) -> Value {
    response((|| {
        let store = store(&state)?;
        let tenant = tenant(tenant_id)?;
        let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
        crate::work_note_history::get(&conn, &tenant, &page_id, &version_id)
    })())
}
#[tauri::command]
pub(crate) fn list_local_work_note_trash(
    state: tauri::State<'_, AppState>,
    tenant_id: String,
) -> Value {
    response((|| {
        let store = store(&state)?;
        let tenant = tenant(tenant_id)?;
        crate::work_note_retention::maintain(&store, &tenant)?;
        let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
        documents::trash(&conn, &tenant)
    })())
}

pub(crate) fn save_draft(store: &SqliteStore, input: Value) -> Result<Value, String> {
    let (tenant, page) = documents::scope(&input)?;
    documents::validate_content(&input)?;
    let generation = input["generation"]
        .as_i64()
        .filter(|g| *g > 0)
        .ok_or("work_note_draft_generation_required")?;
    if input["baseRevision"].as_i64().is_none() {
        return Err("work_note_draft_base_revision_required".into());
    }
    let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    documents::assert_local_editable(&conn, &tenant, &input)?;
    let changed=conn.execute("INSERT INTO work_note_local_drafts(tenant_id,page_id,generation,payload_json,updated_at_ms) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(tenant_id,page_id) DO UPDATE SET generation=excluded.generation,payload_json=excluded.payload_json,updated_at_ms=excluded.updated_at_ms WHERE excluded.generation>work_note_local_drafts.generation",params![tenant,page,generation,input.to_string(),crate::now_ms()]).map_err(|e|format!("work_note_draft_save_failed:{e}"))?;
    if changed == 0 {
        let saved: String = conn
            .query_row(
                "SELECT payload_json FROM work_note_local_drafts WHERE tenant_id=?1 AND page_id=?2",
                params![tenant, page],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if serde_json::from_str::<Value>(&saved).ok().as_ref() != Some(&input) {
            return Err("work_note_draft_generation_conflict".into());
        }
    }
    Ok(json!({"ok":true,"draft":input}))
}
#[tauri::command]
pub(crate) fn save_local_work_note_draft(state: tauri::State<'_, AppState>, input: Value) -> Value {
    response(store(&state).and_then(|store| save_draft(&store, input)))
}
#[tauri::command]
pub(crate) fn get_local_work_note_draft(
    state: tauri::State<'_, AppState>,
    tenant_id: String,
    page_id: String,
) -> Value {
    response((|| {
        let store = store(&state)?;
        let tenant = tenant(tenant_id)?;
        let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT payload_json FROM work_note_local_drafts WHERE tenant_id=?1 AND page_id=?2",
                params![tenant, page_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let draft = raw
            .map(|raw| {
                serde_json::from_str::<Value>(&raw)
                    .map_err(|_| "work_note_draft_invalid".to_string())
            })
            .transpose()?;
        Ok(json!({"ok":true,"draft":draft}))
    })())
}
#[tauri::command]
pub(crate) fn discard_local_work_note_draft(
    state: tauri::State<'_, AppState>,
    tenant_id: String,
    page_id: String,
    generation: i64,
) -> Value {
    response((|| {
        let store = store(&state)?;
        let tenant = tenant(tenant_id)?;
        let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
        let deleted=conn.execute("DELETE FROM work_note_local_drafts WHERE tenant_id=?1 AND page_id=?2 AND generation=?3",params![tenant,page_id,generation]).map_err(|e|e.to_string())?;
        Ok(json!({"ok":true,"deleted":deleted}))
    })())
}
#[tauri::command]
pub(crate) fn list_local_work_note_attachments(
    state: tauri::State<'_, AppState>,
    tenant_id: String,
    page_id: Option<String>,
) -> Value {
    response(
        store(&state)
            .and_then(|store| {
                crate::work_note_attachments::list(&store, tenant_id, page_id.unwrap_or_default())
            })
            .map(|items| json!({"ok":true,"items":items})),
    )
}
#[tauri::command]
pub(crate) fn save_local_work_note_attachment(
    state: tauri::State<'_, AppState>,
    input: Value,
) -> Value {
    response(store(&state).and_then(|store| save_attachment(&store, input)))
}

pub(crate) fn save_attachment(store: &SqliteStore, input: Value) -> Result<Value, String> {
    let (tenant, page) = documents::scope(&input)?;
    {
        let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
        let document = documents::read(&conn, &tenant, &page)?.ok_or("work_note_not_found")?;
        documents::assert_local_editable(&conn, &tenant, &document)?;
        if documents::is_trashed(&document) {
            return Err("work_note_trashed".into());
        }
    }
    // The main window's native picker supplies a path for large files. Stream it through
    // the existing attachment writer instead of copying it through JSON IPC in memory.
    let mut reader: Box<dyn Read> = if let Some(path) = input["sourcePath"].as_str() {
        if path.trim().is_empty() {
            return Err("work_note_attachment_source_path_invalid".into());
        }
        let file =
            std::fs::File::open(path).map_err(|_| "work_note_attachment_source_read_failed")?;
        if !file
            .metadata()
            .map_err(|_| "work_note_attachment_source_read_failed")?
            .is_file()
        {
            return Err("work_note_attachment_source_path_invalid".into());
        }
        Box::new(file)
    } else {
        let values = input["bytes"]
            .as_array()
            .ok_or("work_note_attachment_bytes_required")?;
        if values.len() > 20 * 1024 * 1024 {
            return Err("work_note_attachment_ipc_limit_exceeded".into());
        }
        let bytes = values
            .iter()
            .map(|v| {
                v.as_u64()
                    .filter(|n| *n <= 255)
                    .map(|n| n as u8)
                    .ok_or("work_note_attachment_bytes_invalid")
            })
            .collect::<Result<Vec<_>, _>>()?;
        Box::new(std::io::Cursor::new(bytes))
    };
    let text = |key: &str| input[key].as_str().unwrap_or_default().to_string();
    let attachment = crate::work_note_attachments::save(
        store,
        tenant,
        text("attachmentId"),
        page,
        text("blockId"),
        text("fileName"),
        text("contentType"),
        reader.as_mut(),
    )?;
    Ok(json!({"ok":true,"attachment":attachment}))
}
#[tauri::command]
pub(crate) fn read_local_work_note_attachment(
    state: tauri::State<'_, AppState>,
    tenant_id: String,
    attachment_id: String,
) -> Value {
    response((|| {
        let store = store(&state)?;
        let mut file = crate::work_note_attachments::open(&store, tenant_id, attachment_id)?;
        if file.size > 20 * 1024 * 1024 {
            return Err("work_note_attachment_ipc_limit_exceeded".into());
        }
        let mut bytes = Vec::new();
        file.file
            .read_to_end(&mut bytes)
            .map_err(|_| "work_note_attachment_read_failed")?;
        Ok(
            json!({"ok":true,"base64":base64::engine::general_purpose::STANDARD.encode(bytes),"contentType":file.record.content_type,"fileName":file.record.file_name}),
        )
    })())
}
#[tauri::command]
pub(crate) fn delete_local_work_note_attachment(
    state: tauri::State<'_, AppState>,
    tenant_id: String,
    attachment_id: String,
) -> Value {
    response((|| {
        let store = store(&state)?;
        let attachments =
            crate::work_note_attachments::list(&store, tenant_id.clone(), String::new())?;
        if let Some(attachment) = attachments
            .iter()
            .find(|row| row.attachment_id == attachment_id)
        {
            let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
            let page = documents::read(&conn, &tenant_id, &attachment.page_id)?
                .ok_or("work_note_not_found")?;
            documents::assert_local_editable(&conn, &tenant_id, &page)?;
        }
        let deleted =
            crate::work_note_attachments::delete(&store, tenant_id.clone(), attachment_id.clone())?;
        let retained = crate::work_note_attachments::list(&store, tenant_id, String::new())?
            .iter()
            .any(|row| row.attachment_id == attachment_id);
        Ok(json!({"ok":true,"deleted":deleted,"retainedForHistory":retained}))
    })())
}

#[tauri::command]
pub(crate) fn ensure_local_work_note_workspace(
    state: tauri::State<'_, AppState>,
    tenant_id: String,
    workspace: String,
) -> Value {
    response((|| {
        if workspace != "lesson_materials" {
            return Err("local_workspace_invalid".into());
        }
        let store = store(&state)?;
        let tenant = tenant(tenant_id)?;
        let mut conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let id = "lesson-materials-root";
        if let Some(page) = documents::read(&tx, &tenant, id)? {
            if page
                .pointer("/properties/systemKind")
                .and_then(Value::as_str)
                != Some("lesson_materials_folder")
                || documents::is_trashed(&page)
            {
                return Err("work_note_system_folder_conflict".into());
            }
        } else {
            crate::canonical_write_transactions::upsert_work_note(
                &tx,
                json!({"tenantId":tenant,"pageId":id,"parentId":null,"title":"수업자료","emoji":"📚","position":0,"properties":{"systemKind":"lesson_materials_folder"},"blocks":[],"markdown":""}),
            )?;
        }
        let page = documents::read(&tx, &tenant, id)?.ok_or("work_note_readback_failed")?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(json!({"ok":true,"revision":documents::revision(&page),"page":page}))
    })())
}
