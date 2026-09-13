use crate::{backup, SqliteStore};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

const SOURCE_COLUMNS: &str = "owner_uid,source_id,origin_tenant_id,source_type,grade,semester_scope,subject_code,title,publisher,original_file_name,content_type,byte_size,sha256,local_path,extraction_status,lifecycle_status,extractor_version,page_count,revision,created_at_ms,updated_at_ms";
const CHUNK_COLUMNS: &str = "owner_uid,source_id,chunk_id,ordinal,page_start,page_end,unit,topic,text,text_sha256,revision,created_at_ms,updated_at_ms";
const LINK_COLUMNS: &str = "tenant_id,owner_uid,school_year,semester,grade,curriculum_source_kind,curriculum_source_scope_id,curriculum_item_id,curriculum_source_revision,teaching_source_revision,subject_code_snapshot,unit_snapshot,title_snapshot,source_id,chunk_id,revision,created_at_ms,updated_at_ms";

#[derive(Clone, Debug)]
pub(crate) struct FileRow {
    pub(crate) owner_uid: String,
    pub(crate) source_id: String,
    pub(crate) local_path: String,
    pub(crate) size: u64,
    pub(crate) sha256: String,
    pub(crate) revision: i64,
    pub(crate) bundle_sha256: String,
    pub(crate) updated_at_ms: i64,
}

