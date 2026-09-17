use super::*;
use rusqlite::params;

struct Fixture {
    root: PathBuf,
    target: PathBuf,
    school: PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn fixture() -> Fixture {
    let root =
        std::env::temp_dir().join(format!("cam-recovery-{}", &crate::random_url_token()[..8]));
    let target = root.join("target");
    let school = root.join("school");
    fs::create_dir_all(&target).unwrap();
    fs::create_dir_all(&school).unwrap();
    for path in [&target, &school] {
        let store = SqliteStore::open(path.join(DATABASE)).unwrap();
        // Reconcile schema/triggers exactly as an installed desktop would.
        crate::backup::seed_sync_records(&store, "tenant-a").unwrap();
    }
    Fixture {
        root,
        target,
        school,
    }
}

fn note(store: &SqliteStore, key: &str, text: &str, time: i64) {
    store.conn.lock().unwrap().execute("INSERT INTO student_private_details VALUES('tenant-a',?1,?2,?3) ON CONFLICT(tenant_id,student_code) DO UPDATE SET payload_json=excluded.payload_json,updated_at_ms=excluded.updated_at_ms",params![key,json!({"text":text}).to_string(),time]).unwrap();
}

fn snapshot(f: &Fixture) -> PathBuf {
    let store = SqliteStore::open(f.school.join(DATABASE)).unwrap();
    let backup_root = f.root.join("backups");
    fs::create_dir_all(&backup_root).unwrap();
    backup::set_folder(
        &store,
        "tenant-a".into(),
        backup_root.to_string_lossy().into_owned(),
    )
    .unwrap();
    let result = crate::shared_archive::with_test_root(&f.school, || {
        backup::run_with_kind(&store, "tenant-a".into(), "manual", None)
    })
    .unwrap();
    PathBuf::from(result["manifestPath"].as_str().unwrap())
}

fn shared_archive(root: &Path, id: &str) -> Connection {
    let archive = crate::shared_archive::open_db_at(root).unwrap();
    let bytes = b"immutable synthetic archive";
    let hash = format!("{:x}",Sha256::digest(bytes));
    let payload = "{\"synthetic\":true}";
    let payload_hash = format!("{:x}",Sha256::digest(payload.as_bytes()));
    let manifest_hash = format!("{:x}",Sha256::digest(id.as_bytes()));
    let file = root.join("shared-archive-files").join(id).join("0000-record.txt");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, bytes).unwrap();
    archive.execute("INSERT INTO shared_archives VALUES(?1,'tenant-a','board','synthetic','synthetic',?2,1,1,?3,1,2,3,'{}')", params![id,manifest_hash,bytes.len() as i64]).unwrap();
    archive.execute("INSERT INTO shared_archive_records VALUES(?1,0,'board',?2,?3)",params![id,payload,payload_hash]).unwrap();
    archive.execute("INSERT INTO shared_archive_files VALUES(?1,0,'record.txt','text/plain',?2,?3,?4)",params![id,bytes.len() as i64,hash,file.to_string_lossy().to_string()]).unwrap();
    archive
}

