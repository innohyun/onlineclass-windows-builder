//! Deliberately local CLI. No workers, HTTP routes, credentials, or network jobs.
//! Preview always rehearses in a separate directory; apply rechecks every input.
use crate::{backup, SqliteStore};
use rusqlite::{types::ValueRef, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

const DATABASE: &str = "onlineclass-sensitive.sqlite";
const ARCHIVE_DATABASE: &str = "onlineclass-shared-archive.sqlite";
const PLAN_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Plan {
    version: u32,
    tenant: String,
    target: PathBuf,
    school_manifest: PathBuf,
    school_manifest_sha256: String,
    target_fingerprint: String,
    protection_fingerprint: String,
    rehearsal_fingerprint: String,
    counts: Value,
    school_source_confirmed_current: bool,
}

fn error(code: &str) -> String {
    code.to_string()
}
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn app_stopped() -> Result<(), String> {
    ports_closed(&crate::PORTS)
}

fn ports_closed(ports: &[u16]) -> Result<(), String> {
    for port in ports {
        let address = SocketAddr::from(([127, 0, 0, 1], *port));
        if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() {
            return Err(error("recovery_close_desktop_app_required"));
        }
    }
    Ok(())
}

fn locked_store(root: &Path) -> Result<SqliteStore, String> {
    let database = root.join(DATABASE);
    let conn = Connection::open_with_flags(
        &database,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|_| error("recovery_target_open_failed"))?;
    conn.busy_timeout(Duration::from_millis(100))
        .map_err(|_| error("recovery_target_lock_failed"))?;
    // EXCLUSIVE mode retains SQLite's cross-process lock after COMMIT, including
    // between staging and the restore transaction. A new writer cannot race us.
    conn.execute_batch(
        "PRAGMA foreign_keys=ON; PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE; COMMIT;",
    )
    .map_err(|_| error("recovery_target_in_use"))?;
    Ok(SqliteStore {
        conn: Mutex::new(conn),
        db_path: database,
        data_dir: root.into(),
    })
}

fn db_fingerprint(conn: &Connection) -> Result<String, String> {
    let mut digest = Sha256::new();
    let mut statement=conn.prepare("SELECT name,sql FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").map_err(|_|error("recovery_schema_read_failed"))?;
    let tables = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        })
        .map_err(|_| error("recovery_schema_read_failed"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| error("recovery_schema_read_failed"))?;
    for (name, schema) in tables {
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(schema.as_bytes());
        digest.update([0]);
        let mut query = conn
            .prepare(&format!("SELECT * FROM {}", quote(&name)))
            .map_err(|_| error("recovery_rows_read_failed"))?;
        let columns = query.column_count();
        drop(query);
        let order = (1..=columns)
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(",");
        query = conn
            .prepare(&format!("SELECT * FROM {} ORDER BY {order}", quote(&name)))
            .map_err(|_| error("recovery_rows_read_failed"))?;
        let mut rows = query
            .query([])
            .map_err(|_| error("recovery_rows_read_failed"))?;
        while let Some(row) = rows
            .next()
            .map_err(|_| error("recovery_rows_read_failed"))?
        {
            digest.update([0xff]);
            for column in 0..columns {
                let value = row
                    .get_ref(column)
                    .map_err(|_| error("recovery_rows_read_failed"))?;
                match value {
                    ValueRef::Null => digest.update([0]),
                    ValueRef::Integer(v) => {
                        digest.update([1]);
                        digest.update(v.to_le_bytes());
                    }
                    ValueRef::Real(v) => {
                        digest.update([2]);
                        digest.update(v.to_bits().to_le_bytes());
                    }
                    ValueRef::Text(v) | ValueRef::Blob(v) => {
                        digest.update([if matches!(value, ValueRef::Text(_)) {
                            3
                        } else {
                            4
                        }]);
                        digest.update((v.len() as u64).to_le_bytes());
                        digest.update(v);
                    }
                }
            }
        }
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn files(root: &Path) -> Result<BTreeMap<PathBuf, String>, String> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, String>) -> Result<(), String> {
        for entry in fs::read_dir(at).map_err(|_| error("recovery_files_read_failed"))? {
            let entry = entry.map_err(|_| error("recovery_files_read_failed"))?;
            let kind = entry
                .file_type()
                .map_err(|_| error("recovery_files_read_failed"))?;
            if kind.is_symlink() {
                return Err(error("recovery_symlink_not_allowed"));
            }
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| error("recovery_path_escape"))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".restore-access.lock" || name.ends_with("-shm") || name.ends_with(".log") {
                continue;
            }
            // Main DB sidecars are represented by the consistent SQL view and
            // VACUUM copy. EXCLUSIVE rollback journals can remain until close.
            if relative == Path::new(DATABASE)
                || relative == Path::new(&format!("{DATABASE}-wal"))
                || relative == Path::new(&format!("{DATABASE}-journal"))
                || relative == Path::new(ARCHIVE_DATABASE)
                || relative == Path::new(&format!("{ARCHIVE_DATABASE}-wal"))
                || relative == Path::new(&format!("{ARCHIVE_DATABASE}-journal"))
            {
                continue;
            }
            if kind.is_dir() {
                walk(root, &path, out)?;
            } else if kind.is_file() {
                out.insert(relative.into(), backup::sha256_file(&path)?.1);
            } else {
                return Err(error("recovery_special_file_not_allowed"));
            }
        }
        Ok(())
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result)?;
    Ok(result)
}

