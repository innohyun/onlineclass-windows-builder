use super::*;
use std::fs;
use std::path::PathBuf;

const OPS: [&str; 7] = [
    "student_record_save_drafts",
    "counseling_record_save_draft",
    "counseling_record_prepare_create",
    "work_notes_save_draft",
    "materials_save_draft",
    "materials_update_draft",
    "materials_restructure_page",
];
const TABLES: [&str; 7] = [
    "work_note_pages",
    "work_note_pages_fts",
    "student_record_draft_sets",
    "student_record_drafts",
    "teacher_counseling_sessions",
    "teacher_counseling_mcp_drafts",
    "classaimate_mcp_local_write_receipts",
];
struct Fixture {
    store: SqliteStore,
    dir: PathBuf,
    input: Value,
}
impl Fixture {
    fn new(operation: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("mcp-atomic-writes-{}", crate::random_url_token()));
        fs::create_dir_all(&dir).unwrap();
        let store = SqliteStore::open(dir.join("store.sqlite3")).unwrap();
        store.upsert_work_note(json!({"tenantId":"tenant-a","pageId":"parent-a","title":"자료","blocks":[],"markdown":"","properties":{},"updatedAtMs":10})).unwrap();
        store.upsert_work_note(json!({"tenantId":"tenant-a","pageId":"mcp_page_a","parentId":"parent-a","title":"ChatGPT 초안 · 이전",
            "blocks":[{"id":"original","type":"text","text":"이전 본문"}],"markdown":"이전 본문","properties":{"sourceType":"classAimatePublicMcp"},"updatedAtMs":100})).unwrap();
        store.upsert_teacher_counseling_session(json!({"tenantId":"tenant-a","sessionId":"source-a","studentCode":"A001","counselingAtMs":1_777_777_777_000_i64,
            "status":"completed","summary":"이전 상담","transcript":"변경하면 안 되는 원문"})).unwrap();
        let data = match operation {
            "student_record_save_drafts" => {
                json!({"draftSetId":"mcp_student_set","scope":{"recordType":"behavior","fromDate":"2026-08-01","toDate":"2026-09-08"},
                "rows":[{"studentCode":"A001","studentName":"검증학생","classNo":1,"studentAlias":"학생-01","text":"근거 기반 분석 1","baselineDigest":digest(r#"{"draftId":"","text":"","updatedAtMs":0}"#)},
                    {"studentCode":"A002","studentName":"검증학생","classNo":2,"studentAlias":"학생-02","text":"근거 기반 분석 2","baselineDigest":digest(r#"{"draftId":"","text":"","updatedAtMs":0}"#)}]})
            }
            "counseling_record_save_draft" => {
                json!({"draftId":"draft-a","counselingRef":"source-a","summary":"새 상담 초안","followUpNote":"다음 확인","status":"pending"})
            }
            "counseling_record_prepare_create" => {
                json!({"counselingId":"new-session","studentCode":"A001","studentName":"검증학생","counselingAtMs":1_777_777_778_000_i64,
                "participantType":"student","channel":"in_person","status":"completed","summary":"새 상담 정본","followUpNote":"","topics":["학습"]})
            }
            "materials_update_draft" => {
                json!({"workspace":"work_materials","pageRef":"mcp_page_a","expectedRevision":100,"title":"ChatGPT 초안 · 수정","markdown":"새 본문","blocks":[{"id":"next","type":"text","text":"새 본문"}]})
            }
            "materials_restructure_page" => {
                json!({"workspace":"work_materials","pageRef":"mcp_page_a","expectedRevision":100,"markdown":"재구성 본문","blocks":[{"id":"next","type":"text","text":"재구성 본문"}]})
            }
            _ => {
                json!({"pageId":"mcp_new_page","parentPageRef":"parent-a","documentRef":"document-a","workspace":"work_materials","title":"ChatGPT 초안 · 생성","markdown":"새 본문","blocks":[{"id":"body","type":"text","text":"새 본문"}]})
            }
        };
        Self {
            store,
            dir,
            input: json!({"tenantId":"tenant-a","receiptId":"receipt-a","operation":operation,"requestSha256":crate::sha256_json(&data).unwrap(),"data":data}),
        }
    }
    fn snapshot(&self) -> Vec<Vec<Vec<String>>> {
        let conn = self.store.conn.lock().unwrap();
        TABLES
            .iter()
            .map(|table| {
                let mut statement = conn.prepare(&format!("SELECT * FROM {table}")).unwrap();
                let count = statement.column_count();
                statement
                    .query_map([], |row| {
                        Ok((0..count)
                            .map(|index| format!("{:?}", row.get_ref(index).unwrap()))
                            .collect::<Vec<_>>())
                    })
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap()
            })
            .collect()
    }
    fn count(&self, table: &str) -> i64 {
        self.store
            .conn
            .lock()
            .unwrap()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(mut connection) = self.store.conn.lock() {
            let old = std::mem::replace(&mut *connection, Connection::open_in_memory().unwrap());
            drop(old);
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn every_nonimage_operation_rolls_back_canonical_rows_when_receipt_insert_fails() {
    for operation in OPS {
        let fixture = Fixture::new(operation);
        let before = fixture.snapshot();
        fixture.store.conn.lock().unwrap().execute_batch("CREATE TRIGGER fail_receipt BEFORE INSERT ON classaimate_mcp_local_write_receipts BEGIN SELECT RAISE(ABORT,'receipt_failed'); END").unwrap();
        assert!(
            apply(&fixture.store, &fixture.input)
                .unwrap_err()
                .contains("receipt_failed"),
            "{operation}"
        );
        assert_eq!(fixture.snapshot(), before, "{operation}");
    }
}

#[test]
fn every_nonimage_operation_preserves_public_results_and_verifies_replay_digest() {
    for operation in OPS {
        let fixture = Fixture::new(operation);
        let saved = apply(&fixture.store, &fixture.input).unwrap();
        let after = fixture.snapshot();
        let replay = apply(&fixture.store, &fixture.input).unwrap();
        assert_eq!(replay["replayed"], true, "{operation}");
        assert_eq!(replay["result"], saved["result"]);
        assert_eq!(fixture.snapshot(), after);
        let raw: String = fixture
            .store
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT result_json FROM classaimate_mcp_local_write_receipts",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let envelope: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(envelope["kind"], receipt_verification::ENVELOPE);
        assert_eq!(envelope["verification"].as_object().unwrap().len(), 2);
        assert!(saved["result"].get("verification").is_none());
        let table = match operation {
            "student_record_save_drafts" => "student_record_drafts",
            "counseling_record_save_draft" => "teacher_counseling_mcp_drafts",
            "counseling_record_prepare_create" => "teacher_counseling_sessions",
            _ => "work_note_pages",
        };
        fixture
            .store
            .conn
            .lock()
            .unwrap()
            .execute_batch(&format!("UPDATE {table} SET updated_at_ms=updated_at_ms+1"))
            .unwrap();
        assert_eq!(
            apply(&fixture.store, &fixture.input).unwrap_err(),
            "LOCAL_STORE_WRITE_FAILED",
            "{operation}"
        );
    }
}

#[test]
fn second_student_failure_rolls_back_first_student_and_draft_set() {
    let fixture = Fixture::new("student_record_save_drafts");
    let before = fixture.snapshot();
    fixture.store.conn.lock().unwrap().execute_batch("CREATE TRIGGER fail_second BEFORE INSERT ON student_record_drafts WHEN NEW.student_code='A002' BEGIN SELECT RAISE(ABORT,'second_student_failed'); END").unwrap();
    assert!(apply(&fixture.store, &fixture.input)
        .unwrap_err()
        .contains("second_student_failed"));
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn text_search_write_failure_does_not_commit_page_changes() {
    for operation in [
        "materials_save_draft",
        "materials_update_draft",
        "materials_restructure_page",
    ] {
        let fixture = Fixture::new(operation);
        // A temporary ordinary table supplies a deterministic insert fault instead of changing FTS behavior.
        fixture.store.conn.lock().unwrap().execute_batch("DROP TABLE work_note_pages_fts; CREATE TABLE work_note_pages_fts(tenant_id TEXT,page_id TEXT,title TEXT,markdown TEXT); INSERT INTO work_note_pages_fts SELECT tenant_id,page_id,title,markdown FROM work_note_pages; CREATE TRIGGER fail_search BEFORE INSERT ON work_note_pages_fts BEGIN SELECT RAISE(ABORT,'search_write_failed'); END").unwrap();
        let before = fixture.snapshot();
        assert!(apply(&fixture.store, &fixture.input)
            .unwrap_err()
            .contains("search_write_failed"));
        assert_eq!(fixture.snapshot(), before);
    }
}

#[test]
fn committed_receipt_survives_readback_failure_and_recovers_without_reapplying() {
    let fixture = Fixture::new("materials_save_draft");
    assert!(!committed_receipt_exists(&fixture.store, &fixture.input).unwrap());
    receipt_verification::fail_next_post_commit_check();
    assert_eq!(
        apply(&fixture.store, &fixture.input).unwrap_err(),
        "LOCAL_STORE_WRITE_FAILED"
    );
    assert_eq!(fixture.count("classaimate_mcp_local_write_receipts"), 1);
    assert!(committed_receipt_exists(&fixture.store, &fixture.input).unwrap());
    let mut wrong = fixture.input.clone();
    wrong["requestSha256"] = json!("b".repeat(64));
    assert_eq!(
        committed_receipt_exists(&fixture.store, &wrong).unwrap_err(),
        "IDEMPOTENCY_CONFLICT"
    );
    assert_eq!(fixture.count("work_note_pages"), 3);
    assert_eq!(
        apply(&fixture.store, &fixture.input).unwrap()["replayed"],
        true
    );
    assert_eq!(fixture.count("work_note_pages"), 3);
}

#[test]
fn legacy_receipt_replays_require_exact_body_and_never_overwrite_teacher_edits() {
    let fixture = Fixture::new("materials_restructure_page");
    let saved = apply(&fixture.store, &fixture.input).unwrap();
    fixture
        .store
        .conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE classaimate_mcp_local_write_receipts SET result_json=?1",
            params![serde_json::to_string(&saved["result"]).unwrap()],
        )
        .unwrap();
    assert_eq!(
        apply(&fixture.store, &fixture.input).unwrap()["replayed"],
        true
    );
    fixture
        .store
        .conn
        .lock()
        .unwrap()
        .execute_batch(
            "UPDATE work_note_pages SET markdown='교사가 바꾼 본문' WHERE page_id='mcp_page_a'",
        )
        .unwrap();
    assert_eq!(
        apply(&fixture.store, &fixture.input).unwrap_err(),
        "LOCAL_STORE_WRITE_FAILED"
    );
}

#[test]
fn equal_clock_writes_still_advance_the_revision() {
    let mut fixture = Fixture::new("materials_update_draft");
    let future = now_ms() + 60_000;
    fixture
        .store
        .conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE work_note_pages SET updated_at_ms=?1 WHERE page_id='mcp_page_a'",
            params![future],
        )
        .unwrap();
    fixture.input["data"]["expectedRevision"] = json!(future);
    apply(&fixture.store, &fixture.input).unwrap();
    assert_eq!(
        fixture
            .store
            .get_work_note("tenant-a".into(), "mcp_page_a".into())
            .unwrap()
            .unwrap()["updatedAtMs"],
        future + 1
    );
}
