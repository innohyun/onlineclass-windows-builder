use super::*;
use sha2::{Digest, Sha256};

pub(super) struct StagingGuard(pub(super) PathBuf);
impl Drop for StagingGuard {
    fn drop(&mut self) {
        // This unique directory was created by the current capture only.
        if !self.0.as_os_str().is_empty() && self.0.exists() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

pub(super) struct Capture {
    pub(super) sync: Value,
    pub(super) sequence: i64,
    pub(super) media: Vec<MediaRow>,
    pub(super) media_stamps: HashMap<String, (u64, std::time::SystemTime)>,
    pub(super) attachments: Vec<WorkNoteAttachmentRow>,
    pub(super) teaching_sources: Vec<crate::teaching_source_backup::FileRow>,
    pub(super) teaching_source_stamps: HashMap<String, (u64, std::time::SystemTime)>,
}

// The database and its record versions/attachment locators are one SQLite cut.
// Slow file hashing happens after releasing the live connection.
pub(super) fn export(
    store: &SqliteStore,
    tenant_id: &str,
    db_path: &Path,
    generation: i64,
) -> Result<Capture, String> {
    seed_sync_records(store, tenant_id)?;
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    conn.execute(
        "ATTACH DATABASE ?1 AS backup",
        params![db_path.to_string_lossy().to_string()],
    )
    .map_err(|e| format!("backup_db_attach_failed:{e}"))?;
    let result: Result<Capture, String> = (|| {
        let transaction = conn
            .unchecked_transaction()
            .map_err(|e| format!("backup_capture_transaction_failed:{e}"))?;
        transaction
            .execute_batch(&format!("{}\n{}", backup_schema_sql("backup."), crate::teaching_source_backup::schema("backup.")))
            .map_err(|e| format!("backup_schema_failed:{e}"))?;
        for table in BACKUP_TABLES {
            if !table_exists(&transaction, table.name)? {
                if table.optional {
                    continue;
                }
                return Err(format!("backup_table_required:{}", table.name));
            }
            let columns = table.columns.join(", ");
            transaction.execute(&format!(
                "INSERT INTO backup.{0} ({columns}) SELECT {columns} FROM main.{0} WHERE tenant_id=?1", table.name), params![tenant_id])
                .map_err(|e| format!("backup_table_copy_failed:{}:{e}", table.name))?;
        }
        let teaching_sources = crate::teaching_source_backup::capture(&transaction, tenant_id)?;
        let mut teaching_source_stamps = HashMap::new();
        for row in &teaching_sources {
            let source_path=crate::teaching_source_backup::source_path(store,&row.owner_uid,&row.local_path)?;
            if let Ok(stamp) = file_stamp(&source_path) {
                teaching_source_stamps.insert(format!("{}:{}", row.owner_uid, row.source_id), stamp);
            }
        }
        crate::observation_evidence::check_captured_backup(&transaction, tenant_id)?;
        check_tracking_coverage(&transaction, tenant_id)?;
        let media = media_rows_from(&transaction, tenant_id)?;
        let mut media_stamps = HashMap::new();
        for row in &media {
            if let Ok(stamp) = file_stamp(&store.data_dir.join(&row.local_path)) {
                media_stamps.insert(row.media_id.clone(), stamp);
            }
        }
        let captured = Capture {
            sync: sync_manifest(&transaction, tenant_id, generation)?,
            sequence: transaction
                .query_row(
                    "SELECT change_sequence FROM local_store_device_sync_state WHERE tenant_id=?1",
                    params![tenant_id],
                    |row| row.get(0),
                )
                .map_err(|e| format!("backup_capture_sequence_failed:{e}"))?,
            media,
            media_stamps,
            attachments: attachment_rows_from(&transaction, tenant_id)?,
            teaching_sources,
            teaching_source_stamps,
        };
        transaction
            .commit()
            .map_err(|e| format!("backup_capture_commit_failed:{e}"))?;
        Ok(captured)
    })();
    let detached = conn.execute_batch("DETACH DATABASE backup");
    let captured = result?;
    detached.map_err(|e| format!("backup_db_detach_failed:{e}"))?;
    Ok(captured)
}

fn check_tracking_coverage(conn: &Connection, tenant: &str) -> Result<(), String> {
    for table in syncable_tables() {
        if !table_exists(conn, table.name)? { continue; }
        let name = table.name;
        let key = record_key_expression(name, table);
        let missing: bool = conn.query_row(&format!(
            "SELECT EXISTS(SELECT 1 FROM main.{name} WHERE tenant_id=?1 AND NOT EXISTS (
              SELECT 1 FROM local_store_device_sync_records r WHERE r.tenant_id={name}.tenant_id
              AND r.table_name='{name}' AND r.record_key={key} AND r.tombstone=0))"),
            params![tenant], |row| row.get(0))
            .map_err(|e| format!("backup_sync_tracking_check_failed:{e}"))?;
        if missing { return Err(format!("backup_sync_tracking_incomplete:{name}")); }
    }
    Ok(())
}

pub(super) fn file_stamp(path: &Path) -> Result<(u64, std::time::SystemTime), String> {
    let metadata = fs::metadata(path).map_err(|e| format!("backup_source_metadata_failed:{e}"))?;
    Ok((
        metadata.len(),
        metadata
            .modified()
            .map_err(|e| format!("backup_source_metadata_failed:{e}"))?,
    ))
}

pub(super) fn content_root(
    db_path: &Path,
    tenant_id: &str,
    sync: &Value,
    media: &Value,
    attachments: &Value,
    teaching_sources: &Value,
    archives: &Value,
) -> Result<String, String> {
    let connection =
        Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("backup_capture_read_failed:{e}"))?;
    let records = sync
        .get("records")
        .and_then(Value::as_array)
        .ok_or("backup_sync_records_required")?;
    let mut primary = database_content_hasher(&connection, tenant_id, records)?;
    crate::teaching_source_backup::update_content_hasher(&connection, "main", tenant_id, &mut primary)?;
    for (kind, id_key, collection) in [
        (b"board-media\0".as_slice(), "mediaId", media),
        (
            b"work-note-attachment\0".as_slice(),
            "attachmentId",
            attachments,
        ),
        (b"teaching-source\0".as_slice(), "sourceKey", teaching_sources),
    ] {
        for file in collection
            .get("records")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let hash = file.get("sha256").and_then(Value::as_str).unwrap_or("");
            if hash.is_empty() {
                continue;
            }
            primary.update(kind);
            primary.update(
                file.get(id_key)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .as_bytes(),
            );
            primary.update([0]);
            primary.update(
                file.get("size")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .to_string()
                    .as_bytes(),
            );
            primary.update([0]);
            primary.update(hash.as_bytes());
            primary.update([b'\n']);
        }
    }
    let mut archive_hash = Sha256::new();
    archive_hash.update(b"classaimate-shared-archive-set-v1\0");
    let mut references = archives
        .get("records")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    references.sort_by(|a, b| {
        a.get("archiveId")
            .and_then(Value::as_str)
            .cmp(&b.get("archiveId").and_then(Value::as_str))
    });
    for reference in references {
        archive_hash.update(
            reference
                .get("archiveId")
                .and_then(Value::as_str)
                .ok_or("archive_sync_id_required")?
                .as_bytes(),
        );
        archive_hash.update([0]);
        archive_hash.update(
            reference
                .get("manifestSha256")
                .and_then(Value::as_str)
                .ok_or("archive_sync_hash_required")?
                .as_bytes(),
        );
        archive_hash.update([b'\n']);
    }
    let mut composite = Sha256::new();
    composite.update(b"classaimate-device-sync-content-v4\0");
    composite.update(format!("{:x}", primary.finalize()).as_bytes());
    composite.update([0]);
    composite.update(format!("{:x}", archive_hash.finalize()).as_bytes());
    Ok(format!("{:x}", composite.finalize()))
}

