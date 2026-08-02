use chrono::Utc;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use url::Url;

const DB_FILE: &str = "onlineclass-shared-archive.sqlite";
const FILE_DIR: &str = "shared-archive-files";

fn paths() -> (PathBuf, PathBuf) {
    let root = super::default_data_dir();
    (root.join(DB_FILE), root.join(FILE_DIR))
}

fn open_db() -> Result<Connection, String> {
    let (path, files) = paths();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("archive_dir_create_failed:{e}"))?;
    }
    fs::create_dir_all(files).map_err(|e| format!("archive_file_dir_create_failed:{e}"))?;
    let connection = Connection::open(path).map_err(|e| format!("archive_db_open_failed:{e}"))?;
    connection
        .execute_batch(
            r#"
      PRAGMA foreign_keys=ON;
      CREATE TABLE IF NOT EXISTS shared_archives(
        id TEXT PRIMARY KEY,tenant_id TEXT NOT NULL,source_type TEXT NOT NULL,source_id TEXT NOT NULL,
        title TEXT NOT NULL,manifest_sha256 TEXT NOT NULL,record_count INTEGER NOT NULL,file_count INTEGER NOT NULL,
        total_file_bytes INTEGER NOT NULL,source_created_at INTEGER NOT NULL,source_expires_at INTEGER NOT NULL,
        imported_at INTEGER NOT NULL,manifest_json TEXT NOT NULL
      ) STRICT;
      CREATE TABLE IF NOT EXISTS shared_archive_records(
        archive_id TEXT NOT NULL,ordinal INTEGER NOT NULL,record_type TEXT NOT NULL,payload_json TEXT NOT NULL,
        payload_sha256 TEXT NOT NULL,PRIMARY KEY(archive_id,ordinal),
        FOREIGN KEY(archive_id) REFERENCES shared_archives(id) ON DELETE RESTRICT
      ) WITHOUT ROWID,STRICT;
      CREATE TABLE IF NOT EXISTS shared_archive_files(
        archive_id TEXT NOT NULL,ordinal INTEGER NOT NULL,original_name TEXT NOT NULL,content_type TEXT NOT NULL,
        byte_size INTEGER NOT NULL,sha256 TEXT NOT NULL,local_path TEXT NOT NULL,
        PRIMARY KEY(archive_id,ordinal),FOREIGN KEY(archive_id) REFERENCES shared_archives(id) ON DELETE RESTRICT
      ) WITHOUT ROWID,STRICT;
      CREATE TRIGGER IF NOT EXISTS shared_archives_no_update BEFORE UPDATE ON shared_archives
        BEGIN SELECT RAISE(ABORT,'shared_archive_read_only'); END;
      CREATE TRIGGER IF NOT EXISTS shared_archives_no_delete BEFORE DELETE ON shared_archives
        BEGIN SELECT RAISE(ABORT,'shared_archive_read_only'); END;
      CREATE TRIGGER IF NOT EXISTS shared_archive_records_no_update BEFORE UPDATE ON shared_archive_records
        BEGIN SELECT RAISE(ABORT,'shared_archive_read_only'); END;
      CREATE TRIGGER IF NOT EXISTS shared_archive_records_no_delete BEFORE DELETE ON shared_archive_records
        BEGIN SELECT RAISE(ABORT,'shared_archive_read_only'); END;
      CREATE TRIGGER IF NOT EXISTS shared_archive_files_no_update BEFORE UPDATE ON shared_archive_files
        BEGIN SELECT RAISE(ABORT,'shared_archive_read_only'); END;
      CREATE TRIGGER IF NOT EXISTS shared_archive_files_no_delete BEFORE DELETE ON shared_archive_files
        BEGIN SELECT RAISE(ABORT,'shared_archive_read_only'); END;
    "#,
        )
        .map_err(|e| format!("archive_schema_failed:{e}"))?;
    Ok(connection)
}

fn api_root(value: &str) -> Result<String, String> {
    let url = Url::parse(value.trim()).map_err(|_| "archive_api_url_invalid".to_string())?;
    let host = url.host_str().unwrap_or("");
    if url.scheme() != "https"
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !(host.ends_with(".classaimate.com") || host == "classaimate.com" || host == "classaimate-v3.pages.dev")
    {
        return Err("archive_api_url_forbidden".to_string());
    }
    Ok(format!("{}://{}{}", url.scheme(), url.host_str().unwrap_or(""), url.port().map(|port| format!(":{port}")).unwrap_or_default()))
}

