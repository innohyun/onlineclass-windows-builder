use crate::SqliteStore;
use sha2::{Digest, Sha256};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
const INVALID: &str = "INVALID_LOCAL_READ_REQUEST";
const CONFLICT: &str = "student_record_draft_revision_conflict";
fn decode(raw: String) -> Result<Value,String> { serde_json::from_str(&raw).map_err(|_| "MCP_LOCAL_RESULT_INVALID".into()) }
fn scoped(draft: &Value, set: &Value, scope: &Value) -> bool {
    ["schoolYear","semester","fromDate","toDate"].iter().all(|key| {
        draft.get(*key).or_else(||draft.get("scope").and_then(|s|s.get(*key)))
            .or_else(||set.get(*key)).or_else(||set.get("scope").and_then(|s|s.get(*key))) == scope.get(*key)
    })
}
pub(crate) fn read(store:&SqliteStore,tenant:&str,input:&Value)->Result<Value,String> {
    let scope=&input["scope"];
    let codes=input["studentCodes"].as_array().filter(|rows| !rows.is_empty() && rows.len()<=500).ok_or(INVALID)?;
    let limit=input["limit"].as_u64().filter(|n| *n>0 && *n<=1000).ok_or(INVALID)? as usize;
    let offset=input["offset"].as_u64().filter(|n| *n<=10000).ok_or(INVALID)? as usize;
    if !matches!(scope["semester"].as_u64(),Some(1|2)) || scope["schoolYear"].as_u64().is_none()
        || ["fromDate","toDate"].iter().any(|key| scope[*key].as_str().is_none()) {return Err(INVALID.into());}
    let conn=store.conn.lock().map_err(|_| "db_lock_failed")?;
    let mut statement=conn.prepare("SELECT d.payload_json,s.payload_json FROM student_record_drafts d JOIN student_record_draft_sets s ON s.tenant_id=d.tenant_id AND s.draft_set_id=d.draft_set_id WHERE d.tenant_id=?1 AND d.student_code IN (SELECT value FROM json_each(?2)) AND (?3 IS NULL OR d.draft_id=?3) ORDER BY d.draft_id LIMIT 10001").map_err(|_| INVALID)?;
    let rows=statement.query_map(params![tenant,serde_json::to_string(codes).map_err(|_| INVALID)?,input["draftId"].as_str()],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?))).map_err(|_| INVALID)?;
    let mut drafts=Vec::new();let mut sets=Vec::new();let mut scanned=0;
    for row in rows {scanned+=1;let (draft,set)=row.map_err(|_| INVALID)?;let mut draft=decode(draft)?;let set=decode(set)?;
        if !scoped(&draft,&set,scope) {continue;}
        for key in ["schoolYear","semester","fromDate","toDate"] {draft[key]=scope[key].clone();}
        if !sets.iter().any(|prior:&Value|prior["draftSetId"]==set["draftSetId"]) {sets.push(set);}
        drafts.push(draft);
    }
    if scanned>10000 {return Err("MCP_DRAFT_LIST_INCOMPLETE".into());}
    let snapshot=format!("{:x}",Sha256::digest(serde_json::to_vec(&json!(drafts.iter().map(|row|json!([row["draftId"],row["updatedAtMs"]])).collect::<Vec<_>>())).map_err(|_| INVALID)?));
    let complete=offset+limit>=drafts.len();let page=drafts.into_iter().skip(offset).take(limit).collect::<Vec<_>>();
    sets.retain(|set|page.iter().any(|row|row["draftSetId"]==set["draftSetId"]));
    Ok(json!({"drafts":page,"sets":sets,"complete":complete,"nextOffset":if complete{Value::Null}else{json!(offset+limit)},"snapshot":snapshot}))
}
pub(crate) fn save(conn:&Connection,tenant:&str,data:&Value)->Result<Value,String> {
    let mut draft=data["draft"].clone();
    let id=draft["draftId"].as_str().filter(|v| !v.is_empty()).ok_or(CONFLICT)?.to_string();
    let code=draft["studentCode"].as_str().ok_or(CONFLICT)?.to_string();
    let expected=data["expectedRevision"].as_i64().filter(|v| *v>=0).ok_or(CONFLICT)?;
    if !["draft","reviewed"].contains(&draft["status"].as_str().unwrap_or("")) {return Err(CONFLICT.into());}
    for source in data["sourceDraftSnapshots"].as_array().ok_or(CONFLICT)? {
        let prior:Option<(String,i64)>=conn.query_row("SELECT student_code,updated_at_ms FROM student_record_drafts WHERE tenant_id=?1 AND draft_id=?2 AND draft_set_id=?3",params![tenant,source["draftId"].as_str(),source["draftSetId"].as_str()],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(|_| CONFLICT)?;
        if !prior.is_some_and(|(student,revision)|student==code && Some(revision)==source["revision"].as_i64()) {return Err("MCP_SOURCE_CHANGED".into());}
    }
    if let Some(set)=data.get("draftSet") {
        if expected!=0 || draft["finalRecordKind"]!="creative_activity" || data["sourceDraftSnapshots"].as_array().is_none_or(|rows|rows.is_empty()) {return Err(CONFLICT.into());}
        let exists:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM student_record_draft_sets WHERE tenant_id=?1 AND draft_set_id=?2)",params![tenant,set["draftSetId"].as_str()],|row|row.get(0)).map_err(|_| CONFLICT)?;
        if exists || set["draftSetId"]!=draft["draftSetId"] {return Err(CONFLICT.into());}
        let mut set=set.clone();set["tenantId"]=json!(tenant);
        crate::canonical_write_transactions::upsert_student_record_draft_set(conn,set)?;
    }
    draft["tenantId"]=json!(tenant);draft["expectedRevision"]=json!(expected);
    crate::canonical_write_transactions::upsert_student_record_draft(conn,draft)?;
    verify(conn,tenant,data)?;
    Ok(json!({"result":data,"localRef":format!("student-record-draft:{id}")}))
}
pub(crate) fn verify(conn:&Connection,tenant:&str,data:&Value)->Result<(),String> {
    let raw:String=conn.query_row("SELECT payload_json FROM student_record_drafts WHERE tenant_id=?1 AND draft_id=?2",params![tenant,data["draft"]["draftId"].as_str()],|row|row.get(0)).map_err(|_| "LOCAL_STORE_WRITE_FAILED")?;
    let actual=decode(raw)?;
    for (key,value) in data["draft"].as_object().ok_or("LOCAL_STORE_WRITE_FAILED")? {
        if !["id","docId","tenantId","updatedAt","updatedAtMs","updatedAtIso","revisionProtected","history"].contains(&key.as_str()) && actual[key]!=*value {return Err("LOCAL_STORE_WRITE_FAILED".into());}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mcp_record_edit_exact_cas_history_and_receipt_replay() {
        let root=std::env::temp_dir().join(format!("mcp-record-edit-{}",crate::random_url_token()));
        std::fs::create_dir_all(&root).unwrap();
        let store=SqliteStore::open(root.join("store.sqlite3")).unwrap();
        let scope=json!({"schoolYear":2026,"semester":2,"fromDate":"2026-09-01","toDate":"2026-09-26"});
        let mut set=scope.clone();set["tenantId"]=json!("tenant-a");set["draftSetId"]=json!("set-a");set["recordTypes"]=json!(["behavior"]);set["status"]=json!("draft");
        let mut draft=scope.clone();draft["tenantId"]=json!("tenant-a");draft["draftSetId"]=json!("set-a");draft["draftId"]=json!("draft-a");draft["studentCode"]=json!("S01");draft["recordType"]=json!("behavior");draft["behaviorComment"]=json!("참여함.");draft["status"]=json!("draft");draft["expectedRevision"]=json!(0);
        {let conn=store.conn.lock().unwrap();crate::canonical_write_transactions::upsert_student_record_draft_set(&conn,set).unwrap();crate::canonical_write_transactions::upsert_student_record_draft(&conn,draft).unwrap();}
        let input=json!({"scope":scope,"studentCodes":["S01"],"offset":0,"limit":20});
        let before=read(&store,"tenant-a",&input).unwrap();let mut draft=before["drafts"][0].clone();let revision=draft["updatedAtMs"].clone();draft["status"]=json!("reviewed");
        let data=json!({"draft":draft,"expectedRevision":revision,"sourceDraftSnapshots":[]});
        let request=json!({"tenantId":"tenant-a","receiptId":"audit-receipt-a","operation":"student_record_edit","requestSha256":"a".repeat(64),"data":data});
        crate::classaimate_mcp_write_jobs::apply(&store,&request).unwrap();
        let replay=crate::classaimate_mcp_write_jobs::apply(&store,&request).unwrap();assert_eq!(replay["replayed"],true);
        let after=read(&store,"tenant-a",&input).unwrap();assert_eq!(after["drafts"][0]["status"],"reviewed");assert_eq!(after["drafts"][0]["history"].as_array().unwrap().len(),1);
        let mut stale=request.clone();stale["receiptId"]=json!("audit-stale");assert!(crate::classaimate_mcp_write_jobs::apply(&store,&stale).is_err());
        drop(store);std::fs::remove_dir_all(root).unwrap();
    }
}
