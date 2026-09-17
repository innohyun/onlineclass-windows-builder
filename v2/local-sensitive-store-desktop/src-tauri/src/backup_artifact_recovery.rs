//! Exact-byte repair. A checkpoint authorizes hashes, never a newer local DB.
//! This module runs only in the sync worker, not in status/listing calls.
use super::*;
use rusqlite::OptionalExtension;
use std::io::{self, Write};

const MAX_METADATA_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CANDIDATES: usize = 256;
const MAX_CANDIDATE_SEAL_PROBES: usize = 8;

pub(super) fn install_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS local_store_artifact_issue (
        tenant_id TEXT PRIMARY KEY, generation INTEGER NOT NULL, artifact_root TEXT NOT NULL,
        kind TEXT NOT NULL, missing_count INTEGER NOT NULL, kinds_json TEXT NOT NULL,
        first_seen_at_ms INTEGER NOT NULL, last_seen_at_ms INTEGER NOT NULL,
        retry_count INTEGER NOT NULL, repair_available INTEGER NOT NULL);",
    )
    .map_err(|_| "artifact_issue_schema_failed".into())
}

pub(crate) fn artifact_issue_status(
    store: &SqliteStore,
    tenant: &str,
    generation: i64,
) -> Result<Value, String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    conn.query_row("SELECT generation,kind,missing_count,kinds_json,first_seen_at_ms,last_seen_at_ms,retry_count,repair_available
        FROM local_store_artifact_issue WHERE tenant_id=?1 AND generation=?2", params![tenant,generation], |r| {
        let first: i64 = r.get(4)?;
        let retries: i64 = r.get(6)?;
        let kind: String = r.get(1)?;
        Ok(json!({"generation":r.get::<_,i64>(0)?,"kind":kind,"missingFileCount":r.get::<_,i64>(2)?,
            "missingFileKinds":serde_json::from_str::<Value>(&r.get::<_,String>(3)?).unwrap_or(json!([])),
            "firstSeenAtMs":first,"lastSeenAtMs":r.get::<_,i64>(5)?,"retryCount":retries,
            "repairAvailable":r.get::<_,bool>(7)?,
            "prolonged":kind=="missing" && retries>=3 && now_ms().saturating_sub(first)>=600_000}))
    }).optional().map(|v|v.unwrap_or(Value::Null)).map_err(|_| "artifact_issue_read_failed".into())
}

pub(crate) fn pending_local_change_count(store: &SqliteStore, tenant: &str) -> Result<i64, String> {
    store.conn.lock().map_err(|_| "db_lock_failed")?.query_row(
        "SELECT COUNT(*) FROM local_store_device_sync_records WHERE tenant_id=?1 AND changed_generation=0",
        params![tenant], |r| r.get(0)).map_err(|_| "artifact_pending_count_failed".into())
}

pub(crate) fn clear_artifact_issue(store: &SqliteStore, tenant: &str) -> Result<(), String> {
    store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed")?
        .execute(
            "DELETE FROM local_store_artifact_issue WHERE tenant_id=?1",
            params![tenant],
        )
        .map_err(|_| "artifact_issue_clear_failed")?;
    Ok(())
}

fn remember_issue(
    store: &SqliteStore,
    tenant: &str,
    generation: i64,
    root: &str,
    kind: &str,
    kinds: &[String],
    count: usize,
    available: bool,
) -> Result<(), String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    let now = now_ms();
    conn.execute("INSERT INTO local_store_artifact_issue VALUES (?1,?2,?3,?4,?5,?6,?7,?7,1,?8)
        ON CONFLICT(tenant_id) DO UPDATE SET generation=excluded.generation,artifact_root=excluded.artifact_root,
        kind=excluded.kind,missing_count=excluded.missing_count,kinds_json=excluded.kinds_json,
        first_seen_at_ms=CASE WHEN generation=excluded.generation AND artifact_root=excluded.artifact_root
          AND kind=excluded.kind THEN first_seen_at_ms ELSE excluded.first_seen_at_ms END,
        retry_count=CASE WHEN generation=excluded.generation AND artifact_root=excluded.artifact_root
          AND kind=excluded.kind THEN retry_count+1 ELSE 1 END,
        last_seen_at_ms=excluded.last_seen_at_ms,repair_available=excluded.repair_available",
        params![tenant,generation,root,kind,count as i64,json!(kinds).to_string(),now,available])
        .map_err(|_| "artifact_issue_write_failed")?;
    Ok(())
}

