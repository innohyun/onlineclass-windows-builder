use super::*;

// Persist the exact, verified file set before deletion. A locked file may leave
// a partial purge; only these same bytes can be removed on the next attempt.
pub(super) fn inventory(root: &Path) -> Result<Vec<Value>, String> {
    fn collect(root: &Path, path: &Path, files: &mut Vec<Value>) -> Result<(), String> {
        let metadata =
            fs::symlink_metadata(path).map_err(|_| "backup_purge_inventory_unavailable")?;
        if metadata.file_type().is_symlink() {
            return Err("backup_purge_symlink".into());
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(path).map_err(|_| "backup_purge_inventory_unavailable")? {
                collect(
                    root,
                    &entry
                        .map_err(|_| "backup_purge_inventory_unavailable")?
                        .path(),
                    files,
                )?;
            }
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "backup_purge_scope_invalid")?
                .to_string_lossy()
                .replace('\\', "/");
            let (size, hash) = sha256_file(path)?;
            files.push(json!({"path":relative,"size":size,"sha256":hash}));
        } else {
            return Err("backup_purge_file_type_invalid".into());
        }
        Ok(())
    }
    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Ok(files)
}

pub(super) fn remove(root: &Path, expected: &[Value]) -> Result<Value, String> {
    let current = inventory(root)?;
    if current.iter().any(|file| !expected.contains(file)) {
        return Err("backup_purge_inventory_changed".into());
    }
    let mut bytes = 0u64;
    let mut files = 0usize;
    let mut dirs = HashSet::new();
    for record in &current {
        let relative = crate::backup::safe_relative_path(
            record["path"].as_str().ok_or("backup_purge_path_invalid")?,
        )
        .ok_or("backup_purge_path_invalid")?;
        let path = root.join(relative);
        let mut parent = path.parent();
        while let Some(dir) = parent {
            if !dir.starts_with(root) {
                break;
            }
            dirs.insert(dir.to_path_buf());
            parent = dir.parent();
        }
        if fs::remove_file(&path).is_err() {
            return Ok(
                json!({"ok":false,"error":"backup_purge_file_locked_or_unavailable","deletedBytes":bytes,"deletedFiles":files}),
            );
        }
        bytes += record["size"].as_u64().unwrap_or(0);
        files += 1;
    }
    // Empty directories from a prior interrupted attempt are safe to remove;
    // remove_dir is non-recursive and refuses any new/unrecognized file.
    fn empty_dirs(path: &Path, dirs: &mut HashSet<PathBuf>) -> Result<(), String> {
        for entry in fs::read_dir(path).map_err(|_| "backup_purge_inventory_unavailable")? {
            let entry = entry.map_err(|_| "backup_purge_inventory_unavailable")?;
            if entry
                .file_type()
                .map_err(|_| "backup_purge_inventory_unavailable")?
                .is_dir()
            {
                empty_dirs(&entry.path(), dirs)?;
            }
        }
        dirs.insert(path.to_path_buf());
        Ok(())
    }
    if empty_dirs(root, &mut dirs).is_err() {
        return Ok(
            json!({"ok":false,"error":"backup_purge_directory_unavailable","deletedBytes":bytes}),
        );
    }
    let mut dirs = dirs.into_iter().collect::<Vec<_>>();
    dirs.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for dir in dirs {
        if fs::remove_dir(dir).is_err() {
            return Ok(
                json!({"ok":false,"error":"backup_purge_directory_changed","deletedBytes":bytes}),
            );
        }
    }
    Ok(json!({"ok":true,"deletedBytes":bytes,"deletedFiles":files}))
}
