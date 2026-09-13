use super::*;
use crate::observation_evidence_receipts;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use p256::ecdsa::{signature::Signer, Signature, SigningKey};

struct Fixture {
    store: SqliteStore,
    dir: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("observation-evidence-{}", uuid()));
        let store = SqliteStore::open(dir.join("store.sqlite")).unwrap();
        Self { store, dir }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn input(id: &str) -> Value {
    json!({"tenantId":"tenant-a","docId":id,"date":"2026-09-08","period":0,"studentCode":"S1","observationKind":"non_lesson","contextType":"recess","note":"관찰 원문","eventTimePrecision":"unknown","eventAtMs":0,"createdAtMs":1,"updatedAtMs":1})
}

#[test]
fn occurrence_authority_cas_atomic_retry_and_history() {
    let f = Fixture::new();
    let first = f
        .store
        .evidence_save("tenant-a", vec![input("one"), input("two")], "mutation-1")
        .unwrap();
    assert_eq!(first[0]["eventAtMs"], Value::Null);
    assert!(first[0]["createdAtMs"].as_i64().unwrap() > 1);
    assert_eq!(
        f.store.evidence_outbox("tenant-a").unwrap()["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        f.store
            .evidence_save("tenant-a", vec![input("one"), input("two")], "mutation-1")
            .unwrap(),
        first
    );
    assert!(f
        .store
        .evidence_save("tenant-a", vec![input("one")], "mutation-1")
        .unwrap_err()
        .contains("mutation_conflict"));
    assert!(f
        .store
        .evidence_save("tenant-a", vec![input("one")], "stale")
        .unwrap_err()
        .contains("revision_conflict"));
    let mut corrected = input("one");
    corrected["expectedRevisionId"] = first[0]["revisionId"].clone();
    assert!(f
        .store
        .evidence_save("tenant-a", vec![corrected.clone()], "reason")
        .unwrap_err()
        .contains("reason_required"));
    corrected["correctionReason"] = json!("실제 발생 시각 확인");
    corrected["eventTimePrecision"] = json!("approximate");
    corrected["eventAtMs"] = json!(1788827400000i64);
    // 2026-09-08 10:50 KST. The exact day is checked independently of the local PC timezone.
    let actual_date = (chrono::DateTime::from_timestamp_millis(1788827400000).unwrap()
        + chrono::Duration::hours(9))
    .format("%Y-%m-%d")
    .to_string();
    corrected["date"] = json!(actual_date);
    let second = f
        .store
        .evidence_save("tenant-a", vec![corrected], "correct-1")
        .unwrap();
    assert_eq!(second[0]["createdAtMs"], first[0]["createdAtMs"]);
    let detail = f.store.evidence_detail("tenant-a", "one", false).unwrap();
    assert_eq!(detail["revisions"].as_array().unwrap().len(), 2);
    assert_eq!(detail["verification"]["valid"], true);
    let mut invalid = input("invalid");
    invalid["eventTimePrecision"] = json!("exact");
    invalid["eventAtMs"] = Value::Null;
    assert!(f
        .store
        .evidence_save("tenant-a", vec![input("not-saved"), invalid], "rollback")
        .is_err());
    assert!(f
        .store
        .evidence_detail("tenant-a", "not-saved", false)
        .is_err());
    assert!(f.store.evidence_detail("tenant-b", "one", false).is_err());
}

#[test]
fn detects_record_revision_photo_tampering_and_blocks_replacement() {
    let f = Fixture::new();
    let media = json!({"tenantId":"tenant-a","boardId":"student-observations","postId":"one","mediaId":"photo-one","fileName":"one.png","contentType":"image/png","dataBase64":"aW1hZ2U="});
    f.store.upsert_board_media(media.clone()).unwrap();
    let mut record = input("one");
    record["photoMediaIds"] = json!(["photo-one"]);
    f.store
        .evidence_save("tenant-a", vec![record], "one")
        .unwrap();
    assert_eq!(
        f.store.evidence_detail("tenant-a", "one", true).unwrap()["media"][0]["dataBase64"],
        "aW1hZ2U="
    );
    let mut replacement = media;
    replacement["dataBase64"] = json!("dGFtcGVy");
    assert_eq!(
        f.store.upsert_board_media(replacement).unwrap_err(),
        "observation_photo_immutable"
    );
    {
        let conn = f.store.conn.lock().unwrap();
        conn.execute(
            "UPDATE lesson_observations SET payload_json=json_set(payload_json,'$.note','changed')",
            [],
        )
        .unwrap();
    }
    assert_eq!(
        f.store.evidence_detail("tenant-a", "one", false).unwrap()["verification"]["valid"],
        false
    );
    {
        let conn = f.store.conn.lock().unwrap();
        conn.execute("UPDATE observation_evidence_revisions SET payload_json=json_set(payload_json,'$.reason','tampered')",[]).unwrap();
    }
    assert!(
        f.store.evidence_detail("tenant-a", "one", false).unwrap()["verification"]["errors"]
            .as_array()
            .unwrap()
            .contains(&json!("revision_hash_mismatch"))
    );
}

#[test]
fn legacy_baseline_does_not_claim_historical_time() {
    let f = Fixture::new();
    let mut record = input("legacy");
    record["eventAtMs"] = json!(1000);
    {
        let conn = f.store.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO lesson_observations VALUES('tenant-a','legacy','2026-09-08',0,'S1',?1,1)",
            params![record.to_string()],
        )
        .unwrap();
        ensure_schema(&conn, &f.dir).unwrap();
    }
    let detail = f
        .store
        .evidence_detail("tenant-a", "legacy", false)
        .unwrap();
    assert_eq!(detail["record"]["eventAtMs"], Value::Null);
    assert_eq!(
        detail["record"]["legacyOriginalTimestamps"]["eventAtMs"],
        1000
    );
    assert_eq!(detail["revisions"][0]["eventKind"], "legacy_baseline");
}

