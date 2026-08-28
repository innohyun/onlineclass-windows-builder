use crate::{local_arch, local_os_name, local_pc_name, normalize, normalize_json_text, normalize_tenant_id, SqliteStore, SERVICE_VERSION};
use chrono::Utc;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::fs;
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[path = "backup_restore.rs"]
mod restore_runtime;

const BACKUP_CONFIG_FILE: &str = "backup-config.json";
const BACKUP_NAMESPACE_DIR: &str = "OnlineClassLocalBackups";
const BACKUP_INTERVAL_MS: i64 = 24 * 60 * 60 * 1000;
const AUTO_SYNC_RECENT_KEEP: usize = 10;
const AUTO_SYNC_DAILY_KEEP_DAYS: i64 = 30;

#[derive(Clone, Debug, Default)]
pub(crate) struct LocalSyncState {
    pub(crate) applied_generation: i64,
    pub(crate) published_generation: i64,
    pub(crate) latest_generation: i64,
    pub(crate) latest_status: String,
    pub(crate) last_content_sha256: String,
    pub(crate) first_dirty_at_ms: i64,
    pub(crate) last_dirty_at_ms: i64,
    pub(crate) last_checked_at_ms: i64,
    pub(crate) last_success_at_ms: i64,
    pub(crate) last_error: String,
    pub(crate) conflict_count: i64,
    pub(crate) conflict_unreviewed_count: i64,
    pub(crate) conflict_lifetime_count: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct ArtifactDigest {
    pub(crate) relative_path: String,
    pub(crate) size: u64,
    pub(crate) sha256: String,
}

pub(crate) fn sha256_file(path: &Path) -> Result<(u64, String), String> {
    use sha2::{Digest, Sha256};
    let mut file = File::open(path).map_err(|e| format!("backup_hash_open_failed:{e}"))?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|e| format!("backup_hash_read_failed:{e}"))?;
        if read == 0 { break; }
        hasher.update(&buffer[..read]);
        size = size.saturating_add(read as u64);
    }
    Ok((size, format!("{:x}", hasher.finalize())))
}

pub(crate) fn artifact_set_sha256(artifacts: &mut [ArtifactDigest]) -> String {
    use sha2::{Digest, Sha256};
    artifacts.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let mut hasher = Sha256::new();
    for artifact in artifacts {
        hasher.update(artifact.relative_path.as_bytes());
        hasher.update([0]);
        hasher.update(artifact.size.to_string().as_bytes());
        hasher.update([0]);
        hasher.update(artifact.sha256.as_bytes());
        hasher.update([b'\n']);
    }
    format!("{:x}", hasher.finalize())
}

