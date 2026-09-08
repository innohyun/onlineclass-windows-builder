use crate::canonical_write_transactions;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

// A borrowed transaction connection: domain functions cannot relock or open a
// second connection while the worker owns the canonical mutation transaction.
pub(super) struct TransactionStore<'a> {
    pub conn: &'a Connection,
}

impl TransactionStore<'_> {
    pub fn upsert_work_note(&self, input: Value) -> Result<Value, String> {
        canonical_write_transactions::upsert_work_note(self.conn, input)
    }
    pub fn upsert_teacher_counseling_session(&self, input: Value) -> Result<Value, String> {
        canonical_write_transactions::upsert_teacher_counseling_session(self.conn, input)
    }
    pub fn get_teacher_counseling_session(
        &self,
        tenant: String,
        session: String,
    ) -> Result<Option<Value>, String> {
        let raw: Option<String> = self.conn.query_row("SELECT payload_json FROM teacher_counseling_sessions WHERE tenant_id=?1 AND session_id=?2",
            params![tenant,session], |row|row.get(0)).optional().map_err(|error|format!("db_teacher_counseling_get_failed:{error}"))?;
        raw.map(|raw| {
            serde_json::from_str(&raw).map_err(|_| "classaimate_mcp_local_payload_invalid".into())
        })
        .transpose()
    }
    pub fn list_work_notes(&self, tenant: String, _query: String) -> Result<Vec<Value>, String> {
        let mut statement = self.conn.prepare("SELECT tenant_id,page_id,parent_id,title,emoji,position,properties_json,document_json,markdown,created_at_ms,updated_at_ms FROM work_note_pages WHERE tenant_id=?1 ORDER BY COALESCE(parent_id,''),position,page_id")
            .map_err(|error|format!("db_work_note_query_failed:{error}"))?;
        let rows = statement.query_map(params![tenant], |row| {
            let properties: String = row.get(6)?; let blocks: String = row.get(7)?;
            Ok(json!({"tenantId":row.get::<_,String>(0)?,"pageId":row.get::<_,String>(1)?,"parentId":row.get::<_,Option<String>>(2)?,
                "title":row.get::<_,String>(3)?,"emoji":row.get::<_,String>(4)?,"position":row.get::<_,i64>(5)?,
                "properties":serde_json::from_str::<Value>(&properties).unwrap_or_else(|_|json!({})),"blocks":serde_json::from_str::<Value>(&blocks).unwrap_or_else(|_|json!([])),
                "markdown":row.get::<_,String>(8)?,"createdAtMs":row.get::<_,i64>(9)?,"updatedAtMs":row.get::<_,i64>(10)?}))
        }).map_err(|error|format!("db_work_note_query_failed:{error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("db_work_note_query_failed:{error}"))
    }
    pub fn get_work_note(&self, tenant: String, page: String) -> Result<Option<Value>, String> {
        Ok(self
            .list_work_notes(tenant, String::new())?
            .into_iter()
            .find(|row| row["pageId"] == page))
    }
    pub fn save_student_record_draft_batch(&self, input: Value) -> Result<Value, String> {
        let draft_set = canonical_write_transactions::upsert_student_record_draft_set(
            self.conn,
            input["draftSet"].clone(),
        )?;
        let mut drafts = Vec::new();
        for draft in input["drafts"]
            .as_array()
            .ok_or("student_record_drafts_required")?
        {
            drafts.push(canonical_write_transactions::upsert_student_record_draft(
                self.conn,
                draft.clone(),
            )?);
        }
        Ok(json!({"draftSet":draft_set,"drafts":drafts}))
    }
}