fn body_data(response: ureq::Response) -> Result<Value, String> {
    let payload = response.into_json::<Value>().map_err(|e| format!("archive_response_decode_failed:{e}"))?;
    if payload.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err("archive_api_rejected".to_string());
    }
    Ok(payload.get("data").cloned().unwrap_or(Value::Null))
}

fn send_json(request: ureq::Request, body: Value) -> Result<Value, String> {
    match request.send_json(body) {
        Ok(response) => body_data(response),
        Err(ureq::Error::Status(status, _)) => Err(format!("archive_http_{status}")),
        Err(ureq::Error::Transport(_)) => Err("archive_network_error".to_string()),
    }
}

fn get_json(request: ureq::Request) -> Result<Value, String> {
    match request.call() {
        Ok(response) => body_data(response),
        Err(ureq::Error::Status(status, _)) => Err(format!("archive_http_{status}")),
        Err(ureq::Error::Transport(_)) => Err("archive_network_error".to_string()),
    }
}

fn hex_sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn file_sha(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("archive_file_open_failed:{e}"))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(|e| format!("archive_file_read_failed:{e}"))?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

fn sanitized_name(value: &str) -> String {
    let result: String = value
        .chars()
        .map(|character| if character.is_alphanumeric() || matches!(character, '.' | '-' | '_' | ' ') { character } else { '_' })
        .take(120)
        .collect();
    if result.trim_matches('.').is_empty() {
        "attachment.bin".to_string()
    } else {
        result
    }
}

fn download_file(agent: &ureq::Agent, root: &str, capability: &str, file: &Value, dir: &Path) -> Result<Value, String> {
    let ordinal = file.get("ordinal").and_then(Value::as_i64).ok_or("archive_file_ordinal_missing")?;
    let size = file.get("byteSize").and_then(Value::as_u64).ok_or("archive_file_size_missing")?;
    let expected = file.get("sha256").and_then(Value::as_str).ok_or("archive_file_sha_missing")?;
    let name = sanitized_name(file.get("originalName").and_then(Value::as_str).unwrap_or("attachment.bin"));
    let final_path = dir.join(format!("{ordinal:04}-{name}"));
    let part_path = dir.join(format!("{ordinal:04}-{name}.part"));
    if final_path.exists() && fs::metadata(&final_path).map(|item| item.len()).unwrap_or(0) == size && file_sha(&final_path)? == expected {
        return Ok(json!({"ordinal":ordinal,"localPath":final_path.to_string_lossy()}));
    }
    let offset = fs::metadata(&part_path).map(|item| item.len()).unwrap_or(0).min(size);
    let url = format!("{root}/api/v3/archive-export/files/{ordinal}");
    let mut request = agent.get(&url).set("Authorization", &format!("Archive {capability}"));
    if offset > 0 && offset < size {
        request = request.set("Range", &format!("bytes={offset}-"));
    }
    if offset < size {
        let response = request.call().map_err(|error| match error {
            ureq::Error::Status(status, _) => format!("archive_file_http_{status}"),
            _ => "archive_file_network_error".to_string(),
        })?;
        let append = offset > 0 && response.status() == 206;
        let mut output = OpenOptions::new()
            .create(true)
            .write(true)
            .append(append)
            .truncate(!append)
            .open(&part_path)
            .map_err(|e| format!("archive_file_write_open_failed:{e}"))?;
        let mut input = response.into_reader();
        let mut buffer = [0u8; 65536];
        loop {
            let count = input.read(&mut buffer).map_err(|e| format!("archive_file_download_failed:{e}"))?;
            if count == 0 {
                break;
            }
            output.write_all(&buffer[..count]).map_err(|e| format!("archive_file_write_failed:{e}"))?;
        }
        output.flush().map_err(|e| format!("archive_file_flush_failed:{e}"))?;
    }
    if fs::metadata(&part_path).map(|item| item.len()).unwrap_or(0) != size || file_sha(&part_path)? != expected {
        return Err(format!("archive_file_verify_failed:{ordinal}"));
    }
    fs::rename(&part_path, &final_path).map_err(|e| format!("archive_file_commit_failed:{e}"))?;
    Ok(json!({"ordinal":ordinal,"localPath":final_path.to_string_lossy()}))
}

