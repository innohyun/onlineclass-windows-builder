use crate::{normalize_json_text, normalize_student_code, normalize_tenant_id, SqliteStore};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::Utc;
use rusqlite::params;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

const MAX_PHOTO_BYTES: usize = 1024 * 1024;
const MAX_PHOTO_BASE64_CHARS: usize = 1_398_104;

#[derive(Debug)]
struct NormalizedStudentPrivatePhoto {
    tenant_id: String,
    student_code: String,
    payload: Value,
    updated_at_ms: i64,
}

fn normalize_photo(input: Value) -> Result<NormalizedStudentPrivatePhoto, String> {
    let source = input
        .as_object()
        .ok_or_else(|| "invalid_record".to_string())?;
    let tenant_id = normalize_tenant_id(source.get("tenantId"));
    let student_code = normalize_student_code(source.get("studentCode"));
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    if student_code.is_empty() {
        return Err("student_code_required".to_string());
    }
    let content_type = normalize_json_text(source.get("contentType"), 80).to_lowercase();
    if content_type != "image/webp" {
        return Err("student_photo_content_type_invalid".to_string());
    }
    let content_base64 = source
        .get("contentBase64")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if content_base64.is_empty() || content_base64.len() > MAX_PHOTO_BASE64_CHARS {
        return Err("student_photo_data_invalid".to_string());
    }
    let decoded = BASE64_STANDARD
        .decode(content_base64.as_bytes())
        .map_err(|_| "student_photo_data_invalid".to_string())?;
    if decoded.is_empty() || decoded.len() > MAX_PHOTO_BYTES {
        return Err("student_photo_size_invalid".to_string());
    }
    let byte_size = source.get("byteSize").and_then(Value::as_i64).unwrap_or(0);
    if byte_size != decoded.len() as i64 {
        return Err("student_photo_size_invalid".to_string());
    }
    let sha256 = normalize_json_text(source.get("sha256"), 64).to_lowercase();
    let actual_sha256 = format!("{:x}", Sha256::digest(&decoded));
    if sha256.len() != 64
        || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || sha256 != actual_sha256
    {
        return Err("student_photo_digest_mismatch".to_string());
    }
    let updated_at_ms = Utc::now().timestamp_millis();
    let mut payload = Map::new();
    payload.insert("tenantId".to_string(), Value::String(tenant_id.clone()));
    payload.insert(
        "studentCode".to_string(),
        Value::String(student_code.clone()),
    );
    payload.insert("contentType".to_string(), Value::String(content_type));
    payload.insert("contentBase64".to_string(), Value::String(content_base64));
    payload.insert("byteSize".to_string(), Value::Number(byte_size.into()));
    payload.insert("sha256".to_string(), Value::String(sha256));
    payload.insert(
        "updatedAtMs".to_string(),
        Value::Number(updated_at_ms.into()),
    );
    Ok(NormalizedStudentPrivatePhoto {
        tenant_id,
        student_code,
        payload: Value::Object(payload),
        updated_at_ms,
    })
}

impl SqliteStore {
    pub(crate) fn upsert_student_private_photo(&self, input: Value) -> Result<Value, String> {
        let record = normalize_photo(input)?;
        let payload_json = serde_json::to_string(&record.payload)
            .map_err(|e| format!("payload_encode_failed:{e}"))?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT INTO student_private_photos (tenant_id, student_code, payload_json, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(tenant_id, student_code) DO UPDATE SET
               payload_json = excluded.payload_json,
               updated_at_ms = excluded.updated_at_ms",
            params![record.tenant_id, record.student_code, payload_json, record.updated_at_ms],
        )
        .map_err(|e| format!("db_student_private_photo_upsert_failed:{e}"))?;
        Ok(record.payload)
    }

    pub(crate) fn get_student_private_photo(
        &self,
        tenant_id: String,
        student_code: String,
    ) -> Result<Option<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        let safe_student_code = normalize_student_code(Some(&Value::String(student_code)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        if safe_student_code.is_empty() {
            return Err("student_code_required".to_string());
        }
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        match conn.query_row(
            "SELECT payload_json FROM student_private_photos WHERE tenant_id = ?1 AND student_code = ?2",
            params![safe_tenant, safe_student_code],
            |row| row.get::<_, String>(0),
        ) {
            Ok(payload_json) => serde_json::from_str(&payload_json)
                .map(Some)
                .map_err(|e| format!("db_student_private_photo_decode_failed:{e}")),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(format!("db_student_private_photo_query_failed:{error}")),
        }
    }

    pub(crate) fn delete_student_private_photo(
        &self,
        tenant_id: String,
        student_code: String,
    ) -> Result<bool, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        let safe_student_code = normalize_student_code(Some(&Value::String(student_code)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        if safe_student_code.is_empty() {
            return Err("student_code_required".to_string());
        }
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "DELETE FROM student_private_photos WHERE tenant_id = ?1 AND student_code = ?2",
            params![safe_tenant, safe_student_code],
        )
        .map(|count| count > 0)
        .map_err(|e| format!("db_student_private_photo_delete_failed:{e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::random_url_token;
    use serde_json::json;
    use std::{env, fs};

    #[test]
    fn round_trip_validates_and_stays_tenant_scoped() {
        let directory = env::temp_dir().join(format!(
            "onlineclass-student-photo-test-{}",
            random_url_token()
        ));
        let store = SqliteStore::open(directory.join("test.sqlite")).expect("open local store");
        let bytes = [1_u8, 2, 3];
        let saved = store
            .upsert_student_private_photo(json!({
                "tenantId": "tenant-a",
                "studentCode": "student01",
                "contentType": "image/webp",
                "contentBase64": BASE64_STANDARD.encode(bytes),
                "byteSize": bytes.len(),
                "sha256": format!("{:x}", Sha256::digest(bytes))
            }))
            .expect("save student photo");
        assert_eq!(
            saved.get("studentCode").and_then(Value::as_str),
            Some("STUDENT01")
        );
        assert_eq!(
            store
                .get_student_private_photo("tenant-a".to_string(), "STUDENT01".to_string())
                .expect("get student photo")
                .and_then(|record| record.get("byteSize").and_then(Value::as_i64)),
            Some(3)
        );
        assert!(store
            .get_student_private_photo("tenant-b".to_string(), "STUDENT01".to_string())
            .expect("other tenant lookup")
            .is_none());
        assert!(store
            .delete_student_private_photo("tenant-a".to_string(), "STUDENT01".to_string())
            .expect("delete student photo"));
        drop(store);
        fs::remove_dir_all(&directory).expect("remove student photo test directory");
    }

    #[test]
    fn rejects_digest_and_size_mismatch() {
        assert_eq!(
            normalize_photo(json!({
                "tenantId": "tenant-a",
                "studentCode": "STUDENT01",
                "contentType": "image/webp",
                "contentBase64": "AQID",
                "byteSize": 3,
                "sha256": "0".repeat(64)
            }))
            .unwrap_err(),
            "student_photo_digest_mismatch"
        );
        assert_eq!(
            normalize_photo(json!({
                "tenantId": "tenant-a",
                "studentCode": "STUDENT01",
                "contentType": "image/webp",
                "contentBase64": "AQID",
                "byteSize": 2,
                "sha256": format!("{:x}", Sha256::digest([1_u8, 2, 3]))
            }))
            .unwrap_err(),
            "student_photo_size_invalid"
        );
    }
}
