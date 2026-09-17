use super::*;

#[test]
fn publication_cache_rejects_a_store_inside_onedrive() {
    let root = env_root();
    fs::create_dir_all(root.join("OneDrive - synthetic/local")).unwrap();
    fs::create_dir_all(root.join("shared")).unwrap();
    let store = SqliteStore::open(root.join("OneDrive - synthetic/local/db.sqlite")).unwrap();
    assert_eq!(cache_root(&store,"tenant-a",&root.join("shared")).unwrap_err(),"artifact_recovery_cache_outside_onedrive_required");
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

fn fixture() -> (PathBuf, SqliteStore, PathBuf) {
    let root = env_root();
    fs::create_dir_all(root.join("local")).unwrap();
    let store = SqliteStore::open(root.join("local/db.sqlite")).unwrap();
    set_folder(
        &store,
        "tenant-a".into(),
        root.join("backups").to_string_lossy().into(),
    )
    .unwrap();
    let shared = configured_tenant_dir(&store, "tenant-a").unwrap();
    fs::create_dir_all(shared.join("snapshots")).unwrap();
    (root, store, shared)
}
fn env_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "artifact-repair-test-{:016x}",
        rand::random::<u64>()
    ))
}
fn snapshot(
    shared: &Path,
    id: &str,
    tenant: &str,
    generation: Option<i64>,
    bytes: &[u8],
) -> (PathBuf, String, String) {
    let dir = shared.join("snapshots").join(id);
    fs::create_dir_all(dir.join("db")).unwrap();
    let db = dir.join("db/local-sensitive.sqlite");
    fs::write(&db, bytes).unwrap();
    let (size, sha) = sha256_file(&db).unwrap();
    let mut artifacts = vec![ArtifactDigest {
        relative_path: "db/local-sensitive.sqlite".into(),
        size,
        sha256: sha.clone(),
    }];
    let root = artifact_set_sha256(&mut artifacts);
    let doc = json!({"ok":true,"version":3,"tenantId":tenant,"generation":generation,"artifactSetSha256":root,
        "artifacts":[{"relativePath":"db/local-sensitive.sqlite","size":size,"sha256":sha}]});
    let path = dir.join("manifest.json");
    fs::write(&path, doc.to_string()).unwrap();
    fs::write(dir.join("commit.json"), doc.to_string()).unwrap();
    (path, root, sha)
}

