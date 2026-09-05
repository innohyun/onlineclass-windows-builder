use super::*;

fn fixture() -> (PathBuf, SqliteStore) {
    let root = std::env::temp_dir().join(format!(
        "classaimate-backup-optimization-{}",
        crate::random_url_token()
    ));
    fs::create_dir_all(root.join("local")).unwrap();
    let store = SqliteStore::open(root.join("local/db.sqlite")).unwrap();
    set_folder(
        &store,
        "qa-sync".into(),
        root.join("backups").to_string_lossy().into(),
    )
    .unwrap();
    (root, store)
}

fn edit(store: &SqliteStore, text: &str, timestamp: i64) {
    store
        .upsert_observation(
            json!({"tenantId":"qa-sync","docId":"record","dateKey":"2026-09-05",
        "period":1,"studentCode":"1","observation":text,"updatedAtMs":timestamp}),
        )
        .unwrap();
}

fn dirty_version(store: &SqliteStore) -> (i64, i64, i64) {
    store.conn.lock().unwrap().query_row("SELECT record_version,changed_generation,tombstone
        FROM local_store_device_sync_records WHERE tenant_id='qa-sync' AND table_name='lesson_observations'",
        [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap()
}

#[test]
fn backup_noop_and_published_capture_never_clear_later_edits() {
    let (root, store) = fixture();
    edit(&store, "before", 1);
    seed_sync_records(&store, "qa-sync").unwrap();
    let before = local_sync_state(&store, "qa-sync").unwrap();
    edit(&store, "during hash", 2);
    mark_sync_unchanged(&store, "qa-sync", 7, before.change_sequence).unwrap();
    assert_eq!(dirty_version(&store).1, 0);
    assert!(
        local_sync_state(&store, "qa-sync")
            .unwrap()
            .first_dirty_at_ms
            > 0
    );
    let snapshot = run_with_kind(&store, "qa-sync".into(), "auto_sync", Some(8)).unwrap();
    edit(&store, "after capture", 3);
    mark_sync_published(
        &store,
        "qa-sync",
        8,
        Path::new(snapshot["manifestPath"].as_str().unwrap()),
        snapshot["contentSha256"].as_str().unwrap(),
        "announced",
        snapshot["capturedSequence"].as_i64().unwrap(),
    )
    .unwrap();
    assert_eq!(dirty_version(&store).1, 0);
    assert!(
        local_sync_state(&store, "qa-sync")
            .unwrap()
            .first_dirty_at_ms
            > 0
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn backup_capture_database_and_index_survive_delete_after_capture() {
    let (root, store) = fixture();
    edit(&store, "captured", 1);
    let db = root.join("capture.sqlite");
    let captured = capture::export(&store, "qa-sync", &db, 1).unwrap();
    store
        .conn
        .lock()
        .unwrap()
        .execute(
            "DELETE FROM lesson_observations WHERE tenant_id='qa-sync'",
            [],
        )
        .unwrap();
    let frozen = Connection::open(&db).unwrap();
    let count: i64 = frozen
        .query_row("SELECT COUNT(*) FROM lesson_observations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(captured.sync["records"][0]["tombstone"], false);
    assert_eq!(dirty_version(&store).2, 1);
    assert!(local_sync_state(&store, "qa-sync").unwrap().change_sequence > captured.sequence);
    assert!(!table_exists(&frozen, "local_store_device_sync_runtime").unwrap());
    drop(frozen);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn backup_frozen_content_matches_live_content_and_detects_replaced_media() {
    let (root, store) = fixture();
    edit(&store, "content", 1);
    store
        .upsert_board_media(
            json!({"tenantId":"qa-sync","boardId":"board","postId":"post",
        "mediaId":"media","fileName":"fixture.txt","dataBase64":"Zml4dHVyZQ=="}),
        )
        .unwrap();
    let snapshot = run_with_kind(&store, "qa-sync".into(), "auto_sync", Some(1)).unwrap();
    assert_eq!(
        snapshot["contentSha256"],
        tenant_content_sha256(&store, "qa-sync").unwrap()
    );
    assert_eq!(snapshot["ok"], true);
    // A source file modified after its DB locator capture must not pass the stamp guard.
    let captured = capture::export(&store, "qa-sync", &root.join("captured.sqlite"), 2).unwrap();
    let row = &captured.media[0];
    fs::write(store.data_dir.join(&row.local_path), b"different length").unwrap();
    assert_ne!(
        captured.media_stamps[&row.media_id],
        capture::file_stamp(&store.data_dir.join(&row.local_path)).unwrap()
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn backup_rejects_attachment_bytes_that_disagree_with_captured_metadata() {
    let (root, store) = fixture();
    store.upsert_work_note(json!({"tenantId":"qa-sync","pageId":"page","title":"Fixture","blocks":[],"markdown":""})).unwrap();
    crate::work_note_attachments::save(
        &store,
        "qa-sync".into(),
        "file".into(),
        "page".into(),
        "block".into(),
        "fixture.txt".into(),
        "text/plain".into(),
        &mut std::io::Cursor::new(b"old".to_vec()),
    )
    .unwrap();
    let path = list_work_note_attachment_rows(&store, "qa-sync")
        .unwrap()
        .remove(0)
        .local_path;
    fs::write(store.data_dir.join(path), b"new").unwrap();
    assert_eq!(
        run_now(&store, "qa-sync".into()).unwrap_err(),
        "backup_capture_attachment_changed"
    );
    assert!(
        manifest_paths_in_dir(&configured_tenant_dir(&store, "qa-sync").unwrap())
            .unwrap()
            .is_empty()
    );
    let snapshots = configured_tenant_dir(&store, "qa-sync")
        .unwrap()
        .join("snapshots");
    assert_eq!(fs::read_dir(snapshots).unwrap().count(), 0);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn backup_mcp_revision_checked_write_stays_dirty_after_previous_capture_is_published() {
    let (root, store) = fixture();
    store
        .upsert_work_note(
            json!({"tenantId":"qa-sync","pageId":"page","title":"Fixture",
        "blocks":[],"markdown":"before","createdAtMs":10,"updatedAtMs":10}),
        )
        .unwrap();
    let snapshot = run_with_kind(&store, "qa-sync".into(), "auto_sync", Some(1)).unwrap();
    let request = json!({"tenantId":"qa-sync","receiptId":"fixture-receipt","operation":"materials_restructure_page",
        "requestSha256":"a".repeat(64),"data":{"workspace":"work_materials","pageRef":"page","expectedRevision":10,
            "blocks":[{"id":"heading","type":"heading","attrs":{"level":1},"content":[{"type":"text","text":"[AI 정리본]"}]}],"markdown":"# [AI 정리본]"}});
    let first = crate::classaimate_mcp_write_jobs::apply(&store, &request).unwrap();
    assert_eq!(first["replayed"], false);
    let sequence = local_sync_state(&store, "qa-sync").unwrap().change_sequence;
    assert_eq!(
        crate::classaimate_mcp_write_jobs::apply(&store, &request).unwrap()["replayed"],
        true
    );
    assert_eq!(
        local_sync_state(&store, "qa-sync").unwrap().change_sequence,
        sequence
    );
    mark_sync_published(
        &store,
        "qa-sync",
        1,
        Path::new(snapshot["manifestPath"].as_str().unwrap()),
        snapshot["contentSha256"].as_str().unwrap(),
        "announced",
        snapshot["capturedSequence"].as_i64().unwrap(),
    )
    .unwrap();
    assert!(
        local_sync_state(&store, "qa-sync")
            .unwrap()
            .first_dirty_at_ms
            > 0
    );
    let frozen = Connection::open(snapshot["dbPath"].as_str().unwrap()).unwrap();
    assert!(!table_exists(&frozen, "classaimate_mcp_local_write_receipts").unwrap());
    drop(frozen);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn backup_archive_union_keeps_local_only_archive_dirty_after_receive() {
    let (root, store) = fixture();
    let tenant = format!("qa-archive-{}", crate::random_url_token());
    set_folder(
        &store,
        tenant.clone(),
        root.join("backups").to_string_lossy().into(),
    )
    .unwrap();
    store
        .upsert_observation(
            json!({"tenantId":tenant,"docId":"record","dateKey":"2026-09-05",
        "period":1,"studentCode":"1","observation":"snapshot","updatedAtMs":1}),
        )
        .unwrap();
    let snapshot = run_with_kind(&store, tenant.clone(), "auto_sync", Some(1)).unwrap();
    let archive_id = format!("fixture-{}", crate::random_url_token());
    let archives = crate::shared_archive::open_db().unwrap();
    archives.execute("INSERT INTO shared_archives VALUES (?1,?2,'board','fixture','Fixture',?3,0,0,0,1,2,3,'{}')",
        params![archive_id,tenant,"a".repeat(64)]).unwrap();
    mark_external_sync_dirty(&store, &tenant).unwrap();
    restore_generation(
        &store,
        &tenant,
        Path::new(snapshot["manifestPath"].as_str().unwrap()),
        1,
        "announced",
        false,
    )
    .unwrap();
    assert!(local_sync_state(&store, &tenant).unwrap().first_dirty_at_ms > 0);
    assert!(crate::shared_archive_sync::has_local_only_references(
        &tenant,
        Some(&json!({"records":[]}))
    )
    .unwrap());
    drop(archives);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn backup_artifact_root_is_recomputed_after_individual_hashes_match() {
    let (root, store) = fixture();
    edit(&store, "root", 1);
    let snapshot = run_with_kind(&store, "qa-sync".into(), "auto_sync", Some(1)).unwrap();
    let path = PathBuf::from(snapshot["manifestPath"].as_str().unwrap());
    let mut manifest = read_manifest(&path).unwrap();
    let forged = "f".repeat(64);
    manifest["artifactSetSha256"] = json!(forged);
    let commit_path = path.parent().unwrap().join("commit.json");
    let mut commit = read_manifest(&commit_path).unwrap();
    commit["artifactSetSha256"] = json!(forged);
    fs::write(&commit_path, commit.to_string()).unwrap();
    assert_eq!(
        verify_snapshot_artifacts(&path, &manifest, "qa-sync", Some(1), &forged).unwrap_err(),
        "backup_artifact_set_digest_mismatch"
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn backup_idle_status_is_read_only_and_noop_update_has_no_change_sequence() {
    let (root, store) = fixture();
    edit(&store, "idle", 1);
    seed_sync_records(&store, "qa-sync").unwrap();
    let sequence = local_sync_state(&store, "qa-sync").unwrap().change_sequence;
    let total: i64 = store
        .conn
        .lock()
        .unwrap()
        .query_row("SELECT total_changes()", [], |r| r.get(0))
        .unwrap();
    for _ in 0..40 {
        // ten minutes at the existing 15-second device tick
        local_sync_state(&store, "qa-sync").unwrap();
        seed_sync_records(&store, "qa-sync").unwrap();
        connection_status(&store, "qa-sync").unwrap();
    }
    let after: i64 = store
        .conn
        .lock()
        .unwrap()
        .query_row("SELECT total_changes()", [], |r| r.get(0))
        .unwrap();
    assert_eq!(total, after);
    store
        .conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE lesson_observations SET payload_json=payload_json WHERE tenant_id='qa-sync'",
            [],
        )
        .unwrap();
    assert_eq!(
        local_sync_state(&store, "qa-sync").unwrap().change_sequence,
        sequence
    );
    assert_eq!(highest_local_generation(&store, "qa-sync").unwrap(), 0);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn backup_retry_clock_persists_and_ack_cache_is_device_scoped() {
    let (root, store) = fixture();
    let mut now = 1_000;
    for delay in [30_000, 60_000, 120_000, 300_000, 300_000] {
        update_retry(&store, "qa-sync", true, now).unwrap();
        assert!(retry_pending(&store, "qa-sync").unwrap());
        assert!(!retry_due(&store, "qa-sync", now + delay - 1).unwrap());
        now += delay;
        assert!(retry_due(&store, "qa-sync", now).unwrap());
    }
    remember_ack(&store, "qa-sync", 1, "first-device", "root").unwrap();
    assert!(acknowledged_locally(&store, "qa-sync", 1, "first-device", "root").unwrap());
    assert!(!acknowledged_locally(&store, "qa-sync", 1, "new-device", "root").unwrap());
    update_retry(&store, "qa-sync", false, now).unwrap();
    assert!(!retry_pending(&store, "qa-sync").unwrap());
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn backup_server_and_pending_pins_include_recovery_and_fail_closed_when_stale() {
    let (root, store) = fixture();
    mark_sync_latest(&store, "qa-sync", 10, "announced").unwrap();
    assert!(maintenance::require_pin_context(&store, "qa-sync", now_ms()).is_err());
    remember_checkpoint_pins(
        &store,
        "qa-sync",
        &json!({
            "checkpoint":{"generation":10,"recoveryOfGeneration":4},
            "latestVerifiedCheckpoint":{"generation":8,"recoveryOfGeneration":3}
        }),
    )
    .unwrap();
    save_pending_publication(
        &store,
        "qa-sync",
        Some(&json!({"baseGeneration":10,"snapshot":{"generation":11}})),
    )
    .unwrap();
    assert_eq!(
        pinned_sync_generations(&store, "qa-sync").unwrap(),
        HashSet::from([3, 4, 8, 10, 11])
    );
    assert!(maintenance::require_pin_context(&store, "qa-sync", now_ms()).is_ok());
    assert!(
        maintenance::require_pin_context(&store, "qa-sync", now_ms() + 7 * 60 * 60 * 1000).is_err()
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn backup_new_install_cannot_prune_existing_shared_root_without_server_pins() {
    let (root, source) = fixture();
    edit(&source, "shared", 1);
    run_with_kind(&source, "qa-sync".into(), "auto_sync", Some(12)).unwrap();
    fs::create_dir_all(root.join("fresh-install")).unwrap();
    let fresh = SqliteStore::open(root.join("fresh-install/db.sqlite")).unwrap();
    set_folder(
        &fresh,
        "qa-sync".into(),
        root.join("backups").to_string_lossy().into(),
    )
    .unwrap();
    assert_eq!(
        local_sync_state(&fresh, "qa-sync")
            .unwrap()
            .latest_generation,
        0
    );
    assert_eq!(
        maintenance::require_pin_context(&fresh, "qa-sync", now_ms()).unwrap_err(),
        "backup_sync_pin_context_stale"
    );
    drop(fresh);
    drop(source);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn backup_maintenance_error_does_not_invalidate_committed_snapshot() {
    let (root, store) = fixture();
    edit(&store, "safe", 1);
    mark_sync_latest(&store, "qa-sync", 5, "announced").unwrap();
    let snapshot = run_now(&store, "qa-sync".into()).unwrap();
    assert_eq!(snapshot["ok"], true);
    assert_eq!(
        snapshot["maintenance"]["error"],
        "backup_sync_pin_context_stale"
    );
    let path = PathBuf::from(snapshot["manifestPath"].as_str().unwrap());
    authoritative_restore_manifest(&path, &read_manifest(&path).unwrap(), "qa-sync").unwrap();
    assert_eq!(
        maintenance::run_if_due(&store, "qa-sync", now_ms(), false).unwrap()["skipped"],
        true
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}
