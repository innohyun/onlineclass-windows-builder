use crate::{now_ms, set_obj, set_updated_payload_fields, SqliteStore};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use serde_json::Value;

pub(crate) fn is_workspace(input: &Value) -> bool {
    input["kind"] == "student_record_workspace_v1" || input["status"] == "workspace"
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
}
