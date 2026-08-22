use crate::{json_response, normalize_id_segment, normalize_tenant_id, parse_request_url, query, request_authority, scope_tenant_id, BrowserLinkStore, SqliteStore};
use chrono::Utc;
use rand::{distributions::Alphanumeric, Rng};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use tiny_http::{Header, Method, Request, Response, ResponseBox, StatusCode};

const ROOT_DIR: &str = "work-note-attachments";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttachmentRecord {
    pub tenant_id: String,
    pub attachment_id: String,
    pub page_id: String,
    pub block_id: String,
    pub file_name: String,
    pub content_type: String,
    pub size: i64,
    pub sha256: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

struct AttachmentRow {
    record: AttachmentRecord,
    local_path: String,
}

pub(crate) struct AttachmentFile {
    pub record: AttachmentRecord,
    pub file: File,
    pub size: u64,
}

pub(crate) fn ensure_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS work_note_attachments (
          tenant_id TEXT NOT NULL,
          attachment_id TEXT NOT NULL,
          page_id TEXT NOT NULL,
          block_id TEXT NOT NULL,
          file_name TEXT NOT NULL,
          content_type TEXT NOT NULL,
          byte_size INTEGER NOT NULL,
          sha256 TEXT NOT NULL,
          local_path TEXT NOT NULL,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, attachment_id),
          FOREIGN KEY (tenant_id, page_id) REFERENCES work_note_pages(tenant_id, page_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_work_note_attachments_page
          ON work_note_attachments (tenant_id, page_id, created_at_ms);
        "#,
    )
    .map_err(|e| format!("db_work_note_attachment_schema_failed:{e}"))
}

fn safe_id(value: &str) -> String {
    let normalized = normalize_id_segment(Some(&Value::String(value.to_string())), 180);
    if normalized.is_empty()
        || normalized != value
        || !normalized
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
    {
        String::new()
    } else {
        normalized
    }
}

fn safe_file_name(value: &str) -> String {
    let name = value
        .trim()
        .chars()
        .take(240)
        .map(|ch| {
            if ch.is_control() || matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                ch
            }
        })
        .collect::<String>()
        .trim_start_matches('.')
        .to_string();
    if name.is_empty() { "첨부파일".to_string() } else { name }
}

fn safe_content_type(value: &str) -> String {
    let mime = value.trim().to_ascii_lowercase();
    if mime.contains('/') && !mime.chars().any(|ch| ch.is_control() || ch.is_whitespace()) {
        mime.chars().take(160).collect()
    } else {
        "application/octet-stream".to_string()
    }
}

fn tenant_folder(tenant_id: &str) -> String {
    format!("{:x}", Sha256::digest(tenant_id.as_bytes()))[..32].to_string()
}

fn relative_path(tenant_id: &str, attachment_id: &str, file_name: &str) -> PathBuf {
    let extension = Path::new(file_name)
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty() && value.len() <= 15)
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    PathBuf::from(ROOT_DIR)
        .join(tenant_folder(tenant_id))
        .join(attachment_id)
        .join(format!("content{extension}"))
}

fn checked_path(store: &SqliteStore, relative: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative);
    if path.is_absolute() || path.components().any(|part| matches!(part, Component::ParentDir | Component::RootDir | Component::Prefix(_))) {
        return Err("work_note_attachment_path_invalid".to_string());
    }
    Ok(store.data_dir.join(path))
}

fn row_for(store: &SqliteStore, tenant_id: &str, attachment_id: &str) -> Result<Option<AttachmentRow>, String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    conn.query_row(
        "SELECT tenant_id,attachment_id,page_id,block_id,file_name,content_type,byte_size,sha256,local_path,created_at_ms,updated_at_ms FROM work_note_attachments WHERE tenant_id=?1 AND attachment_id=?2",
        params![tenant_id, attachment_id],
        |row| Ok(AttachmentRow {
            record: AttachmentRecord {
                tenant_id: row.get(0)?, attachment_id: row.get(1)?, page_id: row.get(2)?, block_id: row.get(3)?,
                file_name: row.get(4)?, content_type: row.get(5)?, size: row.get(6)?, sha256: row.get(7)?,
                created_at_ms: row.get(9)?, updated_at_ms: row.get(10)?,
            },
            local_path: row.get(8)?,
        }),
    ).optional().map_err(|e| format!("db_work_note_attachment_query_failed:{e}"))
}

