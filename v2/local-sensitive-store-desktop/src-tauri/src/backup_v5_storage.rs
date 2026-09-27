use super::*;
use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub(crate) struct StorageScan {
    pub(crate) object_count: i64,
    pub(crate) object_bytes: i64,
    pub(crate) database_history_bytes: i64,
    pub(crate) legacy_snapshot_count: i64,
    pub(crate) legacy_snapshot_bytes: i64,
    pub(crate) manual_snapshot_count: i64,
    pub(crate) manual_snapshot_bytes: i64,
    pub(crate) storage_breakdown: Value,
    pub(crate) total_logical_bytes: i64,
    pub(crate) scan_complete: bool,
    pub(crate) scanned_at_ms: i64,
    pub(crate) errors: Vec<String>,
    pub(crate) snapshot_bytes: HashMap<PathBuf, i64>,
    pub(crate) latest_snapshot_version: Option<i64>,
    pub(crate) other_entries: Vec<Value>,
    pub(crate) staging_entries: Vec<Value>,
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<(PathBuf, i64)>,
    errors: &mut Vec<String>,
) {
    let relative = directory.strip_prefix(root).unwrap_or(Path::new("."));
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            errors.push(format!(
                "storage_directory_read_failed:{}:{:?}",
                relative.display(),
                error.kind()
            ));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(format!(
                    "storage_directory_entry_failed:{}:{:?}",
                    relative.display(),
                    error.kind()
                ));
                continue;
            }
        };
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(&path);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                errors.push(format!(
                    "storage_symlink_not_followed:{}",
                    relative.display()
                ));
            }
            Ok(metadata) if metadata.is_dir() => collect_files(root, &path, files, errors),
            Ok(metadata) if metadata.is_file() => files.push((
                relative.to_path_buf(),
                metadata.len().min(i64::MAX as u64) as i64,
            )),
            Ok(_) => errors.push(format!(
                "storage_file_type_unsupported:{}",
                relative.display()
            )),
            Err(error) => errors.push(format!(
                "storage_metadata_failed:{}:{:?}",
                relative.display(),
                error.kind()
            )),
        }
    }
}

