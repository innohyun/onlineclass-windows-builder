use crate::{json_response, parse_request_url, query, BrowserLinkStore, SqliteStore};
use chrono::Utc;
use fs2::available_space;
use rand::{distributions::Alphanumeric, Rng};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use tiny_http::{Method, Request, ResponseBox};

const ROOT_DIR: &str = "teaching-sources";
const MAX_FILE_BYTES: u64 = 500 * 1024 * 1024;
const MAX_JSON_BYTES: u64 = 12 * 1024 * 1024;
const MAX_CHUNKS_PER_BATCH: usize = 200;
const MAX_MCP_MATCHES: usize = 20;
const MAX_MCP_CHUNKS: usize = 20;
const MAX_CHUNK_TEXT: usize = 3_000;

pub(crate) fn ensure_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS teaching_sources (
          owner_uid TEXT NOT NULL,
          source_id TEXT NOT NULL,
          origin_tenant_id TEXT NOT NULL,
          source_type TEXT NOT NULL,
          grade TEXT NOT NULL,
          semester_scope TEXT NOT NULL CHECK (semester_scope IN ('1','2','full_year')),
          subject_code TEXT NOT NULL,
          title TEXT NOT NULL,
          publisher TEXT NOT NULL,
          original_file_name TEXT NOT NULL,
          content_type TEXT NOT NULL,
          byte_size INTEGER NOT NULL DEFAULT 0,
          sha256 TEXT,
          local_path TEXT,
          extraction_status TEXT NOT NULL CHECK (extraction_status IN ('awaiting_file','extracting','ready','needs_ocr','unsupported','failed')),
          lifecycle_status TEXT NOT NULL DEFAULT 'active' CHECK (lifecycle_status IN ('active','archived')),
          extractor_version TEXT,
          page_count INTEGER,
          revision INTEGER NOT NULL DEFAULT 1,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (owner_uid, source_id)
        );
        CREATE INDEX IF NOT EXISTS idx_teaching_sources_owner_subject
          ON teaching_sources (owner_uid, subject_code, updated_at_ms DESC);
        CREATE TABLE IF NOT EXISTS teaching_source_actor_homes (
          owner_uid TEXT PRIMARY KEY,
          backup_tenant_id TEXT NOT NULL,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS teaching_source_chunks (
          owner_uid TEXT NOT NULL,
          source_id TEXT NOT NULL,
          chunk_id TEXT NOT NULL,
          ordinal INTEGER NOT NULL,
          page_start INTEGER,
          page_end INTEGER,
          unit TEXT NOT NULL DEFAULT '',
          topic TEXT NOT NULL DEFAULT '',
          text TEXT NOT NULL,
          text_sha256 TEXT NOT NULL,
          revision INTEGER NOT NULL DEFAULT 1,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (owner_uid, source_id, chunk_id),
          UNIQUE (owner_uid, source_id, ordinal),
          FOREIGN KEY (owner_uid, source_id)
            REFERENCES teaching_sources (owner_uid, source_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_teaching_source_chunks_source
          ON teaching_source_chunks (owner_uid, source_id, ordinal);
        CREATE VIRTUAL TABLE IF NOT EXISTS teaching_source_chunks_fts USING fts5(
          owner_uid UNINDEXED, source_id UNINDEXED, chunk_id UNINDEXED,
          subject_code, title, unit, topic, text, tokenize='unicode61'
        );
        CREATE TABLE IF NOT EXISTS curriculum_source_links (
          tenant_id TEXT NOT NULL,
          owner_uid TEXT NOT NULL,
          school_year INTEGER NOT NULL,
          semester INTEGER NOT NULL CHECK (semester IN (1,2)),
          grade INTEGER,
          curriculum_source_kind TEXT NOT NULL CHECK (curriculum_source_kind IN ('class','grade_draft','grade_published')),
          curriculum_source_scope_id TEXT NOT NULL,
          curriculum_item_id TEXT NOT NULL,
          curriculum_source_revision INTEGER NOT NULL,
          teaching_source_revision INTEGER NOT NULL,
          subject_code_snapshot TEXT NOT NULL,
          unit_snapshot TEXT NOT NULL,
          title_snapshot TEXT NOT NULL,
          source_id TEXT NOT NULL,
          chunk_id TEXT NOT NULL DEFAULT '',
          revision INTEGER NOT NULL DEFAULT 1,
          created_at_ms INTEGER NOT NULL,
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, owner_uid, school_year, semester, curriculum_source_kind,
            curriculum_source_scope_id, curriculum_item_id, source_id, chunk_id),
          FOREIGN KEY (owner_uid, source_id)
            REFERENCES teaching_sources (owner_uid, source_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_curriculum_source_links_lookup
          ON curriculum_source_links (tenant_id, owner_uid, school_year, semester,
            curriculum_source_kind, curriculum_source_scope_id, curriculum_item_id);
        "#,
    )
    .map_err(|e| format!("db_teaching_sources_schema_failed:{e}"))?;
    let fts_columns={
        let mut statement=conn.prepare("PRAGMA table_info(teaching_source_chunks_fts)").map_err(|e|format!("db_teaching_source_fts_schema_inspect_failed:{e}"))?;
        let columns=statement.query_map([],|row|row.get::<_,String>(1)).map_err(|e|format!("db_teaching_source_fts_schema_inspect_failed:{e}"))?.collect::<Result<Vec<_>,_>>().map_err(|e|format!("db_teaching_source_fts_schema_inspect_failed:{e}"))?;
        columns
    };
    if !fts_columns.iter().any(|column|column=="title") {
        conn.execute_batch(r#"
          DROP TABLE teaching_source_chunks_fts;
          CREATE VIRTUAL TABLE teaching_source_chunks_fts USING fts5(
            owner_uid UNINDEXED, source_id UNINDEXED, chunk_id UNINDEXED,
            subject_code, title, unit, topic, text, tokenize='unicode61'
          );
          INSERT INTO teaching_source_chunks_fts(owner_uid,source_id,chunk_id,subject_code,title,unit,topic,text)
          SELECT c.owner_uid,c.source_id,c.chunk_id,s.subject_code,s.title,c.unit,c.topic,c.text
            FROM teaching_source_chunks c JOIN teaching_sources s
              ON s.owner_uid=c.owner_uid AND s.source_id=c.source_id;
        "#).map_err(|e|format!("db_teaching_source_fts_schema_upgrade_failed:{e}"))?;
    }
    for (table,column,definition) in [
        ("teaching_sources","lifecycle_status","TEXT NOT NULL DEFAULT 'active' CHECK (lifecycle_status IN ('active','archived'))"),
        ("curriculum_source_links","teaching_source_revision","INTEGER NOT NULL DEFAULT 1"),
    ] {
        let mut statement=conn.prepare(&format!("PRAGMA table_info({table})")).map_err(|e|format!("db_teaching_sources_schema_inspect_failed:{e}"))?;
        let columns=statement.query_map([],|row|row.get::<_,String>(1)).map_err(|e|format!("db_teaching_sources_schema_inspect_failed:{e}"))?.collect::<Result<Vec<_>,_>>().map_err(|e|format!("db_teaching_sources_schema_inspect_failed:{e}"))?;
        if !columns.iter().any(|value|value==column){conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {definition}" )).map_err(|e|format!("db_teaching_sources_schema_upgrade_failed:{e}"))?;}
    }
    conn.execute("UPDATE curriculum_source_links SET teaching_source_revision=COALESCE((SELECT revision FROM teaching_sources s WHERE s.owner_uid=curriculum_source_links.owner_uid AND s.source_id=curriculum_source_links.source_id),teaching_source_revision)",[]).map_err(|e|format!("db_teaching_sources_link_revision_backfill_failed:{e}"))?;
    Ok(())
}

fn safe_id(value: &str, max: usize) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > max
        || !value.bytes().all(|ch| ch.is_ascii_alphanumeric() || b"._:-".contains(&ch)) {
        None
    } else { Some(value.to_string()) }
}

fn text(value: &Value, key: &str, max: usize) -> Result<String, String> {
    let value = value.get(key).and_then(Value::as_str).unwrap_or("").trim();
    if value.is_empty() || value.chars().count() > max || value.chars().any(char::is_control) {
        Err(format!("teaching_source_{key}_invalid"))
    } else { Ok(value.to_string()) }
}

fn optional_text(value: &Value, key: &str, max: usize) -> Result<String, String> {
    let value = value.get(key).and_then(Value::as_str).unwrap_or("").trim();
    if value.chars().count() > max || value.chars().any(|ch| ch.is_control() && ch != '\n' && ch != '\t') {
        Err(format!("teaching_source_{key}_invalid"))
    } else { Ok(value.to_string()) }
}

fn safe_file_name(value: &str) -> String {
    let value = value.trim().chars().take(240).map(|ch| {
        if ch.is_control() || matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { ch }
    }).collect::<String>().trim_start_matches('.').to_string();
    if value.is_empty() { "원자료".to_string() } else { value }
}

fn extension(file_name: &str) -> Option<&'static str> {
    match Path::new(file_name).extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "pdf" => Some("pdf"), "pptx" => Some("pptx"), "docx" => Some("docx"),
        "txt" => Some("txt"), "md" | "markdown" => Some("md"), _ => None,
    }
}

fn expected_mime(ext: &str) -> &'static str {
    match ext {
        "pdf" => "application/pdf",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "txt" | "md" => "text/plain",
        _ => "application/octet-stream",
    }
}

