use rusqlite::{params, types::ValueRef, Connection};
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
        if values.is_empty() {
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
