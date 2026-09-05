use crate::shared_archive;
use chrono::Utc;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

const BUNDLE_SCHEMA: &str = "classaimate-shared-archive-bundle-v1";
const ARCHIVE_DOCUMENT: &str = "archive.json";
const BUNDLE_COMMIT: &str = "commit.json";

#[derive(Clone, Debug)]
struct DigestEntry {
    relative_path: String,
    size: u64,
    sha256: String,
}

#[derive(Clone, Debug)]
struct SourceFile {
    byte_size: u64,
    sha256: String,
    source_path: PathBuf,
    bundle_relative_path: String,
}

#[derive(Clone, Debug)]
struct ArchiveSource {
    archive_id: String,
    tenant_id: String,
    source_type: String,
    manifest_sha256: String,
    document: Value,
    document_bytes: Vec<u8>,
    files: Vec<SourceFile>,
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn sha256_file(path: &Path) -> Result<(u64, String), String> {
    use std::io::Read;
    let mut file =
        fs::File::open(path).map_err(|e| format!("archive_sync_file_open_failed:{e}"))?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| format!("archive_sync_file_read_failed:{e}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size = size.saturating_add(read as u64);
    }
    Ok((size, format!("{:x}", hasher.finalize())))
}

fn bundle_root(entries: &mut [DigestEntry]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"classaimate-shared-archive-bundle-v1\0");
    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    for entry in entries {
        hasher.update(entry.relative_path.as_bytes());
        hasher.update([0]);
        hasher.update(entry.size.to_string().as_bytes());
        hasher.update([0]);
        hasher.update(entry.sha256.as_bytes());
        hasher.update([b'\n']);
    }
    format!("{:x}", hasher.finalize())
}

fn valid_hex_sha(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_archive_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | ':' | '.')
        })
}

pub(crate) fn safe_relative_path(value: &str) -> Option<PathBuf> {
    let path = PathBuf::from(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return None;
    }
    Some(path)
}

pub(crate) fn sanitized_name(value: &str) -> String {
    let result: String = value
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, '.' | '-' | '_' | ' ') {
                character
            } else {
                '_'
            }
        })
        .take(120)
        .collect();
    if result.trim_matches('.').is_empty() {
        "attachment.bin".to_string()
    } else {
        result
    }
}

fn file_relative_path(ordinal: i64, original_name: &str) -> String {
    format!("files/{ordinal:04}-{}", sanitized_name(original_name))
}