fn mime_matches(ext: &str, value: &str) -> bool {
    let mime=value.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    match ext {
        "md" => ["text/plain","text/markdown","application/octet-stream"].contains(&mime.as_str()),
        "txt" => ["text/plain","application/octet-stream"].contains(&mime.as_str()),
        _ => mime==expected_mime(ext) || mime=="application/octet-stream",
    }
}

pub(crate) fn actor_folder(owner_uid: &str) -> String {
    format!("{:x}", Sha256::digest(owner_uid.as_bytes()))[..32].to_string()
}

fn managed_directory(store: &SqliteStore, parts: &[&str]) -> Result<PathBuf, String> {
    let base=fs::canonicalize(&store.data_dir).map_err(|_|"local_data_dir_missing".to_string())?;
    let mut cursor=base.clone();
    for part in parts {
        if part.is_empty() || *part=="." || *part==".." || part.contains('/') || part.contains('\\') { return Err("teaching_source_path_invalid".into()); }
        cursor.push(part);
        match fs::symlink_metadata(&cursor) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => return Err("teaching_source_path_invalid".into()),
            Ok(_) => {},
            Err(error) if error.kind()==std::io::ErrorKind::NotFound => fs::create_dir(&cursor).map_err(|e|format!("teaching_source_directory_failed:{e}"))?,
            Err(error) => return Err(format!("teaching_source_directory_inspect_failed:{error}")),
        }
        let canonical=fs::canonicalize(&cursor).map_err(|_|"teaching_source_path_invalid".to_string())?;
        if !canonical.starts_with(&base){return Err("teaching_source_path_invalid".into());}
    }
    Ok(cursor)
}

fn consume_utf8(pending: &mut Vec<u8>, bytes: &[u8]) -> bool {
    let mut candidate=Vec::with_capacity(pending.len()+bytes.len());candidate.extend_from_slice(pending);candidate.extend_from_slice(bytes);pending.clear();
    match std::str::from_utf8(&candidate) {
        Ok(_) => true,
        Err(error) if error.error_len().is_none() => { pending.extend_from_slice(&candidate[error.valid_up_to()..]);pending.len()<=3 },
        Err(_) => false,
    }
}

fn zip_has_marker(path:&Path,size:u64,marker:&[u8])->Result<bool,String>{
    let mut file=File::open(path).map_err(|e|format!("teaching_source_file_verify_open_failed:{e}"))?;
    let take=size.min(8*1024*1024);file.seek(SeekFrom::End(-(take as i64))).map_err(|e|format!("teaching_source_file_verify_seek_failed:{e}"))?;
    let mut tail=vec![0_u8;take as usize];file.read_exact(&mut tail).map_err(|e|format!("teaching_source_file_verify_read_failed:{e}"))?;
    Ok(tail.windows(marker.len()).any(|window|window==marker))
}

fn checked_path(store: &SqliteStore, relative: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative);
    if path.is_absolute() || path.components().any(|part| matches!(part, Component::ParentDir | Component::RootDir | Component::Prefix(_))) {
        return Err("teaching_source_path_invalid".into());
    }
    Ok(store.data_dir.join(path))
}

fn read_json(request: &mut Request) -> Result<Value, String> {
    let mut raw = String::new();
    request.as_reader().take(MAX_JSON_BYTES + 1).read_to_string(&mut raw)
        .map_err(|e| format!("teaching_source_body_read_failed:{e}"))?;
    if raw.len() as u64 > MAX_JSON_BYTES { return Err("teaching_source_body_too_large".into()); }
    serde_json::from_str(&raw).map_err(|_| "teaching_source_invalid_json".into())
}

fn source_row(store: &SqliteStore, owner: &str, source: &str) -> Result<Option<Value>, String> {
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    conn.query_row(
        "SELECT source_id,source_type,grade,semester_scope,subject_code,title,publisher,original_file_name,content_type,byte_size,sha256,extraction_status,extractor_version,page_count,revision,created_at_ms,updated_at_ms,origin_tenant_id,lifecycle_status FROM teaching_sources WHERE owner_uid=?1 AND source_id=?2",
        params![owner, source], |row| Ok(json!({
            "sourceId":row.get::<_,String>(0)?,"sourceType":row.get::<_,String>(1)?,"grade":row.get::<_,String>(2)?,
            "semesterScope":row.get::<_,String>(3)?,"subjectCode":row.get::<_,String>(4)?,"title":row.get::<_,String>(5)?,
            "publisher":row.get::<_,String>(6)?,"originalFileName":row.get::<_,String>(7)?,"contentType":row.get::<_,String>(8)?,
            "byteSize":row.get::<_,i64>(9)?,"sha256":row.get::<_,Option<String>>(10)?,"extractionStatus":row.get::<_,String>(11)?,
            "extractorVersion":row.get::<_,Option<String>>(12)?,"pageCount":row.get::<_,Option<i64>>(13)?,"revision":row.get::<_,i64>(14)?,
            "createdAt":row.get::<_,i64>(15)?,"updatedAt":row.get::<_,i64>(16)?,"originTenantId":row.get::<_,String>(17)?,
            "lifecycleStatus":row.get::<_,String>(18)?,"backupState":"onedrive_included"
        }))
    ).optional().map_err(|e| format!("db_teaching_source_read_failed:{e}"))
}

fn update_import_run(conn: &Connection, tenant: &str, owner: &str, source: &str, status: &str, now: i64) -> Result<(), String> {
    let payload = json!({"sourceId":source,"state":status,"ownerScope":actor_folder(owner)}).to_string();
    conn.execute(
        "INSERT INTO local_import_runs (tenant_id,run_id,kind,status,payload_json,started_at_ms,finished_at_ms) VALUES (?1,?2,'teaching_source',?3,?4,?5,?5) ON CONFLICT(tenant_id,run_id) DO UPDATE SET status=excluded.status,payload_json=excluded.payload_json,finished_at_ms=excluded.finished_at_ms",
        params![tenant, format!("teaching-source:{}:{source}",actor_folder(owner)), status, payload, now],
    ).map_err(|e| format!("db_teaching_source_import_run_failed:{e}"))?;
    Ok(())
}

fn mark_backup_dirty(store:&SqliteStore,owner:&str)->Result<(),String>{
    let tenant=store.conn.lock().map_err(|_|"db_lock_failed".to_string())?.query_row(
        "SELECT backup_tenant_id FROM teaching_source_actor_homes WHERE owner_uid=?1",params![owner],|row|row.get::<_,String>(0))
        .map_err(|e|format!("db_teaching_source_actor_home_read_failed:{e}"))?;
    crate::backup::mark_external_sync_dirty(store,&tenant)
}

fn verify_managed_file(store:&SqliteStore,tenant:&str,owner:&str,source:&str,expected_sha:&str)->Result<(),String>{
    let row=store.conn.lock().map_err(|_|"db_lock_failed".to_string())?.query_row(
        "SELECT local_path,sha256,byte_size FROM teaching_sources WHERE owner_uid=?1 AND source_id=?2",
        params![owner,source],|row|Ok((row.get::<_,Option<String>>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,i64>(2)?)))
        .optional().map_err(|e|format!("db_teaching_source_file_verify_read_failed:{e}"))?.ok_or("teaching_source_not_found")?;
    if row.1.as_deref()!=Some(expected_sha){return Err("teaching_source_file_sha_conflict".into());}
    let relative=row.0.ok_or("teaching_source_file_missing")?;
    let _access=store.media_access(tenant)?;
    let path=resolve_local_path(store,&relative)?;
    let mut file=File::open(path).map_err(|_|"teaching_source_file_missing".to_string())?;
    let mut hash=Sha256::new();let mut size=0_i64;let mut buffer=[0_u8;64*1024];
    loop{let read=file.read(&mut buffer).map_err(|_|"teaching_source_file_verify_failed".to_string())?;if read==0{break;}size+=read as i64;hash.update(&buffer[..read]);}
    if size!=row.2||format!("{:x}",hash.finalize())!=expected_sha{return Err("teaching_source_file_sha_conflict".into());}
    Ok(())
}

