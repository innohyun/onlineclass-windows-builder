use crate::{json_response, normalize_id_segment, normalize_json_text, normalize_tenant_id, parse_request_url,
    query, request_authority, scope_tenant_id, BrowserLinkStore, SqliteStore};
use chrono::Utc;
use rand::{distributions::Alphanumeric, Rng};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tiny_http::{Header, Method, Request, Response, ResponseBox};

const STAGING_ROOT: &str = "work-note-localization-staging";
const STATUS_COPYING: &str = "copying";
const STATUS_VERIFIED: &str = "verified";
const STATUS_COMPLETED: &str = "completed";
const STATUS_INTERRUPTED: &str = "interrupted";

pub(crate) fn ensure_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS work_note_localization_receipts (
          tenant_id TEXT NOT NULL,
          document_id TEXT NOT NULL,
          root_page_id TEXT NOT NULL,
          document_title TEXT NOT NULL,
          prepared_revision INTEGER NOT NULL,
          snapshot_sha256 TEXT NOT NULL,
          expected_page_count INTEGER NOT NULL,
          status TEXT NOT NULL CHECK (status IN ('copying','verified','completed','interrupted')),
          verified_page_count INTEGER NOT NULL DEFAULT 0,
          verified_attachment_count INTEGER NOT NULL DEFAULT 0,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          completed_at_ms INTEGER,
          PRIMARY KEY (tenant_id, document_id)
        );
        CREATE TABLE IF NOT EXISTS work_note_localization_pages (
          tenant_id TEXT NOT NULL,
          document_id TEXT NOT NULL,
          page_id TEXT NOT NULL,
          payload_json TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, document_id, page_id),
          FOREIGN KEY (tenant_id, document_id) REFERENCES work_note_localization_receipts(tenant_id, document_id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS work_note_localization_attachments (
          tenant_id TEXT NOT NULL,
          document_id TEXT NOT NULL,
          attachment_id TEXT NOT NULL,
          page_id TEXT NOT NULL,
          block_id TEXT NOT NULL,
          file_name TEXT NOT NULL,
          content_type TEXT NOT NULL,
          byte_size INTEGER NOT NULL,
          sha256 TEXT NOT NULL,
          staging_path TEXT NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, document_id, attachment_id),
          FOREIGN KEY (tenant_id, document_id) REFERENCES work_note_localization_receipts(tenant_id, document_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_work_note_localization_receipts_tenant
          ON work_note_localization_receipts (tenant_id, status, updated_at_ms DESC);
        "#,
    ).map_err(|error| format!("db_work_note_localization_schema_failed:{error}"))
}

fn now_ms() -> i64 { Utc::now().timestamp_millis() }

fn safe_id(value: &str, max: usize) -> Result<String, String> {
    let normalized = normalize_id_segment(Some(&Value::String(value.to_string())), max);
    if normalized.is_empty() || normalized != value { return Err("work_note_localization_identity_invalid".to_string()); }
    Ok(normalized)
}

fn safe_sha(value: &str) -> Result<String, String> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.len() != 64 || !normalized.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err("work_note_localization_snapshot_invalid".to_string());
    }
    Ok(normalized)
}

fn safe_file_name(value: &str) -> String {
    let name = value.trim().chars().take(240).map(|ch| {
        if ch.is_control() || matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { ch }
    }).collect::<String>().trim_start_matches('.').to_string();
    if name.is_empty() { "첨부파일".to_string() } else { name }
}

fn safe_content_type(value: &str) -> String {
    let mime = value.trim().to_ascii_lowercase();
    if mime.contains('/') && !mime.chars().any(|ch| ch.is_control() || ch.is_whitespace()) {
        mime.chars().take(160).collect()
    } else { "application/octet-stream".to_string() }
}

fn tenant_folder(tenant_id: &str) -> String {
    format!("{:x}", Sha256::digest(tenant_id.as_bytes()))[..32].to_string()
}

fn extension(file_name: &str) -> String {
    Path::new(file_name).extension().and_then(|value| value.to_str())
        .filter(|value| !value.is_empty() && value.len() <= 15)
        .map(|value| format!(".{value}")).unwrap_or_default()
}

fn staging_relative(tenant_id: &str, document_id: &str, attachment_id: &str, file_name: &str) -> PathBuf {
    PathBuf::from(STAGING_ROOT).join(tenant_folder(tenant_id)).join(document_id)
        .join(attachment_id).join(format!("content{}", extension(file_name)))
}

fn final_relative(tenant_id: &str, attachment_id: &str, file_name: &str) -> PathBuf {
    PathBuf::from("work-note-attachments").join(tenant_folder(tenant_id)).join(attachment_id)
        .join(format!("content{}", extension(file_name)))
}

