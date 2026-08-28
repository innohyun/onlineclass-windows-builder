use crate::{device_sync::DeviceSyncManager, normalize_json_text, SqliteStore};
use chrono::Utc;
use fs2::FileExt;
use rand::{distributions::Alphanumeric, Rng};
use regex::{Regex, RegexBuilder};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};

#[path = "student_record_mcp_protocol.rs"]
mod protocol;
#[path = "student_record_mcp_work_bundle.rs"]
mod work_bundle;

pub(crate) const SESSION_DB_FILE: &str = "student-record-mcp-session.sqlite";
const SAVE_LOCK_FILE: &str = "student-record-mcp-save.lock";
const SELECTION_TTL_MS: i64 = 30 * 60 * 1000;
const BUNDLE_TTL_MS: i64 = 60 * 60 * 1000;
const AUDIT_TTL_MS: i64 = 90 * 24 * 60 * 60 * 1000;
const MAX_STUDENTS: usize = 30;
const MAX_PRIVACY_ROSTER: usize = 100;
const MAX_EVIDENCE: usize = 24;
const MAX_EVIDENCE_CHARS: usize = 12_000;
const MAX_EVIDENCE_ITEM_CHARS: usize = 1_200;
const MAX_DRAFT_CHARS: usize = 2_400;

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}
fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn identifier(prefix: &str) -> String {
    let value: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();
    format!("{prefix}:{value}")
}
fn valid_identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}
fn clean(value: Option<&Value>, max: usize) -> String {
    normalize_json_text(value, max)
}
fn date_key(value: Option<&Value>) -> String {
    let value = clean(value, 10);
    if value.len() == 10 && value.as_bytes()[4] == b'-' && value.as_bytes()[7] == b'-' {
        value
    } else {
        String::new()
    }
}
fn json_object(value: &str, code: &str) -> Result<Value, String> {
    serde_json::from_str::<Value>(value).map_err(|_| code.to_string())
}
fn value_text<'a>(value: &'a Value, keys: &[&str]) -> String {
    for key in keys {
        let text = clean(value.get(*key), 40_000);
        if !text.is_empty() {
            return text;
        }
    }
    String::new()
}
fn student_code(value: Option<&Value>) -> Result<String, String> {
    let value = clean(value, 80).to_uppercase();
    if !valid_identifier(&value, 80) {
        return Err("LOCAL_SELECTION_REQUIRED".to_string());
    }
    Ok(value)
}
fn source_date(value: &Value) -> String {
    date_key(
        value
            .get("dateKey")
            .or_else(|| value.get("date"))
            .or_else(|| value.get("resultDate"))
            .or_else(|| value.get("scheduledDate")),
    )
}
fn source_subject(value: &Value) -> String {
    let direct = value_text(value, &["subject", "area"]);
    if !direct.is_empty() {
        return direct;
    }
    clean(value.pointer("/lessonContext/subject"), 160)
}
fn is_active_observation(value: &Value) -> bool {
    let status = clean(value.get("status"), 40).to_lowercase();
    let state = clean(value.get("recordState"), 40).to_lowercase();
    if status == "archived"
        || state == "archived"
        || value.get("isActive").and_then(Value::as_bool) == Some(false)
    {
        return false;
    }
    if value
        .get("archivedAtMs")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        > 0
        || value
            .get("deletedAtMs")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            > 0
    {
        return false;
    }
    !value_text(
        value,
        &["note", "content", "observationText", "memo", "comment"],
    )
    .is_empty()
}
fn creative_area(subject: &str) -> String {
    if subject.contains("동아리") {
        "동아리"
    } else if subject.contains("봉사") {
        "봉사"
    } else if subject.contains("진로") {
        "진로"
    } else if subject.contains("자율") || subject.contains("자치") {
        "자율"
    } else if subject.contains("창체") || subject.contains("창의적 체험") {
        "창체"
    } else {
        ""
    }
    .to_string()
}
fn observation_domain(value: &Value) -> (String, String) {
    let explicit = clean(value.get("recordDomain"), 20);
    let subject = source_subject(value);
    if explicit == "behavior" || explicit == "subjects" {
        return (explicit, String::new());
    }
    if explicit == "creative" {
        let area = value_text(value, &["creativeArea"]);
        return (
            explicit,
            if area.is_empty() {
                creative_area(&subject)
            } else {
                area
            },
        );
    }
    let context = clean(value.get("contextType"), 40);
    if context != "lesson" && value.get("period").and_then(Value::as_i64).unwrap_or(0) <= 0 {
        return ("behavior".to_string(), String::new());
    }
    let area = creative_area(&subject);
    if !area.is_empty() {
        return ("creative".to_string(), area);
    }
    if !subject.is_empty() {
        return ("subjects".to_string(), String::new());
    }
    (String::new(), String::new())
}
fn observation_matches(value: &Value, scope: &Value) -> bool {
    if !is_active_observation(value) {
        return false;
    }
    let (domain, area) = observation_domain(value);
    match scope
        .get("recordType")
        .and_then(Value::as_str)
        .unwrap_or("")
    {
        "subjects" => {
            domain == "subjects" && source_subject(value) == clean(scope.get("subject"), 160)
        }
        "creative" => domain == "creative" && area == clean(scope.get("creativeArea"), 40),
        "behavior" => domain == "behavior",
        _ => false,
    }
}
fn evaluation_text(value: &Value) -> String {
    let mode = clean(value.get("resultMode").or_else(|| value.get("mode")), 40).to_lowercase();
    let custom = value_text(value, &["customResultText"]);
    let resolved = if mode == "custom"
        || mode == "custom_text"
        || mode == "manual"
        || (mode.is_empty() && !custom.is_empty())
    {
        value_text(value, &["customResultText", "resultText", "levelLabel"])
    } else {
        let index = value
            .get("levelIndex")
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        let selected = value
            .get("levelTexts")
            .and_then(Value::as_array)
            .and_then(|rows| rows.get(index.max(0) as usize))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if index >= 0 && !selected.is_empty() {
            selected
        } else {
            value_text(value, &["levelLabel", "resultText"])
        }
    };
    let mut seen = HashSet::new();
    [
        value_text(value, &["title", "evaluationTitle", "planTitle", "name"]),
        value_text(
            value,
            &[
                "coreStandard",
                "achievementStandard",
                "standard",
                "coreAchievementStandard",
            ],
        ),
        resolved,
        value_text(value, &["note", "memo", "comment"]),
    ]
    .into_iter()
    .filter(|entry| !entry.is_empty() && seen.insert(entry.clone()))
    .collect::<Vec<_>>()
    .join(" · ")
}
fn evaluation_included(value: &Value, code: &str) -> bool {
    if value.get("isExcluded").and_then(Value::as_bool) == Some(true)
        || value.get("excluded").and_then(Value::as_bool) == Some(true)
        || value.get("isRecorded").and_then(Value::as_bool) == Some(false)
    {
        return false;
    }
    if value
        .get("excludedStudentIds")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .any(|row| clean(Some(row), 160).to_uppercase() == code)
        })
        .unwrap_or(false)
    {
        return false;
    }
    if value.get("isRecorded").and_then(Value::as_bool) == Some(true) {
        return !evaluation_text(value).is_empty();
    }
    let level = value
        .get("levelIndex")
        .and_then(Value::as_i64)
        .map(|v| v >= 0)
        .unwrap_or(false);
    let custom = !value_text(value, &["customResultText", "resultText"]).is_empty();
    let score = value
        .get("score")
        .map(|row| !row.is_null() && !clean(Some(row), 40).is_empty())
        .unwrap_or(false);
    (level || custom || score) && !evaluation_text(value).is_empty()
}

