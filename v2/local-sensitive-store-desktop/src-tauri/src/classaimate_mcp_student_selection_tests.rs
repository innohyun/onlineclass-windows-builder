use super::*;

fn fixture() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE lesson_observations(tenant_id TEXT,doc_id TEXT,date_key TEXT,period INTEGER,student_code TEXT,payload_json TEXT,updated_at_ms INTEGER);
        CREATE TABLE eval_assignments(tenant_id TEXT,assignment_id TEXT,scheduled_date TEXT,payload_json TEXT,updated_at_ms INTEGER);
        CREATE TABLE eval_results(tenant_id TEXT,result_id TEXT,assignment_id TEXT,student_id TEXT,date_key TEXT,payload_json TEXT,updated_at_ms INTEGER);
        CREATE TABLE teacher_counseling_sessions(tenant_id TEXT,session_id TEXT,student_code TEXT,counseling_at_ms INTEGER,status TEXT,archived_at_ms INTEGER,payload_json TEXT,updated_at_ms INTEGER);
        CREATE TABLE student_record_draft_sets(tenant_id TEXT,draft_set_id TEXT,status TEXT,from_date TEXT,to_date TEXT,payload_json TEXT,updated_at_ms INTEGER);").unwrap();
    conn
}

fn input() -> Value {
    json!({"schoolYear":2026,"semester":2,"fromDate":"2026-08-17","toDate":"2026-09-09",
        "studentCodes":["S01"],"sourceTypes":["observation","evaluation","manual_supplement","counseling_summary"],"limit":200})
}

fn read(conn: &Connection, value: &Value) -> Result<Value, String> {
    read_connection(conn, "tenant-a", &Scope::parse("tenant-a", value)?)
}

fn observation(conn: &Connection, tenant: &str, id: &str, student: &str, day: &str, value: Value) {
    conn.execute("INSERT INTO lesson_observations VALUES(?1,?2,?3,1,?4,?5,100)", params![tenant,id,day,student,value.to_string()]).unwrap();
}

#[test]
fn strict_scope_rejects_arbitrary_authority_and_invalid_pagination() {
    for (key, value) in [("tenantId",json!("tenant-b")),("sql",json!("SELECT *")),("semester",json!(3)),
        ("schoolYear",json!(2026.5)),("fromDate",json!("2026-02-30")),("toDate",json!("2026-08-16")),
        ("studentCodes",json!([])),("studentCodes",json!(["S01","S01"])),("studentCodes",json!(["S01' OR 1=1"])),
        ("sourceTypes",json!(["transcript"])),("sourceTypes",json!([])),("cursor",json!(1)),("cursor",json!("01")),
        ("cursor",json!("-1")),("cursor",json!("10001")),("limit",json!(201)),("workspaceId",Value::Null)] {
        let mut request = input(); request[key] = value;
        assert_eq!(Scope::parse("tenant-a", &request).err().as_deref(), Some(INVALID), "{key}");
    }
    assert!(Scope::parse("tenant-a", &input()).is_ok());
    assert!(Scope::parse("", &input()).is_err());
}

#[test]
fn observation_projection_uses_columns_and_never_mutates_or_expands_scope() {
    let conn = fixture();
    observation(&conn,"tenant-a","correct","S01","2026-09-08",json!({"note":"합성 관찰","subject":"수학","period":99,
        "sourceId":"forged","studentCode":"OTHER","date":"2020-01-01","updatedAtMs":999,"studentName":"private","secret":"hidden",
        "content":{"arbitrary":"hidden"},"recordDomain":"subjects","revisionId":"revision-1","isActive":true,
        "categoryId":"category-a","categoryMeaning":"확인된 의미","categoryScopeKind":"subject","categoryScopeKey":"수학"}));
    observation(&conn,"tenant-b","other-tenant","S01","2026-09-08",json!({"note":"private"}));
    observation(&conn,"tenant-a","other-student","S02","2026-09-08",json!({"note":"private"}));
    observation(&conn,"tenant-a","other-date","S01","2026-08-16",json!({"note":"private"}));
    let before: i64 = conn.query_row("SELECT total_changes()", [], |row| row.get(0)).unwrap();
    let result = read(&conn,&input()).unwrap();
    let after: i64 = conn.query_row("SELECT total_changes()", [], |row| row.get(0)).unwrap();
    assert_eq!(after,before);
    assert_eq!(result["records"].as_array().unwrap().len(),1);
    let row = &result["records"][0];
    assert_eq!(row["sourceId"],"correct"); assert_eq!(row["studentCode"],"S01");
    assert_eq!(row["date"],"2026-09-08"); assert_eq!(row["period"],1); assert_eq!(row["updatedAtMs"],100);
    assert_eq!(row["revisionId"],"revision-1");
    assert_eq!(row["categoryId"],"category-a"); assert_eq!(row["categoryMeaning"],"확인된 의미");
    assert_eq!(row["categoryScopeKind"],"subject"); assert_eq!(row["categoryScopeKey"],"수학");
    for key in ["studentName","secret","tenantId","content"] { assert!(row.get(key).is_none()); }
}