fn receipt(store: &SqliteStore, tenant_id: &str, document_id: &str) -> Result<Option<Value>, String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    conn.query_row(
        "SELECT tenant_id,document_id,root_page_id,document_title,prepared_revision,snapshot_sha256,expected_page_count,status,verified_page_count,verified_attachment_count,created_at_ms,updated_at_ms,completed_at_ms FROM work_note_localization_receipts WHERE tenant_id=?1 AND document_id=?2",
        params![tenant_id, document_id], |row| Ok(json!({
            "tenantId": row.get::<_, String>(0)?, "documentId": row.get::<_, String>(1)?,
            "rootPageId": row.get::<_, String>(2)?, "documentTitle": row.get::<_, String>(3)?,
            "preparedRevision": row.get::<_, i64>(4)?, "snapshotSha256": row.get::<_, String>(5)?,
            "expectedPageCount": row.get::<_, i64>(6)?, "status": row.get::<_, String>(7)?,
            "verifiedPageCount": row.get::<_, i64>(8)?, "verifiedAttachmentCount": row.get::<_, i64>(9)?,
            "createdAtMs": row.get::<_, i64>(10)?, "updatedAtMs": row.get::<_, i64>(11)?,
            "completedAtMs": row.get::<_, Option<i64>>(12)?,
        })),
    ).optional().map_err(|error| format!("db_work_note_localization_receipt_failed:{error}"))
}

pub(crate) fn list(store: &SqliteStore, tenant_id: String) -> Result<Vec<Value>, String> {
    let tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
    if tenant.is_empty() { return Err("tenant_id_required".to_string()); }
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    let mut statement = conn.prepare(
        "SELECT document_id,root_page_id,document_title,prepared_revision,snapshot_sha256,expected_page_count,status,verified_page_count,verified_attachment_count,created_at_ms,updated_at_ms,completed_at_ms FROM work_note_localization_receipts WHERE tenant_id=?1 ORDER BY updated_at_ms DESC,document_id",
    ).map_err(|error| format!("db_work_note_localization_list_prepare_failed:{error}"))?;
    let rows = statement.query_map(params![tenant], |row| Ok(json!({
        "tenantId": tenant, "documentId": row.get::<_, String>(0)?, "rootPageId": row.get::<_, String>(1)?,
        "documentTitle": row.get::<_, String>(2)?, "preparedRevision": row.get::<_, i64>(3)?,
        "snapshotSha256": row.get::<_, String>(4)?, "expectedPageCount": row.get::<_, i64>(5)?,
        "status": row.get::<_, String>(6)?, "verifiedPageCount": row.get::<_, i64>(7)?,
        "verifiedAttachmentCount": row.get::<_, i64>(8)?, "createdAtMs": row.get::<_, i64>(9)?,
        "updatedAtMs": row.get::<_, i64>(10)?, "completedAtMs": row.get::<_, Option<i64>>(11)?,
    }))).map_err(|error| format!("db_work_note_localization_list_failed:{error}"))?;
    rows.map(|row| row.map_err(|error| format!("db_work_note_localization_list_row_failed:{error}"))).collect()
}

pub(crate) fn begin(store: &SqliteStore, body: Value) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(body.get("tenantId"));
    let document_id = safe_id(&normalize_json_text(body.get("documentId"), 128), 128)?;
    let root_page_id = safe_id(&normalize_json_text(body.get("rootPageId"), 180), 180)?;
    let document_title = {
        let value = normalize_json_text(body.get("documentTitle"), 240);
        if value.is_empty() { "클라우드 문서".to_string() } else { value }
    };
    let prepared_revision = body.get("preparedRevision").and_then(Value::as_i64).unwrap_or(0);
    let snapshot_sha256 = safe_sha(&normalize_json_text(body.get("snapshotSha256"), 64))?;
    let expected_page_count = body.get("expectedPageCount").and_then(Value::as_i64).unwrap_or(0);
    if tenant_id.is_empty() { return Err("tenant_id_required".to_string()); }
    if prepared_revision < 1 || !(1..=5000).contains(&expected_page_count) {
        return Err("work_note_localization_manifest_invalid".to_string());
    }
    let previous = receipt(store, &tenant_id, &document_id)?;
    if let Some(previous) = &previous {
        let same_root = previous.get("rootPageId").and_then(Value::as_str) == Some(root_page_id.as_str());
        let completed = previous.get("status").and_then(Value::as_str) == Some(STATUS_COMPLETED);
        if same_root && completed { return Err("work_note_localization_already_completed".to_string()); }
        if !same_root && !completed {
            return Err("work_note_localization_identity_mismatch".to_string());
        }
    }
    let reset = previous.as_ref().is_some_and(|value| {
        value.get("rootPageId").and_then(Value::as_str) != Some(root_page_id.as_str())
          || value.get("snapshotSha256").and_then(Value::as_str).is_some_and(|value| value != snapshot_sha256)
    });
    let now = now_ms();
    {
        let mut conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let transaction = conn.transaction().map_err(|error| format!("db_work_note_localization_begin_failed:{error}"))?;
        if reset {
            transaction.execute("DELETE FROM work_note_localization_pages WHERE tenant_id=?1 AND document_id=?2", params![tenant_id, document_id])
                .map_err(|error| format!("db_work_note_localization_reset_pages_failed:{error}"))?;
            transaction.execute("DELETE FROM work_note_localization_attachments WHERE tenant_id=?1 AND document_id=?2", params![tenant_id, document_id])
                .map_err(|error| format!("db_work_note_localization_reset_attachments_failed:{error}"))?;
        }
        transaction.execute(
            "INSERT INTO work_note_localization_receipts (tenant_id,document_id,root_page_id,document_title,prepared_revision,snapshot_sha256,expected_page_count,status,created_at_ms,updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,'copying',?8,?8) ON CONFLICT(tenant_id,document_id) DO UPDATE SET root_page_id=excluded.root_page_id,document_title=excluded.document_title,prepared_revision=excluded.prepared_revision,snapshot_sha256=excluded.snapshot_sha256,expected_page_count=excluded.expected_page_count,status='copying',verified_page_count=0,verified_attachment_count=0,updated_at_ms=excluded.updated_at_ms,completed_at_ms=NULL",
            params![tenant_id, document_id, root_page_id, document_title, prepared_revision, snapshot_sha256, expected_page_count, now],
        ).map_err(|error| format!("db_work_note_localization_begin_upsert_failed:{error}"))?;
        transaction.commit().map_err(|error| format!("db_work_note_localization_begin_commit_failed:{error}"))?;
    }
    if reset {
        let _ = fs::remove_dir_all(store.data_dir.join(STAGING_ROOT).join(tenant_folder(&tenant_id)).join(&document_id));
    }
    receipt(store, &tenant_id, &document_id)?.ok_or_else(|| "work_note_localization_not_found".to_string())
}

