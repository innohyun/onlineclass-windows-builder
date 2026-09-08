use super::*;
use std::fs;

fn request(receipt_id: &str, data: Value) -> Value {
    json!({
        "tenantId":"tenant-a",
        "receiptId":receipt_id,
        "operation":"materials_restructure_page",
        "requestSha256":"a".repeat(64),
        "data":data
    })
}

#[test]
fn restructure_page_requires_exact_revision_and_preserves_document_references() {
    let directory = std::env::temp_dir().join(format!(
        "classaimate-mcp-restructure-{}",
        crate::random_url_token()
    ));
    fs::create_dir_all(&directory).expect("create test directory");
    let store = SqliteStore::open(directory.join("store.sqlite3")).expect("open test store");
    let original_blocks = json!([
        {"id":"source","type":"text","content":[{"type":"text","text":"출처","marks":[{"type":"link","attrs":{"href":"https://example.com/source"}}]}]},
        {"id":"file","type":"attachment","localAttachmentId":"attachment-a","fileName":"원본.pdf"}
    ]);
    store
        .upsert_work_note(json!({
            "tenantId":"tenant-a","pageId":"page-a","parentId":null,"title":"원본 제목",
            "emoji":"📄","position":0,"properties":{"source":"teacher"},
            "blocks":original_blocks,"markdown":"원문","createdAtMs":10,"updatedAtMs":10
        }))
        .expect("seed work note");
    let reorganized = json!([
        {"id":"summary","type":"heading","attrs":{"level":1},"content":[{"type":"text","text":"[AI 정리본]"}]},
        {"id":"original","type":"toggle","attrs":{"summary":"원본 보기"},"content":original_blocks}
    ]);
    let data = json!({
        "workspace":"work_materials","pageRef":"page-a","expectedRevision":10,
        "blocks":reorganized,"markdown":"# [AI 정리본]\n\n<details>원본 보기</details>"
    });
    let applied = apply(&store, &request("receipt-a", data.clone())).expect("apply restructure");
    assert_eq!(applied["result"]["appliedFromRevision"], 10);
    let readback = store
        .get_work_note("tenant-a".to_string(), "page-a".to_string())
        .expect("read work note")
        .expect("work note exists");
    assert_eq!(readback["title"], "원본 제목");
    assert_eq!(readback["properties"]["source"], "teacher");
    assert_eq!(readback["blocks"], reorganized);
    assert_eq!(
        apply(&store, &request("receipt-b", data)).expect_err("reject stale revision"),
        "MATERIAL_REVISION_CONFLICT"
    );
    let current_revision = readback["updatedAtMs"].as_i64().expect("current revision");
    let missing_references = json!({
        "workspace":"work_materials","pageRef":"page-a","expectedRevision":current_revision,
        "blocks":[{"id":"summary","type":"text","text":"reference 제거"}],"markdown":"reference 제거"
    });
    assert_eq!(
        apply(&store, &request("receipt-c", missing_references))
            .expect_err("reject removed references"),
        "MATERIAL_REFERENCE_CONFLICT"
    );
    drop(store);
    fs::remove_dir_all(directory).expect("remove test directory");
}