pub(crate) fn schema(prefix: &str) -> String {
    format!(
        r#"
      CREATE TABLE IF NOT EXISTS {prefix}teaching_source_actor_homes(
        owner_uid TEXT PRIMARY KEY, backup_tenant_id TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS {prefix}teaching_sources(
        owner_uid TEXT NOT NULL, source_id TEXT NOT NULL, origin_tenant_id TEXT NOT NULL,
        source_type TEXT NOT NULL, grade TEXT NOT NULL, semester_scope TEXT NOT NULL,
        subject_code TEXT NOT NULL, title TEXT NOT NULL, publisher TEXT NOT NULL,
        original_file_name TEXT NOT NULL, content_type TEXT NOT NULL, byte_size INTEGER NOT NULL,
        sha256 TEXT, local_path TEXT, extraction_status TEXT NOT NULL,
        lifecycle_status TEXT NOT NULL, extractor_version TEXT, page_count INTEGER,
        revision INTEGER NOT NULL, created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY(owner_uid,source_id)
      );
      CREATE TABLE IF NOT EXISTS {prefix}teaching_source_chunks(
        owner_uid TEXT NOT NULL, source_id TEXT NOT NULL, chunk_id TEXT NOT NULL,
        ordinal INTEGER NOT NULL, page_start INTEGER, page_end INTEGER,
        unit TEXT NOT NULL, topic TEXT NOT NULL, text TEXT NOT NULL, text_sha256 TEXT NOT NULL,
        revision INTEGER NOT NULL, created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY(owner_uid,source_id,chunk_id), UNIQUE(owner_uid,source_id,ordinal)
      );
      CREATE TABLE IF NOT EXISTS {prefix}curriculum_source_links(
        tenant_id TEXT NOT NULL, owner_uid TEXT NOT NULL, school_year INTEGER NOT NULL,
        semester INTEGER NOT NULL, grade INTEGER, curriculum_source_kind TEXT NOT NULL,
        curriculum_source_scope_id TEXT NOT NULL, curriculum_item_id TEXT NOT NULL,
        curriculum_source_revision INTEGER NOT NULL, teaching_source_revision INTEGER NOT NULL,
        subject_code_snapshot TEXT NOT NULL, unit_snapshot TEXT NOT NULL, title_snapshot TEXT NOT NULL,
        source_id TEXT NOT NULL, chunk_id TEXT NOT NULL, revision INTEGER NOT NULL,
        created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL,
        PRIMARY KEY(tenant_id,owner_uid,school_year,semester,curriculum_source_kind,
          curriculum_source_scope_id,curriculum_item_id,source_id,chunk_id)
      );
    "#
    )
}

pub(crate) fn capture(conn: &Connection, tenant_id: &str) -> Result<Vec<FileRow>, String> {
    conn.execute(
        "INSERT INTO backup.teaching_source_actor_homes(owner_uid,backup_tenant_id,created_at_ms,updated_at_ms)
         SELECT owner_uid,backup_tenant_id,created_at_ms,updated_at_ms FROM main.teaching_source_actor_homes
         WHERE backup_tenant_id=?1",
        params![tenant_id],
    ).map_err(|e| format!("backup_teaching_source_home_copy_failed:{e}"))?;
    conn.execute(&format!(
        "INSERT INTO backup.teaching_sources({SOURCE_COLUMNS}) SELECT {source_select}
         FROM main.teaching_sources s JOIN main.teaching_source_actor_homes h ON h.owner_uid=s.owner_uid
         WHERE h.backup_tenant_id=?1",
        source_select = prefixed_columns("s", SOURCE_COLUMNS),
    ), params![tenant_id]).map_err(|e| format!("backup_teaching_source_copy_failed:{e}"))?;
    conn.execute(
        &format!(
            "INSERT INTO backup.teaching_source_chunks({CHUNK_COLUMNS}) SELECT {chunk_select}
         FROM main.teaching_source_chunks c JOIN backup.teaching_sources s
           ON s.owner_uid=c.owner_uid AND s.source_id=c.source_id",
            chunk_select = prefixed_columns("c", CHUNK_COLUMNS),
        ),
        [],
    )
    .map_err(|e| format!("backup_teaching_source_chunk_copy_failed:{e}"))?;
    conn.execute(
        &format!(
            "INSERT INTO backup.curriculum_source_links({LINK_COLUMNS}) SELECT {link_select}
         FROM main.curriculum_source_links l JOIN backup.teaching_sources s
           ON s.owner_uid=l.owner_uid AND s.source_id=l.source_id",
            link_select = prefixed_columns("l", LINK_COLUMNS),
        ),
        [],
    )
    .map_err(|e| format!("backup_teaching_source_link_copy_failed:{e}"))?;
    file_rows(conn, "main", tenant_id)
}

fn prefixed_columns(alias: &str, columns: &str) -> String {
    columns
        .split(',')
        .map(|column| format!("{alias}.{column}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn valid_schema(schema: &str) -> Result<&str, String> {
    match schema {
        "main" | "restore" | "backup" => Ok(schema),
        _ => Err("teaching_source_backup_schema_invalid".into()),
    }
}

fn table_exists(conn: &Connection, schema: &str, table: &str) -> Result<bool, String> {
    let schema = valid_schema(schema)?;
    conn.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM {schema}.sqlite_master WHERE type='table' AND name=?1)"
        ),
        params![table],
        |row| row.get::<_, i64>(0),
    )
    .map(|value| value == 1)
    .map_err(|e| format!("teaching_source_backup_table_check_failed:{e}"))
}