fn create_source(store: &SqliteStore, tenant: &str, owner: &str, body: &Value) -> Result<Value, String> {
    let source = safe_id(body.get("sourceId").and_then(Value::as_str).unwrap_or(""), 160)
        .ok_or_else(|| "teaching_source_id_invalid".to_string())?;
    let source_type = text(body, "sourceType", 40)?;
    if !["textbook","teacher_guide","reference","worksheet","presentation","other"].contains(&source_type.as_str()) {
        return Err("teaching_source_sourceType_invalid".into());
    }
    let grade = text(body, "grade", 40)?;
    let semester = text(body, "semesterScope", 16)?;
    if !["1","2","full_year"].contains(&semester.as_str()) { return Err("teaching_source_semesterScope_invalid".into()); }
    let subject = text(body, "subjectCode", 80)?;
    let title = text(body, "title", 240)?;
    let publisher = text(body, "publisher", 160)?;
    let file_name = safe_file_name(&text(body, "originalFileName", 240)?);
    let ext = extension(&file_name).ok_or_else(|| "teaching_source_file_unsupported".to_string())?;
    let content_type = expected_mime(ext);
    let now = Utc::now().timestamp_millis();
    let mut conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    let tx = conn.transaction().map_err(|e| format!("db_teaching_source_transaction_failed:{e}"))?;
    tx.execute("INSERT OR IGNORE INTO teaching_source_actor_homes(owner_uid,backup_tenant_id,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?3)",params![owner,tenant,now])
        .map_err(|e|format!("db_teaching_source_actor_home_failed:{e}"))?;
    let backup_tenant:String=tx.query_row("SELECT backup_tenant_id FROM teaching_source_actor_homes WHERE owner_uid=?1",params![owner],|row|row.get(0))
        .map_err(|e|format!("db_teaching_source_actor_home_read_failed:{e}"))?;
    tx.execute(
        "INSERT INTO teaching_sources (origin_tenant_id,owner_uid,source_id,source_type,grade,semester_scope,subject_code,title,publisher,original_file_name,content_type,extraction_status,created_at_ms,updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'awaiting_file',?12,?12)",
        params![tenant,owner,source,source_type,grade,semester,subject,title,publisher,file_name,content_type,now],
    ).map_err(|e| if e.to_string().contains("UNIQUE") { "teaching_source_conflict".into() } else { format!("db_teaching_source_insert_failed:{e}") })?;
    update_import_run(&tx, &backup_tenant, owner, &source, "started", now)?;
    tx.commit().map_err(|e| format!("db_teaching_source_commit_failed:{e}"))?;
    drop(conn);
    mark_backup_dirty(store,owner)?;
    source_row(store, owner, &source)?.ok_or_else(|| "teaching_source_not_found".into())
}

fn save_file(store: &SqliteStore, tenant: &str, owner: &str, source: &str, request: &mut Request) -> Result<Value, String> {
    let source_id = safe_id(source, 160).ok_or_else(|| "teaching_source_id_invalid".to_string())?;
    let row = source_row(store, owner, &source_id)?.ok_or_else(|| "teaching_source_not_found".to_string())?;
    let expected_revision = query(&parse_request_url(request)?, "expectedRevision").parse::<i64>().map_err(|_| "teaching_source_revision_required")?;
    if row["revision"].as_i64() != Some(expected_revision) { return Err("teaching_source_revision_conflict".into()); }
    let file_name = row["originalFileName"].as_str().unwrap_or("");
    let ext = extension(file_name).ok_or_else(|| "teaching_source_file_unsupported".to_string())?;
    let request_mime=request.headers().iter().find(|h|h.field.equiv("Content-Type")).map(|h|h.value.as_str()).unwrap_or("");
    if !mime_matches(ext,request_mime){return Err("teaching_source_content_type_mismatch".into());}
    let declared_length=request.headers().iter().find(|h| h.field.equiv("Content-Length"))
        .and_then(|h| h.value.as_str().parse::<u64>().ok());
    if let Some(length) = declared_length {
        if length == 0 || length > MAX_FILE_BYTES { return Err("teaching_source_file_size_invalid".into()); }
    }
    let base=fs::canonicalize(&store.data_dir).map_err(|_|"local_data_dir_missing".to_string())?;
    let actor=actor_folder(owner);let temp_root=managed_directory(store,&[ROOT_DIR,&actor,"staging"])?;
    let free=available_space(&temp_root).map_err(|e|format!("teaching_source_disk_check_failed:{e}"))?;
    const RESERVE:u64=64*1024*1024;
    if declared_length.is_some_and(|size|size.saturating_add(RESERVE)>free){return Err("teaching_source_insufficient_disk_space".into());}
    let suffix: String = rand::thread_rng().sample_iter(&Alphanumeric).take(16).map(char::from).collect();
    let temp_path = temp_root.join(format!(".{source_id}.{suffix}.tmp"));
    let mut file = OpenOptions::new().write(true).create_new(true).open(&temp_path).map_err(|e| format!("teaching_source_file_create_failed:{e}"))?;
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    let mut prefix = Vec::new();
    let mut text_pending=Vec::new();
    let validate_text=matches!(ext,"txt"|"md");
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = request.as_reader().read(&mut buffer).map_err(|e| format!("teaching_source_file_read_failed:{e}"))?;
        if read == 0 { break; }
        size = size.checked_add(read as u64).ok_or("teaching_source_file_size_invalid")?;
        if size > MAX_FILE_BYTES { let _ = fs::remove_file(&temp_path); return Err("teaching_source_file_size_invalid".into()); }
        if size.saturating_add(RESERVE)>free { let _=fs::remove_file(&temp_path);return Err("teaching_source_insufficient_disk_space".into()); }
        if prefix.len() < 8 { prefix.extend_from_slice(&buffer[..read.min(8 - prefix.len())]); }
        if validate_text && (buffer[..read].contains(&0) || !consume_utf8(&mut text_pending,&buffer[..read])) { let _=fs::remove_file(&temp_path);return Err("teaching_source_file_signature_invalid".into()); }
        hash.update(&buffer[..read]);
        if let Err(error)=file.write_all(&buffer[..read]){let _=fs::remove_file(&temp_path);return Err(format!("teaching_source_file_write_failed:{error}"));}
    }
    if size == 0 { let _ = fs::remove_file(&temp_path); return Err("teaching_source_file_size_invalid".into()); }
    if declared_length.is_some_and(|declared|declared!=size) { let _=fs::remove_file(&temp_path);return Err("teaching_source_file_size_mismatch".into()); }
    if validate_text&&!text_pending.is_empty() { let _=fs::remove_file(&temp_path);return Err("teaching_source_file_signature_invalid".into()); }
    if let Err(error)=file.sync_all(){let _=fs::remove_file(&temp_path);return Err(format!("teaching_source_file_sync_failed:{error}"));}
    drop(file);
    let zip_marker=|marker:&[u8]|match zip_has_marker(&temp_path,size,marker){Ok(value)=>Ok(value),Err(error)=>{let _=fs::remove_file(&temp_path);Err(error)}};
    let magic_ok = match ext { "pdf" => prefix.starts_with(b"%PDF-"), "pptx" => prefix.starts_with(b"PK\x03\x04")&&zip_marker(b"ppt/presentation.xml")?, "docx" => prefix.starts_with(b"PK\x03\x04")&&zip_marker(b"word/document.xml")?, _ => true };
    if !magic_ok { let _ = fs::remove_file(&temp_path); return Err("teaching_source_file_signature_invalid".into()); }
    let sha = format!("{:x}", hash.finalize());
    let object_dir=managed_directory(store,&[ROOT_DIR,&actor,"objects",&sha[..2]])?;
    let relative = PathBuf::from(ROOT_DIR).join(&actor).join("objects").join(&sha[..2]).join(format!("{sha}.{ext}"));
    let target = store.data_dir.join(&relative);
    let safe_parent=fs::canonicalize(&object_dir).map_err(|_|"teaching_source_path_invalid".to_string())?;
    if !safe_parent.starts_with(&base){let _=fs::remove_file(&temp_path);return Err("teaching_source_path_invalid".into());}
    if fs::symlink_metadata(&target).is_ok_and(|meta|meta.file_type().is_symlink()){let _=fs::remove_file(&temp_path);return Err("teaching_source_path_invalid".into());}
    let _access = store.media_access(tenant)?;
    if target.exists() { fs::remove_file(&temp_path).map_err(|e| format!("teaching_source_temp_cleanup_failed:{e}"))?; }
    else { fs::rename(&temp_path, &target).map_err(|e| format!("teaching_source_file_commit_failed:{e}"))?; }
    let now = Utc::now().timestamp_millis();
    let changed = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?.execute(
        "UPDATE teaching_sources SET content_type=?1,byte_size=?2,sha256=?3,local_path=?4,extraction_status='extracting',revision=revision+1,updated_at_ms=?5 WHERE owner_uid=?6 AND source_id=?7 AND revision=?8",
        params![expected_mime(ext),size as i64,sha,relative.to_string_lossy(),now,owner,source_id,expected_revision],
    ).map_err(|e| format!("db_teaching_source_file_update_failed:{e}"))?;
    if changed != 1 { return Err("teaching_source_revision_conflict".into()); }
    mark_backup_dirty(store,owner)?;
    source_row(store, owner, &source_id)?.ok_or_else(|| "teaching_source_not_found".into())
}

