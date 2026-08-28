use crate::{
    device_sync_credential::DeviceSyncCredentialStore, password_vault_crypto as crypto,
    BrowserLinkToken, SqliteStore,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use zeroize::Zeroize;

#[path = "password_vault_http.rs"]
mod http;
#[path = "password_vault_validation.rs"]
mod validation;

pub(crate) use http::handle_http;
use validation::*;

pub(crate) fn ensure_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS password_vault_personal_profiles (
          tenant_id TEXT NOT NULL,
          owner_uid TEXT NOT NULL,
          school_code TEXT NOT NULL,
          wrapped_key_json TEXT NOT NULL,
          revision INTEGER NOT NULL DEFAULT 1,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, owner_uid, school_code)
        );
        CREATE TABLE IF NOT EXISTS password_vault_personal_entries (
          tenant_id TEXT NOT NULL,
          owner_uid TEXT NOT NULL,
          school_code TEXT NOT NULL,
          entry_id TEXT NOT NULL,
          category TEXT NOT NULL,
          ciphertext_json TEXT NOT NULL,
          revision INTEGER NOT NULL,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, owner_uid, school_code, entry_id),
          FOREIGN KEY (tenant_id, owner_uid, school_code)
            REFERENCES password_vault_personal_profiles (tenant_id, owner_uid, school_code)
            ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_password_vault_personal_entries_updated
          ON password_vault_personal_entries (tenant_id, owner_uid, school_code, updated_at_ms DESC, entry_id);
        CREATE TABLE IF NOT EXISTS password_vault_shared_local_devices (
          tenant_id TEXT NOT NULL,
          owner_uid TEXT NOT NULL,
          school_code TEXT NOT NULL,
          device_id TEXT NOT NULL,
          public_key TEXT NOT NULL,
          key_version INTEGER,
          status TEXT NOT NULL,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, owner_uid, school_code)
        );
        CREATE TABLE IF NOT EXISTS password_vault_shared_local_state (
          tenant_id TEXT NOT NULL,
          owner_uid TEXT NOT NULL,
          school_code TEXT NOT NULL,
          key_version INTEGER NOT NULL,
          recovery_id_digest TEXT,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, owner_uid, school_code)
        );
        "#,
    )
    .map_err(|error| format!("password_vault_schema_failed:{error}"))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn scope_hash(principal: &BrowserLinkToken, school: &str, suffix: &str) -> String {
    crypto::sha256_hex(format!(
        "{}\0{}\0{}\0{suffix}",
        principal.tenant_id, principal.uid, school
    ))
}

fn personal_account(principal: &BrowserLinkToken, school: &str) -> String {
    format!(
        "password-vault-personal-{}",
        scope_hash(principal, school, "personal")
    )
}

fn device_account(principal: &BrowserLinkToken, school: &str) -> String {
    format!(
        "password-vault-device-{}",
        scope_hash(principal, school, "device")
    )
}

fn school_key_account(principal: &BrowserLinkToken, school: &str, key_version: i64) -> String {
    format!(
        "password-vault-school-{}-{key_version}",
        scope_hash(principal, school, "school")
    )
}

fn recovery_id_account(principal: &BrowserLinkToken, school: &str) -> String {
    format!(
        "password-vault-recovery-{}",
        scope_hash(principal, school, "recovery")
    )
}

fn personal_aad(principal: &BrowserLinkToken, school: &str) -> Vec<u8> {
    crypto::aad(&["personal-key", &principal.tenant_id, &principal.uid, school])
}

fn personal_entry_aad(
    principal: &BrowserLinkToken,
    school: &str,
    entry_id: &str,
    revision: i64,
) -> Vec<u8> {
    crypto::aad(&[
        "personal-entry",
        &principal.tenant_id,
        &principal.uid,
        school,
        entry_id,
        &revision.to_string(),
    ])
}

fn shared_aad(
    school: &str,
    key_version: i64,
    purpose: &str,
    record_id: &str,
    revision: i64,
) -> Vec<u8> {
    crypto::aad(&[
        "shared-record",
        school,
        &key_version.to_string(),
        purpose,
        record_id,
        &revision.to_string(),
    ])
}

fn envelope_aad(school: &str, key_version: i64, device_id: &str, public_key: &str) -> Vec<u8> {
    crypto::aad(&[
        "school-key-envelope",
        school,
        &key_version.to_string(),
        device_id,
        public_key,
    ])
}

