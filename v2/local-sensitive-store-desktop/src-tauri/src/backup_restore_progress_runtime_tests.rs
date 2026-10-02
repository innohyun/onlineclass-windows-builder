use super::*;
use std::io::Cursor;

const TENANT: &str = "qa-bounded-restore";

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("classaimate-qa-progress-{}", crate::random_url_token()));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn store(&self, name: &str) -> SqliteStore {
        let store = SqliteStore::open(self.0.join(name).join("store.sqlite")).unwrap();
        set_folder(&store, TENANT.into(), self.0.join("backups").to_string_lossy().into()).unwrap();
        store
    }
}
impl Drop for Fixture {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}

fn note(store: &SqliteStore, title: &str) {
    store.upsert_work_note(json!({"tenantId":TENANT,"pageId":"qa-page", "title":title,
        "blocks":[],"markdown":"Synthetic restore fixture"})).unwrap();
}

fn selected(source: &SqliteStore) -> Value {
    let snapshot = run_now(source, TENANT.into()).unwrap();
    json!({"tenantId":TENANT,"manifestPath":snapshot["manifestPath"]})
}

fn safety() -> Value {
    json!({"ok":true,"createdAtMs":1790928300000_i64, "media":{"missing":0,"failed":0},
        "workNoteAttachments":{"missing":0,"failed":0},"teachingSources":{"missing":0,"failed":0}})
}

fn tracked_restore<F>(store: &SqliteStore, body: Value, create_safety: F) -> (Result<Value, String>, Vec<RestoreProgress>)
where F: FnOnce(&SqliteStore, String) -> Result<Value, String> {
    let mut events = Vec::new();
    let result = {
        let mut observe = |event| events.push(event);
        let mut progress = ProgressTracker::new(Some(&mut observe));
        progress.checkpoint(RestorePhase::VerifyStage);
        let result = restore_with_policy_progress(store, body, create_safety, false, &mut progress);
        progress.complete(&result);
        result
    };
    (result, events)
}

fn terminal(events: &[RestoreProgress], phase: RestorePhase) -> &RestoreProgress {
    assert_eq!(events.iter().filter(|e| matches!(e.phase, RestorePhase::Result | RestorePhase::Failure)).count(), 1);
    let last = events.last().unwrap();
    assert_eq!(last.phase, phase);
    last
}

