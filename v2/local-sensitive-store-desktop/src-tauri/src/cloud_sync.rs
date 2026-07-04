use crate::SqliteStore;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::{DateTime, Utc};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use url::Url;

const KEYRING_SERVICE: &str = "OnlineClassLocalSensitiveStore";
const SESSION_FILE_NAME: &str = "cloud-sync-session.json";
const CREDENTIAL_FILE_NAME: &str = "cloud-sync-credentials.json";
const SYNC_INTERVAL_SECS: u64 = 60;
const SYNC_LIMIT: i64 = 50;
const MODE_FIRESTORE: &str = "firestore";
const MODE_LOCAL_SQLITE: &str = "local_sqlite";
const MODE_HYBRID_FIRESTORE_LOCAL: &str = "hybrid_firestore_local";
const MODE_HYBRID_FIRESTORE_LOCAL_KEEP_REMOTE: &str = "hybrid_firestore_local_keep_remote";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
struct CloudSyncSession {
    tenant_id: String,
    project_id: String,
    api_key: String,
    uid: String,
    account_email: String,
    account_display_name: String,
    tenant_name: String,
    observation_storage_mode: String,
    keyring_account: String,
    credential_storage: String,
    connected_at_ms: i64,
    last_run_at_ms: i64,
    last_sync_at_ms: i64,
    last_imported: i64,
    last_deleted: i64,
    last_marked: i64,
    last_pending: i64,
    last_failed: i64,
    last_conflicts: i64,
    last_error: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
struct StoredCredential {
    account: String,
    protected_refresh_token: String,
    updated_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
struct CredentialFile {
    version: i64,
    updated_at_ms: i64,
    credentials: Vec<StoredCredential>,
}

impl Default for CredentialFile {
    fn default() -> Self {
        Self {
            version: 1,
            updated_at_ms: 0,
            credentials: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct RemoteObservation {
    name: String,
    update_time: String,
    doc_id: String,
    storage_mode: String,
    payload: Value,
    updated_at_ms: i64,
}

#[derive(Clone, Debug)]
struct RemoteStudentPrivateDetail {
    name: String,
    update_time: String,
    student_code: String,
    storage_mode: String,
    payload: Value,
    updated_at_ms: i64,
}

pub(crate) struct CloudSyncManager {
    session_path: PathBuf,
    credential_path: PathBuf,
    store: Arc<SqliteStore>,
    sync_lock: Mutex<()>,
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn normalize(value: impl ToString, max_len: usize) -> String {
    let mut text = value.to_string().trim().to_string();
    if max_len > 0 && text.chars().count() > max_len {
        text = text.chars().take(max_len).collect();
    }
    text
}

fn normalize_json_text(value: Option<&Value>, max_len: usize) -> String {
    match value {
        Some(Value::String(text)) => normalize(text, max_len),
        Some(Value::Number(number)) => normalize(number, max_len),
        Some(Value::Bool(flag)) => normalize(flag, max_len),
        _ => String::new(),
    }
}

fn normalize_tenant_id(value: Option<&Value>) -> String {
    let tenant_id = normalize_json_text(value, 160);
    if tenant_id.is_empty() || tenant_id.contains('/') || tenant_id.contains('\\') {
        String::new()
    } else {
        tenant_id
    }
}

fn normalize_observation_storage_mode(value: Option<&Value>) -> String {
    let mode = normalize_json_text(value, 80);
    match mode.as_str() {
        MODE_FIRESTORE
        | MODE_LOCAL_SQLITE
        | MODE_HYBRID_FIRESTORE_LOCAL
        | MODE_HYBRID_FIRESTORE_LOCAL_KEEP_REMOTE => mode,
        _ => String::new(),
    }
}

fn is_keyring_get_failed(error: &str) -> bool {
    error.starts_with("keyring_get_failed:")
}

fn status_from_session(session: Option<&CloudSyncSession>) -> Value {
    match session {
        Some(session) => {
            let credential_missing = is_keyring_get_failed(&session.last_error);
            json!({
                "ok": true,
                "connected": true,
                "tenantId": session.tenant_id,
                "projectId": session.project_id,
                "uid": session.uid,
                "accountEmail": session.account_email,
                "accountDisplayName": session.account_display_name,
                "tenantName": session.tenant_name,
                "observationStorageMode": session.observation_storage_mode,
                "credentialStorage": session.credential_storage,
                "connectedAtMs": session.connected_at_ms,
                "lastRunAtMs": session.last_run_at_ms,
                "lastSyncAtMs": session.last_sync_at_ms,
                "lastImported": session.last_imported,
                "lastDeleted": session.last_deleted,
                "lastMarked": session.last_marked,
                "lastPending": session.last_pending,
                "lastFailed": session.last_failed,
                "lastConflicts": session.last_conflicts,
                "lastError": session.last_error,
                "lastErrorCode": if credential_missing { "credential_missing" } else { "" },
                "credentialMissing": credential_missing,
                "needsReconnect": credential_missing,
                "reconnectMessage": if credential_missing { "저장된 로그인 연결이 만료되었거나 Windows 보안 저장소에서 삭제되었습니다. 이 PC 자동 연결을 다시 실행하세요." } else { "" },
            })
        },
        None => json!({
            "ok": true,
            "connected": false,
            "tenantId": "",
            "accountEmail": "",
            "accountDisplayName": "",
            "tenantName": "",
            "lastImported": 0,
            "lastDeleted": 0,
            "lastMarked": 0,
            "lastPending": 0,
            "lastFailed": 0,
            "lastConflicts": 0,
            "lastError": "",
            "lastErrorCode": "",
            "credentialStorage": "",
            "credentialMissing": false,
            "needsReconnect": false,
            "reconnectMessage": "",
        }),
    }
}

fn http_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(status, response) => {
            let body = response.into_string().unwrap_or_default();
            format!("http_status:{status}:{}", normalize(body, 300))
        }
        ureq::Error::Transport(error) => format!("http_transport:{error}"),
    }
}

fn decode_firestore_field(field: &Value) -> Value {
    if let Some(value) = field.get("nullValue") {
        let _ = value;
        return Value::Null;
    }
    if let Some(value) = field.get("stringValue").and_then(|v| v.as_str()) {
        return Value::String(value.to_string());
    }
    if let Some(value) = field.get("timestampValue").and_then(|v| v.as_str()) {
        return Value::String(value.to_string());
    }
    if let Some(value) = field.get("booleanValue").and_then(|v| v.as_bool()) {
        return Value::Bool(value);
    }
    if let Some(value) = field.get("integerValue") {
        if let Some(text) = value.as_str() {
            if let Ok(parsed) = text.parse::<i64>() {
                return Value::Number(parsed.into());
            }
        }
        if let Some(number) = value.as_i64() {
            return Value::Number(number.into());
        }
    }
    if let Some(value) = field.get("doubleValue").and_then(|v| v.as_f64()) {
        return serde_json::Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or(Value::Null);
    }
    if let Some(values) = field
        .get("arrayValue")
        .and_then(|v| v.get("values"))
        .and_then(|v| v.as_array())
    {
        return Value::Array(values.iter().map(decode_firestore_field).collect());
    }
    if let Some(fields) = field
        .get("mapValue")
        .and_then(|v| v.get("fields"))
        .and_then(|v| v.as_object())
    {
        let mut out = Map::new();
        for (key, value) in fields {
            out.insert(key.clone(), decode_firestore_field(value));
        }
        return Value::Object(out);
    }
    Value::Null
}

fn decode_firestore_fields(fields: &Map<String, Value>) -> Value {
    let mut out = Map::new();
    for (key, value) in fields {
        out.insert(key.clone(), decode_firestore_field(value));
    }
    Value::Object(out)
}

fn timestamp_ms(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(number)) => number.as_i64().unwrap_or(0),
        Some(Value::String(text)) => DateTime::parse_from_rfc3339(text.trim())
            .map(|dt| dt.timestamp_millis())
            .unwrap_or(0),
        Some(Value::Object(obj)) => obj
            .get("seconds")
            .or_else(|| obj.get("_seconds"))
            .and_then(|v| v.as_i64())
            .map(|seconds| seconds * 1000)
            .unwrap_or(0),
        _ => 0,
    }
}

fn keyring_account(tenant_id: &str, uid: &str) -> String {
    format!("cloud-sync:{tenant_id}:{uid}")
}

fn keyring_entry(account: &str) -> Result<Entry, String> {
    Entry::new(KEYRING_SERVICE, account).map_err(|e| format!("keyring_entry_failed:{e}"))
}

#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
extern "system" {
    fn LocalFree(hmem: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}

#[cfg(target_os = "windows")]
fn protect_refresh_token(refresh_token: &str) -> Result<String, String> {
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};

    let bytes = refresh_token.as_bytes();
    let mut input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &mut input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            0,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(format!("dpapi_protect_failed:{}", std::io::Error::last_os_error()));
    }
    let encrypted = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData as _);
    }
    Ok(BASE64_STANDARD.encode(encrypted))
}