fn fingerprint(store: &SqliteStore) -> Result<String, String> {
    let conn = store.conn.lock().map_err(|_| error("db_lock_failed"))?;
    let mut digest = Sha256::new();
    digest.update(db_fingerprint(&conn)?.as_bytes());
    if let Some(archive) = archive_read(&store.data_dir)? {
        digest.update(ARCHIVE_DATABASE.as_bytes());
        digest.update(db_fingerprint(&archive)?.as_bytes());
    }
    for (path, hash) in files(&store.data_dir)? {
        digest.update(path.to_string_lossy().replace('\\', "/").as_bytes());
        digest.update([0]);
        digest.update(hash.as_bytes());
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn archive_read(root: &Path) -> Result<Option<Connection>, String> {
    let path = root.join(ARCHIVE_DATABASE);
    if !path.exists() { return Ok(None); }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| error("recovery_archive_read_failed"))?;
    conn.busy_timeout(Duration::from_millis(100)).map_err(|_|error("recovery_archive_read_failed"))?;
    Ok(Some(conn))
}

fn copy_store(source: &SqliteStore, target: &Path) -> Result<(), String> {
    fs::create_dir(target).map_err(|_| error("recovery_output_already_exists"))?;
    let before = fingerprint(source)?;
    for relative in files(&source.data_dir)?.keys() {
        let output = target.join(relative);
        fs::create_dir_all(output.parent().ok_or("recovery_path_invalid")?)
            .map_err(|_| error("recovery_copy_failed"))?;
        fs::copy(source.data_dir.join(relative), output)
            .map_err(|_| error("recovery_copy_failed"))?;
    }
    let conn = source.conn.lock().map_err(|_| error("db_lock_failed"))?;
    conn.execute(
        "VACUUM INTO ?1",
        [target.join(DATABASE).to_string_lossy().as_ref()],
    )
    .map_err(|_| error("recovery_database_copy_failed"))?;
    drop(conn);
    if let Some(archive) = archive_read(&source.data_dir)? {
        archive.execute("VACUUM INTO ?1", [target.join(ARCHIVE_DATABASE).to_string_lossy().as_ref()])
            .map_err(|_| error("recovery_archive_copy_failed"))?;
        let copy = archive_read(target)?.ok_or("recovery_archive_copy_failed")?;
        let check: String = copy.query_row("PRAGMA quick_check", [], |r|r.get(0)).map_err(|_|error("recovery_archive_copy_failed"))?;
        if check != "ok" { return Err(error("recovery_archive_copy_failed")); }
    }
    let copied = locked_store(target)?;
    let check: String = copied
        .conn
        .lock()
        .map_err(|_| error("db_lock_failed"))?
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .map_err(|_| error("recovery_integrity_failed"))?;
    if check != "ok" || fingerprint(&copied)? != before || fingerprint(source)? != before {
        return Err(error("recovery_copy_verification_failed"));
    }
    Ok(())
}

fn canonical(path: &Path) -> Result<PathBuf, String> {
    fs::canonicalize(path).map_err(|_| error("recovery_path_missing"))
}

fn workspace_path(path: &Path, target: &Path, school: &Path) -> Result<PathBuf, String> {
    let parent = canonical(path.parent().ok_or("recovery_workspace_invalid")?)?;
    let path = parent.join(path.file_name().ok_or("recovery_workspace_invalid")?);
    if path.exists()
        || path.starts_with(target)
        || target.starts_with(&path)
        || school.starts_with(&path)
        || path.starts_with(school.parent().ok_or("recovery_workspace_invalid")?)
    {
        return Err(error("recovery_workspace_must_be_separate_new_directory"));
    }
    if path.components().any(|part| {
        part.as_os_str()
            .to_string_lossy()
            .to_lowercase()
            .starts_with("onedrive")
    }) {
        return Err(error("recovery_workspace_outside_onedrive_required"));
    }
    for key in ["OneDrive", "OneDriveCommercial", "OneDriveConsumer"] {
        if std::env::var_os(key)
            .and_then(|v| fs::canonicalize(v).ok())
            .is_some_and(|root| path.starts_with(root))
        {
            return Err(error("recovery_workspace_outside_onedrive_required"));
        }
    }
    Ok(path)
}

fn source_hash(path: &Path) -> Result<String, String> {
    backup::sha256_file(path).map(|v| v.1)
}

fn frozen_manifest(workspace: &Path, tenant: &str, original: &Path) -> Result<PathBuf, String> {
    let snapshot = original
        .parent()
        .and_then(Path::file_name)
        .ok_or("recovery_source_path_invalid")?;
    Ok(workspace
        .join("source")
        .join("tenants")
        .join(tenant)
        .join("snapshots")
        .join(snapshot)
        .join("manifest.json"))
}

fn freeze_source(workspace: &Path, tenant: &str, original: &Path) -> Result<PathBuf, String> {
    let bytes = fs::read(original).map_err(|_| error("recovery_source_read_failed"))?;
    let manifest: Value =
        serde_json::from_slice(&bytes).map_err(|_| error("recovery_source_invalid"))?;
    if !matches!(manifest["version"].as_i64(), Some(4 | 5)) {
        return Err(error("recovery_sealed_snapshot_required"));
    }
    backup::authoritative_restore_manifest(original, &manifest, tenant)?;
    let destination = frozen_manifest(workspace, tenant, original)?;
    fs::create_dir_all(destination.parent().ok_or("recovery_source_path_invalid")?)
        .map_err(|_| error("recovery_copy_failed"))?;
    for artifact in manifest["artifacts"]
        .as_array()
        .ok_or("backup_artifacts_required")?
    {
        let relative = backup::safe_relative_path(
            artifact["relativePath"]
                .as_str()
                .ok_or("backup_artifact_path_required")?,
        )
        .ok_or("backup_artifact_path_invalid")?;
        let version = manifest["version"].as_i64().unwrap_or(0);
        let from = crate::backup_v5::artifact_path(original, version, &relative)?;
        let to = crate::backup_v5::artifact_path(&destination, version, &relative)?;
        fs::create_dir_all(to.parent().ok_or("recovery_source_path_invalid")?)
            .map_err(|_| error("recovery_copy_failed"))?;
        fs::copy(from, to).map_err(|_| error("recovery_copy_failed"))?;
    }
    fs::write(&destination, &bytes).map_err(|_| error("recovery_copy_failed"))?;
    fs::copy(
        original.with_file_name("commit.json"),
        destination.with_file_name("commit.json"),
    )
    .map_err(|_| error("recovery_copy_failed"))?;
    let index = crate::backup_v4::projection(original, &manifest)?;
    for reference in index["archives"]["records"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let relative = backup::safe_relative_path(
            reference["bundleRelativePath"]
                .as_str()
                .ok_or("recovery_bundle_path_invalid")?,
        )
        .ok_or("recovery_bundle_path_invalid")?;
        let from = crate::backup_v4::tenant_dir(original)?.join(&relative);
        let to = crate::backup_v4::tenant_dir(&destination)?.join(&relative);
        let document =
            crate::shared_archive_sync::verify_bundle_reference_at(&from, tenant, reference)?;
        let mut names = vec![PathBuf::from("archive.json"), PathBuf::from("commit.json")];
        for file in document["files"]
            .as_array()
            .ok_or("recovery_bundle_invalid")?
        {
            names.push(
                backup::safe_relative_path(
                    file["bundleRelativePath"]
                        .as_str()
                        .ok_or("recovery_bundle_path_invalid")?,
                )
                .ok_or("recovery_bundle_path_invalid")?,
            );
        }
        for name in names {
            let output = to.join(&name);
            fs::create_dir_all(output.parent().ok_or("recovery_bundle_path_invalid")?)
                .map_err(|_| error("recovery_copy_failed"))?;
            fs::copy(from.join(name), output).map_err(|_| error("recovery_copy_failed"))?;
        }
    }
    // Hash the destination, not merely the source checked before copying.
    backup::authoritative_restore_manifest(&destination, &manifest, tenant)?;
    Ok(destination)
}

fn body(tenant: &str, manifest: &Path) -> Value {
    json!({"tenantId":tenant,"manifestPath":manifest})
}

fn write_new(path: &Path, value: &Value) -> Result<(), String> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|_| error("recovery_receipt_already_exists"))?;
    file.write_all(
        serde_json::to_vec_pretty(value)
            .map_err(|_| error("recovery_plan_encode_failed"))?
            .as_slice(),
    )
    .map_err(|_| error("recovery_plan_write_failed"))?;
    file.sync_all()
        .map_err(|_| error("recovery_plan_write_failed"))
}

