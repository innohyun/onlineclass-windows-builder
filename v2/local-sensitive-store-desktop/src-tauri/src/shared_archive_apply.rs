use crate::shared_archive;
use crate::shared_archive_sync::{
    reference_text, safe_relative_path, sanitized_name, sha256_file, verify_bundle_reference_at,
    verify_snapshot_bundles,
};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

fn replace_file_from_bundle(
    source: &Path,
    target: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<bool, String> {
    if target.is_file() {
        let (size, sha256) = sha256_file(target)?;
        if size == expected_size && sha256 == expected_sha256 {
            return Ok(false);
        }
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("archive_sync_target_dir_failed:{e}"))?;
    }
    let temporary = target.with_extension(format!("sync-{}.tmp", Utc::now().timestamp_millis()));
    fs::copy(source, &temporary).map_err(|e| format!("archive_sync_target_copy_failed:{e}"))?;
    let (size, sha256) = sha256_file(&temporary)?;
    if size != expected_size || sha256 != expected_sha256 {
        let _ = fs::remove_file(&temporary);
        return Err("archive_sync_target_copy_digest_mismatch".to_string());
    }
    if target.exists() {
        fs::remove_file(target).map_err(|e| format!("archive_sync_target_replace_failed:{e}"))?;
    }
    fs::rename(&temporary, target).map_err(|e| format!("archive_sync_target_commit_failed:{e}"))?;
    Ok(true)
}

pub(crate) fn verify_existing_archive(
    connection: &Connection,
    file_root: &Path,
    tenant_id: &str,
    document: &Value,
    bundle_dir: &Path,
) -> Result<i64, String> {
    let archive = document
        .get("archive")
        .ok_or_else(|| "archive_sync_document_invalid".to_string())?;
    let archive_id = reference_text(archive, "id")?;
    let manifest_sha256 = reference_text(archive, "manifestSha256")?;
    let existing = connection.query_row(
        "SELECT tenant_id,manifest_sha256,record_count,file_count,total_file_bytes FROM shared_archives WHERE id=?1",
        params![archive_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?, row.get::<_, i64>(4)?)),
    ).optional().map_err(|e| format!("archive_sync_existing_read_failed:{e}"))?;
    let Some((existing_tenant, existing_manifest, record_count, file_count, total_file_bytes)) =
        existing
    else {
        return Ok(-1);
    };
    if existing_tenant != tenant_id
        || existing_manifest != manifest_sha256
        || archive.get("recordCount").and_then(Value::as_i64) != Some(record_count)
        || archive.get("fileCount").and_then(Value::as_i64) != Some(file_count)
        || archive.get("totalFileBytes").and_then(Value::as_i64) != Some(total_file_bytes)
    {
        return Err("archive_sync_existing_manifest_mismatch".to_string());
    }
    for record in document
        .get("records")
        .and_then(Value::as_array)
        .ok_or_else(|| "archive_sync_records_invalid".to_string())?
    {
        let ordinal = record.get("ordinal").and_then(Value::as_i64).unwrap_or(-1);
        let stored = connection.query_row(
            "SELECT record_type,payload_json,payload_sha256 FROM shared_archive_records WHERE archive_id=?1 AND ordinal=?2",
            params![archive_id, ordinal],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
        ).optional().map_err(|e| format!("archive_sync_existing_record_failed:{e}"))?;
        let expected = (
            record
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            serde_json::to_string(record.get("payload").unwrap_or(&Value::Null))
                .unwrap_or_default(),
            record
                .get("payloadSha256")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        );
        if stored != Some(expected) {
            return Err("archive_sync_existing_record_mismatch".to_string());
        }
    }
    let expected_archive_dir = file_root.join(archive_id);
    let mut repaired = 0i64;
    for file in document
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| "archive_sync_files_invalid".to_string())?
    {
        let ordinal = file.get("ordinal").and_then(Value::as_i64).unwrap_or(-1);
        let original_name = file
            .get("originalName")
            .and_then(Value::as_str)
            .unwrap_or("");
        let target =
            expected_archive_dir.join(format!("{ordinal:04}-{}", sanitized_name(original_name)));
        let stored = connection.query_row(
            "SELECT original_name,content_type,byte_size,sha256,local_path FROM shared_archive_files WHERE archive_id=?1 AND ordinal=?2",
            params![archive_id, ordinal],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?)),
        ).optional().map_err(|e| format!("archive_sync_existing_file_failed:{e}"))?;
        let expected = (
            original_name.to_string(),
            file.get("contentType")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            file.get("byteSize").and_then(Value::as_i64).unwrap_or(-1),
            file.get("sha256")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            target.to_string_lossy().to_string(),
        );
        if stored != Some(expected) {
            return Err("archive_sync_existing_file_mismatch".to_string());
        }
        let relative = reference_text(file, "bundleRelativePath")?;
        if replace_file_from_bundle(
            &bundle_dir.join(
                safe_relative_path(relative)
                    .ok_or_else(|| "archive_sync_bundle_file_path_invalid".to_string())?,
            ),
            &target,
            file.get("byteSize")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX),
            reference_text(file, "sha256")?,
        )? {
            repaired += 1;
        }
    }
    Ok(repaired)
}

