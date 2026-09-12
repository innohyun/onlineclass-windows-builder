//! Provider evidence is not a cloud receipt or a peer ACK. Status never reads
//! manifest/file contents: the selected paths come only from successful verification.
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::SystemTime,
};

#[derive(Clone)]
struct Expected {
    path: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
}
#[derive(Clone)]
struct Snapshot {
    generation: i64,
    artifact_root: String,
    files: Vec<Expected>,
}
#[derive(Clone)]
struct Selection {
    generation: i64,
    artifact_generation: i64,
    artifact_root: String,
}
#[derive(Default)]
struct Cache {
    snapshots: HashMap<PathBuf, Snapshot>,
    bundles: HashMap<PathBuf, Vec<PathBuf>>,
    selected: HashMap<PathBuf, Selection>,
}
fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

// Called only with authenticated checkpoint responses, never local candidates.
pub(crate) fn select_checkpoint(root: &Path, checkpoint: Option<&Value>) {
    let Ok(mut cache) = cache().lock() else {
        return;
    };
    cache.selected.remove(root);
    let Some(cp) = checkpoint else {
        return;
    };
    let Some(generation) = cp["generation"].as_i64().filter(|g| *g > 0) else {
        return;
    };
    let Some(artifact_root) = cp["artifactSetSha256"]
        .as_str()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    else {
        return;
    };
    let artifact_generation = cp["recoveryOfGeneration"]
        .as_i64()
        .filter(|g| *g > 0)
        .unwrap_or(generation);
    if cache.selected.len() >= 128 {
        cache.selected.clear();
    }
    cache.selected.insert(
        root.into(),
        Selection {
            generation,
            artifact_generation,
            artifact_root: artifact_root.to_ascii_lowercase(),
        },
    );
}
fn selected_snapshot(cache: &Cache, root: &Path, generation: i64) -> Option<Snapshot> {
    let selected = cache.selected.get(root)?;
    let snapshot = cache.snapshots.get(root)?;
    (selected.generation == generation
        && snapshot.generation == selected.artifact_generation
        && snapshot.artifact_root == selected.artifact_root)
        .then(|| snapshot.clone())
}

#[cfg(test)]
pub(crate) fn test_selected_files(root: &Path, paths: &[PathBuf]) {
    assert!(root.starts_with(std::env::temp_dir()));
    let files = paths
        .iter()
        .map(|path| {
            let meta = fs::metadata(path).unwrap();
            Expected {
                path: path.clone(),
                size: meta.len(),
                modified: meta.modified().ok(),
            }
        })
        .collect();
    let mut cache = cache().lock().unwrap();
    cache.snapshots.insert(
        root.into(),
        Snapshot {
            generation: 354,
            artifact_root: "a".repeat(64),
            files,
        },
    );
    cache.selected.insert(
        root.into(),
        Selection {
            generation: 354,
            artifact_generation: 354,
            artifact_root: "a".repeat(64),
        },
    );
}

pub(crate) fn remember_bundle(root: &Path, document: &Value) {
    let mut files = vec![root.join("archive.json"), root.join("commit.json")];
    for file in document["files"].as_array().into_iter().flatten() {
        let Some(relative) = file["bundleRelativePath"]
            .as_str()
            .and_then(crate::shared_archive_sync::safe_relative_path)
        else {
            return;
        };
        files.push(root.join(relative));
    }
    if let Ok(mut cache) = cache().lock() {
        if cache.bundles.len() >= 128 {
            cache.bundles.clear();
        }
        cache.bundles.insert(root.to_path_buf(), files);
    }
}
pub(crate) fn remember(manifest_path: &Path, manifest: &Value, authoritative: &Value) {
    let Some(generation) = manifest["generation"].as_i64().filter(|g| *g > 0) else {
        return;
    };
    let Ok(root) = crate::backup_v4::tenant_dir(manifest_path) else {
        return;
    };
    let Some(snapshot_dir) = manifest_path.parent() else {
        return;
    };
    let version = manifest["version"].as_i64().unwrap_or(0);
    let mut paths = vec![
        manifest_path.to_path_buf(),
        snapshot_dir.join("commit.json"),
    ];
    for artifact in manifest["artifacts"].as_array().into_iter().flatten() {
        let Some(relative) = artifact["relativePath"]
            .as_str()
            .and_then(crate::shared_archive_sync::safe_relative_path)
        else {
            return;
        };
        let Ok(path) = crate::backup_v5::artifact_path(manifest_path, version, &relative) else {
            return;
        };
        paths.push(path);
    }
    let Ok(mut cache) = cache().lock() else {
        return;
    };
    for reference in authoritative["archives"]["records"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let Some(relative) = reference["bundleRelativePath"]
            .as_str()
            .and_then(crate::shared_archive_sync::safe_relative_path)
        else {
            return;
        };
        let Some(files) = cache.bundles.get(&root.join(relative)) else {
            return;
        };
        paths.extend(files.iter().cloned());
    }
    paths.sort();
    paths.dedup();
    // Bound a read-only status call. An oversized selection stays unknown.
    if paths.len() > 1024 {
        return;
    }
    let files = paths
        .into_iter()
        .map(|path| {
            fs::metadata(&path).ok().map(|m| Expected {
                path,
                size: m.len(),
                modified: m.modified().ok(),
            })
        })
        .collect::<Option<Vec<_>>>();
    if let Some(files) = files {
        if cache.snapshots.len() >= 128 {
            cache.snapshots.clear();
        }
        cache.snapshots.insert(
            root,
            Snapshot {
                generation,
                artifact_root: manifest["artifactSetSha256"]
                    .as_str()
                    .unwrap_or("")
                    .to_ascii_lowercase(),
                files,
            },
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileState {
    Unknown,
    Pending,
    InSync,
    Missing,
    Error,
}
#[cfg(windows)]
pub(crate) fn file_state(path: &Path) -> FileState {
    use std::os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    };
    use windows_sys::Win32::Storage::{
        CloudFilters::*,
        FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES},
    };
    let file = match fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    {
        Ok(file) => file,
        Err(e) => {
            return if e.kind() == std::io::ErrorKind::NotFound {
                FileState::Missing
            } else {
                FileState::Error
            }
        }
    };
    let Ok(meta) = file.metadata() else {
        return FileState::Error;
    };
    if meta.file_attributes() & 0x400 == 0 {
        return FileState::Unknown;
    }
    // Aligned, bounded storage includes the provider's variable-length identity.
    let mut buffer = vec![0u64; 8192];
    let mut returned = 0;
    let result = unsafe {
        CfGetPlaceholderInfo(
            file.as_raw_handle(),
            CF_PLACEHOLDER_INFO_STANDARD,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 8) as u32,
            &mut returned,
        )
    };
    if result < 0 {
        return FileState::Error;
    }
    // ReturnedLength excludes trailing ABI padding after FileIdentity[1].
    if (returned as usize) < std::mem::offset_of!(CF_PLACEHOLDER_STANDARD_INFO, FileIdentity) {
        return FileState::Error;
    }
    let info = unsafe { &*(buffer.as_ptr() as *const CF_PLACEHOLDER_STANDARD_INFO) };
    if info.ModifiedDataSize > 0 || info.InSyncState == CF_IN_SYNC_STATE_NOT_IN_SYNC {
        FileState::Pending
    } else if info.InSyncState == CF_IN_SYNC_STATE_IN_SYNC {
        FileState::InSync
    } else {
        FileState::Unknown
    }
}
#[cfg(not(windows))]
pub(crate) fn file_state(_: &Path) -> FileState {
    FileState::Unknown
}

