use crate::SqliteStore;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_BATCH_BYTES: u64 = 100 * 1024 * 1024;
fn invalid() -> String {
    "classaimate_mcp_write_job_invalid".into()
}
fn db(error: rusqlite::Error) -> String {
    format!("db_mcp_material_assets_failed:{error}")
}
fn io(_error: std::io::Error) -> String {
    "IMAGE_LOCAL_FILE_FAILED".into()
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn id(value: &str, page: bool) -> bool {
    !value.is_empty()
        && value.len() <= 180
        && value.starts_with(|c: char| c.is_ascii_alphanumeric())
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') || (page && c == ':')
        })
}
fn extension(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}
fn signature(bytes: &[u8], mime: &str) -> bool {
    match mime {
        "image/png" => bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]),
        "image/jpeg" => bytes.starts_with(&[255, 216, 255]),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP",
        _ => false,
    }
}
fn nodes<'a>(value: &'a Value, output: &mut Vec<&'a Value>) {
    if let Some(rows) = value.as_array() {
        for child in rows {
            nodes(child, output);
        }
    } else if let Some(object) = value.as_object() {
        output.push(value);
        for child in object.values() {
            nodes(child, output);
        }
    }
}

pub(super) fn validate(data: &Value) -> Result<(), String> {
    let mode = text(data, "mode");
    let title = text(data, "title");
    let revision = data["expectedRevision"].as_i64().unwrap_or(0);
    let blocks = data["blocks"].as_array().ok_or_else(invalid)?;
    let attachments = data["attachments"].as_array().ok_or_else(invalid)?;
    if data.get("appendText").is_some_and(|value| {
        !value.is_string() || value.as_str().unwrap_or("").encode_utf16().count() > 200_000
    }) {
        return Err(invalid());
    }
    if !matches!(mode, "create" | "append")
        || !matches!(
            text(data, "workspace"),
            "work_materials" | "lesson_materials"
        )
        || !id(text(data, "pageRef"), true)
        || title.trim().is_empty()
        || title.encode_utf16().count() > 240
        || !data["markdown"].is_string()
        || text(data, "markdown").encode_utf16().count() > 200_000
        || blocks.len() > 5_000
        || !data["properties"].is_object()
        || attachments.is_empty()
        || attachments.len() > 12
        || revision <= 0
    {
        return Err(invalid());
    }
    if mode == "create" {
        let page = text(data, "pageRef");
        if !id(text(data, "parentPageRef"), true)
            || data["parentPageRef"] == data["pageRef"]
            || data["parentRevision"].as_i64() != Some(revision)
            || !title.starts_with("ChatGPT 초안 ·")
            || page.len() != 36
            || !page.starts_with("mcp_")
            || !page[4..]
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        {
            return Err(invalid());
        }
        let mut properties = json!({"sourceType":"classAimatePublicMcp","sourceLabel":"내 ChatGPT","teacherReviewRequired":true});
        if data["properties"].get("tags").is_some() {
            properties["tags"] = json!([]);
        }
        if data["properties"] != properties {
            return Err(invalid());
        }
    }
    let mut ids = HashSet::new();
    let mut block_ids = HashSet::new();
    let mut total = 0_u64;
    let mut all_nodes = Vec::new();
    nodes(&data["blocks"], &mut all_nodes);
    for asset in attachments {
        let asset_id = text(asset, "assetId");
        let block_id = text(asset, "blockId");
        let name = text(asset, "fileName");
        let hash = text(asset, "sha256");
        let size = asset["size"].as_u64().unwrap_or(0);
        if !id(asset_id, false)
            || !id(block_id, false)
            || !ids.insert(asset_id)
            || !block_ids.insert(block_id)
            || name.trim().is_empty()
            || name.encode_utf16().count() > 240
            || name.chars().any(char::is_control)
            || extension(text(asset, "contentType")).is_none()
            || size == 0
            || size > MAX_FILE_BYTES
            || hash.len() != 64
            || !hash
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
            || text(asset, "objectKey").is_empty()
            || text(asset, "objectKey").len() > 1024
        {
            return Err(invalid());
        }
        total += size;
        let matching: Vec<_> = all_nodes
            .iter()
            .filter(|node| text(node, "id") == block_id)
            .collect();
        if matching.len() != 1 {
            return Err("MATERIAL_REFERENCE_CONFLICT".into());
        }
        let block = matching[0];
        if text(block, "type") != "attachment"
            || text(block, "kind") != "image"
            || text(block, "attachmentId") != asset_id
            || block["fileName"] != asset["fileName"]
            || block["contentType"] != asset["contentType"]
            || block["size"] != asset["size"]
        {
            return Err("MATERIAL_REFERENCE_CONFLICT".into());
        }
    }
    if total > MAX_BATCH_BYTES {
        return Err("IMAGE_TOO_LARGE".into());
    }
    let canonical = serde_json::to_vec(&crate::canonicalize_json(&json!({"properties":data["properties"],"blocks":data["blocks"],"markdown":data["markdown"]}))).map_err(|_| invalid())?;
    if sha(&canonical) != text(data, "contentSha256") {
        return Err("IMAGE_INTEGRITY_FAILED".into());
    }
    Ok(())
}

