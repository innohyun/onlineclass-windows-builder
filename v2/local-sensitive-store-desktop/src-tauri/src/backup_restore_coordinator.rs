//! Device-local two-component restore. Each SQLite/media journal commits alone;
//! this durable receipt keeps common and class access blocked until both agree.
use crate::{backup, restore_journal, SqliteStore};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Component {
    Combined,
    Common,
    Class,
}
thread_local! { static ACTIVE: RefCell<(String, Component)> = const { RefCell::new((String::new(), Component::Combined)) }; }
struct Scope((String, Component));
impl Drop for Scope {
    fn drop(&mut self) {
        ACTIVE.with(|active| *active.borrow_mut() = self.0.clone());
    }
}
fn scope(tenant: &str, component: Component) -> Scope {
    Scope(ACTIVE.with(|active| active.replace((tenant.into(), component))))
}
pub(crate) fn component() -> Component {
    ACTIVE.with(|active| active.borrow().1)
}
pub(crate) fn running() -> bool {
    ACTIVE.with(|active| !active.borrow().0.is_empty())
}
pub(crate) fn bypass(tenant: &str) -> bool {
    ACTIVE.with(|active| active.borrow().0 == tenant)
}
pub(crate) fn include_class() -> bool {
    component() != Component::Common
}
pub(crate) fn include_common() -> bool {
    component() != Component::Class
}
pub(crate) fn include_table(name: &str) -> bool {
    if name == "local_import_runs" {
        // This legacy table contains both personal source processing receipts
        // and class imports; its rows are filtered by their ownership below.
        true
    } else if name.starts_with("password_vault_") {
        include_common()
    } else {
        include_class()
    }
}

// Live source receipts retain their existing kind. The unchanged v5 tombstone
// key has no row, so canonical teaching-source run IDs supply its provenance.
pub(crate) fn import_run_common(kind: Option<&str>, run_id: &str) -> bool {
    kind.map_or_else(
        || run_id.starts_with("teaching-source:"),
        |kind| kind == "teaching_source",
    )
}
pub(crate) fn include_import_run(kind: Option<&str>, run_id: &str) -> bool {
    match component() {
        Component::Combined => true,
        Component::Common => import_run_common(kind, run_id),
        Component::Class => !import_run_common(kind, run_id),
    }
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Prepared,
    CommonApplied,
    ClassApplied,
    Committed,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Journal {
    version: i64,
    operation_id: String,
    tenant_id: String,
    mode: String,
    body: Value,
    manifest_sha256: String,
    common_protection: Value,
    class_protection: Value,
    common_protection_sha256: String,
    class_protection_sha256: String,
    phase: Phase,
    common_result: Option<Value>,
    class_result: Option<Value>,
}
const DIRECTORY: &str = ".class-restore-operations";
fn blocked() -> String {
    "restore_recovery_required".into()
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
}
fn directory(root: &Path) -> Result<PathBuf, String> {
    let root = fs::canonicalize(root).map_err(|_| blocked())?;
    let path = root.join(DIRECTORY);
    if let Ok(meta) = fs::symlink_metadata(&path) {
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(blocked());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err(blocked());
            }
        }
    }
    Ok(path)
}
fn path(root: &Path, tenant: &str) -> Result<PathBuf, String> {
    if !valid_id(tenant) {
        return Err(blocked());
    }
    Ok(directory(root)?.join(format!("{:x}.json", Sha256::digest(tenant.as_bytes()))))
}
fn manifest_path(body: &Value) -> Result<PathBuf, String> {
    let path = body["manifestPath"].as_str().ok_or_else(blocked)?;
    if path.is_empty() {
        return Err(blocked());
    }
    Ok(PathBuf::from(path))
}
fn exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(blocked()),
    }
}
fn hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn digest_manifest(value: &Value) -> Result<String, String> {
    restore_journal::digest(&manifest_path(value)?)
}
fn read(path: &Path) -> Result<Journal, String> {
    let meta = fs::symlink_metadata(path).map_err(|_| blocked())?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(blocked());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err(blocked());
        }
    }
    let journal: Journal =
        serde_json::from_slice(&fs::read(path).map_err(|_| blocked())?).map_err(|_| blocked())?;
    if journal.version != 1
        || !valid_id(&journal.tenant_id)
        || !valid_id(&journal.operation_id)
        || journal.body["tenantId"] != journal.tenant_id
        || !matches!(journal.mode.as_str(), "manual" | "generation" | "recovery")
        || !hash(&journal.manifest_sha256)
        || !hash(&journal.common_protection_sha256)
        || !hash(&journal.class_protection_sha256)
        || journal.common_protection["ok"] != true
        || journal.class_protection["ok"] != true
        || (journal.phase != Phase::Prepared
            && journal
                .common_result
                .as_ref()
                .is_none_or(|result| result["ok"] != true))
        || (matches!(journal.phase, Phase::ClassApplied | Phase::Committed)
            && journal
                .class_result
                .as_ref()
                .is_none_or(|result| result["ok"] != true))
        || path.file_name()
            != Some(std::ffi::OsStr::new(&format!(
                "{:x}.json",
                Sha256::digest(journal.tenant_id.as_bytes())
            )))
    {
        return Err(blocked());
    }
    Ok(journal)
}
fn write(path: &Path, journal: &Journal) -> Result<(), String> {
    let parent = path.parent().ok_or_else(blocked)?;
    fs::create_dir_all(parent).map_err(|_| blocked())?;
    let temporary = path.with_extension("json.tmp");
    if fs::symlink_metadata(&temporary).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(blocked());
    }
    let bytes = serde_json::to_vec(journal).map_err(|_| blocked())?;
    let mut file = fs::File::create(&temporary).map_err(|_| blocked())?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| blocked())?;
    drop(file);
    fs::rename(&temporary, path).map_err(|_| blocked())?;
    #[cfg(unix)]
    fs::File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(|_| blocked())?;
    Ok(())
}

