use super::{backup, normalize, normalize_tenant_id, AppState, SqliteStore};
use chrono::Utc;
use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection, OptionalExtension, Transaction};
use serde_json::{json, Value};
use std::fs;

fn now_ms() -> i64 { Utc::now().timestamp_millis() }

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, String> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|error| format!("device_sync_conflict_schema_prepare_failed:{error}"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| format!("device_sync_conflict_schema_query_failed:{error}"))?;
    for row in rows {
        if row.map_err(|error| format!("device_sync_conflict_schema_row_failed:{error}"))? == column { return Ok(true); }
    }
    Ok(false)
}

pub(crate) fn ensure_schema(conn: &Connection) -> Result<(), String> {
    if !column_exists(conn, "local_store_device_sync_conflicts", "reviewed_at_ms")? {
        conn.execute("ALTER TABLE local_store_device_sync_conflicts ADD COLUMN reviewed_at_ms INTEGER", [])
            .map_err(|error| format!("device_sync_conflict_review_column_failed:{error}"))?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS local_store_device_sync_conflict_stats (
           tenant_id TEXT PRIMARY KEY, lifetime_count INTEGER NOT NULL DEFAULT 0, updated_at_ms INTEGER NOT NULL
         );",
    ).map_err(|error| format!("device_sync_conflict_stats_schema_failed:{error}"))?;
    conn.execute(
        "INSERT INTO local_store_device_sync_conflict_stats (tenant_id,lifetime_count,updated_at_ms)
         SELECT tenant_id,COUNT(*),?1 FROM local_store_device_sync_conflicts GROUP BY tenant_id
         ON CONFLICT(tenant_id) DO UPDATE SET lifetime_count=MAX(lifetime_count,excluded.lifetime_count),updated_at_ms=excluded.updated_at_ms",
        params![now_ms()],
    ).map_err(|error| format!("device_sync_conflict_stats_seed_failed:{error}"))?;
    Ok(())
}

pub(crate) fn increment_lifetime(transaction: &Transaction<'_>, tenant_id: &str, captured_at_ms: i64) -> Result<(), String> {
    transaction.execute(
        "INSERT INTO local_store_device_sync_conflict_stats (tenant_id,lifetime_count,updated_at_ms) VALUES (?1,1,?2)
         ON CONFLICT(tenant_id) DO UPDATE SET lifetime_count=lifetime_count+1,updated_at_ms=excluded.updated_at_ms",
        params![tenant_id, captured_at_ms],
    ).map_err(|error| format!("device_sync_conflict_stats_increment_failed:{error}"))?;
    Ok(())
}

fn tenant(raw: String) -> Result<String, String> {
    let value = normalize_tenant_id(Some(&Value::String(raw)));
    if value.is_empty() { Err("tenant_id_required".to_string()) } else { Ok(value) }
}

fn store(state: tauri::State<'_, AppState>) -> Result<std::sync::Arc<SqliteStore>, String> {
    state.store.lock().ok().and_then(|store| store.clone()).ok_or_else(|| "local_store_unavailable".to_string())
}

fn stats(store: &SqliteStore, tenant_id: &str) -> Result<Value, String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    conn.query_row(
        "SELECT COUNT(*),SUM(CASE WHEN reviewed_at_ms IS NULL THEN 1 ELSE 0 END),
                COALESCE((SELECT lifetime_count FROM local_store_device_sync_conflict_stats s WHERE s.tenant_id=?1),COUNT(*))
         FROM local_store_device_sync_conflicts WHERE tenant_id=?1",
        params![tenant_id], |row| Ok(json!({ "retained": row.get::<_, i64>(0)?,
            "unreviewed": row.get::<_, Option<i64>>(1)?.unwrap_or(0), "lifetime": row.get::<_, i64>(2)? })),
    ).map_err(|error| format!("device_sync_conflict_stats_failed:{error}"))
}

fn preview_text(sources: &[&Value], keys: &[&str]) -> Option<String> {
    for source in sources {
        for key in keys {
            if let Some(value) = source.get(key) {
                let text = match value {
                    Value::String(value) => value.trim().to_string(),
                    Value::Number(value) => value.to_string(),
                    _ => String::new(),
                };
                if !text.is_empty() { return Some(text); }
            }
        }
    }
    None
}

fn preview_number(sources: &[&Value], keys: &[&str]) -> Option<i64> {
    for source in sources {
        for key in keys {
            if let Some(value) = source.get(key) {
                if let Some(number) = value.as_i64() { return Some(number); }
                if let Some(number) = value.as_str().and_then(|text| text.parse::<i64>().ok()) { return Some(number); }
            }
        }
    }
    None
}