fn normalized_page(mut body: Value, page_id: &str) -> Result<Value, String> {
    let page_id = safe_id(page_id, 180)?;
    let parent_id = normalize_id_segment(body.get("parentId"), 180);
    if parent_id == page_id { return Err("work_note_parent_cycle".to_string()); }
    let title = {
        let value = normalize_json_text(body.get("title"), 240);
        if value.is_empty() { "제목 없음".to_string() } else { value }
    };
    let emoji = {
        let value = normalize_json_text(body.get("emoji"), 16);
        if value.is_empty() { "📄".to_string() } else { value }
    };
    let position = body.get("position").and_then(Value::as_i64).unwrap_or(0).max(0);
    let properties = body.get("properties").cloned().unwrap_or_else(|| json!({}));
    let blocks = body.get("blocks").cloned().unwrap_or_else(|| json!([]));
    let markdown = body.get("markdown").and_then(Value::as_str).unwrap_or("").chars().take(2_000_000).collect::<String>();
    let updated_at_ms = body.get("updatedAtMs").and_then(Value::as_i64).filter(|value| *value > 0).unwrap_or_else(now_ms);
    let created_at_ms = body.get("createdAtMs").and_then(Value::as_i64).filter(|value| *value > 0).unwrap_or(updated_at_ms);
    let object = body.as_object_mut().ok_or_else(|| "invalid_json".to_string())?;
    object.insert("pageId".to_string(), Value::String(page_id));
    object.insert("parentId".to_string(), if parent_id.is_empty() { Value::Null } else { Value::String(parent_id) });
    object.insert("title".to_string(), Value::String(title));
    object.insert("emoji".to_string(), Value::String(emoji));
    object.insert("position".to_string(), Value::Number(position.into()));
    object.insert("properties".to_string(), properties);
    object.insert("blocks".to_string(), blocks);
    object.insert("markdown".to_string(), Value::String(markdown));
    object.insert("createdAtMs".to_string(), Value::Number(created_at_ms.into()));
    object.insert("updatedAtMs".to_string(), Value::Number(updated_at_ms.into()));
    Ok(body)
}

pub(crate) fn stage_page(store: &SqliteStore, tenant_id: String, document_id: String, page_id: String, body: Value) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
    let document_id = safe_id(&document_id, 128)?;
    let page = normalized_page(body, &page_id)?;
    let current = receipt(store, &tenant_id, &document_id)?.ok_or_else(|| "work_note_localization_not_found".to_string())?;
    if !matches!(current.get("status").and_then(Value::as_str), Some(STATUS_COPYING | STATUS_INTERRUPTED)) {
        return Err("work_note_localization_state_invalid".to_string());
    }
    let page_id = page.get("pageId").and_then(Value::as_str).unwrap_or("");
    let payload = serde_json::to_string(&page).map_err(|error| format!("work_note_localization_page_encode_failed:{error}"))?;
    let now = now_ms();
    store.conn.lock().map_err(|_| "db_lock_failed".to_string())?.execute(
        "INSERT INTO work_note_localization_pages (tenant_id,document_id,page_id,payload_json,updated_at_ms) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(tenant_id,document_id,page_id) DO UPDATE SET payload_json=excluded.payload_json,updated_at_ms=excluded.updated_at_ms",
        params![tenant_id, document_id, page_id, payload, now],
    ).map_err(|error| format!("db_work_note_localization_page_upsert_failed:{error}"))?;
    Ok(page)
}

