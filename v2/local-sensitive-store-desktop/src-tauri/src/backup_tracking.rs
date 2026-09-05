use super::*;
use rusqlite::OptionalExtension;

pub(crate) fn syncable_tables() -> impl Iterator<Item = &'static BackupTable> {
    BACKUP_TABLES
        .iter()
        .filter(|table| table.name != "cloud_sync_runs")
}

pub(super) fn record_key_expression(prefix: &str, table: &BackupTable) -> String {
    let columns = table
        .key_columns
        .iter()
        .copied()
        .filter(|column| *column != "tenant_id")
        .map(|column| format!("{prefix}.{column}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("json_array({columns})")
}

pub(crate) fn install_sync_tracking(conn: &Connection) -> Result<(), String> {
    // Other helper/MCP connections must never observe the trigger upgrade gap.
    let transaction = conn
        .unchecked_transaction()
        .map_err(|e| format!("db_sync_upgrade_begin_failed:{e}"))?;
    let conn = &*transaction;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS local_store_device_sync_state (
          tenant_id TEXT PRIMARY KEY,
          applied_generation INTEGER NOT NULL DEFAULT 0,
          published_generation INTEGER NOT NULL DEFAULT 0,
          latest_generation INTEGER NOT NULL DEFAULT 0,
          latest_status TEXT NOT NULL DEFAULT '',
          last_content_sha256 TEXT NOT NULL DEFAULT '',
          first_dirty_at_ms INTEGER,
          last_dirty_at_ms INTEGER,
          last_checked_at_ms INTEGER NOT NULL DEFAULT 0,
          last_success_at_ms INTEGER NOT NULL DEFAULT 0,
          last_error TEXT NOT NULL DEFAULT '',
          applying INTEGER NOT NULL DEFAULT 0 CHECK (applying IN (0, 1))
        );
        CREATE TABLE IF NOT EXISTS local_store_device_sync_records (
          tenant_id TEXT NOT NULL,
          table_name TEXT NOT NULL,
          record_key TEXT NOT NULL,
          dirty_base_generation INTEGER NOT NULL DEFAULT 0,
          record_version INTEGER NOT NULL DEFAULT 1,
          changed_generation INTEGER NOT NULL DEFAULT 0,
          tombstone INTEGER NOT NULL DEFAULT 0 CHECK (tombstone IN (0, 1)),
          changed_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, table_name, record_key)
        );
        CREATE INDEX IF NOT EXISTS idx_local_store_device_sync_records_dirty
          ON local_store_device_sync_records (tenant_id, changed_generation, changed_at_ms);
        CREATE TABLE IF NOT EXISTS local_store_device_sync_conflicts (
          conflict_id TEXT PRIMARY KEY,
          tenant_id TEXT NOT NULL,
          table_name TEXT NOT NULL,
          record_key TEXT NOT NULL,
          losing_generation INTEGER NOT NULL,
          winning_generation INTEGER NOT NULL,
          payload_json TEXT NOT NULL,
          captured_at_ms INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_local_store_device_sync_conflicts_tenant
          ON local_store_device_sync_conflicts (tenant_id, captured_at_ms DESC);
        "#,
    )
    .map_err(|e| format!("db_sync_tracking_schema_failed:{e}"))?;

    for column in ["change_sequence", "seed_version"] {
        let present = conn
            .prepare("PRAGMA table_info(local_store_device_sync_state)")
            .map_err(|e| format!("db_sync_columns_failed:{e}"))?
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| format!("db_sync_columns_failed:{e}"))?
            .filter_map(Result::ok)
            .any(|name| name == column);
        if !present {
            conn.execute_batch(&format!("ALTER TABLE local_store_device_sync_state ADD COLUMN {column} INTEGER NOT NULL DEFAULT 0"))
                .map_err(|e| format!("db_sync_columns_failed:{e}"))?;
        }
    }
    super::runtime::install_schema(conn)?;
    for table in syncable_tables() {
        if !table_exists(conn, table.name)? {
            continue;
        }
        conn.execute_batch(&format!(
            "DROP TRIGGER IF EXISTS local_store_sync_{0}_insert;
             DROP TRIGGER IF EXISTS local_store_sync_{0}_update;
             DROP TRIGGER IF EXISTS local_store_sync_{0}_delete;",
            table.name
        ))
        .map_err(|e| format!("db_sync_trigger_upgrade_failed:{e}"))?;
        let changed = table
            .columns
            .iter()
            .map(|column| format!("OLD.{column} IS NOT NEW.{column}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let insert_key = record_key_expression("NEW", table);
        let delete_key = record_key_expression("OLD", table);
        let timestamp = "CAST(strftime('%s', 'now') AS INTEGER) * 1000";
        let trigger_sql = format!(
            r#"
            CREATE TRIGGER IF NOT EXISTS local_store_sync_{name}_insert
            AFTER INSERT ON {name}
            WHEN COALESCE((SELECT applying FROM local_store_device_sync_state WHERE tenant_id = NEW.tenant_id), 0) = 0
            BEGIN
              INSERT INTO local_store_device_sync_state (tenant_id, first_dirty_at_ms, last_dirty_at_ms, change_sequence)
              VALUES (NEW.tenant_id, {timestamp}, {timestamp}, 1)
              ON CONFLICT(tenant_id) DO UPDATE SET
                first_dirty_at_ms = COALESCE(first_dirty_at_ms, {timestamp}),
                last_dirty_at_ms = {timestamp},
                change_sequence = change_sequence + 1;
              INSERT INTO local_store_device_sync_records (
                tenant_id, table_name, record_key, dirty_base_generation,
                record_version, changed_generation, tombstone, changed_at_ms
              ) VALUES (
                NEW.tenant_id, '{name}', {insert_key},
                COALESCE((SELECT applied_generation FROM local_store_device_sync_state WHERE tenant_id = NEW.tenant_id), 0),
                1, 0, 0, {timestamp}
              )
              ON CONFLICT(tenant_id, table_name, record_key) DO UPDATE SET
                dirty_base_generation = COALESCE((SELECT applied_generation FROM local_store_device_sync_state WHERE tenant_id = NEW.tenant_id), 0),
                record_version = record_version + 1,
                changed_generation = 0,
                tombstone = 0,
                changed_at_ms = {timestamp};
            END;
            CREATE TRIGGER IF NOT EXISTS local_store_sync_{name}_update
            AFTER UPDATE ON {name}
            WHEN ({changed}) AND COALESCE((SELECT applying FROM local_store_device_sync_state WHERE tenant_id = NEW.tenant_id), 0) = 0
            BEGIN
              INSERT INTO local_store_device_sync_state (tenant_id, first_dirty_at_ms, last_dirty_at_ms, change_sequence)
              VALUES (NEW.tenant_id, {timestamp}, {timestamp}, 1)
              ON CONFLICT(tenant_id) DO UPDATE SET
                first_dirty_at_ms = COALESCE(first_dirty_at_ms, {timestamp}),
                last_dirty_at_ms = {timestamp},
                change_sequence = change_sequence + 1;
              INSERT INTO local_store_device_sync_records (
                tenant_id, table_name, record_key, dirty_base_generation,
                record_version, changed_generation, tombstone, changed_at_ms
              ) VALUES (
                NEW.tenant_id, '{name}', {insert_key},
                COALESCE((SELECT applied_generation FROM local_store_device_sync_state WHERE tenant_id = NEW.tenant_id), 0),
                1, 0, 0, {timestamp}
              )
              ON CONFLICT(tenant_id, table_name, record_key) DO UPDATE SET
                dirty_base_generation = MIN(dirty_base_generation, COALESCE((SELECT applied_generation FROM local_store_device_sync_state WHERE tenant_id = NEW.tenant_id), 0)),
                record_version = record_version + 1,
                changed_generation = 0,
                tombstone = 0,
                changed_at_ms = {timestamp};
            END;
            CREATE TRIGGER IF NOT EXISTS local_store_sync_{name}_delete
            AFTER DELETE ON {name}
            WHEN COALESCE((SELECT applying FROM local_store_device_sync_state WHERE tenant_id = OLD.tenant_id), 0) = 0
            BEGIN
              INSERT INTO local_store_device_sync_state (tenant_id, first_dirty_at_ms, last_dirty_at_ms, change_sequence)
              VALUES (OLD.tenant_id, {timestamp}, {timestamp}, 1)
              ON CONFLICT(tenant_id) DO UPDATE SET
                first_dirty_at_ms = COALESCE(first_dirty_at_ms, {timestamp}),
                last_dirty_at_ms = {timestamp},
                change_sequence = change_sequence + 1;
              INSERT INTO local_store_device_sync_records (
                tenant_id, table_name, record_key, dirty_base_generation,
                record_version, changed_generation, tombstone, changed_at_ms
              ) VALUES (
                OLD.tenant_id, '{name}', {delete_key},
                COALESCE((SELECT applied_generation FROM local_store_device_sync_state WHERE tenant_id = OLD.tenant_id), 0),
                1, 0, 1, {timestamp}
              )
              ON CONFLICT(tenant_id, table_name, record_key) DO UPDATE SET
                dirty_base_generation = MIN(dirty_base_generation, COALESCE((SELECT applied_generation FROM local_store_device_sync_state WHERE tenant_id = OLD.tenant_id), 0)),
                record_version = record_version + 1,
                changed_generation = 0,
                tombstone = 1,
                changed_at_ms = {timestamp};
            END;
            "#,
            name = table.name,
        );
        conn.execute_batch(&trigger_sql)
            .map_err(|e| format!("db_sync_tracking_trigger_failed:{}:{e}", table.name))?;
    }
    transaction
        .commit()
        .map_err(|e| format!("db_sync_upgrade_commit_failed:{e}"))
}

pub(crate) fn seed_sync_records(store: &SqliteStore, tenant_id: &str) -> Result<(), String> {
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let seeded = conn
        .query_row(
            "SELECT seed_version FROM local_store_device_sync_state WHERE tenant_id=?1",
            params![tenant_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|e| format!("db_sync_seed_version_failed:{e}"))?
        .unwrap_or(0);
    if seeded == 1 {
        return Ok(());
    }
    let timestamp = now_ms();
    let mut inserted = 0i64;
    for table in syncable_tables() {
        if !table_exists(&conn, table.name)? {
            continue;
        }
        let key = record_key_expression(table.name, table);
        let sql = format!(
            "INSERT OR IGNORE INTO local_store_device_sync_records (
               tenant_id, table_name, record_key, dirty_base_generation,
               record_version, changed_generation, tombstone, changed_at_ms
             )
             SELECT tenant_id, '{name}', {key},
               COALESCE((SELECT applied_generation FROM local_store_device_sync_state WHERE tenant_id = ?1), 0),
               1, 0, 0, ?2
             FROM {name} WHERE tenant_id = ?1",
            name = table.name,
        );
        inserted += conn
            .execute(&sql, params![tenant_id, timestamp])
            .map_err(|e| format!("db_sync_tracking_seed_failed:{}:{e}", table.name))?
            as i64;
    }
    if inserted > 0 {
        conn.execute(
            "INSERT INTO local_store_device_sync_state (tenant_id, first_dirty_at_ms, last_dirty_at_ms)
             VALUES (?1, ?2, ?2)
             ON CONFLICT(tenant_id) DO UPDATE SET
               first_dirty_at_ms = COALESCE(first_dirty_at_ms, ?2),
               last_dirty_at_ms = ?2,
               change_sequence = change_sequence + 1",
            params![tenant_id, timestamp],
        )
        .map_err(|e| format!("db_sync_tracking_seed_state_failed:{e}"))?;
    } else {
        conn.execute(
            "INSERT OR IGNORE INTO local_store_device_sync_state (tenant_id) VALUES (?1)",
            params![tenant_id],
        )
        .map_err(|e| format!("db_sync_tracking_state_failed:{e}"))?;
    }
    conn.execute(
        "UPDATE local_store_device_sync_state SET seed_version=1 WHERE tenant_id=?1",
        params![tenant_id],
    )
    .map_err(|e| format!("db_sync_seed_version_failed:{e}"))?;
    Ok(())
}

pub(super) fn sync_manifest(
    conn: &Connection,
    tenant_id: &str,
    generation: i64,
) -> Result<Value, String> {
    let mut statement = conn
        .prepare(
            "SELECT table_name, record_key, dirty_base_generation, record_version,
                    changed_generation, tombstone, changed_at_ms
             FROM local_store_device_sync_records WHERE tenant_id = ?1
             ORDER BY table_name, record_key",
        )
        .map_err(|e| format!("db_sync_manifest_prepare_failed:{e}"))?;
    let rows = statement
        .query_map(params![tenant_id], |row| {
            let record_key: String = row.get(1)?;
            let changed_generation: i64 = row.get(4)?;
            Ok(json!({
                "table": row.get::<_, String>(0)?,
                "recordKey": serde_json::from_str::<Value>(&record_key).unwrap_or_else(|_| json!([])),
                "dirtyBaseGeneration": row.get::<_, i64>(2)?,
                "recordVersion": row.get::<_, i64>(3)?,
                "changedGeneration": if changed_generation > 0 { changed_generation } else { generation },
                "tombstone": row.get::<_, i64>(5)? == 1,
                "changedAtMs": row.get::<_, i64>(6)?,
            }))
        })
        .map_err(|e| format!("db_sync_manifest_query_failed:{e}"))?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row.map_err(|e| format!("db_sync_manifest_row_failed:{e}"))?);
    }
    drop(statement);
    Ok(json!({
        "baseGeneration": generation.saturating_sub(1),
        "contentSha256": "",
        "records": records,
    }))
}

