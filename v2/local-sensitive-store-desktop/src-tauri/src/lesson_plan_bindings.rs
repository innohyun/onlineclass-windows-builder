use crate::{
    normalize_date_key, normalize_id_segment, normalize_json_text, normalize_tenant_id, SqliteStore,
};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

pub(crate) fn ensure_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS lesson_plan_bindings (
          tenant_id TEXT NOT NULL,
          plan_id TEXT NOT NULL,
          page_id TEXT NOT NULL,
          plan_kind TEXT NOT NULL CHECK (plan_kind IN ('lesson','event')),
          date_key TEXT NOT NULL,
          start_period INTEGER NOT NULL CHECK (start_period BETWEEN 1 AND 20),
          end_period INTEGER NOT NULL CHECK (end_period BETWEEN start_period AND 20),
          subject TEXT NOT NULL,
          binding_revision INTEGER NOT NULL CHECK (binding_revision >= 1),
          updated_at_ms INTEGER NOT NULL,
          PRIMARY KEY (tenant_id, plan_id)
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_lesson_plan_bindings_page
          ON lesson_plan_bindings (tenant_id, page_id);
        "#,
    )
    .map_err(|error| format!("db_lesson_plan_binding_schema_failed:{error}"))
}

fn row(
    tenant_id: String,
    plan_id: String,
    page_id: String,
    plan_kind: String,
    date_key: String,
    start_period: i64,
    end_period: i64,
    subject: String,
    binding_revision: i64,
    updated_at_ms: i64,
) -> Value {
    json!({
        "tenantId": tenant_id,
        "planId": plan_id,
        "pageId": page_id,
        "planKind": plan_kind,
        "dateKey": date_key,
        "startPeriod": start_period,
        "endPeriod": end_period,
        "subject": subject,
        "bindingRevision": binding_revision,
        "updatedAt": updated_at_ms,
    })
}

pub(crate) fn list(store: &SqliteStore, tenant_id: String) -> Result<Vec<Value>, String> {
    let tenant = normalize_tenant_id(Some(&Value::String(tenant_id)));
    if tenant.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let mut statement = conn.prepare(
        "SELECT tenant_id,plan_id,page_id,plan_kind,date_key,start_period,end_period,subject,binding_revision,updated_at_ms
         FROM lesson_plan_bindings WHERE tenant_id=?1 ORDER BY date_key,start_period,plan_id",
    ).map_err(|error| format!("db_lesson_plan_binding_query_prepare_failed:{error}"))?;
    let rows = statement
        .query_map(params![tenant], |candidate| {
            Ok(row(
                candidate.get(0)?,
                candidate.get(1)?,
                candidate.get(2)?,
                candidate.get(3)?,
                candidate.get(4)?,
                candidate.get(5)?,
                candidate.get(6)?,
                candidate.get(7)?,
                candidate.get(8)?,
                candidate.get(9)?,
            ))
        })
        .map_err(|error| format!("db_lesson_plan_binding_query_failed:{error}"))?;
    let mut records = Vec::new();
    for candidate in rows {
        records
            .push(candidate.map_err(|error| format!("db_lesson_plan_binding_row_failed:{error}"))?);
    }
    Ok(records)
}