fn conflict_preview(payload_json: &str) -> Value {
    let outer = serde_json::from_str::<Value>(payload_json).unwrap_or_else(|_| json!({}));
    let nested = outer.get("payload_json").and_then(Value::as_str)
        .and_then(|value| serde_json::from_str::<Value>(value).ok()).unwrap_or_else(|| json!({}));
    let sources = [&nested, &outer];
    json!({
        "title": preview_text(&sources, &["title", "pageTitle", "assignmentTitle", "name"]),
        "emoji": preview_text(&sources, &["emoji"]),
        "studentName": preview_text(&sources, &["studentName", "displayName"]),
        "studentCode": preview_text(&sources, &["studentCode", "student_code", "studentId", "student_id"]),
        "classNo": preview_number(&sources, &["classNo", "class_no", "number"]),
        "dateKey": preview_text(&sources, &["dateKey", "date_key", "date", "scheduledDate", "scheduled_date"]),
        "subject": preview_text(&sources, &["subject", "curriculum", "topic", "category"]),
        "fileName": preview_text(&sources, &["fileName", "file_name"]),
        "status": preview_text(&sources, &["status", "kind"]),
        "summary": preview_text(&sources, &["summary", "description", "observation", "reason", "content", "draftText", "text"]),
    })
}

fn current_payload(store: &SqliteStore, tenant_id: &str, table_name: &str, record_key: &str) -> Result<Option<Value>, String> {
    let table = backup::syncable_tables().find(|table| table.name == table_name)
        .ok_or_else(|| "device_sync_conflict_table_unknown".to_string())?;
    let key_values = serde_json::from_str::<Vec<Value>>(record_key)
        .map_err(|_| "device_sync_conflict_record_key_invalid".to_string())?;
    let key_columns = table.key_columns.iter().filter(|column| **column != "tenant_id").collect::<Vec<_>>();
    if key_values.len() != key_columns.len() { return Err("device_sync_conflict_record_key_invalid".to_string()); }
    let mut values = vec![SqlValue::Text(tenant_id.to_string())];
    for value in key_values {
        values.push(match value { Value::String(value) => SqlValue::Text(value),
            Value::Number(value) if value.is_i64() => SqlValue::Integer(value.as_i64().unwrap_or(0)),
            _ => return Err("device_sync_conflict_record_key_invalid".to_string()) });
    }
    let where_clause = key_columns.iter().enumerate().map(|(index, column)| format!("{column}=?{}", index + 2)).collect::<Vec<_>>().join(" AND ");
    let fields = table.columns.iter().flat_map(|column| [format!("'{column}'"), column.to_string()]).collect::<Vec<_>>().join(",");
    let sql = format!("SELECT json_object({fields}) FROM {} WHERE tenant_id=?1 AND {where_clause}", table.name);
    let raw = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?.query_row(&sql, params_from_iter(values.iter()), |row| row.get::<_, String>(0))
        .optional().map_err(|error| format!("device_sync_conflict_current_failed:{error}"))?;
    Ok(raw.and_then(|value| serde_json::from_str(&value).ok()))
}

#[tauri::command]
pub(crate) fn list_device_sync_conflicts(state: tauri::State<'_, AppState>, tenant_id: String) -> Value {
    let result = (|| -> Result<Value, String> {
        let tenant_id = tenant(tenant_id)?; let store = store(state)?;
        let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let mut statement = conn.prepare("SELECT conflict_id,table_name,record_key,losing_generation,winning_generation,captured_at_ms,reviewed_at_ms,payload_json FROM local_store_device_sync_conflicts WHERE tenant_id=?1 ORDER BY captured_at_ms DESC,conflict_id")
            .map_err(|error| format!("device_sync_conflict_list_prepare_failed:{error}"))?;
        let rows = statement.query_map(params![tenant_id], |row| Ok(json!({ "conflictId": row.get::<_, String>(0)?,
            "tableName": row.get::<_, String>(1)?, "recordKey": row.get::<_, String>(2)?,
            "losingGeneration": row.get::<_, i64>(3)?, "winningGeneration": row.get::<_, i64>(4)?,
            "capturedAtMs": row.get::<_, i64>(5)?, "reviewedAtMs": row.get::<_, Option<i64>>(6)?,
            "preview": conflict_preview(&row.get::<_, String>(7)?) })))
            .map_err(|error| format!("device_sync_conflict_list_failed:{error}"))?;
        let mut records = Vec::new(); for row in rows { records.push(row.map_err(|error| format!("device_sync_conflict_list_row_failed:{error}"))?); }
        drop(statement); drop(conn);
        Ok(json!({ "ok": true, "records": records, "stats": stats(&store, &tenant_id)? }))
    })(); result.unwrap_or_else(|error| json!({ "ok": false, "error": error }))
}

