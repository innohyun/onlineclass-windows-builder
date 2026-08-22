use base64::{engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE_NO_PAD}, Engine as _};
use chrono::{DateTime, Utc};
mod backup;
mod cloud_sync;
mod data_explorer;
mod device_sync;
mod device_sync_credential;
mod desktop_preferences;
mod shared_archive;
mod student_private_photos;
mod work_note_attachments;
use rand::{distributions::Alphanumeric, Rng};
use rusqlite::{params, params_from_iter, Connection, ToSql};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager,
};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use url::Url;

const SERVICE_NAME: &str = "onlineclass-local-sensitive-store";
pub(crate) const SERVICE_VERSION: &str = "2026-08-22.3-teacher-desktop-shell";
const WORK_MEETING_ROOT_PAGE_ID: &str = "classaimate:work-meeting-minutes";
const WORK_MEETING_ROOT_TITLE: &str = "업무 회의록";
const WORK_MEETING_ROOT_INTRO: &str = "모바일에서 확정한 업무 회의록이 자동으로 들어옵니다.";
const DB_FILE_NAME: &str = "onlineclass-sensitive.sqlite";
const KEY_FILE_NAME: &str = "pairing-key.txt";
const BROWSER_LINK_FILE_NAME: &str = "browser-link-tokens.json";
const BROWSER_LINK_HEADER: &str = "X-OnlineClass-Local-Browser-Token";
const DEVICE_AUTHORIZATION_API_URL: &str = "https://t.classaimate.com/api/v3/local-store-device-authorizations";
const DEVICE_AUTHORIZATION_PAGE_URL: &str = "https://t.classaimate.com/connect-local";
const DEVICE_AUTHORIZATION_TTL_MS: i64 = 10 * 60 * 1000;
const BROWSER_LINK_PICKUP_TTL_MS: i64 = 60 * 1000;
const DESKTOP_BROWSER_LINK_PICKUP_TTL_MS: i64 = 10 * 60 * 1000;
const DESKTOP_BROWSER_LINK_AUDIENCE: &str = "teacher-home-webview";
const HOST: &str = "127.0.0.1";
const PORTS: [u16; 5] = [51273, 51274, 51275, 51276, 51277];
const MAX_BODY_BYTES: u64 = 14 * 1024 * 1024;
const LOCAL_SENSITIVE_STORE_ROUTES: &[&str] = &[
    "/v1/health",
    "/v1/browser-link/disconnect",
    "/v1/device-authorization/browser-link",
    "/v1/observations",
    "/v1/stats",
    "/v1/observations/import",
    "/v1/teacher-counseling-sessions",
    "/v1/student-private-details",
    "/v1/student-private-details/import",
    "/v1/student-private-photos",
    "/v1/math-daily/cache",
    "/v1/math-daily/cache-status",
    "/v1/math-daily/import",
    "/v1/math-daily/attempts",
    "/v1/math-daily/student-profiles",
    "/v1/math-daily/review-sessions",
    "/v1/math-daily/assignments",
    "/v1/math-daily/assignment-results",
    "/v1/board-post-snapshots",
    "/v1/board-media",
    "/v1/attendance-records",
    "/v1/attendance-records/import",
    "/v1/attendance-nais-checks",
    "/v1/attendance-nais-checks/import",
    "/v1/attendance-document-requests",
    "/v1/attendance-document-requests/import",
    "/v1/counseling-records",
    "/v1/counseling-teacher-notes",
    "/v1/counseling-import",
    "/v1/counseling-compare",
    "/v1/eval-assignments",
    "/v1/eval-assignments/import",
    "/v1/eval-results",
    "/v1/eval-results/import",
    "/v1/student-record-draft-sets",
    "/v1/student-record-draft-sets/import",
    "/v1/student-record-drafts",
    "/v1/student-record-drafts/import",
    "/v1/import-runs",
    "/v1/work-notes",
    "/v1/work-notes/move",
    "/v1/work-notes/reconcile-mobile-meeting-root",
    "/v1/work-note-attachments",
    "/v1/work-notes/import",
    "/v1/work-notes/export",
    "/v1/overview",
    "/v1/cloud-sync/status",
    "/v1/cloud-sync/runs",
    "/v1/cloud-sync/connect",
    "/v1/cloud-sync/run",
    "/v1/cloud-sync/disconnect",
    "/v1/device-sync/status",
    "/v1/device-sync/run",
    "/v1/backups/status",
    "/v1/backups/run",
    "/v1/backups/list",
    "/v1/backups/restore-preview",
    "/v1/backups/restore",
    "/v1/shared-archives/import",
    "/v1/shared-archives/import-jobs",
];
const LOCAL_SENSITIVE_STORE_FEATURES: [&str; 6] = [
    "non_lesson_observations",
    "teacher_local_records",
    "work_notes",
    "work_note_tree_move",
    "counseling_local_authority",
    "onedrive_device_sync",
];

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceStatus {
    ok: bool,
    service: String,
    version: String,
    pc_name: String,
    os: String,
    arch: String,
    host: String,
    port: u16,
    endpoint: String,
    data_dir: String,
    db_path: String,
    key_path: String,
    pairing_key: String,
    error: Option<String>,
}

impl ServiceStatus {
    fn failed(error: String) -> Self {
        Self {
            ok: false,
            service: SERVICE_NAME.to_string(),
            version: SERVICE_VERSION.to_string(),
            pc_name: local_pc_name(),
            os: local_os_name(),
            arch: local_arch(),
            host: HOST.to_string(),
            port: 0,
            endpoint: String::new(),
            data_dir: String::new(),
            db_path: String::new(),
            key_path: String::new(),
            pairing_key: String::new(),
            error: Some(error),
        }
    }
}

struct AppState {
    status: Mutex<ServiceStatus>,
    store: Mutex<Option<Arc<SqliteStore>>>,
    sync_manager: Mutex<Option<Arc<cloud_sync::CloudSyncManager>>>,
    device_sync_manager: Mutex<Option<Arc<device_sync::DeviceSyncManager>>>,
    browser_links: Mutex<Option<Arc<BrowserLinkStore>>>,
    pending_device_authorization: Mutex<Option<PendingDeviceAuthorization>>,
    preferences: desktop_preferences::DesktopPreferencesStore,
}

struct StorePaths {
    data_dir: PathBuf,
    db_path: PathBuf,
    key_path: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrowserLinkToken {
    tenant_id: String,
    uid: String,
    token: String,
    account_email: String,
    account_display_name: String,
    tenant_name: String,
    #[serde(default)]
    audience: String,
    created_at_ms: i64,
    last_used_at_ms: i64,
}

struct BrowserLinkStore {
    path: PathBuf,
    tokens: Mutex<Vec<BrowserLinkToken>>,
    pending_requests: Mutex<Vec<PendingBrowserLink>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingBrowserLink {
    request_id: String,
    link: BrowserLinkToken,
    expires_at_ms: i64,
    retire_siblings_on_use: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingDeviceAuthorization {
    request_id: String,
    verifier: String,
    authorization_url: String,
    created_at_ms: i64,
    expires_at_ms: i64,
}

pub(crate) struct SqliteStore {
    conn: Mutex<Connection>,
    db_path: PathBuf,
    data_dir: PathBuf,
}

struct NormalizedObservation {
    tenant_id: String,
    doc_id: String,
    date_key: String,
    period: i64,
    student_code: String,
    payload: Value,
    updated_at_ms: i64,
}

struct NormalizedTeacherCounselingSession {
    tenant_id: String,
    session_id: String,
    student_code: String,
    counseling_at_ms: i64,
    status: String,
    follow_up_on: String,
    archived_at_ms: i64,
    payload: Value,
    updated_at_ms: i64,
}

struct NormalizedStudentPrivateDetail {
    tenant_id: String,
    student_code: String,
    payload: Value,
    updated_at_ms: i64,
}

struct PreparedCounselingSnapshot {
    tenant_id: String,
    records: Vec<Value>,
    teacher_notes: Vec<Value>,
    source_snapshot_sha256: String,
}

fn normalize(value: impl ToString, max_len: usize) -> String {
    let mut text = value.to_string().trim().to_string();
    if max_len > 0 && text.chars().count() > max_len {
        text = text.chars().take(max_len).collect();
    }
    text
}

pub(crate) fn local_pc_name() -> String {
    env::var("COMPUTERNAME")
        .or_else(|_| env::var("HOSTNAME"))
        .map(|value| normalize(value, 120))
        .unwrap_or_default()
}

pub(crate) fn local_os_name() -> String {
    env::consts::OS.to_string()
}

pub(crate) fn local_arch() -> String {
    env::consts::ARCH.to_string()
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn normalize_json_text(value: Option<&Value>, max_len: usize) -> String {
    match value {
        Some(Value::String(text)) => normalize(text, max_len),
        Some(Value::Number(number)) => normalize(number, max_len),
        Some(Value::Bool(flag)) => normalize(flag, max_len),
        _ => String::new(),
    }
}

fn normalize_tenant_id(value: Option<&Value>) -> String {
    let tenant_id = normalize_json_text(value, 160);
    if tenant_id.is_empty() || tenant_id.contains('/') || tenant_id.contains('\\') {
        String::new()
    } else {
        tenant_id
    }
}

fn normalize_student_code(value: Option<&Value>) -> String {
    normalize_json_text(value, 80).to_uppercase()
}

fn normalize_id_segment(value: Option<&Value>, max_len: usize) -> String {
    normalize_json_text(value, max_len).replace(['/', '\\'], "_")
}

fn normalize_date_key(value: Option<&Value>) -> String {
    let date = normalize_json_text(value, 10);
    let bytes = date.as_bytes();
    let valid = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(idx, b)| idx == 4 || idx == 7 || b.is_ascii_digit());
    if valid {
        date
    } else {
        String::new()
    }
}

fn normalize_period(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(number)) => number.as_i64().unwrap_or(0).max(0),
        Some(Value::String(text)) => text.trim().parse::<i64>().unwrap_or(0).max(0),
        _ => 0,
    }
}

fn normalize_observation_context_type(value: Option<&Value>) -> String {
    let context_type = normalize_json_text(value, 40);
    if matches!(context_type.as_str(), "recess" | "counseling" | "daily_guidance" | "other") {
        context_type
    } else {
        String::new()
    }
}

fn default_observation_context_label(context_type: &str) -> String {
    match context_type {
        "recess" => "쉬는시간".to_string(),
        "counseling" => "상담".to_string(),
        "daily_guidance" => "생활지도".to_string(),
        "other" => "기타".to_string(),
        _ => String::new(),
    }
}

fn normalize_tags(value: Option<&Value>) -> Value {
    let mut tags = Vec::new();
    if let Some(Value::Array(items)) = value {
        for item in items.iter().take(20) {
            let tag = normalize_json_text(Some(item), 60);
            if !tag.is_empty() {
                tags.push(Value::String(tag));
            }
        }
    }
    Value::Array(tags)
}

fn normalize_topics(value: Option<&Value>) -> Value {
    let mut topics = Vec::new();
    if let Some(Value::Array(items)) = value {
        for item in items.iter().take(8) {
            let topic = normalize_json_text(Some(item), 60);
            if !topic.is_empty() {
                topics.push(Value::String(topic));
            }
        }
    }
    Value::Array(topics)
}

fn normalize_enum(value: Option<&Value>, allowed: &[&str], fallback: &str) -> String {
    let text = normalize_json_text(value, 60);
    if allowed.contains(&text.as_str()) { text } else { fallback.to_string() }
}

fn timestamp_like(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(number)) => number.as_i64().unwrap_or(0),
        Some(Value::String(text)) => DateTime::parse_from_rfc3339(text.trim())
            .map(|dt| dt.timestamp_millis())
            .unwrap_or(0),
        Some(Value::Object(obj)) => obj
            .get("seconds")
            .or_else(|| obj.get("_seconds"))
            .and_then(|v| v.as_i64())
            .map(|seconds| seconds * 1000)
            .unwrap_or(0),
        _ => 0,
    }
}

fn updated_at_ms(input: &Value) -> i64 {
    let value = timestamp_like(input.get("updatedAtMs"))
        .max(timestamp_like(input.get("updatedAt")))
        .max(timestamp_like(input.get("updatedAtIso")))
        .max(timestamp_like(input.get("serverSavedAtIso")))
        .max(timestamp_like(input.get("completedAt")))
        .max(timestamp_like(input.get("completedAtIso")))
        .max(timestamp_like(input.get("createdAt")))
        .max(timestamp_like(input.get("createdAtIso")));
    if value > 0 { value } else { now_ms() }
}

fn math_assignment_date_key(input: &Value) -> String {
    normalize_date_key(
        input
            .get("dateKey")
            .or_else(|| input.get("dueDateKey"))
            .or_else(|| input.get("mathDailyConfig").and_then(|config| config.get("dateKey"))),
    )
}

fn math_assignment_curriculum(input: &Value) -> String {
    normalize_json_text(
        input
            .get("curriculum")
            .or_else(|| input.get("mathDailyConfig").and_then(|config| config.get("curriculum"))),
        120,
    )
}

fn math_daily_cache_key(input: &Value) -> String {
    let action = {
        let value = normalize_json_text(input.get("action"), 80);
        if value.is_empty() { "cache".to_string() } else { value }
    };
    let date_key = normalize_date_key(input.get("dateKey").or_else(|| input.get("date")));
    let date_from = normalize_date_key(input.get("dateFrom").or_else(|| input.get("from")));
    let date_to = normalize_date_key(input.get("dateTo").or_else(|| input.get("to")));
    let range = if !date_key.is_empty() {
        date_key
    } else {
        format!("{date_from}..{date_to}")
    };
    let curriculum = {
        let value = normalize_json_text(input.get("curriculum"), 120);
        if value.is_empty() { "all".to_string() } else { value }
    };
    format!("{action}|{range}|{curriculum}")
}

fn set_obj(obj: &mut Map<String, Value>, key: &str, value: impl Into<Value>) {
    obj.insert(key.to_string(), value.into());
}

fn normalize_local_record_id(value: Option<&Value>, fallback: String, error_code: &str) -> Result<String, String> {
    let explicit = normalize_id_segment(value, 260);
    let record_id = if explicit.is_empty() {
        normalize(fallback, 260).replace(['/', '\\'], "_")
    } else {
        explicit
    };
    if record_id.is_empty() {
        Err(error_code.to_string())
    } else {
        Ok(record_id)
    }
}

fn payload_json(value: &Value, code: &str) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| format!("{code}:{e}"))
}

fn set_updated_payload_fields(obj: &mut Map<String, Value>, updated_at_ms: i64) {
    obj.insert("updatedAtMs".to_string(), Value::Number(updated_at_ms.into()));
    let updated_at_iso = DateTime::<Utc>::from_timestamp_millis(updated_at_ms)
        .unwrap_or_else(Utc::now)
        .to_rfc3339();
    obj.insert("updatedAtIso".to_string(), Value::String(updated_at_iso));
}

const COUNSELING_STATUSES: [&str; 4] = ["unread", "read", "replied", "resolved"];

fn normalize_counseling_record(input: Value) -> Result<Value, String> {
    let mut obj = input
        .as_object()
        .cloned()
        .ok_or_else(|| "invalid_record".to_string())?;
    let tenant_id = normalize_tenant_id(obj.get("tenantId"));
    let request_id = normalize_local_record_id(
        obj.get("requestId").or_else(|| obj.get("id")).or_else(|| obj.get("docId")),
        String::new(),
        "counseling_request_id_required",
    )?;
    let student_code = normalize_student_code(obj.get("studentCode").or_else(|| obj.get("code")));
    let status = {
        let value = normalize_json_text(obj.get("status"), 40).to_lowercase();
        if value.is_empty() { "unread".to_string() } else { value }
    };
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    if student_code.is_empty() {
        return Err("student_code_required".to_string());
    }
    if !COUNSELING_STATUSES.contains(&status.as_str()) {
        return Err("counseling_status_invalid".to_string());
    }
    let content = normalize_json_text(obj.get("content").or_else(|| obj.get("message")), 20_000);
    if content.is_empty() {
        return Err("counseling_content_required".to_string());
    }
    let updated_at_ms = timestamp_like(obj.get("updatedAtMs"))
        .max(timestamp_like(obj.get("updatedAt")))
        .max(timestamp_like(obj.get("timestamp")));
    let updated_at_ms = if updated_at_ms > 0 { updated_at_ms } else { now_ms() };
    let created_at_ms = timestamp_like(obj.get("createdAtMs"))
        .max(timestamp_like(obj.get("createdAt")))
        .max(timestamp_like(obj.get("timestamp")));
    let created_at_ms = if created_at_ms > 0 { created_at_ms } else { updated_at_ms };
    let replies = obj
        .get("replies")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(200)
        .map(|reply| {
            let mut reply_obj = reply
                .as_object()
                .cloned()
                .ok_or_else(|| "invalid_record".to_string())?;
            let reply_content = normalize_json_text(
                reply_obj.get("content").or_else(|| reply_obj.get("message")),
                20_000,
            );
            if reply_content.is_empty() {
                return Err("counseling_reply_content_required".to_string());
            }
            let author_role = {
                let value = normalize_json_text(
                    reply_obj.get("authorRole").or_else(|| reply_obj.get("role")),
                    40,
                );
                if value.is_empty() { "teacher".to_string() } else { value }
            };
            let reply_at_ms = timestamp_like(reply_obj.get("createdAtMs"))
                .max(timestamp_like(reply_obj.get("createdAt")))
                .max(timestamp_like(reply_obj.get("timestamp")));
            set_obj(&mut reply_obj, "content", reply_content);
            set_obj(&mut reply_obj, "authorRole", author_role);
            set_obj(
                &mut reply_obj,
                "createdAtMs",
                if reply_at_ms > 0 { reply_at_ms } else { updated_at_ms },
            );
            Ok(Value::Object(reply_obj))
        })
        .collect::<Result<Vec<_>, String>>()?;

    set_obj(&mut obj, "id", request_id.clone());
    set_obj(&mut obj, "docId", request_id.clone());
    set_obj(&mut obj, "requestId", request_id);
    set_obj(&mut obj, "tenantId", tenant_id);
    set_obj(&mut obj, "studentCode", student_code);
    let student_name = normalize_json_text(
        obj.get("studentName")
            .or_else(|| obj.get("displayName"))
            .or_else(|| obj.get("name")),
        160,
    );
    set_obj(&mut obj, "studentName", student_name);
    let counseling_type = {
        let value = normalize_json_text(obj.get("type").or_else(|| obj.get("category")), 80);
        if value.is_empty() { "general".to_string() } else { value }
    };
    set_obj(&mut obj, "type", counseling_type);
    set_obj(&mut obj, "content", content);
    set_obj(&mut obj, "status", status);
    set_obj(&mut obj, "replies", Value::Array(replies));
    set_obj(&mut obj, "createdAtMs", created_at_ms);
    set_obj(&mut obj, "updatedAtMs", updated_at_ms);
    Ok(Value::Object(obj))
}

fn normalize_counseling_teacher_note(input: Value) -> Result<Value, String> {
    let mut obj = input
        .as_object()
        .cloned()
        .ok_or_else(|| "invalid_record".to_string())?;
    let tenant_id = normalize_tenant_id(obj.get("tenantId"));
    let request_id = normalize_local_record_id(
        obj.get("requestId")
            .or_else(|| obj.get("counselingId"))
            .or_else(|| obj.get("id"))
            .or_else(|| obj.get("docId")),
        String::new(),
        "counseling_request_id_required",
    )?;
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let updated_at_ms = timestamp_like(obj.get("updatedAtMs"))
        .max(timestamp_like(obj.get("updatedAt")))
        .max(timestamp_like(obj.get("timestamp")));
    let updated_at_ms = if updated_at_ms > 0 { updated_at_ms } else { now_ms() };
    let tags = obj
        .get("teacherNoteTags")
        .or_else(|| obj.get("tags"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| {
            let tag = normalize_json_text(Some(&value), 60);
            if tag.is_empty() { None } else { Some(Value::String(tag)) }
        })
        .take(20)
        .collect::<Vec<_>>();
    let teacher_note = normalize_json_text(
        obj.get("teacherNote").or_else(|| obj.get("memo")),
        30_000,
    );
    set_obj(&mut obj, "id", request_id.clone());
    set_obj(&mut obj, "docId", request_id.clone());
    set_obj(&mut obj, "requestId", request_id.clone());
    set_obj(&mut obj, "counselingId", request_id);
    set_obj(&mut obj, "tenantId", tenant_id);
    set_obj(&mut obj, "teacherNote", teacher_note);
    set_obj(&mut obj, "teacherNoteTags", Value::Array(tags));
    set_obj(&mut obj, "updatedAtMs", updated_at_ms);
    Ok(Value::Object(obj))
}

fn canonicalize_json(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonicalize_json).collect()),
        Value::Object(obj) => {
            let mut keys = obj.keys().collect::<Vec<_>>();
            keys.sort();
            let mut out = Map::new();
            for key in keys {
                out.insert(key.clone(), canonicalize_json(&obj[key]));
            }
            Value::Object(out)
        }
        _ => value.clone(),
    }
}

fn sha256_json(value: &Value) -> Result<String, String> {
    let encoded = serde_json::to_vec(&canonicalize_json(value))
        .map_err(|error| format!("counseling_hash_encode_failed:{error}"))?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

fn prepare_counseling_snapshot(input: &Value) -> Result<PreparedCounselingSnapshot, String> {
    let tenant_id = normalize_tenant_id(input.get("tenantId"));
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let mut records = input
        .get("records")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut record| {
            if let Value::Object(ref mut obj) = record {
                set_obj(obj, "tenantId", tenant_id.clone());
            }
            normalize_counseling_record(record)
        })
        .collect::<Result<Vec<_>, String>>()?;
    records.sort_by(|left, right| {
        normalize_json_text(left.get("requestId"), 260)
            .cmp(&normalize_json_text(right.get("requestId"), 260))
    });
    let mut teacher_notes = input
        .get("teacherNotes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut note| {
            if let Value::Object(ref mut obj) = note {
                set_obj(obj, "tenantId", tenant_id.clone());
            }
            normalize_counseling_teacher_note(note)
        })
        .collect::<Result<Vec<_>, String>>()?;
    teacher_notes.sort_by(|left, right| {
        normalize_json_text(left.get("requestId"), 260)
            .cmp(&normalize_json_text(right.get("requestId"), 260))
    });
    let mut record_ids = std::collections::HashSet::new();
    for record in &records {
        let request_id = normalize_json_text(record.get("requestId"), 260);
        if !record_ids.insert(request_id) {
            return Err("counseling_duplicate_request_id".to_string());
        }
    }
    let mut note_ids = std::collections::HashSet::new();
    for note in &teacher_notes {
        let request_id = normalize_json_text(note.get("requestId"), 260);
        if !note_ids.insert(request_id.clone()) {
            return Err("counseling_duplicate_teacher_note_id".to_string());
        }
        if !record_ids.contains(&request_id) {
            return Err("counseling_record_not_found".to_string());
        }
    }
    let source_snapshot_sha256 = sha256_json(&json!({
        "records": records,
        "teacherNotes": teacher_notes
    }))?;
    Ok(PreparedCounselingSnapshot {
        tenant_id,
        records,
        teacher_notes,
        source_snapshot_sha256,
    })
}