fn stage_attachment<R: Read + ?Sized>(store: &SqliteStore, tenant_id: String, document_id: String,
    attachment_id: String, page_id: String, block_id: String, file_name: String,
    content_type: String, reader: &mut R) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
    let document_id = safe_id(&document_id, 128)?;
    let attachment_id = safe_id(&attachment_id, 180)?;
    let page_id = safe_id(&page_id, 180)?;
    let block_id = safe_id(if block_id.is_empty() { &attachment_id } else { &block_id }, 180)?;
    let current = receipt(store, &tenant_id, &document_id)?.ok_or_else(|| "work_note_localization_not_found".to_string())?;
    if !matches!(current.get("status").and_then(Value::as_str), Some(STATUS_COPYING | STATUS_INTERRUPTED)) {
        return Err("work_note_localization_state_invalid".to_string());
    }
    let file_name = safe_file_name(&file_name);
    let content_type = safe_content_type(&content_type);
    let relative = staging_relative(&tenant_id, &document_id, &attachment_id, &file_name);
    let target = store.data_dir.join(&relative);
    let parent = target.parent().ok_or_else(|| "work_note_localization_attachment_path_invalid".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("work_note_localization_attachment_dir_failed:{error}"))?;
    let suffix: String = rand::thread_rng().sample_iter(&Alphanumeric).take(16).map(char::from).collect();
    let temporary = parent.join(format!(".{attachment_id}.{suffix}.tmp"));
    let mut output = File::create(&temporary).map_err(|error| format!("work_note_localization_attachment_create_failed:{error}"))?;
    let mut hash = Sha256::new();
    let mut size = 0_i64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer).map_err(|error| format!("work_note_localization_attachment_read_failed:{error}"))?;
        if read == 0 { break; }
        output.write_all(&buffer[..read]).map_err(|error| format!("work_note_localization_attachment_write_failed:{error}"))?;
        hash.update(&buffer[..read]);
        size += read as i64;
    }
    output.sync_all().map_err(|error| format!("work_note_localization_attachment_sync_failed:{error}"))?;
    drop(output);
    if target.exists() { fs::remove_file(&target).map_err(|error| format!("work_note_localization_attachment_replace_failed:{error}"))?; }
    fs::rename(&temporary, &target).map_err(|error| format!("work_note_localization_attachment_rename_failed:{error}"))?;
    let sha256 = format!("{:x}", hash.finalize());
    let now = now_ms();
    let relative_text = relative.to_string_lossy().to_string();
    store.conn.lock().map_err(|_| "db_lock_failed".to_string())?.execute(
        "INSERT INTO work_note_localization_attachments (tenant_id,document_id,attachment_id,page_id,block_id,file_name,content_type,byte_size,sha256,staging_path,updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(tenant_id,document_id,attachment_id) DO UPDATE SET page_id=excluded.page_id,block_id=excluded.block_id,file_name=excluded.file_name,content_type=excluded.content_type,byte_size=excluded.byte_size,sha256=excluded.sha256,staging_path=excluded.staging_path,updated_at_ms=excluded.updated_at_ms",
        params![tenant_id, document_id, attachment_id, page_id, block_id, file_name, content_type, size, sha256, relative_text, now],
    ).map_err(|error| format!("db_work_note_localization_attachment_upsert_failed:{error}"))?;
    Ok(json!({ "tenantId": tenant_id, "documentId": document_id, "attachmentId": attachment_id,
        "pageId": page_id, "blockId": block_id, "fileName": file_name, "contentType": content_type,
        "size": size, "sha256": sha256, "updatedAtMs": now }))
}

pub(crate) fn list_attachments(store: &SqliteStore, tenant_id: String, document_id: String, page_id: String) -> Result<Vec<Value>, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
    let document_id = safe_id(&document_id, 128)?;
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    let (sql, values) = if page_id.is_empty() {
        ("SELECT attachment_id,page_id,block_id,file_name,content_type,byte_size,sha256,updated_at_ms FROM work_note_localization_attachments WHERE tenant_id=?1 AND document_id=?2 ORDER BY attachment_id", vec![tenant_id.clone(), document_id.clone()])
    } else {
        let page_id = safe_id(&page_id, 180)?;
        ("SELECT attachment_id,page_id,block_id,file_name,content_type,byte_size,sha256,updated_at_ms FROM work_note_localization_attachments WHERE tenant_id=?1 AND document_id=?2 AND page_id=?3 ORDER BY attachment_id", vec![tenant_id.clone(), document_id.clone(), page_id])
    };
    let mut statement = conn.prepare(sql).map_err(|error| format!("db_work_note_localization_attachment_list_prepare_failed:{error}"))?;
    let rows = statement.query_map(params_from_iter(values.iter()), |row| Ok(json!({
        "tenantId": tenant_id, "documentId": document_id, "attachmentId": row.get::<_, String>(0)?,
        "pageId": row.get::<_, String>(1)?, "blockId": row.get::<_, String>(2)?, "fileName": row.get::<_, String>(3)?,
        "contentType": row.get::<_, String>(4)?, "size": row.get::<_, i64>(5)?, "sha256": row.get::<_, String>(6)?,
        "updatedAtMs": row.get::<_, i64>(7)?,
    }))).map_err(|error| format!("db_work_note_localization_attachment_list_failed:{error}"))?;
    rows.map(|row| row.map_err(|error| format!("db_work_note_localization_attachment_list_row_failed:{error}"))).collect()
}

fn staged_pages(store: &SqliteStore, tenant_id: &str, document_id: &str) -> Result<Vec<Value>, String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    let mut statement = conn.prepare("SELECT payload_json FROM work_note_localization_pages WHERE tenant_id=?1 AND document_id=?2 ORDER BY page_id")
        .map_err(|error| format!("db_work_note_localization_pages_prepare_failed:{error}"))?;
    let rows = statement.query_map(params![tenant_id, document_id], |row| row.get::<_, String>(0))
        .map_err(|error| format!("db_work_note_localization_pages_failed:{error}"))?;
    rows.map(|row| {
        let raw = row.map_err(|error| format!("db_work_note_localization_page_row_failed:{error}"))?;
        serde_json::from_str(&raw).map_err(|error| format!("db_work_note_localization_page_decode_failed:{error}"))
    }).collect()
}