pub(super) fn statistics(db_path: &Path, tenant_id: &str) -> Result<Value, String> {
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("backup_capture_read_failed:{e}"))?;
    let mut counts = json!({});
    for (table, key) in [
        ("lesson_observations", "observationCount"),
        (
            "teacher_counseling_sessions",
            "teacherCounselingSessionCount",
        ),
        ("student_private_details", "studentPrivateDetailCount"),
        ("math_daily_attempts", "mathDailyAttemptCount"),
        ("math_daily_student_profiles", "mathDailyProfileCount"),
        ("math_daily_review_sessions", "mathDailyReviewSessionCount"),
        ("math_daily_assignments", "mathDailyAssignmentCount"),
        (
            "math_daily_assignment_results",
            "mathDailyAssignmentResultCount",
        ),
        ("math_daily_cache_runs", "mathDailyCacheRunCount"),
        ("board_post_snapshots", "boardSnapshotCount"),
        ("board_media_files", "boardMediaCount"),
        ("attendance_records", "attendanceRecordCount"),
        ("attendance_nais_checks", "attendanceNaisCheckCount"),
        (
            "attendance_document_requests",
            "attendanceDocumentRequestCount",
        ),
        ("counseling_records", "counselingRecordCount"),
        ("counseling_teacher_notes", "counselingTeacherNoteCount"),
        ("eval_assignments", "evalAssignmentCount"),
        ("eval_results", "evalResultCount"),
        ("student_record_draft_sets", "studentRecordDraftSetCount"),
        ("student_record_drafts", "studentRecordDraftCount"),
        ("local_import_runs", "importRunCount"),
        ("work_note_pages", "workNoteCount"),
        ("cloud_sync_runs", "cloudSyncRunCount"),
    ] {
        let count: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE tenant_id=?1"),
                params![tenant_id],
                |row| row.get(0),
            )
            .map_err(|e| format!("backup_capture_count_failed:{table}:{e}"))?;
        counts[key] = json!(count);
    }
    let (homes,sources,chunks,links)=crate::teaching_source_backup::counts(&conn,"main",tenant_id)?;
    counts["teachingSourceActorHomeCount"]=json!(homes);
    counts["teachingSourceCount"]=json!(sources);
    counts["teachingSourceChunkCount"]=json!(chunks);
    counts["curriculumSourceLinkCount"]=json!(links);
    Ok(counts)
}