#[test]
fn missing_checkpoint_db_is_repaired_from_exact_manual_bytes_without_advancing_state() {
    let (root, store, shared) = fixture();
    let (path, hash, dbhash) = snapshot(&shared, "latest", "tenant-a", Some(377), b"expected-db");
    snapshot(&shared, "manual", "tenant-a", None, b"expected-db");
    fs::remove_file(path.parent().unwrap().join("db/local-sensitive.sqlite")).unwrap();
    let state = local_sync_state(&store, "tenant-a").unwrap();
    assert!(repair_checkpoint_artifacts(
        &store,
        "tenant-a",
        377,
        377,
        &hash,
        &dbhash,
        "onedrive_download_pending:file_not_arrived"
    )
    .unwrap());
    assert_eq!(
        fs::read(path.parent().unwrap().join("db/local-sensitive.sqlite")).unwrap(),
        b"expected-db"
    );
    assert_eq!(
        local_sync_state(&store, "tenant-a")
            .unwrap()
            .applied_generation,
        state.applied_generation
    );
    assert!(!repair_checkpoint_artifacts(
        &store,
        "tenant-a",
        377,
        377,
        &hash,
        &dbhash,
        "onedrive_download_pending:file_not_arrived"
    )
    .unwrap());
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn wrong_tenant_and_wrong_bytes_never_repair_and_attempts_survive_reopen() {
    let (root, store, shared) = fixture();
    let (path, hash, dbhash) = snapshot(&shared, "latest", "tenant-a", Some(377), b"expected-db");
    snapshot(&shared, "other-tenant", "tenant-b", None, b"expected-db");
    snapshot(&shared, "wrong-bytes", "tenant-a", None, b"different-db");
    fs::remove_file(path.parent().unwrap().join("db/local-sensitive.sqlite")).unwrap();
    for _ in 0..3 {
        assert!(!repair_checkpoint_artifacts(
            &store,
            "tenant-a",
            377,
            377,
            &hash,
            &dbhash,
            "onedrive_download_pending:file_not_arrived"
        )
        .unwrap());
    }
    store
        .conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE local_store_artifact_issue SET first_seen_at_ms=?1",
            params![now_ms() - 600_001],
        )
        .unwrap();
    drop(store);
    let store = SqliteStore::open(root.join("local/db.sqlite")).unwrap();
    let issue = artifact_issue_status(&store, "tenant-a", 377).unwrap();
    assert_eq!(issue["retryCount"], 3);
    assert_eq!(issue["prolonged"], true);
    assert_eq!(issue["missingFileKinds"], json!(["database"]));
    assert_eq!(issue["repairAvailable"], false);
    assert_eq!(
        artifact_issue_status(&store, "tenant-a", 378).unwrap(),
        Value::Null
    );
    assert!(!path
        .parent()
        .unwrap()
        .join("db/local-sensitive.sqlite")
        .exists());
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn atomic_install_rejects_existing_mismatch_and_source_race() {
    let root = env_root();
    fs::create_dir_all(&root).unwrap();
    let source = root.join("source");
    fs::write(&source, b"before").unwrap();
    let (size, sha256) = sha256_file(&source).unwrap();
    let a = ArtifactDigest {
        relative_path: "target".into(),
        size,
        sha256,
    };
    fs::write(root.join("target"), b"concurrent").unwrap();
    assert_eq!(
        install_bytes(&source, &root, "target", &a).unwrap_err(),
        "artifact_repair_existing_mismatch"
    );
    fs::write(&source, b"after").unwrap();
    assert_eq!(
        install_bytes(&source, &root, "new-target", &a).unwrap_err(),
        "artifact_repair_source_changed"
    );
    assert!(!root.join("new-target").exists());
    assert_eq!(fs::read(root.join("target")).unwrap(), b"concurrent");
    assert!(fs::read_dir(&root).unwrap().all(|e| !e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".repair-")));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unsafe_artifact_path_fails_even_with_matching_root() {
    let (root, store, shared) = fixture();
    let (path, _, dbhash) = snapshot(&shared, "latest", "tenant-a", Some(377), b"expected-db");
    let mut doc = metadata_json(&path).unwrap();
    doc["artifacts"][0]["relativePath"] = json!("../escape");
    let mut a = vec![ArtifactDigest {
        relative_path: "../escape".into(),
        size: 11,
        sha256: dbhash,
    }];
    let hash = artifact_set_sha256(&mut a);
    doc["artifactSetSha256"] = json!(hash);
    fs::write(&path, doc.to_string()).unwrap();
    fs::write(path.with_file_name("commit.json"), doc.to_string()).unwrap();
    assert!(sealed(&path, "tenant-a", Some(377), &hash).is_err());
    for p in [
        "../outside",
        "/absolute",
        "C:/outside",
        "a\\b",
        "a/./b",
        "a//b",
    ] {
        assert!(checked_path(&shared, p).is_err(), "{p}");
    }
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn protected_publication_can_repair_a_deleted_database() {
    let (root, store, shared) = fixture();
    let snapshot =
        run_with_kind_version(&store, "tenant-a".into(), "auto_sync", Some(377), 4).unwrap();
    protect_publication(&store, "tenant-a", &snapshot).unwrap();
    let path = PathBuf::from(snapshot["manifestPath"].as_str().unwrap());
    let db = path.parent().unwrap().join("db/local-sensitive.sqlite");
    fs::remove_file(&db).unwrap();
    assert!(repair_checkpoint_artifacts(
        &store,
        "tenant-a",
        377,
        377,
        snapshot["artifactSetSha256"].as_str().unwrap(),
        snapshot["databaseSha256"].as_str().unwrap(),
        "onedrive_download_pending:file_not_arrived"
    )
    .unwrap());
    assert_eq!(
        sha256_file(&db).unwrap().1,
        snapshot["databaseSha256"].as_str().unwrap()
    );
    assert!(verify_checkpoint_manifest_path(
        &path,
        "tenant-a",
        377,
        snapshot["artifactSetSha256"].as_str().unwrap()
    )
    .is_ok());
    assert!(cache_root(&store, "tenant-a", &shared)
        .unwrap()
        .starts_with(&store.data_dir));
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn dirty_count_is_tenant_scoped_and_status_is_read_only() {
    let (root, store, _) = fixture();
    store.conn.lock().unwrap().execute("INSERT INTO local_store_device_sync_records
        (tenant_id,table_name,record_key,changed_at_ms) VALUES ('tenant-a','work_note_pages','[\"a\"]',1),('tenant-b','work_note_pages','[\"b\"]',1)",[]).unwrap();
    assert_eq!(pending_local_change_count(&store, "tenant-a").unwrap(), 1);
    for _ in 0..3 {
        assert_eq!(
            artifact_issue_status(&store, "tenant-a", 377).unwrap(),
            Value::Null
        );
    }
    assert_eq!(
        store
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM local_store_artifact_issue", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn protected_publication_restores_missing_archive_bytes_and_metadata() {
    let (root, store, shared) = fixture();
    crate::shared_archive::with_test_root(&root.join("archives"), || {
        let archives = crate::shared_archive::open_db().unwrap();
        archives.execute("INSERT INTO shared_archives VALUES ('archive-a','tenant-a','board','fixture','Fixture',?1,0,0,0,1,2,3,'{}')",
            params!["a".repeat(64)]).unwrap();
        let snapshot =
            run_with_kind_version(&store, "tenant-a".into(), "auto_sync", Some(377), 4).unwrap();
        protect_publication(&store, "tenant-a", &snapshot).unwrap();
        let path = PathBuf::from(snapshot["manifestPath"].as_str().unwrap());
        let hash = snapshot["artifactSetSha256"].as_str().unwrap();
        let dbhash = snapshot["databaseSha256"].as_str().unwrap();
        let s = sealed(&path, "tenant-a", Some(377), hash).unwrap();
        let files = bundle_files(&s).unwrap();
        assert!(!files.is_empty());
        let missing = shared.join(&files[0].0);
        fs::remove_file(&missing).unwrap();
        assert!(repair_checkpoint_artifacts(
            &store,
            "tenant-a",
            377,
            377,
            hash,
            dbhash,
            "archive_sync_bundle_unavailable"
        )
        .unwrap());
        assert!(verify_checkpoint_manifest_path(&path, "tenant-a", 377, hash).is_ok());
        fs::remove_file(&path).unwrap();
        fs::remove_file(path.with_file_name("commit.json")).unwrap();
        assert!(repair_checkpoint_artifacts(
            &store,
            "tenant-a",
            377,
            377,
            hash,
            dbhash,
            "onedrive_snapshot_pending"
        )
        .unwrap());
        assert!(verify_checkpoint_manifest_path(&path, "tenant-a", 377, hash).is_ok());
    });
    drop(store);
    fs::remove_dir_all(root).unwrap();
}