pub(crate) fn list(store: &SqliteStore, tenant_id: String, page_id: String) -> Result<Vec<AttachmentRecord>, String> {
    let tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
    let page = safe_id(&page_id);
    if tenant.is_empty() { return Err("tenant_id_required".to_string()); }
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    let (sql, values) = if page.is_empty() {
        ("SELECT tenant_id,attachment_id,page_id,block_id,file_name,content_type,byte_size,sha256,created_at_ms,updated_at_ms FROM work_note_attachments WHERE tenant_id=?1 ORDER BY created_at_ms", vec![tenant])
    } else {
        ("SELECT tenant_id,attachment_id,page_id,block_id,file_name,content_type,byte_size,sha256,created_at_ms,updated_at_ms FROM work_note_attachments WHERE tenant_id=?1 AND page_id=?2 ORDER BY created_at_ms", vec![tenant, page])
    };
    let mut statement = conn.prepare(sql).map_err(|e| format!("db_work_note_attachment_prepare_failed:{e}"))?;
    let rows = statement.query_map(rusqlite::params_from_iter(values.iter()), |row| Ok(AttachmentRecord {
        tenant_id: row.get(0)?, attachment_id: row.get(1)?, page_id: row.get(2)?, block_id: row.get(3)?,
        file_name: row.get(4)?, content_type: row.get(5)?, size: row.get(6)?, sha256: row.get(7)?,
        created_at_ms: row.get(8)?, updated_at_ms: row.get(9)?,
    })).map_err(|e| format!("db_work_note_attachment_query_failed:{e}"))?;
    rows.map(|row| row.map_err(|e| format!("db_work_note_attachment_row_failed:{e}"))).collect()
}

pub(crate) fn save<R: Read + ?Sized>(
    store: &SqliteStore,
    tenant_id: String,
    attachment_id: String,
    page_id: String,
    block_id: String,
    file_name: String,
    content_type: String,
    reader: &mut R,
) -> Result<AttachmentRecord, String> {
    let tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
    let attachment = safe_id(&attachment_id);
    let page = safe_id(&page_id);
    let block = if safe_id(&block_id).is_empty() { attachment.clone() } else { safe_id(&block_id) };
    if tenant.is_empty() { return Err("tenant_id_required".to_string()); }
    if attachment.is_empty() { return Err("work_note_attachment_id_required".to_string()); }
    if page.is_empty() { return Err("work_note_page_id_required".to_string()); }
    {
        let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let exists: i64 = conn.query_row("SELECT COUNT(*) FROM work_note_pages WHERE tenant_id=?1 AND page_id=?2", params![tenant, page], |row| row.get(0)).map_err(|e| format!("db_work_note_attachment_page_check_failed:{e}"))?;
        if exists == 0 { return Err("work_note_not_found".to_string()); }
    }
    let name = safe_file_name(&file_name);
    let mime = safe_content_type(&content_type);
    let local_path = relative_path(&tenant, &attachment, &name);
    let target_path = store.data_dir.join(&local_path);
    let parent = target_path.parent().ok_or_else(|| "work_note_attachment_path_invalid".to_string())?;
    fs::create_dir_all(parent).map_err(|e| format!("work_note_attachment_dir_failed:{e}"))?;
    let suffix: String = rand::thread_rng().sample_iter(&Alphanumeric).take(16).map(char::from).collect();
    let temp_path = parent.join(format!(".{attachment}.{suffix}.tmp"));
    let mut output = File::create(&temp_path).map_err(|e| format!("work_note_attachment_file_create_failed:{e}"))?;
    let mut hash = Sha256::new();
    let mut size = 0_i64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer).map_err(|e| format!("work_note_attachment_read_failed:{e}"))?;
        if read == 0 { break; }
        output.write_all(&buffer[..read]).map_err(|e| format!("work_note_attachment_write_failed:{e}"))?;
        hash.update(&buffer[..read]);
        size += read as i64;
    }
    output.sync_all().map_err(|e| format!("work_note_attachment_sync_failed:{e}"))?;
    drop(output);
    if target_path.exists() { fs::remove_file(&target_path).map_err(|e| format!("work_note_attachment_replace_failed:{e}"))?; }
    fs::rename(&temp_path, &target_path).map_err(|e| format!("work_note_attachment_rename_failed:{e}"))?;
    let local_path_text = local_path.to_string_lossy().to_string();
    let now = Utc::now().timestamp_millis();
    let sha256 = format!("{:x}", hash.finalize());
    let result = (|| -> Result<(), String> {
        let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT INTO work_note_attachments (tenant_id,attachment_id,page_id,block_id,file_name,content_type,byte_size,sha256,local_path,created_at_ms,updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10) ON CONFLICT(tenant_id,attachment_id) DO UPDATE SET page_id=excluded.page_id,block_id=excluded.block_id,file_name=excluded.file_name,content_type=excluded.content_type,byte_size=excluded.byte_size,sha256=excluded.sha256,local_path=excluded.local_path,updated_at_ms=excluded.updated_at_ms",
            params![tenant, attachment, page, block, name, mime, size, sha256, local_path_text, now],
        ).map_err(|e| format!("db_work_note_attachment_upsert_failed:{e}"))?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&target_path);
        return Err(error);
    }
    row_for(store, &tenant, &attachment)?.map(|row| row.record).ok_or_else(|| "work_note_attachment_not_found".to_string())
}

