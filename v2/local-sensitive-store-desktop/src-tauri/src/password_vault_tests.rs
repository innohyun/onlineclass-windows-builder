use super::*;

#[test]
fn schema_keeps_personal_payload_encrypted_and_owner_scoped() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    ensure_schema(&conn).unwrap();
    let columns = conn
        .prepare("PRAGMA table_info(password_vault_personal_entries)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(columns.contains(&"ciphertext_json".to_string()));
    assert!(!columns
        .iter()
        .any(|column| matches!(column.as_str(), "password" | "username" | "service_name")));
    assert!(columns.contains(&"owner_uid".to_string()));
}

#[test]
fn correction_requires_reason_before_encryption() {
    assert!(validate_shared_plaintext(
        "correction",
        &json!({ "reason": "잘못된 비밀번호입니다.", "proposed": null })
    )
    .is_ok());
    assert_eq!(
        validate_shared_plaintext("correction", &json!({ "reason": "" })).unwrap_err(),
        "password_vault_correction_reason_invalid"
    );
}

#[test]
fn recovery_rotates_entry_ciphertext_revision_and_key_version() {
    let old_key = crypto::random_key();
    let new_key = crypto::random_key();
    let plain = json!({ "serviceName": "업무 포털", "username": "teacher", "password": "secret", "url": "", "note": "" });
    let encrypted = crypto::encrypt_json(
        &old_key,
        &shared_aad("1234567", 1, "entry", "entry-0001", 2),
        &plain,
    )
    .unwrap();
    let rows = json!([{ "entryId": "entry-0001", "revision": 2, "ciphertext": encrypted }]);
    let rotated = rotate_records(
        Some(&rows),
        "entryId",
        "entry",
        "1234567",
        1,
        2,
        &old_key,
        &new_key,
    )
    .unwrap();
    let decoded = crypto::decrypt_json(
        &new_key,
        &shared_aad("1234567", 2, "entry", "entry-0001", 3),
        rotated[0]["ciphertext"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(decoded["password"], "secret");
    assert_eq!(rotated[0]["expectedRevision"], 2);
}

#[test]
fn redacted_shared_plaintext_can_drop_password_without_changing_ciphertext_validation() {
    let mut value = json!({ "serviceName": "포털", "username": "teacher", "password": "secret", "url": "", "note": "" });
    validate_shared_plaintext("entry", &value).unwrap();
    let object = value.as_object_mut().unwrap();
    object.remove("password");
    object.insert("passwordSet".to_string(), Value::Bool(true));
    assert_eq!(value.get("password"), None);
    assert_eq!(value["passwordSet"], true);
}