#[tauri::command]
pub(crate) fn get_device_sync_conflict(state: tauri::State<'_, AppState>, tenant_id: String, conflict_id: String) -> Value {
    let result = (|| -> Result<Value, String> {
        let tenant_id = tenant(tenant_id)?; let conflict_id = normalize(&conflict_id, 180); let store = store(state)?;
        let row = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?.query_row(
            "SELECT table_name,record_key,losing_generation,winning_generation,payload_json,captured_at_ms,reviewed_at_ms FROM local_store_device_sync_conflicts WHERE tenant_id=?1 AND conflict_id=?2",
            params![tenant_id, conflict_id], |row| Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?,row.get::<_, i64>(2)?,row.get::<_, i64>(3)?,row.get::<_, String>(4)?,row.get::<_, i64>(5)?,row.get::<_, Option<i64>>(6)?)),
        ).optional().map_err(|error| format!("device_sync_conflict_detail_failed:{error}"))?.ok_or_else(|| "device_sync_conflict_not_found".to_string())?;
        let current = current_payload(&store, &tenant_id, &row.0, &row.1)?;
        Ok(json!({ "ok": true, "conflict": { "conflictId": conflict_id, "tableName": row.0, "recordKey": row.1,
            "losingGeneration": row.2, "winningGeneration": row.3,
            "losingPayload": serde_json::from_str::<Value>(&row.4).unwrap_or_else(|_| json!({})),
            "currentPayload": current, "capturedAtMs": row.5, "reviewedAtMs": row.6 } }))
    })(); result.unwrap_or_else(|error| json!({ "ok": false, "error": error }))
}

fn ids(value: Vec<String>) -> Result<Vec<String>, String> {
    let values = value.into_iter().map(|id| normalize(&id, 180)).filter(|id| !id.is_empty()).collect::<Vec<_>>();
    if values.is_empty() || values.len() > 500 { Err("device_sync_conflict_selection_required".to_string()) } else { Ok(values) }
}

#[tauri::command]
pub(crate) fn review_device_sync_conflicts(state: tauri::State<'_, AppState>, tenant_id: String, conflict_ids: Vec<String>) -> Value {
    let result = (|| -> Result<Value, String> { let tenant_id=tenant(tenant_id)?;let values=ids(conflict_ids)?;let store=store(state)?;
        let now=now_ms();let conn=store.conn.lock().map_err(|_|"db_lock_failed".to_string())?;
        let mut changed=0;for id in &values { changed+=conn.execute("UPDATE local_store_device_sync_conflicts SET reviewed_at_ms=COALESCE(reviewed_at_ms,?3) WHERE tenant_id=?1 AND conflict_id=?2",params![tenant_id,id,now]).map_err(|error|format!("device_sync_conflict_review_failed:{error}"))?; }
        if changed!=values.len(){return Err("device_sync_conflict_selection_changed".to_string())}drop(conn);Ok(json!({"ok":true,"stats":stats(&store,&tenant_id)?}))
    })();result.unwrap_or_else(|error|json!({"ok":false,"error":error}))
}

#[tauri::command]
pub(crate) fn delete_device_sync_conflicts(state: tauri::State<'_, AppState>, tenant_id: String, conflict_ids: Vec<String>) -> Value {
    let result=(||->Result<Value,String>{let tenant_id=tenant(tenant_id)?;let values=ids(conflict_ids)?;let store=store(state)?;let mut conn=store.conn.lock().map_err(|_|"db_lock_failed".to_string())?;let transaction=conn.transaction().map_err(|error|format!("device_sync_conflict_delete_begin_failed:{error}"))?;
        for id in &values { let reviewed=transaction.query_row("SELECT reviewed_at_ms IS NOT NULL FROM local_store_device_sync_conflicts WHERE tenant_id=?1 AND conflict_id=?2",params![tenant_id,id],|row|row.get::<_,bool>(0)).optional().map_err(|error|format!("device_sync_conflict_delete_check_failed:{error}"))?;if reviewed!=Some(true){return Err("device_sync_conflict_review_required".to_string())} }
        for id in &values { transaction.execute("DELETE FROM local_store_device_sync_conflicts WHERE tenant_id=?1 AND conflict_id=?2",params![tenant_id,id]).map_err(|error|format!("device_sync_conflict_delete_failed:{error}"))?; }
        transaction.commit().map_err(|error|format!("device_sync_conflict_delete_commit_failed:{error}"))?;drop(conn);Ok(json!({"ok":true,"deleted":values.len(),"stats":stats(&store,&tenant_id)?}))
    })();result.unwrap_or_else(|error|json!({"ok":false,"error":error}))
}

