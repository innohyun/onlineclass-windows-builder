use super::journal::*;
use super::*;

fn legacy_cleanup_candidates(
    tenant_dir: &Path,
    pinned_generations: &HashSet<i64>,
) -> Vec<(PathBuf, i64, i64)> {
    let mut legacy = snapshot_manifests(tenant_dir, false)
        .into_iter()
        .filter_map(|path| {
            let manifest = json_file(&path)?;
            let version = manifest.get("version").and_then(Value::as_i64).unwrap_or(0);
            if version >= SNAPSHOT_VERSION
                || manifest.get("kind").and_then(Value::as_str) == Some("manual")
            {
                return None;
            }
            let generation = manifest
                .get("generation")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let created = manifest
                .get("createdAtMs")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            Some((path, created, generation))
        })
        .collect::<Vec<_>>();
    legacy.sort_by(|left, right| right.1.cmp(&left.1));
    legacy
        .into_iter()
        .enumerate()
        .filter(|(index, (_, _, generation))| {
            *index > 0 && (*generation == 0 || !pinned_generations.contains(generation))
        })
        .map(|(_, item)| item)
        .collect()
}

pub(crate) fn legacy_cleanup_summary_from_scan(
    tenant_dir: &Path,
    pinned_generations: &HashSet<i64>,
    scan: &StorageScan,
) -> Value {
    let candidates = legacy_cleanup_candidates(tenant_dir, pinned_generations);
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
    let records = load_reconciled_legacy_quarantine_records(tenant_dir, now_ms)?;
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
        "reviewCount": review_count
    }))
}

pub(crate) fn legacy_cleanup_preview(
    tenant_dir: &Path,
    pinned_generations: &HashSet<i64>,
) -> Value {
    let candidates = legacy_cleanup_candidates(tenant_dir, pinned_generations);
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
    validated_v5_created_at_ms: i64,
    now_ms: i64,
) -> Result<Value, String> {
    let preview = legacy_cleanup_preview(tenant_dir, pinned_generations);
    if preview.get("previewToken").and_then(Value::as_str) != Some(preview_token) {
        return Err("backup_legacy_cleanup_preview_changed".to_string());
    }
    let result = quarantine_legacy_snapshots(
        tenant_dir,
        pinned_generations,
        validated_v5_created_at_ms,
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
    validated_v5_created_at_ms: i64,
    now_ms: i64,
) -> Result<Value, String> {
    if validated_v5_created_at_ms <= 0 {
        return Err("backup_legacy_quarantine_verified_v5_required".to_string());
    }
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
        legacy_cleanup_candidates(tenant_dir, pinned_generations)
    {
        if created_at_ms > validated_v5_created_at_ms {
            continue;
        }
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
            format!("{snapshot_name}\0{created_at_ms}\0{generation}\0{fingerprint}\0{now_ms}");
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
        records.push(json!({
            "id": id,
            "snapshotName": snapshot_name,
            "originalRelativePath": original_relative,
            "quarantineRelativePath": quarantine_relative,
            "fingerprint": fingerprint,
            "bytes": bytes,
            "generation": generation,
            "createdAtMs": created_at_ms,
            "quarantinedAtMs": now_ms,
            "purgeAfterMs": now_ms.saturating_add(QUARANTINE_DAYS * 86_400_000),
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
    validated_v5_created_at_ms: i64,
    now_ms: i64,
) -> Result<Value, String> {
    if validated_v5_created_at_ms <= 0 {
        return Err("backup_legacy_quarantine_verified_v5_required".to_string());
    }
    let mut records = load_reconciled_legacy_quarantine_records(tenant_dir, now_ms)?;
    let ids = records
        .iter()
        .filter(|record| record.get("status").and_then(Value::as_str) == Some("quarantined"))
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
            if created_at_ms > validated_v5_created_at_ms {
                return Err("verified_v5_is_older".to_string());
            }
            let (original, quarantined) = legacy_record_paths(tenant_dir, &record)?;
            if original.exists() || !quarantined.is_dir() {
                return Err("purge_path_state_changed".to_string());
            }
            let fingerprint = snapshot_fingerprint(&quarantined)?;
            if record.get("fingerprint").and_then(Value::as_str) != Some(fingerprint.as_str()) {
                return Err("purge_fingerprint_changed".to_string());
            }
            Ok(quarantined)
        })();
        let quarantined = match validation {
            Ok(path) => path,
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
        set_legacy_record_status(&mut records, &id, "purging", None, now_ms);
        save_legacy_quarantine_records(tenant_dir, &records, now_ms)?;
        if let Err(error) = fs::remove_dir_all(&quarantined) {
            set_legacy_record_status(
                &mut records,
                &id,
                "review_required",
                Some("purge_delete_failed"),
                now_ms,
            );
            save_legacy_quarantine_records(tenant_dir, &records, now_ms)?;
            return Err(format!("backup_legacy_quarantine_purge_failed:{error}"));
        }
        purged += 1;
        purged_bytes += record.get("bytes").and_then(Value::as_i64).unwrap_or(0);
        set_legacy_record_status(&mut records, &id, "purged", None, now_ms);
        save_legacy_quarantine_records(tenant_dir, &records, now_ms)?;
    }
    let review_count = records
        .iter()
        .filter(|record| record.get("status").and_then(Value::as_str) == Some("review_required"))
        .count();
    Ok(json!({
        "ok": true,
        "purged": purged,
        "purgedBytes": purged_bytes,
        "reviewCount": review_count
    }))
}

pub(crate) fn maintain_legacy_quarantine(
    tenant_dir: &Path,
    pinned_generations: &HashSet<i64>,
    validated_v5_created_at_ms: i64,
    now_ms: i64,
) -> Result<Value, String> {
    let purge = purge_legacy_quarantine(
        tenant_dir,
        pinned_generations,
        validated_v5_created_at_ms,
        now_ms,
    )?;
    let quarantine = quarantine_legacy_snapshots(
        tenant_dir,
        pinned_generations,
        validated_v5_created_at_ms,
        now_ms,
    )?;
    Ok(json!({ "ok": true, "purge": purge, "quarantine": quarantine }))
}