pub(crate) struct BackupTable {
    pub(crate) name: &'static str,
    pub(crate) columns: &'static [&'static str],
    pub(crate) key_columns: &'static [&'static str],
    timestamp_column: &'static str,
    optional: bool,
}
const BACKUP_TABLES: &[BackupTable] = &[
    BackupTable {
        name: "lesson_observations",
        columns: &["tenant_id", "doc_id", "date_key", "period", "student_code", "payload_json", "updated_at_ms"],
        key_columns: &["tenant_id", "doc_id"],
        timestamp_column: "updated_at_ms",
        optional: false,
    },
    BackupTable {
        name: "teacher_counseling_sessions",
        columns: &["tenant_id", "session_id", "student_code", "counseling_at_ms", "status", "follow_up_on", "archived_at_ms", "payload_json", "updated_at_ms"],
        key_columns: &["tenant_id", "session_id"],
        timestamp_column: "updated_at_ms",
        optional: false,
    },
    BackupTable {
        name: "student_private_details",
        columns: &["tenant_id", "student_code", "payload_json", "updated_at_ms"],
        key_columns: &["tenant_id", "student_code"],
        timestamp_column: "updated_at_ms",
        optional: false,
    },
    BackupTable {
        name: "student_private_photos",
        columns: &["tenant_id", "student_code", "payload_json", "updated_at_ms"],
        key_columns: &["tenant_id", "student_code"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "math_daily_attempts",
        columns: &["tenant_id", "attempt_id", "date_key", "student_code", "curriculum", "payload_json", "updated_at_ms"],
        key_columns: &["tenant_id", "attempt_id"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "math_daily_student_profiles",
        columns: &["tenant_id", "student_code", "payload_json", "updated_at_ms"],
        key_columns: &["tenant_id", "student_code"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "math_daily_review_sessions",
        columns: &["tenant_id", "review_session_id", "date_key", "student_code", "curriculum", "payload_json", "updated_at_ms"],
        key_columns: &["tenant_id", "review_session_id"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "math_daily_assignments",
        columns: &["tenant_id", "assignment_id", "date_key", "curriculum", "payload_json", "updated_at_ms"],
        key_columns: &["tenant_id", "assignment_id"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "math_daily_assignment_results",
        columns: &["tenant_id", "submission_id", "assignment_id", "student_code", "date_key", "curriculum", "payload_json", "updated_at_ms"],
        key_columns: &["tenant_id", "submission_id"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "math_daily_cache_runs",
        columns: &["tenant_id", "cache_key", "action", "date_from", "date_to", "date_key", "curriculum", "payload_json", "updated_at_ms"],
        key_columns: &["tenant_id", "cache_key"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "board_post_snapshots",
        columns: &["tenant_id", "board_id", "post_id", "payload_json", "updated_at_ms", "archived_at_ms"],
        key_columns: &["tenant_id", "board_id", "post_id"],
        timestamp_column: "updated_at_ms",
        optional: false,
    },
    BackupTable {
        name: "board_media_files",
        columns: &[
            "tenant_id",
            "board_id",
            "post_id",
            "media_id",
            "storage_path",
            "local_path",
            "content_type",
            "file_name",
            "size",
            "expires_at_ms",
            "archived_at_ms",
            "payload_json",
        ],
        key_columns: &["tenant_id", "media_id"],
        timestamp_column: "archived_at_ms",
        optional: false,
    },
    BackupTable {
        name: "attendance_records",
        columns: &["tenant_id", "record_id", "date_key", "student_code", "payload_json", "updated_at_ms"],
        key_columns: &["tenant_id", "record_id"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "attendance_nais_checks",
        columns: &["tenant_id", "check_id", "date_key", "student_code", "payload_json", "updated_at_ms"],
        key_columns: &["tenant_id", "check_id"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "attendance_document_requests",
        columns: &["tenant_id", "request_id", "date_key", "student_code", "payload_json", "updated_at_ms"],
        key_columns: &["tenant_id", "request_id"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "counseling_records",
        columns: &["tenant_id", "request_id", "student_code", "status", "created_at_ms", "updated_at_ms", "payload_json"],
        key_columns: &["tenant_id", "request_id"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "counseling_teacher_notes",
        columns: &["tenant_id", "request_id", "payload_json", "updated_at_ms"],
        key_columns: &["tenant_id", "request_id"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "eval_assignments",
        columns: &["tenant_id", "assignment_id", "shared_plan_id", "scheduled_date", "payload_json", "updated_at_ms"],
        key_columns: &["tenant_id", "assignment_id"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "eval_results",
        columns: &["tenant_id", "result_id", "assignment_id", "student_id", "date_key", "payload_json", "updated_at_ms"],
        key_columns: &["tenant_id", "result_id"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "student_record_draft_sets",
        columns: &["tenant_id", "draft_set_id", "status", "from_date", "to_date", "payload_json", "created_at_ms", "updated_at_ms"],
        key_columns: &["tenant_id", "draft_set_id"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "student_record_drafts",
        columns: &["tenant_id", "draft_id", "draft_set_id", "student_code", "class_no", "payload_json", "updated_at_ms"],
        key_columns: &["tenant_id", "draft_id"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "local_import_runs",
        columns: &["tenant_id", "run_id", "kind", "status", "payload_json", "started_at_ms", "finished_at_ms"],
        key_columns: &["tenant_id", "run_id"],
        timestamp_column: "finished_at_ms",
        optional: true,
    },
    BackupTable {
        name: "work_note_pages",
        columns: &["tenant_id", "page_id", "parent_id", "title", "emoji", "position", "properties_json", "document_json", "markdown", "created_at_ms", "updated_at_ms"],
        key_columns: &["tenant_id", "page_id"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "work_note_attachments",
        columns: &["tenant_id", "attachment_id", "page_id", "block_id", "file_name", "content_type", "byte_size", "sha256", "local_path", "created_at_ms", "updated_at_ms"],
        key_columns: &["tenant_id", "attachment_id"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "lesson_plan_bindings",
        columns: &["tenant_id", "plan_id", "page_id", "plan_kind", "date_key", "start_period", "end_period", "subject", "binding_revision", "updated_at_ms"],
        key_columns: &["tenant_id", "plan_id"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "password_vault_personal_profiles",
        columns: &["tenant_id", "owner_uid", "school_code", "wrapped_key_json", "revision", "created_at_ms", "updated_at_ms"],
        key_columns: &["tenant_id", "owner_uid", "school_code"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "password_vault_personal_entries",
        columns: &["tenant_id", "owner_uid", "school_code", "entry_id", "category", "ciphertext_json", "revision", "created_at_ms", "updated_at_ms"],
        key_columns: &["tenant_id", "owner_uid", "school_code", "entry_id"],
        timestamp_column: "updated_at_ms",
        optional: true,
    },
    BackupTable {
        name: "cloud_sync_runs",
        columns: &["tenant_id", "run_id", "payload_json", "started_at_ms", "finished_at_ms"],
        key_columns: &["tenant_id", "run_id"],
        timestamp_column: "finished_at_ms",
        optional: true,
    },
];

pub(crate) fn syncable_tables() -> impl Iterator<Item = &'static BackupTable> {
    BACKUP_TABLES
        .iter()
        .filter(|table| table.name != "cloud_sync_runs")
}

fn record_key_expression(prefix: &str, table: &BackupTable) -> String {
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

    for table in syncable_tables() {
        if !table_exists(conn, table.name)? {
            continue;
        }
        let insert_key = record_key_expression("NEW", table);
        let delete_key = record_key_expression("OLD", table);
        let timestamp = "CAST(strftime('%s', 'now') AS INTEGER) * 1000";
        let trigger_sql = format!(
            r#"
            CREATE TRIGGER IF NOT EXISTS local_store_sync_{name}_insert
            AFTER INSERT ON {name}
            WHEN COALESCE((SELECT applying FROM local_store_device_sync_state WHERE tenant_id = NEW.tenant_id), 0) = 0
            BEGIN
              INSERT INTO local_store_device_sync_state (tenant_id, first_dirty_at_ms, last_dirty_at_ms)
              VALUES (NEW.tenant_id, {timestamp}, {timestamp})
              ON CONFLICT(tenant_id) DO UPDATE SET
                first_dirty_at_ms = COALESCE(first_dirty_at_ms, {timestamp}),
                last_dirty_at_ms = {timestamp};
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
            WHEN COALESCE((SELECT applying FROM local_store_device_sync_state WHERE tenant_id = NEW.tenant_id), 0) = 0
            BEGIN
              INSERT INTO local_store_device_sync_state (tenant_id, first_dirty_at_ms, last_dirty_at_ms)
              VALUES (NEW.tenant_id, {timestamp}, {timestamp})
              ON CONFLICT(tenant_id) DO UPDATE SET
                first_dirty_at_ms = COALESCE(first_dirty_at_ms, {timestamp}),
                last_dirty_at_ms = {timestamp};
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
              INSERT INTO local_store_device_sync_state (tenant_id, first_dirty_at_ms, last_dirty_at_ms)
              VALUES (OLD.tenant_id, {timestamp}, {timestamp})
              ON CONFLICT(tenant_id) DO UPDATE SET
                first_dirty_at_ms = COALESCE(first_dirty_at_ms, {timestamp}),
                last_dirty_at_ms = {timestamp};
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
    Ok(())
}

fn seed_sync_records(store: &SqliteStore, tenant_id: &str) -> Result<(), String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    install_sync_tracking(&conn)?;
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
            .map_err(|e| format!("db_sync_tracking_seed_failed:{}:{e}", table.name))? as i64;
    }
    if inserted > 0 {
        conn.execute(
            "INSERT INTO local_store_device_sync_state (tenant_id, first_dirty_at_ms, last_dirty_at_ms)
             VALUES (?1, ?2, ?2)
             ON CONFLICT(tenant_id) DO UPDATE SET
               first_dirty_at_ms = COALESCE(first_dirty_at_ms, ?2),
               last_dirty_at_ms = ?2",
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
    Ok(())
}

fn sync_manifest(store: &SqliteStore, tenant_id: &str, generation: i64) -> Result<Value, String> {
    seed_sync_records(store, tenant_id)?;
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
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
    drop(conn);
    Ok(json!({
        "baseGeneration": generation - 1,
        "contentSha256": tenant_content_sha256(store, tenant_id)?,
        "records": records,
    }))
}

pub(crate) fn local_sync_state(store: &SqliteStore, tenant_id: &str) -> Result<LocalSyncState, String> {
    seed_sync_records(store, tenant_id)?;
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    conn.query_row(
        "SELECT applied_generation, published_generation, latest_generation, latest_status,
                last_content_sha256, COALESCE(first_dirty_at_ms, 0), COALESCE(last_dirty_at_ms, 0),
                last_checked_at_ms, last_success_at_ms, last_error,
                (SELECT COUNT(*) FROM local_store_device_sync_conflicts c WHERE c.tenant_id = s.tenant_id),
                (SELECT COUNT(*) FROM local_store_device_sync_conflicts c WHERE c.tenant_id = s.tenant_id AND c.reviewed_at_ms IS NULL),
                COALESCE((SELECT lifetime_count FROM local_store_device_sync_conflict_stats c WHERE c.tenant_id = s.tenant_id), 0)
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
        }),
    )
    .map_err(|e| format!("db_sync_state_read_failed:{e}"))
}

fn tenant_primary_content_sha256(store: &SqliteStore, tenant_id: &str) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    seed_sync_records(store, tenant_id)?;
    let mut hasher = Sha256::new();
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
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
    let mut tombstones = conn
        .prepare(
            "SELECT table_name, record_key FROM local_store_device_sync_records
             WHERE tenant_id = ?1 AND tombstone = 1 ORDER BY table_name, record_key",
        )
        .map_err(|e| format!("db_sync_tombstone_prepare_failed:{e}"))?;
    let rows = tombstones
        .query_map(params![tenant_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|e| format!("db_sync_tombstone_query_failed:{e}"))?;
    for row in rows {
        let (table, key) = row.map_err(|e| format!("db_sync_tombstone_row_failed:{e}"))?;
        hasher.update(b"tombstone\0");
        hasher.update(table.as_bytes());
        hasher.update([0]);
        hasher.update(key.as_bytes());
        hasher.update([b'\n']);
    }
    drop(tombstones);
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
) -> Result<(), String> {
    let manifest = read_manifest(manifest_path)?;
    let authoritative = crate::backup_v4::projection(manifest_path, &manifest)?;
    let records = authoritative
        .get("sync")
        .and_then(|sync| sync.get("records"))
        .and_then(Value::as_array)
        .ok_or_else(|| "backup_sync_records_required".to_string())?;
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    let transaction = conn.unchecked_transaction()
        .map_err(|e| format!("db_sync_publish_transaction_failed:{e}"))?;
    for record in records {
        let table_name = normalize_json_text(record.get("table"), 120);
        let record_key = serde_json::to_string(
            record.get("recordKey").and_then(Value::as_array)
                .ok_or_else(|| "backup_sync_record_key_invalid".to_string())?,
        ).map_err(|e| format!("backup_sync_record_key_encode_failed:{e}"))?;
        let record_version = record.get("recordVersion").and_then(Value::as_i64).unwrap_or(0);
        let changed_generation = record.get("changedGeneration").and_then(Value::as_i64).unwrap_or(0);
        if changed_generation != generation || record_version < 1 {
            continue;
        }
        transaction.execute(
            "UPDATE local_store_device_sync_records SET changed_generation = ?4
             WHERE tenant_id = ?1 AND table_name = ?2 AND record_key = ?3
               AND changed_generation = 0 AND record_version = ?5",
            params![tenant_id, table_name, record_key, generation, record_version],
        ).map_err(|e| format!("db_sync_publish_records_failed:{e}"))?;
    }
    let remaining_dirty: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM local_store_device_sync_records
         WHERE tenant_id = ?1 AND changed_generation = 0",
        params![tenant_id],
        |row| row.get(0),
    ).map_err(|e| format!("db_sync_publish_dirty_count_failed:{e}"))?;
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
           first_dirty_at_ms = CASE WHEN ?6 = 0 THEN NULL ELSE first_dirty_at_ms END,
           last_dirty_at_ms = CASE WHEN ?6 = 0 THEN NULL ELSE last_dirty_at_ms END,
           last_success_at_ms = excluded.last_success_at_ms,
           last_error = ''",
        params![tenant_id, generation, latest_status, content_sha256, now_ms(), remaining_dirty],
    ).map_err(|e| format!("db_sync_publish_state_failed:{e}"))?;
    transaction.commit().map_err(|e| format!("db_sync_publish_commit_failed:{e}"))
}

pub(crate) fn mark_sync_applied_content(
    store: &SqliteStore,
    tenant_id: &str,
    content_sha256: &str,
) -> Result<(), String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    conn.execute(
        "UPDATE local_store_device_sync_state
         SET last_content_sha256 = ?2, last_success_at_ms = ?3, last_error = ''
         WHERE tenant_id = ?1",
        params![tenant_id, content_sha256, now_ms()],
    ).map_err(|e| format!("db_sync_applied_content_failed:{e}"))?;
    Ok(())
}

pub(crate) fn mark_sync_unchanged(
    store: &SqliteStore,
    tenant_id: &str,
    generation: i64,
) -> Result<(), String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    let transaction = conn.unchecked_transaction()
        .map_err(|e| format!("db_sync_unchanged_transaction_failed:{e}"))?;
    transaction.execute(
        "UPDATE local_store_device_sync_records SET changed_generation = ?2
         WHERE tenant_id = ?1 AND changed_generation = 0",
        params![tenant_id, generation],
    ).map_err(|e| format!("db_sync_unchanged_records_failed:{e}"))?;
    transaction.execute(
        "UPDATE local_store_device_sync_state
         SET first_dirty_at_ms = NULL, last_dirty_at_ms = NULL, last_success_at_ms = ?2, last_error = ''
         WHERE tenant_id = ?1",
        params![tenant_id, now_ms()],
    ).map_err(|e| format!("db_sync_unchanged_state_failed:{e}"))?;
    transaction.commit().map_err(|e| format!("db_sync_unchanged_commit_failed:{e}"))
}

pub(crate) fn mark_sync_latest(
    store: &SqliteStore,
    tenant_id: &str,
    generation: i64,
    status: &str,
) -> Result<(), String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
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
    install_sync_tracking(&conn)?;
    conn.execute(
        "INSERT INTO local_store_device_sync_state (tenant_id, first_dirty_at_ms, last_dirty_at_ms)
         VALUES (?1, ?2, ?2)
         ON CONFLICT(tenant_id) DO UPDATE SET
           first_dirty_at_ms = COALESCE(first_dirty_at_ms, excluded.first_dirty_at_ms),
           last_dirty_at_ms = excluded.last_dirty_at_ms",
        params![tenant_id, timestamp],
    )
    .map_err(|e| format!("db_sync_external_dirty_failed:{e}"))?;
    Ok(())
}

#[derive(Clone)]
struct MediaRow {
    board_id: String,
    post_id: String,
    media_id: String,
    local_path: String,
    content_type: String,
    file_name: String,
    size: i64,
    archived_at_ms: i64,
}

#[derive(Clone)]
struct WorkNoteAttachmentRow {
    attachment_id: String,
    page_id: String,
    block_id: String,
    file_name: String,
    content_type: String,
    size: i64,
    sha256: String,
    local_path: String,
    created_at_ms: i64,
    updated_at_ms: i64,
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn backup_schema_sql(prefix: &str) -> String {
    format!(
        r#"
        CREATE TABLE IF NOT EXISTS {prefix}lesson_observations (
          tenant_id TEXT NOT NULL,
          doc_id TEXT NOT NULL,
          date_key TEXT NOT NULL,
          period INTEGER NOT NULL,
          student_code TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, doc_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}teacher_counseling_sessions (
          tenant_id TEXT NOT NULL,
          session_id TEXT NOT NULL,
          student_code TEXT NOT NULL,
          counseling_at_ms INTEGER NOT NULL,
          status TEXT NOT NULL CHECK (status IN ('completed', 'follow_up')),
          follow_up_on TEXT,
          archived_at_ms INTEGER,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, session_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}student_private_details (
          tenant_id TEXT NOT NULL,
          student_code TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, student_code)
        );
        CREATE TABLE IF NOT EXISTS {prefix}student_private_photos (
          tenant_id TEXT NOT NULL,
          student_code TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, student_code)
        );
        CREATE TABLE IF NOT EXISTS {prefix}math_daily_attempts (
          tenant_id TEXT NOT NULL,
          attempt_id TEXT NOT NULL,
          date_key TEXT NOT NULL,
          student_code TEXT NOT NULL,
          curriculum TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, attempt_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}math_daily_student_profiles (
          tenant_id TEXT NOT NULL,
          student_code TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, student_code)
        );
        CREATE TABLE IF NOT EXISTS {prefix}math_daily_review_sessions (
          tenant_id TEXT NOT NULL,
          review_session_id TEXT NOT NULL,
          date_key TEXT NOT NULL,
          student_code TEXT NOT NULL,
          curriculum TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, review_session_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}math_daily_assignments (
          tenant_id TEXT NOT NULL,
          assignment_id TEXT NOT NULL,
          date_key TEXT NOT NULL,
          curriculum TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, assignment_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}math_daily_assignment_results (
          tenant_id TEXT NOT NULL,
          submission_id TEXT NOT NULL,
          assignment_id TEXT NOT NULL,
          student_code TEXT NOT NULL,
          date_key TEXT NOT NULL,
          curriculum TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, submission_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}math_daily_cache_runs (
          tenant_id TEXT NOT NULL,
          cache_key TEXT NOT NULL,
          action TEXT NOT NULL,
          date_from TEXT,
          date_to TEXT,
          date_key TEXT,
          curriculum TEXT,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, cache_key)
        );
        CREATE TABLE IF NOT EXISTS {prefix}board_post_snapshots (
          tenant_id TEXT NOT NULL,
          board_id TEXT NOT NULL,
          post_id TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          archived_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, board_id, post_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}board_media_files (
          tenant_id TEXT NOT NULL,
          board_id TEXT NOT NULL,
          post_id TEXT NOT NULL,
          media_id TEXT NOT NULL,
          storage_path TEXT,
          local_path TEXT NOT NULL,
          content_type TEXT NOT NULL,
          file_name TEXT,
          size INTEGER NOT NULL,
          expires_at_ms INTEGER,
          archived_at_ms INTEGER NOT NULL,
          payload_json TEXT NOT NULL,
          PRIMARY KEY (tenant_id, media_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}attendance_records (
          tenant_id TEXT NOT NULL,
          record_id TEXT NOT NULL,
          date_key TEXT NOT NULL,
          student_code TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, record_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}attendance_nais_checks (
          tenant_id TEXT NOT NULL,
          check_id TEXT NOT NULL,
          date_key TEXT NOT NULL,
          student_code TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, check_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}attendance_document_requests (
          tenant_id TEXT NOT NULL,
          request_id TEXT NOT NULL,
          date_key TEXT NOT NULL,
          student_code TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, request_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}counseling_records (
          tenant_id TEXT NOT NULL,
          request_id TEXT NOT NULL,
          student_code TEXT NOT NULL,
          status TEXT NOT NULL,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          payload_json TEXT NOT NULL,
          PRIMARY KEY (tenant_id, request_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}counseling_teacher_notes (
          tenant_id TEXT NOT NULL,
          request_id TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, request_id),
          FOREIGN KEY (tenant_id, request_id) REFERENCES counseling_records (tenant_id, request_id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS {prefix}eval_assignments (
          tenant_id TEXT NOT NULL,
          assignment_id TEXT NOT NULL,
          shared_plan_id TEXT NOT NULL,
          scheduled_date TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, assignment_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}eval_results (
          tenant_id TEXT NOT NULL,
          result_id TEXT NOT NULL,
          assignment_id TEXT NOT NULL,
          student_id TEXT NOT NULL,
          date_key TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, result_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}student_record_draft_sets (
          tenant_id TEXT NOT NULL,
          draft_set_id TEXT NOT NULL,
          status TEXT NOT NULL,
          from_date TEXT NOT NULL,
          to_date TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, draft_set_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}student_record_drafts (
          tenant_id TEXT NOT NULL,
          draft_id TEXT NOT NULL,
          draft_set_id TEXT NOT NULL,
          student_code TEXT NOT NULL,
          class_no INTEGER NOT NULL,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, draft_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}local_import_runs (
          tenant_id TEXT NOT NULL,
          run_id TEXT NOT NULL,
          kind TEXT NOT NULL,
          status TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          started_at_ms INTEGER NOT NULL,
          finished_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, run_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}work_note_pages (
          tenant_id TEXT NOT NULL,
          page_id TEXT NOT NULL,
          parent_id TEXT,
          title TEXT NOT NULL,
          emoji TEXT NOT NULL,
          position INTEGER NOT NULL DEFAULT 0,
          properties_json TEXT NOT NULL,
          document_json TEXT NOT NULL,
          markdown TEXT NOT NULL,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, page_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}work_note_attachments (
          tenant_id TEXT NOT NULL,
          attachment_id TEXT NOT NULL,
          page_id TEXT NOT NULL,
          block_id TEXT NOT NULL,
          file_name TEXT NOT NULL,
          content_type TEXT NOT NULL,
          byte_size INTEGER NOT NULL,
          sha256 TEXT NOT NULL,
          local_path TEXT NOT NULL,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, attachment_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}lesson_plan_bindings (
          tenant_id TEXT NOT NULL,
          plan_id TEXT NOT NULL,
          page_id TEXT NOT NULL,
          plan_kind TEXT NOT NULL,
          date_key TEXT NOT NULL,
          start_period INTEGER NOT NULL,
          end_period INTEGER NOT NULL,
          subject TEXT NOT NULL,
          binding_revision INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, plan_id)
        );
        CREATE TABLE IF NOT EXISTS {prefix}password_vault_personal_profiles (
          tenant_id TEXT NOT NULL,
          owner_uid TEXT NOT NULL,
          school_code TEXT NOT NULL,
          wrapped_key_json TEXT NOT NULL,
          revision INTEGER NOT NULL DEFAULT 1,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, owner_uid, school_code)
        );
        CREATE TABLE IF NOT EXISTS {prefix}password_vault_personal_entries (
          tenant_id TEXT NOT NULL,
          owner_uid TEXT NOT NULL,
          school_code TEXT NOT NULL,
          entry_id TEXT NOT NULL,
          category TEXT NOT NULL,
          ciphertext_json TEXT NOT NULL,
          revision INTEGER NOT NULL,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, owner_uid, school_code, entry_id),
          FOREIGN KEY (tenant_id, owner_uid, school_code)
            REFERENCES password_vault_personal_profiles (tenant_id, owner_uid, school_code)
            ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS {prefix}cloud_sync_runs (
          tenant_id TEXT NOT NULL,
          run_id TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          started_at_ms INTEGER NOT NULL,
          finished_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, run_id)
        );
        "#
    )
}

fn config_path(store: &SqliteStore) -> PathBuf {
    store.data_dir.join(BACKUP_CONFIG_FILE)
}

fn read_config(store: &SqliteStore) -> Value {
    let path = config_path(store);
    let raw = fs::read_to_string(path).unwrap_or_default();
    let mut config = serde_json::from_str::<Value>(&raw).unwrap_or_else(|_| json!({ "tenants": {} }));
    if !config.get("tenants").map(|value| value.is_object()).unwrap_or(false) {
        config["tenants"] = json!({});
    }
    config
}

fn write_config(store: &SqliteStore, mut config: Value) -> Result<(), String> {
    config["updatedAtMs"] = json!(now_ms());
    let path = config_path(store);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("backup_config_dir_failed:{e}"))?;
    }
    let raw = serde_json::to_string_pretty(&config).map_err(|e| format!("backup_config_encode_failed:{e}"))?;
    fs::write(path, format!("{raw}\n")).map_err(|e| format!("backup_config_write_failed:{e}"))
}

fn safe_segment(value: impl ToString, fallback: &str) -> String {
    let text = normalize(value, 220);
    let safe: String = text
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '"' | '*' | '?' | '<' | '>' | '|' => '_',
            ch if ch.is_whitespace() => '_',
            ch => ch,
        })
        .collect();
    if safe.is_empty() {
        fallback.to_string()
    } else {
        safe
    }
}

fn backup_root_dir(value: impl ToString) -> PathBuf {
    PathBuf::from(normalize(value, 0))
}

fn assert_backup_root_allowed(store: &SqliteStore, root: PathBuf) -> Result<PathBuf, String> {
    if root.as_os_str().is_empty() {
        return Err("backup_root_required".to_string());
    }
    let root = root
        .canonicalize()
        .unwrap_or(root);
    let data_dir = store.data_dir.canonicalize().unwrap_or_else(|_| store.data_dir.clone());
    if root == data_dir || root.starts_with(&data_dir) {
        return Err("backup_root_inside_local_store".to_string());
    }
    Ok(root)
}

fn tenant_backup_dir(root: &Path, tenant_id: &str) -> PathBuf {
    root.join(BACKUP_NAMESPACE_DIR)
        .join("tenants")
        .join(safe_segment(tenant_id, "tenant"))
}

fn table_exists(conn: &Connection, table_name: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        params![table_name],
        |row| row.get::<_, i64>(0),
    )
    .map(|value| value == 1)
    .map_err(|e| format!("db_table_exists_failed:{e}"))
}

fn list_media_rows(store: &SqliteStore, tenant_id: &str) -> Result<Vec<MediaRow>, String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT board_id, post_id, media_id, local_path, content_type, file_name, size, archived_at_ms
             FROM board_media_files WHERE tenant_id = ?1 ORDER BY media_id",
        )
        .map_err(|e| format!("db_backup_media_prepare_failed:{e}"))?;
    let rows = stmt
        .query_map(params![tenant_id], |row| {
            Ok(MediaRow {
                board_id: row.get(0)?,
                post_id: row.get(1)?,
                media_id: row.get(2)?,
                local_path: row.get(3)?,
                content_type: row.get(4)?,
                file_name: row.get(5).unwrap_or_default(),
                size: row.get(6).unwrap_or(0),
                archived_at_ms: row.get(7).unwrap_or(0),
            })
        })
        .map_err(|e| format!("db_backup_media_query_failed:{e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("db_backup_media_row_failed:{e}"))?);
    }
    Ok(out)
}

fn list_work_note_attachment_rows(store: &SqliteStore, tenant_id: &str) -> Result<Vec<WorkNoteAttachmentRow>, String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    if !table_exists(&conn, "work_note_attachments")? { return Ok(Vec::new()); }
    let mut statement = conn.prepare(
        "SELECT attachment_id,page_id,block_id,file_name,content_type,byte_size,sha256,local_path,created_at_ms,updated_at_ms FROM work_note_attachments WHERE tenant_id=?1 ORDER BY attachment_id",
    ).map_err(|e| format!("db_backup_work_note_attachment_prepare_failed:{e}"))?;
    let rows = statement.query_map(params![tenant_id], |row| Ok(WorkNoteAttachmentRow {
        attachment_id: row.get(0)?, page_id: row.get(1)?, block_id: row.get(2)?, file_name: row.get(3)?,
        content_type: row.get(4)?, size: row.get(5)?, sha256: row.get(6)?, local_path: row.get(7)?,
        created_at_ms: row.get(8)?, updated_at_ms: row.get(9)?,
    })).map_err(|e| format!("db_backup_work_note_attachment_query_failed:{e}"))?;
    rows.map(|row| row.map_err(|e| format!("db_backup_work_note_attachment_row_failed:{e}"))).collect()
}

fn media_extension(row: &MediaRow) -> String {
    let source = if row.file_name.is_empty() {
        row.local_path.clone()
    } else {
        row.file_name.clone()
    };
    source
        .rsplit('.')
        .next()
        .filter(|value| !value.is_empty() && value.len() <= 10)
        .unwrap_or("bin")
        .to_string()
}

fn read_manifest(path: &Path) -> Result<Value, String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("backup_manifest_read_failed:{e}"))?;
    serde_json::from_str::<Value>(&raw).map_err(|e| format!("backup_manifest_decode_failed:{e}"))
}

fn backup_source(_store: &SqliteStore, created_at_ms: i64) -> Value {
    json!({
        "service": "onlineclass-local-sensitive-store",
        "serviceVersion": SERVICE_VERSION,
        "appVersion": env!("CARGO_PKG_VERSION"),
        "pcName": local_pc_name(),
        "os": local_os_name(),
        "arch": local_arch(),
        "createdAtMs": created_at_ms
    })
}

fn latest_manifest_path(store: &SqliteStore, tenant_id: &str) -> Result<PathBuf, String> {
    let list = list_backups(store, tenant_id.to_string(), 1)?;
    list.get("backups")
        .and_then(|value| value.as_array())
        .and_then(|items| items.first())
        .and_then(|item| item.get("manifestPath"))
        .and_then(|value| value.as_str())
        .map(PathBuf::from)
        .ok_or_else(|| "backup_manifest_required".to_string())
}

fn resolve_manifest_path(store: &SqliteStore, tenant_id: &str, path_value: Option<&Value>) -> Result<PathBuf, String> {
    let config = read_config(store);
    let root_text = config
        .get("tenants")
        .and_then(|tenants| tenants.get(tenant_id))
        .and_then(|tenant| tenant.get("backupRootDir"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let root = assert_backup_root_allowed(store, backup_root_dir(root_text))?;
    let base = tenant_backup_dir(&root, tenant_id)
        .canonicalize()
        .unwrap_or_else(|_| tenant_backup_dir(&root, tenant_id));
    let manifest_path = path_value
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or(latest_manifest_path(store, tenant_id)?);
    let resolved = manifest_path.canonicalize().unwrap_or(manifest_path);
    if resolved != base && !resolved.starts_with(&base) {
        return Err("backup_manifest_outside_configured_root".to_string());
    }
    Ok(resolved)
}

pub(crate) fn safe_relative_path(value: &str) -> Option<PathBuf> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return None;
    }
    if path.components().any(|component| matches!(component, Component::ParentDir | Component::RootDir | Component::Prefix(_))) {
        return None;
    }
    Some(path)
}

pub(crate) fn set_folder(store: &SqliteStore, tenant_id: String, folder_path: String) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let root = assert_backup_root_allowed(store, backup_root_dir(folder_path))?;
    fs::create_dir_all(tenant_backup_dir(&root, &tenant_id)).map_err(|e| format!("backup_tenant_dir_failed:{e}"))?;
    let mut config = read_config(store);
    config["tenants"][&tenant_id] = json!({
        "tenantId": tenant_id,
        "enabled": true,
        "backupRootDir": root.to_string_lossy(),
        "intervalMs": BACKUP_INTERVAL_MS,
        "updatedAtMs": now_ms()
    });
    write_config(store, config)?;
    status(store, tenant_id)
}

pub(crate) fn status(store: &SqliteStore, tenant_id: String) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let config = read_config(store);
    let tenant = config.get("tenants").and_then(|tenants| tenants.get(&tenant_id)).cloned().unwrap_or_else(|| json!({}));
    let root_text = tenant.get("backupRootDir").and_then(|value| value.as_str()).unwrap_or("");
    let configured = !root_text.trim().is_empty();
    let interval_ms = tenant.get("intervalMs").and_then(|value| value.as_i64()).unwrap_or(BACKUP_INTERVAL_MS);
    let last_run_at_ms = tenant.get("lastRunAtMs").and_then(|value| value.as_i64()).unwrap_or(0);
    let backups = if configured {
        list_backups(store, tenant_id.clone(), 5)?.get("backups").cloned().unwrap_or_else(|| json!([]))
    } else {
        json!([])
    };
    let latest_backup = backups.as_array().and_then(|items| items.first()).cloned().unwrap_or(Value::Null);
    Ok(json!({
        "ok": true,
        "tenantId": tenant_id,
        "configured": configured,
        "enabled": configured && tenant.get("enabled").and_then(|value| value.as_bool()).unwrap_or(true),
        "backupRootDir": root_text,
        "tenantBackupDir": if configured {
            tenant_backup_dir(&backup_root_dir(root_text), tenant_id.as_str()).to_string_lossy().to_string()
        } else {
            String::new()
        },
        "intervalMs": interval_ms,
        "lastRunAtMs": last_run_at_ms,
        "nextRunAtMs": if configured { (last_run_at_ms + interval_ms).max(now_ms()) } else { 0 },
        "lastResult": tenant.get("lastResult").cloned().unwrap_or(Value::Null),
        "latestBackup": latest_backup,
        "backups": backups,
        "securityMode": "plain_warning",
        "mediaMode": "content_addressed_objects_v5"
    }))
}

