use super::journal::*;
use super::*;

fn legacy_cleanup_candidates(
    tenant_dir: &Path, pins: &HashSet<i64>, now: i64, verify: bool,
) -> Result<Vec<(PathBuf, i64, i64)>, String> {
    Ok(retention::retention_plan(tenant_dir, now, pins, verify)?.into_iter()
        .filter(|entry| !entry.keep && entry.manifest["version"] == 4 && entry.manifest["kind"] != "manual")
        .map(|entry| (entry.path, entry.manifest["createdAtMs"].as_i64().unwrap_or(0), entry.manifest["generation"].as_i64().unwrap_or(0))).collect())
}

pub(crate) fn legacy_cleanup_summary_from_scan(
    tenant_dir: &Path,
    pinned_generations: &HashSet<i64>,
    scan: &StorageScan,
) -> Value {
    let candidates = match legacy_cleanup_candidates(tenant_dir, pinned_generations, scan.scanned_at_ms, false) { Ok(items) => items, Err(error) => return json!({"ok":false,"error":error}) };
    let reclaimable_bytes = candidates
        .iter()
        .filter_map(|(manifest, _, _)| {
            manifest
                .parent()
                .and_then(|directory| scan.snapshot_bytes.get(directory))
        })
        .copied()
        .sum::<i64>();
    json!({
        "ok": scan.scan_complete,
        "candidateCount": candidates.len(),
        "reclaimableBytes": reclaimable_bytes,
        "scanComplete": scan.scan_complete
    })
}

pub(crate) fn legacy_quarantine_summary(tenant_dir: &Path, now_ms: i64) -> Result<Value, String> {
    let records = read_legacy_quarantine_records(tenant_dir, now_ms)?;
    let active = records
        .iter()
        .filter(|record| record.get("status").and_then(Value::as_str) == Some("quarantined"))
        .collect::<Vec<_>>();
    let quarantined_bytes = active
        .iter()
        .map(|record| record.get("bytes").and_then(Value::as_i64).unwrap_or(0))
        .sum::<i64>();
    let purge_after_ms = active
        .iter()
        .filter_map(|record| record.get("purgeAfterMs").and_then(Value::as_i64))
        .min()
        .unwrap_or(0);
    let review_count = records
        .iter()
        .filter(|record| record.get("status").and_then(Value::as_str) == Some("review_required"))
        .count();
    Ok(json!({
        "ok": true,
        "quarantinedCount": active.len(),
        "quarantinedBytes": quarantined_bytes,
        "purgeAfterMs": purge_after_ms,
        "reviewCount": review_count,
        "items": records.iter().map(|record| json!({"snapshotName":record["snapshotName"],"generation":record["generation"],"bytes":record["bytes"],
            "purgeAfterMs":record["purgeAfterMs"],"reason":record["reviewReason"],"status":record["status"],
            "action":if record["status"]=="quarantined" && record["purgeAfterMs"].as_i64().is_some_and(|at| at<=now_ms) {"verify_before_delete"} else {"review"}})).collect::<Vec<_>>()
    }))
}

pub(crate) fn legacy_cleanup_preview(
    tenant_dir: &Path,
    pinned_generations: &HashSet<i64>,
) -> Value {
    let candidates = match legacy_cleanup_candidates(tenant_dir, pinned_generations, Utc::now().timestamp_millis(), true) {
        Ok(items) => items,
        Err(error) => return json!({"ok":false,"error":error}),
    };
    let mut hasher = Sha256::new();
    let mut bytes = 0i64;
    let mut records = Vec::new();
    for (manifest, created, generation) in &candidates {
        let snapshot = manifest.parent().unwrap_or(Path::new("."));
        let Ok(fingerprint) = snapshot_fingerprint(snapshot) else {
            continue;
        };
        let size = directory_size(snapshot);
        bytes += size;
        let line = format!(
            "{}\0{}\0{}\0{}\n",
            snapshot.to_string_lossy(),
            size,
            fingerprint,
            generation
        );
        hasher.update(line.as_bytes());
        records.push(json!({ "manifestPath": manifest.to_string_lossy(), "createdAtMs": created, "generation": generation, "bytes": size }));
    }
    json!({ "ok": true, "previewToken": format!("{:x}", hasher.finalize()), "candidateCount": records.len(), "reclaimableBytes": bytes, "candidates": records })
}

