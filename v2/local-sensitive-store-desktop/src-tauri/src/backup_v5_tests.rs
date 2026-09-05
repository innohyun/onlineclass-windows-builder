use super::journal::*;
use super::*;

fn valid_snapshot(
    root: &Path,
    name: &str,
    kind: &str,
    created: i64,
    generation: Option<i64>,
) -> PathBuf {
    let directory = root.join("snapshots").join(name);
    fs::create_dir_all(directory.join("db")).unwrap();
    fs::write(
        directory.join("db/local-sensitive.sqlite"),
        b"synthetic database",
    )
    .unwrap();
    let (size, sha256) = sha256_file(&directory.join("db/local-sensitive.sqlite")).unwrap();
    let database = json!({"relativePath":"db/local-sensitive.sqlite","size":size,"sha256":sha256});
    let mut artifacts = vec![ArtifactDigest {
        relative_path: "db/local-sensitive.sqlite".to_string(),
        size,
        sha256,
    }];
    let sync = generation
        .map(|_| json!({"contentSha256":"a".repeat(64),"records":[]}))
        .unwrap_or(Value::Null);
    let (_, index_size, index_sha256) = crate::backup_v4::write_apply_index(
        &directory,
        "tenant-a",
        generation,
        database.clone(),
        sync,
        json!({"records":[]}),
        json!({"records":[]}),
        json!({"count":0,"records":[]}),
        json!({}),
    )
    .unwrap();
    artifacts.push(ArtifactDigest {
        relative_path: crate::backup_v4::APPLY_INDEX_RELATIVE_PATH.to_string(),
        size: index_size,
        sha256: index_sha256.clone(),
    });
    let root_sha = crate::backup::artifact_set_sha256(&mut artifacts);
    let manifest = json!({
        "ok":true,"version":5,"kind":kind,"createdAtMs":created,"generation":generation,"tenantId":"tenant-a","backupId":name,
        "db":database,"applyIndex":{"relativePath":crate::backup_v4::APPLY_INDEX_RELATIVE_PATH,"size":index_size,"sha256":index_sha256},
        "artifactSetSha256":root_sha,
        "artifacts":artifacts.iter().map(|artifact| json!({"relativePath":artifact.relative_path,"size":artifact.size,"sha256":artifact.sha256})).collect::<Vec<_>>()
    });
    fs::write(directory.join("manifest.json"), manifest.to_string()).unwrap();
    fs::write(directory.join("commit.json"), json!({"version":1,"tenantId":"tenant-a","backupId":name,"generation":generation,"artifactSetSha256":root_sha}).to_string()).unwrap();
    directory
}

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
        valid_snapshot(
            &root,
            &format!("auto-{index:02}"),
            "auto_sync",
            created,
            Some(index + 1),
        );
    }
    for index in 0..8 {
        valid_snapshot(
            &root,
            &format!("pre-{index:02}"),
            "pre_restore",
            now - index,
            None,
        );
    }
    let manual = snapshots.join("manual");
    fs::create_dir_all(&manual).unwrap();
    fs::write(
        manual.join("manifest.json"),
        json!({"version":5,"kind":"manual","createdAtMs":1}).to_string(),
    )
    .unwrap();
    prune_snapshots(&root, now, &HashSet::new()).expect("prune snapshots");
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

struct FixtureRoot(PathBuf);
impl FixtureRoot {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "backup-v5-{label}-{}",
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}
impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn pins_and_valid_restore_points_survive_failed_newer_snapshots() {
    let fixture = FixtureRoot::new("pins");
    let root = &fixture.0;
    let now = DateTime::parse_from_rfc3339("2026-09-05T12:00:00+09:00")
        .unwrap()
        .timestamp_millis();
    let pinned = valid_snapshot(root, "pinned", "auto_sync", now - 400 * 86_400_000, Some(1));
    for index in 0..11 {
        valid_snapshot(
            root,
            &format!("valid-{index}"),
            "scheduled",
            now - index,
            None,
        );
    }
    for index in 0..12 {
        let invalid = valid_snapshot(
            root,
            &format!("failed-{index}"),
            "scheduled",
            now + index,
            None,
        );
        fs::write(
            invalid.join("db/local-sensitive.sqlite"),
            b"corrupted database",
        )
        .unwrap();
    }
    prune_snapshots(root, now, &HashSet::from([1])).unwrap();
    assert!(pinned.exists());
    assert!(root.join("snapshots/valid-9").exists());
    assert!(!root.join("snapshots/valid-10").exists());
    assert!(root.join("snapshots/failed-11").exists());
}

