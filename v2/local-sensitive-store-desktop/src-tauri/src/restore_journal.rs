//! Device-local restore intent + SQLite commit receipt. Never part of a snapshot.
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::HashMap,
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
};

thread_local! { static DEPTH: RefCell<HashMap<PathBuf, usize>> = RefCell::new(HashMap::new()); }
pub(crate) struct AccessGuard {
    root: PathBuf,
    file: Option<File>,
}
impl Drop for AccessGuard {
    fn drop(&mut self) {
        DEPTH.with(|depth| {
            let mut depth = depth.borrow_mut();
            if let Some(n) = depth.get_mut(&self.root) {
                *n -= 1;
                if *n == 0 {
                    depth.remove(&self.root);
                }
            }
        });
        if let Some(file) = &self.file {
            let _ = fs2::FileExt::unlock(file);
        }
    }
}

// Lock order: media access guard -> SQLite mutex/transaction. Reentrant only on
// this thread; the file lock also serializes independent SqliteStore connections.
pub(crate) fn access(root: &Path) -> Result<AccessGuard, String> {
    let root = fs::canonicalize(root).map_err(|_| "restore_root_unavailable")?;
    if DEPTH.with(|depth| {
        let mut depth = depth.borrow_mut();
        if let Some(n) = depth.get_mut(&root) {
            *n += 1;
            true
        } else {
            false
        }
    }) {
        return Ok(AccessGuard { root, file: None });
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(".restore-access.lock"))
        .map_err(|_| "restore_access_lock_failed")?;
    fs2::FileExt::lock_exclusive(&file).map_err(|_| "restore_access_lock_failed")?;
    DEPTH.with(|depth| {
        depth.borrow_mut().insert(root.clone(), 1);
    });
    Ok(AccessGuard {
        root,
        file: Some(file),
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Media {
    pub staged: PathBuf,
    pub target: PathBuf,
    pub rollback: PathBuf,
    pub incoming_sha256: String,
    pub previous_sha256: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Intent {
    pub operation_id: String,
    pub staging_root: PathBuf,
    pub files: Vec<Media>,
}

fn error() -> String {
    "restore_recovery_required".into()
}
pub(crate) fn digest(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|_| error())?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let size = file.read(&mut buffer).map_err(|_| error())?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn matches(path: &Path, hash: &str) -> bool {
    digest(path).is_ok_and(|value| value == hash)
}

// Reject traversal and symlinks/reparse points in every existing component. A
// locally damaged journal must never authorize moving/deleting an outside file.
fn checked(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(error());
    }
    let mut path = root.to_path_buf();
    for part in relative.components() {
        path.push(part.as_os_str());
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(error());
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if meta.file_attributes() & 0x400 != 0 {
                        return Err(error());
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(error()),
        }
    }
    Ok(path)
}

pub(crate) fn install(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS local_store_restore_journal (
      tenant_id TEXT PRIMARY KEY, operation_id TEXT NOT NULL UNIQUE,
      generation INTEGER NOT NULL, artifact_root TEXT NOT NULL,
      phase TEXT NOT NULL CHECK(phase IN ('prepared','applying','committed')),
      intent_json TEXT NOT NULL);",
    )
    .map_err(|_| error())?;
    Ok(())
}
pub(crate) fn install_guards(conn: &Connection) -> Result<(), String> {
    for name in crate::backup::restore_guard_tables() {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                params![name],
                |r| r.get(0),
            )
            .map_err(|_| error())?;
        if !exists {
            continue;
        }
        for (event, row) in [("INSERT", "NEW"), ("UPDATE", "NEW"), ("DELETE", "OLD")] {
            let scope = if event == "UPDATE" {
                "tenant_id IN (OLD.tenant_id,NEW.tenant_id)".to_string()
            } else {
                format!("tenant_id={row}.tenant_id")
            };
            conn.execute_batch(&format!(
                "CREATE TRIGGER IF NOT EXISTS restore_block_{name}_{event}
              BEFORE {event} ON {name} WHEN EXISTS(SELECT 1 FROM local_store_restore_journal
                WHERE {scope} AND phase<>'applying')
              BEGIN SELECT RAISE(ABORT,'restore_recovery_required'); END;"
            ))
            .map_err(|_| error())?;
        }
    }
    Ok(())
}
pub(crate) fn ready(conn: &Connection, tenant: &str) -> Result<(), String> {
    let blocked: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM local_store_restore_journal WHERE tenant_id=?1)",
            params![tenant],
            |r| r.get(0),
        )
        .map_err(|_| error())?;
    if blocked {
        Err(error())
    } else {
        Ok(())
    }
}

