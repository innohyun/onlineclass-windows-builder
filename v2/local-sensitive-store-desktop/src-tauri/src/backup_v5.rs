use crate::backup::{sha256_file, ArtifactDigest};
use chrono::{DateTime, Datelike, FixedOffset, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const SNAPSHOT_VERSION: i64 = 5;
const RECENT_KEEP: usize = 10;
const DAILY_KEEP_DAYS: i64 = 30;
const MONTHLY_KEEP_MONTHS: i32 = 12;
const PRE_RESTORE_KEEP: usize = 5;
const QUARANTINE_DAYS: i64 = 30;
const LEGACY_QUARANTINE_DIR: &str = "legacy-snapshot-quarantine";
const LEGACY_QUARANTINE_MANIFEST: &str = "manifest.json";

#[derive(Clone, Debug)]
pub(crate) struct ContentObject {
    pub(crate) artifact: ArtifactDigest,
    pub(crate) created: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct StorageScan {
    pub(crate) object_count: i64,
    pub(crate) object_bytes: i64,
    pub(crate) database_history_bytes: i64,
    pub(crate) legacy_snapshot_count: i64,
    pub(crate) legacy_snapshot_bytes: i64,
}

fn json_file(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn snapshot_manifests(tenant_dir: &Path, include_staging: bool) -> Vec<PathBuf> {
    let snapshots = tenant_dir.join("snapshots");
    let Ok(entries) = fs::read_dir(snapshots) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir()
                || (!include_staging && entry.file_name().to_string_lossy().ends_with(".staging"))
            {
                return None;
            }
            let manifest = path.join("manifest.json");
            manifest.is_file().then_some(manifest)
        })
        .collect()
}

fn safe_object_relative_path(sha256: &str) -> Result<String, String> {
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("backup_object_hash_invalid".to_string());
    }
    Ok(format!("objects/sha256/{}/{}", &sha256[..2], sha256))
}

pub(crate) fn put_object(
    tenant_dir: &Path,
    source: &Path,
    staging_tag: &str,
) -> Result<ContentObject, String> {
    let (size, sha256) = sha256_file(source)?;
    let relative_path = safe_object_relative_path(&sha256)?;
    let target = tenant_dir.join(&relative_path);
    if target.is_file() {
        let existing = sha256_file(&target)?;
        if existing != (size, sha256.clone()) {
            return Err("backup_object_digest_conflict".to_string());
        }
        return Ok(ContentObject {
            artifact: ArtifactDigest {
                relative_path,
                size,
                sha256,
            },
            created: false,
        });
    }
    let parent = target
        .parent()
        .ok_or_else(|| "backup_object_parent_missing".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("backup_object_dir_failed:{error}"))?;
    let temporary = parent.join(format!("{}.{}.staging", sha256, staging_tag));
    if temporary.exists() {
        fs::remove_file(&temporary)
            .map_err(|error| format!("backup_object_staging_cleanup_failed:{error}"))?;
    }
    fs::copy(source, &temporary).map_err(|error| format!("backup_object_copy_failed:{error}"))?;
    let staged = sha256_file(&temporary)?;
    if staged != (size, sha256.clone()) {
        let _ = fs::remove_file(&temporary);
        return Err("backup_object_staging_digest_mismatch".to_string());
    }
    let created = match fs::rename(&temporary, &target) {
        Ok(()) => true,
        Err(_) if target.is_file() => {
            let _ = fs::remove_file(&temporary);
            if sha256_file(&target)? != (size, sha256.clone()) {
                return Err("backup_object_digest_conflict".to_string());
            }
            false
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            return Err(format!("backup_object_commit_failed:{error}"));
        }
    };
    Ok(ContentObject {
        artifact: ArtifactDigest {
            relative_path,
            size,
            sha256,
        },
        created,
    })
}

pub(crate) fn artifact_path(
    manifest_path: &Path,
    version: i64,
    relative: &Path,
) -> Result<PathBuf, String> {
    if version == SNAPSHOT_VERSION && relative.starts_with("objects/sha256") {
        return Ok(crate::backup_v4::tenant_dir(manifest_path)?.join(relative));
    }
    Ok(manifest_path
        .parent()
        .ok_or_else(|| "backup_manifest_parent_missing".to_string())?
        .join(relative))
}

