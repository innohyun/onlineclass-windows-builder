use crate::shared_archive_sync;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const APPLY_INDEX_RELATIVE_PATH: &str = "meta/apply-index.json";
const APPLY_INDEX_SCHEMA: &str = "classaimate-device-sync-apply-index-v1";

fn read_json(path: &Path) -> Result<Value, String> {
    let raw = fs::read(path).map_err(|e| format!("backup_apply_index_read_failed:{e}"))?;
    serde_json::from_slice(&raw).map_err(|e| format!("backup_apply_index_decode_failed:{e}"))
}

fn snapshot_dir(manifest_path: &Path) -> Result<&Path, String> {
    manifest_path
        .parent()
        .ok_or_else(|| "backup_manifest_parent_missing".to_string())
}

pub(crate) fn tenant_dir(manifest_path: &Path) -> Result<PathBuf, String> {
    let snapshot = snapshot_dir(manifest_path)?;
    let snapshots = snapshot
        .parent()
        .ok_or_else(|| "backup_snapshots_parent_missing".to_string())?;
    if snapshots.file_name().and_then(|value| value.to_str()) != Some("snapshots") {
        return Err("backup_snapshot_layout_invalid".to_string());
    }
    snapshots
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "backup_tenant_parent_missing".to_string())
}

pub(crate) fn write_apply_index(
    staging_dir: &Path,
    tenant_id: &str,
    generation: Option<i64>,
    database: Value,
    sync: Value,
    media: Value,
    work_note_attachments: Value,
    archives: Value,
    counts: Value,
) -> Result<(Value, u64, String), String> {
    let index = json!({
        "schemaVersion": APPLY_INDEX_SCHEMA,
        "tenantId": tenant_id,
        "generation": generation,
        "database": database,
        "sync": sync,
        "media": media,
        "workNoteAttachments": work_note_attachments,
        "archives": archives,
        "counts": counts,
    });
    let raw = serde_json::to_vec_pretty(&index)
        .map_err(|e| format!("backup_apply_index_encode_failed:{e}"))?;
    let target = staging_dir.join(APPLY_INDEX_RELATIVE_PATH);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("backup_apply_index_dir_failed:{e}"))?;
    }
    fs::write(&target, &raw).map_err(|e| format!("backup_apply_index_write_failed:{e}"))?;
    let (size, sha256) = super::backup::sha256_file(&target)?;
    Ok((index, size, sha256))
}

pub(crate) fn projection(manifest_path: &Path, manifest: &Value) -> Result<Value, String> {
    let version = manifest.get("version").and_then(Value::as_i64).unwrap_or(0);
    if !matches!(version, 4 | 5) {
        return Ok(manifest.clone());
    }
    let relative = manifest
        .get("applyIndex")
        .and_then(|value| value.get("relativePath"))
        .and_then(Value::as_str)
        .ok_or_else(|| "backup_apply_index_required".to_string())?;
    if relative != APPLY_INDEX_RELATIVE_PATH {
        return Err("backup_apply_index_path_invalid".to_string());
    }
    let index = read_json(&snapshot_dir(manifest_path)?.join(APPLY_INDEX_RELATIVE_PATH))?;
    if index.get("schemaVersion").and_then(Value::as_str) != Some(APPLY_INDEX_SCHEMA)
        || index.get("tenantId") != manifest.get("tenantId")
        || index.get("generation") != manifest.get("generation")
    {
        return Err("backup_apply_index_manifest_mismatch".to_string());
    }
    Ok(json!({
        "version": version,
        "tenantId": index.get("tenantId").cloned().unwrap_or(Value::Null),
        "generation": index.get("generation").cloned().unwrap_or(Value::Null),
        "db": index.get("database").cloned().unwrap_or(Value::Null),
        "sync": index.get("sync").cloned().unwrap_or(Value::Null),
        "media": index.get("media").cloned().unwrap_or_else(|| json!({})),
        "workNoteAttachments": index.get("workNoteAttachments").cloned().unwrap_or_else(|| json!({})),
        "archives": index.get("archives").cloned().unwrap_or_else(|| json!({"count":0,"records":[]})),
        "counts": index.get("counts").cloned().unwrap_or_else(|| json!({})),
    }))
}

fn artifact_map(manifest: &Value) -> Result<HashMap<String, (u64, String)>, String> {
    let artifacts = manifest
        .get("artifacts")
        .and_then(Value::as_array)
        .ok_or_else(|| "backup_artifacts_required".to_string())?;
    let mut output = HashMap::new();
    for artifact in artifacts {
        let relative = artifact
            .get("relativePath")
            .and_then(Value::as_str)
            .ok_or_else(|| "backup_artifact_path_required".to_string())?;
        if output
            .insert(
                relative.to_string(),
                (
                    artifact
                        .get("size")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| "backup_artifact_size_required".to_string())?,
                    artifact
                        .get("sha256")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "backup_artifact_hash_required".to_string())?
                        .to_string(),
                ),
            )
            .is_some()
        {
            return Err("backup_artifact_path_duplicate".to_string());
        }
    }
    Ok(output)
}