#[test]
fn recovery_tool_archives_rehearse_in_copy_include_wal_and_union_without_global_io() {
    let f = fixture();
    drop(shared_archive(&f.school,"shared"));
    drop(shared_archive(&f.school,"school-only"));
    drop(shared_archive(&f.target,"shared"));
    let archive = shared_archive(&f.target,"local-only");
    archive.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;").unwrap();
    archive.execute("INSERT INTO shared_archives VALUES('wal-only','other-tenant','board','synthetic','WAL preserved','wal',0,0,0,1,2,3,'{}')",[]).unwrap();
    let manifest = snapshot(&f);
    let before = fingerprint(&locked_store(&f.target).unwrap()).unwrap();
    let work = f.root.join("review-archives");
    let global_trap = f.root.join("must-not-create-global");
    crate::shared_archive::with_test_root(&global_trap, || preview("tenant-a",&f.target,&manifest,&work,true)).unwrap();
    let plan: Plan = serde_json::from_slice(&fs::read(work.join("plan.json")).unwrap()).unwrap();
    assert_eq!(plan.target, f.target, "preserve the authorized original root spelling, including Windows 8.3 aliases");
    assert!(!global_trap.exists(),"rehearsal must not open global archive storage");
    assert_eq!(fingerprint(&locked_store(&f.target).unwrap()).unwrap(),before);
    let protection = archive_read(&work.join("protection")).unwrap().unwrap();
    assert_eq!(protection.query_row("SELECT COUNT(*) FROM shared_archives WHERE id='wal-only'",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    drop(protection);
    drop(archive);
    crate::shared_archive::with_test_root(&global_trap, || apply(&work.join("plan.json"))).unwrap();
    assert!(!global_trap.exists());
    let result = archive_read(&f.target).unwrap().unwrap();
    assert_eq!(result.query_row("SELECT COUNT(*) FROM shared_archives",[],|r|r.get::<_,i64>(0)).unwrap(),4);
    assert!(f.target.join("shared-archive-files/school-only/0000-record.txt").is_file());
    drop(result);
    assert_eq!(apply(&work.join("plan.json")).unwrap()["phase"],"already_applied");
}

fn teaching_source(root: &Path, title: &str, revision: i64) {
    let store = locked_store(root).unwrap();
    let conn = store.conn.lock().unwrap();
    conn.execute("INSERT INTO teaching_source_actor_homes VALUES('owner-a','tenant-a',1,1)",[]).unwrap();
    conn.execute("INSERT INTO teaching_sources(owner_uid,source_id,origin_tenant_id,source_type,grade,semester_scope,subject_code,title,publisher,original_file_name,content_type,byte_size,sha256,local_path,extraction_status,lifecycle_status,extractor_version,page_count,revision,created_at_ms,updated_at_ms) VALUES('owner-a','source-a','tenant-a','textbook','5','2','SOC',?1,'synthetic','synthetic.pdf','application/pdf',0,NULL,NULL,'ready','active','test-v1',1,?2,1,1)",params![title,revision]).unwrap();
}

#[test]
fn recovery_tool_teaching_source_uses_canonical_revision_and_protects_previous_bundle() {
    let f=fixture();
    teaching_source(&f.target,"previous local",2);
    teaching_source(&f.school,"school",3);
    let manifest=snapshot(&f);
    let work=f.root.join("review-sources");
    preview("tenant-a",&f.target,&manifest,&work,true).unwrap();
    apply(&work.join("plan.json")).unwrap();
    for (root,title,revision) in [(&f.target,"school",3),( &work.join("protection"),"previous local",2)] {
        let store=locked_store(root).unwrap();
        let row=store.conn.lock().unwrap().query_row("SELECT title,revision FROM teaching_sources",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?))).unwrap();
        assert_eq!(row,(title.to_string(),revision));
    }
    let conflict=fixture();
    teaching_source(&conflict.target,"local branch",3);
    teaching_source(&conflict.school,"school branch",3);
    let manifest=snapshot(&conflict);
    assert_eq!(preview("tenant-a",&conflict.target,&manifest,&conflict.root.join("blocked-source"),true).unwrap_err(),"recovery_teaching_source_canonical_resolution_required");
}

#[test]
fn recovery_tool_school_wins_older_timestamp_union_archives_and_idempotent_apply() {
    let f = fixture();
    {
        let local = locked_store(&f.target).unwrap();
        let school = locked_store(&f.school).unwrap();
        note(&local, "same", "local newer clock", 900);
        note(&local, "only-local", "keep", 900);
        note(&school, "same", "school preferred", 100);
        note(&school, "only-school", "add", 100);
        local
            .conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO student_private_details VALUES('other-tenant','same','{}',999)",
                [],
            )
            .unwrap();
    }
    let manifest = snapshot(&f);
    let work = f.root.join("review");
    let before = fingerprint(&locked_store(&f.target).unwrap()).unwrap();
    let result = preview("tenant-a", &f.target, &manifest, &work, true).unwrap();
    assert_eq!(
        result["counts"]["counts"]["student_private_details"],
        json!({"add":1,"schoolPreferred":1})
    );
    assert_eq!(
        fingerprint(&locked_store(&f.target).unwrap()).unwrap(),
        before,
        "preview must not write target"
    );
    assert_eq!(apply(&work.join("plan.json")).unwrap()["phase"], "applied");
    {
        let target = locked_store(&f.target).unwrap();
        let conn = target.conn.lock().unwrap();
        let value:String=conn.query_row("SELECT payload_json FROM student_private_details WHERE tenant_id='tenant-a' AND student_code='same'",[],|r|r.get(0)).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&value).unwrap()["text"],
            "school preferred"
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM student_private_details WHERE tenant_id='tenant-a'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            3
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM student_private_details WHERE tenant_id='other-tenant'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        let loser:String=conn.query_row("SELECT payload_json FROM local_store_device_sync_conflicts WHERE table_name='student_private_details'",[],|r|r.get(0)).unwrap();
        assert!(loser.contains("local newer clock"));
        assert!(conn.query_row("SELECT COUNT(*) FROM local_store_device_sync_records WHERE tenant_id='tenant-a' AND changed_generation=0",[],|r|r.get::<_,i64>(0)).unwrap()>0);
    }
    assert_eq!(
        apply(&work.join("plan.json")).unwrap()["phase"],
        "already_applied"
    );
}

