use super::{normalize, normalize_tenant_id, AppState, SqliteStore};
use rusqlite::{params, params_from_iter, types::Value as SqlValue};
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const MAX_PAGE_SIZE: i64 = 100;
const WORK_NOTE_ATTACHMENT_EXPRESSION: &str =
    "EXISTS (SELECT 1 FROM work_note_attachments a WHERE a.tenant_id = work_note_pages.tenant_id AND a.page_id = work_note_pages.page_id)";
const WORK_NOTE_PAYLOAD_EXPRESSION: &str = r#"json_object(
    'pageId', page_id,
    'parentId', parent_id,
    'title', title,
    'emoji', emoji,
    'properties', json(properties_json),
    'blocks', json(document_json),
    'markdown', markdown,
    'attachments', json(COALESCE((
        SELECT json_group_array(json_object(
            'attachmentId', a.attachment_id,
            'mediaId', a.attachment_id,
            'attachmentKind', 'work-note',
            'fileName', a.file_name,
            'contentType', a.content_type,
            'size', a.byte_size
        ))
        FROM work_note_attachments a
        WHERE a.tenant_id = work_note_pages.tenant_id AND a.page_id = work_note_pages.page_id
    ), '[]'))
)"#;

#[derive(Clone, Copy)]
struct SearchSource {
    key: &'static str,
    label: &'static str,
    group: &'static str,
    table: &'static str,
    sort_column: &'static str,
    date_expression: &'static str,
    attachment_expression: &'static str,
    student_column: &'static str,
}

impl SearchSource {
    fn payload_expression(self) -> &'static str {
        if self.key == "work-notes" {
            WORK_NOTE_PAYLOAD_EXPRESSION
        } else {
            "payload_json"
        }
    }

    fn search_expression(self) -> &'static str {
        if self.key == "work-notes" {
            "title || ' ' || markdown || ' ' || properties_json"
        } else {
            "payload_json"
        }
    }
}