#[test]
fn manual_sources_are_separate_and_archived_rows_remain_available_for_policy() {
    let conn = fixture();
    observation(&conn,"tenant-a","ordinary","S01","2026-09-08",json!({"note":"일반"}));
    observation(&conn,"tenant-a","manual","S01","2026-09-08",json!({"note":"보강","sourceType":"teacherManualSupplement","recordState":"archived","isActive":false}));
    let mut request = input(); request["sourceTypes"] = json!(["manual_supplement"]);
    let result = read(&conn,&request).unwrap();
    assert_eq!(result["records"].as_array().unwrap().len(),1);
    assert_eq!(result["records"][0]["sourceId"],"manual");
    assert_eq!(result["records"][0]["recordState"],"archived");
    request["sourceTypes"] = json!(["observation"]);
    assert_eq!(read(&conn,&request).unwrap()["records"][0]["sourceId"],"ordinary");
}

#[test]
fn evaluations_join_same_tenant_and_preserve_only_relevant_exclusion() {
    let conn = fixture();
    let assignment = json!({"title":"합성 평가","subject":"수학","levelTexts":["설명함","추가 확인"],"excludedStudentIds":["S01","OTHER"],"studentName":"hidden","sourceTranscript":"hidden","status":"archived","isExcluded":true,"archivedAtMs":10});
    conn.execute("INSERT INTO eval_assignments VALUES('tenant-a','assignment','2026-09-08',?1,200)",params![assignment.to_string()]).unwrap();
    conn.execute("INSERT INTO eval_assignments VALUES('tenant-b','foreign-only','2026-09-08',?1,200)",params![assignment.to_string()]).unwrap();
    for (id, assignment_id) in [("valid","assignment"),("orphan","foreign-only")] {
        conn.execute("INSERT INTO eval_results VALUES('tenant-a',?1,?2,'S01','',?3,100)",params![id,assignment_id,json!({"levelIndex":0,"isRecorded":true,"studentId":"spoof","date":"2020-01-01","status":"active","isExcluded":false,"archivedAtMs":0}).to_string()]).unwrap();
    }
    let result = read(&conn,&input()).unwrap();
    assert_eq!(result["records"].as_array().unwrap().len(),1);
    let row = &result["records"][0];
    assert_eq!(row["date"],"2026-09-08"); assert_eq!(row["updatedAtMs"],200);
    assert_eq!(row["excludedStudentIds"],json!(["S01"])); assert_eq!(row["levelIndex"],0);
    assert_eq!(row["title"],"합성 평가");
    assert_eq!(row["status"],"archived"); assert_eq!(row["isExcluded"],true); assert_eq!(row["archivedAtMs"],10);
    assert!(row.get("studentId").is_none()); assert!(!row.to_string().contains("hidden"));
}

#[test]
fn counseling_uses_kst_boundaries_and_summary_only() {
    let conn = fixture();
    let start = chrono::DateTime::parse_from_rfc3339("2026-09-09T00:00:00+09:00").unwrap().timestamp_millis();
    for (id, timestamp) in [("before",start-1),("start",start),("last",start+86_400_000-1),("after",start+86_400_000)] {
        let raw = json!({"summary":"정제된 합성 요약","content":"hidden","transcript":"hidden","sourceTranscript":{"text":"hidden"},"preview":"hidden","followUpNote":"hidden","studentName":"hidden","archivedAtMs":999,"counselingAtMs":0});
        conn.execute("INSERT INTO teacher_counseling_sessions VALUES('tenant-a',?1,'S01',?2,'completed',NULL,?3,100)",params![id,timestamp,raw.to_string()]).unwrap();
    }
    let mut request = input(); request["fromDate"] = json!("2026-09-09");
    let result = read(&conn,&request).unwrap();
    assert_eq!(result["records"].as_array().unwrap().len(),2);
    for row in result["records"].as_array().unwrap() {
        assert_eq!(row["date"],"2026-09-09"); assert_eq!(row["summary"],"정제된 합성 요약");
        assert!(row["archivedAtMs"].is_null()); assert!(!row.to_string().contains("hidden"));
    }
}