#[test]
fn recovery_tool_apply_refuses_old_school_or_changed_inputs() {
    let f = fixture();
    let manifest = snapshot(&f);
    let old = f.root.join("old");
    preview("tenant-a", &f.target, &manifest, &old, false).unwrap();
    assert_eq!(
        apply(&old.join("plan.json")).unwrap_err(),
        "recovery_current_school_snapshot_required"
    );
    let current = f.root.join("current");
    preview("tenant-a", &f.target, &manifest, &current, true).unwrap();
    note(
        &locked_store(&f.target).unwrap(),
        "late",
        "edited after preview",
        200,
    );
    assert_eq!(
        apply(&current.join("plan.json")).unwrap_err(),
        "recovery_input_changed"
    );
    assert!(!current.join("apply-started.json").exists());
}

#[test]
fn recovery_tool_refuses_wrong_tenant_and_modified_source() {
    let f = fixture();
    let manifest = snapshot(&f);
    assert!(preview(
        "wrong-tenant",
        &f.target,
        &manifest,
        &f.root.join("wrong"),
        true
    )
    .unwrap_err()
    .contains("tenant_mismatch"));
    let work = f.root.join("review");
    preview("tenant-a", &f.target, &manifest, &work, true).unwrap();
    let mut value: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    value["source"]["pcName"] = json!("changed");
    fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        apply(&work.join("plan.json")).unwrap_err(),
        "recovery_input_changed"
    );
}

#[test]
fn recovery_tool_database_lock_and_running_service_reject_concurrency() {
    let f = fixture();
    let first = locked_store(&f.target).unwrap();
    assert_eq!(
        locked_store(&f.target).err().unwrap(),
        "recovery_target_in_use"
    );
    let second = Connection::open(f.target.join(DATABASE)).unwrap();
    second.busy_timeout(Duration::from_millis(1)).unwrap();
    assert!(second
        .execute(
            "INSERT INTO student_private_details VALUES('tenant-a','race','{}',1)",
            []
        )
        .is_err());
    drop(first);
    drop(second);
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    assert_eq!(
        ports_closed(&[port]).unwrap_err(),
        "recovery_close_desktop_app_required"
    );
    drop(listener);
    assert!(ports_closed(&[port]).is_ok());
}

#[test]
fn recovery_tool_copy_keeps_wal_and_fingerprint_survives_checkpoint() {
    let f = fixture();
    let source = locked_store(&f.target).unwrap();
    note(&source, "wal", "not checkpointed", 20);
    let before = fingerprint(&source).unwrap();
    copy_store(&source, &f.root.join("copy")).unwrap();
    assert_eq!(
        fingerprint(&locked_store(&f.root.join("copy")).unwrap()).unwrap(),
        before
    );
    source
        .conn
        .lock()
        .unwrap()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    assert_eq!(fingerprint(&source).unwrap(), before);
}