fn pii_regexes() -> &'static [Regex; 4] {
    static PATTERNS: OnceLock<[Regex; 4]> = OnceLock::new();
    PATTERNS.get_or_init(|| [
        RegexBuilder::new(r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b").build().unwrap(),
        Regex::new(r"(?:^|\D)(?:01[016789]|0[2-6][1-5]?)[-\s)]?\d{3,4}[-\s]?\d{4}(?:\D|$)").unwrap(),
        Regex::new(r"(?:^|\D)\d{6}[-\s]?[1-4]\d{6}(?:\D|$)").unwrap(),
        RegexBuilder::new(r"(?i)\b(?:password|passwd|api[_ -]?key|secret|access[_ -]?token|refresh[_ -]?token)\b\s*[:=]\s*\S{8,}").build().unwrap(),
    ])
}
fn sanitize(raw: &str, identities: &[Value]) -> Result<String, String> {
    let patterns = pii_regexes();
    if patterns[2].is_match(raw) || patterns[3].is_match(raw) {
        return Err("PII_OUTPUT_BLOCKED".to_string());
    }
    let mut value = raw.trim().to_string();
    for identity in identities {
        let alias = clean(identity.get("alias"), 20);
        for target in [
            clean(identity.get("studentName"), 160),
            clean(identity.get("studentCode"), 80),
        ] {
            if target.chars().count() >= 2 {
                let pattern = RegexBuilder::new(&regex::escape(&target))
                    .case_insensitive(true)
                    .build()
                    .map_err(|_| "PII_OUTPUT_BLOCKED".to_string())?;
                value = pattern.replace_all(&value, alias.as_str()).to_string();
            }
        }
    }
    value = patterns[0].replace_all(&value, "[이메일 제거]").to_string();
    value = patterns[1]
        .replace_all(&value, "[전화번호 제거]")
        .to_string();
    Ok(value.trim().to_string())
}
fn reject_draft_pii(raw: &str, identities: &[Value]) -> Result<(), String> {
    if pii_regexes().iter().any(|pattern| pattern.is_match(raw)) {
        return Err("PII_OUTPUT_BLOCKED".to_string());
    }
    for identity in identities {
        for target in [
            clean(identity.get("studentName"), 160),
            clean(identity.get("studentCode"), 80),
        ] {
            if target.chars().count() >= 2 {
                let pattern = RegexBuilder::new(&regex::escape(&target))
                    .case_insensitive(true)
                    .build()
                    .map_err(|_| "PII_OUTPUT_BLOCKED".to_string())?;
                if pattern.is_match(raw) {
                    return Err("PII_OUTPUT_BLOCKED".to_string());
                }
            }
        }
    }
    Ok(())
}