fn configured_tenant_dir(store: &SqliteStore, tenant_id: &str) -> Result<PathBuf, String> {
    let config = read_config(store);
    let root_text = config
        .get("tenants")
        .and_then(|tenants| tenants.get(tenant_id))
        .and_then(|tenant| tenant.get("backupRootDir"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if root_text.trim().is_empty() { return Err("backup_not_configured".to_string()); }
    Ok(tenant_backup_dir(&assert_backup_root_allowed(store, backup_root_dir(root_text))?, tenant_id))
}

fn pinned_sync_generations(store: &SqliteStore, tenant_id: &str) -> Result<HashSet<i64>, String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    let values = conn.query_row(
        "SELECT applied_generation,published_generation,latest_generation FROM local_store_device_sync_state WHERE tenant_id=?1",
        params![tenant_id],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)),
    ).unwrap_or((0, 0, 0));
    Ok([values.0, values.1, values.2]
        .into_iter()
        .filter(|generation| *generation > 0)
        .collect())
}

pub(crate) fn storage_overview(store: &SqliteStore, tenant_id: String) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
    if tenant_id.is_empty() { return Err("tenant_id_required".to_string()); }
    let tenant_dir = configured_tenant_dir(store, &tenant_id)?;
    let scan = crate::backup_v5::scan_storage(&tenant_dir);
    let cleanup = crate::backup_v5::legacy_cleanup_summary(&tenant_dir, &pinned_sync_generations(store, &tenant_id)?);
    let quarantine = crate::backup_v5::legacy_quarantine_summary(&tenant_dir, now_ms())
        .unwrap_or_else(|error| json!({ "ok": false, "error": error }));
    let mut current_files = HashMap::<String, Value>::new();
    for row in list_media_rows(store, &tenant_id)? {
        current_files.entry(row.local_path.clone()).or_insert_with(|| json!({
            "kind": "게시판 첨부", "name": row.file_name, "localPath": row.local_path, "bytes": row.size.max(0)
        }));
    }
    for row in list_work_note_attachment_rows(store, &tenant_id)? {
        current_files.entry(row.local_path.clone()).or_insert_with(|| json!({
            "kind": "자료 첨부", "name": row.file_name, "localPath": row.local_path, "bytes": row.size.max(0)
        }));
    }
    let current_original_count = current_files.len();
    let current_original_bytes = current_files.values().map(|item| item.get("bytes").and_then(Value::as_i64).unwrap_or(0)).sum::<i64>();
    let mut largest_files = current_files.into_values().filter(|item| item.get("bytes").and_then(Value::as_i64).unwrap_or(0) >= 100 * 1024 * 1024).collect::<Vec<_>>();
    largest_files.sort_by(|left, right| right.get("bytes").and_then(Value::as_i64).unwrap_or(0).cmp(&left.get("bytes").and_then(Value::as_i64).unwrap_or(0)));
    largest_files.truncate(10);
    Ok(json!({
        "ok": true,
        "tenantId": tenant_id,
        "snapshotVersion": crate::backup_v5::SNAPSHOT_VERSION,
        "currentOriginalBytes": current_original_bytes,
        "currentOriginalCount": current_original_count,
        "uniqueObjectCount": scan.object_count,
        "uniqueObjectBytes": scan.object_bytes,
        "databaseHistoryBytes": scan.database_history_bytes,
        "legacySnapshotCount": scan.legacy_snapshot_count,
        "legacySnapshotBytes": scan.legacy_snapshot_bytes,
        "legacyReclaimableBytes": cleanup.get("reclaimableBytes").cloned().unwrap_or(json!(0)),
        "legacyCleanupCandidateCount": cleanup.get("candidateCount").cloned().unwrap_or(json!(0)),
        "legacyQuarantineCount": quarantine.get("quarantinedCount").cloned().unwrap_or(json!(0)),
        "legacyQuarantineBytes": quarantine.get("quarantinedBytes").cloned().unwrap_or(json!(0)),
        "legacyQuarantinePurgeAfterMs": quarantine.get("purgeAfterMs").cloned().unwrap_or(json!(0)),
        "legacyQuarantineReviewCount": quarantine.get("reviewCount").cloned().unwrap_or(json!(0)),
        "legacyQuarantineError": quarantine.get("error").cloned().unwrap_or(Value::Null),
        "largeFileThresholdBytes": 100 * 1024 * 1024,
        "largestFiles": largest_files,
        "retention": { "recent": 10, "dailyDays": 30, "monthlyMonths": 12, "preRestore": 5, "manual": "explicit_delete_only" }
    }))
}