fn save_chunks(store: &SqliteStore, tenant: &str, owner: &str, source: &str, body: &Value) -> Result<Value, String> {
    let source_id = safe_id(source, 160).ok_or_else(|| "teaching_source_id_invalid".to_string())?;
    let expected = body.get("expectedRevision").and_then(Value::as_i64).filter(|v| *v > 0).ok_or("teaching_source_revision_required")?;
    let expected_sha = body.get("expectedFileSha256").and_then(Value::as_str).filter(|v| v.len()==64 && v.bytes().all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())).ok_or("teaching_source_file_sha_required")?;
    let replace = body.get("replace").and_then(Value::as_bool).unwrap_or(false);
    let chunks = body.get("chunks").and_then(Value::as_array).filter(|v| !v.is_empty() && v.len() <= MAX_CHUNKS_PER_BATCH)
        .ok_or("teaching_source_chunks_invalid")?;
    let source_row = source_row(store, owner, &source_id)?.ok_or("teaching_source_not_found")?;
    if source_row["revision"].as_i64() != Some(expected) || source_row["sha256"].as_str() != Some(expected_sha) || source_row["extractionStatus"] != "extracting" {
        return Err("teaching_source_revision_conflict".into());
    }
    verify_managed_file(store,tenant,owner,&source_id,expected_sha)?;
    let subject = source_row["subjectCode"].as_str().unwrap_or("").to_string();
    let title = source_row["title"].as_str().unwrap_or("").to_string();
    let now = Utc::now().timestamp_millis();
    let mut conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    let tx = conn.transaction().map_err(|e| format!("db_teaching_source_transaction_failed:{e}"))?;
    if replace {
        tx.execute("DELETE FROM teaching_source_chunks_fts WHERE owner_uid=?1 AND source_id=?2", params![owner,source_id])
            .map_err(|e| format!("db_teaching_source_fts_reset_failed:{e}"))?;
        tx.execute("DELETE FROM teaching_source_chunks WHERE owner_uid=?1 AND source_id=?2", params![owner,source_id])
            .map_err(|e| format!("db_teaching_source_chunks_reset_failed:{e}"))?;
    }
    let mut seen = HashSet::new();
    for raw in chunks {
        let chunk_id = safe_id(raw.get("chunkId").and_then(Value::as_str).unwrap_or(""), 160).ok_or("teaching_source_chunk_id_invalid")?;
        let ordinal = raw.get("ordinal").and_then(Value::as_i64).filter(|v| *v >= 0).ok_or("teaching_source_chunk_ordinal_invalid")?;
        if !seen.insert(chunk_id.clone()) { return Err("teaching_source_chunk_duplicate".into()); }
        let page_start = raw.get("pageStart").and_then(Value::as_i64).filter(|v| *v > 0);
        let page_end = raw.get("pageEnd").and_then(Value::as_i64).filter(|v| *v > 0);
        if page_start.zip(page_end).is_some_and(|(start,end)| end < start) { return Err("teaching_source_chunk_page_invalid".into()); }
        let unit = optional_text(raw, "unit", 240)?;
        let topic = optional_text(raw, "topic", 300)?;
        let chunk_text = optional_text(raw, "text", MAX_CHUNK_TEXT)?;
        if chunk_text.is_empty() { return Err("teaching_source_chunk_text_invalid".into()); }
        let digest = format!("{:x}", Sha256::digest(chunk_text.as_bytes()));
        tx.execute("INSERT INTO teaching_source_chunks (owner_uid,source_id,chunk_id,ordinal,page_start,page_end,unit,topic,text,text_sha256,created_at_ms,updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?11)",
            params![owner,source_id,chunk_id,ordinal,page_start,page_end,unit,topic,chunk_text,digest,now])
            .map_err(|e| format!("db_teaching_source_chunk_insert_failed:{e}"))?;
        tx.execute("INSERT INTO teaching_source_chunks_fts (owner_uid,source_id,chunk_id,subject_code,title,unit,topic,text) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![owner,source_id,chunk_id,subject,title,unit,topic,chunk_text])
            .map_err(|e| format!("db_teaching_source_fts_insert_failed:{e}"))?;
    }
    tx.commit().map_err(|e| format!("db_teaching_source_chunks_commit_failed:{e}"))?;
    drop(conn);
    mark_backup_dirty(store,owner)?;
    Ok(json!({"accepted":chunks.len(),"sourceId":source_id,"revision":expected}))
}

fn finalize_source(store: &SqliteStore, tenant: &str, owner: &str, source: &str, body: &Value) -> Result<Value, String> {
    let source_id = safe_id(source, 160).ok_or_else(|| "teaching_source_id_invalid".to_string())?;
    let expected = body.get("expectedRevision").and_then(Value::as_i64).filter(|v| *v > 0).ok_or("teaching_source_revision_required")?;
    let expected_sha = body.get("expectedFileSha256").and_then(Value::as_str).filter(|v| v.len()==64 && v.bytes().all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())).ok_or("teaching_source_file_sha_required")?;
    let status = body.get("extractionStatus").and_then(Value::as_str).unwrap_or("");
    if !["ready","needs_ocr","unsupported","failed"].contains(&status) { return Err("teaching_source_extraction_status_invalid".into()); }
    let extractor = optional_text(body, "extractorVersion", 80)?;
    let page_count = body.get("pageCount").and_then(Value::as_i64).filter(|v| *v >= 0 && *v <= 100_000);
    let links = body.get("curriculumLinks").and_then(Value::as_array).cloned().unwrap_or_default();
    if links.len() > 200 { return Err("teaching_source_curriculum_links_invalid".into()); }
    verify_managed_file(store,tenant,owner,&source_id,expected_sha)?;
    let now = Utc::now().timestamp_millis();
    let mut conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    let tx = conn.transaction().map_err(|e| format!("db_teaching_source_transaction_failed:{e}"))?;
    let current = tx.query_row("SELECT s.revision,s.sha256,h.backup_tenant_id FROM teaching_sources s JOIN teaching_source_actor_homes h ON h.owner_uid=s.owner_uid WHERE s.owner_uid=?1 AND s.source_id=?2",params![owner,source_id],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,String>(2)?))).optional().map_err(|e|format!("db_teaching_source_finalize_read_failed:{e}"))?.ok_or("teaching_source_not_found")?;
    if current.0 != expected || current.1.as_deref() != Some(expected_sha) { return Err("teaching_source_revision_conflict".into()); }
    let chunks: i64 = tx.query_row("SELECT COUNT(*) FROM teaching_source_chunks WHERE owner_uid=?1 AND source_id=?2", params![owner,source_id], |row| row.get(0))
        .map_err(|e| format!("db_teaching_source_chunk_count_failed:{e}"))?;
    if status == "ready" && chunks == 0 { return Err("teaching_source_ready_requires_chunks".into()); }
    let updated = tx.execute("UPDATE teaching_sources SET extraction_status=?1,extractor_version=?2,page_count=?3,revision=revision+1,updated_at_ms=?4 WHERE owner_uid=?5 AND source_id=?6 AND revision=?7 AND sha256=?8 AND local_path IS NOT NULL",
        params![status,extractor,page_count,now,owner,source_id,expected,expected_sha]).map_err(|e| format!("db_teaching_source_finalize_failed:{e}"))?;
    if updated != 1 { return Err("teaching_source_revision_conflict".into()); }
    tx.execute("DELETE FROM curriculum_source_links WHERE tenant_id=?1 AND owner_uid=?2 AND source_id=?3", params![tenant,owner,source_id])
        .map_err(|e| format!("db_teaching_source_links_reset_failed:{e}"))?;
    for link in links {
        let school_year = link.get("schoolYear").and_then(Value::as_i64).filter(|v| (2000..=2200).contains(v)).ok_or("teaching_source_school_year_invalid")?;
        let semester = link.get("semester").and_then(Value::as_i64).filter(|v| [1,2].contains(v)).ok_or("teaching_source_semester_invalid")?;
        let grade = link.get("grade").and_then(Value::as_i64).filter(|v| (1..=12).contains(v));
        let kind = text(&link, "curriculumSourceKind", 32)?;
        if !["class","grade_draft","grade_published"].contains(&kind.as_str()) { return Err("teaching_source_curriculum_authority_invalid".into()); }
        let source_scope_id = text(&link, "curriculumSourceScopeId", 200)?;
        let item = safe_id(link.get("curriculumItemId").and_then(Value::as_str).unwrap_or(""), 160).ok_or("teaching_source_curriculum_item_invalid")?;
        let source_revision = link.get("curriculumSourceRevision").and_then(Value::as_i64).filter(|v| *v>0).ok_or("teaching_source_curriculum_revision_invalid")?;
        let subject_snapshot = text(&link,"subjectCodeSnapshot",80)?;
        let unit_snapshot = optional_text(&link,"unitSnapshot",240)?;
        let title_snapshot = optional_text(&link,"titleSnapshot",300)?;
        let chunk = link.get("chunkId").and_then(Value::as_str).unwrap_or("");
        let chunk_id = if chunk.is_empty() { String::new() } else { safe_id(chunk,160).ok_or("teaching_source_chunk_id_invalid")? };
        if !chunk_id.is_empty() {
            let exists: i64 = tx.query_row("SELECT COUNT(*) FROM teaching_source_chunks WHERE owner_uid=?1 AND source_id=?2 AND chunk_id=?3", params![owner,source_id,chunk_id], |row| row.get(0)).map_err(|e| format!("db_teaching_source_link_chunk_check_failed:{e}"))?;
            if exists != 1 { return Err("teaching_source_link_chunk_missing".into()); }
        }
        tx.execute("INSERT INTO curriculum_source_links (tenant_id,owner_uid,school_year,semester,grade,curriculum_source_kind,curriculum_source_scope_id,curriculum_item_id,curriculum_source_revision,teaching_source_revision,subject_code_snapshot,unit_snapshot,title_snapshot,source_id,chunk_id,created_at_ms,updated_at_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?16)", params![tenant,owner,school_year,semester,grade,kind,source_scope_id,item,source_revision,expected+1,subject_snapshot,unit_snapshot,title_snapshot,source_id,chunk_id,now])
            .map_err(|e| format!("db_teaching_source_link_insert_failed:{e}"))?;
    }
    update_import_run(&tx, &current.2, owner, &source_id, status, now)?;
    tx.commit().map_err(|e| format!("db_teaching_source_finalize_commit_failed:{e}"))?;
    drop(conn);
    mark_backup_dirty(store,owner)?;
    source_row(store, owner, &source_id)?.ok_or_else(|| "teaching_source_not_found".into())
}