fn write_counseling_record(conn: &Connection, record: &Value) -> Result<(), String> {
    let tenant_id = normalize_tenant_id(record.get("tenantId"));
    let request_id = normalize_id_segment(record.get("requestId"), 260);
    let student_code = normalize_student_code(record.get("studentCode"));
    let status = normalize_json_text(record.get("status"), 40).to_lowercase();
    let created_at_ms = timestamp_like(record.get("createdAtMs"));
    let updated_at_ms = timestamp_like(record.get("updatedAtMs"));
    let payload_json = payload_json(record, "counseling_record_encode_failed")?;
    conn.execute(
        "INSERT INTO counseling_records
         (tenant_id, request_id, student_code, status, created_at_ms, updated_at_ms, payload_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(tenant_id, request_id) DO UPDATE SET
           student_code = excluded.student_code,
           status = excluded.status,
           created_at_ms = excluded.created_at_ms,
           updated_at_ms = excluded.updated_at_ms,
           payload_json = excluded.payload_json
         WHERE excluded.updated_at_ms >= counseling_records.updated_at_ms",
        params![
            tenant_id,
            request_id,
            student_code,
            status,
            created_at_ms,
            updated_at_ms,
            payload_json
        ],
    )
    .map_err(|error| format!("db_counseling_record_upsert_failed:{error}"))?;
    Ok(())
}

fn write_counseling_teacher_note(conn: &Connection, note: &Value) -> Result<(), String> {
    let tenant_id = normalize_tenant_id(note.get("tenantId"));
    let request_id = normalize_id_segment(note.get("requestId"), 260);
    let record_exists = conn
        .query_row(
            "SELECT 1 FROM counseling_records WHERE tenant_id = ?1 AND request_id = ?2",
            params![&tenant_id, &request_id],
            |_| Ok(()),
        )
        .is_ok();
    if !record_exists {
        return Err("counseling_record_not_found".to_string());
    }
    let updated_at_ms = timestamp_like(note.get("updatedAtMs"));
    let payload_json = payload_json(note, "counseling_teacher_note_encode_failed")?;
    conn.execute(
        "INSERT INTO counseling_teacher_notes
         (tenant_id, request_id, payload_json, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(tenant_id, request_id) DO UPDATE SET
           payload_json = excluded.payload_json,
           updated_at_ms = excluded.updated_at_ms
         WHERE excluded.updated_at_ms >= counseling_teacher_notes.updated_at_ms",
        params![tenant_id, request_id, payload_json, updated_at_ms],
    )
    .map_err(|error| format!("db_counseling_teacher_note_upsert_failed:{error}"))?;
    Ok(())
}

fn query_counseling_records(
    conn: &Connection,
    tenant_id: &str,
    status: &str,
    student_code: &str,
    request_id: &str,
    limit: i64,
) -> Result<Vec<Value>, String> {
    let mut where_parts = vec!["tenant_id = ?".to_string()];
    let mut params_vec: Vec<Box<dyn ToSql>> = vec![Box::new(tenant_id.to_string())];
    if !status.is_empty() {
        where_parts.push("status = ?".to_string());
        params_vec.push(Box::new(status.to_string()));
    }
    if !student_code.is_empty() {
        where_parts.push("student_code = ?".to_string());
        params_vec.push(Box::new(student_code.to_string()));
    }
    if !request_id.is_empty() {
        where_parts.push("request_id = ?".to_string());
        params_vec.push(Box::new(request_id.to_string()));
    }
    let sql = format!(
        "SELECT payload_json FROM counseling_records WHERE {} ORDER BY updated_at_ms DESC, request_id ASC LIMIT {}",
        where_parts.join(" AND "),
        limit.clamp(1, 10_000)
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|error| format!("db_counseling_records_prepare_failed:{error}"))?;
    let params_ref = params_vec
        .iter()
        .map(|value| value.as_ref() as &dyn ToSql)
        .collect::<Vec<_>>();
    let rows = stmt
        .query_map(params_from_iter(params_ref), |row| {
            let payload: String = row.get(0)?;
            Ok(serde_json::from_str(&payload).unwrap_or_else(|_| json!({})))
        })
        .map_err(|error| format!("db_counseling_records_query_failed:{error}"))?;
    rows.map(|row| row.map_err(|error| format!("db_counseling_records_row_failed:{error}")))
        .collect()
}

fn query_counseling_teacher_notes(
    conn: &Connection,
    tenant_id: &str,
    request_id: &str,
    limit: i64,
) -> Result<Vec<Value>, String> {
    let mut where_parts = vec!["tenant_id = ?".to_string()];
    let mut params_vec: Vec<Box<dyn ToSql>> = vec![Box::new(tenant_id.to_string())];
    if !request_id.is_empty() {
        where_parts.push("request_id = ?".to_string());
        params_vec.push(Box::new(request_id.to_string()));
    }
    let sql = format!(
        "SELECT payload_json FROM counseling_teacher_notes WHERE {} ORDER BY updated_at_ms DESC, request_id ASC LIMIT {}",
        where_parts.join(" AND "),
        limit.clamp(1, 10_000)
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|error| format!("db_counseling_teacher_notes_prepare_failed:{error}"))?;
    let params_ref = params_vec
        .iter()
        .map(|value| value.as_ref() as &dyn ToSql)
        .collect::<Vec<_>>();
    let rows = stmt
        .query_map(params_from_iter(params_ref), |row| {
            let payload: String = row.get(0)?;
            Ok(serde_json::from_str(&payload).unwrap_or_else(|_| json!({})))
        })
        .map_err(|error| format!("db_counseling_teacher_notes_query_failed:{error}"))?;
    rows.map(|row| row.map_err(|error| format!("db_counseling_teacher_notes_row_failed:{error}")))
        .collect()
}

fn compare_prepared_counseling_snapshot(
    conn: &Connection,
    source: &PreparedCounselingSnapshot,
) -> Result<Value, String> {
    let mut local_records = query_counseling_records(
        conn,
        &source.tenant_id,
        "",
        "",
        "",
        10_000,
    )?;
    local_records.sort_by(|left, right| {
        normalize_json_text(left.get("requestId"), 260)
            .cmp(&normalize_json_text(right.get("requestId"), 260))
    });
    let mut local_teacher_notes =
        query_counseling_teacher_notes(conn, &source.tenant_id, "", 10_000)?;
    local_teacher_notes.sort_by(|left, right| {
        normalize_json_text(left.get("requestId"), 260)
            .cmp(&normalize_json_text(right.get("requestId"), 260))
    });
    let source_record_ids = source
        .records
        .iter()
        .map(|value| normalize_json_text(value.get("requestId"), 260))
        .collect::<Vec<_>>();
    let local_record_ids = local_records
        .iter()
        .map(|value| normalize_json_text(value.get("requestId"), 260))
        .collect::<Vec<_>>();
    let source_note_ids = source
        .teacher_notes
        .iter()
        .map(|value| normalize_json_text(value.get("requestId"), 260))
        .collect::<Vec<_>>();
    let local_note_ids = local_teacher_notes
        .iter()
        .map(|value| normalize_json_text(value.get("requestId"), 260))
        .collect::<Vec<_>>();
    let source_records_value = Value::Array(source.records.clone());
    let local_records_value = Value::Array(local_records.clone());
    let source_notes_value = Value::Array(source.teacher_notes.clone());
    let local_notes_value = Value::Array(local_teacher_notes.clone());
    let records_match = canonicalize_json(&source_records_value) == canonicalize_json(&local_records_value)
        && source_record_ids == local_record_ids;
    let notes_match = canonicalize_json(&source_notes_value) == canonicalize_json(&local_notes_value)
        && source_note_ids == local_note_ids;
    Ok(json!({
        "ok": true,
        "tenantId": source.tenant_id,
        "sourceSnapshotSha256": source.source_snapshot_sha256,
        "counts": {
            "sourceRecords": source.records.len(),
            "localRecords": local_records.len(),
            "sourceTeacherNotes": source.teacher_notes.len(),
            "localTeacherNotes": local_teacher_notes.len()
        },
        "records": {
            "sourceCount": source.records.len(),
            "localCount": local_records.len(),
            "sourceSha256": sha256_json(&source_records_value)?,
            "localSha256": sha256_json(&local_records_value)?,
            "matches": records_match
        },
        "teacherNotes": {
            "sourceCount": source.teacher_notes.len(),
            "localCount": local_teacher_notes.len(),
            "sourceSha256": sha256_json(&source_notes_value)?,
            "localSha256": sha256_json(&local_notes_value)?,
            "matches": notes_match
        },
        "matches": records_match && notes_match
    }))
}

fn normalize_observation(input: Value) -> Result<NormalizedObservation, String> {
    let mut obj = input
        .as_object()
        .cloned()
        .ok_or_else(|| "invalid_record".to_string())?;

    let tenant_id = normalize_tenant_id(obj.get("tenantId"));
    let date_key = normalize_date_key(obj.get("date").or_else(|| obj.get("dateKey")));
    let period = normalize_period(obj.get("period"));
    let student_code = normalize_student_code(obj.get("studentCode").or_else(|| obj.get("code")));
    let observation_kind = normalize_json_text(obj.get("observationKind"), 40);
    let context_type = normalize_observation_context_type(obj.get("contextType"));
    let context_label = {
        let value = normalize_json_text(obj.get("contextLabel"), 80);
        if value.is_empty() {
            default_observation_context_label(&context_type)
        } else {
            value
        }
    };
    let is_non_lesson = observation_kind == "non_lesson" || !context_type.is_empty();

    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    if date_key.is_empty() {
        return Err("date_required".to_string());
    }
    if period <= 0 && !is_non_lesson {
        return Err("period_required".to_string());
    }
    if student_code.is_empty() {
        return Err("student_code_required".to_string());
    }

    let safe_period = if is_non_lesson { 0 } else { period };
    let updated_at_ms = timestamp_like(
        obj.get("updatedAtMs")
            .or_else(|| obj.get("updatedAt"))
            .or_else(|| obj.get("updatedAtIso")),
    );
    let updated_at_ms = if updated_at_ms > 0 {
        updated_at_ms
    } else {
        Utc::now().timestamp_millis()
    };
    let fallback_doc_id = if is_non_lesson {
        let safe_context = if context_type.is_empty() { "other" } else { context_type.as_str() };
        format!("{date_key}_nonlesson_{safe_context}_{student_code}_{updated_at_ms}")
    } else {
        format!("{date_key}_{safe_period}_{student_code}")
    };
    let mut doc_id = normalize_json_text(obj.get("id").or_else(|| obj.get("docId")), 240);
    if doc_id.is_empty() {
        doc_id = fallback_doc_id;
    }
    doc_id = doc_id.replace(['/', '\\'], "_");
    if doc_id.is_empty() {
        return Err("doc_id_required".to_string());
    }

    let event_at_ms = timestamp_like(
        obj.get("eventAtMs")
            .or_else(|| obj.get("eventAt"))
            .or_else(|| obj.get("eventAtIso")),
    );
    let updated_at_iso = DateTime::<Utc>::from_timestamp_millis(updated_at_ms)
        .unwrap_or_else(Utc::now)
        .to_rfc3339();

    let student_name = normalize_json_text(obj.get("studentName").or_else(|| obj.get("name")), 160);
    let class_no = normalize_period(obj.get("classNo").or_else(|| obj.get("number")));
    let subject = {
        let value = normalize_json_text(obj.get("subject"), 160);
        if value.is_empty() { "-".to_string() } else { value }
    };
    let objective = {
        let value = normalize_json_text(obj.get("objective").or_else(|| obj.get("title")), 400);
        if value.is_empty() { "-".to_string() } else { value }
    };
    let status = {
        let value = normalize_json_text(obj.get("status"), 40);
        if value.is_empty() { "none".to_string() } else { value }
    };
    let note = normalize_json_text(obj.get("note"), 1000);
    let tags = normalize_tags(obj.get("tags"));
    let timestamps = obj
        .get("timestamps")
        .and_then(|v| v.as_object())
        .cloned()
        .map(Value::Object)
        .unwrap_or_else(|| json!({}));

    set_obj(&mut obj, "id", doc_id.clone());
    set_obj(&mut obj, "docId", doc_id.clone());
    set_obj(&mut obj, "tenantId", tenant_id.clone());
    set_obj(&mut obj, "date", date_key.clone());
    set_obj(&mut obj, "period", safe_period);
    if is_non_lesson {
        let safe_context_type = if context_type.is_empty() { "other".to_string() } else { context_type.clone() };
        let safe_context_label = if context_label.is_empty() { "기타".to_string() } else { context_label.clone() };
        let period_label = {
            let value = normalize_json_text(obj.get("periodLabel"), 80);
            if value.is_empty() { safe_context_label.clone() } else { value }
        };
        let event_time_label = normalize_json_text(obj.get("eventTimeLabel"), 20);
        set_obj(&mut obj, "observationKind", "non_lesson");
        set_obj(&mut obj, "contextType", safe_context_type);
        set_obj(&mut obj, "contextLabel", safe_context_label);
        set_obj(&mut obj, "periodLabel", period_label);
        set_obj(&mut obj, "eventAtMs", if event_at_ms > 0 { event_at_ms } else { updated_at_ms });
        set_obj(&mut obj, "eventTimeLabel", event_time_label);
    }
    set_obj(&mut obj, "studentCode", student_code.clone());
    set_obj(&mut obj, "studentName", student_name);
    set_obj(&mut obj, "classNo", class_no);
    set_obj(&mut obj, "subject", subject);
    set_obj(&mut obj, "objective", objective);
    set_obj(&mut obj, "status", status);
    set_obj(&mut obj, "tags", tags);
    set_obj(&mut obj, "note", note);
    set_obj(&mut obj, "timestamps", timestamps);
    set_obj(&mut obj, "updatedAtMs", updated_at_ms);
    set_obj(&mut obj, "updatedAtIso", updated_at_iso);

    Ok(NormalizedObservation {
        tenant_id,
        doc_id,
        date_key,
        period: safe_period,
        student_code,
        payload: Value::Object(obj),
        updated_at_ms,
    })
}

fn normalize_teacher_counseling_session(input: Value) -> Result<NormalizedTeacherCounselingSession, String> {
    let mut obj = input.as_object().cloned().ok_or_else(|| "invalid_record".to_string())?;
    let tenant_id = normalize_tenant_id(obj.get("tenantId"));
    let student_code = normalize_student_code(obj.get("studentCode").or_else(|| obj.get("code")).or_else(|| obj.get("studentId")));
    let counseling_at_ms = {
        let value = timestamp_like(obj.get("counselingAtMs").or_else(|| obj.get("counselingAt")).or_else(|| obj.get("counselingDate")));
        if value > 0 { value } else { now_ms() }
    };
    let updated_at_ms = updated_at_ms(&Value::Object(obj.clone()));
    let fallback_id = format!("counseling_{student_code}_{counseling_at_ms}");
    let session_id = normalize_local_record_id(
        obj.get("sessionId").or_else(|| obj.get("id")).or_else(|| obj.get("docId")),
        fallback_id,
        "teacher_counseling_session_id_required",
    )?;
    let summary = normalize_json_text(obj.get("summary"), 5000);
    if tenant_id.is_empty() { return Err("tenant_id_required".to_string()); }
    if student_code.is_empty() { return Err("student_code_required".to_string()); }
    if summary.is_empty() { return Err("teacher_counseling_summary_required".to_string()); }
    let participant_type = normalize_enum(obj.get("participantType"), &["student", "guardian", "student_guardian"], "student");
    let channel = normalize_enum(obj.get("channel"), &["in_person", "phone", "online"], "in_person");
    let status = normalize_enum(obj.get("status"), &["completed", "follow_up"], "completed");
    let follow_up_on = if status == "follow_up" { normalize_date_key(obj.get("followUpOn")) } else { String::new() };
    let archived_at_ms = timestamp_like(obj.get("archivedAtMs"));
    let student_name = normalize_json_text(obj.get("studentName").or_else(|| obj.get("name")), 160);
    let class_no = normalize_period(obj.get("classNo").or_else(|| obj.get("number")));
    let topics = normalize_topics(obj.get("topics"));
    let follow_up_note = normalize_json_text(obj.get("followUpNote"), 2000);
    let source_transcript = obj.get("sourceTranscript").and_then(Value::as_object).and_then(|source| {
        let text = normalize_json_text(source.get("text"), 120000);
        if text.is_empty() { return None; }
        Some(json!({
            "version": 1,
            "text": text,
            "model": normalize_json_text(source.get("model"), 120),
            "transcribedAtMs": timestamp_like(source.get("transcribedAtMs")),
            "audioStored": false
        }))
    });
    let created_at_ms = {
        let value = timestamp_like(obj.get("createdAtMs").or_else(|| obj.get("createdAt")));
        if value > 0 { value } else { updated_at_ms }
    };
    set_obj(&mut obj, "id", session_id.clone());
    set_obj(&mut obj, "docId", session_id.clone());
    set_obj(&mut obj, "sessionId", session_id.clone());
    set_obj(&mut obj, "tenantId", tenant_id.clone());
    set_obj(&mut obj, "studentCode", student_code.clone());
    set_obj(&mut obj, "studentName", student_name);
    set_obj(&mut obj, "classNo", class_no);
    set_obj(&mut obj, "participantType", participant_type);
    set_obj(&mut obj, "channel", channel);
    set_obj(&mut obj, "counselingAtMs", counseling_at_ms);
    set_obj(&mut obj, "counselingAtIso", DateTime::<Utc>::from_timestamp_millis(counseling_at_ms).unwrap_or_else(Utc::now).to_rfc3339());
    set_obj(&mut obj, "status", status.clone());
    set_obj(&mut obj, "followUpOn", follow_up_on.clone());
    set_obj(&mut obj, "topics", topics);
    set_obj(&mut obj, "summary", summary);
    set_obj(&mut obj, "followUpNote", follow_up_note);
    if let Some(value) = source_transcript { set_obj(&mut obj, "sourceTranscript", value); }
    set_obj(&mut obj, "archivedAtMs", archived_at_ms);
    set_obj(&mut obj, "archivedAtIso", if archived_at_ms > 0 { DateTime::<Utc>::from_timestamp_millis(archived_at_ms).unwrap_or_else(Utc::now).to_rfc3339() } else { String::new() });
    set_obj(&mut obj, "recordOrigin", "teacher_local_counseling");
    set_obj(&mut obj, "createdAtMs", created_at_ms);
    set_updated_payload_fields(&mut obj, updated_at_ms);
    Ok(NormalizedTeacherCounselingSession {
        tenant_id, session_id, student_code, counseling_at_ms, status, follow_up_on,
        archived_at_ms, payload: Value::Object(obj), updated_at_ms,
    })
}

fn normalize_string_list(value: Option<&Value>) -> Value {
    let mut out = Vec::new();
    if let Some(Value::Array(items)) = value {
        for item in items.iter().take(40) {
            let text = normalize_json_text(Some(item), 120);
            if !text.is_empty() {
                out.push(Value::String(text));
            }
        }
    }
    Value::Array(out)
}

fn normalize_guardian(value: Option<&Value>) -> Value {
    let mut out = Map::new();
    let source = value.and_then(|v| v.as_object());
    let name = source
        .and_then(|obj| obj.get("name"))
        .map(|v| normalize_json_text(Some(v), 120))
        .unwrap_or_default();
    let phone = source
        .and_then(|obj| obj.get("phone"))
        .map(|v| normalize_json_text(Some(v), 80))
        .unwrap_or_default();
    out.insert("name".to_string(), if name.is_empty() { Value::Null } else { Value::String(name) });
    out.insert("phone".to_string(), if phone.is_empty() { Value::Null } else { Value::String(phone) });
    Value::Object(out)
}

fn normalize_student_private_detail(input: Value) -> Result<NormalizedStudentPrivateDetail, String> {
    let mut obj = input
        .as_object()
        .cloned()
        .ok_or_else(|| "invalid_record".to_string())?;
    let tenant_id = normalize_tenant_id(obj.get("tenantId"));
    let student_code = normalize_student_code(
        obj.get("studentCode")
            .or_else(|| obj.get("code"))
            .or_else(|| obj.get("id"))
            .or_else(|| obj.get("docId")),
    );
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    if student_code.is_empty() {
        return Err("student_code_required".to_string());
    }
    let updated_at_ms = timestamp_like(
        obj.get("updatedAtMs")
            .or_else(|| obj.get("updatedAt"))
            .or_else(|| obj.get("updatedAtIso")),
    );
    let updated_at_ms = if updated_at_ms > 0 {
        updated_at_ms
    } else {
        Utc::now().timestamp_millis()
    };
    let updated_at_iso = DateTime::<Utc>::from_timestamp_millis(updated_at_ms)
        .unwrap_or_else(Utc::now)
        .to_rfc3339();

    let student_name = normalize_json_text(obj.get("studentName").or_else(|| obj.get("name")), 160);
    let siblings_note = normalize_json_text(obj.get("siblingsNote"), 1000);
    let special_note = normalize_json_text(obj.get("specialNote"), 1000);
    let guardian1 = normalize_guardian(obj.get("guardian1"));
    let guardian2 = normalize_guardian(obj.get("guardian2"));
    let health_source = obj.get("health").and_then(|value| value.as_object());
    let mut health = Map::new();
    health.insert(
        "conditions".to_string(),
        normalize_string_list(health_source.and_then(|h| h.get("conditions"))),
    );
    health.insert(
        "allergies".to_string(),
        normalize_string_list(health_source.and_then(|h| h.get("allergies"))),
    );
    health.insert(
        "cautionFoods".to_string(),
        normalize_string_list(health_source.and_then(|h| h.get("cautionFoods"))),
    );
    let emergency_note = health_source
        .and_then(|h| h.get("emergencyNote"))
        .map(|v| normalize_json_text(Some(v), 1000))
        .unwrap_or_default();
    health.insert(
        "emergencyNote".to_string(),
        if emergency_note.is_empty() { Value::Null } else { Value::String(emergency_note) },
    );

    set_obj(&mut obj, "id", student_code.clone());
    set_obj(&mut obj, "docId", student_code.clone());
    set_obj(&mut obj, "tenantId", tenant_id.clone());
    set_obj(&mut obj, "studentCode", student_code.clone());
    set_obj(&mut obj, "studentName", student_name);
    obj.insert("guardian1".to_string(), guardian1);
    obj.insert("guardian2".to_string(), guardian2);
    obj.insert(
        "siblingsNote".to_string(),
        if siblings_note.is_empty() { Value::Null } else { Value::String(siblings_note) },
    );
    obj.insert(
        "specialNote".to_string(),
        if special_note.is_empty() { Value::Null } else { Value::String(special_note) },
    );
    obj.insert("health".to_string(), Value::Object(health));
    set_obj(&mut obj, "updatedAtMs", updated_at_ms);
    set_obj(&mut obj, "updatedAtIso", updated_at_iso);

    Ok(NormalizedStudentPrivateDetail {
        tenant_id,
        student_code,
        payload: Value::Object(obj),
        updated_at_ms,
    })
}

fn default_data_dir() -> PathBuf {
    if let Ok(explicit) = env::var("ONLINECLASS_LOCAL_STORE_DIR") {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    if cfg!(target_os = "windows") {
        if let Ok(appdata) = env::var("APPDATA") {
            if !appdata.trim().is_empty() {
                return PathBuf::from(appdata)
                    .join("OnlineClass")
                    .join("local-sensitive-store");
            }
        }
    }

    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".onlineclass")
        .join("local-sensitive-store")
}

fn resolve_paths() -> StorePaths {
    let data_dir = default_data_dir();
    StorePaths {
        db_path: data_dir.join(DB_FILE_NAME),
        key_path: data_dir.join(KEY_FILE_NAME),
        data_dir,
    }
}

fn ensure_pairing_key(path: &Path) -> Result<String, String> {
    if let Ok(env_key) = env::var("ONLINECLASS_LOCAL_STORE_KEY") {
        let key = normalize(env_key, 200);
        if !key.is_empty() {
            return Ok(key);
        }
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("key_dir_create_failed:{e}"))?;
    }
    if path.exists() {
        let existing = normalize(
            fs::read_to_string(path).map_err(|e| format!("key_read_failed:{e}"))?,
            200,
        );
        if !existing.is_empty() {
            return Ok(existing);
        }
    }

    let generated: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();
    fs::write(path, format!("{generated}\n")).map_err(|e| format!("key_write_failed:{e}"))?;
    Ok(generated)
}

impl BrowserLinkStore {
    fn open(data_dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(data_dir).map_err(|e| format!("browser_link_dir_create_failed:{e}"))?;
        let path = data_dir.join(BROWSER_LINK_FILE_NAME);
        Ok(Self {
            tokens: Mutex::new(Self::read_tokens(&path)),
            pending_requests: Mutex::new(Vec::new()),
            path,
        })
    }

    fn read_tokens(path: &Path) -> Vec<BrowserLinkToken> {
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(_) => return Vec::new(),
        };
        if let Ok(tokens) = serde_json::from_str::<Vec<BrowserLinkToken>>(&raw) {
            return tokens;
        }
        serde_json::from_str::<Value>(&raw)
            .ok()
            .and_then(|value| value.get("tokens").cloned())
            .and_then(|tokens| serde_json::from_value::<Vec<BrowserLinkToken>>(tokens).ok())
            .unwrap_or_default()
    }

    fn write_tokens(&self, tokens: &[BrowserLinkToken]) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("browser_link_dir_create_failed:{e}"))?;
        }
        let payload = json!({
            "version": 1,
            "updatedAtMs": now_ms(),
            "tokens": tokens
        });
        let raw = serde_json::to_string_pretty(&payload)
            .map_err(|e| format!("browser_link_encode_failed:{e}"))?;
        fs::write(&self.path, raw).map_err(|e| format!("browser_link_write_failed:{e}"))
    }

    fn issue(&self, input: &Value) -> Result<BrowserLinkToken, String> {
        self.issue_for_audience(input, "")
    }

    fn issue_for_audience(&self, input: &Value, audience: &str) -> Result<BrowserLinkToken, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        let uid = normalize_json_text(input.get("uid"), 200);
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        if uid.is_empty() {
            return Err("cloud_sync_session_required".to_string());
        }
        let token: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(48)
            .map(char::from)
            .collect();
        let now = now_ms();
        let record = BrowserLinkToken {
            tenant_id,
            uid,
            token,
            account_email: normalize_json_text(input.get("accountEmail").or_else(|| input.get("email")), 320),
            account_display_name: normalize_json_text(input.get("accountDisplayName"), 120),
            tenant_name: normalize_json_text(input.get("tenantName"), 180),
            audience: audience.to_string(),
            created_at_ms: now,
            last_used_at_ms: now,
        };
        let snapshot = {
            let mut tokens = self.tokens.lock().map_err(|_| "browser_link_lock_failed".to_string())?;
            if !audience.is_empty() {
                tokens.retain(|entry| {
                    entry.audience != audience
                        || entry.tenant_id != record.tenant_id
                        || entry.uid != record.uid
                });
            }
            tokens.push(record.clone());
            if tokens.len() > 20 {
                let keep_from = tokens.len().saturating_sub(20);
                tokens.drain(0..keep_from);
            }
            tokens.clone()
        };
        self.write_tokens(&snapshot)?;
        Ok(record)
    }

    fn issue_for_request(&self, request_id: &str, input: &Value) -> Result<BrowserLinkToken, String> {
        let link = self.issue(input)?;
        self.stage_for_request(
            request_id,
            link,
            BROWSER_LINK_PICKUP_TTL_MS,
            true,
        )
    }

    fn issue_desktop_for_request(&self, request_id: &str) -> Result<BrowserLinkToken, String> {
        let current = self
            .latest()
            .ok_or_else(|| "browser_link_missing".to_string())?;
        let link = self.issue_for_audience(&json!({
            "tenantId": current.tenant_id,
            "uid": current.uid,
            "accountEmail": current.account_email,
            "accountDisplayName": current.account_display_name,
            "tenantName": current.tenant_name,
        }), DESKTOP_BROWSER_LINK_AUDIENCE)?;
        self.stage_for_request(
            request_id,
            link,
            DESKTOP_BROWSER_LINK_PICKUP_TTL_MS,
            false,
        )
    }

    fn stage_for_request(
        &self,
        request_id: &str,
        link: BrowserLinkToken,
        ttl_ms: i64,
        retire_siblings_on_use: bool,
    ) -> Result<BrowserLinkToken, String> {
        let safe_request_id = normalize(request_id, 80);
        if safe_request_id.len() != 43
            || !safe_request_id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        {
            return Err("device_authorization_request_invalid".to_string());
        }
        let mut pending = self.pending_requests.lock().map_err(|_| "browser_link_lock_failed".to_string())?;
        pending.retain(|entry| entry.request_id != safe_request_id);
        pending.push(PendingBrowserLink {
            request_id: safe_request_id,
            link: link.clone(),
            expires_at_ms: now_ms() + ttl_ms.max(1),
            retire_siblings_on_use,
        });
        Ok(link)
    }

    fn read_for_request(&self, request_id: &str) -> Result<Option<BrowserLinkToken>, String> {
        let safe_request_id = normalize(request_id, 80);
        let mut pending = self.pending_requests.lock().map_err(|_| "browser_link_lock_failed".to_string())?;
        let now = now_ms();
        pending.retain(|entry| entry.expires_at_ms > now);
        Ok(pending
            .iter()
            .find(|entry| entry.request_id == safe_request_id)
            .map(|entry| entry.link.clone()))
    }

    fn latest(&self) -> Option<BrowserLinkToken> {
        self.tokens.lock().ok().and_then(|tokens| tokens.iter().max_by_key(|entry| entry.created_at_ms).cloned())
    }

    fn authorize_token(&self, raw_token: &str) -> Option<String> {
        let token = normalize(raw_token, 200);
        if token.is_empty() {
            return None;
        }
        let staged = self.pending_requests.lock().ok().and_then(|pending| {
            pending
                .iter()
                .find(|entry| entry.link.token == token)
                .map(|entry| entry.retire_siblings_on_use)
        });
        let mut changed = false;
        let mut snapshot = None;
        let tenant_id = {
            let mut tokens = match self.tokens.lock() {
                Ok(tokens) => tokens,
                Err(_) => return None,
            };
            let now = now_ms();
            let found = tokens.iter_mut().find(|entry| entry.token == token);
            if let Some(entry) = found {
                entry.last_used_at_ms = now;
                changed = true;
                let tenant_id = entry.tenant_id.clone();
                let uid = entry.uid.clone();
                if staged == Some(true) {
                    tokens.retain(|entry| entry.token == token
                        || entry.tenant_id != tenant_id || entry.uid != uid);
                }
                snapshot = Some(tokens.clone());
                Some(tenant_id)
            } else {
                None
            }
        };
        if changed {
            if let Some(tokens) = snapshot { let _ = self.write_tokens(&tokens); }
        }
        if staged.is_some() && tenant_id.is_some() {
            if let Ok(mut pending) = self.pending_requests.lock() {
                pending.retain(|entry| entry.link.token != token);
            }
        }
        tenant_id
    }

    fn authorize_tenant(&self, request: &Request) -> Option<String> {
        self.authorize_token(&get_header(request, BROWSER_LINK_HEADER))
    }

    fn revoke_tenant(&self, tenant_id: &str) -> Result<(), String> {
        let safe_tenant = normalize(tenant_id, 160);
        if safe_tenant.is_empty() {
            return Ok(());
        }
        let snapshot = {
            let mut tokens = self.tokens.lock().map_err(|_| "browser_link_lock_failed".to_string())?;
            let before = tokens.len();
            tokens.retain(|entry| entry.tenant_id != safe_tenant);
            if before == tokens.len() {
                return Ok(());
            }
            tokens.clone()
        };
        self.write_tokens(&snapshot)
    }

    fn revoke_request_token(&self, request: &Request) -> Result<bool, String> {
        let token = normalize(get_header(request, BROWSER_LINK_HEADER), 200);
        if token.is_empty() {
            return Ok(false);
        }
        let snapshot = {
            let mut tokens = self.tokens.lock().map_err(|_| "browser_link_lock_failed".to_string())?;
            let before = tokens.len();
            tokens.retain(|entry| entry.token != token);
            if before == tokens.len() {
                return Ok(false);
            }
            tokens.clone()
        };
        self.write_tokens(&snapshot)?;
        Ok(true)
    }

    fn revoke_all(&self) -> Result<(), String> {
        {
            let mut tokens = self.tokens.lock().map_err(|_| "browser_link_lock_failed".to_string())?;
            tokens.clear();
            self.write_tokens(&tokens)?;
        }
        if let Ok(mut pending) = self.pending_requests.lock() {
            pending.clear();
        }
        Ok(())
    }
}

fn is_bug_blanked_mobile_meeting_root(page: &Value) -> bool {
    let blocks = page.get("blocks").and_then(Value::as_array).cloned().unwrap_or_default();
    let meaningful = blocks.iter().any(|block| {
        !block.get("text").and_then(Value::as_str).unwrap_or("").trim().is_empty()
            || matches!(block.get("type").and_then(Value::as_str), Some("attachment" | "page"))
    });
    let markdown = page.get("markdown").and_then(Value::as_str).unwrap_or("").trim();
    !meaningful && blocks.len() <= 1 && (markdown.is_empty() || markdown == format!("# {WORK_MEETING_ROOT_TITLE}"))
}

fn is_safe_mobile_meeting_root(page: &Value, include_canonical: bool) -> bool {
    if !include_canonical && page.get("pageId").and_then(Value::as_str) == Some(WORK_MEETING_ROOT_PAGE_ID) { return false; }
    if page.get("title").and_then(Value::as_str) != Some(WORK_MEETING_ROOT_TITLE)
        || page.pointer("/properties/systemKind").and_then(Value::as_str) != Some("mobile_work_meeting_folder") { return false; }
    let blocks = page.get("blocks").and_then(Value::as_array);
    let untouched = blocks.and_then(|value| value.first()).and_then(|block| block.get("id")).and_then(Value::as_str) == Some("work-meeting-root-intro")
        && blocks.and_then(|value| value.first()).and_then(|block| block.get("text")).and_then(Value::as_str) == Some(WORK_MEETING_ROOT_INTRO)
        && blocks.and_then(|value| value.get(1)).and_then(|block| block.get("id")).and_then(Value::as_str) == Some("work-meeting-root-end")
        && page.get("markdown").and_then(Value::as_str) == Some(&format!("# {WORK_MEETING_ROOT_TITLE}\n\n{WORK_MEETING_ROOT_INTRO}"));
    untouched || is_bug_blanked_mobile_meeting_root(page)
}

