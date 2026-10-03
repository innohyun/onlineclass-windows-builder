//! Physical class stores. The legacy database remains the shared personal store
//! and a recovery source; it is never renamed, deleted, or used as a fallback.
use crate::{SqliteStore, DB_FILE_NAME};
use rusqlite::{params, types::ValueRef, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

#[path = "class_store_activation.rs"]
mod activation;
#[cfg(test)]
use activation::activation_path;
use activation::{register_activation, verify_activation};

const CLASS_TABLES: &[&str] = &[
    "lesson_observations",
    "teacher_counseling_sessions",
    "lesson_observation_conflicts",
    "student_private_details",
    "student_private_photos",
    "student_private_detail_conflicts",
    "math_daily_attempts",
    "math_daily_student_profiles",
    "math_daily_review_sessions",
    "math_daily_assignments",
    "math_daily_assignment_results",
    "math_daily_cache_runs",
    "board_post_snapshots",
    "board_media_files",
    "attendance_records",
    "attendance_nais_checks",
    "attendance_document_requests",
    "eval_assignments",
    "eval_results",
    "student_record_draft_sets",
    "student_record_drafts",
    "counseling_records",
    "counseling_teacher_notes",
    "local_import_runs",
    "cloud_sync_runs",
    "work_note_pages",
    "work_note_pages_fts",
    "work_note_attachments",
    "work_note_versions",
    "work_note_local_drafts",
    "teacher_roster_snapshots",
    "observation_evidence_revisions",
    "observation_evidence_batches",
    "observation_evidence_mutations",
    "observation_evidence_deletions",
    "observation_evidence_receipts",
    "observation_evidence_exports",
    "observation_evidence_reconciliation",
    "work_note_localization_receipts",
    "work_note_localization_pages",
    "work_note_localization_attachments",
    "lesson_plan_bindings",
    "teacher_counseling_mcp_drafts",
    "classaimate_mcp_local_write_receipts",
    "local_store_device_sync_state",
    "local_store_device_sync_records",
    "local_store_device_sync_conflicts",
    "local_store_device_sync_runtime",
    "local_store_backup_maintenance",
    "local_store_artifact_issue",
    "local_store_device_sync_conflict_stats",
    "local_store_restore_journal",
    "local_store_component_restore_blocks",
];
const SHARED_TABLES: &[&str] = &[
    "teaching_sources",
    "teaching_source_chunks",
    "teaching_source_chunks_fts",
    "teaching_source_actor_homes",
    "curriculum_source_links",
    "password_vault_personal_profiles",
    "password_vault_personal_entries",
    "password_vault_shared_local_devices",
    "password_vault_shared_local_state",
    "local_store_common_sync_conflict_stats",
];

fn fail(error: impl std::fmt::Display) -> String {
    format!("class_storage_migration_failed:{error}")
}
pub(crate) fn checked_tenant(tenant: &str) -> Result<&str, String> {
    if tenant.is_empty()
        || tenant.len() > 128
        || tenant == "."
        || tenant == ".."
        || !tenant
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b':' | b'-'))
    {
        return Err("class_storage_tenant_invalid".into());
    }
    // Tenant IDs remain opaque; only their filesystem representation is encoded.
    Ok(tenant)
}
pub(crate) fn class_directory(root: &Path, tenant: &str) -> Result<PathBuf, String> {
    checked_tenant(tenant)?;
    // Encode uppercase letters as well as Windows-unsafe separators. This is
    // injective even on case-insensitive filesystems and avoids reserved names.
    let mut segment: String = tenant
        .bytes()
        .map(|byte| {
            if byte.is_ascii_uppercase() || byte == b':' || byte == b'.' {
                format!("%{byte:02X}")
            } else {
                (byte as char).to_string()
            }
        })
        .collect();
    let base = tenant
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if tenant.as_bytes()[0].is_ascii_lowercase()
        && (matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (base.len() == 4
                && (base.starts_with("COM") || base.starts_with("LPT"))
                && matches!(base.as_bytes()[3], b'1'..=b'9')))
    {
        segment = format!("%{:02X}{}", tenant.as_bytes()[0], &segment[1..]);
    }
    Ok(root.join("classes").join(segment))
}
fn tables(conn: &Connection) -> Result<Vec<String>, String> {
    let mut statement = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").map_err(fail)?;
    let rows = statement
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(fail)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(fail)
}
fn shadow(name: &str) -> bool {
    ["work_note_pages_fts", "teaching_source_chunks_fts"]
        .iter()
        .any(|base| {
            ["data", "idx", "content", "docsize", "config"]
                .iter()
                .any(|suffix| name == format!("{base}_{suffix}"))
        })
}
fn catalog(conn: &Connection) -> Result<Vec<String>, String> {
    let all = tables(conn)?;
    for name in &all {
        if !CLASS_TABLES.contains(&name.as_str())
            && !SHARED_TABLES.contains(&name.as_str())
            && !shadow(name)
        {
            return Err(format!("class_storage_unknown_table:{name}"));
        }
    }
    Ok(all)
}
fn table_hash(conn: &Connection, table: &str, tenant: &str) -> Result<Value, String> {
    query_hash(
        conn,
        &format!("SELECT * FROM \"{table}\" WHERE tenant_id=?1"),
        tenant,
    )
}
fn query_hash(conn: &Connection, sql: &str, tenant: &str) -> Result<Value, String> {
    let mut statement = conn.prepare(sql).map_err(fail)?;
    let columns = statement.column_count();
    let mut rows = statement.query(params![tenant]).map_err(fail)?;
    let mut digests = Vec::new();
    while let Some(row) = rows.next().map_err(fail)? {
        let mut hash = Sha256::new();
        for index in 0..columns {
            match row.get_ref(index).map_err(fail)? {
                ValueRef::Null => hash.update([0]),
                ValueRef::Integer(v) => {
                    hash.update([1]);
                    hash.update(v.to_le_bytes());
                }
                ValueRef::Real(v) => {
                    hash.update([2]);
                    hash.update(v.to_bits().to_le_bytes());
                }
                ValueRef::Text(v) => {
                    hash.update([3]);
                    hash.update((v.len() as u64).to_le_bytes());
                    hash.update(v);
                }
                ValueRef::Blob(v) => {
                    hash.update([4]);
                    hash.update((v.len() as u64).to_le_bytes());
                    hash.update(v);
                }
            }
        }
        digests.push(hash.finalize().to_vec());
    }
    digests.sort();
    let mut hash = Sha256::new();
    for digest in &digests {
        hash.update(digest);
    }
    Ok(json!({"count":digests.len(),"sha256":format!("{:x}",hash.finalize())}))
}
fn drop_triggers(conn: &Connection) -> Result<(), String> {
    let mut statement = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='trigger'")
        .map_err(fail)?;
    let names = statement
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(fail)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(fail)?;
    drop(statement);
    for name in names {
        conn.execute_batch(&format!("DROP TRIGGER \"{}\"", name.replace('"', "\"\"")))
            .map_err(fail)?;
    }
    Ok(())
}
fn trigger_definitions(conn: &Connection) -> Result<Vec<String>, String> {
    let mut statement = conn
        .prepare("SELECT sql FROM sqlite_master WHERE type='trigger' ORDER BY name")
        .map_err(fail)?;
    let definitions = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(fail)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(fail)?;
    Ok(definitions)
}
fn restore_trigger_definitions(conn: &Connection, definitions: &[String]) -> Result<(), String> {
    for definition in definitions {
        conn.execute_batch(definition).map_err(fail)?;
    }
    Ok(())
}
fn safe_file(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("class_storage_file_path_invalid".into());
    }
    let mut result = root.to_path_buf();
    for part in path.components() {
        result.push(part.as_os_str());
        if fs::symlink_metadata(&result).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err("class_storage_file_symlink".into());
        }
    }
    Ok(result)
}
fn copy_files(conn: &Connection, root: &Path, stage: &Path, tenant: &str) -> Result<Value, String> {
    let mut copied = 0;
    for (table, path_column) in [
        ("board_media_files", "local_path"),
        ("work_note_attachments", "local_path"),
        ("work_note_localization_attachments", "staging_path"),
    ] {
        let size_column = if table == "board_media_files" {
            "size"
        } else {
            "byte_size"
        };
        let sha_column = if table == "board_media_files" {
            "COALESCE(json_extract(payload_json,'$.sha256'),'')"
        } else {
            "sha256"
        };
        let mut statement = conn
            .prepare(&format!(
                "SELECT {path_column},{size_column},{sha_column} FROM {table} WHERE tenant_id=?1"
            ))
            .map_err(fail)?;
        let rows = statement
            .query_map(params![tenant], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(fail)?;
        for row in rows {
            let (relative, size, mut sha) = row.map_err(fail)?;
            let source = safe_file(root, &relative)?;
            let destination = safe_file(stage, &relative)?;
            if sha.is_empty() {
                sha = crate::restore_journal::digest(&source)?;
            }
            if fs::metadata(&source).map_err(fail)?.len() != size as u64
                || crate::restore_journal::digest(&source)? != sha
            {
                return Err("class_storage_source_file_invalid".into());
            }
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(fail)?;
            }
            fs::copy(&source, &destination).map_err(fail)?;
            if crate::restore_journal::digest(&destination)? != sha {
                return Err("class_storage_copy_file_invalid".into());
            }
            copied += 1;
        }
    }
    Ok(json!({"verifiedFiles":copied}))
}
fn verify_database(conn: &Connection) -> Result<(), String> {
    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(fail)?;
    if integrity != "ok" {
        return Err("class_storage_integrity_failed".into());
    }
    let mut statement = conn.prepare("PRAGMA foreign_key_check").map_err(fail)?;
    if statement
        .query([])
        .map_err(fail)?
        .next()
        .map_err(fail)?
        .is_some()
    {
        return Err("class_storage_foreign_key_failed".into());
    }
    Ok(())
}