#[tauri::command]
pub(crate) fn export_device_sync_conflicts(state: tauri::State<'_, AppState>, tenant_id: String, conflict_ids: Vec<String>, target_path: String) -> Value {
    let result=(||->Result<Value,String>{let tenant_id=tenant(tenant_id)?;let values=ids(conflict_ids)?;let target=std::path::PathBuf::from(target_path);if target.as_os_str().is_empty(){return Err("device_sync_conflict_export_path_required".to_string())}let store=store(state)?;let mut exported=Vec::new();for id in values { let detail=get_detail_value(&store,&tenant_id,&id)?;exported.push(detail); }let payload=json!({"schemaVersion":1,"tenantId":tenant_id,"exportedAtMs":now_ms(),"conflicts":exported});fs::write(target,serde_json::to_vec_pretty(&payload).map_err(|error|format!("device_sync_conflict_export_encode_failed:{error}"))?).map_err(|error|format!("device_sync_conflict_export_write_failed:{error}"))?;Ok(json!({"ok":true,"count":exported.len()}))})();result.unwrap_or_else(|error|json!({"ok":false,"error":error}))
}

fn get_detail_value(store: &SqliteStore, tenant_id: &str, conflict_id: &str) -> Result<Value,String>{let row=store.conn.lock().map_err(|_|"db_lock_failed".to_string())?.query_row("SELECT table_name,record_key,losing_generation,winning_generation,payload_json,captured_at_ms,reviewed_at_ms FROM local_store_device_sync_conflicts WHERE tenant_id=?1 AND conflict_id=?2",params![tenant_id,conflict_id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,i64>(2)?,row.get::<_,i64>(3)?,row.get::<_,String>(4)?,row.get::<_,i64>(5)?,row.get::<_,Option<i64>>(6)?))).optional().map_err(|error|format!("device_sync_conflict_export_read_failed:{error}"))?.ok_or_else(||"device_sync_conflict_not_found".to_string())?;Ok(json!({"conflictId":conflict_id,"tableName":row.0,"recordKey":row.1,"losingGeneration":row.2,"winningGeneration":row.3,"losingPayload":serde_json::from_str::<Value>(&row.4).unwrap_or_else(|_|json!({})),"currentPayload":current_payload(store,tenant_id,&row.0,&row.1)?,"capturedAtMs":row.5,"reviewedAtMs":row.6}))}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_conflicts_start_unreviewed_and_seed_lifetime_without_resetting_it() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE local_store_device_sync_conflicts (conflict_id TEXT PRIMARY KEY,tenant_id TEXT NOT NULL,table_name TEXT NOT NULL,record_key TEXT NOT NULL,losing_generation INTEGER NOT NULL,winning_generation INTEGER NOT NULL,payload_json TEXT NOT NULL,captured_at_ms INTEGER NOT NULL);").unwrap();
        for index in 0..114 { conn.execute("INSERT INTO local_store_device_sync_conflicts VALUES (?1,'tenant-a','work_note_pages','[\"page-a\"]',1,2,'{}',100)", params![format!("conflict-{index}")]).unwrap(); }
        ensure_schema(&conn).unwrap();
        let (retained, unreviewed, lifetime): (i64,i64,i64) = conn.query_row("SELECT COUNT(*),SUM(reviewed_at_ms IS NULL),(SELECT lifetime_count FROM local_store_device_sync_conflict_stats WHERE tenant_id='tenant-a') FROM local_store_device_sync_conflicts WHERE tenant_id='tenant-a'", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
        assert_eq!((retained, unreviewed, lifetime), (114,114,114));
        conn.execute("UPDATE local_store_device_sync_conflict_stats SET lifetime_count=120 WHERE tenant_id='tenant-a'", []).unwrap();
        ensure_schema(&conn).unwrap();
        let lifetime: i64 = conn.query_row("SELECT lifetime_count FROM local_store_device_sync_conflict_stats WHERE tenant_id='tenant-a'", [], |row| row.get(0)).unwrap();
        assert_eq!(lifetime, 120);
    }

    #[test]
    fn conflict_preview_uses_human_record_identity_without_returning_the_full_payload() {
        let work_note = conflict_preview(r#"{"page_id":"internal-page","title":"학교업무 기억 노트","emoji":"📁","document_json":"[{\"text\":\"민감 원문\"}]"}"#);
        assert_eq!(work_note["title"], "학교업무 기억 노트");
        assert_eq!(work_note["emoji"], "📁");
        assert!(work_note.get("document_json").is_none());

        let student_draft = conflict_preview(r#"{"draft_id":"internal-draft","class_no":22,"student_code":"S22","payload_json":"{\"studentName\":\"김하늘\",\"title\":\"행동특성 초안\",\"content\":\"책임감 있게 참여함\"}"}"#);
        assert_eq!(student_draft["studentName"], "김하늘");
        assert_eq!(student_draft["classNo"], 22);
        assert_eq!(student_draft["title"], "행동특성 초안");
    }
}