impl SqliteStore {
    fn open(db_path: PathBuf) -> Result<Self, String> {
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("db_dir_create_failed:{e}"))?;
        }
        let conn = Connection::open(&db_path).map_err(|e| format!("db_open_failed:{e}"))?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS lesson_observations (
              tenant_id TEXT NOT NULL,
              doc_id TEXT NOT NULL,
              date_key TEXT NOT NULL,
              period INTEGER NOT NULL,
              student_code TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, doc_id)
            );
            CREATE INDEX IF NOT EXISTS idx_lesson_observations_tenant_date
              ON lesson_observations (tenant_id, date_key, period);
            CREATE INDEX IF NOT EXISTS idx_lesson_observations_tenant_student_date
              ON lesson_observations (tenant_id, student_code, date_key);
            CREATE TABLE IF NOT EXISTS teacher_counseling_sessions (
              tenant_id TEXT NOT NULL,
              session_id TEXT NOT NULL,
              student_code TEXT NOT NULL,
              counseling_at_ms INTEGER NOT NULL,
              status TEXT NOT NULL CHECK (status IN ('completed', 'follow_up')),
              follow_up_on TEXT,
              archived_at_ms INTEGER,
              payload_json TEXT NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, session_id)
            );
            CREATE INDEX IF NOT EXISTS idx_teacher_counseling_tenant_student_date
              ON teacher_counseling_sessions (tenant_id, student_code, counseling_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_teacher_counseling_tenant_status_follow_up
              ON teacher_counseling_sessions (tenant_id, status, follow_up_on);
            CREATE TABLE IF NOT EXISTS lesson_observation_conflicts (
              tenant_id TEXT NOT NULL,
              doc_id TEXT NOT NULL,
              source_update_time TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              captured_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, doc_id, source_update_time)
            );
            CREATE TABLE IF NOT EXISTS student_private_details (
              tenant_id TEXT NOT NULL,
              student_code TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, student_code)
            );
            CREATE INDEX IF NOT EXISTS idx_student_private_details_tenant_updated
              ON student_private_details (tenant_id, updated_at_ms);
            CREATE TABLE IF NOT EXISTS student_private_photos (
              tenant_id TEXT NOT NULL,
              student_code TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, student_code)
            );
            CREATE INDEX IF NOT EXISTS idx_student_private_photos_tenant_updated
              ON student_private_photos (tenant_id, updated_at_ms);
            CREATE TABLE IF NOT EXISTS student_private_detail_conflicts (
              tenant_id TEXT NOT NULL,
              student_code TEXT NOT NULL,
              source_update_time TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              captured_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, student_code, source_update_time)
            );
            CREATE TABLE IF NOT EXISTS math_daily_attempts (
              tenant_id TEXT NOT NULL,
              attempt_id TEXT NOT NULL,
              date_key TEXT NOT NULL,
              student_code TEXT NOT NULL,
              curriculum TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, attempt_id)
            );
            CREATE INDEX IF NOT EXISTS idx_math_daily_attempts_tenant_date
              ON math_daily_attempts (tenant_id, date_key, student_code);
            CREATE INDEX IF NOT EXISTS idx_math_daily_attempts_tenant_student_date
              ON math_daily_attempts (tenant_id, student_code, date_key);
            CREATE TABLE IF NOT EXISTS math_daily_student_profiles (
              tenant_id TEXT NOT NULL,
              student_code TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, student_code)
            );
            CREATE INDEX IF NOT EXISTS idx_math_daily_profiles_tenant_updated
              ON math_daily_student_profiles (tenant_id, updated_at_ms);
            CREATE TABLE IF NOT EXISTS math_daily_review_sessions (
              tenant_id TEXT NOT NULL,
              review_session_id TEXT NOT NULL,
              date_key TEXT NOT NULL,
              student_code TEXT NOT NULL,
              curriculum TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, review_session_id)
            );
            CREATE INDEX IF NOT EXISTS idx_math_daily_reviews_tenant_date
              ON math_daily_review_sessions (tenant_id, date_key, student_code);
            CREATE TABLE IF NOT EXISTS math_daily_assignments (
              tenant_id TEXT NOT NULL,
              assignment_id TEXT NOT NULL,
              date_key TEXT NOT NULL,
              curriculum TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, assignment_id)
            );
            CREATE INDEX IF NOT EXISTS idx_math_daily_assignments_tenant_date
              ON math_daily_assignments (tenant_id, date_key);
            CREATE TABLE IF NOT EXISTS math_daily_assignment_results (
              tenant_id TEXT NOT NULL,
              submission_id TEXT NOT NULL,
              assignment_id TEXT NOT NULL,
              student_code TEXT NOT NULL,
              date_key TEXT NOT NULL,
              curriculum TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, submission_id)
            );
            CREATE INDEX IF NOT EXISTS idx_math_daily_results_tenant_assignment
              ON math_daily_assignment_results (tenant_id, assignment_id, student_code);
            CREATE INDEX IF NOT EXISTS idx_math_daily_results_tenant_date
              ON math_daily_assignment_results (tenant_id, date_key, student_code);
            CREATE TABLE IF NOT EXISTS math_daily_cache_runs (
              tenant_id TEXT NOT NULL,
              cache_key TEXT NOT NULL,
              action TEXT NOT NULL,
              date_from TEXT,
              date_to TEXT,
              date_key TEXT,
              curriculum TEXT,
              payload_json TEXT NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, cache_key)
            );
            CREATE INDEX IF NOT EXISTS idx_math_daily_cache_runs_tenant_updated
              ON math_daily_cache_runs (tenant_id, updated_at_ms DESC);
            CREATE TABLE IF NOT EXISTS board_post_snapshots (
              tenant_id TEXT NOT NULL,
              board_id TEXT NOT NULL,
              post_id TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              archived_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, board_id, post_id)
            );
            CREATE INDEX IF NOT EXISTS idx_board_post_snapshots_tenant_board
              ON board_post_snapshots (tenant_id, board_id, updated_at_ms);
            CREATE TABLE IF NOT EXISTS board_media_files (
              tenant_id TEXT NOT NULL,
              board_id TEXT NOT NULL,
              post_id TEXT NOT NULL,
              media_id TEXT NOT NULL,
              storage_path TEXT,
              local_path TEXT NOT NULL,
              content_type TEXT NOT NULL,
              file_name TEXT,
              size INTEGER NOT NULL,
              expires_at_ms INTEGER,
              archived_at_ms INTEGER NOT NULL,
              payload_json TEXT NOT NULL,
              PRIMARY KEY (tenant_id, media_id)
            );
            CREATE INDEX IF NOT EXISTS idx_board_media_files_tenant_board
              ON board_media_files (tenant_id, board_id, post_id);
            CREATE INDEX IF NOT EXISTS idx_board_media_files_storage_path
              ON board_media_files (tenant_id, storage_path);
            CREATE TABLE IF NOT EXISTS attendance_records (
              tenant_id TEXT NOT NULL,
              record_id TEXT NOT NULL,
              date_key TEXT NOT NULL,
              student_code TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, record_id)
            );
            CREATE INDEX IF NOT EXISTS idx_attendance_records_tenant_date
              ON attendance_records (tenant_id, date_key, student_code);
            CREATE TABLE IF NOT EXISTS attendance_nais_checks (
              tenant_id TEXT NOT NULL,
              check_id TEXT NOT NULL,
              date_key TEXT NOT NULL,
              student_code TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, check_id)
            );
            CREATE INDEX IF NOT EXISTS idx_attendance_nais_checks_tenant_date
              ON attendance_nais_checks (tenant_id, date_key, student_code);
            CREATE TABLE IF NOT EXISTS attendance_document_requests (
              tenant_id TEXT NOT NULL,
              request_id TEXT NOT NULL,
              date_key TEXT NOT NULL,
              student_code TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, request_id)
            );
            CREATE INDEX IF NOT EXISTS idx_attendance_document_requests_tenant_date
              ON attendance_document_requests (tenant_id, date_key, student_code);
            CREATE TABLE IF NOT EXISTS eval_assignments (
              tenant_id TEXT NOT NULL,
              assignment_id TEXT NOT NULL,
              shared_plan_id TEXT NOT NULL,
              scheduled_date TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, assignment_id)
            );
            CREATE INDEX IF NOT EXISTS idx_eval_assignments_tenant_plan
              ON eval_assignments (tenant_id, shared_plan_id);
            CREATE INDEX IF NOT EXISTS idx_eval_assignments_tenant_date
              ON eval_assignments (tenant_id, scheduled_date);
            CREATE TABLE IF NOT EXISTS eval_results (
              tenant_id TEXT NOT NULL,
              result_id TEXT NOT NULL,
              assignment_id TEXT NOT NULL,
              student_id TEXT NOT NULL,
              date_key TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, result_id)
            );
            CREATE INDEX IF NOT EXISTS idx_eval_results_tenant_assignment
              ON eval_results (tenant_id, assignment_id, student_id);
            CREATE INDEX IF NOT EXISTS idx_eval_results_tenant_student_date
              ON eval_results (tenant_id, student_id, date_key);
            CREATE TABLE IF NOT EXISTS student_record_draft_sets (
              tenant_id TEXT NOT NULL,
              draft_set_id TEXT NOT NULL,
              status TEXT NOT NULL,
              from_date TEXT NOT NULL,
              to_date TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              created_at_ms INTEGER NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, draft_set_id)
            );
            CREATE INDEX IF NOT EXISTS idx_student_record_draft_sets_tenant_updated
              ON student_record_draft_sets (tenant_id, updated_at_ms DESC);
            CREATE TABLE IF NOT EXISTS student_record_drafts (
              tenant_id TEXT NOT NULL,
              draft_id TEXT NOT NULL,
              draft_set_id TEXT NOT NULL,
              student_code TEXT NOT NULL,
              class_no INTEGER NOT NULL,
              payload_json TEXT NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, draft_id)
            );
            CREATE INDEX IF NOT EXISTS idx_student_record_drafts_tenant_set
              ON student_record_drafts (tenant_id, draft_set_id, class_no, student_code);
            CREATE INDEX IF NOT EXISTS idx_student_record_drafts_tenant_student
              ON student_record_drafts (tenant_id, student_code, updated_at_ms DESC);
            CREATE TABLE IF NOT EXISTS counseling_records (
              tenant_id TEXT NOT NULL,
              request_id TEXT NOT NULL,
              student_code TEXT NOT NULL,
              status TEXT NOT NULL,
              created_at_ms INTEGER NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              payload_json TEXT NOT NULL,
              PRIMARY KEY (tenant_id, request_id)
            );
            CREATE INDEX IF NOT EXISTS idx_counseling_records_tenant_status_updated
              ON counseling_records (tenant_id, status, updated_at_ms DESC, request_id);
            CREATE INDEX IF NOT EXISTS idx_counseling_records_tenant_student_updated
              ON counseling_records (tenant_id, student_code, updated_at_ms DESC, request_id);
            CREATE TABLE IF NOT EXISTS counseling_teacher_notes (
              tenant_id TEXT NOT NULL,
              request_id TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, request_id),
              FOREIGN KEY (tenant_id, request_id) REFERENCES counseling_records (tenant_id, request_id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS local_import_runs (
              tenant_id TEXT NOT NULL,
              run_id TEXT NOT NULL,
              kind TEXT NOT NULL,
              status TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              started_at_ms INTEGER NOT NULL,
              finished_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, run_id)
            );
            CREATE INDEX IF NOT EXISTS idx_local_import_runs_tenant_kind
              ON local_import_runs (tenant_id, kind, finished_at_ms DESC);
            CREATE TABLE IF NOT EXISTS cloud_sync_runs (
              tenant_id TEXT NOT NULL,
              run_id TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              started_at_ms INTEGER NOT NULL,
              finished_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, run_id)
            );
            CREATE INDEX IF NOT EXISTS idx_cloud_sync_runs_tenant_finished
              ON cloud_sync_runs (tenant_id, finished_at_ms DESC);
            CREATE TABLE IF NOT EXISTS work_note_pages (
              tenant_id TEXT NOT NULL,
              page_id TEXT NOT NULL,
              parent_id TEXT,
              title TEXT NOT NULL,
              emoji TEXT NOT NULL,
              position INTEGER NOT NULL DEFAULT 0,
              properties_json TEXT NOT NULL,
              document_json TEXT NOT NULL,
              markdown TEXT NOT NULL,
              created_at_ms INTEGER NOT NULL,
              updated_at_ms INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, page_id)
            );
            CREATE INDEX IF NOT EXISTS idx_work_note_pages_tree
              ON work_note_pages (tenant_id, parent_id, position, updated_at_ms DESC);
            CREATE VIRTUAL TABLE IF NOT EXISTS work_note_pages_fts USING fts5(
              tenant_id UNINDEXED, page_id UNINDEXED, title, markdown, tokenize='unicode61'
            );
            "#,
        )
        .map_err(|e| format!("db_schema_failed:{e}"))?;
        work_note_attachments::ensure_schema(&conn)?;
        backup::install_sync_tracking(&conn)?;
        let data_dir = db_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        Ok(Self {
            conn: Mutex::new(conn),
            db_path,
            data_dir,
        })
    }

    fn health(&self) -> Value {
        json!({
            "ok": true,
            "service": SERVICE_NAME,
            "version": SERVICE_VERSION,
            "pcName": local_pc_name(),
            "os": local_os_name(),
            "arch": local_arch(),
            "dbPath": self.db_path.to_string_lossy(),
            "routes": LOCAL_SENSITIVE_STORE_ROUTES,
            "features": LOCAL_SENSITIVE_STORE_FEATURES
        })
    }

    fn upsert_work_note(&self, mut input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        let page_id = normalize_id_segment(input.get("pageId").or_else(|| input.get("id")), 180);
        let parent_id = normalize_id_segment(input.get("parentId"), 180);
        let title = {
            let value = normalize_json_text(input.get("title"), 240);
            if value.is_empty() { "제목 없음".to_string() } else { value }
        };
        let emoji = {
            let value = normalize_json_text(input.get("emoji"), 16);
            if value.is_empty() { "📄".to_string() } else { value }
        };
        let position = input.get("position").and_then(Value::as_i64).unwrap_or(0).max(0);
        let properties = input.get("properties").cloned().unwrap_or_else(|| json!({}));
        let blocks = input.get("blocks").cloned().unwrap_or_else(|| json!([]));
        let markdown = input.get("markdown").and_then(Value::as_str).unwrap_or("").chars().take(2_000_000).collect::<String>();
        let now = Utc::now().timestamp_millis();
        let updated_at_ms = input.get("updatedAtMs").and_then(Value::as_i64).filter(|value| *value > 0).unwrap_or(now);
        let created_at_ms = input.get("createdAtMs").and_then(Value::as_i64).filter(|value| *value > 0).unwrap_or(updated_at_ms);
        if tenant_id.is_empty() { return Err("tenant_id_required".to_string()); }
        if page_id.is_empty() { return Err("work_note_page_id_required".to_string()); }
        if parent_id == page_id { return Err("work_note_parent_cycle".to_string()); }
        if let Some(object) = input.as_object_mut() {
            object.insert("tenantId".to_string(), Value::String(tenant_id.clone()));
            object.insert("pageId".to_string(), Value::String(page_id.clone()));
            object.insert("parentId".to_string(), if parent_id.is_empty() { Value::Null } else { Value::String(parent_id.clone()) });
            object.insert("title".to_string(), Value::String(title.clone()));
            object.insert("emoji".to_string(), Value::String(emoji.clone()));
            object.insert("position".to_string(), Value::Number(position.into()));
            object.insert("properties".to_string(), properties.clone());
            object.insert("blocks".to_string(), blocks.clone());
            object.insert("markdown".to_string(), Value::String(markdown.clone()));
            object.insert("updatedAtMs".to_string(), Value::Number(updated_at_ms.into()));
        }
        let properties_json = serde_json::to_string(&properties).map_err(|e| format!("work_note_properties_encode_failed:{e}"))?;
        let document_json = serde_json::to_string(&blocks).map_err(|e| format!("work_note_document_encode_failed:{e}"))?;
        let mut conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let transaction = conn.transaction().map_err(|e| format!("db_work_note_transaction_failed:{e}"))?;
        transaction.execute(
            r#"INSERT INTO work_note_pages (
              tenant_id, page_id, parent_id, title, emoji, position, properties_json,
              document_json, markdown, created_at_ms, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
              COALESCE((SELECT created_at_ms FROM work_note_pages WHERE tenant_id = ?1 AND page_id = ?2), ?10), ?11)
            ON CONFLICT(tenant_id, page_id) DO UPDATE SET
              parent_id=excluded.parent_id, title=excluded.title, emoji=excluded.emoji,
              position=excluded.position, properties_json=excluded.properties_json,
              document_json=excluded.document_json, markdown=excluded.markdown,
              updated_at_ms=excluded.updated_at_ms"#,
            params![tenant_id, page_id, if parent_id.is_empty(){None::<String>}else{Some(parent_id)}, title, emoji,
                position, properties_json, document_json, markdown, created_at_ms, updated_at_ms],
        ).map_err(|e| format!("db_work_note_upsert_failed:{e}"))?;
        transaction.execute("DELETE FROM work_note_pages_fts WHERE tenant_id = ?1 AND page_id = ?2", params![tenant_id, page_id])
            .map_err(|e| format!("db_work_note_fts_delete_failed:{e}"))?;
        transaction.execute("INSERT INTO work_note_pages_fts (tenant_id,page_id,title,markdown) VALUES (?1,?2,?3,?4)", params![tenant_id,page_id,title,markdown])
            .map_err(|e| format!("db_work_note_fts_insert_failed:{e}"))?;
        transaction.commit().map_err(|e| format!("db_work_note_commit_failed:{e}"))?;
        Ok(input)
    }

    fn list_work_notes(&self, tenant_id: String, query: String) -> Result<Vec<Value>, String> {
        let tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if tenant.is_empty() { return Err("tenant_id_required".to_string()); }
        let search = normalize(&query, 200);
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let (sql, values): (&str, Vec<String>) = if search.is_empty() {
            ("SELECT p.tenant_id,p.page_id,p.parent_id,p.title,p.emoji,p.position,p.properties_json,p.document_json,p.markdown,p.created_at_ms,p.updated_at_ms FROM work_note_pages p WHERE p.tenant_id=?1 ORDER BY COALESCE(p.parent_id,''),p.position,p.page_id", vec![tenant])
        } else {
            let terms = search.split_whitespace().map(|term| format!("\"{}\"*", term.replace('"', "\"\""))).collect::<Vec<_>>().join(" AND ");
            ("SELECT p.tenant_id,p.page_id,p.parent_id,p.title,p.emoji,p.position,p.properties_json,p.document_json,p.markdown,p.created_at_ms,p.updated_at_ms FROM work_note_pages_fts f JOIN work_note_pages p ON p.tenant_id=f.tenant_id AND p.page_id=f.page_id WHERE f.tenant_id=?1 AND work_note_pages_fts MATCH ?2 ORDER BY rank,p.updated_at_ms DESC LIMIT 100", vec![tenant, terms])
        };
        let mut statement = conn.prepare(sql).map_err(|e| format!("db_work_note_query_prepare_failed:{e}"))?;
        let rows = statement.query_map(params_from_iter(values.iter()), |row| {
            let properties_raw: String = row.get(6)?;
            let blocks_raw: String = row.get(7)?;
            Ok(json!({
                "tenantId": row.get::<_, String>(0)?, "pageId": row.get::<_, String>(1)?,
                "parentId": row.get::<_, Option<String>>(2)?, "title": row.get::<_, String>(3)?,
                "emoji": row.get::<_, String>(4)?, "position": row.get::<_, i64>(5)?,
                "properties": serde_json::from_str::<Value>(&properties_raw).unwrap_or_else(|_| json!({})),
                "blocks": serde_json::from_str::<Value>(&blocks_raw).unwrap_or_else(|_| json!([])),
                "markdown": row.get::<_, String>(8)?, "createdAtMs": row.get::<_, i64>(9)?,
                "updatedAtMs": row.get::<_, i64>(10)?
            }))
        }).map_err(|e| format!("db_work_note_query_failed:{e}"))?;
        let mut records = Vec::new();
        for row in rows { records.push(row.map_err(|e| format!("db_work_note_row_failed:{e}"))?); }
        Ok(records)
    }

    fn get_work_note(&self, tenant_id: String, page_id: String) -> Result<Option<Value>, String> {
        Ok(self.list_work_notes(tenant_id, String::new())?.into_iter().find(|page| page.get("pageId").and_then(Value::as_str) == Some(page_id.as_str())))
    }

    fn delete_work_note(&self, tenant_id: String, page_id: String) -> Result<Value, String> {
        let tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        let page = normalize_id_segment(Some(&Value::String(page_id)), 180);
        if tenant.is_empty() { return Err("tenant_id_required".to_string()); }
        if page.is_empty() { return Err("work_note_page_id_required".to_string()); }
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let child: i64 = conn.query_row("SELECT COUNT(*) FROM work_note_pages WHERE tenant_id=?1 AND parent_id=?2", params![tenant,page], |row| row.get(0)).map_err(|e| format!("db_work_note_child_check_failed:{e}"))?;
        if child > 0 { return Err("work_note_has_children".to_string()); }
        drop(conn);
        let attachment_paths = work_note_attachments::page_local_paths(self, &tenant, &page)?;
        let mut conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let transaction = conn.transaction().map_err(|e| format!("db_work_note_transaction_failed:{e}"))?;
        transaction.execute("DELETE FROM work_note_pages_fts WHERE tenant_id=?1 AND page_id=?2",params![tenant,page]).map_err(|e|format!("db_work_note_fts_delete_failed:{e}"))?;
        let deleted=transaction.execute("DELETE FROM work_note_pages WHERE tenant_id=?1 AND page_id=?2",params![tenant,page]).map_err(|e|format!("db_work_note_delete_failed:{e}"))?;
        transaction.commit().map_err(|e|format!("db_work_note_commit_failed:{e}"))?;
        work_note_attachments::delete_local_paths(self, &attachment_paths);
        Ok(json!({"ok":true,"deleted":deleted}))
    }

    fn move_work_note(&self, input: Value) -> Result<Value, String> {
        #[derive(Clone)] struct Page { id: String, parent: Option<String>, position: i64 }
        let tenant = normalize_tenant_id(input.get("tenantId"));
        let source_id = normalize_id_segment(input.get("pageId"), 180);
        let target_id = normalize_id_segment(input.get("targetPageId"), 180);
        let placement = normalize_json_text(input.get("placement"), 16);
        if tenant.is_empty() { return Err("tenant_id_required".to_string()); }
        if source_id.is_empty() || target_id.is_empty() { return Err("work_note_page_id_required".to_string()); }
        if !["before", "inside", "after"].contains(&placement.as_str()) { return Err("work_note_move_placement_invalid".to_string()); }
        if source_id == target_id || source_id == "root" { return Err("work_note_root_move_forbidden".to_string()); }
        if placement != "inside" && target_id == "root" { return Err("work_note_root_sibling_forbidden".to_string()); }
        let records = self.list_work_notes(tenant.clone(), String::new())?;
        let pages = records.iter().map(|value| Page {
            id: value.get("pageId").and_then(Value::as_str).unwrap_or("").to_string(),
            parent: value.get("parentId").and_then(Value::as_str).map(str::to_string),
            position: value.get("position").and_then(Value::as_i64).unwrap_or(0),
        }).collect::<Vec<_>>();
        let source = pages.iter().find(|page| page.id == source_id).cloned().ok_or_else(|| "work_note_not_found".to_string())?;
        let target = pages.iter().find(|page| page.id == target_id).cloned().ok_or_else(|| "work_note_not_found".to_string())?;
        let destination_parent = if placement == "inside" { Some(target_id.clone()) } else { target.parent.clone() };
        let mut cursor = destination_parent.clone();
        while let Some(parent_id) = cursor { if parent_id == source_id { return Err("work_note_parent_cycle".to_string()); } cursor = pages.iter().find(|page| page.id == parent_id).and_then(|page| page.parent.clone()); }
        let mut parent_ids = vec![source.parent.clone()];if source.parent != destination_parent { parent_ids.push(destination_parent.clone()); }
        let mut groups = parent_ids.into_iter().map(|parent| { let mut group=pages.iter().filter(|page| page.id!=source_id&&page.parent==parent).cloned().collect::<Vec<_>>();group.sort_by(|a,b|a.position.cmp(&b.position).then(a.id.cmp(&b.id)));(parent,group) }).collect::<Vec<_>>();
        let destination = groups.iter_mut().find(|(parent,_)| *parent == destination_parent).ok_or_else(|| "work_note_move_target_changed".to_string())?;
        let index = if placement == "inside" { destination.1.len() } else { destination.1.iter().position(|page|page.id==target_id).ok_or_else(|| "work_note_move_target_changed".to_string())? + if placement == "after" {1}else{0} };
        let mut moved=source.clone();moved.parent=destination_parent.clone();destination.1.insert(index,moved);
        let mut changed=Vec::new();for(parent,group) in groups { for(position,page) in group.into_iter().enumerate(){if page.parent!=parent||page.position!=position as i64{changed.push((page.id,parent.clone(),position as i64));}} }
        let now=Utc::now().timestamp_millis();let mut conn=self.conn.lock().map_err(|_|"db_lock_failed".to_string())?;let transaction=conn.transaction().map_err(|e|format!("db_work_note_transaction_failed:{e}"))?;
        for(page_id,parent,position) in &changed { transaction.execute("UPDATE work_note_pages SET parent_id=?1,position=?2,updated_at_ms=?3 WHERE tenant_id=?4 AND page_id=?5",params![parent,position,now,tenant,page_id]).map_err(|e|format!("db_work_note_move_failed:{e}"))?; }
        transaction.commit().map_err(|e|format!("db_work_note_commit_failed:{e}"))?;drop(conn);
        Ok(json!({"ok":true,"records":self.list_work_notes(tenant,String::new())?,"changed":changed.iter().map(|(page_id,parent_id,position)|json!({"pageId":page_id,"parentId":parent_id,"position":position})).collect::<Vec<_>>()}))
    }

    fn reconcile_mobile_meeting_root(&self, tenant_id: String, ensure_root: bool) -> Result<Value, String> {
        let tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if tenant.is_empty() { return Err("tenant_id_required".to_string()); }
        let now = Utc::now().timestamp_millis();
        let mut conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let transaction = conn.transaction().map_err(|e| format!("db_work_note_transaction_failed:{e}"))?;
        let pages = {
            let mut statement = transaction.prepare("SELECT page_id,parent_id,title,emoji,position,properties_json,document_json,markdown,created_at_ms,updated_at_ms FROM work_note_pages WHERE tenant_id=?1 ORDER BY COALESCE(parent_id,''),position,updated_at_ms DESC")
                .map_err(|e| format!("db_work_note_query_prepare_failed:{e}"))?;
            let rows = statement.query_map(params![tenant], |row| {
                let properties_raw: String = row.get(5)?;
                let blocks_raw: String = row.get(6)?;
                Ok(json!({
                    "tenantId": tenant, "pageId": row.get::<_, String>(0)?, "parentId": row.get::<_, Option<String>>(1)?,
                    "title": row.get::<_, String>(2)?, "emoji": row.get::<_, String>(3)?, "position": row.get::<_, i64>(4)?,
                    "properties": serde_json::from_str::<Value>(&properties_raw).unwrap_or_else(|_| json!({})),
                    "blocks": serde_json::from_str::<Value>(&blocks_raw).unwrap_or_else(|_| json!([])), "markdown": row.get::<_, String>(7)?,
                    "createdAtMs": row.get::<_, i64>(8)?, "updatedAtMs": row.get::<_, i64>(9)?
                }))
            }).map_err(|e| format!("db_work_note_query_failed:{e}"))?;
            let mut result = Vec::new();
            for row in rows { result.push(row.map_err(|e| format!("db_work_note_row_failed:{e}"))?); }
            result
        };
        let canonical = pages.iter().find(|page| page.get("pageId").and_then(Value::as_str) == Some(WORK_MEETING_ROOT_PAGE_ID));
        if canonical.is_some_and(|page| page.pointer("/properties/systemKind").and_then(Value::as_str) != Some("mobile_work_meeting_folder")) {
            return Err("work_note_mobile_meeting_root_conflict".to_string());
        }
        let generated = pages.iter().filter(|page| page.get("title").and_then(Value::as_str) == Some(WORK_MEETING_ROOT_TITLE)
            && page.pointer("/properties/systemKind").and_then(Value::as_str) == Some("mobile_work_meeting_folder")).collect::<Vec<_>>();
        let duplicates = generated.iter().filter(|page| is_safe_mobile_meeting_root(page, false)).map(|page| page.get("pageId").and_then(Value::as_str).unwrap_or("").to_string()).collect::<Vec<_>>();
        let mut preserved = generated.iter().filter(|page| page.get("pageId").and_then(Value::as_str) != Some(WORK_MEETING_ROOT_PAGE_ID) && !is_safe_mobile_meeting_root(page, false)).count();
        let should_have_root = canonical.is_some() || ensure_root || !duplicates.is_empty();
        if canonical.is_none() && should_have_root || canonical.is_some_and(|page| is_safe_mobile_meeting_root(page, true) && is_bug_blanked_mobile_meeting_root(page)) {
            let created_at = canonical.and_then(|page| page.get("createdAtMs").and_then(Value::as_i64)).unwrap_or(now);
            let properties = json!({"systemKind":"mobile_work_meeting_folder","schemaVersion":1,"tags":[WORK_MEETING_ROOT_TITLE]}).to_string();
            let blocks = json!([
                {"id":"work-meeting-root-intro","type":"callout","text":WORK_MEETING_ROOT_INTRO},
                {"id":"work-meeting-root-end","type":"text","text":""}
            ]).to_string();
            let markdown = format!("# {WORK_MEETING_ROOT_TITLE}\n\n{WORK_MEETING_ROOT_INTRO}");
            transaction.execute(r#"INSERT INTO work_note_pages(tenant_id,page_id,parent_id,title,emoji,position,properties_json,document_json,markdown,created_at_ms,updated_at_ms)
                VALUES(?1,?2,NULL,?3,'🗂️',0,?4,?5,?6,?7,?8) ON CONFLICT(tenant_id,page_id) DO UPDATE SET parent_id=NULL,title=excluded.title,emoji=excluded.emoji,position=0,properties_json=excluded.properties_json,document_json=excluded.document_json,markdown=excluded.markdown,updated_at_ms=excluded.updated_at_ms"#,
                params![tenant,WORK_MEETING_ROOT_PAGE_ID,WORK_MEETING_ROOT_TITLE,properties,blocks,markdown,created_at,now])
                .map_err(|e| format!("db_work_note_upsert_failed:{e}"))?;
            transaction.execute("DELETE FROM work_note_pages_fts WHERE tenant_id=?1 AND page_id=?2", params![tenant,WORK_MEETING_ROOT_PAGE_ID]).map_err(|e| format!("db_work_note_fts_delete_failed:{e}"))?;
            transaction.execute("INSERT INTO work_note_pages_fts(tenant_id,page_id,title,markdown) VALUES(?1,?2,?3,?4)", params![tenant,WORK_MEETING_ROOT_PAGE_ID,WORK_MEETING_ROOT_TITLE,markdown]).map_err(|e| format!("db_work_note_fts_insert_failed:{e}"))?;
        }
        let mut deduplicated = 0usize;
        if should_have_root {
            for duplicate in duplicates {
                let attachment: i64 = transaction.query_row("SELECT COUNT(*) FROM work_note_attachments WHERE tenant_id=?1 AND page_id=?2", params![tenant,duplicate], |row| row.get(0)).map_err(|e| format!("db_work_note_attachment_query_failed:{e}"))?;
                let unsafe_child = pages.iter().any(|page| page.get("parentId").and_then(Value::as_str) == Some(duplicate.as_str())
                    && page.pointer("/properties/systemKind").and_then(Value::as_str) != Some("mobile_work_meeting"));
                if attachment > 0 || unsafe_child { preserved += 1; continue; }
                transaction.execute("UPDATE work_note_pages SET parent_id=?1,updated_at_ms=?2 WHERE tenant_id=?3 AND parent_id=?4", params![WORK_MEETING_ROOT_PAGE_ID,now,tenant,duplicate]).map_err(|e| format!("db_work_note_reparent_failed:{e}"))?;
                transaction.execute("DELETE FROM work_note_pages_fts WHERE tenant_id=?1 AND page_id=?2", params![tenant,duplicate]).map_err(|e| format!("db_work_note_fts_delete_failed:{e}"))?;
                let deleted = transaction.execute("DELETE FROM work_note_pages WHERE tenant_id=?1 AND page_id=?2", params![tenant,duplicate]).map_err(|e| format!("db_work_note_delete_failed:{e}"))?;
                if deleted != 1 { return Err("work_note_mobile_meeting_reconcile_failed".to_string()); }
                deduplicated += 1;
            }
        }
        transaction.commit().map_err(|e| format!("db_work_note_commit_failed:{e}"))?;
        drop(conn);
        Ok(json!({"ok":true,"deduplicated":deduplicated,"preserved":preserved,"records":self.list_work_notes(tenant,String::new())?}))
    }

    pub(crate) fn upsert_observation(&self, input: Value) -> Result<Value, String> {
        let record = normalize_observation(input)?;
        let payload_json =
            serde_json::to_string(&record.payload).map_err(|e| format!("payload_encode_failed:{e}"))?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            r#"
            INSERT INTO lesson_observations (
              tenant_id, doc_id, date_key, period, student_code, payload_json, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(tenant_id, doc_id) DO UPDATE SET
              date_key = excluded.date_key,
              period = excluded.period,
              student_code = excluded.student_code,
              payload_json = excluded.payload_json,
              updated_at_ms = excluded.updated_at_ms
            "#,
            params![
                record.tenant_id,
                record.doc_id,
                record.date_key,
                record.period,
                record.student_code,
                payload_json,
                record.updated_at_ms
            ],
        )
        .map_err(|e| format!("db_upsert_failed:{e}"))?;
        Ok(record.payload)
    }

    fn upsert_teacher_counseling_session(&self, input: Value) -> Result<Value, String> {
        let record = normalize_teacher_counseling_session(input)?;
        let payload_json = payload_json(&record.payload, "teacher_counseling_payload_encode_failed")?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            r#"INSERT INTO teacher_counseling_sessions (
              tenant_id, session_id, student_code, counseling_at_ms, status, follow_up_on,
              archived_at_ms, payload_json, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(tenant_id, session_id) DO UPDATE SET
              student_code = excluded.student_code,
              counseling_at_ms = excluded.counseling_at_ms,
              status = excluded.status,
              follow_up_on = excluded.follow_up_on,
              archived_at_ms = excluded.archived_at_ms,
              payload_json = excluded.payload_json,
              updated_at_ms = excluded.updated_at_ms"#,
            params![record.tenant_id, record.session_id, record.student_code, record.counseling_at_ms,
                record.status, if record.follow_up_on.is_empty() { None::<String> } else { Some(record.follow_up_on) },
                if record.archived_at_ms > 0 { Some(record.archived_at_ms) } else { None },
                payload_json, record.updated_at_ms],
        ).map_err(|e| format!("db_teacher_counseling_upsert_failed:{e}"))?;
        Ok(record.payload)
    }

    fn get_teacher_counseling_session(&self, tenant_id: String, session_id: String) -> Result<Option<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        let safe_session = normalize_id_segment(Some(&Value::String(session_id)), 240);
        if safe_tenant.is_empty() { return Err("tenant_id_required".to_string()); }
        if safe_session.is_empty() { return Err("teacher_counseling_session_id_required".to_string()); }
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        match conn.query_row(
            "SELECT payload_json FROM teacher_counseling_sessions WHERE tenant_id = ?1 AND session_id = ?2",
            params![safe_tenant, safe_session], |row| row.get::<_, String>(0),
        ) {
            Ok(raw) => Ok(Some(serde_json::from_str(&raw).unwrap_or_else(|_| json!({})))),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(format!("db_teacher_counseling_get_failed:{error}")),
        }
    }

    fn list_teacher_counseling_sessions(
        &self, tenant_id: String, student_code: String, status: String, include_archived: bool, limit: i64,
    ) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() { return Err("tenant_id_required".to_string()); }
        let safe_student = normalize_student_code(Some(&Value::String(student_code)));
        let safe_status = normalize_enum(Some(&Value::String(status)), &["completed", "follow_up"], "");
        let mut where_parts = vec!["tenant_id = ?".to_string()];
        let mut values: Vec<Box<dyn ToSql>> = vec![Box::new(safe_tenant)];
        if !safe_student.is_empty() { where_parts.push("student_code = ?".to_string()); values.push(Box::new(safe_student)); }
        if !safe_status.is_empty() { where_parts.push("status = ?".to_string()); values.push(Box::new(safe_status)); }
        if !include_archived { where_parts.push("archived_at_ms IS NULL".to_string()); }
        let safe_limit = limit.clamp(1, 500);
        let sql = format!("SELECT payload_json FROM teacher_counseling_sessions WHERE {} ORDER BY counseling_at_ms DESC, session_id ASC LIMIT {safe_limit}", where_parts.join(" AND "));
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let mut stmt = conn.prepare(&sql).map_err(|e| format!("db_teacher_counseling_query_prepare_failed:{e}"))?;
        let refs: Vec<&dyn ToSql> = values.iter().map(|value| value.as_ref() as &dyn ToSql).collect();
        let rows = stmt.query_map(params_from_iter(refs), |row| row.get::<_, String>(0))
            .map_err(|e| format!("db_teacher_counseling_query_failed:{e}"))?;
        let mut out = Vec::new();
        for row in rows {
            let raw = row.map_err(|e| format!("db_teacher_counseling_row_failed:{e}"))?;
            let payload: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
            out.push(json!({
                "id": payload.get("sessionId"), "sessionId": payload.get("sessionId"),
                "tenantId": payload.get("tenantId"), "studentCode": payload.get("studentCode"),
                "studentName": payload.get("studentName"), "classNo": payload.get("classNo"),
                "participantType": payload.get("participantType"), "channel": payload.get("channel"),
                "counselingAtMs": payload.get("counselingAtMs"), "counselingAtIso": payload.get("counselingAtIso"),
                "status": payload.get("status"), "followUpOn": payload.get("followUpOn"),
                "topics": payload.get("topics"), "summaryPreview": normalize_json_text(payload.get("summary"), 160),
                "archivedAtMs": payload.get("archivedAtMs"), "updatedAtMs": payload.get("updatedAtMs")
            }));
        }
        Ok(out)
    }

    pub(crate) fn get_observation_updated_at_ms(&self, tenant_id: &str, doc_id: &str) -> Result<Option<i64>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id.to_string())));
        let safe_doc_id = normalize(doc_id, 240).replace(['/', '\\'], "_");
        if safe_tenant.is_empty() || safe_doc_id.is_empty() {
            return Ok(None);
        }
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        match conn.query_row(
            "SELECT updated_at_ms FROM lesson_observations WHERE tenant_id = ?1 AND doc_id = ?2",
            params![safe_tenant, safe_doc_id],
            |row| row.get::<_, i64>(0),
        ) {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(format!("db_lookup_failed:{error}")),
        }
    }

    pub(crate) fn store_observation_conflict(
        &self,
        tenant_id: &str,
        doc_id: &str,
        source_update_time: &str,
        payload: Value,
    ) -> Result<(), String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id.to_string())));
        let safe_doc_id = normalize(doc_id, 240).replace(['/', '\\'], "_");
        let safe_source_update_time = normalize(source_update_time, 100);
        if safe_tenant.is_empty() || safe_doc_id.is_empty() || safe_source_update_time.is_empty() {
            return Err("conflict_identity_required".to_string());
        }
        let payload_json = serde_json::to_string(&payload).map_err(|e| format!("conflict_encode_failed:{e}"))?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT OR REPLACE INTO lesson_observation_conflicts
             (tenant_id, doc_id, source_update_time, payload_json, captured_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![safe_tenant, safe_doc_id, safe_source_update_time, payload_json, Utc::now().timestamp_millis()],
        )
        .map_err(|e| format!("db_conflict_store_failed:{e}"))?;
        Ok(())
    }

    pub(crate) fn upsert_student_private_detail(&self, input: Value) -> Result<Value, String> {
        let record = normalize_student_private_detail(input)?;
        let payload_json =
            serde_json::to_string(&record.payload).map_err(|e| format!("payload_encode_failed:{e}"))?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            r#"
            INSERT INTO student_private_details (
              tenant_id, student_code, payload_json, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(tenant_id, student_code) DO UPDATE SET
              payload_json = excluded.payload_json,
              updated_at_ms = excluded.updated_at_ms
            "#,
            params![
                record.tenant_id,
                record.student_code,
                payload_json,
                record.updated_at_ms
            ],
        )
        .map_err(|e| format!("db_student_private_upsert_failed:{e}"))?;
        Ok(record.payload)
    }

    pub(crate) fn get_student_private_detail_updated_at_ms(
        &self,
        tenant_id: &str,
        student_code: &str,
    ) -> Result<Option<i64>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id.to_string())));
        let safe_student_code = normalize_student_code(Some(&Value::String(student_code.to_string())));
        if safe_tenant.is_empty() || safe_student_code.is_empty() {
            return Ok(None);
        }
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        match conn.query_row(
            "SELECT updated_at_ms FROM student_private_details WHERE tenant_id = ?1 AND student_code = ?2",
            params![safe_tenant, safe_student_code],
            |row| row.get::<_, i64>(0),
        ) {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(format!("db_student_private_lookup_failed:{error}")),
        }
    }

    pub(crate) fn store_student_private_detail_conflict(
        &self,
        tenant_id: &str,
        student_code: &str,
        source_update_time: &str,
        payload: Value,
    ) -> Result<(), String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id.to_string())));
        let safe_student_code = normalize_student_code(Some(&Value::String(student_code.to_string())));
        let safe_source_update_time = normalize(source_update_time, 100);
        if safe_tenant.is_empty() || safe_student_code.is_empty() || safe_source_update_time.is_empty() {
            return Err("conflict_identity_required".to_string());
        }
        let payload_json = serde_json::to_string(&payload).map_err(|e| format!("conflict_encode_failed:{e}"))?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT OR REPLACE INTO student_private_detail_conflicts
             (tenant_id, student_code, source_update_time, payload_json, captured_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![safe_tenant, safe_student_code, safe_source_update_time, payload_json, Utc::now().timestamp_millis()],
        )
        .map_err(|e| format!("db_student_private_conflict_store_failed:{e}"))?;
        Ok(())
    }

    fn import_student_private_details(&self, tenant_id: String, records: Vec<Value>) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut saved = Vec::new();
        for mut record in records {
            if let Value::Object(ref mut obj) = record {
                obj.insert("tenantId".to_string(), Value::String(safe_tenant.clone()));
            }
            saved.push(self.upsert_student_private_detail(record)?);
        }
        Ok(saved)
    }

    fn list_student_private_details(
        &self,
        tenant_id: String,
        student_code: String,
    ) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        let safe_student_code = normalize_student_code(Some(&Value::String(student_code)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }

        let mut where_parts = vec!["tenant_id = ?".to_string()];
        let mut params_vec: Vec<Box<dyn ToSql>> = vec![Box::new(safe_tenant)];
        if !safe_student_code.is_empty() {
            where_parts.push("student_code = ?".to_string());
            params_vec.push(Box::new(safe_student_code));
        }
        let sql = format!(
            "SELECT payload_json, tenant_id, student_code, updated_at_ms \
             FROM student_private_details WHERE {} \
             ORDER BY student_code ASC LIMIT 1000",
            where_parts.join(" AND ")
        );
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let mut stmt = conn.prepare(&sql).map_err(|e| format!("db_student_private_query_prepare_failed:{e}"))?;
        let params_ref: Vec<&dyn ToSql> = params_vec.iter().map(|v| v.as_ref() as &dyn ToSql).collect();
        let rows = stmt
            .query_map(params_from_iter(params_ref), |row| {
                let payload_json: String = row.get(0)?;
                let mut payload: Value = serde_json::from_str(&payload_json).unwrap_or_else(|_| json!({}));
                if let Value::Object(ref mut obj) = payload {
                    obj.insert("tenantId".to_string(), Value::String(row.get::<_, String>(1)?));
                    obj.insert("studentCode".to_string(), Value::String(row.get::<_, String>(2)?));
                    obj.insert("docId".to_string(), Value::String(row.get::<_, String>(2)?));
                    obj.insert("id".to_string(), Value::String(row.get::<_, String>(2)?));
                    obj.insert("updatedAtMs".to_string(), Value::Number(row.get::<_, i64>(3)?.into()));
                }
                Ok(payload)
            })
            .map_err(|e| format!("db_student_private_query_failed:{e}"))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("db_student_private_row_failed:{e}"))?);
        }
        Ok(out)
    }

    fn import_observations(&self, tenant_id: String, records: Vec<Value>) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut normalized = Vec::new();
        for mut record in records {
            if let Value::Object(ref mut obj) = record {
                obj.insert("tenantId".to_string(), Value::String(safe_tenant.clone()));
            }
            normalized.push(normalize_observation(record)?);
        }
        let mut conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let tx = conn.transaction().map_err(|e| format!("db_observation_import_begin_failed:{e}"))?;
        for record in &normalized {
            let raw = payload_json(&record.payload, "observation_import_payload_encode_failed")?;
            tx.execute(
                r#"INSERT INTO lesson_observations (
                  tenant_id, doc_id, date_key, period, student_code, payload_json, updated_at_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(tenant_id, doc_id) DO UPDATE SET
                  date_key = excluded.date_key, period = excluded.period,
                  student_code = excluded.student_code, payload_json = excluded.payload_json,
                  updated_at_ms = excluded.updated_at_ms"#,
                params![record.tenant_id, record.doc_id, record.date_key, record.period, record.student_code, raw, record.updated_at_ms],
            ).map_err(|e| format!("db_observation_import_failed:{e}"))?;
        }
        tx.commit().map_err(|e| format!("db_observation_import_commit_failed:{e}"))?;
        Ok(normalized.into_iter().map(|record| record.payload).collect())
    }

    fn list_observations(
        &self,
        tenant_id: String,
        from: String,
        to: String,
        date: String,
        period: i64,
        student_code: String,
    ) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }

        let mut where_parts = vec!["tenant_id = ?".to_string()];
        let mut params_vec: Vec<Box<dyn ToSql>> = vec![Box::new(safe_tenant)];
        if !date.is_empty() {
            where_parts.push("date_key = ?".to_string());
            params_vec.push(Box::new(date));
        } else {
            if !from.is_empty() {
                where_parts.push("date_key >= ?".to_string());
                params_vec.push(Box::new(from));
            }
            if !to.is_empty() {
                where_parts.push("date_key <= ?".to_string());
                params_vec.push(Box::new(to));
            }
        }
        if period > 0 {
            where_parts.push("period = ?".to_string());
            params_vec.push(Box::new(period));
        }
        if !student_code.is_empty() {
            where_parts.push("student_code = ?".to_string());
            params_vec.push(Box::new(student_code));
        }

        let sql = format!(
            "SELECT payload_json, tenant_id, doc_id, date_key, period, student_code, updated_at_ms \
             FROM lesson_observations WHERE {} \
             ORDER BY date_key DESC, period DESC, student_code ASC",
            where_parts.join(" AND ")
        );
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let mut stmt = conn.prepare(&sql).map_err(|e| format!("db_query_prepare_failed:{e}"))?;
        let params_ref: Vec<&dyn ToSql> = params_vec.iter().map(|v| v.as_ref() as &dyn ToSql).collect();
        let rows = stmt
            .query_map(params_from_iter(params_ref), |row| {
                let payload_json: String = row.get(0)?;
                let mut payload: Value = serde_json::from_str(&payload_json).unwrap_or_else(|_| json!({}));
                if let Value::Object(ref mut obj) = payload {
                    obj.insert("tenantId".to_string(), Value::String(row.get::<_, String>(1)?));
                    obj.insert("docId".to_string(), Value::String(row.get::<_, String>(2)?));
                    obj.insert("id".to_string(), Value::String(row.get::<_, String>(2)?));
                    obj.insert("date".to_string(), Value::String(row.get::<_, String>(3)?));
                    obj.insert("period".to_string(), Value::Number(row.get::<_, i64>(4)?.into()));
                    obj.insert("studentCode".to_string(), Value::String(row.get::<_, String>(5)?));
                    obj.insert("updatedAtMs".to_string(), Value::Number(row.get::<_, i64>(6)?.into()));
                }
                Ok(payload)
            })
            .map_err(|e| format!("db_query_failed:{e}"))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("db_row_failed:{e}"))?);
        }
        Ok(out)
    }

    fn stats(&self, tenant_id: String) -> Result<Value, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let mut observation_stmt = conn
            .prepare(
                "SELECT COUNT(*), MIN(date_key), MAX(date_key), MAX(updated_at_ms) \
                 FROM lesson_observations WHERE tenant_id = ?1",
            )
            .map_err(|e| format!("db_stats_prepare_failed:{e}"))?;
        let observation = observation_stmt
            .query_row(params![&safe_tenant], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            })
            .map_err(|e| format!("db_stats_failed:{e}"))?;
        let teacher_counseling = conn.query_row(
            "SELECT COUNT(*), MAX(updated_at_ms) FROM teacher_counseling_sessions WHERE tenant_id = ?1",
            params![&safe_tenant],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
        ).map_err(|e| format!("db_stats_teacher_counseling_failed:{e}"))?;
        let mut student_private_stmt = conn
            .prepare(
                "SELECT COUNT(*), MAX(updated_at_ms) \
                 FROM student_private_details WHERE tenant_id = ?1",
            )
            .map_err(|e| format!("db_stats_student_private_prepare_failed:{e}"))?;
        let student_private = student_private_stmt
            .query_row(params![&safe_tenant], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                ))
            })
            .map_err(|e| format!("db_stats_student_private_failed:{e}"))?;
        let mut snapshot_stmt = conn
            .prepare(
                "SELECT COUNT(*), MAX(updated_at_ms), MAX(archived_at_ms) \
                 FROM board_post_snapshots WHERE tenant_id = ?1",
            )
            .map_err(|e| format!("db_stats_board_snapshot_prepare_failed:{e}"))?;
        let snapshots = snapshot_stmt
            .query_row(params![&safe_tenant], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            })
            .map_err(|e| format!("db_stats_board_snapshot_failed:{e}"))?;
        let mut media_stmt = conn
            .prepare(
                "SELECT COUNT(*), COALESCE(SUM(size), 0), MAX(archived_at_ms) \
                 FROM board_media_files WHERE tenant_id = ?1",
            )
            .map_err(|e| format!("db_stats_board_media_prepare_failed:{e}"))?;
        let media = media_stmt
            .query_row(params![&safe_tenant], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            })
            .map_err(|e| format!("db_stats_board_media_failed:{e}"))?;
        let math_attempts = conn
            .query_row(
                "SELECT COUNT(*), MIN(date_key), MAX(date_key), MAX(updated_at_ms) FROM math_daily_attempts WHERE tenant_id = ?1",
                params![&safe_tenant],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, Option<i64>>(3)?)),
            )
            .map_err(|e| format!("db_stats_math_attempts_failed:{e}"))?;
        let math_profiles = conn
            .query_row(
                "SELECT COUNT(*), MAX(updated_at_ms) FROM math_daily_student_profiles WHERE tenant_id = ?1",
                params![&safe_tenant],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .map_err(|e| format!("db_stats_math_profiles_failed:{e}"))?;
        let math_reviews = conn
            .query_row(
                "SELECT COUNT(*), MAX(updated_at_ms) FROM math_daily_review_sessions WHERE tenant_id = ?1",
                params![&safe_tenant],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .map_err(|e| format!("db_stats_math_reviews_failed:{e}"))?;
        let math_assignments = conn
            .query_row(
                "SELECT COUNT(*), MAX(updated_at_ms) FROM math_daily_assignments WHERE tenant_id = ?1",
                params![&safe_tenant],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .map_err(|e| format!("db_stats_math_assignments_failed:{e}"))?;
        let math_assignment_results = conn
            .query_row(
                "SELECT COUNT(*), MAX(updated_at_ms) FROM math_daily_assignment_results WHERE tenant_id = ?1",
                params![&safe_tenant],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .map_err(|e| format!("db_stats_math_assignment_results_failed:{e}"))?;
        let math_cache_runs = conn
            .query_row(
                "SELECT COUNT(*), MAX(updated_at_ms) FROM math_daily_cache_runs WHERE tenant_id = ?1",
                params![&safe_tenant],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .map_err(|e| format!("db_stats_math_cache_runs_failed:{e}"))?;
        let math_cache = conn
            .query_row(
                "SELECT action, date_from, date_to, date_key, curriculum, updated_at_ms FROM math_daily_cache_runs WHERE tenant_id = ?1 ORDER BY updated_at_ms DESC LIMIT 1",
                params![&safe_tenant],
                |row| Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                )),
            )
            .unwrap_or_else(|_| (None, None, None, None, None, None));
        let attendance_records = conn
            .query_row(
                "SELECT COUNT(*), MIN(date_key), MAX(date_key), MAX(updated_at_ms) FROM attendance_records WHERE tenant_id = ?1",
                params![&safe_tenant],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, Option<i64>>(3)?)),
            )
            .map_err(|e| format!("db_stats_attendance_records_failed:{e}"))?;
        let attendance_nais_checks = conn
            .query_row(
                "SELECT COUNT(*), MAX(updated_at_ms) FROM attendance_nais_checks WHERE tenant_id = ?1",
                params![&safe_tenant],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .map_err(|e| format!("db_stats_attendance_nais_checks_failed:{e}"))?;
        let attendance_document_requests = conn
            .query_row(
                "SELECT COUNT(*), MAX(updated_at_ms) FROM attendance_document_requests WHERE tenant_id = ?1",
                params![&safe_tenant],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .map_err(|e| format!("db_stats_attendance_document_requests_failed:{e}"))?;
        let counseling_records = conn
            .query_row(
                "SELECT COUNT(*), MAX(updated_at_ms) FROM counseling_records WHERE tenant_id = ?1",
                params![&safe_tenant],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .map_err(|e| format!("db_stats_counseling_records_failed:{e}"))?;
        let counseling_teacher_notes = conn
            .query_row(
                "SELECT COUNT(*), MAX(updated_at_ms) FROM counseling_teacher_notes WHERE tenant_id = ?1",
                params![&safe_tenant],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .map_err(|e| format!("db_stats_counseling_teacher_notes_failed:{e}"))?;
        let eval_assignments = conn
            .query_row(
                "SELECT COUNT(*), MAX(updated_at_ms) FROM eval_assignments WHERE tenant_id = ?1",
                params![&safe_tenant],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .map_err(|e| format!("db_stats_eval_assignments_failed:{e}"))?;
        let eval_results = conn
            .query_row(
                "SELECT COUNT(*), MAX(updated_at_ms) FROM eval_results WHERE tenant_id = ?1",
                params![&safe_tenant],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .map_err(|e| format!("db_stats_eval_results_failed:{e}"))?;
        let student_record_draft_sets = conn
            .query_row(
                "SELECT COUNT(*), MAX(updated_at_ms) FROM student_record_draft_sets WHERE tenant_id = ?1",
                params![&safe_tenant],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .map_err(|e| format!("db_stats_student_record_draft_sets_failed:{e}"))?;
        let student_record_drafts = conn
            .query_row(
                "SELECT COUNT(*), MAX(updated_at_ms) FROM student_record_drafts WHERE tenant_id = ?1",
                params![&safe_tenant],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .map_err(|e| format!("db_stats_student_record_drafts_failed:{e}"))?;
        let import_runs = conn
            .query_row(
                "SELECT COUNT(*), MAX(finished_at_ms) FROM local_import_runs WHERE tenant_id = ?1",
                params![&safe_tenant],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .map_err(|e| format!("db_stats_import_runs_failed:{e}"))?;
        let work_notes = conn
            .query_row(
                "SELECT COUNT(*), MAX(updated_at_ms) FROM work_note_pages WHERE tenant_id = ?1",
                params![&safe_tenant],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .map_err(|e| format!("db_stats_work_notes_failed:{e}"))?;
        let cloud_sync_runs = conn
            .query_row(
                "SELECT COUNT(*), MAX(finished_at_ms) FROM cloud_sync_runs WHERE tenant_id = ?1",
                params![&safe_tenant],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .map_err(|e| format!("db_stats_cloud_sync_runs_failed:{e}"))?;
        let observation_updated_at_ms = observation.3.unwrap_or(0);
        let teacher_counseling_updated_at_ms = teacher_counseling.1.unwrap_or(0);
        let student_private_updated_at_ms = student_private.1.unwrap_or(0);
        let math_daily_updated_at_ms = math_attempts
            .3
            .unwrap_or(0)
            .max(math_profiles.1.unwrap_or(0))
            .max(math_reviews.1.unwrap_or(0))
            .max(math_assignments.1.unwrap_or(0))
            .max(math_assignment_results.1.unwrap_or(0))
            .max(math_cache_runs.1.unwrap_or(0))
            .max(math_cache.5.unwrap_or(0));
        let board_snapshot_archived_at_ms = snapshots.2.unwrap_or(0);
        let board_media_archived_at_ms = media.2.unwrap_or(0);
        let attendance_updated_at_ms = attendance_records
            .3
            .unwrap_or(0)
            .max(attendance_nais_checks.1.unwrap_or(0))
            .max(attendance_document_requests.1.unwrap_or(0));
        let counseling_updated_at_ms = counseling_records
            .1
            .unwrap_or(0)
            .max(counseling_teacher_notes.1.unwrap_or(0));
        let evals_updated_at_ms = eval_assignments
            .1
            .unwrap_or(0)
            .max(eval_results.1.unwrap_or(0));
        let student_record_draft_updated_at_ms = student_record_draft_sets
            .1
            .unwrap_or(0)
            .max(student_record_drafts.1.unwrap_or(0));
        let import_run_updated_at_ms = import_runs.1.unwrap_or(0);
        let work_note_updated_at_ms = work_notes.1.unwrap_or(0);
        let cloud_sync_run_updated_at_ms = cloud_sync_runs.1.unwrap_or(0);
        let latest_local_write_at_ms = observation_updated_at_ms
            .max(teacher_counseling_updated_at_ms)
            .max(student_private_updated_at_ms)
            .max(math_daily_updated_at_ms)
            .max(board_snapshot_archived_at_ms)
            .max(board_media_archived_at_ms)
            .max(attendance_updated_at_ms)
            .max(counseling_updated_at_ms)
            .max(evals_updated_at_ms)
            .max(student_record_draft_updated_at_ms)
            .max(import_run_updated_at_ms)
            .max(work_note_updated_at_ms)
            .max(cloud_sync_run_updated_at_ms);
        let mut stats = Map::new();
        macro_rules! put_stat {
            ($key:literal, $value:expr) => {
                stats.insert($key.to_string(), json!($value));
            };
        }
        put_stat!("count", observation.0);
        put_stat!("observationCount", observation.0);
        put_stat!("firstDate", observation.1.unwrap_or_default());
        put_stat!("lastDate", observation.2.unwrap_or_default());
        put_stat!("updatedAtMs", observation_updated_at_ms);
        put_stat!("observationUpdatedAtMs", observation_updated_at_ms);
        put_stat!("teacherCounselingSessionCount", teacher_counseling.0);
        put_stat!("teacherCounselingUpdatedAtMs", teacher_counseling_updated_at_ms);
        put_stat!("studentPrivateDetailCount", student_private.0);
        put_stat!("studentPrivateDetailUpdatedAtMs", student_private_updated_at_ms);
        put_stat!("mathDailyAttemptCount", math_attempts.0);
        put_stat!("mathDailyProfileCount", math_profiles.0);
        put_stat!("mathDailyReviewSessionCount", math_reviews.0);
        put_stat!("mathDailyAssignmentCount", math_assignments.0);
        put_stat!("mathDailyAssignmentResultCount", math_assignment_results.0);
        put_stat!("mathDailyCacheRunCount", math_cache_runs.0);
        put_stat!("mathDailyFirstDate", math_attempts.1.unwrap_or_default());
        put_stat!("mathDailyLastDate", math_attempts.2.unwrap_or_default());
        put_stat!("mathDailyCacheAction", math_cache.0.unwrap_or_default());
        put_stat!("mathDailyCacheDateFrom", math_cache.1.unwrap_or_default());
        put_stat!("mathDailyCacheDateTo", math_cache.2.unwrap_or_default());
        put_stat!("mathDailyCacheDateKey", math_cache.3.unwrap_or_default());
        put_stat!("mathDailyCacheCurriculum", math_cache.4.unwrap_or_default());
        put_stat!("mathDailyUpdatedAtMs", math_daily_updated_at_ms);
        put_stat!("boardSnapshotCount", snapshots.0);
        put_stat!("boardSnapshotUpdatedAtMs", snapshots.1.unwrap_or(0));
        put_stat!("boardSnapshotArchivedAtMs", board_snapshot_archived_at_ms);
        put_stat!("boardMediaCount", media.0);
        put_stat!("boardMediaSizeBytes", media.1);
        put_stat!("boardMediaArchivedAtMs", board_media_archived_at_ms);
        put_stat!(
            "boardArchivedAtMs",
            board_snapshot_archived_at_ms.max(board_media_archived_at_ms)
        );
        put_stat!("attendanceRecordCount", attendance_records.0);
        put_stat!("attendanceNaisCheckCount", attendance_nais_checks.0);
        put_stat!(
            "attendanceDocumentRequestCount",
            attendance_document_requests.0
        );
        put_stat!("attendanceFirstDate", attendance_records.1.unwrap_or_default());
        put_stat!("attendanceLastDate", attendance_records.2.unwrap_or_default());
        put_stat!("attendanceUpdatedAtMs", attendance_updated_at_ms);
        put_stat!("counselingRecordCount", counseling_records.0);
        put_stat!("counselingTeacherNoteCount", counseling_teacher_notes.0);
        put_stat!("counselingUpdatedAtMs", counseling_updated_at_ms);
        put_stat!("evalAssignmentCount", eval_assignments.0);
        put_stat!("evalResultCount", eval_results.0);
        put_stat!("evalsUpdatedAtMs", evals_updated_at_ms);
        put_stat!("studentRecordDraftSetCount", student_record_draft_sets.0);
        put_stat!("studentRecordDraftCount", student_record_drafts.0);
        put_stat!(
            "studentRecordDraftUpdatedAtMs",
            student_record_draft_updated_at_ms
        );
        put_stat!("importRunCount", import_runs.0);
        put_stat!("importRunUpdatedAtMs", import_run_updated_at_ms);
        put_stat!("workNoteCount", work_notes.0);
        put_stat!("workNoteUpdatedAtMs", work_note_updated_at_ms);
        put_stat!("cloudSyncRunCount", cloud_sync_runs.0);
        put_stat!("cloudSyncRunUpdatedAtMs", cloud_sync_run_updated_at_ms);
        put_stat!("latestLocalWriteAtMs", latest_local_write_at_ms);
        Ok(Value::Object(stats))
    }

    pub(crate) fn record_cloud_sync_run(&self, mut input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        let run_id = normalize_id_segment(input.get("runId"), 160);
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        if run_id.is_empty() {
            return Err("cloud_sync_run_id_required".to_string());
        }
        let started_at_ms = input
            .get("startedAtMs")
            .and_then(|v| v.as_i64())
            .unwrap_or_else(|| Utc::now().timestamp_millis());
        let finished_at_ms = input
            .get("finishedAtMs")
            .and_then(|v| v.as_i64())
            .unwrap_or(started_at_ms)
            .max(started_at_ms);
        if let Value::Object(ref mut obj) = input {
            obj.insert("tenantId".to_string(), Value::String(tenant_id.clone()));
            obj.insert("runId".to_string(), Value::String(run_id.clone()));
            obj.insert("startedAtMs".to_string(), Value::Number(started_at_ms.into()));
            obj.insert("finishedAtMs".to_string(), Value::Number(finished_at_ms.into()));
        }
        let payload_json =
            serde_json::to_string(&input).map_err(|e| format!("cloud_sync_run_encode_failed:{e}"))?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            r#"
            INSERT INTO cloud_sync_runs (
              tenant_id, run_id, payload_json, started_at_ms, finished_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(tenant_id, run_id) DO UPDATE SET
              payload_json = excluded.payload_json,
              started_at_ms = excluded.started_at_ms,
              finished_at_ms = excluded.finished_at_ms
            "#,
            params![tenant_id, run_id, payload_json, started_at_ms, finished_at_ms],
        )
        .map_err(|e| format!("db_cloud_sync_run_store_failed:{e}"))?;
        Ok(input)
    }

    fn list_cloud_sync_runs(&self, tenant_id: String, limit: i64) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let safe_limit = limit.clamp(1, 50);
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT payload_json FROM cloud_sync_runs \
                 WHERE tenant_id = ?1 ORDER BY finished_at_ms DESC LIMIT ?2",
            )
            .map_err(|e| format!("db_cloud_sync_runs_prepare_failed:{e}"))?;
        let rows = stmt
            .query_map(params![safe_tenant, safe_limit], |row| {
                let payload_json: String = row.get(0)?;
                Ok(serde_json::from_str::<Value>(&payload_json).unwrap_or_else(|_| json!({})))
            })
            .map_err(|e| format!("db_cloud_sync_runs_query_failed:{e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("db_cloud_sync_runs_row_failed:{e}"))?);
        }
        Ok(out)
    }

    fn upsert_math_daily_attempt(&self, mut input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        let student_code = normalize_student_code(input.get("studentCode").or_else(|| input.get("code")));
        let date_key = normalize_date_key(input.get("dateKey").or_else(|| input.get("date")));
        let fallback_id = format!("{tenant_id}__{student_code}__{date_key}");
        let attempt_id = {
            let explicit = normalize_id_segment(input.get("attemptId").or_else(|| input.get("id")), 240);
            if explicit.is_empty() { normalize(fallback_id, 240).replace(['/', '\\'], "_") } else { explicit }
        };
        let curriculum = normalize_json_text(input.get("curriculum"), 120);
        if tenant_id.is_empty() || student_code.is_empty() || date_key.is_empty() || attempt_id.is_empty() {
            return Err("math_attempt_identity_required".to_string());
        }
        let updated_at_ms = updated_at_ms(&input);
        if let Value::Object(ref mut obj) = input {
            obj.insert("tenantId".to_string(), Value::String(tenant_id.clone()));
            obj.insert("studentCode".to_string(), Value::String(student_code.clone()));
            obj.insert("dateKey".to_string(), Value::String(date_key.clone()));
            obj.insert("id".to_string(), Value::String(attempt_id.clone()));
            obj.insert("attemptId".to_string(), Value::String(attempt_id.clone()));
            obj.insert("curriculum".to_string(), Value::String(curriculum.clone()));
            obj.insert("updatedAtMs".to_string(), Value::Number(updated_at_ms.into()));
        }
        let payload_json = serde_json::to_string(&input).map_err(|e| format!("math_attempt_encode_failed:{e}"))?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT INTO math_daily_attempts
             (tenant_id, attempt_id, date_key, student_code, curriculum, payload_json, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(tenant_id, attempt_id) DO UPDATE SET
               date_key = excluded.date_key,
               student_code = excluded.student_code,
               curriculum = excluded.curriculum,
               payload_json = excluded.payload_json,
               updated_at_ms = excluded.updated_at_ms",
            params![tenant_id, attempt_id, date_key, student_code, curriculum, payload_json, updated_at_ms],
        )
        .map_err(|e| format!("db_math_attempt_upsert_failed:{e}"))?;
        Ok(input)
    }

    fn upsert_math_daily_student_profile(&self, mut input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        let student_code = normalize_student_code(
            input.get("studentCode").or_else(|| input.get("code")).or_else(|| input.get("id")).or_else(|| input.get("docId")),
        );
        if tenant_id.is_empty() || student_code.is_empty() {
            return Err("math_profile_identity_required".to_string());
        }
        let updated_at_ms = updated_at_ms(&input);
        if let Value::Object(ref mut obj) = input {
            obj.insert("tenantId".to_string(), Value::String(tenant_id.clone()));
            obj.insert("studentCode".to_string(), Value::String(student_code.clone()));
            obj.insert("id".to_string(), Value::String(format!("{tenant_id}__{student_code}")));
            obj.insert("updatedAtMs".to_string(), Value::Number(updated_at_ms.into()));
        }
        let payload_json = serde_json::to_string(&input).map_err(|e| format!("math_profile_encode_failed:{e}"))?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT INTO math_daily_student_profiles
             (tenant_id, student_code, payload_json, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(tenant_id, student_code) DO UPDATE SET
               payload_json = excluded.payload_json,
               updated_at_ms = excluded.updated_at_ms",
            params![tenant_id, student_code, payload_json, updated_at_ms],
        )
        .map_err(|e| format!("db_math_profile_upsert_failed:{e}"))?;
        Ok(input)
    }

    fn upsert_math_daily_review_session(&self, mut input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        let student_code = normalize_student_code(input.get("studentCode").or_else(|| input.get("code")));
        let date_key = normalize_date_key(input.get("dateKey").or_else(|| input.get("date")));
        let fallback_id = format!("{tenant_id}__{student_code}__{date_key}__review");
        let review_session_id = {
            let explicit = normalize_id_segment(input.get("reviewSessionId").or_else(|| input.get("sessionId")).or_else(|| input.get("id")), 240);
            if explicit.is_empty() { normalize(fallback_id, 240).replace(['/', '\\'], "_") } else { explicit }
        };
        let curriculum = normalize_json_text(input.get("curriculum"), 120);
        if tenant_id.is_empty() || student_code.is_empty() || date_key.is_empty() || review_session_id.is_empty() {
            return Err("math_review_identity_required".to_string());
        }
        let updated_at_ms = updated_at_ms(&input);
        if let Value::Object(ref mut obj) = input {
            obj.insert("tenantId".to_string(), Value::String(tenant_id.clone()));
            obj.insert("studentCode".to_string(), Value::String(student_code.clone()));
            obj.insert("dateKey".to_string(), Value::String(date_key.clone()));
            obj.insert("id".to_string(), Value::String(review_session_id.clone()));
            obj.insert("reviewSessionId".to_string(), Value::String(review_session_id.clone()));
            obj.insert("curriculum".to_string(), Value::String(curriculum.clone()));
            obj.insert("updatedAtMs".to_string(), Value::Number(updated_at_ms.into()));
        }
        let payload_json = serde_json::to_string(&input).map_err(|e| format!("math_review_encode_failed:{e}"))?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT INTO math_daily_review_sessions
             (tenant_id, review_session_id, date_key, student_code, curriculum, payload_json, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(tenant_id, review_session_id) DO UPDATE SET
               date_key = excluded.date_key,
               student_code = excluded.student_code,
               curriculum = excluded.curriculum,
               payload_json = excluded.payload_json,
               updated_at_ms = excluded.updated_at_ms",
            params![tenant_id, review_session_id, date_key, student_code, curriculum, payload_json, updated_at_ms],
        )
        .map_err(|e| format!("db_math_review_upsert_failed:{e}"))?;
        Ok(input)
    }

    fn upsert_math_daily_assignment(&self, mut input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        let assignment_id = normalize_id_segment(input.get("assignmentId").or_else(|| input.get("id")).or_else(|| input.get("docId")), 240);
        if tenant_id.is_empty() || assignment_id.is_empty() {
            return Err("math_assignment_identity_required".to_string());
        }
        let date_key = math_assignment_date_key(&input);
        let curriculum = math_assignment_curriculum(&input);
        let updated_at_ms = updated_at_ms(&input);
        if let Value::Object(ref mut obj) = input {
            obj.insert("tenantId".to_string(), Value::String(tenant_id.clone()));
            obj.insert("id".to_string(), Value::String(assignment_id.clone()));
            obj.insert("assignmentId".to_string(), Value::String(assignment_id.clone()));
            obj.insert("dateKey".to_string(), Value::String(date_key.clone()));
            obj.insert("curriculum".to_string(), Value::String(curriculum.clone()));
            obj.insert("updatedAtMs".to_string(), Value::Number(updated_at_ms.into()));
        }
        let payload_json = serde_json::to_string(&input).map_err(|e| format!("math_assignment_encode_failed:{e}"))?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT INTO math_daily_assignments
             (tenant_id, assignment_id, date_key, curriculum, payload_json, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(tenant_id, assignment_id) DO UPDATE SET
               date_key = excluded.date_key,
               curriculum = excluded.curriculum,
               payload_json = excluded.payload_json,
               updated_at_ms = excluded.updated_at_ms",
            params![tenant_id, assignment_id, date_key, curriculum, payload_json, updated_at_ms],
        )
        .map_err(|e| format!("db_math_assignment_upsert_failed:{e}"))?;
        Ok(input)
    }

    fn upsert_math_daily_assignment_result(&self, mut input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        let assignment_id = normalize_id_segment(input.get("assignmentId").or_else(|| input.get("assignment").and_then(|assignment| assignment.get("id"))), 240);
        let student_code = normalize_student_code(input.get("studentCode").or_else(|| input.get("code")));
        let fallback_id = format!("{assignment_id}_{student_code}");
        let submission_id = {
            let explicit = normalize_id_segment(input.get("submissionId").or_else(|| input.get("id")).or_else(|| input.get("docId")), 260);
            if explicit.is_empty() { normalize(fallback_id, 260).replace(['/', '\\'], "_") } else { explicit }
        };
        if tenant_id.is_empty() || assignment_id.is_empty() || student_code.is_empty() || submission_id.is_empty() {
            return Err("math_assignment_result_identity_required".to_string());
        }
        let result = input.get("mathDailyResult").unwrap_or(&Value::Null);
        let date_key = normalize_date_key(
            input.get("dateKey")
                .or_else(|| result.get("dateKey"))
                .or_else(|| input.get("assignmentDateKey"))
                .or_else(|| input.get("dueDateKey")),
        );
        let curriculum = normalize_json_text(
            input.get("curriculum")
                .or_else(|| result.get("curriculum"))
                .or_else(|| input.get("mathDailyConfig").and_then(|config| config.get("curriculum"))),
            120,
        );
        let updated_at_ms = updated_at_ms(&input);
        if let Value::Object(ref mut obj) = input {
            obj.insert("tenantId".to_string(), Value::String(tenant_id.clone()));
            obj.insert("id".to_string(), Value::String(submission_id.clone()));
            obj.insert("submissionId".to_string(), Value::String(submission_id.clone()));
            obj.insert("assignmentId".to_string(), Value::String(assignment_id.clone()));
            obj.insert("studentCode".to_string(), Value::String(student_code.clone()));
            obj.insert("dateKey".to_string(), Value::String(date_key.clone()));
            obj.insert("curriculum".to_string(), Value::String(curriculum.clone()));
            obj.insert("updatedAtMs".to_string(), Value::Number(updated_at_ms.into()));
        }
        let payload_json = serde_json::to_string(&input).map_err(|e| format!("math_assignment_result_encode_failed:{e}"))?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT INTO math_daily_assignment_results
             (tenant_id, submission_id, assignment_id, student_code, date_key, curriculum, payload_json, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(tenant_id, submission_id) DO UPDATE SET
               assignment_id = excluded.assignment_id,
               student_code = excluded.student_code,
               date_key = excluded.date_key,
               curriculum = excluded.curriculum,
               payload_json = excluded.payload_json,
               updated_at_ms = excluded.updated_at_ms",
            params![tenant_id, submission_id, assignment_id, student_code, date_key, curriculum, payload_json, updated_at_ms],
        )
        .map_err(|e| format!("db_math_assignment_result_upsert_failed:{e}"))?;
        Ok(input)
    }

    fn upsert_math_daily_cache_run(&self, mut input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let action = {
            let value = normalize_json_text(input.get("action"), 80);
            if value.is_empty() { "cache".to_string() } else { value }
        };
        let cache_key = math_daily_cache_key(&input);
        let date_key = normalize_date_key(input.get("dateKey").or_else(|| input.get("date")));
        let date_from = normalize_date_key(input.get("dateFrom").or_else(|| input.get("from")));
        let date_to = normalize_date_key(input.get("dateTo").or_else(|| input.get("to")));
        let curriculum = normalize_json_text(input.get("curriculum"), 120);
        let updated_at_ms = updated_at_ms(&input);
        if let Value::Object(ref mut obj) = input {
            obj.insert("tenantId".to_string(), Value::String(tenant_id.clone()));
            obj.insert("action".to_string(), Value::String(action.clone()));
            obj.insert("cacheKey".to_string(), Value::String(cache_key.clone()));
            obj.insert("dateKey".to_string(), Value::String(date_key.clone()));
            obj.insert("dateFrom".to_string(), Value::String(date_from.clone()));
            obj.insert("dateTo".to_string(), Value::String(date_to.clone()));
            obj.insert("curriculum".to_string(), Value::String(curriculum.clone()));
            obj.insert("updatedAtMs".to_string(), Value::Number(updated_at_ms.into()));
        }
        let payload_json = serde_json::to_string(&input).map_err(|e| format!("math_cache_encode_failed:{e}"))?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT INTO math_daily_cache_runs
             (tenant_id, cache_key, action, date_from, date_to, date_key, curriculum, payload_json, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(tenant_id, cache_key) DO UPDATE SET
               action = excluded.action,
               date_from = excluded.date_from,
               date_to = excluded.date_to,
               date_key = excluded.date_key,
               curriculum = excluded.curriculum,
               payload_json = excluded.payload_json,
               updated_at_ms = excluded.updated_at_ms",
            params![tenant_id, cache_key, action, date_from, date_to, date_key, curriculum, payload_json, updated_at_ms],
        )
        .map_err(|e| format!("db_math_cache_upsert_failed:{e}"))?;
        Ok(input)
    }

    fn import_math_daily_cache(&self, mut input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let arrays = |key: &str, value: &Value| -> Vec<Value> {
            value.get(key).and_then(|v| v.as_array()).cloned().unwrap_or_default()
        };
        let mut attempts = Vec::new();
        for mut record in arrays("attempts", &input) {
            if let Value::Object(ref mut obj) = record {
                obj.insert("tenantId".to_string(), Value::String(tenant_id.clone()));
            }
            attempts.push(self.upsert_math_daily_attempt(record)?);
        }
        let mut student_profiles = Vec::new();
        let profile_rows = if let Some(rows) = input.get("studentProfiles").and_then(|v| v.as_array()).cloned() {
            rows
        } else {
            input.get("studentProfilesByCode")
                .and_then(|v| v.as_object())
                .map(|obj| obj.values().cloned().collect())
                .unwrap_or_default()
        };
        for mut record in profile_rows {
            if let Value::Object(ref mut obj) = record {
                obj.insert("tenantId".to_string(), Value::String(tenant_id.clone()));
            }
            student_profiles.push(self.upsert_math_daily_student_profile(record)?);
        }
        let mut review_sessions = Vec::new();
        for mut record in arrays("reviewSessions", &input) {
            if let Value::Object(ref mut obj) = record {
                obj.insert("tenantId".to_string(), Value::String(tenant_id.clone()));
            }
            review_sessions.push(self.upsert_math_daily_review_session(record)?);
        }
        let mut assignments = Vec::new();
        for mut record in arrays("assignments", &input) {
            if let Value::Object(ref mut obj) = record {
                obj.insert("tenantId".to_string(), Value::String(tenant_id.clone()));
            }
            assignments.push(self.upsert_math_daily_assignment(record)?);
        }
        let mut assignment_submissions = Vec::new();
        let submission_rows = if let Some(rows) = input.get("assignmentSubmissions").and_then(|v| v.as_array()).cloned() {
            rows
        } else {
            arrays("assignmentResults", &input)
        };
        for mut record in submission_rows {
            if let Value::Object(ref mut obj) = record {
                obj.insert("tenantId".to_string(), Value::String(tenant_id.clone()));
            }
            assignment_submissions.push(self.upsert_math_daily_assignment_result(record)?);
        }
        if let Value::Object(ref mut obj) = input {
            obj.insert("tenantId".to_string(), Value::String(tenant_id.clone()));
            obj.insert("attempts".to_string(), Value::Array(attempts.clone()));
            obj.insert("studentProfiles".to_string(), Value::Array(student_profiles.clone()));
            obj.insert(
                "studentProfilesByCode".to_string(),
                Value::Object(student_profiles.iter().filter_map(|profile| {
                    let code = profile.get("studentCode").and_then(|v| v.as_str())?;
                    Some((code.to_string(), profile.clone()))
                }).collect::<Map<String, Value>>()),
            );
            obj.insert("reviewSessions".to_string(), Value::Array(review_sessions.clone()));
            obj.insert("assignments".to_string(), Value::Array(assignments.clone()));
            obj.insert("assignmentSubmissions".to_string(), Value::Array(assignment_submissions.clone()));
            obj.insert("importedAtMs".to_string(), Value::Number(now_ms().into()));
        }
        let cache = self.upsert_math_daily_cache_run(input)?;
        Ok(json!({
            "ok": true,
            "tenantId": tenant_id,
            "cache": cache,
            "imported": {
                "attempts": attempts.len(),
                "studentProfiles": student_profiles.len(),
                "reviewSessions": review_sessions.len(),
                "assignments": assignments.len(),
                "assignmentSubmissions": assignment_submissions.len()
            }
        }))
    }

    fn query_math_payloads(&self, table: &str, select: &str, where_parts: Vec<String>, params_vec: Vec<Box<dyn ToSql>>, order: &str, limit: i64) -> Result<Vec<Value>, String> {
        let sql = format!(
            "SELECT {select} FROM {table} WHERE {} ORDER BY {order} LIMIT {}",
            where_parts.join(" AND "),
            limit.clamp(1, 5000)
        );
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let mut stmt = conn.prepare(&sql).map_err(|e| format!("db_math_query_prepare_failed:{e}"))?;
        let params_ref: Vec<&dyn ToSql> = params_vec.iter().map(|v| v.as_ref() as &dyn ToSql).collect();
        let rows = stmt
            .query_map(params_from_iter(params_ref), |row| {
                let payload_json: String = row.get(0)?;
                Ok(serde_json::from_str::<Value>(&payload_json).unwrap_or_else(|_| json!({})))
            })
            .map_err(|e| format!("db_math_query_failed:{e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("db_math_row_failed:{e}"))?);
        }
        Ok(out)
    }

    fn list_math_daily_attempts(&self, tenant_id: String, date_key: String, date_from: String, date_to: String, student_code: String, curriculum: String) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut where_parts = vec!["tenant_id = ?".to_string()];
        let mut params_vec: Vec<Box<dyn ToSql>> = vec![Box::new(safe_tenant)];
        let safe_date = normalize_date_key(Some(&Value::String(date_key)));
        if !safe_date.is_empty() {
            where_parts.push("date_key = ?".to_string());
            params_vec.push(Box::new(safe_date));
        } else {
            let from = normalize_date_key(Some(&Value::String(date_from)));
            let to = normalize_date_key(Some(&Value::String(date_to)));
            if !from.is_empty() {
                where_parts.push("date_key >= ?".to_string());
                params_vec.push(Box::new(from));
            }
            if !to.is_empty() {
                where_parts.push("date_key <= ?".to_string());
                params_vec.push(Box::new(to));
            }
        }
        let safe_student = normalize_student_code(Some(&Value::String(student_code)));
        if !safe_student.is_empty() {
            where_parts.push("student_code = ?".to_string());
            params_vec.push(Box::new(safe_student));
        }
        let safe_curriculum = normalize(curriculum, 120);
        if !safe_curriculum.is_empty() {
            where_parts.push("curriculum = ?".to_string());
            params_vec.push(Box::new(safe_curriculum));
        }
        self.query_math_payloads("math_daily_attempts", "payload_json", where_parts, params_vec, "date_key DESC, student_code ASC", 5000)
    }

    fn list_math_daily_student_profiles(&self, tenant_id: String, student_code: String) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut where_parts = vec!["tenant_id = ?".to_string()];
        let mut params_vec: Vec<Box<dyn ToSql>> = vec![Box::new(safe_tenant)];
        let safe_student = normalize_student_code(Some(&Value::String(student_code)));
        if !safe_student.is_empty() {
            where_parts.push("student_code = ?".to_string());
            params_vec.push(Box::new(safe_student));
        }
        self.query_math_payloads("math_daily_student_profiles", "payload_json", where_parts, params_vec, "student_code ASC", 1000)
    }

    fn list_math_daily_simple_range(&self, table: &str, tenant_id: String, date_from: String, date_to: String, student_code: String, curriculum: String, order: &str) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut where_parts = vec!["tenant_id = ?".to_string()];
        let mut params_vec: Vec<Box<dyn ToSql>> = vec![Box::new(safe_tenant)];
        let from = normalize_date_key(Some(&Value::String(date_from)));
        let to = normalize_date_key(Some(&Value::String(date_to)));
        if !from.is_empty() {
            where_parts.push("date_key >= ?".to_string());
            params_vec.push(Box::new(from));
        }
        if !to.is_empty() {
            where_parts.push("date_key <= ?".to_string());
            params_vec.push(Box::new(to));
        }
        let safe_student = normalize_student_code(Some(&Value::String(student_code)));
        if !safe_student.is_empty() {
            where_parts.push("student_code = ?".to_string());
            params_vec.push(Box::new(safe_student));
        }
        let safe_curriculum = normalize(curriculum, 120);
        if !safe_curriculum.is_empty() {
            where_parts.push("curriculum = ?".to_string());
            params_vec.push(Box::new(safe_curriculum));
        }
        self.query_math_payloads(table, "payload_json", where_parts, params_vec, order, 5000)
    }

    fn list_math_daily_assignments(&self, tenant_id: String, date_from: String, date_to: String, curriculum: String) -> Result<Vec<Value>, String> {
        self.list_math_daily_simple_range("math_daily_assignments", tenant_id, date_from, date_to, String::new(), curriculum, "date_key DESC, assignment_id ASC")
    }

    fn list_math_daily_assignment_results(&self, tenant_id: String, assignment_id: String, student_code: String, date_from: String, date_to: String, curriculum: String) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut where_parts = vec!["tenant_id = ?".to_string()];
        let mut params_vec: Vec<Box<dyn ToSql>> = vec![Box::new(safe_tenant)];
        let safe_assignment = normalize(assignment_id, 240).replace(['/', '\\'], "_");
        if !safe_assignment.is_empty() {
            where_parts.push("assignment_id = ?".to_string());
            params_vec.push(Box::new(safe_assignment));
        }
        let safe_student = normalize_student_code(Some(&Value::String(student_code)));
        if !safe_student.is_empty() {
            where_parts.push("student_code = ?".to_string());
            params_vec.push(Box::new(safe_student));
        }
        let from = normalize_date_key(Some(&Value::String(date_from)));
        let to = normalize_date_key(Some(&Value::String(date_to)));
        if !from.is_empty() {
            where_parts.push("date_key >= ?".to_string());
            params_vec.push(Box::new(from));
        }
        if !to.is_empty() {
            where_parts.push("date_key <= ?".to_string());
            params_vec.push(Box::new(to));
        }
        let safe_curriculum = normalize(curriculum, 120);
        if !safe_curriculum.is_empty() {
            where_parts.push("curriculum = ?".to_string());
            params_vec.push(Box::new(safe_curriculum));
        }
        self.query_math_payloads("math_daily_assignment_results", "payload_json", where_parts, params_vec, "date_key DESC, assignment_id ASC, student_code ASC", 5000)
    }

    fn get_math_daily_cache(&self, input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let cache_key = math_daily_cache_key(&input);
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        match conn.query_row(
            "SELECT payload_json FROM math_daily_cache_runs WHERE tenant_id = ?1 AND cache_key = ?2",
            params![tenant_id, cache_key],
            |row| row.get::<_, String>(0),
        ) {
            Ok(payload_json) => {
                let cache = serde_json::from_str::<Value>(&payload_json).unwrap_or_else(|_| json!({}));
                Ok(json!({ "ok": true, "cached": true, "cache": cache }))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(json!({ "ok": true, "cached": false, "cache": null })),
            Err(error) => Err(format!("db_math_cache_lookup_failed:{error}")),
        }
    }

    fn get_math_daily_cache_status(&self, tenant_id: String) -> Result<Value, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        match conn.query_row(
            "SELECT cache_key, action, date_from, date_to, date_key, curriculum, updated_at_ms FROM math_daily_cache_runs WHERE tenant_id = ?1 ORDER BY updated_at_ms DESC LIMIT 1",
            params![safe_tenant.clone()],
            |row| {
                let tenant_for_row = safe_tenant.clone();
                Ok(json!({
                    "ok": true,
                    "tenantId": tenant_for_row,
                    "cached": true,
                    "cacheKey": row.get::<_, String>(0)?,
                    "action": row.get::<_, String>(1)?,
                    "dateFrom": row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    "dateTo": row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    "dateKey": row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    "curriculum": row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    "updatedAtMs": row.get::<_, i64>(6)?
                }))
            },
        ) {
            Ok(value) => Ok(value),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(json!({ "ok": true, "tenantId": safe_tenant, "cached": false })),
            Err(error) => Err(format!("db_math_cache_status_failed:{error}")),
        }
    }

    fn upsert_board_snapshot(&self, mut input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        let board_id = normalize_id_segment(input.get("boardId"), 180);
        let post_id = normalize_id_segment(input.get("postId").or_else(|| input.get("id")).or_else(|| input.get("docId")), 180);
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        if board_id.is_empty() || post_id.is_empty() {
            return Err("board_snapshot_identity_required".to_string());
        }
        let updated_at_ms = input
            .get("updatedAtMs")
            .and_then(|v| v.as_i64())
            .unwrap_or_else(|| Utc::now().timestamp_millis());
        let archived_at_ms = Utc::now().timestamp_millis();
        if let Value::Object(ref mut obj) = input {
            obj.insert("tenantId".to_string(), Value::String(tenant_id.clone()));
            obj.insert("boardId".to_string(), Value::String(board_id.clone()));
            obj.insert("postId".to_string(), Value::String(post_id.clone()));
            obj.insert("id".to_string(), Value::String(post_id.clone()));
            obj.insert("docId".to_string(), Value::String(post_id.clone()));
            obj.insert("updatedAtMs".to_string(), Value::Number(updated_at_ms.into()));
            obj.insert("archivedAtMs".to_string(), Value::Number(archived_at_ms.into()));
        }
        let payload_json = serde_json::to_string(&input).map_err(|e| format!("payload_encode_failed:{e}"))?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT INTO board_post_snapshots
             (tenant_id, board_id, post_id, payload_json, updated_at_ms, archived_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(tenant_id, board_id, post_id) DO UPDATE SET
               payload_json = excluded.payload_json,
               updated_at_ms = excluded.updated_at_ms,
               archived_at_ms = excluded.archived_at_ms",
            params![tenant_id, board_id, post_id, payload_json, updated_at_ms, archived_at_ms],
        )
        .map_err(|e| format!("db_board_snapshot_upsert_failed:{e}"))?;
        Ok(input)
    }

    fn list_board_snapshots(&self, tenant_id: String, board_id: String, post_id: String) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        let safe_board = normalize(&board_id, 180).replace(['/', '\\'], "_");
        let safe_post = normalize(&post_id, 180).replace(['/', '\\'], "_");
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut where_parts = vec!["tenant_id = ?".to_string()];
        let mut params_vec: Vec<Box<dyn ToSql>> = vec![Box::new(safe_tenant)];
        if !safe_board.is_empty() {
            where_parts.push("board_id = ?".to_string());
            params_vec.push(Box::new(safe_board));
        }
        if !safe_post.is_empty() {
            where_parts.push("post_id = ?".to_string());
            params_vec.push(Box::new(safe_post));
        }
        let sql = format!(
            "SELECT payload_json FROM board_post_snapshots WHERE {} ORDER BY updated_at_ms DESC LIMIT 500",
            where_parts.join(" AND ")
        );
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let mut stmt = conn.prepare(&sql).map_err(|e| format!("db_board_snapshot_query_prepare_failed:{e}"))?;
        let params_ref: Vec<&dyn ToSql> = params_vec.iter().map(|v| v.as_ref() as &dyn ToSql).collect();
        let rows = stmt
            .query_map(params_from_iter(params_ref), |row| {
                let payload_json: String = row.get(0)?;
                Ok(serde_json::from_str(&payload_json).unwrap_or_else(|_| json!({})))
            })
            .map_err(|e| format!("db_board_snapshot_query_failed:{e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("db_board_snapshot_row_failed:{e}"))?);
        }
        Ok(out)
    }

    fn upsert_board_media(&self, input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        let board_id = normalize_id_segment(input.get("boardId"), 180);
        let post_id = normalize_id_segment(input.get("postId").or_else(|| input.get("id")).or_else(|| input.get("docId")), 180);
        let media_id = normalize_id_segment(input.get("mediaId").or_else(|| input.get("storagePath")).or_else(|| input.get("sourceUrl")), 220);
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        if board_id.is_empty() || post_id.is_empty() || media_id.is_empty() {
            return Err("media_identity_required".to_string());
        }
        let data_base64 = normalize_json_text(input.get("dataBase64").or_else(|| input.get("base64")), 0);
        if data_base64.is_empty() {
            return Err("media_data_required".to_string());
        }
        let bytes = BASE64_STANDARD
            .decode(data_base64.as_bytes())
            .map_err(|e| format!("media_decode_failed:{e}"))?;
        let content_type = normalize_json_text(input.get("contentType").or_else(|| input.get("type")), 120);
        let file_name = normalize_json_text(input.get("fileName").or_else(|| input.get("name")), 180);
        let storage_path = normalize_json_text(input.get("storagePath").or_else(|| input.get("path")), 600);
        let source_url = normalize_json_text(input.get("sourceUrl").or_else(|| input.get("url")).or_else(|| input.get("downloadURL")), 1200);
        let expires_at_ms = input.get("expiresAtMs").and_then(|v| v.as_i64()).unwrap_or(0);
        let archived_at_ms = Utc::now().timestamp_millis();
        let extension = file_name.rsplit('.').next().filter(|s| s.len() <= 8).unwrap_or("bin");
        let relative_path = PathBuf::from("board-media")
            .join(&tenant_id)
            .join(&board_id)
            .join(format!("{media_id}.{extension}"));
        let absolute_path = self.data_dir.join(&relative_path);
        if let Some(parent) = absolute_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("media_dir_create_failed:{e}"))?;
        }
        fs::write(&absolute_path, &bytes).map_err(|e| format!("media_write_failed:{e}"))?;
        let payload = json!({
            "tenantId": tenant_id,
            "boardId": board_id,
            "postId": post_id,
            "mediaId": media_id,
            "storagePath": storage_path,
            "sourceUrl": source_url,
            "contentType": if content_type.is_empty() { "application/octet-stream" } else { content_type.as_str() },
            "fileName": if file_name.is_empty() { format!("{media_id}.{extension}") } else { file_name.clone() },
            "size": bytes.len() as i64,
            "expiresAtMs": expires_at_ms,
            "archivedAtMs": archived_at_ms
        });
        let payload_json = serde_json::to_string(&payload).map_err(|e| format!("payload_encode_failed:{e}"))?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT INTO board_media_files
             (tenant_id, board_id, post_id, media_id, storage_path, local_path, content_type, file_name, size, expires_at_ms, archived_at_ms, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(tenant_id, media_id) DO UPDATE SET
               board_id = excluded.board_id,
               post_id = excluded.post_id,
               storage_path = excluded.storage_path,
               local_path = excluded.local_path,
               content_type = excluded.content_type,
               file_name = excluded.file_name,
               size = excluded.size,
               expires_at_ms = excluded.expires_at_ms,
               archived_at_ms = excluded.archived_at_ms,
               payload_json = excluded.payload_json",
            params![
                payload["tenantId"].as_str().unwrap_or_default(),
                payload["boardId"].as_str().unwrap_or_default(),
                payload["postId"].as_str().unwrap_or_default(),
                payload["mediaId"].as_str().unwrap_or_default(),
                payload["storagePath"].as_str().unwrap_or_default(),
                relative_path.to_string_lossy().to_string(),
                payload["contentType"].as_str().unwrap_or("application/octet-stream"),
                payload["fileName"].as_str().unwrap_or("board-media.bin"),
                bytes.len() as i64,
                expires_at_ms,
                archived_at_ms,
                payload_json
            ],
        )
        .map_err(|e| format!("db_board_media_upsert_failed:{e}"))?;
        Ok(payload)
    }

    fn list_board_media(&self, tenant_id: String, board_id: String, post_id: String) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        let safe_board = normalize(&board_id, 180).replace(['/', '\\'], "_");
        let safe_post = normalize(&post_id, 180).replace(['/', '\\'], "_");
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut where_parts = vec!["tenant_id = ?".to_string()];
        let mut params_vec: Vec<Box<dyn ToSql>> = vec![Box::new(safe_tenant)];
        if !safe_board.is_empty() {
            where_parts.push("board_id = ?".to_string());
            params_vec.push(Box::new(safe_board));
        }
        if !safe_post.is_empty() {
            where_parts.push("post_id = ?".to_string());
            params_vec.push(Box::new(safe_post));
        }
        let sql = format!("SELECT payload_json FROM board_media_files WHERE {} ORDER BY archived_at_ms DESC LIMIT 1000", where_parts.join(" AND "));
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let mut stmt = conn.prepare(&sql).map_err(|e| format!("db_board_media_query_prepare_failed:{e}"))?;
        let params_ref: Vec<&dyn ToSql> = params_vec.iter().map(|v| v.as_ref() as &dyn ToSql).collect();
        let rows = stmt
            .query_map(params_from_iter(params_ref), |row| {
                let payload_json: String = row.get(0)?;
                Ok(serde_json::from_str(&payload_json).unwrap_or_else(|_| json!({})))
            })
            .map_err(|e| format!("db_board_media_query_failed:{e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("db_board_media_row_failed:{e}"))?);
        }
        Ok(out)
    }

    fn get_board_media_file(&self, tenant_id: String, media_id: String) -> Result<Value, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        let safe_media = normalize(&media_id, 220).replace(['/', '\\'], "_");
        if safe_tenant.is_empty() || safe_media.is_empty() {
            return Err("media_identity_required".to_string());
        }
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let row = conn
            .query_row(
                "SELECT local_path, content_type, file_name, payload_json FROM board_media_files WHERE tenant_id = ?1 AND media_id = ?2",
                params![safe_tenant, safe_media],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => "media_not_found".to_string(),
                other => format!("db_board_media_lookup_failed:{other}"),
            })?;
        let path = self.data_dir.join(row.0);
        let bytes = fs::read(&path).map_err(|_| "media_file_missing".to_string())?;
        Ok(json!({
            "ok": true,
            "contentType": row.1,
            "fileName": row.2,
            "dataBase64": BASE64_STANDARD.encode(bytes),
            "meta": serde_json::from_str::<Value>(&row.3).unwrap_or_else(|_| json!({}))
        }))
    }

    fn upsert_attendance_record(&self, mut input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        let date_key = normalize_date_key(input.get("dateKey").or_else(|| input.get("date")));
        let student_code = normalize_student_code(input.get("studentCode").or_else(|| input.get("code")));
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        if date_key.is_empty() {
            return Err("date_required".to_string());
        }
        if student_code.is_empty() {
            return Err("student_code_required".to_string());
        }
        let record_id = normalize_local_record_id(
            input.get("recordId").or_else(|| input.get("id")).or_else(|| input.get("docId")),
            format!("{date_key}_{student_code}"),
            "attendance_record_id_required",
        )?;
        let updated_at_ms = updated_at_ms(&input);
        if let Value::Object(ref mut obj) = input {
            set_obj(obj, "tenantId", tenant_id.clone());
            set_obj(obj, "id", record_id.clone());
            set_obj(obj, "docId", record_id.clone());
            set_obj(obj, "recordId", record_id.clone());
            set_obj(obj, "dateKey", date_key.clone());
            set_obj(obj, "date", date_key.clone());
            set_obj(obj, "studentCode", student_code.clone());
            set_updated_payload_fields(obj, updated_at_ms);
        }
        let payload_json = payload_json(&input, "attendance_record_encode_failed")?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT INTO attendance_records
             (tenant_id, record_id, date_key, student_code, payload_json, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(tenant_id, record_id) DO UPDATE SET
               date_key = excluded.date_key,
               student_code = excluded.student_code,
               payload_json = excluded.payload_json,
               updated_at_ms = excluded.updated_at_ms",
            params![tenant_id, record_id, date_key, student_code, payload_json, updated_at_ms],
        )
        .map_err(|e| format!("db_attendance_record_upsert_failed:{e}"))?;
        Ok(input)
    }

    fn import_attendance_records(&self, tenant_id: String, records: Vec<Value>) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut saved = Vec::new();
        for mut record in records {
            if let Value::Object(ref mut obj) = record {
                obj.insert("tenantId".to_string(), Value::String(safe_tenant.clone()));
            }
            saved.push(self.upsert_attendance_record(record)?);
        }
        Ok(saved)
    }

    fn list_attendance_records(&self, tenant_id: String, date: String, date_from: String, date_to: String, student_code: String, limit: i64) -> Result<Vec<Value>, String> {
        self.list_date_student_payloads(
            "attendance_records",
            tenant_id,
            date,
            date_from,
            date_to,
            student_code,
            "date_key DESC, student_code ASC",
            limit,
        )
    }

    fn upsert_attendance_nais_check(&self, mut input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let date_key = normalize_date_key(input.get("dateKey").or_else(|| input.get("date")));
        let student_code = normalize_student_code(input.get("studentCode").or_else(|| input.get("code")));
        let fallback = vec![
            date_key.clone(),
            student_code.clone(),
            normalize_json_text(input.get("status"), 40),
            normalize_json_text(input.get("neisReasonType"), 40),
            normalize_json_text(input.get("neisPeriod"), 20),
        ]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<String>>()
        .join("__");
        let check_id = normalize_local_record_id(
            input.get("checkId").or_else(|| input.get("checkKey")).or_else(|| input.get("id")).or_else(|| input.get("docId")),
            fallback,
            "attendance_check_id_required",
        )?;
        let updated_at_ms = updated_at_ms(&input);
        if let Value::Object(ref mut obj) = input {
            set_obj(obj, "tenantId", tenant_id.clone());
            set_obj(obj, "id", check_id.clone());
            set_obj(obj, "docId", check_id.clone());
            set_obj(obj, "checkId", check_id.clone());
            if normalize_json_text(obj.get("checkKey"), 260).is_empty() {
                set_obj(obj, "checkKey", check_id.clone());
            }
            set_obj(obj, "dateKey", date_key.clone());
            set_obj(obj, "date", date_key.clone());
            set_obj(obj, "studentCode", student_code.clone());
            set_updated_payload_fields(obj, updated_at_ms);
        }
        let payload_json = payload_json(&input, "attendance_nais_check_encode_failed")?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT INTO attendance_nais_checks
             (tenant_id, check_id, date_key, student_code, payload_json, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(tenant_id, check_id) DO UPDATE SET
               date_key = excluded.date_key,
               student_code = excluded.student_code,
               payload_json = excluded.payload_json,
               updated_at_ms = excluded.updated_at_ms",
            params![tenant_id, check_id, date_key, student_code, payload_json, updated_at_ms],
        )
        .map_err(|e| format!("db_attendance_nais_check_upsert_failed:{e}"))?;
        Ok(input)
    }

    fn import_attendance_nais_checks(&self, tenant_id: String, records: Vec<Value>) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut saved = Vec::new();
        for mut record in records {
            if let Value::Object(ref mut obj) = record {
                obj.insert("tenantId".to_string(), Value::String(safe_tenant.clone()));
            }
            saved.push(self.upsert_attendance_nais_check(record)?);
        }
        Ok(saved)
    }

    fn list_attendance_nais_checks(&self, tenant_id: String, date: String, date_from: String, date_to: String, student_code: String, limit: i64) -> Result<Vec<Value>, String> {
        self.list_date_student_payloads(
            "attendance_nais_checks",
            tenant_id,
            date,
            date_from,
            date_to,
            student_code,
            "date_key DESC, student_code ASC, check_id ASC",
            limit,
        )
    }

    fn upsert_attendance_document_request(&self, mut input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let date_key = normalize_date_key(input.get("dateKey").or_else(|| input.get("date")).or_else(|| input.get("dueDate")));
        let student_code = normalize_student_code(input.get("studentCode").or_else(|| input.get("code")));
        let fallback = vec![
            normalize_json_text(input.get("taskId"), 260),
            date_key.clone(),
            student_code.clone(),
            normalize_json_text(input.get("attendanceDocumentKind").or_else(|| input.get("documentRequestKind")), 80),
        ]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<String>>()
        .join("__");
        let request_id = normalize_local_record_id(
            input.get("requestId")
                .or_else(|| input.get("taskId"))
                .or_else(|| input.get("statusId"))
                .or_else(|| input.get("id"))
                .or_else(|| input.get("docId")),
            fallback,
            "attendance_request_id_required",
        )?;
        let updated_at_ms = updated_at_ms(&input);
        if let Value::Object(ref mut obj) = input {
            set_obj(obj, "tenantId", tenant_id.clone());
            set_obj(obj, "id", request_id.clone());
            set_obj(obj, "docId", request_id.clone());
            set_obj(obj, "requestId", request_id.clone());
            set_obj(obj, "dateKey", date_key.clone());
            set_obj(obj, "date", date_key.clone());
            set_obj(obj, "studentCode", student_code.clone());
            set_updated_payload_fields(obj, updated_at_ms);
        }
        let payload_json = payload_json(&input, "attendance_document_request_encode_failed")?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT INTO attendance_document_requests
             (tenant_id, request_id, date_key, student_code, payload_json, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(tenant_id, request_id) DO UPDATE SET
               date_key = excluded.date_key,
               student_code = excluded.student_code,
               payload_json = excluded.payload_json,
               updated_at_ms = excluded.updated_at_ms",
            params![tenant_id, request_id, date_key, student_code, payload_json, updated_at_ms],
        )
        .map_err(|e| format!("db_attendance_document_request_upsert_failed:{e}"))?;
        Ok(input)
    }

    fn import_attendance_document_requests(&self, tenant_id: String, records: Vec<Value>) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut saved = Vec::new();
        for mut record in records {
            if let Value::Object(ref mut obj) = record {
                obj.insert("tenantId".to_string(), Value::String(safe_tenant.clone()));
            }
            saved.push(self.upsert_attendance_document_request(record)?);
        }
        Ok(saved)
    }

    fn list_attendance_document_requests(&self, tenant_id: String, date: String, date_from: String, date_to: String, student_code: String, limit: i64) -> Result<Vec<Value>, String> {
        self.list_date_student_payloads(
            "attendance_document_requests",
            tenant_id,
            date,
            date_from,
            date_to,
            student_code,
            "date_key DESC, student_code ASC, request_id ASC",
            limit,
        )
    }

    fn upsert_counseling_record(&self, input: Value) -> Result<Value, String> {
        let record = normalize_counseling_record(input)?;
        let tenant_id = normalize_tenant_id(record.get("tenantId"));
        let request_id = normalize_id_segment(record.get("requestId"), 260);
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        write_counseling_record(&conn, &record)?;
        let mut records = query_counseling_records(
            &conn,
            &tenant_id,
            "",
            "",
            &request_id,
            1,
        )?;
        records.pop().ok_or_else(|| "counseling_record_not_found".to_string())
    }

    fn list_counseling_records(
        &self,
        tenant_id: String,
        status: String,
        student_code: String,
        limit: i64,
    ) -> Result<Vec<Value>, String> {
        let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let status = normalize(status, 40).to_lowercase();
        if !status.is_empty() && !COUNSELING_STATUSES.contains(&status.as_str()) {
            return Err("counseling_status_invalid".to_string());
        }
        let student_code = normalize_student_code(Some(&Value::String(student_code)));
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        query_counseling_records(&conn, &tenant_id, &status, &student_code, "", limit)
    }

    fn get_counseling_record(
        &self,
        tenant_id: String,
        request_id: String,
    ) -> Result<Option<Value>, String> {
        let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
        let request_id = normalize_id_segment(Some(&Value::String(request_id)), 260);
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        if request_id.is_empty() {
            return Err("counseling_request_id_required".to_string());
        }
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        Ok(query_counseling_records(
            &conn,
            &tenant_id,
            "",
            "",
            &request_id,
            1,
        )?
        .pop())
    }

    fn upsert_counseling_teacher_note(&self, input: Value) -> Result<Value, String> {
        let note = normalize_counseling_teacher_note(input)?;
        let tenant_id = normalize_tenant_id(note.get("tenantId"));
        let request_id = normalize_id_segment(note.get("requestId"), 260);
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        write_counseling_teacher_note(&conn, &note)?;
        query_counseling_teacher_notes(&conn, &tenant_id, &request_id, 1)?
            .pop()
            .ok_or_else(|| "counseling_teacher_note_not_found".to_string())
    }

    fn list_counseling_teacher_notes(
        &self,
        tenant_id: String,
        request_id: String,
        limit: i64,
    ) -> Result<Vec<Value>, String> {
        let tenant_id = normalize_tenant_id(Some(&Value::String(tenant_id)));
        let request_id = normalize_id_segment(Some(&Value::String(request_id)), 260);
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        query_counseling_teacher_notes(&conn, &tenant_id, &request_id, limit)
    }

    fn get_counseling_teacher_note(
        &self,
        tenant_id: String,
        request_id: String,
    ) -> Result<Option<Value>, String> {
        Ok(self
            .list_counseling_teacher_notes(tenant_id, request_id, 1)?
            .pop())
    }

    fn compare_counseling_snapshot(&self, input: Value) -> Result<Value, String> {
        let source = prepare_counseling_snapshot(&input)?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        compare_prepared_counseling_snapshot(&conn, &source)
    }

    fn import_counseling_snapshot(&self, input: Value) -> Result<Value, String> {
        let source = prepare_counseling_snapshot(&input)?;
        let run_id = normalize_id_segment(
            input.get("runId").or_else(|| input.get("id")),
            260,
        );
        if run_id.is_empty() {
            return Err("import_run_id_required".to_string());
        }
        let expected_sha256 = normalize_json_text(input.get("sourceSnapshotSha256"), 64).to_lowercase();
        if expected_sha256.len() != 64
            || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            || expected_sha256 != source.source_snapshot_sha256
        {
            return Err("counseling_source_hash_mismatch".to_string());
        }
        let mut conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let transaction = conn
            .transaction()
            .map_err(|error| format!("db_counseling_import_begin_failed:{error}"))?;
        let existing = transaction.query_row(
            "SELECT kind, payload_json FROM local_import_runs WHERE tenant_id = ?1 AND run_id = ?2",
            params![&source.tenant_id, &run_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        );
        if let Ok((kind, payload_json)) = existing {
            let receipt = serde_json::from_str::<Value>(&payload_json).unwrap_or_else(|_| json!({}));
            if kind != "counseling_firestore_copy"
                || normalize_json_text(receipt.get("sourceSnapshotSha256"), 64)
                    != source.source_snapshot_sha256
            {
                return Err("counseling_import_run_conflict".to_string());
            }
            let mut result = receipt.get("result").cloned().unwrap_or_else(|| json!({}));
            if let Value::Object(ref mut obj) = result {
                set_obj(obj, "replayed", true);
            }
            transaction
                .commit()
                .map_err(|error| format!("db_counseling_import_commit_failed:{error}"))?;
            return Ok(result);
        }
        for record in &source.records {
            write_counseling_record(&transaction, record)?;
        }
        for note in &source.teacher_notes {
            write_counseling_teacher_note(&transaction, note)?;
        }
        let compare = compare_prepared_counseling_snapshot(&transaction, &source)?;
        let started_at_ms = now_ms();
        let finished_at_ms = now_ms();
        let result = json!({
            "ok": true,
            "tenantId": source.tenant_id,
            "runId": run_id,
            "sourceSnapshotSha256": source.source_snapshot_sha256,
            "imported": {
                "records": source.records.len(),
                "teacherNotes": source.teacher_notes.len()
            },
            "compare": compare,
            "replayed": false
        });
        let status = if result
            .get("compare")
            .and_then(|value| value.get("matches"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "completed"
        } else {
            "mismatch"
        };
        let receipt = json!({
            "id": run_id,
            "runId": run_id,
            "tenantId": source.tenant_id,
            "kind": "counseling_firestore_copy",
            "status": status,
            "sourceRetention": "copy_only",
            "sourceSnapshotSha256": source.source_snapshot_sha256,
            "startedAtMs": started_at_ms,
            "finishedAtMs": finished_at_ms,
            "result": result
        });
        transaction
            .execute(
                "INSERT INTO local_import_runs
                 (tenant_id, run_id, kind, status, payload_json, started_at_ms, finished_at_ms)
                 VALUES (?1, ?2, 'counseling_firestore_copy', ?3, ?4, ?5, ?6)",
                params![
                    &source.tenant_id,
                    &run_id,
                    status,
                    payload_json(&receipt, "counseling_import_receipt_encode_failed")?,
                    started_at_ms,
                    finished_at_ms
                ],
            )
            .map_err(|error| format!("db_counseling_import_receipt_failed:{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("db_counseling_import_commit_failed:{error}"))?;
        Ok(result)
    }

    fn upsert_eval_assignment(&self, mut input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let shared_plan_id = normalize_id_segment(input.get("sharedPlanId").or_else(|| input.get("planId")), 260);
        let assignment_id = normalize_local_record_id(
            input.get("assignmentId").or_else(|| input.get("id")).or_else(|| input.get("docId")),
            if shared_plan_id.is_empty() { String::new() } else { format!("{tenant_id}__{shared_plan_id}") },
            "eval_assignment_id_required",
        )?;
        let scheduled_date = normalize_date_key(input.get("scheduledDate").or_else(|| input.get("linkedDateKey")).or_else(|| input.get("dateKey")).or_else(|| input.get("date")));
        let updated_at_ms = updated_at_ms(&input);
        if let Value::Object(ref mut obj) = input {
            set_obj(obj, "tenantId", tenant_id.clone());
            set_obj(obj, "id", assignment_id.clone());
            set_obj(obj, "docId", assignment_id.clone());
            set_obj(obj, "assignmentId", assignment_id.clone());
            set_obj(obj, "sharedPlanId", shared_plan_id.clone());
            set_obj(obj, "scheduledDate", scheduled_date.clone());
            if normalize_json_text(obj.get("linkedDateKey"), 10).is_empty() {
                set_obj(obj, "linkedDateKey", scheduled_date.clone());
            }
            set_updated_payload_fields(obj, updated_at_ms);
        }
        let payload_json = payload_json(&input, "eval_assignment_encode_failed")?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT INTO eval_assignments
             (tenant_id, assignment_id, shared_plan_id, scheduled_date, payload_json, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(tenant_id, assignment_id) DO UPDATE SET
               shared_plan_id = excluded.shared_plan_id,
               scheduled_date = excluded.scheduled_date,
               payload_json = excluded.payload_json,
               updated_at_ms = excluded.updated_at_ms",
            params![tenant_id, assignment_id, shared_plan_id, scheduled_date, payload_json, updated_at_ms],
        )
        .map_err(|e| format!("db_eval_assignment_upsert_failed:{e}"))?;
        Ok(input)
    }

    fn import_eval_assignments(&self, tenant_id: String, records: Vec<Value>) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut saved = Vec::new();
        for mut record in records {
            if let Value::Object(ref mut obj) = record {
                obj.insert("tenantId".to_string(), Value::String(safe_tenant.clone()));
            }
            saved.push(self.upsert_eval_assignment(record)?);
        }
        Ok(saved)
    }

    fn list_eval_assignments(&self, tenant_id: String, assignment_id: String, shared_plan_id: String, scheduled_date: String, limit: i64) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut where_parts = vec!["tenant_id = ?".to_string()];
        let mut params_vec: Vec<Box<dyn ToSql>> = vec![Box::new(safe_tenant)];
        let safe_assignment = normalize(assignment_id, 260).replace(['/', '\\'], "_");
        if !safe_assignment.is_empty() {
            where_parts.push("assignment_id = ?".to_string());
            params_vec.push(Box::new(safe_assignment));
        }
        let safe_plan = normalize(shared_plan_id, 260).replace(['/', '\\'], "_");
        if !safe_plan.is_empty() {
            where_parts.push("shared_plan_id = ?".to_string());
            params_vec.push(Box::new(safe_plan));
        }
        let safe_date = normalize_date_key(Some(&Value::String(scheduled_date)));
        if !safe_date.is_empty() {
            where_parts.push("scheduled_date = ?".to_string());
            params_vec.push(Box::new(safe_date));
        }
        self.query_math_payloads("eval_assignments", "payload_json", where_parts, params_vec, "scheduled_date DESC, assignment_id ASC", limit.clamp(1, 5000))
    }

    fn upsert_eval_result(&self, mut input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        let assignment_id = normalize_id_segment(input.get("assignmentId").or_else(|| input.get("assignment").and_then(|assignment| assignment.get("id"))), 260);
        let student_id = normalize_id_segment(input.get("studentId").or_else(|| input.get("studentCode")).or_else(|| input.get("code")), 160);
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        if assignment_id.is_empty() {
            return Err("eval_assignment_id_required".to_string());
        }
        if student_id.is_empty() {
            return Err("student_id_required".to_string());
        }
        let result_id = normalize_local_record_id(
            input.get("resultId").or_else(|| input.get("id")).or_else(|| input.get("docId")),
            format!("{assignment_id}__{student_id}"),
            "eval_result_id_required",
        )?;
        let date_key = normalize_date_key(
            input.get("dateKey")
                .or_else(|| input.get("recordedDate"))
                .or_else(|| input.get("scheduledDate"))
                .or_else(|| input.get("assignment").and_then(|assignment| assignment.get("scheduledDate")))
                .or_else(|| input.get("assignment").and_then(|assignment| assignment.get("linkedDateKey"))),
        );
        let updated_at_ms = updated_at_ms(&input);
        if let Value::Object(ref mut obj) = input {
            set_obj(obj, "tenantId", tenant_id.clone());
            set_obj(obj, "id", result_id.clone());
            set_obj(obj, "docId", result_id.clone());
            set_obj(obj, "resultId", result_id.clone());
            set_obj(obj, "assignmentId", assignment_id.clone());
            set_obj(obj, "studentId", student_id.clone());
            if normalize_json_text(obj.get("studentCode"), 160).is_empty() {
                set_obj(obj, "studentCode", student_id.clone());
            }
            set_obj(obj, "dateKey", date_key.clone());
            set_updated_payload_fields(obj, updated_at_ms);
        }
        let payload_json = payload_json(&input, "eval_result_encode_failed")?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT INTO eval_results
             (tenant_id, result_id, assignment_id, student_id, date_key, payload_json, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(tenant_id, result_id) DO UPDATE SET
               assignment_id = excluded.assignment_id,
               student_id = excluded.student_id,
               date_key = excluded.date_key,
               payload_json = excluded.payload_json,
               updated_at_ms = excluded.updated_at_ms",
            params![tenant_id, result_id, assignment_id, student_id, date_key, payload_json, updated_at_ms],
        )
        .map_err(|e| format!("db_eval_result_upsert_failed:{e}"))?;
        Ok(input)
    }

    fn import_eval_results(&self, tenant_id: String, records: Vec<Value>) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut saved = Vec::new();
        for mut record in records {
            if let Value::Object(ref mut obj) = record {
                obj.insert("tenantId".to_string(), Value::String(safe_tenant.clone()));
            }
            saved.push(self.upsert_eval_result(record)?);
        }
        Ok(saved)
    }

    fn list_eval_results(&self, tenant_id: String, result_id: String, assignment_id: String, student_id: String, date_key: String, limit: i64) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut where_parts = vec!["tenant_id = ?".to_string()];
        let mut params_vec: Vec<Box<dyn ToSql>> = vec![Box::new(safe_tenant)];
        let safe_result = normalize(result_id, 260).replace(['/', '\\'], "_");
        if !safe_result.is_empty() {
            where_parts.push("result_id = ?".to_string());
            params_vec.push(Box::new(safe_result));
        }
        let safe_assignment = normalize(assignment_id, 260).replace(['/', '\\'], "_");
        if !safe_assignment.is_empty() {
            where_parts.push("assignment_id = ?".to_string());
            params_vec.push(Box::new(safe_assignment));
        }
        let safe_student = normalize(student_id, 160).replace(['/', '\\'], "_");
        if !safe_student.is_empty() {
            where_parts.push("student_id = ?".to_string());
            params_vec.push(Box::new(safe_student));
        }
        let safe_date = normalize_date_key(Some(&Value::String(date_key)));
        if !safe_date.is_empty() {
            where_parts.push("date_key = ?".to_string());
            params_vec.push(Box::new(safe_date));
        }
        self.query_math_payloads("eval_results", "payload_json", where_parts, params_vec, "date_key DESC, assignment_id ASC, student_id ASC", limit.clamp(1, 10000))
    }

    fn upsert_student_record_draft_set(&self, mut input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let fallback = vec![
            normalize_json_text(
                input
                    .get("generatedAtMs")
                    .or_else(|| input.get("createdAtMs"))
                    .or_else(|| input.get("createdAt")),
                80,
            ),
            normalize_json_text(input.get("fromDate").or_else(|| input.get("dateFrom")), 10),
            normalize_json_text(input.get("toDate").or_else(|| input.get("dateTo")), 10),
        ]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<String>>()
        .join("__");
        let draft_set_id = normalize_local_record_id(
            input.get("draftSetId").or_else(|| input.get("id")).or_else(|| input.get("docId")),
            if fallback.is_empty() { now_ms().to_string() } else { fallback },
            "student_record_draft_set_id_required",
        )?;
        let status = {
            let value = normalize_json_text(input.get("status"), 40);
            if value.is_empty() { "ready".to_string() } else { value }
        };
        let from_date = normalize_date_key(input.get("fromDate").or_else(|| input.get("dateFrom")));
        let to_date = normalize_date_key(input.get("toDate").or_else(|| input.get("dateTo")));
        let updated_at_ms = updated_at_ms(&input);
        let parsed_created_at_ms = timestamp_like(
            input
                .get("createdAtMs")
                .or_else(|| input.get("createdAt"))
                .or_else(|| input.get("generatedAtMs"))
                .or_else(|| input.get("generatedAt")),
        );
        let created_at_ms = if parsed_created_at_ms > 0 { parsed_created_at_ms } else { updated_at_ms };
        if let Value::Object(ref mut obj) = input {
            set_obj(obj, "tenantId", tenant_id.clone());
            set_obj(obj, "id", draft_set_id.clone());
            set_obj(obj, "docId", draft_set_id.clone());
            set_obj(obj, "draftSetId", draft_set_id.clone());
            set_obj(obj, "status", status.clone());
            set_obj(obj, "fromDate", from_date.clone());
            set_obj(obj, "toDate", to_date.clone());
            set_obj(obj, "createdAtMs", created_at_ms);
            let created_at_iso = DateTime::<Utc>::from_timestamp_millis(created_at_ms)
                .unwrap_or_else(Utc::now)
                .to_rfc3339();
            set_obj(obj, "createdAtIso", created_at_iso);
            set_updated_payload_fields(obj, updated_at_ms);
        }
        let payload_json = payload_json(&input, "student_record_draft_set_encode_failed")?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT INTO student_record_draft_sets
             (tenant_id, draft_set_id, status, from_date, to_date, payload_json, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(tenant_id, draft_set_id) DO UPDATE SET
               status = excluded.status,
               from_date = excluded.from_date,
               to_date = excluded.to_date,
               payload_json = excluded.payload_json,
               created_at_ms = excluded.created_at_ms,
               updated_at_ms = excluded.updated_at_ms",
            params![tenant_id, draft_set_id, status, from_date, to_date, payload_json, created_at_ms, updated_at_ms],
        )
        .map_err(|e| format!("db_student_record_draft_set_upsert_failed:{e}"))?;
        Ok(input)
    }

    fn import_student_record_draft_sets(&self, tenant_id: String, records: Vec<Value>) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut saved = Vec::new();
        for mut record in records {
            if let Value::Object(ref mut obj) = record {
                obj.insert("tenantId".to_string(), Value::String(safe_tenant.clone()));
            }
            saved.push(self.upsert_student_record_draft_set(record)?);
        }
        Ok(saved)
    }

    fn list_student_record_draft_sets(&self, tenant_id: String, draft_set_id: String, status: String, limit: i64) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut where_parts = vec!["tenant_id = ?".to_string()];
        let mut params_vec: Vec<Box<dyn ToSql>> = vec![Box::new(safe_tenant)];
        let safe_draft_set_id = normalize(draft_set_id, 260).replace(['/', '\\'], "_");
        if !safe_draft_set_id.is_empty() {
            where_parts.push("draft_set_id = ?".to_string());
            params_vec.push(Box::new(safe_draft_set_id));
        }
        let safe_status = normalize(status, 40);
        if !safe_status.is_empty() {
            where_parts.push("status = ?".to_string());
            params_vec.push(Box::new(safe_status));
        }
        self.query_math_payloads(
            "student_record_draft_sets",
            "payload_json",
            where_parts,
            params_vec,
            "updated_at_ms DESC, created_at_ms DESC, draft_set_id ASC",
            limit.clamp(1, 5000),
        )
    }

    fn upsert_student_record_draft(&self, mut input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        let draft_set_id = normalize_id_segment(input.get("draftSetId").or_else(|| input.get("setId")), 260);
        let student_code = normalize_student_code(
            input
                .get("studentCode")
                .or_else(|| input.get("code"))
                .or_else(|| input.get("studentId")),
        );
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        if draft_set_id.is_empty() {
            return Err("student_record_draft_set_id_required".to_string());
        }
        if student_code.is_empty() {
            return Err("student_code_required".to_string());
        }
        let draft_id = normalize_local_record_id(
            input.get("draftId").or_else(|| input.get("id")).or_else(|| input.get("docId")),
            format!("{draft_set_id}__{student_code}"),
            "student_record_draft_id_required",
        )?;
        let class_no = normalize_period(input.get("classNo").or_else(|| input.get("number")));
        let updated_at_ms = updated_at_ms(&input);
        if let Value::Object(ref mut obj) = input {
            set_obj(obj, "tenantId", tenant_id.clone());
            set_obj(obj, "id", draft_id.clone());
            set_obj(obj, "docId", draft_id.clone());
            set_obj(obj, "draftId", draft_id.clone());
            set_obj(obj, "draftSetId", draft_set_id.clone());
            set_obj(obj, "studentCode", student_code.clone());
            set_obj(obj, "classNo", class_no);
            set_updated_payload_fields(obj, updated_at_ms);
        }
        let payload_json = payload_json(&input, "student_record_draft_encode_failed")?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT INTO student_record_drafts
             (tenant_id, draft_id, draft_set_id, student_code, class_no, payload_json, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(tenant_id, draft_id) DO UPDATE SET
               draft_set_id = excluded.draft_set_id,
               student_code = excluded.student_code,
               class_no = excluded.class_no,
               payload_json = excluded.payload_json,
               updated_at_ms = excluded.updated_at_ms",
            params![tenant_id, draft_id, draft_set_id, student_code, class_no, payload_json, updated_at_ms],
        )
        .map_err(|e| format!("db_student_record_draft_upsert_failed:{e}"))?;
        Ok(input)
    }

    fn import_student_record_drafts(&self, tenant_id: String, records: Vec<Value>) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut saved = Vec::new();
        for mut record in records {
            if let Value::Object(ref mut obj) = record {
                obj.insert("tenantId".to_string(), Value::String(safe_tenant.clone()));
            }
            saved.push(self.upsert_student_record_draft(record)?);
        }
        Ok(saved)
    }

    fn list_student_record_drafts(&self, tenant_id: String, draft_id: String, draft_set_id: String, student_code: String, limit: i64) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut where_parts = vec!["tenant_id = ?".to_string()];
        let mut params_vec: Vec<Box<dyn ToSql>> = vec![Box::new(safe_tenant)];
        let safe_draft_id = normalize(draft_id, 260).replace(['/', '\\'], "_");
        if !safe_draft_id.is_empty() {
            where_parts.push("draft_id = ?".to_string());
            params_vec.push(Box::new(safe_draft_id));
        }
        let safe_draft_set_id = normalize(draft_set_id, 260).replace(['/', '\\'], "_");
        if !safe_draft_set_id.is_empty() {
            where_parts.push("draft_set_id = ?".to_string());
            params_vec.push(Box::new(safe_draft_set_id));
        }
        let safe_student_code = normalize(student_code, 80).to_uppercase().replace(['/', '\\'], "_");
        if !safe_student_code.is_empty() {
            where_parts.push("student_code = ?".to_string());
            params_vec.push(Box::new(safe_student_code));
        }
        self.query_math_payloads(
            "student_record_drafts",
            "payload_json",
            where_parts,
            params_vec,
            "class_no ASC, student_code ASC, draft_id ASC",
            limit.clamp(1, 10000),
        )
    }

    fn record_import_run(&self, mut input: Value) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(input.get("tenantId"));
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let kind = {
            let value = normalize_json_text(input.get("kind").or_else(|| input.get("source")), 80);
            if value.is_empty() { "manual".to_string() } else { value }
        };
        let parsed_started_at_ms = timestamp_like(input.get("startedAtMs").or_else(|| input.get("startedAt")));
        let started_at_ms = if parsed_started_at_ms > 0 { parsed_started_at_ms } else { now_ms() };
        let parsed_finished_at_ms = timestamp_like(input.get("finishedAtMs").or_else(|| input.get("finishedAt")));
        let finished_at_ms = if parsed_finished_at_ms > 0 { parsed_finished_at_ms.max(started_at_ms) } else { now_ms().max(started_at_ms) };
        let run_id = normalize_local_record_id(
            input.get("runId").or_else(|| input.get("id")),
            format!("{kind}_{started_at_ms}"),
            "import_run_id_required",
        )?;
        let status = {
            let value = normalize_json_text(input.get("status"), 40);
            if value.is_empty() { "completed".to_string() } else { value }
        };
        if let Value::Object(ref mut obj) = input {
            set_obj(obj, "tenantId", tenant_id.clone());
            set_obj(obj, "id", run_id.clone());
            set_obj(obj, "runId", run_id.clone());
            set_obj(obj, "kind", kind.clone());
            set_obj(obj, "status", status.clone());
            set_obj(obj, "startedAtMs", started_at_ms);
            set_obj(obj, "finishedAtMs", finished_at_ms);
        }
        let payload_json = payload_json(&input, "import_run_encode_failed")?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        conn.execute(
            "INSERT INTO local_import_runs
             (tenant_id, run_id, kind, status, payload_json, started_at_ms, finished_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(tenant_id, run_id) DO UPDATE SET
               kind = excluded.kind,
               status = excluded.status,
               payload_json = excluded.payload_json,
               started_at_ms = excluded.started_at_ms,
               finished_at_ms = excluded.finished_at_ms",
            params![tenant_id, run_id, kind, status, payload_json, started_at_ms, finished_at_ms],
        )
        .map_err(|e| format!("db_import_run_upsert_failed:{e}"))?;
        Ok(input)
    }

    fn list_import_runs(&self, tenant_id: String, kind: String, status: String, limit: i64) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut where_parts = vec!["tenant_id = ?".to_string()];
        let mut params_vec: Vec<Box<dyn ToSql>> = vec![Box::new(safe_tenant)];
        let safe_kind = normalize(kind, 80);
        if !safe_kind.is_empty() {
            where_parts.push("kind = ?".to_string());
            params_vec.push(Box::new(safe_kind));
        }
        let safe_status = normalize(status, 40);
        if !safe_status.is_empty() {
            where_parts.push("status = ?".to_string());
            params_vec.push(Box::new(safe_status));
        }
        self.query_math_payloads("local_import_runs", "payload_json", where_parts, params_vec, "finished_at_ms DESC", limit.clamp(1, 100))
    }

    fn list_date_student_payloads(&self, table: &str, tenant_id: String, date: String, date_from: String, date_to: String, student_code: String, order: &str, limit: i64) -> Result<Vec<Value>, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let mut where_parts = vec!["tenant_id = ?".to_string()];
        let mut params_vec: Vec<Box<dyn ToSql>> = vec![Box::new(safe_tenant)];
        let safe_date = normalize_date_key(Some(&Value::String(date)));
        if !safe_date.is_empty() {
            where_parts.push("date_key = ?".to_string());
            params_vec.push(Box::new(safe_date));
        } else {
            let from = normalize_date_key(Some(&Value::String(date_from)));
            let to = normalize_date_key(Some(&Value::String(date_to)));
            if !from.is_empty() {
                where_parts.push("date_key >= ?".to_string());
                params_vec.push(Box::new(from));
            }
            if !to.is_empty() {
                where_parts.push("date_key <= ?".to_string());
                params_vec.push(Box::new(to));
            }
        }
        let safe_student = normalize_student_code(Some(&Value::String(student_code)));
        if !safe_student.is_empty() {
            where_parts.push("student_code = ?".to_string());
            params_vec.push(Box::new(safe_student));
        }
        self.query_math_payloads(table, "payload_json", where_parts, params_vec, order, limit.clamp(1, 10000))
    }

    fn overview(&self, tenant_id: String) -> Result<Value, String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        if safe_tenant.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let stats = self.stats(safe_tenant.clone())?;
        let sections = json!([
            { "key": "observations", "label": "수업 관찰", "count": stats.get("observationCount").and_then(|value| value.as_i64()).unwrap_or(0), "updatedAtMs": stats.get("observationUpdatedAtMs").and_then(|value| value.as_i64()).unwrap_or(0), "route": "/v1/observations" },
            { "key": "teacher-counseling-sessions", "label": "교사 상담기록", "count": stats.get("teacherCounselingSessionCount").and_then(|value| value.as_i64()).unwrap_or(0), "updatedAtMs": stats.get("teacherCounselingUpdatedAtMs").and_then(|value| value.as_i64()).unwrap_or(0), "route": "/v1/teacher-counseling-sessions" },
            { "key": "student-private-details", "label": "학생 민감정보", "count": stats.get("studentPrivateDetailCount").and_then(|value| value.as_i64()).unwrap_or(0), "updatedAtMs": stats.get("studentPrivateDetailUpdatedAtMs").and_then(|value| value.as_i64()).unwrap_or(0), "route": "/v1/student-private-details" },
            { "key": "math-daily-attempts", "label": "매일수학 시도", "count": stats.get("mathDailyAttemptCount").and_then(|value| value.as_i64()).unwrap_or(0), "updatedAtMs": stats.get("mathDailyUpdatedAtMs").and_then(|value| value.as_i64()).unwrap_or(0), "route": "/v1/math-daily/attempts" },
            { "key": "board-post-snapshots", "label": "게시판 스냅샷", "count": stats.get("boardSnapshotCount").and_then(|value| value.as_i64()).unwrap_or(0), "updatedAtMs": stats.get("boardSnapshotUpdatedAtMs").and_then(|value| value.as_i64()).unwrap_or(0), "route": "/v1/board-post-snapshots" },
            { "key": "board-media", "label": "게시판 첨부파일", "count": stats.get("boardMediaCount").and_then(|value| value.as_i64()).unwrap_or(0), "updatedAtMs": stats.get("boardMediaArchivedAtMs").and_then(|value| value.as_i64()).unwrap_or(0), "route": "/v1/board-media" },
            { "key": "attendance-records", "label": "출결 기록", "count": stats.get("attendanceRecordCount").and_then(|value| value.as_i64()).unwrap_or(0), "updatedAtMs": stats.get("attendanceUpdatedAtMs").and_then(|value| value.as_i64()).unwrap_or(0), "route": "/v1/attendance-records" },
            { "key": "attendance-nais-checks", "label": "출결 NEIS 확인", "count": stats.get("attendanceNaisCheckCount").and_then(|value| value.as_i64()).unwrap_or(0), "updatedAtMs": stats.get("attendanceUpdatedAtMs").and_then(|value| value.as_i64()).unwrap_or(0), "route": "/v1/attendance-nais-checks" },
            { "key": "attendance-document-requests", "label": "출결 증빙 요청", "count": stats.get("attendanceDocumentRequestCount").and_then(|value| value.as_i64()).unwrap_or(0), "updatedAtMs": stats.get("attendanceUpdatedAtMs").and_then(|value| value.as_i64()).unwrap_or(0), "route": "/v1/attendance-document-requests" },
            { "key": "counseling-records", "label": "상담 원문", "count": stats.get("counselingRecordCount").and_then(|value| value.as_i64()).unwrap_or(0), "updatedAtMs": stats.get("counselingUpdatedAtMs").and_then(|value| value.as_i64()).unwrap_or(0), "route": "/v1/counseling-records" },
            { "key": "counseling-teacher-notes", "label": "상담 교사 메모", "count": stats.get("counselingTeacherNoteCount").and_then(|value| value.as_i64()).unwrap_or(0), "updatedAtMs": stats.get("counselingUpdatedAtMs").and_then(|value| value.as_i64()).unwrap_or(0), "route": "/v1/counseling-teacher-notes" },
            { "key": "eval-assignments", "label": "평가 운영", "count": stats.get("evalAssignmentCount").and_then(|value| value.as_i64()).unwrap_or(0), "updatedAtMs": stats.get("evalsUpdatedAtMs").and_then(|value| value.as_i64()).unwrap_or(0), "route": "/v1/eval-assignments" },
            { "key": "eval-results", "label": "평가 기록", "count": stats.get("evalResultCount").and_then(|value| value.as_i64()).unwrap_or(0), "updatedAtMs": stats.get("evalsUpdatedAtMs").and_then(|value| value.as_i64()).unwrap_or(0), "route": "/v1/eval-results" },
            { "key": "student-record-draft-sets", "label": "학생부 초안 세트", "count": stats.get("studentRecordDraftSetCount").and_then(|value| value.as_i64()).unwrap_or(0), "updatedAtMs": stats.get("studentRecordDraftUpdatedAtMs").and_then(|value| value.as_i64()).unwrap_or(0), "route": "/v1/student-record-draft-sets" },
            { "key": "student-record-drafts", "label": "학생부 초안", "count": stats.get("studentRecordDraftCount").and_then(|value| value.as_i64()).unwrap_or(0), "updatedAtMs": stats.get("studentRecordDraftUpdatedAtMs").and_then(|value| value.as_i64()).unwrap_or(0), "route": "/v1/student-record-drafts" },
            { "key": "work-notes", "label": "업무 노트", "count": stats.get("workNoteCount").and_then(|value| value.as_i64()).unwrap_or(0), "updatedAtMs": stats.get("workNoteUpdatedAtMs").and_then(|value| value.as_i64()).unwrap_or(0), "route": "/v1/work-notes" },
            { "key": "import-runs", "label": "가져오기 이력", "count": stats.get("importRunCount").and_then(|value| value.as_i64()).unwrap_or(0), "updatedAtMs": stats.get("importRunUpdatedAtMs").and_then(|value| value.as_i64()).unwrap_or(0), "route": "/v1/import-runs" }
        ]);
        let recent_import_runs = self.list_import_runs(safe_tenant.clone(), String::new(), String::new(), 10)?;
        Ok(json!({
            "ok": true,
            "tenantId": safe_tenant,
            "stats": stats,
            "sections": sections,
            "recentImportRuns": recent_import_runs
        }))
    }
}