fn credential_store(store: &SqliteStore) -> DeviceSyncCredentialStore {
    DeviceSyncCredentialStore::new(store.data_dir.clone())
}

fn personal_key(
    store: &SqliteStore,
    principal: &BrowserLinkToken,
    school: &str,
) -> Result<[u8; 32], String> {
    let raw = credential_store(store)
        .read(&personal_account(principal, school))
        .map_err(|_| "password_vault_personal_locked".to_string())?;
    crypto::decode_key(&raw, "password_vault_personal_key_invalid")
}

fn shared_key(
    store: &SqliteStore,
    principal: &BrowserLinkToken,
    school: &str,
    key_version: i64,
) -> Result<[u8; 32], String> {
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let approved = conn
        .query_row(
            "SELECT 1 FROM password_vault_shared_local_devices
             WHERE tenant_id=?1 AND owner_uid=?2 AND school_code=?3 AND status='approved' AND key_version=?4",
            params![principal.tenant_id, principal.uid, school, key_version],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| format!("password_vault_device_read_failed:{error}"))?
        .is_some();
    drop(conn);
    if !approved {
        return Err("password_vault_device_not_approved".to_string());
    }
    let raw = credential_store(store)
        .read(&school_key_account(principal, school, key_version))
        .map_err(|_| "password_vault_school_key_missing".to_string())?;
    crypto::decode_key(&raw, "password_vault_school_key_invalid")
}

fn personal_status(
    store: &SqliteStore,
    principal: &BrowserLinkToken,
    school: &str,
) -> Result<Value, String> {
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let profile = conn
        .query_row(
            "SELECT revision,created_at_ms,updated_at_ms FROM password_vault_personal_profiles
             WHERE tenant_id=?1 AND owner_uid=?2 AND school_code=?3",
            params![principal.tenant_id, principal.uid, school],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("password_vault_profile_read_failed:{error}"))?;
    let count = conn
        .query_row(
            "SELECT COUNT(*) FROM password_vault_personal_entries WHERE tenant_id=?1 AND owner_uid=?2 AND school_code=?3",
            params![principal.tenant_id, principal.uid, school],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0);
    drop(conn);
    let unlocked = profile.is_some() && personal_key(store, principal, school).is_ok();
    Ok(json!({
        "initialized": profile.is_some(),
        "unlocked": unlocked,
        "entryCount": count,
        "revision": profile.map(|item| item.0).unwrap_or(0),
        "createdAtMs": profile.map(|item| item.1).unwrap_or(0),
        "updatedAtMs": profile.map(|item| item.2).unwrap_or(0)
    }))
}

fn setup_personal(
    store: &SqliteStore,
    principal: &BrowserLinkToken,
    body: &Value,
) -> Result<Value, String> {
    let school = school_code(body.get("schoolCode"))?;
    let passphrase = string(
        body.get("recoveryPassphrase"),
        "recovery_passphrase",
        12,
        256,
        false,
    )?;
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let exists = conn
        .query_row(
            "SELECT 1 FROM password_vault_personal_profiles WHERE tenant_id=?1 AND owner_uid=?2 AND school_code=?3",
            params![principal.tenant_id, principal.uid, school],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| format!("password_vault_profile_read_failed:{error}"))?
        .is_some();
    if exists {
        return Err("password_vault_personal_already_initialized".to_string());
    }
    let mut key = crypto::random_key();
    let wrapped = crypto::wrap_personal_key(&passphrase, &key, &personal_aad(principal, &school))?;
    let encoded = crypto::encode_key(&key);
    credential_store(store).store(&personal_account(principal, &school), &encoded)?;
    let now = now_ms();
    let result = conn.execute(
        "INSERT INTO password_vault_personal_profiles
         (tenant_id,owner_uid,school_code,wrapped_key_json,revision,created_at_ms,updated_at_ms)
         VALUES (?1,?2,?3,?4,1,?5,?5)",
        params![principal.tenant_id, principal.uid, school, wrapped, now],
    );
    key.zeroize();
    if let Err(error) = result {
        credential_store(store).delete(&personal_account(principal, &school));
        return Err(format!("password_vault_profile_create_failed:{error}"));
    }
    drop(conn);
    personal_status(store, principal, &school)
}