#[test]
fn validates_receipt_signature_tenant_payload_and_trusted_key() {
    let signing = SigningKey::random(&mut rand::thread_rng());
    let point = signing.verifying_key().to_encoded_point(false);
    let jwk = json!({"kty":"EC","crv":"P-256","x":URL_SAFE_NO_PAD.encode(point.x().unwrap()),"y":URL_SAFE_NO_PAD.encode(point.y().unwrap())});
    let key_id = URL_SAFE_NO_PAD.encode(Sha256::digest(jwk.to_string().as_bytes()));
    let payload = json!({"version":1,"tenantId":"tenant-a","receiptId":uuid(),"commitmentSha256":"a".repeat(64),"actorUserId":"teacher","receivedAtMs":now_ms(),"externalTimestampStatus":"inactive"});
    let bytes = payload.to_string();
    let signature: Signature = signing.sign(bytes.as_bytes());
    let receipt = json!({"payload":payload,"payloadBase64url":URL_SAFE_NO_PAD.encode(bytes),"signatureBase64url":URL_SAFE_NO_PAD.encode(signature.to_bytes()),"keyId":key_id,"algorithm":"ES256","publicKeyJwk":jwk});
    let keys = json!([{"keyId":key_id,"algorithm":"ES256","publicKeyJwk":jwk}]);
    observation_evidence_receipts::verify(&receipt, "tenant-a", &"a".repeat(64), &keys).unwrap();
    assert!(
        observation_evidence_receipts::verify(&receipt, "tenant-b", &"a".repeat(64), &keys)
            .is_err()
    );
    assert!(observation_evidence_receipts::verify(
        &receipt,
        "tenant-a",
        &"a".repeat(64),
        &json!([])
    )
    .is_err());
    let mut changed = receipt;
    changed["payload"]["receivedAtMs"] = json!(1);
    assert!(
        observation_evidence_receipts::verify(&changed, "tenant-a", &"a".repeat(64), &keys)
            .is_err()
    );
}

#[test]
fn restore_rejects_old_head_and_preserves_local_lineage() {
    let f = Fixture::new();
    let first = f
        .store
        .evidence_save("tenant-a", vec![input("one")], "one")
        .unwrap();
    let snapshot = f.dir.join("old.sqlite");
    {
        let conn = f.store.conn.lock().unwrap();
        conn.execute("VACUUM INTO ?1", params![snapshot.to_string_lossy()])
            .unwrap();
    }
    let mut next = input("one");
    next["expectedRevisionId"] = first[0]["revisionId"].clone();
    next["correctionReason"] = json!("내용 보완");
    next["note"] = json!("보완 기록");
    f.store
        .evidence_save("tenant-a", vec![next], "two")
        .unwrap();
    let conn = f.store.conn.lock().unwrap();
    conn.execute(
        "ATTACH DATABASE ?1 AS restore",
        params![snapshot.to_string_lossy()],
    )
    .unwrap();
    check_restore(&conn, "tenant-a").unwrap();
    let sql=format!("INSERT INTO main.lesson_observations SELECT * FROM restore.lesson_observations WHERE tenant_id='tenant-a' ON CONFLICT(tenant_id,doc_id) DO UPDATE SET payload_json=excluded.payload_json,updated_at_ms=excluded.updated_at_ms WHERE {}",observation_merge_guard());
    assert_eq!(conn.execute(&sql, []).unwrap(), 0);
    conn.execute_batch("DETACH DATABASE restore").unwrap();
}

