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
    let sources = {
        let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
        crate::teaching_source_backup::file_rows(&conn, "main", tenant_id)?
    };
    for row in sources {
        let path =
            crate::teaching_source_backup::source_path(store, &row.owner_uid, &row.local_path)?;
        let (_, hash) = sha256_file(&path)?;
        references.insert(format!("objects/sha256/{}/{}", &hash[..2], hash));
    }
    Ok((sequence, references))
}

pub(crate) fn maintenance_status(store: &SqliteStore, tenant: &str) -> Result<Value, String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    let raw: Option<String> = conn
        .query_row(
            "SELECT state_json FROM local_store_backup_maintenance WHERE tenant_id=?1",
            params![tenant],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| "backup_maintenance_read_failed")?;
    serde_json::from_str(raw.as_deref().unwrap_or("{}"))
        .map_err(|_| "backup_maintenance_state_invalid".into())
}

fn save_status(store: &SqliteStore, tenant: &str, state: &Value) -> Result<(), String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    conn.execute("INSERT INTO local_store_backup_maintenance (tenant_id,state_json) VALUES (?1,?2) ON CONFLICT(tenant_id) DO UPDATE SET state_json=excluded.state_json",params![tenant,state.to_string()])
        .map_err(|_| "backup_maintenance_write_failed")?;
    Ok(())
}

pub(super) fn after_backup(
    store: &SqliteStore,
    tenant: &str,
    now: i64,
    snapshot: &Value,
) -> Result<Value, String> {
    let root = configured_tenant_dir(store, tenant)?;
    let mut state = maintenance_status(store, tenant)?;
    // Metadata only. Queue one batch; never hash every prior DB after each backup.
    let scan =
        crate::backup_v5::retention_check(&root, now, &pinned_sync_generations(store, tenant)?)?;
    state["backupCreation"] =
        json!({"ok":true,"lastSuccessAtMs":now,"version":snapshot["snapshotVersion"]});
    state["retentionCheck"] = scan.clone();
    if scan["overLimit"] == true && state["failureCount"].as_i64().unwrap_or(0) == 0 {
        state["retentionRequestedAtMs"] = json!(now);
        let next = state["nextRetryAtMs"].as_i64().unwrap_or(0);
        let last = state["stages"]["snapshots"]["lastSuccessAtMs"]
            .as_i64()
            .unwrap_or(0);
        let batch_at = (now + 60_000).max(last + 15 * 60_000);
        state["nextRetryAtMs"] = json!(if next > 0 {
            next.min(batch_at)
        } else {
            batch_at
        });
    }
    save_status(store, tenant, &state)?;
    Ok(json!({"ok":true,"queued":true,"retentionCheck":scan,"nextRunAtMs":state["nextRetryAtMs"]}))
}

fn stage(
    store: &SqliteStore,
    tenant: &str,
    state: &mut Value,
    name: &str,
    now: i64,
    result: Result<Value, String>,
) -> Result<bool, String> {
    let previous_success = state["stages"][name]["lastSuccessAtMs"].clone();
    let mut value = result.unwrap_or_else(|error| json!({"ok":false,"error":error}));
    let ok = value["ok"] == true;
    value["lastAttemptAtMs"] = json!(now);
    value["lastSuccessAtMs"] = if ok { json!(now) } else { previous_success };
    state["stages"][name] = value;
    save_status(store, tenant, state)?;
    Ok(ok)
}

fn run_stage<F: FnOnce() -> Result<Value, String>>(
    store: &SqliteStore,
    tenant: &str,
    state: &mut Value,
    name: &str,
    now: i64,
    force: bool,
    run: F,
) -> Result<bool, String> {
    let last = state["stages"][name]["lastSuccessAtMs"]
        .as_i64()
        .unwrap_or(0);
    let new_retention = matches!(name, "snapshots" | "manualRetention" | "legacyQuarantine")
        && state["retentionRequestedAtMs"].as_i64().unwrap_or(0) > last;
    if !force
        && !new_retention
        && state["stages"][name]["ok"] == true
        && last > 0
        && now >= last
        && now - last < BACKUP_INTERVAL_MS
    {
        return Ok(true);
    }
    stage(store, tenant, state, name, now, run())
}

