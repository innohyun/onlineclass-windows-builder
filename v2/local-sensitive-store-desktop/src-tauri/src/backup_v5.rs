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

#[path = "backup_v5_journal.rs"]
mod journal;
#[path = "backup_v5_legacy.rs"]
mod legacy;
#[path = "backup_v5_retention.rs"]
mod retention;
#[path = "backup_v5_storage.rs"]
mod storage;

pub(crate) use legacy::{
    apply_legacy_cleanup, legacy_cleanup_preview,
    legacy_cleanup_summary_from_scan, legacy_quarantine_summary, maintain_legacy_quarantine,
    purge_legacy_quarantine, quarantine_legacy_snapshots, undo_legacy_quarantine,
};
pub(crate) use retention::prune_snapshots;
pub(crate) use storage::{scan_storage, StorageScan};

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

fn manifest_object_references(tenant_dir: &Path) -> Result<HashSet<String>, String> {
    let mut references = HashSet::new();
    let metadata = fs::symlink_metadata(tenant_dir.join("snapshots"))
        .map_err(|_| "backup_object_reference_scan_incomplete".to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("backup_object_reference_scan_symlink".to_string());
    }
    let entries = fs::read_dir(tenant_dir.join("snapshots"))
        .map_err(|_| "backup_object_reference_scan_incomplete".to_string())?;
    for entry in entries {
        let entry = entry.map_err(|_| "backup_object_reference_scan_incomplete".to_string())?;
        let file_type = entry
            .file_type()
            .map_err(|_| "backup_object_reference_scan_incomplete".to_string())?;
        if file_type.is_symlink() {
            return Err("backup_object_reference_scan_symlink".to_string());
        }
        if !file_type.is_dir() {
            continue;
        }
        if entry.file_name().to_string_lossy().ends_with(".staging") {
            return Err("backup_object_reference_staging_incomplete".to_string());
        }
        let path = entry.path().join("manifest.json");
        let manifest = json_file(&path)
            .ok_or_else(|| "backup_object_reference_manifest_incomplete".to_string())?;
        let version = manifest.get("version").and_then(Value::as_i64).unwrap_or(0);
        if matches!(version, 2 | 3 | 4) {
            continue;
        }
        if version != SNAPSHOT_VERSION || manifest.get("ok").and_then(Value::as_bool) != Some(true)
        {
            return Err("backup_object_reference_manifest_invalid".to_string());
        }
        let tenant_id = manifest
            .get("tenantId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "backup_object_reference_tenant_invalid".to_string())?;
        let records = manifest
            .get("artifacts")
            .and_then(Value::as_array)
            .ok_or_else(|| "backup_object_reference_artifacts_incomplete".to_string())?;
        let mut artifacts = Vec::new();
        let mut paths = HashSet::new();
        for record in records {
            let relative = record
                .get("relativePath")
                .and_then(Value::as_str)
                .ok_or_else(|| "backup_object_reference_path_invalid".to_string())?;
            crate::backup::safe_relative_path(relative)
                .ok_or_else(|| "backup_object_reference_path_invalid".to_string())?;
            if !paths.insert(relative.to_string()) {
                return Err("backup_object_reference_path_duplicate".to_string());
            }
            let sha256 = record
                .get("sha256")
                .and_then(Value::as_str)
                .ok_or_else(|| "backup_object_reference_digest_invalid".to_string())?;
            let size = record
                .get("size")
                .and_then(Value::as_u64)
                .ok_or_else(|| "backup_object_reference_size_invalid".to_string())?;
            if relative.starts_with("objects/") {
                if relative != safe_object_relative_path(sha256)? {
                    return Err("backup_object_reference_path_invalid".to_string());
                }
                references.insert(relative.to_string());
            } else {
                let artifact_path = entry.path().join(relative);
                let metadata = fs::symlink_metadata(&artifact_path)
                    .map_err(|_| "backup_object_reference_artifact_incomplete".to_string())?;
                if !metadata.is_file() || metadata.len() != size {
                    return Err("backup_object_reference_artifact_incomplete".to_string());
                }
            }
            artifacts.push(ArtifactDigest {
                relative_path: relative.to_string(),
                size,
                sha256: sha256.to_string(),
            });
        }
        let root = crate::backup::artifact_set_sha256(&mut artifacts);
        if manifest.get("artifactSetSha256").and_then(Value::as_str) != Some(root.as_str()) {
            return Err("backup_object_reference_root_mismatch".to_string());
        }
        let commit = json_file(&entry.path().join("commit.json"))
            .ok_or_else(|| "backup_object_reference_commit_incomplete".to_string())?;
        if commit.get("tenantId").and_then(Value::as_str) != Some(tenant_id)
            || commit.get("generation") != manifest.get("generation")
            || commit.get("artifactSetSha256").and_then(Value::as_str) != Some(root.as_str())
        {
            return Err("backup_object_reference_commit_mismatch".to_string());
        }
        let index = artifacts
            .iter()
            .find(|artifact| artifact.relative_path == crate::backup_v4::APPLY_INDEX_RELATIVE_PATH)
            .ok_or_else(|| "backup_object_reference_index_incomplete".to_string())?;
        if sha256_file(&entry.path().join(&index.relative_path))?
            != (index.size, index.sha256.clone())
        {
            return Err("backup_object_reference_index_mismatch".to_string());
        }
        let authoritative = crate::backup_v4::projection(&path, &manifest)?;
        for (item, path_key) in [
            (&manifest["applyIndex"], "relativePath"),
            (&authoritative["db"], "relativePath"),
        ] {
            if !artifacts.iter().any(|artifact| {
                item[path_key].as_str() == Some(artifact.relative_path.as_str())
                    && item["size"].as_u64() == Some(artifact.size)
                    && item["sha256"].as_str() == Some(artifact.sha256.as_str())
            }) {
                return Err("backup_object_reference_index_mismatch".to_string());
            }
        }
        for group in ["media", "workNoteAttachments"] {
            let records = authoritative[group]["records"]
                .as_array()
                .ok_or_else(|| "backup_object_reference_index_incomplete".to_string())?;
            for record in records {
                if !matches!(record["status"].as_str(), Some("copied" | "skipped"))
                    || !artifacts.iter().any(|artifact| {
                        record["backupRelativePath"].as_str()
                            == Some(artifact.relative_path.as_str())
                            && record["size"].as_u64() == Some(artifact.size)
                            && record["sha256"].as_str() == Some(artifact.sha256.as_str())
                    })
                {
                    return Err("backup_object_reference_index_mismatch".to_string());
                }
            }
        }
    }
    Ok(references)
}

fn object_files(root: &Path, prefix: &str) -> Result<Vec<(String, PathBuf)>, String> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err("backup_object_inventory_path_invalid".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("backup_object_inventory_incomplete".to_string()),
    }
    let prefixes = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("backup_object_inventory_incomplete".to_string()),
    };
    let mut files = Vec::new();
    for directory in prefixes {
        let directory = directory.map_err(|_| "backup_object_inventory_incomplete".to_string())?;
        if !directory
            .file_type()
            .map_err(|_| "backup_object_inventory_incomplete".to_string())?
            .is_dir()
        {
            return Err("backup_object_inventory_path_invalid".to_string());
        }
        let bucket = directory.file_name().to_string_lossy().to_string();
        let entries = fs::read_dir(directory.path())
            .map_err(|_| "backup_object_inventory_incomplete".to_string())?;
        for entry in entries {
            let entry = entry.map_err(|_| "backup_object_inventory_incomplete".to_string())?;
            let hash = entry.file_name().to_string_lossy().to_string();
            if !entry
                .file_type()
                .map_err(|_| "backup_object_inventory_incomplete".to_string())?
                .is_file()
                || hash.len() != 64
                || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                || bucket != hash[..2]
            {
                return Err("backup_object_inventory_path_invalid".to_string());
            }
            files.push((format!("{prefix}/{bucket}/{hash}"), entry.path()));
        }
    }
    Ok(files)
}