pub(crate) fn preview_legacy_cleanup(store: &SqliteStore, tenant_id: String) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
    if tenant_id.is_empty() { return Err("tenant_id_required".to_string()); }
    let tenant_dir = configured_tenant_dir(store, &tenant_id)?;
    Ok(crate::backup_v5::legacy_cleanup_preview(&tenant_dir, &pinned_sync_generations(store, &tenant_id)?))
}

pub(crate) fn apply_legacy_cleanup(store: &SqliteStore, tenant_id: String, preview_token: String) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
    if tenant_id.is_empty() { return Err("tenant_id_required".to_string()); }
    if preview_token.len() != 64 { return Err("backup_legacy_cleanup_preview_required".to_string()); }
    let tenant_dir = configured_tenant_dir(store, &tenant_id)?;
    let verified_v5_created_at_ms = latest_verified_v5_created_at(store, &tenant_id, &tenant_dir)?;
    crate::backup_v5::apply_legacy_cleanup(
        &tenant_dir,
        &pinned_sync_generations(store, &tenant_id)?,
        &preview_token,
        verified_v5_created_at_ms,
        now_ms(),
    )
}

pub(crate) fn undo_legacy_cleanup(store: &SqliteStore, tenant_id: String) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
    if tenant_id.is_empty() { return Err("tenant_id_required".to_string()); }
    let tenant_dir = configured_tenant_dir(store, &tenant_id)?;
    crate::backup_v5::undo_legacy_quarantine(&tenant_dir, now_ms())
}

