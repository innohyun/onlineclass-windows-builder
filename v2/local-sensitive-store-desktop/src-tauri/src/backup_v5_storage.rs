use super::*;
use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub(crate) struct StorageScan {
    pub(crate) object_count: i64,
    pub(crate) object_bytes: i64,
    pub(crate) database_history_bytes: i64,
    pub(crate) legacy_snapshot_count: i64,
    pub(crate) legacy_snapshot_bytes: i64,
    pub(crate) storage_breakdown: Value,
    pub(crate) total_logical_bytes: i64,
    pub(crate) scan_complete: bool,
    pub(crate) scanned_at_ms: i64,
    pub(crate) errors: Vec<String>,
    pub(crate) snapshot_bytes: HashMap<PathBuf, i64>,
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
    let mut snapshots = HashMap::<String, (i64, PathBuf)>::new();
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
        snapshots.insert(parts[1].to_string(), (version, database.unwrap()));
    }
    let mut missing_databases = snapshots.keys().cloned().collect::<HashSet<_>>();
    for (relative, size) in files {
        let parts = relative
            .iter()
            .map(|part| part.to_string_lossy())
            .collect::<Vec<_>>();
        let mut category = "otherBytes";
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
            if let Some((version, database)) = snapshots.get(parts[1].as_ref()) {
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
                } else {
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
    }
    for name in missing_databases {
        scan.errors.push(format!(
            "storage_snapshot_database_missing:snapshots/{name}"
        ));
    }
    scan.errors.sort();
    scan.errors.dedup();
    scan.scan_complete = scan.errors.is_empty();
    scan
}