#[test]
fn incomplete_snapshot_inventory_defers_all_object_gc() {
    let fixture = FixtureRoot::new("incomplete");
    let root = &fixture.0;
    fs::create_dir_all(root.join("snapshots/unfinished.staging")).unwrap();
    fs::write(root.join("source.bin"), b"keep this object").unwrap();
    let object = put_object(root, &root.join("source.bin"), "test").unwrap();
    assert!(quarantine_unreferenced_objects(root, &HashSet::new(), 100).is_err());
    assert!(root.join(&object.artifact.relative_path).exists());
    fs::rename(
        root.join("snapshots/unfinished.staging"),
        root.join("snapshots/unfinished"),
    )
    .unwrap();
    fs::write(root.join("snapshots/unfinished/manifest.json"), b"{partial").unwrap();
    assert!(quarantine_unreferenced_objects(root, &HashSet::new(), 200).is_err());
    assert!(root.join(&object.artifact.relative_path).exists());
}

#[test]
fn quarantine_object_is_restored_immediately_when_referenced_again() {
    let fixture = FixtureRoot::new("reintroduced");
    let root = &fixture.0;
    fs::create_dir_all(root.join("snapshots")).unwrap();
    fs::write(root.join("source.bin"), b"reintroduced object").unwrap();
    let object = put_object(root, &root.join("source.bin"), "test").unwrap();
    assert_eq!(
        quarantine_unreferenced_objects(root, &HashSet::new(), 100).unwrap()["quarantined"],
        1
    );
    assert!(!root.join(&object.artifact.relative_path).exists());
    let result = quarantine_unreferenced_objects(
        root,
        &HashSet::from([object.artifact.relative_path.clone()]),
        101,
    )
    .unwrap();
    assert_eq!(result["restored"], 1);
    assert_eq!(result["deleted"], 0);
    assert_eq!(
        sha256_file(&root.join(&object.artifact.relative_path))
            .unwrap()
            .1,
        object.artifact.sha256
    );
}

#[test]
fn invalid_commit_or_index_defers_gc_without_hiding_objects() {
    let fixture = FixtureRoot::new("commit");
    let root = &fixture.0;
    let snapshot = valid_snapshot(root, "valid", "manual", 10, None);
    fs::write(root.join("source.bin"), b"not referenced yet").unwrap();
    let object = put_object(root, &root.join("source.bin"), "test").unwrap();
    fs::write(snapshot.join("commit.json"), b"{}").unwrap();
    assert!(quarantine_unreferenced_objects(root, &HashSet::new(), 100).is_err());
    assert!(root.join(&object.artifact.relative_path).exists());
}