pub(crate) fn prepare(
    conn: &Connection,
    root: &Path,
    tenant: &str,
    generation: i64,
    artifact_root: &str,
    intent: &Intent,
) -> Result<(), String> {
    ready(conn, tenant)?;
    validate(root, intent)?;
    for file in &intent.files {
        let target = checked(root, &file.target)?;
        match &file.previous_sha256 {
            Some(hash) if !matches(&target, hash) => {
                return Err("restore_local_file_changed".into())
            }
            None if target.exists() => return Err("restore_local_file_changed".into()),
            _ => {}
        }
        let staged = checked(root, &file.staged)?;
        if !matches(&staged, &file.incoming_sha256) {
            return Err(error());
        }
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(staged)
            .and_then(|file| file.sync_all())
            .map_err(|_| error())?;
    }
    conn.execute(
        "INSERT INTO local_store_restore_journal VALUES (?1,?2,?3,?4,'prepared',?5)",
        params![
            tenant,
            intent.operation_id,
            generation,
            artifact_root,
            serde_json::to_string(intent).map_err(|_| error())?
        ],
    )
    .map_err(|_| error())?;
    failpoint("prepared");
    Ok(())
}

fn validate(root: &Path, intent: &Intent) -> Result<(), String> {
    if !intent.staging_root.starts_with(".restore-staging")
        || intent.staging_root.components().count() != 2
    {
        return Err(error());
    }
    checked(root, &intent.staging_root)?;
    let mut targets = std::collections::HashSet::new();
    for file in &intent.files {
        if !file.staged.starts_with(intent.staging_root.join("staged"))
            || !file
                .rollback
                .starts_with(intent.staging_root.join("rollback"))
            || !(file.target.starts_with("board-media")
                || file.target.starts_with("work-note-attachments"))
            || !targets.insert(file.target.clone())
        {
            return Err(error());
        }
        for path in [&file.staged, &file.target, &file.rollback] {
            checked(root, path)?;
        }
    }
    Ok(())
}
pub(crate) fn apply(
    conn: &Connection,
    root: &Path,
    tenant: &str,
    intent: &Intent,
) -> Result<(), String> {
    // This phase change rolls back to prepared if the SQLite transaction dies.
    let changed = conn.execute("UPDATE local_store_restore_journal SET phase='applying' WHERE tenant_id=?1 AND operation_id=?2 AND phase='prepared'",params![tenant,intent.operation_id]).map_err(|_| error())?;
    if changed != 1 {
        return Err(error());
    }
    for (index, file) in intent.files.iter().enumerate() {
        let target = checked(root, &file.target)?;
        let rollback = checked(root, &file.rollback)?;
        fs::create_dir_all(target.parent().ok_or_else(error)?).map_err(|_| error())?;
        fs::create_dir_all(rollback.parent().ok_or_else(error)?).map_err(|_| error())?;
        if file.previous_sha256.is_some() {
            fs::rename(&target, &rollback).map_err(|_| error())?;
        }
        failpoint(&format!("preserved-{index}"));
        fs::rename(checked(root, &file.staged)?, target).map_err(|_| error())?;
        failpoint(&format!("replaced-{index}"));
    }
    Ok(())
}
pub(crate) fn receipt(conn: &Connection, tenant: &str, intent: &Intent) -> Result<(), String> {
    failpoint("before-commit");
    let changed = conn.execute("UPDATE local_store_restore_journal SET phase='committed' WHERE tenant_id=?1 AND operation_id=?2 AND phase='applying'",params![tenant,intent.operation_id]).map_err(|_| error())?;
    if changed != 1 {
        return Err(error());
    }
    Ok(())
}