fn preview(
    tenant: &str,
    target: &Path,
    school: &Path,
    workspace: &Path,
    current: bool,
) -> Result<Value, String> {
    let target = canonical(target)?;
    let school = canonical(school)?;
    let workspace = workspace_path(workspace, &target, &school)?;
    let source = locked_store(&target)?;
    source.restore_ready(tenant)?;
    let target_hash = fingerprint(&source)?;
    let manifest_hash = source_hash(&school)?;
    fs::create_dir(&workspace).map_err(|_| error("recovery_workspace_create_failed"))?;
    copy_store(&source, &workspace.join("protection"))?;
    copy_store(&source, &workspace.join("rehearsal"))?;
    let frozen = freeze_source(&workspace, tenant, &school)?;
    let rehearsal = locked_store(&workspace.join("rehearsal"))?;
    let counts = backup::recovery_preflight(&rehearsal, body(tenant, &frozen));
    let mut counts = match counts {
        Ok(value) => value,
        Err(reason) => {
            write_new(
                &workspace.join("blocked.json"),
                &json!({"ok":false,"reason":reason,"targetFingerprint":target_hash,"schoolManifestSha256":manifest_hash}),
            )?;
            return Err(reason);
        }
    };
    let protection = json!({"ok":true,"kind":"verified_offline_copy","fingerprint":target_hash});
    let mut rehearsal_body = body(tenant, &frozen);
    rehearsal_body["recoveryArchiveLocatorRoot"] = json!(target);
    let restored = backup::recovery_restore(&rehearsal, rehearsal_body, protection)?;
    counts["archives"] = restored["archives"].clone();
    counts["teachingSourceFilesRestored"] = restored["teachingSourcesRestored"].clone();
    counts["priorStorePreserved"] = json!(true);
    if fingerprint(&source)? != target_hash || source_hash(&school)? != manifest_hash {
        return Err(error("recovery_input_changed"));
    }
    let plan = Plan {
        version: PLAN_VERSION,
        tenant: tenant.into(),
        target,
        school_manifest: school,
        school_manifest_sha256: manifest_hash,
        target_fingerprint: target_hash.clone(),
        protection_fingerprint: target_hash,
        rehearsal_fingerprint: fingerprint(&rehearsal)?,
        counts: counts.clone(),
        school_source_confirmed_current: current,
    };
    write_new(
        &workspace.join("plan.json"),
        &serde_json::to_value(plan).map_err(|_| error("recovery_plan_encode_failed"))?,
    )?;
    Ok(json!({"ok":true,"phase":"preview","applyAllowed":current,"counts":counts}))
}