fn list_sources(store: &SqliteStore, _tenant: &str, owner: &str, url: &url::Url) -> Result<Value, String> {
    let subject = query(url, "subjectCode");
    let status = query(url, "status");
    let limit = query(url, "limit").parse::<i64>().unwrap_or(100).clamp(1, 200);
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    let include_archived=query(url,"includeArchived")=="true";
    let mut statement = conn.prepare("SELECT source_id FROM teaching_sources WHERE owner_uid=?1 AND (?2='' OR subject_code=?2) AND (?3='' OR extraction_status=?3) AND (?4=1 OR lifecycle_status='active') ORDER BY updated_at_ms DESC,source_id LIMIT ?5")
        .map_err(|e| format!("db_teaching_sources_list_prepare_failed:{e}"))?;
    let ids = statement.query_map(params![owner,subject,status,include_archived,limit], |row| row.get::<_,String>(0))
        .map_err(|e| format!("db_teaching_sources_list_failed:{e}"))?.collect::<Result<Vec<_>,_>>().map_err(|e| format!("db_teaching_sources_list_row_failed:{e}"))?;
    drop(statement); drop(conn);
    let sources = ids.into_iter().filter_map(|id| source_row(store,owner,&id).ok().flatten()).collect::<Vec<_>>();
    Ok(json!({"sources":sources,"complete":sources.len() < limit as usize,"backupState":"onedrive_included"}))
}

fn source_links(store:&SqliteStore,tenant:&str,owner:&str,source:&str)->Result<Vec<Value>,String>{
    let conn=store.conn.lock().map_err(|_|"db_lock_failed".to_string())?;
    let mut stmt=conn.prepare("SELECT school_year,semester,grade,curriculum_source_kind,curriculum_source_scope_id,curriculum_item_id,curriculum_source_revision,teaching_source_revision,subject_code_snapshot,unit_snapshot,title_snapshot,chunk_id,revision FROM curriculum_source_links WHERE tenant_id=?1 AND owner_uid=?2 AND source_id=?3 ORDER BY school_year,semester,curriculum_source_kind,curriculum_source_scope_id,curriculum_item_id,chunk_id")
        .map_err(|e|format!("db_teaching_source_links_prepare_failed:{e}"))?;
    let rows=stmt.query_map(params![tenant,owner,source],|row|Ok(json!({"schoolYear":row.get::<_,i64>(0)?,"semester":row.get::<_,i64>(1)?,"grade":row.get::<_,Option<i64>>(2)?,"curriculumSourceKind":row.get::<_,String>(3)?,"curriculumSourceScopeId":row.get::<_,String>(4)?,"curriculumItemId":row.get::<_,String>(5)?,"curriculumSourceRevision":row.get::<_,i64>(6)?,"teachingSourceRevision":row.get::<_,i64>(7)?,"subjectCodeSnapshot":row.get::<_,String>(8)?,"unitSnapshot":row.get::<_,String>(9)?,"titleSnapshot":row.get::<_,String>(10)?,"chunkId":row.get::<_,String>(11)?,"revision":row.get::<_,i64>(12)?})))
        .map_err(|e|format!("db_teaching_source_links_failed:{e}"))?;
    rows.collect::<Result<Vec<_>,_>>().map_err(|e|format!("db_teaching_source_link_row_failed:{e}"))
}

fn source_detail(store:&SqliteStore,tenant:&str,owner:&str,source:&str)->Result<Value,String>{
    let source_id=safe_id(source,160).ok_or("teaching_source_id_invalid")?;
    Ok(json!({"source":source_row(store,owner,&source_id)?.ok_or("teaching_source_not_found")?,"curriculumLinks":source_links(store,tenant,owner,&source_id)?}))
}

fn update_source(store:&SqliteStore,owner:&str,source:&str,body:&Value)->Result<Value,String>{
    let source_id=safe_id(source,160).ok_or("teaching_source_id_invalid")?;
    let expected=body.get("expectedRevision").and_then(Value::as_i64).filter(|v|*v>0).ok_or("teaching_source_revision_required")?;
    let current=source_row(store,owner,&source_id)?.ok_or("teaching_source_not_found")?;
    let source_type=body.get("sourceType").map(|_|text(body,"sourceType",40)).transpose()?.unwrap_or_else(||current["sourceType"].as_str().unwrap_or("").to_string());
    if !["textbook","teacher_guide","reference","worksheet","presentation","other"].contains(&source_type.as_str()){return Err("teaching_source_sourceType_invalid".into());}
    let grade=body.get("grade").map(|_|text(body,"grade",40)).transpose()?.unwrap_or_else(||current["grade"].as_str().unwrap_or("").to_string());
    let semester=body.get("semesterScope").map(|_|text(body,"semesterScope",16)).transpose()?.unwrap_or_else(||current["semesterScope"].as_str().unwrap_or("").to_string());
    if !["1","2","full_year"].contains(&semester.as_str()){return Err("teaching_source_semesterScope_invalid".into());}
    let subject=body.get("subjectCode").map(|_|text(body,"subjectCode",80)).transpose()?.unwrap_or_else(||current["subjectCode"].as_str().unwrap_or("").to_string());
    let title=body.get("title").map(|_|text(body,"title",240)).transpose()?.unwrap_or_else(||current["title"].as_str().unwrap_or("").to_string());
    let publisher=body.get("publisher").map(|_|text(body,"publisher",160)).transpose()?.unwrap_or_else(||current["publisher"].as_str().unwrap_or("").to_string());
    let now=Utc::now().timestamp_millis();
    let mut conn=store.conn.lock().map_err(|_|"db_lock_failed".to_string())?;
    let tx=conn.transaction().map_err(|e|format!("db_teaching_source_transaction_failed:{e}"))?;
    let changed=tx.execute("UPDATE teaching_sources SET source_type=?1,grade=?2,semester_scope=?3,subject_code=?4,title=?5,publisher=?6,revision=revision+1,updated_at_ms=?7 WHERE owner_uid=?8 AND source_id=?9 AND revision=?10",params![source_type,grade,semester,subject,title,publisher,now,owner,source_id,expected]).map_err(|e|format!("db_teaching_source_update_failed:{e}"))?;
    if changed!=1{return Err("teaching_source_revision_conflict".into());}
    tx.execute("UPDATE teaching_source_chunks_fts SET subject_code=?1,title=?2 WHERE owner_uid=?3 AND source_id=?4",params![subject,title,owner,source_id]).map_err(|e|format!("db_teaching_source_fts_update_failed:{e}"))?;
    tx.commit().map_err(|e|format!("db_teaching_source_update_commit_failed:{e}"))?;
    drop(conn);
    mark_backup_dirty(store,owner)?;
    source_row(store,owner,&source_id)?.ok_or("teaching_source_not_found".into())
}

fn archive_source(store:&SqliteStore,owner:&str,source:&str,body:&Value)->Result<Value,String>{
    let source_id=safe_id(source,160).ok_or("teaching_source_id_invalid")?;
    let expected=body.get("expectedRevision").and_then(Value::as_i64).filter(|v|*v>0).ok_or("teaching_source_revision_required")?;
    let now=Utc::now().timestamp_millis();
    let changed=store.conn.lock().map_err(|_|"db_lock_failed".to_string())?.execute("UPDATE teaching_sources SET lifecycle_status='archived',revision=revision+1,updated_at_ms=?1 WHERE owner_uid=?2 AND source_id=?3 AND revision=?4 AND lifecycle_status='active'",params![now,owner,source_id,expected]).map_err(|e|format!("db_teaching_source_archive_failed:{e}"))?;
    if changed!=1{return Err("teaching_source_revision_conflict".into());}
    mark_backup_dirty(store,owner)?;
    source_row(store,owner,&source_id)?.ok_or("teaching_source_not_found".into())
}

fn fts_query(raw: &str) -> Option<String> {
    let tokens = raw.split(|ch: char| !ch.is_alphanumeric()).filter(|token| token.chars().count() >= 2)
        .take(12).map(|token| format!("\"{}\"*", token.replace('"', ""))).collect::<Vec<_>>();
    if tokens.is_empty() { None } else { Some(tokens.join(" OR ")) }
}