#[test]
fn revision_covers_all_pages_and_detects_edits_outside_the_current_page() {
    let conn = fixture();
    for id in ["c","a","b"] { observation(&conn,"tenant-a",id,"S01","2026-09-08",json!({"note":id})); }
    let mut request = input(); request["limit"] = json!(1);
    let first = read(&conn,&request).unwrap();
    assert_eq!(first["records"][0]["sourceId"],"a"); assert_eq!(first["nextCursor"],"1"); assert_eq!(first["complete"],false);
    request["cursor"] = first["nextCursor"].clone();
    let second = read(&conn,&request).unwrap();
    assert_eq!(second["sourceRevision"],first["sourceRevision"]); assert_eq!(second["records"][0]["sourceId"],"b");
    request["cursor"] = json!("2");
    let last = read(&conn,&request).unwrap(); assert_eq!(last["complete"],true); assert!(last["nextCursor"].is_null());
    conn.execute("UPDATE lesson_observations SET payload_json=?1 WHERE doc_id='c'",params![json!({"note":"수정"}).to_string()]).unwrap();
    request.as_object_mut().unwrap().remove("cursor");
    let changed = read(&conn,&request).unwrap();
    assert_eq!(changed["records"],first["records"]); assert_ne!(changed["sourceRevision"],first["sourceRevision"]);
    request["cursor"] = json!("4"); assert_eq!(read(&conn,&request).unwrap_err(),INVALID);
}

fn seed_workspace(conn: &Connection) {
    let body = json!({"kind":"student_record_workspace_v1","status":"workspace","workspace":{
        "workspaceId":"workspace-a","academicYear":2026,"semester":2,"fromDate":"2026-08-17","toDate":"2026-09-09",
        "studentCodes":["S01","OTHER"],"selectedEvidence":{"S01":{"subjects:수학":["observation:a"]},"OTHER":{"behavior":["observation:private"]}},
        "checks":{"S01:subjects:수학":"reviewed-source-fingerprint","OTHER:behavior":"private-check"},
        "evidenceLinks":{"observation:a":["subjects:수학"],"observation:private":["behavior"]},
        "counselingFollowUps":{"observation:a":true,"observation:private":true},
        "supplements":{"S01":"private-freeform"},"additionalInstructions":"private-freeform","promptSnapshots":{"text":"private-freeform"}}});
    conn.execute("INSERT INTO student_record_draft_sets VALUES('tenant-a','workspace-a','workspace','2026-08-17','2026-09-09',?1,123)",params![body.to_string()]).unwrap();
}

#[test]
fn workspace_projection_is_exact_and_selected_student_only() {
    let conn = fixture(); seed_workspace(&conn);
    let mut request = input(); request["workspaceId"] = json!("workspace-a");
    let result = read(&conn,&request).unwrap(); let workspace = &result["workspace"];
    assert_eq!(workspace["revision"],123); assert_eq!(workspace["studentCodes"],json!(["S01"]));
    assert_eq!(workspace["checks"]["S01:subjects:수학"],"reviewed-source-fingerprint");
    assert!(!workspace.to_string().contains("OTHER")); assert!(!workspace.to_string().contains("private"));
    request["semester"] = json!(1);
    assert_eq!(read(&conn,&request).unwrap_err(),"MCP_STUDENT_WORKSPACE_SCOPE_MISMATCH");
    request["workspaceId"] = json!("missing");
    assert_eq!(read(&conn,&request).unwrap_err(),"MCP_STUDENT_WORKSPACE_NOT_FOUND");
}

#[test]
fn complete_scope_over_limit_and_invalid_json_fail_without_partial_results() {
    let conn = fixture();
    conn.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10001)
        INSERT INTO lesson_observations SELECT 'tenant-a',CAST(x AS TEXT),'2026-09-08',1,'S01','{\"note\":\"fixture\"}',1 FROM n;").unwrap();
    assert_eq!(read(&conn,&input()).unwrap_err(),TOO_LARGE);
    conn.execute("DELETE FROM lesson_observations",[]).unwrap();
    conn.execute("INSERT INTO lesson_observations VALUES('tenant-a','bad','2026-09-08',1,'S01','not-json',1)",[]).unwrap();
    assert_eq!(read(&conn,&input()).unwrap_err(),READ_FAILED);
}