fn load_archive_sources(
    connection: &Connection,
    tenant_id: &str,
) -> Result<Vec<ArchiveSource>, String> {
    let mut archive_statement = connection
        .prepare(
            "SELECT id,tenant_id,source_type,source_id,title,manifest_sha256,record_count,file_count,
                    total_file_bytes,source_created_at,source_expires_at
             FROM shared_archives WHERE tenant_id=?1 ORDER BY id",
        )
        .map_err(|e| format!("archive_sync_list_prepare_failed:{e}"))?;
    let archive_rows = archive_statement
        .query_map(params![tenant_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
            ))
        })
        .map_err(|e| format!("archive_sync_list_query_failed:{e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("archive_sync_list_row_failed:{e}"))?;
    drop(archive_statement);

    let mut sources = Vec::with_capacity(archive_rows.len());
    for (
        archive_id,
        archive_tenant_id,
        source_type,
        source_id,
        title,
        manifest_sha256,
        record_count,
        file_count,
        total_file_bytes,
        source_created_at,
        source_expires_at,
    ) in archive_rows
    {
        if archive_tenant_id != tenant_id
            || !valid_archive_id(&archive_id)
            || !valid_hex_sha(&manifest_sha256)
            || !matches!(source_type.as_str(), "assignment" | "board")
        {
            return Err("archive_sync_source_metadata_invalid".to_string());
        }
        let mut record_statement = connection
            .prepare(
                "SELECT ordinal,record_type,payload_json,payload_sha256
                 FROM shared_archive_records WHERE archive_id=?1 ORDER BY ordinal",
            )
            .map_err(|e| format!("archive_sync_record_prepare_failed:{e}"))?;
        let record_rows = record_statement
            .query_map(params![archive_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| format!("archive_sync_record_query_failed:{e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("archive_sync_record_row_failed:{e}"))?;
        drop(record_statement);
        if record_rows.len() as i64 != record_count {
            return Err("archive_sync_record_count_mismatch".to_string());
        }
        let mut records = Vec::with_capacity(record_rows.len());
        let mut record_ordinals = HashSet::new();
        for (ordinal, record_type, payload_json, payload_sha256) in record_rows {
            if ordinal < 0
                || !record_ordinals.insert(ordinal)
                || !valid_hex_sha(&payload_sha256)
                || sha256_bytes(payload_json.as_bytes()) != payload_sha256
            {
                return Err("archive_sync_record_digest_mismatch".to_string());
            }
            let payload = serde_json::from_str::<Value>(&payload_json)
                .map_err(|e| format!("archive_sync_record_decode_failed:{e}"))?;
            records.push(json!({
                "ordinal": ordinal,
                "type": record_type,
                "payload": payload,
                "payloadSha256": payload_sha256,
            }));
        }

        let mut file_statement = connection
            .prepare(
                "SELECT ordinal,original_name,content_type,byte_size,sha256,local_path
                 FROM shared_archive_files WHERE archive_id=?1 ORDER BY ordinal",
            )
            .map_err(|e| format!("archive_sync_file_prepare_failed:{e}"))?;
        let file_rows = file_statement
            .query_map(params![archive_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(|e| format!("archive_sync_file_query_failed:{e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("archive_sync_file_row_failed:{e}"))?;
        drop(file_statement);
        if file_rows.len() as i64 != file_count {
            return Err("archive_sync_file_count_mismatch".to_string());
        }
        let mut files = Vec::with_capacity(file_rows.len());
        let mut file_documents = Vec::with_capacity(file_rows.len());
        let mut file_ordinals = HashSet::new();
        let mut actual_total = 0u64;
        for (ordinal, original_name, content_type, byte_size, sha256, local_path) in file_rows {
            if ordinal < 0
                || byte_size < 0
                || !file_ordinals.insert(ordinal)
                || !valid_hex_sha(&sha256)
            {
                return Err("archive_sync_file_metadata_invalid".to_string());
            }
            let source_path = PathBuf::from(local_path);
            let (actual_size, actual_sha256) = sha256_file(&source_path)?;
            if actual_size != byte_size as u64 || actual_sha256 != sha256 {
                return Err("archive_sync_source_file_digest_mismatch".to_string());
            }
            actual_total = actual_total.saturating_add(actual_size);
            let bundle_relative_path = file_relative_path(ordinal, &original_name);
            file_documents.push(json!({
                "ordinal": ordinal,
                "originalName": original_name,
                "contentType": content_type,
                "byteSize": actual_size,
                "sha256": sha256,
                "bundleRelativePath": bundle_relative_path,
            }));
            files.push(SourceFile {
                byte_size: actual_size,
                sha256,
                source_path,
                bundle_relative_path,
            });
        }
        if actual_total != total_file_bytes.max(0) as u64 {
            return Err("archive_sync_total_file_bytes_mismatch".to_string());
        }
        let document = json!({
            "schemaVersion": BUNDLE_SCHEMA,
            "archive": {
                "id": archive_id,
                "tenantId": archive_tenant_id,
                "sourceType": source_type,
                "sourceId": source_id,
                "title": title,
                "manifestSha256": manifest_sha256,
                "recordCount": record_count,
                "fileCount": file_count,
                "totalFileBytes": actual_total,
                "sourceCreatedAt": source_created_at,
                "sourceExpiresAt": source_expires_at,
            },
            "records": records,
            "files": file_documents,
        });
        let document_bytes = serde_json::to_vec(&document)
            .map_err(|e| format!("archive_sync_document_encode_failed:{e}"))?;
        sources.push(ArchiveSource {
            archive_id,
            tenant_id: archive_tenant_id,
            source_type,
            manifest_sha256,
            document,
            document_bytes,
            files,
        });
    }
    Ok(sources)
}

fn source_digests(source: &ArchiveSource) -> Vec<DigestEntry> {
    let mut entries = vec![DigestEntry {
        relative_path: ARCHIVE_DOCUMENT.to_string(),
        size: source.document_bytes.len() as u64,
        sha256: sha256_bytes(&source.document_bytes),
    }];
    entries.extend(source.files.iter().map(|file| DigestEntry {
        relative_path: file.bundle_relative_path.clone(),
        size: file.byte_size,
        sha256: file.sha256.clone(),
    }));
    entries
}

fn reference_for(source: &ArchiveSource, root_sha256: &str) -> Value {
    let archive = source
        .document
        .get("archive")
        .cloned()
        .unwrap_or(Value::Null);
    json!({
        "archiveId": source.archive_id,
        "sourceType": source.source_type,
        "manifestSha256": source.manifest_sha256,
        "bundleRelativePath": format!("archive-bundles/{}", source.manifest_sha256),
        "bundleRootSha256": root_sha256,
        "recordCount": archive.get("recordCount").and_then(Value::as_i64).unwrap_or(0),
        "fileCount": archive.get("fileCount").and_then(Value::as_i64).unwrap_or(0),
        "totalFileBytes": archive.get("totalFileBytes").and_then(Value::as_i64).unwrap_or(0),
    })
}

fn write_bundle(
    source: &ArchiveSource,
    tenant_dir: &Path,
    reference: &Value,
) -> Result<(), String> {
    let bundles_root = tenant_dir.join("archive-bundles");
    fs::create_dir_all(&bundles_root)
        .map_err(|e| format!("archive_sync_bundle_root_failed:{e}"))?;
    let final_dir = bundles_root.join(&source.manifest_sha256);
    if final_dir.exists() {
        verify_bundle_reference(tenant_dir, &source.tenant_id, reference)?;
        return Ok(());
    }
    let staging_dir = bundles_root.join(format!(
        ".{}.staging-{}",
        source.manifest_sha256,
        Utc::now().timestamp_millis()
    ));
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir)
            .map_err(|e| format!("archive_sync_staging_cleanup_failed:{e}"))?;
    }
    fs::create_dir_all(staging_dir.join("files"))
        .map_err(|e| format!("archive_sync_staging_create_failed:{e}"))?;
    let result = (|| -> Result<(), String> {
        fs::write(staging_dir.join(ARCHIVE_DOCUMENT), &source.document_bytes)
            .map_err(|e| format!("archive_sync_document_write_failed:{e}"))?;
        for file in &source.files {
            let destination = staging_dir.join(
                safe_relative_path(&file.bundle_relative_path)
                    .ok_or_else(|| "archive_sync_bundle_file_path_invalid".to_string())?,
            );
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("archive_sync_bundle_file_dir_failed:{e}"))?;
            }
            fs::copy(&file.source_path, &destination)
                .map_err(|e| format!("archive_sync_bundle_file_copy_failed:{e}"))?;
        }
        let commit = json!({
            "schemaVersion": BUNDLE_SCHEMA,
            "tenantId": source.tenant_id,
            "archiveId": source.archive_id,
            "manifestSha256": source.manifest_sha256,
            "bundleRootSha256": reference.get("bundleRootSha256"),
            "committedAtMs": Utc::now().timestamp_millis(),
        });
        let raw = serde_json::to_vec_pretty(&commit)
            .map_err(|e| format!("archive_sync_commit_encode_failed:{e}"))?;
        fs::write(staging_dir.join(BUNDLE_COMMIT), raw)
            .map_err(|e| format!("archive_sync_commit_write_failed:{e}"))?;
        verify_bundle_reference_at(&staging_dir, &source.tenant_id, reference)?;
        fs::rename(&staging_dir, &final_dir)
            .map_err(|e| format!("archive_sync_bundle_commit_failed:{e}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging_dir);
    }
    result
}

fn read_json(path: &Path, error: &str) -> Result<Value, String> {
    let raw = fs::read(path).map_err(|e| format!("{error}:{e}"))?;
    serde_json::from_slice(&raw).map_err(|e| format!("{error}:{e}"))
}

pub(crate) fn reference_text<'a>(reference: &'a Value, key: &str) -> Result<&'a str, String> {
    reference
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "archive_sync_reference_invalid".to_string())
}

pub(crate) fn verify_bundle_reference_at(
    bundle_dir: &Path,
    tenant_id: &str,
    reference: &Value,
) -> Result<Value, String> {
    let archive_id = reference_text(reference, "archiveId")?;
    let manifest_sha256 = reference_text(reference, "manifestSha256")?;
    let expected_root = reference_text(reference, "bundleRootSha256")?;
    if !valid_archive_id(archive_id)
        || !valid_hex_sha(manifest_sha256)
        || !valid_hex_sha(expected_root)
    {
        return Err("archive_sync_reference_invalid".to_string());
    }
    let commit = read_json(
        &bundle_dir.join(BUNDLE_COMMIT),
        "archive_sync_commit_read_failed",
    )?;
    if commit.get("schemaVersion").and_then(Value::as_str) != Some(BUNDLE_SCHEMA)
        || commit.get("tenantId").and_then(Value::as_str) != Some(tenant_id)
        || commit.get("archiveId").and_then(Value::as_str) != Some(archive_id)
        || commit.get("manifestSha256").and_then(Value::as_str) != Some(manifest_sha256)
        || commit.get("bundleRootSha256").and_then(Value::as_str) != Some(expected_root)
    {
        return Err("archive_sync_commit_mismatch".to_string());
    }
    let document_path = bundle_dir.join(ARCHIVE_DOCUMENT);
    let document_bytes =
        fs::read(&document_path).map_err(|e| format!("archive_sync_document_read_failed:{e}"))?;
    let document: Value = serde_json::from_slice(&document_bytes)
        .map_err(|e| format!("archive_sync_document_decode_failed:{e}"))?;
    let archive = document
        .get("archive")
        .ok_or_else(|| "archive_sync_document_invalid".to_string())?;
    if document.get("schemaVersion").and_then(Value::as_str) != Some(BUNDLE_SCHEMA)
        || archive.get("id").and_then(Value::as_str) != Some(archive_id)
        || archive.get("tenantId").and_then(Value::as_str) != Some(tenant_id)
        || archive.get("manifestSha256").and_then(Value::as_str) != Some(manifest_sha256)
    {
        return Err("archive_sync_document_mismatch".to_string());
    }
    let records = document
        .get("records")
        .and_then(Value::as_array)
        .ok_or_else(|| "archive_sync_records_invalid".to_string())?;
    let files = document
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| "archive_sync_files_invalid".to_string())?;
    if archive.get("recordCount").and_then(Value::as_i64) != Some(records.len() as i64)
        || archive.get("fileCount").and_then(Value::as_i64) != Some(files.len() as i64)
        || reference.get("recordCount").and_then(Value::as_i64) != Some(records.len() as i64)
        || reference.get("fileCount").and_then(Value::as_i64) != Some(files.len() as i64)
    {
        return Err("archive_sync_bundle_count_mismatch".to_string());
    }
    let mut record_ordinals = HashSet::new();
    for record in records {
        let ordinal = record.get("ordinal").and_then(Value::as_i64).unwrap_or(-1);
        let payload = record
            .get("payload")
            .ok_or_else(|| "archive_sync_record_payload_missing".to_string())?;
        let expected = reference_text(record, "payloadSha256")?;
        let encoded = serde_json::to_vec(payload)
            .map_err(|e| format!("archive_sync_record_encode_failed:{e}"))?;
        if ordinal < 0
            || !record_ordinals.insert(ordinal)
            || !valid_hex_sha(expected)
            || sha256_bytes(&encoded) != expected
        {
            return Err("archive_sync_record_digest_mismatch".to_string());
        }
    }
    let mut entries = vec![DigestEntry {
        relative_path: ARCHIVE_DOCUMENT.to_string(),
        size: document_bytes.len() as u64,
        sha256: sha256_bytes(&document_bytes),
    }];
    let mut file_ordinals = HashSet::new();
    let mut file_paths = HashSet::new();
    let mut total_bytes = 0u64;
    for file in files {
        let ordinal = file.get("ordinal").and_then(Value::as_i64).unwrap_or(-1);
        let relative = reference_text(file, "bundleRelativePath")?;
        let expected_relative = file_relative_path(
            ordinal,
            file.get("originalName")
                .and_then(Value::as_str)
                .unwrap_or(""),
        );
        let expected_size = file
            .get("byteSize")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        let expected_sha256 = reference_text(file, "sha256")?;
        if ordinal < 0
            || !file_ordinals.insert(ordinal)
            || relative != expected_relative
            || !file_paths.insert(relative.to_string())
            || !valid_hex_sha(expected_sha256)
        {
            return Err("archive_sync_bundle_file_metadata_invalid".to_string());
        }
        let safe = safe_relative_path(relative)
            .ok_or_else(|| "archive_sync_bundle_file_path_invalid".to_string())?;
        let (size, sha256) = sha256_file(&bundle_dir.join(safe))?;
        if size != expected_size || sha256 != expected_sha256 {
            return Err("archive_sync_bundle_file_digest_mismatch".to_string());
        }
        total_bytes = total_bytes.saturating_add(size);
        entries.push(DigestEntry {
            relative_path: relative.to_string(),
            size,
            sha256,
        });
    }
    if archive.get("totalFileBytes").and_then(Value::as_u64) != Some(total_bytes)
        || reference.get("totalFileBytes").and_then(Value::as_u64) != Some(total_bytes)
        || bundle_root(&mut entries) != expected_root
    {
        return Err("archive_sync_bundle_root_mismatch".to_string());
    }
    Ok(document)
}

fn verify_bundle_reference(
    tenant_dir: &Path,
    tenant_id: &str,
    reference: &Value,
) -> Result<Value, String> {
    let manifest_sha256 = reference_text(reference, "manifestSha256")?;
    let relative = reference_text(reference, "bundleRelativePath")?;
    let expected_relative = format!("archive-bundles/{manifest_sha256}");
    if relative != expected_relative {
        return Err("archive_sync_bundle_path_invalid".to_string());
    }
    let safe = safe_relative_path(relative)
        .ok_or_else(|| "archive_sync_bundle_path_invalid".to_string())?;
    verify_bundle_reference_at(&tenant_dir.join(safe), tenant_id, reference)
}

pub(crate) fn tenant_content_sha256(tenant_id: &str) -> Result<String, String> {
    let connection = shared_archive::open_db()?;
    let sources = load_archive_sources(&connection, tenant_id)?;
    let mut hasher = Sha256::new();
    hasher.update(b"classaimate-shared-archive-set-v1\0");
    for source in sources {
        hasher.update(source.archive_id.as_bytes());
        hasher.update([0]);
        hasher.update(source.manifest_sha256.as_bytes());
        hasher.update([b'\n']);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(crate) fn has_local_only_references(tenant_id: &str, archives: Option<&Value>) -> Result<bool, String> {
    let incoming = archives.and_then(|value| value.get("records")).and_then(Value::as_array)
        .into_iter().flatten().filter_map(|record| Some((
            record.get("archiveId")?.as_str()?.to_string(),
            record.get("manifestSha256")?.as_str()?.to_string(),
        ))).collect::<HashSet<_>>();
    let connection = shared_archive::open_db()?;
    let mut statement = connection.prepare("SELECT id,manifest_sha256 FROM shared_archives WHERE tenant_id=?1")
        .map_err(|e| format!("archive_sync_references_prepare_failed:{e}"))?;
    let rows = statement.query_map(params![tenant_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|e| format!("archive_sync_references_query_failed:{e}"))?;
    for row in rows {
        if !incoming.contains(&row.map_err(|e| format!("archive_sync_reference_failed:{e}"))?) { return Ok(true); }
    }
    Ok(false)
}

pub(crate) fn ensure_tenant_bundles(tenant_id: &str, tenant_dir: &Path) -> Result<Value, String> {
    let connection = shared_archive::open_db()?;
    ensure_tenant_bundles_from(&connection, tenant_id, tenant_dir)
}

fn ensure_tenant_bundles_from(
    connection: &Connection,
    tenant_id: &str,
    tenant_dir: &Path,
) -> Result<Value, String> {
    let sources = load_archive_sources(connection, tenant_id)?;
    let mut references = Vec::with_capacity(sources.len());
    let mut board_count = 0i64;
    let mut assignment_count = 0i64;
    let mut file_count = 0i64;
    let mut total_file_bytes = 0i64;
    for source in sources {
        let mut entries = source_digests(&source);
        let root_sha256 = bundle_root(&mut entries);
        let reference = reference_for(&source, &root_sha256);
        write_bundle(&source, tenant_dir, &reference)?;
        verify_bundle_reference(tenant_dir, tenant_id, &reference)?;
        if source.source_type == "board" {
            board_count += 1;
        } else {
            assignment_count += 1;
        }
        file_count += reference
            .get("fileCount")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        total_file_bytes += reference
            .get("totalFileBytes")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        references.push(reference);
    }
    Ok(json!({
        "mode": "content_addressed_union",
        "count": references.len(),
        "boardCount": board_count,
        "assignmentCount": assignment_count,
        "fileCount": file_count,
        "totalFileBytes": total_file_bytes,
        "records": references,
    }))
}

pub(crate) fn verify_snapshot_bundles(
    tenant_id: &str,
    tenant_dir: &Path,
    archives: &Value,
) -> Result<(), String> {
    let references = archives
        .get("records")
        .and_then(Value::as_array)
        .ok_or_else(|| "archive_sync_references_required".to_string())?;
    let mut archive_ids = HashSet::new();
    let mut manifest_hashes = HashSet::new();
    for reference in references {
        let archive_id = reference_text(reference, "archiveId")?;
        let manifest_sha256 = reference_text(reference, "manifestSha256")?;
        if !archive_ids.insert(archive_id.to_string())
            || !manifest_hashes.insert(manifest_sha256.to_string())
        {
            return Err("archive_sync_reference_duplicate".to_string());
        }
        verify_bundle_reference(tenant_dir, tenant_id, reference)?;
    }
    if archives.get("count").and_then(Value::as_u64) != Some(references.len() as u64) {
        return Err("archive_sync_reference_count_mismatch".to_string());
    }
    Ok(())
}

#[cfg(test)]
#[path = "shared_archive_sync_tests.rs"]
mod tests;
