use crate::SqliteStore;
use chrono::{FixedOffset, NaiveDate, TimeZone};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const INVALID: &str = "MCP_STUDENT_SELECTION_INVALID";
const READ_FAILED: &str = "MCP_STUDENT_SELECTION_READ_FAILED";
const TOO_LARGE: &str = "MCP_STUDENT_SELECTION_TOO_LARGE";
const MAX_RECORDS: usize = 10_000;
const MAX_PROJECTION_BYTES: usize = 16 * 1024 * 1024;

struct Scope {
    school_year: i64,
    semester: i64,
    from: String,
    to: String,
    students: BTreeSet<String>,
    sources: BTreeSet<String>,
    workspace: Option<String>,
    offset: usize,
    limit: usize,
}

fn identifier(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && value.trim() == value
        && value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

fn date(value: Option<&Value>) -> Result<String, String> {
    let raw = value.and_then(Value::as_str).ok_or(INVALID)?;
    let parsed = NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|_| INVALID)?;
    if raw.len() != 10 || parsed.format("%Y-%m-%d").to_string() != raw { return Err(INVALID.into()); }
    Ok(raw.into())
}

fn strings(value: Option<&Value>, max: usize) -> Result<BTreeSet<String>, String> {
    let rows = value.and_then(Value::as_array).ok_or(INVALID)?;
    if rows.is_empty() || rows.len() > max { return Err(INVALID.into()); }
    let mut result = BTreeSet::new();
    for row in rows {
        let text = row.as_str().filter(|s| identifier(s, 160)).ok_or(INVALID)?;
        if !result.insert(text.to_string()) { return Err(INVALID.into()); }
    }
    Ok(result)
}

impl Scope {
    fn parse(tenant: &str, input: &Value) -> Result<Self, String> {
        let object = input.as_object().ok_or(INVALID)?;
        let allowed = ["schoolYear", "semester", "fromDate", "toDate", "studentCodes", "sourceTypes", "workspaceId", "cursor", "limit"];
        if !identifier(tenant, 128) || object.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(INVALID.into());
        }
        let school_year = input["schoolYear"].as_i64().filter(|n| (2000..=2200).contains(n)).ok_or(INVALID)?;
        let semester = input["semester"].as_i64().filter(|n| [1, 2].contains(n)).ok_or(INVALID)?;
        let from = date(input.get("fromDate"))?;
        let to = date(input.get("toDate"))?;
        if from > to { return Err(INVALID.into()); }
        let students = strings(input.get("studentCodes"), 30)?;
        let sources = strings(input.get("sourceTypes"), 4)?;
        if sources.iter().any(|s| !["observation", "evaluation", "manual_supplement", "counseling_summary"].contains(&s.as_str())) {
            return Err(INVALID.into());
        }
        let workspace = match input.get("workspaceId") {
            None => None,
            Some(value) => Some(value.as_str().filter(|s| identifier(s, 260)).ok_or(INVALID)?.to_string()),
        };
        let offset = match input.get("cursor") {
            None => 0,
            Some(value) => {
                let raw = value.as_str().filter(|s| !s.is_empty() && s.len() <= 5 && s.bytes().all(|c| c.is_ascii_digit())).ok_or(INVALID)?;
                let parsed = raw.parse::<usize>().map_err(|_| INVALID)?;
                if parsed > MAX_RECORDS || parsed.to_string() != raw { return Err(INVALID.into()); }
                parsed
            }
        };
        let limit = input["limit"].as_u64().filter(|n| (1..=200).contains(n)).ok_or(INVALID)? as usize;
        Ok(Self { school_year, semester, from, to, students, sources, workspace, offset, limit })
    }

    fn bindings(&self, tenant: &str) -> Vec<String> {
        let mut result = vec![tenant.to_string(), self.from.clone(), self.to.clone()];
        result.extend(self.students.iter().cloned());
        result
    }

    fn placeholders(&self) -> String { vec!["?"; self.students.len()].join(",") }
}

