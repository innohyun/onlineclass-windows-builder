use crate::{
    normalize_date_key, normalize_id_segment, normalize_json_text, normalize_local_record_id,
    normalize_period, normalize_student_code, normalize_teacher_counseling_session,
    normalize_tenant_id, now_ms, payload_json, set_obj, set_updated_payload_fields, timestamp_like,
    updated_at_ms, STUDENT_MATERIAL_ROOT_PAGE_ID, STUDENT_MATERIAL_ROOT_SYSTEM_KIND,
    STUDENT_MATERIAL_ROOT_TITLE, WORK_MEETING_ROOT_PAGE_ID, WORK_REFERENCE_ROOT_PAGE_ID,
};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

// Connection-scoped canonical writes are shared by the UI store and one MCP transaction.
// They never acquire the store mutex, open another connection, or start/commit a transaction.
pub(crate) fn upsert_work_note(conn: &Connection, mut input: Value) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(input.get("tenantId"));
    let page_id = normalize_id_segment(input.get("pageId").or_else(|| input.get("id")), 180);
    let mut parent_id = normalize_id_segment(input.get("parentId"), 180);
    let mut title = {
        let value = normalize_json_text(input.get("title"), 240);
        if value.is_empty() {
            "제목 없음".to_string()
        } else {
            value
        }
    };
    let emoji = {
        let value = normalize_json_text(input.get("emoji"), 16);
        if value.is_empty() {
            "📄".to_string()
        } else {
            value
        }
    };
    let mut position = input
        .get("position")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .max(0);
    let properties = input
        .get("properties")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let blocks = input.get("blocks").cloned().unwrap_or_else(|| json!([]));
    let markdown = input
        .get("markdown")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .take(2_000_000)
        .collect::<String>();
    let now = Utc::now().timestamp_millis();
    let updated_at_ms = input
        .get("updatedAtMs")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .unwrap_or(now);
    let created_at_ms = input
        .get("createdAtMs")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .unwrap_or(updated_at_ms);
    if tenant_id.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    if page_id.is_empty() {
        return Err("work_note_page_id_required".to_string());
    }
    if [WORK_MEETING_ROOT_PAGE_ID, WORK_REFERENCE_ROOT_PAGE_ID].contains(&page_id.as_str()) {
        return Err("work_note_system_folder_protected".to_string());
    }
    if page_id == STUDENT_MATERIAL_ROOT_PAGE_ID
        && (!parent_id.is_empty()
            || title != STUDENT_MATERIAL_ROOT_TITLE
            || properties.get("systemKind").and_then(Value::as_str)
                != Some(STUDENT_MATERIAL_ROOT_SYSTEM_KIND))
    {
        return Err("work_note_system_folder_protected".to_string());
    }
    if parent_id == page_id {
        return Err("work_note_parent_cycle".to_string());
    }
    if let Some((stored_parent, stored_title, stored_position)) =
        crate::lesson_plan_bindings::stored_page_structure(conn, &tenant_id, &page_id)?
    {
        parent_id = stored_parent.unwrap_or_default();
        title = stored_title;
        position = stored_position;
    }
    if let Some(object) = input.as_object_mut() {
        object.insert("tenantId".to_string(), Value::String(tenant_id.clone()));
        object.insert("pageId".to_string(), Value::String(page_id.clone()));
        object.insert(
            "parentId".to_string(),
            if parent_id.is_empty() {
                Value::Null
            } else {
                Value::String(parent_id.clone())
            },
        );
        object.insert("title".to_string(), Value::String(title.clone()));
        object.insert("emoji".to_string(), Value::String(emoji.clone()));
        object.insert("position".to_string(), Value::Number(position.into()));
        object.insert("properties".to_string(), properties.clone());
        object.insert("blocks".to_string(), blocks.clone());
        object.insert("markdown".to_string(), Value::String(markdown.clone()));
        object.insert(
            "updatedAtMs".to_string(),
            Value::Number(updated_at_ms.into()),
        );
    }
    let properties_json = serde_json::to_string(&properties)
        .map_err(|e| format!("work_note_properties_encode_failed:{e}"))?;
    let document_json = serde_json::to_string(&blocks)
        .map_err(|e| format!("work_note_document_encode_failed:{e}"))?;
    conn.execute(
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
    conn.execute(
        "DELETE FROM work_note_pages_fts WHERE tenant_id = ?1 AND page_id = ?2",
        params![tenant_id, page_id],
    )
    .map_err(|e| format!("db_work_note_fts_delete_failed:{e}"))?;
    conn.execute(
        "INSERT INTO work_note_pages_fts (tenant_id,page_id,title,markdown) VALUES (?1,?2,?3,?4)",
        params![tenant_id, page_id, title, markdown],
    )
    .map_err(|e| format!("db_work_note_fts_insert_failed:{e}"))?;
    Ok(input)
}

