use super::*;

fn kst_parts(value: i64) -> Option<(chrono::NaiveDate, i32, u32)> {
    let offset = FixedOffset::east_opt(9 * 60 * 60)?;
    let date = DateTime::<Utc>::from_timestamp_millis(value)?.with_timezone(&offset);
    Some((date.date_naive(), date.year(), date.month()))
}

fn verified_snapshot(path: &Path, manifest: &Value) -> bool {
    let Some(tenant_id) = manifest
        .get("tenantId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
    else {
        return false;
    };
    manifest.get("ok").and_then(Value::as_bool) == Some(true)
        && crate::backup::authoritative_restore_manifest(path, manifest, tenant_id).is_ok()
}

type SnapshotIdentity = Vec<(PathBuf, u64, std::time::SystemTime)>;

pub(super) struct RetentionEntry {
    pub(super) path: PathBuf,
    pub(super) manifest: Value,
    pub(super) keep: bool,
    pub(super) reason: String,
    pub(super) identity: Option<SnapshotIdentity>,
}

// One calendar/count set for both formats. Invalid/incomplete entries never
// consume a healthy restore-point slot and are never deletion candidates.
pub(super) fn retention_plan(
    root: &Path,
    now: i64,
    pins: &HashSet<i64>,
    verify: bool,
) -> Result<Vec<RetentionEntry>, String> {
    if fs::symlink_metadata(root.join("snapshots"))
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err("backup_retention_inventory_symlink".into());
    }
    let directories = match fs::read_dir(root.join("snapshots")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("backup_retention_inventory_unavailable".into()),
    };
    let mut entries = Vec::new();
    let mut tenant = None;
    for directory in directories {
        let directory = directory.map_err(|_| "backup_retention_inventory_incomplete")?;
        let metadata = directory
            .file_type()
            .map_err(|_| "backup_retention_inventory_incomplete")?;
        if !metadata.is_dir() || metadata.is_symlink() {
            continue;
        }
        let path = directory.path().join("manifest.json");
        let manifest = if verify {
            json_file(&path)
        } else {
            crate::backup::listed_manifest(&path).ok()
        }
        .unwrap_or(Value::Null);
        let version = manifest["version"].as_i64().unwrap_or(0);
        let created = manifest["createdAtMs"].as_i64().unwrap_or(0);
        let kind = manifest["kind"].as_str().unwrap_or("");
        let reason = if directory
            .file_name()
            .to_string_lossy()
            .ends_with(".staging")
        {
            "staging_owner_unconfirmed"
        } else if !matches!(version, 4 | 5) {
            "legacy_or_incomplete_manifest"
        } else if !matches!(kind, "auto_sync" | "scheduled" | "pre_restore" | "manual") {
            "unknown_backup_purpose"
        } else if created <= 0 || created > now {
            "creation_time_unconfirmed"
        } else if manifest["ok"] != true {
            "snapshot_incomplete"
        } else {
            ""
        };
        let mut entry = RetentionEntry {
            path,
            manifest,
            keep: true,
            reason: reason.into(),
            identity: None,
        };
        if reason.is_empty() {
            let id = entry.manifest["tenantId"]
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or("backup_retention_tenant_missing")?
                .to_string();
            if tenant.as_ref().is_some_and(|expected| expected != &id) {
                return Err("backup_retention_tenant_mismatch".into());
            }
            tenant = Some(id);
            if verify {
                let identity = snapshot_identity(&entry.path, &entry.manifest);
                if identity.is_none()
                    || !verified_snapshot(&entry.path, &entry.manifest)
                    || snapshot_identity(&entry.path, &entry.manifest) != identity
                {
                    entry.reason = "verification_failed_or_file_changed".into();
                } else {
                    entry.identity = identity;
                }
            }
        }
        entries.push(entry);
    }
    entries.sort_by(|a, b| {
        b.manifest["createdAtMs"]
            .as_i64()
            .cmp(&a.manifest["createdAtMs"].as_i64())
            .then_with(|| a.path.cmp(&b.path))
    });
    let (mut recent, mut pre, mut manual) = (0usize, 0usize, 0usize);
    let (mut days, mut months, mut formats) = (HashSet::new(), HashSet::new(), HashSet::new());
    for entry in &mut entries {
        if !entry.reason.is_empty() {
            continue;
        }
        let kind = entry.manifest["kind"].as_str().unwrap_or("");
        let mut reasons = Vec::new();
        match kind {
            "manual" => {
                manual += 1;
                if manual <= MANUAL_KEEP {
                    reasons.push("manual");
                }
            }
            "pre_restore" => {
                pre += 1;
                if pre <= PRE_RESTORE_KEEP {
                    reasons.push("pre_restore");
                }
            }
            _ => {
                recent += 1;
                if recent <= RECENT_KEEP {
                    reasons.push("recent");
                }
                if formats.insert(entry.manifest["version"].as_i64().unwrap_or(0)) {
                    reasons.push("latest_recoverable_format");
                }
                if let (Some((date, y, m)), Some((today, ny, nm))) = (
                    kst_parts(entry.manifest["createdAtMs"].as_i64().unwrap_or(0)),
                    kst_parts(now),
                ) {
                    if (0..DAILY_KEEP_DAYS).contains(&today.signed_duration_since(date).num_days())
                        && days.insert(date)
                    {
                        reasons.push("daily");
                    }
                    if (0..MONTHLY_KEEP_MONTHS)
                        .contains(&((ny * 12 + nm as i32) - (y * 12 + m as i32)))
                        && months.insert((y, m))
                    {
                        reasons.push("monthly");
                    }
                }
            }
        }
        if entry.manifest["generation"]
            .as_i64()
            .is_some_and(|g| pins.contains(&g))
        {
            reasons.push("protected_generation");
        }
        entry.keep = !reasons.is_empty();
        entry.reason = if entry.keep {
            reasons.join(",")
        } else {
            "outside_retention".into()
        };
    }
    Ok(entries)
}