#[test]
fn sealed_snapshot_object_references_are_preserved_without_live_references() {
    let fixture = FixtureRoot::new("sealed-reference");
    let root = &fixture.0;
    let snapshot = valid_snapshot(root, "retained", "manual", 10, None);
    fs::write(root.join("source.bin"), b"snapshot-only object").unwrap();
    let object = put_object(root, &root.join("source.bin"), "test")
        .unwrap()
        .artifact;
    let index_path = snapshot.join(crate::backup_v4::APPLY_INDEX_RELATIVE_PATH);
    let mut index = json_file(&index_path).unwrap();
    index["media"]["records"] = json!([{"status":"copied","backupRelativePath":object.relative_path,"size":object.size,"sha256":object.sha256}]);
    fs::write(&index_path, index.to_string()).unwrap();
    let (size, sha256) = sha256_file(&index_path).unwrap();
    let mut manifest = json_file(&snapshot.join("manifest.json")).unwrap();
    manifest["applyIndex"]["size"] = json!(size);
    manifest["applyIndex"]["sha256"] = json!(sha256);
    for artifact in manifest["artifacts"].as_array_mut().unwrap() {
        if artifact["relativePath"] == crate::backup_v4::APPLY_INDEX_RELATIVE_PATH {
            artifact["size"] = json!(size);
            artifact["sha256"] = json!(sha256);
        }
    }
    manifest["artifacts"].as_array_mut().unwrap().push(
        json!({"relativePath":object.relative_path,"size":object.size,"sha256":object.sha256}),
    );
    let mut artifacts = manifest["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|artifact| ArtifactDigest {
            relative_path: artifact["relativePath"].as_str().unwrap().to_string(),
            size: artifact["size"].as_u64().unwrap(),
            sha256: artifact["sha256"].as_str().unwrap().to_string(),
        })
        .collect::<Vec<_>>();
    let digest = crate::backup::artifact_set_sha256(&mut artifacts);
    manifest["artifactSetSha256"] = json!(digest);
    fs::write(snapshot.join("manifest.json"), manifest.to_string()).unwrap();
    let mut commit = json_file(&snapshot.join("commit.json")).unwrap();
    commit["artifactSetSha256"] = json!(digest);
    fs::write(snapshot.join("commit.json"), commit.to_string()).unwrap();
    assert_eq!(
        quarantine_unreferenced_objects(root, &HashSet::new(), 100).unwrap()["quarantined"],
        0
    );
    assert!(root.join(&object.relative_path).exists());
    fs::write(index_path, b"{partial").unwrap();
    assert!(quarantine_unreferenced_objects(root, &HashSet::new(), 200).is_err());
    assert!(root.join(&object.relative_path).exists());
}

#[test]
fn expired_quarantine_never_recursively_removes_unrecognized_files() {
    let fixture = FixtureRoot::new("quarantine-unknown");
    let root = &fixture.0;
    fs::create_dir_all(root.join("snapshots")).unwrap();
    fs::write(root.join("source.bin"), b"old object").unwrap();
    let object = put_object(root, &root.join("source.bin"), "test")
        .unwrap()
        .artifact;
    quarantine_unreferenced_objects(root, &HashSet::new(), 100).unwrap();
    let unexpected = root.join("objects-quarantine/100/unexpected.txt");
    fs::write(&unexpected, b"unrecognized file").unwrap();
    assert!(quarantine_unreferenced_objects(root, &HashSet::new(), 100 + 31 * 86_400_000).is_err());
    assert!(unexpected.exists());
    assert!(root
        .join("objects-quarantine/100")
        .join(
            object
                .relative_path
                .strip_prefix("objects/sha256/")
                .unwrap()
        )
        .exists());
}

fn write_journal(path: &Path, sequence: u64, records: &[Value]) {
    fs::write(path, json!({"version":1,"sequence":sequence,"updatedAtMs":sequence,"records":records,"manifestDigest":legacy_records_digest(records).unwrap()}).to_string()).unwrap();
}

#[test]
fn journal_recovers_both_rename_boundaries_and_corrupt_current() {
    for mode in [
        "before_rotate",
        "after_rotate",
        "corrupt_current",
        "previous_only",
        "corrupt_current_previous",
    ] {
        let fixture = FixtureRoot::new(mode);
        let root = &fixture.0;
        let journal_root = legacy_quarantine_root(root);
        fs::create_dir_all(&journal_root).unwrap();
        let old = vec![json!({"id":"old","status":"quarantined"})];
        let new = vec![json!({"id":"new","status":"restored"})];
        match mode {
            "before_rotate" => write_journal(&journal_root.join("manifest.json"), 1, &old),
            "after_rotate" | "previous_only" => {
                write_journal(&journal_root.join("manifest.previous.json"), 1, &old)
            }
            "corrupt_current" | "corrupt_current_previous" => {
                write_journal(&journal_root.join("manifest.previous.json"), 1, &old);
                fs::write(journal_root.join("manifest.json"), b"{partial").unwrap();
            }
            _ => unreachable!(),
        }
        if mode != "previous_only" && mode != "corrupt_current_previous" {
            write_journal(&journal_root.join("manifest.next.json"), 2, &new);
        }
        let expected = if mode == "previous_only" || mode == "corrupt_current_previous" {
            old
        } else {
            new
        };
        assert_eq!(
            load_legacy_quarantine_records(root).unwrap(),
            expected,
            "{mode}"
        );
        assert_eq!(
            load_legacy_quarantine_records(root).unwrap(),
            expected,
            "{mode} second read"
        );
        assert!(journal_root.join("manifest.json").is_file());
    }
}

