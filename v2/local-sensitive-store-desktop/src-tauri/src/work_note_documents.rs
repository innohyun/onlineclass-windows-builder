use crate::{normalize_id_segment, normalize_tenant_id, now_ms, SqliteStore};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

pub(crate) const RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;
pub(crate) fn ensure_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(r#"
        CREATE TABLE IF NOT EXISTS work_note_versions (
          tenant_id TEXT NOT NULL, version_id TEXT NOT NULL, page_id TEXT NOT NULL,
          payload_json TEXT NOT NULL, captured_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY(tenant_id, version_id));
        CREATE INDEX IF NOT EXISTS idx_work_note_versions_page ON work_note_versions(tenant_id,page_id,captured_at_ms DESC);
        CREATE INDEX IF NOT EXISTS idx_work_note_versions_cursor ON work_note_versions(tenant_id,page_id,captured_at_ms DESC,version_id DESC);
        CREATE TABLE IF NOT EXISTS work_note_local_drafts (
          tenant_id TEXT NOT NULL, page_id TEXT NOT NULL, generation INTEGER NOT NULL,
          payload_json TEXT NOT NULL, updated_at_ms INTEGER NOT NULL, PRIMARY KEY(tenant_id,page_id));
        CREATE TRIGGER IF NOT EXISTS work_note_capture_update BEFORE UPDATE ON work_note_pages
        WHEN (OLD.title<>NEW.title OR OLD.emoji<>NEW.emoji OR OLD.parent_id IS NOT NEW.parent_id
          OR OLD.position<>NEW.position OR OLD.properties_json<>NEW.properties_json
          OR OLD.document_json<>NEW.document_json OR OLD.markdown<>NEW.markdown)
          AND COALESCE((SELECT applying FROM local_store_device_sync_state WHERE tenant_id=OLD.tenant_id),0)=0
        BEGIN
          INSERT INTO work_note_versions VALUES(OLD.tenant_id,lower(hex(randomblob(16))),OLD.page_id,
            json_object('tenantId',OLD.tenant_id,'pageId',OLD.page_id,'parentId',OLD.parent_id,
              'title',OLD.title,'emoji',OLD.emoji,'position',OLD.position,'properties',json(OLD.properties_json),
              'blocks',json(OLD.document_json),'markdown',OLD.markdown,
              'createdAtMs',OLD.created_at_ms,'updatedAtMs',OLD.updated_at_ms),
            CAST(unixepoch('subsec')*1000 AS INTEGER),CAST(unixepoch('subsec')*1000 AS INTEGER));
        END;
    "#).map_err(|e| format!("work_note_documents_schema_failed:{e}"))
}

pub(crate) fn scope(input: &Value) -> Result<(String, String), String> {
    let tenant = normalize_tenant_id(input.get("tenantId"));
    let page = normalize_id_segment(input.get("pageId"), 180);
    if tenant.is_empty() {
        return Err("tenant_id_required".into());
    }
    if page.is_empty() {
        return Err("work_note_page_id_required".into());
    }
    Ok((tenant, page))
}

pub(crate) fn read(conn: &Connection, tenant: &str, page: &str) -> Result<Option<Value>, String> {
    conn.query_row("SELECT parent_id,title,emoji,position,properties_json,document_json,markdown,created_at_ms,updated_at_ms FROM work_note_pages WHERE tenant_id=?1 AND page_id=?2", params![tenant,page], |row| {
        let properties:String=row.get(4)?; let blocks:String=row.get(5)?;
        Ok(json!({"tenantId":tenant,"pageId":page,"parentId":row.get::<_,Option<String>>(0)?,
            "title":row.get::<_,String>(1)?,"emoji":row.get::<_,String>(2)?,"position":row.get::<_,i64>(3)?,
            "properties":serde_json::from_str::<Value>(&properties).unwrap_or(Value::Null),
            "blocks":serde_json::from_str::<Value>(&blocks).unwrap_or(Value::Null),"markdown":row.get::<_,String>(6)?,
            "createdAtMs":row.get::<_,i64>(7)?,"updatedAtMs":row.get::<_,i64>(8)?}))
    }).optional().map_err(|e|format!("work_note_document_read_failed:{e}"))
}