fn get_header(request: &Request, name: &'static str) -> String {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv(name))
        .map(|header| header.value.as_str().trim().to_string())
        .unwrap_or_default()
}

fn allowed_origin(request: &Request) -> String {
    let origin = get_header(request, "Origin");
    if origin.is_empty() {
        return "*".to_string();
    }
    if let Ok(parsed) = Url::parse(&origin) {
        if parsed.scheme() == "chrome-extension" {
            return origin;
        }
        if let Some(host) = parsed.host_str() {
            if matches!(
                host,
                "localhost" | "127.0.0.1" | "::1" | "classaimate.pages.dev"
                    | "classaimate-v3.pages.dev" | "v3.classaimate.com"
                    | "classaimate.netlify.app" | "t.classaimate.com"
            ) {
                return origin;
            }
        }
    }
    "null".to_string()
}

fn request_authority(
    request: &Request,
    pairing_key: &str,
    browser_links: &BrowserLinkStore,
) -> (bool, Option<String>) {
    let header_key = get_header(request, "X-OnlineClass-Local-Store-Key");
    let auth = get_header(request, "Authorization");
    let bearer = auth
        .strip_prefix("Bearer ")
        .or_else(|| auth.strip_prefix("bearer "))
        .unwrap_or("")
        .trim()
        .to_string();
    if !pairing_key.is_empty() && (header_key == pairing_key || bearer == pairing_key) {
        return (true, None);
    }
    let browser_tenant = browser_links.authorize_tenant(request);
    (browser_tenant.is_some(), browser_tenant)
}