fn recover_personal(
    store: &SqliteStore,
    principal: &BrowserLinkToken,
    body: &Value,
) -> Result<Value, String> {
    let school = school_code(body.get("schoolCode"))?;
    let passphrase = string(
        body.get("recoveryPassphrase"),
        "recovery_passphrase",
        12,
        256,
        false,
    )?;
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let wrapped = conn
        .query_row(
            "SELECT wrapped_key_json FROM password_vault_personal_profiles
             WHERE tenant_id=?1 AND owner_uid=?2 AND school_code=?3",
            params![principal.tenant_id, principal.uid, school],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("password_vault_profile_read_failed:{error}"))?
        .ok_or_else(|| "password_vault_personal_not_initialized".to_string())?;
    drop(conn);
    let mut key =
        crypto::unwrap_personal_key(&passphrase, &wrapped, &personal_aad(principal, &school))?;
    credential_store(store).store(
        &personal_account(principal, &school),
        &crypto::encode_key(&key),
    )?;
    key.zeroize();
    personal_status(store, principal, &school)
}

fn list_personal_entries(
    store: &SqliteStore,
    principal: &BrowserLinkToken,
    school: &str,
) -> Result<Value, String> {
    let mut key = personal_key(store, principal, school)?;
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let mut statement = conn
        .prepare(
            "SELECT entry_id,category,ciphertext_json,revision,created_at_ms,updated_at_ms
             FROM password_vault_personal_entries
             WHERE tenant_id=?1 AND owner_uid=?2 AND school_code=?3 ORDER BY updated_at_ms DESC,entry_id",
        )
        .map_err(|error| format!("password_vault_entry_query_failed:{error}"))?;
    let rows = statement
        .query_map(params![principal.tenant_id, principal.uid, school], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(|error| format!("password_vault_entry_query_failed:{error}"))?;
    let mut entries = Vec::new();
    for row in rows {
        let (entry_id, category, ciphertext, revision, created_at, updated_at) =
            row.map_err(|error| format!("password_vault_entry_read_failed:{error}"))?;
        let plaintext = crypto::decrypt_json(
            &key,
            &personal_entry_aad(principal, school, &entry_id, revision),
            &ciphertext,
        )?;
        let mut object = plaintext
            .as_object()
            .cloned()
            .ok_or_else(|| "password_vault_plaintext_invalid".to_string())?;
        object.remove("password");
        object.insert("passwordSet".to_string(), Value::Bool(true));
        object.insert("entryId".to_string(), Value::String(entry_id));
        object.insert("category".to_string(), Value::String(category));
        object.insert("revision".to_string(), json!(revision));
        object.insert("createdAtMs".to_string(), json!(created_at));
        object.insert("updatedAtMs".to_string(), json!(updated_at));
        entries.push(Value::Object(object));
    }
    key.zeroize();
    Ok(json!({ "entries": entries }))
}

fn reveal_personal_entry(
    store: &SqliteStore,
    principal: &BrowserLinkToken,
    body: &Value,
) -> Result<Value, String> {
    let school = school_code(body.get("schoolCode"))?;
    let entry_id = opaque(body.get("entryId"), "entry_id", 8)?;
    let expected = positive(body.get("revision"), "revision", false)?;
    let mut key = personal_key(store, principal, &school)?;
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let row = conn
        .query_row(
            "SELECT category,ciphertext_json,revision,created_at_ms,updated_at_ms
             FROM password_vault_personal_entries
             WHERE tenant_id=?1 AND owner_uid=?2 AND school_code=?3 AND entry_id=?4 AND revision=?5",
            params![principal.tenant_id, principal.uid, school, entry_id, expected],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?, row.get::<_, i64>(4)?)),
        )
        .optional()
        .map_err(|error| format!("password_vault_entry_read_failed:{error}"))?
        .ok_or_else(|| "password_vault_revision_conflict".to_string())?;
    let plaintext = crypto::decrypt_json(
        &key,
        &personal_entry_aad(principal, &school, &entry_id, row.2),
        &row.1,
    )?;
    key.zeroize();
    let mut object = plaintext
        .as_object()
        .cloned()
        .ok_or_else(|| "password_vault_plaintext_invalid".to_string())?;
    object.extend([
        ("entryId".to_string(), Value::String(entry_id)),
        ("category".to_string(), Value::String(row.0)),
        ("revision".to_string(), json!(row.2)),
        ("createdAtMs".to_string(), json!(row.3)),
        ("updatedAtMs".to_string(), json!(row.4)),
    ]);
    Ok(Value::Object(object))
}

