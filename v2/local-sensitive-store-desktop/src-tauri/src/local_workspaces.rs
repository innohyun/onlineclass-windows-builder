use super::{normalize, normalize_id_segment, normalize_tenant_id, AppState, SqliteStore};
use rusqlite::{params, params_from_iter, types::Value as SqlValue};
use serde::Deserialize;
use serde_json::{json, Value};

const MAX_TREE_PAGES: i64 = 5_000;
const MAX_SEARCH_RESULTS: i64 = 100;
#[cfg(test)]
const LESSON_ROOT_PAGE_ID: &str = "lesson-materials-root";
#[cfg(test)]
const LESSON_SYSTEM_KIND: &str = "lesson_materials_folder";

const LESSON_TREE_CTE: &str = r#"WITH RECURSIVE lesson_pages(page_id) AS (
  SELECT page_id FROM work_note_pages
  WHERE tenant_id=?1 AND (page_id='lesson-materials-root'
    OR json_extract(properties_json,'$.systemKind')='lesson_materials_folder')
  UNION
  SELECT child.page_id FROM work_note_pages child
  JOIN lesson_pages parent ON child.parent_id=parent.page_id
  WHERE child.tenant_id=?1
)"#;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LocalWorkspaceSearchInput {
    tenant_id: String,
    workspace: String,
    #[serde(default)]
    query: String,
    #[serde(default)]
    offset: i64,
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    40
}

fn workspace(value: &str) -> Result<&'static str, String> {
    match normalize(value, 40).as_str() {
        "lesson_materials" => Ok("lesson_materials"),
        "work_materials" => Ok("work_materials"),
        _ => Err("local_workspace_invalid".to_string()),
    }
}

fn membership_clause(value: &str) -> Result<&'static str, String> {
    Ok(match workspace(value)? {
        "lesson_materials" => "p.page_id IN (SELECT page_id FROM lesson_pages)",
        _ => "p.page_id NOT IN (SELECT page_id FROM lesson_pages)",
    })
}

fn metadata_json(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "pageId": row.get::<_, String>(0)?,
        "parentId": row.get::<_, Option<String>>(1)?,
        "title": row.get::<_, String>(2)?,
        "emoji": row.get::<_, String>(3)?,
        "position": row.get::<_, i64>(4)?,
        "updatedAtMs": row.get::<_, i64>(5)?,
        "systemKind": row.get::<_, Option<String>>(6)?,
        "attachmentCount": row.get::<_, i64>(7)?,
        "attachmentBytes": row.get::<_, i64>(8)?,
    }))
}

fn tree(
    store: &SqliteStore,
    raw_tenant_id: String,
    raw_workspace: String,
) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(raw_tenant_id)));
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let selected_workspace = workspace(&raw_workspace)?;
    let membership = membership_clause(selected_workspace)?;
    let sql = format!(
        r#"{LESSON_TREE_CTE}
      SELECT p.page_id,p.parent_id,p.title,p.emoji,p.position,p.updated_at_ms,
        json_extract(p.properties_json,'$.systemKind'),
        COUNT(a.attachment_id),COALESCE(SUM(a.byte_size),0)
      FROM work_note_pages p
      LEFT JOIN work_note_attachments a ON a.tenant_id=p.tenant_id AND a.page_id=p.page_id
      WHERE p.tenant_id=?1 AND {membership}
      GROUP BY p.tenant_id,p.page_id
      ORDER BY COALESCE(p.parent_id,''),p.position,p.page_id
      LIMIT ?2"#
    );
    let count_sql = format!(
        r#"{LESSON_TREE_CTE}
      SELECT COUNT(*) FROM work_note_pages p WHERE p.tenant_id=?1 AND {membership}"#
    );
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let total = conn
        .query_row(&count_sql, params![tenant_id], |row| row.get::<_, i64>(0))
        .map_err(|error| format!("local_workspace_count_failed:{error}"))?;
    let mut statement = conn
        .prepare(&sql)
        .map_err(|error| format!("local_workspace_tree_prepare_failed:{error}"))?;
    let rows = statement
        .query_map(params![tenant_id, MAX_TREE_PAGES], metadata_json)
        .map_err(|error| format!("local_workspace_tree_query_failed:{error}"))?;
    let mut pages = Vec::new();
    for row in rows {
        pages.push(row.map_err(|error| format!("local_workspace_tree_row_failed:{error}"))?);
    }
    Ok(json!({
        "ok": true,
        "tenantId": tenant_id,
        "workspace": selected_workspace,
        "total": total,
        "truncated": total > MAX_TREE_PAGES,
        "pages": pages,
    }))
}