fn manifest_hash(manifest: &Value) -> Result<String, String> {
    let archive = manifest.get("archive").ok_or("archive_manifest_archive_missing")?;
    let exact = json!({
      "schemaVersion":"shared_content_archive_v1","archiveId":archive.get("id"),"tenantId":archive.get("tenantId"),
      "sourceType":archive.get("sourceType"),"sourceId":archive.get("sourceId"),"sourceRevision":archive.get("sourceRevision"),
      "title":archive.get("title"),"createdAt":archive.get("createdAt"),"expiresAt":archive.get("expiresAt"),
      "records":manifest.get("records"),"files":manifest.get("files").and_then(Value::as_array).map(|files|files.iter().map(|file|json!({
        "ordinal":file.get("ordinal"),"fileId":file.get("fileId"),"originalName":file.get("originalName"),
        "contentType":file.get("contentType"),"byteSize":file.get("byteSize"),"sha256":file.get("sha256")
      })).collect::<Vec<_>>()).unwrap_or_default()
    });
    serde_json::to_vec(&exact).map(|bytes| hex_sha(&bytes)).map_err(|e| format!("archive_manifest_encode_failed:{e}"))
}

#[tauri::command]
pub(crate) fn import_shared_archive(base_url: String, code: String) -> Value {
    match import_archive(&base_url, &code) {
        Ok(value) => value,
        Err(error) => json!({"ok":false,"error":error}),
    }
}

fn import_archive(base_url: &str, code: &str) -> Result<Value, String> {
    let root = api_root(base_url)?;
    let safe_code = code.trim();
    if safe_code.len() != 43 || !safe_code.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')) {
        return Err("archive_code_invalid".to_string());
    }
    let agent = ureq::AgentBuilder::new().build();
    let exchange = send_json(agent.post(&format!("{root}/api/v3/archive-export/exchange")), json!({"code":safe_code}))?;
    let capability = exchange.get("capability").and_then(Value::as_str).ok_or("archive_capability_missing")?.to_string();
    let auth = || format!("Archive {capability}");
    let manifest = get_json(agent.get(&format!("{root}/api/v3/archive-export/manifest")).set("Authorization", &auth()))?;
    let archive = manifest.get("archive").ok_or("archive_manifest_invalid")?;
    let archive_id = archive.get("id").and_then(Value::as_str).ok_or("archive_id_missing")?;
    if archive_id.is_empty() || archive_id.len() > 128 || !archive_id.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':' | '.')) {
        return Err("archive_id_invalid".to_string());
    }
    let expected_manifest = archive.get("manifestSha256").and_then(Value::as_str).ok_or("archive_manifest_sha_missing")?;
    if manifest_hash(&manifest)? != expected_manifest {
        return Err("archive_manifest_verify_failed".to_string());
    }
    let records = get_json(agent.get(&format!("{root}/api/v3/archive-export/records")).set("Authorization", &auth()))?;
    let record_rows = records.get("records").and_then(Value::as_array).ok_or("archive_records_invalid")?;
    for record in record_rows {
        let encoded = serde_json::to_vec(record.get("payload").ok_or("archive_record_payload_missing")?).map_err(|e| format!("archive_record_encode_failed:{e}"))?;
        if hex_sha(&encoded) != record.get("sha256").and_then(Value::as_str).unwrap_or("") {
            return Err("archive_record_verify_failed".to_string());
        }
    }
    let (_, file_root) = paths();
    let archive_dir = file_root.join(archive_id);
    fs::create_dir_all(&archive_dir).map_err(|e| format!("archive_target_dir_failed:{e}"))?;
    let mut local_files = Vec::new();
    for file in manifest.get("files").and_then(Value::as_array).ok_or("archive_files_invalid")? {
        local_files.push(download_file(&agent, &root, &capability, file, &archive_dir)?);
    }
    let mut connection = open_db()?;
    let existing: i64 = connection.query_row("SELECT COUNT(*) FROM shared_archives WHERE id=?1", params![archive_id], |row| row.get(0)).unwrap_or(0);
    if existing == 0 {
        let tx = connection.transaction().map_err(|e| format!("archive_db_transaction_failed:{e}"))?;
        tx.execute(
            "INSERT INTO shared_archives VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                archive_id,
                archive.get("tenantId").and_then(Value::as_str).unwrap_or(""),
                archive.get("sourceType").and_then(Value::as_str).unwrap_or(""),
                archive.get("sourceId").and_then(Value::as_str).unwrap_or(""),
                archive.get("title").and_then(Value::as_str).unwrap_or(""),
                expected_manifest,
                record_rows.len() as i64,
                local_files.len() as i64,
                archive.get("totalFileBytes").and_then(Value::as_i64).unwrap_or(0),
                archive.get("createdAt").and_then(Value::as_i64).unwrap_or(0),
                archive.get("expiresAt").and_then(Value::as_i64).unwrap_or(0),
                Utc::now().timestamp_millis(),
                serde_json::to_string(&manifest).unwrap_or_default()
            ],
        )
        .map_err(|e| format!("archive_db_insert_failed:{e}"))?;
        for record in record_rows {
            tx.execute(
                "INSERT INTO shared_archive_records VALUES (?,?,?,?,?)",
                params![
                    archive_id,
                    record.get("ordinal").and_then(Value::as_i64).unwrap_or(0),
                    record.get("type").and_then(Value::as_str).unwrap_or(""),
                    serde_json::to_string(record.get("payload").unwrap_or(&Value::Null)).unwrap_or_default(),
                    record.get("sha256").and_then(Value::as_str).unwrap_or("")
                ],
            )
            .map_err(|e| format!("archive_record_insert_failed:{e}"))?;
        }
        for (file, local) in manifest.get("files").and_then(Value::as_array).expect("validated archive files").iter().zip(local_files.iter()) {
            tx.execute(
                "INSERT INTO shared_archive_files VALUES (?,?,?,?,?,?,?)",
                params![
                    archive_id,
                    file.get("ordinal").and_then(Value::as_i64).unwrap_or(0),
                    file.get("originalName").and_then(Value::as_str).unwrap_or(""),
                    file.get("contentType").and_then(Value::as_str).unwrap_or(""),
                    file.get("byteSize").and_then(Value::as_i64).unwrap_or(0),
                    file.get("sha256").and_then(Value::as_str).unwrap_or(""),
                    local.get("localPath").and_then(Value::as_str).unwrap_or("")
                ],
            )
            .map_err(|e| format!("archive_file_insert_failed:{e}"))?;
        }
        tx.commit().map_err(|e| format!("archive_db_commit_failed:{e}"))?;
    }
    send_json(agent.post(&format!("{root}/api/v3/archive-export/complete")).set("Authorization", &auth()), json!({"manifestSha256":expected_manifest}))?;
    Ok(json!({"ok":true,"archiveId":archive_id,"title":archive.get("title"),"recordCount":record_rows.len(),"fileCount":local_files.len()}))
}

