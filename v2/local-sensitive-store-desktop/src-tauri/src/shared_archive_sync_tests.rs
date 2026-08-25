use super::*;
use crate::random_url_token;
use crate::shared_archive_apply::{apply_snapshot_bundles_to, verify_existing_archive};

fn archive_connection(path: &Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection.execute_batch(
        r#"
        PRAGMA foreign_keys=ON;
        CREATE TABLE shared_archives(
          id TEXT PRIMARY KEY,tenant_id TEXT NOT NULL,source_type TEXT NOT NULL,source_id TEXT NOT NULL,
          title TEXT NOT NULL,manifest_sha256 TEXT NOT NULL,record_count INTEGER NOT NULL,file_count INTEGER NOT NULL,
          total_file_bytes INTEGER NOT NULL,source_created_at INTEGER NOT NULL,source_expires_at INTEGER NOT NULL,
          imported_at INTEGER NOT NULL,manifest_json TEXT NOT NULL
        );
        CREATE TABLE shared_archive_records(
          archive_id TEXT NOT NULL,ordinal INTEGER NOT NULL,record_type TEXT NOT NULL,payload_json TEXT NOT NULL,
          payload_sha256 TEXT NOT NULL,PRIMARY KEY(archive_id,ordinal)
        );
        CREATE TABLE shared_archive_files(
          archive_id TEXT NOT NULL,ordinal INTEGER NOT NULL,original_name TEXT NOT NULL,content_type TEXT NOT NULL,
          byte_size INTEGER NOT NULL,sha256 TEXT NOT NULL,local_path TEXT NOT NULL,PRIMARY KEY(archive_id,ordinal)
        );
        "#,
    ).unwrap();
    connection
}

fn fixture() -> (PathBuf, Connection, PathBuf, String) {
    let base = std::env::temp_dir().join(format!("archive-sync-test-{}", random_url_token()));
    let source_files = base.join("source-files");
    let tenant_dir = base
        .join("onedrive")
        .join("OnlineClassLocalBackups")
        .join("tenants")
        .join("tenant-a");
    fs::create_dir_all(&source_files).unwrap();
    fs::create_dir_all(&tenant_dir).unwrap();
    let source_path = source_files.join("0000-기록.txt");
    fs::write(&source_path, b"immutable archive file").unwrap();
    let file_sha256 = sha256_bytes(b"immutable archive file");
    let payload = json!({"title":"이야기에서 인상적인 부분 기록하기"});
    let payload_json = serde_json::to_string(&payload).unwrap();
    let payload_sha256 = sha256_bytes(payload_json.as_bytes());
    let manifest_sha256 = sha256_bytes(b"manifest-a");
    let connection = archive_connection(&base.join("source.sqlite"));
    connection.execute(
        "INSERT INTO shared_archives VALUES ('archive-a','tenant-a','board','board-a','이야기에서 인상적인 부분 기록하기',?1,1,1,22,1,2,3,'{}')",
        params![manifest_sha256],
    ).unwrap();
    connection
        .execute(
            "INSERT INTO shared_archive_records VALUES ('archive-a',0,'board',?1,?2)",
            params![payload_json, payload_sha256],
        )
        .unwrap();
    connection.execute(
        "INSERT INTO shared_archive_files VALUES ('archive-a',0,'기록.txt','text/plain',22,?1,?2)",
        params![file_sha256, source_path.to_string_lossy().to_string()],
    ).unwrap();
    (base, connection, tenant_dir, manifest_sha256)
}

#[test]
fn content_addressed_bundle_verifies_and_rejects_tampering() {
    let (base, connection, tenant_dir, manifest_sha256) = fixture();
    let archives = ensure_tenant_bundles_from(&connection, "tenant-a", &tenant_dir).unwrap();
    verify_snapshot_bundles("tenant-a", &tenant_dir, &archives).unwrap();
    let bundle_file = tenant_dir
        .join("archive-bundles")
        .join(&manifest_sha256)
        .join("files/0000-기록.txt");
    fs::write(&bundle_file, b"tampered").unwrap();
    assert_eq!(
        verify_snapshot_bundles("tenant-a", &tenant_dir, &archives).unwrap_err(),
        "archive_sync_bundle_file_digest_mismatch"
    );
    let _ = fs::remove_dir_all(base);
}

#[test]
fn bundle_path_and_tenant_are_fail_closed() {
    let (base, connection, tenant_dir, _) = fixture();
    let mut archives = ensure_tenant_bundles_from(&connection, "tenant-a", &tenant_dir).unwrap();
    archives["records"][0]["bundleRelativePath"] = json!("../outside");
    assert_eq!(
        verify_snapshot_bundles("tenant-a", &tenant_dir, &archives).unwrap_err(),
        "archive_sync_bundle_path_invalid"
    );
    let clean = ensure_tenant_bundles_from(&connection, "tenant-a", &tenant_dir).unwrap();
    assert_eq!(
        verify_snapshot_bundles("tenant-b", &tenant_dir, &clean).unwrap_err(),
        "archive_sync_commit_mismatch"
    );
    let _ = fs::remove_dir_all(base);
}