fn payload(raw: &str) -> Result<Value, String> {
    if raw.len() > 1024 * 1024 { return Err(TOO_LARGE.into()); }
    let value: Value = serde_json::from_str(raw).map_err(|_| READ_FAILED)?;
    if !value.is_object() { return Err(READ_FAILED.into()); }
    Ok(value)
}

fn copy_scalars(target: &mut Map<String, Value>, source: &Value, fields: &[&str]) {
    for field in fields {
        if let Some(value) = source.get(*field).filter(|v| !v.is_object() && !v.is_array()) {
            target.insert((*field).into(), value.clone());
        }
    }
}

fn copy_strings(target: &mut Map<String, Value>, source: &Value, fields: &[&str]) {
    for field in fields {
        if let Some(rows) = source.get(*field).and_then(Value::as_array) {
            if rows.iter().all(Value::is_string) { target.insert((*field).into(), Value::Array(rows.clone())); }
        }
    }
}

const STATE_FIELDS: &[&str] = &["status", "recordState", "archivedAtMs", "deletedAtMs", "isActive"];
const OBSERVATION_FIELDS: &[&str] = &["note", "content", "observationText", "memo", "comment", "subject", "recordDomain", "creativeArea", "contextType", "sourceType", "revisionId"];
const EVALUATION_FIELDS: &[&str] = &["subject", "title", "evaluationTitle", "planTitle", "coreStandard", "achievementStandard", "standard", "coreAchievementStandard", "resultMode", "mode", "levelIndex", "levelLabel", "customResultText", "resultText", "score", "isRecorded", "isExcluded", "excluded", "note", "memo", "comment"];

fn base(kind: &str, id: String, student: String, day: String, updated: i64) -> Map<String, Value> {
    json!({"kind":kind,"sourceId":id,"studentCode":student,"date":day,"updatedAtMs":updated}).as_object().unwrap().clone()
}

#[derive(Default)]
struct Projection { records: Vec<Value>, bytes: usize }

fn push(projection: &mut Projection, record: Map<String, Value>) -> Result<(), String> {
    let bytes = serde_json::to_vec(&record).map_err(|_| READ_FAILED)?.len();
    if projection.records.len() >= MAX_RECORDS || projection.bytes.saturating_add(bytes) > MAX_PROJECTION_BYTES {
        return Err(TOO_LARGE.into());
    }
    projection.bytes += bytes;
    projection.records.push(Value::Object(record));
    Ok(())
}

fn observations(conn: &Connection, tenant: &str, scope: &Scope, records: &mut Projection) -> Result<(), String> {
    let ordinary = scope.sources.contains("observation");
    let manual = scope.sources.contains("manual_supplement");
    if !ordinary && !manual { return Ok(()); }
    let source_clause = match (ordinary, manual) {
        (true, true) => "",
        (true, false) => " AND COALESCE(json_extract(payload_json,'$.sourceType'),'') NOT IN ('teacherManualSupplement','manual_supplement')",
        _ => " AND json_extract(payload_json,'$.sourceType') IN ('teacherManualSupplement','manual_supplement')",
    };
    let sql = format!("SELECT doc_id,student_code,date_key,period,updated_at_ms,payload_json FROM lesson_observations WHERE tenant_id=? AND date_key>=? AND date_key<=? AND student_code IN ({}){} ORDER BY date_key,doc_id LIMIT {}", scope.placeholders(), source_clause, MAX_RECORDS + 1);
    let mut statement = conn.prepare(&sql).map_err(|_| READ_FAILED)?;
    let mut rows = statement.query(params_from_iter(scope.bindings(tenant))).map_err(|_| READ_FAILED)?;
    while let Some(row) = rows.next().map_err(|_| READ_FAILED)? {
        let parsed = payload(&row.get::<_, String>(5).map_err(|_| READ_FAILED)?)?;
        let mut record = Map::new();
        copy_scalars(&mut record, &parsed, OBSERVATION_FIELDS);
        copy_scalars(&mut record, &parsed, STATE_FIELDS);
        record.extend(base("observation", row.get(0).map_err(|_| READ_FAILED)?, row.get(1).map_err(|_| READ_FAILED)?, row.get(2).map_err(|_| READ_FAILED)?, row.get(4).map_err(|_| READ_FAILED)?));
        record.insert("period".into(), json!(row.get::<_, i64>(3).map_err(|_| READ_FAILED)?));
        push(records, record)?;
    }
    Ok(())
}