fn file_path(
    data_dir: &Path,
    tenant: &str,
    asset: &Value,
    prepare: bool,
) -> Result<PathBuf, String> {
    let mut directory = fs::canonicalize(data_dir).map_err(io)?;
    for part in [
        "work-note-attachments",
        &sha(tenant.as_bytes())[..32],
        text(asset, "assetId"),
    ] {
        directory = directory.join(part);
        if prepare {
            match fs::create_dir(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(io(error)),
            }
        }
        let meta = fs::symlink_metadata(&directory).map_err(io)?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err("IMAGE_SOURCE_NOT_ALLOWED".into());
        }
    }
    Ok(directory.join(format!(
        "content.{}",
        extension(text(asset, "contentType")).ok_or_else(invalid)?
    )))
}
fn check_file(path: &Path, asset: &Value) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(io)?;
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || Some(meta.len()) != asset["size"].as_u64()
    {
        return Err("IMAGE_INTEGRITY_FAILED".into());
    }
    let bytes = fs::read(path).map_err(io)?;
    if sha(&bytes) != text(asset, "sha256") || !signature(&bytes, text(asset, "contentType")) {
        return Err("IMAGE_INTEGRITY_FAILED".into());
    }
    Ok(())
}
fn attachment_exists(conn: &Connection, tenant: &str, asset: &str) -> Result<bool, String> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM work_note_attachments WHERE tenant_id=?1 AND attachment_id=?2)", params![tenant, asset], |row| row.get(0)).map_err(db)
}
pub(super) struct PreparedFile {
    pub(super) asset: Value,
    path: PathBuf,
    pub(super) relative_path: String,
    created: bool,
}
pub(super) fn cleanup(conn: &Connection, tenant: &str, prepared: &[PreparedFile]) {
    for file in prepared {
        if file.created
            && matches!(
                attachment_exists(conn, tenant, text(&file.asset, "assetId")),
                Ok(false)
            )
        {
            let _ = fs::remove_file(&file.path);
        }
    }
}
pub(super) fn prepare(
    store: &SqliteStore,
    tenant: &str,
    data: &Value,
    assets: &HashMap<String, Vec<u8>>,
) -> Result<Vec<PreparedFile>, String> {
    let attachments = data["attachments"].as_array().ok_or_else(invalid)?;
    {
        let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
        // No files are staged unless every member of this document is validated.
        for asset in attachments {
            let bytes = assets
                .get(text(asset, "assetId"))
                .ok_or("IMAGE_INTEGRITY_FAILED")?;
            if Some(bytes.len() as u64) != asset["size"].as_u64()
                || sha(bytes) != text(asset, "sha256")
                || !signature(bytes, text(asset, "contentType"))
            {
                return Err("IMAGE_INTEGRITY_FAILED".into());
            }
            if attachment_exists(&conn, tenant, text(asset, "assetId"))? {
                return Err("MATERIAL_REFERENCE_CONFLICT".into());
            }
        }
    }
    let mut prepared = Vec::new();
    let result = (|| {
        for asset in attachments {
            let path = file_path(&store.data_dir, tenant, asset, true)?;
            let mut created = false;
            if path.exists() {
                check_file(&path, asset)?;
            } else {
                let temp = path.with_extension(format!("{}.tmp", crate::random_url_token()));
                let staged = (|| {
                    let mut file = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&temp)
                        .map_err(io)?;
                    file.write_all(&assets[text(asset, "assetId")])
                        .map_err(io)?;
                    file.sync_all().map_err(io)?;
                    drop(file);
                    match fs::hard_link(&temp, &path) {
                        Ok(()) => created = true,
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                            check_file(&path, asset)?
                        }
                        Err(error) => return Err(io(error)),
                    }
                    Ok::<(), String>(())
                })();
                let _ = fs::remove_file(&temp);
                staged?;
            }
            let root = fs::canonicalize(&store.data_dir).map_err(io)?;
            let relative_path = path
                .strip_prefix(root)
                .map_err(|_| "IMAGE_SOURCE_NOT_ALLOWED")?
                .to_string_lossy()
                .replace('\\', "/");
            prepared.push(PreparedFile {
                asset: asset.clone(),
                path,
                relative_path,
                created,
            });
        }
        Ok::<(), String>(())
    })();
    if let Err(error) = result {
        if let Ok(conn) = store.conn.lock() {
            cleanup(&conn, tenant, &prepared);
        }
        return Err(error);
    }
    Ok(prepared)
}

