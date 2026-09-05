use super::*;
use rusqlite::OptionalExtension;
use std::cell::RefCell;
use std::sync::{Mutex, OnceLock};

pub(super) fn install_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS local_store_device_sync_runtime (
        tenant_id TEXT PRIMARY KEY,
        pending_json TEXT NOT NULL DEFAULT 'null',
        pins_json TEXT NOT NULL DEFAULT '[]',
        pins_checked_at_ms INTEGER NOT NULL DEFAULT 0,
        retry_at_ms INTEGER NOT NULL DEFAULT 0,
        retry_count INTEGER NOT NULL DEFAULT 0,
        maintenance_at_ms INTEGER NOT NULL DEFAULT 0,
        acked_generation INTEGER NOT NULL DEFAULT 0,
        acked_root TEXT NOT NULL DEFAULT ''
    );",
    )
    .map_err(|e| format!("db_sync_runtime_schema_failed:{e}"))
}

pub(crate) fn pending_publication(
    store: &SqliteStore,
    tenant_id: &str,
) -> Result<Option<Value>, String> {
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let raw: Option<String> = conn
        .query_row(
            "SELECT pending_json FROM local_store_device_sync_runtime WHERE tenant_id=?1",
            params![tenant_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("db_sync_pending_read_failed:{e}"))?;
    let value: Value = serde_json::from_str(raw.as_deref().unwrap_or("null"))
        .map_err(|e| format!("db_sync_pending_invalid:{e}"))?;
    Ok((!value.is_null()).then_some(value))
}

pub(crate) fn save_pending_publication(
    store: &SqliteStore,
    tenant_id: &str,
    value: Option<&Value>,
) -> Result<(), String> {
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    conn.execute(
        "INSERT INTO local_store_device_sync_runtime (tenant_id,pending_json) VALUES (?1,?2)
        ON CONFLICT(tenant_id) DO UPDATE SET pending_json=excluded.pending_json",
        params![tenant_id, value.unwrap_or(&Value::Null).to_string()],
    )
    .map_err(|e| format!("db_sync_pending_write_failed:{e}"))?;
    Ok(())
}

pub(crate) fn remember_checkpoint_pins(
    store: &SqliteStore,
    tenant_id: &str,
    response: &Value,
) -> Result<(), String> {
    if response.get("latestVerifiedCheckpoint").is_none() {
        return Err("device_sync_verified_context_missing".into());
    }
    let mut pins = HashSet::new();
    for checkpoint in [
        response.get("checkpoint"),
        response.get("latestVerifiedCheckpoint"),
    ]
    .into_iter()
    .flatten()
    {
        for key in ["generation", "recoveryOfGeneration"] {
            if let Some(value) = checkpoint
                .get(key)
                .and_then(Value::as_i64)
                .filter(|v| *v > 0)
            {
                pins.insert(value);
            }
        }
    }
    let mut pins = pins.into_iter().collect::<Vec<_>>();
    pins.sort_unstable();
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    conn.execute("INSERT INTO local_store_device_sync_runtime (tenant_id,pins_json,pins_checked_at_ms) VALUES (?1,?2,?3)
        ON CONFLICT(tenant_id) DO UPDATE SET pins_json=excluded.pins_json,pins_checked_at_ms=excluded.pins_checked_at_ms",
        params![tenant_id, json!(pins).to_string(), now_ms()]).map_err(|e| format!("db_sync_pins_write_failed:{e}"))?;
    Ok(())
}

pub(super) fn server_pins(
    store: &SqliteStore,
    tenant_id: &str,
) -> Result<(HashSet<i64>, i64), String> {
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let (raw, checked): (String, i64) = conn.query_row("SELECT pins_json,pins_checked_at_ms FROM local_store_device_sync_runtime WHERE tenant_id=?1", params![tenant_id], |row| Ok((row.get(0)?,row.get(1)?)))
        .optional().map_err(|e| format!("db_sync_pins_read_failed:{e}"))?.unwrap_or(("[]".into(),0));
    let pins: HashSet<i64> =
        serde_json::from_str(&raw).map_err(|e| format!("db_sync_pins_invalid:{e}"))?;
    Ok((pins, checked))
}

pub(crate) fn retry_due(store: &SqliteStore, tenant_id: &str, now: i64) -> Result<bool, String> {
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let retry: i64 = conn
        .query_row(
            "SELECT retry_at_ms FROM local_store_device_sync_runtime WHERE tenant_id=?1",
            params![tenant_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("db_sync_retry_read_failed:{e}"))?
        .unwrap_or(0);
    Ok(now >= retry)
}

pub(crate) fn retry_pending(store: &SqliteStore, tenant_id: &str) -> Result<bool, String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    conn.query_row(
        "SELECT retry_count > 0 FROM local_store_device_sync_runtime WHERE tenant_id=?1",
        params![tenant_id],
        |row| row.get(0),
    )
    .optional()
    .map(|value| value.unwrap_or(false))
    .map_err(|e| format!("db_sync_retry_read_failed:{e}"))
}

pub(crate) fn update_retry(
    store: &SqliteStore,
    tenant_id: &str,
    failed: bool,
    now: i64,
) -> Result<(), String> {
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    conn.execute(
        "INSERT OR IGNORE INTO local_store_device_sync_runtime (tenant_id) VALUES (?1)",
        params![tenant_id],
    )
    .map_err(|e| format!("db_sync_retry_write_failed:{e}"))?;
    conn.execute(if failed {
        "UPDATE local_store_device_sync_runtime SET retry_at_ms=?2 + CASE retry_count WHEN 0 THEN 30000 WHEN 1 THEN 60000 WHEN 2 THEN 120000 ELSE 300000 END,retry_count=MIN(retry_count+1,4) WHERE tenant_id=?1"
    } else {
        "UPDATE local_store_device_sync_runtime SET retry_at_ms=0,retry_count=0 WHERE tenant_id=?1 AND ?2>=0"
    }, params![tenant_id,now]).map_err(|e| format!("db_sync_retry_write_failed:{e}"))?;
    Ok(())
}

pub(crate) fn acknowledged_locally(
    store: &SqliteStore,
    tenant_id: &str,
    generation: i64,
    device_id: &str,
    root: &str,
) -> Result<bool, String> {
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    conn.query_row("SELECT acked_generation=?2 AND acked_root=?3 FROM local_store_device_sync_runtime WHERE tenant_id=?1", params![tenant_id,generation,format!("{device_id}:{root}")], |row| row.get(0))
        .optional().map(|v|v.unwrap_or(false)).map_err(|e| format!("db_sync_ack_read_failed:{e}"))
}

pub(crate) fn remember_ack(
    store: &SqliteStore,
    tenant_id: &str,
    generation: i64,
    device_id: &str,
    root: &str,
) -> Result<(), String> {
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    conn.execute("INSERT INTO local_store_device_sync_runtime (tenant_id,acked_generation,acked_root) VALUES (?1,?2,?3)
        ON CONFLICT(tenant_id) DO UPDATE SET acked_generation=excluded.acked_generation,acked_root=excluded.acked_root", params![tenant_id,generation,format!("{device_id}:{root}")])
        .map_err(|e| format!("db_sync_ack_write_failed:{e}"))?;
    Ok(())
}

thread_local! { static ROOT_DEPTH: RefCell<HashMap<PathBuf, usize>> = RefCell::new(HashMap::new()); }
pub(super) struct RootOperation {
    key: PathBuf,
    file: Option<File>,
}
impl Drop for RootOperation {
    fn drop(&mut self) {
        ROOT_DEPTH.with(|depths| {
            let mut depths = depths.borrow_mut();
            if let Some(depth) = depths.get_mut(&self.key) {
                *depth -= 1;
                if *depth == 0 {
                    depths.remove(&self.key);
                }
            }
        });
        if let Some(file) = &self.file {
            let _ = fs2::FileExt::unlock(file);
        }
    }
}

pub(super) fn root_operation(
    store: &SqliteStore,
    tenant_dir: &Path,
) -> Result<RootOperation, String> {
    use sha2::{Digest, Sha256};
    let key = tenant_dir
        .canonicalize()
        .unwrap_or_else(|_| tenant_dir.to_path_buf());
    if ROOT_DEPTH.with(|depths| {
        let mut depths = depths.borrow_mut();
        if let Some(depth) = depths.get_mut(&key) {
            *depth += 1;
            true
        } else {
            false
        }
    }) {
        return Ok(RootOperation { key, file: None });
    }
    let locks = store.data_dir.join("backup-operation-locks");
    fs::create_dir_all(&locks).map_err(|e| format!("backup_lock_dir_failed:{e}"))?;
    let name = format!(
        "{:x}.lock",
        Sha256::digest(key.to_string_lossy().as_bytes())
    );
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(locks.join(name))
        .map_err(|e| format!("backup_lock_open_failed:{e}"))?;
    fs2::FileExt::lock_exclusive(&file).map_err(|e| format!("backup_lock_failed:{e}"))?;
    ROOT_DEPTH.with(|depths| {
        depths.borrow_mut().insert(key.clone(), 1);
    });
    Ok(RootOperation {
        key,
        file: Some(file),
    })
}

#[derive(Clone)]
struct CachedManifest {
    size: u64,
    modified: std::time::SystemTime,
    value: Value,
}
static MANIFEST_CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedManifest>>> = OnceLock::new();

// Only for discovery/listing. Verification must always read the actual bytes.
pub(super) fn listed_manifest(path: &Path) -> Result<Value, String> {
    let metadata =
        fs::metadata(path).map_err(|e| format!("backup_manifest_metadata_failed:{e}"))?;
    let modified = metadata
        .modified()
        .map_err(|e| format!("backup_manifest_metadata_failed:{e}"))?;
    let cache = MANIFEST_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache
        .lock()
        .map_err(|_| "backup_manifest_cache_failed")?
        .get(path)
        .filter(|hit| hit.size == metadata.len() && hit.modified == modified)
    {
        return Ok(hit.value.clone());
    }
    let value = read_manifest(path)?;
    let mut cache = cache.lock().map_err(|_| "backup_manifest_cache_failed")?;
    if cache.len() >= 2048 {
        cache.clear();
    }
    cache.insert(
        path.to_path_buf(),
        CachedManifest {
            size: metadata.len(),
            modified,
            value: value.clone(),
        },
    );
    Ok(value)
}
