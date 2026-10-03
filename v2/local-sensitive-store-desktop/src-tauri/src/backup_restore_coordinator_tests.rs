use super::*;
use std::sync::Arc;

fn fixture() -> (
    PathBuf,
    Arc<SqliteStore>,
    Arc<SqliteStore>,
    PathBuf,
    Journal,
) {
    let root = std::env::temp_dir().join(format!(
        "classaimate-component-restore-{}",
        crate::random_url_token()
    ));
    fs::create_dir_all(&root).unwrap();
    let common = Arc::new(SqliteStore::open(root.join("fixture.sqlite")).unwrap());
    let class = common.for_tenant("class-2027").unwrap();
    let source = root.join("source-manifest.json");
    let common_protection = root.join("common-protection.json");
    let class_protection = root.join("class-protection.json");
    for path in [&source, &common_protection, &class_protection] {
        fs::write(path, b"{\"fixture\":true}").unwrap();
    }
    let body = json!({"tenantId":"class-2027","manifestPath":source});
    let common_snapshot = json!({"ok":true,"manifestPath":common_protection});
    let class_snapshot = json!({"ok":true,"manifestPath":class_protection});
    let journal = Journal {
        version: 1,
        operation_id: crate::random_url_token(),
        tenant_id: "class-2027".into(),
        mode: "manual".into(),
        manifest_sha256: digest_manifest(&body).unwrap(),
        common_protection_sha256: digest_manifest(&common_snapshot).unwrap(),
        class_protection_sha256: digest_manifest(&class_snapshot).unwrap(),
        body,
        common_protection: common_snapshot,
        class_protection: class_snapshot,
        phase: Phase::Prepared,
        common_result: None,
        class_result: None,
    };
    let path = path(&root, "class-2027").unwrap();
    write(&path, &journal).unwrap();
    (root, common, class, path, journal)
}