fn evaluations(conn: &Connection, tenant: &str, scope: &Scope, records: &mut Projection) -> Result<(), String> {
    if !scope.sources.contains("evaluation") { return Ok(()); }
    let day = "COALESCE(NULLIF(r.date_key,''),a.scheduled_date)";
    let sql = format!("SELECT r.result_id,r.student_id,{day},MAX(r.updated_at_ms,a.updated_at_ms),r.payload_json,a.payload_json FROM eval_results r JOIN eval_assignments a ON a.tenant_id=r.tenant_id AND a.assignment_id=r.assignment_id WHERE r.tenant_id=? AND {day}>=? AND {day}<=? AND r.student_id IN ({}) ORDER BY {day},r.result_id LIMIT {}", scope.placeholders(), MAX_RECORDS + 1);
    let mut statement = conn.prepare(&sql).map_err(|_| READ_FAILED)?;
    let mut rows = statement.query(params_from_iter(scope.bindings(tenant))).map_err(|_| READ_FAILED)?;
    while let Some(row) = rows.next().map_err(|_| READ_FAILED)? {
        let result = payload(&row.get::<_, String>(4).map_err(|_| READ_FAILED)?)?;
        let assignment = payload(&row.get::<_, String>(5).map_err(|_| READ_FAILED)?)?;
        let student: String = row.get(1).map_err(|_| READ_FAILED)?;
        let mut record = Map::new();
        copy_scalars(&mut record, &assignment, EVALUATION_FIELDS);
        copy_strings(&mut record, &assignment, &["levelTexts", "levelDescriptions"]);
        copy_scalars(&mut record, &result, EVALUATION_FIELDS);
        copy_strings(&mut record, &result, &["levelTexts", "levelDescriptions"]);
        copy_scalars(&mut record, &assignment, STATE_FIELDS);
        copy_scalars(&mut record, &result, STATE_FIELDS);
        // Either source can exclude a result; a result's active flags cannot revive an archived assignment.
        for field in ["status", "recordState"] {
            if [&assignment, &result].iter().any(|source| source[field].as_str().map(|v| v.trim().eq_ignore_ascii_case("archived")).unwrap_or(false)) {
                record.insert(field.into(), json!("archived"));
            }
        }
        for field in ["archivedAtMs", "deletedAtMs"] {
            let latest = [&assignment, &result].iter().filter_map(|source| source[field].as_i64()).max();
            if let Some(value) = latest { record.insert(field.into(), json!(value)); }
        }
        if [&assignment, &result].iter().any(|source| source["isActive"] == false) { record.insert("isActive".into(), json!(false)); }
        for field in ["isExcluded", "excluded"] {
            if [&assignment, &result].iter().any(|source| source[field] == true) { record.insert(field.into(), json!(true)); }
        }
        // Preserve exclusion semantics without disclosing other students' identifiers.
        let excluded = [&assignment, &result].iter().any(|source| source.get("excludedStudentIds").and_then(Value::as_array)
            .map(|ids| ids.iter().any(|id| id.as_str().map(|s| s.trim().eq_ignore_ascii_case(&student)).unwrap_or(false))).unwrap_or(false));
        record.insert("excludedStudentIds".into(), if excluded { json!([student]) } else { json!([]) });
        record.extend(base("evaluation", row.get(0).map_err(|_| READ_FAILED)?, student, row.get(2).map_err(|_| READ_FAILED)?, row.get(3).map_err(|_| READ_FAILED)?));
        push(records, record)?;
    }
    Ok(())
}