pub(crate) fn verify(store: &SqliteStore, body: Value) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(body.get("tenantId"));
    let document_id = safe_id(&normalize_json_text(body.get("documentId"), 128), 128)?;
    let current = receipt(store, &tenant_id, &document_id)?.ok_or_else(|| "work_note_localization_not_found".to_string())?;
    let root_page_id = current.get("rootPageId").and_then(Value::as_str).unwrap_or("");
    let expected_count = current.get("expectedPageCount").and_then(Value::as_i64).unwrap_or(0) as usize;
    let pages = staged_pages(store, &tenant_id, &document_id)?;
    if pages.len() != expected_count { return Err("work_note_localization_page_count_mismatch".to_string()); }
    let ids = pages.iter().filter_map(|page| page.get("pageId").and_then(Value::as_str)).collect::<HashSet<_>>();
    if ids.len() != pages.len() || !ids.contains(root_page_id) { return Err("work_note_localization_tree_mismatch".to_string()); }
    for page in &pages {
        let page_id = page.get("pageId").and_then(Value::as_str).unwrap_or("");
        let parent_id = page.get("parentId").and_then(Value::as_str);
        if (page_id == root_page_id && parent_id.is_some()) || (page_id != root_page_id && parent_id.is_none_or(|id| !ids.contains(id))) {
            return Err("work_note_localization_tree_mismatch".to_string());
        }
    }
    let expected_attachments = body.get("attachments").and_then(Value::as_array).cloned().unwrap_or_default();
    let actual_attachments = list_attachments(store, tenant_id.clone(), document_id.clone(), String::new())?;
    if expected_attachments.len() != actual_attachments.len() { return Err("work_note_localization_attachment_count_mismatch".to_string()); }
    if actual_attachments.iter().any(|item| item.get("pageId").and_then(Value::as_str).is_none_or(|page_id| !ids.contains(page_id))) {
        return Err("work_note_localization_attachment_mismatch".to_string());
    }
    let actual_by_id = actual_attachments.iter().filter_map(|item| item.get("attachmentId").and_then(Value::as_str).map(|id| (id, item))).collect::<HashMap<_, _>>();
    for expected in &expected_attachments {
        let attachment_id = expected.get("attachmentId").and_then(Value::as_str).unwrap_or("");
        let actual = actual_by_id.get(attachment_id).ok_or_else(|| "work_note_localization_attachment_mismatch".to_string())?;
        for key in ["pageId", "blockId", "sha256", "size"] {
            if actual.get(key) != expected.get(key) { return Err("work_note_localization_attachment_mismatch".to_string()); }
        }
    }
    let now = now_ms();
    store.conn.lock().map_err(|_| "db_lock_failed".to_string())?.execute(
        "UPDATE work_note_localization_receipts SET status='verified',verified_page_count=?3,verified_attachment_count=?4,updated_at_ms=?5 WHERE tenant_id=?1 AND document_id=?2 AND status IN ('copying','interrupted','verified')",
        params![tenant_id, document_id, pages.len() as i64, actual_attachments.len() as i64, now],
    ).map_err(|error| format!("db_work_note_localization_verify_failed:{error}"))?;
    Ok(json!({ "receipt": receipt(store, &tenant_id, &document_id)?, "pages": pages, "attachments": actual_attachments }))
}

#[derive(Clone)]
struct StagedAttachment {
    attachment_id: String,
    page_id: String,
    block_id: String,
    file_name: String,
    content_type: String,
    size: i64,
    sha256: String,
    staging_path: String,
}

fn staged_attachments(store: &SqliteStore, tenant_id: &str, document_id: &str) -> Result<Vec<StagedAttachment>, String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    let mut statement = conn.prepare("SELECT attachment_id,page_id,block_id,file_name,content_type,byte_size,sha256,staging_path FROM work_note_localization_attachments WHERE tenant_id=?1 AND document_id=?2 ORDER BY attachment_id")
        .map_err(|error| format!("db_work_note_localization_finalize_attachment_prepare_failed:{error}"))?;
    let rows = statement.query_map(params![tenant_id, document_id], |row| Ok(StagedAttachment {
        attachment_id: row.get(0)?, page_id: row.get(1)?, block_id: row.get(2)?, file_name: row.get(3)?,
        content_type: row.get(4)?, size: row.get(5)?, sha256: row.get(6)?, staging_path: row.get(7)?,
    })).map_err(|error| format!("db_work_note_localization_finalize_attachment_failed:{error}"))?;
    rows.map(|row| row.map_err(|error| format!("db_work_note_localization_finalize_attachment_row_failed:{error}"))).collect()
}

struct PublishedFile { target: PathBuf, previous: Option<PathBuf> }