fn credential_payload(body: &Value) -> Result<Value, String> {
    Ok(json!({
        "serviceName": string(body.get("serviceName"), "service_name", 1, 160, true)?,
        "username": string(body.get("username"), "username", 0, 320, true)?,
        "password": string(body.get("password"), "password", 1, 2048, false)?,
        "url": string(body.get("url"), "url", 0, 2048, true)?,
        "note": string(body.get("note"), "note", 0, 4000, true)?
    }))
}

fn save_personal_entry(
    store: &SqliteStore,
    principal: &BrowserLinkToken,
    body: &Value,
) -> Result<Value, String> {
    let school = school_code(body.get("schoolCode"))?;
    let entry_id = opaque(body.get("entryId"), "entry_id", 8)?;
    let category = category(body.get("category"))?;
    let expected = positive(body.get("expectedRevision"), "expected_revision", true)?;
    let payload = credential_payload(body)?;
    let mut key = personal_key(store, principal, &school)?;
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let current = conn
        .query_row(
            "SELECT revision,created_at_ms FROM password_vault_personal_entries
             WHERE tenant_id=?1 AND owner_uid=?2 AND school_code=?3 AND entry_id=?4",
            params![principal.tenant_id, principal.uid, school, entry_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(|error| format!("password_vault_entry_read_failed:{error}"))?;
    if current.map(|item| item.0).unwrap_or(0) != expected {
        return Err("password_vault_revision_conflict".to_string());
    }
    let revision = expected + 1;
    let ciphertext = crypto::encrypt_json(
        &key,
        &personal_entry_aad(principal, &school, &entry_id, revision),
        &payload,
    )?;
    key.zeroize();
    let now = now_ms();
    let created_at = current.map(|item| item.1).unwrap_or(now);
    let changed = if current.is_some() {
        conn.execute(
            "UPDATE password_vault_personal_entries SET category=?1,ciphertext_json=?2,revision=revision+1,updated_at_ms=?3
             WHERE tenant_id=?4 AND owner_uid=?5 AND school_code=?6 AND entry_id=?7 AND revision=?8",
            params![category, ciphertext, now, principal.tenant_id, principal.uid, school, entry_id, expected],
        )
    } else {
        conn.execute(
            "INSERT INTO password_vault_personal_entries
             (tenant_id,owner_uid,school_code,entry_id,category,ciphertext_json,revision,created_at_ms,updated_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,1,?7,?7)",
            params![principal.tenant_id, principal.uid, school, entry_id, category, ciphertext, now],
        )
    }
    .map_err(|error| format!("password_vault_entry_write_failed:{error}"))?;
    if changed != 1 {
        return Err("password_vault_revision_conflict".to_string());
    }
    let mut object = payload.as_object().cloned().unwrap_or_default();
    object.extend([
        ("entryId".to_string(), Value::String(entry_id)),
        ("category".to_string(), Value::String(category)),
        ("revision".to_string(), json!(revision)),
        ("createdAtMs".to_string(), json!(created_at)),
        ("updatedAtMs".to_string(), json!(now)),
    ]);
    Ok(Value::Object(object))
}

fn delete_personal_entry(
    store: &SqliteStore,
    principal: &BrowserLinkToken,
    body: &Value,
    entry_id: &str,
) -> Result<Value, String> {
    let school = school_code(body.get("schoolCode"))?;
    let expected = positive(body.get("expectedRevision"), "expected_revision", false)?;
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let changed = conn
        .execute(
            "DELETE FROM password_vault_personal_entries
             WHERE tenant_id=?1 AND owner_uid=?2 AND school_code=?3 AND entry_id=?4 AND revision=?5",
            params![principal.tenant_id, principal.uid, school, entry_id, expected],
        )
        .map_err(|error| format!("password_vault_entry_delete_failed:{error}"))?;
    if changed != 1 {
        return Err("password_vault_revision_conflict".to_string());
    }
    Ok(json!({ "entryId": entry_id, "deleted": true }))
}

fn device_status(
    store: &SqliteStore,
    principal: &BrowserLinkToken,
    school: &str,
) -> Result<Value, String> {
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let row = conn
        .query_row(
            "SELECT device_id,public_key,key_version,status,created_at_ms,updated_at_ms
             FROM password_vault_shared_local_devices WHERE tenant_id=?1 AND owner_uid=?2 AND school_code=?3",
            params![principal.tenant_id, principal.uid, school],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<i64>>(2)?,
                row.get::<_, String>(3)?, row.get::<_, i64>(4)?, row.get::<_, i64>(5)?)),
        )
        .optional()
        .map_err(|error| format!("password_vault_device_read_failed:{error}"))?;
    drop(conn);
    match row {
        Some((device_id, public_key, key_version, status, created_at, updated_at)) => {
            let key_available = credential_store(store)
                .read(&device_account(principal, school))
                .is_ok();
            Ok(
                json!({ "deviceId": device_id, "publicKey": public_key, "keyVersion": key_version,
                "status": if key_available { status } else { "key_missing".to_string() }, "createdAtMs": created_at, "updatedAtMs": updated_at }),
            )
        }
        None => Ok(json!({ "status": "missing" })),
    }
}