pub(crate) fn mcp_matches(store: &SqliteStore, tenant: &str, owner: &str, input: &Value) -> Result<Value, String> {
    let lessons = input.get("lessons").and_then(Value::as_array).cloned().unwrap_or_default();
    if lessons.is_empty() || lessons.len() > 100 { return Err("INVALID_LOCAL_READ_REQUEST".into()); }
    let source_types=input.get("sourceTypes").and_then(Value::as_array).cloned().unwrap_or_default();
    let valid_types=["textbook","teacher_guide","reference","worksheet","presentation","other"];
    let source_type_values=source_types.iter().map(|value|value.as_str().filter(|item|valid_types.contains(item)).ok_or("INVALID_LOCAL_READ_REQUEST")).collect::<Result<Vec<_>,_>>()?;
    if source_type_values.len()>6||source_type_values.iter().collect::<HashSet<_>>().len()!=source_type_values.len(){return Err("INVALID_LOCAL_READ_REQUEST".into());}
    let source_type_filter=source_type_values.join(",");
    let limit = input.get("limit").and_then(Value::as_u64).unwrap_or(12).clamp(1,MAX_MCP_MATCHES as u64) as usize;
    let offset=input.get("offset").and_then(Value::as_u64).unwrap_or(0).min(100_000) as usize;
    let expected_digest=input.get("expectedSnapshotDigest").and_then(Value::as_str).unwrap_or("");
    if !expected_digest.is_empty() && (expected_digest.len()!=64 || !expected_digest.bytes().all(|ch|ch.is_ascii_hexdigit()&&!ch.is_ascii_uppercase())) {
        return Err("INVALID_LOCAL_READ_REQUEST".into());
    }
    let mut results: Vec<Value> = Vec::new();
    let mut diagnostics: Vec<Value> = Vec::new();
    let mut seen = HashSet::new();
    let mut fresh_source_links = HashSet::new();
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    for (lesson_index,lesson) in lessons.iter().enumerate() {
        let year = lesson.get("schoolYear").and_then(Value::as_i64).unwrap_or(0);
        let semester = lesson.get("semester").and_then(Value::as_i64).unwrap_or(0);
        let kind = lesson.get("curriculumSourceKind").and_then(Value::as_str).unwrap_or("");
        let scope_id = lesson.get("curriculumSourceScopeId").and_then(Value::as_str).unwrap_or("");
        let authority_revision = lesson.get("curriculumSourceRevision").and_then(Value::as_i64).unwrap_or(0);
        let item = lesson.get("curriculumItemId").and_then(Value::as_str).unwrap_or("");
        let curriculum_status=lesson.get("curriculumStatus").and_then(Value::as_str).unwrap_or("unlinked");
        if !(2000..=2200).contains(&year) || ![1,2].contains(&semester) || item.is_empty() || curriculum_status=="unlinked" { continue; }
        if curriculum_status=="qualified" && (!(["class","grade_draft","grade_published"].contains(&kind)) || scope_id.is_empty() || authority_revision<1) { return Err("INVALID_LOCAL_READ_REQUEST".into()); }
        let mut statement = conn.prepare("SELECT s.source_id,s.title,s.source_type,s.grade,s.semester_scope,s.subject_code,s.publisher,s.revision,s.sha256,c.chunk_id,c.page_start,c.page_end,c.unit,c.topic,c.revision,l.curriculum_source_revision,l.teaching_source_revision,l.subject_code_snapshot,l.unit_snapshot,l.title_snapshot,l.revision,substr(COALESCE(c.text,''),1,900),l.chunk_id FROM curriculum_source_links l JOIN teaching_sources s ON s.owner_uid=l.owner_uid AND s.source_id=l.source_id LEFT JOIN teaching_source_chunks c ON c.owner_uid=l.owner_uid AND c.source_id=l.source_id AND c.chunk_id=l.chunk_id WHERE l.tenant_id=?1 AND l.owner_uid=?2 AND l.school_year=?3 AND l.semester=?4 AND l.curriculum_item_id=?7 AND (?8='orphaned' OR (l.curriculum_source_kind=?5 AND l.curriculum_source_scope_id=?6)) AND s.extraction_status='ready' AND s.lifecycle_status='active' ORDER BY s.updated_at_ms DESC,s.source_id LIMIT 100")
            .map_err(|e| format!("db_teaching_source_exact_prepare_failed:{e}"))?;
        let subject_snapshot=lesson.get("subjectCode").and_then(Value::as_str).unwrap_or("");
        let unit_snapshot=lesson.get("unit").and_then(Value::as_str).unwrap_or("");
        let title_snapshot=lesson.get("title").and_then(Value::as_str).unwrap_or("");
        let rows = statement.query_map(params![tenant,owner,year,semester,kind,scope_id,item,curriculum_status], |row| {
          let stored_revision=row.get::<_,i64>(15)?;
          let stored_source_revision=row.get::<_,i64>(16)?;
          let stored_subject=row.get::<_,String>(17)?; let stored_unit=row.get::<_,String>(18)?; let stored_title=row.get::<_,String>(19)?;
           let linked_chunk=row.get::<_,String>(22)?;let chunk_present=row.get::<_,Option<String>>(9)?.is_some();
           let link_status=if curriculum_status=="orphaned" {"orphaned"} else if stored_revision==authority_revision && stored_source_revision==row.get::<_,i64>(7)? && stored_subject==subject_snapshot && stored_unit==unit_snapshot && stored_title==title_snapshot && (linked_chunk.is_empty()||chunk_present) {"fresh"} else {"stale"};
          Ok(json!({
            "sourceRef":row.get::<_,String>(0)?,"title":row.get::<_,String>(1)?,"sourceType":row.get::<_,String>(2)?,"grade":row.get::<_,String>(3)?,
            "semesterScope":row.get::<_,String>(4)?,"subjectCode":row.get::<_,String>(5)?,"publisher":row.get::<_,String>(6)?,"sourceRevision":row.get::<_,i64>(7)?,
            "fileSha256":row.get::<_,String>(8)?,"chunkRef":row.get::<_,Option<String>>(9)?,"pageStart":row.get::<_,Option<i64>>(10)?,"pageEnd":row.get::<_,Option<i64>>(11)?,
            "unit":row.get::<_,Option<String>>(12)?.unwrap_or_default(),"topic":row.get::<_,Option<String>>(13)?.unwrap_or_default(),"chunkRevision":row.get::<_,Option<i64>>(14)?,
            "snippet":row.get::<_,String>(21)?,"matchKind":"exact_curriculum_link","linkStatus":link_status,
            "matchReason":if link_status=="fresh" {"Yearbook 교육과정 항목에 정확히 연결됨"} else {"연결 snapshot이 현재 Yearbook 정본과 달라 진단용으로만 반환됨"},
            "lessonRef":{"dateKey":lesson.get("dateKey").and_then(Value::as_str).unwrap_or(""),"period":lesson.get("period").and_then(Value::as_i64).unwrap_or(0),"revision":lesson.get("revision").and_then(Value::as_i64).unwrap_or(0)},
             "_lessonIndex":lesson_index,"_linkRevision":row.get::<_,i64>(20)?,"_linkedChunkId":linked_chunk
           }))}).map_err(|e| format!("db_teaching_source_exact_failed:{e}"))?;
        let linked=rows.collect::<Result<Vec<_>,_>>().map_err(|e|format!("db_teaching_source_exact_row_failed:{e}"))?;drop(statement);
        for row in linked {
            if !source_type_values.is_empty()&&!source_type_values.contains(&row["sourceType"].as_str().unwrap_or("")){continue;}
            if row["linkStatus"]!="fresh" { diagnostics.push(row);continue; }
            if row["_linkedChunkId"].as_str().is_some_and(|value|!value.is_empty()) {
                let key = format!("{}:{}:{}",lesson_index,row["sourceRef"].as_str().unwrap_or(""),row["chunkRef"].as_str().unwrap_or(""));
                if seen.insert(key) { results.push(row); }
                continue;
            }
            fresh_source_links.insert(format!("{lesson_index}:{}", row["sourceRef"].as_str().unwrap_or("")));
            let raw=[lesson.get("unit").and_then(Value::as_str).unwrap_or(""),lesson.get("title").and_then(Value::as_str).unwrap_or(""),lesson.get("curriculumItemLabel").and_then(Value::as_str).unwrap_or(""),input.get("query").and_then(Value::as_str).unwrap_or(""),row.get("title").and_then(Value::as_str).unwrap_or("")].join(" ");
            let Some(search)=fts_query(&raw) else {continue;};let source_ref=row["sourceRef"].as_str().unwrap_or("");let link_revision=row["_linkRevision"].as_i64().unwrap_or(0);
            let mut source_stmt=conn.prepare("SELECT s.source_id,s.title,s.source_type,s.grade,s.semester_scope,s.subject_code,s.publisher,s.revision,s.sha256,c.chunk_id,c.page_start,c.page_end,c.unit,c.topic,c.revision,snippet(teaching_source_chunks_fts,7,'','', '…',18),bm25(teaching_source_chunks_fts) FROM teaching_source_chunks_fts JOIN teaching_source_chunks c ON c.owner_uid=teaching_source_chunks_fts.owner_uid AND c.source_id=teaching_source_chunks_fts.source_id AND c.chunk_id=teaching_source_chunks_fts.chunk_id JOIN teaching_sources s ON s.owner_uid=c.owner_uid AND s.source_id=c.source_id WHERE teaching_source_chunks_fts MATCH ?1 AND teaching_source_chunks_fts.owner_uid=?2 AND s.source_id=?3 AND (?4='' OR instr(','||?4||',',','||s.source_type||',')>0) AND s.extraction_status='ready' AND s.lifecycle_status='active' ORDER BY bm25(teaching_source_chunks_fts),c.ordinal,c.chunk_id LIMIT 12").map_err(|e|format!("db_teaching_source_linked_fts_prepare_failed:{e}"))?;
            let source_rows=source_stmt.query_map(params![search,owner,source_ref,source_type_filter],|row|Ok(json!({
              "sourceRef":row.get::<_,String>(0)?,"title":row.get::<_,String>(1)?,"sourceType":row.get::<_,String>(2)?,"grade":row.get::<_,String>(3)?,"semesterScope":row.get::<_,String>(4)?,"subjectCode":row.get::<_,String>(5)?,"publisher":row.get::<_,String>(6)?,"sourceRevision":row.get::<_,i64>(7)?,"fileSha256":row.get::<_,String>(8)?,"chunkRef":row.get::<_,String>(9)?,"pageStart":row.get::<_,Option<i64>>(10)?,"pageEnd":row.get::<_,Option<i64>>(11)?,"unit":row.get::<_,String>(12)?,"topic":row.get::<_,String>(13)?,"chunkRevision":row.get::<_,i64>(14)?,"snippet":row.get::<_,String>(15)?,"_score":row.get::<_,f64>(16)?,"matchKind":"lesson_context_fts","linkStatus":"fresh","matchReason":"정확히 연결된 원자료 안에서 단원·차시·원자료 제목으로 제한 검색함","lessonRef":{"dateKey":lesson.get("dateKey").and_then(Value::as_str).unwrap_or(""),"period":lesson.get("period").and_then(Value::as_i64).unwrap_or(0),"revision":lesson.get("revision").and_then(Value::as_i64).unwrap_or(0)},"_lessonIndex":lesson_index,"_linkRevision":link_revision
            }))).map_err(|e|format!("db_teaching_source_linked_fts_failed:{e}"))?;
            for source_row in source_rows {let source_row=source_row.map_err(|e|format!("db_teaching_source_linked_fts_row_failed:{e}"))?;let key=format!("{}:{}:{}",lesson_index,source_row["sourceRef"].as_str().unwrap_or(""),source_row["chunkRef"].as_str().unwrap_or(""));if seen.insert(key){results.push(source_row);}}
        }
    }
    for stage in ["lesson_context_fts","subject_query_fts"] {
      for (lesson_index,lesson) in lessons.iter().enumerate() {
        let subject=lesson.get("subjectCode").and_then(Value::as_str).filter(|value|!value.is_empty())
            .or_else(||lesson.get("subject").and_then(Value::as_str)).unwrap_or("");
        let grade=lesson.get("grade").and_then(Value::as_i64).filter(|value|(1..=12).contains(value));
        if subject.is_empty()||grade.is_none(){continue;} let grade=grade.unwrap();
        let raw=if stage=="lesson_context_fts" {
          [lesson.get("unit").and_then(Value::as_str).unwrap_or(""),lesson.get("title").and_then(Value::as_str).unwrap_or(""),lesson.get("curriculumItemLabel").and_then(Value::as_str).unwrap_or("")].join(" ")
        } else { input.get("query").and_then(Value::as_str).unwrap_or("").to_string() };
        if let Some(search) = fts_query(&raw) {
            let mut statement = conn.prepare("SELECT s.source_id,s.title,s.source_type,s.grade,s.semester_scope,s.subject_code,s.publisher,s.revision,s.sha256,c.chunk_id,c.page_start,c.page_end,c.unit,c.topic,c.revision,snippet(teaching_source_chunks_fts,7,'','', '…',18),bm25(teaching_source_chunks_fts) FROM teaching_source_chunks_fts JOIN teaching_source_chunks c ON c.owner_uid=teaching_source_chunks_fts.owner_uid AND c.source_id=teaching_source_chunks_fts.source_id AND c.chunk_id=teaching_source_chunks_fts.chunk_id JOIN teaching_sources s ON s.owner_uid=c.owner_uid AND s.source_id=c.source_id WHERE teaching_source_chunks_fts MATCH ?1 AND teaching_source_chunks_fts.owner_uid=?2 AND (?3='' OR s.subject_code=?3) AND s.grade IN (?4,?5) AND s.semester_scope IN (?6,'full_year') AND (?7='' OR instr(','||?7||',',','||s.source_type||',')>0) AND s.extraction_status='ready' AND s.lifecycle_status='active' ORDER BY bm25(teaching_source_chunks_fts),s.updated_at_ms DESC,s.source_id,c.chunk_id LIMIT ?8")
                .map_err(|e| format!("db_teaching_source_fts_prepare_failed:{e}"))?;
            let rows = statement.query_map(params![search,owner,subject,grade.to_string(),format!("{grade}학년"),lesson.get("semester").and_then(Value::as_i64).unwrap_or(0).to_string(),source_type_filter,100_i64], |row| Ok(json!({
                "sourceRef":row.get::<_,String>(0)?,"title":row.get::<_,String>(1)?,"sourceType":row.get::<_,String>(2)?,"grade":row.get::<_,String>(3)?,
                "semesterScope":row.get::<_,String>(4)?,"subjectCode":row.get::<_,String>(5)?,"publisher":row.get::<_,String>(6)?,"sourceRevision":row.get::<_,i64>(7)?,
                "fileSha256":row.get::<_,String>(8)?,"chunkRef":row.get::<_,String>(9)?,"pageStart":row.get::<_,Option<i64>>(10)?,"pageEnd":row.get::<_,Option<i64>>(11)?,
                "unit":row.get::<_,String>(12)?,"topic":row.get::<_,String>(13)?,"chunkRevision":row.get::<_,i64>(14)?,"snippet":row.get::<_,String>(15)?,"_score":row.get::<_,f64>(16)?,"matchKind":stage,"linkStatus":"unverified",
                "matchReason":if stage=="lesson_context_fts" {"같은 교과·학년·학기의 단원·차시 문맥 전문검색"} else {"같은 교과·학년·학기의 사용자 검색어 전문검색"},
                "lessonRef":{"dateKey":lesson.get("dateKey").and_then(Value::as_str).unwrap_or(""),"period":lesson.get("period").and_then(Value::as_i64).unwrap_or(0),"revision":lesson.get("revision").and_then(Value::as_i64).unwrap_or(0)},
                "_lessonIndex":lesson_index,"_linkRevision":0
            }))).map_err(|e| format!("db_teaching_source_fts_failed:{e}"))?;
            for row in rows {
                let row = row.map_err(|e| format!("db_teaching_source_fts_row_failed:{e}"))?;
                if fresh_source_links.contains(&format!("{lesson_index}:{}", row["sourceRef"].as_str().unwrap_or(""))) { continue; }
                let key = format!("{}:{}:{}",lesson_index,row["sourceRef"].as_str().unwrap_or(""),row["chunkRef"].as_str().unwrap_or(""));
                if seen.insert(key) { results.push(row); }
            }
        }
      }
    }
    let mut diagnostic_rows=Vec::new();let mut diagnostic_seen=HashSet::new();for row in diagnostics { let lesson_index=row["_lessonIndex"].as_u64().unwrap_or(0);let key=format!("{}:{}:{}:{}",lesson_index,row["sourceRef"].as_str().unwrap_or(""),row["chunkRef"].as_str().unwrap_or(""),row["linkStatus"].as_str().unwrap_or(""));if diagnostic_seen.insert(key){diagnostic_rows.push(row);} }
    let digest_rows=results.iter().chain(diagnostic_rows.iter()).map(|row|json!([row["lessonRef"],row["sourceRef"],row["chunkRef"],row["sourceRevision"],row["chunkRevision"],row["fileSha256"],row["matchKind"],row["linkStatus"],row["_linkRevision"]])).collect::<Vec<_>>();
    let snapshot_digest=format!("{:x}",Sha256::digest(serde_json::to_vec(&digest_rows).map_err(|_|"teaching_source_snapshot_failed")?));
    if !expected_digest.is_empty() && expected_digest!=snapshot_digest { return Ok(json!({"matches":[],"diagnostics":[],"diagnosticsTruncated":false,"complete":false,"nextOffset":offset,"snapshotDigest":snapshot_digest,"snapshotStale":true,"strategy":"exact_link_then_lesson_context_fts_then_subject_query_fts"})); }
    let complete=offset.saturating_add(limit)>=results.len();
    let mut page=results.into_iter().skip(offset).take(limit).collect::<Vec<_>>();
    for row in &mut page { if let Some(object)=row.as_object_mut(){object.remove("_lessonIndex");object.remove("_linkRevision");object.remove("_linkedChunkId");object.remove("_score");} }
    let diagnostics_truncated=diagnostic_rows.len()>20;diagnostic_rows.truncate(20);for row in &mut diagnostic_rows {if let Some(object)=row.as_object_mut(){object.remove("_lessonIndex");object.remove("_linkRevision");object.remove("_linkedChunkId");object.remove("_score");}}
    Ok(json!({"matches":page,"diagnostics":diagnostic_rows,"diagnosticsTruncated":diagnostics_truncated,"complete":complete,"nextOffset":if complete {Value::Null}else{json!(offset+page.len())},"snapshotDigest":snapshot_digest,"snapshotStale":false,"strategy":"exact_link_then_lesson_context_fts_then_subject_query_fts"}))
}

