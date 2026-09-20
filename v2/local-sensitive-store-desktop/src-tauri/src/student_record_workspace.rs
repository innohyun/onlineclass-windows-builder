use crate::{now_ms, set_obj, set_updated_payload_fields, SqliteStore};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

pub(crate) fn is_workspace(input: &Value) -> bool {
    input["kind"] == "student_record_workspace_v1" || input["status"] == "workspace"
}

pub(crate) fn validate_behavior(value: &Value) -> Result<(), String> {
    let invalid = "MCP_STUDENT_TRAITS_INVALID";
    if !["records", "keywords", "combined"].contains(&value["inputMode"].as_str().unwrap_or(""))
        || !value["confirmed"].is_boolean() || !value["revision"].as_i64().is_some_and(|v| v > 0)
        || !value["scope"]["schoolYear"].as_i64().is_some_and(|v| (2000..=2200).contains(&v))
        || !matches!(value["scope"]["semester"].as_i64(), Some(1 | 2)) {
        return Err(invalid.into());
    }
    for field in ["fromDate", "toDate"] {
        let text = value["scope"][field].as_str().ok_or(invalid)?;
        if text.len() != 10 || chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").is_err() { return Err(invalid.into()); }
    }
    if value["scope"]["fromDate"].as_str() > value["scope"]["toDate"].as_str() { return Err(invalid.into()); }
    if value.get("note").is_some_and(|v| !v.as_str().is_some_and(|text| text.chars().count() <= 1200)) { return Err(invalid.into()); }
    let traits = value["traits"].as_array().filter(|rows| rows.len() <= 100).ok_or(invalid)?;
    let mut ids = std::collections::BTreeSet::new();
    for item in traits {
        let object = item.as_object().ok_or(invalid)?;
        if object.keys().any(|key| !["traitId","keywordId","categoryId","label","meaning","keywordVersion","categoryVersion","explanation"].contains(&key.as_str())) { return Err(invalid.into()); }
        for field in ["traitId","label","meaning"] {
            if !item[field].as_str().is_some_and(|v| !v.trim().is_empty() && v.chars().count() <= if field == "traitId" {260} else {1200}) { return Err(invalid.into()); }
        }
        if !ids.insert(item["traitId"].as_str().unwrap()) { return Err(invalid.into()); }
        for field in ["keywordId","categoryId","explanation"] {
            if item.get(field).is_some_and(|v| !v.as_str().is_some_and(|text| text.chars().count() <= 1200)) { return Err(invalid.into()); }
        }
        for field in ["keywordVersion","categoryVersion"] {
            if item.get(field).is_some_and(|v| !v.as_i64().is_some_and(|n| n > 0)) { return Err(invalid.into()); }
        }
    }
    if value["confirmed"] == true && (value["confirmation"]["revision"] != value["revision"]
        || !value["confirmation"]["actorId"].as_str().is_some_and(|v| !v.is_empty())
        || !value["confirmation"]["confirmedAtMs"].as_i64().is_some_and(|v| v > 0)) { return Err(invalid.into()); }
    Ok(())
}