fn ensure_device(
    store: &SqliteStore,
    principal: &BrowserLinkToken,
    school: &str,
    force_new: bool,
) -> Result<Value, String> {
    let current = device_status(store, principal, school)?;
    if !force_new && current.get("status").and_then(Value::as_str) != Some("missing") {
        return Ok(current);
    }
    let device_id = format!("password-vault-{}", &crypto::random_token()[..24]);
    let (private_key, public_key) = crypto::generate_device_keypair();
    credential_store(store).store(&device_account(principal, school), &private_key)?;
    let now = now_ms();
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    conn.execute(
        "INSERT INTO password_vault_shared_local_devices
         (tenant_id,owner_uid,school_code,device_id,public_key,key_version,status,created_at_ms,updated_at_ms)
         VALUES (?1,?2,?3,?4,?5,NULL,'pending',?6,?6)
         ON CONFLICT(tenant_id,owner_uid,school_code) DO UPDATE SET
           device_id=excluded.device_id,public_key=excluded.public_key,key_version=NULL,status='pending',updated_at_ms=excluded.updated_at_ms",
        params![principal.tenant_id, principal.uid, school, device_id, public_key, now],
    )
    .map_err(|error| format!("password_vault_device_write_failed:{error}"))?;
    Ok(
        json!({ "deviceId": device_id, "publicKey": public_key, "keyVersion": Value::Null, "status": "pending",
        "createdAtMs": now, "updatedAtMs": now }),
    )
}

fn recovery_document(
    school: &str,
    key_version: i64,
    school_key: &[u8; 32],
    recovery_id: &str,
    created_at: i64,
) -> Value {
    json!({
        "createdAtMs": created_at,
        "keyVersion": key_version,
        "recoveryId": recovery_id,
        "schoolCode": school,
        "schoolKey": crypto::encode_key(school_key),
        "v": 1
    })
}

fn bootstrap_shared(
    store: &SqliteStore,
    principal: &BrowserLinkToken,
    body: &Value,
) -> Result<Value, String> {
    let school = school_code(body.get("schoolCode"))?;
    let device = ensure_device(store, principal, &school, false)?;
    let device_id = string(device.get("deviceId"), "device_id", 16, 128, true)?;
    let public_key = string(device.get("publicKey"), "public_key", 43, 43, true)?;
    let credentials = credential_store(store);
    let existing_key = credentials
        .read(&school_key_account(principal, &school, 1))
        .ok()
        .and_then(|value| crypto::decode_key(&value, "password_vault_school_key_invalid").ok());
    let existing_recovery = credentials
        .read(&recovery_id_account(principal, &school))
        .ok();
    let (mut school_key, recovery_id) = match (existing_key, existing_recovery) {
        (Some(key), Some(recovery)) => (key, recovery),
        _ => (crypto::random_key(), crypto::random_token()),
    };
    credentials.store(
        &school_key_account(principal, &school, 1),
        &crypto::encode_key(&school_key),
    )?;
    credentials.store(&recovery_id_account(principal, &school), &recovery_id)?;
    let envelope = crypto::wrap_key(
        &public_key,
        &school_key,
        &envelope_aad(&school, 1, &device_id, &public_key),
    )?;
    let recovery_digest = crypto::sha256_hex(recovery_id.as_bytes());
    let now = now_ms();
    let document = recovery_document(&school, 1, &school_key, &recovery_id, now);
    school_key.zeroize();
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    conn.execute(
        "UPDATE password_vault_shared_local_devices SET key_version=1,status='approved',updated_at_ms=?1
         WHERE tenant_id=?2 AND owner_uid=?3 AND school_code=?4 AND device_id=?5",
        params![now, principal.tenant_id, principal.uid, school, device_id],
    )
    .map_err(|error| format!("password_vault_device_write_failed:{error}"))?;
    conn.execute(
        "INSERT INTO password_vault_shared_local_state
         (tenant_id,owner_uid,school_code,key_version,recovery_id_digest,created_at_ms,updated_at_ms)
         VALUES (?1,?2,?3,1,?4,?5,?5)
         ON CONFLICT(tenant_id,owner_uid,school_code) DO UPDATE SET
           key_version=1,recovery_id_digest=excluded.recovery_id_digest,updated_at_ms=excluded.updated_at_ms",
        params![principal.tenant_id, principal.uid, school, recovery_digest, now],
    )
    .map_err(|error| format!("password_vault_state_write_failed:{error}"))?;
    Ok(
        json!({ "deviceId": device_id, "publicKey": public_key, "keyVersion": 1,
        "encryptedSchoolKey": envelope, "recoveryIdDigest": recovery_digest, "recoveryDocument": document }),
    )
}