pub(crate) fn open(store: &SqliteStore, tenant_id: String, attachment_id: String) -> Result<AttachmentFile, String> {
    let tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
    let attachment = safe_id(&attachment_id);
    if tenant.is_empty() { return Err("tenant_id_required".to_string()); }
    if attachment.is_empty() { return Err("work_note_attachment_id_required".to_string()); }
    let row = row_for(store, &tenant, &attachment)?.ok_or_else(|| "work_note_attachment_not_found".to_string())?;
    let path = checked_path(store, &row.local_path)?;
    let file = File::open(&path).map_err(|_| "work_note_attachment_file_missing".to_string())?;
    let size = file.metadata().map_err(|_| "work_note_attachment_file_missing".to_string())?.len();
    Ok(AttachmentFile { record: row.record, file, size })
}

pub(crate) fn resolve_local_path(
    store: &SqliteStore,
    tenant_id: String,
    attachment_id: String,
) -> Result<(PathBuf, String), String> {
    let tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
    let attachment = safe_id(&attachment_id);
    if tenant.is_empty() { return Err("tenant_id_required".to_string()); }
    if attachment.is_empty() { return Err("work_note_attachment_id_required".to_string()); }
    let row = row_for(store, &tenant, &attachment)?
        .ok_or_else(|| "work_note_attachment_not_found".to_string())?;
    let base = fs::canonicalize(&store.data_dir)
        .map_err(|_| "local_data_dir_missing".to_string())?;
    let target = fs::canonicalize(checked_path(store, &row.local_path)?)
        .map_err(|_| "work_note_attachment_file_missing".to_string())?;
    if !target.starts_with(&base) || !target.is_file() {
        return Err("work_note_attachment_path_invalid".to_string());
    }
    Ok((target, row.record.content_type))
}

pub(crate) fn delete(store: &SqliteStore, tenant_id: String, attachment_id: String) -> Result<usize, String> {
    let tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
    let attachment = safe_id(&attachment_id);
    if tenant.is_empty() { return Err("tenant_id_required".to_string()); }
    if attachment.is_empty() { return Err("work_note_attachment_id_required".to_string()); }
    let Some(row) = row_for(store, &tenant, &attachment)? else { return Ok(0); };
    let deleted = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?
        .execute("DELETE FROM work_note_attachments WHERE tenant_id=?1 AND attachment_id=?2", params![tenant, attachment])
        .map_err(|e| format!("db_work_note_attachment_delete_failed:{e}"))?;
    if let Ok(path) = checked_path(store, &row.local_path) { let _ = fs::remove_file(path); }
    Ok(deleted)
}

pub(crate) fn page_local_paths(store: &SqliteStore, tenant_id: &str, page_id: &str) -> Result<Vec<String>, String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    let mut statement = conn.prepare("SELECT local_path FROM work_note_attachments WHERE tenant_id=?1 AND page_id=?2").map_err(|e| format!("db_work_note_attachment_prepare_failed:{e}"))?;
    let rows = statement.query_map(params![tenant_id, page_id], |row| row.get::<_, String>(0)).map_err(|e| format!("db_work_note_attachment_query_failed:{e}"))?;
    rows.map(|row| row.map_err(|e| format!("db_work_note_attachment_row_failed:{e}"))).collect()
}

