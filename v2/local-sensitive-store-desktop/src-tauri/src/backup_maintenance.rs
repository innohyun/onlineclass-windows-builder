use super::*;
use rusqlite::OptionalExtension;

const PIN_CONTEXT_MAX_AGE_MS: i64 = 6 * 60 * 60 * 1000;

pub(super) fn require_pin_context(
    store: &SqliteStore,
    tenant_id: &str,
    now: i64,
) -> Result<(), String> {
    let (_, checked) = server_pins(store, tenant_id)?;
    let state = local_sync_state(store, tenant_id)?;
    let session_path = store.data_dir.join("device-sync-session.json");
    let connected = match fs::read(&session_path) {
        Ok(raw) => {
            serde_json::from_slice::<Value>(&raw)
                .map_err(|_| "backup_sync_pin_context_invalid")?
                .get("tenantId")
                .and_then(Value::as_str)
                == Some(tenant_id)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return Err("backup_sync_pin_context_unavailable".into()),
    };
    let shared = connected
        || state.applied_generation > 0
        || state.published_generation > 0
        || state.latest_generation > 0
        || pending_publication(store, tenant_id)?.is_some()
        || highest_local_generation(store, tenant_id)? > 0;
    if shared && (checked <= 0 || now < checked || now - checked > PIN_CONTEXT_MAX_AGE_MS) {
        return Err("backup_sync_pin_context_stale".into());
    }
    Ok(())
}

// Hash original files outside the live DB lock, then check the monotonic
// sequence before making any object moves/deletions. Never use the last
// snapshot's references as the current live-data reference set.
fn live_references(store: &SqliteStore, tenant_id: &str) -> Result<(i64, HashSet<String>), String> {
    let (sequence, paths) = {
        let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
        let sequence: i64 = conn
            .query_row(
                "SELECT change_sequence FROM local_store_device_sync_state WHERE tenant_id=?1",
                params![tenant_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("backup_live_sequence_failed:{e}"))?
            .unwrap_or(0);
        let mut paths = media_rows_from(&conn, tenant_id)?
            .into_iter()
            .map(|row| row.local_path)
            .collect::<HashSet<_>>();
        paths.extend(
            attachment_rows_from(&conn, tenant_id)?
                .into_iter()
                .map(|row| row.local_path),
        );
        (sequence, paths)
    };
    let mut references = HashSet::new();
    for path in paths {
        let relative = safe_relative_path(&path).ok_or("backup_live_reference_invalid")?;
        let (_, hash) = sha256_file(&store.data_dir.join(relative))?;
        references.insert(format!("objects/sha256/{}/{}", &hash[..2], hash));
    }
    Ok((sequence, references))
}

pub(super) fn run_if_due(
    store: &SqliteStore,
    tenant_id: &str,
    now: i64,
    force: bool,
) -> Result<Value, String> {
    let tenant_dir = configured_tenant_dir(store, tenant_id)?;
    let _operation = root_operation(store, &tenant_dir)?;
    {
        let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
        let last: i64 = conn
            .query_row(
                "SELECT maintenance_at_ms FROM local_store_device_sync_runtime WHERE tenant_id=?1",
                params![tenant_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("backup_maintenance_read_failed:{e}"))?
            .unwrap_or(0);
        if !force && last > 0 && now.saturating_sub(last) < BACKUP_INTERVAL_MS {
            return Ok(json!({"ok":true,"skipped":true,"nextRunAtMs":last + BACKUP_INTERVAL_MS}));
        }
        // Persist attempts as well as successes: unavailable OneDrive must not
        // turn the 15-second device poll into a recursive cleanup scan.
        conn.execute("INSERT INTO local_store_device_sync_runtime (tenant_id,maintenance_at_ms) VALUES (?1,?2)
            ON CONFLICT(tenant_id) DO UPDATE SET maintenance_at_ms=excluded.maintenance_at_ms", params![tenant_id,now])
            .map_err(|e| format!("backup_maintenance_write_failed:{e}"))?;
    }
    require_pin_context(store, tenant_id, now)?;
    let pins = pinned_sync_generations(store, tenant_id)?;
    let verified_at = latest_verified_v5_created_at(store, tenant_id, &tenant_dir)?;
    crate::backup_v5::prune_snapshots(&tenant_dir, now, &pins)?;
    let (sequence, references) = live_references(store, tenant_id)?;
    let objects = {
        // File hashing is finished. Serialize the final reference check and
        // object GC with writers, so a newly reattached object cannot be purged.
        let mut conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
        let transaction = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| format!("backup_gc_transaction_failed:{e}"))?;
        let current: i64 = transaction
            .query_row(
                "SELECT change_sequence FROM local_store_device_sync_state WHERE tenant_id=?1",
                params![tenant_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("backup_live_sequence_failed:{e}"))?
            .unwrap_or(0);
        if sequence != current {
            return Err("backup_live_references_changed".into());
        }
        let result =
            crate::backup_v5::quarantine_unreferenced_objects(&tenant_dir, &references, now)?;
        transaction
            .commit()
            .map_err(|e| format!("backup_gc_transaction_commit_failed:{e}"))?;
        result
    };
    let legacy =
        crate::backup_v5::maintain_legacy_quarantine(&tenant_dir, &pins, verified_at, now)?;
    Ok(json!({"ok":true,"objects":objects,"legacy":legacy,"nextRunAtMs":now + BACKUP_INTERVAL_MS}))
}