fn approve_device(
    store: &SqliteStore,
    principal: &BrowserLinkToken,
    body: &Value,
) -> Result<Value, String> {
    let school = school_code(body.get("schoolCode"))?;
    let key_version = positive(body.get("keyVersion"), "key_version", false)?;
    let target_device_id = opaque(body.get("targetDeviceId"), "device_id", 16)?;
    let target_public_key = string(body.get("targetPublicKey"), "public_key", 43, 43, true)?;
    let mut key = shared_key(store, principal, &school, key_version)?;
    let envelope = crypto::wrap_key(
        &target_public_key,
        &key,
        &envelope_aad(&school, key_version, &target_device_id, &target_public_key),
    )?;
    key.zeroize();
    Ok(
        json!({ "deviceId": target_device_id, "keyVersion": key_version, "encryptedSchoolKey": envelope }),
    )
}

fn accept_envelope(
    store: &SqliteStore,
    principal: &BrowserLinkToken,
    body: &Value,
) -> Result<Value, String> {
    let school = school_code(body.get("schoolCode"))?;
    let key_version = positive(body.get("keyVersion"), "key_version", false)?;
    let envelope = string(
        body.get("encryptedSchoolKey"),
        "encrypted_school_key",
        40,
        8192,
        false,
    )?;
    let device = device_status(store, principal, &school)?;
    let device_id = string(device.get("deviceId"), "device_id", 16, 128, true)?;
    let public_key = string(device.get("publicKey"), "public_key", 43, 43, true)?;
    let private_key = credential_store(store)
        .read(&device_account(principal, &school))
        .map_err(|_| "password_vault_private_key_missing".to_string())?;
    let mut key = crypto::unwrap_key(
        &private_key,
        &envelope,
        &envelope_aad(&school, key_version, &device_id, &public_key),
    )?;
    credential_store(store).store(
        &school_key_account(principal, &school, key_version),
        &crypto::encode_key(&key),
    )?;
    key.zeroize();
    let now = now_ms();
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    conn.execute(
        "UPDATE password_vault_shared_local_devices SET key_version=?1,status='approved',updated_at_ms=?2
         WHERE tenant_id=?3 AND owner_uid=?4 AND school_code=?5 AND device_id=?6 AND public_key=?7",
        params![key_version, now, principal.tenant_id, principal.uid, school, device_id, public_key],
    )
    .map_err(|error| format!("password_vault_device_write_failed:{error}"))?;
    conn.execute(
        "INSERT INTO password_vault_shared_local_state
         (tenant_id,owner_uid,school_code,key_version,recovery_id_digest,created_at_ms,updated_at_ms)
         VALUES (?1,?2,?3,?4,NULL,?5,?5)
         ON CONFLICT(tenant_id,owner_uid,school_code) DO UPDATE SET key_version=excluded.key_version,updated_at_ms=excluded.updated_at_ms",
        params![principal.tenant_id, principal.uid, school, key_version, now],
    )
    .map_err(|error| format!("password_vault_state_write_failed:{error}"))?;
    Ok(json!({ "deviceId": device_id, "status": "approved", "keyVersion": key_version }))
}