fn counseling(conn: &Connection, tenant: &str, scope: &Scope, records: &mut Projection) -> Result<(), String> {
    if !scope.sources.contains("counseling_summary") { return Ok(()); }
    let zone = FixedOffset::east_opt(9 * 3600).ok_or(INVALID)?;
    let start = NaiveDate::parse_from_str(&scope.from, "%Y-%m-%d").map_err(|_| INVALID)?.and_hms_opt(0, 0, 0).ok_or(INVALID)?;
    let end = NaiveDate::parse_from_str(&scope.to, "%Y-%m-%d").map_err(|_| INVALID)?.succ_opt().ok_or(INVALID)?.and_hms_opt(0, 0, 0).ok_or(INVALID)?;
    let mut bindings = vec![tenant.to_string(), zone.from_local_datetime(&start).single().ok_or(INVALID)?.timestamp_millis().to_string(), zone.from_local_datetime(&end).single().ok_or(INVALID)?.timestamp_millis().to_string()];
    bindings.extend(scope.students.iter().cloned());
    let sql = format!("SELECT session_id,student_code,counseling_at_ms,updated_at_ms,status,archived_at_ms,payload_json FROM teacher_counseling_sessions WHERE tenant_id=? AND counseling_at_ms>=CAST(? AS INTEGER) AND counseling_at_ms<CAST(? AS INTEGER) AND student_code IN ({}) ORDER BY counseling_at_ms,session_id LIMIT {}", scope.placeholders(), MAX_RECORDS + 1);
    let mut statement = conn.prepare(&sql).map_err(|_| READ_FAILED)?;
    let mut rows = statement.query(params_from_iter(bindings)).map_err(|_| READ_FAILED)?;
    while let Some(row) = rows.next().map_err(|_| READ_FAILED)? {
        let parsed = payload(&row.get::<_, String>(6).map_err(|_| READ_FAILED)?)?;
        let instant = chrono::DateTime::from_timestamp_millis(row.get(2).map_err(|_| READ_FAILED)?).ok_or(READ_FAILED)?;
        let mut record = base("counseling", row.get(0).map_err(|_| READ_FAILED)?, row.get(1).map_err(|_| READ_FAILED)?, instant.with_timezone(&zone).format("%Y-%m-%d").to_string(), row.get(3).map_err(|_| READ_FAILED)?);
        copy_scalars(&mut record, &parsed, &["summary"]);
        record.insert("status".into(), json!(row.get::<_, String>(4).map_err(|_| READ_FAILED)?));
        record.insert("archivedAtMs".into(), json!(row.get::<_, Option<i64>>(5).map_err(|_| READ_FAILED)?));
        record.insert("sourceType".into(), json!("counseling_summary"));
        push(records, record)?;
    }
    Ok(())
}