fn page(
    store: &SqliteStore,
    raw_tenant_id: String,
    raw_workspace: String,
    raw_page_id: String,
) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(raw_tenant_id)));
    let page_id = normalize_id_segment(Some(&Value::String(raw_page_id)), 180);
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    if page_id.is_empty() {
        return Err("page_id_required".to_string());
    }
    let selected_workspace = workspace(&raw_workspace)?;
    let membership = membership_clause(selected_workspace)?;
    let sql = format!(
        r#"{LESSON_TREE_CTE}
      SELECT p.page_id,p.parent_id,p.title,p.emoji,p.position,p.properties_json,
        p.document_json,p.markdown,p.created_at_ms,p.updated_at_ms
      FROM work_note_pages p
      WHERE p.tenant_id=?1 AND p.page_id=?2 AND {membership}"#
    );
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let mut statement = conn
        .prepare(&sql)
        .map_err(|error| format!("local_workspace_page_prepare_failed:{error}"))?;
    let selected = statement
        .query_row(params![tenant_id, page_id], |row| {
            let properties: String = row.get(5)?;
            let blocks: String = row.get(6)?;
            Ok(json!({
                "pageId": row.get::<_, String>(0)?,
                "parentId": row.get::<_, Option<String>>(1)?,
                "title": row.get::<_, String>(2)?,
                "emoji": row.get::<_, String>(3)?,
                "position": row.get::<_, i64>(4)?,
                "properties": serde_json::from_str::<Value>(&properties).unwrap_or_else(|_| json!({})),
                "blocks": serde_json::from_str::<Value>(&blocks).unwrap_or_else(|_| json!([])),
                "markdown": row.get::<_, String>(7)?,
                "createdAtMs": row.get::<_, i64>(8)?,
                "updatedAtMs": row.get::<_, i64>(9)?,
            }))
        })
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => "local_workspace_page_not_found".to_string(),
            other => format!("local_workspace_page_query_failed:{other}"),
        })?;
    drop(statement);
    let mut attachment_statement = conn
        .prepare(
            "SELECT attachment_id,block_id,file_name,content_type,byte_size,sha256,updated_at_ms
         FROM work_note_attachments WHERE tenant_id=?1 AND page_id=?2 ORDER BY attachment_id",
        )
        .map_err(|error| format!("local_workspace_attachment_prepare_failed:{error}"))?;
    let attachments = attachment_statement
        .query_map(params![tenant_id, page_id], |row| {
            Ok(json!({
                "attachmentId": row.get::<_, String>(0)?,
                "mediaId": row.get::<_, String>(0)?,
                "pageId": page_id,
                "blockId": row.get::<_, String>(1)?,
                "fileName": row.get::<_, String>(2)?,
                "contentType": row.get::<_, String>(3)?,
                "size": row.get::<_, i64>(4)?,
                "sha256": row.get::<_, String>(5)?,
                "updatedAtMs": row.get::<_, i64>(6)?,
            }))
        })
        .map_err(|error| format!("local_workspace_attachment_query_failed:{error}"))?;
    let mut files = Vec::new();
    for attachment in attachments {
        files.push(
            attachment.map_err(|error| format!("local_workspace_attachment_row_failed:{error}"))?,
        );
    }
    Ok(json!({
        "ok": true,
        "tenantId": tenant_id,
        "workspace": selected_workspace,
        "page": selected,
        "attachments": files,
    }))
}