pub(crate) fn local_sync_state(
    store: &SqliteStore,
    tenant_id: &str,
) -> Result<LocalSyncState, String> {
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    conn.query_row(
        "SELECT applied_generation, published_generation, latest_generation, latest_status,
                last_content_sha256, COALESCE(first_dirty_at_ms, 0), COALESCE(last_dirty_at_ms, 0),
                last_checked_at_ms, last_success_at_ms, last_error,
                (SELECT COUNT(*) FROM local_store_device_sync_conflicts c WHERE c.tenant_id = s.tenant_id),
                (SELECT COUNT(*) FROM local_store_device_sync_conflicts c WHERE c.tenant_id = s.tenant_id AND c.reviewed_at_ms IS NULL),
                COALESCE((SELECT lifetime_count FROM local_store_device_sync_conflict_stats c WHERE c.tenant_id = s.tenant_id), 0),
                change_sequence
         FROM local_store_device_sync_state s WHERE tenant_id = ?1",
        params![tenant_id],
        |row| Ok(LocalSyncState {
            applied_generation: row.get(0)?,
            published_generation: row.get(1)?,
            latest_generation: row.get(2)?,
            latest_status: row.get(3)?,
            last_content_sha256: row.get(4)?,
            first_dirty_at_ms: row.get(5)?,
            last_dirty_at_ms: row.get(6)?,
            last_checked_at_ms: row.get(7)?,
            last_success_at_ms: row.get(8)?,
            last_error: row.get(9)?,
            conflict_count: row.get(10)?,
            conflict_unreviewed_count: row.get(11)?,
            conflict_lifetime_count: row.get(12)?,
            change_sequence: row.get(13)?,
        }),
    )
    .optional().map(|state| state.unwrap_or_default())
    .map_err(|e| format!("db_sync_state_read_failed:{e}"))
}

