use super::*;
use std::collections::HashMap;

#[derive(Debug)]
struct RestoreMediaPlan {
    media_id: String,
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
) -> Result<(PathBuf, Vec<RestoreMediaPlan>, i64), String> {
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
            || current_media_timestamps.get(&media_id).copied().unwrap_or(i64::MIN) > archived_at_ms
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
            media_id,
            staged_path,
            target_path: store.data_dir.join(local_path),
            rollback_path: rollback_dir.join(format!("{index}")),
        });
    }
    Ok((staging_root, plans, media_missing))
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
    if safety_backup.get("ok").and_then(Value::as_bool) != Some(true)
        || safety_media.get("missing").and_then(Value::as_i64).unwrap_or(0) > 0
        || safety_media.get("failed").and_then(Value::as_i64).unwrap_or(0) > 0
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
    let (staging_root, media_plans, media_missing) = stage_restore_media(store, &tenant_id, &manifest_path, &manifest)?;
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
            transaction.execute(
                "UPDATE board_media_files SET local_path = ?1 WHERE tenant_id = ?2 AND media_id = ?3",
                params![local_path, tenant_id, plan.media_id],
            )
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
    let media_restored = media_plans.len() as i64;
    let _ = fs::remove_dir_all(&staging_root);
    Ok(json!({
        "ok": true,
        "tenantId": tenant_id,
        "backupId": preview.get("backupId").cloned().unwrap_or(Value::Null),
        "manifestPath": manifest_path.to_string_lossy(),
        "imported": imported,
        "mediaRestored": media_restored,
        "mediaMissing": media_missing,
        "safetyBackup": safety_backup
    }))
}

pub(super) fn restore(store: &SqliteStore, body: Value) -> Result<Value, String> {
    restore_with_prebackup(store, body, |store, tenant_id| run_now(store, tenant_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::random_url_token;

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
                media_id: "one".to_string(), staged_path: staged.join("one"),
                target_path: target.join("one"), rollback_path: rollback.join("one"),
            },
            RestoreMediaPlan {
                media_id: "two".to_string(), staged_path: staged.join("missing"),
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