const SEARCH_SOURCES: &[SearchSource] = &[
    SearchSource {
        key: "observations",
        label: "수업 관찰",
        group: "care",
        table: "lesson_observations",
        sort_column: "updated_at_ms",
        date_expression: "date_key",
        attachment_expression: "0",
        student_column: "student_code",
    },
    SearchSource {
        key: "teacher-counseling-sessions",
        label: "교사 상담기록",
        group: "care",
        table: "teacher_counseling_sessions",
        sort_column: "updated_at_ms",
        date_expression: "strftime('%Y-%m-%d', counseling_at_ms / 1000, 'unixepoch', 'localtime')",
        attachment_expression: "0",
        student_column: "student_code",
    },
    SearchSource {
        key: "student-private-details",
        label: "학생 민감정보",
        group: "care",
        table: "student_private_details",
        sort_column: "updated_at_ms",
        date_expression: "strftime('%Y-%m-%d', updated_at_ms / 1000, 'unixepoch', 'localtime')",
        attachment_expression: "0",
        student_column: "student_code",
    },
    SearchSource {
        key: "attendance-records",
        label: "출결 기록",
        group: "attendance",
        table: "attendance_records",
        sort_column: "updated_at_ms",
        date_expression: "date_key",
        attachment_expression:
            "(lower(payload_json) LIKE '%\"attachment%' OR lower(payload_json) LIKE '%\"file%')",
        student_column: "student_code",
    },
    SearchSource {
        key: "attendance-nais-checks",
        label: "출결 NEIS 확인",
        group: "attendance",
        table: "attendance_nais_checks",
        sort_column: "updated_at_ms",
        date_expression: "date_key",
        attachment_expression:
            "(lower(payload_json) LIKE '%\"attachment%' OR lower(payload_json) LIKE '%\"file%')",
        student_column: "student_code",
    },
    SearchSource {
        key: "attendance-document-requests",
        label: "출결 증빙 요청",
        group: "attendance",
        table: "attendance_document_requests",
        sort_column: "updated_at_ms",
        date_expression: "date_key",
        attachment_expression:
            "(lower(payload_json) LIKE '%\"attachment%' OR lower(payload_json) LIKE '%\"file%')",
        student_column: "student_code",
    },
    SearchSource {
        key: "math-daily-attempts",
        label: "매일수학 시도",
        group: "learning",
        table: "math_daily_attempts",
        sort_column: "updated_at_ms",
        date_expression: "date_key",
        attachment_expression: "0",
        student_column: "student_code",
    },
    SearchSource {
        key: "eval-assignments",
        label: "평가 운영",
        group: "learning",
        table: "eval_assignments",
        sort_column: "updated_at_ms",
        date_expression: "scheduled_date",
        attachment_expression: "0",
        student_column: "",
    },
    SearchSource {
        key: "eval-results",
        label: "평가 기록",
        group: "learning",
        table: "eval_results",
        sort_column: "updated_at_ms",
        date_expression: "date_key",
        attachment_expression: "0",
        student_column: "student_id",
    },
    SearchSource {
        key: "board-post-snapshots",
        label: "게시판 스냅샷",
        group: "learning",
        table: "board_post_snapshots",
        sort_column: "updated_at_ms",
        date_expression: "strftime('%Y-%m-%d', updated_at_ms / 1000, 'unixepoch', 'localtime')",
        attachment_expression:
            "(lower(payload_json) LIKE '%\"attachment%' OR lower(payload_json) LIKE '%\"file%')",
        student_column: "",
    },
    SearchSource {
        key: "board-media",
        label: "게시판 첨부파일",
        group: "learning",
        table: "board_media_files",
        sort_column: "archived_at_ms",
        date_expression: "strftime('%Y-%m-%d', archived_at_ms / 1000, 'unixepoch', 'localtime')",
        attachment_expression: "1",
        student_column: "",
    },
    SearchSource {
        key: "student-record-draft-sets",
        label: "학생부 초안 세트",
        group: "student-record",
        table: "student_record_draft_sets",
        sort_column: "updated_at_ms",
        date_expression: "to_date",
        attachment_expression: "0",
        student_column: "",
    },
    SearchSource {
        key: "student-record-drafts",
        label: "학생부 초안",
        group: "student-record",
        table: "student_record_drafts",
        sort_column: "updated_at_ms",
        date_expression: "strftime('%Y-%m-%d', updated_at_ms / 1000, 'unixepoch', 'localtime')",
        attachment_expression: "0",
        student_column: "student_code",
    },
    SearchSource {
        key: "work-notes",
        label: "업무 노트",
        group: "work-notes",
        table: "work_note_pages",
        sort_column: "updated_at_ms",
        date_expression:
            "strftime('%Y-%m-%d', updated_at_ms / 1000, 'unixepoch', 'localtime')",
        attachment_expression: WORK_NOTE_ATTACHMENT_EXPRESSION,
        student_column: "",
    },
];

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LocalDataSearchInput {
    tenant_id: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    section_key: String,
    #[serde(default)]
    student_id: String,
    #[serde(default)]
    student_query: String,
    #[serde(default)]
    text_query: String,
    #[serde(default)]
    date_from: String,
    #[serde(default)]
    date_to: String,
    #[serde(default)]
    has_attachment: bool,
    #[serde(default)]
    offset: i64,
    #[serde(default = "default_limit")]
    limit: i64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LocalStudentListInput {
    tenant_id: String,
    #[serde(default)]
    query: String,
    #[serde(default)]
    offset: i64,
    #[serde(default = "default_student_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    40
}

fn default_student_limit() -> i64 {
    200
}

fn valid_date(value: &str) -> bool {
    value.is_empty()
        || (value.len() == 10
            && value.as_bytes()[4] == b'-'
            && value.as_bytes()[7] == b'-'
            && chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok())
}

fn selected_sources(group: &str, section_key: &str) -> Result<Vec<SearchSource>, String> {
    let safe_group = normalize(group, 40);
    let safe_section = normalize(section_key, 80);
    if !safe_group.is_empty()
        && !matches!(
            safe_group.as_str(),
            "care" | "attendance" | "learning" | "student-record" | "work-notes"
        )
    {
        return Err("local_data_group_invalid".to_string());
    }
    let sources: Vec<SearchSource> = SEARCH_SOURCES
        .iter()
        .copied()
        .filter(|source| safe_group.is_empty() || source.group == safe_group)
        .filter(|source| safe_section.is_empty() || source.key == safe_section)
        .collect();
    if !safe_section.is_empty() && sources.is_empty() {
        return Err("local_data_section_unsupported".to_string());
    }
    Ok(sources)
}

fn build_union_query(input: &LocalDataSearchInput) -> Result<(String, Vec<SqlValue>), String> {
    let tenant_id = normalize_tenant_id(Some(&Value::String(input.tenant_id.clone())));
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let text_query = normalize(&input.text_query, 160);
    let student_query = normalize(&input.student_query, 120);
    let student_id = normalize(&input.student_id, 160);
    let date_from = normalize(&input.date_from, 10);
    let date_to = normalize(&input.date_to, 10);
    if !valid_date(&date_from)
        || !valid_date(&date_to)
        || (!date_from.is_empty() && !date_to.is_empty() && date_from > date_to)
    {
        return Err("date_filter_invalid".to_string());
    }

    let mut statements = Vec::new();
    let mut values = Vec::new();
    for source in selected_sources(&input.group, &input.section_key)? {
        if !student_id.is_empty() && source.student_column.is_empty() {
            continue;
        }
        let mut where_parts = vec!["tenant_id = ?".to_string()];
        values.push(SqlValue::Text(tenant_id.clone()));
        if !student_id.is_empty() {
            where_parts.push(format!("{} = ?", source.student_column));
            values.push(SqlValue::Text(student_id.clone()));
        }
        if !student_query.is_empty() {
            where_parts.push(format!("lower({}) LIKE ?", source.search_expression()));
            values.push(SqlValue::Text(format!(
                "%{}%",
                student_query.to_lowercase()
            )));
        }
        if !text_query.is_empty() {
            let pattern = format!("%{}%", text_query.to_lowercase());
            if source.key == "work-notes" {
                let terms = text_query
                    .split_whitespace()
                    .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
                    .collect::<Vec<_>>()
                    .join(" AND ");
                where_parts.push("(page_id IN (SELECT page_id FROM work_note_pages_fts WHERE tenant_id = ? AND work_note_pages_fts MATCH ?) OR lower(properties_json) LIKE ? OR EXISTS (SELECT 1 FROM work_note_attachments a WHERE a.tenant_id = work_note_pages.tenant_id AND a.page_id = work_note_pages.page_id AND lower(a.file_name) LIKE ?))".to_string());
                values.push(SqlValue::Text(tenant_id.clone()));
                values.push(SqlValue::Text(terms));
                values.push(SqlValue::Text(pattern.clone()));
                values.push(SqlValue::Text(pattern));
            } else {
                where_parts.push(format!("lower({}) LIKE ?", source.search_expression()));
                values.push(SqlValue::Text(pattern));
            }
        }
        if !date_from.is_empty() {
            where_parts.push(format!("{} >= ?", source.date_expression));
            values.push(SqlValue::Text(date_from.clone()));
        }
        if !date_to.is_empty() {
            where_parts.push(format!("{} <= ?", source.date_expression));
            values.push(SqlValue::Text(date_to.clone()));
        }
        if input.has_attachment {
            where_parts.push(source.attachment_expression.to_string());
        }
        statements.push(format!(
            "SELECT '{}' AS section_key, '{}' AS section_label, '{}' AS group_key, {} AS payload_json, {} AS sort_ms, {} AS date_key, {} AS has_attachment FROM {} WHERE {}",
            source.key,
            source.label,
            source.group,
            source.payload_expression(),
            source.sort_column,
            source.date_expression,
            source.attachment_expression,
            source.table,
            where_parts.join(" AND ")
        ));
    }
    if statements.is_empty() {
        return Ok(("SELECT '' AS section_key, '' AS section_label, '' AS group_key, '{}' AS payload_json, 0 AS sort_ms, '' AS date_key, 0 AS has_attachment WHERE 0".to_string(), values));
    }
    Ok((statements.join(" UNION ALL "), values))
}

impl SqliteStore {
    fn search_local_data(&self, input: LocalDataSearchInput) -> Result<Value, String> {
        let offset = input.offset.clamp(0, 100_000);
        let limit = input.limit.clamp(1, MAX_PAGE_SIZE);
        let (union_sql, values) = build_union_query(&input)?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let total: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM ({union_sql})"),
                params_from_iter(values.iter()),
                |row| row.get(0),
            )
            .map_err(|e| format!("local_data_count_failed:{e}"))?;

        let mut page_values = values;
        page_values.push(SqlValue::Integer(limit));
        page_values.push(SqlValue::Integer(offset));
        let page_sql = format!("SELECT section_key, section_label, group_key, payload_json, sort_ms, date_key, has_attachment FROM ({union_sql}) ORDER BY sort_ms DESC, section_key ASC LIMIT ? OFFSET ?");
        let mut stmt = conn
            .prepare(&page_sql)
            .map_err(|e| format!("local_data_search_prepare_failed:{e}"))?;
        let rows = stmt
            .query_map(params_from_iter(page_values.iter()), |row| {
                let payload_json: String = row.get(3)?;
                Ok(json!({
                    "sectionKey": row.get::<_, String>(0)?,
                    "sectionLabel": row.get::<_, String>(1)?,
                    "groupKey": row.get::<_, String>(2)?,
                    "payload": serde_json::from_str::<Value>(&payload_json).unwrap_or_else(|_| json!({})),
                    "updatedAtMs": row.get::<_, i64>(4)?,
                    "dateKey": row.get::<_, String>(5)?,
                    "hasAttachment": row.get::<_, i64>(6)? != 0
                }))
            })
            .map_err(|e| format!("local_data_search_failed:{e}"))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(|e| format!("local_data_search_row_failed:{e}"))?);
        }
        Ok(json!({
            "ok": true,
            "total": total,
            "offset": offset,
            "limit": limit,
            "hasMore": (offset + records.len() as i64) < total,
            "records": records
        }))
    }

    fn list_local_students(&self, input: LocalStudentListInput) -> Result<Value, String> {
        let tenant_id = normalize_tenant_id(Some(&Value::String(input.tenant_id)));
        if tenant_id.is_empty() {
            return Err("tenant_id_required".to_string());
        }
        let query = normalize(&input.query, 120).to_lowercase();
        let offset = input.offset.clamp(0, 10_000);
        let limit = input.limit.clamp(1, 200);
        let mut values = Vec::new();
        let mut statements = Vec::new();
        for source in SEARCH_SOURCES
            .iter()
            .filter(|source| !source.student_column.is_empty())
        {
            values.push(SqlValue::Text(tenant_id.clone()));
            statements.push(format!(
                "SELECT CAST({student} AS TEXT) AS student_id, COALESCE(NULLIF(CAST(json_extract(payload_json, '$.studentName') AS TEXT), ''), NULLIF(CAST(json_extract(payload_json, '$.displayName') AS TEXT), ''), NULLIF(CAST(json_extract(payload_json, '$.name') AS TEXT), ''), '') AS student_name, {sort} AS sort_ms FROM {table} WHERE tenant_id = ? AND trim(CAST({student} AS TEXT)) <> ''",
                student = source.student_column,
                sort = source.sort_column,
                table = source.table,
            ));
        }
        let union_sql = statements.join(" UNION ALL ");
        let aggregate_sql = format!(
            "SELECT student_id, MAX(student_name) AS student_name, COUNT(*) AS record_count, MAX(sort_ms) AS last_updated_ms FROM ({union_sql}) GROUP BY student_id"
        );
        let mut filtered_values = values.clone();
        let filtered_sql = if query.is_empty() {
            aggregate_sql
        } else {
            filtered_values.push(SqlValue::Text(format!("%{query}%")));
            filtered_values.push(SqlValue::Text(format!("%{query}%")));
            format!(
                "SELECT * FROM ({aggregate_sql}) WHERE lower(student_id) LIKE ? OR lower(student_name) LIKE ?"
            )
        };
        let conn = self.conn.lock().map_err(|_| "db_lock_failed".to_string())?;
        let total: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM ({filtered_sql})"),
                params_from_iter(filtered_values.iter()),
                |row| row.get(0),
            )
            .map_err(|e| format!("local_student_count_failed:{e}"))?;
        let mut page_values = filtered_values;
        page_values.push(SqlValue::Integer(limit));
        page_values.push(SqlValue::Integer(offset));
        let page_sql = format!(
            "SELECT student_id, student_name, record_count, last_updated_ms FROM ({filtered_sql}) ORDER BY CASE WHEN student_id <> '' AND student_id NOT GLOB '*[^0-9]*' THEN CAST(student_id AS INTEGER) ELSE 2147483647 END, student_id LIMIT ? OFFSET ?"
        );
        let mut stmt = conn
            .prepare(&page_sql)
            .map_err(|e| format!("local_student_list_prepare_failed:{e}"))?;
        let rows = stmt
            .query_map(params_from_iter(page_values.iter()), |row| {
                let student_id = row.get::<_, String>(0)?;
                let student_name = row.get::<_, String>(1)?;
                Ok(json!({
                    "studentId": student_id,
                    "studentName": student_name,
                    "recordCount": row.get::<_, i64>(2)?,
                    "lastUpdatedMs": row.get::<_, i64>(3)?,
                }))
            })
            .map_err(|e| format!("local_student_list_failed:{e}"))?;
        let mut students = Vec::new();
        for row in rows {
            students.push(row.map_err(|e| format!("local_student_list_row_failed:{e}"))?);
        }
        Ok(json!({
            "ok": true,
            "total": total,
            "offset": offset,
            "limit": limit,
            "students": students,
        }))
    }

    fn resolve_local_attachment(
        &self,
        tenant_id: String,
        media_id: String,
        attachment_kind: String,
    ) -> Result<(PathBuf, String), String> {
        let safe_tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
        let safe_media = normalize(&media_id, 220);
        if safe_tenant.is_empty() || safe_media.is_empty() || safe_media.contains(['/', '\\']) {
            return Err("media_identity_required".to_string());
        }
        let kind = normalize(&attachment_kind, 40);
        if kind == "work-note" {
            let (path, content_type) = super::work_note_attachments::resolve_local_path(
                self,
                safe_tenant,
                safe_media,
            )?;
            if !allowed_attachment_type(&content_type) {
                return Err("media_content_type_unsupported".to_string());
            }
            return Ok((path, content_type));
        }
        if !kind.is_empty() && kind != "board-media" {
            return Err("media_kind_unsupported".to_string());
        }
        let (relative_path, content_type): (String, String) = self
            .conn
            .lock()
            .map_err(|_| "db_lock_failed".to_string())?
            .query_row(
                "SELECT local_path, content_type FROM board_media_files WHERE tenant_id = ?1 AND media_id = ?2",
                params![safe_tenant, safe_media],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => "media_not_found".to_string(),
                other => format!("db_board_media_lookup_failed:{other}"),
            })?;
        if !allowed_attachment_type(&content_type) {
            return Err("media_content_type_unsupported".to_string());
        }
        let base =
            fs::canonicalize(&self.data_dir).map_err(|_| "local_data_dir_missing".to_string())?;
        let target = fs::canonicalize(self.data_dir.join(relative_path))
            .map_err(|_| "media_file_missing".to_string())?;
        if !target.starts_with(&base) || !target.is_file() {
            return Err("media_path_invalid".to_string());
        }
        Ok((target, content_type))
    }
}

