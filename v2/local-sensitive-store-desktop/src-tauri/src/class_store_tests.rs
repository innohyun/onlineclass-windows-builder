use super::*;
fn fixture() -> (PathBuf, SqliteStore) {
    let root = std::env::temp_dir().join(format!("class-store-{}", crate::random_url_token()));
    fs::create_dir_all(&root).unwrap();
    let store = SqliteStore::open(root.join(DB_FILE_NAME)).unwrap();
    (root, store)
}
#[test]
fn migration_preserves_drafts_runtime_and_isolates_identical_ids() {
    let (root, store) = fixture();
    {
        let conn = store.conn.lock().unwrap();
        for tenant in ["class-2026", "class-2027"] {
            conn.execute(
                "INSERT INTO work_note_local_drafts VALUES(?1,'same-page',2,?2,10)",
                params![tenant, format!("{{\"tenant\":\"{tenant}\"}}")],
            )
            .unwrap();
            conn.execute("INSERT INTO local_store_device_sync_runtime(tenant_id,pending_json,retry_count) VALUES(?1,'{\"receipt\":\"pending\"}',3)",params![tenant]).unwrap();
            conn.execute("INSERT INTO classaimate_mcp_local_write_receipts VALUES(?1,'old-receipt','observation','sha','{}','same-ref',0)",params![tenant]).unwrap();
        }
    }
    let first = store.for_tenant("class-2026").unwrap();
    let second = store.for_tenant("class-2027").unwrap();
    assert_ne!(first.db_path, second.db_path);
    assert_eq!(
        first.class_storage_status("class-2027").unwrap_err(),
        "tenant_scope_mismatch"
    );
    for (scoped, tenant) in [(&first, "class-2026"), (&second, "class-2027")] {
        let conn = scoped.conn.lock().unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM work_note_local_drafts", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1);
        let runtime: i64 = conn
            .query_row(
                "SELECT retry_count FROM local_store_device_sync_runtime WHERE tenant_id=?1",
                params![tenant],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(runtime, 3);
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM classaimate_mcp_local_write_receipts",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
    assert_eq!(
        store
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM work_note_local_drafts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    drop(first);
    drop(second);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

fn add_page_and_attachment(store: &SqliteStore, tenant: &str, payload: &[u8]) -> String {
    let relative = format!("work-note-attachments/{tenant}/same-file.bin");
    let sha = format!("{:x}", Sha256::digest(payload));
    let conn = store.conn.lock().unwrap();
    conn.execute(
        "INSERT INTO work_note_pages VALUES(?1,'same-page',NULL,?2,'',0,'{}','[]',?2,10,20)",
        params![tenant, format!("marker-{tenant}")],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO work_note_pages_fts VALUES(?1,'same-page',?2,?2)",
        params![tenant, format!("marker-{tenant}")],
    )
    .unwrap();
    conn.execute("INSERT INTO work_note_attachments VALUES(?1,'same-file','same-page','block','file.bin','application/octet-stream',?2,?3,?4,10,20)",params![tenant,payload.len() as i64,sha,relative]).unwrap();
    relative
}
#[test]
fn same_page_ids_search_indexes_and_attachment_files_are_class_owned() {
    let (root, store) = fixture();
    for tenant in ["class-2026", "class-2027"] {
        let relative = add_page_and_attachment(&store, tenant, tenant.as_bytes());
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, tenant).unwrap();
    }
    for tenant in ["class-2026", "class-2027"] {
        let scoped = store.for_tenant(tenant).unwrap();
        let own = scoped
            .list_work_notes(tenant.into(), format!("marker-{tenant}"))
            .unwrap();
        assert_eq!(own.len(), 1);
        let conn = scoped.conn.lock().unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM work_note_pages_fts", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let relative: String = conn
            .query_row("SELECT local_path FROM work_note_attachments", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            fs::read(scoped.data_dir.join(relative)).unwrap(),
            tenant.as_bytes()
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM main.teaching_sources", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            0
        );
    }
    drop(store);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn missing_and_corrupt_attachments_never_activate_and_retry_preserves_protection() {
    let (root, store) = fixture();
    let relative = add_page_and_attachment(&store, "class-a", b"valid");
    assert!(store.for_tenant("class-a").is_err());
    assert!(!class_directory(&root, "class-a").unwrap().exists());
    let protected = root.join("class-migrations/legacy-protection.sqlite");
    let before = crate::restore_journal::digest(&protected).unwrap();
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"bad!!").unwrap();
    assert!(store.for_tenant("class-a").is_err());
    assert!(!class_directory(&root, "class-a").unwrap().exists());
    fs::write(&path, b"valid").unwrap();
    let scoped = store.for_tenant("class-a").unwrap();
    assert_eq!(before, crate::restore_journal::digest(&protected).unwrap());
    assert!(root.join(DB_FILE_NAME).is_file());
    assert!(fs::read_dir(root.join("class-migrations"))
        .unwrap()
        .any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".interrupted-")));
    drop(scoped);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn interrupted_staging_is_retained_and_empty_class_does_not_clone_personal_data() {
    let (root, store) = fixture();
    let recovery = root.join("class-migrations");
    fs::create_dir_all(&recovery).unwrap();
    let key = format!("{:x}", Sha256::digest(b"next-class"));
    let stage = recovery.join(format!("{key}.staging"));
    fs::create_dir_all(&stage).unwrap();
    fs::write(stage.join("interrupted-evidence"), b"retained").unwrap();
    let scoped = store.for_tenant("next-class").unwrap();
    assert!(!recovery.join("legacy-protection.sqlite").exists());
    let retained = fs::read_dir(&recovery)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .contains(".interrupted-")
        })
        .unwrap();
    assert_eq!(
        fs::read(retained.join("interrupted-evidence")).unwrap(),
        b"retained"
    );
    assert!(scoped.class_storage_status("next-class").unwrap()["ready"]
        .as_bool()
        .unwrap());
    drop(scoped);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn archive_records_and_files_are_verified_before_class_activation() {
    let (root, store) = fixture();
    let archive = crate::shared_archive::open_db_at(&root).unwrap();
    for tenant in ["class-2026", "class-2027"] {
        let id = format!("archive-{tenant}");
        let path = root.join("shared-archive-files").join(&id).join("same.bin");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, tenant).unwrap();
        let sha = format!("{:x}", Sha256::digest(tenant.as_bytes()));
        archive.execute("INSERT INTO shared_archives VALUES(?1,?2,'board','same-board',?2,'manifest',1,1,?3,10,20,30,'{}')",params![id,tenant,tenant.len() as i64]).unwrap();
        archive
            .execute(
                "INSERT INTO shared_archive_records VALUES(?1,0,'board_post',?2,?3)",
                params![id, tenant, sha],
            )
            .unwrap();
        archive.execute("INSERT INTO shared_archive_files VALUES(?1,0,'same.bin','application/octet-stream',?2,?3,?4)",params![id,tenant.len() as i64,sha,path.to_string_lossy().as_ref()]).unwrap();
    }
    drop(archive);
    for tenant in ["class-2026", "class-2027"] {
        let scoped = store.for_tenant(tenant).unwrap();
        let archived = crate::shared_archive::open_db_at(&scoped.data_dir).unwrap();
        assert_eq!(
            archived
                .query_row("SELECT COUNT(*) FROM shared_archives", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            archived
                .query_row(
                    "SELECT payload_json FROM shared_archive_records",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            tenant
        );
        let path: String = archived
            .query_row("SELECT local_path FROM shared_archive_files", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(Path::new(&path).starts_with(&scoped.data_dir));
        assert_eq!(fs::read(path).unwrap(), tenant.as_bytes());
        let receipt: Value =
            serde_json::from_slice(&fs::read(scoped.data_dir.join("class-storage.json")).unwrap())
                .unwrap();
        assert_eq!(
            receipt["archives"]["tables"]["shared_archive_records"]["count"],
            1
        );
    }
    drop(store);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn filesystem_keys_do_not_collide_on_case_insensitive_windows_paths() {
    let root = Path::new("root");
    for (left, right) in [
        ("Class-A", "class-a"),
        ("CON", "C43ON"),
        ("con", "CON"),
        ("a:b", "a_3Ab"),
        ("a.", "a"),
    ] {
        assert_ne!(
            class_directory(root, left)
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase(),
            class_directory(root, right)
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase()
        );
    }
}

#[test]
fn unknown_tables_and_traversal_never_activate_a_class() {
    let (root, store) = fixture();
    assert!(store.for_tenant("../other").is_err());
    store
        .conn
        .lock()
        .unwrap()
        .execute_batch("CREATE TABLE future_record(tenant_id TEXT,payload TEXT)")
        .unwrap();
    assert!(store
        .for_tenant("class-a")
        .err()
        .unwrap()
        .starts_with("class_storage_unknown_table:"));
    assert!(!class_directory(&root, "class-a").unwrap().exists());
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn personal_teaching_library_keeps_owner_access_and_original_backup_home() {
    let (root, store) = fixture();
    let owner = "teacher-a";
    let text = "original teaching source";
    let relative = format!(
        "teaching-sources/{}/objects/source-one/original.txt",
        crate::teaching_sources::actor_folder(owner)
    );
    let path = root.join(&relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    let sha = format!("{:x}", Sha256::digest(text.as_bytes()));
    {
        let conn = store.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO teaching_source_actor_homes VALUES(?1,'class-2026',10,10)",
            [owner],
        )
        .unwrap();
        conn.execute("INSERT INTO teaching_sources(owner_uid,source_id,origin_tenant_id,source_type,grade,semester_scope,subject_code,title,publisher,original_file_name,content_type,byte_size,sha256,local_path,extraction_status,lifecycle_status,extractor_version,revision,created_at_ms,updated_at_ms) VALUES(?1,'source-one','class-2026','reference','3','full_year','social','Original source','','original.txt','text/plain',?2,?3,?4,'ready','active','text-v1',1,10,10)",params![owner,text.len() as i64,sha,relative]).unwrap();
        conn.execute("INSERT INTO teaching_source_chunks VALUES(?1,'source-one','chunk-one',0,NULL,NULL,'','',?2,?3,1,10,10)",params![owner,text,sha]).unwrap();
    }
    let next = store.for_tenant("class-2027").unwrap();
    let common = next.shared_store().unwrap();
    let request = json!({"refs":[{"sourceRef":"source-one","chunkRef":"chunk-one","sourceRevision":1,"chunkRevision":1,"fileSha256":sha}],"maxChars":1000});
    let read = crate::teaching_sources::mcp_chunks(&common, "class-2027", owner, &request).unwrap();
    assert_eq!(read["chunks"][0]["text"], text);
    assert!(
        crate::teaching_sources::mcp_chunks(&common, "class-2027", "other-teacher", &request)
            .is_err()
    );
    let conn = next.conn.lock().unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM main.teaching_sources", [], |row| row
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT backup_tenant_id FROM shared.teaching_source_actor_homes WHERE owner_uid=?1",
            [owner],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "class-2026"
    );
    for (tenant, expected) in [("class-2027", 0usize), ("class-2026", 1usize)] {
        conn.execute_batch("ATTACH DATABASE ':memory:' AS backup")
            .unwrap();
        conn.execute_batch(&crate::teaching_source_backup::schema("backup."))
            .unwrap();
        let files = crate::teaching_source_backup::capture(&conn, tenant).unwrap();
        assert_eq!(files.len(), expected);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM backup.teaching_sources", [], |row| {
                row.get::<_, usize>(0)
            })
            .unwrap(),
            expected
        );
        if let Some(file) = files.first() {
            assert_eq!(
                crate::teaching_source_backup::source_path(&next, owner, &file.local_path).unwrap(),
                path
            );
        }
        conn.execute_batch("DETACH DATABASE backup").unwrap();
    }
    drop(conn);
    drop(common);
    drop(next);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn initialized_components_resolve_during_canonical_write_without_schema_mutation() {
    let (root, store) = fixture();
    let class = store.for_tenant("class-2026").unwrap();
    let common = class.shared_store().unwrap();
    {
        let mut conn = store.conn.lock().unwrap();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        tx.execute(
            "INSERT INTO teaching_source_actor_homes VALUES('teacher-a','class-2026',10,10)",
            [],
        )
        .unwrap();
        let resolved = class.shared_store().unwrap();
        assert_eq!(resolved.db_path, store.db_path);
        drop(resolved);
        tx.rollback().unwrap();
    }
    drop(common);
    drop(class);
    // A verified class also resolves without trigger upgrades while its original
    // connection owns a write transaction.
    let class = store.for_tenant("class-2026").unwrap();
    let db = class.db_path.clone();
    drop(class);
    let mut writer = Connection::open(&db).unwrap();
    let tx = writer
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let resolved = store.for_tenant("class-2026").unwrap();
    assert_eq!(resolved.db_path, db);
    tx.rollback().unwrap();
    drop(writer);
    drop(resolved);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn personal_import_progress_changes_preserve_class_migration_and_home_backup() {
    let (root, store) = fixture();
    {
        let conn = store.conn.lock().unwrap();
        for tenant in ["class-2026", "class-2027"] {
            conn.execute(
                "INSERT INTO work_note_local_drafts VALUES(?1,'draft',1,'{}',10)",
                [tenant],
            )
            .unwrap();
            conn.execute("INSERT INTO local_import_runs VALUES(?1,'class-import','manual','completed','{}',10,10)",[tenant]).unwrap();
        }
        conn.execute("INSERT INTO local_import_runs VALUES('class-2027','teaching-source:actor:source','teaching_source','started','{}',10,10)",[]).unwrap();
    }
    let old = store.for_tenant("class-2026").unwrap();
    let protection = root.join("class-migrations/legacy-protection.sqlite");
    let protected_sha = crate::restore_journal::digest(&protection).unwrap();
    store.conn.lock().unwrap().execute("UPDATE local_import_runs SET status='completed',finished_at_ms=20 WHERE kind='teaching_source'",[]).unwrap();
    let next = store.for_tenant("class-2027").unwrap();
    assert_eq!(
        crate::restore_journal::digest(&protection).unwrap(),
        protected_sha
    );
    assert_eq!(
        next.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT status FROM main.local_import_runs WHERE kind='teaching_source'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "started"
    );
    // The protected legacy copy is retained as evidence, while all live views
    // and wire snapshots use canonical current common progress.
    let backup_root =
        root.with_file_name(format!("class-store-backups-{}", crate::random_url_token()));
    crate::backup::set_folder(
        &next,
        "class-2027".into(),
        backup_root.to_string_lossy().into(),
    )
    .unwrap();
    let snapshot =
        crate::backup::run_with_kind(&next, "class-2027".into(), "auto_sync", Some(1)).unwrap();
    let manifest_path = PathBuf::from(snapshot["manifestPath"].as_str().unwrap());
    let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let snapshot_conn = Connection::open(
        manifest_path
            .parent()
            .unwrap()
            .join(manifest["db"]["relativePath"].as_str().unwrap()),
    )
    .unwrap();
    assert_eq!(
        snapshot_conn
            .query_row(
                "SELECT status FROM local_import_runs WHERE kind='teaching_source'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "completed"
    );
    assert_eq!(
        snapshot_conn
            .query_row("SELECT COUNT(*) FROM local_import_runs", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        manifest["sync"]["records"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["table"] == "local_import_runs")
            .count(),
        2
    );
    crate::backup::mark_sync_published(
        &next,
        "class-2027",
        1,
        &manifest_path,
        snapshot["contentSha256"].as_str().unwrap(),
        "announced",
        snapshot["capturedSequence"].as_i64().unwrap(),
    )
    .unwrap();
    assert_eq!(
        crate::backup::local_sync_state(&next, "class-2027")
            .unwrap()
            .first_dirty_at_ms,
        0
    );
    {
        let conn = store.conn.lock().unwrap();
        assert_eq!(conn.query_row("SELECT changed_generation FROM local_store_device_sync_records WHERE tenant_id='class-2027' AND table_name='local_import_runs' AND record_key=json_array('teaching-source:actor:source')",[],|row|row.get::<_,i64>(0)).unwrap(),1);
        conn.execute(
            "UPDATE local_import_runs SET finished_at_ms=30 WHERE kind='teaching_source'",
            [],
        )
        .unwrap();
        assert_eq!(conn.query_row("SELECT dirty_base_generation FROM local_store_device_sync_records WHERE tenant_id='class-2027' AND table_name='local_import_runs' AND record_key=json_array('teaching-source:actor:source')",[],|row|row.get::<_,i64>(0)).unwrap(),1);
    }
    crate::backup::mark_external_sync_dirty(&store, "class-2027").unwrap();
    assert!(
        crate::backup::local_sync_state(&next, "class-2027")
            .unwrap()
            .first_dirty_at_ms
            > 0
    );
    drop(snapshot_conn);
    drop(next);
    drop(old);
    drop(store);
    fs::remove_dir_all(backup_root).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn activated_class_directory_loss_never_falls_back_even_with_a_cached_handle() {
    let (root, store) = fixture();
    store.conn.lock().unwrap().execute("INSERT INTO work_note_local_drafts VALUES('class-2026','draft',1,'{\"value\":\"legacy\"}',10)",[]).unwrap();
    let class = store.for_tenant("class-2026").unwrap();
    class.conn.lock().unwrap().execute("UPDATE work_note_local_drafts SET generation=2,payload_json='{\"value\":\"after-migration\"}'",[]).unwrap();
    let marker = activation_path(&root, "class-2026");
    assert!(marker.is_file());
    let marker_sha = crate::restore_journal::digest(&marker).unwrap();
    let lost = class.data_dir.clone();
    let retained = root.join("lost-class-evidence");
    let mut cached = Some(class);
    // Windows denies moving SQLite files with active handles; other supported
    // platforms exercise the same-process cached-handle disappearance too.
    #[cfg(target_os = "windows")]
    drop(cached.take());
    fs::rename(&lost, &retained).unwrap();
    // An open SQLite handle must not make a missing physical class look ready.
    assert_eq!(
        store.for_tenant("class-2026").err().unwrap(),
        "class_storage_activated_component_missing"
    );
    if let Some(class) = cached.as_ref() {
        assert_eq!(
            class.for_tenant("class-2026").err().unwrap(),
            "class_storage_activated_component_missing"
        );
    }
    assert!(!lost.exists());
    assert_eq!(crate::restore_journal::digest(&marker).unwrap(), marker_sha);
    drop(cached.take());
    drop(store);
    let reopened = SqliteStore::open(root.join(DB_FILE_NAME)).unwrap();
    assert_eq!(
        reopened.for_tenant("class-2026").err().unwrap(),
        "class_storage_activated_component_missing"
    );
    assert!(!lost.exists());
    // Restoring the exact class directory restores its newer content.
    fs::rename(&retained, &lost).unwrap();
    let recovered = reopened.for_tenant("class-2026").unwrap();
    assert_eq!(
        recovered
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT generation FROM work_note_local_drafts", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        2
    );
    drop(recovered);
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn verified_directory_can_finish_activation_registration_without_recopied_data() {
    let (root, store) = fixture();
    let class = store.for_tenant("class-2027").unwrap();
    class
        .conn
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO work_note_local_drafts VALUES('class-2027','new',9,'{}',10)",
            [],
        )
        .unwrap();
    let marker = activation_path(&root, "class-2027");
    fs::remove_file(&marker).unwrap();
    drop(class);
    let recovered = store.for_tenant("class-2027").unwrap();
    assert!(marker.is_file());
    assert_eq!(
        recovered
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT generation FROM work_note_local_drafts", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        9
    );
    drop(recovered);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}
