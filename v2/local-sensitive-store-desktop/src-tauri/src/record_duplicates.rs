//! Exact observation duplicates. Searching never writes; cleanup uses the evidence archive writer.
use crate::{normalize_tenant_id, now_ms, observation_evidence, AppState, SqliteStore};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
#[path = "record_duplicates_tests.rs"]
mod tests;

const MAX_ARCHIVE: usize = 200;
const APPLY_PREFIX: &str = "local-duplicate-cleanup:";
const UNDO_PREFIX: &str = "local-duplicate-restore:";
const MARKER: &str = "localDuplicateCleanup";

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScanInput {
    tenant_id: String,
    #[serde(default)]
    student_id: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GroupSelection {
    group_id: String,
    snapshot_hash: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApplyInput {
    tenant_id: String,
    cleanup_id: String,
    groups: Vec<GroupSelection>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UndoInput {
    tenant_id: String,
    cleanup_id: String,
}

fn db(error: rusqlite::Error) -> String {
    format!("record_duplicates_db_failed:{error}")
}
fn tenant(value: &str) -> Result<&str, String> {
    if value.is_empty() || normalize_tenant_id(Some(&json!(value))) != value {
        return Err("tenant_id_required".into());
    }
    Ok(value)
}
fn cleanup_id(value: &str) -> Result<&str, String> {
    if value.len() < 8 || value.len() > 100
        || !value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("duplicate_cleanup_id_invalid".into());
    }
    Ok(value)
}
fn parse(raw: &str) -> Result<Value, String> {
    serde_json::from_str(raw).map_err(|_| "record_duplicates_payload_invalid".into())
}
fn archived(record: &Value) -> bool {
    record["recordState"] == "archived" || record["archivedAtMs"].as_i64().unwrap_or(0) > 0
}
fn body(record: &Value) -> String {
    ["note", "observation", "content", "summary"].iter()
        .filter_map(|key| record[*key].as_str())
        .find(|text| !text.trim().is_empty()).unwrap_or_default().to_string()
}
fn semantic(record: &Value) -> Option<Value> {
    if archived(record) || body(record).trim().is_empty()
        || record["studentCode"].as_str().is_none_or(str::is_empty)
        || record["date"].as_str().is_none_or(str::is_empty)
        || record["revisionId"].as_str().is_none_or(str::is_empty)
    {
        return None;
    }
    let mut result = record.as_object()?.clone();
    // Only top-level storage/provenance fields are ignored. Unknown metadata, nested IDs,
    // actual event time, lesson/page links, status, tags and photo references remain exact.
    for key in ["id", "docId", "batchId", "createdAt", "createdAtMs", "createdAtIso",
        "updatedAt", "updatedAtMs", "updatedAtIso", "savedAtMs", "revisionId",
        "revisionHash", "expectedRevisionId", "evidenceVersion", "evidenceBaseline",
        "writerAttribution", "correctionReason", "mutationId",
        "recordState", "archivedAtMs", MARKER]
    {
        result.remove(key);
    }
    if let Some(timestamps) = result.get_mut("legacyOriginalTimestamps").and_then(Value::as_object_mut) {
        timestamps.remove("createdAtMs");
        timestamps.remove("updatedAtMs");
    }
    for key in ["note", "observation", "content", "summary"] {
        if let Some(text) = result.get(key).and_then(Value::as_str) {
            result.insert(key.into(), json!(text.split_whitespace().collect::<Vec<_>>().join(" ")));
        }
    }
    Some(Value::Object(result))
}
fn read(conn: &Connection, tenant: &str, doc: &str) -> Result<Option<Value>, String> {
    conn.query_row("SELECT payload_json FROM lesson_observations WHERE tenant_id=?1 AND doc_id=?2",
        params![tenant, doc], |r| r.get::<_, String>(0)).optional().map_err(db)?
        .map(|raw| parse(&raw)).transpose()
}
fn collect_references(value: &Value, ids: &BTreeSet<String>, result: &mut BTreeSet<String>) {
    fn add(text: &str, ids: &BTreeSet<String>, result: &mut BTreeSet<String>) {
        let id = text.strip_prefix("observation:").unwrap_or(text);
        if ids.contains(id) { result.insert(id.to_string()); }
    }
    match value {
        Value::String(text) => add(text, ids, result),
        Value::Array(items) => items.iter().for_each(|v| collect_references(v, ids, result)),
        Value::Object(map) => {
            for (key, value) in map {
                add(key, ids, result);
                collect_references(value, ids, result);
            }
        }
        _ => {}
    }
}
fn references(conn: &Connection, tenant: &str, ids: &BTreeSet<String>) -> Result<BTreeSet<String>, String> {
    let mut found = BTreeSet::new();
    // Covers workspace selectedEvidence/evidenceLinks/behaviorTopics/followUps and draft
    // evidence JSON, including object keys. Never rewrite a student's selected evidence.
    let mut statement = conn.prepare("SELECT payload_json FROM student_record_draft_sets WHERE tenant_id=?1 UNION ALL SELECT payload_json FROM student_record_drafts WHERE tenant_id=?1").map_err(db)?;
    let rows = statement.query_map([tenant], |r| r.get::<_, String>(0)).map_err(db)?;
    for row in rows { collect_references(&parse(&row.map_err(db)?)?, ids, &mut found); }
    Ok(found)
}
fn scan(conn: &Connection, input: &ScanInput) -> Result<Value, String> {
    let tenant = tenant(&input.tenant_id)?;
    let mut statement = conn.prepare("SELECT doc_id,student_code,date_key,period,payload_json FROM lesson_observations WHERE tenant_id=?1 AND (?2='' OR student_code=?2) ORDER BY doc_id").map_err(db)?;
    let rows = statement.query_map(params![tenant, input.student_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, String>(4)?))).map_err(db)?;
    let mut by_content: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut scanned = 0;
    for row in rows {
        let (doc, student, date, period, raw) = row.map_err(db)?;
        let record = parse(&raw)?;
        if record["tenantId"] != tenant || record["docId"] != doc
            || record["id"].as_str().is_some_and(|id| id != doc)
            || record["studentCode"] != student || record["date"] != date
            || record["period"].as_i64() != Some(period)
        { return Err("duplicate_record_authority_mismatch".into()); }
        if !archived(&record) { scanned += 1; }
        if let Some(content) = semantic(&record) {
            ids.insert(doc);
            by_content.entry(observation_evidence::hash(&content)).or_default().push(record);
        }
    }
    let referenced = references(conn, tenant, &ids)?;
    let mut groups = Vec::new();
    for (group_id, mut records) in by_content {
        if records.len() < 2 { continue; }
        records.sort_by(|a,b| {
            let key = |v: &Value| (!referenced.contains(v["docId"].as_str().unwrap_or_default()),
                v["createdAtMs"].as_i64().unwrap_or(i64::MAX), v["docId"].as_str().unwrap_or_default().to_string());
            key(a).cmp(&key(b))
        });
        let reference_count = records.iter().filter(|v| referenced.contains(v["docId"].as_str().unwrap())).count();
        let blocked = if reference_count > 1 { vec!["학생기록 근거로 연결된 기록이 여러 건이어서 자동 정리할 수 없습니다."] } else { vec![] };
        let summaries: Vec<Value> = records.iter().map(|v| json!({"docId":v["docId"],"revisionId":v["revisionId"],
            "savedAtMs":v["updatedAtMs"],"body":body(v),"referenced":referenced.contains(v["docId"].as_str().unwrap()),
            "recordHash":observation_evidence::hash(v)})).collect();
        let snapshot_hash = observation_evidence::hash(&json!({"groupId":group_id,"records":summaries}));
        let first = &records[0];
        groups.push(json!({"groupId":group_id,"snapshotHash":snapshot_hash,"sectionKey":"observations",
            "studentId":first["studentCode"],"studentName":first["studentName"],"date":first["date"],"body":body(first),
            "matchReason":"같은 학생·날짜·수업/일상 맥락과 본문·상태·태그·사진·기타 정보가 같습니다. 본문의 공백과 저장 식별자·시각만 제외했습니다.",
            "keeperId":first["docId"],"records":summaries,"archiveIds":records.iter().skip(1).map(|v| v["docId"].clone()).collect::<Vec<_>>(),
            "canApply":blocked.is_empty(),"blockedReasons":blocked}));
    }
    Ok(json!({"ok":true,"tenantId":tenant,"scannedCount":scanned,"groups":groups,"supportedSections":["observations"],"maxArchiveCount":MAX_ARCHIVE}))
}

fn prior_mutation(conn: &Connection, tenant: &str, id: &str, identity: &Value) -> Result<Option<Vec<Value>>, String> {
    let prior = conn.query_row("SELECT request_hash,payload_json FROM observation_evidence_mutations WHERE tenant_id=?1 AND mutation_id=?2",
        params![tenant, id], |r| Ok((r.get::<_, String>(0)?,r.get::<_, String>(1)?))).optional().map_err(db)?;
    let Some((hash, raw)) = prior else { return Ok(None); };
    if hash != observation_evidence::hash(&json!({"tenantId":tenant,"records":identity,"mutationId":id})) {
        return Err("duplicate_cleanup_replay_conflict".into());
    }
    let records: Vec<Value> = serde_json::from_str(&raw).map_err(|_| "duplicate_cleanup_history_invalid")?;
    readback(conn, tenant, &records)?;
    Ok(Some(records))
}
fn readback(conn: &Connection, tenant: &str, records: &[Value]) -> Result<(), String> {
    for record in records {
        if read(conn, tenant, record["docId"].as_str().ok_or("duplicate_cleanup_history_invalid")?)?.as_ref() != Some(record) {
            return Err("duplicate_cleanup_readback_conflict".into());
        }
    }
    Ok(())
}
fn preserved_content(record: &Value) -> Value {
    let mut content = record.as_object().cloned().unwrap_or_default();
    for key in ["updatedAtMs", "updatedAtIso", "revisionId", "revisionHash", "evidenceVersion",
        "expectedRevisionId", "correctionReason", "mutationId", "writerAttribution", "recordState", "archivedAtMs", MARKER] {
        content.remove(key);
    }
    Value::Object(content)
}
fn verify_preserved(before: &[Value], after: &[Value]) -> Result<(), String> {
    if before.len() != after.len() || before.iter().zip(after).any(|(a,b)| preserved_content(a) != preserved_content(b)) {
        return Err("duplicate_cleanup_content_changed".into());
    }
    Ok(())
}
fn keeper_readback(conn: &Connection, tenant: &str, records: &[Value]) -> Result<(), String> {
    for record in records {
        let marker = &record[MARKER];
        let keeper = read(conn, tenant, marker["keeperId"].as_str().ok_or("duplicate_cleanup_history_invalid")?)?
            .ok_or("duplicate_cleanup_readback_conflict")?;
        if archived(&keeper) || observation_evidence::hash(&keeper) != marker["keeperHash"] {
            return Err("duplicate_cleanup_readback_conflict".into());
        }
    }
    Ok(())
}
fn history_records(conn: &Connection, tenant: &str, id: &str) -> Result<Vec<Value>, String> {
    let raw: String = conn.query_row("SELECT payload_json FROM observation_evidence_mutations WHERE tenant_id=?1 AND mutation_id=?2",
        params![tenant, format!("{APPLY_PREFIX}{id}")], |r| r.get(0)).optional().map_err(db)?.ok_or("duplicate_cleanup_not_found")?;
    let records: Vec<Value> = serde_json::from_str(&raw).map_err(|_| "duplicate_cleanup_history_invalid")?;
    if records.is_empty() || records.len() > MAX_ARCHIVE || records.iter().any(|v| v[MARKER]["cleanupId"] != id || !archived(v)) {
        return Err("duplicate_cleanup_history_invalid".into());
    }
    Ok(records)
}
impl SqliteStore {
    fn scan_record_duplicates(&self, input: ScanInput) -> Result<Value, String> {
        let conn = self.conn.lock().map_err(|_| "db_lock_failed")?;
        scan(&conn, &input)
    }
    fn apply_record_duplicates(&self, input: ApplyInput) -> Result<Value, String> {
        let tenant = tenant(&input.tenant_id)?;
        cleanup_id(&input.cleanup_id)?;
        if input.groups.is_empty() || input.groups.len() > MAX_ARCHIVE { return Err("duplicate_cleanup_selection_invalid".into()); }
        let identity = json!(input);
        let mutation_id = format!("{APPLY_PREFIX}{}", input.cleanup_id);
        let _access = self.media_access(tenant)?;
        let mut conn = self.conn.lock().map_err(|_| "db_lock_failed")?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(db)?;
        crate::restore_journal::ready(&tx, tenant)?;
        if let Some(records) = prior_mutation(&tx, tenant, &mutation_id, &identity)? {
            keeper_readback(&tx, tenant, &records)?;
            return Ok(json!({"ok":true,"cleanupId":input.cleanup_id,"archivedCount":records.len(),"replayed":true,"records":records}));
        }
        let current = scan(&tx, &ScanInput { tenant_id: tenant.into(), student_id: String::new() })?;
        let mut prepared = Vec::new();
        let mut keepers = Vec::new();
        let mut selected = BTreeSet::new();
        for selection in &input.groups {
            if !selected.insert(&selection.group_id) { return Err("duplicate_cleanup_selection_invalid".into()); }
            let group = current["groups"].as_array().unwrap().iter().find(|g| g["groupId"] == selection.group_id)
                .ok_or("duplicate_cleanup_scan_stale")?;
            if group["snapshotHash"] != selection.snapshot_hash { return Err("duplicate_cleanup_scan_stale".into()); }
            if group["canApply"] != true { return Err("duplicate_cleanup_referenced".into()); }
            let keeper = read(&tx, tenant, group["keeperId"].as_str().unwrap())?.ok_or("duplicate_cleanup_scan_stale")?;
            let verification = observation_evidence::verify_record(&tx, &self.data_dir, tenant, group["keeperId"].as_str().unwrap())?;
            if verification["errors"].as_array().is_none_or(|errors| errors.iter().any(|e| e != "legacy_photo_unverified" && e != "server_batches_missing_locally")) {
                return Err("observation_evidence_integrity_mismatch".into());
            }
            let keeper_hash = observation_evidence::hash(&keeper);
            keepers.push(keeper);
            for doc in group["archiveIds"].as_array().unwrap() {
                let mut record = read(&tx, tenant, doc.as_str().unwrap())?.ok_or("duplicate_cleanup_scan_stale")?;
                record[MARKER] = json!({"version":1,"cleanupId":input.cleanup_id,"groupId":group["groupId"],
                    "keeperId":group["keeperId"],"keeperHash":keeper_hash,"originalRevisionId":record["revisionId"],"createdAtMs":now_ms()});
                record["expectedRevisionId"] = record["revisionId"].clone();
                record["recordState"] = json!("archived");
                record["correctionReason"] = json!("로컬 자료함의 정확 중복 정리: 대표 기록을 남기고 복구 가능하게 보관");
                prepared.push(record);
            }
        }
        if prepared.is_empty() || prepared.len() > MAX_ARCHIVE { return Err("duplicate_cleanup_limit_exceeded".into()); }
        let original = prepared.clone();
        let saved = self.evidence_save_in_transaction(&tx, tenant, prepared, &mutation_id, Some(&identity))?;
        verify_preserved(&original, &saved)?;
        readback(&tx, tenant, &saved)?;
        readback(&tx, tenant, &keepers)?;
        tx.commit().map_err(db)?;
        Ok(json!({"ok":true,"cleanupId":input.cleanup_id,"archivedCount":saved.len(),"replayed":false,"records":saved}))
    }
    fn list_record_duplicate_history(&self, input: ScanInput) -> Result<Value, String> {
        let tenant = tenant(&input.tenant_id)?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed")?;
        let mut statement = conn.prepare("SELECT mutation_id,created_at_ms FROM observation_evidence_mutations WHERE tenant_id=?1 AND mutation_id LIKE 'local-duplicate-cleanup:%' ORDER BY created_at_ms DESC,mutation_id DESC LIMIT 50").map_err(db)?;
        let rows = statement.query_map([tenant], |r| Ok((r.get::<_, String>(0)?,r.get::<_, i64>(1)?))).map_err(db)?;
        let mut entries = Vec::new();
        for row in rows {
            let (mutation, created) = row.map_err(db)?;
            let id = mutation.strip_prefix(APPLY_PREFIX).ok_or("duplicate_cleanup_history_invalid")?;
            let records = history_records(&conn, tenant, id)?;
            let can_undo = readback(&conn, tenant, &records).is_ok();
            let restored = prior_mutation(&conn, tenant, &format!("{UNDO_PREFIX}{id}"),
                &json!({"tenantId":tenant,"cleanupId":id})).ok().flatten().is_some();
            let state = if can_undo { "archived" } else if restored { "restored" } else { "changed" };
            entries.push(json!({"cleanupId":id,"createdAtMs":created,"archivedCount":records.len(),"canUndo":can_undo && !restored,"state":state,
                "blockedReason":if state=="changed" { "정리 후 기록이 변경되어 자동 복원할 수 없습니다." } else { "" },
                "records":records.iter().map(|v| json!({"docId":v["docId"],"studentName":v["studentName"],"date":v["date"],"body":body(v)})).collect::<Vec<_>>()}));
        }
        Ok(json!({"ok":true,"entries":entries}))
    }
    fn undo_record_duplicate_cleanup(&self, input: UndoInput) -> Result<Value, String> {
        let tenant = tenant(&input.tenant_id)?;
        cleanup_id(&input.cleanup_id)?;
        let identity = json!(input);
        let mutation_id = format!("{UNDO_PREFIX}{}", input.cleanup_id);
        let _access = self.media_access(tenant)?;
        let mut conn = self.conn.lock().map_err(|_| "db_lock_failed")?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(db)?;
        crate::restore_journal::ready(&tx, tenant)?;
        if let Some(records) = prior_mutation(&tx, tenant, &mutation_id, &identity)? {
            return Ok(json!({"ok":true,"cleanupId":input.cleanup_id,"restoredCount":records.len(),"replayed":true}));
        }
        let archived_records = history_records(&tx, tenant, &input.cleanup_id)?;
        readback(&tx, tenant, &archived_records).map_err(|_| "duplicate_cleanup_restore_stale")?;
        let mut prepared = Vec::new();
        for archived_record in &archived_records {
            let raw: String = tx.query_row("SELECT payload_json FROM observation_evidence_revisions WHERE tenant_id=?1 AND doc_id=?2 AND revision_id=?3",
                params![tenant,archived_record["docId"].as_str(),archived_record[MARKER]["originalRevisionId"].as_str()], |r| r.get(0)).map_err(db)?;
            let mut record = parse(&raw)?["record"].clone();
            if record["docId"] != archived_record["docId"] || record["tenantId"] != tenant || archived(&record) {
                return Err("duplicate_cleanup_history_invalid".into());
            }
            record["expectedRevisionId"] = archived_record["revisionId"].clone();
            record["recordState"] = json!("active");
            record["archivedAtMs"] = json!(0);
            record["correctionReason"] = json!("로컬 자료함의 중복 정리 되돌리기");
            prepared.push(record);
        }
        let original = prepared.clone();
        let saved = self.evidence_save_in_transaction(&tx, tenant, prepared, &mutation_id, Some(&identity))?;
        verify_preserved(&original, &saved)?;
        readback(&tx, tenant, &saved)?;
        tx.commit().map_err(db)?;
        Ok(json!({"ok":true,"cleanupId":input.cleanup_id,"restoredCount":saved.len(),"replayed":false}))
    }
}

fn command_result(state: tauri::State<'_, AppState>, action: impl FnOnce(&SqliteStore) -> Result<Value, String>) -> Value {
    let result = state.store.lock().ok().and_then(|s| s.clone()).ok_or_else(|| "local_store_unavailable".to_string())
        .and_then(|store| action(&store));
    result.unwrap_or_else(|error| json!({"ok":false,"error":error}))
}
#[tauri::command]
pub(crate) fn scan_record_duplicates(state: tauri::State<'_, AppState>, input: ScanInput) -> Value {
    command_result(state, |store| store.scan_record_duplicates(input))
}
#[tauri::command]
pub(crate) fn apply_record_duplicates(state: tauri::State<'_, AppState>, input: ApplyInput) -> Value {
    command_result(state, |store| store.apply_record_duplicates(input))
}
#[tauri::command]
pub(crate) fn list_record_duplicate_history(state: tauri::State<'_, AppState>, input: ScanInput) -> Value {
    command_result(state, |store| store.list_record_duplicate_history(input))
}
#[tauri::command]
pub(crate) fn undo_record_duplicate_cleanup(state: tauri::State<'_, AppState>, input: UndoInput) -> Value {
    command_result(state, |store| store.undo_record_duplicate_cleanup(input))
}