fn scope_tenant_id(explicit: String, browser_tenant: Option<&str>) -> Result<String, String> {
    let Some(expected) = browser_tenant else { return Ok(explicit); };
    if !explicit.is_empty() && explicit != expected {
        return Err("tenant_scope_mismatch".to_string());
    }
    Ok(expected.to_string())
}

fn scope_body_to_tenant(mut body: Value, browser_tenant: Option<&str>) -> Result<Value, String> {
    let Some(expected) = browser_tenant else { return Ok(body); };
    let Value::Object(ref mut object) = body else {
        return Err("invalid_json".to_string());
    };
    let explicit = normalize_json_text(object.get("tenantId"), 160);
    if !explicit.is_empty() && explicit != expected {
        return Err("tenant_scope_mismatch".to_string());
    }
    object.insert("tenantId".to_string(), Value::String(expected.to_string()));
    Ok(body)
}

fn health_payload(store: &SqliteStore, authorized: bool) -> Value {
    if authorized {
        let mut payload = store.health();
        if let Value::Object(ref mut obj) = payload {
            obj.insert("authorized".to_string(), Value::Bool(true));
        }
        return payload;
    }
    json!({
        "ok": true,
        "service": SERVICE_NAME,
        "version": SERVICE_VERSION,
        "routes": LOCAL_SENSITIVE_STORE_ROUTES,
        "features": LOCAL_SENSITIVE_STORE_FEATURES,
        "authorized": false
    })
}