#[test]
fn divergent_histories_are_preserved_and_explicitly_resolved() {
    let f = Fixture::new();
    let first = f
        .store
        .evidence_save("tenant-a", vec![input("one")], "one")
        .unwrap();
    let mut correction = input("one");
    correction["expectedRevisionId"] = first[0]["revisionId"].clone();
    correction["correctionReason"] = json!("첫 PC 정정");
    let second = f
        .store
        .evidence_save("tenant-a", vec![correction], "two")
        .unwrap();
    let fork = {
        let conn = f.store.conn.lock().unwrap();
        let mut record = first[0].clone();
        record["note"] = json!("두 번째 PC 정정");
        let fork = append(
            &conn,
            &f.dir,
            record,
            Some(&first[0]),
            "correct",
            "두 번째 PC",
            now_ms(),
            None,
        )
        .unwrap();
        batch(
            &conn,
            "tenant-a",
            &[fork["revisionHash"].as_str().unwrap().to_string()],
        )
        .unwrap();
        fork
    };
    let detail = f.store.evidence_detail("tenant-a", "one", false).unwrap();
    assert_eq!(detail["heads"].as_array().unwrap().len(), 2);
    assert_eq!(detail["verification"]["valid"], false);
    let request = json!({"docId":"one","expectedHeadIds":[second[0]["revisionId"],fork["revisionId"]],"selectedRevisionId":second[0]["revisionId"],"correctionReason":"두 PC 기록 대조 후 선택","mutationId":"resolve-one"});
    let result = f.store.evidence_resolve("tenant-a", &request).unwrap();
    assert_eq!(
        f.store.evidence_resolve("tenant-a", &request).unwrap(),
        result
    );
    let detail = f.store.evidence_detail("tenant-a", "one", false).unwrap();
    assert_eq!(detail["revisions"].as_array().unwrap().len(), 4);
    assert_eq!(detail["heads"].as_array().unwrap().len(), 1);
    assert_eq!(detail["verification"]["valid"], true);
    let mut stale = request.clone();
    stale["mutationId"] = json!("stale");
    assert!(f.store.evidence_resolve("tenant-a", &stale).is_err());
}

#[test]
fn detects_missing_and_tampered_batch_commitments() {
    let f = Fixture::new();
    f.store
        .evidence_save("tenant-a", vec![input("one")], "one")
        .unwrap();
    {
        let conn = f.store.conn.lock().unwrap();
        conn.execute("UPDATE observation_evidence_batches SET payload_json=json_set(payload_json,'$.nonce','tampered')", []).unwrap();
    }
    let detail = f.store.evidence_detail("tenant-a", "one", false).unwrap();
    assert!(detail["verification"]["errors"]
        .as_array()
        .unwrap()
        .contains(&json!("batch_hash_mismatch")));
    {
        let conn = f.store.conn.lock().unwrap();
        conn.execute("DELETE FROM observation_evidence_batches", [])
            .unwrap();
    }
    let detail = f.store.evidence_detail("tenant-a", "one", false).unwrap();
    assert!(detail["verification"]["errors"]
        .as_array()
        .unwrap()
        .contains(&json!("batch_missing")));
}

