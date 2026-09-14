use super::*;
use std::path::{Path, PathBuf};

struct Fixture {
    store: Option<SqliteStore>,
    dir: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("local-duplicates-{}", crate::random_url_token()));
        let store = SqliteStore::open(dir.join("store.sqlite")).unwrap();
        Self { store: Some(store), dir }
    }
    fn store(&self) -> &SqliteStore { self.store.as_ref().unwrap() }
    fn reopen(&mut self) {
        self.store.take();
        self.store = Some(SqliteStore::open(self.dir.join("store.sqlite")).unwrap());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.store.take();
        std::fs::remove_dir_all(&self.dir).expect("remove isolated duplicate fixture");
    }
}
fn record(doc: &str) -> Value {
    json!({"tenantId":"tenant-a","docId":doc,"studentCode":"S01","studentName":"합성 학생",
        "date":"2026-09-11","period":0,"observationKind":"non_lesson","contextType":"recess",
        "note":"경험을 시간 순서에 따라 구체적으로 서술함.","status":"good","eventTimePrecision":"unknown"})
}
fn scan_input() -> ScanInput { ScanInput { tenant_id: "tenant-a".into(), student_id: String::new() } }
fn scan_result(store: &SqliteStore) -> Value { store.scan_record_duplicates(scan_input()).unwrap() }
fn selection(store: &SqliteStore, id: &str) -> ApplyInput {
    let result = scan_result(store);
    ApplyInput { tenant_id: "tenant-a".into(), cleanup_id: id.into(), groups: result["groups"].as_array().unwrap().iter()
        .map(|g| GroupSelection { group_id: g["groupId"].as_str().unwrap().into(), snapshot_hash: g["snapshotHash"].as_str().unwrap().into() }).collect() }
}
fn undo_input(id: &str) -> UndoInput { UndoInput { tenant_id: "tenant-a".into(), cleanup_id: id.into() } }
fn current(store: &SqliteStore, doc: &str) -> Value { read(&store.conn.lock().unwrap(), "tenant-a", doc).unwrap().unwrap() }
fn save_pair(store: &SqliteStore) -> Vec<Value> { store.evidence_save("tenant-a", vec![record("a"), record("b")], "initial").unwrap() }
fn correct(store: &SqliteStore, doc: &str, note: &str, mutation: &str) {
    let mut value = current(store, doc);
    value["expectedRevisionId"] = value["revisionId"].clone();
    value["correctionReason"] = json!("합성 회귀 시험 정정");
    value["note"] = json!(note);
    store.evidence_save("tenant-a", vec![value], mutation).unwrap();
}
fn counts(store: &SqliteStore) -> (i64,i64,i64,i64) {
    store.conn.lock().unwrap().query_row("SELECT (SELECT COUNT(*) FROM lesson_observations),(SELECT COUNT(*) FROM observation_evidence_revisions),(SELECT COUNT(*) FROM observation_evidence_mutations),(SELECT COUNT(*) FROM observation_evidence_deletions)", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap()
}
fn reference(store: &SqliteStore, payload: Value) {
    store.conn.lock().unwrap().execute("INSERT INTO student_record_draft_sets VALUES('tenant-a','workspace','workspace','2026-09-01','2026-09-30',?1,1,1) ON CONFLICT(tenant_id,draft_set_id) DO UPDATE SET payload_json=excluded.payload_json,updated_at_ms=updated_at_ms+1", [payload.to_string()]).unwrap();
}

#[test]
fn duplicate_scan_is_read_only_and_ignores_only_text_whitespace_and_storage_identity() {
    let f = Fixture::new();
    let mut first = record("a");
    first["batchId"] = json!("batch-one");
    let mut second = record("b");
    second["batchId"] = json!("batch-two");
    second["note"] = json!("경험을  시간 순서에\n따라 구체적으로 서술함.");
    let initial = f.store().evidence_save("tenant-a", vec![first,second], "initial").unwrap();
    let before = counts(f.store());
    let scanned = scan_result(f.store());
    assert_eq!(scanned["groups"].as_array().unwrap().len(), 1);
    assert_eq!(scanned["groups"][0]["archiveIds"], json!(["b"]));
    assert_eq!(scanned["groups"][0]["records"][1]["body"], initial[1]["note"]);
    assert_eq!(counts(f.store()), before);
    assert_eq!(current(f.store(), "a"), initial[0]);
    assert_eq!(current(f.store(), "b"), initial[1]);
    let other = f.store().scan_record_duplicates(ScanInput { tenant_id:"tenant-b".into(), student_id:String::new() }).unwrap();
    assert_eq!(other["scannedCount"], 0);
    assert!(other["groups"].as_array().unwrap().is_empty());
}

#[test]
fn different_student_date_context_metadata_event_photos_and_empty_body_never_group() {
    let f = Fixture::new();
    let fields = [("studentCode",json!("S02")),("date",json!("2026-09-10")),("contextType",json!("counseling")),
        ("contextLabel",json!("별도의 실제 상황")),("status",json!("excellent")),("subject",json!("수학")),
        ("objective",json!("다른 목표")),("recordDomain",json!("creative")),("creativeArea",json!("career")),
        ("tags",json!(["협력"])),("lessonContext",json!({"documentId":"other-plan","pageId":"page-a"})),
        ("unknownMetadata",json!({"id":"meaningful-context"})),("note",json!("경험을 시간 순서에 따라 서술함."))];
    let mut records = vec![record("base")];
    for (index,(key,value)) in fields.into_iter().enumerate() {
        let mut variant = record(&format!("variant-{index}"));
        variant[key] = value;
        records.push(variant);
    }
    for id in ["empty-one","empty-two"] { let mut empty = record(id); empty["note"] = json!(" "); records.push(empty); }
    let mut actual_time = record("actual-time");
    actual_time["eventTimePrecision"] = json!("exact");
    actual_time["eventAtMs"] = json!(chrono::DateTime::parse_from_rfc3339("2026-09-11T09:00:00+09:00").unwrap().timestamp_millis());
    records.push(actual_time);
    let mut lesson = record("lesson");
    lesson["observationKind"] = json!("lesson");
    lesson["contextType"] = json!("");
    lesson["period"] = json!(1);
    records.push(lesson);
    let media = json!({"tenantId":"tenant-a","boardId":"student-observations","postId":"photo","mediaId":"photo-one","fileName":"one.png","contentType":"image/png","dataBase64":"aW1hZ2U="});
    f.store().upsert_board_media(media).unwrap();
    let mut photo = record("photo"); photo["photoMediaIds"] = json!(["photo-one"]); records.push(photo);
    f.store().evidence_save("tenant-a", records, "initial").unwrap();
    f.store().evidence_save("tenant-b", vec![record("other-tenant")], "initial").unwrap();
    assert!(scan_result(f.store())["groups"].as_array().unwrap().is_empty());
}

#[test]
fn legacy_original_occurrence_time_is_preserved_in_comparison_and_index_authority_is_checked() {
    let f = Fixture::new();
    let mut first = record("a");
    first["legacyOriginalTimestamps"] = json!({"createdAtMs":1,"updatedAtMs":2,"eventAtMs":100});
    let mut second = record("b");
    second["legacyOriginalTimestamps"] = json!({"createdAtMs":3,"updatedAtMs":4,"eventAtMs":200});
    f.store().evidence_save("tenant-a",vec![first,second],"initial").unwrap();
    assert!(scan_result(f.store())["groups"].as_array().unwrap().is_empty());
    f.store().conn.lock().unwrap().execute("UPDATE lesson_observations SET period=9 WHERE doc_id='a'",[]).unwrap();
    assert_eq!(f.store().scan_record_duplicates(scan_input()).unwrap_err(),"duplicate_record_authority_mismatch");
}

#[test]
fn archive_preserves_keeper_original_ids_history_and_retry_then_restores_after_restart() {
    let mut f = Fixture::new();
    let initial = save_pair(f.store());
    let input = selection(f.store(), "cleanup-archive-restore");
    let result = f.store().apply_record_duplicates(input.clone()).unwrap();
    assert_eq!(result["archivedCount"], 1);
    assert_eq!(current(f.store(), "a"), initial[0]);
    let archived_record = current(f.store(), "b");
    assert!(archived(&archived_record));
    assert_eq!(preserved_content(&archived_record), preserved_content(&initial[1]));
    assert_eq!(f.store().evidence_detail("tenant-a","b",false).unwrap()["revisions"].as_array().unwrap().len(), 2);
    assert_eq!(counts(f.store()), (2,3,2,0));
    assert!(scan_result(f.store())["groups"].as_array().unwrap().is_empty());
    assert_eq!(f.store().apply_record_duplicates(input.clone()).unwrap()["replayed"], true);
    assert_eq!(counts(f.store()), (2,3,2,0));
    f.reopen();
    let history = f.store().list_record_duplicate_history(scan_input()).unwrap();
    assert_eq!(history["entries"][0]["canUndo"], true);
    assert_eq!(f.store().apply_record_duplicates(input).unwrap()["replayed"], true);
    let result = f.store().undo_record_duplicate_cleanup(undo_input("cleanup-archive-restore")).unwrap();
    assert_eq!(result["restoredCount"], 1);
    assert_eq!(current(f.store(), "a"), initial[0]);
    assert!(!archived(&current(f.store(), "b")));
    assert_eq!(preserved_content(&current(f.store(), "b")), preserved_content(&initial[1]));
    assert_eq!(counts(f.store()), (2,4,3,0));
    assert_eq!(f.store().undo_record_duplicate_cleanup(undo_input("cleanup-archive-restore")).unwrap()["replayed"], true);
    assert_eq!(f.store().list_record_duplicate_history(scan_input()).unwrap()["entries"][0]["state"], "restored");
    assert!(f.store().apply_record_duplicates(selection(f.store(),"cleanup-after-undo")).is_ok());
}

#[test]
fn referenced_evidence_becomes_keeper_and_multiple_reference_keys_block_cleanup() {
    let f = Fixture::new();
    save_pair(f.store());
    let payload = json!({"kind":"student_record_workspace_v1","workspace":{"selectedEvidence":{"S01":{"subjects:국어":["observation:b"]}}}});
    reference(f.store(), payload.clone());
    let result = scan_result(f.store());
    assert_eq!(result["groups"][0]["keeperId"], "b");
    assert_eq!(result["groups"][0]["archiveIds"], json!(["a"]));
    let mut both = payload.clone(); both["workspace"]["evidenceLinks"] = json!({"observation:a":["behavior"]});
    reference(f.store(), both);
    assert_eq!(scan_result(f.store())["groups"][0]["canApply"], false);
    let before = counts(f.store());
    assert_eq!(f.store().apply_record_duplicates(selection(f.store(),"cleanup-references")).unwrap_err(), "duplicate_cleanup_referenced");
    assert_eq!(counts(f.store()), before);
    reference(f.store(), payload.clone());
    f.store().apply_record_duplicates(selection(f.store(),"cleanup-single-reference")).unwrap();
    assert!(!archived(&current(f.store(),"b")));
    let conn = f.store().conn.lock().unwrap();
    let raw: String = conn.query_row("SELECT payload_json FROM student_record_draft_sets",[],|r|r.get(0)).unwrap();
    assert_eq!(parse(&raw).unwrap(),payload);
}

#[test]
fn draft_evidence_json_and_workspace_key_references_are_detected() {
    let f = Fixture::new();
    save_pair(f.store());
    for key in ["behaviorTopics", "counselingFollowUps", "evidenceLinks"] {
        let mut payload = json!({"workspace":{}});
        payload["workspace"][key] = json!({"observation:b":true});
        reference(f.store(), payload);
        assert_eq!(scan_result(f.store())["groups"][0]["keeperId"], "b");
    }
    f.store().conn.lock().unwrap().execute("INSERT INTO student_record_drafts VALUES('tenant-a','draft','set','S01',1,?1,1)",
        [json!({"evidenceRefs":[{"kind":"observation","recordId":"a"}]}).to_string()]).unwrap();
    assert_eq!(scan_result(f.store())["groups"][0]["canApply"], false);
}

#[test]
fn stale_reference_revision_membership_and_tenant_selections_never_write() {
    let f = Fixture::new();
    save_pair(f.store());
    let before_selection = selection(f.store(), "cleanup-stale-reference");
    reference(f.store(), json!({"workspace":{"selectedEvidence":{"S01":{"behavior":["observation:b"]}}}}));
    let before = counts(f.store());
    assert_eq!(f.store().apply_record_duplicates(before_selection).unwrap_err(), "duplicate_cleanup_scan_stale");
    assert_eq!(counts(f.store()), before);
    let stale_revision = selection(f.store(), "cleanup-stale-revision");
    correct(f.store(), "b", record("b")["note"].as_str().unwrap(), "same-note-new-revision");
    let before = counts(f.store());
    assert_eq!(f.store().apply_record_duplicates(stale_revision).unwrap_err(), "duplicate_cleanup_scan_stale");
    assert_eq!(counts(f.store()), before);
    let stale_members = selection(f.store(), "cleanup-stale-members");
    f.store().evidence_save("tenant-a",vec![record("c")],"new-member").unwrap();
    let before = counts(f.store());
    assert_eq!(f.store().apply_record_duplicates(stale_members).unwrap_err(), "duplicate_cleanup_scan_stale");
    let mut cross_tenant = selection(f.store(), "cleanup-cross-tenant"); cross_tenant.tenant_id = "tenant-b".into();
    assert_eq!(f.store().apply_record_duplicates(cross_tenant).unwrap_err(), "duplicate_cleanup_scan_stale");
    assert_eq!(counts(f.store()), before);
}

#[test]
fn mid_batch_integrity_failure_rolls_back_every_archive_and_receipt() {
    let f = Fixture::new();
    f.store().evidence_save("tenant-a", vec![record("a"),record("b"),record("c")], "initial").unwrap();
    let input = selection(f.store(), "cleanup-rollback");
    let first = current(f.store(),"b");
    f.store().conn.lock().unwrap().execute("UPDATE observation_evidence_revisions SET payload_json=json_set(payload_json,'$.reason','tampered') WHERE tenant_id='tenant-a' AND doc_id='c'", []).unwrap();
    let before = counts(f.store());
    assert_eq!(f.store().apply_record_duplicates(input).unwrap_err(), "observation_evidence_integrity_mismatch");
    assert_eq!(counts(f.store()),before);
    assert_eq!(current(f.store(),"b"),first);
    assert!(!archived(&current(f.store(),"c")));
}

#[test]
fn keeper_integrity_failure_blocks_archive_and_replay_checks_current_keeper() {
    let f = Fixture::new();
    save_pair(f.store());
    let input = selection(f.store(),"cleanup-keeper-replay");
    f.store().apply_record_duplicates(input.clone()).unwrap();
    correct(f.store(),"a","정리 뒤 정정된 대표 본문","correct-keeper");
    let before = counts(f.store());
    assert_eq!(f.store().apply_record_duplicates(input).unwrap_err(),"duplicate_cleanup_readback_conflict");
    assert_eq!(counts(f.store()),before);
    let second = Fixture::new();
    save_pair(second.store());
    let input = selection(second.store(),"cleanup-keeper-tamper");
    second.store().conn.lock().unwrap().execute("UPDATE observation_evidence_revisions SET revision_hash='tampered' WHERE doc_id='a'", []).unwrap();
    assert_eq!(second.store().apply_record_duplicates(input).unwrap_err(),"observation_evidence_integrity_mismatch");
    assert!(!archived(&current(second.store(),"b")));
}

#[test]
fn undo_never_overwrites_later_edits_or_accepts_changed_replay_identity() {
    let f = Fixture::new();
    save_pair(f.store());
    let input = selection(f.store(),"cleanup-stale-undo");
    f.store().apply_record_duplicates(input.clone()).unwrap();
    let mut conflicting = input; conflicting.groups[0].snapshot_hash = "changed".into();
    assert_eq!(f.store().apply_record_duplicates(conflicting).unwrap_err(),"duplicate_cleanup_replay_conflict");
    correct(f.store(),"b","보관 후 정정된 관찰 본문","edit-archived");
    let before = counts(f.store());
    assert_eq!(f.store().undo_record_duplicate_cleanup(undo_input("cleanup-stale-undo")).unwrap_err(),"duplicate_cleanup_restore_stale");
    assert_eq!(counts(f.store()),before);
    assert_eq!(f.store().list_record_duplicate_history(scan_input()).unwrap()["entries"][0]["state"],"changed");
    assert!(f.store().undo_record_duplicate_cleanup(UndoInput {tenant_id:"tenant-b".into(),cleanup_id:"cleanup-stale-undo".into()}).is_err());
}

#[test]
fn cleanup_limit_and_repeated_group_are_validated_before_writing() {
    let f = Fixture::new();
    let records: Vec<Value> = (0..202).map(|i| record(&format!("doc-{i:03}"))).collect();
    for (index, chunk) in records.chunks(200).enumerate() {
        f.store().evidence_save("tenant-a",chunk.to_vec(),&format!("initial-{index}")).unwrap();
    }
    let input = selection(f.store(),"cleanup-too-large");
    let before = counts(f.store());
    assert_eq!(f.store().apply_record_duplicates(input.clone()).unwrap_err(),"duplicate_cleanup_limit_exceeded");
    let mut repeated = input; repeated.groups.push(repeated.groups[0].clone());
    assert_eq!(f.store().apply_record_duplicates(repeated).unwrap_err(),"duplicate_cleanup_selection_invalid");
    assert_eq!(counts(f.store()),before);
}

#[test]
fn pending_restore_blocks_cleanup_before_any_observation_write() {
    let f = Fixture::new();
    save_pair(f.store());
    let input = selection(f.store(),"cleanup-pending-restore");
    f.store().conn.lock().unwrap().execute("INSERT INTO local_store_restore_journal (tenant_id,operation_id,generation,artifact_root,phase,intent_json) VALUES('tenant-a','pending-operation',1,'artifact','prepared','{}')",[]).unwrap();
    let before = counts(f.store());
    assert_eq!(f.store().apply_record_duplicates(input).unwrap_err(),"restore_recovery_required");
    assert_eq!(counts(f.store()),before);
}

#[test]
fn cleanup_and_undo_round_trip_through_existing_backup_and_device_sync_tracking() {
    let f = Fixture::new();
    crate::shared_archive::with_test_root(&f.dir.join("archives"), || {
        let source = f.store();
        let target = SqliteStore::open(f.dir.join("target/store.sqlite")).unwrap();
        let folder = f.dir.join("transport");
        for store in [source,&target] {
            crate::backup::set_folder(store,"tenant-a".into(),folder.to_string_lossy().into()).unwrap();
        }
        let initial = save_pair(source);
        source.apply_record_duplicates(selection(source,"cleanup-backup-sync")).unwrap();
        {
            let conn = source.conn.lock().unwrap();
            for table in ["lesson_observations","observation_evidence_revisions","observation_evidence_mutations"] {
                let tracked:i64 = conn.query_row("SELECT COUNT(*) FROM local_store_device_sync_records WHERE tenant_id='tenant-a' AND table_name=?1",[table],|r|r.get(0)).unwrap();
                assert!(tracked>0,"{table} is tracked by existing sync triggers");
            }
        }
        let backup = crate::backup::run_with_kind(source,"tenant-a".into(),"auto_sync",Some(1)).unwrap();
        let result = crate::backup::restore_generation(&target,"tenant-a",Path::new(backup["manifestPath"].as_str().unwrap()),1,"announced",false).unwrap();
        assert_eq!(result["applied"],true);
        assert_eq!(current(&target,"a"),initial[0]);
        assert!(archived(&current(&target,"b")));
        assert_eq!(target.list_record_duplicate_history(scan_input()).unwrap()["entries"][0]["canUndo"],true);
        target.undo_record_duplicate_cleanup(undo_input("cleanup-backup-sync")).unwrap();
        assert!(!archived(&current(&target,"b")));
        assert_eq!(preserved_content(&current(&target,"b")),preserved_content(&initial[1]));
        assert_eq!(target.evidence_detail("tenant-a","b",false).unwrap()["verification"]["valid"],true);
    });
}
