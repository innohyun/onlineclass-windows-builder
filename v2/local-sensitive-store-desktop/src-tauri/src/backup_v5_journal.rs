use super::*;
use std::io::Write;

struct Journal {
    records: Vec<Value>,
    sequence: u64,
    updated_at_ms: i64,
    priority: usize,
}

fn selected_journal(tenant_dir: &Path) -> Result<Option<Journal>, String> {
    let root = legacy_quarantine_root(tenant_dir);
    let mut candidates = Vec::new();
    let mut found = false;
    for (priority, name) in [
        "manifest.previous.json",
        "manifest.next.json",
        LEGACY_QUARANTINE_MANIFEST,
    ]
    .iter()
    .enumerate()
    {
        let path = root.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => found = true,
            Ok(_) => return Err("backup_legacy_quarantine_journal_path_invalid".to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "backup_legacy_quarantine_journal_read_failed:{error}"
                ))
            }
        }
        let Some(manifest) = json_file(&path) else {
            continue;
        };
        if manifest.get("version").and_then(Value::as_i64) != Some(1) {
            continue;
        }
        let Some(records) = manifest.get("records").and_then(Value::as_array) else {
            continue;
        };
        if manifest.get("manifestDigest").and_then(Value::as_str)
            != Some(legacy_records_digest(records)?.as_str())
        {
            continue;
        }
        candidates.push(Journal {
            records: records.clone(),
            sequence: manifest
                .get("sequence")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            updated_at_ms: manifest
                .get("updatedAtMs")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            priority,
        });
    }
    candidates.sort_by_key(|journal| (journal.sequence, journal.updated_at_ms, journal.priority));
    let Some(selected) = candidates.pop() else {
        return if found {
            Err("backup_legacy_quarantine_manifest_digest_mismatch".to_string())
        } else {
            Ok(None)
        };
    };
    if candidates.iter().any(|journal| {
        journal.sequence == selected.sequence
            && (selected.sequence > 0 || journal.updated_at_ms == selected.updated_at_ms)
            && journal.records != selected.records
    }) {
        return Err("backup_legacy_quarantine_journal_conflict".to_string());
    }
    Ok(Some(selected))
}

pub(super) fn legacy_quarantine_root(tenant_dir: &Path) -> PathBuf {
    tenant_dir.join(LEGACY_QUARANTINE_DIR)
}

pub(super) fn legacy_quarantine_manifest_path(tenant_dir: &Path) -> PathBuf {
    legacy_quarantine_root(tenant_dir).join(LEGACY_QUARANTINE_MANIFEST)
}

pub(super) fn legacy_records_digest(records: &[Value]) -> Result<String, String> {
    let raw = serde_json::to_vec(records)
        .map_err(|error| format!("backup_legacy_quarantine_manifest_encode_failed:{error}"))?;
    Ok(format!("{:x}", Sha256::digest(raw)))
}

pub(super) fn save_legacy_quarantine_records(
    tenant_dir: &Path,
    records: &[Value],
    now_ms: i64,
) -> Result<(), String> {
    let root = legacy_quarantine_root(tenant_dir);
    fs::create_dir_all(&root)
        .map_err(|error| format!("backup_legacy_quarantine_dir_failed:{error}"))?;
    let path = legacy_quarantine_manifest_path(tenant_dir);
    let temporary = root.join("manifest.next.json");
    let previous = root.join("manifest.previous.json");
    let sequence = selected_journal(tenant_dir)?
        .map(|journal| journal.sequence)
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| "backup_legacy_quarantine_sequence_overflow".to_string())?;
    let manifest = json!({
        "version": 1,
        "sequence": sequence,
        "updatedAtMs": now_ms,
        "records": records,
        "manifestDigest": legacy_records_digest(records)?
    });
    let raw = serde_json::to_string_pretty(&manifest)
        .map_err(|error| format!("backup_legacy_quarantine_manifest_encode_failed:{error}"))?;
    let mut file = fs::File::create(&temporary)
        .map_err(|error| format!("backup_legacy_quarantine_manifest_write_failed:{error}"))?;
    file.write_all(format!("{raw}\n").as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("backup_legacy_quarantine_manifest_write_failed:{error}"))?;
    drop(file);
    commit_staged_journal(&path, &temporary, &previous)
}