pub(crate) fn apply_legacy_cleanup(
    tenant_dir: &Path,
    pinned_generations: &HashSet<i64>,
    preview_token: &str,
    validated_replacement_created_at_ms: i64,
    now_ms: i64,
) -> Result<Value, String> {
    let preview = legacy_cleanup_preview(tenant_dir, pinned_generations);
    if preview.get("previewToken").and_then(Value::as_str) != Some(preview_token) {
        return Err("backup_legacy_cleanup_preview_changed".to_string());
    }
    let result = quarantine_legacy_snapshots(
        tenant_dir,
        pinned_generations,
        validated_replacement_created_at_ms,
        now_ms,
    )?;
    Ok(json!({
        "ok": true,
        "quarantined": result.get("quarantined").cloned().unwrap_or(json!(0)),
        "quarantinedBytes": result.get("quarantinedBytes").cloned().unwrap_or(json!(0)),
        "deleted": 0,
        "reclaimedBytes": 0
    }))
}

pub(crate) fn quarantine_legacy_snapshots(
    tenant_dir: &Path,
    pinned_generations: &HashSet<i64>,
    _validated_replacement_created_at_ms: i64,
    now_ms: i64,
) -> Result<Value, String> {
    let mut records = load_reconciled_legacy_quarantine_records(tenant_dir, now_ms)?;
    let protected = records
        .iter()
        .filter(|record| record.get("status").and_then(Value::as_str) == Some("restored"))
        .filter_map(|record| {
            Some((
                record.get("originalRelativePath")?.as_str()?.to_string(),
                record.get("fingerprint")?.as_str()?.to_string(),
            ))
        })
        .collect::<HashSet<_>>();
    let mut quarantined_count = 0i64;
    let mut quarantined_bytes = 0i64;
    for (manifest_path, created_at_ms, generation) in
        legacy_cleanup_candidates(tenant_dir, pinned_generations, now_ms, true)?
    {
        let snapshot = manifest_path
            .parent()
            .ok_or_else(|| "backup_snapshot_parent_missing".to_string())?;
        let snapshot_name = snapshot
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "backup_snapshot_name_missing".to_string())?
            .to_string();
        if snapshot.parent() != Some(tenant_dir.join("snapshots").as_path()) {
            return Err("backup_snapshot_cleanup_scope_invalid".to_string());
        }
        let fingerprint = snapshot_fingerprint(snapshot)?;
        let original_relative = format!("snapshots/{snapshot_name}");
        if protected.contains(&(original_relative.clone(), fingerprint.clone())) {
            continue;
        }
        let bytes = directory_size(snapshot);
        let id_seed =
            format!("{snapshot_name}\0{created_at_ms}\0{generation}\0{fingerprint}");
        let id = format!("{:x}", Sha256::digest(id_seed.as_bytes()));
        let quarantine_relative = format!("{LEGACY_QUARANTINE_DIR}/items/{id}");
        let target = tenant_dir.join(&quarantine_relative);
        if target.exists() {
            return Err("backup_legacy_quarantine_target_exists".to_string());
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("backup_legacy_quarantine_dir_failed:{error}"))?;
        }
        let prior = records.iter().find(|record| record["originalRelativePath"] == original_relative && record["fingerprint"] == fingerprint).cloned();
        records.retain(|record| record["id"] != id);
        records.push(json!({
            "id": id,
            "snapshotName": snapshot_name,
            "originalRelativePath": original_relative,
            "quarantineRelativePath": quarantine_relative,
            "fingerprint": fingerprint,
            "bytes": bytes,
            "generation": generation,
            "createdAtMs": created_at_ms,
            "quarantinedAtMs": prior.as_ref().and_then(|r| r["quarantinedAtMs"].as_i64()).unwrap_or(now_ms),
            "purgeAfterMs": prior.as_ref().and_then(|r| r["purgeAfterMs"].as_i64()).unwrap_or(now_ms.saturating_add(QUARANTINE_DAYS * 86_400_000)),
            "status": "pending",
            "updatedAtMs": now_ms
        }));
        save_legacy_quarantine_records(tenant_dir, &records, now_ms)?;
        if let Err(error) = fs::rename(snapshot, &target) {
            set_legacy_record_status(
                &mut records,
                &id,
                "review_required",
                Some("move_failed"),
                now_ms,
            );
            save_legacy_quarantine_records(tenant_dir, &records, now_ms)?;
            return Err(format!("backup_legacy_quarantine_move_failed:{error}"));
        }
        if snapshot_fingerprint(&target)? != fingerprint {
            let restored = fs::rename(&target, snapshot).is_ok();
            set_legacy_record_status(
                &mut records,
                &id,
                if restored {
                    "cancelled"
                } else {
                    "review_required"
                },
                Some("fingerprint_changed_during_move"),
                now_ms,
            );
            save_legacy_quarantine_records(tenant_dir, &records, now_ms)?;
            continue;
        }
        set_legacy_record_status(&mut records, &id, "quarantined", None, now_ms);
        save_legacy_quarantine_records(tenant_dir, &records, now_ms)?;
        quarantined_count += 1;
        quarantined_bytes += bytes;
    }
    Ok(json!({
        "ok": true,
        "quarantined": quarantined_count,
        "quarantinedBytes": quarantined_bytes
    }))
}