fn page(conn: &Connection, tenant: &str, page_id: &str) -> Result<Option<Value>, String> {
    let raw: Option<(String, String, String, String, Option<String>, i64)> = conn.query_row(
        "SELECT title,markdown,document_json,properties_json,parent_id,updated_at_ms FROM work_note_pages WHERE tenant_id=?1 AND page_id=?2",
        params![tenant, page_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
    ).optional().map_err(db)?;
    raw.map(|(title, markdown, blocks, properties, parent_id, revision)| Ok(json!({"title":title,"markdown":markdown,
        "blocks":serde_json::from_str::<Value>(&blocks).map_err(|_| invalid())?,"properties":serde_json::from_str::<Value>(&properties).map_err(|_| invalid())?,
        "parentId":parent_id,"updatedAtMs":revision}))).transpose()
}
pub(super) fn verify(
    conn: &Connection,
    data_dir: &Path,
    tenant: &str,
    data: &Value,
) -> Result<(), String> {
    validate(data)?;
    verify_content(conn, tenant, data)?;
    verify_files(conn, data_dir, tenant, data)
}

fn verify_content(conn: &Connection, tenant: &str, data: &Value) -> Result<(), String> {
    let page = page(conn, tenant, text(data, "pageRef"))?.ok_or("LOCAL_STORE_WRITE_FAILED")?;
    if ["title", "markdown", "blocks", "properties"]
        .iter()
        .any(|key| page[key] != data[key])
        || (text(data, "mode") == "create" && page["parentId"] != data["parentPageRef"])
        || (text(data, "mode") == "append"
            && page["updatedAtMs"].as_i64() <= data["expectedRevision"].as_i64())
    {
        return Err("LOCAL_STORE_WRITE_FAILED".into());
    }
    Ok(())
}

pub(super) fn verify_files(
    conn: &Connection,
    data_dir: &Path,
    tenant: &str,
    data: &Value,
) -> Result<(), String> {
    validate(data)?;
    verify_file_references(conn, data_dir, tenant, data)
}

pub(crate) fn verify_file_references(conn: &Connection, data_dir: &Path, tenant: &str, data: &Value) -> Result<(), String> {
    if !id(text(data, "pageRef"), true) || data["attachments"].as_array().is_none_or(|rows| rows.len() > 12) {
        return Err(invalid());
    }
    for asset in data["attachments"].as_array().ok_or_else(invalid)? {
        if !id(text(asset, "assetId"), false) || extension(text(asset, "contentType")).is_none() { return Err(invalid()); }
        let row: Option<(String, String, String, String, u64, String, String)> = conn.query_row(
            "SELECT page_id,block_id,file_name,content_type,byte_size,sha256,local_path FROM work_note_attachments WHERE tenant_id=?1 AND attachment_id=?2",
            params![tenant, text(asset, "assetId")], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?)),
        ).optional().map_err(db)?;
        let row = row.ok_or("LOCAL_STORE_WRITE_FAILED")?;
        let path = file_path(data_dir, tenant, asset, false)?;
        let root = fs::canonicalize(data_dir).map_err(io)?;
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "IMAGE_SOURCE_NOT_ALLOWED")?
            .to_string_lossy()
            .replace('\\', "/");
        if row.0 != text(data, "pageRef")
            || row.1 != text(asset, "blockId")
            || row.2 != text(asset, "fileName")
            || row.3 != text(asset, "contentType")
            || Some(row.4) != asset["size"].as_u64()
            || row.5 != text(asset, "sha256")
            || row.6 != relative
        {
            return Err("LOCAL_STORE_WRITE_FAILED".into());
        }
        check_file(&path, asset)?;
    }
    Ok(())
}