#[test]
fn progress_and_no_observer_have_identical_result_and_document_outcome() {
    let fixture = Fixture::new();
    crate::shared_archive::with_test_root(&fixture.0.join("archives"), || {
        let source = fixture.store("source");
        note(&source, "Synthetic incoming note");
        let body = selected(&source);
        let target_none = fixture.store("target-none");
        let target_some = fixture.store("target-some");
        let result_none = restore_with_policy(&target_none, body.clone(), |_, _| Ok(safety()), false);
        let (result_some, events) = tracked_restore(&target_some, body, |_, _| Ok(safety()));
        assert_eq!(result_some, result_none);
        result_some.unwrap();
        assert_eq!(target_some.get_work_note(TENANT.into(), "qa-page".into()).unwrap(),
            target_none.get_work_note(TENANT.into(), "qa-page".into()).unwrap());
        assert_eq!(events.iter().map(|e| e.phase).collect::<Vec<_>>(), vec![RestorePhase::VerifyStage,
            RestorePhase::ProtectedBackupVerified, RestorePhase::VerifyStage, RestorePhase::MergeStarted, RestorePhase::Result]);
        let result = terminal(&events, RestorePhase::Result);
        assert!(result.safety_backup_verified);
        assert_eq!(result.safety_created_at_ms, Some(1790928300000));
        assert_eq!(target_some.conn.lock().unwrap().query_row("SELECT COUNT(*) FROM local_store_restore_journal", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        target_some.restore_ready(TENANT).unwrap();
    });
}

#[test]
fn incomplete_safety_never_reports_protection_or_merge_and_preserves_current_note() {
    let fixture = Fixture::new();
    crate::shared_archive::with_test_root(&fixture.0.join("archives"), || {
        let source = fixture.store("source");
        note(&source, "Synthetic incoming note");
        let body = selected(&source);
        let target = fixture.store("target");
        note(&target, "Synthetic current note");
        let before = target.get_work_note(TENANT.into(), "qa-page".into()).unwrap();
        for category in ["media", "workNoteAttachments", "teachingSources"] {
            for count in ["missing", "failed"] {
                let mut incomplete = safety();
                incomplete[category][count] = json!(1);
                let (result, events) = tracked_restore(&target, body.clone(), |_, _| Ok(incomplete));
                assert_eq!(result.unwrap_err(), "pre_restore_backup_failed:safety_backup_incomplete");
                let failure = terminal(&events, RestorePhase::Failure);
                assert!(!failure.safety_backup_verified);
                assert!(failure.safety_created_at_ms.is_none());
                assert_eq!(failure.error_kind, Some(RestoreErrorKind::SafetyBackupFailed));
                assert!(!events.iter().any(|e| matches!(e.phase, RestorePhase::ProtectedBackupVerified | RestorePhase::MergeStarted)));
                assert_eq!(before, target.get_work_note(TENANT.into(), "qa-page".into()).unwrap());
            }
        }
    });
}

#[test]
fn staging_failure_reports_only_verified_protection_before_merge() {
    let fixture = Fixture::new();
    crate::shared_archive::with_test_root(&fixture.0.join("archives"), || {
        let source = fixture.store("source");
        note(&source, "Synthetic incoming note");
        crate::work_note_attachments::save(&source, TENANT.into(), "qa-attachment".into(), "qa-page".into(),
            "qa-block".into(), "fixture.txt".into(), "text/plain".into(), &mut Cursor::new(b"synthetic bytes")).unwrap();
        let body = selected(&source);
        let target = fixture.store("target");
        // A file at the staging root causes the real staging directory creation
        // to fail, after authoritative verification and before any live apply.
        fs::write(target.data_dir.join(".restore-staging"), b"synthetic obstruction").unwrap();
        let (result, events) = tracked_restore(&target, body, |_, _| Ok(safety()));
        assert!(result.unwrap_err().starts_with("restore_work_note_attachment_stage_dir_failed:"));
        let failure = terminal(&events, RestorePhase::Failure);
        assert!(failure.safety_backup_verified);
        assert_eq!(failure.last_confirmed_phase, Some(RestorePhase::VerifyStage));
        assert_eq!(failure.error_kind, Some(RestoreErrorKind::StagingFailed));
        assert!(!events.iter().any(|e| e.phase == RestorePhase::MergeStarted));
        assert!(target.get_work_note(TENANT.into(), "qa-page".into()).unwrap().is_none());
    });
}

#[test]
fn preflight_binding_conflict_keeps_protection_without_reporting_merge() {
    let fixture = Fixture::new();
    crate::shared_archive::with_test_root(&fixture.0.join("archives"), || {
        let source = fixture.store("source");
        let target = fixture.store("target");
        for (store, date) in [(&source, "2026-10-01"), (&target, "2026-10-02")] {
            crate::lesson_plan_bindings::upsert(store, json!({"tenantId":TENANT,"bindings":[{
                "planId":"qa-plan","pageId":"qa-page","planKind":"lesson","dateKey":date,
                "startPeriod":2,"endPeriod":2,"subject":"국어","bindingRevision":4,"updatedAt":1790928000000_i64}]})).unwrap();
        }
        let body = selected(&source);
        let (result, events) = tracked_restore(&target, body, |_, _| Ok(safety()));
        assert_eq!(result.unwrap_err(), "lesson_plan_binding_revision_conflict");
        let failure = terminal(&events, RestorePhase::Failure);
        assert!(failure.safety_backup_verified);
        assert_eq!(failure.error_kind, Some(RestoreErrorKind::Conflict));
        assert_eq!(failure.last_confirmed_phase, Some(RestorePhase::VerifyStage));
        assert!(!events.iter().any(|e| e.phase == RestorePhase::MergeStarted));
        let date: String = target.conn.lock().unwrap().query_row("SELECT date_key FROM lesson_plan_bindings WHERE tenant_id=?1 AND plan_id='qa-plan'", params![TENANT], |r| r.get(0)).unwrap();
        assert_eq!(date, "2026-10-02");
    });
}

#[test]
fn merge_checkpoint_precedes_live_files_and_success_follows_journal_cleanup() {
    let fixture = Fixture::new();
    crate::shared_archive::with_test_root(&fixture.0.join("archives"), || {
        let source = fixture.store("source");
        note(&source, "Synthetic incoming note");
        crate::work_note_attachments::save(&source, TENANT.into(), "qa-attachment".into(), "qa-page".into(),
            "qa-block".into(), "fixture.txt".into(), "text/plain".into(), &mut Cursor::new(b"incoming bytes")).unwrap();
        let attachment = list_work_note_attachment_rows(&source, TENANT).unwrap().remove(0);
        let body = selected(&source);
        let target = fixture.store("target");
        let target_file = target.data_dir.join(attachment.local_path);
        let mut events = Vec::new();
        {
            let mut observe = |event: RestoreProgress| {
                if event.phase == RestorePhase::MergeStarted {
                    assert!(!target_file.exists(), "merge event arrived after live file application");
                }
                if event.phase == RestorePhase::Result {
                    assert_eq!(fs::read(&target_file).unwrap(), b"incoming bytes");
                    assert_eq!(target.conn.lock().unwrap().query_row("SELECT COUNT(*) FROM local_store_restore_journal", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
                }
                events.push(event);
            };
            crate::backup::restore_with_progress(&target, body, Some(&mut observe)).unwrap();
        }
        terminal(&events, RestorePhase::Result);
        assert_eq!(events.iter().filter(|e| e.phase == RestorePhase::MergeStarted).count(), 1);
    });
}

#[test]
fn wrapper_captures_initial_configuration_and_readiness_failures() {
    let fixture = Fixture::new();
    crate::shared_archive::with_test_root(&fixture.0.join("archives"), || {
        let unconfigured = SqliteStore::open(fixture.0.join("unconfigured/store.sqlite")).unwrap();
        let body = json!({"tenantId":TENANT,"manifestPath":"synthetic-unused"});
        let baseline = crate::backup::restore(&unconfigured, body.clone());
        let mut events = Vec::new();
        let observed = crate::backup::restore_with_progress(&unconfigured, body.clone(), Some(&mut |e| events.push(e)));
        assert_eq!(baseline, observed);
        assert_eq!(observed.unwrap_err(), "backup_not_configured");
        assert_eq!(terminal(&events, RestorePhase::Failure).error_kind, Some(RestoreErrorKind::BackupUnavailable));

        let target = fixture.store("blocked");
        target.conn.lock().unwrap().execute("INSERT INTO local_store_restore_journal(tenant_id,operation_id,generation,artifact_root,phase,intent_json) VALUES(?1,'qa-block',0,'manual','prepared','{}')", params![TENANT]).unwrap();
        events.clear();
        let observed = crate::backup::restore_with_progress(&target, body, Some(&mut |e| events.push(e)));
        assert_eq!(observed.unwrap_err(), "restore_recovery_required");
        let failure = terminal(&events, RestorePhase::Failure);
        assert_eq!(failure.error_kind, Some(RestoreErrorKind::RecoveryRequired));
        assert!(!failure.safety_backup_verified);
        assert_eq!(events.len(), 2);

        let locked = fixture.store("lock-obstructed");
        fs::write(locked.data_dir.join("backup-operation-locks"), b"synthetic obstruction").unwrap();
        events.clear();
        let observed = crate::backup::restore_with_progress(&locked, json!({"tenantId":TENANT}), Some(&mut |e| events.push(e)));
        assert!(observed.unwrap_err().starts_with("backup_lock_dir_failed:"));
        let failure = terminal(&events, RestorePhase::Failure);
        assert_eq!(failure.error_kind, Some(RestoreErrorKind::BackupUnavailable));
        assert!(!failure.safety_backup_verified);
        assert_eq!(events.len(), 2);
    });
}

#[test]
fn merge_failure_retains_the_entered_phase_and_original_error_result() {
    let fixture = Fixture::new();
    crate::shared_archive::with_test_root(&fixture.0.join("archives"), || {
        let source = fixture.store("source");
        note(&source, "Synthetic incoming note");
        let body = selected(&source);
        let target_none = fixture.store("target-none");
        let target_some = fixture.store("target-some");
        for target in [&target_none, &target_some] {
            target.conn.lock().unwrap().execute_batch("DROP TABLE work_note_pages_fts").unwrap();
        }
        let baseline = restore_with_policy(&target_none, body.clone(), |_, _| Ok(safety()), false);
        let (observed, events) = tracked_restore(&target_some, body, |_, _| Ok(safety()));
        assert_eq!(baseline, observed);
        assert!(observed.is_err());
        let failure = terminal(&events, RestorePhase::Failure);
        assert_eq!(failure.last_confirmed_phase, Some(RestorePhase::MergeStarted));
        assert!(failure.safety_backup_verified);
        assert_eq!(failure.error_kind, Some(RestoreErrorKind::MergeFailed));
        assert_eq!(target_some.conn.lock().unwrap().query_row("SELECT COUNT(*) FROM local_store_restore_journal", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert!(target_some.get_work_note(TENANT.into(), "qa-page".into()).unwrap().is_none());
    });
}

#[test]
fn panicking_observer_does_not_interrupt_restore_or_journal_cleanup() {
    let fixture = Fixture::new();
    crate::shared_archive::with_test_root(&fixture.0.join("archives"), || {
        let source = fixture.store("source");
        note(&source, "Synthetic incoming note");
        let body = selected(&source);
        let target = fixture.store("target");
        let result = crate::backup::restore_with_progress(&target, body, Some(&mut |event: RestoreProgress| {
            if event.phase == RestorePhase::MergeStarted { panic!("synthetic failed observer"); }
        })).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(target.get_work_note(TENANT.into(), "qa-page".into()).unwrap().unwrap()["title"], "Synthetic incoming note");
        assert_eq!(target.conn.lock().unwrap().query_row("SELECT COUNT(*) FROM local_store_restore_journal", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        target.restore_ready(TENANT).unwrap();
    });
}
