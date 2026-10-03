use super::*;
use std::path::PathBuf;

fn fixture() -> SqliteStore {
    let conn = Connection::open_in_memory().unwrap();
    super::super::ensure_schema(&conn).unwrap();
    SqliteStore::from_connection(conn, PathBuf::from("unused.sqlite"), std::env::temp_dir())
}
fn principal(tenant: &str, owner: &str) -> BrowserLinkToken {
    BrowserLinkToken {
        tenant_id: tenant.into(),
        uid: owner.into(),
        token: "test-authorized-token".into(),
        account_email: String::new(),
        account_display_name: String::new(),
        tenant_name: String::new(),
        audience: String::new(),
        created_at_ms: 1,
        last_used_at_ms: 1,
    }
}
fn profile(store: &SqliteStore, tenant: &str, owner: &str, school: &str) {
    store.conn.lock().unwrap().execute("INSERT INTO password_vault_personal_profiles VALUES(?1,?2,?3,'wrapped-original-key',1,1,1)",params![tenant,owner,school]).unwrap();
}
#[test]
fn same_owner_school_reuses_original_ciphertext_and_keyring_account_without_copying() {
    let store = fixture();
    let previous = principal("class-2026", "teacher-a");
    let current = principal("class-2027", "teacher-a");
    profile(&store, &previous.tenant_id, &previous.uid, "school-a");
    let key = [7u8; 32];
    let plain = json!({"serviceName":"test","username":"fixture","password":"fixture-only"});
    let cipher = crypto::encrypt_json(
        &key,
        &personal_entry_aad(&previous, "school-a", "entry-one", 1),
        &plain,
    )
    .unwrap();
    store.conn.lock().unwrap().execute("INSERT INTO password_vault_personal_entries VALUES('class-2026','teacher-a','school-a','entry-one','other',?1,1,1,1)",[&cipher]).unwrap();
    let origin = resolve(&store, &current, "school-a").unwrap();
    assert_eq!(origin.tenant_id, "class-2026");
    assert_eq!(current.tenant_id, "class-2027");
    assert_eq!(
        personal_account(&origin, "school-a"),
        personal_account(&previous, "school-a")
    );
    assert_eq!(
        personal_aad(&origin, "school-a"),
        personal_aad(&previous, "school-a")
    );
    assert_eq!(
        crypto::decrypt_json(
            &key,
            &personal_entry_aad(&origin, "school-a", "entry-one", 1),
            &cipher
        )
        .unwrap(),
        plain
    );
    assert!(crypto::decrypt_json(
        &key,
        &personal_entry_aad(&current, "school-a", "entry-one", 1),
        &cipher
    )
    .is_err());
    let conn = store.conn.lock().unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM password_vault_personal_profiles",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM password_vault_personal_entries WHERE tenant_id='class-2027'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT ciphertext_json FROM password_vault_personal_entries",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        cipher
    );
}
#[test]
fn different_owner_or_school_does_not_resolve_the_previous_personal_vault() {
    let store = fixture();
    profile(&store, "class-2026", "teacher-a", "school-a");
    assert_eq!(
        resolve(&store, &principal("class-2027", "teacher-b"), "school-a")
            .unwrap()
            .tenant_id,
        "class-2027"
    );
    assert_eq!(
        resolve(&store, &principal("class-2027", "teacher-a"), "school-b")
            .unwrap()
            .tenant_id,
        "class-2027"
    );
    assert!(resolve(&store, &principal("../other", "teacher-a"), "school-a").is_err());
    assert!(resolve(&store, &principal("class-2027", "teacher-a"), "school/a").is_err());
}
#[test]
fn ambiguous_legacy_origins_fail_closed_and_current_vault_keeps_its_scope() {
    let store = fixture();
    profile(&store, "class-2025", "teacher-a", "school-a");
    profile(&store, "class-2026", "teacher-a", "school-a");
    let current = principal("class-2027", "teacher-a");
    assert_eq!(
        resolve(&store, &current, "school-a").unwrap_err(),
        "password_vault_personal_origin_ambiguous"
    );
    profile(&store, "class-2027", "teacher-a", "school-a");
    assert_eq!(
        resolve(&store, &current, "school-a").unwrap().tenant_id,
        "class-2027"
    );
}
#[test]
fn personal_origin_never_grants_shared_school_device_or_key_rights() {
    let store = fixture();
    profile(&store, "class-2026", "teacher-a", "school-a");
    store.conn.lock().unwrap().execute("INSERT INTO password_vault_shared_local_devices VALUES('class-2026','teacher-a','school-a','old-device','old-public-key',1,'approved',1,1)",[]).unwrap();
    let current = principal("class-2027", "teacher-a");
    let origin = resolve(&store, &current, "school-a").unwrap();
    assert_eq!(origin.tenant_id, "class-2026");
    assert_eq!(
        device_status(&store, &current, "school-a").unwrap()["status"],
        "missing"
    );
    assert_eq!(
        shared_key(&store, &current, "school-a", 1).unwrap_err(),
        "password_vault_device_not_approved"
    );
    assert_ne!(
        device_account(&current, "school-a"),
        device_account(&origin, "school-a")
    );
}