fn valid_sha(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn relative(value: &str) -> Result<PathBuf, String> {
    if value.is_empty()
        || value.contains(['\\', ':', '\0'])
        || value.split('/').any(|p| matches!(p, "" | "." | ".."))
    {
        return Err("artifact_repair_path_invalid".into());
    }
    safe_relative_path(value).ok_or_else(|| "artifact_repair_path_invalid".into())
}

// Cloud placeholders are permitted, but redirecting links and path escapes are not.
fn checked_path(base: &Path, rel: &str) -> Result<PathBuf, String> {
    let rel = relative(rel)?;
    let canonical = base
        .canonicalize()
        .map_err(|_| "artifact_repair_root_unavailable")?;
    let mut path = base.to_path_buf();
    for part in rel.components() {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                if meta.file_type().is_symlink()
                    || !path
                        .canonicalize()
                        .map_err(|_| "artifact_repair_path_unavailable")?
                        .starts_with(&canonical)
                {
                    return Err("artifact_repair_path_escape".into());
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err("artifact_repair_path_unavailable".into()),
        }
    }
    Ok(path)
}

fn resident(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & (0x1000 | 0x40000 | 0x400000) != 0 {
            return false;
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        if meta.st_flags() & 0x40000000 != 0 {
            return false;
        }
    }
    true
}

fn metadata_json(path: &Path) -> Result<Value, String> {
    if !resident(path)
        || fs::metadata(path)
            .map_err(|_| "artifact_metadata_unavailable")?
            .len()
            > MAX_METADATA_BYTES
    {
        return Err("artifact_metadata_unavailable".into());
    }
    serde_json::from_slice(&fs::read(path).map_err(|_| "artifact_metadata_unavailable")?)
        .map_err(|_| "artifact_metadata_invalid".into())
}

#[derive(Clone)]
struct Sealed {
    manifest: Value,
    path: PathBuf,
    root: PathBuf,
    artifacts: Vec<ArtifactDigest>,
}

fn manifest_seal(
    path: &Path,
    tenant: &str,
    generation: Option<i64>,
    root: &str,
) -> Result<Sealed, String> {
    let base = crate::backup_v4::tenant_dir(path)?;
    let rel = path
        .strip_prefix(&base)
        .map_err(|_| "artifact_repair_path_escape")?
        .to_string_lossy()
        .replace('\\', "/");
    checked_path(&base, &rel)?;
    let manifest = metadata_json(path)?;
    if !matches!(manifest["version"].as_i64(), Some(3 | 4 | 5))
        || manifest["tenantId"] != tenant
        || manifest["generation"].as_i64() != generation
        || manifest["artifactSetSha256"] != root
    {
        return Err("artifact_repair_seal_mismatch".into());
    }
    let mut artifacts = Vec::new();
    let mut seen = HashSet::new();
    for a in manifest["artifacts"]
        .as_array()
        .ok_or("artifact_repair_seal_invalid")?
    {
        let rel = a["relativePath"]
            .as_str()
            .ok_or("artifact_repair_seal_invalid")?;
        relative(rel)?;
        if !seen.insert(rel.to_string()) {
            return Err("artifact_repair_duplicate_path".into());
        }
        let sha = a["sha256"]
            .as_str()
            .filter(|v| valid_sha(v))
            .ok_or("artifact_repair_seal_invalid")?;
        artifacts.push(ArtifactDigest {
            relative_path: rel.into(),
            size: a["size"].as_u64().ok_or("artifact_repair_seal_invalid")?,
            sha256: sha.into(),
        });
    }
    if artifacts.is_empty() || artifact_set_sha256(&mut artifacts) != root {
        return Err("artifact_repair_root_mismatch".into());
    }
    Ok(Sealed {
        manifest,
        path: path.into(),
        root: base,
        artifacts,
    })
}

fn verify_commit(seal: &Sealed, commit: &Value) -> Result<(), String> {
    if commit["tenantId"] != seal.manifest["tenantId"]
        || commit["generation"].as_i64() != seal.manifest["generation"].as_i64()
        || commit["artifactSetSha256"] != seal.manifest["artifactSetSha256"]
    {
        return Err("artifact_repair_seal_mismatch".into());
    }
    Ok(())
}

fn sealed(
    path: &Path,
    tenant: &str,
    generation: Option<i64>,
    root: &str,
) -> Result<Sealed, String> {
    let seal = manifest_seal(path, tenant, generation, root)?;
    verify_commit(&seal, &metadata_json(&path.with_file_name("commit.json"))?)?;
    Ok(seal)
}

fn candidate_seal(
    path: &Path,
    tenant: &str,
    missing: &[(ArtifactDigest, String)],
    probes: &mut usize,
) -> Result<Sealed, String> {
    let manifest = metadata_json(path)?;
    let root = manifest["artifactSetSha256"].as_str().ok_or("artifact_repair_seal_invalid")?;
    let seal = manifest_seal(path, tenant, manifest["generation"].as_i64(), root)?;
    let commit = path.with_file_name("commit.json");
    let rel = commit.strip_prefix(&seal.root).map_err(|_| "artifact_repair_path_escape")?
        .to_string_lossy().replace('\\', "/");
    let commit = checked_path(&seal.root, &rel)?;
    let meta = fs::symlink_metadata(&commit).map_err(|_| "artifact_metadata_unavailable")?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_METADATA_BYTES {
        return Err("artifact_metadata_unavailable".into());
    }
    if resident(&commit) {
        verify_commit(&seal, &metadata_json(&commit)?)?;
        return Ok(seal);
    }
    if *probes >= MAX_CANDIDATE_SEAL_PROBES {
        return Err("artifact_metadata_unavailable".into());
    }
    // Only locally verified bytes matching an authenticated missing digest
    // may request their seal. Historical DBs/manifests stay offline.
    if !seal.artifacts.iter().any(|candidate| {
        missing.iter().any(|(expected, _)| {
            candidate.size == expected.size && candidate.sha256 == expected.sha256
        }) && artifact_relative(&seal, candidate).ok()
            .and_then(|rel| checked_path(&seal.root, &rel).ok())
            .is_some_and(|path| {
                if *probes >= MAX_CANDIDATE_SEAL_PROBES || !resident(&path)
                    || !fs::metadata(&path).is_ok_and(|m| m.len() == candidate.size)
                {
                    return false;
                }
                // Bound extra full-file hash probes as well as seal requests.
                *probes += 1;
                matches_file(&path, candidate)
            })
    }) {
        return Err("artifact_repair_candidate_unavailable".into());
    }
    crate::onedrive_download::with_downloads(|| crate::onedrive_download::prepare(&commit))?;
    verify_commit(&seal, &metadata_json(&commit)?)?;
    Ok(seal)
}

fn artifact_relative(s: &Sealed, artifact: &ArtifactDigest) -> Result<String, String> {
    let path = crate::backup_v5::artifact_path(
        &s.path,
        s.manifest["version"].as_i64().unwrap_or(0),
        &relative(&artifact.relative_path)?,
    )?;
    Ok(path
        .strip_prefix(&s.root)
        .map_err(|_| "artifact_repair_path_escape")?
        .to_string_lossy()
        .replace('\\', "/"))
}

fn cache_root(store: &SqliteStore, tenant: &str, shared: &Path) -> Result<PathBuf, String> {
    use sha2::{Digest, Sha256};
    if tenant.is_empty()
        || !tenant
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return Err("artifact_repair_tenant_invalid".into());
    }
    let canonical = shared
        .canonicalize()
        .map_err(|_| "artifact_repair_root_unavailable")?;
    let local = store.data_dir.canonicalize().map_err(|_| "artifact_recovery_cache_unavailable")?;
    let in_onedrive = local.components().any(|part| part.as_os_str().to_string_lossy().to_lowercase().starts_with("onedrive"))
        || ["OneDrive", "OneDriveCommercial", "OneDriveConsumer"].iter().any(|key| {
            std::env::var_os(key).and_then(|value| fs::canonicalize(value).ok()).is_some_and(|root| local.starts_with(root))
        });
    if local.starts_with(&canonical) || in_onedrive {
        return Err("artifact_recovery_cache_outside_onedrive_required".into());
    }
    let scope = format!(
        "{:x}",
        Sha256::digest(canonical.to_string_lossy().as_bytes())
    );
    let cache = store
        .data_dir
        .join("sync-recovery-cache")
        .join(scope)
        .join("tenants")
        .join(tenant);
    let mut at = cache.as_path();
    while at != store.data_dir {
        if let Ok(meta) = fs::symlink_metadata(at) {
            if meta.file_type().is_symlink() || !at.canonicalize().is_ok_and(|path| path.starts_with(&local) && !path.starts_with(&canonical)) {
                return Err("artifact_recovery_cache_path_invalid".into());
            }
        }
        at = at.parent().ok_or("artifact_recovery_cache_path_invalid")?;
    }
    Ok(cache)
}