fn commit_staged_journal(path: &Path, temporary: &Path, previous: &Path) -> Result<(), String> {
    if previous.exists() {
        fs::remove_file(&previous)
            .map_err(|error| format!("backup_legacy_quarantine_previous_cleanup_failed:{error}"))?;
    }
    if path.exists() {
        fs::rename(&path, &previous)
            .map_err(|error| format!("backup_legacy_quarantine_manifest_rotate_failed:{error}"))?;
    }
    if let Err(error) = fs::rename(&temporary, &path) {
        if previous.exists() {
            let _ = fs::rename(&previous, &path);
        }
        return Err(format!(
            "backup_legacy_quarantine_manifest_commit_failed:{error}"
        ));
    }
    if previous.exists() {
        let _ = fs::remove_file(previous);
    }
    Ok(())
}

pub(super) fn load_legacy_quarantine_records(tenant_dir: &Path) -> Result<Vec<Value>, String> {
    let Some(journal) = selected_journal(tenant_dir)? else {
        return Ok(Vec::new());
    };
    // A complete next/previous journal remains authoritative across either rename boundary.
    if journal.priority == 1 {
        let root = legacy_quarantine_root(tenant_dir);
        commit_staged_journal(
            &legacy_quarantine_manifest_path(tenant_dir),
            &root.join("manifest.next.json"),
            &root.join("manifest.previous.json"),
        )?;
    } else if journal.priority == 0 {
        save_legacy_quarantine_records(tenant_dir, &journal.records, journal.updated_at_ms)?;
    }
    Ok(journal.records)
}

pub(super) fn legacy_record_paths(
    tenant_dir: &Path,
    record: &Value,
) -> Result<(PathBuf, PathBuf), String> {
    let snapshot_name = record
        .get("snapshotName")
        .and_then(Value::as_str)
        .filter(|name| {
            !name.is_empty()
                && Path::new(name).components().count() == 1
                && Path::new(name)
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_)))
                && !matches!(name.as_bytes(), b"." | b"..")
        })
        .ok_or_else(|| "backup_legacy_quarantine_snapshot_name_invalid".to_string())?;
    let id = record
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| "backup_legacy_quarantine_id_invalid".to_string())?;
    let original = tenant_dir.join("snapshots").join(snapshot_name);
    let quarantined = legacy_quarantine_root(tenant_dir).join("items").join(id);
    let expected_original = format!("snapshots/{snapshot_name}");
    let expected_quarantined = format!("{LEGACY_QUARANTINE_DIR}/items/{id}");
    if record.get("originalRelativePath").and_then(Value::as_str)
        != Some(expected_original.as_str())
        || record.get("quarantineRelativePath").and_then(Value::as_str)
            != Some(expected_quarantined.as_str())
    {
        return Err("backup_legacy_quarantine_scope_invalid".to_string());
    }
    Ok((original, quarantined))
}

pub(super) fn set_legacy_record_status(
    records: &mut [Value],
    id: &str,
    status: &str,
    reason: Option<&str>,
    now_ms: i64,
) {
    if let Some(record) = records
        .iter_mut()
        .find(|record| record.get("id").and_then(Value::as_str) == Some(id))
    {
        record["status"] = json!(status);
        record["updatedAtMs"] = json!(now_ms);
        if let Some(reason) = reason {
            record["reviewReason"] = json!(reason);
        } else if let Some(object) = record.as_object_mut() {
            object.remove("reviewReason");
        }
    }
}