#[tauri::command]
pub(crate) fn list_shared_archives() -> Value {
    match list_archives() {
        Ok(items) => json!({"ok":true,"archives":items}),
        Err(error) => json!({"ok":false,"archives":[],"error":error}),
    }
}

fn list_archives() -> Result<Vec<Value>, String> {
    let connection = open_db()?;
    let mut statement = connection
        .prepare("SELECT id,tenant_id,source_type,title,record_count,file_count,total_file_bytes,imported_at FROM shared_archives ORDER BY imported_at DESC,id DESC")
        .map_err(|e| format!("archive_list_prepare_failed:{e}"))?;
    let rows=statement.query_map([],|row|Ok(json!({"id":row.get::<_,String>(0)?,"tenantId":row.get::<_,String>(1)?,"sourceType":row.get::<_,String>(2)?,"title":row.get::<_,String>(3)?,"recordCount":row.get::<_,i64>(4)?,"fileCount":row.get::<_,i64>(5)?,"totalFileBytes":row.get::<_,i64>(6)?,"importedAt":row.get::<_,i64>(7)?}))).map_err(|e|format!("archive_list_query_failed:{e}"))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| format!("archive_list_row_failed:{e}"))
}

#[tauri::command]
pub(crate) fn get_shared_archive(archive_id: String) -> Value {
    match archive_detail(&archive_id) {
        Ok(value) => json!({"ok":true,"archive":value}),
        Err(error) => json!({"ok":false,"error":error}),
    }
}

