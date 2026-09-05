use super::*;

pub(crate) fn set_folder(
    store: &SqliteStore,
    tenant_id: String,
    folder_path: String,
) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let root = assert_backup_root_allowed(store, backup_root_dir(folder_path))?;
    fs::create_dir_all(tenant_backup_dir(&root, &tenant_id))
        .map_err(|e| format!("backup_tenant_dir_failed:{e}"))?;
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

pub(crate) fn connection_status(store: &SqliteStore, tenant_id: &str) -> Result<Value, String> {
    let config = read_config(store);
    let root = config
        .get("tenants")
        .and_then(|tenants| tenants.get(tenant_id))
        .and_then(|tenant| tenant.get("backupRootDir"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if root.is_empty() {
        return Ok(json!({"configured":false}));
    }
    let root = assert_backup_root_allowed(store, backup_root_dir(root))?;
    let dir = tenant_backup_dir(&root, tenant_id);
    fs::read_dir(&dir).map_err(|e| format!("backup_list_dir_failed:{e}"))?;
    Ok(json!({"configured":true,"backupRootDir":root.to_string_lossy()}))
}

pub(crate) fn status(store: &SqliteStore, tenant_id: String) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let config = read_config(store);
    let tenant = config
        .get("tenants")
        .and_then(|tenants| tenants.get(&tenant_id))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let root_text = tenant
        .get("backupRootDir")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let configured = !root_text.trim().is_empty();
    let interval_ms = tenant
        .get("intervalMs")
        .and_then(|value| value.as_i64())
        .unwrap_or(BACKUP_INTERVAL_MS);
    let last_run_at_ms = tenant
        .get("lastRunAtMs")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    let backups = if configured {
        list_backups(store, tenant_id.clone(), 5)?
            .get("backups")
            .cloned()
            .unwrap_or_else(|| json!([]))
    } else {
        json!([])
    };
    let latest_backup = backups
        .as_array()
        .and_then(|items| items.first())
        .cloned()
        .unwrap_or(Value::Null);
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

pub(crate) fn configured_tenant_dir(
    store: &SqliteStore,
    tenant_id: &str,
) -> Result<PathBuf, String> {
    let config = read_config(store);
    let root_text = config
        .get("tenants")
        .and_then(|tenants| tenants.get(tenant_id))
        .and_then(|tenant| tenant.get("backupRootDir"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if root_text.trim().is_empty() {
        return Err("backup_not_configured".to_string());
    }
    Ok(tenant_backup_dir(
        &assert_backup_root_allowed(store, backup_root_dir(root_text))?,
        tenant_id,
    ))
}

pub(super) fn pinned_sync_generations(
    store: &SqliteStore,
    tenant_id: &str,
) -> Result<HashSet<i64>, String> {
    let state = local_sync_state(store, tenant_id)?;
    let (mut pins, _) = server_pins(store, tenant_id)?;
    pins.extend(
        [
            state.applied_generation,
            state.published_generation,
            state.latest_generation,
        ]
        .into_iter()
        .filter(|generation| *generation > 0),
    );
    if let Some(pending) = pending_publication(store, tenant_id)? {
        for value in [
            pending.pointer("/snapshot/generation"),
            pending.get("baseGeneration"),
        ] {
            if let Some(generation) = value.and_then(Value::as_i64).filter(|g| *g > 0) {
                pins.insert(generation);
            }
        }
    }
    Ok(pins)
}

pub(crate) fn storage_overview(store: &SqliteStore, tenant_id: String) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let tenant_dir = configured_tenant_dir(store, &tenant_id)?;
    let _operation = root_operation(store, &tenant_dir)?;
    let scan = crate::backup_v5::scan_storage(&tenant_dir);
    let cleanup = crate::backup_v5::legacy_cleanup_summary_from_scan(
        &tenant_dir,
        &pinned_sync_generations(store, &tenant_id)?,
        &scan,
    );
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
    let current_original_bytes = current_files
        .values()
        .map(|item| item.get("bytes").and_then(Value::as_i64).unwrap_or(0))
        .sum::<i64>();
    let mut largest_files = current_files
        .into_values()
        .filter(|item| item.get("bytes").and_then(Value::as_i64).unwrap_or(0) >= 100 * 1024 * 1024)
        .collect::<Vec<_>>();
    largest_files.sort_by(|left, right| {
        right
            .get("bytes")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .cmp(&left.get("bytes").and_then(Value::as_i64).unwrap_or(0))
    });
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
        "storageBreakdown": scan.storage_breakdown,
        "totalLogicalBytes": scan.total_logical_bytes,
        "scanComplete": scan.scan_complete,
        "scannedAtMs": scan.scanned_at_ms,
        "scanErrors": scan.errors,
        "legacySnapshotCount": scan.legacy_snapshot_count,
        "legacySnapshotBytes": scan.legacy_snapshot_bytes,
        "legacyReclaimableBytes": cleanup.get("reclaimableBytes").cloned().unwrap_or(json!(0)),
        "legacyCleanupCandidateCount": cleanup.get("candidateCount").cloned().unwrap_or(json!(0)),
        "legacyQuarantineCount": quarantine.get("quarantinedCount").cloned().unwrap_or(json!(0)),
        "legacyQuarantineBytes": scan.storage_breakdown.get("legacyQuarantineBytes").cloned().unwrap_or(json!(0)),
        "legacyQuarantinePurgeAfterMs": quarantine.get("purgeAfterMs").cloned().unwrap_or(json!(0)),
        "legacyQuarantineReviewCount": quarantine.get("reviewCount").cloned().unwrap_or(json!(0)),
        "legacyQuarantineError": quarantine.get("error").cloned().unwrap_or(Value::Null),
        "largeFileThresholdBytes": 100 * 1024 * 1024,
        "largestFiles": largest_files,
        "retention": { "recent": 10, "dailyDays": 30, "monthlyMonths": 12, "preRestore": 5, "manual": "explicit_delete_only" }
    }))
}

