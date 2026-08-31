use super::{normalize, normalize_id_segment, normalize_tenant_id, AppState, SqliteStore};
use rusqlite::{params, params_from_iter, types::Value as SqlValue};
use serde::Deserialize;
use serde_json::{json, Value};

const MAX_TREE_PAGES: i64 = 5_000;
const MAX_SEARCH_RESULTS: i64 = 100;
const MAX_MCP_SEARCH_RESULTS: i64 = 20;
const MAX_MCP_MARKDOWN_CHARS: usize = 200_000;
const MAX_MCP_SNIPPET_CHARS: usize = 320;
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
        _ => "p.page_id NOT IN (SELECT page_id FROM lesson_pages) AND COALESCE(json_extract(p.properties_json,'$.systemKind'),'')=''",
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

fn title_path(
    conn: &rusqlite::Connection,
    tenant_id: &str,
    selected_workspace: &str,
    page_id: &str,
) -> Result<Vec<String>, String> {
    let membership = membership_clause(selected_workspace)?;
    let sql = format!(
        r#"{LESSON_TREE_CTE},
      ancestors(page_id,parent_id,title,depth) AS (
        SELECT p.page_id,p.parent_id,p.title,0
        FROM work_note_pages p
        WHERE p.tenant_id=?1 AND p.page_id=?2 AND {membership}
        UNION ALL
        SELECT p.page_id,p.parent_id,p.title,child.depth+1
        FROM work_note_pages p
        JOIN ancestors child ON child.parent_id=p.page_id
        WHERE p.tenant_id=?1 AND {membership} AND child.depth<100
      )
      SELECT title FROM ancestors ORDER BY depth DESC"#
    );
    let mut statement = conn
        .prepare(&sql)
        .map_err(|error| format!("local_workspace_path_prepare_failed:{error}"))?;
    let rows = statement
        .query_map(params![tenant_id, page_id], |row| row.get::<_, String>(0))
        .map_err(|error| format!("local_workspace_path_query_failed:{error}"))?;
    let mut path = Vec::new();
    for row in rows {
        path.push(row.map_err(|error| format!("local_workspace_path_row_failed:{error}"))?);
    }
    Ok(path)
}

fn markdown_snippet(markdown: &str, query: &str) -> String {
    let query = query.to_lowercase();
    let terms = query.split_whitespace().collect::<Vec<_>>();
    let candidate = if terms.is_empty() {
        markdown.trim()
    } else {
        markdown
            .lines()
            .find(|line| {
                let line = line.to_lowercase();
                terms.iter().any(|term| line.contains(term))
            })
            .unwrap_or_else(|| markdown.trim())
            .trim()
    };
    candidate.chars().take(MAX_MCP_SNIPPET_CHARS).collect()
}

fn mcp_safe_markdown(markdown: &str) -> String {
    super::classaimate_mcp_materials_markdown::sanitize(markdown)
}