pub(crate) fn finish(conn: &Connection, root: &Path, tenant: &str) -> Result<(), String> {
    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT phase,intent_json,operation_id FROM local_store_restore_journal WHERE tenant_id=?1",
            params![tenant],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|_| error())?;
    let Some((phase, raw, operation_id)) = row else {
        return Ok(());
    };
    let intent: Intent = serde_json::from_str(&raw).map_err(|_| error())?;
    if intent.operation_id != operation_id || !matches!(phase.as_str(), "prepared" | "committed") {
        return Err(error());
    }
    validate(root, &intent)?;
    if phase == "committed" {
        failpoint("after-commit");
    }
    for file in intent.files.iter().rev() {
        let target = checked(root, &file.target)?;
        let rollback = checked(root, &file.rollback)?;
        if phase == "committed" {
            if !matches(&target, &file.incoming_sha256) {
                return Err(error());
            }
        } else if let Some(hash) = &file.previous_sha256 {
            if rollback.exists() {
                if !matches(&rollback, hash) {
                    return Err(error());
                }
                if target.exists() {
                    if !matches(&target, &file.incoming_sha256) {
                        return Err(error());
                    }
                    fs::remove_file(&target).map_err(|_| error())?;
                }
                fs::rename(rollback, &target).map_err(|_| error())?;
            }
            if !matches(&target, hash) {
                return Err(error());
            }
        } else if target.exists() {
            if !matches(&target, &file.incoming_sha256) {
                return Err(error());
            }
            fs::remove_file(target).map_err(|_| error())?;
        }
    }
    // Clear only after file consistency is verified. Leftover staging is not
    // authority; retain it on cleanup error rather than deleting rollback bytes.
    failpoint("before-cleanup");
    let staging = checked(root, &intent.staging_root)?;
    if staging.exists() {
        fs::remove_dir_all(staging).map_err(|_| error())?;
    }
    conn.execute(
        "DELETE FROM local_store_restore_journal WHERE tenant_id=?1 AND operation_id=?2",
        params![tenant, intent.operation_id],
    )
    .map_err(|_| error())?;
    Ok(())
}

pub(crate) fn recover(conn: &Connection, root: &Path) -> Result<(), String> {
    let _access = access(root)?;
    install(conn)?;
    let tenants = conn
        .prepare("SELECT tenant_id FROM local_store_restore_journal")
        .map_err(|_| error())?
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|_| error())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| error())?;
    // Failed tenants stay guarded; other tenants and read-only diagnostics work.
    for tenant in tenants {
        let _ = finish(conn, root, &tenant);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn failpoint(name: &str) {
    if std::env::var("CLASSAIMATE_QA_RESTORE_CRASH")
        .ok()
        .as_deref()
        == Some(name)
    {
        let root = PathBuf::from(
            std::env::var("CLASSAIMATE_QA_RESTORE_ROOT").expect("child fixture root"),
        );
        assert!(
            root.starts_with(std::env::temp_dir())
                && !root.components().any(|p| matches!(p, Component::ParentDir))
        );
        assert!(root
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("classaimate-qa-restore-"));
        fs::write(root.join("crash-ready"), name).expect("signal parent");
        loop {
            std::thread::park();
        } // Parent forcibly kills us; no unwinding.
    }
}
#[cfg(not(test))]
pub(crate) fn failpoint(_: &str) {}

#[cfg(test)]
#[path = "restore_journal_tests.rs"]
mod tests;
