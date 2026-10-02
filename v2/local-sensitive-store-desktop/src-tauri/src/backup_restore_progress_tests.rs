use super::*;
use serde_json::json;

#[test]
fn progress_is_bounded_and_preserves_actual_protection_facts() {
    let mut events = Vec::new();
    {
        let mut observe = |event| events.push(event);
        let mut tracker = ProgressTracker::new(Some(&mut observe));
        tracker.checkpoint(RestorePhase::VerifyStage);
        tracker.protected_backup_verified(&json!({"createdAtMs": 1790928300000_i64,
            "manifestPath":"C:/private/backup.json", "tenantId":"private-tenant", "studentCode":"private-student"}));
        tracker.checkpoint(RestorePhase::VerifyStage);
        tracker.complete(&Err("restore_work_note_attachment_stage_failed:C:/private/student-file.txt".into()));
        tracker.complete(&Ok(json!({"ok":true})));
        tracker.checkpoint(RestorePhase::MergeStarted);
    }
    assert_eq!(events.len(), 4);
    let failure = events.last().unwrap();
    assert_eq!(failure.phase, RestorePhase::Failure);
    assert_eq!(failure.last_confirmed_phase, Some(RestorePhase::VerifyStage));
    assert!(failure.safety_backup_verified);
    assert_eq!(failure.safety_created_at_ms, Some(1790928300000));
    assert_eq!(failure.error_kind, Some(RestoreErrorKind::StagingFailed));
    let serialized = serde_json::to_value(&events).unwrap();
    assert_eq!(serialized[3]["phase"], "failure");
    assert_eq!(serialized[3]["lastConfirmedPhase"], "verify_stage");
    assert_eq!(serialized[3]["errorKind"], "staging_failed");
    for event in serialized.as_array().unwrap() {
        for key in event.as_object().unwrap().keys() {
            assert!(["phase", "lastConfirmedPhase", "safetyBackupVerified", "safetyCreatedAtMs", "errorKind"].contains(&key.as_str()));
        }
    }
    let raw = serialized.to_string();
    for private in ["C:/", "private", "studentCode", "tenantId", "manifestPath", "stage_failed:"] {
        assert!(!raw.contains(private), "unexpected private progress field");
    }
}

#[test]
fn unknown_error_never_becomes_an_event_message_or_timestamp() {
    let mut events = Vec::new();
    {
        let mut observe = |event| events.push(event);
        let mut tracker = ProgressTracker::new(Some(&mut observe));
        tracker.protected_backup_verified(&json!({"createdAtMs":"not-an-authoritative-time"}));
        tracker.complete(&Err("private arbitrary message with 학생 데이터".into()));
    }
    assert_eq!(events[1].error_kind, Some(RestoreErrorKind::Other));
    assert!(events[1].safety_created_at_ms.is_none());
    assert!(!serde_json::to_value(&events[1]).unwrap().as_object().unwrap().contains_key("safetyCreatedAtMs"));
}

#[test]
fn panicking_observer_is_disabled_without_changing_terminal_result() {
    let mut calls = 0;
    let result: Result<Value, String> = Err("original_failure".into());
    {
        let mut observe = |_event| { calls += 1; panic!("synthetic failed observer"); };
        let mut tracker = ProgressTracker::new(Some(&mut observe));
        tracker.checkpoint(RestorePhase::VerifyStage);
        tracker.complete(&result);
    }
    assert_eq!(calls, 1);
    assert_eq!(result, Err("original_failure".into()));
}