#[test]
fn bundle_commit_is_required_and_digest_pinned() {
    let (base, connection, tenant_dir, manifest_sha256) = fixture();
    let archives = ensure_tenant_bundles_from(&connection, "tenant-a", &tenant_dir).unwrap();
    let commit = tenant_dir
        .join("archive-bundles")
        .join(manifest_sha256)
        .join("commit.json");
    fs::write(commit, b"{}").unwrap();
    assert_eq!(
        verify_snapshot_bundles("tenant-a", &tenant_dir, &archives).unwrap_err(),
        "archive_sync_commit_mismatch"
    );
    let _ = fs::remove_dir_all(base);
}

#[test]
fn board_and_assignment_archives_share_one_union_inventory() {
    let (base, connection, tenant_dir, _) = fixture();
    let payload_json = serde_json::to_string(&json!({"title":"과제 보관본"})).unwrap();
    let payload_sha256 = sha256_bytes(payload_json.as_bytes());
    connection.execute(
        "INSERT INTO shared_archives VALUES ('archive-b','tenant-a','assignment','assignment-a','과제 보관본',?1,1,0,0,1,2,3,'{}')",
        params![sha256_bytes(b"manifest-b")],
    ).unwrap();
    connection
        .execute(
            "INSERT INTO shared_archive_records VALUES ('archive-b',0,'assignment',?1,?2)",
            params![payload_json, payload_sha256],
        )
        .unwrap();
    let archives = ensure_tenant_bundles_from(&connection, "tenant-a", &tenant_dir).unwrap();
    assert_eq!(archives["count"], 2);
    assert_eq!(archives["boardCount"], 1);
    assert_eq!(archives["assignmentCount"], 1);
    verify_snapshot_bundles("tenant-a", &tenant_dir, &archives).unwrap();
    let _ = fs::remove_dir_all(base);
}

#[test]
fn same_archive_id_with_different_manifest_is_rejected() {
    let (base, source, tenant_dir, _) = fixture();
    let archives = ensure_tenant_bundles_from(&source, "tenant-a", &tenant_dir).unwrap();
    let target_root = base.join("target-files");
    fs::create_dir_all(&target_root).unwrap();
    let target = archive_connection(&base.join("target.sqlite"));
    target.execute(
        "INSERT INTO shared_archives VALUES ('archive-a','tenant-a','board','board-a','old',?1,0,0,0,1,2,3,'{}')",
        params![sha256_bytes(b"different")],
    ).unwrap();
    let reference = &archives["records"][0];
    let bundle_dir = tenant_dir.join(reference["bundleRelativePath"].as_str().unwrap());
    let document = verify_bundle_reference_at(&bundle_dir, "tenant-a", reference).unwrap();
    assert_eq!(
        verify_existing_archive(&target, &target_root, "tenant-a", &document, &bundle_dir)
            .unwrap_err(),
        "archive_sync_existing_manifest_mismatch"
    );
    let _ = fs::remove_dir_all(base);
}

#[test]
fn archive_union_is_idempotent_and_repairs_a_missing_file() {
    let (base, source, tenant_dir, _) = fixture();
    let archives = ensure_tenant_bundles_from(&source, "tenant-a", &tenant_dir).unwrap();
    let target_root = base.join("target-files");
    fs::create_dir_all(&target_root).unwrap();
    let mut target = archive_connection(&base.join("target.sqlite"));

    let first = apply_snapshot_bundles_to(
        &mut target,
        &target_root,
        "tenant-a",
        &tenant_dir,
        &archives,
    )
    .unwrap();
    assert_eq!(first["archiveImported"], 1);
    assert_eq!(
        target
            .query_row("SELECT COUNT(*) FROM shared_archives", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );

    let second = apply_snapshot_bundles_to(
        &mut target,
        &target_root,
        "tenant-a",
        &tenant_dir,
        &archives,
    )
    .unwrap();
    assert_eq!(second["archiveUnchanged"], 1);
    let local_path: String = target
        .query_row("SELECT local_path FROM shared_archive_files", [], |row| {
            row.get(0)
        })
        .unwrap();
    fs::remove_file(&local_path).unwrap();

    let repaired = apply_snapshot_bundles_to(
        &mut target,
        &target_root,
        "tenant-a",
        &tenant_dir,
        &archives,
    )
    .unwrap();
    assert_eq!(repaired["archiveRepairedFiles"], 1);
    assert!(Path::new(&local_path).is_file());
    let _ = fs::remove_dir_all(base);
}
