use crate::{work_note_documents, SqliteStore};
use rusqlite::{params, Connection, TransactionBehavior};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

pub(crate) fn is_folder(page: &Value) -> bool {
    page.pointer("/properties/nodeKind").and_then(Value::as_str) == Some("folder")
        || matches!(page.pointer("/properties/systemKind").and_then(Value::as_str),
            Some("mobile_work_meeting_folder" | "work_reference_materials_folder" | "lesson_materials_folder"
                | "student_learning_materials_folder" | "lesson_plan_date_projection"))
        || matches!(page["pageId"].as_str(), Some("classaimate:work-meeting-minutes" |
            "classaimate:work-reference-materials" | "lesson-materials-root" | "student-learning-materials-root"))
}

pub(crate) fn validate_write(conn: &Connection, tenant: &str, input: &Value, existing: Option<&Value>) -> Result<(), String> {
    let kind = input.pointer("/properties/nodeKind").and_then(Value::as_str);
    if kind.is_some_and(|v| !matches!(v, "folder" | "note")) { return Err("work_note_node_kind_invalid".into()); }
    if let Some(old) = existing {
        if old.pointer("/properties/nodeKind").is_some() && is_folder(old) != is_folder(input) {
            return Err("work_note_node_kind_immutable".into());
        }
    }
    if kind == Some("folder") && (!input["markdown"].as_str().unwrap_or("").is_empty()
        || input["blocks"].as_array().is_some_and(|v| !v.is_empty())) {
        return Err("work_note_folder_body_forbidden".into());
    }
    if let Some(parent_id) = input["parentId"].as_str() {
        if let Some(parent) = work_note_documents::read(conn, tenant, parent_id)? {
            // Legacy records are converted by the explicit organizer; typed notes are always leaves.
            if parent.pointer("/properties/nodeKind").is_some() && !is_folder(&parent) {
                return Err("work_note_parent_not_folder".into());
            }
        }
    }
    Ok(())
}

fn read_all(conn: &Connection, tenant: &str) -> Result<Vec<Value>, String> {
    let mut stmt = conn.prepare("SELECT page_id FROM work_note_pages WHERE tenant_id=?1 ORDER BY page_id").map_err(|e|e.to_string())?;
    let ids = stmt.query_map([tenant], |row| row.get::<_, String>(0)).map_err(|e|e.to_string())?
        .collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
    ids.iter().map(|id| work_note_documents::read(conn, tenant, id)?.ok_or("work_note_not_found".into()))
        .filter(|page| !page.as_ref().is_ok_and(work_note_documents::is_trashed)).collect()
}

fn new_id(tenant: &str, id: &str, kind: &str) -> String {
    format!("wn-{kind}-{:x}", Sha256::digest(format!("{tenant}:{id}:{kind}")))
}

