use chrono::{Duration as ChronoDuration, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

use super::{
    normalize, normalize_date_key, normalize_json_text, normalize_student_code,
    normalize_tenant_id, now_ms, random_url_token, sha256_hex, BrowserLinkToken, SqliteStore,
};

const MAX_ROSTER_STUDENTS: usize = 200;
const ROSTER_STALE_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QuickRosterStudent {
    id: String,
    display_name: String,
    class_no: Option<i64>,
    status: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct QuickRosterSnapshot {
    tenant_id: String,
    students: Vec<QuickRosterStudent>,
    roster_sha256: String,
    synced_at_ms: i64,
    stale: bool,
}

pub(crate) fn ensure_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS teacher_roster_snapshots (
          tenant_id TEXT NOT NULL PRIMARY KEY,
          payload_json TEXT NOT NULL,
          roster_sha256 TEXT NOT NULL,
          synced_at_ms INTEGER NOT NULL
        );
        "#,
    )
    .map_err(|error| format!("quick_observation_schema_failed:{error}"))
}

fn normalize_roster_students(input: Option<&Value>) -> Result<Vec<QuickRosterStudent>, String> {
    let rows = input
        .and_then(Value::as_array)
        .ok_or_else(|| "quick_roster_students_required".to_string())?;
    if rows.len() > MAX_ROSTER_STUDENTS {
        return Err("quick_roster_student_limit_exceeded".to_string());
    }
    let mut seen = HashSet::new();
    let mut students = Vec::with_capacity(rows.len());
    for row in rows {
        let id = normalize_student_code(row.get("id"));
        let display_name = normalize_json_text(row.get("displayName"), 120);
        if id.is_empty() || display_name.is_empty() || !seen.insert(id.clone()) {
            return Err("quick_roster_student_invalid".to_string());
        }
        let class_no = row
            .get("classNo")
            .and_then(Value::as_i64)
            .filter(|number| (1..=999).contains(number));
        let raw_status = normalize_json_text(row.get("status"), 24);
        let status = if raw_status == "archived" { "archived" } else { "active" }.to_string();
        students.push(QuickRosterStudent { id, display_name, class_no, status });
    }
    students.sort_by(|left, right| {
        left.class_no
            .unwrap_or(i64::MAX)
            .cmp(&right.class_no.unwrap_or(i64::MAX))
            .then_with(|| left.display_name.cmp(&right.display_name))
    });
    Ok(students)
}

impl SqliteStore {
    pub(crate) fn put_quick_roster_snapshot(&self, body: &Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(body.get("tenantId"));
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let students = normalize_roster_students(body.get("students"))?;
        let payload_json = serde_json::to_string(&students)
            .map_err(|error| format!("quick_roster_serialize_failed:{error}"))?;
        let roster_sha256 = sha256_hex(&payload_json);
        let synced_at_ms = now_ms();
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            r#"
            INSERT INTO teacher_roster_snapshots (tenant_id, payload_json, roster_sha256, synced_at_ms)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(tenant_id) DO UPDATE SET
              payload_json = excluded.payload_json,
              roster_sha256 = excluded.roster_sha256,
              synced_at_ms = excluded.synced_at_ms
            "#,
            params![tenant_id, payload_json, roster_sha256, synced_at_ms],
        )
        .map_err(|error| format!("quick_roster_save_failed:{error}"))?;
        Ok(json!({
            "ok": true,
            "tenantId": tenant_id,
            "studentCount": students.len(),
            "rosterSha256": roster_sha256,
            "syncedAtMs": synced_at_ms
        }))
    }

    fn quick_roster_snapshot(&self, tenant_id: &str) -> Result<Option<QuickRosterSnapshot>, String> {
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let row = conn
            .query_row(
                "SELECT payload_json, roster_sha256, synced_at_ms FROM teacher_roster_snapshots WHERE tenant_id = ?1",
                params![tenant_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)),
            )
            .optional()
            .map_err(|error| format!("quick_roster_read_failed:{error}"))?;
        let Some((payload_json, roster_sha256, synced_at_ms)) = row else { return Ok(None); };
        let students = serde_json::from_str::<Vec<QuickRosterStudent>>(&payload_json)
            .map_err(|error| format!("quick_roster_payload_invalid:{error}"))?;
        Ok(Some(QuickRosterSnapshot {
            tenant_id: tenant_id.to_string(),
            students,
            roster_sha256,
            synced_at_ms,
            stale: now_ms().saturating_sub(synced_at_ms) > ROSTER_STALE_MS,
        }))
    }

    fn recent_quick_observations(&self, tenant_id: &str, limit: i64) -> Result<Vec<Value>, String> {
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT payload_json FROM lesson_observations WHERE tenant_id = ?1 AND json_extract(payload_json, '$.recordOrigin') = 'teacher_quick_observation' ORDER BY updated_at_ms DESC LIMIT ?2",
            )
            .map_err(|error| format!("quick_observation_recent_prepare_failed:{error}"))?;
        let rows = stmt
            .query_map(params![tenant_id, limit.clamp(1, 20)], |row| row.get::<_, String>(0))
            .map_err(|error| format!("quick_observation_recent_query_failed:{error}"))?;
        let mut records = Vec::new();
        for row in rows {
            let payload = row.map_err(|error| format!("quick_observation_recent_row_failed:{error}"))?;
            records.push(
                serde_json::from_str(&payload)
                    .map_err(|error| format!("quick_observation_recent_payload_failed:{error}"))?,
            );
        }
        Ok(records)
    }
}

