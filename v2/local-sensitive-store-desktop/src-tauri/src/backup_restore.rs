use super::*;
use rusqlite::{params_from_iter, types::Value as SqlValue, OptionalExtension};
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
struct RestoreMediaPlan {
    record_id: String,
    kind: &'static str,
    staged_path: PathBuf,
    target_path: PathBuf,
    rollback_path: PathBuf,
}

#[derive(Debug)]
struct AppliedRestoreMedia {
    target_path: PathBuf,
    rollback_path: Option<PathBuf>,
}

fn attached_table_exists(conn: &Connection, schema: &str, table_name: &str) -> Result<bool, String> {
    if schema != "restore" {
        return Err("db_schema_not_allowed".to_string());
    }
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM restore.sqlite_master WHERE type = 'table' AND name = ?1)",
        params![table_name],
        |row| row.get::<_, i64>(0),
    )
    .map(|value| value == 1)
    .map_err(|e| format!("db_attached_table_exists_failed:{e}"))
}

fn rollback_applied_media(applied: &[AppliedRestoreMedia]) {
    for item in applied.iter().rev() {
        let _ = fs::remove_file(&item.target_path);
        if let Some(rollback_path) = &item.rollback_path {
            if let Some(parent) = item.target_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::rename(rollback_path, &item.target_path);
        }
    }
}

fn apply_staged_media(plans: &[RestoreMediaPlan]) -> Result<Vec<AppliedRestoreMedia>, String> {
    let mut applied = Vec::new();
    for plan in plans {
        if let Some(parent) = plan.target_path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                rollback_applied_media(&applied);
                return Err(format!("restore_media_dir_failed:{error}"));
            }
        }
        let rollback_path = if plan.target_path.exists() {
            if let Some(parent) = plan.rollback_path.parent() {
                if let Err(error) = fs::create_dir_all(parent) {
                    rollback_applied_media(&applied);
                    return Err(format!("restore_media_rollback_dir_failed:{error}"));
                }
            }
            if let Err(error) = fs::rename(&plan.target_path, &plan.rollback_path) {
                rollback_applied_media(&applied);
                return Err(format!("restore_media_preserve_failed:{error}"));
            }
            Some(plan.rollback_path.clone())
        } else {
            None
        };
        if let Err(error) = fs::rename(&plan.staged_path, &plan.target_path) {
            if let Some(path) = &rollback_path {
                let _ = fs::rename(path, &plan.target_path);
            }
            rollback_applied_media(&applied);
            return Err(format!("restore_media_apply_failed:{error}"));
        }
        applied.push(AppliedRestoreMedia {
            target_path: plan.target_path.clone(),
            rollback_path,
        });
    }
    Ok(applied)
}

