use crate::device_sync_credential::DeviceSyncCredentialStore;
use crate::{backup, local_arch, local_os_name, local_pc_name, normalize_json_text, SqliteStore};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use url::Url;

const SESSION_FILE_NAME: &str = "device-sync-session.json";
const DEFAULT_API_ROOT: &str = "https://t.classaimate.com/api/v3/local-store-sync";
const IDLE_PUBLISH_MS: i64 = 30 * 1000;
const MAX_DIRTY_MS: i64 = 5 * 60 * 1000;
const SAFETY_CHECK_MS: i64 = 6 * 60 * 60 * 1000;
const BACKGROUND_TICK_SECS: u64 = 15;
const SNAPSHOT_FORMAT_MAX: i64 = 5;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
struct DeviceSyncSession {
    version: i64,
    tenant_id: String,
    uid: String,
    device_id: String,
    device_name: String,
    platform_label: String,
    app_version: String,
    keyring_account: String,
    credential_storage: String,
    connected_at_ms: i64,
}

pub(crate) struct DeviceSyncManager {
    session_path: PathBuf,
    credential_store: DeviceSyncCredentialStore,
    store: Arc<SqliteStore>,
    sync_lock: Mutex<()>,
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn keyring_account(tenant_id: &str, device_id: &str) -> String {
    format!("device-sync:{tenant_id}:{device_id}")
}

fn valid_identifier(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '-')
        })
}

fn validated_api_root(value: &str) -> Option<String> {
    let parsed = Url::parse(value.trim()).ok()?;
    let host = parsed.host_str()?;
    let allowed_origin = (parsed.scheme() == "https" && host == "t.classaimate.com")
        || (parsed.scheme() == "http" && matches!(host, "localhost" | "127.0.0.1"));
    if !allowed_origin || parsed.query().is_some() || parsed.fragment().is_some() {
        return None;
    }
    Some(parsed.to_string().trim_end_matches('/').to_string())
}

fn api_root() -> String {
    env::var("ONLINECLASS_DEVICE_SYNC_API_URL")
        .ok()
        .and_then(|value| validated_api_root(&value))
        .unwrap_or_else(|| DEFAULT_API_ROOT.to_string())
}

fn response_data(response: Result<ureq::Response, ureq::Error>) -> Result<Value, String> {
    let payload = match response {
        Ok(response) => response
            .into_json::<Value>()
            .map_err(|e| format!("device_sync_decode_failed:{e}"))?,
        Err(ureq::Error::Status(status, response)) => {
            let payload = response.into_json::<Value>().unwrap_or_else(|_| json!({}));
            let code = payload
                .pointer("/error/code")
                .and_then(Value::as_str)
                .or_else(|| payload.get("error").and_then(Value::as_str))
                .unwrap_or("request_failed");
            return Err(format!("device_sync_http_{status}:{code}"));
        }
        Err(ureq::Error::Transport(error)) => {
            return Err(format!("device_sync_network_failed:{error}"));
        }
    };
    if payload.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err("device_sync_response_invalid".to_string());
    }
    payload
        .get("data")
        .cloned()
        .ok_or_else(|| "device_sync_response_invalid".to_string())
}