fn manifests(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root.join("snapshots")) else {
        return Vec::new();
    };
    let mut result = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path().join("manifest.json"))
        .collect::<Vec<_>>();
    result.sort();
    result.reverse();
    result.truncate(MAX_CANDIDATES);
    result
}

fn matches_file(path: &Path, a: &ArtifactDigest) -> bool {
    resident(path)
        && fs::metadata(path).is_ok_and(|m| m.len() == a.size)
        && sha256_file(path).is_ok_and(|v| v == (a.size, a.sha256.clone()))
}

fn install_bytes(
    source: &Path,
    base: &Path,
    rel: &str,
    expected: &ArtifactDigest,
) -> Result<bool, String> {
    let target = checked_path(base, rel)?;
    if fs::symlink_metadata(&target).is_ok() {
        return if matches_file(&target, expected) {
            Ok(false)
        } else {
            Err("artifact_repair_existing_mismatch".into())
        };
    }
    let parent = target.parent().ok_or("artifact_repair_path_invalid")?;
    fs::create_dir_all(parent).map_err(|_| "artifact_repair_directory_failed")?;
    checked_path(base, rel)?;
    let temporary = parent.join(format!(".repair-{:016x}.tmp", rand::random::<u64>()));
    let result = (|| {
        let mut input = File::open(source).map_err(|_| "artifact_repair_source_unavailable")?;
        let mut output = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|_| "artifact_repair_stage_failed")?;
        io::copy(&mut input, &mut output).map_err(|_| "artifact_repair_copy_failed")?;
        output
            .flush()
            .and_then(|_| output.sync_all())
            .map_err(|_| "artifact_repair_flush_failed")?;
        drop(output);
        if !matches_file(&temporary, expected) {
            return Err("artifact_repair_source_changed".to_string());
        }
        checked_path(base, rel)?;
        // Atomic no-clobber commit: a concurrent provider arrival always wins.
        #[cfg(windows)]
        let committed = {
            use std::os::windows::ffi::OsStrExt;
            // canonicalize preserves the Windows extended-length prefix. Cache
            // and archive hash paths can exceed MAX_PATH on ordinary profiles.
            let from_path = temporary
                .canonicalize()
                .map_err(|_| "artifact_repair_path_unavailable")?;
            let to_path = parent
                .canonicalize()
                .map_err(|_| "artifact_repair_path_unavailable")?
                .join(target.file_name().ok_or("artifact_repair_path_invalid")?);
            let from = from_path
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>();
            let to = to_path
                .as_os_str()
                .encode_wide()
                .chain(Some(0))
                .collect::<Vec<_>>();
            unsafe {
                windows_sys::Win32::Storage::FileSystem::MoveFileExW(from.as_ptr(), to.as_ptr(), 0)
                    != 0
            }
        };
        #[cfg(not(windows))]
        let committed = fs::hard_link(&temporary, &target).is_ok();
        if !committed && !matches_file(&target, expected) {
            return Err("artifact_repair_commit_failed".into());
        }
        Ok(committed)
    })();
    let _ = fs::remove_file(&temporary);
    result
}