pub(crate) fn undo_legacy_quarantine(tenant_dir: &Path, now_ms: i64) -> Result<Value, String> {
    let mut records = load_reconciled_legacy_quarantine_records(tenant_dir, now_ms)?;
    let ids = records
        .iter()
        .filter(|record| record.get("status").and_then(Value::as_str) == Some("quarantined"))
        .filter_map(|record| record.get("id").and_then(Value::as_str).map(str::to_string))
        .collect::<Vec<_>>();
    let mut restored = 0i64;
    let mut restored_bytes = 0i64;
    for id in ids {
        let Some(record) = records
            .iter()
            .find(|record| record.get("id").and_then(Value::as_str) == Some(id.as_str()))
            .cloned()
        else {
            continue;
        };
        let validation = (|| -> Result<(PathBuf, PathBuf), String> {
            let (original, quarantined) = legacy_record_paths(tenant_dir, &record)?;
            if original.exists() || !quarantined.is_dir() {
                return Err("undo_path_state_changed".to_string());
            }
            let fingerprint = snapshot_fingerprint(&quarantined)?;
            if record.get("fingerprint").and_then(Value::as_str) != Some(fingerprint.as_str()) {
                return Err("undo_fingerprint_changed".to_string());
            }
            Ok((original, quarantined))
        })();
        let (original, quarantined) = match validation {
            Ok(paths) => paths,
            Err(reason) => {
                set_legacy_record_status(
                    &mut records,
                    &id,
                    "review_required",
                    Some(&reason),
                    now_ms,
                );
                save_legacy_quarantine_records(tenant_dir, &records, now_ms)?;
                continue;
            }
        };
        set_legacy_record_status(&mut records, &id, "restoring", None, now_ms);
        save_legacy_quarantine_records(tenant_dir, &records, now_ms)?;
        if let Err(error) = fs::rename(&quarantined, &original) {
            set_legacy_record_status(
                &mut records,
                &id,
                "review_required",
                Some("undo_move_failed"),
                now_ms,
            );
            save_legacy_quarantine_records(tenant_dir, &records, now_ms)?;
            return Err(format!("backup_legacy_quarantine_undo_failed:{error}"));
        }
        restored += 1;
        restored_bytes += record.get("bytes").and_then(Value::as_i64).unwrap_or(0);
        set_legacy_record_status(&mut records, &id, "restored", None, now_ms);
        save_legacy_quarantine_records(tenant_dir, &records, now_ms)?;
    }
    let review_count = records
        .iter()
        .filter(|record| record.get("status").and_then(Value::as_str) == Some("review_required"))
        .count();
    Ok(json!({
        "ok": true,
        "restored": restored,
        "restoredBytes": restored_bytes,
        "reviewCount": review_count
    }))
}