fn copy_new_archive_files(
    document: &Value,
    bundle_dir: &Path,
    final_dir: &Path,
) -> Result<(), String> {
    if final_dir.exists() {
        for file in document
            .get("files")
            .and_then(Value::as_array)
            .ok_or_else(|| "archive_sync_files_invalid".to_string())?
        {
            let ordinal = file.get("ordinal").and_then(Value::as_i64).unwrap_or(-1);
            let original_name = file
                .get("originalName")
                .and_then(Value::as_str)
                .unwrap_or("");
            let relative = reference_text(file, "bundleRelativePath")?;
            replace_file_from_bundle(
                &bundle_dir.join(
                    safe_relative_path(relative)
                        .ok_or_else(|| "archive_sync_bundle_file_path_invalid".to_string())?,
                ),
                &final_dir.join(format!("{ordinal:04}-{}", sanitized_name(original_name))),
                file.get("byteSize")
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX),
                reference_text(file, "sha256")?,
            )?;
        }
        return Ok(());
    }
    let parent = final_dir
        .parent()
        .ok_or_else(|| "archive_sync_target_dir_failed".to_string())?;
    let name = final_dir
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "archive_sync_target_dir_failed".to_string())?;
    let staging_dir = parent.join(format!(
        ".{name}.sync-staging-{}",
        Utc::now().timestamp_millis()
    ));
    fs::create_dir_all(&staging_dir)
        .map_err(|e| format!("archive_sync_apply_staging_failed:{e}"))?;
    let result = (|| -> Result<(), String> {
        for file in document
            .get("files")
            .and_then(Value::as_array)
            .ok_or_else(|| "archive_sync_files_invalid".to_string())?
        {
            let ordinal = file.get("ordinal").and_then(Value::as_i64).unwrap_or(-1);
            let original_name = file
                .get("originalName")
                .and_then(Value::as_str)
                .unwrap_or("");
            let relative = reference_text(file, "bundleRelativePath")?;
            fs::copy(
                bundle_dir.join(
                    safe_relative_path(relative)
                        .ok_or_else(|| "archive_sync_bundle_file_path_invalid".to_string())?,
                ),
                staging_dir.join(format!("{ordinal:04}-{}", sanitized_name(original_name))),
            )
            .map_err(|e| format!("archive_sync_apply_copy_failed:{e}"))?;
        }
        fs::rename(&staging_dir, final_dir)
            .map_err(|e| format!("archive_sync_apply_files_commit_failed:{e}"))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging_dir);
    }
    result
}

