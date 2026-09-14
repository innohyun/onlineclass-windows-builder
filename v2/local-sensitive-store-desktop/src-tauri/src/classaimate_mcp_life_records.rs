use crate::SqliteStore;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::BTreeSet;

fn invalid() -> String {
    "classaimate_mcp_write_job_invalid".into()
}
fn db(e: rusqlite::Error) -> String {
    format!("db_mcp_observation_failed:{e}")
}
fn id(v: &Value) -> bool {
    v.as_str().is_some_and(|s| {
        s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
            && s.len() <= 240
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
    })
}
fn scope(input: &Value) -> Result<Value, String> {
    let from = input["fromDate"].as_str().ok_or_else(invalid)?;
    let to = input["toDate"].as_str().ok_or_else(invalid)?;
    let from_date = chrono::NaiveDate::parse_from_str(from, "%Y-%m-%d").map_err(|_| invalid())?;
    let to_date = chrono::NaiveDate::parse_from_str(to, "%Y-%m-%d").map_err(|_| invalid())?;
    if from_date > to_date || (to_date - from_date).num_days() > 366 { return Err(invalid()); }
    Ok(json!({"fromDate":from,"toDate":to}))
}
fn in_scope(record: &Value, scope: &Value) -> bool {
    record["date"].as_str().is_some_and(|date| {
        date >= scope["fromDate"].as_str().unwrap_or("") && date <= scope["toDate"].as_str().unwrap_or("")
    }) && record["period"] == 0 && record["observationKind"] == "non_lesson"
}
fn read(conn: &Connection, tenant: &str, doc: &str) -> Result<Option<Value>, String> {
    conn.query_row(
        "SELECT payload_json FROM lesson_observations WHERE tenant_id=?1 AND doc_id=?2",
        params![tenant, doc],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(db)?
    .map(|raw| serde_json::from_str(&raw).map_err(|_| invalid()))
    .transpose()
}
fn records(conn: &Connection, tenant: &str, scope: &Value) -> Result<Vec<Value>, String> {
    let mut stmt = conn.prepare("SELECT payload_json FROM lesson_observations WHERE tenant_id=?1 AND date_key BETWEEN ?2 AND ?3 ORDER BY date_key,doc_id").map_err(db)?;
    let rows = stmt
        .query_map(
            params![tenant, scope["fromDate"].as_str(), scope["toDate"].as_str()],
            |r| r.get::<_, String>(0),
        )
        .map_err(db)?;
    let mut output = Vec::new();
    for row in rows {
        let record: Value = serde_json::from_str(&row.map_err(db)?).map_err(|_| invalid())?;
        if in_scope(&record, scope) {
            output.push(record);
        }
    }
    Ok(output)
}
fn baseline(records: &[Value]) -> Value {
    let mut rows: Vec<Value> = records
        .iter()
        .map(|r| json!({"docId":r["docId"],"revisionId":r["revisionId"]}))
        .collect();
    rows.sort_by(|a, b| a["docId"].as_str().cmp(&b["docId"].as_str()));
    json!(rows)
}
pub(crate) fn list(store: &SqliteStore, input: &Value) -> Result<Value, String> {
    let scope = scope(input)?;
    if !id(&input["tenantId"]) {
        return Err(invalid());
    }
    let limit = input
        .get("limit")
        .map(|v| v.as_u64().unwrap_or(0))
        .unwrap_or(200);
    let cursor = match input.get("cursor") {
        None | Some(Value::Null) => 0,
        Some(v) => v
            .as_str()
            .ok_or_else(invalid)?
            .parse::<usize>()
            .map_err(|_| invalid())?,
    };
    if !(1..=200).contains(&limit) {
        return Err(invalid());
    }
    for key in ["studentCodes", "docIds"] {
        if let Some(value) = input.get(key).filter(|v| !v.is_null()) {
            if value
                .as_array()
                .is_none_or(|a| a.len() > 200 || !a.iter().all(id))
            {
                return Err(invalid());
            }
        }
    }
    let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    let all = records(&conn, input["tenantId"].as_str().unwrap(), &scope)?;
    let filtered: Vec<Value> = all
        .into_iter()
        .filter(|r| {
            [("studentCodes", "studentCode"), ("docIds", "docId")]
                .iter()
                .all(|(filter, key)| input[filter].as_array().is_none_or(|a| a.contains(&r[key])))
        })
        .collect();
    let page: Vec<Value> = filtered
        .iter()
        .skip(cursor)
        .take(limit as usize)
        .cloned()
        .collect();
    let complete = cursor.saturating_add(page.len()) >= filtered.len();
    Ok(
        json!({"ok":true,"records":page,"complete":complete,"nextCursor":if complete { Value::Null } else {json!((cursor+page.len()).to_string())}}),
    )
}
pub(crate) fn apply(store: &SqliteStore, input: &Value) -> Result<Value, String> {
    let tenant = input["tenantId"].as_str().ok_or_else(invalid)?;
    let data = &input["data"];
    let scope = scope(&data["scope"])?;
    let items = data["items"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 200)
        .ok_or_else(invalid)?;
    if !id(&data["mutationId"]) {
        return Err(invalid());
    }
    if data["action"] == "delete" {
        return apply_delete(store, input, tenant, data, &scope, items);
    }
    if data.get("action").is_some_and(|action| action != "save") {
        return Err(invalid());
    }
    let mutation = data["mutationId"].as_str().unwrap();
    let mut conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    let tx = conn.transaction().map_err(db)?;
    let prior_mutation: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM observation_evidence_mutations WHERE tenant_id=?1 AND mutation_id=?2)",params![tenant,mutation],|r|r.get(0)).map_err(db)?;
    let mut prepared = Vec::new();
    if !prior_mutation {
        let existing = records(&tx, tenant, &scope)?;
        let mut docs = BTreeSet::new();
        for item in items {
            if !id(&item["studentCode"])
                || item["studentCode"]
                    .as_str()
                    .is_some_and(|s| s.len() > 80 || s != s.to_uppercase())
                || !id(&item["docId"])
                || !docs.insert(item["docId"].as_str().unwrap())
            {
                return Err(invalid());
            }
            let action = item["action"].as_str().ok_or_else(invalid)?;
            if !matches!(action, "create" | "update") {
                return Err(invalid());
            }
            let patch = item["record"].as_object().ok_or_else(invalid)?;
            let note = item["record"]["note"].as_str().ok_or_else(invalid)?;
            if note.trim().is_empty() || note.encode_utf16().count() > 1000 {
                return Err(invalid());
            }
            for (key, expected) in [
                ("studentCode", &item["studentCode"]),
                ("docId", &item["docId"]),
                ("id", &item["docId"]),
                ("tenantId", &input["tenantId"]),
            ] {
                if patch
                    .get(key)
                    .is_some_and(|v| !v.is_null() && v != expected)
                {
                    return Err(invalid());
                }
            }
            let record_date = item["record"]["date"].as_str().ok_or_else(invalid)?;
            chrono::NaiveDate::parse_from_str(record_date, "%Y-%m-%d").map_err(|_| invalid())?;
            let valid_context = matches!(item["record"]["contextType"].as_str(), Some("recess" | "counseling" | "daily_guidance" | "other"));
            if !in_scope(&item["record"], &scope)
                || !valid_context
                || item["record"]["eventTimePrecision"] != "exact"
                || item["record"]["eventTimeZone"] != "Asia/Seoul"
                || item["record"]["eventAtMs"].as_i64().is_none()
            {
                return Err(invalid());
            }
            let previous = read(&tx, tenant, item["docId"].as_str().unwrap())?;
            if action == "create" {
                let expected = item["baselineRecords"].as_array().ok_or_else(invalid)?;
                if !expected
                    .iter()
                    .all(|r| id(&r["docId"]) && id(&r["revisionId"]))
                {
                    return Err(invalid());
                }
                if previous.is_some()
                    || baseline(&existing) != baseline(expected)
                {
                    return Err("observation_revision_conflict".into());
                }
            } else {
                let old = previous.as_ref().ok_or("observation_revision_conflict")?;
                if !in_scope(old, &scope)
                    || old["studentCode"] != item["studentCode"]
                    || item["expectedRevisionId"]
                        .as_str()
                        .is_none_or(|s| s.is_empty())
                    || old["revisionId"] != item["expectedRevisionId"]
                {
                    return Err("observation_revision_conflict".into());
                }
                if item["correctionReason"]
                    .as_str()
                    .is_none_or(|s| s.trim().is_empty() || s.encode_utf16().count() > 1000)
                {
                    return Err(invalid());
                }
            }
            let mut record = previous.unwrap_or(json!({}));
            for (key, value) in patch {
                record[key] = value.clone();
            }
            for key in ["studentCode", "docId"] {
                record[key] = item[key].clone();
            }
            record["id"] = item["docId"].clone();
            record["tenantId"] = json!(tenant);
            if let Some(name) = item.get("studentName") {
                record["studentName"] = name.clone();
            }
            record["expectedRevisionId"] = if action == "update" {
                item["expectedRevisionId"].clone()
            } else {
                Value::Null
            };
            record["correctionReason"] = item.get("correctionReason").cloned().unwrap_or(json!(""));
            prepared.push(record);
        }
    }
    let saved = store.evidence_save_in_transaction(&tx, tenant, prepared, mutation, Some(data))?;
    for record in saved {
        if read(&tx, tenant, record["docId"].as_str().unwrap())? != Some(record) {
            return Err("LOCAL_STORE_WRITE_FAILED".into());
        }
    }
    let local_ref = format!("life-records:{mutation}");
    tx.execute(
        "INSERT INTO classaimate_mcp_local_write_receipts VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            tenant,
            input["receiptId"].as_str(),
            "life_records_manage",
            input["requestSha256"].as_str(),
            data.to_string(),
            local_ref,
            chrono::Utc::now().timestamp_millis()
        ],
    )
    .map_err(db)?;
    tx.commit().map_err(db)?;
    Ok(json!({"replayed":false,"result":data,"localRef":local_ref}))
}