fn kind(a: &ArtifactDigest) -> String {
    if a.relative_path.starts_with("db/") {
        "database"
    } else if a.relative_path.starts_with("meta/") {
        "apply_index"
    } else {
        "attachment"
    }
    .into()
}

/// Called once after failed verification, bound to an authenticated checkpoint.
pub(crate) fn repair_checkpoint_artifacts(
    store: &SqliteStore,
    tenant: &str,
    generation: i64,
    artifact_generation: i64,
    root: &str,
    database: &str,
    failure: &str,
) -> Result<bool, String> {
    if !valid_sha(root) || !valid_sha(database) {
        return Err("artifact_repair_checkpoint_invalid".into());
    }
    let shared = configured_tenant_dir(store, tenant)?;
    let _operation = root_operation(store, &shared)?;
    let cache = cache_root(store, tenant, &shared)?;
    let selected = manifests(&shared)
        .into_iter()
        .chain(manifests(&cache))
        .find_map(|p| sealed(&p, tenant, Some(artifact_generation), root).ok());
    let Some(selected) = selected else {
        let k = if failure.contains("mismatch") {
            "integrity"
        } else {
            "unavailable"
        };
        remember_issue(store, tenant, generation, root, k, &[], 0, false)?;
        return Ok(false);
    };
    if !selected
        .artifacts
        .iter()
        .any(|a| a.relative_path == "db/local-sensitive.sqlite" && a.sha256 == database)
    {
        remember_issue(store, tenant, generation, root, "integrity", &[], 0, false)?;
        return Err("artifact_repair_database_checkpoint_mismatch".into());
    }
    let mut missing = Vec::new();
    for a in &selected.artifacts {
        let rel = artifact_relative(&selected, a)?;
        let target = checked_path(&shared, &rel)?;
        match fs::symlink_metadata(target) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => missing.push((a.clone(), rel)),
            Err(_) => return Err("artifact_repair_target_unavailable".into()),
            Ok(_) => {}
        }
    }
    if missing.is_empty() && selected.root == shared {
        if let Some(repaired) = repair_cached_bundles(
            store,
            tenant,
            generation,
            artifact_generation,
            root,
            &shared,
            &cache,
        )? {
            return Ok(repaired);
        }
        if failure.contains("mismatch") || failure.contains("invalid") {
            remember_issue(store, tenant, generation, root, "integrity", &[], 0, false)?;
        } else {
            clear_artifact_issue(store, tenant)?;
        }
        return Ok(false);
    }
    let mut candidates = Vec::new();
    let mut seal_probes = 0;
    for p in manifests(&cache).into_iter().chain(manifests(&shared)) {
        if let Ok(s) = candidate_seal(&p, tenant, &missing, &mut seal_probes) {
            candidates.push(s);
        }
    }
    let mut repairs = Vec::new();
    for (a, rel) in &missing {
        let found = candidates.iter().find_map(|s| {
            s.artifacts.iter().find_map(|candidate| {
                if candidate.size != a.size || candidate.sha256 != a.sha256 {
                    return None;
                }
                let path = checked_path(&s.root, &artifact_relative(s, candidate).ok()?).ok()?;
                matches_file(&path, a).then_some(path)
            })
        });
        if let Some(source) = found {
            repairs.push((source, a, rel));
        }
    }
    let mut kinds = missing.iter().map(|(a, _)| kind(a)).collect::<Vec<_>>();
    kinds.sort();
    kinds.dedup();
    if !missing.is_empty() {
        remember_issue(
            store,
            tenant,
            generation,
            root,
            "missing",
            &kinds,
            missing.len(),
            !repairs.is_empty(),
        )?;
    }
    let mut repaired = false;
    for (source, a, rel) in repairs {
        repaired |= install_bytes(&source, &shared, rel, a)?;
    }
    // A cached manifest is copied only after all expected artifact bytes exist.
    // Final standard verification still owns all authority and ACK decisions.
    if selected.root != shared
        && selected.artifacts.iter().all(|a| {
            artifact_relative(&selected, a)
                .ok()
                .and_then(|r| checked_path(&shared, &r).ok())
                .is_some_and(|p| matches_file(&p, a))
        })
    {
        for name in ["manifest.json", "commit.json"] {
            let source = selected.path.with_file_name(name);
            let (size, sha256) = sha256_file(&source)?;
            let rel = source
                .strip_prefix(&selected.root)
                .map_err(|_| "artifact_repair_path_escape")?
                .to_string_lossy()
                .replace('\\', "/");
            repaired |= install_bytes(
                &source,
                &shared,
                &rel,
                &ArtifactDigest {
                    relative_path: rel.clone(),
                    size,
                    sha256,
                },
            )?;
        }
    }
    Ok(repaired)
}

