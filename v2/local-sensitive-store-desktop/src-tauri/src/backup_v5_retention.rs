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

pub(crate) fn prune_snapshots(
    tenant_dir: &Path,
    now_ms: i64,
    pinned_generations: &HashSet<i64>,
) -> Result<(), String> {
    let snapshots_root = tenant_dir.join("snapshots");
    let mut managed = Vec::new();
    let mut pre_restore = Vec::new();
    let mut keep = HashSet::new();
    let mut candidates = Vec::new();
    for path in snapshot_manifests(tenant_dir, false) {
        let Some(manifest) = json_file(&path) else {
            continue;
        };
        if manifest.get("version").and_then(Value::as_i64) != Some(SNAPSHOT_VERSION) {
            continue;
        }
        if manifest
            .get("generation")
            .and_then(Value::as_i64)
            .is_some_and(|generation| pinned_generations.contains(&generation))
        {
            keep.insert(path.clone());
            continue;
        }
        if !matches!(
            manifest.get("kind").and_then(Value::as_str),
            Some("auto_sync" | "scheduled" | "pre_restore")
        ) {
            continue;
        }
        candidates.push((path, manifest));
    }
    let managed_count = candidates
        .iter()
        .filter(|(_, manifest)| manifest.get("kind").and_then(Value::as_str) != Some("pre_restore"))
        .count();
    if managed_count <= RECENT_KEEP
        && candidates.len().saturating_sub(managed_count) <= PRE_RESTORE_KEEP
    {
        return Ok(());
    }
    for (path, manifest) in candidates {
        // Validate each candidate once per maintenance operation; invalid snapshots never occupy slots.
        let Some(identity) = snapshot_identity(&path, &manifest) else {
            continue;
        };
        if !verified_snapshot(&path, &manifest)
            || snapshot_identity(&path, &manifest).as_ref() != Some(&identity)
        {
            continue;
        }
        let created = manifest
            .get("createdAtMs")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        match manifest.get("kind").and_then(Value::as_str).unwrap_or("") {
            "auto_sync" | "scheduled" => managed.push((path, created, manifest, identity)),
            "pre_restore" => pre_restore.push((path, created, manifest, identity)),
            _ => {}
        }
    }
    managed.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    pre_restore.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let mut days = HashSet::new();
    let mut months = HashSet::new();
    let now_parts = kst_parts(now_ms);
    for (index, (path, created, _, _)) in managed.iter().enumerate() {
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
    for (path, _, _, _) in pre_restore.iter().take(PRE_RESTORE_KEEP) {
        keep.insert(path.clone());
    }
    for (path, _, manifest, identity) in managed.into_iter().chain(pre_restore) {
        if keep.contains(&path) {
            continue;
        }
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
    }
    Ok(())
}