fn kst_parts(value: i64) -> Option<(chrono::NaiveDate, i32, u32)> {
    let offset = FixedOffset::east_opt(9 * 60 * 60)?;
    let date = DateTime::<Utc>::from_timestamp_millis(value)?.with_timezone(&offset);
    Some((date.date_naive(), date.year(), date.month()))
}

pub(crate) fn prune_snapshots(tenant_dir: &Path, now_ms: i64) -> Result<(), String> {
    let snapshots_root = tenant_dir.join("snapshots");
    let mut managed = Vec::new();
    let mut pre_restore = Vec::new();
    for path in snapshot_manifests(tenant_dir, false) {
        let Some(manifest) = json_file(&path) else {
            continue;
        };
        if manifest.get("version").and_then(Value::as_i64) != Some(SNAPSHOT_VERSION) {
            continue;
        }
        let created = manifest
            .get("createdAtMs")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        match manifest.get("kind").and_then(Value::as_str).unwrap_or("") {
            "auto_sync" | "scheduled" => managed.push((path, created)),
            "pre_restore" => pre_restore.push((path, created)),
            _ => {}
        }
    }
    managed.sort_by(|left, right| right.1.cmp(&left.1));
    pre_restore.sort_by(|left, right| right.1.cmp(&left.1));
    let mut keep = HashSet::new();
    let mut days = HashSet::new();
    let mut months = HashSet::new();
    let now_parts = kst_parts(now_ms);
    for (index, (path, created)) in managed.iter().enumerate() {
        if index < RECENT_KEEP {
            keep.insert(path.clone());
        }
        let (Some((created_date, year, month)), Some((now_date, now_year, now_month))) =
            (kst_parts(*created), now_parts)
        else {
            continue;
        };
        let day_age = now_date.signed_duration_since(created_date).num_days();
        if (0..DAILY_KEEP_DAYS).contains(&day_age) && days.insert(created_date) {
            keep.insert(path.clone());
        }
        let month_age = (now_year * 12 + now_month as i32) - (year * 12 + month as i32);
        if (0..MONTHLY_KEEP_MONTHS).contains(&month_age) && months.insert((year, month)) {
            keep.insert(path.clone());
        }
    }
    for (path, _) in pre_restore.iter().take(PRE_RESTORE_KEEP) {
        keep.insert(path.clone());
    }
    for (path, _) in managed.into_iter().chain(pre_restore) {
        if keep.contains(&path) {
            continue;
        }
        let snapshot = path
            .parent()
            .ok_or_else(|| "backup_snapshot_parent_missing".to_string())?;
        if snapshot.parent() != Some(snapshots_root.as_path()) {
            return Err("backup_snapshot_prune_scope_invalid".to_string());
        }
        fs::remove_dir_all(snapshot)
            .map_err(|error| format!("backup_snapshot_prune_failed:{error}"))?;
    }
    Ok(())
}

fn manifest_object_references(tenant_dir: &Path) -> HashSet<String> {
    let mut references = HashSet::new();
    for path in snapshot_manifests(tenant_dir, true) {
        let Some(manifest) = json_file(&path) else {
            continue;
        };
        if manifest.get("version").and_then(Value::as_i64) != Some(SNAPSHOT_VERSION) {
            continue;
        }
        for artifact in manifest
            .get("artifacts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(relative) = artifact.get("relativePath").and_then(Value::as_str) else {
                continue;
            };
            if relative.starts_with("objects/sha256/") {
                references.insert(relative.to_string());
            }
        }
    }
    references
}

fn object_files(root: &Path, prefix: &str) -> Vec<(String, PathBuf)> {
    let Ok(prefixes) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for directory in prefixes
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
    {
        let Ok(entries) = fs::read_dir(directory.path()) else {
            continue;
        };
        for entry in entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_file())
        {
            let hash = entry.file_name().to_string_lossy().to_string();
            if hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                files.push((
                    format!(
                        "{prefix}/{}/{}",
                        directory.file_name().to_string_lossy(),
                        hash
                    ),
                    entry.path(),
                ));
            }
        }
    }
    files
}