pub(super) fn database_content_hasher(
    conn: &Connection,
    tenant_id: &str,
    records: &[Value],
) -> Result<sha2::Sha256, String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for table in syncable_tables() {
        if !table_exists(&conn, table.name)? {
            continue;
        }
        let columns = table
            .columns
            .iter()
            .map(|column| format!("{name}.{column}", name = table.name))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT json_array({columns}) FROM {name} WHERE tenant_id = ?1 ORDER BY {keys}",
            name = table.name,
            keys = table.key_columns.join(", "),
        );
        let mut statement = conn
            .prepare(&sql)
            .map_err(|e| format!("db_sync_content_prepare_failed:{}:{e}", table.name))?;
        let rows = statement
            .query_map(params![tenant_id], |row| row.get::<_, String>(0))
            .map_err(|e| format!("db_sync_content_query_failed:{}:{e}", table.name))?;
        hasher.update(table.name.as_bytes());
        hasher.update([0]);
        for row in rows {
            hasher.update(
                row.map_err(|e| format!("db_sync_content_row_failed:{}:{e}", table.name))?
                    .as_bytes(),
            );
            hasher.update([b'\n']);
        }
    }
    for record in records
        .iter()
        .filter(|record| record.get("tombstone").and_then(Value::as_bool) == Some(true))
    {
        let table = record
            .get("table")
            .and_then(Value::as_str)
            .ok_or("backup_sync_table_required")?;
        let key = serde_json::to_string(record.get("recordKey").ok_or("backup_sync_key_required")?)
            .map_err(|e| format!("backup_sync_key_encode_failed:{e}"))?;
        hasher.update(b"tombstone\0");
        hasher.update(table.as_bytes());
        hasher.update([0]);
        hasher.update(key.as_bytes());
        hasher.update([b'\n']);
    }
    Ok(hasher)
}