fn scope_from(input: &Value) -> Result<Value, String> {
    let record_type = clean(input.get("recordType"), 20);
    if !matches!(record_type.as_str(), "behavior" | "subjects" | "creative") {
        return Err("LOCAL_SELECTION_REQUIRED".to_string());
    }
    let subject = if record_type == "subjects" {
        clean(input.get("subject"), 160)
    } else {
        String::new()
    };
    let area = if record_type == "creative" {
        clean(input.get("creativeArea"), 40)
    } else {
        String::new()
    };
    let from = date_key(input.get("fromDate").or_else(|| input.get("from")));
    let to = date_key(input.get("toDate").or_else(|| input.get("to")));
    if (record_type == "subjects" && subject.is_empty())
        || (record_type == "creative" && area.is_empty())
        || from.is_empty()
        || to.is_empty()
        || from > to
    {
        return Err("LOCAL_SELECTION_REQUIRED".to_string());
    }
    Ok(
        json!({"recordType":record_type,"subject":subject,"creativeArea":area,"fromDate":from,"toDate":to}),
    )
}

fn draft_text(value: &Value, scope: &Value) -> String {
    match scope
        .get("recordType")
        .and_then(Value::as_str)
        .unwrap_or("")
    {
        "behavior" => clean(value.get("behaviorComment"), 20_000),
        "subjects" => value
            .get("subjectComments")
            .and_then(Value::as_array)
            .and_then(|rows| {
                rows.iter()
                    .find(|row| clean(row.get("subject"), 160) == clean(scope.get("subject"), 160))
            })
            .map(|row| clean(row.get("comment"), 20_000))
            .unwrap_or_default(),
        "creative" => value
            .get("creativeComments")
            .and_then(Value::as_array)
            .and_then(|rows| {
                rows.iter()
                    .find(|row| clean(row.get("area"), 40) == clean(scope.get("creativeArea"), 40))
            })
            .map(|row| clean(row.get("comment"), 20_000))
            .unwrap_or_default(),
        _ => String::new(),
    }
}
fn draft_matches(value: &Value, scope: &Value) -> bool {
    let kind = clean(value.get("recordType"), 20);
    if kind != clean(scope.get("recordType"), 20) {
        return false;
    }
    !draft_text(value, scope).is_empty()
}

#[derive(Clone)]
struct DraftSnapshot {
    text: String,
    status: String,
    digest: String,
}

pub(crate) struct StudentRecordMcpManager {
    db: Mutex<Connection>,
    save_lock: Mutex<()>,
    save_lock_file: PathBuf,
    store: Arc<SqliteStore>,
    device_sync: Arc<DeviceSyncManager>,
}