fn behavior_fixture(conn: &Connection, mode: &str, confirmed: bool) {
    seed_workspace(conn);
    let raw: String = conn.query_row("SELECT payload_json FROM student_record_draft_sets",[],|r|r.get(0)).unwrap();
    let mut value: Value = serde_json::from_str(&raw).unwrap();
    value["workspace"]["behaviorInputs"] = json!({"S01":{
        "inputMode":mode,"confirmed":confirmed,"revision":2,
        "confirmation":{"actorId":"teacher-a","confirmedAtMs":100,"revision":2},
        "scope":{"schoolYear":2026,"semester":2,"fromDate":"2026-08-17","toDate":"2026-09-09"},
        "traits":[{"traitId":"t-a","label":"책임감","meaning":"맡은 일을 마무리함"}]},
        "OTHER":{"inputMode":"keywords","traits":[{"label":"PRIVATE"}]}});
    value["workspace"]["selectedEvidence"]["S01"]["behavior"] = json!(["observation:included"]);
    conn.execute("UPDATE student_record_draft_sets SET payload_json=?1",params![value.to_string()]).unwrap();
}

#[test]
fn keyword_mode_skips_all_source_sql_and_projects_only_confirmed_student_traits() {
    let conn = fixture(); behavior_fixture(&conn,"keywords",true);
    conn.execute_batch("DROP TABLE lesson_observations; DROP TABLE eval_results; DROP TABLE eval_assignments; DROP TABLE teacher_counseling_sessions;").unwrap();
    let mut request = input(); request["includeBehaviorInputs"] = json!(true);
    let result = read(&conn,&request).unwrap();
    assert_eq!(result["records"],json!([]));
    assert_eq!(result["workspace"]["selectedEvidence"]["S01"],json!({}));
    assert_eq!(result["workspace"]["checks"],json!({}));
    assert_eq!(result["workspace"]["evidenceLinks"],json!({}));
    assert_eq!(result["workspace"]["workspaceId"],"workspace-a");
    assert_eq!(result["workspace"]["behaviorInputs"]["S01"]["traits"][0]["traitId"],"t-a");
    assert!(!result.to_string().contains("OTHER")); assert!(!result.to_string().contains("PRIVATE"));
    request["sourceTypes"] = json!([]);
    assert!(read(&conn,&request).is_ok());
}

#[test]
fn combined_mode_does_not_read_unselected_or_other_student_payloads() {
    let conn = fixture(); behavior_fixture(&conn,"combined",true);
    observation(&conn,"tenant-a","included","S01","2026-09-08",json!({"note":"선택 근거"}));
    conn.execute("INSERT INTO lesson_observations VALUES('tenant-a','excluded','2026-09-08',1,'S01','MALFORMED PRIVATE',1)",[]).unwrap();
    observation(&conn,"tenant-a","other-student","OTHER","2026-09-08",json!({"note":"PRIVATE"}));
    let mut request = input(); request["includeBehaviorInputs"] = json!(true);
    let result = read(&conn,&request).unwrap();
    assert_eq!(result["records"].as_array().unwrap().len(),1);
    assert_eq!(result["records"][0]["sourceId"],"included");
}

#[test]
fn unconfirmed_or_wrong_term_traits_are_never_projected_and_ambiguous_workspace_fails() {
    let conn = fixture(); behavior_fixture(&conn,"keywords",false);
    let mut request = input(); request["includeBehaviorInputs"] = json!(true);
    assert_eq!(read(&conn,&request).unwrap()["workspace"]["behaviorInputs"]["S01"]["traits"],json!([]));
    conn.execute("UPDATE student_record_draft_sets SET payload_json=json_set(payload_json,'$.workspace.behaviorInputs.S01.confirmed',json('true'),'$.workspace.behaviorInputs.S01.scope.semester',1)",[]).unwrap();
    assert_eq!(read(&conn,&request).unwrap()["workspace"]["behaviorInputs"]["S01"]["traits"],json!([]));
    conn.execute("INSERT INTO student_record_draft_sets SELECT tenant_id,'workspace-b',status,from_date,to_date,json_set(payload_json,'$.workspace.workspaceId','workspace-b'),updated_at_ms FROM student_record_draft_sets",[]).unwrap();
    assert_eq!(read(&conn,&request).unwrap_err(),"MCP_STUDENT_WORKSPACE_AMBIGUOUS");
    assert_eq!(list_workspaces(&conn,"tenant-a",&Scope::parse("tenant-a",&request).unwrap()).unwrap().len(),2);
    request["workspaceId"] = json!("workspace-a");
    assert!(read(&conn,&request).is_ok());
}
