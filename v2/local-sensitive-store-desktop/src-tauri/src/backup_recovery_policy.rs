//! Explicit offline recovery policy; never selected by HTTP or background sync.
use super::*;

pub(super) fn verified_preview(body: &Value) -> Result<Value, String> {
    let tenant = normalize_tenant_id(body.get("tenantId"));
    if tenant.is_empty() {
        return Err("tenant_id_required".into());
    }
    let path = PathBuf::from(
        body.get("manifestPath")
            .and_then(Value::as_str)
            .ok_or("backup_manifest_required")?,
    );
    let manifest = read_manifest(&path)?;
    // This new recovery flow requires a sealed index. Ordinary v2/v3 restore
    // compatibility remains unchanged.
    if !matches!(manifest["version"].as_i64(), Some(4 | 5)) {
        return Err("recovery_sealed_snapshot_required".into());
    }
    authoritative_restore_manifest(&path, &manifest, &tenant)?;
    Ok(json!({"tenantId":tenant,"manifestPath":path,"backupId":manifest["backupId"]}))
}

fn database_path(body: &Value) -> Result<(Value, PathBuf), String> {
    let preview = verified_preview(body)?;
    let path = PathBuf::from(
        preview["manifestPath"]
            .as_str()
            .ok_or("backup_manifest_required")?,
    );
    let manifest = read_manifest(&path)?;
    let authoritative = authoritative_restore_manifest(
        &path,
        &manifest,
        preview["tenantId"].as_str().unwrap_or_default(),
    )?;
    let relative = authoritative["db"]["relativePath"]
        .as_str()
        .ok_or("backup_db_required")?;
    let relative = safe_relative_path(relative).ok_or("backup_db_path_invalid")?;
    let db = crate::backup_v5::artifact_path(
        &path,
        authoritative["version"].as_i64().unwrap_or(0),
        &relative,
    )?;
    Ok((authoritative, db))
}

pub(crate) fn recovery_preflight(store: &SqliteStore, body: Value) -> Result<Value, String> {
    let (authoritative, database) = database_path(&body)?;
    let tenant = normalize_tenant_id(body.get("tenantId"));
    let mut uri = url::Url::from_file_path(database).map_err(|_| "recovery_source_path_invalid")?;
    uri.set_query(Some("mode=ro&immutable=1"));
    let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    conn.execute("ATTACH DATABASE ?1 AS restore", [uri.as_str()])
        .map_err(|_| "recovery_source_attach_failed")?;
    let result = (|| {
        crate::observation_evidence::check_restore(&conn, &tenant)?;
        check_attached(&conn, &tenant, &authoritative)?;
        let mut counts = serde_json::Map::new();
        for table in BACKUP_TABLES {
            if !table_exists(&conn, table.name)?
                || !attached_table_exists(&conn, "restore", table.name)?
            {
                continue;
            }
            let (added, changed) = table_counts(&conn, &tenant, table)?;
            counts.insert(
                table.name.into(),
                json!({"add":added,"schoolPreferred":changed}),
            );
        }
        Ok(json!({"ok":true,"counts":counts,"deletions":0}))
    })();
    conn.execute_batch("DETACH DATABASE restore")
        .map_err(|_| "recovery_source_detach_failed")?;
    result
}

pub(crate) fn recovery_restore(
    store: &SqliteStore,
    body: Value,
    protection: Value,
) -> Result<Value, String> {
    recovery_preflight(store, body.clone())?;
    restore_with_policy(store, body, |_store, _tenant| Ok(protection), true)
}

