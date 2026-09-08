//! Local observation provenance. Hashes prove consistency, not the truth of an observation.
#[cfg(test)]
#[path = "observation_evidence_tests.rs"]
mod tests;
use crate::{normalize_observation, now_ms, SqliteStore};
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fs, path::Path};

fn db(error: rusqlite::Error) -> String {
    format!("observation_evidence_db_failed:{error}")
}
pub(crate) fn hash(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
fn uuid() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let s = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        &s[..8],
        &s[8..12],
        &s[12..16],
        &s[16..20],
        &s[20..]
    )
}
pub(crate) fn schema(prefix: &str) -> String {
    format!("CREATE TABLE IF NOT EXISTS {prefix}observation_evidence_revisions (tenant_id TEXT NOT NULL,revision_id TEXT NOT NULL,doc_id TEXT NOT NULL,payload_json TEXT NOT NULL,revision_hash TEXT NOT NULL,saved_at_ms INTEGER NOT NULL,PRIMARY KEY(tenant_id,revision_id));
    CREATE TABLE IF NOT EXISTS {prefix}observation_evidence_batches (tenant_id TEXT NOT NULL,receipt_id TEXT NOT NULL,payload_json TEXT NOT NULL,commitment_sha256 TEXT NOT NULL,created_at_ms INTEGER NOT NULL,PRIMARY KEY(tenant_id,receipt_id));
    CREATE TABLE IF NOT EXISTS {prefix}observation_evidence_mutations (tenant_id TEXT NOT NULL,mutation_id TEXT NOT NULL,request_hash TEXT NOT NULL,payload_json TEXT NOT NULL,created_at_ms INTEGER NOT NULL,PRIMARY KEY(tenant_id,mutation_id));
    CREATE TABLE IF NOT EXISTS {prefix}observation_evidence_receipts (tenant_id TEXT NOT NULL,receipt_id TEXT NOT NULL,payload_json TEXT NOT NULL,created_at_ms INTEGER NOT NULL,PRIMARY KEY(tenant_id,receipt_id));
    CREATE TABLE IF NOT EXISTS {prefix}observation_evidence_exports (tenant_id TEXT NOT NULL,export_id TEXT NOT NULL,payload_json TEXT NOT NULL,created_at_ms INTEGER NOT NULL,PRIMARY KEY(tenant_id,export_id));
    CREATE TABLE IF NOT EXISTS {prefix}observation_evidence_reconciliation (tenant_id TEXT NOT NULL,state_id TEXT NOT NULL,payload_json TEXT NOT NULL,created_at_ms INTEGER NOT NULL,PRIMARY KEY(tenant_id,state_id));")
}
pub(crate) fn ensure_schema(conn: &Connection, data_dir: &Path) -> Result<(), String> {
    conn.execute_batch(&schema("")).map_err(db)?;
    let rows = {
        let mut statement = conn.prepare("SELECT payload_json FROM lesson_observations WHERE json_extract(payload_json,'$.evidenceVersion') IS NULL").map_err(db)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(db)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db)?
    };
    conn.execute_batch("SAVEPOINT observation_baseline")
        .map_err(db)?;
    let result = (|| {
        for raw in rows {
            let mut record: Value = serde_json::from_str(&raw)
                .map_err(|_| "observation_evidence_invalid_legacy".to_string())?;
            let tenant = record["tenantId"].as_str().unwrap_or_default().to_string();
            record["legacyOriginalTimestamps"] = json!({"createdAtMs":record["createdAtMs"],"updatedAtMs":record["updatedAtMs"],"eventAtMs":record["eventAtMs"]});
            record["evidenceBaseline"] = json!("legacy_unverified");
            record["createdAtMs"] = json!(now_ms());
            record["updatedAtMs"] = record["createdAtMs"].clone();
            record["eventTimePrecision"] = json!("unknown");
            record["eventAtMs"] = Value::Null;
            record["eventTimeZone"] = json!("Asia/Seoul");
            record["eventTimeLabel"] = json!("");
            let saved = append(
                conn,
                data_dir,
                record,
                None,
                "legacy_baseline",
                "도입 이전 기록 기준본",
                now_ms(),
                None,
            )?;
            batch(
                conn,
                &tenant,
                &[saved["revisionHash"].as_str().unwrap().to_string()],
            )?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => conn
            .execute_batch("RELEASE observation_baseline")
            .map_err(db),
        Err(error) => {
            let _ = conn
                .execute_batch("ROLLBACK TO observation_baseline; RELEASE observation_baseline");
            Err(error)
        }
    }
}
fn read_record(conn: &Connection, tenant: &str, doc: &str) -> Result<Option<Value>, String> {
    let raw = conn
        .query_row(
            "SELECT payload_json FROM lesson_observations WHERE tenant_id=?1 AND doc_id=?2",
            params![tenant, doc],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(db)?;
    raw.map(|raw| {
        serde_json::from_str(&raw).map_err(|_| "observation_evidence_invalid_record".to_string())
    })
    .transpose()
}
fn photos(
    conn: &Connection,
    dir: &Path,
    tenant: &str,
    record: &Value,
) -> Result<Vec<Value>, String> {
    let mut result = Vec::new();
    for id in record["photoMediaIds"].as_array().into_iter().flatten() {
        let id = id.as_str().ok_or("observation_photo_invalid")?;
        let row = conn.query_row("SELECT local_path,content_type FROM board_media_files WHERE tenant_id=?1 AND media_id=?2", params![tenant,id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional().map_err(db)?.ok_or("observation_photo_missing")?;
        let path = dir.join(row.0);
        let bytes = fs::read(path).map_err(|_| "observation_photo_missing".to_string())?;
        result.push(json!({"mediaId":id,"sha256":format!("{:x}",Sha256::digest(&bytes)),"byteSize":bytes.len(),"mimeType":row.1}));
    }
    result.sort_by(|a, b| a["mediaId"].as_str().cmp(&b["mediaId"].as_str()));
    Ok(result)
}
fn append(
    conn: &Connection,
    dir: &Path,
    mut record: Value,
    previous: Option<&Value>,
    kind: &str,
    reason: &str,
    now: i64,
    parents: Option<&[Value]>,
) -> Result<Value, String> {
    let tenant = record["tenantId"]
        .as_str()
        .ok_or("tenant_id_required")?
        .to_string();
    let doc = record["docId"]
        .as_str()
        .or(record["id"].as_str())
        .ok_or("doc_id_required")?
        .to_string();
    for field in [
        "evidenceVersion",
        "revisionId",
        "revisionHash",
        "expectedRevisionId",
        "correctionReason",
        "mutationId",
    ] {
        record
            .as_object_mut()
            .ok_or("invalid_record")?
            .remove(field);
    }
    let id = uuid();
    let photo_list = if kind == "legacy_baseline" {
        let mut list = Vec::new();
        for media_id in record["photoMediaIds"].as_array().into_iter().flatten() {
            match photos(conn,dir,&tenant,&json!({"photoMediaIds":[media_id]})) {Ok(mut p)=>list.append(&mut p),Err(_)=>list.push(json!({"mediaId":media_id,"sha256":null,"byteSize":null,"mimeType":null,"missing":true}))}
        }
        list
    } else {
        photos(conn, dir, &tenant, &record)?
    };
    let mut envelope = json!({"version":1,"tenantId":tenant,"docId":doc,"revisionId":id,
        "previousRevisionId":previous.map(|p|p["revisionId"].clone()),"previousRevisionHash":previous.map(|p|p["revisionHash"].clone()),
        "eventKind":kind,"savedAtMs":now,"reason":reason,"photos":photo_list,"record":record});
    if let Some(parents) = parents {
        envelope["parentRevisionIds"] = json!(parents
            .iter()
            .map(|p| p["revisionId"].clone())
            .collect::<Vec<_>>());
        envelope["parentRevisionHashes"] = json!(parents
            .iter()
            .map(|p| p["revisionHash"].clone())
            .collect::<Vec<_>>());
    }
    let digest = hash(&envelope);
    conn.execute(
        "INSERT INTO observation_evidence_revisions VALUES(?1,?2,?3,?4,?5,?6)",
        params![tenant, id, doc, envelope.to_string(), digest, now],
    )
    .map_err(db)?;
    record["evidenceVersion"] = json!(1);
    record["revisionId"] = json!(id);
    record["revisionHash"] = json!(digest);
    conn.execute("INSERT INTO lesson_observations(tenant_id,doc_id,date_key,period,student_code,payload_json,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(tenant_id,doc_id) DO UPDATE SET date_key=excluded.date_key,period=excluded.period,student_code=excluded.student_code,payload_json=excluded.payload_json,updated_at_ms=excluded.updated_at_ms",
        params![tenant,doc,record["date"].as_str().unwrap_or_default(),record["period"].as_i64().unwrap_or(0),record["studentCode"].as_str().unwrap_or_default(),record.to_string(),record["updatedAtMs"].as_i64().unwrap_or(now)]).map_err(db)?;
    Ok(record)
}
fn batch(conn: &Connection, tenant: &str, hashes: &[String]) -> Result<(), String> {
    if hashes.is_empty() {
        return Ok(());
    }
    let id = uuid();
    let mut nonce = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut nonce);
    let mut hashes = hashes.to_vec();
    hashes.sort();
    let payload = json!({"version":1,"receiptId":id,"nonce":nonce.iter().map(|b|format!("{b:02x}")).collect::<String>(),"revisionHashes":hashes});
    conn.execute(
        "INSERT INTO observation_evidence_batches VALUES(?1,?2,?3,?4,?5)",
        params![tenant, id, payload.to_string(), hash(&payload), now_ms()],
    )
    .map_err(db)?;
    Ok(())
}
fn valid_numbers(value: &Value) -> bool {
    match value {
        Value::Number(n) => n
            .as_i64()
            .is_some_and(|n| n.unsigned_abs() <= 9_007_199_254_740_991),
        Value::Array(a) => a.iter().all(valid_numbers),
        Value::Object(o) => o.values().all(valid_numbers),
        _ => true,
    }
}
impl SqliteStore {
    pub(crate) fn evidence_save(
        &self,
        tenant: &str,
        records: Vec<Value>,
        mutation_id: &str,
    ) -> Result<Vec<Value>, String> {
        if tenant.is_empty() {
            return Err("tenant_id_required".into());
        }
        if !valid_numbers(&json!(records)) {
            return Err("observation_evidence_unsafe_number".into());
        }
        let request_hash =
            hash(&json!({"tenantId":tenant,"records":records,"mutationId":mutation_id}));
        let mut conn = self.conn.lock().map_err(|_| "db_lock_failed")?;
        let tx = conn.transaction().map_err(db)?;
        if !mutation_id.is_empty() {
            let prior=tx.query_row("SELECT request_hash,payload_json FROM observation_evidence_mutations WHERE tenant_id=?1 AND mutation_id=?2",params![tenant,mutation_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional().map_err(db)?;
            if let Some((h, p)) = prior {
                if h != request_hash {
                    return Err("observation_mutation_conflict".into());
                }
                return serde_json::from_str(&p)
                    .map_err(|_| "observation_evidence_invalid_mutation".into());
            }
        }
        let mut result = Vec::new();
        let mut hashes = Vec::new();
        let mut ids = HashSet::new();
        for mut input in records {
            if !input.is_object() {
                return Err("invalid_record".into());
            }
            input["tenantId"] = json!(tenant);
            let now = now_ms();
            input["updatedAtMs"] = json!(now);
            let mut record = normalize_observation(input.clone())?;
            chrono::NaiveDate::parse_from_str(
                record["date"].as_str().unwrap_or_default(),
                "%Y-%m-%d",
            )
            .map_err(|_| "observation_event_date_mismatch")?;
            record["writerAttribution"] = json!({"authority":"local_helper","accountVerified":false,"deviceVerified":false,"path":"local_observations_api"});
            let doc = record["docId"].as_str().unwrap_or_default();
            if !ids.insert(doc.to_string()) {
                return Err("observation_duplicate_record".into());
            }
            let prior = read_record(&tx, tenant, doc)?;
            let reason = input["correctionReason"]
                .as_str()
                .unwrap_or_default()
                .trim();
            if let Some(previous) = &prior {
                if input["expectedRevisionId"] != previous["revisionId"]
                    || previous["revisionId"].is_null()
                {
                    return Err("observation_revision_conflict".into());
                }
                if reason.is_empty() {
                    return Err("observation_correction_reason_required".into());
                }
                if verify_record(&tx, &self.data_dir, tenant, doc)?["errors"]
                    .as_array()
                    .is_none_or(|a| {
                        a.iter().any(|e| {
                            e != "legacy_photo_unverified" && e != "server_batches_missing_locally"
                        })
                    })
                {
                    return Err("observation_evidence_integrity_mismatch".into());
                }
            } else if input["expectedRevisionId"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
            {
                return Err("observation_revision_conflict".into());
            }
            let precision = input["eventTimePrecision"].as_str().unwrap_or("unknown");
            if !matches!(precision, "exact" | "approximate" | "unknown") {
                return Err("observation_event_precision_invalid".into());
            }
            let event = if precision == "unknown" {
                Value::Null
            } else {
                let ms = input["eventAtMs"]
                    .as_i64()
                    .filter(|n| *n > 0)
                    .ok_or("observation_event_time_required")?;
                let date = (chrono::DateTime::from_timestamp_millis(ms)
                    .ok_or("observation_event_time_invalid")?
                    + chrono::Duration::hours(9))
                .format("%Y-%m-%d")
                .to_string();
                if record["date"].as_str() != Some(&date) {
                    return Err("observation_event_date_mismatch".into());
                }
                json!(ms)
            };
            record["eventTimePrecision"] = json!(precision);
            record["eventTimeZone"] = json!("Asia/Seoul");
            record["eventAtMs"] = event;
            record["eventTimeLabel"] = json!(record["eventAtMs"]
                .as_i64()
                .and_then(chrono::DateTime::from_timestamp_millis)
                .map(|t| (t + chrono::Duration::hours(9)).format("%H:%M").to_string())
                .unwrap_or_default());
            record["createdAtMs"] = prior
                .as_ref()
                .map(|p| p["createdAtMs"].clone())
                .filter(|v| !v.is_null())
                .unwrap_or(json!(now));
            let was_archived = prior.as_ref().is_some_and(|p| {
                p["recordState"] == "archived" || p["archivedAtMs"].as_i64().unwrap_or(0) > 0
            });
            let archived = record["recordState"] == "archived"
                || record["archivedAtMs"].as_i64().unwrap_or(0) > 0;
            record["archivedAtMs"] = if archived {
                if was_archived {
                    prior.as_ref().unwrap()["archivedAtMs"].clone()
                } else {
                    json!(now)
                }
            } else {
                json!(0)
            };
            record["recordState"] = json!(if archived { "archived" } else { "active" });
            let kind = if prior.is_none() {
                "create"
            } else if archived != was_archived {
                if archived {
                    "archive"
                } else {
                    "restore"
                }
            } else {
                "correct"
            };
            let saved = append(
                &tx,
                &self.data_dir,
                record,
                prior.as_ref(),
                kind,
                reason,
                now,
                None,
            )?;
            hashes.push(saved["revisionHash"].as_str().unwrap().to_string());
            result.push(saved);
        }
        batch(&tx, tenant, &hashes)?;
        if !mutation_id.is_empty() {
            tx.execute(
                "INSERT INTO observation_evidence_mutations VALUES(?1,?2,?3,?4,?5)",
                params![
                    tenant,
                    mutation_id,
                    request_hash,
                    json!(result).to_string(),
                    now_ms()
                ],
            )
            .map_err(db)?;
        }
        tx.commit().map_err(db)?;
        Ok(result)
    }
    pub(crate) fn evidence_detail(
        &self,
        tenant: &str,
        doc: &str,
        export: bool,
    ) -> Result<Value, String> {
        let conn = self.conn.lock().map_err(|_| "db_lock_failed")?;
        let record = read_record(&conn, tenant, doc)?.ok_or("observation_not_found")?;
        let revisions = revisions(&conn, tenant, doc)?;
        let mut batches = Vec::new();
        let mut receipts = Vec::new();
        let mut stmt=conn.prepare("SELECT payload_json,commitment_sha256,receipt_id FROM observation_evidence_batches WHERE tenant_id=?1 ORDER BY created_at_ms,receipt_id").map_err(db)?;
        let rows = stmt
            .query_map(params![tenant], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(db)?;
        for row in rows {
            let (raw, h, id) = row.map_err(db)?;
            let mut b: Value =
                serde_json::from_str(&raw).map_err(|_| "observation_batch_invalid")?;
            if !revisions.iter().any(|r| {
                b["revisionHashes"]
                    .as_array()
                    .is_some_and(|a| a.contains(&r["revisionHash"]))
            }) {
                continue;
            }
            b["commitmentSha256"] = json!(h);
            batches.push(b);
            let receipt=conn.query_row("SELECT payload_json FROM observation_evidence_receipts WHERE tenant_id=?1 AND receipt_id=?2",params![tenant,id],|r|r.get::<_,String>(0)).optional().map_err(db)?;
            if let Some(raw) = receipt {
                receipts.push(
                    serde_json::from_str::<Value>(&raw)
                        .map_err(|_| "observation_receipt_invalid")?,
                );
            }
        }
        let mut verification = verify_record(&conn, &self.data_dir, tenant, doc)?;
        let mut out = json!({"ok":true,"record":record,"revisions":revisions,"heads":heads(&revisions),"batches":batches,"receipts":receipts});
        if export {
            use base64::Engine;
            let mut media = Vec::new();
            let mut seen = HashSet::new();
            for rev in &revisions {
                for p in rev["photos"].as_array().into_iter().flatten() {
                    let id = p["mediaId"].as_str().unwrap_or_default();
                    if !seen.insert(id.to_string()) {
                        continue;
                    }
                    let mut m = p.clone();
                    let relative=conn.query_row("SELECT local_path FROM board_media_files WHERE tenant_id=?1 AND media_id=?2",params![tenant,id],|r|r.get::<_,String>(0)).optional().map_err(db)?;
                    match relative.and_then(|path| fs::read(self.data_dir.join(path)).ok()) {
                        Some(bytes) => {
                            m["dataBase64"] =
                                json!(base64::engine::general_purpose::STANDARD.encode(bytes))
                        }
                        None => m["missing"] = json!(true),
                    }
                    media.push(m);
                }
            }
            out["media"] = json!(media);
        }
        drop(stmt);
        drop(conn);
        verification["scope"] = json!("local_integrity");
        verification["serverReceiptTrust"] = json!(if receipts.is_empty() {
            "not_applicable"
        } else {
            "not_checked"
        });
        if !receipts.is_empty() {
            match crate::observation_evidence_receipts::trusted_keys() {
                Ok(keys) => {
                    verification["serverReceiptTrust"] = json!("verified");
                    for receipt in &receipts {
                        if crate::observation_evidence_receipts::verify(
                            receipt,
                            tenant,
                            receipt["payload"]["commitmentSha256"]
                                .as_str()
                                .unwrap_or_default(),
                            &keys,
                        )
                        .is_err()
                        {
                            verification["serverReceiptTrust"] = json!("invalid");
                            verification["errors"]
                                .as_array_mut()
                                .unwrap()
                                .push(json!("receipt_key_untrusted"));
                        }
                    }
                }
                Err(_) => verification["serverReceiptTrust"] = json!("unavailable"),
            }
        }
        verification["valid"] = json!(verification["errors"].as_array().unwrap().is_empty());
        out["verification"] = verification;
        Ok(out)
    }
    pub(crate) fn evidence_outbox(&self, tenant: &str) -> Result<Value, String> {
        let conn = self.conn.lock().map_err(|_| "db_lock_failed")?;
        let mut stmt=conn.prepare("SELECT b.receipt_id,b.commitment_sha256 FROM observation_evidence_batches b LEFT JOIN observation_evidence_receipts r ON r.tenant_id=b.tenant_id AND r.receipt_id=b.receipt_id WHERE b.tenant_id=?1 AND r.receipt_id IS NULL ORDER BY b.created_at_ms LIMIT 100").map_err(db)?;
        let entries=stmt.query_map(params![tenant],|r|Ok(json!({"receiptId":r.get::<_,String>(0)?,"commitmentSha256":r.get::<_,String>(1)?}))).map_err(db)?.collect::<Result<Vec<_>,_>>().map_err(db)?;
        Ok(json!({"ok":true,"entries":entries}))
    }
    pub(crate) fn evidence_export_audit(
        &self,
        tenant: &str,
        input: &Value,
    ) -> Result<Value, String> {
        let detail = self.evidence_detail(
            tenant,
            input["docId"].as_str().ok_or("doc_id_required")?,
            false,
        )?;
        if !detail["revisions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["revisionId"] == input["revisionId"])
        {
            return Err("observation_revision_conflict".into());
        }
        let digest = input["exportSha256"]
            .as_str()
            .ok_or("observation_export_hash_required")?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("observation_export_hash_invalid".into());
        }
        let id = uuid();
        let now = now_ms();
        let audit = json!({"exportId":id,"tenantId":tenant,"docId":input["docId"],"revisionId":input["revisionId"],"exportSha256":digest,"createdAtMs":now,"source":"local_client_export"});
        let conn = self.conn.lock().map_err(|_| "db_lock_failed")?;
        conn.execute(
            "INSERT INTO observation_evidence_exports VALUES(?1,?2,?3,?4)",
            params![tenant, id, audit.to_string(), now],
        )
        .map_err(db)?;
        Ok(json!({"ok":true,"export":audit}))
    }
    pub(crate) fn evidence_resolve(&self, tenant: &str, input: &Value) -> Result<Value, String> {
        let doc = input["docId"].as_str().ok_or("doc_id_required")?;
        let reason = input["correctionReason"]
            .as_str()
            .unwrap_or_default()
            .trim();
        if reason.is_empty() {
            return Err("observation_correction_reason_required".into());
        }
        let mutation = input["mutationId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("observation_mutation_id_required")?;
        let mut authoritative_input = input.clone();
        authoritative_input["tenantId"] = json!(tenant);
        let request_hash = hash(&authoritative_input);
        let mut conn = self.conn.lock().map_err(|_| "db_lock_failed")?;
        let tx = conn.transaction().map_err(db)?;
        let prior=tx.query_row("SELECT request_hash,payload_json FROM observation_evidence_mutations WHERE tenant_id=?1 AND mutation_id=?2",params![tenant,mutation],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional().map_err(db)?;
        if let Some((h, raw)) = prior {
            if h != request_hash {
                return Err("observation_mutation_conflict".into());
            }
            let rows: Vec<Value> =
                serde_json::from_str(&raw).map_err(|_| "observation_evidence_invalid_mutation")?;
            return Ok(
                json!({"ok":true,"record":rows.first().ok_or("observation_evidence_invalid_mutation")?}),
            );
        }
        let all = revisions(&tx, tenant, doc)?;
        let mut heads = heads(&all);
        heads.sort_by(|a, b| a["revisionId"].as_str().cmp(&b["revisionId"].as_str()));
        let mut expected = input["expectedHeadIds"]
            .as_array()
            .ok_or("observation_revision_conflict")?
            .clone();
        expected.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
        if heads.len() < 2
            || expected
                != heads
                    .iter()
                    .map(|h| h["revisionId"].clone())
                    .collect::<Vec<_>>()
        {
            return Err("observation_revision_conflict".into());
        }
        let verification = verify_record(&tx, &self.data_dir, tenant, doc)?;
        if verification["errors"].as_array().unwrap().iter().any(|e| {
            !matches!(
                e.as_str(),
                Some(
                    "revision_branch_conflict"
                        | "current_record_mismatch"
                        | "legacy_photo_unverified"
                        | "server_batches_missing_locally"
                )
            )
        }) {
            return Err("observation_evidence_integrity_mismatch".into());
        }
        let selected = heads
            .iter()
            .find(|h| h["revisionId"] == input["selectedRevisionId"])
            .ok_or("observation_revision_conflict")?;
        let mut record = selected["record"].clone();
        let now = now_ms();
        record["updatedAtMs"] = json!(now);
        record["updatedAtIso"] = json!(chrono::Utc::now().to_rfc3339());
        let mut previous = record.clone();
        previous["revisionId"] = selected["revisionId"].clone();
        previous["revisionHash"] = selected["revisionHash"].clone();
        let record = append(
            &tx,
            &self.data_dir,
            record,
            Some(&previous),
            "resolve",
            reason,
            now,
            Some(&heads),
        )?;
        batch(
            &tx,
            tenant,
            &[record["revisionHash"].as_str().unwrap().to_string()],
        )?;
        let response = json!({"ok":true,"record":record});
        tx.execute(
            "INSERT INTO observation_evidence_mutations VALUES(?1,?2,?3,?4,?5)",
            params![
                tenant,
                mutation,
                request_hash,
                json!([record]).to_string(),
                now
            ],
        )
        .map_err(db)?;
        tx.commit().map_err(db)?;
        Ok(response)
    }
}
fn parent_ids(revision: &Value) -> Vec<Value> {
    if revision["eventKind"] == "resolve" {
        revision["parentRevisionIds"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    } else if revision["previousRevisionId"].is_null() {
        vec![]
    } else {
        vec![revision["previousRevisionId"].clone()]
    }
}
fn heads(all: &[Value]) -> Vec<Value> {
    let parents = all.iter().flat_map(parent_ids).collect::<Vec<_>>();
    all.iter()
        .filter(|r| !parents.contains(&r["revisionId"]))
        .cloned()
        .collect()
}
fn revisions(conn: &Connection, tenant: &str, doc: &str) -> Result<Vec<Value>, String> {
    let mut stmt=conn.prepare("SELECT payload_json,revision_hash FROM observation_evidence_revisions WHERE tenant_id=?1 AND doc_id=?2 ORDER BY saved_at_ms,revision_id").map_err(db)?;
    let rows = stmt
        .query_map(params![tenant, doc], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(db)?;
    let mut out = Vec::new();
    for row in rows {
        let (raw, h) = row.map_err(db)?;
        let mut v: Value =
            serde_json::from_str(&raw).map_err(|_| "observation_revision_invalid")?;
        v["revisionHash"] = json!(h);
        out.push(v);
    }
    Ok(out)
}
fn verify_record(conn: &Connection, dir: &Path, tenant: &str, doc: &str) -> Result<Value, String> {
    let all = revisions(conn, tenant, doc)?;
    let mut errors = Vec::new();
    let gap:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM observation_evidence_reconciliation WHERE tenant_id=?1 AND json_array_length(payload_json,'$.missing')>0)",params![tenant],|r|r.get(0)).map_err(db)?;
    if gap {
        errors.push("server_batches_missing_locally");
    }
    let mut covered = HashSet::new();
    let mut statement=conn.prepare("SELECT b.payload_json,b.commitment_sha256,b.receipt_id,r.payload_json FROM observation_evidence_batches b LEFT JOIN observation_evidence_receipts r ON r.tenant_id=b.tenant_id AND r.receipt_id=b.receipt_id WHERE b.tenant_id=?1").map_err(db)?;
    let rows = statement
        .query_map(params![tenant], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(db)?;
    for row in rows {
        let (raw, commitment, id, receipt) = row.map_err(db)?;
        let batch: Value = serde_json::from_str(&raw).map_err(|_| "observation_batch_invalid")?;
        let relevant = batch["revisionHashes"]
            .as_array()
            .is_some_and(|hashes| all.iter().any(|r| hashes.contains(&r["revisionHash"])));
        if !relevant {
            continue;
        }
        if hash(&batch) != commitment || batch["receiptId"] != id {
            errors.push("batch_hash_mismatch");
        }
        for h in batch["revisionHashes"].as_array().into_iter().flatten() {
            if let Some(h) = h.as_str() {
                covered.insert(h.to_string());
            }
        }
        if let Some(raw) = receipt {
            match serde_json::from_str::<Value>(&raw) {
                Ok(receipt) => {
                    let embedded = json!([{"keyId":receipt["keyId"],"algorithm":receipt["algorithm"],"publicKeyJwk":receipt["publicKeyJwk"]}]);
                    if receipt["payload"]["receiptId"] != id
                        || crate::observation_evidence_receipts::verify(
                            &receipt,
                            tenant,
                            &commitment,
                            &embedded,
                        )
                        .is_err()
                    {
                        errors.push("receipt_invalid");
                    }
                }
                Err(_) => errors.push("receipt_invalid"),
            }
        }
    }
    if all
        .iter()
        .any(|r| !covered.contains(r["revisionHash"].as_str().unwrap_or_default()))
    {
        errors.push("batch_missing");
    }
    for revision in &all {
        let mut raw = revision.clone();
        raw.as_object_mut().unwrap().remove("revisionHash");
        if hash(&raw) != revision["revisionHash"].as_str().unwrap_or_default() {
            errors.push("revision_hash_mismatch");
        }
        for (i, parent) in parent_ids(revision).iter().enumerate() {
            let expected_hash = if revision["eventKind"] == "resolve" {
                &revision["parentRevisionHashes"][i]
            } else {
                &revision["previousRevisionHash"]
            };
            if !all
                .iter()
                .any(|r| r["revisionId"] == *parent && r["revisionHash"] == *expected_hash)
            {
                errors.push("revision_parent_missing_or_changed");
            }
        }
        for photo in revision["photos"].as_array().into_iter().flatten() {
            if photo["missing"] == true {
                errors.push("legacy_photo_unverified");
                continue;
            }
            match photos(
                conn,
                dir,
                tenant,
                &json!({"photoMediaIds":[photo["mediaId"]]}),
            ) {
                Ok(actual) if actual.first() == Some(photo) => {}
                _ => errors.push("photo_hash_mismatch"),
            }
        }
    }
    let heads = heads(&all);
    if heads.len() != 1 {
        errors.push("revision_branch_conflict");
    }
    if let Some(record) = read_record(conn, tenant, doc)? {
        if let Some(head) = heads.first() {
            let mut expected = head["record"].clone();
            expected["evidenceVersion"] = json!(1);
            expected["revisionId"] = head["revisionId"].clone();
            expected["revisionHash"] = head["revisionHash"].clone();
            if record != expected {
                errors.push("current_record_mismatch");
            }
        }
    } else {
        errors.push("current_record_missing");
    }
    Ok(json!({"valid":errors.is_empty(),"errors":errors}))
}

/// A restore may add history, but cannot replace known provenance or select an older head.
pub(crate) fn check_restore(conn: &Connection, tenant: &str) -> Result<(), String> {
    for (table, key) in [
        ("observation_evidence_revisions", "revision_id"),
        ("observation_evidence_batches", "receipt_id"),
        ("observation_evidence_mutations", "mutation_id"),
        ("observation_evidence_receipts", "receipt_id"),
        ("observation_evidence_exports", "export_id"),
    ] {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM restore.sqlite_master WHERE type='table' AND name=?1)",
                params![table],
                |r| r.get(0),
            )
            .map_err(db)?;
        if !exists {
            continue;
        }
        let sql=format!("SELECT EXISTS(SELECT 1 FROM main.{table} m JOIN restore.{table} r ON r.tenant_id=m.tenant_id AND r.{key}=m.{key} WHERE m.tenant_id=?1 AND m.payload_json<>r.payload_json)");
        if conn
            .query_row(&sql, params![tenant], |r| r.get::<_, bool>(0))
            .map_err(db)?
        {
            return Err("observation_evidence_restore_conflict".into());
        }
    }
    let exists:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM restore.sqlite_master WHERE type='table' AND name='observation_evidence_revisions')",[],|r|r.get(0)).map_err(db)?;
    if exists {
        let mut stmt=conn.prepare("SELECT payload_json,revision_hash FROM restore.observation_evidence_revisions WHERE tenant_id=?1").map_err(db)?;
        let rows = stmt
            .query_map(params![tenant], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(db)?;
        for row in rows {
            let (raw, digest) = row.map_err(db)?;
            let v: Value =
                serde_json::from_str(&raw).map_err(|_| "observation_revision_invalid")?;
            if hash(&v) != digest || v["tenantId"] != tenant {
                return Err("observation_evidence_restore_conflict".into());
            }
        }
        let conflict:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM main.observation_evidence_revisions m JOIN restore.observation_evidence_revisions r ON r.tenant_id=m.tenant_id AND r.revision_id=m.revision_id WHERE m.tenant_id=?1 AND (m.payload_json<>r.payload_json OR m.revision_hash<>r.revision_hash))",params![tenant],|r|r.get(0)).map_err(db)?;
        if conflict {
            return Err("observation_evidence_restore_conflict".into());
        }
        let mut statement=conn.prepare("SELECT payload_json FROM restore.lesson_observations WHERE tenant_id=?1 AND json_extract(payload_json,'$.evidenceVersion')=1").map_err(db)?;
        let rows = statement
            .query_map(params![tenant], |r| r.get::<_, String>(0))
            .map_err(db)?;
        for raw in rows {
            let mut record: Value = serde_json::from_str(&raw.map_err(db)?)
                .map_err(|_| "observation_evidence_restore_invalid")?;
            let revision=conn.query_row("SELECT payload_json,revision_hash FROM restore.observation_evidence_revisions WHERE tenant_id=?1 AND revision_id=?2",params![tenant,record["revisionId"].as_str().unwrap_or_default()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional().map_err(db)?.ok_or("observation_evidence_restore_invalid")?;
            let envelope: Value = serde_json::from_str(&revision.0)
                .map_err(|_| "observation_evidence_restore_invalid")?;
            if record["revisionHash"] != revision.1 {
                return Err("observation_evidence_restore_invalid".into());
            }
            for key in ["evidenceVersion", "revisionId", "revisionHash"] {
                record.as_object_mut().unwrap().remove(key);
            }
            if record != envelope["record"] {
                return Err("observation_evidence_restore_invalid".into());
            }
        }
    }
    Ok(())
}

pub(crate) fn observation_merge_guard() -> String {
    "((json_extract(main.lesson_observations.payload_json,'$.revisionId') IS NULL AND excluded.updated_at_ms>=main.lesson_observations.updated_at_ms) OR
      json_extract(excluded.payload_json,'$.revisionHash')=json_extract(main.lesson_observations.payload_json,'$.revisionHash') OR
      EXISTS(WITH RECURSIVE ancestors(id) AS (
        SELECT json_extract(excluded.payload_json,'$.revisionId') UNION
        SELECT p.value FROM ancestors a JOIN observation_evidence_revisions r ON r.tenant_id=excluded.tenant_id AND r.revision_id=a.id
        JOIN json_each(CASE WHEN json_extract(r.payload_json,'$.eventKind')='resolve' THEN json_extract(r.payload_json,'$.parentRevisionIds') ELSE json_array(json_extract(r.payload_json,'$.previousRevisionId')) END) p WHERE p.value IS NOT NULL
      ) SELECT 1 FROM ancestors WHERE id=json_extract(main.lesson_observations.payload_json,'$.revisionId')))".to_string()
}