fn latest_verified_v5_created_at(_store: &SqliteStore, tenant_id: &str, tenant_dir: &Path) -> Result<i64, String> {
    let mut candidates = manifest_paths_in_dir(tenant_dir)?
        .into_iter()
        .filter_map(|path| {
            let manifest = read_manifest(&path).ok()?;
            if manifest.get("version").and_then(Value::as_i64) != Some(crate::backup_v5::SNAPSHOT_VERSION)
                || manifest.get("ok").and_then(Value::as_bool) != Some(true)
                || manifest.get("tenantId").and_then(Value::as_str) != Some(tenant_id)
            {
                return None;
            }
            let created_at_ms = manifest.get("createdAtMs").and_then(Value::as_i64).unwrap_or(0);
            Some((path, manifest, created_at_ms))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| right.2.cmp(&left.2));
    for (path, manifest, created_at_ms) in candidates {
        if authoritative_restore_manifest(&path, &manifest, tenant_id).is_ok() {
            return Ok(created_at_ms);
        }
    }
    Err("backup_legacy_quarantine_verified_v5_required".to_string())
}

fn manifest_paths_in_dir(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !dir.exists() { return Ok(Vec::new()); }
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| format!("backup_list_dir_failed:{e}"))? {
        let entry = entry.map_err(|e| format!("backup_list_entry_failed:{e}"))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("manifest-") && name.ends_with(".json") {
            paths.push(entry.path());
        }
    }
    let snapshots = dir.join("snapshots");
    if snapshots.exists() {
        for entry in fs::read_dir(&snapshots).map_err(|e| format!("backup_snapshot_list_failed:{e}"))? {
            let entry = entry.map_err(|e| format!("backup_snapshot_entry_failed:{e}"))?;
            let snapshot = entry.path();
            if !snapshot.is_dir() || entry.file_name().to_string_lossy().ends_with(".staging") {
                continue;
            }
            let manifest = snapshot.join("manifest.json");
            if manifest.is_file() && snapshot.join("commit.json").is_file() {
                paths.push(manifest);
            }
        }
    }
    Ok(paths)
}

pub(crate) fn list_backups(store: &SqliteStore, tenant_id: String, limit: i64) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let config = read_config(store);
    let root_text = config
        .get("tenants")
        .and_then(|tenants| tenants.get(&tenant_id))
        .and_then(|tenant| tenant.get("backupRootDir"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if root_text.trim().is_empty() {
        return Ok(json!({ "ok": true, "backups": [] }));
    }
    let dir = tenant_backup_dir(&backup_root_dir(root_text), &tenant_id);
    if !dir.exists() {
        return Ok(json!({ "ok": true, "backups": [] }));
    }
    let max = limit.clamp(1, 50) as usize;
    let mut backups = Vec::new();
    for path in manifest_paths_in_dir(&dir)? {
        if let Ok(manifest) = read_manifest(&path) {
            let db_path = manifest.get("db")
                .and_then(|db| db.get("absolutePath"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| manifest.get("db").and_then(|db| db.get("relativePath")).and_then(Value::as_str)
                    .map(|relative| path.parent().unwrap_or_else(|| Path::new(".")).join(relative).to_string_lossy().to_string()))
                .unwrap_or_default();
            backups.push(json!({
                "ok": manifest.get("ok").and_then(|value| value.as_bool()).unwrap_or(true),
                "tenantId": manifest.get("tenantId").and_then(|value| value.as_str()).unwrap_or(&tenant_id),
                "backupId": manifest.get("backupId").and_then(|value| value.as_str()).unwrap_or(""),
                "createdAtMs": manifest.get("createdAtMs").and_then(|value| value.as_i64()).unwrap_or(0),
                "manifestPath": path.to_string_lossy(),
                "dbPath": db_path,
                "kind": manifest.get("kind").and_then(Value::as_str).unwrap_or("legacy"),
                "generation": manifest.get("generation").and_then(Value::as_i64),
                "artifactSetSha256": manifest.get("artifactSetSha256").and_then(Value::as_str).unwrap_or(""),
                "source": manifest.get("source").cloned().unwrap_or_else(|| json!({})),
                "counts": manifest.get("counts").cloned().unwrap_or_else(|| json!({})),
                "media": manifest.get("media").cloned().unwrap_or_else(|| json!({})),
                "workNoteAttachments": manifest.get("workNoteAttachments").cloned().unwrap_or_else(|| json!({}))
            }));
        }
    }
    backups.sort_by(|a, b| {
        let av = a.get("createdAtMs").and_then(|value| value.as_i64()).unwrap_or(0);
        let bv = b.get("createdAtMs").and_then(|value| value.as_i64()).unwrap_or(0);
        bv.cmp(&av)
    });
    backups.truncate(max);
    Ok(json!({ "ok": true, "backups": backups }))
}

fn file_name_is(path: &Path, expected: &str) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case(expected))
        .unwrap_or(false)
}

fn has_backup_manifest(dir: &Path) -> bool {
    manifest_paths_in_dir(dir).map(|paths| !paths.is_empty()).unwrap_or(false)
}

fn selected_backup_root_and_tenant(selected: &Path) -> (PathBuf, String) {
    if file_name_is(selected, BACKUP_NAMESPACE_DIR) {
        return (
            selected.parent().unwrap_or_else(|| Path::new("")).to_path_buf(),
            String::new(),
        );
    }
    if file_name_is(selected, "tenants") {
        if let Some(parent) = selected.parent() {
            if file_name_is(parent, BACKUP_NAMESPACE_DIR) {
                return (
                    parent.parent().unwrap_or_else(|| Path::new("")).to_path_buf(),
                    String::new(),
                );
            }
        }
    }
    if has_backup_manifest(selected) {
        if let Some(parent) = selected.parent() {
            if file_name_is(parent, "tenants") {
                if let Some(namespace) = parent.parent() {
                    if file_name_is(namespace, BACKUP_NAMESPACE_DIR) {
                        let tenant_id = selected
                            .file_name()
                            .and_then(|value| value.to_str())
                            .unwrap_or("")
                            .to_string();
                        return (
                            namespace.parent().unwrap_or_else(|| Path::new("")).to_path_buf(),
                            tenant_id,
                        );
                    }
                }
            }
        }
    }
    (selected.to_path_buf(), String::new())
}

fn backup_manifest_summary(path: &Path, fallback_tenant_id: &str) -> Option<Value> {
    let manifest = read_manifest(path).ok()?;
    let db_path = manifest
        .get("db")
        .and_then(|db| db.get("absolutePath"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
        .or_else(|| {
            manifest
                .get("db")
                .and_then(|db| db.get("relativePath"))
                .and_then(|value| value.as_str())
                .map(|relative| {
                    path.parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(relative)
                        .to_string_lossy()
                        .to_string()
                })
        })
        .unwrap_or_default();
    Some(json!({
        "ok": manifest.get("ok").and_then(|value| value.as_bool()).unwrap_or(true),
        "tenantId": manifest.get("tenantId").and_then(|value| value.as_str()).unwrap_or(fallback_tenant_id),
        "backupId": manifest.get("backupId").and_then(|value| value.as_str()).unwrap_or(""),
        "createdAtMs": manifest.get("createdAtMs").and_then(|value| value.as_i64()).unwrap_or(0),
        "manifestPath": path.to_string_lossy(),
        "dbPath": db_path,
        "source": manifest.get("source").cloned().unwrap_or_else(|| json!({})),
        "counts": manifest.get("counts").cloned().unwrap_or_else(|| json!({})),
        "media": manifest.get("media").cloned().unwrap_or_else(|| json!({})),
        "workNoteAttachments": manifest.get("workNoteAttachments").cloned().unwrap_or_else(|| json!({})),
        "archives": manifest.get("archives").cloned().unwrap_or_else(|| json!({}))
    }))
}

fn list_backup_manifests_in_dir(dir: &Path, fallback_tenant_id: &str, limit: usize) -> Result<Vec<Value>, String> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut backups = Vec::new();
    for path in manifest_paths_in_dir(dir)? {
        if let Some(summary) = backup_manifest_summary(&path, fallback_tenant_id) {
            backups.push(summary);
        }
    }
    backups.sort_by(|a, b| {
        let av = a.get("createdAtMs").and_then(|value| value.as_i64()).unwrap_or(0);
        let bv = b.get("createdAtMs").and_then(|value| value.as_i64()).unwrap_or(0);
        bv.cmp(&av)
    });
    backups.truncate(limit);
    Ok(backups)
}

pub(crate) fn discover_tenants(store: &SqliteStore, folder_path: String) -> Result<Value, String> {
    let selected = backup_root_dir(folder_path);
    if selected.as_os_str().is_empty() {
        return Err("backup_root_required".to_string());
    }
    let selected = selected.canonicalize().unwrap_or(selected);
    let (root, focused_tenant_id) = selected_backup_root_and_tenant(&selected);
    let root = assert_backup_root_allowed(store, root)?;
    let tenants_dir = root.join(BACKUP_NAMESPACE_DIR).join("tenants");
    let mut tenants = Vec::new();
    if tenants_dir.exists() {
        for entry in fs::read_dir(&tenants_dir).map_err(|e| format!("backup_discover_tenants_dir_failed:{e}"))? {
            let entry = entry.map_err(|e| format!("backup_discover_tenant_entry_failed:{e}"))?;
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let folder_tenant_id = entry.file_name().to_string_lossy().to_string();
            if !focused_tenant_id.is_empty() && folder_tenant_id != focused_tenant_id {
                continue;
            }
            let backups = list_backup_manifests_in_dir(&dir, &folder_tenant_id, 10)?;
            if backups.is_empty() {
                continue;
            }
            let latest = backups.first().cloned().unwrap_or_else(|| json!({}));
            let tenant_id = latest
                .get("tenantId")
                .and_then(|value| value.as_str())
                .filter(|value| !value.trim().is_empty())
                .map(|value| value.to_string())
                .unwrap_or_else(|| folder_tenant_id.clone());
            tenants.push(json!({
                "tenantId": tenant_id,
                "tenantBackupDir": dir.to_string_lossy(),
                "latestBackup": latest,
                "backups": backups
            }));
        }
    }
    tenants.sort_by(|a, b| {
        let av = a
            .get("latestBackup")
            .and_then(|backup| backup.get("createdAtMs"))
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        let bv = b
            .get("latestBackup")
            .and_then(|backup| backup.get("createdAtMs"))
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        bv.cmp(&av)
    });
    Ok(json!({
        "ok": true,
        "selectedPath": selected.to_string_lossy(),
        "backupRootDir": root.to_string_lossy(),
        "namespaceDir": root.join(BACKUP_NAMESPACE_DIR).to_string_lossy(),
        "tenantCount": tenants.len(),
        "tenants": tenants
    }))
}

fn export_tenant_db(store: &SqliteStore, tenant_id: &str, db_path: &Path) -> Result<(), String> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("backup_db_dir_failed:{e}"))?;
    }
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    conn.execute("ATTACH DATABASE ?1 AS backup", params![db_path.to_string_lossy().to_string()])
        .map_err(|e| format!("backup_db_attach_failed:{e}"))?;
    let result = (|| -> Result<(), String> {
        conn.execute_batch(&backup_schema_sql("backup."))
            .map_err(|e| format!("backup_schema_failed:{e}"))?;
        for table in BACKUP_TABLES {
            if table.optional && !table_exists(&conn, table.name)? {
                continue;
            }
            if !table_exists(&conn, table.name)? {
                continue;
            }
            let columns = table.columns.join(", ");
            let sql = format!(
                "INSERT INTO backup.{name} ({columns}) SELECT {columns} FROM main.{name} WHERE tenant_id = ?1",
                name = table.name,
                columns = columns
            );
            conn.execute(&sql, params![tenant_id])
                .map_err(|e| format!("backup_table_copy_failed:{}:{e}", table.name))?;
        }
        Ok(())
    })();
    let _ = conn.execute_batch("DETACH DATABASE backup");
    result
}