pub(crate) fn status(root: Option<&Path>, generation: i64) -> Value {
    let snapshot = root.and_then(|root| {
        let cache = cache().lock().ok()?;
        selected_snapshot(&cache, root, generation)
    });
    let Some(snapshot) = snapshot else {
        return json!({"state":if cfg!(windows){"unknown"}else{"unsupported"},"checkedAtMs":0,"requiredFileCount":0,"inSyncFileCount":0,"pendingFileCount":0,"missingFileCount":0,"unknownFileCount":0,"errorFileCount":0});
    };
    let states = snapshot
        .files
        .iter()
        .map(|file| match fs::metadata(&file.path) {
            Ok(meta) if meta.len() != file.size || meta.modified().ok() != file.modified => {
                FileState::Unknown
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => FileState::Missing,
            Err(_) => FileState::Error,
            _ => file_state(&file.path),
        })
        .collect::<Vec<_>>();
    aggregate(&states)
}
fn aggregate(states: &[FileState]) -> Value {
    let count = |state| states.iter().filter(|s| **s == state).count();
    let (pending, missing, errors, unknown) = (
        count(FileState::Pending),
        count(FileState::Missing),
        count(FileState::Error),
        count(FileState::Unknown),
    );
    let state = if !cfg!(windows) {
        "unsupported"
    } else if errors > 0 {
        "error"
    } else if missing + pending > 0 {
        "pending"
    } else if unknown > 0 || states.is_empty() {
        "unknown"
    } else {
        "in_sync"
    };
    json!({"state":state,"checkedAtMs":chrono::Utc::now().timestamp_millis(),"requiredFileCount":states.len(),
        "inSyncFileCount":count(FileState::InSync),"pendingFileCount":pending,"missingFileCount":missing,
        "unknownFileCount":unknown,"errorFileCount":errors})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evidence_requires_exact_server_root_and_supports_recovery_generation() {
        let root = Path::new("synthetic-only");
        let mut cache = Cache::default();
        cache.snapshots.insert(
            root.into(),
            Snapshot {
                generation: 354,
                artifact_root: "a".repeat(64),
                files: vec![],
            },
        );
        assert!(selected_snapshot(&cache, root, 354).is_none());
        cache.selected.insert(
            root.into(),
            Selection {
                generation: 354,
                artifact_generation: 354,
                artifact_root: "b".repeat(64),
            },
        );
        assert!(selected_snapshot(&cache, root, 354).is_none());
        cache.selected.get_mut(root).unwrap().artifact_root = "a".repeat(64);
        assert!(selected_snapshot(&cache, root, 354).is_some());
        cache.selected.get_mut(root).unwrap().generation = 355;
        assert!(selected_snapshot(&cache, root, 355).is_some());
        assert!(selected_snapshot(&cache, root, 354).is_none());
        assert!(selected_snapshot(&cache, Path::new("different-tenant"), 355).is_none());
    }
    #[test]
    fn missing_and_unknown_never_become_delivery_proof() {
        for states in [
            vec![],
            vec![FileState::Unknown],
            vec![FileState::InSync, FileState::Missing],
        ] {
            assert_ne!(aggregate(&states)["state"], "in_sync");
        }
        assert_ne!(status(None, 354)["state"], "in_sync");
        assert_eq!(status(None, 354)["checkedAtMs"], 0);
    }
}