fn join(table: &BackupTable) -> String {
    table
        .key_columns
        .iter()
        .map(|key| format!("m.{key}=r.{key}"))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn difference(table: &BackupTable) -> String {
    table
        .columns
        .iter()
        .map(|column| format!("m.{column} IS NOT r.{column}"))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn table_counts(
    conn: &Connection,
    tenant: &str,
    table: &BackupTable,
) -> Result<(i64, i64), String> {
    let added = conn.query_row(&format!("SELECT COUNT(*) FROM restore.{} r WHERE r.tenant_id=?1 AND NOT EXISTS(SELECT 1 FROM main.{} m WHERE {})",table.name,table.name,join(table)),[tenant],|row|row.get(0)).map_err(|_|"recovery_compare_failed")?;
    let changed = if table.name.starts_with("observation_evidence_") {
        0
    } else {
        conn.query_row(&format!("SELECT COUNT(*) FROM main.{} m JOIN restore.{} r ON {} WHERE m.tenant_id=?1 AND ({})",table.name,table.name,join(table),difference(table)),[tenant],|row|row.get(0)).map_err(|_|"recovery_compare_failed")?
    };
    Ok((added, changed))
}

fn observation_content(raw: &str) -> Result<Value, String> {
    let mut value: Value = serde_json::from_str(raw).map_err(|_| "recovery_observation_invalid")?;
    let object = value
        .as_object_mut()
        .ok_or("recovery_observation_invalid")?;
    // Imported legacy baselines may have different audit times and provenance
    // for the same authored observation. Retain both immutable histories and the
    // current head; these fields are excluded only for this no-op comparison.
    for key in [
        "evidenceVersion",
        "revisionId",
        "revisionHash",
        "createdAtMs",
        "updatedAtMs",
    ] {
        object.remove(key);
    }
    Ok(value)
}

pub(super) fn check_attached(
    conn: &Connection,
    tenant: &str,
    authoritative: &Value,
) -> Result<(), String> {
    // Conflicting immutable history must never be treated as mutable school data.
    crate::observation_evidence::check_restore(conn, tenant)?;
    let mut statement = conn.prepare("SELECT m.payload_json,r.payload_json,m.date_key=r.date_key AND m.period=r.period AND m.student_code=r.student_code FROM main.lesson_observations m JOIN restore.lesson_observations r ON m.tenant_id=r.tenant_id AND m.doc_id=r.doc_id WHERE m.tenant_id=?1").map_err(|_|"recovery_observation_compare_failed")?;
    let rows = statement
        .query_map([tenant], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })
        .map_err(|_| "recovery_observation_compare_failed")?;
    for row in rows {
        let (local, school, same_columns) =
            row.map_err(|_| "recovery_observation_compare_failed")?;
        if !same_columns || observation_content(&local)? != observation_content(&school)? {
            let local: Value =
                serde_json::from_str(&local).map_err(|_| "recovery_observation_invalid")?;
            let school: Value =
                serde_json::from_str(&school).map_err(|_| "recovery_observation_invalid")?;
            if local["revisionId"].as_str().is_none() || school["revisionId"].as_str().is_none() {
                return Err("recovery_observation_canonical_resolution_required".into());
            }
        }
    }
    let deleted_locally:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM restore.lesson_observations r JOIN main.observation_evidence_deletions d ON d.tenant_id=r.tenant_id AND d.doc_id=r.doc_id WHERE r.tenant_id=?1)",[tenant],|r|r.get(0)).map_err(|_|"recovery_tombstone_compare_failed")?;
    if deleted_locally {
        return Err("recovery_tombstone_canonical_resolution_required".into());
    }
    if attached_table_exists(conn, "restore", "observation_evidence_deletions")? {
        let deleted_at_school:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM main.lesson_observations m JOIN restore.observation_evidence_deletions d ON d.tenant_id=m.tenant_id AND d.doc_id=m.doc_id WHERE m.tenant_id=?1)",[tenant],|r|r.get(0)).map_err(|_|"recovery_tombstone_compare_failed")?;
        if deleted_at_school {
            return Err("recovery_tombstone_canonical_resolution_required".into());
        }
    }
    for table in BACKUP_TABLES.iter().filter(|table| {
        table.name == "lesson_plan_bindings" || table.name.starts_with("observation_evidence_")
    }) {
        if !attached_table_exists(conn, "restore", table.name)? {
            continue;
        }
        let differs:bool = conn.query_row(&format!("SELECT EXISTS(SELECT 1 FROM main.{} m JOIN restore.{} r ON {} WHERE m.tenant_id=?1 AND ({}))",table.name,table.name,join(table),difference(table)),[tenant],|row|row.get(0)).map_err(|_|"recovery_compare_failed")?;
        // Independent reconciliation baselines are retained, not overwritten.
        if differs && table.name != "observation_evidence_reconciliation" {
            return Err(if table.name == "lesson_plan_bindings" {
                "recovery_binding_canonical_resolution_required"
            } else {
                "observation_evidence_restore_conflict"
            }
            .into());
        }
    }
    // Deletion absence is never authority. Proven tombstones are surfaced for
    // a canonical domain resolution instead of silently resurrecting/deleting.
    for record in authoritative["sync"]["records"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if record["tombstone"].as_bool() != Some(true) {
            continue;
        }
        let name = record["table"]
            .as_str()
            .ok_or("backup_sync_table_invalid")?;
        let table = BACKUP_TABLES
            .iter()
            .find(|table| table.name == name)
            .ok_or("backup_sync_table_invalid")?;
        let keys = record["recordKey"]
            .as_array()
            .ok_or("backup_sync_record_key_invalid")?;
        if keys.len() != table.key_columns.len() - 1 {
            return Err("backup_sync_record_key_invalid".into());
        }
        let mut values = vec![SqlValue::Text(tenant.into())];
        values.extend(
            keys.iter()
                .map(json_key_value)
                .collect::<Result<Vec<_>, _>>()?,
        );
        let exists: bool = conn
            .query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM main.{} WHERE tenant_id=?1 AND {})",
                    table.name,
                    record_where(table)
                ),
                params_from_iter(values),
                |row| row.get(0),
            )
            .map_err(|_| "recovery_tombstone_compare_failed")?;
        if exists {
            return Err("recovery_tombstone_canonical_resolution_required".into());
        }
    }
    crate::teaching_source_backup::recovery_preflight(conn, tenant)?;
    Ok(())
}