fn apply_delete(
    store: &SqliteStore,
    input: &Value,
    tenant: &str,
    data: &Value,
    scope: &Value,
    items: &[Value],
) -> Result<Value, String> {
    if data.as_object().is_none_or(|object| {
        object.len() != 4
            || object
                .keys()
                .any(|key| !matches!(key.as_str(), "action" | "scope" | "items" | "mutationId"))
    }) {
        return Err(invalid());
    }
    let mutation = data["mutationId"].as_str().ok_or_else(invalid)?;
    let mut conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    let tx = conn.transaction().map_err(db)?;
    let replay: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM observation_evidence_mutations WHERE tenant_id=?1 AND mutation_id=?2)",
            params![tenant, mutation],
            |row| row.get(0),
        )
        .map_err(db)?;
    let mut docs = BTreeSet::new();
    for item in items {
        if item.as_object().is_none_or(|object| {
            object.len() != 4
                || object.keys().any(|key| {
                    !matches!(key.as_str(), "action" | "studentCode" | "docId" | "expectedRevisionId")
                })
        }) || item["action"] != "delete"
            || !id(&item["studentCode"])
            || item["studentCode"].as_str().is_some_and(|student| {
                student.len() > 80 || student != student.to_uppercase()
            })
            || !id(&item["docId"])
            || !id(&item["expectedRevisionId"])
            || !docs.insert(item["docId"].as_str().unwrap())
        {
            return Err(invalid());
        }
        if !replay {
            let current = read(&tx, tenant, item["docId"].as_str().unwrap())?
                .ok_or("observation_revision_conflict")?;
            if !in_scope(&current, scope)
                || current["studentCode"] != item["studentCode"]
                || current["revisionId"] != item["expectedRevisionId"]
            {
                return Err("observation_revision_conflict".into());
            }
        }
    }
    let deletions = store.evidence_delete_in_transaction(&tx, tenant, items, mutation, data)?;
    for deletion in &deletions {
        if read(&tx, tenant, deletion["docId"].as_str().unwrap_or_default())?.is_some() {
            return Err("LOCAL_STORE_WRITE_FAILED".into());
        }
        let valid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM observation_evidence_deletions WHERE tenant_id=?1 AND deletion_id=?2 AND deletion_hash=?3)",
            params![tenant, deletion["deletionId"].as_str(), crate::observation_evidence::hash(deletion)],
            |row| row.get(0),
        ).map_err(db)?;
        if !valid { return Err("LOCAL_STORE_WRITE_FAILED".into()); }
    }
    let local_ref = format!("life-records:{mutation}");
    tx.execute(
        "INSERT INTO classaimate_mcp_local_write_receipts VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![tenant, input["receiptId"].as_str(), "life_records_manage",
            input["requestSha256"].as_str(), data.to_string(), local_ref, chrono::Utc::now().timestamp_millis()],
    ).map_err(db)?;
    tx.commit().map_err(db)?;
    Ok(json!({"replayed":false,"result":data,"localRef":local_ref}))
}