pub(crate) fn scan_storage(tenant_dir: &Path) -> StorageScan {
    let mut scan = StorageScan {
        scan_complete: true,
        scanned_at_ms: Utc::now().timestamp_millis(),
        storage_breakdown: json!({
            "v5DatabaseBytes": 0, "v5MetadataBytes": 0, "legacySnapshotBytes": 0,
            "objectBytes": 0, "objectQuarantineBytes": 0, "legacyQuarantineBytes": 0,
            "archiveBundleBytes": 0, "stagingBytes": 0, "otherBytes": 0
        }),
        ..StorageScan::default()
    };
    let mut files = Vec::new();
    collect_files(tenant_dir, tenant_dir, &mut files, &mut scan.errors);
    let mut snapshots = HashMap::<String, (i64, PathBuf, String)>::new();
    let mut latest_created = 0;
    for (relative, _) in &files {
        let parts = relative
            .iter()
            .map(|part| part.to_string_lossy())
            .collect::<Vec<_>>();
        if parts.len() != 3
            || parts[0] != "snapshots"
            || parts[2] != "manifest.json"
            || parts[1].ends_with(".staging")
        {
            continue;
        }
        let manifest_path = tenant_dir.join(relative);
        let Some(manifest) = json_file(&manifest_path) else {
            scan.errors.push(format!(
                "storage_snapshot_manifest_invalid:{}",
                relative.display()
            ));
            continue;
        };
        let version = manifest.get("version").and_then(Value::as_i64).unwrap_or(0);
        let created = manifest["createdAtMs"].as_i64().unwrap_or(0);
        if created > latest_created {
            latest_created = created;
            scan.latest_snapshot_version = Some(version);
        }
        let database = manifest
            .pointer("/db/relativePath")
            .and_then(Value::as_str)
            .and_then(crate::backup::safe_relative_path);
        if !matches!(version, 2 | 3 | 4 | 5) || database.is_none() {
            scan.errors.push(format!(
                "storage_snapshot_metadata_invalid:{}",
                relative.display()
            ));
            continue;
        }
        if version < SNAPSHOT_VERSION {
            scan.legacy_snapshot_count += 1;
        }
        let kind = manifest
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("legacy")
            .to_string();
        if matches!(version,4|5) && kind == "manual" {
            scan.manual_snapshot_count += 1;
        }
        snapshots.insert(parts[1].to_string(), (version, database.unwrap(), kind));
    }
    let mut missing_databases = snapshots.keys().cloned().collect::<HashSet<_>>();
    let mut staging = HashMap::<String, i64>::new();
    for (relative, size) in files {
        let parts = relative
            .iter()
            .map(|part| part.to_string_lossy())
            .collect::<Vec<_>>();
        let mut category = "otherBytes";
        if parts.len() >= 3 && parts[0] == "snapshots" && parts[1].ends_with(".staging") {
            *scan.snapshot_bytes.entry(tenant_dir.join("snapshots").join(parts[1].as_ref())).or_default() += size;
        }
        if parts.iter().any(|part| part.ends_with(".staging")) {
            category = "stagingBytes";
        } else if parts.first().is_some_and(|part| part == "objects") {
            category = "objectBytes";
            if parts.len() == 4
                && parts[1] == "sha256"
                && parts[3].len() == 64
                && parts[3].bytes().all(|byte| byte.is_ascii_hexdigit())
                && parts[2] == parts[3][..2]
            {
                scan.object_count += 1;
                scan.object_bytes = scan.object_bytes.saturating_add(size);
            } else {
                scan.errors.push(format!(
                    "storage_object_path_invalid:{}",
                    relative.display()
                ));
            }
        } else if parts
            .first()
            .is_some_and(|part| part == "objects-quarantine")
        {
            category = "objectQuarantineBytes";
        } else if parts
            .first()
            .is_some_and(|part| part == LEGACY_QUARANTINE_DIR)
        {
            category = "legacyQuarantineBytes";
        } else if parts.first().is_some_and(|part| part == "archive-bundles") {
            category = "archiveBundleBytes";
        } else if parts.len() >= 3 && parts[0] == "snapshots" {
            let snapshot_dir = tenant_dir.join("snapshots").join(parts[1].as_ref());
            let bytes = scan.snapshot_bytes.entry(snapshot_dir).or_default();
            *bytes = bytes.saturating_add(size);
            if let Some((version, database, kind)) = snapshots.get(parts[1].as_ref()) {
                let is_database = relative
                    == Path::new("snapshots")
                        .join(parts[1].as_ref())
                        .join(database);
                if is_database {
                    scan.database_history_bytes = scan.database_history_bytes.saturating_add(size);
                    missing_databases.remove(parts[1].as_ref());
                }
                if *version < SNAPSHOT_VERSION {
                    category = "legacySnapshotBytes";
                    scan.legacy_snapshot_bytes = scan.legacy_snapshot_bytes.saturating_add(size);
                    if *version == 4 && kind == "manual" {scan.manual_snapshot_bytes=scan.manual_snapshot_bytes.saturating_add(size);}
                } else {
                    if kind == "manual" {
                        scan.manual_snapshot_bytes =
                            scan.manual_snapshot_bytes.saturating_add(size);
                    }
                    category = if is_database {
                        "v5DatabaseBytes"
                    } else {
                        "v5MetadataBytes"
                    };
                }
            }
        }
        scan.total_logical_bytes = scan.total_logical_bytes.saturating_add(size);
        let previous = scan.storage_breakdown[category].as_i64().unwrap_or(0);
        scan.storage_breakdown[category] = json!(previous.saturating_add(size));
        if category == "otherBytes" {
            scan.other_entries.push(json!({"relativePath":relative.to_string_lossy(),"bytes":size,
                "type":if parts.first().is_some_and(|part| part=="db") {"legacy_layout_database"}
                    else if parts.len()==1 && parts[0].starts_with("manifest-") {"legacy_layout_manifest"}
                    else if parts.first().is_some_and(|part| part=="OnlineClassLocalBackups") {"nested_backup_namespace"}
                    else {"unclassified"},"action":"review"}));
        }
        if category == "stagingBytes" {
            let end = parts.iter().position(|part| part.ends_with(".staging")).unwrap_or(0);
            *staging.entry(parts[..=end].iter().map(|part| part.as_ref()).collect::<Vec<_>>().join("/")).or_default() += size;
        }
    }
    for name in missing_databases {
        scan.errors.push(format!(
            "storage_snapshot_database_missing:snapshots/{name}"
        ));
    }
    scan.errors.sort();
    scan.errors.dedup();
    scan.scan_complete = scan.errors.is_empty();
    scan.other_entries.sort_by(|a,b| b["bytes"].as_i64().cmp(&a["bytes"].as_i64()));
    scan.staging_entries = staging.into_iter().map(|(path,bytes)| {
        let owner = json_file(&tenant_dir.join(&path).join("operation.json"));
        json!({"relativePath":path,"bytes":bytes,"bytesComplete":scan.scan_complete,"owner":owner,
            "state":"activity_unconfirmed","action":"review","reason":"cross_device_activity_must_be_confirmed"})
    }).collect();
    scan
}