pub(super) fn apply_archives(store: &SqliteStore, body: &Value, manifest: &Path, authoritative: &Value, tenant: &str) -> Result<Value, String> {
    let mut archive = crate::shared_archive::open_db_at(&store.data_dir)?;
    let locator = body.get("recoveryArchiveLocatorRoot").and_then(Value::as_str)
        .map(PathBuf::from).unwrap_or_else(|| store.data_dir.clone());
    crate::shared_archive_apply::apply_snapshot_bundles_at(
        &mut archive, &store.data_dir.join("shared-archive-files"),
        &locator.join("shared-archive-files"), tenant,
        &crate::backup_v4::tenant_dir(manifest)?, &authoritative["archives"])
}

pub(super) fn merge_guard(table: &BackupTable) -> String {
    if table.name == "lesson_observations" {
        crate::observation_evidence::observation_merge_guard()
    } else if table.name.starts_with("observation_evidence_")
        || table.name == "lesson_plan_bindings"
    {
        "0".into()
    } else {
        "1".into()
    }
}

pub(super) fn archive_losers(
    transaction: &rusqlite::Transaction<'_>,
    tenant: &str,
    table: &BackupTable,
) -> Result<(), String> {
    if merge_guard(table) == "0" || table.name == "lesson_observations" {
        return Ok(());
    }
    let key = table
        .key_columns
        .iter()
        .filter(|key| **key != "tenant_id")
        .map(|key| format!("m.{key}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql=format!("SELECT json_array({key}),json_object({}) FROM main.{} m JOIN restore.{} r ON {} WHERE m.tenant_id=?1 AND ({})",row_json_expression(table,"m"),table.name,table.name,join(table),difference(table));
    let mut statement = transaction
        .prepare(&sql)
        .map_err(|_| "recovery_conflict_read_failed")?;
    let rows = statement
        .query_map([tenant], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|_| "recovery_conflict_read_failed")?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "recovery_conflict_read_failed")?;
    for (key, payload) in rows {
        let record = SyncRecord {
            table_name: table.name.into(),
            record_key: key,
            key_values: Vec::new(),
            changed_generation: 0,
            record_version: 0,
            tombstone: false,
        };
        // Recovery is a local edit, not a fabricated D1 generation.
        archive_conflict(transaction, tenant, &record, 0, 0, &payload)?;
    }
    Ok(())
}

pub(super) fn resolve_observations(
    store: &SqliteStore,
    tx: &Connection,
    tenant: &str,
) -> Result<(), String> {
    let mut statement=tx.prepare("SELECT r.payload_json FROM restore.lesson_observations r JOIN main.lesson_observations m ON m.tenant_id=r.tenant_id AND m.doc_id=r.doc_id WHERE r.tenant_id=?1 AND json_extract(r.payload_json,'$.revisionId') IS NOT json_extract(m.payload_json,'$.revisionId')").map_err(|_|"recovery_observation_compare_failed")?;
    let records = statement
        .query_map([tenant], |r| r.get::<_, String>(0))
        .map_err(|_| "recovery_observation_compare_failed")?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "recovery_observation_compare_failed")?;
    drop(statement);
    for raw in records {
        let record: Value =
            serde_json::from_str(&raw).map_err(|_| "recovery_observation_invalid")?;
        store.evidence_recovery_select(tx, tenant, &record)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_observation_metadata_is_not_a_content_conflict() {
        assert_eq!(
            observation_content(
                r#"{"note":"same","revisionId":"a","revisionHash":"h1","evidenceVersion":1}"#
            )
            .unwrap(),
            observation_content(
                r#"{"note":"same","revisionId":"b","revisionHash":"h2","evidenceVersion":1}"#
            )
            .unwrap()
        );
        assert_ne!(
            observation_content(r#"{"note":"local"}"#).unwrap(),
            observation_content(r#"{"note":"school"}"#).unwrap()
        );
    }
    #[test]
    fn recovery_preference_keeps_domain_guards() {
        for table in BACKUP_TABLES {
            if table.name.starts_with("observation_evidence_")
                || table.name == "lesson_plan_bindings"
            {
                assert_eq!(merge_guard(table), "0");
            }
        }
    }
}