fn collect_objects(
    store: &SqliteStore,
    tenant: &str,
    root: &Path,
    now: i64,
) -> Result<Value, String> {
    let (sequence, references) = live_references(store, tenant)?;
    let mut conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| "backup_gc_transaction_failed")?;
    let current: i64 = tx
        .query_row(
            "SELECT change_sequence FROM local_store_device_sync_state WHERE tenant_id=?1",
            params![tenant],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| "backup_live_sequence_failed")?
        .unwrap_or(0);
    if sequence != current {
        return Err("backup_live_references_changed".into());
    }
    let result = crate::backup_v5::quarantine_unreferenced_objects(root, &references, now)?;
    tx.commit()
        .map_err(|_| "backup_gc_transaction_commit_failed")?;
    Ok(result)
}

pub(super) fn run_if_due(
    store: &SqliteStore,
    tenant: &str,
    now: i64,
    force: bool,
) -> Result<Value, String> {
    let state = maintenance_status(store, tenant)?;
    if !force && now < state["nextRetryAtMs"].as_i64().unwrap_or(0) {
        return Ok(json!({"ok":state["ok"],"skipped":true,"nextRunAtMs":state["nextRetryAtMs"]}));
    }
    let root = configured_tenant_dir(store, tenant)?;
    let _operation = root_operation(store, &root)?;
    // Recheck after acquiring the cross-process root lock.
    let mut state = maintenance_status(store, tenant)?;
    if !force && now < state["nextRetryAtMs"].as_i64().unwrap_or(0) {
        return Ok(json!({"ok":state["ok"],"skipped":true}));
    }
    state["lastAttemptAtMs"] = json!(now);
    state["nextRetryAtMs"] = json!(now + 300_000);
    state["running"] = json!(true);
    save_status(store, tenant, &state)?;
    let access = store.media_access(tenant);
    let common = access
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|_| store.restore_ready(tenant))
        .and_then(|_| require_pin_context(store, tenant, now))
        .and_then(|_| pinned_sync_generations(store, tenant));
    let mut all_ok = true;
    match common {
        Err(error) => {
            state["deferredReason"] = json!(error);
            for name in [
                "safety",
                "cache",
                "manualRetention",
                "snapshots",
                "objects",
                "legacyQuarantine",
                "legacyPurge",
            ] {
                all_ok &= stage(store, tenant, &mut state, name, now, Err(error.clone()))?;
            }
        }
        Ok(pins) => {
            state["deferredReason"] = Value::Null;
            stage(
                store,
                tenant,
                &mut state,
                "safety",
                now,
                Ok(json!({"ok":true})),
            )?;
            all_ok &= run_stage(store, tenant, &mut state, "cache", now, force, || {
                super::artifact_recovery::maintain_cache(store, tenant, now, &pins)
                    .map(|_| json!({"ok":true}))
            })?;
            all_ok &= run_stage(
                store,
                tenant,
                &mut state,
                "manualRetention",
                now,
                force,
                || crate::backup_v5::prune_manual_snapshots(&root),
            )?;
            all_ok &= run_stage(store, tenant, &mut state, "snapshots", now, force, || {
                crate::backup_v5::prune_snapshots(&root, now, &pins)
            })?;
            // A failed object reference scan does not suppress safe snapshot/quarantine stages.
            all_ok &= run_stage(store, tenant, &mut state, "objects", now, force, || {
                collect_objects(store, tenant, &root, now)
            })?;
            all_ok &= run_stage(
                store,
                tenant,
                &mut state,
                "legacyQuarantine",
                now,
                force,
                || crate::backup_v5::quarantine_legacy_snapshots(&root, &pins, 0, now),
            )?;
            all_ok &= run_stage(store, tenant, &mut state, "legacyPurge", now, force, || {
                latest_verified_snapshot_created_at(store, tenant, &root)
                    .and_then(|at| crate::backup_v5::purge_legacy_quarantine(&root, &pins, at, now))
            })?;
        }
    }
    let failures = if all_ok {
        0
    } else {
        state["failureCount"]
            .as_i64()
            .unwrap_or(0)
            .saturating_add(1)
            .min(16)
    };
    let delay = if all_ok {
        BACKUP_INTERVAL_MS
    } else {
        (60_000_i64 * (1_i64 << (failures - 1).min(6))).min(3_600_000)
    };
    state["ok"] = json!(all_ok);
    state["running"] = json!(false);
    state["failureCount"] = json!(failures);
    state["nextRetryAtMs"] = json!(now + delay);
    if all_ok {
        state["lastSuccessAtMs"] = json!(now);
    }
    save_status(store, tenant, &state)?;
    Ok(state)
}