fn archive_detail(id: &str) -> Result<Value, String> {
    let connection = open_db()?;
    let archive=connection.query_row("SELECT id,tenant_id,source_type,source_id,title,record_count,file_count,total_file_bytes,imported_at FROM shared_archives WHERE id=?1",params![id],|row|Ok(json!({"id":row.get::<_,String>(0)?,"tenantId":row.get::<_,String>(1)?,"sourceType":row.get::<_,String>(2)?,"sourceId":row.get::<_,String>(3)?,"title":row.get::<_,String>(4)?,"recordCount":row.get::<_,i64>(5)?,"fileCount":row.get::<_,i64>(6)?,"totalFileBytes":row.get::<_,i64>(7)?,"importedAt":row.get::<_,i64>(8)?}))).map_err(|_|"archive_not_found".to_string())?;
    let mut records_stmt = connection
        .prepare("SELECT ordinal,record_type,payload_json,payload_sha256 FROM shared_archive_records WHERE archive_id=?1 ORDER BY ordinal")
        .map_err(|e| format!("archive_detail_prepare_failed:{e}"))?;
    let records = records_stmt
        .query_map(params![id], |row| {
            Ok(json!({"ordinal":row.get::<_,i64>(0)?,"type":row.get::<_,String>(1)?,"payload":serde_json::from_str::<Value>(&row.get::<_,String>(2)?).unwrap_or(Value::Null),"sha256":row.get::<_,String>(3)?}))
        })
        .map_err(|e| format!("archive_detail_query_failed:{e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("archive_detail_row_failed:{e}"))?;
    let mut files_stmt = connection
        .prepare("SELECT ordinal,original_name,content_type,byte_size,sha256,local_path FROM shared_archive_files WHERE archive_id=?1 ORDER BY ordinal")
        .map_err(|e| format!("archive_file_prepare_failed:{e}"))?;
    let files = files_stmt
        .query_map(params![id], |row| {
            Ok(json!({"ordinal":row.get::<_,i64>(0)?,"originalName":row.get::<_,String>(1)?,"contentType":row.get::<_,String>(2)?,"byteSize":row.get::<_,i64>(3)?,"sha256":row.get::<_,String>(4)?,"localPath":row.get::<_,String>(5)?}))
        })
        .map_err(|e| format!("archive_file_query_failed:{e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("archive_file_row_failed:{e}"))?;
    Ok(json!({"meta":archive,"records":records,"files":files}))
}

#[tauri::command]
pub(crate) fn export_shared_archive(archive_id: String, target_path: String) -> Value {
    match archive_detail(&archive_id).and_then(|value| {
        serde_json::to_string_pretty(&value)
            .map_err(|e| format!("archive_export_encode_failed:{e}"))
            .and_then(|raw| fs::write(target_path, format!("{raw}\n")).map_err(|e| format!("archive_export_write_failed:{e}")))
    }) {
        Ok(()) => json!({"ok":true}),
        Err(error) => json!({"ok":false,"error":error}),
    }
}

#[tauri::command]
pub(crate) fn open_shared_archive_file(archive_id: String, ordinal: i64) -> Value {
    let result = (|| -> Result<(), String> {
        let connection = open_db()?;
        let path: String = connection
            .query_row("SELECT local_path FROM shared_archive_files WHERE archive_id=?1 AND ordinal=?2", params![archive_id, ordinal], |row| row.get(0))
            .map_err(|_| "archive_file_not_found".to_string())?;
        let target = PathBuf::from(path);
        let (_, root) = paths();
        let canonical = target.canonicalize().map_err(|e| format!("archive_file_path_failed:{e}"))?;
        let safe_root = root.canonicalize().map_err(|e| format!("archive_root_path_failed:{e}"))?;
        if !canonical.starts_with(safe_root) {
            return Err("archive_file_path_forbidden".to_string());
        }
        #[cfg(target_os = "windows")]
        let opened = Command::new("explorer.exe").arg(&canonical).spawn();
        #[cfg(target_os = "macos")]
        let opened = Command::new("open").arg(&canonical).spawn();
        #[cfg(all(unix, not(target_os = "macos")))]
        let opened = Command::new("xdg-open").arg(&canonical).spawn();
        opened.map(|_| ()).map_err(|e| format!("archive_file_open_failed:{e}"))
    })();
    match result {
        Ok(()) => json!({"ok":true}),
        Err(error) => json!({"ok":false,"error":error}),
    }
}

#[cfg(test)]
mod tests {
    use super::{api_root, manifest_hash};
    use serde_json::json;

    #[test]
    fn archive_api_accepts_only_https_classaimate_hosts() {
        assert_eq!(api_root("https://classaimate-v3.pages.dev/path").unwrap(), "https://classaimate-v3.pages.dev");
        assert_eq!(api_root("https://v3.classaimate.com").unwrap(), "https://v3.classaimate.com");
        assert!(api_root("http://v3.classaimate.com").is_err());
        assert!(api_root("https://classaimate.com.attacker.example").is_err());
        assert!(api_root("https://user@classaimate-v3.pages.dev").is_err());
    }

    #[test]
    fn manifest_hash_ignores_server_only_object_key() {
        let base = json!({
            "archive": { "id":"archive","tenantId":"tenant","sourceType":"board","sourceId":"board",
                "sourceRevision":1,"title":"보드","createdAt":1,"expiresAt":2 },
            "records": [{ "ordinal":0,"type":"board","sha256":"a" }],
            "files": [{ "ordinal":0,"fileId":"file","originalName":"a.pdf","contentType":"application/pdf",
                "byteSize":3,"sha256":"b" }]
        });
        let mut server = base.clone();
        server["files"][0]["objectKey"] = json!("private/server/key");
        assert_eq!(manifest_hash(&base).unwrap(), manifest_hash(&server).unwrap());
    }
}