pub(crate) fn class_ready(root: &Path, tenant: &str) -> Result<(), String> {
    if bypass(tenant) {
        return Ok(());
    }
    let path = path(root, tenant)?;
    if exists(&path)? && read(&path)?.phase != Phase::Committed {
        return Err(blocked());
    }
    Ok(())
}
pub(crate) fn common_ready(root: &Path) -> Result<(), String> {
    if running() {
        return Ok(());
    }
    let directory = directory(root)?;
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory).map_err(|_| blocked())? {
        let entry = entry.map_err(|_| blocked())?;
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "json")
            && read(&entry.path())?.phase != Phase::Committed
        {
            return Err(blocked());
        }
    }
    Ok(())
}
// Cleanup never bypasses a pending receipt, including on the restore thread.
pub(crate) fn maintenance_ready(root: &Path, tenant: &str) -> Result<(), String> {
    let path = path(root, tenant)?;
    if exists(&path)? && read(&path)?.phase != Phase::Committed {
        return Err(blocked());
    }
    Ok(())
}
pub(crate) fn protected_manifest_paths(root: &Path, tenant: &str) -> Result<Vec<PathBuf>, String> {
    let path = path(root, tenant)?;
    if !exists(&path)? {
        return Ok(Vec::new());
    }
    let journal = read(&path)?;
    if journal.phase == Phase::Committed {
        return Ok(Vec::new());
    }
    Ok(vec![
        manifest_path(&journal.common_protection)?,
        manifest_path(&journal.class_protection)?,
    ])
}

