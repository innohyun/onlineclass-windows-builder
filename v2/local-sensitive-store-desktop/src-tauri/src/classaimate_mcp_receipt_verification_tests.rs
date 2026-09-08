use super::*;
use crate::{classaimate_mcp_write_jobs, SqliteStore};

struct Fixture {
    store: SqliteStore,
    directory: std::path::PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "classaimate-receipt-read-{}",
            crate::random_url_token()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let store = SqliteStore::open(directory.join("store.sqlite")).unwrap();
        store
            .upsert_work_note(
                json!({"tenantId":"tenant-a","pageId":"parent-a","title":"합성 자료",
            "properties":{},"blocks":[],"markdown":"","createdAtMs":10,"updatedAtMs":10}),
            )
            .unwrap();
        Self { store, directory }
    }
    fn changes(&self) -> i64 {
        self.store
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(mut conn) = self.store.conn.lock() {
            let old = std::mem::replace(&mut *conn, Connection::open_in_memory().unwrap());
            drop(old);
        }
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}
fn text_job() -> Value {
    json!({"tenantId":"tenant-a","receiptId":"receipt-a","operation":"work_notes_save_draft","requestSha256":"a".repeat(64),
        "data":{"pageId":"mcp-note","parentPageRef":"parent-a","documentRef":"doc-a","title":"합성 초안",
            "markdown":"합성 보호 본문","blocks":[{"id":"text-a","type":"text","text":"합성 보호 본문"}]}})
}
fn input(job: &Value) -> Value {
    json!({"receiptId":job["receiptId"],"operation":job["operation"],"requestSha256":job["requestSha256"]})
}

#[test]
fn receipt_read_verifies_digest_without_result_disclosure_pruning_or_any_write() {
    let fixture = Fixture::new();
    let job = text_job();
    let saved = classaimate_mcp_write_jobs::apply(&fixture.store, &job).unwrap();
    fixture
        .store
        .conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE classaimate_mcp_local_write_receipts SET created_at_ms=1",
            [],
        )
        .unwrap();
    let before = fixture.changes();
    for _ in 0..3 {
        let result = read_only(&fixture.store, "tenant-a", &input(&job)).unwrap();
        assert_eq!(
            result,
            json!({"status":"saved","requestSha256":job["requestSha256"],
            "resultSha256":crate::sha256_json(&saved["result"]).unwrap(),"localRef":saved["localRef"]})
        );
        assert!(!result.to_string().contains("합성 보호"));
    }
    assert_eq!(fixture.changes(), before);
    assert_eq!(
        read_only(&fixture.store, "tenant-b", &input(&job)).unwrap(),
        json!({"status":"missing"})
    );
    let mut invalid = input(&job);
    invalid["tenantId"] = json!("tenant-a");
    assert_eq!(
        read_only(&fixture.store, "tenant-a", &invalid).unwrap_err(),
        "INVALID_LOCAL_READ_REQUEST"
    );
    let mut changed = input(&job);
    changed["requestSha256"] = json!("b".repeat(64));
    assert_eq!(
        read_only(&fixture.store, "tenant-a", &changed).unwrap_err(),
        "MCP_LOCAL_RECEIPT_CONFLICT"
    );
}

#[test]
fn missing_fts_legacy_receipt_and_database_error_never_become_missing_or_apply() {
    let fixture = Fixture::new();
    let job = text_job();
    classaimate_mcp_write_jobs::apply(&fixture.store, &job).unwrap();
    fixture
        .store
        .conn
        .lock()
        .unwrap()
        .execute(
            "DELETE FROM work_note_pages_fts WHERE page_id='mcp-note'",
            [],
        )
        .unwrap();
    let before = fixture.changes();
    assert_eq!(
        read_only(&fixture.store, "tenant-a", &input(&job)).unwrap_err(),
        "MCP_LOCAL_RECEIPT_CONFLICT"
    );
    assert_eq!(fixture.changes(), before);
    fixture
        .store
        .conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE classaimate_mcp_local_write_receipts SET result_json=?1",
            [job["data"].to_string()],
        )
        .unwrap();
    assert_eq!(
        read_only(&fixture.store, "tenant-a", &input(&job)).unwrap_err(),
        "MCP_LOCAL_RECEIPT_UNSUPPORTED"
    );
    fixture
        .store
        .conn
        .lock()
        .unwrap()
        .execute("DROP TABLE classaimate_mcp_local_write_receipts", [])
        .unwrap();
    assert_eq!(
        read_only(&fixture.store, "tenant-a", &input(&job)).unwrap_err(),
        "MCP_LOCAL_RECEIPT_READ_FAILED"
    );
}