#[test]
fn half_applied_components_remain_blocked_and_resume_only_the_unfinished_component() {
    let (root, common, class, path, mut journal) = fixture();
    let mut calls = Vec::new();
    let error = resume(
        &common,
        &class,
        &path,
        &mut journal,
        &mut |component, _target, _protection, body| {
            assert_eq!(body["tenantId"], "class-2027");
            calls.push(component);
            if component == Component::Class {
                return Err("synthetic_class_commit_interrupted".into());
            }
            Ok(json!({"ok":true}))
        },
    )
    .unwrap_err();
    assert_eq!(error, "synthetic_class_commit_interrupted");
    assert!(matches!(read(&path).unwrap().phase, Phase::CommonApplied));
    assert!(class_ready(&root, "class-2027").is_err());
    assert!(common_ready(&root).is_err());
    assert!(maintenance_ready(&root, "class-2027").is_err());
    // Existing handles and direct writers stay blocked, even after the first
    // component's ordinary media journal has already been cleaned up.
    assert!(class
        .conn
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO student_private_details VALUES('class-2027','same-student','{}',1)",
            []
        )
        .is_err());
    drop(class);
    drop(common);
    let common = Arc::new(SqliteStore::open(root.join("fixture.sqlite")).unwrap());
    let class = {
        let _scope = scope("class-2027", Component::Combined);
        common.for_tenant("class-2027").unwrap()
    };
    let mut journal = read(&path).unwrap();
    let result = resume(
        &common,
        &class,
        &path,
        &mut journal,
        &mut |component, _target, _protection, _body| {
            assert_eq!(component, Component::Class);
            calls.push(component);
            Ok(json!({"ok":true}))
        },
    )
    .unwrap();
    assert_eq!(
        calls,
        vec![Component::Common, Component::Class, Component::Class]
    );
    assert_eq!(result["restoreComponentsCommitted"], true);
    assert!(class_ready(&root, "class-2027").is_ok());
    assert!(common_ready(&root).is_ok());
    assert!(class
        .conn
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO student_private_details VALUES('class-2027','same-student','{}',1)",
            []
        )
        .is_ok());
    drop(class);
    drop(common);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn altered_protection_cannot_release_a_pending_component_or_apply_more_data() {
    let (root, common, class, path, mut journal) = fixture();
    fs::write(
        manifest_path(&journal.common_protection).unwrap(),
        b"changed",
    )
    .unwrap();
    let mut calls = 0;
    assert!(resume(
        &common,
        &class,
        &path,
        &mut journal,
        &mut |_component, _target, _protection, _body| {
            calls += 1;
            Ok(json!({"ok":true}))
        }
    )
    .is_err());
    assert_eq!(calls, 0);
    assert!(class_ready(&root, "class-2027").is_err());
    assert!(common_ready(&root).is_err());
    assert_eq!(
        protected_manifest_paths(&root, "class-2027").unwrap().len(),
        2
    );
    drop(class);
    drop(common);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn malformed_receipts_and_cross_class_receipts_fail_closed() {
    let (root, common, class, path, mut journal) = fixture();
    journal.tenant_id = "other-class".into();
    write(&path, &journal).unwrap();
    assert!(class_ready(&root, "class-2027").is_err());
    assert!(common_ready(&root).is_err());
    drop(class);
    drop(common);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn missing_original_component_never_reopens_legacy_as_a_restore_target() {
    let (root, common, class, path, mut journal) = fixture();
    let backup_root = root.with_extension("backups");
    fs::create_dir_all(&backup_root).unwrap();
    {
        let _scope = scope("class-2027", Component::Combined);
        backup::set_folder(
            &common,
            "class-2027".into(),
            backup_root.to_string_lossy().into(),
        )
        .unwrap();
    }
    resume(
        &common,
        &class,
        &path,
        &mut journal,
        &mut |component, _target, _protection, _body| {
            if component == Component::Class {
                Err("interrupted".into())
            } else {
                Ok(json!({"ok":true}))
            }
        },
    )
    .unwrap_err();
    let original = class.data_dir.clone();
    drop(class);
    fs::remove_dir_all(&original).unwrap();
    assert!(recover_all(&common).is_err());
    assert!(!original.exists());
    assert!(common_ready(&root).is_err());
    assert!(matches!(read(&path).unwrap().phase, Phase::CommonApplied));
    assert_eq!(
        protected_manifest_paths(&root, "class-2027").unwrap().len(),
        2
    );
    drop(common);
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(backup_root).unwrap();
}

#[test]
fn mixed_import_rows_and_tombstones_keep_their_component_ownership() {
    {
        let _scope = scope("class-2027", Component::Common);
        assert!(include_table("local_import_runs"));
        assert!(include_import_run(Some("teaching_source"), "legacy-id"));
        assert!(include_import_run(None, "teaching-source:actor:source"));
        assert!(!include_import_run(Some("roster"), "class-import"));
        assert!(!include_import_run(None, "class-import"));
    }
    {
        let _scope = scope("class-2027", Component::Class);
        assert!(include_table("local_import_runs"));
        assert!(!include_import_run(Some("teaching_source"), "legacy-id"));
        assert!(!include_import_run(None, "teaching-source:actor:source"));
        assert!(include_import_run(Some("roster"), "class-import"));
        assert!(include_import_run(None, "class-import"));
    }
}

fn real_store(root: &Path, name: &str) -> Arc<SqliteStore> {
    fs::create_dir_all(root.join("one-drive")).unwrap();
    let store = Arc::new(SqliteStore::open(root.join(name).join("store.sqlite")).unwrap());
    backup::set_folder(
        &store,
        "qa-restore".into(),
        root.join("one-drive").to_string_lossy().into(),
    )
    .unwrap();
    backup::remember_snapshot_policy(
        &store,
        "qa-restore",
        &json!({"snapshotPolicy":{"maxWritableSnapshotVersion":5}}),
    )
    .unwrap();
    store
}
fn real_personal_source(store: &SqliteStore, bytes: &[u8], revision: i64) -> PathBuf {
    let relative = PathBuf::from("teaching-sources")
        .join(crate::teaching_sources::actor_folder("owner-a"))
        .join("objects/source.pdf");
    let target = store.shared_data_dir.join(&relative);
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, bytes).unwrap();
    let conn = store.conn.lock().unwrap();
    conn.execute(
        "INSERT INTO teaching_source_actor_homes VALUES('owner-a','qa-restore',1,1)",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO teaching_sources(owner_uid,source_id,origin_tenant_id,source_type,grade,semester_scope,subject_code,title,publisher,original_file_name,content_type,byte_size,sha256,local_path,extraction_status,lifecycle_status,extractor_version,page_count,revision,created_at_ms,updated_at_ms) VALUES('owner-a','source-a','qa-restore','textbook','5','2','SOC','자료','출판','source.pdf','application/pdf',?1,?2,?3,'ready','active','test-v1',1,?4,1,?4)", params![bytes.len() as i64, restore_journal::digest(&target).unwrap(), relative.to_string_lossy().as_ref(), revision]).unwrap();
    conn.execute("INSERT INTO teaching_source_chunks VALUES('owner-a','source-a','chunk-a',0,1,1,'지역','우리 고장','공통 원자료 내용','hash',1,1,?1)", [revision]).unwrap();
    target
}
fn real_media(store: &SqliteStore, bytes: &[u8]) {
    use base64::Engine;
    store.upsert_board_media(json!({"tenantId":"qa-restore","boardId":"board","postId":"post","mediaId":"shared-id","fileName":"student.bin","contentType":"application/octet-stream","dataBase64":base64::engine::general_purpose::STANDARD.encode(bytes)})).unwrap();
    store
        .conn
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO student_private_details VALUES('qa-restore','same-student',?1,100)",
            [serde_json::to_string(&json!({"text":String::from_utf8_lossy(bytes)})).unwrap()],
        )
        .unwrap();
}

fn real_import_run(store: &SqliteStore, run: &str, kind: &str, status: &str) {
    store.conn.lock().unwrap().execute("INSERT INTO local_import_runs VALUES('qa-restore',?1,?2,?3,'{}',100,100) ON CONFLICT(tenant_id,run_id) DO UPDATE SET status=excluded.status,payload_json=excluded.payload_json,finished_at_ms=excluded.finished_at_ms", params![run,kind,status]).unwrap();
}
fn import_status(store: &SqliteStore, run: &str) -> Option<String> {
    use rusqlite::OptionalExtension;
    store
        .conn
        .lock()
        .unwrap()
        .query_row(
            "SELECT status FROM local_import_runs WHERE tenant_id='qa-restore' AND run_id=?1",
            [run],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

#[test]
fn actual_component_restore_crash_child() {
    let Ok(root) = std::env::var("CLASSAIMATE_QA_COMPONENT_RESTORE_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    assert!(root.starts_with(std::env::temp_dir()));
    let manifest: PathBuf =
        serde_json::from_slice(&fs::read(root.join("selected.json")).unwrap()).unwrap();
    let common = SqliteStore::open(root.join("target/store.sqlite")).unwrap();
    let class = common.for_tenant("qa-restore").unwrap();
    backup::restore_generation(&class, "qa-restore", &manifest, 354, "announced", true).unwrap();
    panic!("requested coordinator boundary was not reached");
}

#[test]
fn actual_common_file_and_class_data_recover_after_each_component_process_death() {
    use base64::Engine;
    for boundary in [
        "coordinator-prepared",
        "coordinator-common-applied",
        "coordinator-class-applied",
        "component-class-prepared",
        "component-class-before-commit",
        "component-class-after-commit",
    ] {
        let root = std::env::temp_dir().join(format!(
            "classaimate-qa-restore-{}",
            crate::random_url_token()
        ));
        fs::create_dir_all(&root).unwrap();
        let source = real_store(&root, "source");
        real_personal_source(&source, b"new-personal-original", 2);
        real_import_run(
            &source,
            "teaching-source:owner-a:source-a",
            "teaching_source",
            "new-source-receipt",
        );
        let source_class = source.for_tenant("qa-restore").unwrap();
        real_media(&source_class, b"new-class-data");
        real_import_run(&source_class, "class-roster", "roster", "new-class-receipt");
        // Other classes can use the same IDs while retaining separate records.
        let target = real_store(&root, "target");
        let personal_path = real_personal_source(&target, b"old-personal-original", 1);
        real_import_run(
            &target,
            "teaching-source:owner-a:source-a",
            "teaching_source",
            "old-source-receipt",
        );
        real_import_run(&target, "class-roster", "roster", "protected-root-shadow");
        let target_class = target.for_tenant("qa-restore").unwrap();
        real_media(&target_class, b"old-class-data");
        real_import_run(&target_class, "class-roster", "roster", "old-class-receipt");
        let other = target.for_tenant("class-2026").unwrap();
        other.conn.lock().unwrap().execute("INSERT INTO student_private_details VALUES('class-2026','same-student','{\"text\":\"keep-old-year\"}',100)", []).unwrap();
        let selected =
            backup::run_with_kind(&source_class, "qa-restore".into(), "auto_sync", Some(354))
                .unwrap();
        fs::write(
            root.join("selected.json"),
            serde_json::to_vec(&PathBuf::from(selected["manifestPath"].as_str().unwrap())).unwrap(),
        )
        .unwrap();
        drop(other);
        drop(target_class);
        drop(target);
        drop(source_class);
        drop(source);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "backup_restore_coordinator::tests::actual_component_restore_crash_child",
                "--nocapture",
            ])
            .env("CLASSAIMATE_QA_COMPONENT_RESTORE_ROOT", &root)
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
            "{boundary}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let target = SqliteStore::open(root.join("target/store.sqlite")).unwrap();
        assert!(common_ready(&target.shared_data_dir).is_err());
        assert!(target.for_tenant("qa-restore").is_err());
        assert!(maintenance_ready(&target.shared_data_dir, "qa-restore").is_err());
        recover_all(&target).unwrap();
        assert!(common_ready(&target.shared_data_dir).is_ok());
        assert_eq!(fs::read(&personal_path).unwrap(), b"new-personal-original");
        let class = target.for_tenant("qa-restore").unwrap();
        assert_eq!(
            import_status(&target, "teaching-source:owner-a:source-a").as_deref(),
            Some("new-source-receipt")
        );
        assert_eq!(
            import_status(&target, "class-roster").as_deref(),
            Some("protected-root-shadow")
        );
        assert_eq!(
            import_status(&class, "class-roster").as_deref(),
            Some("new-class-receipt")
        );
        assert_eq!(
            import_status(&class, "teaching-source:owner-a:source-a").as_deref(),
            Some("old-source-receipt")
        );
        let media = class
            .get_board_media_file("qa-restore".into(), "shared-id".into())
            .unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(media["dataBase64"].as_str().unwrap())
                .unwrap(),
            b"new-class-data"
        );
        let data: String = class.conn.lock().unwrap().query_row("SELECT payload_json FROM student_private_details WHERE student_code='same-student'", [], |row| row.get(0)).unwrap();
        assert!(data.contains("new-class-data"));
        let other = target.for_tenant("class-2026").unwrap();
        let data: String = other.conn.lock().unwrap().query_row("SELECT payload_json FROM student_private_details WHERE student_code='same-student'", [], |row| row.get(0)).unwrap();
        assert!(data.contains("keep-old-year"));
        for store in [&target, class.as_ref(), other.as_ref()] {
            let conn = store.conn.lock().unwrap();
            assert_eq!(
                conn.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                    .unwrap(),
                "ok"
            );
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM local_store_component_restore_blocks",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
        }
        // Tombstones retain the existing v5 [run_id] key and route by the
        // reserved teaching-source prefix after the source row is gone.
        let source = real_store(&root, "source");
        let source_class = source.for_tenant("qa-restore").unwrap();
        source
            .conn
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM local_import_runs WHERE run_id='teaching-source:owner-a:source-a'",
                [],
            )
            .unwrap();
        source_class
            .conn
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM local_import_runs WHERE run_id='class-roster'",
                [],
            )
            .unwrap();
        let deletion =
            backup::run_with_kind(&source_class, "qa-restore".into(), "auto_sync", Some(355))
                .unwrap();
        backup::restore_generation(
            &class,
            "qa-restore",
            Path::new(deletion["manifestPath"].as_str().unwrap()),
            355,
            "announced",
            true,
        )
        .unwrap();
        assert!(import_status(&target, "teaching-source:owner-a:source-a").is_none());
        assert!(import_status(&class, "class-roster").is_none());
        assert_eq!(
            import_status(&target, "class-roster").as_deref(),
            Some("protected-root-shadow")
        );
        assert_eq!(
            import_status(&class, "teaching-source:owner-a:source-a").as_deref(),
            Some("old-source-receipt")
        );
        // Manual restore uses row kinds too, rather than copying the whole
        // mixed table into each component.
        real_import_run(
            &source,
            "teaching-source:owner-a:source-a",
            "teaching_source",
            "manual-source",
        );
        real_import_run(&source_class, "class-roster", "roster", "manual-class");
        let manual =
            backup::run_with_kind(&source_class, "qa-restore".into(), "manual", None).unwrap();
        backup::restore(
            &class,
            json!({"tenantId":"qa-restore","manifestPath":manual["manifestPath"]}),
        )
        .unwrap();
        assert_eq!(
            import_status(&target, "teaching-source:owner-a:source-a").as_deref(),
            Some("manual-source")
        );
        assert_eq!(
            import_status(&class, "class-roster").as_deref(),
            Some("manual-class")
        );
        assert_eq!(
            import_status(&target, "class-roster").as_deref(),
            Some("protected-root-shadow")
        );
        assert_eq!(
            import_status(&class, "teaching-source:owner-a:source-a").as_deref(),
            Some("old-source-receipt")
        );
        drop(source_class);
        drop(source);
        drop(other);
        drop(class);
        drop(target);
        fs::remove_dir_all(root).unwrap();
    }
}
