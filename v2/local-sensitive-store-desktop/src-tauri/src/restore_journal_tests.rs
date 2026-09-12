use super::*;
use crate::{backup, SqliteStore};
use base64::Engine;
use serde_json::{json, Value};

fn fixture() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "classaimate-qa-restore-{}",
        crate::random_url_token()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}
fn media(store: &SqliteStore, id: &str, bytes: &[u8]) {
    store
        .upsert_board_media(
            json!({"tenantId":"qa-restore","boardId":"qa-board","postId":"qa-post",
        "mediaId":id,"fileName":format!("{id}.bin"),"contentType":"application/octet-stream",
        "dataBase64":base64::engine::general_purpose::STANDARD.encode(bytes)}),
        )
        .unwrap();
}
fn store(root: &Path, name: &str) -> SqliteStore {
    let store = SqliteStore::open(root.join(name).join("store.sqlite")).unwrap();
    fs::create_dir_all(root.join("one-drive")).unwrap();
    backup::set_folder(
        &store,
        "qa-restore".into(),
        root.join("one-drive").to_string_lossy().into(),
    )
    .unwrap();
    store
}

#[test]
fn crash_child() {
    let Ok(root) = std::env::var("CLASSAIMATE_QA_RESTORE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    assert!(root.starts_with(std::env::temp_dir()));
    assert!(root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("classaimate-qa-restore-"));
    let manifest: PathBuf =
        serde_json::from_slice(&fs::read(root.join("selected.json")).unwrap()).unwrap();
    let target = SqliteStore::open(root.join("target/store.sqlite")).unwrap();
    backup::restore_generation(&target, "qa-restore", &manifest, 354, "announced", false).unwrap();
    panic!("requested failpoint was not reached");
}

#[test]
fn actual_generation_restore_recovers_each_process_death_boundary() {
    for boundary in [
        "prepared",
        "preserved-0",
        "replaced-0",
        "preserved-1",
        "replaced-1",
        "before-commit",
        "after-commit",
        "before-cleanup",
    ] {
        let root = fixture();
        let source = store(&root, "source");
        media(&source, "one", b"new-one");
        media(&source, "two", b"new-two");
        let selected =
            backup::run_with_kind(&source, "qa-restore".into(), "auto_sync", Some(354)).unwrap();
        let manifest = PathBuf::from(selected["manifestPath"].as_str().unwrap());
        fs::write(
            root.join("selected.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let target = store(&root, "target");
        media(&target, "one", b"old-one");
        media(&target, "two", b"old-two");
        drop(source);
        drop(target);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "restore_journal::tests::crash_child",
                "--nocapture",
            ])
            .env("CLASSAIMATE_QA_RESTORE_ROOT", &root)
            .env("CLASSAIMATE_QA_RESTORE_CRASH", boundary)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !root.join("crash-ready").exists()
            && child.try_wait().unwrap().is_none()
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let signalled = root.join("crash-ready").exists();
        if child.try_wait().unwrap().is_none() {
            child.kill().unwrap();
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            signalled && !output.status.success(),
            "boundary {boundary}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let target = SqliteStore::open(root.join("target/store.sqlite")).unwrap();
        let committed = matches!(boundary, "after-commit" | "before-cleanup");
        let conn = target.conn.lock().unwrap();
        assert_eq!(
            conn.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM local_store_restore_journal",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        drop(conn);
        assert_eq!(
            backup::local_sync_state(&target, "qa-restore")
                .unwrap()
                .applied_generation,
            if committed { 354 } else { 0 }
        );
        for id in ["one", "two"] {
            let row = target
                .get_board_media_file("qa-restore".into(), id.into())
                .unwrap();
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(row["dataBase64"].as_str().unwrap())
                .unwrap();
            assert_eq!(
                bytes,
                format!("{}-{id}", if committed { "new" } else { "old" }).as_bytes(),
                "boundary {boundary}"
            );
        }
        drop(target);
        let reopened = SqliteStore::open(root.join("target/store.sqlite")).unwrap();
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn failed_rollback_keeps_original_and_blocks_tenant_writes_not_other_tenants() {
    let root = fixture();
    let target = store(&root, "target");
    media(&target, "one", b"old");
    let base = &target.data_dir;
    fs::create_dir_all(base.join(".restore-staging/qa/staged")).unwrap();
    fs::write(base.join(".restore-staging/qa/staged/one"), b"new").unwrap();
    let intent = Intent {
        operation_id: "qa".into(),
        staging_root: ".restore-staging/qa".into(),
        files: vec![Media {
            staged: ".restore-staging/qa/staged/one".into(),
            target: "board-media/qa-restore/qa-board/one.bin".into(),
            rollback: ".restore-staging/qa/rollback/one".into(),
            incoming_sha256: format!("{:x}", Sha256::digest(b"new")),
            previous_sha256: Some(format!("{:x}", Sha256::digest(b"old"))),
        }],
    };
    let mut conn = target.conn.lock().unwrap();
    prepare(&conn, base, "qa-restore", 1, "qa-root", &intent).unwrap();
    let tx = conn.transaction().unwrap();
    apply(&tx, base, "qa-restore", &intent).unwrap();
    tx.rollback().unwrap();
    // Unexpected bytes are not deleted to make rollback appear successful.
    fs::write(base.join(&intent.files[0].target), b"unexpected").unwrap();
    assert_eq!(
        finish(&conn, base, "qa-restore").unwrap_err(),
        "restore_recovery_required"
    );
    assert_eq!(
        fs::read(base.join(&intent.files[0].rollback)).unwrap(),
        b"old"
    );
    assert!(conn
        .execute(
            "DELETE FROM board_media_files WHERE tenant_id='qa-restore'",
            []
        )
        .is_err());
    assert!(conn
        .execute(
            "DELETE FROM board_media_files WHERE tenant_id='qa-other'",
            []
        )
        .is_ok());
    drop(conn);
    assert!(target.media_access("qa-restore").is_err());
    assert_eq!(
        backup::run_with_kind(&target, "qa-restore".into(), "manual", None).unwrap_err(),
        "restore_recovery_required"
    );
    assert_eq!(
        crate::classaimate_mcp_write_jobs::apply(&target, &json!({"tenantId":"qa-restore"}))
            .unwrap_err(),
        "restore_recovery_required"
    );
    drop(target);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn independent_mcp_connection_waits_for_restore_and_rechecks_recovery() {
    let root = fixture();
    let target = store(&root, "target");
    let other = SqliteStore::open(root.join("target/store.sqlite")).unwrap();
    let guard = target.media_access("qa-restore").unwrap();
    let (sent, received) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result =
            crate::classaimate_mcp_write_jobs::apply(&other, &json!({"tenantId":"qa-restore"}));
        sent.send(result).unwrap();
    });
    assert!(matches!(
        received.recv_timeout(std::time::Duration::from_millis(50)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    let intent = Intent {
        operation_id: "qa-block".into(),
        staging_root: ".restore-staging/qa-block".into(),
        files: vec![],
    };
    let conn = target.conn.lock().unwrap();
    prepare(&conn, &target.data_dir, "qa-restore", 354, "qa", &intent).unwrap();
    drop(conn);
    drop(guard);
    assert_eq!(
        received
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap_err(),
        "restore_recovery_required"
    );
    worker.join().unwrap();
    drop(target);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn restore_journal_and_access_state_never_enter_snapshot() {
    let root = fixture();
    let source = store(&root, "source");
    media(&source, "one", b"qa");
    let selected = backup::run_with_kind(&source, "qa-restore".into(), "manual", None).unwrap();
    let path = PathBuf::from(selected["manifestPath"].as_str().unwrap());
    let manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let db = Connection::open(
        path.parent()
            .unwrap()
            .join(manifest["db"]["relativePath"].as_str().unwrap()),
    )
    .unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='local_store_restore_journal'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    drop(db);
    drop(source);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn io_failure_mid_replace_rolls_back_files_and_keeps_database() {
    let root = fixture();
    let target = store(&root, "target");
    let base = &target.data_dir;
    fs::create_dir_all(base.join(".restore-staging/io/staged")).unwrap();
    fs::create_dir_all(base.join("board-media/qa")).unwrap();
    let mut files = Vec::new();
    for name in ["one", "two"] {
        let staged = PathBuf::from(format!(".restore-staging/io/staged/{name}"));
        let path = PathBuf::from(format!("board-media/qa/{name}"));
        fs::write(base.join(&staged), b"new").unwrap();
        fs::write(base.join(&path), b"old").unwrap();
        files.push(Media {
            staged,
            target: path,
            rollback: format!(".restore-staging/io/rollback/{name}").into(),
            incoming_sha256: format!("{:x}", Sha256::digest(b"new")),
            previous_sha256: Some(format!("{:x}", Sha256::digest(b"old"))),
        });
    }
    let intent = Intent {
        operation_id: "io".into(),
        staging_root: ".restore-staging/io".into(),
        files,
    };
    let mut conn = target.conn.lock().unwrap();
    prepare(&conn, base, "qa-restore", 354, "qa", &intent).unwrap();
    fs::remove_file(base.join(&intent.files[1].staged)).unwrap();
    let tx = conn.transaction().unwrap();
    assert!(apply(&tx, base, "qa-restore", &intent).is_err());
    tx.rollback().unwrap();
    finish(&conn, base, "qa-restore").unwrap();
    for file in &intent.files {
        assert_eq!(fs::read(base.join(&file.target)).unwrap(), b"old");
    }
    ready(&conn, "qa-restore").unwrap();
    drop(conn);
    drop(target);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sqlite_full_cannot_persist_intent_or_start_file_replacement() {
    let root = fixture();
    let target = store(&root, "target");
    let conn = target.conn.lock().unwrap();
    let pages: i64 = conn
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .unwrap();
    conn.execute_batch(&format!("PRAGMA max_page_count={pages}"))
        .unwrap();
    let intent = Intent {
        operation_id: "x".repeat(2_000_000),
        staging_root: ".restore-staging/full".into(),
        files: vec![],
    };
    assert!(prepare(&conn, &target.data_dir, "qa-restore", 354, "qa", &intent).is_err());
    ready(&conn, "qa-restore").unwrap();
    assert!(!target.data_dir.join(".restore-staging/full").exists());
    drop(conn);
    drop(target);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[test]
fn sharing_denied_preserves_target_and_recovery_material() {
    use std::os::windows::fs::OpenOptionsExt;
    let root = fixture();
    let target = store(&root, "target");
    let base = &target.data_dir;
    fs::create_dir_all(base.join(".restore-staging/denied/staged")).unwrap();
    fs::create_dir_all(base.join("board-media/qa")).unwrap();
    fs::write(base.join(".restore-staging/denied/staged/one"), b"new").unwrap();
    fs::write(base.join("board-media/qa/one"), b"old").unwrap();
    let intent = Intent {
        operation_id: "denied".into(),
        staging_root: ".restore-staging/denied".into(),
        files: vec![Media {
            staged: ".restore-staging/denied/staged/one".into(),
            target: "board-media/qa/one".into(),
            rollback: ".restore-staging/denied/rollback/one".into(),
            incoming_sha256: format!("{:x}", Sha256::digest(b"new")),
            previous_sha256: Some(format!("{:x}", Sha256::digest(b"old"))),
        }],
    };
    let mut conn = target.conn.lock().unwrap();
    prepare(&conn, base, "qa-restore", 354, "qa", &intent).unwrap();
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(base.join(&intent.files[0].target))
        .unwrap();
    let tx = conn.transaction().unwrap();
    assert!(apply(&tx, base, "qa-restore", &intent).is_err());
    tx.rollback().unwrap();
    assert!(finish(&conn, base, "qa-restore").is_err()); // Cannot verify a locked original.
    assert!(base.join(&intent.files[0].staged).exists());
    drop(held);
    finish(&conn, base, "qa-restore").unwrap();
    assert_eq!(
        fs::read(base.join(&intent.files[0].target)).unwrap(),
        b"old"
    );
    drop(conn);
    drop(target);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn corrupted_operation_identity_never_authorizes_cleanup() {
    let root = fixture();
    let target = store(&root, "target");
    let base = &target.data_dir;
    fs::create_dir_all(base.join(".restore-staging/identity")).unwrap();
    fs::write(base.join(".restore-staging/identity/original"), b"keep").unwrap();
    let intent = Intent {
        operation_id: "identity".into(),
        staging_root: ".restore-staging/identity".into(),
        files: vec![],
    };
    let conn = target.conn.lock().unwrap();
    prepare(&conn, base, "qa-restore", 354, "qa", &intent).unwrap();
    assert!(receipt(&conn, "qa-restore", &intent).is_err());
    conn.execute("UPDATE local_store_restore_journal SET operation_id='different' WHERE tenant_id='qa-restore'", []).unwrap();
    assert!(apply(&conn, base, "qa-restore", &intent).is_err());
    assert!(finish(&conn, base, "qa-restore").is_err());
    assert_eq!(
        fs::read(base.join(".restore-staging/identity/original")).unwrap(),
        b"keep"
    );
    assert!(ready(&conn, "qa-restore").is_err());
    drop(conn);
    drop(target);
    fs::remove_dir_all(root).unwrap();
}