fn checkpoint_generation(checkpoint: Option<&Value>) -> i64 {
    checkpoint
        .and_then(|value| value.get("generation"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

fn checkpoint_status(checkpoint: Option<&Value>) -> String {
    checkpoint
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

impl DeviceSyncManager {
    pub(crate) fn new(data_dir: PathBuf, store: Arc<SqliteStore>) -> Self {
        Self {
            session_path: data_dir.join(SESSION_FILE_NAME),
            credential_store: DeviceSyncCredentialStore::new(data_dir),
            store,
            sync_lock: Mutex::new(()),
        }
    }

    fn agent(&self) -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(12))
            .timeout_write(Duration::from_secs(12))
            .build()
    }

    fn load_session(&self) -> Result<Option<DeviceSyncSession>, String> {
        if !self.session_path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(&self.session_path)
            .map_err(|e| format!("device_sync_session_read_failed:{e}"))?;
        let session = serde_json::from_str::<DeviceSyncSession>(&raw)
            .map_err(|e| format!("device_sync_session_decode_failed:{e}"))?;
        if session.version != 1
            || !valid_identifier(&session.tenant_id, 128)
            || !valid_identifier(&session.device_id, 128)
            || session.keyring_account.is_empty()
        {
            return Err("device_sync_session_invalid".to_string());
        }
        Ok(Some(session))
    }

    fn save_session(&self, session: &DeviceSyncSession) -> Result<(), String> {
        if let Some(parent) = self.session_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("device_sync_session_dir_failed:{e}"))?;
        }
        let raw = serde_json::to_string_pretty(session)
            .map_err(|e| format!("device_sync_session_encode_failed:{e}"))?;
        let temporary = self.session_path.with_extension("json.tmp");
        fs::write(&temporary, format!("{raw}\n"))
            .map_err(|e| format!("device_sync_session_write_failed:{e}"))?;
        fs::rename(&temporary, &self.session_path)
            .map_err(|e| format!("device_sync_session_commit_failed:{e}"))
    }

    fn credential(&self, session: &DeviceSyncSession) -> Result<String, String> {
        self.credential_store.read(&session.keyring_account)
    }

    fn authorized_get(
        &self,
        session: &DeviceSyncSession,
        credential: &str,
        path: &str,
    ) -> Result<Value, String> {
        response_data(
            self.agent()
                .get(&format!("{}{path}", api_root()))
                .set("Authorization", &format!("Bearer {credential}"))
                .set("X-Local-Store-Device-Id", &session.device_id)
                .set("X-Local-Store-Snapshot-Max", &SNAPSHOT_FORMAT_MAX.to_string())
                .call(),
        )
    }

    fn authorized_post(
        &self,
        session: &DeviceSyncSession,
        credential: &str,
        path: &str,
        body: Value,
    ) -> Result<Value, String> {
        response_data(
            self.agent()
                .post(&format!("{}{path}", api_root()))
                .set("Authorization", &format!("Bearer {credential}"))
                .set("X-Local-Store-Device-Id", &session.device_id)
                .set("X-Local-Store-Snapshot-Max", &SNAPSHOT_FORMAT_MAX.to_string())
                .send_json(body),
        )
    }

    fn authorized_delete(
        &self,
        session: &DeviceSyncSession,
        credential: &str,
        path: &str,
    ) -> Result<Value, String> {
        response_data(
            self.agent()
                .delete(&format!("{}{path}", api_root()))
                .set("Authorization", &format!("Bearer {credential}"))
                .set("X-Local-Store-Device-Id", &session.device_id)
                .set("X-Local-Store-Snapshot-Max", &SNAPSHOT_FORMAT_MAX.to_string())
                .call(),
        )
    }

    fn latest_checkpoint(
        &self,
        session: &DeviceSyncSession,
        credential: &str,
    ) -> Result<(Option<Value>, i64), String> {
        let data = self.authorized_get(session, credential, "/checkpoints/latest")?;
        let checkpoint = data.get("checkpoint").cloned().filter(|value| !value.is_null());
        let snapshot_version = data
            .pointer("/snapshotPolicy/maxWritableSnapshotVersion")
            .and_then(Value::as_i64)
            .unwrap_or(4)
            .clamp(4, SNAPSHOT_FORMAT_MAX);
        Ok((checkpoint, snapshot_version))
    }

    fn verified_snapshot(
        &self,
        tenant_id: &str,
        artifact_generation: i64,
        artifact_root: &str,
        database_sha256: &str,
    ) -> Result<Value, String> {
        let mut last_error = String::new();
        for delay in [0u64, 1, 2, 4, 8] {
            if delay > 0 {
                thread::sleep(Duration::from_secs(delay));
            }
            match backup::find_and_verify_generation(
                &self.store,
                tenant_id,
                artifact_generation,
                artifact_root,
            ) {
                Ok(Some(snapshot)) => {
                    if snapshot.get("databaseSha256").and_then(Value::as_str)
                        != Some(database_sha256)
                    {
                        last_error = "backup_database_checkpoint_mismatch".to_string();
                        continue;
                    }
                    return Ok(snapshot);
                }
                Ok(None) => last_error = "onedrive_snapshot_pending".to_string(),
                Err(error) => last_error = error,
            }
        }
        Err(if last_error.is_empty() {
            "onedrive_snapshot_pending".to_string()
        } else {
            last_error
        })
    }

    fn apply_checkpoint(
        &self,
        session: &DeviceSyncSession,
        credential: &str,
        checkpoint: &Value,
    ) -> Result<(), String> {
        let generation = checkpoint_generation(Some(checkpoint));
        let state = backup::local_sync_state(&self.store, &session.tenant_id)?;
        let artifact_root =
            normalize_json_text(checkpoint.get("artifactSetSha256"), 64).to_lowercase();
        let database_sha256 =
            normalize_json_text(checkpoint.get("databaseSha256"), 64).to_lowercase();
        let source_device_id = normalize_json_text(checkpoint.get("sourceDeviceId"), 128);
        let recovery_generation = checkpoint
            .get("recoveryOfGeneration")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0);
        let artifact_generation = recovery_generation.unwrap_or(generation);
        if generation < 1
            || artifact_root.len() != 64
            || database_sha256.len() != 64
            || !valid_identifier(&source_device_id, 128)
        {
            return Err("device_sync_checkpoint_invalid".to_string());
        }
        let status = checkpoint_status(Some(checkpoint));
        if generation <= state.applied_generation && source_device_id == session.device_id {
            return Ok(());
        }
        let snapshot = self.verified_snapshot(
            &session.tenant_id,
            artifact_generation,
            &artifact_root,
            &database_sha256,
        )?;
        if generation <= state.applied_generation {
            let manifest_path = PathBuf::from(
                snapshot
                    .get("manifestPath")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "backup_manifest_required".to_string())?,
            );
            backup::restore_generation(
                &self.store,
                &session.tenant_id,
                &manifest_path,
                generation,
                &status,
                recovery_generation.is_some(),
            )?;
            let acknowledged = self.authorized_post(
                session,
                credential,
                &format!("/checkpoints/{generation}/acks"),
                json!({ "artifactSetSha256": artifact_root }),
            )?;
            backup::mark_sync_latest(
                &self.store,
                &session.tenant_id,
                generation,
                acknowledged
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("verified"),
            )?;
            return Ok(());
        }
        let manifest_path = PathBuf::from(
            snapshot
                .get("manifestPath")
                .and_then(Value::as_str)
                .ok_or_else(|| "backup_manifest_required".to_string())?,
        );
        let content_sha256 = normalize_json_text(snapshot.get("contentSha256"), 64);
        if source_device_id == session.device_id && recovery_generation.is_none() {
            backup::mark_sync_published(
                &self.store,
                &session.tenant_id,
                generation,
                &manifest_path,
                &content_sha256,
                &status,
            )?;
            return Ok(());
        }
        backup::restore_generation(
            &self.store,
            &session.tenant_id,
            &manifest_path,
            generation,
            &status,
            recovery_generation.is_some(),
        )?;
        backup::mark_sync_applied_content(&self.store, &session.tenant_id, &content_sha256)?;
        if source_device_id != session.device_id {
            let acknowledged = self.authorized_post(
                session,
                credential,
                &format!("/checkpoints/{generation}/acks"),
                json!({ "artifactSetSha256": artifact_root }),
            )?;
            backup::mark_sync_latest(
                &self.store,
                &session.tenant_id,
                generation,
                acknowledged
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("verified"),
            )?;
        }
        Ok(())
    }

    fn publish(
        &self,
        session: &DeviceSyncSession,
        credential: &str,
        base_generation: i64,
        latest_status: &str,
        snapshot_version: i64,
    ) -> Result<(), String> {
        let content_sha256 = backup::tenant_content_sha256(&self.store, &session.tenant_id)?;
        let state = backup::local_sync_state(&self.store, &session.tenant_id)?;
        if !state.last_content_sha256.is_empty() && state.last_content_sha256 == content_sha256 {
            backup::mark_sync_unchanged(&self.store, &session.tenant_id, base_generation)?;
            return Ok(());
        }
        let generation = base_generation + 1;
        let snapshot = backup::run_with_kind_version(
            &self.store,
            session.tenant_id.clone(),
            "auto_sync",
            Some(generation),
            snapshot_version,
        )?;
        if snapshot.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err("device_sync_snapshot_incomplete".to_string());
        }
        let artifact_root = snapshot
            .get("artifactSetSha256")
            .and_then(Value::as_str)
            .ok_or_else(|| "device_sync_snapshot_invalid".to_string())?;
        let database_sha256 = snapshot
            .get("databaseSha256")
            .and_then(Value::as_str)
            .ok_or_else(|| "device_sync_snapshot_invalid".to_string())?;
        let checkpoint = self.authorized_post(
            session,
            credential,
            "/checkpoints",
            json!({
                "baseGeneration": base_generation,
                "artifactSetSha256": artifact_root,
                "databaseSha256": database_sha256,
                "snapshotVersion": snapshot_version,
            }),
        )?;
        let manifest_path = Path::new(
            snapshot
                .get("manifestPath")
                .and_then(Value::as_str)
                .ok_or_else(|| "backup_manifest_required".to_string())?,
        );
        backup::mark_sync_published(
            &self.store,
            &session.tenant_id,
            generation,
            manifest_path,
            &content_sha256,
            checkpoint
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or(latest_status),
        )
    }

    fn run_locked(&self, force_publish: bool) -> Result<Value, String> {
        let session = match self.load_session()? {
            Some(session) => session,
            None => return Ok(json!({ "ok": true, "connected": false })),
        };
        let credential = self.credential(&session)?;
        let _ = backup::auto_configure_onedrive(&self.store, &session.tenant_id)?;
        let (mut latest, mut snapshot_version) = self.latest_checkpoint(&session, &credential)?;
        backup::mark_sync_latest(
            &self.store,
            &session.tenant_id,
            checkpoint_generation(latest.as_ref()),
            &checkpoint_status(latest.as_ref()),
        )?;
        if let Some(checkpoint) = latest.as_ref() {
            self.apply_checkpoint(&session, &credential, checkpoint)?;
        }
        let state = backup::local_sync_state(&self.store, &session.tenant_id)?;
        let now = now_ms();
        let publish_due = force_publish
            || (state.first_dirty_at_ms > 0
                && (now.saturating_sub(state.last_dirty_at_ms) >= IDLE_PUBLISH_MS
                    || now.saturating_sub(state.first_dirty_at_ms) >= MAX_DIRTY_MS));
        if publish_due {
            let base_generation = checkpoint_generation(latest.as_ref());
            let status = checkpoint_status(latest.as_ref());
            match self.publish(&session, &credential, base_generation, &status, snapshot_version) {
                Ok(()) => {}
                Err(error) if error.starts_with("device_sync_http_409:") => {
                    (latest, snapshot_version) = self.latest_checkpoint(&session, &credential)?;
                    if let Some(checkpoint) = latest.as_ref() {
                        self.apply_checkpoint(&session, &credential, checkpoint)?;
                    }
                    let rebased = backup::local_sync_state(&self.store, &session.tenant_id)?;
                    if rebased.first_dirty_at_ms > 0 {
                        self.publish(
                            &session,
                            &credential,
                            checkpoint_generation(latest.as_ref()),
                            &checkpoint_status(latest.as_ref()),
                            snapshot_version,
                        )?;
                    }
                }
                Err(error) => return Err(error),
            }
        }
        self.status()
    }

    pub(crate) fn connect_from_authorization(&self, input: &Value) -> Result<Value, String> {
        let tenant_id = normalize_json_text(input.get("tenantId"), 128);
        let uid = normalize_json_text(input.get("uid"), 160);
        let device = input
            .get("syncDevice")
            .and_then(Value::as_object)
            .ok_or_else(|| "device_sync_credential_required".to_string())?;
        let device_id = normalize_json_text(device.get("deviceId"), 128);
        let credential = normalize_json_text(device.get("credential"), 80);
        if !valid_identifier(&tenant_id, 128)
            || !valid_identifier(&device_id, 128)
            || credential.len() != 43
        {
            return Err("device_sync_credential_invalid".to_string());
        }
        let previous = self.load_session()?;
        let account = keyring_account(&tenant_id, &device_id);
        let device_id_for_revoke = device_id.clone();
        let credential_storage = match self.credential_store.store(&account, &credential) {
            Ok(storage) => storage,
            Err(error) => {
                let provisional = DeviceSyncSession {
                    device_id: device_id_for_revoke,
                    ..DeviceSyncSession::default()
                };
                let _ = self.authorized_delete(&provisional, &credential, "/devices/current");
                return Err(error);
            }
        };
        let session = DeviceSyncSession {
            version: 1,
            tenant_id: tenant_id.clone(),
            uid,
            device_id,
            device_name: local_pc_name(),
            platform_label: format!("{} {}", local_os_name(), local_arch())
                .trim()
                .to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            keyring_account: account,
            credential_storage,
            connected_at_ms: now_ms(),
        };
        if let Err(error) = self.save_session(&session) {
            let _ = self.authorized_delete(&session, &credential, "/devices/current");
            self.credential_store.delete(&session.keyring_account);
            return Err(error);
        }
        if let Some(previous) = previous {
            if previous.keyring_account != session.keyring_account {
                self.credential_store.delete(&previous.keyring_account);
            }
        }
        let _ = backup::auto_configure_onedrive(&self.store, &tenant_id)?;
        self.status()
    }

    pub(crate) fn status(&self) -> Result<Value, String> {
        let session = match self.load_session()? {
            Some(session) => session,
            None => return Ok(json!({ "ok": true, "connected": false })),
        };
        let state = backup::local_sync_state(&self.store, &session.tenant_id)?;
        let backup_status = backup::status(&self.store, session.tenant_id.clone())?;
        Ok(json!({
            "ok": true,
            "connected": true,
            "tenantId": session.tenant_id,
            "uid": session.uid,
            "deviceId": session.device_id,
            "deviceName": session.device_name,
            "platformLabel": session.platform_label,
            "appVersion": session.app_version,
            "credentialStorage": session.credential_storage,
            "credentialAvailable": self.credential(&session).is_ok(),
            "connectedAtMs": session.connected_at_ms,
            "oneDriveConfigured": backup_status.get("configured").and_then(Value::as_bool).unwrap_or(false),
            "appliedGeneration": state.applied_generation,
            "publishedGeneration": state.published_generation,
            "latestGeneration": state.latest_generation,
            "latestStatus": state.latest_status,
            "hasUnsyncedChanges": state.first_dirty_at_ms > 0,
            "firstDirtyAtMs": state.first_dirty_at_ms,
            "lastDirtyAtMs": state.last_dirty_at_ms,
            "lastCheckedAtMs": state.last_checked_at_ms,
            "lastSuccessAtMs": state.last_success_at_ms,
            "lastError": state.last_error,
            "conflictCount": state.conflict_count,
            "conflictRetainedCount": state.conflict_count,
            "conflictUnreviewedCount": state.conflict_unreviewed_count,
            "conflictLifetimeCount": state.conflict_lifetime_count,
            "waitingForOneDrive": state.latest_generation > state.applied_generation,
        }))
    }

    pub(crate) fn run_once(&self, force_publish: bool) -> Result<Value, String> {
        let _guard = self
            .sync_lock
            .lock()
            .map_err(|_| "device_sync_lock_failed".to_string())?;
        let tenant_id = self
            .load_session()?
            .as_ref()
            .map(|session| session.tenant_id.clone())
            .unwrap_or_default();
        match self.run_locked(force_publish) {
            Ok(value) => Ok(value),
            Err(error) => {
                if !tenant_id.is_empty() {
                    backup::mark_sync_error(&self.store, &tenant_id, &error);
                }
                Err(error)
            }
        }
    }

    pub(crate) fn disconnect(&self) -> Result<Value, String> {
        let session = self.load_session()?;
        let mut revoke_error = None;
        if let Some(session) = session.as_ref() {
            match self.credential(session) {
                Ok(credential) => {
                    if let Err(error) =
                        self.authorized_delete(session, &credential, "/devices/current")
                    {
                        revoke_error = Some(error);
                    }
                }
                Err(error) => revoke_error = Some(error),
            }
            self.credential_store.delete(&session.keyring_account);
        }
        if self.session_path.exists() {
            fs::remove_file(&self.session_path)
                .map_err(|e| format!("device_sync_session_delete_failed:{e}"))?;
        }
        match revoke_error {
            Some(error) => Err(format!("device_sync_revoke_unconfirmed:{error}")),
            None => Ok(json!({ "ok": true, "connected": false, "credentialRevoked": true })),
        }
    }

    pub(crate) fn start_background(self: &Arc<Self>) {
        let manager = Arc::clone(self);
        thread::spawn(move || {
            let _ = manager.run_once(true);
            let mut last_tick = now_ms();
            loop {
                thread::sleep(Duration::from_secs(BACKGROUND_TICK_SECS));
                let current = now_ms();
                let resumed =
                    current.saturating_sub(last_tick) > (BACKGROUND_TICK_SECS as i64 + 45) * 1000;
                last_tick = current;
                let session = match manager.load_session() {
                    Ok(Some(session)) => session,
                    _ => continue,
                };
                let state = match backup::local_sync_state(&manager.store, &session.tenant_id) {
                    Ok(state) => state,
                    Err(_) => continue,
                };
                let local_commit_ahead =
                    backup::highest_local_generation(&manager.store, &session.tenant_id)
                        .map(|generation| generation > state.applied_generation)
                        .unwrap_or(false);
                let publish_due = state.first_dirty_at_ms > 0
                    && (current.saturating_sub(state.last_dirty_at_ms) >= IDLE_PUBLISH_MS
                        || current.saturating_sub(state.first_dirty_at_ms) >= MAX_DIRTY_MS);
                let safety_due = state.last_checked_at_ms == 0
                    || current.saturating_sub(state.last_checked_at_ms) >= SAFETY_CHECK_MS;
                if resumed || local_commit_ahead || publish_due || safety_due {
                    let _ = manager.run_once(false);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_sync_api_root_rejects_non_classaimate_remote_hosts() {
        assert!(DEFAULT_API_ROOT.starts_with("https://t.classaimate.com/"));
        assert_eq!(
            validated_api_root("https://t.classaimate.com/api/v3/local-store-sync"),
            Some("https://t.classaimate.com/api/v3/local-store-sync".to_string())
        );
        assert!(validated_api_root("https://evil.example/api?next=t.classaimate.com").is_none());
        assert!(validated_api_root("https://t.classaimate.com/api#fragment").is_none());
        assert!(validated_api_root("http://127.0.0.1:8793/api/v3/local-store-sync").is_some());
        assert!(valid_identifier("tenant-a", 128));
        assert!(!valid_identifier("tenant/a", 128));
    }

    #[test]
    fn checkpoint_helpers_keep_server_generation_and_status() {
        let checkpoint = json!({ "generation": 7, "status": "verified" });
        assert_eq!(checkpoint_generation(Some(&checkpoint)), 7);
        assert_eq!(checkpoint_status(Some(&checkpoint)), "verified");
        assert_eq!(checkpoint_generation(None), 0);
    }
}