pub(crate) fn save_traits(conn: &Connection, tenant: &str, data: &Value) -> Result<Value, String> {
    let invalid = "MCP_STUDENT_TRAITS_INVALID";
    let id = data["workspaceId"].as_str().filter(|s| !s.is_empty()).ok_or(invalid)?;
    let student = data["studentCode"].as_str().filter(|s| !s.is_empty()).ok_or(invalid)?;
    let expected = data["expectedRevision"].as_i64().filter(|v| *v > 0).ok_or(invalid)?;
    let expected_traits = data["expectedTraitsRevision"].as_i64().filter(|v| *v >= 0 && *v < 9_007_199_254_740_991).ok_or(invalid)?;
    let (raw, revision): (String, i64) = conn.query_row("SELECT payload_json,updated_at_ms FROM student_record_draft_sets WHERE tenant_id=?1 AND draft_set_id=?2 AND status='workspace'", params![tenant,id], |r| Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_| "MCP_STUDENT_WORKSPACE_READ_FAILED")?.ok_or("MCP_STUDENT_WORKSPACE_NOT_FOUND")?;
    if expected != revision { return Err("student_record_workspace_revision_conflict".into()); }
    let mut value: Value = serde_json::from_str(&raw).map_err(|_| invalid)?;
    let body = &mut value["workspace"];
    if body["workspaceId"] != id || !body["studentCodes"].as_array().is_some_and(|codes| codes.iter().any(|code| code == student)) { return Err("MCP_STUDENT_WORKSPACE_SCOPE_MISMATCH".into()); }
    if body["behaviorInputs"][student]["revision"].as_i64().unwrap_or(0) != expected_traits { return Err("MCP_STUDENT_TRAITS_REVISION_CONFLICT".into()); }
    let behavior = &data["behaviorInput"];
    validate_behavior(behavior)?;
    if behavior["revision"].as_i64() != Some(expected_traits + 1)
        || behavior["scope"] != json!({"schoolYear":body["academicYear"],"semester":body["semester"],"fromDate":body["fromDate"],"toDate":body["toDate"]}) { return Err("MCP_STUDENT_WORKSPACE_SCOPE_MISMATCH".into()); }
    if !body["behaviorInputs"].is_object() { body["behaviorInputs"] = json!({}); }
    body["behaviorInputs"][student] = behavior.clone();
    let updated = now_ms().max(revision + 1);
    set_updated_payload_fields(value.as_object_mut().ok_or(invalid)?, updated);
    let changed = conn.execute("UPDATE student_record_draft_sets SET payload_json=?3,updated_at_ms=?4 WHERE tenant_id=?1 AND draft_set_id=?2 AND updated_at_ms=?5",params![tenant,id,value.to_string(),updated,revision]).map_err(|_| "MCP_STUDENT_TRAITS_WRITE_FAILED")?;
    if changed != 1 { return Err("student_record_workspace_revision_conflict".into()); }
    let readback: String = conn.query_row("SELECT payload_json FROM student_record_draft_sets WHERE tenant_id=?1 AND draft_set_id=?2", params![tenant,id], |r| r.get(0)).map_err(|_| "LOCAL_STORE_WRITE_FAILED")?;
    if readback != value.to_string() { return Err("LOCAL_STORE_WRITE_FAILED".into()); }
    Ok(json!({"result":{"workspaceId":id,"studentCode":student,"revision":updated,"behaviorInput":behavior},"localRef":format!("student-record-workspace:{id}")}))
}