pub(super) fn tenant_primary_content_sha256(
    store: &SqliteStore,
    tenant_id: &str,
) -> Result<String, String> {
    use sha2::Digest;
    seed_sync_records(store, tenant_id)?;
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let sync = sync_manifest(&conn, tenant_id, 0)?;
    let mut hasher = database_content_hasher(
        &conn,
        tenant_id,
        sync["records"]
            .as_array()
            .ok_or("backup_sync_records_required")?,
    )?;
    drop(conn);
    for row in list_media_rows(store, tenant_id)? {
        let path = store.data_dir.join(row.local_path);
        if path.is_file() {
            let (size, sha256) = sha256_file(&path)?;
            hasher.update(b"board-media\0");
            hasher.update(row.media_id.as_bytes());
            hasher.update([0]);
            hasher.update(size.to_string().as_bytes());
            hasher.update([0]);
            hasher.update(sha256.as_bytes());
            hasher.update([b'\n']);
        }
    }
    for row in list_work_note_attachment_rows(store, tenant_id)? {
        let path = store.data_dir.join(row.local_path);
        if path.is_file() {
            let (size, sha256) = sha256_file(&path)?;
            hasher.update(b"work-note-attachment\0");
            hasher.update(row.attachment_id.as_bytes());
            hasher.update([0]);
            hasher.update(size.to_string().as_bytes());
            hasher.update([0]);
            hasher.update(sha256.as_bytes());
            hasher.update([b'\n']);
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(crate) fn tenant_content_sha256(
    store: &SqliteStore,
    tenant_id: &str,
) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let primary = tenant_primary_content_sha256(store, tenant_id)?;
    let archives = crate::shared_archive_sync::tenant_content_sha256(tenant_id)?;
    let mut hasher = Sha256::new();
    hasher.update(b"classaimate-device-sync-content-v4\0");
    hasher.update(primary.as_bytes());
    hasher.update([0]);
    hasher.update(archives.as_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

pub(crate) fn mark_sync_published(
    store: &SqliteStore,
    tenant_id: &str,
    generation: i64,
    manifest_path: &Path,
    content_sha256: &str,
    latest_status: &str,
    captured_sequence: i64,
) -> Result<(), String> {
    let manifest = read_manifest(manifest_path)?;
    let authoritative = crate::backup_v4::projection(manifest_path, &manifest)?;
    let records = authoritative
        .get("sync")
        .and_then(|sync| sync.get("records"))
        .and_then(Value::as_array)
        .ok_or_else(|| "backup_sync_records_required".to_string())?;
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let transaction = conn
        .unchecked_transaction()
        .map_err(|e| format!("db_sync_publish_transaction_failed:{e}"))?;
    for record in records {
        let table_name = normalize_json_text(record.get("table"), 120);
        let record_key = serde_json::to_string(
            record
                .get("recordKey")
                .and_then(Value::as_array)
                .ok_or_else(|| "backup_sync_record_key_invalid".to_string())?,
        )
        .map_err(|e| format!("backup_sync_record_key_encode_failed:{e}"))?;
        let record_version = record
            .get("recordVersion")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let changed_generation = record
            .get("changedGeneration")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if changed_generation != generation || record_version < 1 {
            continue;
        }
        transaction
            .execute(
                "UPDATE local_store_device_sync_records SET changed_generation = ?4
             WHERE tenant_id = ?1 AND table_name = ?2 AND record_key = ?3
               AND changed_generation = 0 AND record_version = ?5",
                params![
                    tenant_id,
                    table_name,
                    record_key,
                    generation,
                    record_version
                ],
            )
            .map_err(|e| format!("db_sync_publish_records_failed:{e}"))?;
    }
    let remaining_dirty: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM local_store_device_sync_records
         WHERE tenant_id = ?1 AND changed_generation = 0",
            params![tenant_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("db_sync_publish_dirty_count_failed:{e}"))?;
    transaction.execute(
        "INSERT INTO local_store_device_sync_state (
           tenant_id, applied_generation, published_generation, latest_generation,
           latest_status, last_content_sha256, last_success_at_ms, last_error
         ) VALUES (?1, ?2, ?2, ?2, ?3, ?4, ?5, '')
         ON CONFLICT(tenant_id) DO UPDATE SET
           applied_generation = MAX(applied_generation, excluded.applied_generation),
           published_generation = MAX(published_generation, excluded.published_generation),
           latest_generation = MAX(latest_generation, excluded.latest_generation),
           latest_status = excluded.latest_status,
           last_content_sha256 = CASE WHEN excluded.last_content_sha256 = '' THEN last_content_sha256 ELSE excluded.last_content_sha256 END,
           first_dirty_at_ms = CASE WHEN ?6 = 0 AND change_sequence = ?7 THEN NULL ELSE first_dirty_at_ms END,
           last_dirty_at_ms = CASE WHEN ?6 = 0 AND change_sequence = ?7 THEN NULL ELSE last_dirty_at_ms END,
           last_success_at_ms = excluded.last_success_at_ms,
           last_error = ''",
        params![tenant_id, generation, latest_status, content_sha256, now_ms(), remaining_dirty, captured_sequence],
    ).map_err(|e| format!("db_sync_publish_state_failed:{e}"))?;
    transaction
        .commit()
        .map_err(|e| format!("db_sync_publish_commit_failed:{e}"))
}

pub(crate) fn mark_sync_applied_content(
    store: &SqliteStore,
    tenant_id: &str,
    content_sha256: &str,
) -> Result<(), String> {
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    conn.execute(
        "UPDATE local_store_device_sync_state
         SET last_content_sha256 = ?2, last_success_at_ms = ?3, last_error = ''
         WHERE tenant_id = ?1",
        params![tenant_id, content_sha256, now_ms()],
    )
    .map_err(|e| format!("db_sync_applied_content_failed:{e}"))?;
    Ok(())
}

pub(crate) fn mark_sync_unchanged(
    store: &SqliteStore,
    tenant_id: &str,
    generation: i64,
    captured_sequence: i64,
) -> Result<(), String> {
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let transaction = conn
        .unchecked_transaction()
        .map_err(|e| format!("db_sync_unchanged_transaction_failed:{e}"))?;
    let sequence: i64 = transaction
        .query_row(
            "SELECT change_sequence FROM local_store_device_sync_state WHERE tenant_id=?1",
            params![tenant_id],
            |row| row.get(0),
        )
        .map_err(|e| format!("db_sync_sequence_failed:{e}"))?;
    if sequence != captured_sequence {
        return Ok(());
    }
    transaction
        .execute(
            "UPDATE local_store_device_sync_records SET changed_generation = ?2
         WHERE tenant_id = ?1 AND changed_generation = 0",
            params![tenant_id, generation],
        )
        .map_err(|e| format!("db_sync_unchanged_records_failed:{e}"))?;
    transaction.execute(
        "UPDATE local_store_device_sync_state
         SET first_dirty_at_ms = NULL, last_dirty_at_ms = NULL, last_success_at_ms = ?2, last_error = ''
         WHERE tenant_id = ?1",
        params![tenant_id, now_ms()],
    ).map_err(|e| format!("db_sync_unchanged_state_failed:{e}"))?;
    transaction
        .commit()
        .map_err(|e| format!("db_sync_unchanged_commit_failed:{e}"))
}

pub(crate) fn mark_sync_latest(
    store: &SqliteStore,
    tenant_id: &str,
    generation: i64,
    status: &str,
) -> Result<(), String> {
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    conn.execute(
        "INSERT INTO local_store_device_sync_state (
           tenant_id, latest_generation, latest_status, last_checked_at_ms, last_error
         ) VALUES (?1, ?2, ?3, ?4, '')
         ON CONFLICT(tenant_id) DO UPDATE SET
           latest_generation = excluded.latest_generation,
           latest_status = excluded.latest_status,
           last_checked_at_ms = excluded.last_checked_at_ms,
           last_error = ''",
        params![tenant_id, generation, status, now_ms()],
    )
    .map_err(|e| format!("db_sync_latest_state_failed:{e}"))?;
    Ok(())
}

pub(crate) fn mark_sync_error(store: &SqliteStore, tenant_id: &str, error: &str) {
    if let Ok(conn) = store.conn.lock() {
        let _ = conn.execute(
            "INSERT INTO local_store_device_sync_state (tenant_id, last_error) VALUES (?1, ?2)
             ON CONFLICT(tenant_id) DO UPDATE SET last_error = excluded.last_error",
            params![tenant_id, normalize(error, 800)],
        );
    }
}

pub(crate) fn mark_external_sync_dirty(store: &SqliteStore, tenant_id: &str) -> Result<(), String> {
    let timestamp = now_ms();
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    conn.execute(
        "INSERT INTO local_store_device_sync_state (tenant_id, first_dirty_at_ms, last_dirty_at_ms, change_sequence)
         VALUES (?1, ?2, ?2, 1)
         ON CONFLICT(tenant_id) DO UPDATE SET
           first_dirty_at_ms = COALESCE(first_dirty_at_ms, excluded.first_dirty_at_ms),
           last_dirty_at_ms = excluded.last_dirty_at_ms,
           change_sequence = change_sequence + 1",
        params![tenant_id, timestamp],
    )
    .map_err(|e| format!("db_sync_external_dirty_failed:{e}"))?;
    Ok(())
}
