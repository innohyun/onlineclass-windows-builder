//! Durable class activation evidence outside the class data directory.
use super::*;

pub(super) fn activation_path(root: &Path, tenant: &str) -> PathBuf {
    root.join("class-migrations").join(format!(
        "{:x}.activated.json",
        Sha256::digest(tenant.as_bytes())
    ))
}

pub(super) fn verify_activation(
    root: &Path,
    tenant: &str,
    directory: &Path,
) -> Result<bool, String> {
    let path = activation_path(root, tenant);
    if !path.exists() {
        return Ok(false);
    }
    let activation: Value =
        serde_json::from_slice(&fs::read(&path).map_err(fail)?).map_err(fail)?;
    if activation["version"] != 1
        || activation["tenantId"] != tenant
        || activation["layout"] != "class_files_v1"
    {
        return Err("class_storage_activation_invalid".into());
    }
    let receipt = directory.join("class-storage.json");
    if !receipt.is_file() || !directory.join(DB_FILE_NAME).is_file() {
        return Err("class_storage_activated_component_missing".into());
    }
    if activation["receiptSha256"] != crate::restore_journal::digest(&receipt)? {
        return Err("class_storage_activation_receipt_changed".into());
    }
    Ok(true)
}

pub(super) fn register_activation(
    root: &Path,
    tenant: &str,
    directory: &Path,
) -> Result<(), String> {
    let _registration = crate::restore_journal::access(root)?;
    if verify_activation(root, tenant, directory)? {
        return Ok(());
    }
    let path = activation_path(root, tenant);
    fs::create_dir_all(
        path.parent()
            .ok_or("class_storage_activation_path_invalid")?,
    )
    .map_err(fail)?;
    let activation = json!({"version":1,"tenantId":tenant,"layout":"class_files_v1",
        "receiptSha256":crate::restore_journal::digest(&directory.join("class-storage.json"))?});
    let candidate = path.with_extension(format!("{}.staging", crate::random_url_token()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&candidate)
        .map_err(fail)?;
    std::io::Write::write_all(&mut file, &serde_json::to_vec(&activation).map_err(fail)?)
        .map_err(fail)?;
    file.sync_all().map_err(fail)?;
    drop(file);
    durable_rename(&candidate, &path)?;
    Ok(())
}