fn publish_files(store: &SqliteStore, tenant_id: &str, attachments: &[StagedAttachment]) -> Result<Vec<PublishedFile>, String> {
    let mut published = Vec::new();
    for attachment in attachments {
        let result = (|| -> Result<PublishedFile, String> {
            let source = store.data_dir.join(&attachment.staging_path);
            let (size, sha256) = crate::backup::sha256_file(&source)?;
            if size != attachment.size as u64 || sha256 != attachment.sha256 {
                return Err("work_note_localization_attachment_mismatch".to_string());
            }
            let target = store.data_dir.join(final_relative(tenant_id, &attachment.attachment_id, &attachment.file_name));
            let parent = target.parent().ok_or_else(|| "work_note_localization_attachment_path_invalid".to_string())?;
            fs::create_dir_all(parent).map_err(|error| format!("work_note_localization_finalize_dir_failed:{error}"))?;
            let suffix: String = rand::thread_rng().sample_iter(&Alphanumeric).take(16).map(char::from).collect();
            let candidate = parent.join(format!(".{}.{}.candidate", attachment.attachment_id, suffix));
            if let Err(error) = fs::copy(&source, &candidate) {
                let _ = fs::remove_file(&candidate);
                return Err(format!("work_note_localization_finalize_copy_failed:{error}"));
            }
            let previous = if target.exists() {
                let previous = parent.join(format!(".{}.{}.previous", attachment.attachment_id, suffix));
                if let Err(error) = fs::rename(&target, &previous) {
                    let _ = fs::remove_file(&candidate);
                    return Err(format!("work_note_localization_finalize_previous_failed:{error}"));
                }
                Some(previous)
            } else { None };
            if let Err(error) = fs::rename(&candidate, &target) {
                let _ = fs::remove_file(&candidate);
                if let Some(previous) = &previous { let _ = fs::rename(previous, &target); }
                return Err(format!("work_note_localization_finalize_publish_failed:{error}"));
            }
            Ok(PublishedFile { target, previous })
        })();
        match result { Ok(item) => published.push(item), Err(error) => { rollback_files(&published); return Err(error); } }
    }
    Ok(published)
}

fn rollback_files(published: &[PublishedFile]) {
    for item in published.iter().rev() {
        let _ = fs::remove_file(&item.target);
        if let Some(previous) = &item.previous { let _ = fs::rename(previous, &item.target); }
    }
}

pub(crate) fn finalize(store: &SqliteStore, tenant_id: String, document_id: String) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
    let document_id = safe_id(&document_id, 128)?;
    let current = receipt(store, &tenant_id, &document_id)?.ok_or_else(|| "work_note_localization_not_found".to_string())?;
    if current.get("status").and_then(Value::as_str) == Some(STATUS_COMPLETED) {
        return Ok(json!({ "ok": true, "replayed": true, "receipt": current }));
    }
    if current.get("status").and_then(Value::as_str) != Some(STATUS_VERIFIED) {
        return Err("work_note_localization_not_verified".to_string());
    }
    let pages = staged_pages(store, &tenant_id, &document_id)?;
    let attachments = staged_attachments(store, &tenant_id, &document_id)?;
    let published = publish_files(store, &tenant_id, &attachments)?;
    let now = now_ms();
    let result = (|| -> Result<(), String> {
        let mut conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let transaction = conn.transaction().map_err(|error| format!("db_work_note_localization_finalize_begin_failed:{error}"))?;
        for page in &pages {
            let page_id = page.get("pageId").and_then(Value::as_str).unwrap_or("");
            let parent_id = page.get("parentId").and_then(Value::as_str);
            let title = page.get("title").and_then(Value::as_str).unwrap_or("제목 없음");
            let emoji = page.get("emoji").and_then(Value::as_str).unwrap_or("📄");
            let position = page.get("position").and_then(Value::as_i64).unwrap_or(0);
            let properties = serde_json::to_string(page.get("properties").unwrap_or(&Value::Null)).map_err(|error| format!("work_note_properties_encode_failed:{error}"))?;
            let blocks = serde_json::to_string(page.get("blocks").unwrap_or(&Value::Null)).map_err(|error| format!("work_note_document_encode_failed:{error}"))?;
            let markdown = page.get("markdown").and_then(Value::as_str).unwrap_or("");
            let created_at_ms = page.get("createdAtMs").and_then(Value::as_i64).unwrap_or(now);
            let updated_at_ms = page.get("updatedAtMs").and_then(Value::as_i64).unwrap_or(now);
            transaction.execute(
                "INSERT INTO work_note_pages (tenant_id,page_id,parent_id,title,emoji,position,properties_json,document_json,markdown,created_at_ms,updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,COALESCE((SELECT created_at_ms FROM work_note_pages WHERE tenant_id=?1 AND page_id=?2),?10),?11) ON CONFLICT(tenant_id,page_id) DO UPDATE SET parent_id=excluded.parent_id,title=excluded.title,emoji=excluded.emoji,position=excluded.position,properties_json=excluded.properties_json,document_json=excluded.document_json,markdown=excluded.markdown,updated_at_ms=excluded.updated_at_ms",
                params![tenant_id, page_id, parent_id, title, emoji, position, properties, blocks, markdown, created_at_ms, updated_at_ms],
            ).map_err(|error| format!("db_work_note_localization_finalize_page_failed:{error}"))?;
            transaction.execute("DELETE FROM work_note_pages_fts WHERE tenant_id=?1 AND page_id=?2", params![tenant_id, page_id])
                .map_err(|error| format!("db_work_note_localization_finalize_fts_delete_failed:{error}"))?;
            transaction.execute("INSERT INTO work_note_pages_fts (tenant_id,page_id,title,markdown) VALUES (?1,?2,?3,?4)", params![tenant_id, page_id, title, markdown])
                .map_err(|error| format!("db_work_note_localization_finalize_fts_insert_failed:{error}"))?;
        }
        for attachment in &attachments {
            let relative = final_relative(&tenant_id, &attachment.attachment_id, &attachment.file_name).to_string_lossy().to_string();
            transaction.execute(
                "INSERT INTO work_note_attachments (tenant_id,attachment_id,page_id,block_id,file_name,content_type,byte_size,sha256,local_path,created_at_ms,updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10) ON CONFLICT(tenant_id,attachment_id) DO UPDATE SET page_id=excluded.page_id,block_id=excluded.block_id,file_name=excluded.file_name,content_type=excluded.content_type,byte_size=excluded.byte_size,sha256=excluded.sha256,local_path=excluded.local_path,updated_at_ms=excluded.updated_at_ms",
                params![tenant_id, attachment.attachment_id, attachment.page_id, attachment.block_id, attachment.file_name, attachment.content_type, attachment.size, attachment.sha256, relative, now],
            ).map_err(|error| format!("db_work_note_localization_finalize_attachment_failed:{error}"))?;
        }
        let changed = transaction.execute(
            "UPDATE work_note_localization_receipts SET status='completed',completed_at_ms=?3,updated_at_ms=?3 WHERE tenant_id=?1 AND document_id=?2 AND status='verified'",
            params![tenant_id, document_id, now],
        ).map_err(|error| format!("db_work_note_localization_finalize_receipt_failed:{error}"))?;
        if changed != 1 { return Err("work_note_localization_not_verified".to_string()); }
        transaction.commit().map_err(|error| format!("db_work_note_localization_finalize_commit_failed:{error}"))?;
        Ok(())
    })();
    if let Err(error) = result {
        rollback_files(&published);
        return Err(error);
    }
    for item in &published { if let Some(previous) = &item.previous { let _ = fs::remove_file(previous); } }
    Ok(json!({ "ok": true, "replayed": false, "pageCount": pages.len(), "attachmentCount": attachments.len(),
        "receipt": receipt(store, &tenant_id, &document_id)? }))
}