fn workspace(conn: &Connection, tenant: &str, scope: &Scope) -> Result<Value, String> {
    let Some(id) = &scope.workspace else { return Ok(Value::Null); };
    let found: Option<(String, String, String, i64)> = conn.query_row("SELECT payload_json,from_date,to_date,updated_at_ms FROM student_record_draft_sets WHERE tenant_id=?1 AND draft_set_id=?2 AND status='workspace'", params![tenant, id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).optional().map_err(|_| READ_FAILED)?;
    let Some((raw, from, to, revision)) = found else { return Err("MCP_STUDENT_WORKSPACE_NOT_FOUND".into()); };
    let parsed = payload(&raw)?;
    let body = &parsed["workspace"];
    if parsed["kind"] != "student_record_workspace_v1" || parsed["status"] != "workspace"
        || body["workspaceId"].as_str() != Some(id) || body["academicYear"] != scope.school_year || body["semester"] != scope.semester
        || from != scope.from || to != scope.to || body["fromDate"] != from || body["toDate"] != to {
        return Err("MCP_STUDENT_WORKSPACE_SCOPE_MISMATCH".into());
    }
    let mut selected = Map::new();
    let mut checks = Map::new();
    let mut evidence_ids = BTreeSet::new();
    for student in &scope.students {
        let mut areas = Map::new();
        if let Some(values) = body["selectedEvidence"][student].as_object() {
            for (area, value) in values {
                let Some(ids) = value.as_array() else { return Err(READ_FAILED.into()); };
                if !ids.iter().all(Value::is_string) { return Err(READ_FAILED.into()); }
                evidence_ids.extend(ids.iter().filter_map(Value::as_str).map(str::to_string));
                areas.insert(area.clone(), value.clone());
                let key = format!("{student}:{area}");
                if let Some(check) = body["checks"].get(&key).filter(|v| v.is_string()) { checks.insert(key, check.clone()); }
            }
        }
        selected.insert(student.clone(), Value::Object(areas));
    }
    let mut links = Map::new();
    let mut followups = Map::new();
    for id in evidence_ids {
        if let Some(value) = body["evidenceLinks"].get(&id).and_then(Value::as_array) {
            if value.iter().all(Value::is_string) { links.insert(id.clone(), json!(value)); }
        }
        if let Some(value) = body["counselingFollowUps"].get(&id).and_then(Value::as_bool) { followups.insert(id, json!(value)); }
    }
    // Checks contain teacher-reviewed fingerprints; this projection is server-internal, never a public MCP DTO.
    Ok(json!({"workspaceId":id,"revision":revision,"schoolYear":scope.school_year,"academicYear":scope.school_year,"semester":scope.semester,"fromDate":from,"toDate":to,
        "studentCodes":scope.students,"selectedEvidence":selected,"checks":checks,"evidenceLinks":links,"counselingFollowUps":followups}))
}

fn read_connection(conn: &Connection, tenant: &str, scope: &Scope) -> Result<Value, String> {
    let mut projection = Projection::default();
    observations(conn, tenant, scope, &mut projection)?;
    evaluations(conn, tenant, scope, &mut projection)?;
    counseling(conn, tenant, scope, &mut projection)?;
    let mut records = projection.records;
    records.sort_by(|left, right| ["date", "kind", "sourceId", "studentCode"].iter()
        .map(|field| left[*field].as_str().unwrap_or("").cmp(right[*field].as_str().unwrap_or("")))
        .find(|order| !order.is_eq()).unwrap_or(std::cmp::Ordering::Equal));
    let workspace = workspace(conn, tenant, scope)?;
    let all = json!({"records":records,"workspace":workspace});
    let bytes = serde_json::to_vec(&all).map_err(|_| READ_FAILED)?;
    if bytes.len() > MAX_PROJECTION_BYTES { return Err(TOO_LARGE.into()); }
    let revision = format!("{:x}", Sha256::digest(bytes));
    let all_records = all["records"].as_array().ok_or(READ_FAILED)?;
    if scope.offset > all_records.len() { return Err(INVALID.into()); }
    let end = (scope.offset + scope.limit).min(all_records.len());
    Ok(json!({"records":&all_records[scope.offset..end],"workspace":all["workspace"],"sourceRevision":revision,
        "complete":end == all_records.len(),"nextCursor":if end < all_records.len() { json!(end.to_string()) } else { Value::Null }}))
}

/// Only the authenticated native relay supplies tenant; input cannot override it.
pub(crate) fn read_only(store: &SqliteStore, tenant: &str, input: &Value) -> Result<Value, String> {
    let scope = Scope::parse(tenant, input)?;
    let mut conn = store.conn.lock().map_err(|_| READ_FAILED)?;
    let transaction = conn.transaction().map_err(|_| READ_FAILED)?;
    let result = read_connection(&transaction, tenant, &scope)?;
    transaction.commit().map_err(|_| READ_FAILED)?;
    Ok(result)
}

#[cfg(test)]
#[path = "classaimate_mcp_student_selection_tests.rs"]
mod tests;
