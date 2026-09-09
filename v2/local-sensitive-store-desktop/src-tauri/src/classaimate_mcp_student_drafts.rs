use super::{draft_text, latest_draft, TransactionStore};
use crate::SqliteStore;
use rusqlite::params;
use serde_json::{json, Value};
use std::collections::HashSet;

const INVALID: &str = "INVALID_LOCAL_READ_REQUEST";
fn date(value: &Value) -> bool {
    value.as_str().filter(|text| text.len() == 10)
        .and_then(|text| chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").ok())
        .is_some()
}
fn alias(value: &str) -> bool {
    value.strip_prefix("학생-").is_some_and(|suffix| (2..=3).contains(&suffix.len())
        && suffix.bytes().all(|byte| byte.is_ascii_digit()))
}
fn in_scope(set: &Value, scope: &Value) -> bool {
    // For semester reads the server supplies the full canonical term, independently of evidence dates.
    if scope.get("semester").is_none() { return true; }
    if !date(&set["fromDate"]) || !date(&set["toDate"]) { return false; }
    let year = set.get("schoolYear").or_else(|| set.get("academicYear")).or_else(|| set.pointer("/scope/schoolYear"));
    let term = set.get("semester").or_else(|| set.pointer("/scope/semester"));
    !year.is_some_and(|year| year != &scope["schoolYear"])
        && !term.is_some_and(|term| term != &scope["semester"])
        && set["fromDate"].as_str() <= set["toDate"].as_str()
        && set["fromDate"].as_str() >= scope["fromDate"].as_str()
        && set["toDate"].as_str() <= scope["toDate"].as_str()
}

// Only the server's sealed bundle mapping reaches this operation. Return aliases,
// never student IDs or names; the internal baseline is stripped by the public tool.
pub(crate) fn read_current(store: &SqliteStore, tenant: &str, input: &Value) -> Result<Value, String> {
    let object = input.as_object().ok_or(INVALID)?;
    let scope = input["scope"].as_object().ok_or(INVALID)?;
    let students = input["students"].as_array().ok_or(INVALID)?;
    if object.len() != 2 || !object.keys().all(|key| ["scope", "students"].contains(&key.as_str()))
        || scope.keys().any(|key| !["recordType", "subject", "creativeArea", "fromDate", "toDate", "schoolYear", "semester"].contains(&key.as_str()))
        || !["subjects", "creative", "behavior"].contains(&input["scope"]["recordType"].as_str().unwrap_or(""))
        || !date(&input["scope"]["fromDate"]) || !date(&input["scope"]["toDate"])
        || input["scope"]["fromDate"].as_str() > input["scope"]["toDate"].as_str()
        || scope.contains_key("schoolYear") != scope.contains_key("semester")
        || (scope.contains_key("semester") && (!matches!(input["scope"]["semester"].as_u64(), Some(1 | 2))
            || !input["scope"]["schoolYear"].as_u64().is_some_and(|year| (2000..=2200).contains(&year))))
        || students.is_empty() || students.len() > 30 {
        return Err(INVALID.into());
    }
    for (key, max) in [("subject", 160), ("creativeArea", 40)] {
        if let Some(value) = scope.get(key) {
            if !value.as_str().is_some_and(|text| text.encode_utf16().count() <= max) { return Err(INVALID.into()); }
        }
    }
    if (input["scope"]["recordType"] == "subjects" && input["scope"]["subject"].as_str().unwrap_or("").trim().is_empty())
        || (input["scope"]["recordType"] == "creative" && input["scope"]["creativeArea"].as_str().unwrap_or("").trim().is_empty()) {
        return Err(INVALID.into());
    }
    let mut codes = HashSet::new(); let mut aliases = HashSet::new();
    for student in students {
        let object = student.as_object().ok_or(INVALID)?;
        let code = student["studentCode"].as_str().ok_or(INVALID)?;
        let name = student["studentAlias"].as_str().ok_or(INVALID)?;
        if object.len() != 2 || !object.keys().all(|key| ["studentCode", "studentAlias"].contains(&key.as_str()))
            || code.len() > 160 || !super::valid_id(code) || !alias(name)
            || !codes.insert(code) || !aliases.insert(name) { return Err(INVALID.into()); }
    }
    let conn = store.conn.lock().map_err(|_| "MCP_STUDENT_DRAFT_READ_FAILED")?;
    let transaction = conn.unchecked_transaction().map_err(|_| "MCP_STUDENT_DRAFT_READ_FAILED")?;
    let scoped = TransactionStore { conn: &transaction };
    let mut statement = transaction.prepare("SELECT d.payload_json,d.updated_at_ms,s.payload_json FROM student_record_drafts d
        LEFT JOIN student_record_draft_sets s ON s.tenant_id=d.tenant_id AND s.draft_set_id=d.draft_set_id
        WHERE d.tenant_id=?1 AND d.student_code=?2 ORDER BY d.updated_at_ms DESC,d.draft_id DESC")
        .map_err(|_| "MCP_STUDENT_DRAFT_READ_FAILED")?;
    let mut drafts = Vec::new();
    for student in students {
        let code = student["studentCode"].as_str().ok_or(INVALID)?;
        let baseline = latest_draft(&scoped, tenant, code, &input["scope"])
            .map_err(|_| "MCP_STUDENT_DRAFT_READ_FAILED")?;
        let rows = statement.query_map(params![tenant, code], |row| Ok((row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?, row.get::<_, Option<String>>(2)?)))
            .map_err(|_| "MCP_STUDENT_DRAFT_READ_FAILED")?;
        let mut draft = json!({"studentAlias":student["studentAlias"],"text":"","status":"none","updatedAt":null,"baselineDigest":baseline});
        for row in rows {
            let (raw, updated_at, set) = row.map_err(|_| "MCP_STUDENT_DRAFT_READ_FAILED")?;
            let payload: Value = serde_json::from_str(&raw).map_err(|_| "MCP_STUDENT_DRAFT_READ_FAILED")?;
            let set: Value = set.map(|raw| serde_json::from_str(&raw)).transpose()
                .map_err(|_| "MCP_STUDENT_DRAFT_READ_FAILED")?.unwrap_or(Value::Null);
            let text = draft_text(&payload, &input["scope"]);
            if text.is_empty() || !in_scope(&set, &input["scope"]) { continue; }
            let status = payload["status"].as_str().filter(|text| !text.is_empty()).unwrap_or("none");
            if text.encode_utf16().count() > 2400 || status.encode_utf16().count() > 32 || updated_at < 0 {
                return Err("MCP_STUDENT_DRAFT_READ_FAILED".into());
            }
            draft["text"] = json!(text); draft["status"] = json!(status); draft["updatedAt"] = json!(updated_at);
            break;
        }
        drafts.push(draft);
    }
    Ok(json!({"drafts":drafts}))
}

#[cfg(test)]
#[path = "classaimate_mcp_student_drafts_tests.rs"]
mod tests;