fn json_response(status: u16, payload: Value, origin: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_vec(&payload).unwrap_or_else(|_| b"{\"ok\":false}".to_vec());
    let mut response = Response::from_data(body).with_status_code(StatusCode(status));
    for (name, value) in [
        ("Content-Type", "application/json; charset=utf-8"),
        ("Cache-Control", "no-store"),
        ("Access-Control-Allow-Origin", origin),
        (
            "Access-Control-Allow-Headers",
            "Content-Type, Authorization, X-OnlineClass-Local-Store-Key, X-OnlineClass-Local-Browser-Token",
        ),
        ("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS"),
        ("Access-Control-Allow-Private-Network", "true"),
    ] {
        if let Ok(header) = Header::from_bytes(name.as_bytes(), value.as_bytes()) {
            response.add_header(header);
        }
    }
    response
}

fn request_error_status(error: &str) -> u16 {
    match error {
        "invalid_json" => 400,
        "tenant_scope_mismatch" => 403,
        "body_too_large" => 413,
        "tenant_id_required" | "date_required" | "period_required" | "student_code_required"
        | "doc_id_required" | "cloud_sync_session_required" | "conflict_identity_required"
        | "board_snapshot_identity_required" | "media_identity_required" | "media_data_required"
        | "attendance_record_id_required" | "attendance_check_id_required"
        | "attendance_request_id_required" | "eval_assignment_id_required"
        | "eval_result_id_required" | "student_id_required" | "import_run_id_required"
        | "counseling_request_id_required" | "counseling_status_invalid"
        | "counseling_content_required" | "counseling_reply_content_required"
        | "counseling_source_hash_mismatch" | "counseling_duplicate_request_id"
        | "counseling_duplicate_teacher_note_id"
        | "work_note_page_id_required" | "work_note_parent_cycle" | "work_note_move_placement_invalid"
        | "work_note_attachment_id_required" | "work_note_attachment_path_invalid"
        | "backup_root_required" | "backup_root_inside_local_store"
        | "backup_manifest_required" | "backup_manifest_outside_configured_root" | "backup_db_required" => 400,
        "work_note_has_children" | "work_note_root_move_forbidden" | "work_note_root_sibling_forbidden"
        | "work_note_move_target_changed" | "counseling_import_run_conflict" => 409,
        "student_photo_content_type_invalid" | "student_photo_data_invalid"
        | "student_photo_size_invalid" | "student_photo_digest_mismatch" => 400,
        "media_not_found" | "media_file_missing" | "work_note_not_found"
        | "work_note_attachment_not_found" | "work_note_attachment_file_missing"
        | "counseling_record_not_found" | "counseling_teacher_note_not_found" => 404,
        _ => 500,
    }
}

fn parse_request_url(request: &Request) -> Result<Url, String> {
    Url::parse(&format!("http://{HOST}{}", request.url())).map_err(|e| format!("invalid_url:{e}"))
}

fn query(url: &Url, key: &str) -> String {
    url.query_pairs()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.to_string())
        .unwrap_or_default()
}

fn query_date(url: &Url, key: &str) -> String {
    normalize_date_key(Some(&Value::String(query(url, key))))
}

fn normalize_teacher_settings_url(url: &str) -> Result<String, String> {
    let mut parsed = Url::parse(url).map_err(|_| "invalid_url".to_string())?;
    let host = parsed.host_str().unwrap_or("");
    let scheme = parsed.scheme();
    let host_allowed = matches!(
        host,
        "classaimate.pages.dev" | "classaimate.netlify.app" | "t.classaimate.com" | "localhost" | "127.0.0.1"
    );
    if !host_allowed || !(scheme == "https" || scheme == "http") {
        return Err("url_not_allowed".to_string());
    }
    if host == "t.classaimate.com" && parsed.path().trim_end_matches('/') != "/connect-local" {
        return Err("url_not_allowed".to_string());
    }
    let path = parsed.path().trim_end_matches('/').to_string();
    let is_tenant_settings = path.ends_with("/teacher-dashboard/tenant-settings")
        || path.ends_with("/teacher-dashboard/tenant-settings.html");
    if is_tenant_settings {
        let pairs: Vec<(String, String)> = parsed
            .query_pairs()
            .filter(|(key, _)| !matches!(key.as_ref(), "tab" | "connectLocal" | "source"))
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        parsed.set_path("/teacher-dashboard/tenant-settings");
        parsed.set_query(None);
        {
            let mut query = parsed.query_pairs_mut();
            for (key, value) in pairs {
                query.append_pair(&key, &value);
            }
            query.append_pair("tab", "sensitive");
            query.append_pair("connectLocal", "1");
            query.append_pair("source", "local-sensitive-store");
        }
    }
    Ok(parsed.to_string())
}

fn read_body(request: &mut Request) -> Result<Value, String> {
    let mut reader = request.as_reader().take(MAX_BODY_BYTES + 1);
    let mut raw = String::new();
    reader
        .read_to_string(&mut raw)
        .map_err(|e| format!("body_read_failed:{e}"))?;
    if raw.len() as u64 > MAX_BODY_BYTES {
        return Err("body_too_large".to_string());
    }
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(&raw).map_err(|_| "invalid_json".to_string())
}

