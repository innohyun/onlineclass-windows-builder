use super::*;
use std::io::Write;

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SessionRegistry {
    pub(super) version: i64,
    pub(super) selected_tenant_id: String,
    pub(super) sessions: BTreeMap<String, DeviceSyncSession>,
}

fn validate(session: &DeviceSyncSession) -> Result<(), String> {
    if session.version != 1
        || !valid_identifier(&session.tenant_id, 128)
        || !valid_identifier(&session.device_id, 128)
        || session.keyring_account.is_empty()
    {
        return Err("device_sync_session_invalid".into());
    }
    Ok(())
}

impl DeviceSyncManager {
    pub(crate) fn for_tenant(&self, tenant: &str) -> Result<Self, String> {
        {
            let _guard = self
                .session_lock
                .lock()
                .map_err(|_| "device_sync_session_lock_failed")?;
            let _file_guard = self.registry_file_lock()?;
            if !self.load_registry()?.sessions.contains_key(tenant) {
                return Err("device_sync_session_required".into());
            }
        }
        self.scoped_for_authorization(tenant)
    }

    pub(super) fn scoped_for_authorization(&self, tenant: &str) -> Result<Self, String> {
        self.scoped_with_store(tenant, self.store.for_tenant(tenant)?)
    }

    fn scoped_with_store(&self, tenant: &str, store: Arc<SqliteStore>) -> Result<Self, String> {
        if !valid_identifier(tenant, 128) {
            return Err("tenant_scope_invalid".into());
        }
        let lock = self
            .tenant_locks
            .lock()
            .map_err(|_| "device_sync_lock_failed")?
            .entry(tenant.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        Ok(Self {
            session_path: self.session_path.clone(),
            credential_store: DeviceSyncCredentialStore::new(
                self.session_path
                    .parent()
                    .ok_or("device_sync_session_dir_required")?
                    .to_path_buf(),
            ),
            store,
            sync_lock: lock,
            session_lock: Arc::clone(&self.session_lock),
            tenant_locks: Arc::clone(&self.tenant_locks),
            scoped_tenant: Some(tenant.to_string()),
            #[cfg(test)]
            test_api_root: self.test_api_root.clone(),
            #[cfg(test)]
            test_skip_retry_delay: self.test_skip_retry_delay,
        })
    }

    pub(super) fn load_session(&self) -> Result<Option<DeviceSyncSession>, String> {
        let _guard = self
            .session_lock
            .lock()
            .map_err(|_| "device_sync_session_lock_failed")?;
        let _file_guard = self.registry_file_lock()?;
        let registry = self.load_registry()?;
        let tenant = self
            .scoped_tenant
            .as_deref()
            .unwrap_or(&registry.selected_tenant_id);
        Ok(registry.sessions.get(tenant).cloned())
    }

    pub(super) fn save_session(&self, session: &DeviceSyncSession) -> Result<(), String> {
        let _guard = self
            .session_lock
            .lock()
            .map_err(|_| "device_sync_session_lock_failed")?;
        let _file_guard = self.registry_file_lock()?;
        let mut registry = self.load_registry()?;
        registry
            .sessions
            .insert(session.tenant_id.clone(), session.clone());
        registry.selected_tenant_id = session.tenant_id.clone();
        self.save_registry(&registry)
    }

    pub(crate) fn sessions(&self) -> Result<Value, String> {
        let _guard = self
            .session_lock
            .lock()
            .map_err(|_| "device_sync_session_lock_failed")?;
        let _file_guard = self.registry_file_lock()?;
        let registry = self.load_registry()?;
        Ok(
            json!({"ok":true,"selectedTenantId":registry.selected_tenant_id,"classes":registry.sessions.values().map(|session| json!({
            "tenantId":session.tenant_id,"uid":session.uid,"deviceId":session.device_id,
            "connectedAtMs":session.connected_at_ms,"tenantName":session.tenant_name,
            "schoolName":session.school_name,"academicYear":session.academic_year,
            "grade":session.grade,"classNumber":session.class_number,"lifecycleStatus":session.lifecycle_status,
        })).collect::<Vec<_>>() }),
        )
    }

    pub(crate) fn select_tenant(&self, tenant: &str) -> Result<Value, String> {
        {
            let _guard = self
                .session_lock
                .lock()
                .map_err(|_| "device_sync_session_lock_failed")?;
            let _file_guard = self.registry_file_lock()?;
            let mut registry = self.load_registry()?;
            if !registry.sessions.contains_key(tenant) {
                return Err("device_sync_session_required".into());
            }
            registry.selected_tenant_id = tenant.to_string();
            self.save_registry(&registry)?;
        }
        self.status_for_tenant(tenant)
    }

    pub(crate) fn status_for_tenant(&self, tenant: &str) -> Result<Value, String> {
        self.for_tenant(tenant)?.status()
    }
    pub(crate) fn run_once_for_tenant(&self, tenant: &str, force: bool) -> Result<Value, String> {
        self.for_tenant(tenant)?.run_once(force)
    }
    pub(crate) fn disconnect_for_tenant(&self, tenant: &str) -> Result<Value, String> {
        // Revocation only uses the approved registry and credential. A pending
        // class restore must not prevent removing its local connection.
        self.approved_session_for_tenant(tenant)?;
        self.scoped_with_store(tenant, Arc::clone(&self.store))?
            .disconnect()
    }
    pub(crate) fn native_class_identity_for_tenant(&self, tenant: &str) -> Result<Value, String> {
        let session = self.approved_session_for_tenant(tenant)?;
        self.credential(&session)?;
        Ok(
            json!({"tenantId":session.tenant_id,"actorId":session.uid,"deviceId":session.device_id,"appVersion":session.app_version}),
        )
    }
    pub(crate) fn student_record_mcp_identity_for_tenant(
        &self,
        tenant: &str,
    ) -> Result<Value, String> {
        self.native_class_identity_for_tenant(tenant)
    }
    pub(super) fn approved_session_for_tenant(
        &self,
        tenant: &str,
    ) -> Result<DeviceSyncSession, String> {
        if !valid_identifier(tenant, 128) {
            return Err("tenant_scope_invalid".into());
        }
        let _guard = self
            .session_lock
            .lock()
            .map_err(|_| "device_sync_session_lock_failed")?;
        let _file_guard = self.registry_file_lock()?;
        let session = self
            .load_registry()?
            .sessions
            .get(tenant)
            .cloned()
            .ok_or("device_sync_session_required")?;
        if !valid_identifier(&session.uid, 160) {
            return Err("device_sync_session_invalid".into());
        }
        Ok(session)
    }
    pub(crate) fn mcp_worker_authority_for_tenant(
        &self,
        tenant: &str,
    ) -> Result<crate::classaimate_mcp_worker::WorkerAuthority, String> {
        let session = self.approved_session_for_tenant(tenant)?;
        let credential = self.credential(&session)?;
        let origin = Url::parse(&self.api_root())
            .map_err(|_| "device_sync_origin_invalid")?
            .origin()
            .ascii_serialization();
        Ok(crate::classaimate_mcp_worker::WorkerAuthority {
            tenant_id: session.tenant_id,
            actor_id: session.uid,
            device_id: session.device_id,
            credential: zeroize::Zeroizing::new(credential),
            origin,
        })
    }
    pub(crate) fn mcp_worker_authorities(
        &self,
    ) -> Result<Vec<crate::classaimate_mcp_worker::WorkerAuthority>, String> {
        let tenants = self.connected_tenants()?;
        Ok(tenants
            .iter()
            .filter_map(|tenant| self.mcp_worker_authority_for_tenant(tenant).ok())
            .collect())
    }
    pub(super) fn connected_tenants(&self) -> Result<Vec<String>, String> {
        let _guard = self
            .session_lock
            .lock()
            .map_err(|_| "device_sync_session_lock_failed")?;
        let _file_guard = self.registry_file_lock()?;
        Ok(self.load_registry()?.sessions.keys().cloned().collect())
    }

    pub(super) fn registry_file_lock(&self) -> Result<fs::File, String> {
        let parent = self
            .session_path
            .parent()
            .ok_or("device_sync_session_dir_required")?;
        fs::create_dir_all(parent).map_err(|e| format!("device_sync_session_dir_failed:{e}"))?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(parent.join("device-sync-sessions.lock"))
            .map_err(|e| format!("device_sync_session_lock_failed:{e}"))?;
        file.lock_exclusive()
            .map_err(|e| format!("device_sync_session_lock_failed:{e}"))?;
        Ok(file)
    }

    // Caller holds session_lock, shared by all request-scoped managers.
    pub(super) fn load_registry(&self) -> Result<SessionRegistry, String> {
        if self.session_path.exists() {
            let raw = fs::read(&self.session_path)
                .map_err(|e| format!("device_sync_session_read_failed:{e}"))?;
            let registry: SessionRegistry = serde_json::from_slice(&raw)
                .map_err(|e| format!("device_sync_session_decode_failed:{e}"))?;
            if registry.version != 2
                || (!registry.selected_tenant_id.is_empty()
                    && !registry.sessions.contains_key(&registry.selected_tenant_id))
            {
                return Err("device_sync_session_invalid".into());
            }
            for (tenant, session) in &registry.sessions {
                validate(session)?;
                if tenant != &session.tenant_id {
                    return Err("device_sync_session_invalid".into());
                }
            }
            return Ok(registry);
        }
        let legacy = self.session_path.with_file_name(LEGACY_SESSION_FILE_NAME);
        let mut registry = SessionRegistry {
            version: 2,
            ..SessionRegistry::default()
        };
        if legacy.exists() {
            let raw =
                fs::read(&legacy).map_err(|e| format!("device_sync_session_read_failed:{e}"))?;
            let session: DeviceSyncSession = serde_json::from_slice(&raw)
                .map_err(|e| format!("device_sync_session_decode_failed:{e}"))?;
            validate(&session)?;
            registry.selected_tenant_id = session.tenant_id.clone();
            registry.sessions.insert(session.tenant_id.clone(), session);
            // Persist the registry once so disconnect never resurrects the legacy session.
            // The exact legacy bytes remain available for recovery.
            self.save_registry(&registry)?;
        }
        Ok(registry)
    }

    pub(super) fn save_registry(&self, registry: &SessionRegistry) -> Result<(), String> {
        let parent = self
            .session_path
            .parent()
            .ok_or("device_sync_session_dir_required")?;
        fs::create_dir_all(parent).map_err(|e| format!("device_sync_session_dir_failed:{e}"))?;
        let legacy = parent.join(LEGACY_SESSION_FILE_NAME);
        let protected = parent.join("device-sync-session.pre-classes.json");
        if legacy.exists() && !protected.exists() {
            let raw =
                fs::read(&legacy).map_err(|e| format!("device_sync_session_read_failed:{e}"))?;
            let temporary = parent.join("device-sync-session.pre-classes.json.tmp");
            let mut file = fs::File::create(&temporary)
                .map_err(|e| format!("device_sync_session_protect_failed:{e}"))?;
            file.write_all(&raw)
                .and_then(|_| file.sync_all())
                .map_err(|e| format!("device_sync_session_protect_failed:{e}"))?;
            drop(file);
            fs::rename(&temporary, &protected)
                .map_err(|e| format!("device_sync_session_protect_failed:{e}"))?;
            if fs::read(&protected)
                .map_err(|e| format!("device_sync_session_protect_failed:{e}"))?
                != raw
            {
                return Err("device_sync_session_protect_mismatch".into());
            }
        }
        let raw = serde_json::to_vec_pretty(registry)
            .map_err(|e| format!("device_sync_session_encode_failed:{e}"))?;
        let temporary = self.session_path.with_extension("json.tmp");
        let mut file = fs::File::create(&temporary)
            .map_err(|e| format!("device_sync_session_write_failed:{e}"))?;
        file.write_all(&raw)
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("device_sync_session_write_failed:{e}"))?;
        drop(file);
        fs::rename(&temporary, &self.session_path)
            .map_err(|e| format!("device_sync_session_commit_failed:{e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (PathBuf, DeviceSyncManager) {
        let root = env::temp_dir().join(format!(
            "classaimate-sessions-{}",
            crate::random_url_token()
        ));
        fs::create_dir_all(&root).unwrap();
        let store = Arc::new(SqliteStore::open(root.join("fixture.sqlite")).unwrap());
        let manager = DeviceSyncManager::new(root.clone(), store);
        (root, manager)
    }
    fn session(tenant: &str) -> DeviceSyncSession {
        DeviceSyncSession {
            version: 1,
            tenant_id: tenant.into(),
            uid: "teacher".into(),
            device_id: format!("device-{tenant}"),
            keyring_account: format!("device-sync:{tenant}:device-{tenant}"),
            ..DeviceSyncSession::default()
        }
    }
    #[test]
    fn legacy_session_is_preserved_and_not_resurrected_after_disconnect() {
        let (root, manager) = fixture();
        let raw = serde_json::to_vec(&session("class-2026")).unwrap();
        fs::write(root.join(LEGACY_SESSION_FILE_NAME), &raw).unwrap();
        assert_eq!(
            manager.load_session().unwrap().unwrap().tenant_id,
            "class-2026"
        );
        assert_eq!(
            fs::read(root.join("device-sync-session.pre-classes.json")).unwrap(),
            raw
        );
        let _ = manager.disconnect_for_tenant("class-2026");
        assert!(manager.load_session().unwrap().is_none());
        assert_eq!(fs::read(root.join(LEGACY_SESSION_FILE_NAME)).unwrap(), raw);
        drop(manager);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn class_registration_and_selection_leave_other_class_identity_unchanged() {
        let (root, manager) = fixture();
        manager.save_session(&session("class-2026")).unwrap();
        let old = manager.for_tenant("class-2026").unwrap();
        manager.save_session(&session("class-2027")).unwrap();
        assert_eq!(
            manager.load_session().unwrap().unwrap().tenant_id,
            "class-2027"
        );
        assert_eq!(old.load_session().unwrap().unwrap().tenant_id, "class-2026");
        assert_eq!(
            manager.sessions().unwrap()["classes"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(manager.select_tenant("unknown").is_err());
        let _ = manager.disconnect_for_tenant("class-2027");
        assert_eq!(
            manager.load_session().unwrap().unwrap().tenant_id,
            "class-2026"
        );
        drop(old);
        drop(manager);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn unapproved_class_scope_never_creates_a_class_directory() {
        let (root, manager) = fixture();
        assert!(manager.for_tenant("unapproved-class").is_err());
        assert!(!root.join("classes/unapproved-class").exists());
        assert!(manager
            .native_class_identity_for_tenant("unapproved-class")
            .is_err());
        assert!(manager
            .mcp_worker_authority_for_tenant("unapproved-class")
            .is_err());
        assert!(manager.status_for_tenant("unapproved-class").is_err());
        assert!(!root.join("classes/unapproved-class").exists());
        drop(manager);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disconnect_does_not_open_a_blocked_class_or_remove_other_connections() {
        let (root, manager) = fixture();
        manager.save_session(&session("class-2026")).unwrap();
        manager.save_session(&session("class-2027")).unwrap();
        let receipts = root.join(".class-restore-operations");
        fs::create_dir_all(&receipts).unwrap();
        // A damaged pending receipt is fail-closed for storage, but cached
        // connection control remains independent from physical DB readiness.
        use sha2::{Digest, Sha256};
        fs::write(
            receipts.join(format!("{:x}.json", Sha256::digest(b"class-2027"))),
            b"invalid",
        )
        .unwrap();
        assert!(manager.for_tenant("class-2027").is_err());
        let _ = manager.disconnect_for_tenant("class-2027");
        assert!(manager.approved_session_for_tenant("class-2027").is_err());
        assert_eq!(
            manager
                .approved_session_for_tenant("class-2026")
                .unwrap()
                .device_id,
            "device-class-2026"
        );
        assert!(!root.join("classes/class-2027").exists());
        drop(manager);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn request_scoped_locks_and_generation_state_are_independent() {
        let (root, manager) = fixture();
        manager.save_session(&session("class-2026")).unwrap();
        manager.save_session(&session("class-2027")).unwrap();
        let first = manager.for_tenant("class-2026").unwrap();
        let second = manager.for_tenant("class-2027").unwrap();
        let same = manager.for_tenant("class-2026").unwrap();
        let _guard = first.sync_lock.lock().unwrap();
        assert!(same.sync_lock.try_lock().is_err());
        assert!(second.sync_lock.try_lock().is_ok());
        backup::mark_sync_latest(&first.store, "class-2026", 17, "announced").unwrap();
        backup::update_retry(&first.store, "class-2026", true, now_ms()).unwrap();
        assert_eq!(
            backup::local_sync_state(&second.store, "class-2027")
                .unwrap()
                .latest_generation,
            0
        );
        assert!(!backup::retry_pending(&second.store, "class-2027").unwrap());
        drop(_guard);
        drop(same);
        drop(second);
        drop(first);
        drop(manager);
        fs::remove_dir_all(root).unwrap();
    }
}