impl StudentRecordMcpManager {
    pub(crate) fn open(
        data_dir: PathBuf,
        store: Arc<SqliteStore>,
        device_sync: Arc<DeviceSyncManager>,
    ) -> Result<Self, String> {
        fs::create_dir_all(&data_dir).map_err(|e| format!("student_record_mcp_dir_failed:{e}"))?;
        let conn = Connection::open(data_dir.join(SESSION_DB_FILE))
            .map_err(|e| format!("student_record_mcp_db_open_failed:{e}"))?;
        conn.execute_batch(r#"
          PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;
          CREATE TABLE IF NOT EXISTS connections(grant_id TEXT PRIMARY KEY,tenant_id TEXT NOT NULL,device_id TEXT NOT NULL,mode TEXT NOT NULL CHECK(mode IN('read','read_write')),status TEXT NOT NULL CHECK(status IN('active','disconnected')),activated_at INTEGER NOT NULL,expires_at INTEGER NOT NULL,disconnected_at INTEGER) WITHOUT ROWID,STRICT;
          CREATE UNIQUE INDEX IF NOT EXISTS idx_srmcp_connection_active ON connections(device_id) WHERE status='active';
          CREATE TABLE IF NOT EXISTS selections(selection_handle TEXT PRIMARY KEY,scope_id TEXT NOT NULL UNIQUE,grant_id TEXT NOT NULL,tenant_id TEXT NOT NULL,payload_json TEXT NOT NULL,payload_digest TEXT NOT NULL,created_at INTEGER NOT NULL,expires_at INTEGER NOT NULL,FOREIGN KEY(grant_id) REFERENCES connections(grant_id) ON DELETE CASCADE) WITHOUT ROWID,STRICT;
          CREATE TABLE IF NOT EXISTS bundles(bundle_id TEXT PRIMARY KEY,selection_handle TEXT NOT NULL UNIQUE,grant_id TEXT NOT NULL,context_json TEXT NOT NULL,mapping_json TEXT NOT NULL,created_at INTEGER NOT NULL,expires_at INTEGER NOT NULL,state TEXT NOT NULL CHECK(state IN('active','saving','saved')),save_digest TEXT,receipt_json TEXT,FOREIGN KEY(selection_handle) REFERENCES selections(selection_handle) ON DELETE CASCADE,FOREIGN KEY(grant_id) REFERENCES connections(grant_id) ON DELETE CASCADE) WITHOUT ROWID,STRICT;
          CREATE TABLE IF NOT EXISTS audit_events(id TEXT PRIMARY KEY,action TEXT NOT NULL,grant_id TEXT,record_type TEXT,student_count INTEGER NOT NULL DEFAULT 0,evidence_count INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL,expires_at INTEGER NOT NULL) WITHOUT ROWID,STRICT;
          CREATE INDEX IF NOT EXISTS idx_srmcp_audit_expiry ON audit_events(expires_at);
        "#).map_err(|e| format!("student_record_mcp_schema_failed:{e}"))?;
        let manager = Self {
            db: Mutex::new(conn),
            save_lock: Mutex::new(()),
            save_lock_file: data_dir.join(SAVE_LOCK_FILE),
            store,
            device_sync,
        };
        manager.purge()?;
        Ok(manager)
    }
    fn process_save_lock(&self) -> Result<fs::File, String> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&self.save_lock_file)
            .map_err(|e| format!("student_record_mcp_save_lock_open_failed:{e}"))?;
        file.lock_exclusive()
            .map_err(|e| format!("student_record_mcp_save_lock_failed:{e}"))?;
        Ok(file)
    }
    fn purge(&self) -> Result<(), String> {
        let now = now_ms();
        let mut conn = self
            .db
            .lock()
            .map_err(|_| "student_record_mcp_db_lock_failed".to_string())?;
        let tx = conn
            .transaction()
            .map_err(|e| format!("student_record_mcp_purge_begin_failed:{e}"))?;
        tx.execute("DELETE FROM bundles WHERE expires_at<=?1", params![now])
            .map_err(|e| format!("student_record_mcp_purge_failed:{e}"))?;
        tx.execute(
            "DELETE FROM selections
             WHERE expires_at<=?1
               AND NOT EXISTS (
                 SELECT 1 FROM bundles
                 WHERE bundles.selection_handle=selections.selection_handle
                   AND bundles.expires_at>?1
               )",
            params![now],
        )
        .map_err(|e| format!("student_record_mcp_purge_failed:{e}"))?;
        tx.execute("UPDATE connections SET status='disconnected',disconnected_at=COALESCE(disconnected_at,?1) WHERE status='active' AND expires_at<=?1", params![now]).map_err(|e| format!("student_record_mcp_purge_failed:{e}"))?;
        tx.execute(
            "DELETE FROM audit_events WHERE expires_at<=?1",
            params![now],
        )
        .map_err(|e| format!("student_record_mcp_purge_failed:{e}"))?;
        tx.commit()
            .map_err(|e| format!("student_record_mcp_purge_commit_failed:{e}"))
    }
    fn audit(
        &self,
        action: &str,
        grant: &str,
        record_type: &str,
        students: usize,
        evidence: usize,
    ) -> Result<(), String> {
        let now = now_ms();
        self.db.lock().map_err(|_| "student_record_mcp_db_lock_failed".to_string())?.execute(
            "INSERT INTO audit_events(id,action,grant_id,record_type,student_count,evidence_count,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![identifier("srmcp-local-audit"),action,if grant.is_empty(){None}else{Some(grant)},if record_type.is_empty(){None}else{Some(record_type)},students as i64,evidence as i64,now,now+AUDIT_TTL_MS]
        ).map_err(|e| format!("student_record_mcp_audit_failed:{e}"))?;
        Ok(())
    }
    fn active_connection(&self, grant: Option<&str>) -> Result<Value, String> {
        self.purge()?;
        let conn = self
            .db
            .lock()
            .map_err(|_| "student_record_mcp_db_lock_failed".to_string())?;
        let sql = if grant.is_some() {
            "SELECT grant_id,tenant_id,device_id,mode,expires_at FROM connections WHERE grant_id=?1 AND status='active' AND expires_at>?2"
        } else {
            "SELECT grant_id,tenant_id,device_id,mode,expires_at FROM connections WHERE status='active' AND expires_at>?1 ORDER BY activated_at DESC LIMIT 1"
        };
        let row = if let Some(grant) = grant {
            conn.query_row(sql, params![grant,now_ms()], |row| Ok(json!({"grantId":row.get::<_,String>(0)?,"tenantId":row.get::<_,String>(1)?,"deviceId":row.get::<_,String>(2)?,"mode":row.get::<_,String>(3)?,"expiresAt":row.get::<_,i64>(4)?}))).optional()
        } else {
            conn.query_row(sql, params![now_ms()], |row| Ok(json!({"grantId":row.get::<_,String>(0)?,"tenantId":row.get::<_,String>(1)?,"deviceId":row.get::<_,String>(2)?,"mode":row.get::<_,String>(3)?,"expiresAt":row.get::<_,i64>(4)?}))).optional()
        }.map_err(|e| format!("student_record_mcp_connection_query_failed:{e}"))?;
        row.ok_or_else(|| "MCP_GRANT_REQUIRED".to_string())
    }
    fn authorize(&self, connection: &Value, tool: &str) -> Result<(), String> {
        let grant = clean(connection.get("grantId"), 128);
        self.device_sync
            .authorize_student_record_mcp_tool(&grant, tool)?;
        Ok(())
    }
    pub(crate) fn status(&self, tenant_id: &str) -> Result<Value, String> {
        self.purge()?;
        let conn = self
            .db
            .lock()
            .map_err(|_| "student_record_mcp_db_lock_failed".to_string())?;
        let row = conn.query_row("SELECT grant_id,device_id,mode,expires_at FROM connections WHERE tenant_id=?1 AND status='active' ORDER BY activated_at DESC LIMIT 1",params![tenant_id],|row|Ok(json!({"grantId":row.get::<_,String>(0)?,"deviceId":row.get::<_,String>(1)?,"mode":row.get::<_,String>(2)?,"expiresAt":row.get::<_,i64>(3)?}))).optional().map_err(|e|format!("student_record_mcp_status_failed:{e}"))?;
        drop(conn);
        let device = self
            .device_sync
            .student_record_mcp_identity()
            .ok()
            .filter(|identity| clean(identity.get("tenantId"), 128) == tenant_id)
            .map(|identity| {
                json!({
                    "deviceId": identity.get("deviceId"),
                    "appVersion": identity.get("appVersion"),
                })
            });
        Ok(json!({"connected":row.is_some(),"connection":row,"device":device}))
    }
    pub(crate) fn activate(&self, browser_tenant: &str, body: &Value) -> Result<Value, String> {
        let tenant = clean(body.get("tenantId"), 128);
        let grant = clean(body.get("grantId"), 128);
        let mode = clean(body.get("mode"), 20);
        let expires = body.get("expiresAt").and_then(Value::as_i64).unwrap_or(0);
        let identity = self.device_sync.student_record_mcp_identity()?;
        let device = clean(identity.get("deviceId"), 128);
        if tenant != browser_tenant
            || tenant != clean(identity.get("tenantId"), 128)
            || !valid_identifier(&grant, 128)
            || !matches!(mode.as_str(), "read" | "read_write")
            || expires <= now_ms()
            || expires > now_ms() + 2 * 60 * 60 * 1000
        {
            return Err("MCP_GRANT_REQUIRED".to_string());
        }
        let mut conn = self
            .db
            .lock()
            .map_err(|_| "student_record_mcp_db_lock_failed".to_string())?;
        let tx = conn
            .transaction()
            .map_err(|e| format!("student_record_mcp_activate_begin_failed:{e}"))?;
        tx.execute("UPDATE connections SET status='disconnected',disconnected_at=?1 WHERE device_id=?2 AND status='active'",params![now_ms(),device]).map_err(|e|format!("student_record_mcp_activate_failed:{e}"))?;
        tx.execute("INSERT INTO connections(grant_id,tenant_id,device_id,mode,status,activated_at,expires_at,disconnected_at) VALUES(?1,?2,?3,?4,'active',?5,?6,NULL) ON CONFLICT(grant_id) DO UPDATE SET tenant_id=excluded.tenant_id,device_id=excluded.device_id,mode=excluded.mode,status='active',activated_at=excluded.activated_at,expires_at=excluded.expires_at,disconnected_at=NULL",params![grant,tenant,device,mode,now_ms(),expires]).map_err(|e|format!("student_record_mcp_activate_failed:{e}"))?;
        tx.commit()
            .map_err(|e| format!("student_record_mcp_activate_commit_failed:{e}"))?;
        drop(conn);
        self.audit("connection_activated", &grant, "", 0, 0)?;
        self.status(&tenant)
    }
    pub(crate) fn disconnect(&self, browser_tenant: &str, body: &Value) -> Result<Value, String> {
        let tenant = clean(body.get("tenantId"), 128);
        if tenant != browser_tenant {
            return Err("tenant_scope_mismatch".to_string());
        }
        let now = now_ms();
        let mut conn = self
            .db
            .lock()
            .map_err(|_| "student_record_mcp_db_lock_failed".to_string())?;
        let tx = conn
            .transaction()
            .map_err(|e| format!("student_record_mcp_disconnect_begin_failed:{e}"))?;
        let grant:Option<String>=tx.query_row("SELECT grant_id FROM connections WHERE tenant_id=?1 AND status='active' ORDER BY activated_at DESC LIMIT 1",params![tenant],|row|row.get(0)).optional().map_err(|e|format!("student_record_mcp_disconnect_failed:{e}"))?;
        if let Some(ref id) = grant {
            tx.execute(
                "UPDATE connections SET status='disconnected',disconnected_at=?1 WHERE grant_id=?2",
                params![now, id],
            )
            .map_err(|e| format!("student_record_mcp_disconnect_failed:{e}"))?;
            tx.execute("DELETE FROM selections WHERE grant_id=?1", params![id])
                .map_err(|e| format!("student_record_mcp_disconnect_failed:{e}"))?;
        }
        tx.commit()
            .map_err(|e| format!("student_record_mcp_disconnect_commit_failed:{e}"))?;
        drop(conn);
        if let Some(id) = grant {
            self.audit("connection_disconnected", &id, "", 0, 0)?;
        }
        Ok(json!({"connected":false}))
    }

    fn latest_draft(
        &self,
        tenant: &str,
        code: &str,
        scope: &Value,
    ) -> Result<DraftSnapshot, String> {
        let conn = self
            .store
            .conn
            .lock()
            .map_err(|_| "db_lock_failed".to_string())?;
        let mut stmt=conn.prepare("SELECT draft_id,payload_json,updated_at_ms FROM student_record_drafts WHERE tenant_id=?1 AND student_code=?2 ORDER BY updated_at_ms DESC,draft_id DESC").map_err(|e|format!("student_record_mcp_draft_query_failed:{e}"))?;
        let rows = stmt
            .query_map(params![tenant, code], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| format!("student_record_mcp_draft_query_failed:{e}"))?;
        for row in rows {
            let (id, raw, updated) =
                row.map_err(|e| format!("student_record_mcp_draft_row_failed:{e}"))?;
            let value = json_object(&raw, "student_record_mcp_draft_decode_failed")?;
            if draft_matches(&value, scope) {
                let text = draft_text(&value, scope);
                let status = clean(value.get("status"), 40);
                let hash = digest(
                    &serde_json::to_string(
                        &json!({"draftId":id,"text":text,"updatedAtMs":updated}),
                    )
                    .unwrap(),
                );
                return Ok(DraftSnapshot {
                    text,
                    status,
                    digest: hash,
                });
            }
        }
        let hash = digest("{\"draftId\":\"\",\"text\":\"\",\"updatedAtMs\":0}");
        Ok(DraftSnapshot {
            text: String::new(),
            status: String::new(),
            digest: hash,
        })
    }

    fn saved_mcp_batch(
        &self,
        tenant: &str,
        set_id: &str,
        scope: &Value,
        identities: &[Value],
        by_alias: &HashMap<String, String>,
    ) -> Result<Option<i64>, String> {
        let sets = self.store.list_student_record_draft_sets(
            tenant.to_string(),
            set_id.to_string(),
            String::new(),
            2,
        )?;
        let drafts = self.store.list_student_record_drafts(
            tenant.to_string(),
            String::new(),
            set_id.to_string(),
            String::new(),
            identities.len() as i64 + 1,
        )?;
        if sets.is_empty() && drafts.is_empty() {
            return Ok(None);
        }
        if sets.len() != 1 || drafts.len() != identities.len() {
            return Err("DRAFT_CONFLICT".to_string());
        }
        let set = &sets[0];
        let record_type = clean(scope.get("recordType"), 20);
        let record_types = set
            .get("recordTypes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let set_matches = clean(set.get("status"), 40) == "draft"
            && clean(set.get("sourceType"), 80) == "studentRecordMcp"
            && clean(set.get("sourceLabel"), 80) == "내 ChatGPT"
            && set.get("teacherReviewRequired").and_then(Value::as_bool) == Some(true)
            && record_types.len() == 1
            && clean(record_types.first(), 20) == record_type
            && clean(set.get("subject"), 160) == clean(scope.get("subject"), 160)
            && clean(set.get("creativeArea"), 40) == clean(scope.get("creativeArea"), 40)
            && date_key(set.get("fromDate")) == date_key(scope.get("fromDate"))
            && date_key(set.get("toDate")) == date_key(scope.get("toDate"));
        if !set_matches {
            return Err("DRAFT_CONFLICT".to_string());
        }
        let mut by_code = HashMap::new();
        for row in &drafts {
            let code = clean(row.get("studentCode"), 80);
            if code.is_empty() || by_code.insert(code, row).is_some() {
                return Err("DRAFT_CONFLICT".to_string());
            }
        }
        for identity in identities {
            let code = clean(identity.get("studentCode"), 80);
            let alias = clean(identity.get("alias"), 20);
            let row = by_code
                .get(&code)
                .copied()
                .ok_or_else(|| "DRAFT_CONFLICT".to_string())?;
            let matches = clean(row.get("draftId"), 260) == format!("{set_id}__{code}")
                && clean(row.get("recordType"), 20) == record_type
                && clean(row.get("status"), 40) == "draft"
                && clean(row.get("sourceType"), 80) == "studentRecordMcp"
                && clean(row.get("sourceLabel"), 80) == "내 ChatGPT"
                && row.get("teacherReviewRequired").and_then(Value::as_bool) == Some(true)
                && by_alias.get(&alias).map(String::as_str)
                    == Some(draft_text(row, scope).as_str());
            if !matches {
                return Err("DRAFT_CONFLICT".to_string());
            }
        }
        Ok(Some(
            set.get("createdAtMs")
                .and_then(Value::as_i64)
                .or_else(|| set.get("updatedAtMs").and_then(Value::as_i64))
                .unwrap_or_else(now_ms),
        ))
    }

    pub(crate) fn create_selection(
        &self,
        browser_tenant: &str,
        body: &Value,
    ) -> Result<Value, String> {
        let connection = self.active_connection(body.get("grantId").and_then(Value::as_str))?;
        if clean(connection.get("tenantId"), 128) != browser_tenant
            || clean(body.get("tenantId"), 128) != browser_tenant
        {
            return Err("MCP_SCOPE_DENIED".to_string());
        }
        let scope = scope_from(body)?;
        let source = body
            .get("students")
            .and_then(Value::as_array)
            .ok_or_else(|| "LOCAL_SELECTION_REQUIRED".to_string())?;
        if source.is_empty() || source.len() > MAX_STUDENTS {
            return Err("EVIDENCE_LIMIT_EXCEEDED".to_string());
        }
        let mut codes = HashSet::new();
        let mut students = Vec::new();
        for (row_index, row) in source.iter().enumerate() {
            let code = student_code(row.get("studentCode"))?;
            if !codes.insert(code.clone()) {
                return Err("LOCAL_SELECTION_REQUIRED".to_string());
            }
            let name = clean(row.get("studentName").or_else(|| row.get("name")), 160);
            let class_no = row
                .get("classNo")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .max(0);
            let refs = row
                .get("evidence")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if refs.len() > MAX_EVIDENCE {
                return Err("EVIDENCE_LIMIT_EXCEEDED".to_string());
            }
            let mut seen = HashSet::new();
            let mut normalized_refs = Vec::new();
            for reference in refs {
                let kind = clean(reference.get("kind"), 20);
                let id = clean(
                    reference.get("recordId").or_else(|| reference.get("id")),
                    260,
                );
                if !matches!(kind.as_str(), "observation" | "evaluation" | "attendance")
                    || !valid_identifier(&id, 260)
                    || !seen.insert(format!("{kind}:{id}"))
                {
                    return Err("LOCAL_SELECTION_REQUIRED".to_string());
                }
                normalized_refs.push(json!({"kind":kind,"recordId":id}));
            }
            students.push(json!({"studentCode":code,"studentName":name,"classNo":class_no,"evidence":normalized_refs,"originalIndex":row_index}));
        }
        students.sort_by(|a, b| {
            a.get("classNo")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .cmp(&b.get("classNo").and_then(Value::as_i64).unwrap_or(0))
                .then(clean(a.get("studentCode"), 80).cmp(&clean(b.get("studentCode"), 80)))
        });
        let roster_source = body
            .get("privacyRoster")
            .and_then(Value::as_array)
            .unwrap_or(source);
        if roster_source.is_empty() || roster_source.len() > MAX_PRIVACY_ROSTER {
            return Err("EVIDENCE_LIMIT_EXCEEDED".to_string());
        }
        let mut roster_codes = HashSet::new();
        let mut privacy_roster = Vec::new();
        for row in roster_source {
            let code = student_code(row.get("studentCode"))?;
            if !roster_codes.insert(code.clone()) {
                return Err("LOCAL_SELECTION_REQUIRED".to_string());
            }
            privacy_roster.push(json!({
                "studentCode": code,
                "studentName": clean(row.get("studentName").or_else(|| row.get("name")), 160),
            }));
        }
        for student in &students {
            let code = clean(student.get("studentCode"), 80);
            if roster_codes.insert(code.clone()) {
                privacy_roster.push(json!({
                    "studentCode": code,
                    "studentName": student.get("studentName"),
                }));
            }
        }
        if privacy_roster.len() > MAX_PRIVACY_ROSTER {
            return Err("EVIDENCE_LIMIT_EXCEEDED".to_string());
        }
        privacy_roster.sort_by(|left, right| {
            clean(left.get("studentCode"), 80).cmp(&clean(right.get("studentCode"), 80))
        });
        let mut payload = scope.as_object().cloned().unwrap_or_default();
        payload.insert("students".to_string(), Value::Array(students));
        payload.insert("privacyRoster".to_string(), Value::Array(privacy_roster));
        let payload_value = Value::Object(payload);
        let raw = serde_json::to_string(&payload_value)
            .map_err(|_| "LOCAL_SELECTION_REQUIRED".to_string())?;
        let payload_hash = digest(&raw);
        let grant = clean(connection.get("grantId"), 128);
        let scope_id = format!(
            "srmcp-scope:{}",
            &digest(&format!("{grant}:{payload_hash}"))[..32]
        );
        let now = now_ms();
        let conn = self
            .db
            .lock()
            .map_err(|_| "student_record_mcp_db_lock_failed".to_string())?;
        if let Some(handle)=conn.query_row("SELECT selection_handle FROM selections WHERE scope_id=?1 AND grant_id=?2 AND payload_digest=?3",params![scope_id,grant,payload_hash],|row|row.get::<_,String>(0)).optional().map_err(|e|format!("student_record_mcp_selection_query_failed:{e}"))?{conn.execute("UPDATE selections SET expires_at=?1 WHERE scope_id=?2",params![now+SELECTION_TTL_MS,scope_id]).map_err(|e|format!("student_record_mcp_selection_update_failed:{e}"))?;return Ok(json!({"selectionHandle":handle,"scopeId":scope_id,"expiresAt":now+SELECTION_TTL_MS,"studentCount":source.len()}));}
        let handle = identifier("srmcp-selection");
        conn.execute("INSERT INTO selections(selection_handle,scope_id,grant_id,tenant_id,payload_json,payload_digest,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![handle,scope_id,grant,browser_tenant,raw,payload_hash,now,now+SELECTION_TTL_MS]).map_err(|e|format!("student_record_mcp_selection_insert_failed:{e}"))?;
        drop(conn);
        self.audit(
            "selection_created",
            &grant,
            scope
                .get("recordType")
                .and_then(Value::as_str)
                .unwrap_or(""),
            source.len(),
            source
                .iter()
                .map(|row| {
                    row.get("evidence")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0)
                })
                .sum(),
        )?;
        Ok(
            json!({"selectionHandle":handle,"scopeId":scope_id,"expiresAt":now+SELECTION_TTL_MS,"studentCount":source.len()}),
        )
    }
}

pub async fn run_stdio(manager: Arc<StudentRecordMcpManager>) -> Result<(), String> {
    protocol::run_stdio(manager).await
}