#[test]
fn normal_evidence_generation_round_trip() {
    let f = Fixture::new();
    crate::shared_archive::with_test_root(&f.dir.join("archives"), || {
        let tenant = "tenant-a";
        let source = SqliteStore::open(f.dir.join("source/store.sqlite")).unwrap();
        let target = SqliteStore::open(f.dir.join("target/store.sqlite")).unwrap();
        let backup_root = f.dir.join("transport");
        for store in [&source, &target] {
            crate::backup::set_folder(store, tenant.into(), backup_root.to_string_lossy().into()).unwrap();
        }
        let first = source.evidence_save(tenant, vec![input("one"), input("two")], "initial").unwrap();
        let mut correction = input("one");
        correction["expectedRevisionId"] = first[0]["revisionId"].clone();
        correction["correctionReason"] = json!("합성 시험 정정");
        correction["note"] = json!("합성 시험 정정 기록");
        let corrected = source.evidence_save(tenant, vec![correction], "correction").unwrap();
        let snapshot = crate::backup::run_with_kind(&source, tenant.into(), "auto_sync", Some(1)).unwrap();
        assert_eq!(snapshot["ok"], true);
        assert_eq!(snapshot["snapshotVersion"], 5);
        let manifest_path = Path::new(snapshot["manifestPath"].as_str().unwrap());
        let result = crate::backup::restore_generation(&target, tenant, manifest_path, 1, "announced", false).unwrap();
        assert_eq!(result["applied"], true);
        {
            let conn = target.conn.lock().unwrap();
            assert_eq!(read_record(&conn, tenant, "one").unwrap(), Some(corrected[0].clone()));
            assert_eq!(read_record(&conn, tenant, "two").unwrap(), Some(first[1].clone()));
            assert_eq!(revisions(&conn, tenant, "one").unwrap().len(), 2);
            assert_eq!(revisions(&conn, tenant, "two").unwrap().len(), 1);
            assert_eq!(conn.query_row("SELECT COUNT(*) FROM local_store_restore_journal", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        }
        assert_eq!(crate::backup::local_sync_state(&target, tenant).unwrap().applied_generation, 1);
        target.restore_ready(tenant).unwrap();
        let replay = crate::backup::restore_generation(&target, tenant, manifest_path, 1, "announced", false).unwrap();
        assert_eq!(replay["applied"], false);
    });
}

#[test]
fn legacy_projection_import_without_ledger_cannot_be_backed_up_or_restored() {
    let f = Fixture::new();
    crate::shared_archive::with_test_root(&f.dir.join("archives"), || {
        let tenant = "tenant-a";
        let legacy = input("legacy");
        let projection = {
            let conn = f.store.conn.lock().unwrap();
            conn.execute("INSERT INTO lesson_observations VALUES(?1,'legacy','2026-09-08',0,'S1',?2,1)",
                params![tenant, legacy.to_string()]).unwrap();
            ensure_schema(&conn, &f.dir).unwrap();
            read_record(&conn, tenant, "legacy").unwrap().unwrap()
        };
        assert_eq!(projection["evidenceBaseline"], "legacy_unverified");
        let source_path = f.dir.join("source/store.sqlite");
        {
            let old_import = SqliteStore::open(source_path.clone()).unwrap();
            // Model the pre-evidence helper's projection-only import, using only
            // a legitimately generated synthetic baseline and no original files.
            old_import.conn.lock().unwrap().execute("INSERT INTO lesson_observations VALUES(?1,'legacy','2026-09-08',0,'S1',?2,?3)",
                params![tenant, projection.to_string(), projection["updatedAtMs"].as_i64().unwrap()]).unwrap();
        }
        let source = SqliteStore::open(source_path).unwrap();
        assert_eq!(source.conn.lock().unwrap().query_row("SELECT COUNT(*) FROM observation_evidence_revisions", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        let target = SqliteStore::open(f.dir.join("target/store.sqlite")).unwrap();
        let sentinel = target.evidence_save(tenant, vec![input("sentinel")], "sentinel").unwrap();
        let backup_root = f.dir.join("transport");
        for store in [&source, &target] {
            crate::backup::set_folder(store, tenant.into(), backup_root.to_string_lossy().into()).unwrap();
        }
        let snapshot = crate::backup::run_with_kind(&source, tenant.into(), "auto_sync", Some(1));
        assert_eq!(snapshot.unwrap_err(), "observation_evidence_restore_invalid");
        {
            let conn = target.conn.lock().unwrap();
            // Historical orphan DBs must still fail the same restore validator;
            // the new capture guard no longer creates such snapshots for the test.
            conn.execute("ATTACH DATABASE ?1 AS restore", params![source.db_path.to_string_lossy().to_string()]).unwrap();
            assert_eq!(check_restore(&conn, tenant).unwrap_err(), "observation_evidence_restore_invalid");
            conn.execute_batch("DETACH DATABASE restore").unwrap();
            assert_eq!(read_record(&conn, tenant, "sentinel").unwrap(), Some(sentinel[0].clone()));
            assert_eq!(read_record(&conn, tenant, "legacy").unwrap(), None);
            assert_eq!(conn.query_row("SELECT COUNT(*) FROM local_store_restore_journal", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        }
        assert_eq!(crate::backup::local_sync_state(&target, tenant).unwrap().applied_generation, 0);
        target.restore_ready(tenant).unwrap();
    });
}

#[test]
#[ignore = "requires explicitly prepared, isolated recovery copies and CAM_RECOVERY_VALIDATION_ROOT"]
fn prepared_recovery_copies_pass_actual_restore_validation() -> Result<(), &'static str> {
    // This opt-in diagnostic never opens SqliteStore, migrates a source, or reads
    // the application's data directory. Only memory receives a fresh schema.
    fn attached_copy(root: &Path, name: &str) -> Result<Connection, &'static str> {
        let file = root.join(name);
        let metadata = fs::symlink_metadata(&file).map_err(|_| "prepared_copy_missing")?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("prepared_copy_not_regular_file");
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err("prepared_copy_reparse_point");
            }
        }
        let file = fs::canonicalize(file).map_err(|_| "prepared_copy_unavailable")?;
        if file.parent() != Some(root) {
            return Err("prepared_copy_outside_case_root");
        }
        let mut uri = url::Url::from_file_path(&file).map_err(|_| "prepared_copy_uri_invalid")?;
        uri.query_pairs_mut().append_pair("mode", "ro");
        let conn = Connection::open_with_flags(
            ":memory:",
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                | rusqlite::OpenFlags::SQLITE_OPEN_URI
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| "validation_memory_open_failed")?;
        conn.execute_batch(&schema(""))
            .map_err(|_| "validation_memory_schema_failed")?;
        conn.execute("ATTACH DATABASE ?1 AS restore", params![uri.as_str()])
            .map_err(|_| "prepared_copy_attach_failed")?;
        if !conn
            .is_readonly(rusqlite::DatabaseName::Attached("restore"))
            .map_err(|_| "prepared_copy_readonly_check_failed")?
        {
            return Err("prepared_copy_not_readonly");
        }
        conn.execute_batch("PRAGMA query_only=ON; BEGIN")
            .map_err(|_| "validation_read_transaction_failed")?;
        Ok(conn)
    }

    let root = std::env::var_os("CAM_RECOVERY_VALIDATION_ROOT")
        .ok_or("prepared_recovery_root_required")?;
    let root = std::path::PathBuf::from(root);
    if !root.is_absolute() {
        return Err("prepared_recovery_root_must_be_absolute");
    }
    let root = fs::canonicalize(root).map_err(|_| "prepared_recovery_root_unavailable")?;
    let before = attached_copy(&root, "windows-before-v4.sqlite")?;
    let tenants = {
        let mut query = before.prepare(
            "SELECT o.tenant_id FROM restore.lesson_observations o
             LEFT JOIN restore.observation_evidence_revisions r
               ON r.tenant_id=o.tenant_id
              AND r.revision_id=json_extract(o.payload_json,'$.revisionId')
             WHERE json_extract(o.payload_json,'$.evidenceVersion')=1
               AND r.revision_id IS NULL",
        ).map_err(|_| "prepared_orphan_query_failed")?;
        let rows = query.query_map([], |row| row.get::<_, String>(0))
            .map_err(|_| "prepared_orphan_query_failed")?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|_| "prepared_orphan_read_failed")?
    };
    if tenants.len() != 1 || tenants[0].is_empty() {
        return Err("prepared_orphan_scope_changed");
    }
    let tenant = &tenants[0];
    if !matches!(check_restore(&before, tenant), Err(error) if error == "observation_evidence_restore_invalid") {
        return Err("prepared_before_did_not_reproduce_expected_rejection");
    }
    let after = attached_copy(&root, "windows-rehearsal-v4.sqlite")?;
    let count_sql = "SELECT COUNT(*) FROM restore.lesson_observations
                     WHERE tenant_id=?1 AND json_extract(payload_json,'$.evidenceVersion')=1";
    let before_count: i64 = before.query_row(count_sql, params![tenant], |row| row.get(0))
        .map_err(|_| "prepared_before_count_failed")?;
    let after_count: i64 = after.query_row(count_sql, params![tenant], |row| row.get(0))
        .map_err(|_| "prepared_after_count_failed")?;
    if before_count < 1 || before_count != after_count {
        return Err("prepared_projection_count_changed");
    }
    check_restore(&after, tenant).map_err(|_| "prepared_recovery_still_rejected")?;
    // Fixed error codes only: never format a row, tenant, revision, or raw error.
    Ok(())
}