fn bundle_sha256(
    conn: &Connection,
    schema: &str,
    owner: &str,
    source: &str,
) -> Result<String, String> {
    let schema = valid_schema(schema)?;
    let mut hasher = Sha256::new();
    for (kind, sql) in [
        ("source", format!("SELECT json_array({}) FROM {schema}.teaching_sources s WHERE owner_uid=?1 AND source_id=?2", prefixed_columns("s", SOURCE_COLUMNS))),
        ("chunk", format!("SELECT json_array({}) FROM {schema}.teaching_source_chunks c WHERE owner_uid=?1 AND source_id=?2 ORDER BY ordinal,chunk_id", prefixed_columns("c", CHUNK_COLUMNS))),
        ("link", format!("SELECT json_array({}) FROM {schema}.curriculum_source_links l WHERE owner_uid=?1 AND source_id=?2 ORDER BY tenant_id,school_year,semester,curriculum_source_kind,curriculum_source_scope_id,curriculum_item_id,chunk_id", prefixed_columns("l", LINK_COLUMNS))),
    ] {
        hasher.update(kind.as_bytes());
        hasher.update([0]);
        let mut statement = conn.prepare(&sql).map_err(|e| format!("teaching_source_backup_hash_prepare_failed:{e}"))?;
        let rows = statement.query_map(params![owner,source], |row| row.get::<_, String>(0))
            .map_err(|e| format!("teaching_source_backup_hash_query_failed:{e}"))?;
        for row in rows {
            hasher.update(row.map_err(|e| format!("teaching_source_backup_hash_row_failed:{e}"))?.as_bytes());
            hasher.update([b'\n']);
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub(crate) fn file_rows(
    conn: &Connection,
    schema: &str,
    tenant_id: &str,
) -> Result<Vec<FileRow>, String> {
    let schema = valid_schema(schema)?;
    if !table_exists(conn, schema, "teaching_sources")? {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT s.owner_uid,s.source_id,s.local_path,s.byte_size,s.sha256,s.revision,s.updated_at_ms
         FROM {schema}.teaching_sources s JOIN {schema}.teaching_source_actor_homes h ON h.owner_uid=s.owner_uid
         WHERE h.backup_tenant_id=?1 AND s.local_path IS NOT NULL AND s.sha256 IS NOT NULL
         ORDER BY s.owner_uid,s.source_id"
    );
    let mut statement = conn
        .prepare(&sql)
        .map_err(|e| format!("backup_teaching_source_files_prepare_failed:{e}"))?;
    let rows = statement
        .query_map(params![tenant_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .map_err(|e| format!("backup_teaching_source_files_query_failed:{e}"))?;
    let mut output = Vec::new();
    for row in rows {
        let (owner_uid, source_id, local_path, size, sha256, revision, updated_at_ms) =
            row.map_err(|e| format!("backup_teaching_source_files_row_failed:{e}"))?;
        if size < 0 {
            return Err("backup_teaching_source_file_size_invalid".into());
        }
        output.push(FileRow {
            bundle_sha256: bundle_sha256(conn, schema, &owner_uid, &source_id)?,
            owner_uid,
            source_id,
            local_path,
            size: size as u64,
            sha256,
            revision,
            updated_at_ms,
        });
    }
    Ok(output)
}

pub(crate) fn update_content_hasher(
    conn: &Connection,
    schema: &str,
    tenant_id: &str,
    hasher: &mut Sha256,
) -> Result<(), String> {
    let schema = valid_schema(schema)?;
    if !table_exists(conn, schema, "teaching_source_actor_homes")? {
        return Ok(());
    }
    for row in file_rows(conn, schema, tenant_id)? {
        hasher.update(b"teaching-source-bundle\0");
        hasher.update(row.owner_uid.as_bytes());
        hasher.update([0]);
        hasher.update(row.source_id.as_bytes());
        hasher.update([0]);
        hasher.update(row.bundle_sha256.as_bytes());
        hasher.update([b'\n']);
    }
    let sql = format!(
        "SELECT s.owner_uid,s.source_id FROM {schema}.teaching_sources s
         JOIN {schema}.teaching_source_actor_homes h ON h.owner_uid=s.owner_uid
         WHERE h.backup_tenant_id=?1 AND s.local_path IS NULL ORDER BY s.owner_uid,s.source_id"
    );
    let mut statement = conn
        .prepare(&sql)
        .map_err(|e| format!("teaching_source_backup_content_prepare_failed:{e}"))?;
    let rows = statement
        .query_map(params![tenant_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| format!("teaching_source_backup_content_query_failed:{e}"))?;
    for row in rows {
        let (owner, source) =
            row.map_err(|e| format!("teaching_source_backup_content_row_failed:{e}"))?;
        hasher.update(b"teaching-source-bundle\0");
        hasher.update(owner.as_bytes());
        hasher.update([0]);
        hasher.update(source.as_bytes());
        hasher.update([0]);
        hasher.update(bundle_sha256(conn, schema, &owner, &source)?.as_bytes());
        hasher.update([b'\n']);
    }
    Ok(())
}

fn validate_schema(conn: &Connection, schema: &str, tenant_id: &str) -> Result<bool, String> {
    let schema = valid_schema(schema)?;
    let tables = [
        "teaching_source_actor_homes",
        "teaching_sources",
        "teaching_source_chunks",
        "curriculum_source_links",
    ];
    let present = tables
        .iter()
        .map(|table| table_exists(conn, schema, table))
        .collect::<Result<Vec<_>, _>>()?;
    if present.iter().all(|value| !*value) {
        return Ok(false);
    }
    if present.iter().any(|value| !*value)
        || table_exists(conn, schema, "teaching_source_chunks_fts")?
    {
        return Err("teaching_source_backup_schema_invalid".into());
    }
    let invalid_home: bool=conn.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {schema}.teaching_source_actor_homes WHERE backup_tenant_id<>?1)"),params![tenant_id],|r|r.get(0)).map_err(|e|format!("teaching_source_backup_home_check_failed:{e}"))?;
    let invalid_source: bool=conn.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {schema}.teaching_sources s LEFT JOIN {schema}.teaching_source_actor_homes h ON h.owner_uid=s.owner_uid WHERE h.owner_uid IS NULL)"),[],|r|r.get(0)).map_err(|e|format!("teaching_source_backup_source_check_failed:{e}"))?;
    let invalid_chunk: bool=conn.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {schema}.teaching_source_chunks c LEFT JOIN {schema}.teaching_sources s ON s.owner_uid=c.owner_uid AND s.source_id=c.source_id WHERE s.source_id IS NULL)"),[],|r|r.get(0)).map_err(|e|format!("teaching_source_backup_chunk_check_failed:{e}"))?;
    let invalid_link: bool=conn.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {schema}.curriculum_source_links l LEFT JOIN {schema}.teaching_sources s ON s.owner_uid=l.owner_uid AND s.source_id=l.source_id WHERE s.source_id IS NULL)"),[],|r|r.get(0)).map_err(|e|format!("teaching_source_backup_link_check_failed:{e}"))?;
    if invalid_home || invalid_source || invalid_chunk || invalid_link {
        return Err("teaching_source_backup_scope_invalid".into());
    }
    let mut stmt=conn.prepare(&format!("SELECT owner_uid,local_path,sha256,byte_size FROM {schema}.teaching_sources WHERE local_path IS NOT NULL OR sha256 IS NOT NULL OR byte_size>0")).map_err(|e|format!("teaching_source_backup_path_prepare_failed:{e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|e| format!("teaching_source_backup_path_query_failed:{e}"))?;
    for row in rows {
        let (owner, path, sha, size) =
            row.map_err(|e| format!("teaching_source_backup_path_row_failed:{e}"))?;
        let path = path.ok_or("teaching_source_backup_file_metadata_invalid")?;
        let sha = sha.ok_or("teaching_source_backup_file_metadata_invalid")?;
        let safe =
            backup::safe_relative_path(&path).ok_or("teaching_source_backup_path_invalid")?;
        let expected = PathBuf::from("teaching-sources")
            .join(crate::teaching_sources::actor_folder(&owner))
            .join("objects");
        if !safe.starts_with(expected)
            || size < 1
            || sha.len() != 64
            || !sha.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("teaching_source_backup_file_metadata_invalid".into());
        }
    }
    Ok(true)
}

pub(crate) fn validate_restore(conn: &Connection, tenant_id: &str) -> Result<bool, String> {
    validate_schema(conn, "restore", tenant_id)
}
pub(crate) fn validate_snapshot(conn: &Connection, tenant_id: &str) -> Result<bool, String> {
    validate_schema(conn, "main", tenant_id)
}

pub(crate) fn counts(
    conn: &Connection,
    schema: &str,
    tenant_id: &str,
) -> Result<(i64, i64, i64, i64), String> {
    let schema = valid_schema(schema)?;
    if !table_exists(conn, schema, "teaching_sources")? {
        return Ok((0, 0, 0, 0));
    }
    let homes:i64=conn.query_row(&format!("SELECT COUNT(*) FROM {schema}.teaching_source_actor_homes WHERE backup_tenant_id=?1"),params![tenant_id],|r|r.get(0)).map_err(|e|format!("teaching_source_backup_home_count_failed:{e}"))?;
    let sources:i64=conn.query_row(&format!("SELECT COUNT(*) FROM {schema}.teaching_sources s JOIN {schema}.teaching_source_actor_homes h ON h.owner_uid=s.owner_uid WHERE h.backup_tenant_id=?1"),params![tenant_id],|r|r.get(0)).map_err(|e|format!("teaching_source_backup_source_count_failed:{e}"))?;
    let chunks:i64=conn.query_row(&format!("SELECT COUNT(*) FROM {schema}.teaching_source_chunks c JOIN {schema}.teaching_source_actor_homes h ON h.owner_uid=c.owner_uid WHERE h.backup_tenant_id=?1"),params![tenant_id],|r|r.get(0)).map_err(|e|format!("teaching_source_backup_chunk_count_failed:{e}"))?;
    let links:i64=conn.query_row(&format!("SELECT COUNT(*) FROM {schema}.curriculum_source_links l JOIN {schema}.teaching_source_actor_homes h ON h.owner_uid=l.owner_uid WHERE h.backup_tenant_id=?1"),params![tenant_id],|r|r.get(0)).map_err(|e|format!("teaching_source_backup_link_count_failed:{e}"))?;
    Ok((homes, sources, chunks, links))
}

fn current_state(
    conn: &Connection,
    owner: &str,
    source: &str,
) -> Result<Option<(i64, String)>, String> {
    let revision = conn
        .query_row(
            "SELECT revision FROM main.teaching_sources WHERE owner_uid=?1 AND source_id=?2",
            params![owner, source],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|e| format!("teaching_source_restore_current_read_failed:{e}"))?;
    revision
        .map(|revision| Ok((revision, bundle_sha256(conn, "main", owner, source)?)))
        .transpose()
}

pub(crate) fn current_guard(
    conn: &Connection,
    owner: &str,
    source: &str,
) -> Result<Option<String>, String> {
    current_state(conn, owner, source)
        .map(|state| state.map(|(revision, hash)| format!("{revision}:{hash}")))
}

pub(crate) fn should_restore_file(
    store: &SqliteStore,
    incoming: &FileRow,
) -> Result<(bool, Option<String>), String> {
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let current = current_state(&conn, &incoming.owner_uid, &incoming.source_id)?;
    let guard = current
        .as_ref()
        .map(|(revision, hash)| format!("{revision}:{hash}"));
    if let Some((revision, hash)) = &current {
        if *revision == incoming.revision && hash != &incoming.bundle_sha256 {
            return Err("teaching_source_restore_revision_conflict".into());
        }
        if *revision > incoming.revision {
            return Ok((false, guard));
        }
        if *revision == incoming.revision {
            let file=conn.query_row("SELECT local_path,sha256,byte_size FROM teaching_sources WHERE owner_uid=?1 AND source_id=?2",params![incoming.owner_uid,incoming.source_id],|r|Ok((r.get::<_,Option<String>>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,i64>(2)?))).optional().map_err(|e|format!("teaching_source_restore_file_read_failed:{e}"))?;
            drop(conn);
            let metadata_ok = file.is_some_and(|(path, sha, size)| {
                path.as_deref() == Some(incoming.local_path.as_str())
                    && sha.as_deref() == Some(incoming.sha256.as_str())
                    && size == incoming.size as i64
            });
            let file_ok = if metadata_ok {
                let target = source_path(store, &incoming.owner_uid, &incoming.local_path)?;
                target.is_file()
                    && backup::sha256_file(&target)? == (incoming.size, incoming.sha256.clone())
            } else {
                false
            };
            return Ok((!file_ok, guard));
        }
    }
    Ok((true, guard))
}

pub(crate) fn apply(transaction: &Transaction<'_>, tenant_id: &str) -> Result<(i64, i64), String> {
    if !validate_restore(transaction, tenant_id)? {
        return Ok((0, 0));
    }
    let incoming = file_rows(transaction, "restore", tenant_id)?;
    let mut keys = incoming
        .iter()
        .map(|row| {
            (
                row.owner_uid.clone(),
                row.source_id.clone(),
                row.revision,
                row.bundle_sha256.clone(),
            )
        })
        .collect::<Vec<_>>();
    let mut no_file=transaction.prepare("SELECT s.owner_uid,s.source_id,s.revision FROM restore.teaching_sources s JOIN restore.teaching_source_actor_homes h ON h.owner_uid=s.owner_uid WHERE h.backup_tenant_id=?1 AND s.local_path IS NULL ORDER BY s.owner_uid,s.source_id").map_err(|e|format!("teaching_source_restore_sources_prepare_failed:{e}"))?;
    let rows = no_file
        .query_map(params![tenant_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(|e| format!("teaching_source_restore_sources_query_failed:{e}"))?;
    for row in rows {
        let (owner, source, revision) =
            row.map_err(|e| format!("teaching_source_restore_sources_row_failed:{e}"))?;
        keys.push((
            owner.clone(),
            source.clone(),
            revision,
            bundle_sha256(transaction, "restore", &owner, &source)?,
        ));
    }
    drop(no_file);
    let mut applied = 0_i64;
    let mut retained = 0_i64;
    for (owner, source, incoming_revision, incoming_hash) in keys {
        if let Some(home) = transaction
            .query_row(
                "SELECT backup_tenant_id FROM main.teaching_source_actor_homes WHERE owner_uid=?1",
                params![owner],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| format!("teaching_source_restore_home_read_failed:{e}"))?
        {
            if home != tenant_id {
                return Err("teaching_source_restore_actor_home_conflict".into());
            }
        }
        if let Some((current_revision, current_hash)) = current_state(transaction, &owner, &source)?
        {
            if current_revision == incoming_revision && current_hash != incoming_hash {
                return Err("teaching_source_restore_revision_conflict".into());
            }
            if current_revision >= incoming_revision {
                if current_revision > incoming_revision {
                    retained += 1;
                }
                continue;
            }
        }
        transaction
            .execute(
                "DELETE FROM main.teaching_source_chunks_fts WHERE owner_uid=?1 AND source_id=?2",
                params![owner, source],
            )
            .map_err(|e| format!("teaching_source_restore_fts_clear_failed:{e}"))?;
        transaction
            .execute(
                "DELETE FROM main.curriculum_source_links WHERE owner_uid=?1 AND source_id=?2",
                params![owner, source],
            )
            .map_err(|e| format!("teaching_source_restore_link_clear_failed:{e}"))?;
        transaction
            .execute(
                "DELETE FROM main.teaching_source_chunks WHERE owner_uid=?1 AND source_id=?2",
                params![owner, source],
            )
            .map_err(|e| format!("teaching_source_restore_chunk_clear_failed:{e}"))?;
        transaction
            .execute(
                "DELETE FROM main.teaching_sources WHERE owner_uid=?1 AND source_id=?2",
                params![owner, source],
            )
            .map_err(|e| format!("teaching_source_restore_source_clear_failed:{e}"))?;
        transaction.execute("INSERT INTO main.teaching_source_actor_homes(owner_uid,backup_tenant_id,created_at_ms,updated_at_ms) SELECT owner_uid,backup_tenant_id,created_at_ms,updated_at_ms FROM restore.teaching_source_actor_homes WHERE owner_uid=?1 ON CONFLICT(owner_uid) DO NOTHING",params![owner]).map_err(|e|format!("teaching_source_restore_home_insert_failed:{e}"))?;
        transaction.execute(&format!("INSERT INTO main.teaching_sources({SOURCE_COLUMNS}) SELECT {SOURCE_COLUMNS} FROM restore.teaching_sources WHERE owner_uid=?1 AND source_id=?2"),params![owner,source]).map_err(|e|format!("teaching_source_restore_source_insert_failed:{e}"))?;
        transaction.execute(&format!("INSERT INTO main.teaching_source_chunks({CHUNK_COLUMNS}) SELECT {CHUNK_COLUMNS} FROM restore.teaching_source_chunks WHERE owner_uid=?1 AND source_id=?2"),params![owner,source]).map_err(|e|format!("teaching_source_restore_chunk_insert_failed:{e}"))?;
        transaction.execute(&format!("INSERT INTO main.curriculum_source_links({LINK_COLUMNS}) SELECT {LINK_COLUMNS} FROM restore.curriculum_source_links WHERE owner_uid=?1 AND source_id=?2"),params![owner,source]).map_err(|e|format!("teaching_source_restore_link_insert_failed:{e}"))?;
        applied += 1;
    }
    transaction.execute("DELETE FROM main.teaching_source_chunks_fts WHERE owner_uid IN(SELECT owner_uid FROM restore.teaching_source_actor_homes)",[]).map_err(|e|format!("teaching_source_restore_fts_reset_failed:{e}"))?;
    transaction.execute("INSERT INTO main.teaching_source_chunks_fts(owner_uid,source_id,chunk_id,subject_code,title,unit,topic,text) SELECT c.owner_uid,c.source_id,c.chunk_id,s.subject_code,s.title,c.unit,c.topic,c.text FROM main.teaching_source_chunks c JOIN main.teaching_sources s ON s.owner_uid=c.owner_uid AND s.source_id=c.source_id WHERE c.owner_uid IN(SELECT owner_uid FROM restore.teaching_source_actor_homes)",[]).map_err(|e|format!("teaching_source_restore_fts_rebuild_failed:{e}"))?;
    if retained > 0 {
        let now = chrono::Utc::now().timestamp_millis();
        transaction.execute("INSERT INTO local_store_device_sync_state(tenant_id,first_dirty_at_ms,last_dirty_at_ms,change_sequence) VALUES(?1,?2,?2,1) ON CONFLICT(tenant_id) DO UPDATE SET first_dirty_at_ms=COALESCE(first_dirty_at_ms,excluded.first_dirty_at_ms),last_dirty_at_ms=excluded.last_dirty_at_ms,change_sequence=change_sequence+1",params![tenant_id,now]).map_err(|e|format!("teaching_source_restore_retain_dirty_failed:{e}"))?;
    }
    Ok((applied, retained))
}

pub(crate) fn validate_manifest_file(
    row: &FileRow,
    revision: i64,
    local_path: &str,
    size: u64,
    sha256: &str,
    bundle_sha256: &str,
) -> Result<(), String> {
    if row.revision != revision
        || row.local_path != local_path
        || row.size != size
        || row.sha256 != sha256
        || row.bundle_sha256 != bundle_sha256
    {
        return Err("teaching_source_backup_manifest_mismatch".into());
    }
    Ok(())
}

pub(crate) fn checked_target(
    store: &SqliteStore,
    owner: &str,
    relative: &str,
) -> Result<PathBuf, String> {
    let safe = backup::safe_relative_path(&relative.replace('\\', "/"))
        .ok_or("teaching_source_backup_path_invalid")?;
    let expected = Path::new("teaching-sources")
        .join(crate::teaching_sources::actor_folder(owner))
        .join("objects");
    if !safe.starts_with(expected) {
        return Err("teaching_source_backup_path_invalid".into());
    }
    Ok(store.data_dir.join(safe))
}

pub(crate) fn source_path(
    store: &SqliteStore,
    owner: &str,
    relative: &str,
) -> Result<PathBuf, String> {
    let target = checked_target(store, owner, relative)?;
    let root =
        fs::canonicalize(&store.data_dir).map_err(|e| format!("local_data_dir_missing:{e}"))?;
    let relative_target = target
        .strip_prefix(&store.data_dir)
        .map_err(|_| "teaching_source_backup_path_invalid")?;
    let mut cursor = root.clone();
    for component in relative_target.components() {
        let std::path::Component::Normal(part) = component else {
            return Err("teaching_source_backup_path_invalid".into());
        };
        cursor.push(part);
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("teaching_source_backup_path_invalid".into())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(target),
            Err(error) => {
                return Err(format!(
                    "teaching_source_backup_path_inspect_failed:{error}"
                ))
            }
        }
    }
    let resolved = fs::canonicalize(&target)
        .map_err(|e| format!("teaching_source_backup_path_resolve_failed:{e}"))?;
    if !resolved.starts_with(root) {
        return Err("teaching_source_backup_path_invalid".into());
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_applies_actor_library_and_rebuilds_fts() {
        let root = std::env::temp_dir().join(format!(
            "teaching-source-backup-test-{}",
            crate::random_url_token()
        ));
        fs::create_dir_all(&root).expect("create test directory");
        let store = SqliteStore::open(root.join("store.sqlite")).expect("open test store");
        {
            let mut conn = store.conn.lock().expect("lock store");
            conn.execute_batch(&format!(
                "ATTACH DATABASE ':memory:' AS restore;{}",
                schema("restore.")
            ))
            .expect("create restore schema");
            conn.execute(
                "INSERT INTO restore.teaching_source_actor_homes VALUES('owner-a','tenant-a',1,1)",
                [],
            )
            .expect("insert home");
            conn.execute("INSERT INTO restore.teaching_sources(owner_uid,source_id,origin_tenant_id,source_type,grade,semester_scope,subject_code,title,publisher,original_file_name,content_type,byte_size,sha256,local_path,extraction_status,lifecycle_status,extractor_version,page_count,revision,created_at_ms,updated_at_ms) VALUES('owner-a','source-a','tenant-a','textbook','5','2','SOC','합성 사회','합성출판','social.pdf','application/pdf',0,NULL,NULL,'ready','active','test-v1',1,3,1,1)",[]).expect("insert source");
            conn.execute("INSERT INTO restore.teaching_source_chunks VALUES('owner-a','source-a','chunk-a',0,1,1,'지역','우리 고장','지역 사회 변화 자료','hash',1,1,1)",[]).expect("insert chunk");
            conn.execute("INSERT INTO restore.curriculum_source_links VALUES('tenant-a','owner-a',2026,2,5,'class','scope-a','curriculum-a',4,3,'SOC','지역','우리 고장','source-a','chunk-a',1,1,1)",[]).expect("insert link");
            let transaction = conn.transaction().expect("begin restore");
            assert_eq!(
                apply(&transaction, "tenant-a").expect("apply source backup"),
                (1, 0)
            );
            transaction.commit().expect("commit restore");
            let fts_count:i64=conn.query_row("SELECT COUNT(*) FROM teaching_source_chunks_fts WHERE owner_uid='owner-a' AND source_id='source-a'",[],|row|row.get(0)).expect("read rebuilt FTS");
            assert_eq!(fts_count, 1);
            let link_count:i64=conn.query_row("SELECT COUNT(*) FROM curriculum_source_links WHERE tenant_id='tenant-a' AND source_id='source-a'",[],|row|row.get(0)).expect("read restored link");
            assert_eq!(link_count, 1);
            conn.execute_batch("DETACH DATABASE restore")
                .expect("detach restore");
        }
        drop(store);
        fs::remove_dir_all(root).expect("remove test directory");
    }
}
