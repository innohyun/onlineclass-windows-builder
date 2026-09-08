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
    let date = input["date"].as_str().ok_or_else(invalid)?;
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| invalid())?;
    let period = input["period"]
        .as_i64()
        .filter(|p| (1..=20).contains(p))
        .ok_or_else(invalid)?;
    let subject = input["subject"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.encode_utf16().count() <= 160)
        .ok_or_else(invalid)?;
    Ok(json!({"date":date,"period":period,"subject":subject}))
}
fn in_scope(record: &Value, scope: &Value) -> bool {
    ["date", "period", "subject"]
        .iter()
        .all(|k| record[k] == scope[k])
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
    let mut stmt = conn.prepare("SELECT payload_json FROM lesson_observations WHERE tenant_id=?1 AND date_key=?2 AND period=?3 ORDER BY doc_id").map_err(db)?;
    let rows = stmt
        .query_map(
            params![tenant, scope["date"].as_str(), scope["period"].as_i64()],
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
    let mutation = data["mutationId"].as_str().unwrap();
    let mut conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    let tx = conn.transaction().map_err(db)?;
    let prior_mutation: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM observation_evidence_mutations WHERE tenant_id=?1 AND mutation_id=?2)",params![tenant,mutation],|r|r.get(0)).map_err(db)?;
    let mut prepared = Vec::new();
    if !prior_mutation {
        let existing = records(&tx, tenant, &scope)?;
        let mut students = BTreeSet::new();
        let mut docs = BTreeSet::new();
        for item in items {
            if !id(&item["studentCode"])
                || item["studentCode"]
                    .as_str()
                    .is_some_and(|s| s.len() > 80 || s != s.to_uppercase())
                || !id(&item["docId"])
                || !students.insert(item["studentCode"].as_str().unwrap())
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
                ("date", &scope["date"]),
                ("period", &scope["period"]),
                ("subject", &scope["subject"]),
            ] {
                if patch
                    .get(key)
                    .is_some_and(|v| !v.is_null() && v != expected)
                {
                    return Err(invalid());
                }
            }
            if item["record"]["observationKind"] == "non_lesson"
                || item["record"]["contextType"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
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
                let current: Vec<Value> = existing
                    .iter()
                    .filter(|r| r["studentCode"] == item["studentCode"])
                    .cloned()
                    .collect();
                if previous.is_some()
                    || !current.is_empty()
                    || baseline(&current) != baseline(expected)
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
            for key in ["date", "period", "subject"] {
                record[key] = scope[key].clone();
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
    let local_ref = format!("lesson-observations:{mutation}");
    tx.execute(
        "INSERT INTO classaimate_mcp_local_write_receipts VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![
            tenant,
            input["receiptId"].as_str(),
            "lesson_observations_manage",
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

pub(crate) fn verify_replay(conn: &Connection, tenant: &str, data: &Value) -> Result<(), String> {
    let raw: String = conn.query_row("SELECT payload_json FROM observation_evidence_mutations WHERE tenant_id=?1 AND mutation_id=?2",params![tenant,data["mutationId"].as_str()],|r|r.get(0)).map_err(db)?;
    let saved: Vec<Value> = serde_json::from_str(&raw).map_err(|_| invalid())?;
    for record in saved {
        if read(conn, tenant, record["docId"].as_str().unwrap_or_default())? != Some(record) {
            return Err("LOCAL_STORE_WRITE_FAILED".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(data: Value) -> Value {
        json!({"tenantId":"tenant-a","receiptId":data["mutationId"],"operation":"lesson_observations_manage","requestSha256":crate::observation_evidence::hash(&data),"data":data})
    }
    fn batch() -> Value {
        json!({"scope":{"date":"2026-09-08","period":1,"subject":"수학"},"mutationId":"batch-22","items":(1..=22).map(|n| json!({"action":"create","studentCode":format!("S{n:02}"),"docId":format!("record-{n}"),"baselineRecords":[],"record":{"note":format!("학생 {n} 수학 분석"),"tags":["학습"],"lessonContext":{"unit":"1단원"}}})).collect::<Vec<_>>()})
    }
    #[test]
    fn mcp_observation_batch_replay_update_and_atomic_conflict() {
        let root = std::env::temp_dir().join(format!(
            "classaimate-mcp-observations-{}",
            crate::random_url_token()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let store = SqliteStore::open(root.join("store.sqlite3")).unwrap();
        let data = batch();
        let output =
            crate::classaimate_mcp_write_jobs::apply(&store, &request(data.clone())).unwrap();
        assert_eq!(output["result"], data);
        let query = json!({"tenantId":"tenant-a","date":"2026-09-08","period":1,"subject":"수학","limit":12});
        let first = list(&store, &query).unwrap();
        assert_eq!(first["records"].as_array().unwrap().len(), 12);
        let mut query2 = query.clone();
        query2["cursor"] = first["nextCursor"].clone();
        assert_eq!(
            list(&store, &query2).unwrap()["records"]
                .as_array()
                .unwrap()
                .len(),
            10
        );
        assert_eq!(
            crate::classaimate_mcp_write_jobs::apply(&store, &request(data.clone())).unwrap()
                ["replayed"],
            true
        );
        store
            .conn
            .lock()
            .unwrap()
            .execute("DELETE FROM classaimate_mcp_local_write_receipts", [])
            .unwrap();
        assert_eq!(
            crate::classaimate_mcp_write_jobs::apply(&store, &request(data.clone())).unwrap()
                ["result"],
            data
        );
        let before = first["records"][0].clone();
        let update = json!({"scope":data["scope"],"mutationId":"update","items":[{"action":"update","studentCode":before["studentCode"],"docId":before["docId"],"expectedRevisionId":before["revisionId"],"correctionReason":"오타 정정","record":{"note":"정정 분석"}}]});
        crate::classaimate_mcp_write_jobs::apply(&store, &request(update.clone())).unwrap();
        let detail = store
            .evidence_detail("tenant-a", before["docId"].as_str().unwrap(), false)
            .unwrap();
        assert_eq!(detail["revisions"].as_array().unwrap().len(), 2);
        let current = read(
            &store.conn.lock().unwrap(),
            "tenant-a",
            before["docId"].as_str().unwrap(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(current["note"], "정정 분석");
        assert_eq!(current["lessonContext"], before["lessonContext"]);
        assert_eq!(current["tags"], before["tags"]);
        let mut conflict = data.clone();
        conflict["mutationId"] = json!("conflict");
        for (i, item) in conflict["items"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            if i < 21 {
                item["studentCode"] = json!(format!("NEW{i}"));
                item["docId"] = json!(format!("new-{i}"));
            }
        }
        assert_eq!(
            crate::classaimate_mcp_write_jobs::apply(&store, &request(conflict)).unwrap_err(),
            "observation_revision_conflict"
        );
        assert_eq!(
            store
                .conn
                .lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM lesson_observations", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            22
        );
        let mut stale = update;
        stale["mutationId"] = json!("stale");
        assert_eq!(
            crate::classaimate_mcp_write_jobs::apply(&store, &request(stale)).unwrap_err(),
            "observation_revision_conflict"
        );
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