#[cfg(not(target_os = "windows"))]
fn protect_refresh_token(_refresh_token: &str) -> Result<String, String> {
    Err("dpapi_unsupported".to_string())
}

#[cfg(target_os = "windows")]
fn unprotect_refresh_token(protected_refresh_token: &str) -> Result<String, String> {
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    let encrypted = BASE64_STANDARD
        .decode(protected_refresh_token)
        .map_err(|e| format!("dpapi_base64_decode_failed:{e}"))?;
    let mut input = CRYPT_INTEGER_BLOB {
        cbData: encrypted.len() as u32,
        pbData: encrypted.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &mut input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            0,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(format!("dpapi_unprotect_failed:{}", std::io::Error::last_os_error()));
    }
    let bytes = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData as _);
    }
    String::from_utf8(bytes).map_err(|e| format!("dpapi_utf8_failed:{e}"))
}

#[cfg(not(target_os = "windows"))]
fn unprotect_refresh_token(_protected_refresh_token: &str) -> Result<String, String> {
    Err("dpapi_unsupported".to_string())
}

fn is_cloud_sync_storage_mode(mode: &str) -> bool {
    matches!(
        mode,
        MODE_HYBRID_FIRESTORE_LOCAL | MODE_HYBRID_FIRESTORE_LOCAL_KEEP_REMOTE
    )
}