pub(crate) fn cancel(store: &SqliteStore, tenant_id: String, document_id: String) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
    let document_id = safe_id(&document_id, 128)?;
    let now = now_ms();
    store.conn.lock().map_err(|_| "db_lock_failed".to_string())?.execute(
        "UPDATE work_note_localization_receipts SET status='interrupted',updated_at_ms=?3 WHERE tenant_id=?1 AND document_id=?2 AND status!='completed'",
        params![tenant_id, document_id, now],
    ).map_err(|error| format!("db_work_note_localization_cancel_failed:{error}"))?;
    receipt(store, &tenant_id, &document_id)?.ok_or_else(|| "work_note_localization_not_found".to_string())
}

fn add_response_headers<R: Read>(response: &mut Response<R>, origin: &str) {
    for (name, value) in [
        ("Cache-Control", "no-store"), ("Access-Control-Allow-Origin", origin),
        ("Access-Control-Allow-Headers", "Content-Type, Authorization, X-OnlineClass-Local-Store-Key, X-OnlineClass-Local-Browser-Token"),
        ("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS"),
        ("Access-Control-Allow-Private-Network", "true"),
    ] {
        if let Ok(header) = Header::from_bytes(name.as_bytes(), value.as_bytes()) { response.add_header(header); }
    }
}

