use crate::{local_arch, local_os_name, local_pc_name, normalize, normalize_json_text, normalize_tenant_id, SqliteStore, SERVICE_VERSION};
use chrono::Utc;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const BACKUP_CONFIG_FILE: &str = "backup-config.json";
const BACKUP_NAMESPACE_DIR: &str = "OnlineClassLocalBackups";
const BACKUP_INTERVAL_MS: i64 = 24 * 60 * 60 * 1000;

struct BackupTable {
    name: &'static str,
    columns: &'static [&'static str],
    key_columns: &'static [&'static str],
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
        name: "student_private_details",
        columns: &["tenant_id", "student_code", "payload_json", "updated_at_ms"],
        key_columns: &["tenant_id", "student_code"],
        timestamp_column: "updated_at_ms",
        optional: false,
    },
    BackupTable {
        name: "student_private_photos",
        columns: &["tenant_id", "student_code", "content_type", "content_base64", "sha256", "byte_size", "updated_at_ms"],
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
        name: "cloud_sync_runs",
        columns: &["tenant_id", "run_id", "payload_json", "started_at_ms", "finished_at_ms"],
        key_columns: &["tenant_id", "run_id"],
        timestamp_column: "finished_at_ms",
        optional: true,
    },
];

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
          content_type TEXT NOT NULL,
          content_base64 TEXT NOT NULL,
          sha256 TEXT NOT NULL,
          byte_size INTEGER NOT NULL,
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
             FROM board_media_files WHERE tenant_id = ?1",
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

fn backup_source(store: &SqliteStore, created_at_ms: i64) -> Value {
    json!({
        "service": "onlineclass-local-sensitive-store",
        "serviceVersion": SERVICE_VERSION,
        "appVersion": env!("CARGO_PKG_VERSION"),
        "pcName": local_pc_name(),
        "os": local_os_name(),
        "arch": local_arch(),
        "createdAtMs": created_at_ms,
        "dbPath": store.db_path.to_string_lossy()
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

fn safe_relative_path(value: &str) -> Option<PathBuf> {
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
        "mediaMode": "separate_folder_mirror"
    }))
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
    for entry in fs::read_dir(&dir).map_err(|e| format!("backup_list_dir_failed:{e}"))? {
        let entry = entry.map_err(|e| format!("backup_list_entry_failed:{e}"))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("manifest-") || !name.ends_with(".json") {
            continue;
        }
        if let Ok(manifest) = read_manifest(&path) {
            backups.push(json!({
                "ok": manifest.get("ok").and_then(|value| value.as_bool()).unwrap_or(true),
                "tenantId": manifest.get("tenantId").and_then(|value| value.as_str()).unwrap_or(&tenant_id),
                "backupId": manifest.get("backupId").and_then(|value| value.as_str()).unwrap_or(""),
                "createdAtMs": manifest.get("createdAtMs").and_then(|value| value.as_i64()).unwrap_or(0),
                "manifestPath": path.to_string_lossy(),
                "dbPath": manifest.get("db").and_then(|db| db.get("absolutePath")).and_then(|value| value.as_str()).unwrap_or(""),
                "source": manifest.get("source").cloned().unwrap_or_else(|| json!({})),
                "counts": manifest.get("counts").cloned().unwrap_or_else(|| json!({})),
                "media": manifest.get("media").cloned().unwrap_or_else(|| json!({}))
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
    fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().any(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                name.starts_with("manifest-") && name.ends_with(".json")
            })
        })
        .unwrap_or(false)
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
        "media": manifest.get("media").cloned().unwrap_or_else(|| json!({}))
    }))
}