pub(super) fn apply(
    store: &SqliteStore,
    input: &Value,
    assets: &HashMap<String, Vec<u8>>,
) -> Result<Value, String> {
    let data = &input["data"];
    validate(data)?;
    let tenant = text(input, "tenantId");
    let page_id = text(data, "pageRef");
    let prepared = prepare(store, tenant, data, assets)?;
    let mut conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    let result = (|| {
        let transaction = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(db)?;
        let existing = page(&transaction, tenant, page_id)?;
        let mut expected = data.clone();
        if text(data, "mode") == "append" {
            let page = existing.as_ref().ok_or("MATERIAL_REVISION_CONFLICT")?;
            let original = text(page, "markdown");
            let delta = text(data, "appendText").trim();
            expected["properties"] = page["properties"].clone();
            expected["markdown"] = json!(if delta.is_empty() {
                original.to_string()
            } else if original.is_empty() {
                delta.to_string()
            } else {
                format!("{original}\n\n{delta}")
            });
            if text(&expected, "markdown").encode_utf16().count() > 2_000_000 {
                return Err("MATERIAL_CONTENT_TOO_LARGE".into());
            }
        }
        let mut previous_nodes = Vec::new();
        if let Some(page) = existing.as_ref() {
            nodes(&page["blocks"], &mut previous_nodes);
        }
        let mut allowed_references = HashSet::new();
        for node in previous_nodes {
            for key in ["attachmentId", "localAttachmentId", "assetId"] {
                if !text(node, key).is_empty() {
                    allowed_references.insert(text(node, key));
                }
            }
        }
        for asset in data["attachments"].as_array().ok_or_else(invalid)? {
            allowed_references.insert(text(asset, "assetId"));
        }
        let mut next_nodes = Vec::new();
        nodes(&data["blocks"], &mut next_nodes);
        if next_nodes.iter().any(|node| {
            ["attachmentId", "localAttachmentId", "assetId"]
                .iter()
                .any(|key| {
                    !text(node, key).is_empty() && !allowed_references.contains(text(node, key))
                })
        }) {
            return Err("MATERIAL_REFERENCE_CONFLICT".into());
        }
        let now = Utc::now()
            .timestamp_millis()
            .max(data["expectedRevision"].as_i64().unwrap_or(0) + 1);
        let blocks = serde_json::to_string(&data["blocks"]).map_err(|_| invalid())?;
        if text(data, "mode") == "create" {
            let parent = page(&transaction, tenant, text(data, "parentPageRef"))?
                .ok_or("MATERIAL_REVISION_CONFLICT")?;
            if existing.is_some() || parent["updatedAtMs"] != data["parentRevision"] {
                return Err("MATERIAL_REVISION_CONFLICT".into());
            }
            let position: i64 = transaction.query_row("SELECT COALESCE(MAX(position),-1)+1 FROM work_note_pages WHERE tenant_id=?1 AND parent_id=?2", params![tenant,text(data,"parentPageRef")], |row| row.get(0)).map_err(db)?;
            transaction.execute("INSERT INTO work_note_pages(tenant_id,page_id,parent_id,title,emoji,position,properties_json,document_json,markdown,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,'📝',?5,?6,?7,?8,?9,?9)",
                params![tenant,page_id,text(data,"parentPageRef"),text(data,"title"),position,serde_json::to_string(&data["properties"]).map_err(|_| invalid())?,blocks,text(data,"markdown"),now]).map_err(db)?;
        } else {
            let existing = existing.ok_or("MATERIAL_REVISION_CONFLICT")?;
            if existing["updatedAtMs"] != data["expectedRevision"]
                || existing["title"] != data["title"]
            {
                return Err("MATERIAL_REVISION_CONFLICT".into());
            }
            let mut offset = 0;
            let next = data["blocks"].as_array().ok_or_else(invalid)?;
            for block in existing["blocks"].as_array().ok_or_else(invalid)? {
                let index = next
                    .iter()
                    .enumerate()
                    .skip(offset)
                    .find(|(_, candidate)| *candidate == block)
                    .map(|(index, _)| index)
                    .ok_or("MATERIAL_REFERENCE_CONFLICT")?;
                offset = index + 1;
            }
            transaction.execute("UPDATE work_note_pages SET document_json=?1,markdown=?2,updated_at_ms=?3 WHERE tenant_id=?4 AND page_id=?5 AND updated_at_ms=?6",
                params![blocks,text(&expected,"markdown"),now,tenant,page_id,data["expectedRevision"].as_i64()]).map_err(db)?;
        }
        transaction
            .execute(
                "DELETE FROM work_note_pages_fts WHERE tenant_id=?1 AND page_id=?2",
                params![tenant, page_id],
            )
            .map_err(db)?;
        transaction.execute("INSERT INTO work_note_pages_fts(tenant_id,page_id,title,markdown) VALUES(?1,?2,?3,?4)",params![tenant,page_id,text(data,"title"),text(&expected,"markdown")]).map_err(db)?;
        for file in &prepared {
            let asset = &file.asset;
            transaction.execute("INSERT INTO work_note_attachments(tenant_id,attachment_id,page_id,block_id,file_name,content_type,byte_size,sha256,local_path,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)",
                params![tenant,text(asset,"assetId"),page_id,text(asset,"blockId"),text(asset,"fileName"),text(asset,"contentType"),asset["size"].as_u64(),text(asset,"sha256"),file.relative_path,now]).map_err(db)?;
        }
        verify_content(&transaction, tenant, &expected)?;
        verify_files(&transaction, &store.data_dir, tenant, data)?;
        let local_ref = format!("work-note-page:{page_id}");
        let locator = super::receipt_verification::locator("materials_apply_images", data);
        let envelope = json!({"kind":super::receipt_verification::ENVELOPE,"result":data,
            "verification":{"sha256":super::receipt_verification::digest(&transaction,tenant,&locator)?,"locator":locator}});
        transaction.execute("INSERT INTO classaimate_mcp_local_write_receipts(tenant_id,receipt_id,operation,request_sha256,result_json,local_ref,created_at_ms) VALUES(?1,?2,'materials_apply_images',?3,?4,?5,?6)",
            params![tenant,text(input,"receiptId"),text(input,"requestSha256"),serde_json::to_string(&envelope).map_err(|_| invalid())?,local_ref,now]).map_err(db)?;
        super::receipt_verification::verify(&transaction, tenant, &envelope)?;
        transaction.commit().map_err(db)?;
        super::receipt_verification::verify_after_commit(&conn, tenant, &envelope)?;
        verify_files(&conn, &store.data_dir, tenant, data)?;
        Ok(json!({"replayed":false,"result":data,"localRef":local_ref}))
    })();
    if result.is_err() {
        cleanup(&conn, tenant, &prepared);
    }
    result
}

#[cfg(test)]
#[path = "classaimate_mcp_material_assets_tests.rs"]
mod tests;