fn allowed_attachment_type(content_type: &str) -> bool {
    let value = content_type.trim().to_ascii_lowercase();
    value == "application/pdf"
        || value.starts_with("image/")
        || value.starts_with("text/")
        || value.starts_with("audio/")
        || value.starts_with("video/")
        || value == "application/msword"
        || value.starts_with("application/vnd.ms-")
        || value.starts_with("application/vnd.openxmlformats-officedocument.")
        || value.starts_with("application/vnd.oasis.opendocument.")
}

fn open_attachment_path(path: &PathBuf) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let result = Command::new("rundll32.exe")
        .arg("url.dll,FileProtocolHandler")
        .arg(path)
        .spawn();

    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg(path).spawn();

    #[cfg(all(unix, not(target_os = "macos")))]
    let result = Command::new("xdg-open").arg(path).spawn();

    result
        .map(|_| ())
        .map_err(|e| format!("media_open_failed:{e}"))
}

#[tauri::command]
pub(crate) fn search_local_data(
    state: tauri::State<'_, AppState>,
    input: LocalDataSearchInput,
) -> Value {
    let result = state
        .store
        .lock()
        .ok()
        .and_then(|store| store.clone())
        .ok_or_else(|| "local_store_unavailable".to_string())
        .and_then(|store| store.search_local_data(input));
    match result {
        Ok(value) => value,
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

#[tauri::command]
pub(crate) fn list_local_students(
    state: tauri::State<'_, AppState>,
    input: LocalStudentListInput,
) -> Value {
    let result = state
        .store
        .lock()
        .ok()
        .and_then(|store| store.clone())
        .ok_or_else(|| "local_store_unavailable".to_string())
        .and_then(|store| store.list_local_students(input));
    match result {
        Ok(value) => value,
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

#[tauri::command]
pub(crate) fn open_local_data_attachment(
    state: tauri::State<'_, AppState>,
    tenant_id: String,
    media_id: String,
    attachment_kind: Option<String>,
) -> Value {
    let result = state
        .store
        .lock()
        .ok()
        .and_then(|store| store.clone())
        .ok_or_else(|| "local_store_unavailable".to_string())
        .and_then(|store| {
            store.resolve_local_attachment(
                tenant_id,
                media_id,
                attachment_kind.unwrap_or_default(),
            )
        })
        .and_then(|(path, _)| open_attachment_path(&path));
    match result {
        Ok(()) => json!({ "ok": true }),
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

#[tauri::command]
pub(crate) fn open_local_data_directory(state: tauri::State<'_, AppState>) -> Value {
    let result = state
        .store
        .lock()
        .ok()
        .and_then(|store| store.clone())
        .ok_or_else(|| "local_store_unavailable".to_string())
        .and_then(|store| {
            let path = fs::canonicalize(&store.data_dir)
                .map_err(|_| "local_data_dir_missing".to_string())?;
            if !path.is_dir() {
                return Err("local_data_dir_invalid".to_string());
            }
            open_attachment_path(&path)
        });
    match result {
        Ok(()) => json!({ "ok": true }),
        Err(error) => json!({ "ok": false, "error": error }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::random_url_token;
    use std::io::Cursor;

    fn test_store() -> (PathBuf, SqliteStore) {
        let directory = std::env::temp_dir().join(format!(
            "onlineclass-data-explorer-test-{}",
            random_url_token()
        ));
        fs::create_dir_all(&directory).expect("create test directory");
        let store = SqliteStore::open(directory.join("test.sqlite")).expect("open test store");
        (directory, store)
    }

    fn input(tenant_id: &str) -> LocalDataSearchInput {
        LocalDataSearchInput {
            tenant_id: tenant_id.to_string(),
            group: String::new(),
            section_key: String::new(),
            student_id: String::new(),
            student_query: String::new(),
            text_query: String::new(),
            date_from: String::new(),
            date_to: String::new(),
            has_attachment: false,
            offset: 0,
            limit: 40,
        }
    }

    #[test]
    fn searches_filters_and_pages_local_records() {
        let (directory, store) = test_store();
        store.upsert_observation(json!({
            "tenantId": "tenant-a", "docId": "obs-a", "dateKey": "2026-08-04", "period": 1,
            "studentCode": "1", "studentName": "김하늘", "observation": "또래 관계 행동 관찰", "updatedAtMs": 20
        })).expect("insert observation");
        store.upsert_teacher_counseling_session(json!({
            "tenantId": "tenant-a", "sessionId": "counsel-a", "studentCode": "2", "studentName": "박도윤",
            "counselingAtMs": 10, "status": "completed", "summary": "진로 고민 상담", "updatedAtMs": 10
        })).expect("insert counseling");

        let mut search = input("tenant-a");
        search.group = "care".to_string();
        search.text_query = "또래".to_string();
        let result = store.search_local_data(search).expect("search records");
        assert_eq!(result.get("total").and_then(Value::as_i64), Some(1));
        assert_eq!(
            result
                .pointer("/records/0/sectionKey")
                .and_then(Value::as_str),
            Some("observations")
        );

        let mut counseling = input("tenant-a");
        counseling.section_key = "teacher-counseling-sessions".to_string();
        let result = store
            .search_local_data(counseling)
            .expect("search counseling route");
        assert_eq!(result.get("total").and_then(Value::as_i64), Some(1));

        let mut student = input("tenant-a");
        student.student_query = "박도윤".to_string();
        let result = store.search_local_data(student).expect("search student");
        assert_eq!(result.get("total").and_then(Value::as_i64), Some(1));

        let roster = store
            .list_local_students(LocalStudentListInput {
                tenant_id: "tenant-a".to_string(),
                query: String::new(),
                offset: 0,
                limit: 200,
            })
            .expect("list students");
        assert_eq!(roster.get("total").and_then(Value::as_i64), Some(2));
        assert_eq!(
            roster
                .pointer("/students/0/studentId")
                .and_then(Value::as_str),
            Some("1")
        );

        let mut exact_student = input("tenant-a");
        exact_student.student_id = "2".to_string();
        let result = store
            .search_local_data(exact_student)
            .expect("search exact student");
        assert_eq!(result.get("total").and_then(Value::as_i64), Some(1));
        assert_eq!(
            result
                .pointer("/records/0/sectionKey")
                .and_then(Value::as_str),
            Some("teacher-counseling-sessions")
        );

        let mut dated = input("tenant-a");
        dated.date_from = "2026-08-04".to_string();
        dated.date_to = "2026-08-04".to_string();
        let result = store.search_local_data(dated).expect("filter date");
        assert_eq!(result.get("total").and_then(Value::as_i64), Some(1));

        let mut paged = input("tenant-a");
        paged.group = "care".to_string();
        paged.limit = 1;
        paged.offset = 1;
        let result = store.search_local_data(paged).expect("page records");
        assert_eq!(result.get("total").and_then(Value::as_i64), Some(2));
        assert_eq!(
            result
                .get("records")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn attachment_resolution_stays_inside_store_and_rejects_executables() {
        let (directory, store) = test_store();
        let media_dir = directory.join("board-media");
        fs::create_dir_all(&media_dir).expect("create media directory");
        fs::write(media_dir.join("proof.pdf"), b"pdf").expect("write media");
        let conn = store.conn.lock().expect("lock db");
        conn.execute(
            "INSERT INTO board_media_files (tenant_id, board_id, post_id, media_id, storage_path, local_path, content_type, file_name, size, expires_at_ms, archived_at_ms, payload_json) VALUES (?1, 'b', 'p', ?2, '', ?3, ?4, 'proof.pdf', 3, 0, 1, '{}')",
            params!["tenant-a", "media-a", "board-media/proof.pdf", "application/pdf"],
        ).expect("insert safe media");
        conn.execute(
            "INSERT INTO board_media_files (tenant_id, board_id, post_id, media_id, storage_path, local_path, content_type, file_name, size, expires_at_ms, archived_at_ms, payload_json) VALUES (?1, 'b', 'p', ?2, '', ?3, ?4, 'bad.exe', 3, 0, 1, '{}')",
            params!["tenant-a", "media-b", "board-media/proof.pdf", "application/x-msdownload"],
        ).expect("insert unsafe media");
        drop(conn);

        let mut attachments = input("tenant-a");
        attachments.group = "learning".to_string();
        attachments.has_attachment = true;
        let result = store
            .search_local_data(attachments)
            .expect("filter attachments");
        assert_eq!(result.get("total").and_then(Value::as_i64), Some(2));

        let (path, _) = store
            .resolve_local_attachment(
                "tenant-a".to_string(),
                "media-a".to_string(),
                "board-media".to_string(),
            )
            .expect("resolve safe media");
        assert!(path.starts_with(fs::canonicalize(&directory).expect("canonical directory")));
        assert_eq!(
            store
                .resolve_local_attachment(
                    "tenant-a".to_string(),
                    "media-b".to_string(),
                    "board-media".to_string(),
                )
                .unwrap_err(),
            "media_content_type_unsupported"
        );
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn work_notes_search_title_body_and_attachment_without_crossing_tenants() {
        let (directory, store) = test_store();
        store
            .upsert_work_note(json!({
                "tenantId": "tenant-a",
                "pageId": "page-a",
                "title": "5학년 평가 운영 계획",
                "blocks": [{ "id": "block-a", "type": "text", "text": "회의에서 제출 일정을 정리함" }],
                "markdown": "회의에서 제출 일정을 정리함",
                "updatedAtMs": 20
            }))
            .expect("save work note");
        store
            .upsert_work_note(json!({
                "tenantId": "tenant-b",
                "pageId": "page-b",
                "title": "다른 학급 평가 운영 계획",
                "blocks": [],
                "markdown": "회의에서 제출 일정을 정리함",
                "updatedAtMs": 30
            }))
            .expect("save other tenant work note");
        crate::work_note_attachments::save(
            &store,
            "tenant-a".to_string(),
            "attachment-a".to_string(),
            "page-a".to_string(),
            "block-a".to_string(),
            "평가계획_증빙자료.pdf".to_string(),
            "application/pdf".to_string(),
            &mut Cursor::new(b"pdf"),
        )
        .expect("save work note attachment");

        let mut body = input("tenant-a");
        body.group = "work-notes".to_string();
        body.text_query = "제출 일정".to_string();
        let result = store.search_local_data(body).expect("search work note body");
        assert_eq!(result.get("total").and_then(Value::as_i64), Some(1));
        assert_eq!(
            result.pointer("/records/0/payload/title").and_then(Value::as_str),
            Some("5학년 평가 운영 계획")
        );
        assert_eq!(
            result
                .pointer("/records/0/payload/attachments/0/fileName")
                .and_then(Value::as_str),
            Some("평가계획_증빙자료.pdf")
        );

        let mut file_name = input("tenant-a");
        file_name.group = "work-notes".to_string();
        file_name.text_query = "증빙자료".to_string();
        file_name.has_attachment = true;
        let result = store
            .search_local_data(file_name)
            .expect("search work note attachment name");
        assert_eq!(result.get("total").and_then(Value::as_i64), Some(1));
        assert_eq!(
            result
                .pointer("/records/0/hasAttachment")
                .and_then(Value::as_bool),
            Some(true)
        );

        let (path, content_type) = store
            .resolve_local_attachment(
                "tenant-a".to_string(),
                "attachment-a".to_string(),
                "work-note".to_string(),
            )
            .expect("resolve work note attachment");
        assert!(path.starts_with(fs::canonicalize(&directory).expect("canonical directory")));
        assert_eq!(content_type, "application/pdf");
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn student_roster_keeps_same_names_separate_by_stable_identifier() {
        let (directory, store) = test_store();
        store
            .upsert_observation(json!({
                "tenantId": "tenant-a", "docId": "obs-a", "dateKey": "2026-08-04", "period": 1,
                "studentCode": "1", "studentName": "김민준", "observation": "관찰", "updatedAtMs": 20
            }))
            .expect("insert observation");
        store
            .conn
            .lock()
            .expect("lock db")
            .execute(
                "INSERT INTO eval_results (tenant_id, result_id, assignment_id, student_id, date_key, payload_json, updated_at_ms) VALUES (?1, 'result-a', 'assignment-a', ?2, '2026-08-03', ?3, 10)",
                params!["tenant-a", "2", r#"{"studentName":"김민준","studentId":"2","result":"평가"}"#],
            )
            .expect("insert evaluation result");

        let roster = store
            .list_local_students(LocalStudentListInput {
                tenant_id: "tenant-a".to_string(),
                query: "김민준".to_string(),
                offset: 0,
                limit: 200,
            })
            .expect("list same-name students");
        assert_eq!(roster.get("total").and_then(Value::as_i64), Some(2));

        let mut exact = input("tenant-a");
        exact.student_id = "2".to_string();
        let result = store
            .search_local_data(exact)
            .expect("search evaluation student");
        assert_eq!(result.get("total").and_then(Value::as_i64), Some(1));
        assert_eq!(
            result
                .pointer("/records/0/sectionKey")
                .and_then(Value::as_str),
            Some("eval-results")
        );
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn rejects_invalid_filters() {
        let mut invalid = input("tenant-a");
        invalid.group = "unknown".to_string();
        assert_eq!(
            build_union_query(&invalid).unwrap_err(),
            "local_data_group_invalid"
        );
        invalid.group.clear();
        invalid.date_from = "2026-99-99".to_string();
        assert_eq!(
            build_union_query(&invalid).unwrap_err(),
            "date_filter_invalid"
        );
    }
}