pub(crate) fn quarantine_unreferenced_objects(
    tenant_dir: &Path,
    current_references: &HashSet<String>,
    now_ms: i64,
) -> Result<Value, String> {
    let mut referenced = manifest_object_references(tenant_dir)?;
    referenced.extend(current_references.iter().cloned());
    let object_root = tenant_dir.join("objects/sha256");
    let quarantine_root = tenant_dir.join("objects-quarantine");
    match fs::symlink_metadata(&quarantine_root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err("backup_object_quarantine_path_invalid".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("backup_object_quarantine_inventory_incomplete".to_string()),
    }
    let live_objects = object_files(&object_root, "objects/sha256")?;
    let mut quarantines = Vec::new();
    match fs::read_dir(&quarantine_root) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry
                    .map_err(|_| "backup_object_quarantine_inventory_incomplete".to_string())?;
                if !entry
                    .file_type()
                    .map_err(|_| "backup_object_quarantine_inventory_incomplete".to_string())?
                    .is_dir()
                {
                    return Err("backup_object_quarantine_path_invalid".to_string());
                }
                let quarantined_at = entry
                    .file_name()
                    .to_string_lossy()
                    .parse::<i64>()
                    .map_err(|_| "backup_object_quarantine_time_invalid".to_string())?;
                let files = object_files(&entry.path(), "objects/sha256")?;
                quarantines.push((quarantined_at, files));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("backup_object_quarantine_inventory_incomplete".to_string()),
    }
    let mut restored = 0i64;
    let mut quarantined = 0i64;
    let mut quarantined_bytes = 0i64;
    let mut deleted = 0i64;
    let mut deleted_bytes = 0i64;
    // Reintroduced references are repaired immediately, including during the undo grace period.
    for (_, files) in &quarantines {
        for (relative, path) in files {
            if !referenced.contains(relative) {
                continue;
            }
            let target = tenant_dir.join(relative);
            let expected_hash = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            let digest = sha256_file(path)?;
            if digest.1 != expected_hash {
                return Err("backup_object_restore_digest_conflict".to_string());
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("backup_object_restore_dir_failed:{error}"))?;
            }
            if target.exists() {
                if sha256_file(&target)? != digest {
                    return Err("backup_object_restore_digest_conflict".to_string());
                }
                fs::remove_file(path)
                    .map_err(|error| format!("backup_object_duplicate_cleanup_failed:{error}"))?;
            } else {
                fs::rename(path, target)
                    .map_err(|error| format!("backup_object_restore_failed:{error}"))?;
            }
            restored += 1;
        }
    }
    referenced.extend(manifest_object_references(tenant_dir)?);
    for (relative, path) in live_objects {
        if referenced.contains(&relative) {
            continue;
        }
        let size = fs::metadata(&path)
            .map_err(|_| "backup_object_inventory_changed".to_string())?
            .len() as i64;
        let suffix = relative
            .strip_prefix("objects/sha256/")
            .ok_or_else(|| "backup_object_path_invalid".to_string())?;
        let target = quarantine_root.join(now_ms.to_string()).join(suffix);
        if target.exists() {
            return Err("backup_object_quarantine_target_exists".to_string());
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("backup_object_quarantine_dir_failed:{error}"))?;
        }
        fs::rename(path, target)
            .map_err(|error| format!("backup_object_quarantine_failed:{error}"))?;
        quarantined += 1;
        quarantined_bytes += size;
    }
    referenced.extend(manifest_object_references(tenant_dir)?);
    for (quarantined_at, files) in quarantines {
        if now_ms.saturating_sub(quarantined_at) < QUARANTINE_DAYS * 86_400_000 {
            continue;
        }
        for (relative, path) in files {
            if referenced.contains(&relative) || !path.exists() {
                continue;
            }
            deleted_bytes += fs::metadata(&path)
                .map_err(|_| "backup_object_inventory_changed".to_string())?
                .len() as i64;
            fs::remove_file(&path)
                .map_err(|error| format!("backup_object_delete_failed:{error}"))?;
            deleted += 1;
        }
        // Never recursively delete a quarantine directory: OneDrive may have added an unseen file.
    }
    Ok(json!({
        "ok": true, "restored": restored,
        "quarantined": quarantined, "quarantinedBytes": quarantined_bytes,
        "deleted": deleted, "deletedBytes": deleted_bytes
    }))
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
    let metadata = fs::symlink_metadata(root)
        .map_err(|error| format!("backup_cleanup_scan_failed:{error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("backup_cleanup_scan_scope_invalid".to_string());
    }
    fn collect(
        root: &Path,
        current: &Path,
        output: &mut Vec<(String, u64, String)>,
    ) -> Result<(), String> {
        let mut entries = fs::read_dir(current)
            .map_err(|error| format!("backup_cleanup_scan_failed:{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("backup_cleanup_scan_failed:{error}"))?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| format!("backup_cleanup_scan_failed:{error}"))?;
            if file_type.is_symlink() {
                return Err("backup_cleanup_scan_symlink".to_string());
            }
            if file_type.is_dir() {
                collect(root, &path, output)?;
                continue;
            }
            if !file_type.is_file() {
                return Err("backup_cleanup_scan_file_type_invalid".to_string());
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

#[cfg(test)]
#[path = "backup_v5_tests.rs"]
mod tests;
