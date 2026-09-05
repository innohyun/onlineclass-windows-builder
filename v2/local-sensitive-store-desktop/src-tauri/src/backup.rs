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
#[cfg(test)]
#[path = "backup_optimization_tests.rs"]
mod optimization_tests;

const BACKUP_CONFIG_FILE: &str = "backup-config.json";
const BACKUP_NAMESPACE_DIR: &str = "OnlineClassLocalBackups";
const BACKUP_INTERVAL_MS: i64 = 24 * 60 * 60 * 1000;

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
    pub(crate) change_sequence: i64,
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

#[path = "backup_tracking.rs"]
mod tracking;
pub(crate) use tracking::*;
#[path = "backup_runtime.rs"]
mod runtime;
pub(crate) use runtime::*;
#[path = "backup_capture.rs"]
mod capture;
#[path = "backup_maintenance.rs"]
mod maintenance;

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
    media_rows_from(&conn, tenant_id)
}

fn media_rows_from(conn: &Connection, tenant_id: &str) -> Result<Vec<MediaRow>, String> {
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
    attachment_rows_from(&conn, tenant_id)
}

fn attachment_rows_from(conn: &Connection, tenant_id: &str) -> Result<Vec<WorkNoteAttachmentRow>, String> {
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

#[path = "backup_catalog.rs"]
mod catalog;
pub(crate) use catalog::*;



#[path = "backup_snapshot.rs"]
mod snapshot;
pub(crate) use snapshot::*;

pub(crate) fn auto_configure_onedrive(store: &SqliteStore, tenant_id: &str) -> Result<Value, String> {
    let current = connection_status(store, tenant_id)?;
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
    if artifact_set_sha256(&mut verified) != expected_root {
        return Err("backup_artifact_set_digest_mismatch".to_string());
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
    let tenant_dir = match configured_tenant_dir(store,tenant_id) {
        Ok(dir) => dir,
        Err(error) if error=="backup_not_configured" => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut highest = 0i64;
    for path in manifest_paths_in_dir(&tenant_dir)? {
        let manifest = match listed_manifest(&path) {
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

pub(crate) fn authoritative_restore_manifest(
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
    let _operation = root_operation(store, &configured_tenant_dir(store, &tenant_id)?)?;
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
    let tenant_id = normalize_tenant_id(body.get("tenantId"));
    let _operation = root_operation(store, &configured_tenant_dir(store, &tenant_id)?)?;
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
    let _operation = root_operation(store, &configured_tenant_dir(store, tenant_id)?)?;
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
            let _ = maintenance::run_if_due(&store, &tenant_id, now_ms(), false);
            let last = tenant.get("lastRunAtMs").and_then(|value| value.as_i64()).unwrap_or(0);
            let interval = tenant.get("intervalMs").and_then(|value| value.as_i64()).unwrap_or(BACKUP_INTERVAL_MS);
            if now_ms() >= last + interval {
                let _ = run_with_kind(&store, tenant_id, "scheduled", None);
            }
        }
        thread::sleep(Duration::from_secs(15 * 60));
    });
}
