use crate::{normalize_id_segment, normalize_tenant_id, now_ms, AppState, SqliteStore};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

fn read(conn: &Connection, tenant: &str, kind: &str, id: &str) -> Result<Option<Value>, String> {
    let sql=match kind {
        "observation"=>"SELECT payload_json FROM lesson_observations WHERE tenant_id=?1 AND doc_id=?2",
        "counseling"=>"SELECT payload_json FROM teacher_counseling_sessions WHERE tenant_id=?1 AND session_id=?2",
        _=>return Err("local_teacher_record_kind_unsupported".into()),
    };
    let raw: Option<String> = conn
        .query_row(sql, params![tenant, id], |r| r.get(0))
        .optional()
        .map_err(|e| format!("local_teacher_record_read_failed:{e}"))?;
    raw.map(|raw| serde_json::from_str(&raw).map_err(|_| "local_teacher_record_invalid".into()))
        .transpose()
}
fn revision(record: &Value, kind: &str) -> Value {
    record[if kind == "observation" {
        "revisionId"
    } else {
        "updatedAtMs"
    }]
    .clone()
}
fn scope(input: &Value) -> Result<(String, String, String), String> {
    let tenant = normalize_tenant_id(input.get("tenantId"));
    let id = normalize_id_segment(input.get("recordId"), 240);
    let kind = input["kind"].as_str().unwrap_or_default().to_string();
    if tenant.is_empty() {
        return Err("tenant_id_required".into());
    }
    if id.is_empty() {
        return Err("local_teacher_record_id_required".into());
    }
    if !["observation", "counseling"].contains(&kind.as_str()) {
        return Err("local_teacher_record_kind_unsupported".into());
    }
    Ok((tenant, kind, id))
}
fn get(store: &SqliteStore, tenant: &str, kind: &str, id: &str) -> Result<Value, String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    let record = read(&conn, tenant, kind, id)?.ok_or("local_teacher_record_not_found")?;
    Ok(json!({"ok":true,"revision":revision(&record,kind),"record":record}))
}
pub(crate) fn save(store: &SqliteStore, input: Value) -> Result<Value, String> {
    let (tenant, kind, id) = scope(&input)?;
    let patch = input["patch"]
        .as_object()
        .ok_or("local_teacher_record_patch_required")?;
    let keys = if kind == "observation" {
        vec!["note", "status", "tags"]
    } else {
        vec![
            "summary",
            "followUpNote",
            "followUpOn",
            "status",
            "topics",
            "participantType",
            "channel",
            "counselingAtMs",
        ]
    };
    if patch.keys().any(|key| !keys.contains(&key.as_str())) {
        return Err("local_teacher_record_field_protected".into());
    }
    // Existing writer bounds are explicit errors here, never silent text truncation.
    for (field, limit) in [("summary", 5000), ("followUpNote", 2000), ("note", 1000)] {
        if patch
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|s| s.chars().count() > limit)
        {
            return Err(format!("local_teacher_record_{field}_too_large"));
        }
    }
    for (field, limit) in [("tags", 20), ("topics", 8)] {
        if let Some(value) = patch.get(field) {
            let entries = value
                .as_array()
                .ok_or("local_teacher_record_list_invalid")?;
            if entries.len() > limit
                || entries
                    .iter()
                    .any(|item| item.as_str().is_none_or(|s| s.chars().count() > 60))
            {
                return Err(format!("local_teacher_record_{field}_too_large"));
            }
        }
    }
    let mut conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let existing = read(&tx, &tenant, &kind, &id)?;
    if kind == "observation" {
        if let Some(mutation) = input["mutationId"].as_str().filter(|s| !s.is_empty()) {
            let replay:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM observation_evidence_mutations WHERE tenant_id=?1 AND mutation_id=?2)",params![tenant,mutation],|r|r.get(0)).map_err(|e|e.to_string())?;
            if replay {
                let records = store.evidence_save_in_transaction(
                    &tx,
                    &tenant,
                    vec![],
                    mutation,
                    Some(&input),
                )?;
                let record = records
                    .into_iter()
                    .next()
                    .ok_or("local_teacher_record_readback_failed")?;
                if existing.as_ref() != Some(&record) {
                    return Err("local_teacher_record_revision_conflict".into());
                }
                tx.commit().map_err(|e| e.to_string())?;
                return Ok(
                    json!({"ok":true,"revision":revision(&record,&kind),"record":record,"verified":true,"replayed":true}),
                );
            }
        }
    }
    if input.get("expectedRevision").is_none() {
        return Err("local_teacher_record_expected_revision_required".into());
    }
    let mut record = if let Some(existing) = existing {
        if revision(&existing, &kind) != input["expectedRevision"] {
            return Err("local_teacher_record_revision_conflict".into());
        }
        existing
    } else {
        if kind != "counseling" {
            return Err("local_teacher_record_not_found".into());
        }
        if input["expectedRevision"] != json!(0) {
            return Err("local_teacher_record_revision_conflict".into());
        }
        let code = crate::normalize_student_code(input.get("studentCode"));
        let raw: Option<String> = tx
            .query_row(
                "SELECT payload_json FROM teacher_roster_snapshots WHERE tenant_id=?1",
                params![tenant],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let roster: Value = raw
            .and_then(|s| serde_json::from_str(&s).ok())
            .ok_or("quick_roster_unavailable")?;
        let students = roster
            .as_array()
            .or_else(|| roster["students"].as_array())
            .ok_or("quick_roster_unavailable")?;
        let student = students
            .iter()
            .find(|s| s["id"] == code && s["status"] != "archived")
            .ok_or("quick_observation_student_not_found")?;
        json!({"tenantId":tenant,"sessionId":id,"studentCode":code,"studentName":student["displayName"],"classNo":student["classNo"],"createdAtMs":now_ms()})
    };
    for (key, value) in patch {
        record[key] = value.clone();
    }
    if kind == "observation" {
        let mutation = input["mutationId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("observation_mutation_id_required")?;
        let reason = input["correctionReason"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or("observation_correction_reason_required")?;
        record["expectedRevisionId"] = input["expectedRevision"].clone();
        record["correctionReason"] = json!(reason);
        let records = store.evidence_save_in_transaction(
            &tx,
            &tenant,
            vec![record],
            mutation,
            Some(&input),
        )?;
        record = records
            .into_iter()
            .next()
            .ok_or("local_teacher_record_readback_failed")?;
    } else {
        record["updatedAtMs"] =
            json!(now_ms().max(input["expectedRevision"].as_i64().unwrap_or(0) + 1));
        record =
            crate::canonical_write_transactions::upsert_teacher_counseling_session(&tx, record)?;
    }
    let actual = read(&tx, &tenant, &kind, &id)?.ok_or("local_teacher_record_readback_failed")?;
    if actual != record {
        return Err("local_teacher_record_readback_failed".into());
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(json!({"ok":true,"revision":revision(&actual,&kind),"record":actual,"verified":true}))
}
#[tauri::command]
pub(crate) fn get_local_teacher_record(
    state: tauri::State<'_, AppState>,
    tenant_id: String,
    kind: String,
    record_id: String,
) -> Value {
    let result = (|| {
        let store = state
            .store
            .lock()
            .map_err(|_| "local_store_unavailable")?
            .clone()
            .ok_or("local_store_unavailable")?;
        let (tenant, kind, id) =
            scope(&json!({"tenantId":tenant_id,"kind":kind,"recordId":record_id}))?;
        get(&store, &tenant, &kind, &id)
    })();
    result.unwrap_or_else(|error: String| json!({"ok":false,"error":error}))
}
#[tauri::command]
pub(crate) fn save_local_teacher_record(state: tauri::State<'_, AppState>, input: Value) -> Value {
    let result = (|| {
        let store = state
            .store
            .lock()
            .map_err(|_| "local_store_unavailable")?
            .clone()
            .ok_or("local_store_unavailable")?;
        save(&store, input)
    })();
    result.unwrap_or_else(|error: String| json!({"ok":false,"error":error}))
}
