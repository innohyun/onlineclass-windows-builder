//! Bounded, observational progress for the desktop's manual restore only.
use serde::Serialize;
use serde_json::Value;
use std::panic::{catch_unwind, AssertUnwindSafe};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RestorePhase {
    VerifyStage,
    ProtectedBackupVerified,
    MergeStarted,
    Failure,
    Result,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RestoreErrorKind {
    BackupUnavailable,
    RecoveryRequired,
    SafetyBackupFailed,
    DownloadPending,
    ValidationFailed,
    StagingFailed,
    Conflict,
    MergeFailed,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RestoreProgress {
    pub(crate) phase: RestorePhase,
    // This is the last phase actually entered, not a claim that every check
    // inside it passed or that failure rolled back the current store.
    pub(crate) last_confirmed_phase: Option<RestorePhase>,
    pub(crate) safety_backup_verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) safety_created_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error_kind: Option<RestoreErrorKind>,
}

pub(super) struct ProgressTracker<'a> {
    callback: Option<&'a mut dyn FnMut(RestoreProgress)>,
    last_confirmed_phase: Option<RestorePhase>,
    safety_backup_verified: bool,
    safety_created_at_ms: Option<i64>,
    terminal: bool,
}

impl<'a> ProgressTracker<'a> {
    pub(super) fn new(callback: Option<&'a mut dyn FnMut(RestoreProgress)>) -> Self {
        Self { callback, last_confirmed_phase: None, safety_backup_verified: false,
            safety_created_at_ms: None, terminal: false }
    }

    pub(super) fn checkpoint(&mut self, phase: RestorePhase) {
        if self.terminal || matches!(phase, RestorePhase::Failure | RestorePhase::Result) { return; }
        self.last_confirmed_phase = Some(phase);
        self.emit(phase, None);
    }

    pub(super) fn protected_backup_verified(&mut self, safety_backup: &Value) {
        self.safety_backup_verified = true;
        self.safety_created_at_ms = safety_backup.get("createdAtMs").and_then(Value::as_i64);
        self.checkpoint(RestorePhase::ProtectedBackupVerified);
    }

    pub(super) fn complete(&mut self, result: &Result<Value, String>) {
        if self.terminal { return; }
        self.terminal = true;
        match result {
            Ok(_) => self.emit(RestorePhase::Result, None),
            Err(error) => self.emit(RestorePhase::Failure, Some(error_kind(error))),
        }
    }

    fn emit(&mut self, phase: RestorePhase, error_kind: Option<RestoreErrorKind>) {
        let event = RestoreProgress { phase, last_confirmed_phase: self.last_confirmed_phase,
            safety_backup_verified: self.safety_backup_verified,
            safety_created_at_ms: self.safety_created_at_ms, error_kind };
        if let Some(callback) = self.callback.take() {
            // A failed observer must not interrupt the existing restore, journal
            // cleanup, or its original return value. Disable a panicking observer.
            if catch_unwind(AssertUnwindSafe(|| callback(event))).is_ok() {
                self.callback = Some(callback);
            }
        }
    }
}

fn error_kind(error: &str) -> RestoreErrorKind {
    let code = error.split(':').next().unwrap_or("");
    match code {
        "pre_restore_backup_failed" => RestoreErrorKind::SafetyBackupFailed,
        "restore_recovery_required" => RestoreErrorKind::RecoveryRequired,
        "onedrive_download_pending" => RestoreErrorKind::DownloadPending,
        "backup_not_configured" | "backup_lock_dir_failed" | "backup_lock_open_failed"
        | "backup_lock_failed" | "backup_root_required" | "backup_root_inside_local_store" => RestoreErrorKind::BackupUnavailable,
        "lesson_plan_binding_revision_conflict" | "restore_local_record_changed"
        | "observation_evidence_restore_conflict" => RestoreErrorKind::Conflict,
        "restore_media_stage_dir_failed" | "restore_media_stage_failed"
        | "restore_work_note_attachment_stage_dir_failed" | "restore_work_note_attachment_stage_failed"
        | "restore_teaching_source_stage_dir_failed" | "restore_teaching_source_stage_failed" => RestoreErrorKind::StagingFailed,
        "restore_db_attach_failed" | "restore_transaction_begin_failed" | "restore_table_merge_failed"
        | "restore_media_path_update_failed" | "restore_work_note_fts_delete_failed"
        | "restore_work_note_fts_insert_failed" | "restore_transaction_commit_failed" => RestoreErrorKind::MergeFailed,
        "tenant_id_required" | "backup_manifest_required" | "backup_manifest_read_failed"
        | "backup_manifest_decode_failed" | "backup_db_required" | "backup_db_path_invalid"
        | "backup_db_open_failed" | "restore_media_target_invalid" | "backup_artifact_digest_mismatch"
        | "observation_evidence_restore_invalid"
        | "teaching_source_backup_manifest_mismatch" | "teaching_source_backup_artifact_incomplete"
        | "teaching_source_backup_artifact_path_invalid" | "teaching_source_backup_artifact_digest_mismatch" => RestoreErrorKind::ValidationFailed,
        _ => RestoreErrorKind::Other,
    }
}

#[cfg(test)]
#[path = "backup_restore_progress_tests.rs"]
mod tests;