pub(crate) fn is_trashed(page: &Value) -> bool {
    page.pointer("/properties/_localTrash/deletedAtMs")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        > 0
}

pub(crate) fn revision(page: &Value) -> i64 {
    page["updatedAtMs"].as_i64().unwrap_or(0)
}

pub(crate) fn check_revision(input: &Value, page: Option<&Value>) -> Result<i64, String> {
    let expected = input
        .get("expectedRevision")
        .and_then(Value::as_i64)
        .filter(|v| *v >= 0)
        .ok_or("work_note_expected_revision_required")?;
    if page.map(revision).unwrap_or(0) != expected {
        return Err("work_note_revision_conflict".into());
    }
    Ok(expected)
}

pub(crate) fn validate_content(input: &Value) -> Result<(), String> {
    if input
        .get("title")
        .and_then(Value::as_str)
        .is_some_and(|s| s.chars().count() > 240)
        || input
            .get("emoji")
            .and_then(Value::as_str)
            .is_some_and(|s| s.chars().count() > 16)
    {
        return Err("work_note_metadata_too_large".into());
    }
    if !input.get("blocks").is_some_and(Value::is_array)
        || !input.get("properties").is_some_and(Value::is_object)
        || !input.get("markdown").is_some_and(Value::is_string)
    {
        return Err("work_note_document_invalid".into());
    }
    if input["markdown"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .count()
        > 2_000_000
        || input.to_string().len() > 16 * 1024 * 1024
    {
        return Err("work_note_document_too_large".into());
    }
    Ok(())
}

fn protected(conn: &Connection, tenant: &str, page: &Value, structure: bool) -> Result<(), String> {
    let id = page["pageId"].as_str().unwrap_or_default();
    if [
        crate::WORK_MEETING_ROOT_PAGE_ID,
        crate::WORK_REFERENCE_ROOT_PAGE_ID,
        crate::STUDENT_MATERIAL_ROOT_PAGE_ID,
    ]
    .contains(&id)
        || (structure && id == "root")
        || page
            .pointer("/properties/systemKind")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
    {
        return Err("work_note_system_folder_protected".into());
    }
    assert_local_editable(conn, tenant, page)?;
    if structure {
        let bound: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM lesson_plan_bindings WHERE tenant_id=?1 AND (page_id=?2 OR page_id=?3)",
                params![tenant, id, page.pointer("/properties/folderSourcePageId").and_then(Value::as_str).unwrap_or("")],
                |r| r.get(0),
            )
            .map_err(|e| format!("work_note_binding_read_failed:{e}"))?;
        if bound > 0 {
            return Err("lesson_plan_page_protected".into());
        }
    }
    Ok(())
}

pub(crate) fn assert_local_editable(
    conn: &Connection,
    tenant: &str,
    page: &Value,
) -> Result<(), String> {
    let mut cursor = Some(page.clone());
    let mut seen = HashSet::new();
    while let Some(current) = cursor {
        let id = current["pageId"].as_str().unwrap_or_default();
        if !seen.insert(id.to_string()) {
            return Err("work_note_parent_cycle".into());
        }
        if id == crate::STUDENT_MATERIAL_ROOT_PAGE_ID
            || current
                .pointer("/properties/systemKind")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.starts_with("student_learning_material"))
        {
            return Err("work_note_cloud_material_read_only".into());
        }
        cursor = current["parentId"]
            .as_str()
            .map(|parent| read(conn, tenant, parent))
            .transpose()?
            .flatten();
    }
    Ok(())
}

pub(crate) fn list_editable(
    store: &SqliteStore,
    tenant: String,
    query: String,
) -> Result<Vec<Value>, String> {
    let pages = store.list_work_notes(tenant.clone(), query)?;
    let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    let mut result = Vec::new();
    for page in pages {
        match assert_local_editable(&conn, &tenant, &page) {
            Ok(()) => result.push(page),
            Err(error) if error == "work_note_cloud_material_read_only" => {}
            Err(error) => return Err(error),
        }
    }
    Ok(result)
}