pub(crate) fn delete_local_paths(store: &SqliteStore, paths: &[String]) {
    for local_path in paths {
        if let Ok(path) = checked_path(store, local_path) { let _ = fs::remove_file(path); }
    }
}

fn add_response_headers<R: Read>(response: &mut Response<R>, origin: &str) {
    for (name, value) in [
        ("Cache-Control", "no-store"),
        ("Access-Control-Allow-Origin", origin),
        ("Access-Control-Allow-Headers", "Content-Type, Authorization, X-OnlineClass-Local-Store-Key, X-OnlineClass-Local-Browser-Token"),
        ("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS"),
        ("Access-Control-Allow-Private-Network", "true"),
    ] {
        if let Ok(header) = Header::from_bytes(name.as_bytes(), value.as_bytes()) { response.add_header(header); }
    }
}

pub(crate) fn handle_http_request(
    request: &mut Request,
    store: &SqliteStore,
    browser_links: &BrowserLinkStore,
    pairing_key: &str,
    origin: &str,
) -> Result<Option<ResponseBox>, String> {
    let url = parse_request_url(request)?;
    let path = url.path().to_string();
    if path != "/v1/work-note-attachments" && !path.starts_with("/v1/work-note-attachments/") { return Ok(None); }
    if request.method() == &Method::Options { return Ok(Some(json_response(200, serde_json::json!({ "ok": true }), origin).boxed())); }
    let (authorized, browser_tenant) = request_authority(request, pairing_key, browser_links);
    if !authorized { return Ok(Some(json_response(401, serde_json::json!({ "ok": false, "error": "unauthorized" }), origin).boxed())); }
    let tenant_id = scope_tenant_id(query(&url, "tenantId"), browser_tenant.as_deref())?;
    if request.method() == &Method::Get && path == "/v1/work-note-attachments" {
        let records = list(store, tenant_id, query(&url, "pageId"))?;
        return Ok(Some(json_response(200, serde_json::json!({ "ok": true, "records": records }), origin).boxed()));
    }
    let attachment_id = path.trim_start_matches("/v1/work-note-attachments/").to_string();
    if request.method() == &Method::Put {
        let content_type = request.headers().iter().find(|header| header.field.equiv("Content-Type"))
            .map(|header| header.value.as_str().to_string()).unwrap_or_else(|| "application/octet-stream".to_string());
        let record = save(store, tenant_id, attachment_id, query(&url, "pageId"), query(&url, "blockId"), query(&url, "fileName"), content_type, request.as_reader())?;
        return Ok(Some(json_response(200, serde_json::json!({ "ok": true, "record": record }), origin).boxed()));
    }
    if request.method() == &Method::Delete {
        let deleted = delete(store, tenant_id, attachment_id)?;
        return Ok(Some(json_response(200, serde_json::json!({ "ok": true, "deleted": deleted }), origin).boxed()));
    }
    if request.method() == &Method::Get {
        let attachment = open(store, tenant_id, attachment_id)?;
        let encoded_name: String = url::form_urlencoded::byte_serialize(attachment.record.file_name.as_bytes()).collect();
        let content_type = attachment.record.content_type.clone();
        let mut response = Response::from_file(attachment.file).with_status_code(StatusCode(200));
        if let Ok(header) = Header::from_bytes("Content-Type".as_bytes(), content_type.as_bytes()) { response.add_header(header); }
        if let Ok(header) = Header::from_bytes("Content-Disposition".as_bytes(), format!("inline; filename*=UTF-8''{encoded_name}").as_bytes()) { response.add_header(header); }
        if let Ok(header) = Header::from_bytes("Content-Length".as_bytes(), attachment.size.to_string().as_bytes()) { response.add_header(header); }
        add_response_headers(&mut response, origin);
        return Ok(Some(response.boxed()));
    }
    Ok(Some(json_response(404, serde_json::json!({ "ok": false, "error": "not_found" }), origin).boxed()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn id_and_path_guards_reject_traversal() {
        assert!(safe_id("attachment-a").len() > 0);
        assert!(safe_id("../attachment").is_empty());
        assert_eq!(safe_file_name("../../수업.pdf"), "_.._수업.pdf");
        assert!(Path::new(&relative_path("tenant-a", "attachment-a", "수업.pdf")).is_relative());
        let mut cursor = Cursor::new(Vec::<u8>::new());
        assert_eq!(cursor.read(&mut [0_u8; 1]).unwrap(), 0);
    }
}