fn list_backup_manifests_in_dir(dir: &Path, fallback_tenant_id: &str, limit: usize) -> Result<Vec<Value>, String> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut backups = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| format!("backup_discover_dir_failed:{e}"))? {
        let entry = entry.map_err(|e| format!("backup_discover_entry_failed:{e}"))?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("manifest-") || !name.ends_with(".json") {
            continue;
        }
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

pub(crate) fn run_now(store: &SqliteStore, tenant_id: String) -> Result<Value, String> {
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
    let root = assert_backup_root_allowed(store, backup_root_dir(root_text))?;
    let out_dir = tenant_backup_dir(&root, &tenant_id);
    fs::create_dir_all(out_dir.join("db")).map_err(|e| format!("backup_dir_failed:{e}"))?;
    fs::create_dir_all(out_dir.join("board-media")).map_err(|e| format!("backup_media_dir_failed:{e}"))?;
    let created_at_ms = now_ms();
    let backup_id = format!("{}", Utc::now().format("%Y%m%d%H%M%S%3f"));
    let db_relative_path = PathBuf::from("db").join(format!("local-sensitive-{backup_id}.sqlite"));
    let db_path = out_dir.join(&db_relative_path);
    export_tenant_db(store, &tenant_id, &db_path)?;

    let media_rows = list_media_rows(store, &tenant_id)?;
    let mut media_records = Vec::new();
    let mut copied = 0i64;
    let mut skipped = 0i64;
    let mut missing = 0i64;
    let mut failed = 0i64;
    let mut bytes = 0i64;
    for row in media_rows {
        let ext = media_extension(&row);
        let backup_relative_path = PathBuf::from("board-media")
            .join(safe_segment(&row.board_id, "board"))
            .join(format!("{}.{}", safe_segment(&row.media_id, "media"), ext));
        let source_path = store.data_dir.join(&row.local_path);
        let target_path = out_dir.join(&backup_relative_path);
        let mut status = "copied";
        match fs::metadata(&source_path) {
            Ok(source_meta) => {
                if let Some(parent) = target_path.parent() {
                    fs::create_dir_all(parent).map_err(|e| format!("backup_media_target_dir_failed:{e}"))?;
                }
                let same = fs::metadata(&target_path)
                    .map(|target_meta| target_meta.len() == source_meta.len())
                    .unwrap_or(false);
                if same {
                    skipped += 1;
                    status = "skipped";
                } else if let Err(_) = fs::copy(&source_path, &target_path) {
                    failed += 1;
                    status = "failed";
                } else {
                    copied += 1;
                }
                bytes += source_meta.len() as i64;
            }
            Err(_) => {
                missing += 1;
                status = "missing";
            }
        }
        media_records.push(json!({
            "boardId": row.board_id,
            "postId": row.post_id,
            "mediaId": row.media_id,
            "localPath": row.local_path,
            "backupRelativePath": backup_relative_path.to_string_lossy().replace('\\', "/"),
            "contentType": row.content_type,
            "fileName": row.file_name,
            "size": row.size,
            "archivedAtMs": row.archived_at_ms,
            "status": status
        }));
    }
    let stats = store.stats(tenant_id.clone())?;
    let manifest = json!({
        "ok": failed == 0,
        "version": 1,
        "tenantId": tenant_id,
        "backupId": backup_id,
        "createdAtMs": created_at_ms,
        "source": backup_source(store, created_at_ms),
        "db": {
            "relativePath": db_relative_path.to_string_lossy().replace('\\', "/"),
            "absolutePath": db_path.to_string_lossy()
        },
        "counts": {
            "observationCount": stats.get("observationCount").and_then(|value| value.as_i64()).unwrap_or(0),
            "studentPrivateDetailCount": stats.get("studentPrivateDetailCount").and_then(|value| value.as_i64()).unwrap_or(0),
            "studentPrivatePhotoCount": stats.get("studentPrivatePhotoCount").and_then(|value| value.as_i64()).unwrap_or(0),
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
            "cloudSyncRunCount": stats.get("cloudSyncRunCount").and_then(|value| value.as_i64()).unwrap_or(0)
        },
        "media": {
            "mode": "separate_folder_mirror",
            "copied": copied,
            "skipped": skipped,
            "missing": missing,
            "failed": failed,
            "bytes": bytes,
            "records": media_records
        },
        "securityMode": "plain_warning"
    });
    let manifest_path = out_dir.join(format!("manifest-{backup_id}.json"));
    let manifest_raw = serde_json::to_string_pretty(&manifest).map_err(|e| format!("backup_manifest_encode_failed:{e}"))?;
    fs::write(&manifest_path, format!("{manifest_raw}\n")).map_err(|e| format!("backup_manifest_write_failed:{e}"))?;
    let result = json!({
        "ok": failed == 0,
        "tenantId": tenant_id,
        "backupId": backup_id,
        "manifestPath": manifest_path.to_string_lossy(),
        "dbPath": db_path.to_string_lossy(),
        "createdAtMs": created_at_ms,
        "source": manifest.get("source").cloned().unwrap_or_else(|| json!({})),
        "counts": manifest.get("counts").cloned().unwrap_or_else(|| json!({})),
        "media": manifest.get("media").cloned().unwrap_or_else(|| json!({}))
    });
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

pub(crate) fn restore_preview(store: &SqliteStore, body: Value) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(body.get("tenantId"));
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let manifest_path = resolve_manifest_path(store, &tenant_id, body.get("manifestPath"))?;
    let manifest = read_manifest(&manifest_path)?;
    let db_relative = manifest
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
    Ok(json!({
        "ok": true,
        "tenantId": tenant_id,
        "backupId": manifest.get("backupId").and_then(|value| value.as_str()).unwrap_or(""),
        "manifestPath": manifest_path.to_string_lossy(),
        "createdAtMs": manifest.get("createdAtMs").and_then(|value| value.as_i64()).unwrap_or(0),
        "source": manifest.get("source").cloned().unwrap_or_else(|| json!({})),
        "counts": counts,
        "media": manifest.get("media").cloned().unwrap_or_else(|| json!({}))
    }))
}

pub(crate) fn restore(store: &SqliteStore, body: Value) -> Result<Value, String> {
    let preview = restore_preview(store, body.clone())?;
    let tenant_id = preview.get("tenantId").and_then(|value| value.as_str()).unwrap_or("").to_string();
    let manifest_path = PathBuf::from(preview.get("manifestPath").and_then(|value| value.as_str()).unwrap_or(""));
    let manifest = read_manifest(&manifest_path)?;
    let db_relative = manifest
        .get("db")
        .and_then(|db| db.get("relativePath"))
        .and_then(|value| value.as_str())
        .ok_or_else(|| "backup_db_required".to_string())?;
    let db_path = manifest_path.parent().unwrap_or_else(|| Path::new(".")).join(db_relative);
    let mut media_restored = 0i64;
    let mut media_missing = 0i64;
    let mut media_paths = Vec::new();
    if let Some(records) = manifest.get("media").and_then(|media| media.get("records")).and_then(|value| value.as_array()) {
        for record in records {
            let media_id = normalize_json_text(record.get("mediaId"), 220).replace(['/', '\\'], "_");
            let backup_relative = normalize_json_text(record.get("backupRelativePath"), 600);
            let local_path_text = normalize_json_text(record.get("localPath"), 600);
            if media_id.is_empty() {
                continue;
            }
            let Some(backup_relative_path) = safe_relative_path(&backup_relative) else {
                continue;
            };
            let Some(local_path) = safe_relative_path(&local_path_text) else {
                continue;
            };
            let source_path = manifest_path.parent().unwrap_or_else(|| Path::new(".")).join(backup_relative_path);
            let target_path = store.data_dir.join(&local_path);
            if !source_path.exists() {
                media_missing += 1;
                continue;
            }
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("restore_media_dir_failed:{e}"))?;
            }
            fs::copy(&source_path, &target_path).map_err(|e| format!("restore_media_copy_failed:{e}"))?;
            media_restored += 1;
            media_paths.push((media_id, local_path.to_string_lossy().to_string()));
        }
    }
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    conn.execute("ATTACH DATABASE ?1 AS restore", params![db_path.to_string_lossy().to_string()])
        .map_err(|e| format!("restore_db_attach_failed:{e}"))?;
    let result = (|| -> Result<i64, String> {
        let mut imported = 0i64;
        for table in BACKUP_TABLES {
            if !table_exists(&conn, table.name)? {
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
            conn.execute(&sql, params![tenant_id])
                .map_err(|e| format!("restore_table_merge_failed:{}:{e}", table.name))?;
            imported += conn.changes() as i64;
        }
        for (media_id, local_path) in media_paths {
            conn.execute(
                "UPDATE board_media_files SET local_path = ?1 WHERE tenant_id = ?2 AND media_id = ?3",
                params![local_path, tenant_id, media_id],
            )
            .map_err(|e| format!("restore_media_path_update_failed:{e}"))?;
        }
        Ok(imported)
    })();
    let _ = conn.execute_batch("DETACH DATABASE restore");
    let imported = result?;
    Ok(json!({
        "ok": true,
        "tenantId": tenant_id,
        "backupId": preview.get("backupId").cloned().unwrap_or(Value::Null),
        "manifestPath": manifest_path.to_string_lossy(),
        "imported": imported,
        "mediaRestored": media_restored,
        "mediaMissing": media_missing
    }))
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
                let _ = run_now(&store, tenant_id);
            }
        }
        thread::sleep(Duration::from_secs(15 * 60));
    });
}