pub(crate) fn validate_parent(
    conn: &Connection,
    tenant: &str,
    page_id: &str,
    parent: Option<&str>,
) -> Result<(), String> {
    let mut cursor = parent.filter(|p| !p.is_empty()).map(str::to_string);
    let mut seen = HashSet::new();
    while let Some(id) = cursor {
        if id == page_id || !seen.insert(id.clone()) {
            return Err("work_note_parent_cycle".into());
        }
        let row = read(conn, tenant, &id)?.ok_or("work_note_parent_not_found")?;
        if Some(id.as_str()) == parent && row.pointer("/properties/nodeKind").is_some()
            && !crate::work_note_folders::is_folder(&row) {
            return Err("work_note_parent_not_folder".into());
        }
        if is_trashed(&row) {
            return Err("work_note_parent_trashed".into());
        }
        cursor = row["parentId"].as_str().map(str::to_string);
    }
    Ok(())
}

pub(crate) fn save(store: &SqliteStore, mut input: Value) -> Result<Value, String> {
    validate_content(&input)?;
    let (tenant, id) = scope(&input)?;
    let mut conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| format!("work_note_transaction_failed:{e}"))?;
    let old = read(&tx, &tenant, &id)?;
    let expected = check_revision(&input, old.as_ref())?;
    if let Some(page) = old.as_ref() {
        if is_trashed(page) {
            return Err("work_note_trashed".into());
        }
        protected(&tx, &tenant, page, false)?;
        // Desktop metadata never changes lesson binding identity or its canonical title/tree.
        if crate::lesson_plan_bindings::stored_page_structure(&tx, &tenant, &id)?.is_some() {
            for key in ["parentId", "title", "position"] {
                input[key] = page[key].clone();
            }
        }
        input["createdAtMs"] = page["createdAtMs"].clone();
    } else {
        protected(&tx, &tenant, &input, false)?;
        input["createdAtMs"] = json!(now_ms());
    }
    if input.pointer("/properties/_localTrash").is_some() {
        return Err("work_note_trash_metadata_protected".into());
    }
    validate_parent(&tx, &tenant, &id, input["parentId"].as_str())?;
    assert_local_editable(&tx, &tenant, &input)?;
    input["updatedAtMs"] = json!(now_ms().max(expected + 1));
    crate::canonical_write_transactions::upsert_work_note(&tx, input)?;
    let page = read(&tx, &tenant, &id)?.ok_or("work_note_readback_failed")?;
    tx.execute(
        "DELETE FROM work_note_versions WHERE tenant_id=?1 AND captured_at_ms<?2",
        params![tenant, now_ms() - RETENTION_MS],
    )
    .map_err(|e| format!("work_note_history_prune_failed:{e}"))?;
    tx.commit()
        .map_err(|e| format!("work_note_commit_failed:{e}"))?;
    Ok(json!({"ok":true,"revision":revision(&page),"page":page,"verified":true}))
}

