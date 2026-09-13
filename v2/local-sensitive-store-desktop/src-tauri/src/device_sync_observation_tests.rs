//! Synthetic provenance regressions through the actual two-device sync lab.
use super::*;

fn legacy(lab: &Lab, device: usize, note: &str) -> Value {
    let store = &lab.stores[device];
    let record = json!({"tenantId":"qa-lab","docId":"legacy-one","date":"2026-09-08",
        "period":0,"studentCode":"QA1","observationKind":"non_lesson","contextType":"recess",
        "note":note,"eventTimePrecision":"unknown","eventAtMs":0,"createdAtMs":1,"updatedAtMs":1});
    let conn = store.conn.lock().unwrap();
    conn.execute("INSERT INTO lesson_observations VALUES('qa-lab','legacy-one','2026-09-08',0,'QA1',?1,1)",
        params![record.to_string()]).unwrap();
    crate::observation_evidence::ensure_schema(&conn, &store.data_dir).unwrap();
    drop(conn);
    projection(lab, device)
}

fn projection(lab: &Lab, device: usize) -> Value {
    lab.stores[device].evidence_detail("qa-lab", "legacy-one", false).unwrap()["record"].clone()
}

fn tracking(lab: &Lab, device: usize) -> (i64, i64, i64, i64) {
    lab.stores[device].conn.lock().unwrap().query_row(
        "SELECT dirty_base_generation,record_version,changed_generation,changed_at_ms
         FROM local_store_device_sync_records WHERE tenant_id='qa-lab'
         AND table_name='lesson_observations' AND record_key='[\"legacy-one\"]'", [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap()
}

#[test]
fn observation_rejected_projection_preserves_its_tracking_authority() {
    let lab = Lab::new(91301);
    legacy(&lab, 0, "source synthetic observation");
    let target = legacy(&lab, 1, "target synthetic observation");
    let before = tracking(&lab, 1);
    lab.publish(0).unwrap();
    lab.apply(1, true).unwrap();
    assert_eq!(projection(&lab, 1), target, "never choose an independent head");
    assert_eq!(tracking(&lab, 1), before, "rejected projection is not the incoming record version");
}

#[test]
fn observation_independent_baselines_do_not_publish_forever_without_edits() {
    for (seed, right) in [(91302, "same synthetic observation"), (91303, "different synthetic observation")] {
        let lab = Lab::new(seed);
        let left = legacy(&lab, 0, "same synthetic observation");
        let right = legacy(&lab, 1, right);
        for _ in 0..3 { lab.publish(0).unwrap(); lab.publish(1).unwrap(); }
        let generation = checkpoint_generation(lab.cp().as_ref());
        for _ in 0..3 { lab.publish(0).unwrap(); lab.publish(1).unwrap(); }
        assert_eq!(checkpoint_generation(lab.cp().as_ref()), generation, "no force-sync generation ping-pong");
        assert_eq!(projection(&lab, 0), left);
        assert_eq!(projection(&lab, 1), right);
        for device in 0..2 {
            let detail = lab.stores[device].evidence_detail("qa-lab", "legacy-one", false).unwrap();
            assert_eq!(detail["heads"].as_array().unwrap().len(), 2);
        }
    }
}

#[test]
fn observation_orphan_cannot_be_sealed_or_published_as_a_new_generation() {
    let lab = Lab::new(91304);
    legacy(&lab, 0, "synthetic orphan");
    lab.stores[0].conn.lock().unwrap().execute("DELETE FROM observation_evidence_revisions WHERE tenant_id='qa-lab'", []).unwrap();
    let result = lab.publish(0);
    assert!(result.is_err(), "a snapshot that cannot restore must not be published");
    assert!(lab.cp().is_none());
    assert_eq!(lab.cloud.ack_count.load(Ordering::SeqCst), 0);
    assert_eq!(projection(&lab, 0)["note"], "synthetic orphan");
}

#[test]
fn observation_normal_correction_converges_without_a_false_branch() {
    let lab = Lab::new(91305);
    legacy(&lab, 0, "original synthetic observation");
    lab.publish(0).unwrap();
    lab.apply(1, true).unwrap();
    let mut input = projection(&lab, 1);
    input["expectedRevisionId"] = input["revisionId"].clone();
    input["correctionReason"] = json!("synthetic correction test");
    input["note"] = json!("corrected synthetic observation");
    lab.stores[1].evidence_save("qa-lab", vec![input], "qa-correction").unwrap();
    lab.publish(1).unwrap();
    lab.apply(0, true).unwrap();
    assert_eq!(projection(&lab, 0), projection(&lab, 1));
    assert_eq!(projection(&lab, 0)["note"], "corrected synthetic observation");
    for device in 0..2 {
        let detail = lab.stores[device].evidence_detail("qa-lab", "legacy-one", false).unwrap();
        assert_eq!(detail["heads"].as_array().unwrap().len(), 1);
        assert_eq!(detail["verification"]["valid"], true);
    }
}

#[test]
fn observation_explicit_synthetic_resolution_converges_and_keeps_both_roots() {
    let lab = Lab::new(91306);
    legacy(&lab, 0, "synthetic left");
    legacy(&lab, 1, "synthetic right");
    lab.publish(0).unwrap();
    lab.publish(1).unwrap();
    lab.apply(0, true).unwrap();
    let detail = lab.stores[0].evidence_detail("qa-lab", "legacy-one", false).unwrap();
    let heads = detail["heads"].as_array().unwrap();
    let request = json!({"docId":"legacy-one","mutationId":"qa-explicit-resolution",
        "correctionReason":"synthetic test choice only","selectedRevisionId":heads[0]["revisionId"],
        "expectedHeadIds":heads.iter().map(|h|h["revisionId"].clone()).collect::<Vec<_>>()});
    let resolved = lab.stores[0].evidence_resolve("qa-lab", &request).unwrap();
    assert_eq!(lab.stores[0].evidence_resolve("qa-lab", &request).unwrap(), resolved);
    lab.publish(0).unwrap();
    lab.apply(1, true).unwrap();
    assert_eq!(projection(&lab, 0), projection(&lab, 1));
    for device in 0..2 {
        let detail = lab.stores[device].evidence_detail("qa-lab", "legacy-one", false).unwrap();
        assert_eq!(detail["heads"].as_array().unwrap().len(), 1);
        assert_eq!(detail["revisions"].as_array().unwrap().len(), 3);
        assert_eq!(detail["verification"]["valid"], true);
    }
}

#[test]
fn observation_tracking_gap_after_seed_cannot_pass_capture_coverage_guard() {
    let lab = Lab::new(91307);
    legacy(&lab, 0, "synthetic tracking gap");
    backup::seed_sync_records(&lab.stores[0], "qa-lab").unwrap();
    lab.stores[0].conn.lock().unwrap().execute(
        "DELETE FROM local_store_device_sync_records WHERE tenant_id='qa-lab' AND table_name='observation_evidence_revisions'", []).unwrap();
    let error = lab.publish(0).unwrap_err();
    assert!(error.starts_with("backup_sync_tracking_incomplete:"), "{error}");
    assert!(lab.cp().is_none());
}

#[test]
fn observation_tracking_repair_replaces_only_the_pending_candidate_reference() {
    let lab = Lab::new(91308);
    legacy(&lab, 0, "synthetic pending repair");
    lab.cloud.offline.store(true, Ordering::SeqCst);
    assert!(lab.publish(0).is_err());
    let previous = backup::pending_publication(&lab.stores[0], "qa-lab").unwrap().unwrap();
    let previous_path = PathBuf::from(previous["snapshot"]["manifestPath"].as_str().unwrap());
    let unchanged_content = crate::shared_archive::with_test_root(&lab.stores[0].data_dir, ||
        backup::tenant_content_sha256(&lab.stores[0], "qa-lab")).unwrap();
    {
        let conn = lab.stores[0].conn.lock().unwrap();
        conn.execute("DELETE FROM local_store_device_sync_records WHERE tenant_id='qa-lab' AND table_name='observation_evidence_revisions'", []).unwrap();
        conn.execute("UPDATE local_store_device_sync_state SET seed_version=1,last_content_sha256=?1 WHERE tenant_id='qa-lab'", params![unchanged_content]).unwrap();
    }
    lab.cloud.offline.store(false, Ordering::SeqCst);
    lab.publish(0).unwrap();
    assert_ne!(lab.cp().unwrap()["artifactSetSha256"], previous["snapshot"]["artifactSetSha256"]);
    assert!(previous_path.exists(), "never remove an older candidate's source files");
    assert_eq!(backup::local_sync_state(&lab.stores[0], "qa-lab").unwrap().tracking_repair_sequence, 0);
    lab.apply(1, true).unwrap();
    assert_eq!(projection(&lab, 0), projection(&lab, 1));
    lab.publish(0).unwrap();
    assert_eq!(checkpoint_generation(lab.cp().as_ref()), 1);
}

#[test]
fn observation_tracking_repair_acknowledges_a_lost_publish_response_without_a_second_generation() {
    let lab = Lab::new(91309);
    legacy(&lab, 0, "synthetic lost repair response");
    {
        let conn = lab.stores[0].conn.lock().unwrap();
        conn.execute("DELETE FROM local_store_device_sync_records WHERE tenant_id='qa-lab' AND table_name='observation_evidence_revisions'", []).unwrap();
        conn.execute("UPDATE local_store_device_sync_state SET seed_version=1 WHERE tenant_id='qa-lab'", []).unwrap();
    }
    lab.cloud.lose_response.store(true, Ordering::SeqCst);
    assert!(lab.publish(0).is_err());
    assert!(backup::local_sync_state(&lab.stores[0], "qa-lab").unwrap().tracking_repair_sequence > 0);
    let root = lab.cp().unwrap()["artifactSetSha256"].clone();
    lab.publish(0).unwrap();
    assert_eq!(checkpoint_generation(lab.cp().as_ref()), 1);
    assert_eq!(lab.cp().unwrap()["artifactSetSha256"], root);
    assert_eq!(backup::local_sync_state(&lab.stores[0], "qa-lab").unwrap().tracking_repair_sequence, 0);
    assert!(backup::pending_publication(&lab.stores[0], "qa-lab").unwrap().is_none());
    lab.apply(1, true).unwrap();
    assert_eq!(projection(&lab, 0), projection(&lab, 1));
}
