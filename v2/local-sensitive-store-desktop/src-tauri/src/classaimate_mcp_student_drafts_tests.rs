use super::*;
use super::super::{apply, committed_receipt_exists};
use rusqlite::Connection;

struct Fixture { store: SqliteStore, directory: std::path::PathBuf }
impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!("mcp-student-current-{}", crate::random_url_token()));
        std::fs::create_dir_all(&directory).unwrap();
        Self { store: SqliteStore::open(directory.join("store.sqlite")).unwrap(), directory }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(mut conn) = self.store.conn.lock() {
            drop(std::mem::replace(&mut *conn, Connection::open_in_memory().unwrap()));
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
fn scope() -> Value {
    json!({"recordType":"subjects","subject":"수학","fromDate":"2026-08-01","toDate":"2026-09-09","schoolYear":2026,"semester":2})
}
fn students() -> Value {
    json!([{"studentAlias":"학생-01","studentCode":"A001"},{"studentAlias":"학생-02","studentCode":"A002"}])
}
fn seed(store: &SqliteStore, draft_id: &str, time: i64, fields: Value) {
    let conn = store.conn.lock().unwrap();
    crate::canonical_write_transactions::upsert_student_record_draft_set(&conn,
        json!({"tenantId":"tenant-a","draftSetId":"before","status":"draft","fromDate":"2026-08-01","toDate":"2026-09-08","createdAtMs":1,"updatedAtMs":1})).unwrap();
    let mut draft = json!({"tenantId":"tenant-a","draftSetId":"before","draftId":draft_id,"studentCode":"A001","status":"draft","updatedAtMs":time});
    draft.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
    crate::canonical_write_transactions::upsert_student_record_draft(&conn, draft).unwrap();
}

#[test]
fn canonical_picker_matches_legacy_multi_area_trim_and_ignores_empty_newer_rows() {
    let f = Fixture::new();
    seed(&f.store, "older-mixed", 100, json!({"subjectComments":[{"subject":" 수학 ","comment":"  비교하며 설명함  "}],"behaviorComment":"행발 보존"}));
    seed(&f.store, "newer-empty", 200, json!({"recordType":"subjects","subjectComments":[{"subject":"수학","comment":"  "}]}));
    let input = json!({"scope":scope(),"students":students()});
    let before = read_current(&f.store, "tenant-a", &input).unwrap();
    assert_eq!(before["drafts"][0]["text"], "비교하며 설명함");
    assert_eq!(before["drafts"][0]["baselineDigest"], crate::sha256_json(&json!({"draftId":"older-mixed","text":"비교하며 설명함","updatedAtMs":100})).unwrap());
    seed(&f.store, "legacy-text", 300, json!({"recordType":"subjects","subjectFilter":"수학","text":"  다른 방법을 찾음 "}));
    let latest = read_current(&f.store, "tenant-a", &input).unwrap();
    assert_eq!(latest["drafts"][0]["text"], "다른 방법을 찾음");
    assert_eq!(latest["drafts"][0]["baselineDigest"], crate::sha256_json(&json!({"draftId":"legacy-text","text":"다른 방법을 찾음","updatedAtMs":300})).unwrap());
}

#[test]
fn two_student_math_save_replay_current_read_preserves_behavior_and_exact_receipt() {
    let f = Fixture::new();
    seed(&f.store, "mixed-before", 100, json!({"subjectComments":[{"subject":"수학","comment":" 기존 수학 "}],"behaviorComment":"행발은 그대로 보존"}));
    let input = json!({"scope":scope(),"students":students()});
    let before = read_current(&f.store, "tenant-a", &input).unwrap();
    let rows: Vec<Value> = before["drafts"].as_array().unwrap().iter().enumerate().map(|(index,row)| json!({
        "studentCode":format!("A00{}",index+1),"studentAlias":row["studentAlias"],"studentName":"합성학생","classNo":index+1,
        "text":if index==0 {"  합성 수학 결과 1  ".to_string()} else {format!("합성 수학 결과 {}",index+1)},"baselineDigest":row["baselineDigest"]})).collect();
    let job = json!({"tenantId":"tenant-a","receiptId":"student-two","operation":"student_record_save_drafts","requestSha256":"a".repeat(64),
        "data":{"draftSetId":"math-two","scope":scope(),"rows":rows}});
    let first = apply(&f.store,&job).unwrap();
    let replay = apply(&f.store,&job).unwrap();
    assert_eq!(first["replayed"],false); assert_eq!(replay["replayed"],true);
    assert_eq!(first["result"],replay["result"]);
    assert!(committed_receipt_exists(&f.store,&job).unwrap());
    let current = read_current(&f.store,"tenant-a",&input).unwrap();
    assert_eq!(current["drafts"][0]["text"],"합성 수학 결과 1");
    assert_eq!(current["drafts"][1]["text"],"합성 수학 결과 2");
    let behavior = json!({"scope":{"recordType":"behavior","fromDate":"2026-08-01","toDate":"2026-09-09"},"students":students()});
    assert_eq!(read_current(&f.store,"tenant-a",&behavior).unwrap()["drafts"][0]["text"],"행발은 그대로 보존");
    let count: i64 = f.store.conn.lock().unwrap().query_row("SELECT COUNT(*) FROM student_record_drafts WHERE draft_set_id='math-two'",[],|row|row.get(0)).unwrap();
    assert_eq!(count,2);
    let mut altered=job.clone(); altered["data"]["rows"][0]["text"]=json!("다른 내용");
    altered["requestSha256"]=json!("b".repeat(64));
    assert_eq!(apply(&f.store,&altered).unwrap_err(),"IDEMPOTENCY_CONFLICT");
}

#[test]
fn current_read_is_alias_scoped_bounded_tenant_isolated_and_separates_baseline_from_term() {
    let f=Fixture::new();
    seed(&f.store,"prior",100,json!({"recordType":"subjects","text":"이전 학기","subject":"수학"}));
    f.store.conn.lock().unwrap().execute("UPDATE student_record_draft_sets SET payload_json=json_set(payload_json,'$.fromDate','2026-03-01','$.toDate','2026-07-31')",[]).unwrap();
    let input=json!({"scope":scope(),"students":students()});
    let result=read_current(&f.store,"tenant-a",&input).unwrap();
    assert_eq!(result["drafts"][0]["text"],"");
    assert_ne!(result["drafts"][0]["baselineDigest"],result["drafts"][1]["baselineDigest"]);
    assert!(!result.to_string().contains("A001"));
    assert_eq!(read_current(&f.store,"tenant-b",&input).unwrap()["drafts"][0]["status"],"none");
    for invalid in [json!({"scope":scope(),"students":students(),"tenantId":"tenant-b"}),
        json!({"scope":scope(),"students":[{"studentAlias":"학생-01","studentCode":"A001"},{"studentAlias":"학생-01","studentCode":"A002"}]}),
        json!({"scope":scope(),"students":[{"studentAlias":"학생-01","studentCode":"A001","sql":"private"}]})] {
        assert_eq!(read_current(&f.store,"tenant-a",&invalid).unwrap_err(),INVALID);
    }
}

#[test]
fn full_semester_read_includes_broader_and_earlier_draft_sets_than_the_evidence_period() {
    let f = Fixture::new();
    seed(&f.store,"full-term",100,json!({"recordType":"subjects","text":"같은 학기 현재 초안","subject":"수학"}));
    let scope = json!({"recordType":"subjects","subject":"수학","schoolYear":2026,"semester":2,
        "fromDate":"2026-08-17","toDate":"2027-01-08"});
    let input = json!({"scope":scope,"students":students()});
    for (from,to) in [("2026-08-17","2026-09-09"),("2026-08-17","2026-08-31"),("2026-08-17","2027-01-08")] {
        f.store.conn.lock().unwrap().execute("UPDATE student_record_draft_sets SET payload_json=json_set(payload_json,'$.fromDate',?1,'$.toDate',?2)",params![from,to]).unwrap();
        assert_eq!(read_current(&f.store,"tenant-a",&input).unwrap()["drafts"][0]["text"],"같은 학기 현재 초안");
    }
    f.store.conn.lock().unwrap().execute("UPDATE student_record_draft_sets SET payload_json=json_set(payload_json,'$.semester',1)",[]).unwrap();
    let mismatch=read_current(&f.store,"tenant-a",&input).unwrap();
    assert_eq!(mismatch["drafts"][0]["status"],"none");
    assert_ne!(mismatch["drafts"][0]["baselineDigest"],mismatch["drafts"][1]["baselineDigest"]);
}