fn search(store: &SqliteStore, input: LocalWorkspaceSearchInput) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(input.tenant_id)));
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let selected_workspace = workspace(&input.workspace)?;
    let membership = membership_clause(selected_workspace)?;
    let query = normalize(&input.query, 200);
    let offset = input.offset.max(0);
    let limit = input.limit.clamp(1, MAX_SEARCH_RESULTS);
    let mut filters = vec!["p.tenant_id=?1".to_string(), membership.to_string()];
    let mut values = vec![SqlValue::Text(tenant_id.clone())];
    if !query.is_empty() {
        let terms = query
            .split_whitespace()
            .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ");
        let pattern = format!("%{}%", query.to_lowercase());
        filters.push("(p.page_id IN (SELECT page_id FROM work_note_pages_fts WHERE tenant_id=? AND work_note_pages_fts MATCH ?) OR lower(p.title) LIKE ? OR EXISTS (SELECT 1 FROM work_note_attachments a2 WHERE a2.tenant_id=p.tenant_id AND a2.page_id=p.page_id AND lower(a2.file_name) LIKE ?))".to_string());
        values.push(SqlValue::Text(tenant_id.clone()));
        values.push(SqlValue::Text(terms));
        values.push(SqlValue::Text(pattern.clone()));
        values.push(SqlValue::Text(pattern));
    }
    let where_sql = filters.join(" AND ");
    let count_sql = format!(
        r#"{LESSON_TREE_CTE}
      SELECT COUNT(*) FROM work_note_pages p WHERE {where_sql}"#
    );
    let list_sql = format!(
        r#"{LESSON_TREE_CTE}
      SELECT p.page_id,p.parent_id,p.title,p.emoji,p.position,p.updated_at_ms,
        json_extract(p.properties_json,'$.systemKind'),COUNT(a.attachment_id),COALESCE(SUM(a.byte_size),0)
      FROM work_note_pages p
      LEFT JOIN work_note_attachments a ON a.tenant_id=p.tenant_id AND a.page_id=p.page_id
      WHERE {where_sql}
      GROUP BY p.tenant_id,p.page_id
      ORDER BY p.updated_at_ms DESC,p.page_id
      LIMIT ? OFFSET ?"#
    );
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let total = conn
        .query_row(&count_sql, params_from_iter(values.iter()), |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| format!("local_workspace_search_count_failed:{error}"))?;
    let mut list_values = values;
    list_values.push(SqlValue::Integer(limit));
    list_values.push(SqlValue::Integer(offset));
    let mut statement = conn
        .prepare(&list_sql)
        .map_err(|error| format!("local_workspace_search_prepare_failed:{error}"))?;
    let rows = statement
        .query_map(params_from_iter(list_values.iter()), metadata_json)
        .map_err(|error| format!("local_workspace_search_query_failed:{error}"))?;
    let mut pages = Vec::new();
    for row in rows {
        pages.push(row.map_err(|error| format!("local_workspace_search_row_failed:{error}"))?);
    }
    Ok(json!({
        "ok": true,
        "tenantId": tenant_id,
        "workspace": selected_workspace,
        "query": query,
        "total": total,
        "offset": offset,
        "limit": limit,
        "hasMore": (offset + pages.len() as i64) < total,
        "pages": pages,
    }))
}

#[tauri::command]
pub(crate) fn get_local_workspace_tree(
    state: tauri::State<'_, AppState>,
    tenant_id: String,
    workspace: String,
) -> Value {
    state
        .store
        .lock()
        .ok()
        .and_then(|store| store.clone())
        .ok_or_else(|| "local_store_unavailable".to_string())
        .and_then(|store| tree(&store, tenant_id, workspace))
        .unwrap_or_else(|error| json!({ "ok": false, "error": error }))
}

#[tauri::command]
pub(crate) fn get_local_workspace_page(
    state: tauri::State<'_, AppState>,
    tenant_id: String,
    workspace: String,
    page_id: String,
) -> Value {
    state
        .store
        .lock()
        .ok()
        .and_then(|store| store.clone())
        .ok_or_else(|| "local_store_unavailable".to_string())
        .and_then(|store| page(&store, tenant_id, workspace, page_id))
        .unwrap_or_else(|error| json!({ "ok": false, "error": error }))
}