pub(crate) fn retention_preview(
    root: &Path,
    now: i64,
    pins: &HashSet<i64>,
    scan: &StorageScan,
) -> Result<Value, String> {
    let entries = retention_plan(root, now, pins, false)?;
    Ok(
        json!({"ok":true,"verification":"metadata_only","items":entries.iter().map(|entry| {
        let size = entry.path.parent().and_then(|path| scan.snapshot_bytes.get(path)).copied();
        json!({"manifestPath":entry.path.to_string_lossy(),"version":entry.manifest["version"],"kind":entry.manifest["kind"],
            "generation":entry.manifest["generation"],"createdAtMs":entry.manifest["createdAtMs"],"deviceName":entry.manifest.pointer("/source/pcName"),
            "bytes":size,"bytesComplete":scan.scan_complete && size.is_some(),"verification":"pending",
            "action":if entry.keep {if matches!(entry.reason.as_str(),"staging_owner_unconfirmed"|"legacy_or_incomplete_manifest"|"snapshot_incomplete"|"unknown_backup_purpose"|"creation_time_unconfirmed") {"review"} else {"keep"}} else {"verify_before_cleanup"},
            "plannedAction":if entry.manifest["version"]==4 {"quarantine"} else {"delete"},"reason":entry.reason})
    }).collect::<Vec<_>>() }),
    )
}

pub(crate) fn retention_check(root: &Path, now: i64, pins: &HashSet<i64>) -> Result<Value, String> {
    let entries = retention_plan(root, now, pins, false)?;
    let count = entries.iter().filter(|entry| !entry.keep).count();
    Ok(
        json!({"ok":true,"overLimit":count>0,"candidateCount":count,"checkedAtMs":now,"verification":"metadata_only"}),
    )
}

pub(crate) fn prune_snapshots(root: &Path, now: i64, pins: &HashSet<i64>) -> Result<Value, String> {
    let mut deleted = 0usize;
    let mut deleted_bytes = 0i64;
    let mut review = 0usize;
    for entry in retention_plan(root, now, pins, true)? {
        if entry.identity.is_none() {
            review += 1;
        }
        // v4 uses the same plan, but moves through the 30-day legacy journal.
        if entry.keep || entry.manifest["version"] != 5 || entry.manifest["kind"] == "manual" {
            continue;
        }
        let snapshot = entry
            .path
            .parent()
            .ok_or("backup_snapshot_parent_missing")?;
        if snapshot.parent() != Some(root.join("snapshots").as_path()) {
            return Err("backup_snapshot_prune_scope_invalid".into());
        }
        if json_file(&entry.path).as_ref() != Some(&entry.manifest)
            || snapshot_identity(&entry.path, &entry.manifest) != entry.identity
        {
            return Err("backup_snapshot_prune_changed".into());
        }
        let size = directory_size(snapshot);
        fs::remove_dir_all(snapshot).map_err(|_| "backup_snapshot_prune_failed")?;
        deleted += 1;
        deleted_bytes += size;
    }
    Ok(json!({"ok":true,"deleted":deleted,"deletedBytes":deleted_bytes,"reviewCount":review}))
}

