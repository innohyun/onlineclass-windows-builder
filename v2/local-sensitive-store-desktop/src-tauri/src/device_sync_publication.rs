use super::*;

impl DeviceSyncManager {
    pub(super) fn publish(
        &self,
        session: &DeviceSyncSession,
        credential: &str,
        base_generation: i64,
        latest_status: &str,
        snapshot_version: i64,
    ) -> Result<(), String> {
        let pending = backup::pending_publication(&self.store, &session.tenant_id)?;
        let tenant_dir = backup::configured_tenant_dir(&self.store, &session.tenant_id)?;
        let reusable = pending.as_ref().filter(|pending| {
            pending.get("deviceId").and_then(Value::as_str) == Some(session.device_id.as_str())
                && pending.get("baseGeneration").and_then(Value::as_i64) == Some(base_generation)
                && pending
                    .pointer("/snapshot/snapshotVersion")
                    .and_then(Value::as_i64)
                    == Some(snapshot_version)
                && pending
                    .pointer("/snapshot/manifestPath")
                    .and_then(Value::as_str)
                    .map(|path| Path::new(path).starts_with(&tenant_dir))
                    .unwrap_or(false)
        });
        let snapshot = if let Some(pending) = reusable {
            let snapshot = pending
                .get("snapshot")
                .ok_or("device_sync_pending_invalid")?
                .clone();
            self.verified_snapshot(
                &session.tenant_id,
                base_generation + 1,
                snapshot
                    .get("artifactSetSha256")
                    .and_then(Value::as_str)
                    .ok_or("device_sync_pending_invalid")?,
                snapshot
                    .get("databaseSha256")
                    .and_then(Value::as_str)
                    .ok_or("device_sync_pending_invalid")?,
            )?;
            snapshot
        } else {
            // Capture the sequence before hashing; an edit during the read may
            // cause extra work, but can never be marked unchanged accidentally.
            backup::seed_sync_records(&self.store, &session.tenant_id)?;
            let state = backup::local_sync_state(&self.store, &session.tenant_id)?;
            let content = backup::tenant_content_sha256(&self.store, &session.tenant_id)?;
            if !state.last_content_sha256.is_empty() && state.last_content_sha256 == content {
                backup::mark_sync_unchanged(
                    &self.store,
                    &session.tenant_id,
                    base_generation,
                    state.change_sequence,
                )?;
                backup::save_pending_publication(&self.store, &session.tenant_id, None)?;
                return Ok(());
            }
            let snapshot = backup::run_with_kind_version(
                &self.store,
                session.tenant_id.clone(),
                "auto_sync",
                Some(base_generation + 1),
                snapshot_version,
            )?;
            if snapshot.get("ok").and_then(Value::as_bool) != Some(true) {
                return Err("device_sync_snapshot_incomplete".into());
            }
            backup::save_pending_publication(
                &self.store,
                &session.tenant_id,
                Some(&json!({
                    "deviceId":session.device_id,"baseGeneration":base_generation,"snapshot":snapshot,
                })),
            )?;
            snapshot
        };
        let root = snapshot
            .get("artifactSetSha256")
            .and_then(Value::as_str)
            .ok_or("device_sync_snapshot_invalid")?;
        let database = snapshot
            .get("databaseSha256")
            .and_then(Value::as_str)
            .ok_or("device_sync_snapshot_invalid")?;
        let checkpoint=self.authorized_post(session,credential,"/checkpoints",json!({
            "baseGeneration":base_generation,"artifactSetSha256":root,"databaseSha256":database,"snapshotVersion":snapshot_version,
        }))?;
        backup::mark_sync_published(
            &self.store,
            &session.tenant_id,
            base_generation + 1,
            Path::new(
                snapshot
                    .get("manifestPath")
                    .and_then(Value::as_str)
                    .ok_or("backup_manifest_required")?,
            ),
            snapshot
                .get("contentSha256")
                .and_then(Value::as_str)
                .ok_or("device_sync_snapshot_content_required")?,
            checkpoint
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or(latest_status),
            snapshot
                .get("capturedSequence")
                .and_then(Value::as_i64)
                .ok_or("device_sync_snapshot_sequence_required")?,
        )?;
        backup::save_pending_publication(&self.store, &session.tenant_id, None)
    }

    pub(super) fn pending_sequence(
        &self,
        tenant_id: &str,
        generation: i64,
        root: &str,
    ) -> Result<i64, String> {
        let pending = backup::pending_publication(&self.store, tenant_id)?;
        Ok(pending
            .as_ref()
            .filter(|p| {
                p.pointer("/snapshot/generation").and_then(Value::as_i64) == Some(generation)
                    && p.pointer("/snapshot/artifactSetSha256")
                        .and_then(Value::as_str)
                        == Some(root)
            })
            .and_then(|p| p.pointer("/snapshot/capturedSequence"))
            .and_then(Value::as_i64)
            .unwrap_or(-1))
    }
}