pub(crate) fn mcp_chunks(store: &SqliteStore, tenant: &str, owner: &str, input: &Value) -> Result<Value, String> {
    let refs = input.get("refs").and_then(Value::as_array).filter(|v| !v.is_empty() && v.len() <= MAX_MCP_CHUNKS).ok_or("INVALID_LOCAL_READ_REQUEST")?;
    let unique=refs.iter().map(|item|format!("{}:{}",item.get("sourceRef").and_then(Value::as_str).unwrap_or(""),item.get("chunkRef").and_then(Value::as_str).unwrap_or(""))).collect::<HashSet<_>>();
    if unique.len()!=refs.len(){return Err("INVALID_LOCAL_READ_REQUEST".into());}
    let max_chars=input.get("maxChars").and_then(Value::as_u64).filter(|v|*v>=1&&*v<=120_000).ok_or("INVALID_LOCAL_READ_REQUEST")? as usize;
    let mut verified=HashSet::new();
    for item in refs {
        let source=safe_id(item.get("sourceRef").and_then(Value::as_str).unwrap_or(""),160).ok_or("INVALID_LOCAL_READ_REQUEST")?;
        let file_sha=item.get("fileSha256").and_then(Value::as_str).filter(|value|value.len()==64&&value.bytes().all(|ch|ch.is_ascii_hexdigit()&&!ch.is_ascii_uppercase())).ok_or("INVALID_LOCAL_READ_REQUEST")?;
        if verified.insert(format!("{source}:{file_sha}")){verify_managed_file(store,tenant,owner,&source,file_sha)?;}
    }
    let conn = store.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
    let mut chunks = Vec::new();
    let mut total_chars=0usize;
    for item in refs {
        let source = safe_id(item.get("sourceRef").and_then(Value::as_str).unwrap_or(""),160).ok_or("INVALID_LOCAL_READ_REQUEST")?;
        let chunk = safe_id(item.get("chunkRef").and_then(Value::as_str).unwrap_or(""),160).ok_or("INVALID_LOCAL_READ_REQUEST")?;
        let source_revision=item.get("sourceRevision").and_then(Value::as_i64).filter(|v|*v>0).ok_or("INVALID_LOCAL_READ_REQUEST")?;
        let chunk_revision=item.get("chunkRevision").and_then(Value::as_i64).filter(|v|*v>0).ok_or("INVALID_LOCAL_READ_REQUEST")?;
        let file_sha=item.get("fileSha256").and_then(Value::as_str).filter(|value|value.len()==64&&value.bytes().all(|ch|ch.is_ascii_hexdigit()&&!ch.is_ascii_uppercase())).ok_or("INVALID_LOCAL_READ_REQUEST")?;
        let row = conn.query_row("SELECT c.chunk_id,c.ordinal,c.page_start,c.page_end,c.unit,c.topic,c.text,c.text_sha256,c.revision,s.source_id,s.title,s.source_type,s.subject_code,s.publisher,s.revision,s.sha256 FROM teaching_source_chunks c JOIN teaching_sources s ON s.owner_uid=c.owner_uid AND s.source_id=c.source_id WHERE c.owner_uid=?1 AND c.source_id=?2 AND c.chunk_id=?3 AND c.revision=?4 AND s.revision=?5 AND s.sha256=?6 AND s.extraction_status='ready' AND s.lifecycle_status='active'",
            params![owner,source,chunk,chunk_revision,source_revision,file_sha], |row| Ok(json!({"chunkRef":row.get::<_,String>(0)?,"ordinal":row.get::<_,i64>(1)?,"pageStart":row.get::<_,Option<i64>>(2)?,"pageEnd":row.get::<_,Option<i64>>(3)?,"unit":row.get::<_,String>(4)?,"topic":row.get::<_,String>(5)?,"text":row.get::<_,String>(6)?,"textSha256":row.get::<_,String>(7)?,"chunkRevision":row.get::<_,i64>(8)?,"sourceRef":row.get::<_,String>(9)?,"title":row.get::<_,String>(10)?,"sourceType":row.get::<_,String>(11)?,"subjectCode":row.get::<_,String>(12)?,"publisher":row.get::<_,String>(13)?,"sourceRevision":row.get::<_,i64>(14)?,"fileSha256":row.get::<_,String>(15)?})))
            .optional().map_err(|e| format!("db_teaching_source_chunk_read_failed:{e}"))?;
        let row=row.ok_or("teaching_source_stale")?;let count=row["text"].as_str().unwrap_or("").chars().count(); if total_chars+count>max_chars{return Err("teaching_source_max_chars_exceeded".into());} total_chars+=count;chunks.push(row);
    }
    Ok(json!({"chunks":chunks,"complete":true,"totalChars":total_chars}))
}