fn snapshot_identity(path: &Path, manifest: &Value) -> Option<SnapshotIdentity> {
    fn collect(path: &Path, identity: &mut SnapshotIdentity) -> Option<()> {
        let metadata = fs::symlink_metadata(path).ok()?;
        if metadata.file_type().is_symlink() {
            return None;
        }
        identity.push((
            path.to_path_buf(),
            metadata.len(),
            metadata.modified().ok()?,
        ));
        if metadata.is_dir() {
            for entry in fs::read_dir(path).ok()? {
                collect(&entry.ok()?.path(), identity)?;
            }
        }
        Some(())
    }
    let mut identity = Vec::new();
    collect(path.parent()?, &mut identity)?;
    for artifact in manifest.get("artifacts")?.as_array()? {
        let relative = artifact.get("relativePath")?.as_str()?;
        if relative.starts_with("objects/sha256/") {
            collect(
                &artifact_path(
                    path,
                    SNAPSHOT_VERSION,
                    &crate::backup::safe_relative_path(relative)?,
                )
                .ok()?,
                &mut identity,
            )?;
        }
    }
    identity.sort_by(|left, right| left.0.cmp(&right.0));
    Some(identity)
}

pub(crate) fn prune_manual_snapshots(tenant_dir: &Path) -> Result<Value, String> {
    let snapshots_root = tenant_dir.join("snapshots");
    let candidates = snapshot_manifests(tenant_dir, false)
        .into_iter()
        .filter_map(|path| {
            let manifest = json_file(&path)?;
            (matches!(manifest.get("version").and_then(Value::as_i64), Some(4 | 5))
                && manifest.get("kind").and_then(Value::as_str) == Some("manual")
                && manifest.get("generation").is_none_or(Value::is_null))
            .then_some((path, manifest))
        })
        .collect::<Vec<_>>();
    if candidates.len() <= MANUAL_KEEP {
        return Ok(json!({
            "ok": true,
            "limit": MANUAL_KEEP,
            "retained": candidates.len(),
            "deleted": 0,
            "reviewCount": 0
        }));
    }

    let mut verified = Vec::new();
    let mut review_count = 0usize;
    for (path, manifest) in candidates {
        let Some(identity) = snapshot_identity(&path, &manifest) else {
            review_count += 1;
            continue;
        };
        if !verified_snapshot(&path, &manifest)
            || snapshot_identity(&path, &manifest).as_ref() != Some(&identity)
        {
            review_count += 1;
            continue;
        }
        verified.push((
            path,
            manifest
                .get("createdAtMs")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            manifest,
            identity,
        ));
    }
    verified.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let retained = verified.len().min(MANUAL_KEEP);
    let mut deleted = 0usize;
    for (path, _, manifest, identity) in verified.into_iter().skip(MANUAL_KEEP) {
        let snapshot = path
            .parent()
            .ok_or_else(|| "backup_snapshot_parent_missing".to_string())?;
        if snapshot.parent() != Some(snapshots_root.as_path()) {
            return Err("backup_snapshot_prune_scope_invalid".to_string());
        }
        if json_file(&path).as_ref() != Some(&manifest)
            || snapshot_identity(&path, &manifest).as_ref() != Some(&identity)
        {
            return Err("backup_snapshot_prune_changed".to_string());
        }
        fs::remove_dir_all(snapshot)
            .map_err(|error| format!("backup_snapshot_prune_failed:{error}"))?;
        deleted += 1;
    }
    Ok(json!({
        "ok": true,
        "limit": MANUAL_KEEP,
        "retained": retained,
        "deleted": deleted,
        "reviewCount": review_count
    }))
}
