use super::*;

pub(crate) fn run_with_kind(
    store: &SqliteStore,
    tenant_id: String,
    kind: &str,
    generation: Option<i64>,
) -> Result<Value, String> {
    run_with_kind_version(
        store,
        tenant_id,
        kind,
        generation,
        crate::backup_v5::SNAPSHOT_VERSION,
    )
}

pub(crate) fn run_with_kind_version(
    store: &SqliteStore,
    tenant_id: String,
    kind: &str,
    generation: Option<i64>,
    snapshot_version: i64,
) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    if !matches!(kind, "manual" | "scheduled" | "pre_restore" | "auto_sync") {
        return Err("backup_kind_invalid".to_string());
    }
    if generation.is_some_and(|value| value < 1) {
        return Err("backup_generation_invalid".to_string());
    }
    if !matches!(snapshot_version, 4 | 5) {
        return Err("backup_snapshot_version_invalid".to_string());
    }
    let config = read_config(store);
    let root_text = config
        .get("tenants")
        .and_then(|tenants| tenants.get(&tenant_id))
        .and_then(|tenant| tenant.get("backupRootDir"))
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let root = assert_backup_root_allowed(store, backup_root_dir(root_text))?;
    let out_dir = tenant_backup_dir(&root, &tenant_id);
    let _operation = root_operation(store, &out_dir)?;
    let created_at_ms = now_ms();
    let backup_id = format!(
        "{}-{:016x}",
        Utc::now().format("%Y%m%d%H%M%S%3f"),
        rand::random::<u64>()
    );
    let snapshots_dir = out_dir.join("snapshots");
    fs::create_dir_all(&snapshots_dir).map_err(|e| format!("backup_dir_failed:{e}"))?;
    let staging_dir = snapshots_dir.join(format!("{backup_id}.staging"));
    let snapshot_dir = snapshots_dir.join(&backup_id);
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir)
            .map_err(|e| format!("backup_staging_cleanup_failed:{e}"))?;
    }
    fs::create_dir_all(staging_dir.join("db")).map_err(|e| format!("backup_dir_failed:{e}"))?;
    let _staging = capture::StagingGuard(staging_dir.clone());
    if snapshot_version == 4 {
        fs::create_dir_all(staging_dir.join("board-media"))
            .map_err(|e| format!("backup_media_dir_failed:{e}"))?;
        fs::create_dir_all(staging_dir.join("work-note-attachments"))
            .map_err(|e| format!("backup_work_note_attachment_dir_failed:{e}"))?;
    }
    let db_relative_path = PathBuf::from("db").join("local-sensitive.sqlite");
    let db_path = staging_dir.join(&db_relative_path);
    let captured = capture::export(store, &tenant_id, &db_path, generation.unwrap_or(0))?;
    let mut sync = captured.sync;
    let (database_size, database_sha256) = sha256_file(&db_path)?;
    let mut artifacts = vec![ArtifactDigest {
        relative_path: db_relative_path.to_string_lossy().replace('\\', "/"),
        size: database_size,
        sha256: database_sha256.clone(),
    }];
    let mut artifact_paths = HashSet::from([db_relative_path.to_string_lossy().replace('\\', "/")]);

    let media_rows = captured.media;
    let mut media_records = Vec::new();
    let mut copied = 0i64;
    let mut skipped = 0i64;
    let mut missing = 0i64;
    let mut failed = 0i64;
    let mut bytes = 0i64;
    for row in media_rows {
        let ext = media_extension(&row);
        let legacy_relative_path = PathBuf::from("board-media")
            .join(&backup_id)
            .join(safe_segment(&row.board_id, "board"))
            .join(format!("{}.{}", safe_segment(&row.media_id, "media"), ext));
        let source_path = store.data_dir.join(&row.local_path);
        let captured_stamp = captured.media_stamps.get(&row.media_id);
        let mut status = "copied";
        let mut artifact = None;
        match fs::metadata(&source_path) {
            Ok(source_meta) => {
                if snapshot_version == crate::backup_v5::SNAPSHOT_VERSION {
                    match crate::backup_v5::put_object(&out_dir, &source_path, &backup_id) {
                        Ok(object) => {
                            if object.created {
                                copied += 1;
                            } else {
                                skipped += 1;
                                status = "skipped";
                            }
                            artifact = Some(object.artifact);
                        }
                        Err(_) => {
                            failed += 1;
                            status = "failed";
                        }
                    }
                } else {
                    let target_path = staging_dir.join(&legacy_relative_path);
                    if let Some(parent) = target_path.parent() {
                        fs::create_dir_all(parent)
                            .map_err(|e| format!("backup_media_target_dir_failed:{e}"))?;
                    }
                    if fs::copy(&source_path, &target_path).is_err() {
                        failed += 1;
                        status = "failed";
                    } else {
                        copied += 1;
                        let digest = sha256_file(&target_path)?;
                        artifact = Some(ArtifactDigest {
                            relative_path: legacy_relative_path
                                .to_string_lossy()
                                .replace('\\', "/"),
                            size: digest.0,
                            sha256: digest.1,
                        });
                    }
                }
                bytes += source_meta.len() as i64;
            }
            Err(_) => {
                missing += 1;
                status = "missing";
            }
        }
        if artifact.is_some()
            && (capture::file_stamp(&source_path).ok().as_ref() != captured_stamp
                || captured_stamp.is_none())
        {
            return Err("backup_capture_media_changed".into());
        }
        let (backup_relative_path, artifact_size, artifact_sha256) =
            if let Some(artifact) = artifact {
                let relative = artifact.relative_path.clone();
                let size = artifact.size as i64;
                let sha256 = artifact.sha256.clone();
                if artifact_paths.insert(relative.clone()) {
                    artifacts.push(artifact);
                }
                (relative, size, sha256)
            } else {
                (
                    legacy_relative_path.to_string_lossy().replace('\\', "/"),
                    0,
                    String::new(),
                )
            };
        media_records.push(json!({
            "boardId": row.board_id,
            "postId": row.post_id,
            "mediaId": row.media_id,
            "localPath": row.local_path,
            "backupRelativePath": backup_relative_path,
            "contentType": row.content_type,
            "fileName": row.file_name,
            "size": if artifact_size > 0 { artifact_size } else { row.size },
            "sha256": artifact_sha256,
            "archivedAtMs": row.archived_at_ms,
            "status": status
        }));
    }
    let attachment_rows = captured.attachments;
    let attachment_count = attachment_rows.len() as i64;
    let mut attachment_records = Vec::new();
    let mut attachments_copied = 0i64;
    let mut attachments_skipped = 0i64;
    let mut attachments_missing = 0i64;
    let mut attachments_failed = 0i64;
    let mut attachment_bytes = 0i64;
    for row in attachment_rows {
        let legacy_relative_path = PathBuf::from("work-note-attachments")
            .join(&backup_id)
            .join(safe_segment(&row.attachment_id, "attachment"))
            .join(safe_segment(&row.file_name, "attachment.bin"));
        let source_path = store.data_dir.join(&row.local_path);
        let mut status = "copied";
        let mut artifact = None;
        match fs::metadata(&source_path) {
            Ok(source_meta) => {
                if snapshot_version == crate::backup_v5::SNAPSHOT_VERSION {
                    match crate::backup_v5::put_object(&out_dir, &source_path, &backup_id) {
                        Ok(object) => {
                            if object.created {
                                attachments_copied += 1;
                            } else {
                                attachments_skipped += 1;
                                status = "skipped";
                            }
                            artifact = Some(object.artifact);
                        }
                        Err(_) => {
                            attachments_failed += 1;
                            status = "failed";
                        }
                    }
                } else {
                    let target_path = staging_dir.join(&legacy_relative_path);
                    if let Some(parent) = target_path.parent() {
                        fs::create_dir_all(parent).map_err(|e| {
                            format!("backup_work_note_attachment_target_dir_failed:{e}")
                        })?;
                    }
                    if fs::copy(&source_path, &target_path).is_err() {
                        attachments_failed += 1;
                        status = "failed";
                    } else {
                        attachments_copied += 1;
                        let digest = sha256_file(&target_path)?;
                        artifact = Some(ArtifactDigest {
                            relative_path: legacy_relative_path
                                .to_string_lossy()
                                .replace('\\', "/"),
                            size: digest.0,
                            sha256: digest.1,
                        });
                    }
                }
                attachment_bytes += source_meta.len() as i64;
            }
            Err(_) => {
                attachments_missing += 1;
                status = "missing";
            }
        }
        let (backup_relative_path, artifact_size, artifact_sha256) =
            if let Some(artifact) = artifact {
                if artifact.sha256 != row.sha256 || artifact.size != row.size.max(0) as u64 {
                    return Err("backup_capture_attachment_changed".into());
                }
                let relative = artifact.relative_path.clone();
                let size = artifact.size as i64;
                let sha256 = artifact.sha256.clone();
                if artifact_paths.insert(relative.clone()) {
                    artifacts.push(artifact);
                }
                (relative, size, sha256)
            } else {
                (
                    legacy_relative_path.to_string_lossy().replace('\\', "/"),
                    0,
                    String::new(),
                )
            };
        attachment_records.push(json!({
            "attachmentId": row.attachment_id,
            "pageId": row.page_id,
            "blockId": row.block_id,
            "fileName": row.file_name,
            "contentType": row.content_type,
            "size": if artifact_size > 0 { artifact_size } else { row.size },
            "sha256": if artifact_sha256.is_empty() { row.sha256 } else { artifact_sha256 },
            "localPath": row.local_path,
            "backupRelativePath": backup_relative_path,
            "createdAtMs": row.created_at_ms,
            "updatedAtMs": row.updated_at_ms,
            "status": status
        }));
    }
    let stats = capture::statistics(&db_path, &tenant_id)?;
    let archives = crate::shared_archive_sync::ensure_tenant_bundles(&tenant_id, &out_dir)?;
    let counts = json!({
        "observationCount": stats.get("observationCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "teacherCounselingSessionCount": stats.get("teacherCounselingSessionCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "studentPrivateDetailCount": stats.get("studentPrivateDetailCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "mathDailyAttemptCount": stats.get("mathDailyAttemptCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "mathDailyProfileCount": stats.get("mathDailyProfileCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "mathDailyReviewSessionCount": stats.get("mathDailyReviewSessionCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "mathDailyAssignmentCount": stats.get("mathDailyAssignmentCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "mathDailyAssignmentResultCount": stats.get("mathDailyAssignmentResultCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "mathDailyCacheRunCount": stats.get("mathDailyCacheRunCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "boardSnapshotCount": stats.get("boardSnapshotCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "boardMediaCount": stats.get("boardMediaCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "attendanceRecordCount": stats.get("attendanceRecordCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "attendanceNaisCheckCount": stats.get("attendanceNaisCheckCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "attendanceDocumentRequestCount": stats.get("attendanceDocumentRequestCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "counselingRecordCount": stats.get("counselingRecordCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "counselingTeacherNoteCount": stats.get("counselingTeacherNoteCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "evalAssignmentCount": stats.get("evalAssignmentCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "evalResultCount": stats.get("evalResultCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "studentRecordDraftSetCount": stats.get("studentRecordDraftSetCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "studentRecordDraftCount": stats.get("studentRecordDraftCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "importRunCount": stats.get("importRunCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "workNoteCount": stats.get("workNoteCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "workNoteAttachmentCount": attachment_count,
        "cloudSyncRunCount": stats.get("cloudSyncRunCount").and_then(|value| value.as_i64()).unwrap_or(0),
        "sharedArchiveCount": archives.get("count").and_then(Value::as_i64).unwrap_or(0),
        "sharedArchiveBoardCount": archives.get("boardCount").and_then(Value::as_i64).unwrap_or(0),
        "sharedArchiveAssignmentCount": archives.get("assignmentCount").and_then(Value::as_i64).unwrap_or(0),
        "sharedArchiveFileCount": archives.get("fileCount").and_then(Value::as_i64).unwrap_or(0),
    });
    let database = json!({
        "relativePath": db_relative_path.to_string_lossy().replace('\\', "/"),
        "size": database_size,
        "sha256": database_sha256
    });
    let media = json!({
        "mode": if snapshot_version == 5 { "content_addressed_objects" } else { "separate_folder_mirror" },
        "copied": copied,
        "skipped": skipped,
        "missing": missing,
        "failed": failed,
        "bytes": bytes,
        "records": media_records
    });
    let work_note_attachments = json!({
        "mode": if snapshot_version == 5 { "content_addressed_objects" } else { "separate_folder_mirror" },
        "copied": attachments_copied,
        "skipped": attachments_skipped,
        "missing": attachments_missing,
        "failed": attachments_failed,
        "bytes": attachment_bytes,
        "records": attachment_records
    });
    let captured_content = capture::content_root(
        &db_path,
        &tenant_id,
        &sync,
        &media,
        &work_note_attachments,
        &archives,
    )?;
    sync["contentSha256"] = json!(captured_content);
    if generation.is_none() {
        sync = Value::Null;
    }
    let (_, apply_index_size, apply_index_sha256) = crate::backup_v4::write_apply_index(
        &staging_dir,
        &tenant_id,
        generation,
        database.clone(),
        sync.clone(),
        media.clone(),
        work_note_attachments.clone(),
        archives.clone(),
        counts.clone(),
    )?;
    artifacts.push(ArtifactDigest {
        relative_path: crate::backup_v4::APPLY_INDEX_RELATIVE_PATH.to_string(),
        size: apply_index_size,
        sha256: apply_index_sha256.clone(),
    });
    let artifact_set_sha256 = artifact_set_sha256(&mut artifacts);
    let artifact_records = artifacts
        .iter()
        .map(|artifact| {
            json!({
                "relativePath": artifact.relative_path,
                "size": artifact.size,
                "sha256": artifact.sha256
            })
        })
        .collect::<Vec<_>>();
    let snapshot_ok =
        failed == 0 && missing == 0 && attachments_failed == 0 && attachments_missing == 0;
    let manifest = json!({
        "ok": snapshot_ok,
        "version": snapshot_version,
        "kind": kind,
        "generation": generation,
        "tenantId": tenant_id,
        "backupId": backup_id,
        "createdAtMs": created_at_ms,
        "source": backup_source(store, created_at_ms),
        "db": database,
        "applyIndex": {
            "relativePath": crate::backup_v4::APPLY_INDEX_RELATIVE_PATH,
            "size": apply_index_size,
            "sha256": apply_index_sha256,
        },
        "artifactSetSha256": artifact_set_sha256,
        "artifacts": artifact_records,
        "sync": sync,
        "counts": counts,
        "media": media,
        "workNoteAttachments": work_note_attachments,
        "archives": archives,
        "securityMode": "plain_warning"
    });
    let manifest_path = staging_dir.join("manifest.json");
    let manifest_raw = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("backup_manifest_encode_failed:{e}"))?;
    fs::write(&manifest_path, format!("{manifest_raw}\n"))
        .map_err(|e| format!("backup_manifest_write_failed:{e}"))?;
    let commit = json!({
        "version": 1,
        "tenantId": tenant_id,
        "backupId": backup_id,
        "generation": generation,
        "artifactSetSha256": artifact_set_sha256,
        "committedAtMs": now_ms()
    });
    let commit_raw = serde_json::to_string_pretty(&commit)
        .map_err(|e| format!("backup_commit_encode_failed:{e}"))?;
    fs::write(staging_dir.join("commit.json"), format!("{commit_raw}\n"))
        .map_err(|e| format!("backup_commit_write_failed:{e}"))?;
    fs::rename(&staging_dir, &snapshot_dir)
        .map_err(|e| format!("backup_snapshot_commit_failed:{e}"))?;
    let manifest_path = snapshot_dir.join("manifest.json");
    let final_db_path = snapshot_dir.join(&db_relative_path);
    let mut result = json!({
        "ok": snapshot_ok,
        "tenantId": tenant_id,
        "backupId": backup_id,
        "manifestPath": manifest_path.to_string_lossy(),
        "dbPath": final_db_path.to_string_lossy(),
        "kind": kind,
        "generation": generation,
        "artifactSetSha256": artifact_set_sha256,
        "databaseSha256": database_sha256,
        "contentSha256": captured_content,
        "capturedSequence": captured.sequence,
        "snapshotVersion": snapshot_version,
        "createdAtMs": created_at_ms,
        "source": manifest.get("source").cloned().unwrap_or_else(|| json!({})),
        "counts": manifest.get("counts").cloned().unwrap_or_else(|| json!({})),
        "media": manifest.get("media").cloned().unwrap_or_else(|| json!({})),
        "workNoteAttachments": manifest.get("workNoteAttachments").cloned().unwrap_or_else(|| json!({})),
        "archives": manifest.get("archives").cloned().unwrap_or_else(|| json!({}))
    });
    if snapshot_ok {
        authoritative_restore_manifest(&manifest_path, &manifest, &tenant_id)?;
    }
    // A committed backup is still usable when optional cleanup is deferred.
    // A pre-restore backup must never prune the selected restore source.
    if snapshot_ok && kind != "pre_restore" {
        result["maintenance"] = maintenance::run_if_due(store, &tenant_id, created_at_ms, false)
            .unwrap_or_else(|error| json!({ "ok": false, "error": error }));
    }
    let mut config = read_config(store);
    let root_text = root.to_string_lossy().to_string();
    config["tenants"][manifest
        .get("tenantId")
        .and_then(|value| value.as_str())
        .unwrap_or("")] = json!({
        "tenantId": manifest.get("tenantId").and_then(|value| value.as_str()).unwrap_or(""),
        "enabled": true,
        "backupRootDir": root_text,
        "intervalMs": BACKUP_INTERVAL_MS,
        "lastRunAtMs": created_at_ms,
        "lastResult": result
    });
    write_config(store, config)?;
    Ok(result)
}

pub(crate) fn run_now(store: &SqliteStore, tenant_id: String) -> Result<Value, String> {
    run_with_kind(store, tenant_id, "manual", None)
}