// Reuse the existing draft-set backup and device-sync authority for durable work in progress.
pub(crate) fn save(store: &SqliteStore, mut input: Value) -> Result<Value, String> {
    let expected = input.get("expectedRevision").and_then(Value::as_i64)
        .filter(|value| *value >= 0 && *value <= 9_007_199_254_740_991)
        .ok_or_else(|| "student_record_workspace_revision_required".to_string())?;
    if input["kind"] != "student_record_workspace_v1" || input["status"] != "workspace"
        || !input["workspace"].is_object() || input["workspace"]["workspaceId"] != input["draftSetId"] {
        return Err("student_record_workspace_invalid".to_string());
    }
    let tenant_id = input["tenantId"].as_str().unwrap_or_default().to_string();
    let draft_set_id = input["draftSetId"].as_str().unwrap_or_default().to_string();
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    let current: Option<(i64, i64)> = conn.query_row(
        "SELECT created_at_ms, updated_at_ms FROM student_record_draft_sets WHERE tenant_id = ?1 AND draft_set_id = ?2",
        params![tenant_id, draft_set_id], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map_err(|e| format!("db_student_record_workspace_read_failed:{e}"))?;
    if current.map(|row| row.1).unwrap_or(0) != expected {
        return Err("student_record_workspace_revision_conflict".to_string());
    }
    let updated = now_ms().max(expected + 1);
    let created = current.map(|row| row.0).unwrap_or(updated);
    if let Some(obj) = input.as_object_mut() {
        obj.remove("expectedRevision");
        set_obj(obj, "createdAtMs", created);
        set_obj(obj, "createdAtIso", DateTime::<Utc>::from_timestamp_millis(created)
            .unwrap_or_else(Utc::now).to_rfc3339());
        set_updated_payload_fields(obj, updated);
    }
    let body = serde_json::to_string(&input).map_err(|_| "student_record_workspace_invalid".to_string())?;
    let changed = conn.execute(
        "INSERT INTO student_record_draft_sets
         (tenant_id, draft_set_id, status, from_date, to_date, payload_json, created_at_ms, updated_at_ms)
         SELECT ?1, ?2, 'workspace', ?3, ?4, ?5, ?6, ?7 WHERE ?8 = 0 OR EXISTS (
           SELECT 1 FROM student_record_draft_sets WHERE tenant_id = ?1 AND draft_set_id = ?2 AND updated_at_ms = ?8)
         ON CONFLICT(tenant_id, draft_set_id) DO UPDATE SET
           status = excluded.status, from_date = excluded.from_date, to_date = excluded.to_date,
           payload_json = excluded.payload_json, updated_at_ms = excluded.updated_at_ms
         WHERE student_record_draft_sets.updated_at_ms = ?8",
        params![tenant_id, draft_set_id, input["fromDate"].as_str().unwrap_or_default(),
            input["toDate"].as_str().unwrap_or_default(), body, created, updated, expected],
    ).map_err(|e| format!("db_student_record_workspace_save_failed:{e}"))?;
    if changed != 1 { return Err("student_record_workspace_revision_conflict".to_string()); }
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn workspace_cas_preserves_context_and_rejects_stale_writes() {
        let dir = std::env::temp_dir().join(format!("record-workspace-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = SqliteStore::open(dir.join("test.sqlite")).unwrap();
        let input = json!({"tenantId":"tenant-a","draftSetId":"workspace-2026",
            "kind":"student_record_workspace_v1","status":"workspace","expectedRevision":0,
            "fromDate":"2026-03-01","toDate":"2026-09-08",
            "workspace":{"workspaceId":"workspace-2026","academicYear":2026,
                "selectedEvidence":{"S01":{"subjects:국어":["observation:one"]}},
                "checks":{"S01:subjects:국어":true},"promptSnapshots":{"subjects":{"version":1,"text":"근거만 사용"}},
                "draftLinks":{"S01:subjects:국어":"draft-1"}}});
        let first = store.upsert_student_record_draft_set(input.clone()).unwrap();
        assert!(first["updatedAtMs"].as_i64().unwrap() > 0);
        assert!(first.get("expectedRevision").is_none());
        assert_eq!(store.upsert_student_record_draft_set(input.clone()).unwrap_err(), "student_record_workspace_revision_conflict");
        let mut next = input.clone();
        next["expectedRevision"] = first["updatedAtMs"].clone();
        next["workspace"]["checks"] = json!({});
        let second = store.upsert_student_record_draft_set(next).unwrap();
        assert!(second["updatedAtMs"].as_i64().unwrap() > first["updatedAtMs"].as_i64().unwrap());
        assert_eq!(second["workspace"]["selectedEvidence"], input["workspace"]["selectedEvidence"]);
        let rows = store.list_student_record_draft_sets("tenant-a".into(), "workspace-2026".into(), "workspace".into(), 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["workspace"], second["workspace"]);
        assert!(store.list_student_record_draft_sets("tenant-b".into(), String::new(), String::new(), 10).unwrap().is_empty());
        {
            let conn = store.conn.lock().unwrap();
            let (version, tombstone): (i64, i64) = conn.query_row(
                "SELECT record_version, tombstone FROM local_store_device_sync_records
                 WHERE tenant_id = 'tenant-a' AND table_name = 'student_record_draft_sets' AND record_key = '[\"workspace-2026\"]'",
                [], |row| Ok((row.get(0)?, row.get(1)?)),
            ).unwrap();
            assert_eq!(version, 2, "workspace create and update participate in device-sync tracking");
            assert_eq!(tombstone, 0);
        }
        let mut invalid = input;
        invalid.as_object_mut().unwrap().remove("expectedRevision");
        assert_eq!(store.upsert_student_record_draft_set(invalid).unwrap_err(), "student_record_workspace_revision_required");
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn teacher_draft_cas_preserves_history_and_saves_review_atomically() {
        let dir = std::env::temp_dir().join(format!("record-cas-{}", crate::random_url_token()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = SqliteStore::open(dir.join("test.sqlite")).unwrap();
        store.upsert_student_record_draft_set(json!({"tenantId":"tenant-a","draftSetId":"set-a","status":"draft"})).unwrap();
        let input = json!({"tenantId":"tenant-a","draftSetId":"set-a","draftId":"draft-a","studentCode":"S01","recordType":"behavior","behaviorComment":"직접 작성함.","status":"draft","expectedRevision":0});
        let first = store.upsert_student_record_draft(input.clone()).unwrap();
        assert_eq!(store.upsert_student_record_draft(input).unwrap_err(),"student_record_draft_revision_conflict");
        let mut edited = first.clone(); edited["expectedRevision"] = first["updatedAtMs"].clone();
        edited["behaviorComment"] = json!("고쳐 작성함."); edited["status"] = json!("reviewed"); edited["history"] = json!([{"forged":true}]);
        let saved = store.upsert_student_record_draft(edited.clone()).unwrap();
        assert_eq!(saved["history"][0]["behaviorComment"],first["behaviorComment"]);
        assert_eq!(saved["status"],"reviewed"); assert_eq!(saved["history"].as_array().unwrap().len(),1);
        assert_eq!(store.upsert_student_record_draft(edited.clone()).unwrap_err(),"student_record_draft_revision_conflict");
        edited["expectedRevision"] = saved["updatedAtMs"].clone(); edited["studentCode"] = json!("S02");
        assert_eq!(store.upsert_student_record_draft(edited).unwrap_err(),"student_record_draft_scope_mismatch");
        assert_eq!(store.upsert_student_record_draft(saved).unwrap_err(),"student_record_draft_revision_required");
        drop(store); std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn traits_write_cas_scope_readback_and_receipt_are_atomic() {
        let dir = std::env::temp_dir().join(format!("record-traits-{}", crate::random_url_token()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = SqliteStore::open(dir.join("test.sqlite")).unwrap();
        let first = store.upsert_student_record_draft_set(json!({"tenantId":"tenant-a","draftSetId":"workspace-a","kind":"student_record_workspace_v1","status":"workspace","expectedRevision":0,"fromDate":"2026-08-17","toDate":"2026-09-09",
            "workspace":{"workspaceId":"workspace-a","academicYear":2026,"semester":2,"fromDate":"2026-08-17","toDate":"2026-09-09","studentCodes":["S01","S02"],"behaviorInputs":{"S02":{"private":"preserved"}}}})).unwrap();
        let behavior = json!({"inputMode":"keywords","traits":[{"traitId":"trait-a","label":"책임감","meaning":"맡은 일을 마무리함"}],"confirmed":true,"revision":1,"confirmation":{"actorId":"teacher-a","confirmedAtMs":100,"revision":1},"scope":{"schoolYear":2026,"semester":2,"fromDate":"2026-08-17","toDate":"2026-09-09"}});
        let request = json!({"tenantId":"tenant-a","receiptId":"traits-receipt","operation":"student_record_traits_save","requestSha256":"a".repeat(64),"data":{"workspaceId":"workspace-a","expectedRevision":first["updatedAtMs"],"studentCode":"S01","expectedTraitsRevision":0,"behaviorInput":behavior,"mutationId":"traits-a"}});
        store.conn.lock().unwrap().execute_batch("CREATE TEMP TRIGGER fail_traits_receipt BEFORE INSERT ON classaimate_mcp_local_write_receipts BEGIN SELECT RAISE(ABORT,'synthetic receipt failure'); END;").unwrap();
        assert!(crate::classaimate_mcp_write_jobs::apply(&store,&request).is_err());
        let unchanged = store.list_student_record_draft_sets("tenant-a".into(),"workspace-a".into(),"workspace".into(),10).unwrap();
        assert_eq!(unchanged[0]["updatedAtMs"],first["updatedAtMs"]);
        assert!(unchanged[0]["workspace"]["behaviorInputs"]["S01"].is_null());
        store.conn.lock().unwrap().execute_batch("DROP TRIGGER fail_traits_receipt;").unwrap();
        let result = crate::classaimate_mcp_write_jobs::apply(&store,&request).unwrap();
        assert_eq!(result["result"]["behaviorInput"],behavior);
        assert_eq!(crate::classaimate_mcp_write_jobs::apply(&store,&request).unwrap()["replayed"],true);
        let rows = store.list_student_record_draft_sets("tenant-a".into(),"workspace-a".into(),"workspace".into(),10).unwrap();
        assert_eq!(rows[0]["workspace"]["behaviorInputs"]["S02"]["private"],"preserved");
        let mut stale = request.clone(); stale["receiptId"] = json!("traits-stale");
        assert_eq!(crate::classaimate_mcp_write_jobs::apply(&store,&stale).unwrap_err(),"student_record_workspace_revision_conflict");
        let mut changed = rows[0].clone(); changed["expectedRevision"] = changed["updatedAtMs"].clone();
        changed["workspace"]["behaviorInputs"]["S01"]["traits"][0]["label"] = json!("교사 수정");
        store.upsert_student_record_draft_set(changed).unwrap();
        assert!(crate::classaimate_mcp_write_jobs::apply(&store,&request).is_err());
        drop(store); std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mcp_input_snapshot_survives_save_and_replay_and_cannot_be_rewritten() {
        use sha2::{Digest, Sha256};
        let dir = std::env::temp_dir().join(format!("record-snapshot-{}", crate::random_url_token()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = SqliteStore::open(dir.join("test.sqlite")).unwrap();
        let baseline = format!("{:x}",Sha256::digest(r#"{"draftId":"","text":"","updatedAtMs":0}"#));
        let snapshot = json!({"v":1,"taskId":"task-a","scope":{"recordType":"behavior"},"promptSnapshot":{"version":2,"text":"확인 자료만 사용"},"writingConditions":{"minChars":200},"students":[{"studentAlias":"학생-01","inputMode":"keywords","traits":[{"label":"책임감","meaning":"맡은 일을 마무리함"}],"evidence":[]}]});
        let request = json!({"tenantId":"tenant-a","receiptId":"snapshot-receipt","operation":"student_record_save_drafts","requestSha256":"b".repeat(64),"data":{"draftSetId":"set-snapshot","scope":{"recordType":"behavior","fromDate":"2026-08-17","toDate":"2026-09-09","schoolYear":2026,"semester":2},"inputSnapshot":snapshot,"rows":[{"studentCode":"S01","text":"맡은 일을 마무리함.","reviewNotes":["확인 근거가 짧아 목표 분량에 미달함"],"baselineDigest":baseline}]}});
        crate::classaimate_mcp_write_jobs::apply(&store,&request).unwrap();
        assert_eq!(crate::classaimate_mcp_write_jobs::apply(&store,&request).unwrap()["replayed"],true);
        let rows = store.list_student_record_draft_sets("tenant-a".into(),"set-snapshot".into(),String::new(),10).unwrap();
        assert_eq!(rows[0]["inputSnapshot"],snapshot);
        let drafts = store.list_student_record_drafts("tenant-a".into(),String::new(),"set-snapshot".into(),String::new(),10).unwrap();
        assert_eq!(drafts[0]["reviewNotes"],json!(["확인 근거가 짧아 목표 분량에 미달함"]));
        let mut wrong_scope = request.clone(); wrong_scope["receiptId"] = json!("wrong-scope"); wrong_scope["data"]["scope"]["semester"] = json!(1);
        assert_eq!(crate::classaimate_mcp_write_jobs::apply(&store,&wrong_scope).unwrap_err(),"DRAFT_CONFLICT");
        let mut change = rows[0].clone(); change["inputSnapshot"]["promptSnapshot"]["text"] = json!("바뀐 지침");
        assert_eq!(store.upsert_student_record_draft_set(change).unwrap_err(),"student_record_input_snapshot_immutable");
        drop(store); std::fs::remove_dir_all(dir).unwrap();
    }

}