pub(crate) fn upsert(store: &SqliteStore, body: Value) -> Result<Vec<Value>, String> {
    let tenant = normalize_tenant_id(body.get("tenantId"));
    if tenant.is_empty() {
        return Err("tenant_id_required".to_string());
    }
    let bindings = body
        .get("bindings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if bindings.len() > 5000 {
        return Err("lesson_plan_binding_limit".to_string());
    }
    let mut conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let transaction = conn
        .transaction()
        .map_err(|error| format!("db_lesson_plan_binding_transaction_failed:{error}"))?;
    for binding in bindings {
        let plan_id = normalize_id_segment(binding.get("planId"), 160);
        let page_id = normalize_id_segment(binding.get("pageId"), 180);
        let plan_kind = normalize_json_text(binding.get("planKind"), 20);
        let date_key = normalize_date_key(binding.get("dateKey"));
        let start_period = binding
            .get("startPeriod")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let end_period = binding
            .get("endPeriod")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let subject = {
            let value = normalize_json_text(binding.get("subject"), 180);
            if value.is_empty() {
                "수업".to_string()
            } else {
                value
            }
        };
        let revision = binding
            .get("bindingRevision")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let updated_at = binding
            .get("updatedAt")
            .and_then(Value::as_i64)
            .filter(|value| *value >= 0)
            .unwrap_or_else(|| Utc::now().timestamp_millis());
        if plan_id.is_empty() {
            return Err("lesson_plan_id_required".to_string());
        }
        if page_id.is_empty() {
            return Err("work_note_page_id_required".to_string());
        }
        if !["lesson", "event"].contains(&plan_kind.as_str())
            || date_key.is_empty()
            || start_period < 1
            || end_period < start_period
            || end_period > 20
            || revision < 1
        {
            return Err("lesson_plan_binding_invalid".to_string());
        }
        let current = transaction
            .query_row(
                "SELECT page_id,plan_kind,date_key,start_period,end_period,subject,binding_revision
             FROM lesson_plan_bindings WHERE tenant_id=?1 AND plan_id=?2",
                params![tenant, plan_id],
                |candidate| {
                    Ok((
                        candidate.get::<_, String>(0)?,
                        candidate.get::<_, String>(1)?,
                        candidate.get::<_, String>(2)?,
                        candidate.get::<_, i64>(3)?,
                        candidate.get::<_, i64>(4)?,
                        candidate.get::<_, String>(5)?,
                        candidate.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("db_lesson_plan_binding_read_failed:{error}"))?;
        if let Some((old_page, old_kind, old_date, old_start, old_end, old_subject, old_revision)) =
            current
        {
            if old_revision > revision {
                continue;
            }
            if old_revision == revision {
                if old_page != page_id
                    || old_kind != plan_kind
                    || old_date != date_key
                    || old_start != start_period
                    || old_end != end_period
                    || old_subject != subject
                {
                    return Err("lesson_plan_binding_revision_conflict".to_string());
                }
                continue;
            }
        }
        transaction.execute(
            "INSERT INTO lesson_plan_bindings(
               tenant_id,plan_id,page_id,plan_kind,date_key,start_period,end_period,subject,binding_revision,updated_at_ms
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(tenant_id,plan_id) DO UPDATE SET
               page_id=excluded.page_id,plan_kind=excluded.plan_kind,date_key=excluded.date_key,
               start_period=excluded.start_period,end_period=excluded.end_period,subject=excluded.subject,
               binding_revision=excluded.binding_revision,updated_at_ms=excluded.updated_at_ms
             WHERE excluded.binding_revision>lesson_plan_bindings.binding_revision",
            params![tenant, plan_id, page_id, plan_kind, date_key, start_period, end_period, subject, revision, updated_at],
        ).map_err(|error| format!("db_lesson_plan_binding_upsert_failed:{error}"))?;
    }
    transaction
        .commit()
        .map_err(|error| format!("db_lesson_plan_binding_commit_failed:{error}"))?;
    drop(conn);
    list(store, tenant)
}

pub(crate) fn stored_page_structure(
    store: &SqliteStore,
    tenant_id: &str,
    page_id: &str,
) -> Result<Option<(Option<String>, String, i64)>, String> {
    let conn = store
        .conn
        .lock()
        .map_err(|_| "db_lock_failed".to_string())?;
    let bound: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM lesson_plan_bindings WHERE tenant_id=?1 AND page_id=?2",
            params![tenant_id, page_id],
            |candidate| candidate.get(0),
        )
        .map_err(|error| format!("db_lesson_plan_binding_read_failed:{error}"))?;
    if bound == 0 {
        return Ok(None);
    }
    conn.query_row(
        "SELECT parent_id,title,position FROM work_note_pages WHERE tenant_id=?1 AND page_id=?2",
        params![tenant_id, page_id],
        |candidate| Ok((candidate.get(0)?, candidate.get(1)?, candidate.get(2)?)),
    )
    .optional()
    .map_err(|error| format!("db_lesson_plan_page_structure_failed:{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::random_url_token;
    use std::{fs, sync::Mutex};

    fn fixture() -> (SqliteStore, std::path::PathBuf) {
        let data_dir =
            std::env::temp_dir().join(format!("classaimate-lesson-binding-{}", random_url_token()));
        fs::create_dir_all(&data_dir).expect("create lesson binding fixture");
        let db_path = data_dir.join("fixture.sqlite");
        let conn = Connection::open(&db_path).expect("open lesson binding fixture");
        conn.execute_batch(
            "CREATE TABLE work_note_pages(
               tenant_id TEXT NOT NULL,page_id TEXT NOT NULL,parent_id TEXT,title TEXT NOT NULL,
               position INTEGER NOT NULL,PRIMARY KEY(tenant_id,page_id)
             );",
        )
        .expect("create work note fixture");
        ensure_schema(&conn).expect("create binding schema");
        (
            SqliteStore {
                conn: Mutex::new(conn),
                db_path,
                data_dir: data_dir.clone(),
            },
            data_dir,
        )
    }

    fn binding(revision: i64, date_key: &str) -> Value {
        json!({
            "planId": "lesson-plan-1234567890",
            "pageId": "lesson-page-a",
            "planKind": "lesson",
            "dateKey": date_key,
            "startPeriod": 2,
            "endPeriod": 2,
            "subject": "국어",
            "bindingRevision": revision,
            "updatedAt": 100 + revision,
        })
    }

    #[test]
    fn higher_revision_wins_and_equal_revision_conflict_is_closed() {
        let (store, data_dir) = fixture();
        upsert(
            &store,
            json!({ "tenantId": "tenant-a", "bindings": [binding(1, "2026-08-26")] }),
        )
        .expect("insert binding");
        upsert(
            &store,
            json!({ "tenantId": "tenant-a", "bindings": [binding(2, "2026-08-27")] }),
        )
        .expect("advance binding");
        upsert(
            &store,
            json!({ "tenantId": "tenant-a", "bindings": [binding(1, "2026-08-25")] }),
        )
        .expect("ignore stale binding");
        let records = list(&store, "tenant-a".to_string()).expect("list bindings");
        assert_eq!(records[0]["dateKey"], "2026-08-27");
        let error = upsert(
            &store,
            json!({ "tenantId": "tenant-a", "bindings": [binding(2, "2026-08-28")] }),
        )
        .expect_err("equal revision conflict");
        assert_eq!(error, "lesson_plan_binding_revision_conflict");
        drop(store);
        fs::remove_dir_all(data_dir).expect("remove lesson binding fixture");
    }
}