#[test]
fn student_material_save_creates_protected_root_and_update_resets_workflow() {
    let directory = std::env::temp_dir().join(format!(
        "classaimate-mcp-student-material-{}",
        crate::random_url_token()
    ));
    fs::create_dir_all(&directory).expect("create test directory");
    let store = SqliteStore::open(directory.join("store.sqlite3")).expect("open test store");
    let page_id = "mcp_student_activity";
    save_work_note(&store,"tenant-a",&json!({
        "workspace":"student_learning_materials","pageId":page_id,"parentPageRef":STUDENT_MATERIAL_ROOT_ID,
        "documentRef":"document-student-a","title":"태양계 조사 활동지","markdown":"# 태양계 조사 활동지",
        "blocks":[{"type":"paragraph","content":[{"type":"text","text":"행성을 조사합니다."}]}],
        "materialKind":"activity_sheet","workflowStatus":"draft","grade":"5학년","subject":"과학",
        "unit":"2단원","lessonTopic":"태양계의 구성","authorUserId":"teacher-a"
    })).expect("save student material");
    let root = store
        .get_work_note("tenant-a".to_string(), STUDENT_MATERIAL_ROOT_ID.to_string())
        .expect("read student root")
        .expect("student root exists");
    assert_eq!(
        root["properties"]["systemKind"],
        "student_learning_materials_folder"
    );
    let mut material = store
        .get_work_note("tenant-a".to_string(), page_id.to_string())
        .expect("read student material")
        .expect("student material exists");
    assert_eq!(material["parentId"], STUDENT_MATERIAL_ROOT_ID);
    assert_eq!(
        material["properties"]["studentLearningMaterial"]["version"],
        2
    );
    material["properties"]["studentLearningMaterial"]["workflowStatus"] = json!("ready");
    material["updatedAtMs"] = json!(100);
    store
        .upsert_work_note(material)
        .expect("mark material ready");
    update_work_note(
        &store,
        "tenant-a",
        &json!({
            "workspace":"student_learning_materials","pageRef":page_id,"expectedRevision":100,
            "title":"태양계 조사 활동지 수정","markdown":"# 수정한 활동지",
            "blocks":[{"type":"paragraph","content":[{"type":"text","text":"수정했습니다."}]}]
        }),
    )
    .expect("update student material");
    let updated = store
        .get_work_note("tenant-a".to_string(), page_id.to_string())
        .expect("read updated student material")
        .expect("updated student material exists");
    assert_eq!(updated["title"], "태양계 조사 활동지 수정");
    assert_eq!(
        updated["properties"]["studentLearningMaterial"]["workflowStatus"],
        "draft"
    );
    let mut ready = updated;
    ready["properties"]["studentLearningMaterial"]["workflowStatus"] = json!("ready");
    ready["updatedAtMs"] = json!(200);
    store
        .upsert_work_note(ready)
        .expect("mark updated material ready");
    restructure_work_note(&store,"tenant-a",&json!({
        "workspace":"student_learning_materials","pageRef":page_id,"expectedRevision":200,
        "blocks":[{"type":"heading","attrs":{"level":1},"content":[{"type":"text","text":"AI 정리본"}]}],
        "markdown":"# AI 정리본"
    })).expect("restructure student material");
    let restructured = store
        .get_work_note("tenant-a".to_string(), page_id.to_string())
        .expect("read restructured material")
        .expect("restructured material exists");
    assert_eq!(
        restructured["properties"]["studentLearningMaterial"]["workflowStatus"],
        "draft"
    );
    assert_eq!(restructure_work_note(&store,"tenant-a",&json!({
        "workspace":"student_learning_materials","pageRef":STUDENT_MATERIAL_ROOT_ID,"expectedRevision":root["updatedAtMs"],
        "blocks":[{"type":"heading","attrs":{"level":1},"content":[{"type":"text","text":"바뀐 root"}]}],
        "markdown":"# 바뀐 root"
    })).expect_err("protected root cannot be restructured"),"MATERIAL_REVISION_CONFLICT");
    assert_eq!(
        store
            .delete_work_note("tenant-a".to_string(), STUDENT_MATERIAL_ROOT_ID.to_string())
            .expect_err("student root is protected"),
        "work_note_system_folder_protected"
    );
    drop(store);
    fs::remove_dir_all(directory).expect("remove test directory");
}

macro_rules! domain_test_call {
    ($name:ident, $operation:literal) => {
        fn $name(store: &SqliteStore, tenant: &str, data: &Value) -> Result<Value, String> {
            apply(store,&json!({"tenantId":tenant,"receiptId":crate::random_url_token(),"operation":$operation,
                "requestSha256":"a".repeat(64),"data":data}))
        }
    };
}
domain_test_call!(save_work_note, "materials_save_draft");
domain_test_call!(update_work_note, "materials_update_draft");
domain_test_call!(restructure_work_note, "materials_restructure_page");