fn all(conn: &Connection, tenant: &str) -> Result<Vec<Value>, String> {
    let mut stmt = conn
        .prepare("SELECT page_id FROM work_note_pages WHERE tenant_id=?1 ORDER BY position,page_id")
        .map_err(|e| e.to_string())?;
    let ids = stmt
        .query_map(params![tenant], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    ids.iter()
        .map(|id| read(conn, tenant, id)?.ok_or_else(|| "work_note_readback_failed".into()))
        .collect()
}

fn descendants(pages: &[Value], id: &str) -> HashSet<String> {
    let mut ids = HashSet::from([id.to_string()]);
    loop {
        let size = ids.len();
        for p in pages {
            if p["parentId"]
                .as_str()
                .is_some_and(|parent| ids.contains(parent))
            {
                ids.insert(p["pageId"].as_str().unwrap_or_default().into());
            }
        }
        if ids.len() == size {
            return ids;
        }
    }
}

fn persist(conn: &Connection, page: &Value) -> Result<Value, String> {
    // Administrative restore removes protected trash metadata inside this transaction only.
    let mut next = page.clone();
    next.as_object_mut().unwrap().remove("expectedRevision");
    crate::canonical_write_transactions::upsert_work_note(conn, next)?;
    read(
        conn,
        page["tenantId"].as_str().unwrap(),
        page["pageId"].as_str().unwrap(),
    )?
    .ok_or("work_note_readback_failed".into())
}

pub(crate) fn mutate(store: &SqliteStore, input: Value) -> Result<Value, String> {
    let (tenant, id) = scope(&input)?;
    let action = input["action"]
        .as_str()
        .ok_or("work_note_action_required")?;
    let mut conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let existing = read(&tx, &tenant, &id)?;
    let expected = check_revision(&input, existing.as_ref())?;
    let draft = if action == "duplicate_draft" {
        let generation = input["generation"]
            .as_i64()
            .ok_or("work_note_draft_generation_required")?;
        let raw:Option<String>=tx.query_row("SELECT payload_json FROM work_note_local_drafts WHERE tenant_id=?1 AND page_id=?2 AND generation=?3",params![tenant,id,generation],|r|r.get(0)).optional().map_err(|e|e.to_string())?;
        Some(
            serde_json::from_str::<Value>(&raw.ok_or("work_note_draft_generation_conflict")?)
                .map_err(|_| "work_note_draft_invalid")?,
        )
    } else {
        None
    };
    let mut page = existing
        .or_else(|| draft.clone())
        .ok_or("work_note_not_found")?;
    protected(&tx, &tenant, &page, !["restore_version", "duplicate_draft"].contains(&action))?;
    if is_trashed(&page) && !["restore", "duplicate_draft"].contains(&action) {
        return Err("work_note_trashed".into());
    }
    let mut changed = Vec::new();
    match action {
        "move" => {
            if let Some(target_id) = input["targetPageId"].as_str() {
                if target_id == id {
                    return Err("work_note_parent_cycle".into());
                }
                let target = read(&tx, &tenant, target_id)?
                    .filter(|p| !is_trashed(p))
                    .ok_or("work_note_not_found")?;
                let placement = input["placement"]
                    .as_str()
                    .ok_or("work_note_move_placement_invalid")?;
                if !["before", "inside", "after"].contains(&placement) {
                    return Err("work_note_move_placement_invalid".into());
                }
                let inside_folder = placement == "inside" && (crate::work_note_folders::is_folder(&target) || target.pointer("/properties/nodeKind").is_none());
                let parent = if inside_folder {
                    Some(target_id)
                } else {
                    target["parentId"].as_str()
                };
                validate_parent(&tx, &tenant, &id, parent)?;
                let mut siblings = all(&tx, &tenant)?
                    .into_iter()
                    .filter(|p| {
                        !is_trashed(p) && p["parentId"].as_str() == parent && p["pageId"] != id
                    })
                    .collect::<Vec<_>>();
                let index = if placement == "inside" {
                    siblings.len()
                } else {
                    siblings
                        .iter()
                        .position(|p| p["pageId"] == target_id)
                        .ok_or("work_note_move_target_changed")?
                        + usize::from(placement == "after")
                };
                page["parentId"] = parent.map(|p| json!(p)).unwrap_or(Value::Null);
                assert_local_editable(&tx, &tenant, &page)?;
                siblings.insert(index, page.clone());
                for (position, mut sibling) in siblings.into_iter().enumerate() {
                    if sibling["position"] != json!(position) || sibling["pageId"] == id {
                        if sibling["pageId"] != id {
                            protected(&tx, &tenant, &sibling, true)?;
                        }
                        sibling["position"] = json!(position);
                        sibling["updatedAtMs"] = json!(now_ms().max(revision(&sibling) + 1));
                        let saved = persist(&tx, &sibling)?;
                        if saved["pageId"] == id {
                            page = saved.clone();
                        }
                        changed.push(saved);
                    }
                }
            } else {
                let parent = input["parentId"].as_str();
                validate_parent(&tx, &tenant, &id, parent)?;
                page["parentId"] = parent.map(|p| json!(p)).unwrap_or(Value::Null);
                assert_local_editable(&tx, &tenant, &page)?;
                page["position"] = json!(input["position"].as_i64().unwrap_or(0).max(0));
            }
        }
        "trash" => {
            let pages = all(&tx, &tenant)?;
            let ids = descendants(&pages, &id);
            let at = now_ms().max(expected + 1);
            for candidate in pages.iter().filter(|p| {
                ids.contains(p["pageId"].as_str().unwrap_or_default()) && !is_trashed(p)
            }) {
                protected(&tx, &tenant, candidate, true)?;
            }
            for mut candidate in pages.into_iter().filter(|p| {
                ids.contains(p["pageId"].as_str().unwrap_or_default()) && !is_trashed(p)
            }) {
                candidate["properties"]["_localTrash"] =
                    json!({"rootPageId":id,"deletedAtMs":at,"expiresAtMs":at+RETENTION_MS});
                candidate["updatedAtMs"] = json!(at.max(revision(&candidate) + 1));
                changed.push(persist(&tx, &candidate)?);
            }
            page = changed
                .iter()
                .find(|p| p["pageId"] == id)
                .cloned()
                .ok_or("work_note_readback_failed")?;
        }
        "restore" => {
            if !is_trashed(&page) {
                return Err("work_note_not_trashed".into());
            }
            let deleted = page
                .pointer("/properties/_localTrash/deletedAtMs")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            if deleted + RETENTION_MS <= now_ms() {
                return Err("work_note_trash_expired".into());
            }
            let root = page
                .pointer("/properties/_localTrash/rootPageId")
                .and_then(Value::as_str)
                .unwrap_or(&id)
                .to_string();
            if root != id {
                return Err("work_note_restore_root_required".into());
            }
            validate_parent(&tx, &tenant, &id, page["parentId"].as_str())?;
            for mut candidate in all(&tx, &tenant)?.into_iter().filter(|p| {
                p.pointer("/properties/_localTrash/rootPageId")
                    .and_then(Value::as_str)
                    == Some(&id)
                    && p.pointer("/properties/_localTrash/deletedAtMs")
                        .and_then(Value::as_i64)
                        == Some(deleted)
            }) {
                candidate["properties"]
                    .as_object_mut()
                    .ok_or("work_note_document_invalid")?
                    .remove("_localTrash");
                candidate["updatedAtMs"] = json!(now_ms().max(revision(&candidate) + 1));
                changed.push(persist(&tx, &candidate)?);
            }
            page = changed
                .iter()
                .find(|p| p["pageId"] == id)
                .cloned()
                .ok_or("work_note_readback_failed")?;
        }
        "restore_version" => {
            let version = input["versionId"]
                .as_str()
                .ok_or("work_note_version_id_required")?;
            let old = crate::work_note_history::get(&tx, &tenant, &id, version)?["page"].clone();
            for key in ["title", "emoji", "blocks", "markdown", "properties"] {
                page[key] = old[key].clone();
            }
            if let Some(props) = page["properties"].as_object_mut() {
                props.remove("_localTrash");
            }
            validate_content(&page)?;
        }
        "duplicate" | "duplicate_draft" => {
            let pages = all(&tx, &tenant)?;
            let ids = descendants(&pages, &id);
            let mut selected = pages
                .into_iter()
                .filter(|p| {
                    ids.contains(p["pageId"].as_str().unwrap_or_default()) && !is_trashed(p)
                })
                .collect::<Vec<_>>();
            if let Some(mut draft) = draft {
                // Recovery creates an independent note; it never moves or rebinds the lesson page.
                if crate::lesson_plan_bindings::stored_page_structure(&tx, &tenant, &id)?.is_some() {
                    draft["parentId"] = Value::Null;
                    if let Some(props) = draft["properties"].as_object_mut() {
                        props.remove("lessonPlanBinding");
                    }
                }
                if let Some(props) = draft["properties"].as_object_mut() {
                    props.remove("_localTrash");
                }
                if validate_parent(&tx, &tenant, &id, draft["parentId"].as_str()).is_err() {
                    draft["parentId"] = Value::Null;
                }
                selected = vec![draft];
            }
            for candidate in &selected {
                protected(&tx, &tenant, candidate, action != "duplicate_draft")?;
            }
            let mapping = selected
                .iter()
                .map(|p| {
                    (
                        p["pageId"].as_str().unwrap().to_string(),
                        format!("note_{}", crate::random_url_token()),
                    )
                })
                .collect::<HashMap<_, _>>();
            let new_root = mapping[&id].clone();
            for mut candidate in selected {
                let old_id = candidate["pageId"].as_str().unwrap().to_string();
                let new_id = mapping[&old_id].clone();
                let parent = candidate["parentId"].as_str().map(str::to_string);
                candidate["pageId"] = json!(new_id);
                if old_id == id {
                    candidate["title"] = json!(format!(
                        "{} (복사)",
                        candidate["title"].as_str().unwrap_or("문서")
                    ));
                }
                if let Some(mapped) = parent.as_ref().and_then(|p| mapping.get(p)) {
                    candidate["parentId"] = json!(mapped);
                }
                candidate["createdAtMs"] = json!(now_ms());
                candidate["updatedAtMs"] = json!(now_ms());
                for key in ["blocks", "properties", "markdown"] {
                    remap_document(&mut candidate[key], &mapping);
                }
                // Attachment IDs are independent; immutable binary paths may be shared safely.
                let mut stmt=tx.prepare("SELECT attachment_id,block_id,file_name,content_type,byte_size,sha256,local_path FROM work_note_attachments WHERE tenant_id=?1 AND page_id=?2").map_err(|e|e.to_string())?;
                let attachments = stmt
                    .query_map(params![tenant, old_id], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, i64>(4)?,
                            r.get::<_, String>(5)?,
                            r.get::<_, String>(6)?,
                        ))
                    })
                    .map_err(|e| e.to_string())?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string())?;
                drop(stmt);
                let attachment_map = attachments
                    .iter()
                    .map(|a| (a.0.clone(), format!("att_{}", crate::random_url_token())))
                    .collect::<HashMap<_, _>>();
                for key in ["blocks", "properties", "markdown"] {
                    remap_document(&mut candidate[key], &attachment_map);
                }
                let saved = persist(&tx, &candidate)?;
                for a in attachments {
                    tx.execute("INSERT INTO work_note_attachments VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)",params![tenant,attachment_map[&a.0],new_id,a.1,a.2,a.3,a.4,a.5,a.6,now_ms()]).map_err(|e|e.to_string())?;
                }
                changed.push(saved);
            }
            page = changed
                .iter()
                .find(|p| p["pageId"] == new_root)
                .cloned()
                .ok_or("work_note_readback_failed")?;
        }
        _ => return Err("work_note_action_invalid".into()),
    }
    if changed.is_empty() {
        page["updatedAtMs"] = json!(now_ms().max(expected + 1));
        page = persist(&tx, &page)?;
        changed.push(page.clone());
    }
    tx.commit()
        .map_err(|e| format!("work_note_commit_failed:{e}"))?;
    Ok(json!({"ok":true,"revision":revision(&page),"page":page,"pages":changed,"verified":true}))
}

