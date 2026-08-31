use crate::SqliteStore;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const OPERATIONS: [&str; 7] = [
    "student_record_save_drafts",
    "counseling_record_save_draft",
    "counseling_record_prepare_create",
    "work_notes_save_draft",
    "materials_save_draft",
    "materials_update_draft",
    "materials_restructure_page",
];
const LOCAL_RECEIPT_TTL_MS: i64 = 24 * 60 * 60 * 1000;

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}
fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 260
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}
fn required_id(value: Option<&Value>) -> Result<String, String> {
    let value = value
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if !valid_id(&value) {
        return Err("classaimate_mcp_write_job_invalid".to_string());
    }
    Ok(value)
}
fn required_object(value: Option<&Value>) -> Result<&serde_json::Map<String, Value>, String> {
    value
        .and_then(Value::as_object)
        .ok_or_else(|| "classaimate_mcp_write_job_invalid".to_string())
}
fn decode(raw: String) -> Result<Value, String> {
    serde_json::from_str(&raw).map_err(|_| "classaimate_mcp_local_payload_invalid".to_string())
}
fn draft_text(payload: &Value, scope: &Value) -> String {
    match scope
        .get("recordType")
        .and_then(Value::as_str)
        .unwrap_or("")
    {
        "subjects" => payload
            .get("subjectComments")
            .and_then(Value::as_array)
            .and_then(|rows| {
                rows.iter()
                    .find(|row| row.get("subject") == scope.get("subject"))
            })
            .and_then(|row| row.get("comment"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        "creative" => payload
            .get("creativeComments")
            .and_then(Value::as_array)
            .and_then(|rows| {
                rows.iter()
                    .find(|row| row.get("area") == scope.get("creativeArea"))
            })
            .and_then(|row| row.get("comment"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        _ => payload
            .get("behaviorComment")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    }
}
fn draft_matches_scope(payload: &Value, scope: &Value) -> bool {
    if payload.get("recordType") != scope.get("recordType") {
        return false;
    }
    match scope
        .get("recordType")
        .and_then(Value::as_str)
        .unwrap_or("")
    {
        "subjects" => payload
            .get("subjectComments")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .any(|row| row.get("subject") == scope.get("subject"))
            })
            .unwrap_or(false),
        "creative" => payload
            .get("creativeComments")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .any(|row| row.get("area") == scope.get("creativeArea"))
            })
            .unwrap_or(false),
        "behavior" => true,
        _ => false,
    }
}
fn latest_draft(
    store: &SqliteStore,
    tenant: &str,
    student: &str,
    scope: &Value,
) -> Result<String, String> {
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let mut stmt = conn.prepare("SELECT draft_id,payload_json,updated_at_ms FROM student_record_drafts WHERE tenant_id=?1 AND student_code=?2 ORDER BY updated_at_ms DESC,draft_id DESC")
        .map_err(|e| format!("db_classaimate_mcp_draft_query_failed:{e}"))?;
    let rows = stmt
        .query_map(params![tenant, student], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|e| format!("db_classaimate_mcp_draft_query_failed:{e}"))?;
    for row in rows {
        let (draft_id, raw, updated_at_ms) =
            row.map_err(|e| format!("db_classaimate_mcp_draft_row_failed:{e}"))?;
        let payload = decode(raw)?;
        if draft_matches_scope(&payload, scope) {
            return Ok(digest(&serde_json::to_string(&json!({
                "draftId": draft_id, "text": draft_text(&payload, scope), "updatedAtMs": updated_at_ms
            })).map_err(|_| "classaimate_mcp_local_payload_invalid".to_string())?));
        }
    }
    Ok(digest(r#"{"draftId":"","text":"","updatedAtMs":0}"#))
}

fn exact_student_batch(store: &SqliteStore, tenant: &str, data: &Value) -> Result<bool, String> {
    let draft_set_id = data.get("draftSetId").and_then(Value::as_str).unwrap_or("");
    let expected = data
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(|| "classaimate_mcp_write_job_invalid".to_string())?;
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM student_record_draft_sets WHERE tenant_id=?1 AND draft_set_id=?2",
            params![tenant, draft_set_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("db_classaimate_mcp_draft_query_failed:{e}"))?;
    if exists.is_none() {
        return Ok(false);
    }
    let mut stmt = conn
        .prepare(
            "SELECT payload_json FROM student_record_drafts WHERE tenant_id=?1 AND draft_set_id=?2",
        )
        .map_err(|e| format!("db_classaimate_mcp_draft_query_failed:{e}"))?;
    let values = stmt
        .query_map(params![tenant, draft_set_id], |row| row.get::<_, String>(0))
        .map_err(|e| format!("db_classaimate_mcp_draft_query_failed:{e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("db_classaimate_mcp_draft_row_failed:{e}"))?
        .into_iter()
        .map(decode)
        .collect::<Result<Vec<_>, _>>()?;
    if values.len() != expected.len() {
        return Err("DRAFT_CONFLICT".to_string());
    }
    let scope = data.get("scope").unwrap_or(&Value::Null);
    for source in expected {
        let code = source
            .get("studentCode")
            .and_then(Value::as_str)
            .unwrap_or("");
        let value = values
            .iter()
            .find(|row| row.get("studentCode").and_then(Value::as_str) == Some(code))
            .ok_or_else(|| "DRAFT_CONFLICT".to_string())?;
        if draft_text(value, scope) != source.get("text").and_then(Value::as_str).unwrap_or("")
            || value.get("status").and_then(Value::as_str) != Some("draft")
            || value.get("sourceLabel").and_then(Value::as_str) != Some("내 ChatGPT")
            || value.get("teacherReviewRequired").and_then(Value::as_bool) != Some(true)
        {
            return Err("DRAFT_CONFLICT".to_string());
        }
    }
    Ok(true)
}

fn save_student(store: &SqliteStore, tenant: &str, data: &Value) -> Result<Value, String> {
    let draft_set_id = required_id(data.get("draftSetId"))?;
    let scope = data
        .get("scope")
        .filter(|value| value.is_object())
        .ok_or_else(|| "classaimate_mcp_write_job_invalid".to_string())?;
    let rows = data
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(|| "classaimate_mcp_write_job_invalid".to_string())?;
    if rows.is_empty() || rows.len() > 30 {
        return Err("classaimate_mcp_write_job_invalid".to_string());
    }
    if !exact_student_batch(store, tenant, data)? {
        for row in rows {
            let student = required_id(row.get("studentCode"))?;
            let baseline = row
                .get("baselineDigest")
                .and_then(Value::as_str)
                .unwrap_or("");
            let text = row.get("text").and_then(Value::as_str).unwrap_or("").trim();
            if baseline.len() != 64
                || !baseline.chars().all(|c| c.is_ascii_hexdigit())
                || text.is_empty()
                || text.chars().count() > 2400
                || latest_draft(store, tenant, &student, scope)? != baseline
            {
                return Err("DRAFT_CONFLICT".to_string());
            }
        }
        let now = now_ms();
        let record_type = scope
            .get("recordType")
            .and_then(Value::as_str)
            .unwrap_or("");
        let subject = scope.get("subject").and_then(Value::as_str).unwrap_or("");
        let creative_area = scope
            .get("creativeArea")
            .and_then(Value::as_str)
            .unwrap_or("");
        let drafts = rows.iter().map(|row| {
            let code = row.get("studentCode").and_then(Value::as_str).unwrap_or("");
            let text = row.get("text").and_then(Value::as_str).unwrap_or("");
            json!({
                "tenantId":tenant,"draftSetId":draft_set_id,"draftId":format!("{draft_set_id}__{code}"),
                "studentCode":code,"studentName":row.get("studentName").and_then(Value::as_str).unwrap_or(""),
                "classNo":row.get("classNo").and_then(Value::as_i64).unwrap_or(0),"recordType":record_type,"status":"draft",
                "behaviorComment":if record_type=="behavior" { text } else { "" },
                "subjectComments":if record_type=="subjects" { json!([{"subject":subject,"comment":text}]) } else { json!([]) },
                "creativeComments":if record_type=="creative" { json!([{"area":creative_area,"comment":text}]) } else { json!([]) },
                "sourceType":"studentRecordMcp","sourceLabel":"내 ChatGPT","teacherReviewRequired":true,
                "createdAtMs":now,"updatedAtMs":now
            })
        }).collect::<Vec<_>>();
        store.save_student_record_draft_batch(json!({
            "tenantId":tenant,
            "draftSet":{"tenantId":tenant,"draftSetId":draft_set_id,"status":"draft","recordTypes":[record_type],
                "subject":subject,"creativeArea":creative_area,"fromDate":scope.get("fromDate"),"toDate":scope.get("toDate"),
                "sourceType":"studentRecordMcp","sourceLabel":"내 ChatGPT","teacherReviewRequired":true,
                "createdAtMs":now,"updatedAtMs":now},
            "drafts":drafts
        }))?;
        if !exact_student_batch(store, tenant, data)? {
            return Err("LOCAL_STORE_WRITE_FAILED".to_string());
        }
    }
    Ok(json!({"result":data,"localRef":format!("student-record-draft-set:{draft_set_id}")}))
}

fn save_counseling(store: &SqliteStore, tenant: &str, data: &Value) -> Result<Value, String> {
    let draft_id = required_id(data.get("draftId"))?;
    let counseling_ref = required_id(data.get("counselingRef"))?;
    let session = store
        .get_teacher_counseling_session(tenant.to_string(), counseling_ref.clone())?
        .ok_or_else(|| "COUNSELING_SOURCE_NOT_FOUND".to_string())?;
    let summary = data
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let follow_up = data
        .get("followUpNote")
        .and_then(Value::as_str)
        .unwrap_or("");
    if summary.is_empty()
        || summary.chars().count() > 5000
        || follow_up.chars().count() > 2000
        || data.get("status").and_then(Value::as_str) != Some("pending")
    {
        return Err("classaimate_mcp_write_job_invalid".to_string());
    }
    let payload = json!({"tenantId":tenant,"draftId":draft_id,"counselingRef":counseling_ref,
        "studentCode":session.get("studentCode").and_then(Value::as_str).unwrap_or(""),"summary":summary,
        "followUpNote":follow_up,"status":"pending","sourceType":"classAimatePublicMcp","sourceLabel":"내 ChatGPT","teacherReviewRequired":true});
    let raw = serde_json::to_string(&payload)
        .map_err(|_| "classaimate_mcp_local_payload_invalid".to_string())?;
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let existing: Option<String> = conn.query_row("SELECT payload_json FROM teacher_counseling_mcp_drafts WHERE tenant_id=?1 AND draft_id=?2",params![tenant,draft_id],|row|row.get(0)).optional()
        .map_err(|e|format!("db_classaimate_mcp_counseling_query_failed:{e}"))?;
    if let Some(value) = existing {
        if decode(value)? != payload {
            return Err("DRAFT_CONFLICT".to_string());
        }
    } else {
        conn.execute("INSERT INTO teacher_counseling_mcp_drafts(tenant_id,draft_id,counseling_ref,student_code,payload_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?6)",params![tenant,draft_id,counseling_ref,payload.get("studentCode").and_then(Value::as_str).unwrap_or(""),raw,now_ms()]).map_err(|e|format!("db_classaimate_mcp_counseling_save_failed:{e}"))?;
    }
    Ok(json!({"result":data,"localRef":format!("teacher-counseling-mcp-draft:{draft_id}")}))
}

fn create_counseling(store: &SqliteStore, tenant: &str, data: &Value) -> Result<Value, String> {
    let counseling_id = required_id(data.get("counselingId"))?;
    let student_code = required_id(data.get("studentCode"))?;
    let student_name = data.get("studentName").and_then(Value::as_str).unwrap_or("").trim();
    let counseling_at_ms = data.get("counselingAtMs").and_then(Value::as_i64).unwrap_or(0);
    let summary = data.get("summary").and_then(Value::as_str).unwrap_or("").trim();
    let follow_up_note = data.get("followUpNote").and_then(Value::as_str).unwrap_or("");
    let participant_type = data.get("participantType").and_then(Value::as_str).unwrap_or("");
    let channel = data.get("channel").and_then(Value::as_str).unwrap_or("");
    let status = data.get("status").and_then(Value::as_str).unwrap_or("");
    if student_name.is_empty() || student_name.chars().count() > 160 || counseling_at_ms <= 0
        || !["student", "guardian", "student_guardian"].contains(&participant_type)
        || !["in_person", "phone", "online"].contains(&channel)
        || !["completed", "follow_up"].contains(&status)
        || summary.is_empty() || summary.chars().count() > 5000 || follow_up_note.chars().count() > 2000
        || !data.get("topics").map(Value::is_array).unwrap_or(false)
    {
        return Err("classaimate_mcp_write_job_invalid".to_string());
    }
    if let Some(existing) = store.get_teacher_counseling_session(tenant.to_string(), counseling_id.clone())? {
        if existing.get("studentCode").and_then(Value::as_str) != Some(student_code.as_str())
            || existing.get("counselingAtMs").and_then(Value::as_i64) != Some(counseling_at_ms)
            || existing.get("summary").and_then(Value::as_str) != Some(summary)
        {
            return Err("DRAFT_CONFLICT".to_string());
        }
    } else {
        let mut record = data.as_object().cloned().ok_or_else(|| "classaimate_mcp_write_job_invalid".to_string())?;
        record.insert("tenantId".to_string(), json!(tenant));
        record.insert("id".to_string(), json!(counseling_id));
        record.insert("docId".to_string(), json!(counseling_id));
        record.insert("sessionId".to_string(), json!(counseling_id));
        record.insert("studentCode".to_string(), json!(student_code));
        record.insert("recordOrigin".to_string(), json!("teacher_local_counseling"));
        record.insert("sourceType".to_string(), json!("classAimatePublicMcp"));
        record.insert("sourceLabel".to_string(), json!("내 ChatGPT"));
        record.insert("teacherReviewRequired".to_string(), json!(false));
        record.insert("createdAtMs".to_string(), json!(now_ms()));
        record.insert("updatedAtMs".to_string(), json!(now_ms()));
        store.upsert_teacher_counseling_session(Value::Object(record))?;
    }
    let readback = store.get_teacher_counseling_session(tenant.to_string(), counseling_id.clone())?
        .ok_or_else(|| "LOCAL_STORE_WRITE_FAILED".to_string())?;
    if readback.get("studentCode").and_then(Value::as_str) != Some(student_code.as_str())
        || readback.get("summary").and_then(Value::as_str) != Some(summary)
    {
        return Err("LOCAL_STORE_WRITE_FAILED".to_string());
    }
    Ok(json!({"result":data,"localRef":format!("teacher-counseling-session:{counseling_id}")}))
}

fn save_work_note(store: &SqliteStore, tenant: &str, data: &Value) -> Result<Value, String> {
    let page_id = required_id(data.get("pageId"))?;
    let parent_id = required_id(data.get("parentPageRef"))?;
    let document_ref = required_id(data.get("documentRef"))?;
    if store
        .get_work_note(tenant.to_string(), parent_id.clone())?
        .is_none()
    {
        return Err("WORK_NOTE_PARENT_NOT_FOUND".to_string());
    }
    let title = data
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let markdown = data.get("markdown").and_then(Value::as_str).unwrap_or("");
    let blocks = data
        .get("blocks")
        .and_then(Value::as_array)
        .ok_or_else(|| "classaimate_mcp_write_job_invalid".to_string())?;
    if title.is_empty() || title.chars().count() > 300 || markdown.chars().count() > 200_000 {
        return Err("classaimate_mcp_write_job_invalid".to_string());
    }
    if let Some(existing) = store.get_work_note(tenant.to_string(), page_id.clone())? {
        if existing.get("parentId").and_then(Value::as_str) != Some(parent_id.as_str())
            || existing.get("title").and_then(Value::as_str) != Some(title)
            || existing.get("markdown").and_then(Value::as_str) != Some(markdown)
            || existing.get("blocks").and_then(Value::as_array) != Some(blocks)
        {
            return Err("DRAFT_CONFLICT".to_string());
        }
    } else {
        let pages = store.list_work_notes(tenant.to_string(), String::new())?;
        let position = pages
            .iter()
            .filter(|row| row.get("parentId").and_then(Value::as_str) == Some(parent_id.as_str()))
            .map(|row| row.get("position").and_then(Value::as_i64).unwrap_or(0))
            .max()
            .map(|value| value + 1)
            .unwrap_or(0);
        store.upsert_work_note(json!({"tenantId":tenant,"pageId":page_id,"parentId":parent_id,"title":title,"emoji":"📝","position":position,
            "properties":{"sourceType":"classAimatePublicMcp","sourceLabel":"내 ChatGPT","teacherReviewRequired":true,"documentRef":document_ref},
            "blocks":blocks,"markdown":markdown,"createdAtMs":now_ms(),"updatedAtMs":now_ms()}))?;
    }
    Ok(json!({"result":data,"localRef":format!("work-note-page:{page_id}")}))
}

fn update_work_note(store: &SqliteStore, tenant: &str, data: &Value) -> Result<Value, String> {
    let page_ref = required_id(data.get("pageRef"))?;
    let existing = store.get_work_note(tenant.to_string(), page_ref.clone())?
        .ok_or_else(|| "DRAFT_CONFLICT".to_string())?;
    let expected_revision = data.get("expectedRevision").and_then(Value::as_i64).unwrap_or(0);
    let title = data.get("title").and_then(Value::as_str).unwrap_or("").trim();
    let markdown = data.get("markdown").and_then(Value::as_str).unwrap_or("");
    let blocks = data.get("blocks").and_then(Value::as_array)
        .ok_or_else(|| "classaimate_mcp_write_job_invalid".to_string())?;
    let source = existing.get("properties").and_then(|value| value.get("sourceType")).and_then(Value::as_str);
    if !page_ref.starts_with("mcp_")
        || !existing.get("title").and_then(Value::as_str).unwrap_or("").starts_with("ChatGPT 초안 ·")
        || source != Some("classAimatePublicMcp")
        || existing.get("updatedAtMs").and_then(Value::as_i64) != Some(expected_revision)
        || !title.starts_with("ChatGPT 초안 ·") || title.chars().count() > 300
        || markdown.chars().count() > 200_000
    {
        return Err("DRAFT_CONFLICT".to_string());
    }
    let mut updated = existing.as_object().cloned().ok_or_else(|| "DRAFT_CONFLICT".to_string())?;
    updated.insert("tenantId".to_string(), json!(tenant));
    updated.insert("pageId".to_string(), json!(page_ref));
    updated.insert("title".to_string(), json!(title));
    updated.insert("markdown".to_string(), json!(markdown));
    updated.insert("blocks".to_string(), Value::Array(blocks.clone()));
    updated.insert("updatedAtMs".to_string(), json!(now_ms()));
    store.upsert_work_note(Value::Object(updated))?;
    let readback = store.get_work_note(tenant.to_string(), page_ref.clone())?
        .ok_or_else(|| "LOCAL_STORE_WRITE_FAILED".to_string())?;
    if readback.get("title").and_then(Value::as_str) != Some(title)
        || readback.get("markdown").and_then(Value::as_str) != Some(markdown)
    {
        return Err("LOCAL_STORE_WRITE_FAILED".to_string());
    }
    Ok(json!({"result":data,"localRef":format!("work-note-page:{page_ref}")}))
}

fn collect_document_references(value: &Value, references: &mut BTreeSet<String>) {
    if let Some(values) = value.as_array() {
        for nested in values {
            collect_document_references(nested, references);
        }
        return;
    }
    let Some(object) = value.as_object() else {
        return;
    };
    let attachment_id = object
        .get("attachmentId")
        .or_else(|| object.get("localAttachmentId"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if !attachment_id.is_empty() {
        references.insert(format!("attachment:{attachment_id}"));
    }
    if object.get("type").and_then(Value::as_str) == Some("link") {
        let href = object
            .get("href")
            .and_then(Value::as_str)
            .or_else(|| {
                object
                    .get("attrs")
                    .and_then(Value::as_object)
                    .and_then(|attrs| attrs.get("href"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("")
            .trim();
        if !href.is_empty() {
            references.insert(format!("link:{href}"));
        }
    }
    if matches!(
        object.get("type").and_then(Value::as_str),
        Some("pageLinkBlock" | "page")
    ) {
        let page_id = object
            .get("pageId")
            .and_then(Value::as_str)
            .or_else(|| object.get("target").and_then(Value::as_str))
            .or_else(|| {
                object
                    .get("attrs")
                    .and_then(Value::as_object)
                    .and_then(|attrs| attrs.get("pageId"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("")
            .trim();
        if !page_id.is_empty() {
            references.insert(format!("page:{page_id}"));
        }
    }
    for nested in object.values() {
        collect_document_references(nested, references);
    }
}

fn restructure_work_note(store: &SqliteStore, tenant: &str, data: &Value) -> Result<Value, String> {
    let page_ref = required_id(data.get("pageRef"))?;
    let existing = store
        .get_work_note(tenant.to_string(), page_ref.clone())?
        .ok_or_else(|| "MATERIAL_REVISION_CONFLICT".to_string())?;
    let expected_revision = data
        .get("expectedRevision")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let workspace = data.get("workspace").and_then(Value::as_str).unwrap_or("");
    let markdown = data.get("markdown").and_then(Value::as_str).unwrap_or("");
    let blocks = data
        .get("blocks")
        .and_then(Value::as_array)
        .ok_or_else(|| "classaimate_mcp_write_job_invalid".to_string())?;
    if !matches!(workspace, "work_materials" | "lesson_materials")
        || existing.get("updatedAtMs").and_then(Value::as_i64) != Some(expected_revision)
    {
        return Err("MATERIAL_REVISION_CONFLICT".to_string());
    }
    if blocks.len() > 5_000 || markdown.chars().count() > 200_000 {
        return Err("classaimate_mcp_write_job_invalid".to_string());
    }
    let mut existing_references = BTreeSet::new();
    collect_document_references(
        existing.get("blocks").unwrap_or(&Value::Null),
        &mut existing_references,
    );
    let mut next_references = BTreeSet::new();
    collect_document_references(
        data.get("blocks").unwrap_or(&Value::Null),
        &mut next_references,
    );
    if !existing_references.is_subset(&next_references) {
        return Err("MATERIAL_REFERENCE_CONFLICT".to_string());
    }
    let mut updated = existing
        .as_object()
        .cloned()
        .ok_or_else(|| "MATERIAL_REVISION_CONFLICT".to_string())?;
    updated.insert("tenantId".to_string(), json!(tenant));
    updated.insert("pageId".to_string(), json!(page_ref));
    updated.insert("blocks".to_string(), Value::Array(blocks.clone()));
    updated.insert("markdown".to_string(), json!(markdown));
    updated.insert("updatedAtMs".to_string(), json!(now_ms()));
    store.upsert_work_note(Value::Object(updated))?;
    let readback = store
        .get_work_note(tenant.to_string(), page_ref.clone())?
        .ok_or_else(|| "LOCAL_STORE_WRITE_FAILED".to_string())?;
    if readback.get("markdown").and_then(Value::as_str) != Some(markdown)
        || readback.get("blocks").and_then(Value::as_array) != Some(blocks)
    {
        return Err("LOCAL_STORE_WRITE_FAILED".to_string());
    }
    Ok(json!({
        "result":{"pageRef":page_ref,"appliedFromRevision":expected_revision},
        "localRef":format!("work-note-page:{page_ref}")
    }))
}

pub(crate) fn ensure_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(r#"
      CREATE TABLE IF NOT EXISTS teacher_counseling_mcp_drafts(
        tenant_id TEXT NOT NULL,draft_id TEXT NOT NULL,counseling_ref TEXT NOT NULL,student_code TEXT NOT NULL,
        payload_json TEXT NOT NULL,created_at_ms INTEGER NOT NULL,updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY(tenant_id,draft_id)) WITHOUT ROWID,STRICT;
      CREATE INDEX IF NOT EXISTS idx_teacher_counseling_mcp_drafts_source ON teacher_counseling_mcp_drafts(tenant_id,counseling_ref,updated_at_ms DESC);
      CREATE TABLE IF NOT EXISTS classaimate_mcp_local_write_receipts(
        tenant_id TEXT NOT NULL,receipt_id TEXT NOT NULL,operation TEXT NOT NULL,request_sha256 TEXT NOT NULL,
        result_json TEXT NOT NULL,local_ref TEXT NOT NULL,created_at_ms INTEGER NOT NULL,
        PRIMARY KEY(tenant_id,receipt_id)) WITHOUT ROWID,STRICT;
    "#).map_err(|e|format!("db_classaimate_mcp_schema_failed:{e}"))?;
    conn.execute(
        "DELETE FROM classaimate_mcp_local_write_receipts WHERE created_at_ms<?1",
        params![now_ms() - LOCAL_RECEIPT_TTL_MS],
    )
    .map_err(|e| format!("db_classaimate_mcp_receipt_cleanup_failed:{e}"))?;
    Ok(())
}

pub(crate) fn apply(store: &SqliteStore, input: &Value) -> Result<Value, String> {
    required_object(Some(input))?;
    let tenant = required_id(input.get("tenantId"))?;
    let receipt = required_id(input.get("receiptId"))?;
    let operation = input.get("operation").and_then(Value::as_str).unwrap_or("");
    let request_sha = input
        .get("requestSha256")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !OPERATIONS.contains(&operation)
        || request_sha.len() != 64
        || !request_sha.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err("classaimate_mcp_write_job_invalid".to_string());
    }
    {
        let conn = store
            .conn
            .lock()
            .map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "DELETE FROM classaimate_mcp_local_write_receipts WHERE created_at_ms<?1",
            params![now_ms() - LOCAL_RECEIPT_TTL_MS],
        )
        .map_err(|e| format!("db_classaimate_mcp_receipt_cleanup_failed:{e}"))?;
        let existing:Option<(String,String,String,String)>=conn.query_row("SELECT operation,request_sha256,result_json,local_ref FROM classaimate_mcp_local_write_receipts WHERE tenant_id=?1 AND receipt_id=?2",params![tenant,receipt],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional().map_err(|e|format!("db_classaimate_mcp_receipt_query_failed:{e}"))?;
        if let Some((saved_operation, saved_sha, result, local_ref)) = existing {
            if saved_operation != operation || saved_sha != request_sha {
                return Err("IDEMPOTENCY_CONFLICT".to_string());
            }
            return Ok(json!({"replayed":true,"result":decode(result)?,"localRef":local_ref}));
        }
    }
    let data = input
        .get("data")
        .filter(|value| value.is_object())
        .ok_or_else(|| "classaimate_mcp_write_job_invalid".to_string())?;
    let saved = match operation {
        "student_record_save_drafts" => save_student(store, &tenant, data)?,
        "counseling_record_save_draft" => save_counseling(store, &tenant, data)?,
        "counseling_record_prepare_create" => create_counseling(store, &tenant, data)?,
        "materials_update_draft" => update_work_note(store, &tenant, data)?,
        "materials_restructure_page" => restructure_work_note(store, &tenant, data)?,
        _ => save_work_note(store, &tenant, data)?,
    };
    let result = saved.get("result").cloned().unwrap_or(Value::Null);
    let local_ref = saved.get("localRef").and_then(Value::as_str).unwrap_or("");
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    conn.execute("INSERT INTO classaimate_mcp_local_write_receipts(tenant_id,receipt_id,operation,request_sha256,result_json,local_ref,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![tenant,receipt,operation,request_sha,serde_json::to_string(&result).map_err(|_|"classaimate_mcp_local_payload_invalid".to_string())?,local_ref,now_ms()]).map_err(|e|format!("db_classaimate_mcp_receipt_save_failed:{e}"))?;
    Ok(json!({"replayed":false,"result":result,"localRef":local_ref}))
}

pub(crate) fn list_counseling_drafts(
    store: &SqliteStore,
    tenant: &str,
    counseling_ref: &str,
    limit: i64,
) -> Result<Vec<Value>, String> {
    if !valid_id(tenant) || (!counseling_ref.is_empty() && !valid_id(counseling_ref)) {
        return Err("classaimate_mcp_write_job_invalid".to_string());
    }
    let bounded = limit.clamp(1, 50);
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let (sql, params_values) = if counseling_ref.is_empty() {
        ("SELECT payload_json,created_at_ms,updated_at_ms FROM teacher_counseling_mcp_drafts WHERE tenant_id=?1 ORDER BY updated_at_ms DESC,draft_id LIMIT ?2",vec![tenant.to_string(),bounded.to_string()])
    } else {
        ("SELECT payload_json,created_at_ms,updated_at_ms FROM teacher_counseling_mcp_drafts WHERE tenant_id=?1 AND counseling_ref=?2 ORDER BY updated_at_ms DESC,draft_id LIMIT ?3",vec![tenant.to_string(),counseling_ref.to_string(),bounded.to_string()])
    };
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| format!("db_classaimate_mcp_counseling_query_failed:{e}"))?;
    let mut rows = if counseling_ref.is_empty() {
        stmt.query(params![params_values[0], bounded])
    } else {
        stmt.query(params![params_values[0], params_values[1], bounded])
    }
    .map_err(|e| format!("db_classaimate_mcp_counseling_query_failed:{e}"))?;
    let mut result = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|e| format!("db_classaimate_mcp_counseling_row_failed:{e}"))?
    {
        let mut value = decode(
            row.get::<_, String>(0)
                .map_err(|e| format!("db_classaimate_mcp_counseling_row_failed:{e}"))?,
        )?;
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "createdAtMs".to_string(),
                json!(row.get::<_, i64>(1).unwrap_or(0)),
            );
            object.insert(
                "updatedAtMs".to_string(),
                json!(row.get::<_, i64>(2).unwrap_or(0)),
            );
        }
        result.push(value);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn request(receipt_id: &str, data: Value) -> Value {
        json!({
            "tenantId":"tenant-a",
            "receiptId":receipt_id,
            "operation":"materials_restructure_page",
            "requestSha256":"a".repeat(64),
            "data":data
        })
    }

    #[test]
    fn restructure_page_requires_exact_revision_and_preserves_document_references() {
        let directory = std::env::temp_dir().join(format!(
            "classaimate-mcp-restructure-{}",
            crate::random_url_token()
        ));
        fs::create_dir_all(&directory).expect("create test directory");
        let store = SqliteStore::open(directory.join("store.sqlite3")).expect("open test store");
        let original_blocks = json!([
            {"id":"source","type":"text","content":[{"type":"text","text":"출처","marks":[{"type":"link","attrs":{"href":"https://example.com/source"}}]}]},
            {"id":"file","type":"attachment","localAttachmentId":"attachment-a","fileName":"원본.pdf"}
        ]);
        store
            .upsert_work_note(json!({
                "tenantId":"tenant-a","pageId":"page-a","parentId":null,"title":"원본 제목",
                "emoji":"📄","position":0,"properties":{"source":"teacher"},
                "blocks":original_blocks,"markdown":"원문","createdAtMs":10,"updatedAtMs":10
            }))
            .expect("seed work note");
        let reorganized = json!([
            {"id":"summary","type":"heading","attrs":{"level":1},"content":[{"type":"text","text":"[AI 정리본]"}]},
            {"id":"original","type":"toggle","attrs":{"summary":"원본 보기"},"content":original_blocks}
        ]);
        let data = json!({
            "workspace":"work_materials","pageRef":"page-a","expectedRevision":10,
            "blocks":reorganized,"markdown":"# [AI 정리본]\n\n<details>원본 보기</details>"
        });
        let applied =
            apply(&store, &request("receipt-a", data.clone())).expect("apply restructure");
        assert_eq!(applied["result"]["appliedFromRevision"], 10);
        let readback = store
            .get_work_note("tenant-a".to_string(), "page-a".to_string())
            .expect("read work note")
            .expect("work note exists");
        assert_eq!(readback["title"], "원본 제목");
        assert_eq!(readback["properties"]["source"], "teacher");
        assert_eq!(readback["blocks"], reorganized);
        assert_eq!(
            apply(&store, &request("receipt-b", data)).expect_err("reject stale revision"),
            "MATERIAL_REVISION_CONFLICT"
        );
        let current_revision = readback["updatedAtMs"].as_i64().expect("current revision");
        let missing_references = json!({
            "workspace":"work_materials","pageRef":"page-a","expectedRevision":current_revision,
            "blocks":[{"id":"summary","type":"text","text":"reference 제거"}],"markdown":"reference 제거"
        });
        assert_eq!(
            apply(&store, &request("receipt-c", missing_references))
                .expect_err("reject removed references"),
            "MATERIAL_REFERENCE_CONFLICT"
        );
        drop(store);
        fs::remove_dir_all(directory).expect("remove test directory");
    }
}