fn bundle_files(s: &Sealed) -> Result<Vec<(String, ArtifactDigest)>, String> {
    let index = crate::backup_v4::projection(&s.path, &s.manifest)?;
    let mut files = Vec::new();
    for reference in index["archives"]["records"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let bundle = reference["bundleRelativePath"]
            .as_str()
            .ok_or("artifact_recovery_bundle_invalid")?;
        let directory = checked_path(&s.root, bundle)?;
        let doc = crate::shared_archive_sync::verify_bundle_reference_at(
            &directory,
            s.manifest["tenantId"]
                .as_str()
                .ok_or("artifact_recovery_tenant_invalid")?,
            reference,
        )?;
        let mut names = vec!["archive.json".to_string(), "commit.json".to_string()];
        for file in doc["files"]
            .as_array()
            .ok_or("artifact_recovery_bundle_invalid")?
        {
            names.push(
                file["bundleRelativePath"]
                    .as_str()
                    .ok_or("artifact_recovery_bundle_invalid")?
                    .to_string(),
            );
        }
        for name in names {
            let rel = format!("{bundle}/{name}");
            let source = checked_path(&s.root, &rel)?;
            let (size, sha256) = sha256_file(&source)?;
            files.push((
                rel.clone(),
                ArtifactDigest {
                    relative_path: rel,
                    size,
                    sha256,
                },
            ));
        }
    }
    Ok(files)
}