#[tauri::command]
pub(crate) fn search_local_workspace(
    state: tauri::State<'_, AppState>,
    input: LocalWorkspaceSearchInput,
) -> Value {
    state
        .store
        .lock()
        .ok()
        .and_then(|store| store.clone())
        .ok_or_else(|| "local_store_unavailable".to_string())
        .and_then(|store| search(&store, input))
        .unwrap_or_else(|error| json!({ "ok": false, "error": error }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::random_url_token;
    use std::fs;

    fn test_store() -> (std::path::PathBuf, SqliteStore) {
        let directory =
            std::env::temp_dir().join(format!("onlineclass-workspace-test-{}", random_url_token()));
        fs::create_dir_all(&directory).expect("create test directory");
        let store = SqliteStore::open(directory.join("workspace.sqlite")).expect("open test store");
        (directory, store)
    }

    fn insert_page(
        store: &SqliteStore,
        page_id: &str,
        parent_id: Option<&str>,
        title: &str,
        system_kind: Option<&str>,
    ) {
        let properties = system_kind
            .map(|kind| json!({"systemKind":kind}))
            .unwrap_or_else(|| json!({}));
        store.upsert_work_note(json!({
            "tenantId":"tenant-a","pageId":page_id,"parentId":parent_id,"title":title,"emoji":"📄",
            "position":0,"properties":properties,"blocks":[{"type":"text","text":format!("{title} secret body")}],
            "markdown":format!("{title} secret body"),"createdAtMs":1,"updatedAtMs":2
        })).expect("insert page");
    }

    #[test]
    fn tree_partitions_lesson_and_work_without_returning_page_bodies() {
        let (directory, store) = test_store();
        insert_page(
            &store,
            LESSON_ROOT_PAGE_ID,
            None,
            "수업자료",
            Some(LESSON_SYSTEM_KIND),
        );
        insert_page(
            &store,
            "lesson-child",
            Some(LESSON_ROOT_PAGE_ID),
            "국어",
            None,
        );
        insert_page(&store, "work-root", None, "업무자료", None);
        let lessons = tree(
            &store,
            "tenant-a".to_string(),
            "lesson_materials".to_string(),
        )
        .expect("lesson tree");
        let work =
            tree(&store, "tenant-a".to_string(), "work_materials".to_string()).expect("work tree");
        assert_eq!(lessons["total"], 2);
        assert_eq!(work["total"], 1);
        let serialized = serde_json::to_string(&lessons).expect("serialize tree");
        assert!(!serialized.contains("secret body"));
        let selected = page(
            &store,
            "tenant-a".to_string(),
            "lesson_materials".to_string(),
            "lesson-child".to_string(),
        )
        .expect("lesson page");
        assert!(selected["page"]["markdown"]
            .as_str()
            .unwrap_or("")
            .contains("secret body"));
        assert_eq!(
            page(
                &store,
                "tenant-a".to_string(),
                "work_materials".to_string(),
                "lesson-child".to_string()
            )
            .unwrap_err(),
            "local_workspace_page_not_found"
        );
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn search_stays_inside_workspace_and_returns_metadata_only() {
        let (directory, store) = test_store();
        insert_page(
            &store,
            LESSON_ROOT_PAGE_ID,
            None,
            "수업자료",
            Some(LESSON_SYSTEM_KIND),
        );
        insert_page(
            &store,
            "lesson-child",
            Some(LESSON_ROOT_PAGE_ID),
            "공통 검색어 수업",
            None,
        );
        insert_page(&store, "work-root", None, "공통 검색어 업무", None);
        let result = search(
            &store,
            LocalWorkspaceSearchInput {
                tenant_id: "tenant-a".to_string(),
                workspace: "lesson_materials".to_string(),
                query: "공통 검색어".to_string(),
                offset: 0,
                limit: 40,
            },
        )
        .expect("search lesson workspace");
        assert_eq!(result["total"], 1);
        assert_eq!(result["pages"][0]["pageId"], "lesson-child");
        assert!(result["pages"][0].get("markdown").is_none());
        fs::remove_dir_all(directory).expect("remove test directory");
    }
}