pub(crate) fn run_with_kind(
    store: &SqliteStore,
    tenant_id: String,
    kind: &str,
    generation: Option<i64>,
) -> Result<Value, String> {
    run_with_kind_version(
        store,
        tenant_id,
        kind,
        generation,
        crate::backup_v5::SNAPSHOT_VERSION,
    )
}

pub(crate) fn run_with_kind_version(
    store: &SqliteStore,
    tenant_id: String,
    kind: &str,
    generation: Option<i64>,
    snapshot_version: i64,
) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    if !matches!(kind, "manual" | "scheduled" | "pre_restore" | "auto_sync") {
        return Err("backup_kind_invalid".to_string());
    }
    if generation.is_some_and(|value| value < 1) {
        return Err("backup_generation_invalid".to_string());
    }
    if !matches!(snapshot_version, 4 | 5) {
        return Err("backup_snapshot_version_invalid".to_string());
    }
    let config = read_config(store);
    let root_text = config
        .get("tenants")
        .and_then(|tenants| tenants.get(&tenant_id))
        .and_then(|tenant| tenant.get("backupRootDir"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let root = assert_backup_root_allowed(store, backup_root_dir(root_text))?;
    let out_dir = tenant_backup_dir(&root, &tenant_id);
    let created_at_ms = now_ms();
    let backup_id = format!("{}", Utc::now().format("%Y%m%d%H%M%S%3f"));
    let snapshots_dir = out_dir.join("snapshots");
    fs::create_dir_all(&snapshots_dir).map_err(|e| format!("backup_dir_failed:{e}"))?;
    let staging_dir = snapshots_dir.join(format!("{backup_id}.staging"));
    let snapshot_dir = snapshots_dir.join(&backup_id);
    if staging_dir.exists() { fs::remove_dir_all(&staging_dir).map_err(|e| format!("backup_staging_cleanup_failed:{e}"))?; }
    fs::create_dir_all(staging_dir.join("db")).map_err(|e| format!("backup_dir_failed:{e}"))?;
    if snapshot_version == 4 {
        fs::create_dir_all(staging_dir.join("board-media")).map_err(|e| format!("backup_media_dir_failed:{e}"))?;
        fs::create_dir_all(staging_dir.join("work-note-attachments")).map_err(|e| format!("backup_work_note_attachment_dir_failed:{e}"))?;
    }
    let sync = match generation {
        Some(value) => sync_manifest(store, &tenant_id, value)?,
        None => Value::Null,
    };
    let db_relative_path = PathBuf::from("db").join("local-sensitive.sqlite");
    let db_path = staging_dir.join(&db_relative_path);
    export_tenant_db(store, &tenant_id, &db_path)?;
    let (database_size, database_sha256) = sha256_file(&db_path)?;
    let mut artifacts = vec![ArtifactDigest {
        relative_path: db_relative_path.to_string_lossy().replace('\\', "/"),
        size: database_size,
        sha256: database_sha256.clone(),
    }];
    let mut artifact_paths = HashSet::from([
        db_relative_path.to_string_lossy().replace('\\', "/"),
    ]);

    let media_rows = list_media_rows(store, &tenant_id)?;
    let mut media_records = Vec::new();
    let mut copied = 0i64;
    let mut skipped = 0i64;
    let mut missing = 0i64;
    let mut failed = 0i64;
    let mut bytes = 0i64;
    for row in media_rows {
        let ext = media_extension(&row);
        let legacy_relative_path = PathBuf::from("board-media")
            .join(&backup_id)
            .join(safe_segment(&row.board_id, "board"))
            .join(format!("{}.{}", safe_segment(&row.media_id, "media"), ext));
        let source_path = store.data_dir.join(&row.local_path);
        let mut status = "copied";
        let mut artifact = None;
        match fs::metadata(&source_path) {
            Ok(source_meta) => {
                if snapshot_version == crate::backup_v5::SNAPSHOT_VERSION {
                    match crate::backup_v5::put_object(&out_dir, &source_path, &backup_id) {
                        Ok(object) => {
                            if object.created { copied += 1; } else { skipped += 1; status = "skipped"; }
                            artifact = Some(object.artifact);
                        }
                        Err(_) => { failed += 1; status = "failed"; }
                    }
                } else {
                    let target_path = staging_dir.join(&legacy_relative_path);
                    if let Some(parent) = target_path.parent() {
                        fs::create_dir_all(parent).map_err(|e| format!("backup_media_target_dir_failed:{e}"))?;
                    }
                    if fs::copy(&source_path, &target_path).is_err() {
                        failed += 1; status = "failed";
                    } else {
                        copied += 1;
                        let digest = sha256_file(&target_path)?;
                        artifact = Some(ArtifactDigest { relative_path: legacy_relative_path.to_string_lossy().replace('\\', "/"), size: digest.0, sha256: digest.1 });
                    }
                }
                bytes += source_meta.len() as i64;
            }
            Err(_) => {
                missing += 1;
                status = "missing";
            }
        }
        let (backup_relative_path, artifact_size, artifact_sha256) = if let Some(artifact) = artifact {
            let relative = artifact.relative_path.clone();
            let size = artifact.size as i64;
            let sha256 = artifact.sha256.clone();
            if artifact_paths.insert(relative.clone()) { artifacts.push(artifact); }
            (relative, size, sha256)
        } else { (legacy_relative_path.to_string_lossy().replace('\\', "/"), 0, String::new()) };
        media_records.push(json!({
            "boardId": row.board_id,
            "postId": row.post_id,
            "mediaId": row.media_id,
            "localPath": row.local_path,
            "backupRelativePath": backup_relative_path,
            "contentType": row.content_type,
            "fileName": row.file_name,
            "size": if artifact_size > 0 { artifact_size } else { row.size },
            "sha256": artifact_sha256,
            "archivedAtMs": row.archived_at_ms,
            "status": status
        }));
    }
    let attachment_rows = list_work_note_attachment_rows(store, &tenant_id)?;
    let attachment_count = attachment_rows.len() as i64;
    let mut attachment_records = Vec::new();
    let mut attachments_copied = 0i64;
    let mut attachments_skipped = 0i64;
    let mut attachments_missing = 0i64;
    let mut attachments_failed = 0i64;
    let mut attachment_bytes = 0i64;
    for row in attachment_rows {
        let legacy_relative_path = PathBuf::from("work-note-attachments")
            .join(&backup_id)
            .join(safe_segment(&row.attachment_id, "attachment"))
            .join(safe_segment(&row.file_name, "attachment.bin"));
        let source_path = store.data_dir.join(&row.local_path);
        let mut status = "copied";
        let mut artifact = None;
        match fs::metadata(&source_path) {
            Ok(source_meta) => {
                if snapshot_version == crate::backup_v5::SNAPSHOT_VERSION {
                    match crate::backup_v5::put_object(&out_dir, &source_path, &backup_id) {
                        Ok(object) => {
                            if object.created { attachments_copied += 1; } else { attachments_skipped += 1; status = "skipped"; }
                            artifact = Some(object.artifact);
                        }
                        Err(_) => { attachments_failed += 1; status = "failed"; }
                    }
                } else {
                    let target_path = staging_dir.join(&legacy_relative_path);
                    if let Some(parent) = target_path.parent() {
                        fs::create_dir_all(parent).map_err(|e| format!("backup_work_note_attachment_target_dir_failed:{e}"))?;
                    }
                    if fs::copy(&source_path, &target_path).is_err() {
                        attachments_failed += 1; status = "failed";
                    } else {
                        attachments_copied += 1;
                        let digest = sha256_file(&target_path)?;
                        artifact = Some(ArtifactDigest { relative_path: legacy_relative_path.to_string_lossy().replace('\\', "/"), size: digest.0, sha256: digest.1 });
                    }
                }
                attachment_bytes += source_meta.len() as i64;
            }
            Err(_) => {
                attachments_missing += 1;
                status = "missing";
            }
        }
        let (backup_relative_path, artifact_size, artifact_sha256) = if let Some(artifact) = artifact {
            let relative = artifact.relative_path.clone();
            let size = artifact.size as i64;
            let sha256 = artifact.sha256.clone();
            if artifact_paths.insert(relative.clone()) { artifacts.push(artifact); }
            (relative, size, sha256)
        } else { (legacy_relative_path.to_string_lossy().replace('\\', "/"), 0, String::new()) };
        attachment_records.push(json!({
            "attachmentId": row.attachment_id,
            "pageId": row.page_id,
            "blockId": row.block_id,
            "fileName": row.file_name,
            "contentType": row.content_type,
            "size": if artifact_size > 0 { artifact_size } else { row.size },
            "sha256": if artifact_sha256.is_empty() { row.sha256 } else { artifact_sha256 },
            "localPath": row.local_path,
            "backupRelativePath": backup_relative_path,
            "createdAtMs": row.created_at_ms,
            "updatedAtMs": row.updated_at_ms,
            "status": status
        }));
    }
    let stats = store.stats(tenant_id.clone())?;
    let archives = crate::shared_archive_sync::ensure_tenant_bundles(&tenant_id, &out_dir)?;
    let counts = json!({
        "observationCount": stats.get("observationCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "teacherCounselingSessionCount": stats.get("teacherCounselingSessionCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "studentPrivateDetailCount": stats.get("studentPrivateDetailCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "mathDailyAttemptCount": stats.get("mathDailyAttemptCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "mathDailyProfileCount": stats.get("mathDailyProfileCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "mathDailyReviewSessionCount": stats.get("mathDailyReviewSessionCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "mathDailyAssignmentCount": stats.get("mathDailyAssignmentCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "mathDailyAssignmentResultCount": stats.get("mathDailyAssignmentResultCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "mathDailyCacheRunCount": stats.get("mathDailyCacheRunCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "boardSnapshotCount": stats.get("boardSnapshotCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "boardMediaCount": stats.get("boardMediaCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "attendanceRecordCount": stats.get("attendanceRecordCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "attendanceNaisCheckCount": stats.get("attendanceNaisCheckCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "attendanceDocumentRequestCount": stats.get("attendanceDocumentRequestCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "counselingRecordCount": stats.get("counselingRecordCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "counselingTeacherNoteCount": stats.get("counselingTeacherNoteCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "evalAssignmentCount": stats.get("evalAssignmentCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "evalResultCount": stats.get("evalResultCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "studentRecordDraftSetCount": stats.get("studentRecordDraftSetCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "studentRecordDraftCount": stats.get("studentRecordDraftCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "importRunCount": stats.get("importRunCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "workNoteCount": stats.get("workNoteCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "workNoteAttachmentCount": attachment_count,
        "cloudSyncRunCount": stats.get("cloudSyncRunCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "sharedArchiveCount": archives.get("count").and_then(Value::as_i64).unwrap_or(0),
        "sharedArchiveBoardCount": archives.get("boardCount").and_then(Value::as_i64).unwrap_or(0),
        "sharedArchiveAssignmentCount": archives.get("assignmentCount").and_then(Value::as_i64).unwrap_or(0),
        "sharedArchiveFileCount": archives.get("fileCount").and_then(Value::as_i64).unwrap_or(0),
    });
    let database = json!({
        "relativePath": db_relative_path.to_string_lossy().replace('\\', "/"),
        "size": database_size,
        "sha256": database_sha256
    });
    let media = json!({
        "mode": if snapshot_version == 5 { "content_addressed_objects" } else { "separate_folder_mirror" },
        "copied": copied,
        "skipped": skipped,
        "missing": missing,
        "failed": failed,
        "bytes": bytes,
        "records": media_records
    });
    let work_note_attachments = json!({
        "mode": if snapshot_version == 5 { "content_addressed_objects" } else { "separate_folder_mirror" },
        "copied": attachments_copied,
        "skipped": attachments_skipped,
        "missing": attachments_missing,
        "failed": attachments_failed,
        "bytes": attachment_bytes,
        "records": attachment_records
    });
    let (_, apply_index_size, apply_index_sha256) = crate::backup_v4::write_apply_index(
        &staging_dir,
        &tenant_id,
        generation,
        database.clone(),
        sync.clone(),
        media.clone(),
        work_note_attachments.clone(),
        archives.clone(),
        counts.clone(),
    )?;
    artifacts.push(ArtifactDigest {
        relative_path: crate::backup_v4::APPLY_INDEX_RELATIVE_PATH.to_string(),
        size: apply_index_size,
        sha256: apply_index_sha256.clone(),
    });
    let artifact_set_sha256 = artifact_set_sha256(&mut artifacts);
    let artifact_records = artifacts.iter().map(|artifact| json!({
        "relativePath": artifact.relative_path,
        "size": artifact.size,
        "sha256": artifact.sha256
    })).collect::<Vec<_>>();
    let snapshot_ok = failed == 0 && missing == 0 && attachments_failed == 0 && attachments_missing == 0;
    let manifest = json!({
        "ok": snapshot_ok,
        "version": snapshot_version,
        "kind": kind,
        "generation": generation,
        "tenantId": tenant_id,
        "backupId": backup_id,
        "createdAtMs": created_at_ms,
        "source": backup_source(store, created_at_ms),
        "db": database,
        "applyIndex": {
            "relativePath": crate::backup_v4::APPLY_INDEX_RELATIVE_PATH,
            "size": apply_index_size,
            "sha256": apply_index_sha256,
        },
        "artifactSetSha256": artifact_set_sha256,
        "artifacts": artifact_records,
        "sync": sync,
        "counts": counts,
        "media": media,
        "workNoteAttachments": work_note_attachments,
        "archives": archives,
        "securityMode": "plain_warning"
    });
    let manifest_path = staging_dir.join("manifest.json");
    let manifest_raw = serde_json::to_string_pretty(&manifest).map_err(|e| format!("backup_manifest_encode_failed:{e}"))?;
    fs::write(&manifest_path, format!("{manifest_raw}\n")).map_err(|e| format!("backup_manifest_write_failed:{e}"))?;
    let commit = json!({
        "version": 1,
        "tenantId": tenant_id,
        "backupId": backup_id,
        "generation": generation,
        "artifactSetSha256": artifact_set_sha256,
        "committedAtMs": now_ms()
    });
    let commit_raw = serde_json::to_string_pretty(&commit).map_err(|e| format!("backup_commit_encode_failed:{e}"))?;
    fs::write(staging_dir.join("commit.json"), format!("{commit_raw}\n"))
        .map_err(|e| format!("backup_commit_write_failed:{e}"))?;
    fs::rename(&staging_dir, &snapshot_dir).map_err(|e| format!("backup_snapshot_commit_failed:{e}"))?;
    let manifest_path = snapshot_dir.join("manifest.json");
    let final_db_path = snapshot_dir.join(&db_relative_path);
    let mut result = json!({
        "ok": snapshot_ok,
        "tenantId": tenant_id,
        "backupId": backup_id,
        "manifestPath": manifest_path.to_string_lossy(),
        "dbPath": final_db_path.to_string_lossy(),
        "kind": kind,
        "generation": generation,
        "artifactSetSha256": artifact_set_sha256,
        "databaseSha256": database_sha256,
        "createdAtMs": created_at_ms,
        "source": manifest.get("source").cloned().unwrap_or_else(|| json!({})),
        "counts": manifest.get("counts").cloned().unwrap_or_else(|| json!({})),
        "media": manifest.get("media").cloned().unwrap_or_else(|| json!({})),
        "workNoteAttachments": manifest.get("workNoteAttachments").cloned().unwrap_or_else(|| json!({})),
        "archives": manifest.get("archives").cloned().unwrap_or_else(|| json!({}))
    });
    if snapshot_version == crate::backup_v5::SNAPSHOT_VERSION && snapshot_ok {
        authoritative_restore_manifest(&manifest_path, &manifest, &tenant_id)?;
        crate::backup_v5::prune_snapshots(&out_dir, created_at_ms)?;
        let references = artifacts
            .iter()
            .filter(|artifact| artifact.relative_path.starts_with("objects/sha256/"))
            .map(|artifact| artifact.relative_path.clone())
            .collect::<HashSet<_>>();
        let _ = crate::backup_v5::quarantine_unreferenced_objects(&out_dir, &references, created_at_ms)?;
        let pinned_generations = pinned_sync_generations(store, &tenant_id)?;
        result["legacyQuarantineMaintenance"] = crate::backup_v5::maintain_legacy_quarantine(
            &out_dir,
            &pinned_generations,
            created_at_ms,
            created_at_ms,
        )
        .unwrap_or_else(|error| json!({ "ok": false, "error": error }));
    } else if kind == "auto_sync" {
        prune_auto_sync_snapshots(&out_dir, created_at_ms)?;
    }
    let mut config = read_config(store);
    let root_text = root.to_string_lossy().to_string();
    config["tenants"][manifest.get("tenantId").and_then(|value| value.as_str()).unwrap_or("")] = json!({
        "tenantId": manifest.get("tenantId").and_then(|value| value.as_str()).unwrap_or(""),
        "enabled": true,
        "backupRootDir": root_text,
        "intervalMs": BACKUP_INTERVAL_MS,
        "lastRunAtMs": created_at_ms,
        "lastResult": result
    });
    write_config(store, config)?;
    Ok(result)
}

pub(crate) fn run_now(store: &SqliteStore, tenant_id: String) -> Result<Value, String> {
    run_with_kind(store, tenant_id, "manual", None)
}

fn prune_auto_sync_snapshots(tenant_dir: &Path, now: i64) -> Result<(), String> {
    let snapshots_root = tenant_dir.join("snapshots");
    let mut snapshots = manifest_paths_in_dir(tenant_dir)?
        .into_iter()
        .filter_map(|path| {
            let manifest = read_manifest(&path).ok()?;
            if !matches!(
                manifest.get("version").and_then(Value::as_i64),
                Some(3) | Some(4)
            ) || manifest.get("kind").and_then(Value::as_str) != Some("auto_sync")
            {
                return None;
            }
            Some((
                path,
                manifest
                    .get("createdAtMs")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
            ))
        })
        .collect::<Vec<_>>();
    snapshots.sort_by(|left, right| right.1.cmp(&left.1));
    let mut keep = HashSet::new();
    let mut days = HashSet::new();
    for (index, (path, created_at)) in snapshots.iter().enumerate() {
        if index < AUTO_SYNC_RECENT_KEEP { keep.insert(path.clone()); }
        let age = now.saturating_sub(*created_at);
        if age >= 0 && age <= AUTO_SYNC_DAILY_KEEP_DAYS * 24 * 60 * 60 * 1000 {
            let day_key = created_at.div_euclid(24 * 60 * 60 * 1000);
            if days.insert(day_key) { keep.insert(path.clone()); }
        }
    }
    for (path, _) in snapshots {
        if keep.contains(&path) { continue; }
        let snapshot = path.parent().ok_or_else(|| "backup_snapshot_parent_missing".to_string())?;
        if snapshot.parent() != Some(snapshots_root.as_path()) {
            return Err("backup_snapshot_prune_scope_invalid".to_string());
        }
        fs::remove_dir_all(snapshot).map_err(|e| format!("backup_snapshot_prune_failed:{e}"))?;
    }
    Ok(())
}

pub(crate) fn auto_configure_onedrive(store: &SqliteStore, tenant_id: &str) -> Result<Value, String> {
    let current = status(store, tenant_id.to_string())?;
    if current.get("configured").and_then(Value::as_bool) == Some(true) { return Ok(current); }
    let mut roots = Vec::new();
    for key in ["OneDriveCommercial", "OneDriveConsumer", "OneDrive"] {
        let Ok(value) = std::env::var(key) else { continue };
        let root = PathBuf::from(value).canonicalize().unwrap_or_else(|_| PathBuf::from(std::env::var(key).unwrap_or_default()));
        if root.as_os_str().is_empty() { continue; }
        let mut candidates = vec![root.clone()];
        if let Ok(entries) = fs::read_dir(&root) {
            candidates.extend(entries.filter_map(Result::ok).filter_map(|entry| {
                entry.file_type().ok().filter(|kind| kind.is_dir()).map(|_| entry.path())
            }));
        }
        for candidate in candidates {
            if roots.contains(&candidate) { continue; }
            if has_backup_manifest(&tenant_backup_dir(&candidate, tenant_id)) { roots.push(candidate); }
        }
    }
    if roots.len() != 1 { return Ok(current); }
    set_folder(store, tenant_id.to_string(), roots.remove(0).to_string_lossy().to_string())
}

fn verify_snapshot_artifacts(
    path: &Path,
    manifest: &Value,
    tenant_id: &str,
    expected_generation: Option<i64>,
    expected_root: &str,
) -> Result<(), String> {
    let base = path
        .parent()
        .ok_or_else(|| "backup_manifest_parent_missing".to_string())?;
    let commit = read_manifest(&base.join("commit.json"))?;
    if commit.get("tenantId").and_then(Value::as_str) != Some(tenant_id)
        || commit.get("generation").and_then(Value::as_i64) != expected_generation
        || commit.get("artifactSetSha256").and_then(Value::as_str) != Some(expected_root)
    {
        return Err("backup_commit_checkpoint_mismatch".to_string());
    }
    let mut verified = Vec::new();
    for artifact in manifest.get("artifacts").and_then(Value::as_array).ok_or_else(|| "backup_artifacts_required".to_string())? {
        let relative = artifact.get("relativePath").and_then(Value::as_str).ok_or_else(|| "backup_artifact_path_required".to_string())?;
        let safe = safe_relative_path(relative).ok_or_else(|| "backup_artifact_path_invalid".to_string())?;
        let expected_size = artifact.get("size").and_then(Value::as_u64).ok_or_else(|| "backup_artifact_size_required".to_string())?;
        let expected_sha = artifact.get("sha256").and_then(Value::as_str).ok_or_else(|| "backup_artifact_hash_required".to_string())?;
        let version = manifest.get("version").and_then(Value::as_i64).unwrap_or(0);
        let artifact_path = crate::backup_v5::artifact_path(path, version, &safe)?;
        let (size, sha256) = sha256_file(&artifact_path)?;
        if size != expected_size || sha256 != expected_sha { return Err("backup_artifact_digest_mismatch".to_string()); }
        verified.push(ArtifactDigest { relative_path: relative.to_string(), size, sha256 });
    }
    Ok(())
}

fn verify_checkpoint_manifest_path(
    path: &Path,
    tenant_id: &str,
    expected_generation: i64,
    expected_root: &str,
) -> Result<Value, String> {
    let manifest = read_manifest(path)?;
    let version = manifest.get("version").and_then(Value::as_i64).unwrap_or(0);
    if !matches!(version, 3 | 4 | 5)
        || manifest.get("tenantId").and_then(Value::as_str) != Some(tenant_id)
        || manifest.get("generation").and_then(Value::as_i64) != Some(expected_generation)
        || manifest.get("artifactSetSha256").and_then(Value::as_str) != Some(expected_root)
    {
        return Err("backup_manifest_checkpoint_mismatch".to_string());
    }
    verify_snapshot_artifacts(
        path,
        &manifest,
        tenant_id,
        Some(expected_generation),
        expected_root,
    )?;
    let authoritative = if matches!(version, 4 | 5) {
        crate::backup_v4::verify_authoritative_index(
            path,
            &manifest,
            tenant_id,
            Some(expected_generation),
        )?
    } else {
        manifest.clone()
    };
    Ok(json!({
        "ok": true,
        "tenantId": tenant_id,
        "generation": expected_generation,
        "artifactSetSha256": expected_root,
        "manifestPath": path.to_string_lossy(),
        "databaseSha256": authoritative.get("db").and_then(|db| db.get("sha256")).and_then(Value::as_str).unwrap_or(""),
        "contentSha256": authoritative.get("sync").and_then(|sync| sync.get("contentSha256")).and_then(Value::as_str).unwrap_or("")
    }))
}

pub(crate) fn find_and_verify_generation(
    store: &SqliteStore,
    tenant_id: &str,
    generation: i64,
    artifact_set_sha256: &str,
) -> Result<Option<Value>, String> {
    let configured = auto_configure_onedrive(store, tenant_id)?;
    let root_text = configured.get("backupRootDir").and_then(Value::as_str).unwrap_or("");
    if root_text.is_empty() { return Ok(None); }
    let tenant_dir = tenant_backup_dir(&backup_root_dir(root_text), tenant_id);
    let mut mismatch = None;
    for path in manifest_paths_in_dir(&tenant_dir)? {
        let manifest = match read_manifest(&path) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if !matches!(
            manifest.get("version").and_then(Value::as_i64),
            Some(3) | Some(4) | Some(5)
        ) || manifest.get("generation").and_then(Value::as_i64) != Some(generation)
        {
            continue;
        }
        match verify_checkpoint_manifest_path(&path, tenant_id, generation, artifact_set_sha256) {
            Ok(value) => return Ok(Some(value)),
            Err(error) => mismatch = Some(error),
        }
    }
    if let Some(error) = mismatch { return Err(error); }
    Ok(None)
}

pub(crate) fn highest_local_generation(store: &SqliteStore, tenant_id: &str) -> Result<i64, String> {
    let configured = auto_configure_onedrive(store, tenant_id)?;
    let root_text = configured.get("backupRootDir").and_then(Value::as_str).unwrap_or("");
    if root_text.is_empty() {
        return Ok(0);
    }
    let tenant_dir = tenant_backup_dir(&backup_root_dir(root_text), tenant_id);
    let mut highest = 0i64;
    for path in manifest_paths_in_dir(&tenant_dir)? {
        let manifest = match read_manifest(&path) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if !matches!(
            manifest.get("version").and_then(Value::as_i64),
            Some(3) | Some(4) | Some(5)
        ) {
            continue;
        }
        highest = highest.max(manifest.get("generation").and_then(Value::as_i64).unwrap_or(0));
    }
    Ok(highest)
}

pub(crate) fn run_from_body(store: &SqliteStore, body: Value) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(body.get("tenantId"));
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let folder = normalize_json_text(body.get("backupRootDir"), 0);
    if !folder.is_empty() {
        let _ = set_folder(store, tenant_id.clone(), folder)?;
    }
    run_now(store, tenant_id)
}

fn authoritative_restore_manifest(
    manifest_path: &Path,
    manifest: &Value,
    tenant_id: &str,
) -> Result<Value, String> {
    if !matches!(manifest.get("version").and_then(Value::as_i64), Some(4) | Some(5)) {
        return Ok(manifest.clone());
    }
    if manifest.get("tenantId").and_then(Value::as_str) != Some(tenant_id) {
        return Err("backup_manifest_tenant_mismatch".to_string());
    }
    let expected_root = manifest
        .get("artifactSetSha256")
        .and_then(Value::as_str)
        .filter(|value| value.len() == 64)
        .ok_or_else(|| "backup_artifact_set_required".to_string())?;
    let generation = manifest.get("generation").and_then(Value::as_i64);
    verify_snapshot_artifacts(
        manifest_path,
        manifest,
        tenant_id,
        generation,
        expected_root,
    )?;
    crate::backup_v4::verify_authoritative_index(manifest_path, manifest, tenant_id, generation)
}

pub(crate) fn restore_preview(store: &SqliteStore, body: Value) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(body.get("tenantId"));
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let manifest_path = resolve_manifest_path(store, &tenant_id, body.get("manifestPath"))?;
    let manifest = read_manifest(&manifest_path)?;
    let authoritative = authoritative_restore_manifest(&manifest_path, &manifest, &tenant_id)?;
    let db_relative = authoritative
        .get("db")
        .and_then(|db| db.get("relativePath"))
        .and_then(|value| value.as_str())
        .ok_or_else(|| "backup_db_required".to_string())?;
    let db_path = manifest_path.parent().unwrap_or_else(|| Path::new(".")).join(db_relative);
    let conn = Connection::open(&db_path).map_err(|e| format!("backup_db_open_failed:{e}"))?;
    let mut counts = serde_json::Map::new();
    for table in BACKUP_TABLES {
        if !table_exists(&conn, table.name)? {
            continue;
        }
        let sql = format!("SELECT COUNT(*) FROM {} WHERE tenant_id = ?1", table.name);
        let count = conn
            .query_row(&sql, params![tenant_id], |row| row.get::<_, i64>(0))
            .unwrap_or(0);
        counts.insert(table.name.to_string(), json!(count));
    }
    if let Some(archive_counts) = authoritative.get("counts").and_then(Value::as_object) {
        for key in [
            "sharedArchiveCount",
            "sharedArchiveBoardCount",
            "sharedArchiveAssignmentCount",
            "sharedArchiveFileCount",
        ] {
            if let Some(value) = archive_counts.get(key) {
                counts.insert(key.to_string(), value.clone());
            }
        }
    }
    Ok(json!({
        "ok": true,
        "tenantId": tenant_id,
        "backupId": manifest.get("backupId").and_then(|value| value.as_str()).unwrap_or(""),
        "manifestPath": manifest_path.to_string_lossy(),
        "createdAtMs": manifest.get("createdAtMs").and_then(|value| value.as_i64()).unwrap_or(0),
        "source": manifest.get("source").cloned().unwrap_or_else(|| json!({})),
        "counts": counts,
        "media": authoritative.get("media").cloned().unwrap_or_else(|| json!({})),
        "workNoteAttachments": authoritative.get("workNoteAttachments").cloned().unwrap_or_else(|| json!({})),
        "archives": authoritative.get("archives").cloned().unwrap_or_else(|| json!({}))
    }))
}

pub(crate) fn restore(store: &SqliteStore, body: Value) -> Result<Value, String> {
    restore_runtime::restore(store, body)
}

pub(crate) fn restore_generation(
    store: &SqliteStore,
    tenant_id: &str,
    manifest_path: &Path,
    generation: i64,
    latest_status: &str,
    force_all: bool,
) -> Result<Value, String> {
    restore_runtime::restore_generation(store, tenant_id, manifest_path, generation, latest_status, force_all)
}

pub(crate) fn start_background(store: Arc<SqliteStore>) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(90));
        let config = read_config(&store);
        let tenants = config.get("tenants").and_then(|value| value.as_object()).cloned().unwrap_or_default();
        for (tenant_id, tenant) in tenants {
            if tenant.get("enabled").and_then(|value| value.as_bool()) == Some(false) {
                continue;
            }
            let last = tenant.get("lastRunAtMs").and_then(|value| value.as_i64()).unwrap_or(0);
            let interval = tenant.get("intervalMs").and_then(|value| value.as_i64()).unwrap_or(BACKUP_INTERVAL_MS);
            if now_ms() >= last + interval {
                let _ = run_with_kind(&store, tenant_id, "scheduled", None);
            }
        }
        thread::sleep(Duration::from_secs(15 * 60));
    });
}