fn insert_archive(
    connection: &mut Connection,
    file_root: &Path,
    tenant_id: &str,
    document: &Value,
    bundle_dir: &Path,
) -> Result<(), String> {
    let archive = document
        .get("archive")
        .ok_or_else(|| "archive_sync_document_invalid".to_string())?;
    let archive_id = reference_text(archive, "id")?;
    let final_dir = file_root.join(archive_id);
    copy_new_archive_files(document, bundle_dir, &final_dir)?;
    let result = (|| -> Result<(), String> {
        let transaction = connection
            .transaction()
            .map_err(|e| format!("archive_sync_apply_transaction_failed:{e}"))?;
        transaction
            .execute(
                "INSERT INTO shared_archives VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
                params![
                    archive_id,
                    tenant_id,
                    archive
                        .get("sourceType")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                    archive
                        .get("sourceId")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                    archive.get("title").and_then(Value::as_str).unwrap_or(""),
                    archive
                        .get("manifestSha256")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                    archive
                        .get("recordCount")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    archive
                        .get("fileCount")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    archive
                        .get("totalFileBytes")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    archive
                        .get("sourceCreatedAt")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    archive
                        .get("sourceExpiresAt")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    Utc::now().timestamp_millis(),
                    serde_json::to_string(document).unwrap_or_default()
                ],
            )
            .map_err(|e| format!("archive_sync_apply_archive_insert_failed:{e}"))?;
        for record in document
            .get("records")
            .and_then(Value::as_array)
            .ok_or_else(|| "archive_sync_records_invalid".to_string())?
        {
            transaction
                .execute(
                    "INSERT INTO shared_archive_records VALUES (?,?,?,?,?)",
                    params![
                        archive_id,
                        record.get("ordinal").and_then(Value::as_i64).unwrap_or(-1),
                        record.get("type").and_then(Value::as_str).unwrap_or(""),
                        serde_json::to_string(record.get("payload").unwrap_or(&Value::Null))
                            .unwrap_or_default(),
                        record
                            .get("payloadSha256")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                    ],
                )
                .map_err(|e| format!("archive_sync_apply_record_insert_failed:{e}"))?;
        }
        for file in document
            .get("files")
            .and_then(Value::as_array)
            .ok_or_else(|| "archive_sync_files_invalid".to_string())?
        {
            let ordinal = file.get("ordinal").and_then(Value::as_i64).unwrap_or(-1);
            let original_name = file
                .get("originalName")
                .and_then(Value::as_str)
                .unwrap_or("");
            transaction
                .execute(
                    "INSERT INTO shared_archive_files VALUES (?,?,?,?,?,?,?)",
                    params![
                        archive_id,
                        ordinal,
                        original_name,
                        file.get("contentType")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                        file.get("byteSize").and_then(Value::as_i64).unwrap_or(0),
                        file.get("sha256").and_then(Value::as_str).unwrap_or(""),
                        final_dir
                            .join(format!("{ordinal:04}-{}", sanitized_name(original_name)))
                            .to_string_lossy()
                            .to_string()
                    ],
                )
                .map_err(|e| format!("archive_sync_apply_file_insert_failed:{e}"))?;
        }
        transaction
            .commit()
            .map_err(|e| format!("archive_sync_apply_commit_failed:{e}"))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&final_dir);
    }
    result
}

pub(crate) fn apply_snapshot_bundles(
    tenant_id: &str,
    tenant_dir: &Path,
    archives: &Value,
) -> Result<Value, String> {
    let mut connection = shared_archive::open_db()?;
    let (_, file_root) = shared_archive::storage_paths();
    apply_snapshot_bundles_to(&mut connection, &file_root, tenant_id, tenant_dir, archives)
}

pub(crate) fn apply_snapshot_bundles_to(
    connection: &mut Connection,
    file_root: &Path,
    tenant_id: &str,
    tenant_dir: &Path,
    archives: &Value,
) -> Result<Value, String> {
    verify_snapshot_bundles(tenant_id, tenant_dir, archives)?;
    let references = archives
        .get("records")
        .and_then(Value::as_array)
        .ok_or_else(|| "archive_sync_references_required".to_string())?;
    let mut imported = 0i64;
    let mut repaired = 0i64;
    let mut unchanged = 0i64;
    for reference in references {
        let relative = reference_text(reference, "bundleRelativePath")?;
        let bundle_dir = tenant_dir.join(
            safe_relative_path(relative)
                .ok_or_else(|| "archive_sync_bundle_path_invalid".to_string())?,
        );
        let document = verify_bundle_reference_at(&bundle_dir, tenant_id, reference)?;
        match verify_existing_archive(connection, file_root, tenant_id, &document, &bundle_dir)? {
            -1 => {
                insert_archive(connection, file_root, tenant_id, &document, &bundle_dir)?;
                imported += 1;
            }
            0 => unchanged += 1,
            count => repaired += count,
        }
    }
    Ok(
        json!({"ok":true,"archiveImported":imported,"archiveRepairedFiles":repaired,"archiveUnchanged":unchanged}),
    )
}