// One transaction covers snapshot validation, wrapper creation, reparenting and canonical readback.
pub(crate) fn organize(store: &SqliteStore, input: Value) -> Result<Value, String> {
    let tenant = crate::normalize_tenant_id(input.get("tenantId"));
    if tenant.is_empty() { return Err("tenant_id_required".into()); }
    let expected = input["expectedPages"].as_array().ok_or("work_note_expected_revision_required")?;
    let mut conn = store.conn.lock().map_err(|_|"db_lock_failed")?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e|e.to_string())?;
    let original = read_all(&tx, &tenant)?;
    let parents: HashSet<String> = original.iter().filter_map(|p|p["parentId"].as_str().map(str::to_string)).collect();
    let needs_change = original.iter().any(|p| p.pointer("/properties/nodeKind").is_none()
        || (!is_folder(p) && parents.contains(p["pageId"].as_str().unwrap_or(""))));
    if !needs_change { return Ok(json!({"ok":true,"records":original,"changed":false})); }
    let revisions: HashMap<&str,i64> = expected.iter().filter_map(|p|Some((p["pageId"].as_str()?,p["revision"].as_i64()?))).collect();
    if revisions.len()!=original.len() || original.iter().any(|p|revisions.get(p["pageId"].as_str().unwrap_or("")) != Some(&work_note_documents::revision(p))) {
        return Err("work_note_revision_conflict".into());
    }
    let now = crate::now_ms().max(original.iter().map(work_note_documents::revision).max().unwrap_or(0)+1);
    let mut pages = original.clone();
    let mut added = Vec::new();
    let mut wrappers = HashMap::new();
    let mut attachment_moves = Vec::new();
    for page in &original {
        let id = page["pageId"].as_str().ok_or("work_note_page_id_required")?;
        if !is_folder(page) && parents.contains(id) {
            let folder_id = new_id(&tenant,id,"folder");
            wrappers.insert(id.to_string(),folder_id.clone());
            added.push(json!({"tenantId":tenant,"pageId":folder_id,"parentId":page["parentId"],
                "title":page["title"],"emoji":"📁","position":page["position"],
                "properties":{"nodeKind":"folder","folderSourcePageId":id},"blocks":[],"markdown":"",
                "createdAtMs":now,"updatedAtMs":now}));
        }
        if is_folder(page) && page.pointer("/properties/nodeKind").and_then(Value::as_str)!=Some("folder")
            && (page["blocks"].as_array().is_some_and(|b|!b.is_empty()) || !page["markdown"].as_str().unwrap_or("").is_empty()) {
            let body_id = new_id(&tenant,id,"body");
            let mut body=page.clone();
            body["pageId"]=json!(body_id); body["parentId"]=json!(id);
            body["title"]=json!(format!("{} 안내",page["title"].as_str().unwrap_or("폴더")));
            body["emoji"]=json!("📄"); body["position"]=json!(0);
            body["properties"]=json!({"nodeKind":"note","folderSourcePageId":id});
            body["createdAtMs"]=json!(now);body["updatedAtMs"]=json!(now);
            attachment_moves.push((id.to_string(),body_id));added.push(body);
        }
    }
    for page in pages.iter_mut().chain(added.iter_mut()) {
        if let Some(parent)=page["parentId"].as_str().and_then(|id|wrappers.get(id)) { page["parentId"]=json!(parent); }
    }
    for page in &mut pages {
        page["properties"]["nodeKind"]=json!(if is_folder(page){"folder"}else{"note"});
        if let Some(parent)=page["pageId"].as_str().and_then(|id|wrappers.get(id)) { page["parentId"]=json!(parent);page["position"]=json!(0); }
    }
    for page in &added {
        let id=page["pageId"].as_str().unwrap();
        if work_note_documents::read(&tx,&tenant,id)?.is_some() {return Err("work_note_folder_id_conflict".into());}
        tx.execute("INSERT INTO work_note_pages(tenant_id,page_id,parent_id,title,emoji,position,properties_json,document_json,markdown,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)",
            params![tenant,id,page["parentId"].as_str(),page["title"].as_str(),page["emoji"].as_str(),page["position"].as_i64().unwrap_or(0),
                page["properties"].to_string(),page["blocks"].to_string(),page["markdown"].as_str().unwrap_or(""),now]).map_err(|e|e.to_string())?;
        tx.execute("INSERT INTO work_note_pages_fts(tenant_id,page_id,title,markdown) VALUES(?1,?2,?3,?4)",
            params![tenant,id,page["title"].as_str(),page["markdown"].as_str().unwrap_or("")]).map_err(|e|e.to_string())?;
    }
    for (page,old) in pages.iter().zip(original.iter()) {
        if page==old {continue;}
        tx.execute("UPDATE work_note_pages SET parent_id=?1,position=?2,properties_json=?3,updated_at_ms=?4 WHERE tenant_id=?5 AND page_id=?6",
            params![page["parentId"].as_str(),page["position"].as_i64().unwrap_or(0),page["properties"].to_string(),now,tenant,page["pageId"].as_str()]).map_err(|e|e.to_string())?;
    }
    for (from,to) in attachment_moves {
        tx.execute("UPDATE work_note_attachments SET page_id=?1 WHERE tenant_id=?2 AND page_id=?3",params![to,tenant,from]).map_err(|e|e.to_string())?;
    }
    let records=read_all(&tx,&tenant)?;
    tx.commit().map_err(|e|e.to_string())?;
    Ok(json!({"ok":true,"records":records,"changed":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn work_note_folders_preserve_content_and_reject_stale_migration() {
        let path=std::env::temp_dir().join(format!("work-note-folders-{}",crate::random_url_token()));
        std::fs::create_dir_all(&path).unwrap();
        {
            let store=SqliteStore::open(path.join("fixture.sqlite")).unwrap();
            let parent=store.upsert_work_note(json!({"tenantId":"synthetic","pageId":"parent","title":"본문 폴더",
                "parentId":null,"properties":{},"blocks":[{"id":"a","type":"text","text":"보존 worknote://child"}],"markdown":"보존 원문"})).unwrap();
            let child=store.upsert_work_note(json!({"tenantId":"synthetic","pageId":"child","parentId":"parent","title":"자식",
                "properties":{},"blocks":[],"markdown":"child"})).unwrap();
            assert_eq!(organize(&store,json!({"tenantId":"synthetic","expectedPages":[]})).unwrap_err(),"work_note_revision_conflict");
            let snapshot=json!({"tenantId":"synthetic","expectedPages":[{"pageId":"parent","revision":parent["updatedAtMs"]},{"pageId":"child","revision":child["updatedAtMs"]}]});
            let result=organize(&store,snapshot.clone()).unwrap();
            assert_eq!(result["records"].as_array().unwrap().len(),3);
            let saved=store.get_work_note("synthetic".into(),"parent".into()).unwrap().unwrap();
            assert_eq!(saved["blocks"],parent["blocks"]);assert_eq!(saved["markdown"],parent["markdown"]);
            assert_eq!(saved["parentId"],store.get_work_note("synthetic".into(),"child".into()).unwrap().unwrap()["parentId"]);
            assert_eq!(organize(&store,snapshot).unwrap()["changed"],false);
            let conn=store.conn.lock().unwrap();
            assert_eq!(validate_write(&conn,"synthetic",&json!({"pageId":"bad","parentId":"parent","properties":{"nodeKind":"note"},"blocks":[],"markdown":""}),None).unwrap_err(),"work_note_parent_not_folder");
        }
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn work_note_folders_protection_is_an_explicit_403() {
        assert_eq!(crate::request_error_status("work_note_system_folder_protected"),403);
        assert_eq!(crate::request_error_status("work_note_folder_body_forbidden"),403);
        assert_eq!(crate::request_error_status("db_work_note_upsert_failed:disk"),500);
    }
}