pub(crate) fn purge_legacy_quarantine(
    tenant_dir: &Path,
    pinned_generations: &HashSet<i64>,
    validated_replacement_created_at_ms: i64,
    now_ms: i64,
) -> Result<Value, String> {
    if validated_replacement_created_at_ms <= 0 {
        return Err("backup_quarantine_verified_replacement_required".to_string());
    }
    let replacements = retention::retention_plan(tenant_dir, now_ms, pinned_generations, true)?;
    let mut records = load_reconciled_legacy_quarantine_records(tenant_dir, now_ms)?;
    let ids = records
        .iter()
        .filter(|record| matches!(record.get("status").and_then(Value::as_str), Some("quarantined" | "purging")))
        .filter(|record| {
            record
                .get("purgeAfterMs")
                .and_then(Value::as_i64)
                .unwrap_or(i64::MAX)
                <= now_ms
        })
        .filter_map(|record| record.get("id").and_then(Value::as_str).map(str::to_string))
        .collect::<Vec<_>>();
    let mut purged = 0i64;
    let mut purged_bytes = 0i64;
    let mut deferred = Vec::new();
    for id in ids {
        let Some(record) = records
            .iter()
            .find(|record| record.get("id").and_then(Value::as_str) == Some(id.as_str()))
            .cloned()
        else {
            continue;
        };
        let generation = record
            .get("generation")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let created_at_ms = record
            .get("createdAtMs")
            .and_then(Value::as_i64)
            .unwrap_or(i64::MAX);
        let validation = (|| -> Result<PathBuf, String> {
            if generation > 0 && pinned_generations.contains(&generation) {
                return Err("generation_became_pinned".to_string());
            }
            let (original, quarantined) = legacy_record_paths(tenant_dir, &record)?;
            if original.exists() || !quarantined.is_dir() {
                return Err("purge_path_state_changed".to_string());
            }
            let version = if record["purgeInventory"].is_array() {record["snapshotVersion"].as_i64().unwrap_or(0)} else {json_file(&quarantined.join("manifest.json")).ok_or("purge_manifest_unavailable")?["version"].as_i64().unwrap_or(0)};
            if !matches!(version,2|3|4) {return Err("purge_version_unconfirmed".into());}
            if version == 4 && !replacements.iter().any(|entry| entry.keep && entry.manifest["version"] == 4 && entry.manifest["createdAtMs"].as_i64().is_some_and(|at| at >= created_at_ms) && entry.identity.is_some()) {
                return Err("verified_same_format_replacement_required".into());
            }
            if version < 4 && created_at_ms > validated_replacement_created_at_ms { return Err("verified_replacement_is_older".into()); }
            if !record["purgeInventory"].is_array() && record.get("fingerprint").and_then(Value::as_str) != Some(snapshot_fingerprint(&quarantined)?.as_str()) {
                return Err("purge_fingerprint_changed".to_string());
            }
            Ok(quarantined)
        })();
        let quarantined = match validation {
            Ok(path) => path,
            Err(reason) => {
                deferred.push(reason.clone());
                set_legacy_record_status(
                    &mut records,
                    &id,
                    if reason.contains("fingerprint_changed") || reason.contains("path_state_changed") {"review_required"} else if record["purgeInventory"].is_array() {"purging"} else {"quarantined"},
                    Some(&reason),
                    now_ms,
                );
                save_legacy_quarantine_records(tenant_dir, &records, now_ms)?;
                continue;
            }
        };
        let inventory = if let Some(files)=record["purgeInventory"].as_array() {files.clone()} else {purge::inventory(&quarantined)?};
        let version = record["snapshotVersion"].as_i64().or_else(|| json_file(&quarantined.join("manifest.json")).and_then(|m| m["version"].as_i64()));
        let stored=records.iter_mut().find(|r| r["id"]==id).ok_or("backup_quarantine_record_missing")?;
        stored["purgeInventory"]=json!(inventory); stored["snapshotVersion"]=json!(version);
        set_legacy_record_status(&mut records, &id, "purging", None, now_ms);
        save_legacy_quarantine_records(tenant_dir, &records, now_ms)?;
        let result=purge::remove(&quarantined,&inventory).unwrap_or_else(|error| json!({"ok":false,"error":error}));
        purged_bytes+=result["deletedBytes"].as_i64().unwrap_or(0);
        if result["ok"] != true {
            deferred.push(result["error"].as_str().unwrap_or("backup_purge_failed").to_string());
            continue;
        }
        purged += 1;
        set_legacy_record_status(&mut records, &id, "purged", None, now_ms);
        save_legacy_quarantine_records(tenant_dir, &records, now_ms)?;
    }
    let review_count = records
        .iter()
        .filter(|record| record.get("status").and_then(Value::as_str) == Some("review_required"))
        .count();
    Ok(json!({
        "ok": deferred.is_empty(),
        "purged": purged,
        "purgedBytes": purged_bytes,
        "reviewCount": review_count,
        "errors": deferred,
        "error": if deferred.is_empty() {Value::Null} else {json!("backup_quarantine_purge_deferred")}
    }))
}