pub(crate) fn verify_replay(conn: &Connection, tenant: &str, data: &Value) -> Result<(), String> {
    let raw: String = conn.query_row("SELECT payload_json FROM observation_evidence_mutations WHERE tenant_id=?1 AND mutation_id=?2",params![tenant,data["mutationId"].as_str()],|r|r.get(0)).map_err(db)?;
    let saved: Vec<Value> = serde_json::from_str(&raw).map_err(|_| invalid())?;
    for record in saved {
        if data["action"] == "delete" {
            let stored: Option<(String, String)> = conn.query_row(
                "SELECT payload_json,deletion_hash FROM observation_evidence_deletions WHERE tenant_id=?1 AND deletion_id=?2",
                params![tenant, record["deletionId"].as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional().map_err(db)?;
            if read(conn, tenant, record["docId"].as_str().unwrap_or_default())?.is_some()
                || stored.is_none_or(|(payload, digest)| {
                    serde_json::from_str::<Value>(&payload).ok().as_ref() != Some(&record)
                        || crate::observation_evidence::hash(&record) != digest
                })
            {
                return Err("LOCAL_STORE_WRITE_FAILED".into());
            }
        } else if read(conn, tenant, record["docId"].as_str().unwrap_or_default())? != Some(record) {
            return Err("LOCAL_STORE_WRITE_FAILED".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lists_only_non_lesson_rows_in_range() {
        let root = std::env::temp_dir().join(format!("classaimate-mcp-life-records-{}", crate::random_url_token()));
        std::fs::create_dir_all(&root).unwrap();
        let store = SqliteStore::open(root.join("store.sqlite3")).unwrap();
        let life = json!({"tenantId":"tenant-a","docId":"life-1","id":"life-1","studentCode":"S01","date":"2026-09-14","period":0,"subject":"","observationKind":"non_lesson","contextType":"other","contextLabel":"방과후","eventAtMs":1789363620000_i64,"eventTimePrecision":"exact","eventTimeZone":"Asia/Seoul","eventTimeLabel":"14:27","note":"친구와 협력함","revisionId":"rev-1"});
        store.conn.lock().unwrap().execute("INSERT INTO lesson_observations VALUES(?1,?2,?3,?4,?5,?6,?7)", params!["tenant-a","life-1","2026-09-14",0,"S01",life.to_string(),1]).unwrap();
        let result = list(&store, &json!({"tenantId":"tenant-a","fromDate":"2026-09-14","toDate":"2026-09-14","limit":200})).unwrap();
        assert_eq!(result["records"].as_array().unwrap().len(), 1);
        assert_eq!(result["records"][0]["contextLabel"], "방과후");
        let _ = std::fs::remove_dir_all(root);
    }
}