pub(crate) fn mcp_search(store: &SqliteStore, input: &Value) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(input.get("tenantId"));
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let selected_workspace = workspace(
        input
            .get("workspace")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    let membership = membership_clause(selected_workspace)?;
    let query = normalize(
        input
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        200,
    );
    let limit = input
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(MAX_MCP_SEARCH_RESULTS)
        .clamp(1, MAX_MCP_SEARCH_RESULTS);
    let mut filters = vec!["p.tenant_id=?1".to_string(), membership.to_string()];
    let mut values = vec![SqlValue::Text(tenant_id.clone())];
    if !query.is_empty() {
        let terms = query
            .split_whitespace()
            .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ");
        let pattern = format!("%{}%", query.to_lowercase());
        filters.push("(p.page_id IN (SELECT page_id FROM work_note_pages_fts WHERE tenant_id=? AND work_note_pages_fts MATCH ?) OR lower(p.title) LIKE ?)".to_string());
        values.push(SqlValue::Text(tenant_id.clone()));
        values.push(SqlValue::Text(terms));
        values.push(SqlValue::Text(pattern));
    }
    let where_sql = filters.join(" AND ");
    let list_sql = format!(
        r#"{LESSON_TREE_CTE}
      SELECT p.page_id,p.title,p.markdown,p.updated_at_ms
      FROM work_note_pages p
      WHERE {where_sql}
      ORDER BY p.updated_at_ms DESC,p.page_id"#
    );
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let mut statement = conn
        .prepare(&list_sql)
        .map_err(|error| format!("local_workspace_mcp_search_prepare_failed:{error}"))?;
    let rows = statement
        .query_map(params_from_iter(values.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|error| format!("local_workspace_mcp_search_query_failed:{error}"))?;
    let search_terms = query
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut total = 0usize;
    let mut selected = Vec::new();
    for row in rows {
        let (page_id, title, markdown, updated_at_ms) =
            row.map_err(|error| format!("local_workspace_mcp_search_row_failed:{error}"))?;
        if search_terms.is_empty() {
            total += 1;
            if selected.len() < limit as usize {
                selected.push((page_id, title, mcp_safe_markdown(&markdown), updated_at_ms));
            }
            continue;
        }
        let safe_markdown = mcp_safe_markdown(&markdown);
        let searchable = format!("{}\n{}", title.to_lowercase(), safe_markdown.to_lowercase());
        if !search_terms.iter().all(|term| searchable.contains(term)) {
            continue;
        }
        total += 1;
        if selected.len() < limit as usize {
            selected.push((page_id, title, safe_markdown, updated_at_ms));
        }
    }
    drop(statement);
    let mut pages = Vec::with_capacity(selected.len());
    for (page_id, title, markdown, updated_at_ms) in selected {
        pages.push(json!({
            "pageId": page_id,
            "title": title,
            "path": title_path(&conn, &tenant_id, selected_workspace, &page_id)?,
            "snippet": markdown_snippet(&markdown, &query),
            "updatedAtMs": updated_at_ms,
        }));
    }
    Ok(json!({
        "ok": true,
        "tenantId": tenant_id,
        "workspace": selected_workspace,
        "query": query,
        "total": total,
        "limit": limit,
        "hasMore": pages.len() < total,
        "pages": pages,
    }))
}

pub(crate) fn mcp_page(store: &SqliteStore, input: &Value) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(input.get("tenantId"));
    let page_id = normalize_id_segment(input.get("pageId"), 180);
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    if page_id.is_empty() {
        return Err("page_id_required".to_string());
    }
    let selected_workspace = workspace(
        input
            .get("workspace")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    let membership = membership_clause(selected_workspace)?;
    let sql = format!(
        r#"{LESSON_TREE_CTE}
      SELECT p.title,p.markdown,p.document_json,p.updated_at_ms
      FROM work_note_pages p
      WHERE p.tenant_id=?1 AND p.page_id=?2 AND {membership}"#
    );
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let (title, markdown, document_json, updated_at_ms) = conn
        .query_row(&sql, params![tenant_id, page_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => "local_workspace_page_not_found".to_string(),
            other => format!("local_workspace_mcp_page_query_failed:{other}"),
        })?;
    let markdown = mcp_safe_markdown(&markdown);
    let character_count = markdown.chars().count();
    let truncated = character_count > MAX_MCP_MARKDOWN_CHARS;
    let markdown = markdown
        .chars()
        .take(MAX_MCP_MARKDOWN_CHARS)
        .collect::<String>();
    let blocks: Value = serde_json::from_str(&document_json)
        .map_err(|_| "local_workspace_mcp_blocks_invalid".to_string())?;
    if !blocks.is_array() || blocks.as_array().map(Vec::len).unwrap_or(0) > 5_000 {
        return Err("local_workspace_mcp_blocks_invalid".to_string());
    }
    Ok(json!({
        "ok": true,
        "tenantId": tenant_id,
        "workspace": selected_workspace,
        "page": {
            "pageId": page_id,
            "title": title,
            "path": title_path(&conn, &tenant_id, selected_workspace, &page_id)?,
            "markdown": markdown,
            "blocks": blocks,
            "truncated": truncated,
            "characterCount": character_count,
            "revision": updated_at_ms,
        }
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

    #[test]
    fn mcp_search_is_bounded_and_excludes_attachment_file_names() {
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
            "국어 자료",
            None,
        );
        insert_page(&store, "work-root", None, "업무자료", None);
        store
            .upsert_work_note(json!({
                "tenantId":"tenant-a",
                "pageId":"work-with-attachment-markdown",
                "title":"일반 업무",
                "properties":{},
                "blocks":[],
                "markdown":"일반 본문 [공식 안내](https://example.com/private)\n\n확인: @이선생\n\n[기밀첨부파일.pdf](local-attachment://attachment-secret)",
                "createdAtMs":1,
                "updatedAtMs":2
            }))
            .expect("insert attachment Markdown page");
        insert_page(
            &store,
            "protected-work-root",
            None,
            "보호 업무",
            Some("mobile_work_reference_folder"),
        );
        {
            let conn = store.conn.lock().expect("db lock");
            conn.execute(
                "INSERT INTO work_note_attachments
                 (tenant_id,attachment_id,page_id,block_id,file_name,content_type,byte_size,sha256,local_path,created_at_ms,updated_at_ms)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)",
                params![
                    "tenant-a",
                    "attachment-a",
                    "work-root",
                    "block-a",
                    "파일명전용검색어.pdf",
                    "application/pdf",
                    1,
                    "sha256-a",
                    "attachment-a/content.pdf",
                    1
                ],
            )
            .expect("insert attachment");
        }
        let attachment_only = mcp_search(
            &store,
            &json!({
                "tenantId":"tenant-a",
                "workspace":"work_materials",
                "query":"파일명전용검색어"
            }),
        )
        .expect("search work materials");
        assert_eq!(attachment_only["total"], 0);
        assert_eq!(
            mcp_search(
                &store,
                &json!({
                    "tenantId":"tenant-a",
                    "workspace":"work_materials",
                    "query":"기밀첨부파일"
                })
            )
            .expect("search attachment Markdown")["total"],
            0
        );
        let safe_page = mcp_page(
            &store,
            &json!({
                "tenantId":"tenant-a",
                "workspace":"work_materials",
                "pageId":"work-with-attachment-markdown"
            }),
        )
        .expect("read sanitized work page");
        let safe_markdown = safe_page["page"]["markdown"].as_str().unwrap_or_default();
        assert!(safe_markdown.contains("일반 본문 공식 안내"));
        assert!(safe_markdown.contains("[멘션]"));
        for removed in [
            "https://",
            "@이선생",
            "기밀첨부파일.pdf",
            "attachment-secret",
        ] {
            assert!(!safe_markdown.contains(removed));
        }
        assert_eq!(
            mcp_search(
                &store,
                &json!({
                    "tenantId":"tenant-a",
                    "workspace":"work_materials",
                    "query":"보호 업무"
                })
            )
            .expect("search protected system page")["total"],
            0
        );

        for index in 0..25 {
            insert_page(
                &store,
                &format!("lesson-page-{index}"),
                Some(LESSON_ROOT_PAGE_ID),
                &format!("추가 수업 자료 {index}"),
                None,
            );
        }
        let bounded = mcp_search(
            &store,
            &json!({
                "tenantId":"tenant-a",
                "workspace":"lesson_materials",
                "limit":999
            }),
        )
        .expect("bounded search");
        assert_eq!(bounded["limit"], 20);
        assert_eq!(bounded["pages"].as_array().map(Vec::len), Some(20));
        assert_eq!(bounded["hasMore"], true);
        let serialized = serde_json::to_string(&bounded).expect("serialize result");
        for forbidden in [
            "parentId",
            "emoji",
            "position",
            "properties",
            "blocks",
            "attachments",
            "파일명전용검색어",
        ] {
            assert!(!serialized.contains(forbidden));
        }
        assert_eq!(
            mcp_search(
                &store,
                &json!({"tenantId":"tenant-b","workspace":"lesson_materials"})
            )
            .expect("other tenant search")["total"],
            0
        );
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn mcp_page_returns_title_path_bounded_markdown_full_blocks_and_revision() {
        let (directory, store) = test_store();
        insert_page(
            &store,
            LESSON_ROOT_PAGE_ID,
            None,
            "수업자료",
            Some(LESSON_SYSTEM_KIND),
        );
        let long_markdown = "가".repeat(MAX_MCP_MARKDOWN_CHARS + 1);
        store
            .upsert_work_note(json!({
                "tenantId":"tenant-a",
                "pageId":"lesson-child",
                "parentId":LESSON_ROOT_PAGE_ID,
                "title":"국어",
                "emoji":"📄",
                "position":0,
                "properties":{},
                "blocks":[{"type":"text","text":"노출 금지 blocks"}],
                "markdown":long_markdown,
                "createdAtMs":1,
                "updatedAtMs":2
            }))
            .expect("insert long page");
        let result = mcp_page(
            &store,
            &json!({
                "tenantId":"tenant-a",
                "workspace":"lesson_materials",
                "pageId":"lesson-child"
            }),
        )
        .expect("read page");
        assert_eq!(result["page"]["path"], json!(["수업자료", "국어"]));
        assert_eq!(result["page"]["characterCount"], MAX_MCP_MARKDOWN_CHARS + 1);
        assert_eq!(result["page"]["truncated"], true);
        assert_eq!(
            result["page"]["blocks"],
            json!([{"type":"text","text":"노출 금지 blocks"}])
        );
        assert_eq!(result["page"]["revision"], 2);
        assert_eq!(
            result["page"]["markdown"]
                .as_str()
                .map(|value| value.chars().count()),
            Some(MAX_MCP_MARKDOWN_CHARS)
        );
        let keys = result["page"]
            .as_object()
            .expect("page object")
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            keys,
            [
                "characterCount",
                "blocks",
                "markdown",
                "pageId",
                "path",
                "title",
                "truncated",
                "revision"
            ]
            .into_iter()
            .collect()
        );
        assert_eq!(
            mcp_page(
                &store,
                &json!({
                    "tenantId":"tenant-a",
                    "workspace":"work_materials",
                    "pageId":"lesson-child"
                })
            )
            .unwrap_err(),
            "local_workspace_page_not_found"
        );
        fs::remove_dir_all(directory).expect("remove test directory");
    }
}