fn repair_cached_bundles(
    store: &SqliteStore,
    tenant: &str,
    generation: i64,
    artifact_generation: i64,
    root: &str,
    shared: &Path,
    cache: &Path,
) -> Result<Option<bool>, String> {
    let cached = manifests(cache).into_iter().find_map(|p| {
        let s = sealed(&p, tenant, Some(artifact_generation), root).ok()?;
        // The cache index and every bundle must still match the authenticated root.
        verify_checkpoint_manifest_path(&p, tenant, artifact_generation, root).ok()?;
        Some(s)
    });
    let Some(cached) = cached else {
        return Ok(None);
    };
    let files = bundle_files(&cached)?;
    let missing = files
        .iter()
        .filter(|(rel, _)| {
            checked_path(shared, rel).is_ok_and(|p| {
                fs::symlink_metadata(p).is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
            })
        })
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(None);
    }
    remember_issue(
        store,
        tenant,
        generation,
        root,
        "missing",
        &["archive".into()],
        missing.len(),
        true,
    )?;
    let mut repaired = false;
    for (rel, a) in missing {
        repaired |= install_bytes(&checked_path(cache, rel)?, shared, rel, a)?;
    }
    Ok(Some(repaired))
}

/// Retain verified publication bytes outside OneDrive before announcing them.
pub(crate) fn protect_publication(
    store: &SqliteStore,
    tenant: &str,
    snapshot: &Value,
) -> Result<(), String> {
    let path = PathBuf::from(
        snapshot["manifestPath"]
            .as_str()
            .ok_or("backup_manifest_required")?,
    );
    let generation = snapshot["generation"]
        .as_i64()
        .ok_or("artifact_repair_generation_required")?;
    let root = snapshot["artifactSetSha256"]
        .as_str()
        .ok_or("artifact_repair_root_required")?;
    let shared = configured_tenant_dir(store, tenant)?;
    let _operation = root_operation(store, &shared)?;
    let s = sealed(&path, tenant, Some(generation), root)?;
    if s.root.canonicalize().ok() != shared.canonicalize().ok() {
        return Err("artifact_repair_path_escape".into());
    }
    verify_checkpoint_manifest_path(&path, tenant, generation, root)?;
    let cache = cache_root(store, tenant, &shared)?;
    fs::create_dir_all(&cache).map_err(|_| "artifact_recovery_cache_unavailable")?;
    let mut files = Vec::new();
    for a in &s.artifacts {
        files.push((artifact_relative(&s, a)?, a.clone()));
    }
    files.extend(bundle_files(&s)?);
    for name in ["manifest.json", "commit.json"] {
        let source = path.with_file_name(name);
        let (size, sha256) = sha256_file(&source)?;
        let rel = source
            .strip_prefix(&shared)
            .map_err(|_| "artifact_repair_path_escape")?
            .to_string_lossy()
            .replace('\\', "/");
        files.push((
            rel.clone(),
            ArtifactDigest {
                relative_path: rel,
                size,
                sha256,
            },
        ));
    }
    for (rel, a) in files {
        install_bytes(&checked_path(&shared, &rel)?, &cache, &rel, &a)?;
    }
    let rel = path
        .strip_prefix(&shared)
        .map_err(|_| "artifact_repair_path_escape")?;
    verify_checkpoint_manifest_path(&cache.join(rel), tenant, generation, root)?;
    Ok(())
}

pub(super) fn maintain_cache(
    store: &SqliteStore,
    tenant: &str,
    now: i64,
    pins: &HashSet<i64>,
) -> Result<(), String> {
    // Caller has fresh server pin context and holds the shared-root operation lock.
    let shared = configured_tenant_dir(store, tenant)?;
    let cache = cache_root(store, tenant, &shared)?;
    if !cache.exists() {
        return Ok(());
    }
    crate::backup_v5::prune_snapshots(&cache, now, pins)?;
    // Cached attachments are referenced only by cached sealed snapshots; live
    // files are kept in a separate namespace. Unknown references fail closed.
    crate::backup_v5::quarantine_unreferenced_objects(&cache, &HashSet::new(), now)?;
    Ok(())
}

#[cfg(test)]
#[path = "backup_artifact_recovery_tests.rs"]
mod tests;
