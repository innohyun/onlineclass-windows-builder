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