fn keeps_remote_after_import(mode: &str) -> bool {
    mode == MODE_HYBRID_FIRESTORE_LOCAL_KEEP_REMOTE
}

fn firestore_string_field(value: impl ToString) -> Value {
    json!({ "stringValue": value.to_string() })
}

fn firestore_integer_field(value: i64) -> Value {
    json!({ "integerValue": value.to_string() })
}

fn firestore_timestamp_field(value: &DateTime<Utc>) -> Value {
    json!({ "timestampValue": value.to_rfc3339() })
}

impl CloudSyncManager {
    pub(crate) fn new(data_dir: PathBuf, store: Arc<SqliteStore>) -> Self {
        Self {
            session_path: data_dir.join(SESSION_FILE_NAME),
            credential_path: data_dir.join(CREDENTIAL_FILE_NAME),
            store,
            sync_lock: Mutex::new(()),
        }
    }

    pub(crate) fn start_background_sync(self: &Arc<Self>) {
        let manager = Arc::clone(self);
        thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(15));
            let _ = manager.run_once();
            thread::sleep(Duration::from_secs(SYNC_INTERVAL_SECS));
        });
    }

    fn agent(&self) -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(20))
            .build()
    }

    fn load_session(&self) -> Result<Option<CloudSyncSession>, String> {
        if !self.session_path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(&self.session_path)
            .map_err(|e| format!("cloud_session_read_failed:{e}"))?;
        if raw.trim().is_empty() {
            return Ok(None);
        }
        let session = serde_json::from_str::<CloudSyncSession>(&raw)
            .map_err(|e| format!("cloud_session_decode_failed:{e}"))?;
        if session.tenant_id.is_empty() || session.project_id.is_empty() || session.uid.is_empty() {
            return Ok(None);
        }
        Ok(Some(session))
    }

    fn save_session(&self, session: &CloudSyncSession) -> Result<(), String> {
        if let Some(parent) = self.session_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("cloud_session_dir_failed:{e}"))?;
        }
        let raw = serde_json::to_string_pretty(session)
            .map_err(|e| format!("cloud_session_encode_failed:{e}"))?;
        fs::write(&self.session_path, raw).map_err(|e| format!("cloud_session_write_failed:{e}"))
    }

    fn load_credential_file(&self) -> Result<CredentialFile, String> {
        if !self.credential_path.exists() {
            return Ok(CredentialFile::default());
        }
        let raw = fs::read_to_string(&self.credential_path)
            .map_err(|e| format!("credential_file_read_failed:{e}"))?;
        if raw.trim().is_empty() {
            return Ok(CredentialFile::default());
        }
        serde_json::from_str::<CredentialFile>(&raw)
            .map_err(|e| format!("credential_file_decode_failed:{e}"))
    }

    fn save_credential_file(&self, credential_file: &CredentialFile) -> Result<(), String> {
        if let Some(parent) = self.credential_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("credential_file_dir_failed:{e}"))?;
        }
        let raw = serde_json::to_string_pretty(credential_file)
            .map_err(|e| format!("credential_file_encode_failed:{e}"))?;
        fs::write(&self.credential_path, raw).map_err(|e| format!("credential_file_write_failed:{e}"))
    }

    fn save_fallback_refresh_token(&self, account: &str, refresh_token: &str) -> Result<(), String> {
        let protected_refresh_token = protect_refresh_token(refresh_token)?;
        let mut credential_file = self.load_credential_file()?;
        credential_file.credentials.retain(|item| item.account != account);
        credential_file.credentials.push(StoredCredential {
            account: account.to_string(),
            protected_refresh_token,
            updated_at_ms: now_ms(),
        });
        credential_file.version = 1;
        credential_file.updated_at_ms = now_ms();
        self.save_credential_file(&credential_file)
    }

    fn read_fallback_refresh_token(&self, account: &str) -> Result<String, String> {
        let credential_file = self.load_credential_file()?;
        let protected_refresh_token = credential_file
            .credentials
            .iter()
            .find(|item| item.account == account)
            .map(|item| item.protected_refresh_token.clone())
            .unwrap_or_default();
        if protected_refresh_token.is_empty() {
            return Err("fallback_credential_missing".to_string());
        }
        unprotect_refresh_token(&protected_refresh_token)
    }

    fn delete_fallback_refresh_token(&self, account: &str) -> Result<(), String> {
        if !self.credential_path.exists() {
            return Ok(());
        }
        let mut credential_file = self.load_credential_file()?;
        let before = credential_file.credentials.len();
        credential_file.credentials.retain(|item| item.account != account);
        if credential_file.credentials.len() == before {
            return Ok(());
        }
        credential_file.updated_at_ms = now_ms();
        self.save_credential_file(&credential_file)
    }

    fn store_refresh_token(&self, account: &str, refresh_token: &str) -> Result<String, String> {
        let keyring_result = (|| -> Result<(), String> {
            let entry = keyring_entry(account)?;
            entry
                .set_password(refresh_token)
                .map_err(|e| format!("keyring_set_failed:{e}"))?;
            let stored = entry
                .get_password()
                .map_err(|e| format!("keyring_get_failed:{e}"))?;
            if stored != refresh_token {
                return Err("keyring_verify_mismatch".to_string());
            }
            Ok(())
        })();
        if keyring_result.is_ok() {
            #[cfg(target_os = "windows")]
            if self.save_fallback_refresh_token(account, refresh_token).is_ok() {
                return Ok("keyring+windows_dpapi_file".to_string());
            }
            return Ok("keyring".to_string());
        }
        let keyring_error = keyring_result.err().unwrap_or_else(|| "keyring_failed".to_string());
        self.save_fallback_refresh_token(account, refresh_token)
            .map(|_| "windows_dpapi_file".to_string())
            .map_err(|fallback_error| format!("{keyring_error};fallback_set_failed:{fallback_error}"))
    }

    fn read_refresh_token(&self, session: &CloudSyncSession) -> Result<(String, String), String> {
        let keyring_result = keyring_entry(&session.keyring_account)
            .and_then(|entry| entry.get_password().map_err(|e| format!("keyring_get_failed:{e}")));
        if let Ok(refresh_token) = keyring_result.as_ref() {
            if !refresh_token.is_empty() {
                return Ok((refresh_token.to_string(), "keyring".to_string()));
            }
        }
        let keyring_error = keyring_result
            .err()
            .unwrap_or_else(|| "keyring_get_failed:empty_refresh_token".to_string());
        self.read_fallback_refresh_token(&session.keyring_account)
            .map(|refresh_token| (refresh_token, "windows_dpapi_file".to_string()))
            .map_err(|fallback_error| format!("{keyring_error};fallback_get_failed:{fallback_error}"))
    }

    fn exchange_custom_token(&self, api_key: &str, custom_token: &str) -> Result<String, String> {
        let url = format!(
            "https://identitytoolkit.googleapis.com/v1/accounts:signInWithCustomToken?key={}",
            api_key
        );
        let payload = self
            .agent()
            .post(&url)
            .set("Content-Type", "application/json")
            .send_json(json!({
                "token": custom_token,
                "returnSecureToken": true
            }))
            .map_err(http_error)?
            .into_json::<Value>()
            .map_err(|e| format!("custom_token_response_failed:{e}"))?;
        let refresh_token = normalize_json_text(payload.get("refreshToken"), 4000);
        if refresh_token.is_empty() {
            return Err("custom_token_exchange_missing_refresh_token".to_string());
        }
        Ok(refresh_token)
    }

    fn refresh_id_token(&self, session: &CloudSyncSession) -> Result<(String, String), String> {
        let (refresh_token, mut credential_storage) = self.read_refresh_token(session)?;
        let url = format!(
            "https://securetoken.googleapis.com/v1/token?key={}",
            session.api_key
        );
        let payload = self
            .agent()
            .post(&url)
            .set("Content-Type", "application/x-www-form-urlencoded")
            .send_form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token.as_str()),
            ])
            .map_err(http_error)?
            .into_json::<Value>()
            .map_err(|e| format!("refresh_token_response_failed:{e}"))?;
        let id_token = normalize_json_text(payload.get("id_token"), 5000);
        let next_refresh = normalize_json_text(payload.get("refresh_token"), 4000);
        if !next_refresh.is_empty() && next_refresh != refresh_token {
            credential_storage = self.store_refresh_token(&session.keyring_account, &next_refresh)?;
        }
        if id_token.is_empty() {
            return Err("refresh_token_missing_id_token".to_string());
        }
        Ok((id_token, credential_storage))
    }

    fn fetch_pending_observations(
        &self,
        session: &CloudSyncSession,
        id_token: &str,
    ) -> Result<Vec<RemoteObservation>, String> {
        let url = format!(
            "https://firestore.googleapis.com/v1/projects/{}/databases/(default)/documents/tenants/{}:runQuery",
            session.project_id,
            session.tenant_id
        );
        let query = json!({
            "structuredQuery": {
                "from": [{ "collectionId": "lessonObservations" }],
                "where": {
                    "fieldFilter": {
                        "field": { "fieldPath": "localSensitiveSyncState" },
                        "op": "EQUAL",
                        "value": { "stringValue": "pending" }
                    }
                },
                "limit": SYNC_LIMIT
            }
        });
        let payload = self
            .agent()
            .post(&url)
            .set("Authorization", &format!("Bearer {id_token}"))
            .set("Content-Type", "application/json")
            .send_json(query)
            .map_err(http_error)?
            .into_json::<Value>()
            .map_err(|e| format!("firestore_query_response_failed:{e}"))?;
        let rows = payload.as_array().cloned().unwrap_or_default();
        let mut out = Vec::new();
        for row in rows {
            let document = match row.get("document").and_then(|v| v.as_object()) {
                Some(document) => document,
                None => continue,
            };
            let name = normalize_json_text(document.get("name"), 1200);
            let update_time = normalize_json_text(document.get("updateTime"), 80);
            let fields = match document.get("fields").and_then(|v| v.as_object()) {
                Some(fields) => fields,
                None => continue,
            };
            let mut payload = decode_firestore_fields(fields);
            let storage_mode = normalize_json_text(payload.get("sensitiveStorageMode"), 80);
            if !is_cloud_sync_storage_mode(&storage_mode) {
                continue;
            }
            let source_type = normalize_json_text(payload.get("sourceType"), 80);
            if matches!(
                source_type.as_str(),
                "checklistMeta" | "checklist" | "migrationState"
            ) {
                continue;
            }
            let doc_id = normalize(name.split('/').last().unwrap_or(""), 240);
            if doc_id.is_empty() {
                continue;
            }
            if let Value::Object(ref mut obj) = payload {
                obj.insert("id".to_string(), Value::String(doc_id.clone()));
                obj.insert("docId".to_string(), Value::String(doc_id.clone()));
                obj.insert(
                    "tenantId".to_string(),
                    Value::String(session.tenant_id.clone()),
                );
                obj.insert(
                    "cloudUpdateTime".to_string(),
                    Value::String(update_time.clone()),
                );
            }
            let updated_at_ms = timestamp_ms(payload.get("updatedAtMs"))
                .max(timestamp_ms(payload.get("updatedAt")))
                .max(
                    DateTime::parse_from_rfc3339(&update_time)
                        .map(|dt| dt.timestamp_millis())
                        .unwrap_or(0),
                );
            out.push(RemoteObservation {
                name,
                update_time,
                doc_id,
                storage_mode,
                payload,
                updated_at_ms,
            });
        }
        Ok(out)
    }

    fn fetch_pending_student_private_details(
        &self,
        session: &CloudSyncSession,
        id_token: &str,
    ) -> Result<Vec<RemoteStudentPrivateDetail>, String> {
        let url = format!(
            "https://firestore.googleapis.com/v1/projects/{}/databases/(default)/documents/tenants/{}:runQuery",
            session.project_id,
            session.tenant_id
        );
        let query = json!({
            "structuredQuery": {
                "from": [{ "collectionId": "studentPrivateDetails" }],
                "where": {
                    "fieldFilter": {
                        "field": { "fieldPath": "localSensitiveSyncState" },
                        "op": "EQUAL",
                        "value": { "stringValue": "pending" }
                    }
                },
                "limit": SYNC_LIMIT
            }
        });
        let payload = self
            .agent()
            .post(&url)
            .set("Authorization", &format!("Bearer {id_token}"))
            .set("Content-Type", "application/json")
            .send_json(query)
            .map_err(http_error)?
            .into_json::<Value>()
            .map_err(|e| format!("firestore_student_private_query_response_failed:{e}"))?;
        let rows = payload.as_array().cloned().unwrap_or_default();
        let mut out = Vec::new();
        for row in rows {
            let document = match row.get("document").and_then(|v| v.as_object()) {
                Some(document) => document,
                None => continue,
            };
            let name = normalize_json_text(document.get("name"), 1200);
            let update_time = normalize_json_text(document.get("updateTime"), 80);
            let fields = match document.get("fields").and_then(|v| v.as_object()) {
                Some(fields) => fields,
                None => continue,
            };
            let mut payload = decode_firestore_fields(fields);
            let storage_mode = normalize_json_text(payload.get("sensitiveStorageMode"), 80);
            if !is_cloud_sync_storage_mode(&storage_mode) {
                continue;
            }
            let doc_id = normalize(name.split('/').last().unwrap_or(""), 80).to_uppercase();
            if doc_id.is_empty() {
                continue;
            }
            if let Value::Object(ref mut obj) = payload {
                obj.insert("id".to_string(), Value::String(doc_id.clone()));
                obj.insert("docId".to_string(), Value::String(doc_id.clone()));
                obj.insert(
                    "tenantId".to_string(),
                    Value::String(session.tenant_id.clone()),
                );
                obj.insert("studentCode".to_string(), Value::String(doc_id.clone()));
                obj.insert(
                    "cloudUpdateTime".to_string(),
                    Value::String(update_time.clone()),
                );
            }
            let updated_at_ms = timestamp_ms(payload.get("updatedAtMs"))
                .max(timestamp_ms(payload.get("updatedAt")))
                .max(
                    DateTime::parse_from_rfc3339(&update_time)
                        .map(|dt| dt.timestamp_millis())
                        .unwrap_or(0),
                );
            out.push(RemoteStudentPrivateDetail {
                name,
                update_time,
                student_code: doc_id,
                storage_mode,
                payload,
                updated_at_ms,
            });
        }
        Ok(out)
    }

    fn delete_remote_document(
        &self,
        name: &str,
        update_time: &str,
        id_token: &str,
    ) -> Result<(), String> {
        let mut url = Url::parse(&format!("https://firestore.googleapis.com/v1/{}", name))
            .map_err(|e| format!("firestore_delete_url_failed:{e}"))?;
        if !update_time.is_empty() {
            url.query_pairs_mut()
                .append_pair("currentDocument.updateTime", update_time);
        }
        match self
            .agent()
            .delete(url.as_str())
            .set("Authorization", &format!("Bearer {id_token}"))
            .call()
        {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(404, _)) => Ok(()),
            Err(error) => Err(http_error(error)),
        }
    }

    fn patch_remote_document(
        &self,
        name: &str,
        update_time: &str,
        fields: Map<String, Value>,
        id_token: &str,
    ) -> Result<(), String> {
        if update_time.is_empty() {
            return Err("firestore_patch_missing_update_time".to_string());
        }
        let mut url = Url::parse(&format!("https://firestore.googleapis.com/v1/{}", name))
            .map_err(|e| format!("firestore_patch_url_failed:{e}"))?;
        {
            let mut query = url.query_pairs_mut();
            for field_path in fields.keys() {
                query.append_pair("updateMask.fieldPaths", field_path);
            }
            query.append_pair("currentDocument.updateTime", update_time);
        }
        match self
            .agent()
            .request("PATCH", url.as_str())
            .set("Authorization", &format!("Bearer {id_token}"))
            .set("Content-Type", "application/json")
            .send_json(json!({ "fields": fields }))
        {
            Ok(_) => Ok(()),
            Err(error) => Err(http_error(error)),
        }
    }

    fn mark_remote_document_import_state(
        &self,
        name: &str,
        update_time: &str,
        state_value: &str,
        session: &CloudSyncSession,
        run_id: &str,
        id_token: &str,
    ) -> Result<(), String> {
        let now = Utc::now();
        let mut fields = Map::new();
        fields.insert(
            "localSensitiveSyncState".to_string(),
            firestore_string_field(state_value),
        );
        fields.insert(
            "localSensitiveImportRunId".to_string(),
            firestore_string_field(run_id),
        );
        fields.insert(
            "localSensitiveImportSourceUpdateTime".to_string(),
            firestore_string_field(update_time),
        );
        fields.insert(
            "localSensitiveSyncUpdatedAt".to_string(),
            firestore_timestamp_field(&now),
        );
        if state_value == "imported_to_local" {
            fields.insert(
                "localSensitiveImportedAtMs".to_string(),
                firestore_integer_field(now.timestamp_millis()),
            );
            fields.insert(
                "localSensitiveImportedAt".to_string(),
                firestore_timestamp_field(&now),
            );
            fields.insert(
                "localSensitiveImportedByUid".to_string(),
                firestore_string_field(session.uid.clone()),
            );
            fields.insert(
                "localSensitiveImportedByEmail".to_string(),
                firestore_string_field(session.account_email.clone()),
            );
        } else {
            fields.insert(
                "localSensitiveConflictAtMs".to_string(),
                firestore_integer_field(now.timestamp_millis()),
            );
            fields.insert(
                "localSensitiveConflictAt".to_string(),
                firestore_timestamp_field(&now),
            );
            fields.insert(
                "localSensitiveConflictByUid".to_string(),
                firestore_string_field(session.uid.clone()),
            );
            fields.insert(
                "localSensitiveConflictByEmail".to_string(),
                firestore_string_field(session.account_email.clone()),
            );
        }
        self.patch_remote_document(name, update_time, fields, id_token)
    }

    fn delete_remote_observation(
        &self,
        remote: &RemoteObservation,
        id_token: &str,
    ) -> Result<(), String> {
        self.delete_remote_document(&remote.name, &remote.update_time, id_token)
    }

    fn delete_remote_student_private_detail(
        &self,
        remote: &RemoteStudentPrivateDetail,
        id_token: &str,
    ) -> Result<(), String> {
        self.delete_remote_document(&remote.name, &remote.update_time, id_token)
    }

    pub(crate) fn connect(&self, input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        let project_id = normalize_json_text(input.get("projectId"), 200);
        let api_key = normalize_json_text(input.get("apiKey"), 240);
        let uid = normalize_json_text(input.get("uid"), 200);
        let custom_token = normalize_json_text(input.get("customToken"), 5000);
        let account_email = normalize_json_text(
            input.get("accountEmail").or_else(|| input.get("email")),
            320,
        );
        let account_display_name = normalize_json_text(input.get("accountDisplayName"), 120);
        let tenant_name = normalize_json_text(input.get("tenantName"), 180);
        let observation_storage_mode =
            normalize_observation_storage_mode(input.get("observationStorageMode"));
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        if project_id.is_empty() || api_key.is_empty() || uid.is_empty() || custom_token.is_empty()
        {
            return Err("cloud_sync_session_required".to_string());
        }

        let account = keyring_account(&tenant_id, &uid);
        let refresh_token = self.exchange_custom_token(&api_key, &custom_token)?;
        let credential_storage = self.store_refresh_token(&account, &refresh_token)?;

        let session = CloudSyncSession {
            tenant_id,
            project_id,
            api_key,
            uid,
            account_email,
            account_display_name,
            tenant_name,
            observation_storage_mode,
            keyring_account: account,
            credential_storage,
            connected_at_ms: now_ms(),
            ..CloudSyncSession::default()
        };
        self.save_session(&session)?;
        self.run_once()
    }

    pub(crate) fn status(&self) -> Result<Value, String> {
        let session = self.load_session()?;
        Ok(status_from_session(session.as_ref()))
    }

    pub(crate) fn disconnect(&self) -> Result<Value, String> {
        if let Some(session) = self.load_session()? {
            let _ = keyring_entry(&session.keyring_account).and_then(|entry| {
                entry
                    .delete_credential()
                    .map_err(|e| format!("keyring_delete_failed:{e}"))
            });
            let _ = self.delete_fallback_refresh_token(&session.keyring_account);
        }
        if self.session_path.exists() {
            fs::remove_file(&self.session_path)
                .map_err(|e| format!("cloud_session_delete_failed:{e}"))?;
        }
        Ok(status_from_session(None))
    }

    pub(crate) fn run_once(&self) -> Result<Value, String> {
        let _guard = self
            .sync_lock
            .lock()
            .map_err(|_| "cloud_sync_lock_failed".to_string())?;
        let mut session = match self.load_session()? {
            Some(session) => session,
            None => return Ok(status_from_session(None)),
        };
        session.last_run_at_ms = now_ms();
        session.last_error.clear();
        session.last_imported = 0;
        session.last_deleted = 0;
        session.last_marked = 0;
        session.last_pending = 0;
        session.last_failed = 0;
        session.last_conflicts = 0;
        let run_id = format!("{}", session.last_run_at_ms);
        let mut observation_pending = 0i64;
        let mut observation_imported = 0i64;
        let mut observation_deleted = 0i64;
        let mut observation_marked = 0i64;
        let mut observation_conflicts = 0i64;
        let mut student_private_detail_pending = 0i64;
        let mut student_private_detail_imported = 0i64;
        let mut student_private_detail_deleted = 0i64;
        let mut student_private_detail_marked = 0i64;
        let mut student_private_detail_conflicts = 0i64;

        let result = (|| -> Result<(), String> {
            let (id_token, credential_storage) = self.refresh_id_token(&session)?;
            session.credential_storage = credential_storage;
            let pending_observations = self.fetch_pending_observations(&session, &id_token)?;
            let pending_student_private_details =
                self.fetch_pending_student_private_details(&session, &id_token)?;
            observation_pending = pending_observations.len() as i64;
            student_private_detail_pending = pending_student_private_details.len() as i64;
            session.last_pending =
                (pending_observations.len() + pending_student_private_details.len()) as i64;
            for remote in pending_observations {
                let local_updated = self
                    .store
                    .get_observation_updated_at_ms(&session.tenant_id, &remote.doc_id)?;
                let mut remote_state = "imported_to_local";
                if local_updated.unwrap_or(0) > remote.updated_at_ms && remote.updated_at_ms > 0 {
                    self.store.store_observation_conflict(
                        &session.tenant_id,
                        &remote.doc_id,
                        &remote.update_time,
                        remote.payload.clone(),
                    )?;
                    session.last_conflicts += 1;
                    observation_conflicts += 1;
                    remote_state = "local_conflict";
                } else {
                    self.store.upsert_observation(remote.payload.clone())?;
                    session.last_imported += 1;
                    observation_imported += 1;
                }
                let remote_action = if remote_state == "local_conflict"
                    || keeps_remote_after_import(&remote.storage_mode)
                {
                    self.mark_remote_document_import_state(
                        &remote.name,
                        &remote.update_time,
                        remote_state,
                        &session,
                        &run_id,
                        &id_token,
                    )
                } else {
                    self.delete_remote_observation(&remote, &id_token)
                };
                match remote_action {
                    Ok(()) => {
                        if remote_state == "local_conflict"
                            || keeps_remote_after_import(&remote.storage_mode)
                        {
                            session.last_marked += 1;
                            observation_marked += 1;
                        } else {
                            session.last_deleted += 1;
                            observation_deleted += 1;
                        }
                    }
                    Err(error) => {
                        session.last_failed += 1;
                        session.last_error = error;
                    }
                }
            }
            for remote in pending_student_private_details {
                let local_updated = self.store.get_student_private_detail_updated_at_ms(
                    &session.tenant_id,
                    &remote.student_code,
                )?;
                let mut remote_state = "imported_to_local";
                if local_updated.unwrap_or(0) > remote.updated_at_ms && remote.updated_at_ms > 0 {
                    self.store.store_student_private_detail_conflict(
                        &session.tenant_id,
                        &remote.student_code,
                        &remote.update_time,
                        remote.payload.clone(),
                    )?;
                    session.last_conflicts += 1;
                    student_private_detail_conflicts += 1;
                    remote_state = "local_conflict";
                } else {
                    self.store
                        .upsert_student_private_detail(remote.payload.clone())?;
                    session.last_imported += 1;
                    student_private_detail_imported += 1;
                }
                let remote_action = if remote_state == "local_conflict"
                    || keeps_remote_after_import(&remote.storage_mode)
                {
                    self.mark_remote_document_import_state(
                        &remote.name,
                        &remote.update_time,
                        remote_state,
                        &session,
                        &run_id,
                        &id_token,
                    )
                } else {
                    self.delete_remote_student_private_detail(&remote, &id_token)
                };
                match remote_action {
                    Ok(()) => {
                        if remote_state == "local_conflict"
                            || keeps_remote_after_import(&remote.storage_mode)
                        {
                            session.last_marked += 1;
                            student_private_detail_marked += 1;
                        } else {
                            session.last_deleted += 1;
                            student_private_detail_deleted += 1;
                        }
                    }
                    Err(error) => {
                        session.last_failed += 1;
                        session.last_error = error;
                    }
                }
            }
            session.last_sync_at_ms = now_ms();
            Ok(())
        })();

        if let Err(error) = result {
            session.last_error = error;
            session.last_failed += 1;
        }
        let finished_at_ms = now_ms();
        let processed_remote = session.last_deleted + session.last_marked;
        let receipt_status = if session.last_failed > 0 && processed_remote > 0 {
            "partial_failed"
        } else if session.last_failed > 0 {
            "failed"
        } else if session.last_conflicts > 0 {
            "completed_with_conflicts"
        } else {
            "completed"
        };
        let _ = self.store.record_cloud_sync_run(json!({
            "ok": true,
            "runId": run_id,
            "tenantId": session.tenant_id.clone(),
            "uid": session.uid.clone(),
            "startedAtMs": session.last_run_at_ms,
            "finishedAtMs": finished_at_ms,
            "status": receipt_status,
            "pending": session.last_pending,
            "imported": session.last_imported,
            "deleted": session.last_deleted,
            "marked": session.last_marked,
            "failed": session.last_failed,
            "conflicts": session.last_conflicts,
            "lastError": session.last_error.clone(),
            "remoteAction": "delete_or_mark_imported_to_local_or_local_conflict",
            "deleteConfirmation": "firestore_delete_success_or_already_missing",
            "deletePrecondition": "currentDocument.updateTime",
            "markConfirmation": "firestore_patch_imported_to_local_or_local_conflict",
            "markPrecondition": "currentDocument.updateTime",
            "observationPending": observation_pending,
            "observationImported": observation_imported,
            "observationDeleted": observation_deleted,
            "observationMarked": observation_marked,
            "observationConflicts": observation_conflicts,
            "studentPrivateDetailPending": student_private_detail_pending,
            "studentPrivateDetailImported": student_private_detail_imported,
            "studentPrivateDetailDeleted": student_private_detail_deleted,
            "studentPrivateDetailMarked": student_private_detail_marked,
            "studentPrivateDetailConflicts": student_private_detail_conflicts
        }));
        self.save_session(&session)?;
        Ok(status_from_session(Some(&session)))
    }
}