pub(crate) fn preview_legacy_cleanup(
    store: &SqliteStore,
    tenant_id: String,
) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let tenant_dir = configured_tenant_dir(store, &tenant_id)?;
    let _operation = root_operation(store, &tenant_dir)?;
    maintenance::require_pin_context(store, &tenant_id, now_ms())?;
    Ok(crate::backup_v5::legacy_cleanup_preview(
        &tenant_dir,
        &pinned_sync_generations(store, &tenant_id)?,
    ))
}

pub(crate) fn apply_legacy_cleanup(
    store: &SqliteStore,
    tenant_id: String,
    preview_token: String,
) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    if preview_token.len() != 64 {
        return Err("backup_legacy_cleanup_preview_required".to_string());
    }
    let tenant_dir = configured_tenant_dir(store, &tenant_id)?;
    let _operation = root_operation(store, &tenant_dir)?;
    maintenance::require_pin_context(store, &tenant_id, now_ms())?;
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
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let tenant_dir = configured_tenant_dir(store, &tenant_id)?;
    let _operation = root_operation(store, &tenant_dir)?;
    crate::backup_v5::undo_legacy_quarantine(&tenant_dir, now_ms())
}

pub(super) fn latest_verified_v5_created_at(
    _store: &SqliteStore,
    tenant_id: &str,
    tenant_dir: &Path,
) -> Result<i64, String> {
    let mut candidates = manifest_paths_in_dir(tenant_dir)?
        .into_iter()
        .filter_map(|path| {
            let manifest = read_manifest(&path).ok()?;
            if manifest.get("version").and_then(Value::as_i64)
                != Some(crate::backup_v5::SNAPSHOT_VERSION)
                || manifest.get("ok").and_then(Value::as_bool) != Some(true)
                || manifest.get("tenantId").and_then(Value::as_str) != Some(tenant_id)
            {
                return None;
            }
            let created_at_ms = manifest
                .get("createdAtMs")
                .and_then(Value::as_i64)
                .unwrap_or(0);
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

pub(super) fn manifest_paths_in_dir(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
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
        for entry in
            fs::read_dir(&snapshots).map_err(|e| format!("backup_snapshot_list_failed:{e}"))?
        {
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

pub(crate) fn list_backups(
    store: &SqliteStore,
    tenant_id: String,
    limit: i64,
) -> Result<Value, String> {
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
        if let Ok(manifest) = listed_manifest(&path) {
            let db_path = manifest
                .get("db")
                .and_then(|db| db.get("absolutePath"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    manifest
                        .get("db")
                        .and_then(|db| db.get("relativePath"))
                        .and_then(Value::as_str)
                        .map(|relative| {
                            path.parent()
                                .unwrap_or_else(|| Path::new("."))
                                .join(relative)
                                .to_string_lossy()
                                .to_string()
                        })
                })
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
        let av = a
            .get("createdAtMs")
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        let bv = b
            .get("createdAtMs")
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        bv.cmp(&av)
    });
    backups.truncate(max);
    Ok(json!({ "ok": true, "backups": backups }))
}

pub(super) fn file_name_is(path: &Path, expected: &str) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case(expected))
        .unwrap_or(false)
}

pub(super) fn has_backup_manifest(dir: &Path) -> bool {
    manifest_paths_in_dir(dir)
        .map(|paths| !paths.is_empty())
        .unwrap_or(false)
}

pub(super) fn selected_backup_root_and_tenant(selected: &Path) -> (PathBuf, String) {
    if file_name_is(selected, BACKUP_NAMESPACE_DIR) {
        return (
            selected
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .to_path_buf(),
            String::new(),
        );
    }
    if file_name_is(selected, "tenants") {
        if let Some(parent) = selected.parent() {
            if file_name_is(parent, BACKUP_NAMESPACE_DIR) {
                return (
                    parent
                        .parent()
                        .unwrap_or_else(|| Path::new(""))
                        .to_path_buf(),
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
                            namespace
                                .parent()
                                .unwrap_or_else(|| Path::new(""))
                                .to_path_buf(),
                            tenant_id,
                        );
                    }
                }
            }
        }
    }
    (selected.to_path_buf(), String::new())
}

pub(super) fn backup_manifest_summary(path: &Path, fallback_tenant_id: &str) -> Option<Value> {
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

pub(super) fn list_backup_manifests_in_dir(
    dir: &Path,
    fallback_tenant_id: &str,
    limit: usize,
) -> Result<Vec<Value>, String> {
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
        let av = a
            .get("createdAtMs")
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        let bv = b
            .get("createdAtMs")
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
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
        for entry in fs::read_dir(&tenants_dir)
            .map_err(|e| format!("backup_discover_tenants_dir_failed:{e}"))?
        {
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