pub(crate) fn quarantine_unreferenced_objects(
    tenant_dir: &Path,
    current_references: &HashSet<String>,
    now_ms: i64,
) -> Result<Value, String> {
    let mut referenced = manifest_object_references(tenant_dir);
    referenced.extend(current_references.iter().cloned());
    let object_root = tenant_dir.join("objects/sha256");
    let quarantine_root = tenant_dir.join("objects-quarantine");
    let mut quarantined = 0i64;
    let mut quarantined_bytes = 0i64;
    for (relative, path) in object_files(&object_root, "objects/sha256") {
        if referenced.contains(&relative) {
            continue;
        }
        let size = fs::metadata(&path)
            .map(|metadata| metadata.len() as i64)
            .unwrap_or(0);
        let hash = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        let prefix = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            .unwrap_or("00");
        let target = quarantine_root
            .join(now_ms.to_string())
            .join(prefix)
            .join(hash);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("backup_object_quarantine_dir_failed:{error}"))?;
        }
        fs::rename(&path, &target)
            .map_err(|error| format!("backup_object_quarantine_failed:{error}"))?;
        quarantined += 1;
        quarantined_bytes += size;
    }
    let mut deleted = 0i64;
    let mut deleted_bytes = 0i64;
    if let Ok(entries) = fs::read_dir(&quarantine_root) {
        for entry in entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
        {
            let quarantined_at = entry
                .file_name()
                .to_string_lossy()
                .parse::<i64>()
                .unwrap_or(now_ms);
            if now_ms.saturating_sub(quarantined_at) < QUARANTINE_DAYS * 86_400_000 {
                continue;
            }
            for (suffix, path) in object_files(&entry.path(), "objects/sha256") {
                if referenced.contains(&suffix) {
                    let target = tenant_dir.join(&suffix);
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent)
                            .map_err(|error| format!("backup_object_restore_dir_failed:{error}"))?;
                    }
                    if target.exists() {
                        if sha256_file(&target)? != sha256_file(&path)? {
                            return Err("backup_object_restore_digest_conflict".to_string());
                        }
                        fs::remove_file(&path).map_err(|error| {
                            format!("backup_object_duplicate_cleanup_failed:{error}")
                        })?;
                    } else {
                        fs::rename(&path, &target)
                            .map_err(|error| format!("backup_object_restore_failed:{error}"))?;
                    }
                } else {
                    deleted_bytes += fs::metadata(&path)
                        .map(|metadata| metadata.len() as i64)
                        .unwrap_or(0);
                    fs::remove_file(&path)
                        .map_err(|error| format!("backup_object_delete_failed:{error}"))?;
                    deleted += 1;
                }
            }
            let _ = fs::remove_dir_all(entry.path());
        }
    }
    Ok(
        json!({ "ok": true, "quarantined": quarantined, "quarantinedBytes": quarantined_bytes, "deleted": deleted, "deletedBytes": deleted_bytes }),
    )
}

fn directory_size(path: &Path) -> i64 {
    if path.is_file() {
        return fs::metadata(path)
            .map(|metadata| metadata.len() as i64)
            .unwrap_or(0);
    }
    fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| directory_size(&entry.path()))
        .sum()
}

