use super::work_note_documents::RETENTION_MS;
use crate::{now_ms, SqliteStore};
use rusqlite::{params, TransactionBehavior};

// Expiration is bounded by live references. A recoverable draft or newer retained
// version keeps its page and attachment rows even after the trash UI's 30 days.
pub(crate) fn maintain(store: &SqliteStore, tenant: &str) -> Result<(), String> {
    let _access = store.media_access(tenant)?;
    let mut conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let cutoff = now_ms() - RETENTION_MS;
    tx.execute(
        "DELETE FROM work_note_versions WHERE tenant_id=?1 AND captured_at_ms<?2",
        params![tenant, cutoff],
    )
    .map_err(|e| format!("work_note_history_prune_failed:{e}"))?;
    let mut statement=tx.prepare("SELECT page_id FROM work_note_pages WHERE tenant_id=?1 AND COALESCE(json_extract(properties_json,'$._localTrash.deletedAtMs'),0)>0 AND json_extract(properties_json,'$._localTrash.deletedAtMs')<=?2").map_err(|e|e.to_string())?;
    let pages = statement
        .query_map(params![tenant, cutoff], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(statement);
    let mut paths = Vec::new();
    // Leaf-first passes preserve hierarchy when a retained child prevents expiry.
    let mut remaining = pages;
    loop {
        let mut removed = 0;
        let mut retained = Vec::new();
        for page in remaining {
            let blockers:i64=tx.query_row("SELECT (SELECT COUNT(*) FROM work_note_pages WHERE tenant_id=?1 AND parent_id=?2)+(SELECT COUNT(*) FROM work_note_versions WHERE tenant_id=?1 AND page_id=?2)+(SELECT COUNT(*) FROM work_note_local_drafts WHERE tenant_id=?1 AND page_id=?2)+(SELECT COUNT(*) FROM lesson_plan_bindings WHERE tenant_id=?1 AND page_id=?2)",params![tenant,page],|r|r.get(0)).map_err(|e|e.to_string())?;
            if blockers > 0 {
                retained.push(page);
                continue;
            }
            let mut stmt=tx.prepare("SELECT attachment_id,local_path FROM work_note_attachments WHERE tenant_id=?1 AND page_id=?2").map_err(|e|e.to_string())?;
            let files = stmt
                .query_map(params![tenant, page], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            drop(stmt);
            let mut referenced = false;
            for (attachment, _) in &files {
                let count:i64=tx.query_row("SELECT (SELECT COUNT(*) FROM work_note_pages WHERE tenant_id=?1 AND page_id<>?2 AND (instr(document_json,?3)>0 OR instr(markdown,?3)>0))+(SELECT COUNT(*) FROM work_note_versions WHERE tenant_id=?1 AND instr(payload_json,?3)>0)+(SELECT COUNT(*) FROM work_note_local_drafts WHERE tenant_id=?1 AND instr(payload_json,?3)>0)",params![tenant,page,attachment],|r|r.get(0)).map_err(|e|e.to_string())?;
                if count > 0 {
                    referenced = true;
                    break;
                }
            }
            if referenced {
                retained.push(page);
                continue;
            }
            tx.execute(
                "DELETE FROM work_note_pages_fts WHERE tenant_id=?1 AND page_id=?2",
                params![tenant, page],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "DELETE FROM work_note_attachments WHERE tenant_id=?1 AND page_id=?2",
                params![tenant, page],
            )
            .map_err(|e| e.to_string())?;
            tx.execute(
                "DELETE FROM work_note_pages WHERE tenant_id=?1 AND page_id=?2",
                params![tenant, page],
            )
            .map_err(|e| e.to_string())?;
            paths.extend(files.into_iter().map(|(_, path)| path));
            removed += 1;
        }
        if removed == 0 {
            break;
        }
        remaining = retained;
    }
    tx.commit().map_err(|e| e.to_string())?;
    drop(conn);
    crate::work_note_attachments::delete_local_paths(store, &paths);
    Ok(())
}