#[test]
fn recovery_tool_observation_content_difference_is_blocked_before_mutation() {
    let f = fixture();
    for (path, text, time) in [(&f.target, "local", 20), (&f.school, "school", 10)] {
        let store = locked_store(path).unwrap();
        store.conn.lock().unwrap().execute("INSERT INTO lesson_observations VALUES('tenant-a','doc','2026-09-16',1,'1',?1,?2)",params![json!({"tenantId":"tenant-a","docId":"doc","date":"2026-09-16","period":1,"studentCode":"1","note":text,"updatedAtMs":time}).to_string(),time]).unwrap();
    }
    let manifest = snapshot(&f);
    let before = fingerprint(&locked_store(&f.target).unwrap()).unwrap();
    assert_eq!(
        preview(
            "tenant-a",
            &f.target,
            &manifest,
            &f.root.join("review"),
            true
        )
        .unwrap_err(),
        "recovery_observation_canonical_resolution_required"
    );
    assert_eq!(
        fingerprint(&locked_store(&f.target).unwrap()).unwrap(),
        before
    );
}

#[test]
fn recovery_tool_school_observation_uses_canonical_resolution_and_unions_provenance() {
    let f = fixture();
    let mut local_revision = Value::Null;
    for (path, mutation) in [(&f.target, "local-save"), (&f.school, "school-save")] {
        let store = locked_store(path).unwrap();
        let records=store.evidence_save("tenant-a",vec![json!({"tenantId":"tenant-a","docId":"same","date":"2026-09-08","period":0,"studentCode":"S1","observationKind":"non_lesson","contextType":"recess","note":mutation,"eventTimePrecision":"unknown","eventAtMs":0,"createdAtMs":1,"updatedAtMs":1})],mutation).unwrap();
        if mutation == "local-save" {
            local_revision = records[0]["revisionId"].clone();
        }
    }
    let manifest = snapshot(&f);
    let work = f.root.join("review");
    preview("tenant-a", &f.target, &manifest, &work, true).unwrap();
    apply(&work.join("plan.json")).unwrap();
    let target = locked_store(&f.target).unwrap();
    let conn = target.conn.lock().unwrap();
    let revision:String=conn.query_row("SELECT json_extract(payload_json,'$.revisionId') FROM lesson_observations WHERE tenant_id='tenant-a' AND doc_id='same'",[],|r|r.get(0)).unwrap();
    assert_ne!(revision, local_revision.as_str().unwrap());
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM observation_evidence_revisions WHERE tenant_id='tenant-a' AND doc_id='same'",[],|r|r.get::<_,i64>(0)).unwrap(),3);
    drop(conn);
    let detail = target.evidence_detail("tenant-a", "same", false).unwrap();
    assert_eq!(detail["record"]["note"], "school-save");
    assert_eq!(detail["heads"].as_array().unwrap().len(), 1);
    assert_eq!(detail["verification"]["valid"], true);
}

#[test]
fn recovery_tool_frozen_artifact_tamper_and_interrupted_apply_are_blocked() {
    let f = fixture();
    let manifest = snapshot(&f);
    let work = f.root.join("review");
    preview("tenant-a", &f.target, &manifest, &work, true).unwrap();
    let frozen = frozen_manifest(&work, "tenant-a", &manifest).unwrap();
    let value: Value = serde_json::from_slice(&fs::read(&frozen).unwrap()).unwrap();
    let db = frozen
        .parent()
        .unwrap()
        .join(value["db"]["relativePath"].as_str().unwrap());
    let bytes = fs::read(&db).unwrap();
    fs::write(&db, b"tamper").unwrap();
    assert!(apply(&work.join("plan.json"))
        .unwrap_err()
        .contains("digest_mismatch"));
    fs::write(&db, bytes).unwrap();
    fs::write(work.join("apply-started.json"), b"{}").unwrap();
    assert_eq!(
        apply(&work.join("plan.json")).unwrap_err(),
        "recovery_receipt_invalid"
    );
    let plan_path = work.join("plan.json");
    let plan: Value = serde_json::from_slice(&fs::read(&plan_path).unwrap()).unwrap();
    fs::write(work.join("apply-started.json"),json!({"version":1,"planSha256":source_hash(&plan_path).unwrap(),"targetFingerprint":plan["targetFingerprint"]}).to_string()).unwrap();
    assert_eq!(apply(&plan_path).unwrap()["phase"], "applied");
}