fn remap_document(value: &mut Value, mapping: &HashMap<String, String>) {
    match value {
        Value::String(text) => {
            static REFERENCES: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
                regex::Regex::new(r"local-attachment://([A-Za-z0-9._-]+)").unwrap()
            });
            *text = REFERENCES
                .replace_all(text, |capture: &regex::Captures| {
                    mapping
                        .get(&capture[1])
                        .map(|next| format!("local-attachment://{next}"))
                        .unwrap_or_else(|| capture[0].to_string())
                })
                .into_owned();
        }
        Value::Array(items) => {
            for item in items {
                remap_document(item, mapping)
            }
        }
        Value::Object(object) => {
            for (key, item) in object {
                if [
                    "attachmentId",
                    "mediaId",
                    "pageId",
                    "targetPageId",
                    "rootPageId",
                ]
                .contains(&key.as_str())
                {
                    if let Some(next) = item.as_str().and_then(|id| mapping.get(id)) {
                        *item = json!(next);
                    }
                } else {
                    remap_document(item, mapping)
                }
            }
        }
        _ => {}
    }
}
pub(crate) fn trash(conn: &Connection, tenant: &str) -> Result<Value, String> {
    let items = all(conn, tenant)?
        .into_iter()
        .filter(|p| {
            is_trashed(p)
                && p.pointer("/properties/_localTrash/rootPageId") == p.get("pageId")
                && p.pointer("/properties/_localTrash/expiresAtMs")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    > now_ms()
        })
        .collect::<Vec<_>>();
    Ok(json!({"ok":true,"items":items,"retentionDays":30}))
}

#[cfg(test)]
#[path = "work_note_documents_tests.rs"]
mod tests;