pub(crate) fn handle_http_request(request: &mut Request, store: &SqliteStore, browser_links: &BrowserLinkStore, origin: &str) -> Result<Option<ResponseBox>, String> {
    let url = parse_request_url(request)?;
    let path = url.path().to_string();
    if path != "/v1/teaching-sources" && !path.starts_with("/v1/teaching-sources/") { return Ok(None); }
    if request.method() == &Method::Options { return Ok(Some(json_response(200,json!({"ok":true}),origin).boxed())); }
    let principal = browser_links.principal_for_request(request).ok_or("browser_token_required")?;
    let tenant = principal.tenant_id;
    let owner = principal.uid;
    let result = if path == "/v1/teaching-sources" && request.method() == &Method::Get {
        list_sources(store,&tenant,&owner,&url)?
    } else if path == "/v1/teaching-sources" && request.method() == &Method::Post {
        json!({"source":create_source(store,&tenant,&owner,&read_json(request)?)?})
    } else {
        let suffix = path.trim_start_matches("/v1/teaching-sources/");
        let mut parts = suffix.split('/');
        let source = parts.next().unwrap_or("");
        let action = parts.next().unwrap_or("");
        if parts.next().is_some() { return Ok(Some(json_response(404,json!({"ok":false,"error":"not_found"}),origin).boxed())); }
        match (request.method(),action) {
            (&Method::Get,"") => source_detail(store,&tenant,&owner,source)?,
            (&Method::Patch,"") => json!({"source":update_source(store,&owner,source,&read_json(request)?)?}),
            (&Method::Put,"file") => json!({"source":save_file(store,&tenant,&owner,source,request)?}),
            (&Method::Post,"chunks") => json!({"result":save_chunks(store,&tenant,&owner,source,&read_json(request)?)?}),
            (&Method::Post,"finalize") => json!({"source":finalize_source(store,&tenant,&owner,source,&read_json(request)?)?}),
            (&Method::Post,"archive") => json!({"source":archive_source(store,&owner,source,&read_json(request)?)?}),
            _ => return Ok(Some(json_response(404,json!({"ok":false,"error":"not_found"}),origin).boxed())),
        }
    };
    Ok(Some(json_response(200,json!({"ok":true,"data":result}),origin).boxed()))
}

pub(crate) fn resolve_local_path(store: &SqliteStore, relative: &str) -> Result<PathBuf,String> {
    let base=fs::canonicalize(&store.data_dir).map_err(|_|"local_data_dir_missing".to_string())?;
    let unchecked=checked_path(store,relative)?;let rel=Path::new(relative);let mut cursor=base.clone();
    for component in rel.components(){if let Component::Normal(part)=component{cursor.push(part);let meta=fs::symlink_metadata(&cursor).map_err(|_|"teaching_source_file_missing".to_string())?;if meta.file_type().is_symlink(){return Err("teaching_source_path_invalid".into());}}}
    let target=fs::canonicalize(unchecked).map_err(|_|"teaching_source_file_missing".to_string())?;
    if !target.starts_with(base) || !target.is_file(){return Err("teaching_source_path_invalid".into());}
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_supported_extensions_and_fts_tokens() {
        assert_eq!(extension("지도서.PDF"),Some("pdf"));
        assert_eq!(extension("수업.pptx"),Some("pptx"));
        assert_eq!(extension("실행.exe"),None);
        assert_eq!(fts_query("사회 우리 고장"),Some("\"사회\"* OR \"우리\"* OR \"고장\"*".into()));
    }
    #[test]
    fn managed_paths_never_embed_actor_identity() {
        let folder=actor_folder("teacher@example.test");
        assert_eq!(folder.len(),32);
        assert!(!folder.contains("teacher"));
    }
    #[test]
    fn shared_json_response_emits_one_complete_cors_policy() {
        let response=json_response(200,json!({"ok":true}),"https://t.classaimate.com");
        let origins=response.headers().iter().filter(|header|header.field.equiv("Access-Control-Allow-Origin")).collect::<Vec<_>>();
        assert_eq!(origins.len(),1);
        assert_eq!(origins[0].value.as_str(),"https://t.classaimate.com");
        let methods=response.headers().iter().find(|header|header.field.equiv("Access-Control-Allow-Methods")).expect("CORS methods");
        assert!(methods.value.as_str().split(',').any(|method|method.trim()=="PATCH"));
    }
}