fn apply(plan_path: &Path) -> Result<Value, String> {
    let plan_path = canonical(plan_path)?;
    let workspace = plan_path.parent().ok_or("recovery_plan_invalid")?;
    let plan: Plan = serde_json::from_slice(
        &fs::read(&plan_path).map_err(|_| error("recovery_plan_read_failed"))?,
    )
    .map_err(|_| error("recovery_plan_invalid"))?;
    if plan.version != PLAN_VERSION || !plan.school_source_confirmed_current {
        return Err(error("recovery_current_school_snapshot_required"));
    }
    let store = locked_store(&canonical(&plan.target)?)?;
    // A completed receipt is an idempotent read, never a second restore.
    let receipt_path = workspace.join("applied.json");
    if receipt_path.exists() {
        let receipt: Value = serde_json::from_slice(
            &fs::read(receipt_path).map_err(|_| error("recovery_receipt_invalid"))?,
        )
        .map_err(|_| error("recovery_receipt_invalid"))?;
        if receipt["afterFingerprint"].as_str() == Some(fingerprint(&store)?.as_str()) {
            return Ok(json!({"ok":true,"phase":"already_applied"}));
        }
        return Err(error("recovery_target_changed_after_apply"));
    }
    let protection = locked_store(&workspace.join("protection"))?;
    if fingerprint(&protection)? != plan.protection_fingerprint
        || fingerprint(&store)? != plan.target_fingerprint
        || source_hash(&plan.school_manifest)? != plan.school_manifest_sha256
    {
        return Err(error("recovery_input_changed"));
    }
    let rehearsal = locked_store(&workspace.join("rehearsal"))?;
    if fingerprint(&rehearsal)? != plan.rehearsal_fingerprint {
        return Err(error("recovery_rehearsal_changed"));
    }
    store.restore_ready(&plan.tenant)?;
    backup::recovery_preflight(&store, body(&plan.tenant, &plan.school_manifest))?;
    let frozen = frozen_manifest(workspace, &plan.tenant, &plan.school_manifest)?;
    if source_hash(&frozen)? != plan.school_manifest_sha256 {
        return Err(error("recovery_source_changed"));
    }
    backup::recovery_preflight(&store, body(&plan.tenant, &frozen))?;
    // Resume only if every input is still exactly the previewed value and the
    // prior attempt did not leave a restore journal. Committed changes require
    // the completed receipt or a new preview; never blindly replay a merge.
    let started = workspace.join("apply-started.json");
    let expected = json!({"version":1,"planSha256":source_hash(&plan_path)?,"targetFingerprint":plan.target_fingerprint});
    if started.exists() {
        let prior: Value = serde_json::from_slice(
            &fs::read(&started).map_err(|_| error("recovery_receipt_invalid"))?,
        )
        .map_err(|_| error("recovery_receipt_invalid"))?;
        if prior != expected {
            return Err(error("recovery_receipt_invalid"));
        }
    } else {
        write_new(&started, &expected)?;
    }
    backup::recovery_restore(
        &store,
        body(&plan.tenant, &frozen),
        json!({"ok":true,"kind":"verified_offline_copy","fingerprint":plan.protection_fingerprint}),
    )?;
    let result = json!({"ok":true,"phase":"applied","afterFingerprint":fingerprint(&store)?,"counts":plan.counts});
    write_new(&receipt_path, &result)?;
    Ok(json!({"ok":true,"phase":"applied","counts":plan.counts}))
}