pub(crate) fn handle_http_attachment(request: &mut Request, store: &SqliteStore, browser_links: &BrowserLinkStore,
    pairing_key: &str, origin: &str) -> Result<Option<ResponseBox>, String> {
    let url = parse_request_url(request)?;
    let parts = url.path().trim_matches('/').split('/').collect::<Vec<_>>();
    if parts.len() < 4 || parts[0] != "v1" || parts[1] != "work-note-localizations" || parts[3] != "attachments" {
        return Ok(None);
    }
    if request.method() == &Method::Options { return Ok(Some(json_response(200, json!({ "ok": true }), origin).boxed())); }
    let (authorized, browser_tenant) = request_authority(request, pairing_key, browser_links);
    if !authorized { return Ok(Some(json_response(401, json!({ "ok": false, "error": "unauthorized" }), origin).boxed())); }
    let tenant_id = scope_tenant_id(query(&url, "tenantId"), browser_tenant.as_deref())?;
    let document_id = parts[2].to_string();
    if parts.len() == 4 && request.method() == &Method::Get {
        let records = list_attachments(store, tenant_id, document_id, query(&url, "pageId"))?;
        return Ok(Some(json_response(200, json!({ "ok": true, "records": records }), origin).boxed()));
    }
    if parts.len() == 5 && request.method() == &Method::Put {
        let content_type = request.headers().iter().find(|header| header.field.equiv("Content-Type"))
            .map(|header| header.value.as_str().to_string()).unwrap_or_else(|| "application/octet-stream".to_string());
        let record = stage_attachment(store, tenant_id, document_id, parts[4].to_string(), query(&url, "pageId"),
            query(&url, "blockId"), query(&url, "fileName"), content_type, request.as_reader())?;
        return Ok(Some(json_response(200, json!({ "ok": true, "record": record }), origin).boxed()));
    }
    let mut response = json_response(404, json!({ "ok": false, "error": "not_found" }), origin);
    add_response_headers(&mut response, origin);
    Ok(Some(response.boxed()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn verified_pages_are_hidden_until_atomic_finalize_and_replay_is_safe() {
        let directory = std::env::temp_dir().join(format!("classaimate-localization-{}", crate::random_url_token()));
        fs::create_dir_all(&directory).unwrap();
        let db_path = directory.join("store.sqlite3");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE work_note_pages (tenant_id TEXT NOT NULL,page_id TEXT NOT NULL,parent_id TEXT,title TEXT NOT NULL,emoji TEXT NOT NULL,position INTEGER NOT NULL,properties_json TEXT NOT NULL,document_json TEXT NOT NULL,markdown TEXT NOT NULL,created_at_ms INTEGER NOT NULL,updated_at_ms INTEGER NOT NULL,PRIMARY KEY(tenant_id,page_id));
             CREATE VIRTUAL TABLE work_note_pages_fts USING fts5(tenant_id UNINDEXED,page_id UNINDEXED,title,markdown);
             CREATE TABLE work_note_attachments (tenant_id TEXT NOT NULL,attachment_id TEXT NOT NULL,page_id TEXT NOT NULL,block_id TEXT NOT NULL,file_name TEXT NOT NULL,content_type TEXT NOT NULL,byte_size INTEGER NOT NULL,sha256 TEXT NOT NULL,local_path TEXT NOT NULL,created_at_ms INTEGER NOT NULL,updated_at_ms INTEGER NOT NULL,PRIMARY KEY(tenant_id,attachment_id));",
        ).unwrap();
        ensure_schema(&conn).unwrap();
        let store = SqliteStore { conn: Mutex::new(conn), db_path, data_dir: directory.clone() };
        begin(&store, json!({ "tenantId":"tenant-a","documentId":"document-a","rootPageId":"root-a",
            "documentTitle":"참고자료","preparedRevision":2,"snapshotSha256":"a".repeat(64),"expectedPageCount":2 })).unwrap();
        for page in [
            json!({"pageId":"root-a","parentId":null,"title":"참고자료","emoji":"📚","position":0,"properties":{},"blocks":[{"id":"b1","type":"text","text":"원문"}],"markdown":"원문"}),
            json!({"pageId":"child-a","parentId":"root-a","title":"개인정보","emoji":"🔒","position":0,"properties":{},"blocks":[],"markdown":""}),
        ] { stage_page(&store, "tenant-a".into(), "document-a".into(), page["pageId"].as_str().unwrap().into(), page).unwrap(); }
        verify(&store, json!({"tenantId":"tenant-a","documentId":"document-a","attachments":[]})).unwrap();
        let before: i64 = store.conn.lock().unwrap().query_row("SELECT COUNT(*) FROM work_note_pages", [], |row| row.get(0)).unwrap();
        assert_eq!(before, 0);
        assert_eq!(finalize(&store, "tenant-a".into(), "document-a".into()).unwrap()["replayed"], false);
        assert_eq!(finalize(&store, "tenant-a".into(), "document-a".into()).unwrap()["replayed"], true);
        let after: i64 = store.conn.lock().unwrap().query_row("SELECT COUNT(*) FROM work_note_pages", [], |row| row.get(0)).unwrap();
        assert_eq!(after, 2);
        assert_eq!(begin(&store, json!({ "tenantId":"tenant-a","documentId":"document-a","rootPageId":"root-a",
            "documentTitle":"참고자료","preparedRevision":4,"snapshotSha256":"b".repeat(64),"expectedPageCount":1 })).unwrap_err(),
            "work_note_localization_already_completed");
        begin(&store, json!({ "tenantId":"tenant-a","documentId":"document-a","rootPageId":"sibling-a",
            "documentTitle":"다른 참고자료","preparedRevision":5,"snapshotSha256":"c".repeat(64),"expectedPageCount":1 })).unwrap();
        let sibling = json!({"pageId":"sibling-a","parentId":null,"title":"다른 참고자료","emoji":"📄","position":0,
            "properties":{},"blocks":[{"id":"b3","type":"text","text":"형제 원문"}],"markdown":"형제 원문"});
        stage_page(&store, "tenant-a".into(), "document-a".into(), "sibling-a".into(), sibling).unwrap();
        verify(&store, json!({"tenantId":"tenant-a","documentId":"document-a","attachments":[]})).unwrap();
        finalize(&store, "tenant-a".into(), "document-a".into()).unwrap();
        let preserved: i64 = store.conn.lock().unwrap().query_row(
            "SELECT COUNT(*) FROM work_note_pages WHERE tenant_id='tenant-a' AND page_id IN ('root-a','child-a','sibling-a')",
            [], |row| row.get(0)).unwrap();
        assert_eq!(preserved, 3);
        drop(store);
        fs::remove_dir_all(directory).unwrap();
    }
}