pub(super) fn reconcile_legacy_quarantine_records(
    tenant_dir: &Path,
    records: &mut [Value],
    now_ms: i64,
) -> bool {
    let mut changed = false;
    for record in records {
        let status = record
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !matches!(status.as_str(), "pending" | "restoring" | "purging") {
            continue;
        }
        let Ok((original, quarantined)) = legacy_record_paths(tenant_dir, record) else {
            record["status"] = json!("review_required");
            record["reviewReason"] = json!("scope_invalid");
            record["updatedAtMs"] = json!(now_ms);
            changed = true;
            continue;
        };
        let (mut next_status, mut reason) = match status.as_str() {
            "pending" if quarantined.exists() && !original.exists() => ("quarantined", None),
            "pending" if original.exists() && !quarantined.exists() => ("cancelled", None),
            "restoring" if original.exists() && !quarantined.exists() => ("restored", None),
            "restoring" if quarantined.exists() && !original.exists() => ("quarantined", None),
            "purging" if !quarantined.exists() && !original.exists() => ("purged", None),
            "purging" if quarantined.exists() && !original.exists() => ("quarantined", None),
            _ => ("review_required", Some("interrupted_state_conflict")),
        };
        let check_path = match next_status {
            "quarantined" => Some(&quarantined),
            "restored" => Some(&original),
            _ => None,
        };
        if let Some(path) = check_path {
            if !snapshot_fingerprint(path)
                .ok()
                .as_deref()
                .is_some_and(|digest| {
                    record.get("fingerprint").and_then(Value::as_str) == Some(digest)
                })
            {
                next_status = "review_required";
                reason = Some("interrupted_fingerprint_changed");
            }
        }
        record["status"] = json!(next_status);
        record["updatedAtMs"] = json!(now_ms);
        if let Some(reason) = reason {
            record["reviewReason"] = json!(reason);
        }
        changed = true;
    }
    changed
}

pub(super) fn load_reconciled_legacy_quarantine_records(
    tenant_dir: &Path,
    now_ms: i64,
) -> Result<Vec<Value>, String> {
    let mut records = load_legacy_quarantine_records(tenant_dir)?;
    let reconciled = reconcile_legacy_quarantine_records(tenant_dir, &mut records, now_ms);
    let orphans = reconcile_orphan_items(tenant_dir, &mut records, now_ms)?;
    if reconciled || orphans {
        save_legacy_quarantine_records(tenant_dir, &records, now_ms)?;
    }
    Ok(records)
}

fn reconcile_orphan_items(
    tenant_dir: &Path,
    records: &mut Vec<Value>,
    now_ms: i64,
) -> Result<bool, String> {
    let root = legacy_quarantine_root(tenant_dir).join("items");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "backup_legacy_quarantine_items_read_failed:{error}"
            ))
        }
    };
    let mut known = records
        .iter()
        .filter_map(|record| {
            record
                .get("quarantineRelativePath")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect::<HashSet<_>>();
    let mut changed = false;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("backup_legacy_quarantine_items_read_failed:{error}"))?;
        let name = entry.file_name().to_string_lossy().to_string();
        let relative = format!("{LEGACY_QUARANTINE_DIR}/items/{name}");
        if !known.insert(relative.clone()) {
            if let Some(record) = records.iter_mut().find(|record| {
                record.get("quarantineRelativePath").and_then(Value::as_str)
                    == Some(relative.as_str())
                    && matches!(
                        record.get("status").and_then(Value::as_str),
                        Some("purged" | "restored" | "cancelled")
                    )
            }) {
                record["status"] = json!("review_required");
                record["reviewReason"] = json!("quarantine_item_reappeared");
                record["updatedAtMs"] = json!(now_ms);
                changed = true;
            }
            continue;
        }
        let file_type = entry
            .file_type()
            .map_err(|error| format!("backup_legacy_quarantine_items_read_failed:{error}"))?;
        let bytes = if file_type.is_symlink() {
            0
        } else {
            directory_size(&entry.path())
        };
        records.push(json!({
            "id": format!("{:x}", Sha256::digest(format!("orphan\0{name}").as_bytes())),
            "quarantineRelativePath": relative,
            "bytes": bytes,
            "status": "review_required",
            "reviewReason": "orphaned_quarantine_item",
            "updatedAtMs": now_ms
        }));
        changed = true;
    }
    Ok(changed)
}