impl SqliteStore {
    pub(crate) fn with_connection(&self, conn: Connection) -> Self {
        Self {
            conn: std::sync::Mutex::new(conn),
            db_path: self.db_path.clone(),
            data_dir: self.data_dir.clone(),
            shared_data_dir: self.shared_data_dir.clone(),
            legacy_db_path: self.legacy_db_path.clone(),
            class_tenant: self.class_tenant.clone(),
            class_stores: Arc::clone(&self.class_stores),
        }
    }
    pub(crate) fn from_connection(conn: Connection, db_path: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            conn: std::sync::Mutex::new(conn),
            legacy_db_path: db_path.clone(),
            shared_data_dir: data_dir.clone(),
            db_path,
            data_dir,
            class_tenant: None,
            class_stores: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }
    pub(crate) fn shared_store(&self) -> Result<Arc<SqliteStore>, String> {
        crate::backup_restore_coordinator::common_ready(&self.shared_data_dir)?;
        let mut store = SqliteStore::open_initialized(self.legacy_db_path.clone())?;
        store.class_stores = Arc::clone(&self.class_stores);
        Ok(Arc::new(store))
    }
    // Startup and migration install schemas once. Resolving an already initialized
    // component must never rerun DDL or recovery while another connection owns a
    // canonical read/write transaction.
    pub(crate) fn open_initialized(db_path: PathBuf) -> Result<Self, String> {
        let conn =
            Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
                .map_err(fail)?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .map_err(fail)?;
        let initialized: bool = conn.query_row(
            "SELECT COUNT(*)=3 FROM sqlite_master WHERE type='table' AND name IN ('work_note_pages','local_store_restore_journal','local_store_component_restore_blocks')",
            [], |row| row.get(0),
        ).map_err(fail)?;
        if !initialized {
            return Err("class_storage_schema_uninitialized".into());
        }
        let data_dir = db_path
            .parent()
            .ok_or("class_storage_database_path_invalid")?
            .to_path_buf();
        Ok(Self::from_connection(conn, db_path, data_dir))
    }
    pub(crate) fn for_tenant(&self, tenant: &str) -> Result<Arc<SqliteStore>, String> {
        checked_tenant(tenant)?;
        crate::backup_restore_coordinator::class_ready(&self.shared_data_dir, tenant)?;
        if self
            .class_tenant
            .as_deref()
            .is_some_and(|current| current != tenant)
        {
            return Err("tenant_scope_mismatch".into());
        }
        let directory = class_directory(&self.shared_data_dir, tenant)?;
        // This external receipt survives deletion of the whole class directory.
        // Such a class must be recovered, never recreated from old legacy rows.
        verify_activation(&self.shared_data_dir, tenant, &directory)?;
        if !directory.join("class-storage.json").is_file() {
            let shared = self.shared_store()?;
            migrate(&shared, tenant, &directory)?;
        }
        let receipt: Value =
            serde_json::from_slice(&fs::read(directory.join("class-storage.json")).map_err(fail)?)
                .map_err(fail)?;
        if receipt["tenantId"] != tenant
            || receipt["migrationState"] != "verified"
            || receipt["layout"] != "class_files_v1"
        {
            return Err("class_storage_receipt_invalid".into());
        }
        if !directory.join(DB_FILE_NAME).is_file() {
            return Err("class_storage_database_missing".into());
        }
        // A crash after the durable directory rename can safely finish just this
        // registration from its verified receipt. No ready handle precedes it.
        register_activation(&self.shared_data_dir, tenant, &directory)?;
        if let Some(store) = self
            .class_stores
            .lock()
            .map_err(|_| "class_storage_registry_locked")?
            .get(tenant)
            .and_then(std::sync::Weak::upgrade)
        {
            return Ok(store);
        }
        let mut store = SqliteStore::open_initialized(directory.join(DB_FILE_NAME))?;
        store.shared_data_dir = self.shared_data_dir.clone();
        store.legacy_db_path = self.legacy_db_path.clone();
        store.class_tenant = Some(tenant.to_owned());
        store
            .conn
            .lock()
            .map_err(|_| "db_lock_failed")?
            .execute(
                "ATTACH DATABASE ?1 AS shared",
                params![store.legacy_db_path.to_string_lossy().as_ref()],
            )
            .map_err(fail)?;
        store.class_stores = Arc::clone(&self.class_stores);
        let store = Arc::new(store);
        crate::backup_restore_coordinator::class_ready(&self.shared_data_dir, tenant)?;
        let mut registry = self
            .class_stores
            .lock()
            .map_err(|_| "class_storage_registry_locked")?;
        if let Some(existing) = registry.get(tenant).and_then(std::sync::Weak::upgrade) {
            return Ok(existing);
        }
        registry.insert(tenant.to_owned(), Arc::downgrade(&store));
        Ok(store)
    }
    pub(crate) fn with_class_access<T>(
        &self,
        action: impl FnOnce(&SqliteStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let tenant = self
            .class_tenant
            .as_deref()
            .ok_or("class_storage_tenant_required")?;
        let _access = self.media_access(tenant)?;
        action(self)
    }
    pub(crate) fn class_storage_status(&self, tenant: &str) -> Result<Value, String> {
        let store = self.for_tenant(tenant)?;
        let _access = store.media_access(tenant)?;
        Ok(
            json!({"tenantId":tenant,"ready":true,"layout":"class_files_v1","migrationState":"verified",
            "verified":store.class_tenant.as_deref()==Some(tenant)}),
        )
    }
}

fn migrate(shared: &SqliteStore, tenant: &str, target: &Path) -> Result<(), String> {
    fs::create_dir_all(shared.shared_data_dir.join("classes")).map_err(fail)?;
    let _access = crate::restore_journal::access(&shared.shared_data_dir)?;
    // The filesystem lock serializes migration across processes, without holding
    // the class handle cache mutex while waiting for a database or file lock.
    if target.join("class-storage.json").is_file() {
        return Ok(());
    }
    let conn = shared.conn.lock().map_err(|_| "db_lock_failed")?;
    crate::restore_journal::ready(&conn, tenant)?;
    let all = catalog(&conn)?;
    let recovery = shared.shared_data_dir.join("class-migrations");
    fs::create_dir_all(&recovery).map_err(fail)?;
    let key = format!("{:x}", Sha256::digest(tenant.as_bytes()));
    let has_records = all
        .iter()
        .filter(|name| CLASS_TABLES.contains(&name.as_str()))
        .try_fold(false, |found, name| -> Result<bool, String> {
            let present: bool = conn
                .query_row(
                    &format!("SELECT EXISTS(SELECT 1 FROM \"{name}\" WHERE tenant_id=?1)"),
                    params![tenant],
                    |row| row.get(0),
                )
                .map_err(fail)?;
            Ok(found || present)
        })?;
    // All upgraded entry points route class writes to class files. One immutable
    // full legacy protection copy covers every old class, including local-only
    // drafts and pending receipts. An empty new class does not clone the legacy
    // database or the teacher's personal library.
    let source_path = recovery.join("legacy-protection.sqlite");
    if has_records && !source_path.is_file() {
        let candidate = recovery.join(format!(
            "legacy-protection.{}.staging",
            crate::random_url_token()
        ));
        conn.execute(
            "VACUUM INTO ?1",
            params![candidate.to_string_lossy().as_ref()],
        )
        .map_err(fail)?;
        let protected =
            Connection::open_with_flags(&candidate, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(fail)?;
        catalog(&protected)?;
        verify_database(&protected)?;
        drop(protected);
        sync_tree(&candidate)?;
        durable_rename(&candidate, &source_path)?;
    }
    let source = if has_records {
        Some(
            Connection::open_with_flags(&source_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(fail)?,
        )
    } else {
        None
    };
    let source_tables = match &source {
        Some(source) => catalog(source)?,
        None => Vec::new(),
    };
    let stage = recovery.join(format!("{key}.staging"));
    if stage.exists() {
        let retained = recovery.join(format!("{key}.interrupted-{}", crate::random_url_token()));
        durable_rename(&stage, &retained)?;
    }
    fs::create_dir_all(&stage).map_err(fail)?;
    let opened = SqliteStore::open(stage.join(DB_FILE_NAME))?;
    let staged = opened.conn.into_inner().map_err(|_| "db_lock_failed")?;
    let trigger_definitions = trigger_definitions(&staged)?;
    drop_triggers(&staged)?;
    staged
        .execute_batch("PRAGMA foreign_keys=OFF;")
        .map_err(fail)?;
    if let Some(_) = &source {
        staged
            .execute(
                "ATTACH DATABASE ?1 AS legacy",
                params![source_path.to_string_lossy().as_ref()],
            )
            .map_err(fail)?;
    }
    let tx = staged.unchecked_transaction().map_err(fail)?;
    let mut verification = BTreeMap::new();
    for table in &all {
        if !CLASS_TABLES.contains(&table.as_str()) {
            continue;
        }
        let protected_table = source_tables.contains(table);
        let before = if protected_table {
            table_hash(source.as_ref().unwrap(), table, tenant)?
        } else {
            table_hash(&conn, table, tenant)?
        };
        if !protected_table && before["count"] != 0 {
            return Err(format!("class_storage_legacy_changed:{table}"));
        }
        // Common vault triggers continue to update their original tracking rows.
        // Their current records are captured separately from the common store.
        if protected_table
            && !matches!(
                table.as_str(),
                "local_store_device_sync_state" | "local_store_device_sync_records"
            )
            && (if table == "local_import_runs" {
                // Canonical personal-library progress can change after the
                // protection copy. Class import rows still must match exactly.
                query_hash(&conn,"SELECT * FROM local_import_runs WHERE tenant_id=?1 AND kind<>'teaching_source'",tenant)?
                    != query_hash(source.as_ref().unwrap(),"SELECT * FROM local_import_runs WHERE tenant_id=?1 AND kind<>'teaching_source'",tenant)?
            } else {
                table_hash(&conn, table, tenant)? != before
            })
        {
            return Err(format!("class_storage_legacy_changed:{table}"));
        }
        if protected_table {
            // Every column is preserved, rather than using the narrower backup
            // allowlist. FTS rows are copied through the virtual table itself.
            let mut statement = tx
                .prepare(&format!("PRAGMA legacy.table_info(\"{table}\")"))
                .map_err(fail)?;
            let columns = statement
                .query_map([], |r| r.get::<_, String>(1))
                .map_err(fail)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(fail)?;
            drop(statement);
            let columns = columns
                .iter()
                .map(|name| format!("\"{}\"", name.replace('"', "\"\"")))
                .collect::<Vec<_>>()
                .join(",");
            tx.execute(&format!("INSERT INTO main.\"{table}\" ({columns}) SELECT {columns} FROM legacy.\"{table}\" WHERE tenant_id=?1"),params![tenant]).map_err(fail)?;
        }
        let after = table_hash(&tx, table, tenant)?;
        if before != after {
            return Err(format!("class_storage_table_verification_failed:{table}"));
        }
        verification.insert(table.clone(), after);
    }
    tx.commit().map_err(fail)?;
    if source.is_some() {
        staged
            .execute_batch("DETACH DATABASE legacy")
            .map_err(fail)?;
    }
    let files = copy_files(&staged, &shared.shared_data_dir, &stage, tenant)?;
    // Copying suppresses history/tracking/restore triggers only in the private
    // staging connection. All guards are installed before the ready receipt.
    restore_trigger_definitions(&staged, &trigger_definitions)?;
    staged
        .execute_batch("PRAGMA foreign_keys=ON;")
        .map_err(fail)?;
    verify_database(&staged)?;
    drop(staged);
    let archives = migrate_archives(&shared.shared_data_dir, &stage, tenant)?;
    let source_sha = if source.is_some() {
        Some(crate::restore_journal::digest(&source_path)?)
    } else {
        None
    };
    let receipt = json!({"version":1,"tenantId":tenant,"layout":"class_files_v1","migrationState":"verified",
        "sourceSha256":source_sha,"tables":verification,"files":files,"archives":archives,"verifiedAtMs":crate::now_ms()});
    fs::write(
        stage.join("class-storage.json"),
        serde_json::to_vec_pretty(&receipt).map_err(fail)?,
    )
    .map_err(fail)?;
    if target.exists() {
        return Err("class_storage_unverified_target_exists".into());
    }
    sync_tree(&stage)?;
    durable_rename(&stage, target)?;
    Ok(())
}

fn migrate_archives(root: &Path, stage: &Path, tenant: &str) -> Result<Value, String> {
    let source_path = root.join("onlineclass-shared-archive.sqlite");
    if !source_path.is_file() {
        return Ok(json!({"tables":{},"verifiedFiles":0}));
    }
    let source = Connection::open(&source_path).map_err(fail)?;
    for name in tables(&source)? {
        if ![
            "shared_archives",
            "shared_archive_records",
            "shared_archive_files",
        ]
        .contains(&name.as_str())
        {
            return Err(format!("class_storage_unknown_archive_table:{name}"));
        }
    }
    let has_records: bool = source
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM shared_archives WHERE tenant_id=?1)",
            params![tenant],
            |row| row.get(0),
        )
        .map_err(fail)?;
    if !has_records {
        return Ok(json!({"tables":{},"verifiedFiles":0}));
    }
    let queries = [
        ("shared_archives", "SELECT * FROM shared_archives WHERE tenant_id=?1"),
        ("shared_archive_records", "SELECT * FROM shared_archive_records WHERE archive_id IN(SELECT id FROM shared_archives WHERE tenant_id=?1)"),
        ("shared_archive_files", "SELECT archive_id,ordinal,original_name,content_type,byte_size,sha256 FROM shared_archive_files WHERE archive_id IN(SELECT id FROM shared_archives WHERE tenant_id=?1)")
    ];
    let mut expected = BTreeMap::new();
    for (name, query) in queries {
        expected.insert(name, query_hash(&source, query, tenant)?);
    }
    let db = stage.join("onlineclass-shared-archive.sqlite");
    source
        .execute("VACUUM INTO ?1", params![db.to_string_lossy().as_ref()])
        .map_err(fail)?;
    let conn = Connection::open(&db).map_err(fail)?;
    for name in tables(&conn)? {
        if ![
            "shared_archives",
            "shared_archive_records",
            "shared_archive_files",
        ]
        .contains(&name.as_str())
        {
            return Err(format!("class_storage_unknown_archive_table:{name}"));
        }
    }
    let trigger_definitions = trigger_definitions(&conn)?;
    drop_triggers(&conn)?;
    conn.execute_batch("PRAGMA foreign_keys=OFF;")
        .map_err(fail)?;
    for name in ["shared_archive_records", "shared_archive_files"] {
        conn.execute(&format!("DELETE FROM {name} WHERE archive_id NOT IN (SELECT id FROM shared_archives WHERE tenant_id=?1)"),params![tenant]).map_err(fail)?;
    }
    conn.execute(
        "DELETE FROM shared_archives WHERE tenant_id<>?1",
        params![tenant],
    )
    .map_err(fail)?;
    let mut statement = conn
        .prepare("SELECT archive_id,ordinal,local_path,sha256,byte_size FROM shared_archive_files")
        .map_err(fail)?;
    let rows = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })
        .map_err(fail)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(fail)?;
    drop(statement);
    let files_count = rows.len();
    for (archive, ordinal, path, sha, size) in rows {
        let source = PathBuf::from(path);
        let relative = source
            .strip_prefix(root)
            .map_err(|_| "class_storage_archive_path_invalid")?;
        let source = safe_file(root, &relative.to_string_lossy())?;
        let destination = safe_file(stage, &relative.to_string_lossy())?;
        if crate::restore_journal::digest(&source)? != sha
            || fs::metadata(&source).map_err(fail)?.len() != size as u64
        {
            return Err("class_storage_archive_file_invalid".into());
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(fail)?;
        }
        fs::copy(&source, &destination).map_err(fail)?;
        if crate::restore_journal::digest(&destination)? != sha {
            return Err("class_storage_archive_copy_invalid".into());
        }
        // Resolve absolute archive locators after the staging directory rename.
        let final_path = class_directory(root, tenant)?.join(relative);
        conn.execute(
            "UPDATE shared_archive_files SET local_path=?1 WHERE archive_id=?2 AND ordinal=?3",
            params![final_path.to_string_lossy().as_ref(), archive, ordinal],
        )
        .map_err(fail)?;
    }
    restore_trigger_definitions(&conn, &trigger_definitions)?;
    conn.execute_batch("VACUUM; PRAGMA foreign_keys=ON;")
        .map_err(fail)?;
    verify_database(&conn)?;
    for (name, query) in queries {
        if query_hash(&conn, query, tenant)? != expected[name]
            || query_hash(&source, query, tenant)? != expected[name]
        {
            return Err(format!("class_storage_archive_verification_failed:{name}"));
        }
    }
    Ok(json!({"tables":expected,"verifiedFiles":files_count}))
}

fn sync_tree(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(fail)?;
    if metadata.file_type().is_symlink() {
        return Err("class_storage_file_symlink".into());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(fail)? {
            sync_tree(&entry.map_err(fail)?.path())?;
        }
        #[cfg(unix)]
        fs::File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(fail)?;
    } else {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .and_then(|file| file.sync_all())
            .map_err(fail)?;
    }
    Ok(())
}
fn durable_rename(from: &Path, to: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let from_wide: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
        let to_wide: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(
                from_wide.as_ptr(),
                to_wide.as_ptr(),
                windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok == 0 {
            return Err(fail(std::io::Error::last_os_error()));
        }
    }
    #[cfg(not(windows))]
    fs::rename(from, to).map_err(fail)?;
    #[cfg(unix)]
    {
        if let Some(parent) = from.parent() {
            fs::File::open(parent)
                .and_then(|file| file.sync_all())
                .map_err(fail)?;
        }
        if to.parent() != from.parent() {
            if let Some(parent) = to.parent() {
                fs::File::open(parent)
                    .and_then(|file| file.sync_all())
                    .map_err(fail)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "class_store_tests.rs"]
mod tests;
