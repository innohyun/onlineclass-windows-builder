use super::*;
use base64::Engine;

struct Fixture {
    store: SqliteStore,
    dir: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "classaimate-material-assets-{}",
            crate::random_url_token()
        ));
        fs::create_dir_all(&dir).unwrap();
        let store = SqliteStore::open(dir.join("store.sqlite3")).unwrap();
        store.upsert_work_note(json!({"tenantId":"tenant-a","pageId":"parent-a","title":"자료","properties":{},"blocks":[],"markdown":"","updatedAtMs":10,"createdAtMs":10})).unwrap();
        Self { store, dir }
    }
    fn count(&self, table: &str) -> i64 {
        self.store
            .conn
            .lock()
            .unwrap()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }
    fn file(&self, asset: &str) -> PathBuf {
        self.dir
            .join("work-note-attachments")
            .join(&sha(b"tenant-a")[..32])
            .join(asset)
            .join("content.png")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(mut connection) = self.store.conn.lock() {
            let old = std::mem::replace(&mut *connection, Connection::open_in_memory().unwrap());
            drop(old);
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn png() -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aOuoAAAAASUVORK5CYII=").unwrap()
}
fn seal(mut data: Value) -> Value {
    data["contentSha256"] = json!(crate::sha256_json(&json!({"properties":data["properties"],"blocks":data["blocks"],"markdown":data["markdown"]})).unwrap());
    data
}
fn manifest() -> Value {
    let attachments: Vec<Value> = (0..2).map(|index| json!({"assetId":format!("asset-{index}"),"blockId":format!("block-{index}"),"fileName":format!("그림{index}.png"),"contentType":"image/png","size":png().len(),"sha256":sha(&png()),"objectKey":format!("private/mcp/{index}")})).collect();
    let mut blocks = vec![json!({"id":"text","type":"text","text":"원문 안내"})];
    for asset in &attachments {
        blocks.push(json!({"id":asset["blockId"],"type":"attachment","attachmentId":asset["assetId"],"kind":"image","fileName":asset["fileName"],"contentType":asset["contentType"],"size":asset["size"],"displayMode":"preview"}));
    }
    seal(
        json!({"mode":"create","workspace":"work_materials","pageRef":format!("mcp_{}","a".repeat(32)),"parentPageRef":"parent-a","parentRevision":10,"expectedRevision":10,
        "title":"ChatGPT 초안 · 안내","markdown":"원문 안내","properties":{"tags":[],"sourceType":"classAimatePublicMcp","sourceLabel":"내 ChatGPT","teacherReviewRequired":true},"blocks":blocks,"attachments":attachments}),
    )
}
fn request(data: Value, receipt: &str) -> Value {
    json!({"tenantId":"tenant-a","receiptId":receipt,"operation":"materials_apply_images","requestSha256":crate::sha256_json(&data).unwrap(),"data":data})
}
fn assets(data: &Value) -> HashMap<String, Vec<u8>> {
    data["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|asset| (text(asset, "assetId").into(), png()))
        .collect()
}

#[test]
fn new_page_and_images_commit_atomically_and_replay_without_download() {
    for workspace in ["work_materials", "lesson_materials"] {
        let fixture = Fixture::new();
        let mut data = manifest();
        data["workspace"] = json!(workspace);
        let request = request(data.clone(), "receipt-a");
        let saved =
            super::super::apply_with_assets(&fixture.store, &request, &assets(&data)).unwrap();
        assert_eq!(saved["result"], data);
        assert_eq!(saved["replayed"], false);
        assert_eq!(fixture.count("work_note_attachments"), 2);
        assert_eq!(fixture.count("classaimate_mcp_local_write_receipts"), 1);
        assert_eq!(fs::read(fixture.file("asset-0")).unwrap(), png());
        assert_eq!(
            super::super::verified_replay(&fixture.store, &request)
                .unwrap()
                .unwrap()["replayed"],
            true
        );
        assert_eq!(
            super::super::apply(&fixture.store, &request).unwrap()["replayed"],
            true
        );
        assert_eq!(fixture.count("work_note_pages"), 2);
    }
}

#[test]
fn one_bad_file_creates_no_canonical_page_or_staged_file() {
    let fixture = Fixture::new();
    let data = manifest();
    let mut files = assets(&data);
    files.insert("asset-1".into(), vec![0]);
    assert_eq!(
        super::super::apply_with_assets(&fixture.store, &request(data, "receipt-a"), &files)
            .unwrap_err(),
        "IMAGE_INTEGRITY_FAILED"
    );
    assert_eq!(fixture.count("work_note_pages"), 1);
    assert_eq!(fixture.count("work_note_attachments"), 0);
    assert!(!fixture.file("asset-0").exists());
}

#[test]
fn receipt_failure_rolls_back_page_fts_and_attachments_and_removes_only_new_files() {
    let fixture = Fixture::new();
    let data = manifest();
    fixture.store.conn.lock().unwrap().execute_batch("CREATE TRIGGER reject_image_receipt BEFORE INSERT ON classaimate_mcp_local_write_receipts BEGIN SELECT RAISE(ABORT,'receipt_fail'); END").unwrap();
    assert!(super::super::apply_with_assets(
        &fixture.store,
        &request(data.clone(), "receipt-a"),
        &assets(&data)
    )
    .unwrap_err()
    .contains("receipt_fail"));
    assert_eq!(fixture.count("work_note_pages"), 1);
    assert_eq!(fixture.count("work_note_pages_fts"), 1);
    assert_eq!(fixture.count("work_note_attachments"), 0);
    assert_eq!(fixture.count("classaimate_mcp_local_write_receipts"), 0);
    assert!(!fixture.file("asset-0").exists());
    assert!(!fixture.file("asset-1").exists());
}

#[test]
fn stale_parent_does_not_publish_any_new_page() {
    let fixture = Fixture::new();
    let mut data = manifest();
    data["parentRevision"] = json!(9);
    data["expectedRevision"] = json!(9);
    assert_eq!(
        super::super::apply_with_assets(
            &fixture.store,
            &request(data.clone(), "receipt-a"),
            &assets(&data)
        )
        .unwrap_err(),
        "MATERIAL_REVISION_CONFLICT"
    );
    assert_eq!(fixture.count("work_note_pages"), 1);
    assert!(!fixture.file("asset-0").exists());
}

#[test]
fn append_preserves_existing_blocks_and_metadata_and_rejects_stale_revision() {
    let fixture = Fixture::new();
    let data = manifest();
    super::super::apply_with_assets(
        &fixture.store,
        &request(data.clone(), "receipt-a"),
        &assets(&data),
    )
    .unwrap();
    let saved = fixture
        .store
        .get_work_note("tenant-a".into(), text(&data, "pageRef").into())
        .unwrap()
        .unwrap();
    let mut next = data.clone();
    next["mode"] = json!("append");
    next["expectedRevision"] = saved["updatedAtMs"].clone();
    let mut asset = data["attachments"][0].clone();
    asset["assetId"] = json!("asset-next");
    asset["blockId"] = json!("block-next");
    next["attachments"] = json!([asset]);
    let mut block = data["blocks"][1].clone();
    block["id"] = json!("block-next");
    block["attachmentId"] = json!("asset-next");
    next["blocks"].as_array_mut().unwrap().push(block);
    next = seal(next);
    super::super::apply_with_assets(
        &fixture.store,
        &request(next.clone(), "receipt-next"),
        &assets(&next),
    )
    .unwrap();
    assert_eq!(fixture.count("work_note_attachments"), 3);
    assert_eq!(
        fixture
            .store
            .get_work_note("tenant-a".into(), text(&data, "pageRef").into())
            .unwrap()
            .unwrap()["blocks"],
        next["blocks"]
    );
    let mut stale = next.clone();
    stale["attachments"][0]["assetId"] = json!("asset-stale");
    stale["attachments"][0]["blockId"] = json!("block-stale");
    let last = stale["blocks"].as_array_mut().unwrap().last_mut().unwrap();
    last["id"] = json!("block-stale");
    last["attachmentId"] = json!("asset-stale");
    stale = seal(stale);
    assert_eq!(
        super::super::apply_with_assets(
            &fixture.store,
            &request(stale.clone(), "receipt-stale"),
            &assets(&stale)
        )
        .unwrap_err(),
        "MATERIAL_REVISION_CONFLICT"
    );
}

#[test]
fn replay_detects_canonical_file_tampering_and_never_reapplies() {
    let fixture = Fixture::new();
    let data = manifest();
    let input = request(data.clone(), "receipt-a");
    super::super::apply_with_assets(&fixture.store, &input, &assets(&data)).unwrap();
    fs::write(fixture.file("asset-0"), vec![0; png().len()]).unwrap();
    assert_eq!(
        super::super::verified_replay(&fixture.store, &input).unwrap_err(),
        "IMAGE_INTEGRITY_FAILED"
    );
    assert_eq!(fixture.count("work_note_pages"), 2);
    assert_eq!(fixture.count("classaimate_mcp_local_write_receipts"), 1);
}

#[test]
fn existing_file_is_not_replaced_and_paths_and_body_digest_are_validated() {
    let fixture = Fixture::new();
    let data = manifest();
    let target = fixture.file("asset-0");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, b"original").unwrap();
    assert_eq!(
        super::super::apply_with_assets(
            &fixture.store,
            &request(data.clone(), "receipt-a"),
            &assets(&data)
        )
        .unwrap_err(),
        "IMAGE_INTEGRITY_FAILED"
    );
    assert_eq!(fs::read(target).unwrap(), b"original");
    let mut bad = data.clone();
    bad["attachments"][0]["assetId"] = json!("../escape");
    assert_eq!(validate(&bad).unwrap_err(), invalid());
    let mut tampered = data;
    tampered["markdown"] = json!("changed");
    assert_eq!(validate(&tampered).unwrap_err(), "IMAGE_INTEGRITY_FAILED");
}

#[cfg(unix)]
#[test]
fn symlink_directory_cannot_escape_the_canonical_attachment_root() {
    let fixture = Fixture::new();
    let data = manifest();
    let external = fixture.dir.join("external");
    fs::create_dir(&external).unwrap();
    std::os::unix::fs::symlink(&external, fixture.dir.join("work-note-attachments")).unwrap();
    assert_eq!(
        super::super::apply_with_assets(
            &fixture.store,
            &request(data.clone(), "receipt-a"),
            &assets(&data)
        )
        .unwrap_err(),
        "IMAGE_SOURCE_NOT_ALLOWED"
    );
    assert_eq!(fixture.count("work_note_pages"), 1);
}

#[test]
fn append_delta_preserves_hidden_properties_links_mentions_and_long_raw_markdown() {
    let fixture = Fixture::new();
    let raw = format!(
        "https://example.com/result?student=1&key=abc\n@원문멘션\n{}",
        "긴 원문".repeat(50_000)
    );
    let properties = json!({"tags":["기존"],"hidden":{"teacherOnly":true}});
    fixture.store.upsert_work_note(json!({"tenantId":"tenant-a","pageId":"parent-a","title":"자료","properties":properties,"blocks":[],"markdown":raw,"updatedAtMs":10})).unwrap();
    let mut data = manifest();
    data["mode"] = json!("append");
    data["pageRef"] = json!("parent-a");
    data["title"] = json!("자료");
    data["properties"] = json!({});
    data["markdown"] = json!("[링크 제거] @별칭\n미리보기만");
    data["appendText"] = json!("새 설명");
    data = seal(data);
    let input = request(data.clone(), "receipt-delta");
    let saved = super::super::apply_with_assets(&fixture.store, &input, &assets(&data)).unwrap();
    let readback = fixture
        .store
        .get_work_note("tenant-a".into(), "parent-a".into())
        .unwrap()
        .unwrap();
    assert_eq!(readback["markdown"], format!("{raw}\n\n새 설명"));
    assert_eq!(readback["properties"], properties);
    assert_eq!(saved["result"], data);
    assert!(!serde_json::to_string(&saved)
        .unwrap()
        .contains("teacherOnly"));
    assert_eq!(
        super::super::apply(&fixture.store, &input).unwrap()["replayed"],
        true
    );
}