fn require_artifact(
    artifacts: &HashMap<String, (u64, String)>,
    item: &Value,
    path_key: &str,
) -> Result<(), String> {
    let relative = item
        .get(path_key)
        .and_then(Value::as_str)
        .ok_or_else(|| "backup_apply_artifact_path_required".to_string())?;
    super::backup::safe_relative_path(relative)
        .ok_or_else(|| "backup_apply_artifact_path_invalid".to_string())?;
    let expected = (
        item.get("size")
            .and_then(Value::as_u64)
            .ok_or_else(|| "backup_apply_artifact_size_required".to_string())?,
        item.get("sha256")
            .and_then(Value::as_str)
            .ok_or_else(|| "backup_apply_artifact_hash_required".to_string())?
            .to_string(),
    );
    if artifacts.get(relative) != Some(&expected) {
        return Err("backup_apply_artifact_mismatch".to_string());
    }
    Ok(())
}

fn verify_mapped_files(
    artifacts: &HashMap<String, (u64, String)>,
    records: &Value,
    allow_shared_paths: bool,
) -> Result<(), String> {
    let records = records
        .get("records")
        .and_then(Value::as_array)
        .ok_or_else(|| "backup_apply_records_required".to_string())?;
    let mut paths = HashSet::new();
    for record in records {
        let status = record.get("status").and_then(Value::as_str).unwrap_or("");
        if !matches!(status, "copied" | "skipped") {
            continue;
        }
        let relative = record
            .get("backupRelativePath")
            .and_then(Value::as_str)
            .ok_or_else(|| "backup_apply_artifact_path_required".to_string())?;
        if !allow_shared_paths && !paths.insert(relative.to_string()) {
            return Err("backup_apply_artifact_path_duplicate".to_string());
        }
        require_artifact(artifacts, record, "backupRelativePath")?;
    }
    Ok(())
}

pub(crate) fn verify_authoritative_index(
    manifest_path: &Path,
    manifest: &Value,
    tenant_id: &str,
    generation: Option<i64>,
) -> Result<Value, String> {
    let artifacts = artifact_map(manifest)?;
    let apply_index = manifest
        .get("applyIndex")
        .ok_or_else(|| "backup_apply_index_required".to_string())?;
    require_artifact(&artifacts, apply_index, "relativePath")?;
    if apply_index.get("relativePath").and_then(Value::as_str) != Some(APPLY_INDEX_RELATIVE_PATH) {
        return Err("backup_apply_index_path_invalid".to_string());
    }
    let authoritative = projection(manifest_path, manifest)?;
    if authoritative.get("tenantId").and_then(Value::as_str) != Some(tenant_id)
        || authoritative.get("generation").and_then(Value::as_i64) != generation
    {
        return Err("backup_apply_index_checkpoint_mismatch".to_string());
    }
    require_artifact(&artifacts, &authoritative["db"], "relativePath")?;
    let allow_shared_paths = manifest.get("version").and_then(Value::as_i64) == Some(5);
    verify_mapped_files(&artifacts, &authoritative["media"], allow_shared_paths)?;
    verify_mapped_files(&artifacts, &authoritative["workNoteAttachments"], allow_shared_paths)?;
    if generation.is_some() {
        let sync = authoritative
            .get("sync")
            .filter(|value| value.is_object())
            .ok_or_else(|| "backup_sync_records_required".to_string())?;
        let content_sha256 = sync
            .get("contentSha256")
            .and_then(Value::as_str)
            .unwrap_or("");
        if content_sha256.len() != 64
            || !content_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("backup_sync_content_hash_invalid".to_string());
        }
        sync.get("records")
            .and_then(Value::as_array)
            .ok_or_else(|| "backup_sync_records_required".to_string())?;
    }
    shared_archive_sync::verify_snapshot_bundles(
        tenant_id,
        &tenant_dir(manifest_path)?,
        &authoritative["archives"],
    )?;
    Ok(authoritative)
}

pub(crate) fn apply_archives(
    manifest_path: &Path,
    authoritative: &Value,
    tenant_id: &str,
) -> Result<Value, String> {
    if !matches!(authoritative.get("version").and_then(Value::as_i64), Some(4) | Some(5)) {
        return Ok(json!({
            "ok": true,
            "archiveImported": 0,
            "archiveRepairedFiles": 0,
            "archiveUnchanged": 0,
        }));
    }
    crate::shared_archive_apply::apply_snapshot_bundles(
        tenant_id,
        &tenant_dir(manifest_path)?,
        &authoritative["archives"],
    )
}
