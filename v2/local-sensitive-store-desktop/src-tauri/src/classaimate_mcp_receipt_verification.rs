use rusqlite::{params, types::ValueRef, Connection, OptionalExtension};
use serde_json::{json, Map, Number, Value};

pub(super) const ENVELOPE: &str = "classaimate_mcp_local_receipt_v2";
#[cfg(test)]
thread_local! { static FAIL_POST_COMMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[cfg(test)]
pub(super) fn fail_next_post_commit_check() {
    FAIL_POST_COMMIT.with(|flag| flag.set(true));
}
pub(super) fn verify_after_commit(
    conn: &Connection,
    tenant: &str,
    envelope: &Value,
) -> Result<Value, String> {
    #[cfg(test)]
    if FAIL_POST_COMMIT.with(|flag| flag.replace(false)) {
        return Err("LOCAL_STORE_WRITE_FAILED".into());
    }
    verify(conn, tenant, envelope)
}
pub(super) fn locator(operation: &str, data: &Value) -> Value {
    let record_id = match operation {
        "student_record_save_drafts" => &data["draftSetId"],
        "counseling_record_save_draft" => &data["draftId"],
        "counseling_record_prepare_create" => &data["counselingId"],
        _ => data.get("pageRef").unwrap_or(&data["pageId"]),
    };
    json!({"operation":operation,"recordId":record_id})
}
pub(super) fn digest(conn: &Connection, tenant: &str, locator: &Value) -> Result<String, String> {
    let tables: &[(&str, &str, &str)] = match locator["operation"].as_str().unwrap_or("") {
        "student_record_save_drafts" => &[
            ("student_record_draft_sets", "draft_set_id", "draft_set_id"),
            ("student_record_drafts", "draft_set_id", "draft_id"),
        ],
        "counseling_record_save_draft" => {
            &[("teacher_counseling_mcp_drafts", "draft_id", "draft_id")]
        }
        "counseling_record_prepare_create" => {
            &[("teacher_counseling_sessions", "session_id", "session_id")]
        }
        "lesson_material_apply_snapshot" => &[
            ("work_note_pages", "page_id", "page_id"),
            ("work_note_pages_fts", "page_id", "page_id"),
            ("lesson_plan_bindings", "page_id", "plan_id"),
            ("work_note_attachments", "page_id", "attachment_id"),
        ],
        "materials_apply_images" => &[
            ("work_note_pages", "page_id", "page_id"),
            ("work_note_pages_fts", "page_id", "page_id"),
            ("work_note_attachments", "page_id", "attachment_id"),
        ],
        "work_notes_save_draft"
        | "materials_save_draft"
        | "materials_update_draft"
        | "materials_restructure_page" => &[
            ("work_note_pages", "page_id", "page_id"),
            ("work_note_pages_fts", "page_id", "page_id"),
        ],
        _ => return Err("classaimate_mcp_write_job_invalid".into()),
    };
    let mut state = Map::new();
    for (table, key, order) in tables {
        let mut statement = conn
            .prepare(&format!(
                "SELECT * FROM {table} WHERE tenant_id=?1 AND {key}=?2 ORDER BY {order}"
            ))
            .map_err(|error| format!("db_mcp_receipt_readback_failed:{error}"))?;
        let columns = statement
            .column_names()
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<_>>();
        let mut rows = statement
            .query(params![tenant, locator["recordId"].as_str().unwrap_or("")])
            .map_err(|error| format!("db_mcp_receipt_readback_failed:{error}"))?;
        let mut values = Vec::new();
        while let Some(row) = rows
            .next()
            .map_err(|error| format!("db_mcp_receipt_readback_failed:{error}"))?
        {
            let mut object = Map::new();
            for (index, column) in columns.iter().enumerate() {
                let value = match row
                    .get_ref(index)
                    .map_err(|error| format!("db_mcp_receipt_readback_failed:{error}"))?
                {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(value) => json!(value),
                    ValueRef::Real(value) => {
                        Value::Number(Number::from_f64(value).ok_or("LOCAL_STORE_WRITE_FAILED")?)
                    }
                    ValueRef::Text(value) => Value::String(
                        std::str::from_utf8(value)
                            .map_err(|_| "LOCAL_STORE_WRITE_FAILED")?
                            .into(),
                    ),
                    ValueRef::Blob(_) => return Err("LOCAL_STORE_WRITE_FAILED".into()),
                };
                object.insert(column.clone(), value);
            }
            values.push(Value::Object(object));
        }
        if values.is_empty()
            && !(locator["operation"] == "lesson_material_apply_snapshot"
                && *table == "work_note_attachments")
        {
            return Err("LOCAL_STORE_WRITE_FAILED".into());
        }
        state.insert(table.to_string(), Value::Array(values));
    }
    crate::sha256_json(&Value::Object(state))
}
pub(super) fn verify(conn: &Connection, tenant: &str, envelope: &Value) -> Result<Value, String> {
    if envelope["kind"] != ENVELOPE
        || !envelope["verification"]["locator"].is_object()
        || digest(conn, tenant, &envelope["verification"]["locator"])?
            != envelope["verification"]["sha256"].as_str().unwrap_or("")
    {
        return Err("LOCAL_STORE_WRITE_FAILED".into());
    }
    Ok(envelope["result"].clone())
}

fn read_id(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
}

fn verify_observation_receipt(conn: &Connection, tenant: &str, data: &Value) -> Result<(), String> {
    let conflict = || "MCP_LOCAL_RECEIPT_CONFLICT".to_string();
    let mutation = data["mutationId"].as_str().ok_or_else(conflict)?;
    let row: Option<(String, String)> = conn.query_row(
        "SELECT request_hash,payload_json FROM observation_evidence_mutations WHERE tenant_id=?1 AND mutation_id=?2",
        params![tenant, mutation], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map_err(|_| conflict())?;
    let (hash, payload) = row.ok_or_else(conflict)?;
    if hash
        != crate::observation_evidence::hash(
            &json!({"tenantId":tenant,"records":data,"mutationId":mutation}),
        )
    {
        return Err(conflict());
    }
    let records: Vec<Value> = serde_json::from_str(&payload).map_err(|_| conflict())?;
    let items = data["items"].as_array().ok_or_else(conflict)?;
    let mut ids = std::collections::HashSet::new();
    if items.is_empty() || records.len() != items.len() {
        return Err(conflict());
    }
    for record in &records {
        let id = record["docId"].as_str().ok_or_else(conflict)?;
        if !ids.insert(id)
            || !items.iter().any(|item| {
                item["docId"] == record["docId"] && item["studentCode"] == record["studentCode"]
            })
        {
            return Err(conflict());
        }
    }
    crate::classaimate_mcp_observations::verify_replay(conn, tenant, data).map_err(|_| conflict())
}

// Tenant comes from the authenticated device connection, never a relay input.
// No apply/replay helper: even their receipt TTL cleanup would be a write.
pub(crate) fn read_only(
    store: &crate::SqliteStore,
    tenant: &str,
    input: &Value,
) -> Result<Value, String> {
    let invalid = || "INVALID_LOCAL_READ_REQUEST".to_string();
    let conflict = || "MCP_LOCAL_RECEIPT_CONFLICT".to_string();
    let fields = input.as_object().ok_or_else(invalid)?;
    let receipt = input["receiptId"].as_str().ok_or_else(invalid)?;
    let operation = input["operation"].as_str().ok_or_else(invalid)?;
    let request_sha = input["requestSha256"].as_str().ok_or_else(invalid)?;
    if fields.len() != 3
        || fields
            .keys()
            .any(|key| !["receiptId", "operation", "requestSha256"].contains(&key.as_str()))
        || !read_id(tenant, 160)
        || !read_id(receipt, 160)
        || !super::OPERATIONS.contains(&operation)
        || request_sha.len() != 64
        || !request_sha
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid());
    }
    let conn = store
        .conn
        .lock()
        .map_err(|_| "MCP_LOCAL_RECEIPT_READ_FAILED")?;
    let row: Option<(String, String, String, String)> = conn.query_row(
        "SELECT operation,request_sha256,result_json,local_ref FROM classaimate_mcp_local_write_receipts WHERE tenant_id=?1 AND receipt_id=?2",
        params![tenant, receipt], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).optional().map_err(|_| "MCP_LOCAL_RECEIPT_READ_FAILED")?;
    let Some((saved_operation, saved_sha, payload, local_ref)) = row else {
        return Ok(json!({"status":"missing"}));
    };
    if saved_operation != operation || saved_sha != request_sha {
        return Err(conflict());
    }
    let envelope: Value = serde_json::from_str(&payload).map_err(|_| conflict())?;
    let result = if envelope["kind"] == ENVELOPE {
        if envelope["verification"]["locator"]["operation"] != operation {
            return Err(conflict());
        }
        let result = verify(&conn, tenant, &envelope).map_err(|_| conflict())?;
        if operation == "materials_apply_images" {
            super::material_assets::verify_files(&conn, &store.data_dir, tenant, &result)
                .map_err(|_| conflict())?;
        }
        if operation == "lesson_material_apply_snapshot" {
            super::material_assets::verify_file_references(
                &conn,
                &store.data_dir,
                tenant,
                &envelope["lessonAssets"],
            )
            .map_err(|_| conflict())?;
        }
        result
    } else if operation == "lesson_observations_manage" {
        verify_observation_receipt(&conn, tenant, &envelope)?;
        envelope.clone()
    } else {
        return Err("MCP_LOCAL_RECEIPT_UNSUPPORTED".into());
    };
    let record = if operation == "lesson_observations_manage" {
        &result["mutationId"]
    } else {
        &envelope["verification"]["locator"]["recordId"]
    };
    let prefix = match operation {
        "student_record_save_drafts" => "student-record-draft-set",
        "counseling_record_save_draft" => "teacher-counseling-mcp-draft",
        "counseling_record_prepare_create" => "teacher-counseling-session",
        "lesson_observations_manage" => "lesson-observations",
        _ => "work-note-page",
    };
    if !read_id(&local_ref, 350)
        || local_ref != format!("{prefix}:{}", record.as_str().ok_or_else(conflict)?)
    {
        return Err(conflict());
    }
    Ok(json!({"status":"saved","requestSha256":saved_sha,
        "resultSha256":crate::sha256_json(&result).map_err(|_| conflict())?,"localRef":local_ref}))
}

#[cfg(test)]
#[path = "classaimate_mcp_receipt_verification_tests.rs"]
mod tests;