fn stage_restore_media(
    store: &SqliteStore,
    tenant_id: &str,
    manifest_path: &Path,
    manifest: &Value,
    allowed_media: Option<&HashSet<String>>,
    allowed_attachments: Option<&HashSet<String>>,
    force: bool,
) -> Result<(PathBuf, Vec<RestoreMediaPlan>, i64, i64), String> {
    let backup_id = manifest.get("backupId").and_then(Value::as_str).unwrap_or("backup");
    let staging_root = store
        .data_dir
        .join(".restore-staging")
        .join(format!("{}-{}", safe_segment(backup_id, "backup"), now_ms()));
    let staged_dir = staging_root.join("staged");
    let rollback_dir = staging_root.join("rollback");
    let current_media_timestamps = {
        let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let mut stmt = conn
            .prepare("SELECT media_id, archived_at_ms FROM board_media_files WHERE tenant_id = ?1")
            .map_err(|e| format!("restore_media_current_prepare_failed:{e}"))?;
        let rows = stmt
            .query_map(params![tenant_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
            .map_err(|e| format!("restore_media_current_query_failed:{e}"))?;
        let mut timestamps = HashMap::new();
        for row in rows {
            let (media_id, timestamp) = row.map_err(|e| format!("restore_media_current_row_failed:{e}"))?;
            timestamps.insert(media_id, timestamp);
        }
        timestamps
    };
    let records = manifest
        .get("media")
        .and_then(|media| media.get("records"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut plans = Vec::new();
    let mut media_missing = 0i64;
    for (index, record) in records.iter().enumerate() {
        let media_id = normalize_json_text(record.get("mediaId"), 220).replace(['/', '\\'], "_");
        let backup_relative = normalize_json_text(record.get("backupRelativePath"), 600);
        let local_path_text = normalize_json_text(record.get("localPath"), 600);
        let archived_at_ms = record.get("archivedAtMs").and_then(Value::as_i64).unwrap_or(0);
        if media_id.is_empty()
            || allowed_media.is_some_and(|allowed| !allowed.contains(&media_id))
            || (!force && current_media_timestamps.get(&media_id).copied().unwrap_or(i64::MIN) > archived_at_ms)
        {
            continue;
        }
        let Some(backup_relative_path) = safe_relative_path(&backup_relative) else {
            continue;
        };
        let Some(local_path) = safe_relative_path(&local_path_text) else {
            continue;
        };
        let source_path = manifest_path.parent().unwrap_or_else(|| Path::new(".")).join(backup_relative_path);
        if !source_path.is_file() {
            media_missing += 1;
            continue;
        }
        let staged_path = staged_dir.join(format!("{index}-{}", safe_segment(&media_id, "media")));
        if let Some(parent) = staged_path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                let _ = fs::remove_dir_all(&staging_root);
                return Err(format!("restore_media_stage_dir_failed:{error}"));
            }
        }
        if let Err(error) = fs::copy(&source_path, &staged_path) {
            let _ = fs::remove_dir_all(&staging_root);
            return Err(format!("restore_media_stage_failed:{error}"));
        }
        plans.push(RestoreMediaPlan {
            record_id: media_id,
            kind: "board_media",
            staged_path,
            target_path: store.data_dir.join(local_path),
            rollback_path: rollback_dir.join(format!("{index}")),
        });
    }
    let current_attachment_timestamps = {
        let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let mut statement = conn.prepare("SELECT attachment_id,updated_at_ms FROM work_note_attachments WHERE tenant_id=?1")
            .map_err(|e| format!("restore_work_note_attachment_current_prepare_failed:{e}"))?;
        let rows = statement.query_map(params![tenant_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
            .map_err(|e| format!("restore_work_note_attachment_current_query_failed:{e}"))?;
        let mut timestamps = HashMap::new();
        for row in rows {
            let (attachment_id, timestamp) = row.map_err(|e| format!("restore_work_note_attachment_current_row_failed:{e}"))?;
            timestamps.insert(attachment_id, timestamp);
        }
        timestamps
    };
    let attachment_records = manifest.get("workNoteAttachments").and_then(|value| value.get("records"))
        .and_then(Value::as_array).cloned().unwrap_or_default();
    let mut attachment_missing = 0i64;
    for (index, record) in attachment_records.iter().enumerate() {
        let attachment_id = normalize_json_text(record.get("attachmentId"), 180).replace(['/', '\\'], "_");
        let backup_relative = normalize_json_text(record.get("backupRelativePath"), 600);
        let local_path_text = normalize_json_text(record.get("localPath"), 600);
        let updated_at_ms = record.get("updatedAtMs").and_then(Value::as_i64).unwrap_or(0);
        if attachment_id.is_empty()
            || allowed_attachments.is_some_and(|allowed| !allowed.contains(&attachment_id))
            || (!force && current_attachment_timestamps.get(&attachment_id).copied().unwrap_or(i64::MIN) > updated_at_ms)
        { continue; }
        let Some(backup_relative_path) = safe_relative_path(&backup_relative) else { continue; };
        let Some(local_path) = safe_relative_path(&local_path_text) else { continue; };
        let source_path = manifest_path.parent().unwrap_or_else(|| Path::new(".")).join(backup_relative_path);
        if !source_path.is_file() { attachment_missing += 1; continue; }
        let staged_path = staged_dir.join(format!("attachment-{index}-{}", safe_segment(&attachment_id, "attachment")));
        if let Some(parent) = staged_path.parent() { fs::create_dir_all(parent).map_err(|e| format!("restore_work_note_attachment_stage_dir_failed:{e}"))?; }
        fs::copy(&source_path, &staged_path).map_err(|e| format!("restore_work_note_attachment_stage_failed:{e}"))?;
        plans.push(RestoreMediaPlan {
            record_id: attachment_id,
            kind: "work_note_attachment",
            staged_path,
            target_path: store.data_dir.join(local_path),
            rollback_path: rollback_dir.join(format!("attachment-{index}")),
        });
    }
    Ok((staging_root, plans, media_missing, attachment_missing))
}

fn restore_with_prebackup<F>(store: &SqliteStore, body: Value, create_safety_backup: F) -> Result<Value, String>
where
    F: FnOnce(&SqliteStore, String) -> Result<Value, String>,
{
    let preview = restore_preview(store, body.clone())?;
    let tenant_id = preview.get("tenantId").and_then(|value| value.as_str()).unwrap_or("").to_string();
    let safety_backup = create_safety_backup(store, tenant_id.clone())
        .map_err(|error| format!("pre_restore_backup_failed:{error}"))?;
    let safety_media = safety_backup.get("media").cloned().unwrap_or_else(|| json!({}));
    let safety_attachments = safety_backup.get("workNoteAttachments").cloned().unwrap_or_else(|| json!({}));
    if safety_backup.get("ok").and_then(Value::as_bool) != Some(true)
        || safety_media.get("missing").and_then(Value::as_i64).unwrap_or(0) > 0
        || safety_media.get("failed").and_then(Value::as_i64).unwrap_or(0) > 0
        || safety_attachments.get("missing").and_then(Value::as_i64).unwrap_or(0) > 0
        || safety_attachments.get("failed").and_then(Value::as_i64).unwrap_or(0) > 0
    {
        return Err("pre_restore_backup_failed:safety_backup_incomplete".to_string());
    }
    let manifest_path = PathBuf::from(preview.get("manifestPath").and_then(|value| value.as_str()).unwrap_or(""));
    let manifest = read_manifest(&manifest_path)?;
    let db_relative = manifest
        .get("db")
        .and_then(|db| db.get("relativePath"))
        .and_then(|value| value.as_str())
        .ok_or_else(|| "backup_db_required".to_string())?;
    let db_path = manifest_path.parent().unwrap_or_else(|| Path::new(".")).join(db_relative);
    let (staging_root, media_plans, media_missing, work_note_attachments_missing) =
        stage_restore_media(store, &tenant_id, &manifest_path, &manifest, None, None, false)?;
    let applied_media = match apply_staged_media(&media_plans) {
        Ok(applied) => applied,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging_root);
            return Err(error);
        }
    };
    let mut conn = match store.conn.lock() {
        Ok(conn) => conn,
        Err(_) => {
            rollback_applied_media(&applied_media);
            let _ = fs::remove_dir_all(&staging_root);
            return Err("db_lock_failed".to_string());
        }
    };
    if let Err(error) = conn.execute("ATTACH DATABASE ?1 AS restore", params![db_path.to_string_lossy().to_string()]) {
        rollback_applied_media(&applied_media);
        let _ = fs::remove_dir_all(&staging_root);
        return Err(format!("restore_db_attach_failed:{error}"));
    }
    let result = (|| -> Result<i64, String> {
        let transaction = conn.transaction().map_err(|e| format!("restore_transaction_begin_failed:{e}"))?;
        let mut imported = 0i64;
        for table in BACKUP_TABLES {
            if !table_exists(&transaction, table.name)? || !attached_table_exists(&transaction, "restore", table.name)? {
                continue;
            }
            let columns = table.columns.join(", ");
            let update_columns: Vec<&str> = table
                .columns
                .iter()
                .copied()
                .filter(|column| !table.key_columns.contains(column))
                .collect();
            let update_set = update_columns
                .iter()
                .map(|column| format!("{column} = excluded.{column}"))
                .collect::<Vec<String>>()
                .join(", ");
            let sql = format!(
                "INSERT INTO main.{name} ({columns})
                 SELECT {columns} FROM restore.{name} WHERE tenant_id = ?1
                 ON CONFLICT({keys}) DO UPDATE SET {update_set}
                 WHERE excluded.{timestamp} >= main.{name}.{timestamp}",
                name = table.name,
                columns = columns,
                keys = table.key_columns.join(", "),
                update_set = update_set,
                timestamp = table.timestamp_column
            );
            imported += transaction
                .execute(&sql, params![tenant_id])
                .map_err(|e| format!("restore_table_merge_failed:{}:{e}", table.name))? as i64;
        }
        for plan in &media_plans {
            let local_path = plan
                .target_path
                .strip_prefix(&store.data_dir)
                .map_err(|_| "restore_media_target_outside_store".to_string())?
                .to_string_lossy()
                .to_string();
            let sql = if plan.kind == "work_note_attachment" {
                "UPDATE work_note_attachments SET local_path = ?1 WHERE tenant_id = ?2 AND attachment_id = ?3"
            } else {
                "UPDATE board_media_files SET local_path = ?1 WHERE tenant_id = ?2 AND media_id = ?3"
            };
            transaction.execute(sql, params![local_path, tenant_id, plan.record_id])
            .map_err(|e| format!("restore_media_path_update_failed:{e}"))?;
        }
        transaction
            .execute("DELETE FROM work_note_pages_fts WHERE tenant_id = ?1", params![tenant_id])
            .map_err(|e| format!("restore_work_note_fts_delete_failed:{e}"))?;
        transaction
            .execute(
                "INSERT INTO work_note_pages_fts (tenant_id, page_id, title, markdown) SELECT tenant_id, page_id, title, markdown FROM work_note_pages WHERE tenant_id = ?1",
                params![tenant_id],
            )
            .map_err(|e| format!("restore_work_note_fts_insert_failed:{e}"))?;
        transaction.commit().map_err(|e| format!("restore_transaction_commit_failed:{e}"))?;
        Ok(imported)
    })();
    let _ = conn.execute_batch("DETACH DATABASE restore");
    let imported = match result {
        Ok(imported) => imported,
        Err(error) => {
            rollback_applied_media(&applied_media);
            let _ = fs::remove_dir_all(&staging_root);
            return Err(error);
        }
    };
    let media_restored = media_plans.iter().filter(|plan| plan.kind == "board_media").count() as i64;
    let work_note_attachments_restored = media_plans.iter().filter(|plan| plan.kind == "work_note_attachment").count() as i64;
    let _ = fs::remove_dir_all(&staging_root);
    Ok(json!({
        "ok": true,
        "tenantId": tenant_id,
        "backupId": preview.get("backupId").cloned().unwrap_or(Value::Null),
        "manifestPath": manifest_path.to_string_lossy(),
        "imported": imported,
        "mediaRestored": media_restored,
        "mediaMissing": media_missing,
        "workNoteAttachmentsRestored": work_note_attachments_restored,
        "workNoteAttachmentsMissing": work_note_attachments_missing,
        "safetyBackup": safety_backup
    }))
}

pub(super) fn restore(store: &SqliteStore, body: Value) -> Result<Value, String> {
    restore_with_prebackup(store, body, |store, tenant_id| {
        run_with_kind(store, tenant_id, "pre_restore", None)
    })
}

#[derive(Clone, Debug)]
struct SyncRecord {
    table_name: String,
    record_key: String,
    key_values: Vec<SqlValue>,
    changed_generation: i64,
    record_version: i64,
    tombstone: bool,
}

fn json_key_value(value: &Value) -> Result<SqlValue, String> {
    match value {
        Value::String(value) => Ok(SqlValue::Text(value.clone())),
        Value::Number(value) if value.is_i64() => Ok(SqlValue::Integer(value.as_i64().unwrap_or(0))),
        Value::Number(value) if value.is_u64() && value.as_u64().unwrap_or(0) <= i64::MAX as u64 => {
            Ok(SqlValue::Integer(value.as_u64().unwrap_or(0) as i64))
        }
        _ => Err("backup_sync_record_key_invalid".to_string()),
    }
}

fn parse_sync_records(manifest: &Value, generation: i64) -> Result<Vec<SyncRecord>, String> {
    let records = manifest
        .get("sync")
        .and_then(|sync| sync.get("records"))
        .and_then(Value::as_array)
        .ok_or_else(|| "backup_sync_records_required".to_string())?;
    let mut parsed = Vec::with_capacity(records.len());
    for record in records {
        let table_name = normalize_json_text(record.get("table"), 120);
        let table = BACKUP_TABLES
            .iter()
            .find(|table| table.name == table_name && table.name != "cloud_sync_runs")
            .ok_or_else(|| "backup_sync_table_invalid".to_string())?;
        let key = record
            .get("recordKey")
            .and_then(Value::as_array)
            .ok_or_else(|| "backup_sync_record_key_invalid".to_string())?;
        let expected = table
            .key_columns
            .iter()
            .filter(|column| **column != "tenant_id")
            .count();
        if key.len() != expected {
            return Err("backup_sync_record_key_invalid".to_string());
        }
        let changed_generation = record.get("changedGeneration").and_then(Value::as_i64).unwrap_or(0);
        if changed_generation < 1 || changed_generation > generation {
            return Err("backup_sync_changed_generation_invalid".to_string());
        }
        parsed.push(SyncRecord {
            table_name,
            record_key: serde_json::to_string(key)
                .map_err(|e| format!("backup_sync_record_key_encode_failed:{e}"))?,
            key_values: key.iter().map(json_key_value).collect::<Result<Vec<_>, _>>()?,
            changed_generation,
            record_version: record.get("recordVersion").and_then(Value::as_i64).unwrap_or(1).max(1),
            tombstone: record.get("tombstone").and_then(Value::as_bool).unwrap_or(false),
        });
    }
    Ok(parsed)
}

fn record_where(table: &BackupTable) -> String {
    table
        .key_columns
        .iter()
        .copied()
        .filter(|column| *column != "tenant_id")
        .enumerate()
        .map(|(index, column)| format!("{column} = ?{}", index + 2))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn record_params(tenant_id: &str, record: &SyncRecord) -> Vec<SqlValue> {
    let mut values = Vec::with_capacity(record.key_values.len() + 1);
    values.push(SqlValue::Text(tenant_id.to_string()));
    values.extend(record.key_values.iter().cloned());
    values
}

fn row_json_expression(table: &BackupTable, alias: &str) -> String {
    table
        .columns
        .iter()
        .flat_map(|column| [format!("'{column}'"), format!("{alias}.{column}")])
        .collect::<Vec<_>>()
        .join(", ")
}

fn current_record_json(
    transaction: &rusqlite::Transaction<'_>,
    table: &BackupTable,
    tenant_id: &str,
    record: &SyncRecord,
) -> Result<Option<String>, String> {
    let sql = format!(
        "SELECT json_object({json}) FROM main.{name} AS current
         WHERE current.tenant_id = ?1 AND {where_clause}",
        json = row_json_expression(table, "current"),
        name = table.name,
        where_clause = record_where(table),
    );
    transaction
        .query_row(&sql, params_from_iter(record_params(tenant_id, record)), |row| row.get(0))
        .optional()
        .map_err(|e| format!("restore_sync_current_record_failed:{}:{e}", table.name))
}

fn attached_record_json(
    transaction: &rusqlite::Transaction<'_>,
    table: &BackupTable,
    tenant_id: &str,
    record: &SyncRecord,
) -> Result<Option<String>, String> {
    let sql = format!(
        "SELECT json_object({json}) FROM restore.{name} AS incoming
         WHERE incoming.tenant_id = ?1 AND {where_clause}",
        json = row_json_expression(table, "incoming"),
        name = table.name,
        where_clause = record_where(table),
    );
    transaction
        .query_row(&sql, params_from_iter(record_params(tenant_id, record)), |row| row.get(0))
        .optional()
        .map_err(|e| format!("restore_sync_attached_record_failed:{}:{e}", table.name))
}

fn local_record_is_dirty(
    transaction: &rusqlite::Transaction<'_>,
    tenant_id: &str,
    record: &SyncRecord,
) -> Result<bool, String> {
    transaction
        .query_row(
            "SELECT changed_generation = 0 FROM local_store_device_sync_records
             WHERE tenant_id = ?1 AND table_name = ?2 AND record_key = ?3",
            params![tenant_id, record.table_name, record.record_key],
            |row| row.get::<_, bool>(0),
        )
        .optional()
        .map(|value| value.unwrap_or(false))
        .map_err(|e| format!("restore_sync_dirty_check_failed:{e}"))
}

fn archive_conflict(
    transaction: &rusqlite::Transaction<'_>,
    tenant_id: &str,
    record: &SyncRecord,
    losing_generation: i64,
    winning_generation: i64,
    payload_json: &str,
) -> Result<(), String> {
    transaction
        .execute(
            "INSERT INTO local_store_device_sync_conflicts (
               conflict_id, tenant_id, table_name, record_key, losing_generation,
               winning_generation, payload_json, captured_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                crate::random_url_token(),
                tenant_id,
                record.table_name,
                record.record_key,
                losing_generation,
                winning_generation,
                payload_json,
                now_ms(),
            ],
        )
        .map_err(|e| format!("restore_sync_conflict_archive_failed:{e}"))?;
    Ok(())
}

fn upsert_sync_record(
    transaction: &rusqlite::Transaction<'_>,
    tenant_id: &str,
    record: &SyncRecord,
) -> Result<(), String> {
    transaction
        .execute(
            "INSERT INTO local_store_device_sync_records (
               tenant_id, table_name, record_key, dirty_base_generation,
               record_version, changed_generation, tombstone, changed_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?4, ?6, ?7)
             ON CONFLICT(tenant_id, table_name, record_key) DO UPDATE SET
               dirty_base_generation = excluded.dirty_base_generation,
               record_version = MAX(record_version, excluded.record_version),
               changed_generation = excluded.changed_generation,
               tombstone = excluded.tombstone,
               changed_at_ms = excluded.changed_at_ms",
            params![
                tenant_id,
                record.table_name,
                record.record_key,
                record.changed_generation,
                record.record_version,
                i64::from(record.tombstone),
                now_ms(),
            ],
        )
        .map_err(|e| format!("restore_sync_record_state_failed:{e}"))?;
    Ok(())
}

pub(super) fn restore_generation(
    store: &SqliteStore,
    tenant_id: &str,
    manifest_path: &Path,
    generation: i64,
    latest_status: &str,
    force_all: bool,
) -> Result<Value, String> {
    let manifest = read_manifest(manifest_path)?;
    if manifest.get("version").and_then(Value::as_i64) != Some(3)
        || manifest.get("tenantId").and_then(Value::as_str) != Some(tenant_id)
        || generation < 1
    {
        return Err("backup_sync_manifest_invalid".to_string());
    }
    seed_sync_records(store, tenant_id)?;
    let state = local_sync_state(store, tenant_id)?;
    if generation <= state.applied_generation {
        return Ok(json!({ "ok": true, "applied": false, "generation": state.applied_generation }));
    }
    let records = parse_sync_records(&manifest, generation)?;
    let applicable = records
        .iter()
        .filter(|record| force_all || state.applied_generation == 0 || record.changed_generation > state.applied_generation)
        .cloned()
        .collect::<Vec<_>>();
    let allowed_media = applicable
        .iter()
        .filter(|record| record.table_name == "board_media_files" && !record.tombstone)
        .filter_map(|record| record.key_values.first())
        .filter_map(|value| match value { SqlValue::Text(value) => Some(value.clone()), _ => None })
        .collect::<HashSet<_>>();
    let allowed_attachments = applicable
        .iter()
        .filter(|record| record.table_name == "work_note_attachments" && !record.tombstone)
        .filter_map(|record| record.key_values.first())
        .filter_map(|value| match value { SqlValue::Text(value) => Some(value.clone()), _ => None })
        .collect::<HashSet<_>>();
    let safety_backup = run_with_kind(store, tenant_id.to_string(), "pre_restore", None)
        .map_err(|error| format!("pre_restore_backup_failed:{error}"))?;
    let safety_media = safety_backup.get("media").cloned().unwrap_or_else(|| json!({}));
    let safety_attachments = safety_backup
        .get("workNoteAttachments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if safety_backup.get("ok").and_then(Value::as_bool) != Some(true)
        || safety_media.get("missing").and_then(Value::as_i64).unwrap_or(0) > 0
        || safety_media.get("failed").and_then(Value::as_i64).unwrap_or(0) > 0
        || safety_attachments.get("missing").and_then(Value::as_i64).unwrap_or(0) > 0
        || safety_attachments.get("failed").and_then(Value::as_i64).unwrap_or(0) > 0
    {
        return Err("pre_restore_backup_failed:safety_backup_incomplete".to_string());
    }
    let db_relative = manifest
        .get("db")
        .and_then(|db| db.get("relativePath"))
        .and_then(Value::as_str)
        .ok_or_else(|| "backup_db_required".to_string())?;
    let db_path = manifest_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(db_relative);
    let (staging_root, media_plans, media_missing, attachment_missing) = stage_restore_media(
        store,
        tenant_id,
        manifest_path,
        &manifest,
        Some(&allowed_media),
        Some(&allowed_attachments),
        true,
    )?;
    if media_missing > 0 || attachment_missing > 0 {
        let _ = fs::remove_dir_all(&staging_root);
        return Err("backup_sync_artifact_missing".to_string());
    }
    let applied_media = match apply_staged_media(&media_plans) {
        Ok(applied) => applied,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging_root);
            return Err(error);
        }
    };
    let mut conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    if let Err(error) = conn.execute(
        "ATTACH DATABASE ?1 AS restore",
        params![db_path.to_string_lossy().to_string()],
    ) {
        rollback_applied_media(&applied_media);
        let _ = fs::remove_dir_all(&staging_root);
        return Err(format!("restore_db_attach_failed:{error}"));
    }
    let result = (|| -> Result<(i64, i64, Vec<PathBuf>), String> {
        let transaction = conn
            .transaction()
            .map_err(|e| format!("restore_transaction_begin_failed:{e}"))?;
        transaction
            .execute(
                "UPDATE local_store_device_sync_state SET applying = 1 WHERE tenant_id = ?1",
                params![tenant_id],
            )
            .map_err(|e| format!("restore_sync_applying_failed:{e}"))?;
        let mut imported = 0i64;
        let mut conflicts = 0i64;
        let mut deleted_files = Vec::new();
        for record in &applicable {
            let table = BACKUP_TABLES
                .iter()
                .find(|table| table.name == record.table_name)
                .ok_or_else(|| "backup_sync_table_invalid".to_string())?;
            if !table_exists(&transaction, table.name)?
                || !attached_table_exists(&transaction, "restore", table.name)?
            {
                continue;
            }
            let current = current_record_json(&transaction, table, tenant_id, record)?;
            let incoming = if record.tombstone {
                None
            } else {
                Some(
                    attached_record_json(&transaction, table, tenant_id, record)?
                        .ok_or_else(|| "backup_sync_record_missing".to_string())?,
                )
            };
            if local_record_is_dirty(&transaction, tenant_id, record)?
                && current.is_some()
                && current.as_ref() != incoming.as_ref()
            {
                archive_conflict(
                    &transaction,
                    tenant_id,
                    record,
                    state.applied_generation,
                    generation,
                    current.as_deref().unwrap_or("{}"),
                )?;
                conflicts += 1;
            }
            let values = record_params(tenant_id, record);
            let where_clause = record_where(table);
            if record.tombstone {
                if matches!(table.name, "board_media_files" | "work_note_attachments") {
                    let path_sql = format!(
                        "SELECT local_path FROM main.{name} WHERE tenant_id = ?1 AND {where_clause}",
                        name = table.name,
                    );
                    if let Some(relative) = transaction
                        .query_row(&path_sql, params_from_iter(values.clone()), |row| row.get::<_, String>(0))
                        .optional()
                        .map_err(|e| format!("restore_sync_deleted_path_failed:{e}"))?
                    {
                        if let Some(relative) = safe_relative_path(&relative) {
                            deleted_files.push(store.data_dir.join(relative));
                        }
                    }
                }
                let sql = format!(
                    "DELETE FROM main.{name} WHERE tenant_id = ?1 AND {where_clause}",
                    name = table.name,
                );
                imported += transaction
                    .execute(&sql, params_from_iter(values))
                    .map_err(|e| format!("restore_sync_delete_failed:{}:{e}", table.name))?
                    as i64;
            } else {
                let columns = table.columns.join(", ");
                let update_set = table
                    .columns
                    .iter()
                    .copied()
                    .filter(|column| !table.key_columns.contains(column))
                    .map(|column| format!("{column} = excluded.{column}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!(
                    "INSERT INTO main.{name} ({columns})
                     SELECT {columns} FROM restore.{name} WHERE tenant_id = ?1 AND {where_clause}
                     ON CONFLICT({keys}) DO UPDATE SET {update_set}",
                    name = table.name,
                    keys = table.key_columns.join(", "),
                );
                imported += transaction
                    .execute(&sql, params_from_iter(values))
                    .map_err(|e| format!("restore_sync_merge_failed:{}:{e}", table.name))?
                    as i64;
            }
            upsert_sync_record(&transaction, tenant_id, record)?;
        }
        for plan in &media_plans {
            let relative = plan
                .target_path
                .strip_prefix(&store.data_dir)
                .map_err(|_| "restore_media_target_outside_store".to_string())?
                .to_string_lossy()
                .to_string();
            let sql = if plan.kind == "work_note_attachment" {
                "UPDATE work_note_attachments SET local_path = ?1 WHERE tenant_id = ?2 AND attachment_id = ?3"
            } else {
                "UPDATE board_media_files SET local_path = ?1 WHERE tenant_id = ?2 AND media_id = ?3"
            };
            transaction
                .execute(sql, params![relative, tenant_id, plan.record_id])
                .map_err(|e| format!("restore_media_path_update_failed:{e}"))?;
        }
        transaction
            .execute("DELETE FROM work_note_pages_fts WHERE tenant_id = ?1", params![tenant_id])
            .map_err(|e| format!("restore_work_note_fts_delete_failed:{e}"))?;
        transaction
            .execute(
                "INSERT INTO work_note_pages_fts (tenant_id, page_id, title, markdown)
                 SELECT tenant_id, page_id, title, markdown FROM work_note_pages WHERE tenant_id = ?1",
                params![tenant_id],
            )
            .map_err(|e| format!("restore_work_note_fts_insert_failed:{e}"))?;
        let remaining_dirty: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM local_store_device_sync_records
                 WHERE tenant_id = ?1 AND changed_generation = 0",
                params![tenant_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("restore_sync_dirty_count_failed:{e}"))?;
        transaction
            .execute(
                "UPDATE local_store_device_sync_state SET
                   applied_generation = ?2,
                   latest_generation = MAX(latest_generation, ?2),
                   latest_status = ?3,
                   first_dirty_at_ms = CASE WHEN ?4 = 0 THEN NULL ELSE first_dirty_at_ms END,
                   last_dirty_at_ms = CASE WHEN ?4 = 0 THEN NULL ELSE last_dirty_at_ms END,
                   last_success_at_ms = ?5,
                   last_error = '',
                   applying = 0
                 WHERE tenant_id = ?1",
                params![tenant_id, generation, latest_status, remaining_dirty, now_ms()],
            )
            .map_err(|e| format!("restore_sync_state_update_failed:{e}"))?;
        transaction
            .commit()
            .map_err(|e| format!("restore_sync_commit_failed:{e}"))?;
        Ok((imported, conflicts, deleted_files))
    })();
    let _ = conn.execute_batch("DETACH DATABASE restore");
    let (imported, conflicts, deleted_files) = match result {
        Ok(value) => value,
        Err(error) => {
            rollback_applied_media(&applied_media);
            let _ = fs::remove_dir_all(&staging_root);
            return Err(error);
        }
    };
    for path in deleted_files {
        let _ = fs::remove_file(path);
    }
    let _ = fs::remove_dir_all(&staging_root);
    Ok(json!({
        "ok": true,
        "applied": true,
        "generation": generation,
        "imported": imported,
        "conflicts": conflicts,
        "safetyBackup": safety_backup,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::random_url_token;
    use std::io::{Cursor, Read};

    fn test_store() -> (PathBuf, PathBuf, SqliteStore) {
        let base = std::env::temp_dir().join(format!("onlineclass-backup-restore-test-{}", random_url_token()));
        let store_dir = base.join("store");
        let backup_root = base.join("backup-root");
        fs::create_dir_all(&store_dir).expect("create store directory");
        fs::create_dir_all(&backup_root).expect("create backup directory");
        let store = SqliteStore::open(store_dir.join("test.sqlite")).expect("open test store");
        (base, backup_root, store)
    }

    fn observation(store: &SqliteStore, doc_id: &str, note: &str, updated_at_ms: i64) {
        store.upsert_observation(json!({
            "tenantId": "tenant-a", "docId": doc_id, "dateKey": "2026-08-04", "period": 1,
            "studentCode": "1", "observation": note, "updatedAtMs": updated_at_ms
        })).expect("upsert observation");
    }

    fn observation_row(store: &SqliteStore, doc_id: &str) -> Option<(String, i64)> {
        let conn = store.conn.lock().expect("lock store");
        conn.query_row(
            "SELECT payload_json, updated_at_ms FROM lesson_observations WHERE tenant_id = 'tenant-a' AND doc_id = ?1",
            params![doc_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).ok()
    }

    #[test]
    fn generation_merge_keeps_other_keys_archives_same_key_and_applies_tombstone() {
        let base = std::env::temp_dir().join(format!(
            "onlineclass-generation-merge-test-{}",
            random_url_token()
        ));
        let source_dir = base.join("source");
        let target_dir = base.join("target");
        let backup_root = base.join("onedrive");
        fs::create_dir_all(&source_dir).expect("create source directory");
        fs::create_dir_all(&target_dir).expect("create target directory");
        fs::create_dir_all(&backup_root).expect("create backup root");
        let source = SqliteStore::open(source_dir.join("source.sqlite")).expect("open source store");
        let target = SqliteStore::open(target_dir.join("target.sqlite")).expect("open target store");
        set_folder(
            &source,
            "tenant-a".to_string(),
            backup_root.to_string_lossy().to_string(),
        )
        .expect("configure source backup");
        set_folder(
            &target,
            "tenant-a".to_string(),
            backup_root.to_string_lossy().to_string(),
        )
        .expect("configure target backup");

        observation(&source, "shared", "source generation one", 100);
        let generation_one = run_with_kind(&source, "tenant-a".to_string(), "auto_sync", Some(1))
            .expect("create generation one");
        let content_one = tenant_content_sha256(&source, "tenant-a").expect("source content root");
        mark_sync_published(
            &source,
            "tenant-a",
            1,
            Path::new(generation_one["manifestPath"].as_str().expect("manifest path")),
            &content_one,
            "announced",
        )
            .expect("mark generation one published");

        observation(&target, "shared", "target unsynced edit", 200);
        observation(&target, "target-only", "preserve me", 200);
        let applied = restore_generation(
            &target,
            "tenant-a",
            Path::new(generation_one["manifestPath"].as_str().expect("manifest path")),
            1,
            "announced",
            false,
        )
        .expect("apply generation one");
        assert_eq!(applied["conflicts"], 1);
        assert!(observation_row(&target, "shared")
            .expect("shared target row")
            .0
            .contains("source generation one"));
        assert!(observation_row(&target, "target-only")
            .expect("target-only row")
            .0
            .contains("preserve me"));

        source
            .conn
            .lock()
            .expect("lock source")
            .execute(
                "DELETE FROM lesson_observations WHERE tenant_id = 'tenant-a' AND doc_id = 'shared'",
                [],
            )
            .expect("delete source shared row");
        let generation_two = run_with_kind(&source, "tenant-a".to_string(), "auto_sync", Some(2))
            .expect("create generation two");
        restore_generation(
            &target,
            "tenant-a",
            Path::new(generation_two["manifestPath"].as_str().expect("manifest path")),
            2,
            "announced",
            false,
        )
        .expect("apply generation two tombstone");
        assert!(observation_row(&target, "shared").is_none());
        assert!(observation_row(&target, "target-only").is_some());
        let tombstone: i64 = target
            .conn
            .lock()
            .expect("lock target")
            .query_row(
                "SELECT tombstone FROM local_store_device_sync_records
                 WHERE tenant_id = 'tenant-a' AND table_name = 'lesson_observations'
                   AND record_key = '[\"shared\"]'",
                [],
                |row| row.get(0),
            )
            .expect("read tombstone");
        assert_eq!(tombstone, 1);
        restore_generation(
            &target,
            "tenant-a",
            Path::new(generation_one["manifestPath"].as_str().expect("generation one path")),
            3,
            "verified",
            true,
        )
        .expect("apply verified recovery generation");
        assert!(observation_row(&target, "shared")
            .expect("recovered shared row")
            .0
            .contains("source generation one"));
        assert!(observation_row(&target, "target-only").is_some());
        drop(source);
        drop(target);
        fs::remove_dir_all(base).expect("remove generation merge test directory");
    }

    #[test]
    fn generation_snapshot_is_atomic_relative_and_rejects_tampered_artifact() {
        let (base, backup_root, store) = test_store();
        set_folder(&store, "tenant-a".to_string(), backup_root.to_string_lossy().to_string())
            .expect("set backup folder");
        observation(&store, "digest", "verify me", 100);
        let snapshot = run_with_kind(&store, "tenant-a".to_string(), "auto_sync", Some(1))
            .expect("create generation snapshot");
        let manifest_path = PathBuf::from(snapshot["manifestPath"].as_str().expect("manifest path"));
        let manifest = read_manifest(&manifest_path).expect("read manifest");
        assert_eq!(manifest["version"], 3);
        assert_eq!(manifest["db"]["relativePath"], "db/local-sensitive.sqlite");
        assert!(manifest["db"].get("absolutePath").is_none());
        assert!(manifest_path.parent().expect("snapshot directory").join("commit.json").is_file());
        let artifact_root = snapshot["artifactSetSha256"].as_str().expect("artifact root");
        assert!(find_and_verify_generation(&store, "tenant-a", 1, artifact_root)
            .expect("verify snapshot").is_some());

        let database_path = manifest_path.parent().expect("snapshot directory").join("db/local-sensitive.sqlite");
        fs::write(database_path, b"tampered").expect("tamper snapshot database");
        assert_eq!(
            find_and_verify_generation(&store, "tenant-a", 1, artifact_root).expect_err("tamper must fail"),
            "backup_artifact_digest_mismatch"
        );
        drop(store);
        fs::remove_dir_all(base).expect("remove digest test directory");
    }

    #[test]
    fn auto_sync_retention_keeps_recent_ten_and_never_prunes_manual_backup() {
        let (base, backup_root, store) = test_store();
        set_folder(&store, "tenant-a".to_string(), backup_root.to_string_lossy().to_string())
            .expect("set backup folder");
        observation(&store, "retention", "keep snapshots", 100);
        run_now(&store, "tenant-a".to_string()).expect("create manual backup");
        for generation in 1..=12 {
            run_with_kind(&store, "tenant-a".to_string(), "auto_sync", Some(generation))
                .expect("create auto sync snapshot");
            std::thread::sleep(Duration::from_millis(2));
        }
        let tenant_dir = tenant_backup_dir(&backup_root, "tenant-a");
        let manifests = manifest_paths_in_dir(&tenant_dir).expect("list manifests");
        let kinds = manifests.iter().map(|path| {
            read_manifest(path).expect("read retained manifest")["kind"].as_str().unwrap_or("").to_string()
        }).collect::<Vec<_>>();
        assert_eq!(kinds.iter().filter(|kind| kind.as_str() == "manual").count(), 1);
        assert_eq!(kinds.iter().filter(|kind| kind.as_str() == "auto_sync").count(), 10);
        drop(store);
        fs::remove_dir_all(base).expect("remove retention test directory");
    }

    #[test]
    fn restore_creates_safety_backup_and_keeps_newer_current_rows() {
        let (base, backup_root, store) = test_store();
        set_folder(&store, "tenant-a".to_string(), backup_root.to_string_lossy().to_string()).expect("set backup folder");
        observation(&store, "missing-current", "backup copy", 100);
        observation(&store, "newer-current", "backup old", 100);
        let selected = run_now(&store, "tenant-a".to_string()).expect("create selected backup");
        let manifest_path = selected.get("manifestPath").and_then(Value::as_str).expect("selected manifest").to_string();
        observation(&store, "newer-current", "current new", 200);
        store.conn.lock().expect("lock store").execute(
            "DELETE FROM lesson_observations WHERE tenant_id = 'tenant-a' AND doc_id = 'missing-current'", [],
        ).expect("delete current row");

        let restored = restore(&store, json!({ "tenantId": "tenant-a", "manifestPath": manifest_path })).expect("restore backup");
        let restored_missing = observation_row(&store, "missing-current").expect("restored missing row");
        assert_eq!(restored_missing.1, 100);
        assert!(restored_missing.0.contains("backup copy"));
        let kept_newer = observation_row(&store, "newer-current").expect("kept current row");
        assert_eq!(kept_newer.1, 200);
        assert!(kept_newer.0.contains("current new"));
        assert!(restored.get("safetyBackup").and_then(Value::as_object).is_some());
        assert_eq!(
            list_backups(&store, "tenant-a".to_string(), 10).expect("list backups")
                .get("backups").and_then(Value::as_array).map(Vec::len),
            Some(2)
        );
        fs::remove_dir_all(base).expect("remove test directory");
    }

    #[test]
    fn work_note_attachment_file_is_backed_up_and_restored() {
        let (base, backup_root, store) = test_store();
        set_folder(&store, "tenant-a".to_string(), backup_root.to_string_lossy().to_string()).expect("set backup folder");
        store.upsert_work_note(json!({
            "tenantId": "tenant-a", "pageId": "page-a", "title": "첨부 노트", "blocks": [], "markdown": "# 첨부 노트"
        })).expect("create work note");
        crate::work_note_attachments::save(
            &store,
            "tenant-a".to_string(),
            "attachment-a".to_string(),
            "page-a".to_string(),
            "block-a".to_string(),
            "자료.pdf".to_string(),
            "application/pdf".to_string(),
            &mut Cursor::new(b"pdf-fixture".to_vec()),
        ).expect("save attachment");
        let selected = run_now(&store, "tenant-a".to_string()).expect("backup work note attachment");
        assert_eq!(selected.pointer("/counts/workNoteAttachmentCount").and_then(Value::as_i64), Some(1));
        store.delete_work_note("tenant-a".to_string(), "page-a".to_string()).expect("delete page and attachment");
        restore(&store, json!({
            "tenantId": "tenant-a",
            "manifestPath": selected.get("manifestPath").and_then(Value::as_str).unwrap_or("")
        })).expect("restore attachment");
        let mut restored = crate::work_note_attachments::open(&store, "tenant-a".to_string(), "attachment-a".to_string()).expect("open restored attachment");
        let mut bytes = Vec::new();
        restored.file.read_to_end(&mut bytes).expect("read restored attachment");
        assert_eq!(bytes, b"pdf-fixture");
        fs::remove_dir_all(base).expect("remove test directory");
    }

    #[test]
    fn restore_aborts_before_merge_when_safety_backup_fails() {
        let (base, backup_root, store) = test_store();
        set_folder(&store, "tenant-a".to_string(), backup_root.to_string_lossy().to_string()).expect("set backup folder");
        observation(&store, "guarded", "backup old", 100);
        let selected = run_now(&store, "tenant-a".to_string()).expect("create selected backup");
        observation(&store, "guarded", "current new", 200);
        let body = json!({
            "tenantId": "tenant-a",
            "manifestPath": selected.get("manifestPath").and_then(Value::as_str).unwrap_or("")
        });
        let error = restore_with_prebackup(&store, body.clone(), |_store, _tenant_id| Err("forced_failure".to_string()))
            .expect_err("restore must abort");
        assert_eq!(error, "pre_restore_backup_failed:forced_failure");
        let incomplete_error = restore_with_prebackup(&store, body, |_store, _tenant_id| {
            Ok(json!({ "ok": false, "media": { "failed": 1 } }))
        }).expect_err("incomplete safety backup must abort");
        assert_eq!(incomplete_error, "pre_restore_backup_failed:safety_backup_incomplete");
        let current = observation_row(&store, "guarded").expect("current row remains");
        assert_eq!(current.1, 200);
        assert!(current.0.contains("current new"));
        fs::remove_dir_all(base).expect("remove test directory");
    }

    #[test]
    fn media_apply_failure_restores_already_replaced_files() {
        let base = std::env::temp_dir().join(format!("onlineclass-backup-media-rollback-test-{}", random_url_token()));
        let staged = base.join("staged");
        let target = base.join("target");
        let rollback = base.join("rollback");
        fs::create_dir_all(&staged).expect("create staged directory");
        fs::create_dir_all(&target).expect("create target directory");
        fs::write(staged.join("one"), b"new-one").expect("write staged file");
        fs::write(target.join("one"), b"old-one").expect("write target file");
        let plans = vec![
            RestoreMediaPlan {
                record_id: "one".to_string(), kind: "board_media", staged_path: staged.join("one"),
                target_path: target.join("one"), rollback_path: rollback.join("one"),
            },
            RestoreMediaPlan {
                record_id: "two".to_string(), kind: "board_media", staged_path: staged.join("missing"),
                target_path: target.join("two"), rollback_path: rollback.join("two"),
            },
        ];
        let error = apply_staged_media(&plans).expect_err("second media apply must fail");
        assert!(error.starts_with("restore_media_apply_failed:"));
        assert_eq!(fs::read(target.join("one")).expect("read restored target"), b"old-one");
        assert!(!target.join("two").exists());
        fs::remove_dir_all(base).expect("remove test directory");
    }
}