#[test]
fn conflicting_journal_sequences_fail_closed_and_orphans_remain_reviewable() {
    let fixture = FixtureRoot::new("journal-conflict");
    let root = &fixture.0;
    let journal_root = legacy_quarantine_root(root);
    fs::create_dir_all(journal_root.join("items/untracked")).unwrap();
    fs::write(
        journal_root.join("items/untracked/data.bin"),
        b"orphaned data",
    )
    .unwrap();
    let summary = legacy_quarantine_summary(root, 100).unwrap();
    assert_eq!(summary["reviewCount"], 1);
    assert_eq!(summary["quarantinedCount"], 0);
    let records = load_legacy_quarantine_records(root).unwrap();
    assert_eq!(records[0]["reviewReason"], "orphaned_quarantine_item");
    assert_eq!(records[0]["bytes"], 13);
    write_journal(&journal_root.join("manifest.json"), 9, &records);
    write_journal(
        &journal_root.join("manifest.next.json"),
        9,
        &[json!({"id":"other"})],
    );
    assert_eq!(
        load_legacy_quarantine_records(root).unwrap_err(),
        "backup_legacy_quarantine_journal_conflict"
    );
    assert!(journal_root.join("items/untracked/data.bin").exists());
}

#[test]
fn storage_breakdown_counts_every_file_once_including_review_and_staging() {
    let fixture = FixtureRoot::new("storage");
    let root = &fixture.0;
    let modern = valid_snapshot(root, "modern", "manual", 20, None);
    let legacy = root.join("snapshots/legacy");
    fs::create_dir_all(legacy.join("db")).unwrap();
    fs::write(legacy.join("db/data.sqlite"), b"legacy-db").unwrap();
    fs::write(
        legacy.join("manifest.json"),
        json!({"version":4,"db":{"relativePath":"db/data.sqlite"}}).to_string(),
    )
    .unwrap();
    for (path, content) in [
        ("legacy-snapshot-quarantine/items/review/data.bin", "review"),
        ("objects-quarantine/100/aa/placeholder", "quarantine"),
        ("archive-bundles/archive/record.json", "bundle"),
        ("snapshots/active.staging/db/data.sqlite", "staging"),
        ("unclassified.txt", "other"),
    ] {
        let destination = root.join(path);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(destination, content.as_bytes()).unwrap();
    }
    let scan = scan_storage(root);
    assert!(scan.scan_complete, "{:?}", scan.errors);
    let total = scan
        .storage_breakdown
        .as_object()
        .unwrap()
        .values()
        .map(|value| value.as_i64().unwrap())
        .sum::<i64>();
    assert_eq!(total, scan.total_logical_bytes);
    assert_eq!(total, directory_size(root));
    assert_eq!(
        scan.storage_breakdown["v5DatabaseBytes"],
        fs::metadata(modern.join("db/local-sensitive.sqlite"))
            .unwrap()
            .len()
    );
    assert_eq!(
        scan.storage_breakdown["legacySnapshotBytes"],
        directory_size(&legacy)
    );
    assert_eq!(scan.storage_breakdown["legacyQuarantineBytes"], 6);
    assert_eq!(scan.storage_breakdown["stagingBytes"], 7);
    assert_eq!(scan.database_history_bytes, 18 + 9);
    assert_eq!(scan.legacy_snapshot_count, 1);
    fs::write(legacy.join("manifest.json"), b"{partial").unwrap();
    let incomplete = scan_storage(root);
    assert!(!incomplete.scan_complete);
    assert!(!incomplete.errors.is_empty());
    assert_eq!(incomplete.total_logical_bytes, directory_size(root));
}
