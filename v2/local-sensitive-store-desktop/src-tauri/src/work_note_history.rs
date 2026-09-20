use crate::{now_ms, work_note_documents::RETENTION_MS};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct HistoryCursor {
    pub captured_at_ms: i64,
    pub version_id: String,
}

pub(crate) fn list(
    conn: &Connection,
    tenant: &str,
    page: &str,
    limit: Option<i64>,
    cursor: Option<HistoryCursor>,
) -> Result<Value, String> {
    let limit = limit.unwrap_or(50).clamp(1, 100);
    if cursor.as_ref().is_some_and(|cursor| {
        cursor.captured_at_ms <= 0 || cursor.version_id.is_empty() || cursor.version_id.len() > 180
    }) {
        return Err("work_note_history_cursor_invalid".into());
    }
    let before_time = cursor.as_ref().map(|c| c.captured_at_ms);
    let before_id = cursor.as_ref().map(|c| c.version_id.as_str()).unwrap_or("");
    // LIMIT applies inside SQLite. IPC carries small metadata only, regardless of
    // how many full snapshots autosave has retained in the last thirty days.
    let mut stmt = conn
        .prepare(
            "SELECT version_id,captured_at_ms,json_extract(payload_json,'$.title'),
                json_extract(payload_json,'$.updatedAtMs'),length(CAST(payload_json AS BLOB))
         FROM work_note_versions
         WHERE tenant_id=?1 AND page_id=?2 AND captured_at_ms>=?3
           AND (?4 IS NULL OR captured_at_ms<?4 OR (captured_at_ms=?4 AND version_id<?5))
         ORDER BY captured_at_ms DESC,version_id DESC LIMIT ?6",
        )
        .map_err(|e| format!("work_note_history_read_failed:{e}"))?;
    let mut items = stmt
        .query_map(
            params![
                tenant,
                page,
                now_ms() - RETENTION_MS,
                before_time,
                before_id,
                limit + 1
            ],
            |r| {
                Ok(
                    json!({"versionId":r.get::<_,String>(0)?,"capturedAtMs":r.get::<_,i64>(1)?,
            "title":r.get::<_,Option<String>>(2)?.unwrap_or_default(),
            "documentUpdatedAtMs":r.get::<_,Option<i64>>(3)?.unwrap_or(0),
            "byteSize":r.get::<_,i64>(4)?}),
                )
            },
        )
        .map_err(|e| format!("work_note_history_read_failed:{e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("work_note_history_read_failed:{e}"))?;
    let has_more = items.len() > limit as usize;
    items.truncate(limit as usize);
    let next_cursor = if has_more {
        items
            .last()
            .map(|row| json!({"capturedAtMs":row["capturedAtMs"],"versionId":row["versionId"]}))
    } else {
        None
    };
    Ok(
        json!({"ok":true,"items":items,"hasMore":has_more,"nextCursor":next_cursor,"retentionDays":30}),
    )
}

pub(crate) fn get(
    conn: &Connection,
    tenant: &str,
    page: &str,
    version: &str,
) -> Result<Value, String> {
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT payload_json,captured_at_ms FROM work_note_versions
         WHERE tenant_id=?1 AND page_id=?2 AND version_id=?3 AND captured_at_ms>=?4",
            params![tenant, page, version, now_ms() - RETENTION_MS],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| format!("work_note_history_read_failed:{e}"))?;
    let (raw, captured_at_ms) = row.ok_or("work_note_version_not_found")?;
    let document: Value = serde_json::from_str(&raw).map_err(|_| "work_note_version_invalid")?;
    if document["tenantId"] != tenant || document["pageId"] != page {
        return Err("work_note_version_invalid".into());
    }
    Ok(
        json!({"ok":true,"versionId":version,"capturedAtMs":captured_at_ms,
        "revision":crate::work_note_documents::revision(&document),"page":document}),
    )
}