fn protection_ok(protection: &Value) -> Result<(), String> {
    if protection["ok"] != true
        || ["media", "workNoteAttachments", "teachingSources"]
            .iter()
            .any(|key| {
                ["missing", "failed"]
                    .iter()
                    .any(|field| protection[*key][*field].as_i64().unwrap_or(0) > 0)
            })
    {
        return Err("pre_restore_backup_failed:safety_backup_incomplete".into());
    }
    digest_manifest(protection)?;
    Ok(())
}
fn block(store: &SqliteStore, journal: &Journal) -> Result<(), String> {
    let conn = store.conn.lock().map_err(|_| blocked())?;
    let current: Option<String> = {
        use rusqlite::OptionalExtension;
        conn.query_row(
            "SELECT operation_id FROM local_store_component_restore_blocks WHERE tenant_id=?1",
            params![journal.tenant_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| blocked())?
    };
    if current.is_some_and(|id| id != journal.operation_id) {
        return Err(blocked());
    }
    conn.execute("INSERT INTO local_store_component_restore_blocks(tenant_id,operation_id) VALUES(?1,?2) ON CONFLICT(tenant_id) DO NOTHING", params![journal.tenant_id, journal.operation_id]).map_err(|_| blocked())?;
    Ok(())
}
fn unblock(store: &SqliteStore, journal: &Journal) -> Result<(), String> {
    let conn = store.conn.lock().map_err(|_| blocked())?;
    restore_journal::ready(&conn, &journal.tenant_id)?;
    conn.execute(
        "DELETE FROM local_store_component_restore_blocks WHERE tenant_id=?1 AND operation_id=?2",
        params![journal.tenant_id, journal.operation_id],
    )
    .map_err(|_| blocked())?;
    Ok(())
}
fn resume<F>(
    common: &SqliteStore,
    class: &SqliteStore,
    path: &Path,
    journal: &mut Journal,
    apply: &mut F,
) -> Result<Value, String>
where
    F: FnMut(Component, &SqliteStore, Value, Value) -> Result<Value, String>,
{
    let _scope = scope(&journal.tenant_id, Component::Combined);
    if digest_manifest(&journal.body)? != journal.manifest_sha256
        || digest_manifest(&journal.common_protection)? != journal.common_protection_sha256
        || digest_manifest(&journal.class_protection)? != journal.class_protection_sha256
    {
        return Err(blocked());
    }
    if journal.phase == Phase::Committed {
        return Ok(journal
            .class_result
            .clone()
            .unwrap_or_else(|| json!({"ok":true})));
    }
    block(common, journal)?;
    block(class, journal)?;
    if journal.phase == Phase::Prepared {
        {
            let _component = scope(&journal.tenant_id, Component::Common);
            journal.common_result = Some(apply(
                Component::Common,
                common,
                journal.common_protection.clone(),
                journal.body.clone(),
            )?);
        }
        if journal
            .common_result
            .as_ref()
            .is_none_or(|result| result["ok"] != true)
        {
            return Err(blocked());
        }
        journal.phase = Phase::CommonApplied;
        write(path, journal)?;
        restore_journal::failpoint("coordinator-common-applied");
    }
    if journal.phase == Phase::CommonApplied {
        {
            let _component = scope(&journal.tenant_id, Component::Class);
            journal.class_result = Some(apply(
                Component::Class,
                class,
                journal.class_protection.clone(),
                journal.body.clone(),
            )?);
        }
        if journal
            .class_result
            .as_ref()
            .is_none_or(|result| result["ok"] != true)
        {
            return Err(blocked());
        }
        journal.phase = Phase::ClassApplied;
        write(path, journal)?;
        restore_journal::failpoint("coordinator-class-applied");
    }
    unblock(common, journal)?;
    unblock(class, journal)?;
    journal.phase = Phase::Committed;
    write(path, journal)?;
    let mut result = journal
        .class_result
        .clone()
        .unwrap_or_else(|| json!({"ok":true}));
    result["commonComponent"] = journal.common_result.clone().unwrap_or(Value::Null);
    result["restoreComponentsCommitted"] = json!(true);
    Ok(result)
}

pub(crate) fn run<F>(
    store: &SqliteStore,
    body: Value,
    mode: &str,
    apply: F,
) -> Result<Value, String>
where
    F: FnMut(Component, &SqliteStore, Value, Value) -> Result<Value, String>,
{
    let tenant = body["tenantId"]
        .as_str()
        .filter(|tenant| valid_id(tenant))
        .ok_or_else(blocked)?
        .to_string();
    backup::with_restore_component_operation(store, &tenant, || {
        run_locked(store, body, mode, apply)
    })
}

fn run_locked<F>(
    store: &SqliteStore,
    mut body: Value,
    mode: &str,
    mut apply: F,
) -> Result<Value, String>
where
    F: FnMut(Component, &SqliteStore, Value, Value) -> Result<Value, String>,
{
    let tenant = body["tenantId"]
        .as_str()
        .filter(|tenant| valid_id(tenant))
        .ok_or_else(blocked)?
        .to_string();
    if store.class_tenant.as_deref() != Some(tenant.as_str())
        || !matches!(mode, "manual" | "generation" | "recovery")
    {
        return Err(blocked());
    }
    let common = store.shared_store()?;
    let _common_access = restore_journal::access(&common.data_dir)?;
    let _class_access = restore_journal::access(&store.data_dir)?;
    let path = path(&store.shared_data_dir, &tenant)?;
    if exists(&path)? && read(&path)?.phase != Phase::Committed {
        return Err(blocked());
    }
    common_ready(&store.shared_data_dir)?;
    // Invalid or cross-class sources cannot create a durable blocked operation.
    let preview = if mode == "recovery" {
        let source_path = manifest_path(&body)?;
        let manifest: Value =
            serde_json::from_slice(&fs::read(&source_path).map_err(|_| blocked())?)
                .map_err(|_| blocked())?;
        if !matches!(manifest["version"].as_i64(), Some(4 | 5)) {
            return Err("recovery_sealed_snapshot_required".into());
        }
        backup::authoritative_restore_manifest(&source_path, &manifest, &tenant)?;
        json!({"tenantId":tenant,"manifestPath":source_path})
    } else {
        backup::restore_preview(store, body.clone())?
    };
    if preview["tenantId"] != tenant {
        return Err(blocked());
    }
    body["manifestPath"] = preview["manifestPath"].clone();
    if mode == "generation" {
        let manifest: Value =
            serde_json::from_slice(&fs::read(manifest_path(&body)?).map_err(|_| blocked())?)
                .map_err(|_| blocked())?;
        if !matches!(manifest["version"].as_i64(), Some(3 | 4 | 5))
            || body["generation"]
                .as_i64()
                .is_none_or(|generation| generation < 1)
        {
            return Err("backup_sync_manifest_invalid".into());
        }
    }
    let common_protection = backup::run_with_kind(&common, tenant.clone(), "pre_restore", None)?;
    let class_protection = backup::run_with_kind(store, tenant.clone(), "pre_restore", None)?;
    protection_ok(&common_protection)?;
    protection_ok(&class_protection)?;
    let mut journal = Journal {
        version: 1,
        operation_id: crate::random_url_token(),
        tenant_id: tenant.into(),
        mode: mode.into(),
        manifest_sha256: digest_manifest(&body)?,
        common_protection_sha256: digest_manifest(&common_protection)?,
        class_protection_sha256: digest_manifest(&class_protection)?,
        body,
        common_protection,
        class_protection,
        phase: Phase::Prepared,
        common_result: None,
        class_result: None,
    };
    write(&path, &journal)?;
    restore_journal::failpoint("coordinator-prepared");
    resume(&common, store, &path, &mut journal, &mut apply)
}

pub(crate) fn recover_all(store: &SqliteStore) -> Result<(), String> {
    if store.class_tenant.is_some() || running() {
        return Ok(());
    }
    let directory = directory(&store.shared_data_dir)?;
    if !directory.exists() {
        return Ok(());
    }
    let mut failed = false;
    for entry in fs::read_dir(directory).map_err(|_| blocked())? {
        let entry = entry.map_err(|_| blocked())?;
        if !entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            continue;
        }
        let mut journal = match read(&entry.path()) {
            Ok(journal) => journal,
            Err(_) => {
                failed = true;
                continue;
            }
        };
        if journal.phase == Phase::Committed {
            continue;
        }
        let tenant = journal.tenant_id.clone();
        let result = backup::with_restore_component_operation(store, &tenant, || {
            let _common_access = restore_journal::access(&store.data_dir)?;
            let _scope = scope(&journal.tenant_id, Component::Combined);
            // A pending receipt belongs to an already-opened class component.
            // Lost originals cannot bootstrap a replacement from legacy rows.
            let original =
                crate::class_store::class_directory(&store.shared_data_dir, &journal.tenant_id)?;
            if !original.join("class-storage.json").is_file()
                || !original.join(crate::DB_FILE_NAME).is_file()
            {
                return Err("class_restore_original_missing".into());
            }
            let class = store.for_tenant(&journal.tenant_id)?;
            let _class_access = restore_journal::access(&class.data_dir)?;
            {
                let conn = class.conn.lock().map_err(|_| blocked())?;
                restore_journal::recover_initialized(&conn, &class.data_dir)?;
            }
            let mode = journal.mode.clone();
            resume(
                store,
                &class,
                &entry.path(),
                &mut journal,
                &mut |_component, target, protection, canonical_body| {
                    backup::restore_coordinated_component(target, canonical_body, &mode, protection)
                },
            )
        });
        if result.is_err() {
            failed = true;
        }
    }
    if failed {
        Err(blocked())
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "backup_restore_coordinator_tests.rs"]
mod tests;