fn snapshot_fingerprint(root: &Path) -> Result<String, String> {
    fn collect(
        root: &Path,
        current: &Path,
        output: &mut Vec<(String, u64, String)>,
    ) -> Result<(), String> {
        let mut entries = fs::read_dir(current)
            .map_err(|error| format!("backup_cleanup_scan_failed:{error}"))?
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                collect(root, &path, output)?;
                continue;
            }
            if !path.is_file() {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "backup_cleanup_scan_scope_invalid".to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            let (size, digest) = sha256_file(&path)?;
            output.push((relative, size, digest));
        }
        Ok(())
    }
    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    let mut hasher = Sha256::new();
    for (relative, size, digest) in files {
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        hasher.update(size.to_string().as_bytes());
        hasher.update([0]);
        hasher.update(digest.as_bytes());
        hasher.update([b'\n']);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn legacy_quarantine_root(tenant_dir: &Path) -> PathBuf {
    tenant_dir.join(LEGACY_QUARANTINE_DIR)
}

fn legacy_quarantine_manifest_path(tenant_dir: &Path) -> PathBuf {
    legacy_quarantine_root(tenant_dir).join(LEGACY_QUARANTINE_MANIFEST)
}

fn legacy_records_digest(records: &[Value]) -> Result<String, String> {
    let raw = serde_json::to_vec(records)
        .map_err(|error| format!("backup_legacy_quarantine_manifest_encode_failed:{error}"))?;
    Ok(format!("{:x}", Sha256::digest(raw)))
}

fn save_legacy_quarantine_records(
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
    let manifest = json!({
        "version": 1,
        "updatedAtMs": now_ms,
        "records": records,
        "manifestDigest": legacy_records_digest(records)?
    });
    let raw = serde_json::to_string_pretty(&manifest)
        .map_err(|error| format!("backup_legacy_quarantine_manifest_encode_failed:{error}"))?;
    fs::write(&temporary, format!("{raw}\n"))
        .map_err(|error| format!("backup_legacy_quarantine_manifest_write_failed:{error}"))?;
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

fn load_legacy_quarantine_records(tenant_dir: &Path) -> Result<Vec<Value>, String> {
    let path = legacy_quarantine_manifest_path(tenant_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let manifest =
        json_file(&path).ok_or_else(|| "backup_legacy_quarantine_manifest_invalid".to_string())?;
    if manifest.get("version").and_then(Value::as_i64) != Some(1) {
        return Err("backup_legacy_quarantine_manifest_version_invalid".to_string());
    }
    let records = manifest
        .get("records")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| "backup_legacy_quarantine_records_invalid".to_string())?;
    let expected = legacy_records_digest(&records)?;
    if manifest.get("manifestDigest").and_then(Value::as_str) != Some(expected.as_str()) {
        return Err("backup_legacy_quarantine_manifest_digest_mismatch".to_string());
    }
    Ok(records)
}

fn legacy_record_paths(tenant_dir: &Path, record: &Value) -> Result<(PathBuf, PathBuf), String> {
    let snapshot_name = record
        .get("snapshotName")
        .and_then(Value::as_str)
        .filter(|name| {
            !name.is_empty()
                && Path::new(name).components().count() == 1
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

fn set_legacy_record_status(
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

fn reconcile_legacy_quarantine_records(
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
        let (next_status, reason) = match status.as_str() {
            "pending" if quarantined.exists() && !original.exists() => ("quarantined", None),
            "pending" if original.exists() && !quarantined.exists() => ("cancelled", None),
            "restoring" if original.exists() && !quarantined.exists() => ("restored", None),
            "restoring" if quarantined.exists() && !original.exists() => ("quarantined", None),
            "purging" if !quarantined.exists() && !original.exists() => ("purged", None),
            "purging" if quarantined.exists() && !original.exists() => ("quarantined", None),
            _ => ("review_required", Some("interrupted_state_conflict")),
        };
        record["status"] = json!(next_status);
        record["updatedAtMs"] = json!(now_ms);
        if let Some(reason) = reason {
            record["reviewReason"] = json!(reason);
        }
        changed = true;
    }
    changed
}

fn load_reconciled_legacy_quarantine_records(
    tenant_dir: &Path,
    now_ms: i64,
) -> Result<Vec<Value>, String> {
    let mut records = load_legacy_quarantine_records(tenant_dir)?;
    if reconcile_legacy_quarantine_records(tenant_dir, &mut records, now_ms) {
        save_legacy_quarantine_records(tenant_dir, &records, now_ms)?;
    }
    Ok(records)
}

pub(crate) fn scan_storage(tenant_dir: &Path) -> StorageScan {
    let mut scan = StorageScan::default();
    for (_, path) in object_files(&tenant_dir.join("objects/sha256"), "objects/sha256") {
        scan.object_count += 1;
        scan.object_bytes += fs::metadata(path)
            .map(|metadata| metadata.len() as i64)
            .unwrap_or(0);
    }
    for path in snapshot_manifests(tenant_dir, false) {
        let Some(manifest) = json_file(&path) else {
            continue;
        };
        let version = manifest.get("version").and_then(Value::as_i64).unwrap_or(0);
        if let Some(relative) = manifest.pointer("/db/relativePath").and_then(Value::as_str) {
            scan.database_history_bytes +=
                fs::metadata(path.parent().unwrap_or(Path::new(".")).join(relative))
                    .map(|metadata| metadata.len() as i64)
                    .unwrap_or(0);
        }
        if version < SNAPSHOT_VERSION {
            scan.legacy_snapshot_count += 1;
            scan.legacy_snapshot_bytes += path.parent().map(directory_size).unwrap_or(0);
        }
    }
    scan
}

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

pub(crate) fn legacy_cleanup_summary(
    tenant_dir: &Path,
    pinned_generations: &HashSet<i64>,
) -> Value {
    let candidates = legacy_cleanup_candidates(tenant_dir, pinned_generations);
    let reclaimable_bytes = candidates
        .iter()
        .map(|(manifest, _, _)| manifest.parent().map(directory_size).unwrap_or(0))
        .sum::<i64>();
    json!({
        "ok": true,
        "candidateCount": candidates.len(),
        "reclaimableBytes": reclaimable_bytes
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kst_retention_keeps_recent_daily_monthly_and_five_pre_restore() {
        let root = std::env::temp_dir().join(format!(
            "backup-v5-retention-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let snapshots = root.join("snapshots");
        fs::create_dir_all(&snapshots).expect("create snapshots");
        let now = DateTime::parse_from_rfc3339("2026-08-27T12:00:00+09:00")
            .unwrap()
            .timestamp_millis();
        for index in 0..45 {
            let created = now - index * 86_400_000;
            let dir = snapshots.join(format!("auto-{index:02}"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("manifest.json"),
                json!({"version":5,"kind":"auto_sync","createdAtMs":created}).to_string(),
            )
            .unwrap();
        }
        for index in 0..8 {
            let dir = snapshots.join(format!("pre-{index:02}"));
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("manifest.json"),
                json!({"version":5,"kind":"pre_restore","createdAtMs":now-index}).to_string(),
            )
            .unwrap();
        }
        let manual = snapshots.join("manual");
        fs::create_dir_all(&manual).unwrap();
        fs::write(
            manual.join("manifest.json"),
            json!({"version":5,"kind":"manual","createdAtMs":1}).to_string(),
        )
        .unwrap();
        prune_snapshots(&root, now).expect("prune snapshots");
        assert!(manual.exists());
        assert_eq!(
            fs::read_dir(&snapshots)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with("pre-"))
                .count(),
            5
        );
        assert!(snapshots.join("auto-00").exists());
        assert!(snapshots.join("auto-29").exists());
        assert!(!snapshots.join("auto-44").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn content_objects_are_reused_and_digest_conflicts_fail_closed() {
        let root = std::env::temp_dir().join(format!(
            "backup-v5-object-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.bin");
        fs::write(&source, b"same attachment").unwrap();
        let first = put_object(&root, &source, "one").expect("first object");
        let second = put_object(&root, &source, "two").expect("reuse object");
        assert!(first.created);
        assert!(!second.created);
        assert_eq!(first.artifact.relative_path, second.artifact.relative_path);
        fs::write(root.join(&first.artifact.relative_path), b"tampered").unwrap();
        assert_eq!(
            put_object(&root, &source, "three").unwrap_err(),
            "backup_object_digest_conflict"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_cleanup_rejects_same_size_file_changes_after_preview() {
        let root = std::env::temp_dir().join(format!(
            "backup-v5-cleanup-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let older = root.join("snapshots/older");
        let newest = root.join("snapshots/newest");
        fs::create_dir_all(&older).unwrap();
        fs::create_dir_all(&newest).unwrap();
        fs::write(
            older.join("manifest.json"),
            json!({"version":4,"kind":"auto_sync","createdAtMs":1}).to_string(),
        )
        .unwrap();
        fs::write(older.join("attachment.bin"), b"before").unwrap();
        fs::write(
            newest.join("manifest.json"),
            json!({"version":4,"kind":"auto_sync","createdAtMs":2}).to_string(),
        )
        .unwrap();
        let pinned = HashSet::new();
        let preview = legacy_cleanup_preview(&root, &pinned);
        assert_eq!(preview["candidateCount"], 1);
        fs::write(older.join("attachment.bin"), b"after!").unwrap();
        assert_eq!(
            apply_legacy_cleanup(
                &root,
                &pinned,
                preview["previewToken"].as_str().unwrap(),
                10,
                10,
            )
            .unwrap_err(),
            "backup_legacy_cleanup_preview_changed"
        );
        assert!(older.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_snapshots_are_quarantined_for_thirty_days_and_can_be_restored() {
        let root = std::env::temp_dir().join(format!(
            "backup-v5-legacy-quarantine-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        for (name, created) in [("older", 1), ("newest", 2)] {
            let snapshot = root.join("snapshots").join(name);
            fs::create_dir_all(&snapshot).unwrap();
            fs::write(
                snapshot.join("manifest.json"),
                json!({"version":4,"kind":"auto_sync","createdAtMs":created}).to_string(),
            )
            .unwrap();
            fs::write(snapshot.join("attachment.bin"), name.as_bytes()).unwrap();
        }
        let pinned = HashSet::new();
        let quarantined = quarantine_legacy_snapshots(&root, &pinned, 10, 100).unwrap();
        assert_eq!(quarantined["quarantined"], 1);
        assert!(!root.join("snapshots/older").exists());
        assert!(root.join("snapshots/newest").exists());
        let summary = legacy_quarantine_summary(&root, 100).unwrap();
        assert_eq!(summary["quarantinedCount"], 1);
        assert_eq!(summary["purgeAfterMs"], 100 + QUARANTINE_DAYS * 86_400_000);

        let restored = undo_legacy_quarantine(&root, 200).unwrap();
        assert_eq!(restored["restored"], 1);
        assert!(root.join("snapshots/older").exists());
        assert_eq!(
            quarantine_legacy_snapshots(&root, &pinned, 10, 300).unwrap()["quarantined"],
            0
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_quarantine_purge_revalidates_fingerprint_and_marks_review() {
        let root = std::env::temp_dir().join(format!(
            "backup-v5-legacy-review-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        for (name, created) in [("oldest", 1), ("older", 2), ("newest", 3)] {
            let snapshot = root.join("snapshots").join(name);
            fs::create_dir_all(&snapshot).unwrap();
            fs::write(
                snapshot.join("manifest.json"),
                json!({"version":4,"kind":"auto_sync","createdAtMs":created}).to_string(),
            )
            .unwrap();
            fs::write(snapshot.join("attachment.bin"), name.as_bytes()).unwrap();
        }
        let pinned = HashSet::new();
        assert_eq!(
            quarantine_legacy_snapshots(&root, &pinned, 10, 100).unwrap()["quarantined"],
            2
        );
        let records = load_legacy_quarantine_records(&root).unwrap();
        let changed = records
            .iter()
            .find(|record| record["snapshotName"] == "oldest")
            .unwrap();
        let (_, changed_path) = legacy_record_paths(&root, changed).unwrap();
        fs::write(changed_path.join("attachment.bin"), b"changed").unwrap();

        let purge_at = 100 + QUARANTINE_DAYS * 86_400_000;
        let purged = purge_legacy_quarantine(&root, &pinned, 10, purge_at).unwrap();
        assert_eq!(purged["purged"], 1);
        assert_eq!(purged["reviewCount"], 1);
        assert!(changed_path.exists());
        let summary = legacy_quarantine_summary(&root, purge_at).unwrap();
        assert_eq!(summary["quarantinedCount"], 0);
        assert_eq!(summary["reviewCount"], 1);
        fs::remove_dir_all(root).unwrap();
    }
}