pub(crate) fn upsert_teacher_counseling_session(
    conn: &Connection,
    input: Value,
) -> Result<Value, String> {
    let record = normalize_teacher_counseling_session(input)?;
    let payload_json = payload_json(&record.payload, "teacher_counseling_payload_encode_failed")?;
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
        params![
            record.tenant_id,
            record.session_id,
            record.student_code,
            record.counseling_at_ms,
            record.status,
            if record.follow_up_on.is_empty() {
                None::<String>
            } else {
                Some(record.follow_up_on)
            },
            if record.archived_at_ms > 0 {
                Some(record.archived_at_ms)
            } else {
                None
            },
            payload_json,
            record.updated_at_ms
        ],
    )
    .map_err(|e| format!("db_teacher_counseling_upsert_failed:{e}"))?;
    Ok(record.payload)
}

pub(crate) fn normalize_student_record_draft_set(mut input: Value) -> Result<Value, String> {
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
        input
            .get("draftSetId")
            .or_else(|| input.get("id"))
            .or_else(|| input.get("docId")),
        if fallback.is_empty() {
            now_ms().to_string()
        } else {
            fallback
        },
        "student_record_draft_set_id_required",
    )?;
    let status = {
        let value = normalize_json_text(input.get("status"), 40);
        if value.is_empty() {
            "ready".to_string()
        } else {
            value
        }
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
    let created_at_ms = if parsed_created_at_ms > 0 {
        parsed_created_at_ms
    } else {
        updated_at_ms
    };
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
    Ok(input)
}

pub(crate) fn upsert_student_record_draft_set(
    conn: &Connection,
    input: Value,
) -> Result<Value, String> {
    let input = normalize_student_record_draft_set(input)?;
    if crate::student_record_workspace::is_workspace(&input) {
        return Err("student_record_workspace_invalid".into());
    }
    let tenant_id = input["tenantId"].as_str().unwrap_or_default();
    let draft_set_id = input["draftSetId"].as_str().unwrap_or_default();
    let status = input["status"].as_str().unwrap_or_default();
    let from_date = input["fromDate"].as_str().unwrap_or_default();
    let to_date = input["toDate"].as_str().unwrap_or_default();
    let created_at_ms = input["createdAtMs"].as_i64().unwrap_or(0);
    let updated_at_ms = input["updatedAtMs"].as_i64().unwrap_or(0);
    let payload_json = payload_json(&input, "student_record_draft_set_encode_failed")?;
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

pub(crate) fn upsert_student_record_draft(
    conn: &Connection,
    mut input: Value,
) -> Result<Value, String> {
    let tenant_id = normalize_tenant_id(input.get("tenantId"));
    let draft_set_id =
        normalize_id_segment(input.get("draftSetId").or_else(|| input.get("setId")), 260);
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
        input
            .get("draftId")
            .or_else(|| input.get("id"))
            .or_else(|| input.get("docId")),
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
        params![
            tenant_id,
            draft_id,
            draft_set_id,
            student_code,
            class_no,
            payload_json,
            updated_at_ms
        ],
    )
    .map_err(|e| format!("db_student_record_draft_upsert_failed:{e}"))?;
    Ok(input)
}