/// `preview --tenant ID --target-dir DIR --school-manifest FILE --workspace NEW_DIR
/// [--school-current]` or `apply --plan FILE`. Old snapshots omit --school-current
/// and can only produce rehearsal plans. Paths/content are never printed.
pub fn run(arguments: Vec<String>) -> Result<Value, String> {
    let command = arguments.first().ok_or("recovery_command_required")?;
    let mut values = BTreeMap::new();
    let mut current = false;
    let mut index = 1;
    while index < arguments.len() {
        let key = &arguments[index];
        if key == "--school-current" {
            if current {
                return Err(error("recovery_duplicate_argument"));
            }
            current = true;
            index += 1;
            continue;
        }
        if !matches!(
            key.as_str(),
            "--tenant" | "--target-dir" | "--school-manifest" | "--workspace" | "--plan"
        ) {
            return Err(error("recovery_argument_invalid"));
        }
        let value = arguments
            .get(index + 1)
            .filter(|value| !value.starts_with("--"))
            .ok_or("recovery_argument_value_required")?;
        if values.insert(key.as_str(), value.as_str()).is_some() {
            return Err(error("recovery_duplicate_argument"));
        }
        index += 2;
    }
    let required = |key: &str| {
        values
            .get(key)
            .copied()
            .ok_or_else(|| error("recovery_argument_required"))
    };
    match command.as_str() {
        "preview" if values.len() == 4 => {
            let tenant = required("--tenant")?;
            if crate::normalize_tenant_id(Some(&json!(tenant))) != tenant || tenant.is_empty() {
                return Err(error("tenant_id_required"));
            }
            let target = Path::new(required("--target-dir")?);
            if fs::canonicalize(crate::default_data_dir())
                .ok()
                .is_some_and(|live| canonical(target).ok().as_ref() == Some(&live))
            {
                app_stopped()?;
            }
            preview(
                tenant,
                target,
                Path::new(required("--school-manifest")?),
                Path::new(required("--workspace")?),
                current,
            )
        }
        "apply" if values.len() == 1 && !current => {
            app_stopped()?;
            apply(Path::new(required("--plan")?))
        }
        _ => Err(error("recovery_command_invalid")),
    }
}

#[cfg(test)]
#[path = "recovery_tool_tests.rs"]
mod tests;