fn validate_shared_plaintext(purpose: &str, plaintext: &Value) -> Result<(), String> {
    if !plaintext.is_object() {
        return Err("password_vault_plaintext_invalid".to_string());
    }
    match purpose {
        "entry" => {
            credential_payload(plaintext)?;
        }
        "correction" => {
            string(plaintext.get("reason"), "correction_reason", 1, 500, true)?;
            if let Some(proposed) = plaintext.get("proposed").filter(|value| !value.is_null()) {
                if !proposed.is_object() {
                    return Err("password_vault_correction_proposal_invalid".to_string());
                }
            }
        }
        _ => return Err("password_vault_purpose_invalid".to_string()),
    }
    Ok(())
}

fn encrypt_shared(
    store: &SqliteStore,
    principal: &BrowserLinkToken,
    body: &Value,
) -> Result<Value, String> {
    let school = school_code(body.get("schoolCode"))?;
    let key_version = positive(body.get("keyVersion"), "key_version", false)?;
    let purpose = string(body.get("purpose"), "purpose", 1, 32, true)?;
    let record_id = opaque(body.get("recordId"), "record_id", 8)?;
    let revision = positive(body.get("revision"), "revision", false)?;
    let plaintext = body
        .get("plaintext")
        .ok_or_else(|| "password_vault_plaintext_invalid".to_string())?;
    validate_shared_plaintext(&purpose, plaintext)?;
    let mut key = shared_key(store, principal, &school, key_version)?;
    let ciphertext = crypto::encrypt_json(
        &key,
        &shared_aad(&school, key_version, &purpose, &record_id, revision),
        plaintext,
    )?;
    key.zeroize();
    Ok(
        json!({ "ciphertext": ciphertext, "ciphertextSha256": crypto::sha256_hex(ciphertext.as_bytes()),
        "keyVersion": key_version, "recordId": record_id, "revision": revision }),
    )
}

fn decrypt_shared(
    store: &SqliteStore,
    principal: &BrowserLinkToken,
    body: &Value,
) -> Result<Value, String> {
    let school = school_code(body.get("schoolCode"))?;
    let key_version = positive(body.get("keyVersion"), "key_version", false)?;
    let purpose = string(body.get("purpose"), "purpose", 1, 32, true)?;
    let record_id = opaque(body.get("recordId"), "record_id", 8)?;
    let revision = positive(body.get("revision"), "revision", false)?;
    let ciphertext = string(body.get("ciphertext"), "ciphertext", 40, 131_072, false)?;
    let mut key = shared_key(store, principal, &school, key_version)?;
    let mut plaintext = crypto::decrypt_json(
        &key,
        &shared_aad(&school, key_version, &purpose, &record_id, revision),
        &ciphertext,
    )?;
    key.zeroize();
    validate_shared_plaintext(&purpose, &plaintext)?;
    let include_password = body
        .get("includePassword")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !include_password {
        if purpose == "entry" {
            if let Some(object) = plaintext.as_object_mut() {
                object.remove("password");
                object.insert("passwordSet".to_string(), Value::Bool(true));
            }
        } else if let Some(proposed) = plaintext.get_mut("proposed").and_then(Value::as_object_mut)
        {
            proposed.remove("password");
            proposed.insert("passwordSet".to_string(), Value::Bool(true));
        }
    }
    Ok(json!({ "plaintext": plaintext }))
}

fn rotate_records(
    items: Option<&Value>,
    id_field: &str,
    purpose: &str,
    school: &str,
    old_version: i64,
    new_version: i64,
    old_key: &[u8; 32],
    new_key: &[u8; 32],
) -> Result<Vec<Value>, String> {
    let rows = items
        .and_then(Value::as_array)
        .ok_or_else(|| "password_vault_recovery_records_invalid".to_string())?;
    if rows.len() > 10_000 {
        return Err("password_vault_recovery_records_invalid".to_string());
    }
    rows.iter()
        .map(|item| {
            let record_id = opaque(item.get(id_field), "record_id", 8)?;
            let revision = positive(item.get("revision"), "revision", false)?;
            let ciphertext = string(item.get("ciphertext"), "ciphertext", 40, 131_072, false)?;
            let plaintext = crypto::decrypt_json(
                old_key,
                &shared_aad(school, old_version, purpose, &record_id, revision),
                &ciphertext,
            )?;
            validate_shared_plaintext(purpose, &plaintext)?;
            let rotated = crypto::encrypt_json(
                new_key,
                &shared_aad(school, new_version, purpose, &record_id, revision + 1),
                &plaintext,
            )?;
            Ok(json!({ (id_field): record_id, "expectedRevision": revision, "ciphertext": rotated }))
        })
        .collect()
}