fn handle_request(
    mut request: Request,
    store: Arc<SqliteStore>,
    sync_manager: Arc<cloud_sync::CloudSyncManager>,
    device_sync_manager: Arc<device_sync::DeviceSyncManager>,
    browser_links: Arc<BrowserLinkStore>,
    pairing_key: String,
) {
    let origin = allowed_origin(&request);
    match work_note_attachments::handle_http_request(&mut request, &store, &browser_links, &pairing_key, &origin) {
        Ok(Some(response)) => {
            let _ = request.respond(response);
            return;
        }
        Ok(None) => {}
        Err(error) => {
            let status = request_error_status(&error);
            let response = json_response(
                status,
                json!({
                    "ok": false,
                    "error": if status >= 500 { "internal_error" } else { error.as_str() },
                    "details": error
                }),
                &origin,
            );
            let _ = request.respond(response);
            return;
        }
    }
    let result = (|| -> Result<(u16, Value), String> {
        if request.method() == &Method::Options {
            return Ok((200, json!({ "ok": true })));
        }

        let url = parse_request_url(&request)?;
        let path = url.path().to_string();
        let (authorized, browser_tenant) = request_authority(&request, &pairing_key, &browser_links);
        let query = |url: &Url, key: &str| -> String {
            let value = crate::query(url, key);
            if key != "tenantId" {
                return value;
            }
            scope_tenant_id(value, browser_tenant.as_deref()).unwrap_or_default()
        };

        if request.method() == &Method::Get && path == "/v1/health" {
            return Ok((200, health_payload(&store, authorized)));
        }

        if request.method() == &Method::Get && path == "/v1/device-authorization/browser-link" {
            if origin == "null" || origin == "*" {
                return Ok((403, json!({ "ok": false, "error": "origin_forbidden" })));
            }
            let request_id = query(&url, "requestId");
            return match browser_links.read_for_request(&request_id)? {
                Some(link) => Ok((200, json!({
                    "ok": true,
                    "requestId": request_id,
                    "tenantId": link.tenant_id,
                    "uid": link.uid,
                    "accountEmail": link.account_email,
                    "accountDisplayName": link.account_display_name,
                    "tenantName": link.tenant_name,
                    "browserToken": link.token
                }))),
                None => Ok((409, json!({ "ok": false, "error": "device_authorization_pending" }))),
            };
        }

        if !authorized {
            return Ok((401, json!({ "ok": false, "error": "unauthorized" })));
        }

        if browser_tenant.is_some() {
            let explicit_tenant = crate::query(&url, "tenantId");
            if !explicit_tenant.is_empty() {
                scope_tenant_id(explicit_tenant, browser_tenant.as_deref())?;
            }
        }
        let read_body = |request: &mut Request| -> Result<Value, String> {
            scope_body_to_tenant(crate::read_body(request)?, browser_tenant.as_deref())
        };
        let assert_sync_scope = |status: &Value| -> Result<(), String> {
            let Some(expected) = browser_tenant.as_deref() else { return Ok(()); };
            let connected = status.get("connected").and_then(Value::as_bool).unwrap_or(false);
            let actual = normalize_json_text(status.get("tenantId"), 160);
            if connected && actual != expected {
                return Err("tenant_scope_mismatch".to_string());
            }
            Ok(())
        };

        if request.method() == &Method::Post && path == "/v1/browser-link/disconnect" {
            if browser_tenant.is_none() || !browser_links.revoke_request_token(&request)? {
                return Ok((401, json!({ "ok": false, "error": "unauthorized" })));
            }
            let device_sync = device_sync_manager.disconnect();
            return match device_sync {
                Ok(_) => Ok((200, json!({ "ok": true, "disconnected": true, "deviceCredentialRevoked": true }))),
                Err(error) => Err(error),
            };
        }

        if request.method() == &Method::Post && path == "/v1/shared-archives/import" {
            let body = read_body(&mut request)?;
            let tenant_id = normalize_json_text(body.get("tenantId"), 160);
            let code = normalize_json_text(body.get("code"), 80);
            if tenant_id.is_empty() { return Err("tenant_id_required".to_string()); }
            if code.is_empty() { return Err("archive_code_invalid".to_string()); }
            let job_id = format!("shared-archive-{}", random_url_token());
            let base_url = env::var("ONLINECLASS_ARCHIVE_API_URL")
                .unwrap_or_else(|_| "https://t.classaimate.com".to_string());
            let started_at = now_ms();
            let imported = shared_archive::import_archive_for_tenant(&base_url, &code, &tenant_id);
            let (status, result, error) = match imported {
                Ok(value) => ("completed", value, Value::Null),
                Err(message) => ("failed", Value::Null, Value::String(message)),
            };
            let job = json!({
                "tenantId": tenant_id, "id": job_id, "jobId": job_id,
                "kind": "shared_archive", "status": status, "result": result,
                "error": error, "startedAtMs": started_at, "finishedAtMs": now_ms()
            });
            store.record_import_run(job.clone())?;
            return Ok((200, json!({ "ok": true, "job": job })));
        }

        if request.method() == &Method::Get && path.starts_with("/v1/shared-archives/import-jobs/") {
            let job_id = normalize(path.trim_start_matches("/v1/shared-archives/import-jobs/"), 128);
            let tenant_id = query(&url, "tenantId");
            if job_id.is_empty() || tenant_id.is_empty() { return Err("tenant_id_required".to_string()); }
            let jobs = store.list_import_runs(tenant_id, "shared_archive".to_string(), String::new(), 100)?;
            let job = jobs.into_iter().find(|item| item.get("jobId").and_then(Value::as_str) == Some(job_id.as_str()));
            return match job {
                Some(value) => Ok((200, json!({ "ok": true, "job": value }))),
                None => Ok((404, json!({ "ok": false, "error": "archive_import_job_not_found" }))),
            };
        }

        if request.method() == &Method::Get && path == "/v1/observations" {
            let records = store.list_observations(
                query(&url, "tenantId"),
                query_date(&url, "from"),
                query_date(&url, "to"),
                query_date(&url, "date"),
                normalize_period(Some(&Value::String(query(&url, "period")))),
                normalize_student_code(Some(&Value::String(query(&url, "studentCode")))),
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Get && path == "/v1/stats" {
            let stats = store.stats(query(&url, "tenantId"))?;
            return Ok((200, json!({ "ok": true, "stats": stats })));
        }

        if request.method() == &Method::Get && path == "/v1/overview" {
            return Ok((200, store.overview(query(&url, "tenantId"))?));
        }

        if request.method() == &Method::Get && path == "/v1/work-notes" {
            let records = store.list_work_notes(query(&url, "tenantId"), query(&url, "query"))?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Get && path == "/v1/work-notes/export" {
            let records = store.list_work_notes(query(&url, "tenantId"), String::new())?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Post && path == "/v1/work-notes/reconcile-mobile-meeting-root" {
            let body = read_body(&mut request)?;
            let tenant_id = normalize_json_text(body.get("tenantId"), 160);
            let ensure_root = body.get("ensureRoot").and_then(Value::as_bool).unwrap_or(false);
            return Ok((200, store.reconcile_mobile_meeting_root(tenant_id, ensure_root)?));
        }

        if request.method() == &Method::Post && path == "/v1/work-notes/move" {
            let body = read_body(&mut request)?;
            return Ok((200, store.move_work_note(body)?));
        }

        if request.method() == &Method::Get && path.starts_with("/v1/work-notes/") {
            let page_id = path.trim_start_matches("/v1/work-notes/").to_string();
            return match store.get_work_note(query(&url, "tenantId"), page_id)? {
                Some(record) => Ok((200, json!({ "ok": true, "record": record }))),
                None => Ok((404, json!({ "ok": false, "error": "work_note_not_found" }))),
            };
        }

        if request.method() == &Method::Put && path.starts_with("/v1/work-notes/") {
            let page_id = path.trim_start_matches("/v1/work-notes/").to_string();
            let mut body = read_body(&mut request)?;
            if let Some(object) = body.as_object_mut() { object.insert("pageId".to_string(), Value::String(page_id)); }
            let record = store.upsert_work_note(body)?;
            return Ok((200, json!({ "ok": true, "record": record })));
        }

        if request.method() == &Method::Delete && path.starts_with("/v1/work-notes/") {
            let page_id = path.trim_start_matches("/v1/work-notes/").to_string();
            return Ok((200, store.delete_work_note(query(&url, "tenantId"), page_id)?));
        }

        if request.method() == &Method::Post && path == "/v1/work-notes/import" {
            let body = read_body(&mut request)?;
            let records = body.get("records").and_then(Value::as_array).cloned().unwrap_or_default();
            let mut imported = Vec::new();
            for mut record in records.into_iter().take(2000) {
                if let Some(object) = record.as_object_mut() {
                    object.insert("tenantId".to_string(), body.get("tenantId").cloned().unwrap_or(Value::Null));
                }
                imported.push(store.upsert_work_note(record)?);
            }
            return Ok((200, json!({ "ok": true, "imported": imported.len(), "records": imported })));
        }

        if request.method() == &Method::Get && path == "/v1/student-private-details" {
            let records = store.list_student_private_details(
                query(&url, "tenantId"),
                normalize_student_code(Some(&Value::String(query(&url, "studentCode")))),
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Put && path.starts_with("/v1/student-private-details/") {
            let mut body = read_body(&mut request)?;
            let student_code = path.trim_start_matches("/v1/student-private-details/").to_string();
            if let Value::Object(ref mut obj) = body {
                obj.insert("studentCode".to_string(), Value::String(student_code));
            }
            let record = store.upsert_student_private_detail(body)?;
            return Ok((200, json!({ "ok": true, "record": record })));
        }

        if request.method() == &Method::Post && path == "/v1/student-private-details/import" {
            let body = read_body(&mut request)?;
            let tenant_id = normalize_json_text(body.get("tenantId"), 160);
            let records = body
                .get("records")
                .and_then(|value| value.as_array())
                .cloned()
                .unwrap_or_default();
            let saved = store.import_student_private_details(tenant_id, records)?;
            return Ok((
                200,
                json!({ "ok": true, "imported": saved.len(), "records": saved }),
            ));
        }

        if path.starts_with("/v1/student-private-photos/") {
            let student_code = path.trim_start_matches("/v1/student-private-photos/").to_string();
            if request.method() == &Method::Get {
                let record = store.get_student_private_photo(query(&url, "tenantId"), student_code)?;
                return Ok((200, json!({ "ok": true, "record": record })));
            }
            if request.method() == &Method::Put {
                let mut body = read_body(&mut request)?;
                if let Value::Object(ref mut obj) = body {
                    obj.insert("studentCode".to_string(), Value::String(student_code));
                }
                let record = store.upsert_student_private_photo(body)?;
                return Ok((200, json!({ "ok": true, "record": record })));
            }
            if request.method() == &Method::Delete {
                let deleted = store.delete_student_private_photo(query(&url, "tenantId"), student_code)?;
                return Ok((200, json!({ "ok": true, "deleted": deleted })));
            }
        }

        if request.method() == &Method::Get && path == "/v1/math-daily/cache" {
            let payload = json!({
                "tenantId": query(&url, "tenantId"),
                "action": query(&url, "action"),
                "dateKey": query(&url, "dateKey"),
                "dateFrom": query(&url, "dateFrom"),
                "dateTo": query(&url, "dateTo"),
                "curriculum": query(&url, "curriculum")
            });
            return Ok((200, store.get_math_daily_cache(payload)?));
        }

        if request.method() == &Method::Get && path == "/v1/math-daily/cache-status" {
            return Ok((200, store.get_math_daily_cache_status(query(&url, "tenantId"))?));
        }

        if request.method() == &Method::Post && path == "/v1/math-daily/import" {
            let body = read_body(&mut request)?;
            return Ok((200, store.import_math_daily_cache(body)?));
        }

        if request.method() == &Method::Get && path == "/v1/math-daily/attempts" {
            let records = store.list_math_daily_attempts(
                query(&url, "tenantId"),
                query(&url, "dateKey"),
                query(&url, "dateFrom"),
                query(&url, "dateTo"),
                query(&url, "studentCode"),
                query(&url, "curriculum"),
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Get && path == "/v1/math-daily/student-profiles" {
            let records = store.list_math_daily_student_profiles(
                query(&url, "tenantId"),
                query(&url, "studentCode"),
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Get && path == "/v1/math-daily/review-sessions" {
            let records = store.list_math_daily_simple_range(
                "math_daily_review_sessions",
                query(&url, "tenantId"),
                query(&url, "dateFrom"),
                query(&url, "dateTo"),
                query(&url, "studentCode"),
                query(&url, "curriculum"),
                "date_key DESC, student_code ASC",
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Get && path == "/v1/math-daily/assignments" {
            let records = store.list_math_daily_assignments(
                query(&url, "tenantId"),
                query(&url, "dateFrom"),
                query(&url, "dateTo"),
                query(&url, "curriculum"),
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Get && path == "/v1/math-daily/assignment-results" {
            let records = store.list_math_daily_assignment_results(
                query(&url, "tenantId"),
                query(&url, "assignmentId"),
                query(&url, "studentCode"),
                query(&url, "dateFrom"),
                query(&url, "dateTo"),
                query(&url, "curriculum"),
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Put && path.starts_with("/v1/observations/") {
            let mut body = read_body(&mut request)?;
            let doc_id = path.trim_start_matches("/v1/observations/").to_string();
            if let Value::Object(ref mut obj) = body {
                obj.insert("docId".to_string(), Value::String(doc_id));
            }
            let record = store.upsert_observation(body)?;
            return Ok((200, json!({ "ok": true, "record": record })));
        }

        if request.method() == &Method::Post && path == "/v1/observations/import" {
            let body = read_body(&mut request)?;
            let tenant_id = normalize_json_text(body.get("tenantId"), 160);
            let records = body
                .get("records")
                .and_then(|value| value.as_array())
                .cloned()
                .unwrap_or_default();
            let saved = store.import_observations(tenant_id, records)?;
            return Ok((
                200,
                json!({ "ok": true, "imported": saved.len(), "records": saved }),
            ));
        }

        if request.method() == &Method::Get && path == "/v1/teacher-counseling-sessions" {
            let records = store.list_teacher_counseling_sessions(
                query(&url, "tenantId"), query(&url, "studentCode"), query(&url, "status"),
                query(&url, "includeArchived") == "true", query(&url, "limit").parse::<i64>().unwrap_or(100),
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Get && path.starts_with("/v1/teacher-counseling-sessions/") {
            let session_id = path.trim_start_matches("/v1/teacher-counseling-sessions/").to_string();
            return match store.get_teacher_counseling_session(query(&url, "tenantId"), session_id)? {
                Some(record) => Ok((200, json!({ "ok": true, "record": record }))),
                None => Ok((404, json!({ "ok": false, "error": "teacher_counseling_session_not_found" }))),
            };
        }

        if request.method() == &Method::Put && path.starts_with("/v1/teacher-counseling-sessions/") {
            let mut body = read_body(&mut request)?;
            let session_id = path.trim_start_matches("/v1/teacher-counseling-sessions/").to_string();
            if let Value::Object(ref mut obj) = body { obj.insert("sessionId".to_string(), Value::String(session_id)); }
            let record = store.upsert_teacher_counseling_session(body)?;
            return Ok((200, json!({ "ok": true, "record": record })));
        }

        if request.method() == &Method::Get && path == "/v1/attendance-records" {
            let limit = query(&url, "limit").parse::<i64>().unwrap_or(5000);
            let records = store.list_attendance_records(
                query(&url, "tenantId"),
                query(&url, "date"),
                query(&url, "from"),
                query(&url, "to"),
                query(&url, "studentCode"),
                limit,
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Put && path.starts_with("/v1/attendance-records/") {
            let mut body = read_body(&mut request)?;
            let record_id = path.trim_start_matches("/v1/attendance-records/").to_string();
            if let Value::Object(ref mut obj) = body {
                obj.insert("recordId".to_string(), Value::String(record_id));
            }
            let record = store.upsert_attendance_record(body)?;
            return Ok((200, json!({ "ok": true, "record": record })));
        }

        if request.method() == &Method::Post && path == "/v1/attendance-records/import" {
            let body = read_body(&mut request)?;
            let tenant_id = normalize_json_text(body.get("tenantId"), 160);
            let records = body.get("records").and_then(|value| value.as_array()).cloned().unwrap_or_default();
            let saved = store.import_attendance_records(tenant_id, records)?;
            return Ok((200, json!({ "ok": true, "imported": saved.len(), "records": saved })));
        }

        if request.method() == &Method::Get && path == "/v1/attendance-nais-checks" {
            let limit = query(&url, "limit").parse::<i64>().unwrap_or(5000);
            let records = store.list_attendance_nais_checks(
                query(&url, "tenantId"),
                query(&url, "date"),
                query(&url, "from"),
                query(&url, "to"),
                query(&url, "studentCode"),
                limit,
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Put && path.starts_with("/v1/attendance-nais-checks/") {
            let mut body = read_body(&mut request)?;
            let check_id = path.trim_start_matches("/v1/attendance-nais-checks/").to_string();
            if let Value::Object(ref mut obj) = body {
                obj.insert("checkId".to_string(), Value::String(check_id));
            }
            let record = store.upsert_attendance_nais_check(body)?;
            return Ok((200, json!({ "ok": true, "record": record })));
        }

        if request.method() == &Method::Post && path == "/v1/attendance-nais-checks/import" {
            let body = read_body(&mut request)?;
            let tenant_id = normalize_json_text(body.get("tenantId"), 160);
            let records = body.get("records").and_then(|value| value.as_array()).cloned().unwrap_or_default();
            let saved = store.import_attendance_nais_checks(tenant_id, records)?;
            return Ok((200, json!({ "ok": true, "imported": saved.len(), "records": saved })));
        }

        if request.method() == &Method::Get && path == "/v1/attendance-document-requests" {
            let limit = query(&url, "limit").parse::<i64>().unwrap_or(5000);
            let records = store.list_attendance_document_requests(
                query(&url, "tenantId"),
                query(&url, "date"),
                query(&url, "from"),
                query(&url, "to"),
                query(&url, "studentCode"),
                limit,
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Put && path.starts_with("/v1/attendance-document-requests/") {
            let mut body = read_body(&mut request)?;
            let request_id = path.trim_start_matches("/v1/attendance-document-requests/").to_string();
            if let Value::Object(ref mut obj) = body {
                obj.insert("requestId".to_string(), Value::String(request_id));
            }
            let record = store.upsert_attendance_document_request(body)?;
            return Ok((200, json!({ "ok": true, "record": record })));
        }

        if request.method() == &Method::Post && path == "/v1/attendance-document-requests/import" {
            let body = read_body(&mut request)?;
            let tenant_id = normalize_json_text(body.get("tenantId"), 160);
            let records = body.get("records").and_then(|value| value.as_array()).cloned().unwrap_or_default();
            let saved = store.import_attendance_document_requests(tenant_id, records)?;
            return Ok((200, json!({ "ok": true, "imported": saved.len(), "records": saved })));
        }

        if request.method() == &Method::Get && path == "/v1/counseling-records" {
            let limit = query(&url, "limit").parse::<i64>().unwrap_or(500);
            let records = store.list_counseling_records(
                query(&url, "tenantId"),
                query(&url, "status"),
                query(&url, "studentCode"),
                limit,
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Get && path.starts_with("/v1/counseling-records/") {
            let request_id = path.trim_start_matches("/v1/counseling-records/").to_string();
            return match store.get_counseling_record(query(&url, "tenantId"), request_id)? {
                Some(record) => Ok((200, json!({ "ok": true, "record": record }))),
                None => Ok((404, json!({ "ok": false, "error": "counseling_record_not_found" }))),
            };
        }

        if request.method() == &Method::Put && path.starts_with("/v1/counseling-records/") {
            let mut body = read_body(&mut request)?;
            let request_id = path.trim_start_matches("/v1/counseling-records/").to_string();
            if let Value::Object(ref mut obj) = body {
                set_obj(obj, "requestId", request_id);
            }
            let record = store.upsert_counseling_record(body)?;
            return Ok((200, json!({ "ok": true, "record": record })));
        }

        if request.method() == &Method::Get && path == "/v1/counseling-teacher-notes" {
            let limit = query(&url, "limit").parse::<i64>().unwrap_or(500);
            let records = store.list_counseling_teacher_notes(
                query(&url, "tenantId"),
                query(&url, "requestId"),
                limit,
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Get && path.starts_with("/v1/counseling-teacher-notes/") {
            let request_id = path.trim_start_matches("/v1/counseling-teacher-notes/").to_string();
            return match store.get_counseling_teacher_note(query(&url, "tenantId"), request_id)? {
                Some(record) => Ok((200, json!({ "ok": true, "record": record }))),
                None => Ok((404, json!({ "ok": false, "error": "counseling_teacher_note_not_found" }))),
            };
        }

        if request.method() == &Method::Put && path.starts_with("/v1/counseling-teacher-notes/") {
            let mut body = read_body(&mut request)?;
            let request_id = path.trim_start_matches("/v1/counseling-teacher-notes/").to_string();
            if let Value::Object(ref mut obj) = body {
                set_obj(obj, "requestId", request_id);
            }
            let record = store.upsert_counseling_teacher_note(body)?;
            return Ok((200, json!({ "ok": true, "record": record })));
        }

        if request.method() == &Method::Post && path == "/v1/counseling-import" {
            return Ok((200, store.import_counseling_snapshot(read_body(&mut request)?)?));
        }

        if request.method() == &Method::Post && path == "/v1/counseling-compare" {
            return Ok((200, store.compare_counseling_snapshot(read_body(&mut request)?)?));
        }

        if request.method() == &Method::Get && path == "/v1/eval-assignments" {
            let limit = query(&url, "limit").parse::<i64>().unwrap_or(1000);
            let records = store.list_eval_assignments(
                query(&url, "tenantId"),
                query(&url, "assignmentId"),
                query(&url, "sharedPlanId"),
                query(&url, "scheduledDate"),
                limit,
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Put && path.starts_with("/v1/eval-assignments/") {
            let mut body = read_body(&mut request)?;
            let assignment_id = path.trim_start_matches("/v1/eval-assignments/").to_string();
            if let Value::Object(ref mut obj) = body {
                obj.insert("assignmentId".to_string(), Value::String(assignment_id));
            }
            let record = store.upsert_eval_assignment(body)?;
            return Ok((200, json!({ "ok": true, "record": record })));
        }

        if request.method() == &Method::Post && path == "/v1/eval-assignments/import" {
            let body = read_body(&mut request)?;
            let tenant_id = normalize_json_text(body.get("tenantId"), 160);
            let records = body.get("records").and_then(|value| value.as_array()).cloned().unwrap_or_default();
            let saved = store.import_eval_assignments(tenant_id, records)?;
            return Ok((200, json!({ "ok": true, "imported": saved.len(), "records": saved })));
        }

        if request.method() == &Method::Get && path == "/v1/eval-results" {
            let limit = query(&url, "limit").parse::<i64>().unwrap_or(5000);
            let student_id = {
                let explicit = query(&url, "studentId");
                if explicit.is_empty() { query(&url, "studentCode") } else { explicit }
            };
            let records = store.list_eval_results(
                query(&url, "tenantId"),
                query(&url, "resultId"),
                query(&url, "assignmentId"),
                student_id,
                query(&url, "date"),
                limit,
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Put && path.starts_with("/v1/eval-results/") {
            let mut body = read_body(&mut request)?;
            let result_id = path.trim_start_matches("/v1/eval-results/").to_string();
            if let Value::Object(ref mut obj) = body {
                obj.insert("resultId".to_string(), Value::String(result_id));
            }
            let record = store.upsert_eval_result(body)?;
            return Ok((200, json!({ "ok": true, "record": record })));
        }

        if request.method() == &Method::Post && path == "/v1/eval-results/import" {
            let body = read_body(&mut request)?;
            let tenant_id = normalize_json_text(body.get("tenantId"), 160);
            let records = body.get("records").and_then(|value| value.as_array()).cloned().unwrap_or_default();
            let saved = store.import_eval_results(tenant_id, records)?;
            return Ok((200, json!({ "ok": true, "imported": saved.len(), "records": saved })));
        }

        if request.method() == &Method::Get && path == "/v1/student-record-draft-sets" {
            let limit = query(&url, "limit").parse::<i64>().unwrap_or(100);
            let records = store.list_student_record_draft_sets(
                query(&url, "tenantId"),
                query(&url, "draftSetId"),
                query(&url, "status"),
                limit,
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Put && path.starts_with("/v1/student-record-draft-sets/") {
            let mut body = read_body(&mut request)?;
            let draft_set_id = path.trim_start_matches("/v1/student-record-draft-sets/").to_string();
            if let Value::Object(ref mut obj) = body {
                obj.insert("draftSetId".to_string(), Value::String(draft_set_id));
            }
            let record = store.upsert_student_record_draft_set(body)?;
            return Ok((200, json!({ "ok": true, "record": record })));
        }

        if request.method() == &Method::Post && path == "/v1/student-record-draft-sets/import" {
            let body = read_body(&mut request)?;
            let tenant_id = normalize_json_text(body.get("tenantId"), 160);
            let records = body.get("records").and_then(|value| value.as_array()).cloned().unwrap_or_default();
            let saved = store.import_student_record_draft_sets(tenant_id, records)?;
            return Ok((200, json!({ "ok": true, "imported": saved.len(), "records": saved })));
        }

        if request.method() == &Method::Get && path == "/v1/student-record-drafts" {
            let limit = query(&url, "limit").parse::<i64>().unwrap_or(5000);
            let records = store.list_student_record_drafts(
                query(&url, "tenantId"),
                query(&url, "draftId"),
                query(&url, "draftSetId"),
                query(&url, "studentCode"),
                limit,
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Put && path.starts_with("/v1/student-record-drafts/") {
            let mut body = read_body(&mut request)?;
            let draft_id = path.trim_start_matches("/v1/student-record-drafts/").to_string();
            if let Value::Object(ref mut obj) = body {
                obj.insert("draftId".to_string(), Value::String(draft_id));
            }
            let record = store.upsert_student_record_draft(body)?;
            return Ok((200, json!({ "ok": true, "record": record })));
        }

        if request.method() == &Method::Post && path == "/v1/student-record-drafts/import" {
            let body = read_body(&mut request)?;
            let tenant_id = normalize_json_text(body.get("tenantId"), 160);
            let records = body.get("records").and_then(|value| value.as_array()).cloned().unwrap_or_default();
            let saved = store.import_student_record_drafts(tenant_id, records)?;
            return Ok((200, json!({ "ok": true, "imported": saved.len(), "records": saved })));
        }

        if request.method() == &Method::Get && path == "/v1/import-runs" {
            let limit = query(&url, "limit").parse::<i64>().unwrap_or(20);
            let records = store.list_import_runs(
                query(&url, "tenantId"),
                query(&url, "kind"),
                query(&url, "status"),
                limit,
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Post && path == "/v1/import-runs" {
            let body = read_body(&mut request)?;
            let record = store.record_import_run(body)?;
            return Ok((200, json!({ "ok": true, "record": record })));
        }

        if request.method() == &Method::Put && path.starts_with("/v1/import-runs/") {
            let mut body = read_body(&mut request)?;
            let run_id = path.trim_start_matches("/v1/import-runs/").to_string();
            if let Value::Object(ref mut obj) = body {
                obj.insert("runId".to_string(), Value::String(run_id));
            }
            let record = store.record_import_run(body)?;
            return Ok((200, json!({ "ok": true, "record": record })));
        }

        if request.method() == &Method::Get && path == "/v1/board-post-snapshots" {
            let records = store.list_board_snapshots(
                query(&url, "tenantId"),
                query(&url, "boardId"),
                query(&url, "postId"),
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Put && path.starts_with("/v1/board-post-snapshots/") {
            let mut body = read_body(&mut request)?;
            let mut parts = path
                .trim_start_matches("/v1/board-post-snapshots/")
                .split('/');
            let board_id = parts.next().unwrap_or("").to_string();
            let post_id = parts.next().unwrap_or("").to_string();
            if let Value::Object(ref mut obj) = body {
                obj.insert("boardId".to_string(), Value::String(board_id));
                obj.insert("postId".to_string(), Value::String(post_id));
            }
            let record = store.upsert_board_snapshot(body)?;
            return Ok((200, json!({ "ok": true, "record": record })));
        }

        if request.method() == &Method::Get && path == "/v1/board-media" {
            let records = store.list_board_media(
                query(&url, "tenantId"),
                query(&url, "boardId"),
                query(&url, "postId"),
            )?;
            return Ok((200, json!({ "ok": true, "records": records })));
        }

        if request.method() == &Method::Put && path.starts_with("/v1/board-media/") {
            let mut body = read_body(&mut request)?;
            let media_id = path.trim_start_matches("/v1/board-media/").to_string();
            if let Value::Object(ref mut obj) = body {
                obj.insert("mediaId".to_string(), Value::String(media_id));
            }
            let record = store.upsert_board_media(body)?;
            return Ok((200, json!({ "ok": true, "record": record })));
        }

        if request.method() == &Method::Get
            && path.starts_with("/v1/board-media/")
            && path.ends_with("/file")
        {
            let media_id = path
                .trim_start_matches("/v1/board-media/")
                .trim_end_matches("/file")
                .trim_end_matches('/')
                .to_string();
            let file = store.get_board_media_file(query(&url, "tenantId"), media_id)?;
            return Ok((200, file));
        }

        if request.method() == &Method::Get && path == "/v1/cloud-sync/status" {
            let status = sync_manager.status()?;
            assert_sync_scope(&status)?;
            return Ok((200, status));
        }

        if request.method() == &Method::Get && path == "/v1/cloud-sync/runs" {
            let limit = query(&url, "limit").parse::<i64>().unwrap_or(20);
            let runs = store.list_cloud_sync_runs(query(&url, "tenantId"), limit)?;
            return Ok((200, json!({ "ok": true, "runs": runs })));
        }

        if request.method() == &Method::Post && path == "/v1/cloud-sync/connect" {
            let body = read_body(&mut request)?;
            let status = sync_manager.connect(body)?;
            return Ok((200, status));
        }

        if request.method() == &Method::Post && path == "/v1/cloud-sync/run" {
            assert_sync_scope(&sync_manager.status()?)?;
            let status = sync_manager.run_once()?;
            return Ok((200, status));
        }

        if request.method() == &Method::Post && path == "/v1/cloud-sync/disconnect" {
            let current_status = sync_manager.status()?;
            assert_sync_scope(&current_status)?;
            let current_tenant = current_status
                .get("tenantId")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_default();
            let status = sync_manager.disconnect()?;
            if !current_tenant.is_empty() {
                browser_links.revoke_tenant(&current_tenant)?;
            }
            return Ok((200, status));
        }

        if request.method() == &Method::Get && path == "/v1/device-sync/status" {
            let status = device_sync_manager.status()?;
            assert_sync_scope(&status)?;
            return Ok((200, status));
        }

        if request.method() == &Method::Post && path == "/v1/device-sync/run" {
            assert_sync_scope(&device_sync_manager.status()?)?;
            let status = device_sync_manager.run_once(true)?;
            return Ok((200, status));
        }

        if request.method() == &Method::Get && path == "/v1/backups/status" {
            let status = backup::status(&store, query(&url, "tenantId"))?;
            return Ok((200, status));
        }

        if request.method() == &Method::Get && path == "/v1/backups/list" {
            let limit = query(&url, "limit").parse::<i64>().unwrap_or(20);
            let backups = backup::list_backups(&store, query(&url, "tenantId"), limit)?;
            return Ok((200, backups));
        }

        if request.method() == &Method::Post && path == "/v1/backups/run" {
            let body = read_body(&mut request)?;
            let result = backup::run_from_body(&store, body)?;
            return Ok((200, result));
        }

        if request.method() == &Method::Post && path == "/v1/backups/restore-preview" {
            let body = read_body(&mut request)?;
            let result = backup::restore_preview(&store, body)?;
            return Ok((200, result));
        }

        if request.method() == &Method::Post && path == "/v1/backups/restore" {
            let body = read_body(&mut request)?;
            let result = backup::restore(&store, body)?;
            return Ok((200, result));
        }

        Ok((404, json!({ "ok": false, "error": "not_found" })))
    })();

    let response = match result {
        Ok((status, payload)) => json_response(status, payload, &origin),
        Err(error) => {
            let status = request_error_status(&error);
            json_response(
                status,
                json!({
                    "ok": false,
                    "error": if status >= 500 { "internal_error" } else { error.as_str() },
                    "details": error
                }),
                &origin,
            )
        }
    };

    let _ = request.respond(response);
}

fn bind_server() -> Result<(Server, u16), String> {
    let mut last_error = String::new();
    for port in PORTS {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        match Server::http(addr) {
            Ok(server) => return Ok((server, port)),
            Err(error) => last_error = error.to_string(),
        }
    }
    Err(if last_error.is_empty() {
        "no_available_port".to_string()
    } else {
        last_error
    })
}

fn start_service() -> Result<(
    ServiceStatus,
    Arc<SqliteStore>,
    Arc<cloud_sync::CloudSyncManager>,
    Arc<device_sync::DeviceSyncManager>,
    Arc<BrowserLinkStore>,
), String> {
    let paths = resolve_paths();
    fs::create_dir_all(&paths.data_dir).map_err(|e| format!("data_dir_create_failed:{e}"))?;
    let pairing_key = ensure_pairing_key(&paths.key_path)?;
    let store = Arc::new(SqliteStore::open(paths.db_path.clone())?);
    let sync_manager = Arc::new(cloud_sync::CloudSyncManager::new(paths.data_dir.clone(), Arc::clone(&store)));
    let device_sync_manager = Arc::new(device_sync::DeviceSyncManager::new(
        paths.data_dir.clone(),
        Arc::clone(&store),
    ));
    let browser_links = Arc::new(BrowserLinkStore::open(&paths.data_dir)?);
    let (server, port) = bind_server()?;
    let endpoint = format!("http://{HOST}:{port}");
    let thread_store = Arc::clone(&store);
    let thread_sync_manager = Arc::clone(&sync_manager);
    let thread_device_sync_manager = Arc::clone(&device_sync_manager);
    let thread_browser_links = Arc::clone(&browser_links);
    let thread_key = pairing_key.clone();

    thread::spawn(move || {
        for request in server.incoming_requests() {
            handle_request(
                request,
                Arc::clone(&thread_store),
                Arc::clone(&thread_sync_manager),
                Arc::clone(&thread_device_sync_manager),
                Arc::clone(&thread_browser_links),
                thread_key.clone(),
            );
        }
    });

    let status = ServiceStatus {
        ok: true,
        service: SERVICE_NAME.to_string(),
        version: SERVICE_VERSION.to_string(),
        pc_name: local_pc_name(),
        os: local_os_name(),
        arch: local_arch(),
        host: HOST.to_string(),
        port,
        endpoint,
        data_dir: paths.data_dir.to_string_lossy().to_string(),
        db_path: paths.db_path.to_string_lossy().to_string(),
        key_path: paths.key_path.to_string_lossy().to_string(),
        pairing_key,
        error: None,
    };
    Ok((status, store, sync_manager, device_sync_manager, browser_links))
}

#[tauri::command]
fn get_service_status(state: tauri::State<'_, AppState>) -> ServiceStatus {
    state
        .status
        .lock()
        .map(|status| status.clone())
        .unwrap_or_else(|_| ServiceStatus::failed("status_lock_failed".to_string()))
}

fn device_authorization_api_url() -> String {
    env::var("ONLINECLASS_DEVICE_AUTH_API_URL")
        .ok()
        .and_then(|value| {
            let parsed = Url::parse(&value).ok()?;
            let host = parsed.host_str()?;
            if (parsed.scheme() == "https" && host == "t.classaimate.com")
                || (parsed.scheme() == "http" && matches!(host, "localhost" | "127.0.0.1"))
            {
                Some(parsed.to_string().trim_end_matches('/').to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| DEVICE_AUTHORIZATION_API_URL.to_string())
}

fn random_url_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::thread_rng().fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn sha256_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn device_api_response(response: Result<ureq::Response, ureq::Error>) -> Result<Value, String> {
    match response {
        Ok(response) => response.into_json::<Value>().map_err(|e| format!("device_authorization_decode_failed:{e}")),
        Err(ureq::Error::Status(status, response)) => {
            let payload = response.into_json::<Value>().unwrap_or_else(|_| json!({}));
            let code = payload.pointer("/error/code").and_then(Value::as_str).unwrap_or("request_failed");
            Err(format!("device_authorization_http_{status}:{code}"))
        }
        Err(ureq::Error::Transport(error)) => Err(format!("device_authorization_network_failed:{error}")),
    }
}

fn device_api_data(payload: Value) -> Result<Value, String> {
    if payload.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err("device_authorization_response_invalid".to_string());
    }
    payload.get("data").cloned().ok_or_else(|| "device_authorization_response_invalid".to_string())
}

#[tauri::command]
fn start_device_authorization(state: tauri::State<'_, AppState>) -> Value {
    let status = match state.status.lock().map(|status| status.clone()) {
        Ok(status) if status.ok => status,
        _ => return json!({ "ok": false, "error": "local_store_unavailable" }),
    };
    let request_id = random_url_token();
    let verifier = random_url_token();
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(8))
        .timeout_write(Duration::from_secs(8))
        .build();
    let payload = json!({
        "requestId": request_id,
        "verifierDigest": sha256_hex(&verifier),
        "deviceName": status.pc_name,
        "platformLabel": format!("{} {}", status.os, status.arch).trim().to_string(),
        "appVersion": env!("CARGO_PKG_VERSION")
    });
    let created = match device_api_response(agent.post(&device_authorization_api_url()).send_json(payload)).and_then(device_api_data) {
        Ok(value) => value,
        Err(error) => return json!({ "ok": false, "error": error }),
    };
    let created_at_ms = created.get("createdAt").and_then(Value::as_i64).unwrap_or_else(now_ms);
    let expires_at_ms = created.get("expiresAt").and_then(Value::as_i64)
        .unwrap_or(created_at_ms + DEVICE_AUTHORIZATION_TTL_MS);
    let mut page = Url::parse(DEVICE_AUTHORIZATION_PAGE_URL).expect("device authorization page URL");
    page.query_pairs_mut().append_pair("requestId", &request_id);
    let pending = PendingDeviceAuthorization {
        request_id: request_id.clone(),
        verifier,
        authorization_url: page.to_string(),
        created_at_ms,
        expires_at_ms,
    };
    if let Ok(mut slot) = state.pending_device_authorization.lock() {
        *slot = Some(pending.clone());
    } else {
        return json!({ "ok": false, "error": "device_authorization_lock_failed" });
    }
    let opened = open_teacher_settings_url(pending.authorization_url.clone());
    if opened.get("ok").and_then(Value::as_bool) != Some(true) {
        return json!({ "ok": false, "error": opened.get("error").cloned().unwrap_or(Value::String("open_url_failed".to_string())) });
    }
    json!({ "ok": true, "status": "pending", "requestId": request_id, "createdAtMs": created_at_ms, "expiresAtMs": expires_at_ms })
}

#[tauri::command]
fn reopen_device_authorization(state: tauri::State<'_, AppState>) -> Value {
    let pending = state.pending_device_authorization.lock().ok().and_then(|value| value.clone());
    match pending {
        Some(pending) if pending.expires_at_ms > now_ms() => open_teacher_settings_url(pending.authorization_url),
        _ => json!({ "ok": false, "error": "device_authorization_missing" }),
    }
}

#[tauri::command]
fn poll_device_authorization(state: tauri::State<'_, AppState>) -> Value {
    let pending = match state.pending_device_authorization.lock().ok().and_then(|value| value.clone()) {
        Some(pending) => pending,
        None => return json!({ "ok": true, "status": "idle" }),
    };
    if pending.expires_at_ms <= now_ms() {
        if let Ok(mut slot) = state.pending_device_authorization.lock() { *slot = None; }
        return json!({ "ok": true, "status": "expired" });
    }
    let base = device_authorization_api_url();
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(8))
        .timeout_write(Duration::from_secs(8))
        .build();
    let inspected = match device_api_response(agent.get(&format!("{base}/{}", pending.request_id)).call()).and_then(device_api_data) {
        Ok(value) => value,
        Err(error) => return json!({ "ok": false, "status": "pending", "error": error }),
    };
    let remote_status = inspected.get("status").and_then(Value::as_str).unwrap_or("pending");
    if remote_status != "approved" {
        if matches!(remote_status, "expired" | "canceled" | "consumed") {
            if let Ok(mut slot) = state.pending_device_authorization.lock() { *slot = None; }
        }
        return json!({ "ok": true, "status": remote_status, "expiresAtMs": pending.expires_at_ms });
    }
    let consumed = match device_api_response(agent.post(&format!("{base}/{}/consume", pending.request_id)).send_json(json!({
        "verifier": pending.verifier,
        "deviceSync": true
    }))).and_then(device_api_data) {
        Ok(value) => value,
        Err(error) => return json!({ "ok": false, "status": "approved", "error": error }),
    };
    let device_sync_manager = state
        .device_sync_manager
        .lock()
        .ok()
        .and_then(|manager| manager.clone())
        .ok_or_else(|| "device_sync_unavailable".to_string());
    let device_sync_manager = match device_sync_manager {
        Ok(manager) => manager,
        Err(error) => {
            if let Ok(mut slot) = state.pending_device_authorization.lock() { *slot = None; }
            return json!({ "ok": false, "status": "device_sync_failed", "error": error });
        }
    };
    let device_sync = match device_sync_manager.connect_from_authorization(&consumed) {
        Ok(status) => status,
        Err(error) => {
            if let Ok(mut slot) = state.pending_device_authorization.lock() { *slot = None; }
            return json!({
                "ok": false,
                "status": "device_sync_failed",
                "error": if error.starts_with("device_sync_credential_") || error.contains("device_sync_dpapi_") {
                    "device_sync_credential_store_failed"
                } else {
                    "device_sync_connect_failed"
                }
            });
        }
    };
    let browser_links = match state.browser_links.lock().ok().and_then(|value| value.clone()) {
        Some(store) => store,
        None => {
            let _ = device_sync_manager.disconnect();
            return json!({ "ok": false, "status": "approved", "error": "browser_link_unavailable" });
        }
    };
    let link = match browser_links.issue_for_request(&pending.request_id, &consumed) {
        Ok(link) => link,
        Err(error) => {
            let _ = device_sync_manager.disconnect();
            return json!({ "ok": false, "status": "approved", "error": error });
        }
    };
    let background = Arc::clone(&device_sync_manager);
    thread::spawn(move || {
        let _ = background.run_once(false);
    });
    if let Ok(mut slot) = state.pending_device_authorization.lock() { *slot = None; }
    json!({
        "ok": true,
        "status": "connected",
        "tenantId": link.tenant_id,
        "tenantName": link.tenant_name,
        "accountEmail": link.account_email,
        "accountDisplayName": link.account_display_name,
        "deviceSyncConnected": true,
        "deviceSync": device_sync
    })
}

#[tauri::command]
fn get_cloud_sync_status(state: tauri::State<'_, AppState>) -> Value {
    state
        .sync_manager
        .lock()
        .ok()
        .and_then(|manager| manager.clone())
        .and_then(|manager| manager.status().ok())
        .unwrap_or_else(|| json!({ "ok": true, "connected": false }))
}

#[tauri::command]
fn get_device_connection_status(state: tauri::State<'_, AppState>) -> Value {
    state.browser_links.lock().ok().and_then(|store| store.clone()).and_then(|store| store.latest())
        .map(|link| json!({ "ok": true, "connected": true, "tenantId": link.tenant_id, "tenantName": link.tenant_name,
            "uid": link.uid, "accountEmail": link.account_email, "accountDisplayName": link.account_display_name,
            "connectedAtMs": link.created_at_ms }))
        .unwrap_or_else(|| json!({ "ok": true, "connected": false }))
}

#[tauri::command]
fn prepare_teacher_home_bridge(state: tauri::State<'_, AppState>) -> Value {
    let browser_links = match state
        .browser_links
        .lock()
        .ok()
        .and_then(|store| store.clone())
    {
        Some(store) => store,
        None => return json!({ "ok": false, "connected": false, "error": "browser_link_unavailable" }),
    };
    let request_id = random_url_token();
    match browser_links.issue_desktop_for_request(&request_id) {
        Ok(link) => json!({
            "ok": true,
            "connected": true,
            "requestId": request_id,
            "tenantId": link.tenant_id,
            "tenantName": link.tenant_name,
            "expiresAtMs": now_ms() + DESKTOP_BROWSER_LINK_PICKUP_TTL_MS,
        }),
        Err(error) if error == "browser_link_missing" => {
            json!({ "ok": true, "connected": false })
        }
        Err(error) => json!({ "ok": false, "connected": false, "error": error }),
    }
}

#[tauri::command]
fn get_device_sync_status(state: tauri::State<'_, AppState>) -> Value {
    let manager = state
        .device_sync_manager
        .lock()
        .ok()
        .and_then(|manager| manager.clone())
        .ok_or_else(|| "device_sync_unavailable".to_string());
    match manager.and_then(|manager| manager.status()) {
        Ok(status) => status,
        Err(error) => json!({ "ok": false, "connected": false, "error": error }),
    }
}

#[tauri::command]
fn run_device_sync_now(state: tauri::State<'_, AppState>) -> Value {
    let manager = state
        .device_sync_manager
        .lock()
        .ok()
        .and_then(|manager| manager.clone())
        .ok_or_else(|| "device_sync_unavailable".to_string());
    match manager.and_then(|manager| manager.run_once(true)) {
        Ok(status) => status,
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

#[tauri::command]
fn disconnect_local_store(state: tauri::State<'_, AppState>) -> Value {
    let cloud_sync_result = state
        .sync_manager
        .lock()
        .ok()
        .and_then(|manager| manager.clone())
        .map(|manager| manager.disconnect())
        .unwrap_or_else(|| Ok(json!({ "ok": true, "connected": false })));
    let device_sync_result = state
        .device_sync_manager
        .lock()
        .ok()
        .and_then(|manager| manager.clone())
        .map(|manager| manager.disconnect())
        .unwrap_or_else(|| Ok(json!({ "ok": true, "connected": false })));
    let link_result = state
        .browser_links
        .lock()
        .ok()
        .and_then(|store| store.clone())
        .map(|store| store.revoke_all())
        .unwrap_or(Ok(()));
    if let Ok(mut pending) = state.pending_device_authorization.lock() {
        *pending = None;
    }
    match (cloud_sync_result, device_sync_result, link_result) {
        (Ok(_), Ok(_), Ok(())) => json!({ "ok": true, "connected": false, "localDataPreserved": true }),
        (cloud_sync, device_sync, links) => json!({
            "ok": false,
            "connected": false,
            "localDataPreserved": true,
            "error": device_sync.err()
                .or_else(|| cloud_sync.err())
                .or_else(|| links.err())
                .unwrap_or_else(|| "disconnect_failed".to_string())
        }),
    }
}

#[tauri::command]
fn get_desktop_preferences(state: tauri::State<'_, AppState>) -> Value {
    let value = state.preferences.snapshot();
    json!({
        "ok": true,
        "startWithWindows": value.start_with_windows,
        "keepRunningOnClose": value.keep_running_on_close
    })
}

#[tauri::command]
fn set_desktop_preference(
    state: tauri::State<'_, AppState>,
    key: String,
    enabled: bool,
) -> Value {
    match state.preferences.set(&key, enabled) {
        Ok(value) => json!({
            "ok": true,
            "startWithWindows": value.start_with_windows,
            "keepRunningOnClose": value.keep_running_on_close
        }),
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

#[tauri::command]
fn run_cloud_sync(state: tauri::State<'_, AppState>) -> Value {
    state
        .sync_manager
        .lock()
        .ok()
        .and_then(|manager| manager.clone())
        .and_then(|manager| manager.run_once().ok())
        .unwrap_or_else(|| json!({ "ok": false, "connected": false, "error": "cloud_sync_unavailable" }))
}

#[tauri::command]
fn get_backup_status(state: tauri::State<'_, AppState>, tenant_id: String) -> Value {
    state
        .store
        .lock()
        .ok()
        .and_then(|store| store.clone())
        .and_then(|store| backup::status(&store, tenant_id).ok())
        .unwrap_or_else(|| json!({ "ok": false, "error": "backup_unavailable" }))
}

#[tauri::command]
fn set_backup_folder(state: tauri::State<'_, AppState>, tenant_id: String, folder_path: String) -> Value {
    state
        .store
        .lock()
        .ok()
        .and_then(|store| store.clone())
        .and_then(|store| backup::set_folder(&store, tenant_id, folder_path).ok())
        .unwrap_or_else(|| json!({ "ok": false, "error": "backup_folder_set_failed" }))
}

#[tauri::command]
fn run_local_backup(state: tauri::State<'_, AppState>, tenant_id: String) -> Value {
    state
        .store
        .lock()
        .ok()
        .and_then(|store| store.clone())
        .and_then(|store| backup::run_now(&store, tenant_id).ok())
        .unwrap_or_else(|| json!({ "ok": false, "error": "backup_run_failed" }))
}

#[tauri::command]
fn discover_backup_tenants(state: tauri::State<'_, AppState>, folder_path: String) -> Value {
    state
        .store
        .lock()
        .ok()
        .and_then(|store| store.clone())
        .and_then(|store| backup::discover_tenants(&store, folder_path).ok())
        .unwrap_or_else(|| json!({ "ok": false, "error": "backup_discovery_failed" }))
}

#[tauri::command]
fn list_local_backups(state: tauri::State<'_, AppState>, tenant_id: String, limit: i64) -> Value {
    state
        .store
        .lock()
        .ok()
        .and_then(|store| store.clone())
        .and_then(|store| backup::list_backups(&store, tenant_id, limit).ok())
        .unwrap_or_else(|| json!({ "ok": false, "backups": [], "error": "backup_list_failed" }))
}

#[tauri::command]
fn preview_local_backup_restore(
    state: tauri::State<'_, AppState>,
    tenant_id: String,
    manifest_path: String,
) -> Value {
    let store = match state.store.lock().ok().and_then(|store| store.clone()) {
        Some(store) => store,
        None => return json!({ "ok": false, "error": "local_store_unavailable" }),
    };
    match backup::restore_preview(
        &store,
        json!({ "tenantId": tenant_id, "manifestPath": manifest_path }),
    ) {
        Ok(value) => value,
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

#[tauri::command]
fn restore_local_backup(
    state: tauri::State<'_, AppState>,
    tenant_id: String,
    manifest_path: String,
) -> Value {
    let store = match state.store.lock().ok().and_then(|store| store.clone()) {
        Some(store) => store,
        None => return json!({ "ok": false, "error": "local_store_unavailable" }),
    };
    match backup::restore(
        &store,
        json!({ "tenantId": tenant_id, "manifestPath": manifest_path }),
    ) {
        Ok(value) => value,
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

#[tauri::command]
fn get_local_overview(state: tauri::State<'_, AppState>, tenant_id: String) -> Value {
    state
        .store
        .lock()
        .ok()
        .and_then(|store| store.clone())
        .and_then(|store| store.overview(tenant_id).ok())
        .unwrap_or_else(|| json!({ "ok": false, "error": "local_overview_failed" }))
}

#[tauri::command]
fn list_local_data_section(
    state: tauri::State<'_, AppState>,
    tenant_id: String,
    route: String,
    limit: i64,
) -> Value {
    let safe_route = normalize(&route, 120).trim_end_matches('/').to_string();
    let safe_limit = limit.clamp(1, 10000);
    let store = match state
        .store
        .lock()
        .ok()
        .and_then(|store| store.clone())
    {
        Some(store) => store,
        None => return json!({ "ok": false, "error": "local_store_unavailable" }),
    };
    let result = match safe_route.as_str() {
        "/v1/observations" => store.list_observations(tenant_id.clone(), String::new(), String::new(), String::new(), 0, String::new()),
        "/v1/teacher-counseling-sessions" => store.list_teacher_counseling_sessions(tenant_id.clone(), String::new(), String::new(), false, safe_limit),
        "/v1/student-private-details" => store.list_student_private_details(tenant_id.clone(), String::new()),
        "/v1/math-daily/attempts" => store.list_math_daily_attempts(tenant_id.clone(), String::new(), String::new(), String::new(), String::new(), String::new()),
        "/v1/board-post-snapshots" => store.list_board_snapshots(tenant_id.clone(), String::new(), String::new()),
        "/v1/board-media" => store.list_board_media(tenant_id.clone(), String::new(), String::new()),
        "/v1/attendance-records" => store.list_attendance_records(tenant_id.clone(), String::new(), String::new(), String::new(), String::new(), safe_limit),
        "/v1/attendance-nais-checks" => store.list_attendance_nais_checks(tenant_id.clone(), String::new(), String::new(), String::new(), String::new(), safe_limit),
        "/v1/attendance-document-requests" => store.list_attendance_document_requests(tenant_id.clone(), String::new(), String::new(), String::new(), String::new(), safe_limit),
        "/v1/counseling-records" => store.list_counseling_records(tenant_id.clone(), String::new(), String::new(), safe_limit),
        "/v1/counseling-teacher-notes" => store.list_counseling_teacher_notes(tenant_id.clone(), String::new(), safe_limit),
        "/v1/eval-assignments" => store.list_eval_assignments(tenant_id.clone(), String::new(), String::new(), String::new(), safe_limit),
        "/v1/eval-results" => store.list_eval_results(tenant_id.clone(), String::new(), String::new(), String::new(), String::new(), safe_limit),
        "/v1/student-record-draft-sets" => store.list_student_record_draft_sets(tenant_id.clone(), String::new(), String::new(), safe_limit),
        "/v1/student-record-drafts" => store.list_student_record_drafts(tenant_id.clone(), String::new(), String::new(), String::new(), safe_limit),
        "/v1/import-runs" => store.list_import_runs(tenant_id, String::new(), String::new(), safe_limit),
        _ => Err("local_data_section_unsupported".to_string()),
    };
    match result {
        Ok(records) => json!({ "ok": true, "records": records }),
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

#[tauri::command]
fn open_teacher_settings_url(url: String) -> Value {
    let safe_url = match normalize_teacher_settings_url(&url) {
        Ok(safe_url) => safe_url,
        Err(error) => return json!({ "ok": false, "error": error }),
    };

    open_external_url(&safe_url)
}

fn open_external_url(url: &str) -> Value {
    #[cfg(target_os = "windows")]
    let result = Command::new("rundll32.exe")
        .arg("url.dll,FileProtocolHandler")
        .arg(url)
        .spawn();

    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg(url).spawn();

    #[cfg(all(unix, not(target_os = "macos")))]
    let result = Command::new("xdg-open").arg(url).spawn();

    match result {
        Ok(_) => json!({ "ok": true }),
        Err(error) => json!({ "ok": false, "error": format!("open_url_failed:{error}") }),
    }
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn should_start_background() -> bool {
    env::var("ONLINECLASS_LOCAL_STORE_BACKGROUND")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        || env::args().any(|arg| arg == "--background" || arg == "--hidden")
}

fn hide_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
}

fn setup_tray(app: &mut tauri::App) -> Result<(), tauri::Error> {
    let show_i = MenuItem::with_id(app, "show", "열기", true, None::<&str>)?;
    let sync_i = MenuItem::with_id(app, "sync", "지금 수거", true, None::<&str>)?;
    let quit_i = MenuItem::with_id(app, "quit", "종료", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_i, &sync_i, &quit_i])?;
    let mut builder = TrayIconBuilder::new()
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "sync" => {
                if let Some(state) = app.try_state::<AppState>() {
                    if let Some(manager) = state.sync_manager.lock().ok().and_then(|value| value.clone()) {
                        thread::spawn(move || {
                            let _ = manager.run_once();
                        });
                    }
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(&tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

#[cfg(test)]
mod device_authorization_tests {
    use super::*;

    #[test]
    fn device_tokens_are_url_safe_and_digest_is_stable() {
        let token = random_url_token();
        assert_eq!(token.len(), 43);
        assert!(token.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_')));
        assert_eq!(sha256_hex("verifier"), "88c9eae68eb300b2971a2bec9e5a26ff4179fd661d6b7d861e4c6557b9aaee14");
    }

    #[test]
    fn browser_approval_url_is_restricted_to_connect_local() {
        assert!(normalize_teacher_settings_url("https://t.classaimate.com/connect-local?requestId=abc").is_ok());
        assert_eq!(normalize_teacher_settings_url("https://t.classaimate.com/admin/settings").unwrap_err(), "url_not_allowed");
        assert_eq!(normalize_teacher_settings_url("https://example.com/connect-local").unwrap_err(), "url_not_allowed");
    }

    #[test]
    fn browser_link_for_request_is_retryable_during_pickup_ttl() {
        let directory = env::temp_dir().join(format!("onlineclass-device-auth-test-{}", random_url_token()));
        let store = BrowserLinkStore::open(&directory).expect("open browser link store");
        let request_id = random_url_token();
        let input = json!({ "tenantId": "tenant-a", "uid": "teacher-a", "accountEmail": "teacher@example.com", "tenantName": "학급" });
        let issued = store.issue_for_request(&request_id, &input).expect("issue browser link");
        assert_eq!(store.read_for_request(&request_id).expect("read browser link").expect("present").token, issued.token);
        assert_eq!(store.read_for_request(&request_id).expect("read again").expect("still present").token, issued.token);
        fs::remove_dir_all(&directory).expect("remove test directory");
    }

    #[test]
    fn staged_browser_link_keeps_previous_token_until_first_authenticated_use() {
        let directory = env::temp_dir().join(format!("onlineclass-device-auth-handoff-test-{}", random_url_token()));
        let store = BrowserLinkStore::open(&directory).expect("open browser link store");
        let input = json!({ "tenantId": "tenant-a", "uid": "teacher-a", "accountEmail": "teacher@example.com", "tenantName": "학급" });
        let previous = store.issue(&input).expect("issue previous link");
        let request_id = random_url_token();
        let staged = store.issue_for_request(&request_id, &input).expect("issue staged link");

        assert_eq!(store.authorize_token(&previous.token).as_deref(), Some("tenant-a"));
        assert!(store.read_for_request(&request_id).expect("read staged link").is_some());
        assert_eq!(store.authorize_token(&staged.token).as_deref(), Some("tenant-a"));
        assert!(store.authorize_token(&previous.token).is_none());
        assert_eq!(store.authorize_token(&staged.token).as_deref(), Some("tenant-a"));
        fs::remove_dir_all(&directory).expect("remove test directory");
    }

    #[test]
    fn desktop_bridge_uses_a_new_token_without_revoking_existing_browsers() {
        let directory = env::temp_dir().join(format!(
            "onlineclass-desktop-bridge-test-{}",
            random_url_token()
        ));
        let store = BrowserLinkStore::open(&directory).expect("open browser link store");
        let previous = store
            .issue(&json!({
                "tenantId": "tenant-a",
                "uid": "teacher-a",
                "accountEmail": "teacher@example.com",
                "tenantName": "학급"
            }))
            .expect("issue previous link");
        let request_id = random_url_token();
        let desktop = store
            .issue_desktop_for_request(&request_id)
            .expect("issue desktop bridge link");

        assert_ne!(previous.token, desktop.token);
        assert_eq!(
            store.authorize_token(&desktop.token).as_deref(),
            Some("tenant-a")
        );
        assert_eq!(
            store.authorize_token(&previous.token).as_deref(),
            Some("tenant-a")
        );
        assert!(store
            .read_for_request(&request_id)
            .expect("read consumed bridge")
            .is_none());

        let next_request_id = random_url_token();
        let next_desktop = store
            .issue_desktop_for_request(&next_request_id)
            .expect("replace desktop bridge link");
        assert_ne!(desktop.token, next_desktop.token);
        assert!(store.authorize_token(&desktop.token).is_none());
        assert_eq!(
            store.authorize_token(&next_desktop.token).as_deref(),
            Some("tenant-a")
        );
        assert_eq!(
            store.authorize_token(&previous.token).as_deref(),
            Some("tenant-a")
        );
        assert_eq!(store.tokens.lock().expect("browser token lock").len(), 2);
        fs::remove_dir_all(&directory).expect("remove test directory");
    }

    #[test]
    fn browser_link_revoke_all_keeps_local_data_files() {
        let directory = env::temp_dir().join(format!("onlineclass-browser-link-revoke-test-{}", random_url_token()));
        let store = BrowserLinkStore::open(&directory).expect("open browser link store");
        let local_data = directory.join(DB_FILE_NAME);
        fs::write(&local_data, b"local-data-must-remain").expect("write local data sentinel");
        store.issue(&json!({
            "tenantId": "tenant-a",
            "uid": "teacher-a",
            "accountEmail": "teacher@example.com",
            "tenantName": "학급"
        })).expect("issue browser link");
        assert!(store.latest().is_some());
        store.revoke_all().expect("revoke browser links");
        assert!(store.latest().is_none());
        assert_eq!(fs::read(&local_data).expect("read local data sentinel"), b"local-data-must-remain");
        fs::remove_dir_all(&directory).expect("remove test directory");
    }

    #[test]
    fn browser_token_scope_overrides_missing_tenant_and_rejects_mismatch() {
        assert_eq!(scope_tenant_id(String::new(), Some("tenant-a")).unwrap(), "tenant-a");
        assert_eq!(
            scope_tenant_id("tenant-b".to_string(), Some("tenant-a")).unwrap_err(),
            "tenant_scope_mismatch"
        );
        let scoped = scope_body_to_tenant(json!({ "recordId": "record-a" }), Some("tenant-a")).unwrap();
        assert_eq!(scoped.get("tenantId").and_then(Value::as_str), Some("tenant-a"));
        assert_eq!(
            scope_body_to_tenant(json!({ "tenantId": "tenant-b" }), Some("tenant-a")).unwrap_err(),
            "tenant_scope_mismatch"
        );
    }

    #[test]
    fn work_meeting_root_reconcile_is_atomic_and_preserves_user_content() {
        let root = env::temp_dir().join(format!("onlineclass-work-note-reconcile-{}-{}", std::process::id(), now_ms()));
        fs::create_dir_all(&root).expect("create work note test directory");
        let store = SqliteStore::open(root.join("store.sqlite")).expect("open work note store");
        let system_root = |page_id: &str, blocks: Value| json!({
            "tenantId":"tenant-a","pageId":page_id,"parentId":null,"title":WORK_MEETING_ROOT_TITLE,"emoji":"🗂️","position":0,
            "properties":{"systemKind":"mobile_work_meeting_folder","schemaVersion":1},"blocks":blocks,"markdown":format!("# {WORK_MEETING_ROOT_TITLE}")
        });
        store.upsert_work_note(system_root("duplicate:encoded root", json!([]))).expect("save blank duplicate");
        store.upsert_work_note(system_root("duplicate:user root", json!([{"id":"user","type":"text","text":"보존할 메모"}]))).expect("save modified duplicate");
        store.upsert_work_note(json!({
            "tenantId":"tenant-a","pageId":"classaimate:work-meeting:draft-1","parentId":"duplicate:encoded root","title":"회의","emoji":"📝","position":1,
            "properties":{"systemKind":"mobile_work_meeting"},"blocks":[{"id":"body","type":"text","text":"회의 내용"}],"markdown":"# 회의"
        })).expect("save meeting child");
        let result = store.reconcile_mobile_meeting_root("tenant-a".to_string(), true).expect("reconcile roots");
        assert_eq!(result["deduplicated"], 1);
        assert_eq!(result["preserved"], 1);
        let records = result["records"].as_array().expect("records");
        assert!(records.iter().any(|page| page["pageId"] == WORK_MEETING_ROOT_PAGE_ID));
        assert!(records.iter().any(|page| page["pageId"] == "duplicate:user root"));
        assert!(!records.iter().any(|page| page["pageId"] == "duplicate:encoded root"));
        assert_eq!(records.iter().find(|page| page["pageId"] == "classaimate:work-meeting:draft-1").expect("moved child")["parentId"], WORK_MEETING_ROOT_PAGE_ID);
        drop(store);
        fs::remove_dir_all(root).expect("remove work note test directory");
    }

    #[test]
    fn work_note_tree_move_reindexes_siblings_and_rejects_cycles() {
        let root = env::temp_dir().join(format!("onlineclass-work-note-move-{}-{}", std::process::id(), now_ms()));
        fs::create_dir_all(&root).expect("create work note move directory");
        let store = SqliteStore::open(root.join("store.sqlite")).expect("open work note store");
        for (page_id,parent_id,position) in [("root",None,0),("a",Some("root"),2),("b",Some("root"),2),("c",Some("root"),2),("d",Some("a"),0)] {
            store.upsert_work_note(json!({"tenantId":"tenant-a","pageId":page_id,"parentId":parent_id,"title":page_id,"blocks":[],"markdown":"","position":position})).expect("save page");
        }
        store.move_work_note(json!({"tenantId":"tenant-a","pageId":"c","targetPageId":"a","placement":"before"})).expect("move before");
        store.move_work_note(json!({"tenantId":"tenant-a","pageId":"b","targetPageId":"a","placement":"inside"})).expect("move inside");
        let result=store.move_work_note(json!({"tenantId":"tenant-a","pageId":"c","targetPageId":"b","placement":"after"})).expect("move after");
        let children=result["records"].as_array().expect("records").iter().filter(|page|page["parentId"]=="a").map(|page|(page["pageId"].as_str().unwrap().to_string(),page["position"].as_i64().unwrap())).collect::<Vec<_>>();
        assert_eq!(children,vec![("d".to_string(),0),("b".to_string(),1),("c".to_string(),2)]);
        assert_eq!(store.move_work_note(json!({"tenantId":"tenant-a","pageId":"a","targetPageId":"d","placement":"inside"})).unwrap_err(),"work_note_parent_cycle");
        drop(store);fs::remove_dir_all(root).expect("remove work note move directory");
    }

    #[test]
    fn counseling_import_replay_isolation_and_validation() {
        let root = env::temp_dir().join(format!(
            "onlineclass-counseling-rust-store-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&root).expect("create counseling store directory");
        let store = SqliteStore::open(root.join("store.sqlite")).expect("open counseling store");
        let backup_root = env::temp_dir().join(format!(
            "onlineclass-counseling-rust-backup-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let restore_root = env::temp_dir().join(format!(
            "onlineclass-counseling-rust-restore-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&backup_root).expect("create counseling backup directory");
        fs::create_dir_all(&restore_root).expect("create counseling restore directory");
        let source = json!({
            "tenantId": "tenant-a",
            "records": [
                {
                    "requestId": "req-1",
                    "studentCode": "s01",
                    "studentName": "학생 1",
                    "content": "상담 요청 1",
                    "status": "unread",
                    "createdAtMs": 1000,
                    "updatedAtMs": 1000
                },
                {
                    "requestId": "req-2",
                    "studentCode": "s02",
                    "content": "상담 요청 2",
                    "status": "read",
                    "createdAtMs": 1001,
                    "updatedAtMs": 1001
                }
            ],
            "teacherNotes": [{
                "requestId": "req-1",
                "teacherNote": "교사 메모",
                "updatedAtMs": 1002
            }]
        });
        let prepared = prepare_counseling_snapshot(&source).expect("prepare counseling source");
        assert_eq!(
            prepared.source_snapshot_sha256,
            "277a9cd841e1ea400a798eff1924e88081f7c9d63dcfe6c296676b656ef900fe"
        );
        let mut import = source.clone();
        import["runId"] = json!("run-1");
        import["sourceSnapshotSha256"] = json!(prepared.source_snapshot_sha256);

        let first = store
            .import_counseling_snapshot(import.clone())
            .expect("first counseling import");
        assert_eq!(first["replayed"], false);
        assert_eq!(first["compare"]["matches"], true);
        let replay = store
            .import_counseling_snapshot(import)
            .expect("exact counseling replay");
        assert_eq!(replay["replayed"], true);
        assert_eq!(store.stats("tenant-a".to_string()).unwrap()["counselingRecordCount"], 2);
        assert!(store
            .get_counseling_record("tenant-b".to_string(), "req-1".to_string())
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .upsert_counseling_record(json!({
                    "tenantId": "tenant-a",
                    "requestId": "bad-status",
                    "studentCode": "s03",
                    "content": "invalid",
                    "status": "closed"
                }))
                .unwrap_err(),
            "counseling_status_invalid"
        );
        assert_eq!(
            store
                .upsert_counseling_teacher_note(json!({
                    "tenantId": "tenant-a",
                    "requestId": "missing",
                    "teacherNote": "orphan"
                }))
                .unwrap_err(),
            "counseling_record_not_found"
        );
        backup::set_folder(
            &store,
            "tenant-a".to_string(),
            backup_root.to_string_lossy().to_string(),
        )
        .expect("configure counseling backup");
        let backup_result = backup::run_now(&store, "tenant-a".to_string())
            .expect("run counseling backup");
        assert_eq!(backup_result["counts"]["counselingRecordCount"], 2);
        assert_eq!(backup_result["counts"]["counselingTeacherNoteCount"], 1);
        let manifest_path = backup_result["manifestPath"]
            .as_str()
            .expect("counseling backup manifest path")
            .to_string();
        let restored = SqliteStore::open(restore_root.join("store.sqlite"))
            .expect("open counseling restore store");
        backup::set_folder(
            &restored,
            "tenant-a".to_string(),
            backup_root.to_string_lossy().to_string(),
        )
        .expect("configure counseling restore backup root");
        let preview = backup::restore_preview(
            &restored,
            json!({ "tenantId": "tenant-a", "manifestPath": manifest_path }),
        )
        .expect("preview counseling restore");
        assert_eq!(preview["counts"]["counseling_records"], 2);
        assert_eq!(preview["counts"]["counseling_teacher_notes"], 1);
        backup::restore(
            &restored,
            json!({ "tenantId": "tenant-a", "manifestPath": manifest_path }),
        )
        .expect("restore counseling backup");
        assert_eq!(
            restored.stats("tenant-a".to_string()).unwrap()["counselingRecordCount"],
            2
        );
        drop(store);
        drop(restored);
        fs::remove_dir_all(root).expect("remove counseling test directory");
        fs::remove_dir_all(backup_root).expect("remove counseling backup directory");
        fs::remove_dir_all(restore_root).expect("remove counseling restore directory");
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let keep_running_on_close = window
                    .app_handle()
                    .try_state::<AppState>()
                    .map(|state| state.preferences.snapshot().keep_running_on_close)
                    .unwrap_or(true);
                if keep_running_on_close {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .setup(|app| {
            let preferences = desktop_preferences::DesktopPreferencesStore::open(&default_data_dir());
            if let Err(error) = preferences.apply_startup_setting() {
                eprintln!("[local-sensitive-store] autostart setup failed: {error}");
            }
            let started = start_service();
            let (status, store, sync_manager, device_sync_manager, browser_links) = match started {
                Ok((status, store, sync_manager, device_sync_manager, browser_links)) => {
                    sync_manager.start_background_sync();
                    device_sync_manager.start_background();
                    backup::start_background(Arc::clone(&store));
                    (
                        status,
                        Some(store),
                        Some(sync_manager),
                        Some(device_sync_manager),
                        Some(browser_links),
                    )
                }
                Err(error) => (ServiceStatus::failed(error), None, None, None, None),
            };
            app.manage(AppState {
                status: Mutex::new(status),
                store: Mutex::new(store),
                sync_manager: Mutex::new(sync_manager),
                device_sync_manager: Mutex::new(device_sync_manager),
                browser_links: Mutex::new(browser_links),
                pending_device_authorization: Mutex::new(None),
                preferences,
            });
            if let Err(error) = setup_tray(app) {
                eprintln!("[local-sensitive-store] tray setup failed: {error}");
            }
            if should_start_background() {
                hide_main_window(&app.handle());
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_service_status,
            get_cloud_sync_status,
            get_device_connection_status,
            prepare_teacher_home_bridge,
            get_device_sync_status,
            run_device_sync_now,
            disconnect_local_store,
            get_desktop_preferences,
            set_desktop_preference,
            run_cloud_sync,
            get_backup_status,
            set_backup_folder,
            run_local_backup,
            discover_backup_tenants,
            list_local_backups,
            preview_local_backup_restore,
            restore_local_backup,
            get_local_overview,
            list_local_data_section,
            data_explorer::list_local_students,
            data_explorer::search_local_data,
            data_explorer::open_local_data_attachment,
            data_explorer::open_local_data_directory,
            open_teacher_settings_url,
            start_device_authorization,
            reopen_device_authorization,
            poll_device_authorization,
            shared_archive::import_shared_archive,
            shared_archive::list_shared_archives,
            shared_archive::get_shared_archive,
            shared_archive::export_shared_archive,
            shared_archive::open_shared_archive_file
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
