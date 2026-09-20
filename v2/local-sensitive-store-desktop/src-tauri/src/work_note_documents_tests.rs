use super::*;
use std::path::PathBuf;

struct Fixture {
    store: Option<SqliteStore>,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "classaimate-documents-{}",
            crate::random_url_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self {
            store: Some(SqliteStore::open(root.join("source/fixture.sqlite")).unwrap()),
            root,
        }
    }
    fn store(&self) -> &SqliteStore {
        self.store.as_ref().unwrap()
    }
    fn page(&self, id: &str) -> Value {
        let conn = self.store().conn.lock().unwrap();
        read(&conn, "synthetic", id).unwrap().unwrap()
    }
    fn create(&self, id: &str, parent: Option<&str>) -> Value {
        save(self.store(),json!({"tenantId":"synthetic","pageId":id,"expectedRevision":0,
            "title":"전체 수업안","emoji":"📄","parentId":parent,"position":0,"properties":{},
            "blocks":[{"id":"heading","type":"heading","text":"도입 발문"},{"id":"table","type":"table","rows":[["평가","발문"],["관찰","왜 그렇게 생각했나요?"]]}],
            "markdown":"# 도입 발문\n\n|평가|발문|\n|---|---|\n|관찰|왜 그렇게 생각했나요?|"})).unwrap()["page"].clone()
    }
    fn mutate(&self, id: &str, action: &str) -> Value {
        mutate(self.store(),json!({"tenantId":"synthetic","pageId":id,"expectedRevision":revision(&self.page(id)),"action":action})).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.store.take();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn native_document_roundtrip_is_cas_atomic_and_recovers_history() {
    let fixture = Fixture::new();
    let original = fixture.create("note", None);
    let mut update = original.clone();
    update["expectedRevision"] = json!(revision(&original));
    update["title"] = json!("수정한 제목");
    update["markdown"] = json!(format!(
        "{}\n{}",
        original["markdown"].as_str().unwrap(),
        "긴 상세 발문과 예상 반응. ".repeat(3000)
    ));
    let result = save(fixture.store(), update.clone()).unwrap();
    assert_eq!(result["verified"], true);
    assert!(result["page"].get("expectedRevision").is_none());
    assert_eq!(result["page"]["blocks"], original["blocks"]);
    assert!(revision(&result["page"]) > revision(&original));
    assert_eq!(
        save(fixture.store(), update).unwrap_err(),
        "work_note_revision_conflict"
    );
    let versions = {
        let conn = fixture.store().conn.lock().unwrap();
        crate::work_note_history::list(&conn, "synthetic", "note", None, None).unwrap()
    };
    assert_eq!(versions["items"].as_array().unwrap().len(), 1);
    let restored=mutate(fixture.store(),json!({"tenantId":"synthetic","pageId":"note","expectedRevision":result["revision"],"action":"restore_version","versionId":versions["items"][0]["versionId"]})).unwrap();
    assert_eq!(restored["page"]["blocks"], original["blocks"]);
    assert_eq!(restored["page"]["markdown"], original["markdown"]);
    assert!(revision(&restored["page"]) > revision(&result["page"]));
    let reopened = SqliteStore::open(fixture.root.join("source/fixture.sqlite")).unwrap();
    assert_eq!(
        reopened
            .get_work_note("synthetic".into(), "note".into())
            .unwrap()
            .unwrap()["markdown"],
        original["markdown"]
    );
}

#[test]
fn native_document_history_pages_metadata_without_skipping_tied_timestamps() {
    let fixture = Fixture::new();
    let mut original = fixture.create("note", None);
    original["markdown"] = json!("본문 전체 보존을 확인하는 원문. ".repeat(1024));
    let raw = original.to_string();
    let conn = fixture.store().conn.lock().unwrap();
    let captured = now_ms();
    for index in 0..205 {
        let id = format!("version-{index:04}");
        conn.execute(
            "INSERT INTO work_note_versions VALUES('synthetic',?1,'note',?2,?3,?3)",
            params![id, raw, captured],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO work_note_versions VALUES('synthetic','expired','note',?1,?2,?2)",
        params![raw, captured - RETENTION_MS - 1000],
    )
    .unwrap();
    let first = crate::work_note_history::list(&conn, "synthetic", "note", None, None).unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 50);
    assert_eq!(first["items"][0]["versionId"], "version-0204");
    assert_eq!(first["items"][0]["title"], original["title"]);
    assert_eq!(first["items"][0]["byteSize"], raw.len());
    assert!(first["items"][0].get("page").is_none());
    assert!(first.to_string().len() < 20_000);
    assert!(raw.len() > first.to_string().len());
    // A new autosave before the cursor cannot shift or duplicate the older pages.
    conn.execute(
        "INSERT INTO work_note_versions VALUES('synthetic','newer','note',?1,?2,?2)",
        params![raw, captured + 1],
    )
    .unwrap();
    let mut seen = std::collections::HashSet::new();
    let mut result = first;
    loop {
        for row in result["items"].as_array().unwrap() {
            assert!(seen.insert(row["versionId"].as_str().unwrap().to_string()));
        }
        if result["nextCursor"].is_null() {
            assert_eq!(result["hasMore"], false);
            break;
        }
        let cursor = serde_json::from_value(result["nextCursor"].clone()).unwrap();
        result = crate::work_note_history::list(&conn, "synthetic", "note", Some(50), Some(cursor))
            .unwrap();
    }
    assert_eq!(seen.len(), 205);
    assert!(!seen.contains("expired"));
    let detail = crate::work_note_history::get(&conn, "synthetic", "note", "version-0000").unwrap();
    assert_eq!(detail["page"], original);
    assert_eq!(detail["capturedAtMs"], captured);
    assert_eq!(
        crate::work_note_history::get(&conn, "other-tenant", "note", "version-0000").unwrap_err(),
        "work_note_version_not_found"
    );
    assert_eq!(
        crate::work_note_history::get(&conn, "synthetic", "another-page", "version-0000")
            .unwrap_err(),
        "work_note_version_not_found"
    );
    assert_eq!(
        crate::work_note_history::get(&conn, "synthetic", "note", "expired").unwrap_err(),
        "work_note_version_not_found"
    );
    let bounded =
        crate::work_note_history::list(&conn, "synthetic", "note", Some(10_000), None).unwrap();
    assert_eq!(bounded["items"].as_array().unwrap().len(), 100);
}

#[test]
fn native_document_subtree_trash_restore_and_duplicate_preserve_relations() {
    let fixture = Fixture::new();
    fixture.create("parent", None);
    fixture.create("child", Some("parent"));
    let duplicate = fixture.mutate("parent", "duplicate");
    let new_id = duplicate["page"]["pageId"].as_str().unwrap();
    assert_ne!(new_id, "parent");
    let pages = duplicate["pages"].as_array().unwrap();
    assert_eq!(pages.len(), 2);
    assert!(pages.iter().any(|p| p["parentId"] == new_id));
    fixture.mutate("parent", "trash");
    assert!(is_trashed(&fixture.page("child")));
    assert!(!fixture
        .store()
        .list_work_notes("synthetic".into(), String::new())
        .unwrap()
        .iter()
        .any(|p| p["pageId"] == "parent"));
    let items = {
        let conn = fixture.store().conn.lock().unwrap();
        trash(&conn, "synthetic").unwrap()
    };
    assert_eq!(items["items"].as_array().unwrap().len(), 1);
    fixture.mutate("parent", "restore");
    assert!(!is_trashed(&fixture.page("child")));
    assert_eq!(fixture.page("child")["parentId"], "parent");
    let error=mutate(fixture.store(),json!({"tenantId":"synthetic","pageId":"parent","expectedRevision":revision(&fixture.page("parent")),"action":"move","parentId":"child"})).unwrap_err();
    assert_eq!(error, "work_note_parent_cycle");
    assert_eq!(fixture.page("parent")["parentId"], Value::Null);
}

#[test]
fn native_document_binding_blocks_subtree_delete_without_partial_changes() {
    let fixture = Fixture::new();
    fixture.create("parent", None);
    fixture.create("bound", Some("parent"));
    {
        let conn = fixture.store().conn.lock().unwrap();
        conn.execute("INSERT INTO lesson_plan_bindings VALUES('synthetic','plan','bound','lesson','2026-09-20',1,1,'국어',1,1)",[]).unwrap();
    }
    let error=mutate(fixture.store(),json!({"tenantId":"synthetic","pageId":"parent","expectedRevision":revision(&fixture.page("parent")),"action":"trash"})).unwrap_err();
    assert_eq!(error, "lesson_plan_page_protected");
    assert!(!is_trashed(&fixture.page("parent")));
}

#[test]
fn native_document_attachment_history_survives_delete_and_duplicate() {
    let fixture = Fixture::new();
    let page = fixture.create("note", None);
    crate::work_note_attachments::save(
        fixture.store(),
        "synthetic".into(),
        "att-fixture".into(),
        "note".into(),
        "image".into(),
        "fixture.txt".into(),
        "text/plain".into(),
        &mut std::io::Cursor::new(b"synthetic attachment"),
    )
    .unwrap();
    let mut updated = page.clone();
    updated["expectedRevision"] = json!(revision(&page));
    updated["markdown"] = json!("[원본](local-attachment://att-fixture)");
    updated["blocks"] = json!([{"id":"image","type":"file","attachmentId":"att-fixture"}]);
    let saved = save(fixture.store(), updated).unwrap();
    assert_eq!(
        crate::work_note_attachments::delete(
            fixture.store(),
            "synthetic".into(),
            "att-fixture".into()
        )
        .unwrap(),
        1
    );
    let clone = fixture.mutate("note", "duplicate");
    let cloned_id = clone["page"]["blocks"][0]["attachmentId"].as_str().unwrap();
    assert_ne!(cloned_id, "att-fixture");
    assert!(crate::work_note_attachments::open(
        fixture.store(),
        "synthetic".into(),
        cloned_id.into()
    )
    .is_ok());
    let mut remove = saved["page"].clone();
    remove["expectedRevision"] = saved["revision"].clone();
    remove["blocks"] = json!([]);
    remove["markdown"] = json!("");
    save(fixture.store(), remove).unwrap();
    assert_eq!(
        crate::work_note_attachments::delete(
            fixture.store(),
            "synthetic".into(),
            "att-fixture".into()
        )
        .unwrap(),
        1
    );
    assert!(crate::work_note_attachments::open(
        fixture.store(),
        "synthetic".into(),
        "att-fixture".into()
    )
    .is_ok());
    assert_eq!(
        crate::work_note_attachments::save(
            fixture.store(),
            "synthetic".into(),
            "att-fixture".into(),
            "note".into(),
            "image".into(),
            "fixture.txt".into(),
            "text/plain".into(),
            &mut std::io::Cursor::new(b"changed")
        )
        .unwrap_err(),
        "work_note_attachment_immutable"
    );
}

#[test]
fn native_document_draft_generation_prevents_stale_overwrite_and_is_not_canonical() {
    let fixture = Fixture::new();
    let page = fixture.create("note", None);
    let mut draft = page.clone();
    draft["generation"] = json!(2);
    draft["baseRevision"] = json!(revision(&page));
    draft["markdown"] = json!("복구해야 하는 미저장 원문");
    crate::work_note_commands::save_draft(fixture.store(), draft.clone()).unwrap();
    draft["generation"] = json!(1);
    assert_eq!(
        crate::work_note_commands::save_draft(fixture.store(), draft).unwrap_err(),
        "work_note_draft_generation_conflict"
    );
    assert_eq!(fixture.page("note")["markdown"], page["markdown"]);
    let conn = fixture.store().conn.lock().unwrap();
    let count:i64=conn.query_row("SELECT COUNT(*) FROM local_store_device_sync_records WHERE table_name='work_note_local_drafts'",[],|r|r.get(0)).unwrap();
    assert_eq!(count, 0);
}

#[test]
fn native_document_oversize_is_rejected_without_silent_truncation() {
    let fixture = Fixture::new();
    let mut input = fixture.create("note", None);
    input["expectedRevision"] = json!(revision(&input));
    input["markdown"] = json!("가".repeat(2_000_001));
    assert_eq!(
        save(fixture.store(), input).unwrap_err(),
        "work_note_document_too_large"
    );
}

#[test]
fn native_document_student_material_descendants_are_readonly() {
    let fixture = Fixture::new();
    fixture.store().upsert_work_note(json!({"tenantId":"synthetic","pageId":"student-learning-materials-root","title":crate::STUDENT_MATERIAL_ROOT_TITLE,"properties":{"systemKind":crate::STUDENT_MATERIAL_ROOT_SYSTEM_KIND},"blocks":[],"markdown":""})).unwrap();
    fixture.store().upsert_work_note(json!({"tenantId":"synthetic","pageId":"legacy-child","parentId":"student-learning-materials-root","title":"과거 학생자료","properties":{},"blocks":[],"markdown":"원본"})).unwrap();
    let mut page = fixture.page("legacy-child");
    page["expectedRevision"] = page["updatedAtMs"].clone();
    assert_eq!(
        save(fixture.store(), page).unwrap_err(),
        "work_note_cloud_material_read_only"
    );
    assert!(
        list_editable(fixture.store(), "synthetic".into(), String::new())
            .unwrap()
            .is_empty()
    );
    let mut local = fixture.create("local", None);
    local["expectedRevision"] = local["updatedAtMs"].clone();
    local["parentId"] = json!("student-learning-materials-root");
    assert_eq!(
        save(fixture.store(), local).unwrap_err(),
        "work_note_cloud_material_read_only"
    );
    let error=mutate(fixture.store(),json!({"tenantId":"synthetic","pageId":"local","expectedRevision":revision(&fixture.page("local")),"action":"move","targetPageId":"legacy-child","placement":"after"})).unwrap_err();
    assert_eq!(error, "work_note_cloud_material_read_only");
    assert_eq!(fixture.page("local")["parentId"], Value::Null);
}

#[test]
fn native_document_attachment_path_upload_and_draft_reference_preserve_bytes() {
    use std::io::Read;
    let fixture = Fixture::new();
    let page = fixture.create("note", None);
    let path = fixture.root.join("selected-file.bin");
    let bytes = b"synthetic user-selected attachment";
    std::fs::write(&path, bytes).unwrap();
    let input = json!({"tenantId":"synthetic","pageId":"note","attachmentId":"draft-attachment","blockId":"file","fileName":"selected-file.bin","contentType":"application/octet-stream","sourcePath":path});
    let saved = crate::work_note_commands::save_attachment(fixture.store(), input).unwrap();
    assert_eq!(saved["attachment"]["size"], bytes.len());
    assert_eq!(fixture.page("note")["updatedAtMs"], page["updatedAtMs"]);
    let mut draft = page.clone();
    draft["generation"] = json!(1);
    draft["baseRevision"] = page["updatedAtMs"].clone();
    draft["markdown"] = json!("[파일](local-attachment://draft-attachment)");
    crate::work_note_commands::save_draft(fixture.store(), draft).unwrap();
    crate::work_note_attachments::delete(
        fixture.store(),
        "synthetic".into(),
        "draft-attachment".into(),
    )
    .unwrap();
    let mut file = crate::work_note_attachments::open(
        fixture.store(),
        "synthetic".into(),
        "draft-attachment".into(),
    )
    .unwrap();
    let mut actual = Vec::new();
    file.file.read_to_end(&mut actual).unwrap();
    assert_eq!(&actual, bytes);
}

#[test]
fn native_document_draft_copy_preserves_original_and_latest_generation() {
    let fixture = Fixture::new();
    let original = fixture.create("note", None);
    let mut draft = original.clone();
    draft["generation"] = json!(8);
    draft["baseRevision"] = original["updatedAtMs"].clone();
    draft["markdown"] = json!("경합에서도 유지해야 하는 전체 원문");
    crate::work_note_commands::save_draft(fixture.store(), draft.clone()).unwrap();
    let input = json!({"tenantId":"synthetic","pageId":"note","expectedRevision":revision(&original),"action":"duplicate_draft","generation":8});
    let copy = mutate(fixture.store(), input).unwrap();
    assert_ne!(copy["page"]["pageId"], "note");
    assert_eq!(copy["page"]["markdown"], draft["markdown"]);
    assert_eq!(fixture.page("note"), original);
    assert_eq!(mutate(fixture.store(),json!({"tenantId":"synthetic","pageId":"note","expectedRevision":revision(&original),"action":"duplicate_draft","generation":7})).unwrap_err(),"work_note_draft_generation_conflict");
}

#[test]
fn native_document_bound_lesson_draft_recovers_as_independent_note() {
    let fixture = Fixture::new();
    fixture.create("parent", None);
    let original = fixture.create("bound", Some("parent"));
    {
        let conn = fixture.store().conn.lock().unwrap();
        conn.execute("INSERT INTO lesson_plan_bindings VALUES('synthetic','plan','bound','lesson','2026-09-20',1,1,'국어',1,1)",[]).unwrap();
    }
    let mut draft = original.clone();
    draft["generation"] = json!(9);
    draft["baseRevision"] = original["updatedAtMs"].clone();
    draft["markdown"] = json!("충돌 뒤 복구해야 하는 수업계획 전체 본문");
    crate::work_note_commands::save_draft(fixture.store(), draft.clone()).unwrap();
    let copy = mutate(fixture.store(),json!({"tenantId":"synthetic","pageId":"bound",
        "expectedRevision":revision(&original),"action":"duplicate_draft","generation":9})).unwrap();
    assert_ne!(copy["page"]["pageId"], "bound");
    assert_eq!(copy["page"]["markdown"], draft["markdown"]);
    assert_eq!(copy["page"]["parentId"], Value::Null);
    assert_eq!(fixture.page("bound"), original);
    let conn = fixture.store().conn.lock().unwrap();
    assert!(crate::lesson_plan_bindings::stored_page_structure(&conn,"synthetic","bound").unwrap().is_some());
    assert!(crate::lesson_plan_bindings::stored_page_structure(&conn,"synthetic",copy["page"]["pageId"].as_str().unwrap()).unwrap().is_none());
}

#[test]
fn native_document_retention_removes_only_expired_unreferenced_pages() {
    let fixture = Fixture::new();
    fixture.create("note", None);
    fixture.mutate("note", "trash");
    {
        let conn = fixture.store().conn.lock().unwrap();
        let old = now_ms() - RETENTION_MS - 10;
        conn.execute("UPDATE work_note_pages SET properties_json=json_set(properties_json,'$._localTrash.deletedAtMs',?1,'$._localTrash.expiresAtMs',?2) WHERE tenant_id='synthetic'",params![old,old+RETENTION_MS]).unwrap();
        conn.execute(
            "UPDATE work_note_versions SET captured_at_ms=?1",
            params![old],
        )
        .unwrap();
    }
    crate::work_note_retention::maintain(fixture.store(), "synthetic").unwrap();
    let conn = fixture.store().conn.lock().unwrap();
    assert!(read(&conn, "synthetic", "note").unwrap().is_none());
    let tombstones:i64=conn.query_row("SELECT COUNT(*) FROM local_store_device_sync_records WHERE tenant_id='synthetic' AND table_name='work_note_versions' AND tombstone=1",[],|r|r.get(0)).unwrap();
    assert!(tombstones > 0);
}

#[test]
fn native_record_counseling_uses_roster_cas_and_preserves_private_fields() {
    let fixture = Fixture::new();
    fixture.store().put_quick_roster_snapshot(&json!({"tenantId":"synthetic","students":[{"id":"S01","displayName":"가상 학생","status":"active"}]})).unwrap();
    let input = json!({"tenantId":"synthetic","kind":"counseling","recordId":"counsel-1","expectedRevision":0,"studentCode":"S01","patch":{"summary":"상담의 구체적인 원문","topics":["학습"]}});
    let saved = crate::local_record_commands::save(fixture.store(), input.clone()).unwrap();
    assert_eq!(saved["record"]["studentCode"], "S01");
    assert_eq!(
        crate::local_record_commands::save(fixture.store(), input).unwrap_err(),
        "local_teacher_record_revision_conflict"
    );
    let revised=crate::local_record_commands::save(fixture.store(),json!({"tenantId":"synthetic","kind":"counseling","recordId":"counsel-1","expectedRevision":saved["revision"],"patch":{"followUpNote":"다음 주 확인"}})).unwrap();
    assert_eq!(revised["record"]["summary"], saved["record"]["summary"]);
    assert_eq!(revised["record"]["studentName"], "가상 학생");
}

#[test]
fn native_record_observation_correction_preserves_evidence_and_replays_once() {
    let fixture = Fixture::new();
    let original=fixture.store().upsert_observation(json!({"tenantId":"synthetic","docId":"observation","date":"2026-09-20","period":1,"studentCode":"S01","note":"처음 관찰 원문","status":"none"})).unwrap();
    let input = json!({"tenantId":"synthetic","kind":"observation","recordId":"observation","expectedRevision":original["revisionId"],"mutationId":"synthetic-correction","correctionReason":"내용을 더 정확히 기록","patch":{"note":"수정된 구체적 관찰 원문"}});
    let saved = crate::local_record_commands::save(fixture.store(), input.clone()).unwrap();
    assert_ne!(saved["revision"], original["revisionId"]);
    let replay = crate::local_record_commands::save(fixture.store(), input.clone()).unwrap();
    assert_eq!(replay["replayed"], true);
    let mut stale = input;
    stale["mutationId"] = json!("new-mutation");
    assert_eq!(
        crate::local_record_commands::save(fixture.store(), stale).unwrap_err(),
        "local_teacher_record_revision_conflict"
    );
    let detail = fixture
        .store()
        .evidence_detail("synthetic", "observation", false)
        .unwrap();
    assert_eq!(detail["revisions"].as_array().unwrap().len(), 2);
}

#[test]
fn native_document_backup_roundtrip_keeps_trash_history_and_attachment() {
    let fixture = Fixture::new();
    let original = fixture.create("note", None);
    crate::work_note_attachments::save(
        fixture.store(),
        "synthetic".into(),
        "archive-file".into(),
        "note".into(),
        "file".into(),
        "fixture.txt".into(),
        "text/plain".into(),
        &mut std::io::Cursor::new(b"fixture bytes"),
    )
    .unwrap();
    let mut changed = original.clone();
    changed["expectedRevision"] = original["updatedAtMs"].clone();
    changed["markdown"] = json!("[자료](local-attachment://archive-file)");
    save(fixture.store(), changed).unwrap();
    fixture.mutate("note", "trash");
    let backups = fixture.root.join("backups");
    crate::backup::set_folder(
        fixture.store(),
        "synthetic".into(),
        backups.to_string_lossy().into(),
    )
    .unwrap();
    let snapshot =
        crate::backup::run_with_kind(fixture.store(), "synthetic".into(), "auto_sync", Some(1))
            .unwrap();
    assert_eq!(snapshot["ok"], true);
    let target = SqliteStore::open(fixture.root.join("target/fixture.sqlite")).unwrap();
    crate::backup::set_folder(
        &target,
        "synthetic".into(),
        backups.to_string_lossy().into(),
    )
    .unwrap();
    crate::backup::restore_generation(
        &target,
        "synthetic",
        std::path::Path::new(snapshot["manifestPath"].as_str().unwrap()),
        1,
        "verified",
        false,
    )
    .unwrap();
    let conn = target.conn.lock().unwrap();
    assert!(is_trashed(
        &read(&conn, "synthetic", "note").unwrap().unwrap()
    ));
    assert_eq!(
        crate::work_note_history::list(&conn, "synthetic", "note", None, None).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    drop(conn);
    assert!(
        crate::work_note_attachments::open(&target, "synthetic".into(), "archive-file".into())
            .is_ok()
    );
}