fn recover_shared(
    store: &SqliteStore,
    principal: &BrowserLinkToken,
    body: &Value,
) -> Result<Value, String> {
    let document = body
        .get("recoveryDocument")
        .and_then(Value::as_object)
        .ok_or_else(|| "password_vault_recovery_document_invalid".to_string())?;
    if document.get("v").and_then(Value::as_i64) != Some(1) {
        return Err("password_vault_recovery_document_invalid".to_string());
    }
    let school = school_code(document.get("schoolCode"))?;
    let old_version = positive(document.get("keyVersion"), "key_version", false)?;
    let recovery_id = string(document.get("recoveryId"), "recovery_id", 43, 43, false)?;
    let mut old_key = crypto::decode_key(
        document
            .get("schoolKey")
            .and_then(Value::as_str)
            .unwrap_or(""),
        "password_vault_recovery_document_invalid",
    )?;
    let new_version = old_version + 1;
    let mut new_key = crypto::random_key();
    let entries = rotate_records(
        body.get("entries"),
        "entryId",
        "entry",
        &school,
        old_version,
        new_version,
        &old_key,
        &new_key,
    )?;
    let corrections = rotate_records(
        body.get("correctionRequests"),
        "requestId",
        "correction",
        &school,
        old_version,
        new_version,
        &old_key,
        &new_key,
    )?;
    let device = ensure_device(store, principal, &school, true)?;
    let device_id = string(device.get("deviceId"), "device_id", 16, 128, true)?;
    let public_key = string(device.get("publicKey"), "public_key", 43, 43, true)?;
    let envelope = crypto::wrap_key(
        &public_key,
        &new_key,
        &envelope_aad(&school, new_version, &device_id, &public_key),
    )?;
    let new_recovery_id = crypto::random_token();
    let new_recovery_digest = crypto::sha256_hex(new_recovery_id.as_bytes());
    let old_recovery_digest = crypto::sha256_hex(recovery_id.as_bytes());
    let now = now_ms();
    let new_document = recovery_document(&school, new_version, &new_key, &new_recovery_id, now);
    let credentials = credential_store(store);
    credentials.store(
        &school_key_account(principal, &school, new_version),
        &crypto::encode_key(&new_key),
    )?;
    credentials.store(&recovery_id_account(principal, &school), &new_recovery_id)?;
    old_key.zeroize();
    new_key.zeroize();
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    conn.execute(
        "UPDATE password_vault_shared_local_devices SET key_version=?1,status='approved',updated_at_ms=?2
         WHERE tenant_id=?3 AND owner_uid=?4 AND school_code=?5 AND device_id=?6",
        params![new_version, now, principal.tenant_id, principal.uid, school, device_id],
    )
    .map_err(|error| format!("password_vault_device_write_failed:{error}"))?;
    conn.execute(
        "INSERT INTO password_vault_shared_local_state
         (tenant_id,owner_uid,school_code,key_version,recovery_id_digest,created_at_ms,updated_at_ms)
         VALUES (?1,?2,?3,?4,?5,?6,?6)
         ON CONFLICT(tenant_id,owner_uid,school_code) DO UPDATE SET
           key_version=excluded.key_version,recovery_id_digest=excluded.recovery_id_digest,updated_at_ms=excluded.updated_at_ms",
        params![principal.tenant_id, principal.uid, school, new_version, new_recovery_digest, now],
    )
    .map_err(|error| format!("password_vault_state_write_failed:{error}"))?;
    Ok(json!({
        "correctionRequests": corrections,
        "deviceId": device_id,
        "encryptedSchoolKey": envelope,
        "entries": entries,
        "keyVersion": new_version,
        "newRecoveryIdDigest": new_recovery_digest,
        "publicKey": public_key,
        "recoveryDocument": new_document,
        "recoveryIdDigest": old_recovery_digest
    }))
}

#[cfg(test)]
#[path = "password_vault_tests.rs"]
mod tests;