#[test]
fn observation_receipt_checks_all_22_canonical_rows_and_rejects_empty_mutation() {
    let fixture = Fixture::new();
    let items: Vec<Value> = (1..=22).map(|index| json!({"action":"create","studentCode":format!("STU{index:02}"),
        "docId":format!("obs{index:02}"),"baselineRecords":[],"record":{"note":format!("합성 관찰 {index}")}})).collect();
    let job = json!({"tenantId":"tenant-a","receiptId":"receipt-22","operation":"lesson_observations_manage",
        "requestSha256":"a".repeat(64),"data":{"mutationId":"batch-22","scope":{"date":"2026-09-08","period":1,"subject":"수학"},"items":items}});
    classaimate_mcp_write_jobs::apply(&fixture.store, &job).unwrap();
    let before = fixture.changes();
    let result = read_only(&fixture.store, "tenant-a", &input(&job)).unwrap();
    assert_eq!(result["status"], "saved");
    assert_eq!(
        result["resultSha256"],
        crate::sha256_json(&job["data"]).unwrap()
    );
    assert_eq!(fixture.changes(), before);
    fixture
        .store
        .conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE observation_evidence_mutations SET payload_json='[]'",
            [],
        )
        .unwrap();
    assert_eq!(
        read_only(&fixture.store, "tenant-a", &input(&job)).unwrap_err(),
        "MCP_LOCAL_RECEIPT_CONFLICT"
    );
}

#[test]
fn image_receipt_checks_actual_file_bytes_not_only_database_metadata() {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let fixture = Fixture::new();
    let png = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aOuoAAAAASUVORK5CYII=").unwrap();
    let asset = json!({"assetId":"asset-a","blockId":"block-a","fileName":"fixture.png","contentType":"image/png",
        "size":png.len(),"sha256":format!("{:x}",Sha256::digest(&png)),"objectKey":"private/synthetic"});
    let mut data = json!({"mode":"create","workspace":"work_materials","pageRef":format!("mcp_{}","a".repeat(32)),
        "parentPageRef":"parent-a","parentRevision":10,"expectedRevision":10,"title":"ChatGPT 초안 · 합성 그림","markdown":"합성 그림",
        "properties":{"sourceType":"classAimatePublicMcp","sourceLabel":"내 ChatGPT","teacherReviewRequired":true},
        "blocks":[{"id":"block-a","type":"attachment","attachmentId":"asset-a","kind":"image","fileName":"fixture.png",
            "contentType":"image/png","size":png.len(),"displayMode":"preview"}],"attachments":[asset]});
    data["contentSha256"] = json!(crate::sha256_json(&json!({"properties":data["properties"],"blocks":data["blocks"],"markdown":data["markdown"]})).unwrap());
    let job = json!({"tenantId":"tenant-a","receiptId":"receipt-image","operation":"materials_apply_images",
        "requestSha256":crate::sha256_json(&data).unwrap(),"data":data});
    classaimate_mcp_write_jobs::apply_with_assets(
        &fixture.store,
        &job,
        &std::collections::HashMap::from([("asset-a".to_string(), png)]),
    )
    .unwrap();
    let before = fixture.changes();
    assert_eq!(
        read_only(&fixture.store, "tenant-a", &input(&job)).unwrap()["status"],
        "saved"
    );
    let relative: String = fixture
        .store
        .conn
        .lock()
        .unwrap()
        .query_row("SELECT local_path FROM work_note_attachments", [], |row| {
            row.get(0)
        })
        .unwrap();
    std::fs::write(fixture.directory.join(relative), b"broken fixture").unwrap();
    assert_eq!(
        read_only(&fixture.store, "tenant-a", &input(&job)).unwrap_err(),
        "MCP_LOCAL_RECEIPT_CONFLICT"
    );
    assert_eq!(fixture.changes(), before);
}