pub(crate) fn roster_http_get(store: &SqliteStore, tenant_id: &str) -> Result<Value, String> {
    let tenant_id = normalize(tenant_id, 160);
    let snapshot = store.quick_roster_snapshot(&tenant_id)?;
    Ok(json!({ "ok": true, "roster": snapshot }))
}

pub(crate) fn context(store: &SqliteStore, link: Option<BrowserLinkToken>) -> Result<Value, String> {
    let Some(link) = link else {
        return Ok(json!({ "ok": true, "connected": false, "roster": Value::Null, "recent": [] }));
    };
    let roster = store.quick_roster_snapshot(&link.tenant_id)?;
    let recent = store.recent_quick_observations(&link.tenant_id, 4)?;
    Ok(json!({
        "ok": true,
        "connected": true,
        "tenantId": link.tenant_id,
        "tenantName": link.tenant_name,
        "roster": roster,
        "recent": recent
    }))
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .map(|item| normalize_json_text(Some(item), 60))
                .filter(|item| !item.is_empty())
                .take(20)
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn save_batch(store: &SqliteStore, tenant_id: &str, input: Value) -> Result<Value, String> {
    let tenant_id = normalize(tenant_id, 160);
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let snapshot = store
        .quick_roster_snapshot(&tenant_id)?
        .ok_or_else(|| "quick_roster_missing".to_string())?;
    let by_id = snapshot
        .students
        .into_iter()
        .filter(|student| student.status != "archived")
        .map(|student| (student.id.clone(), student))
        .collect::<HashMap<_, _>>();
    let requested = input
        .get("studentIds")
        .and_then(Value::as_array)
        .ok_or_else(|| "quick_observation_students_required".to_string())?;
    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    for value in requested.iter().take(MAX_ROSTER_STUDENTS + 1) {
        let id = normalize_student_code(Some(value));
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        selected.push(by_id.get(&id).cloned().ok_or_else(|| "quick_observation_student_unknown".to_string())?);
    }
    if selected.is_empty() || selected.len() > MAX_ROSTER_STUDENTS {
        return Err("quick_observation_students_required".to_string());
    }

    let context_type = normalize_json_text(input.get("contextType"), 32);
    if !matches!(context_type.as_str(), "lesson" | "recess" | "counseling" | "daily_guidance" | "other") {
        return Err("quick_observation_context_required".to_string());
    }
    let status = normalize_json_text(input.get("status"), 24);
    if !matches!(status.as_str(), "good" | "warning" | "help" | "none") {
        return Err("quick_observation_status_invalid".to_string());
    }
    let note = normalize_json_text(input.get("note"), 1000);
    if note.is_empty() {
        return Err("quick_observation_note_required".to_string());
    }
    let mut date_key = normalize_date_key(input.get("date"));
    if date_key.is_empty() {
        date_key = (Utc::now() + ChronoDuration::hours(9)).format("%Y-%m-%d").to_string();
    }
    let period = if context_type == "lesson" {
        input.get("period").and_then(Value::as_i64).filter(|value| (1..=20).contains(value)).ok_or_else(|| "period_required".to_string())?
    } else {
        0
    };
    let context_label = match context_type.as_str() {
        "lesson" => "수업",
        "recess" => "쉬는 시간",
        "counseling" => "상담",
        "daily_guidance" => "생활지도",
        _ => "기타",
    };
    let subject = if context_type == "lesson" {
        let value = normalize_json_text(input.get("subject"), 120);
        if value.is_empty() { return Err("quick_observation_subject_required".to_string()); }
        value
    } else {
        context_label.to_string()
    };
    let record_domain = match normalize_json_text(input.get("recordDomain"), 24).as_str() {
        "subjects" => "subjects",
        "creative" => "creative",
        "behavior" => "behavior",
        _ if context_type == "lesson" => "subjects",
        _ => "behavior",
    };
    let creative_area = normalize_json_text(input.get("creativeArea"), 120);
    if record_domain == "creative" && creative_area.is_empty() {
        return Err("quick_observation_creative_area_required".to_string());
    }
    let tags = string_list(input.get("tags"));
    let timestamp = now_ms();
    let batch_id = format!("observation-batch-{timestamp}-{}", random_url_token());
    let mut records = Vec::with_capacity(selected.len());
    for student in selected {
        let doc_id = format!("teacher-observation-{date_key}-{}-{}", student.id, random_url_token());
        let mut record = json!({
            "tenantId": tenant_id,
            "docId": doc_id,
            "batchId": batch_id,
            "recordOrigin": "teacher_quick_observation",
            "sourceType": "teacherQuickObservation",
            "recordState": "active",
            "recordDomain": record_domain,
            "creativeArea": creative_area,
            "lessonContext": Value::Null,
            "date": date_key,
            "period": period,
            "studentCode": student.id,
            "studentName": student.display_name,
            "classNo": student.class_no.unwrap_or(0),
            "subject": subject,
            "objective": "교사 빠른 관찰기록",
            "status": status,
            "tags": tags,
            "note": note,
            "createdAtMs": timestamp,
            "updatedAtMs": timestamp
        });
        if context_type != "lesson" {
            let object = record.as_object_mut().expect("quick observation object");
            object.insert("observationKind".to_string(), Value::String("non_lesson".to_string()));
            object.insert("contextType".to_string(), Value::String(context_type.clone()));
            object.insert("contextLabel".to_string(), Value::String(context_label.to_string()));
            object.insert("eventAtMs".to_string(), Value::Number(timestamp.into()));
        }
        records.push(record);
    }

    let saved = store.import_observations(tenant_id.clone(), records.clone())?;
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    for expected in &saved {
        let doc_id = expected.get("docId").and_then(Value::as_str).unwrap_or_default();
        let payload: Option<String> = conn
            .query_row(
                "SELECT payload_json FROM lesson_observations WHERE tenant_id = ?1 AND doc_id = ?2",
                params![tenant_id, doc_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("quick_observation_readback_failed:{error}"))?;
        let actual: Value = payload
            .and_then(|payload| serde_json::from_str(&payload).ok())
            .ok_or_else(|| "quick_observation_readback_failed".to_string())?;
        if actual != *expected {
            return Err("quick_observation_readback_failed".to_string());
        }
    }
    Ok(json!({ "ok": true, "savedCount": saved.len(), "records": saved }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs};

    #[test]
    fn roster_snapshot_and_quick_batch_stay_tenant_scoped() {
        let dir = env::temp_dir().join(format!("classaimate-quick-observation-{}", random_url_token()));
        let store = SqliteStore::open(dir.join("store.sqlite")).expect("open store");
        store.put_quick_roster_snapshot(&json!({
            "tenantId": "tenant-a",
            "students": [{ "id": "s01", "displayName": "김도윤", "classNo": 1, "status": "active" }]
        })).expect("save roster");
        let result = save_batch(&store, "tenant-a", json!({
            "studentIds": ["S01"], "contextType": "recess", "status": "none", "note": "친구의 이야기를 차분히 들음"
        })).expect("save observation");
        assert_eq!(result.get("savedCount").and_then(Value::as_u64), Some(1));
        assert_eq!(store.quick_roster_snapshot("tenant-b").expect("read other" ).is_none(), true);
        fs::remove_dir_all(dir).expect("remove fixture");
    }
}
