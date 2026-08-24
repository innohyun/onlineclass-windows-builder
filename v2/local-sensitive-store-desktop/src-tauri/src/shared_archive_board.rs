use crate::shared_archive::{open_db, record_values};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

fn text(payload: &Value, key: &str) -> String {
    payload
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn number(payload: &Value, key: &str) -> i64 {
    payload.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn board_mode(payload: &Value) -> String {
    match payload.get("student_view_mode").and_then(Value::as_str) {
        Some("detail") => "detail".to_string(),
        Some("shelf") => "shelf".to_string(),
        _ => "gallery".to_string(),
    }
}

#[tauri::command]
pub(crate) fn search_shared_archive_boards(tenant_id: String, query: String, limit: i64) -> Value {
    match search_archive_boards(&tenant_id, &query, limit) {
        Ok((total, boards)) => json!({"ok":true,"total":total,"boards":boards}),
        Err(error) => json!({"ok":false,"total":0,"boards":[],"error":error}),
    }
}

fn search_archive_boards(
    tenant_id: &str,
    query: &str,
    limit: i64,
) -> Result<(usize, Vec<Value>), String> {
    if tenant_id.trim().is_empty() {
        return Err("archive_tenant_missing".to_string());
    }
    let connection = open_db()?;
    let bounded = limit.clamp(1, 100) as usize;
    let needle = query.trim().to_lowercase();
    let mut statement = connection
        .prepare("SELECT id,title,record_count,file_count,total_file_bytes,imported_at FROM shared_archives WHERE tenant_id=?1 AND source_type='board' ORDER BY imported_at DESC,id DESC")
        .map_err(|e| format!("archive_board_search_prepare_failed:{e}"))?;
    let rows = statement
        .query_map(params![tenant_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(|e| format!("archive_board_search_query_failed:{e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("archive_board_search_row_failed:{e}"))?;
    let mut matches = Vec::new();
    for (id, title, record_count, file_count, total_file_bytes, imported_at) in rows {
        let records = record_values(&connection, &id)?;
        if !needle.is_empty() {
            let searchable = format!(
                "{} {}",
                title,
                records
                    .iter()
                    .map(|(_, _, payload)| payload.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
            .to_lowercase();
            if !searchable.contains(&needle) {
                continue;
            }
        }
        let board = records
            .iter()
            .find(|(_, kind, _)| kind == "board")
            .map(|(_, _, payload)| payload);
        let post_count = records
            .iter()
            .filter(|(_, kind, payload)| {
                kind == "board_post"
                    && payload
                        .get("deleted_at")
                        .map(Value::is_null)
                        .unwrap_or(true)
            })
            .count();
        matches.push(json!({
            "archiveId":id,"title":title,"recordCount":record_count,"postCount":post_count,
            "fileCount":file_count,"totalFileBytes":total_file_bytes,"importedAt":imported_at,
            "studentViewMode":board.map(board_mode).unwrap_or_else(||"gallery".to_string())
        }));
    }
    let total = matches.len();
    matches.truncate(bounded);
    Ok((total, matches))
}

#[tauri::command]
pub(crate) fn get_shared_archive_board_view(tenant_id: String, archive_id: String) -> Value {
    let result = open_db().and_then(|connection| {
        archive_board_view_from_connection(&connection, &tenant_id, &archive_id)
    });
    match result {
        Ok(board) => json!({"ok":true,"board":board}),
        Err(error) => json!({"ok":false,"error":error}),
    }
}

fn archive_board_view_from_connection(
    connection: &Connection,
    tenant_id: &str,
    archive_id: &str,
) -> Result<Value, String> {
    let (title, imported_at, manifest_json): (String, i64, String) = connection
        .query_row(
            "SELECT title,imported_at,manifest_json FROM shared_archives WHERE id=?1 AND tenant_id=?2 AND source_type='board'",
            params![archive_id, tenant_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|_| "archive_board_not_found".to_string())?;
    let records = record_values(connection, archive_id)?;
    let board = records
        .iter()
        .find(|(_, kind, _)| kind == "board")
        .map(|(_, _, payload)| payload.clone())
        .unwrap_or(Value::Null);
    let mode = board_mode(&board);
    let shelves: Vec<Value> = records
        .iter()
        .filter(|(_, kind, payload)| {
            kind == "board_shelf"
                && payload
                    .get("deleted_at")
                    .map(Value::is_null)
                    .unwrap_or(true)
        })
        .map(|(_, _, payload)| {
            json!({"id":text(payload,"id"),"name":text(payload,"name"),"sortOrder":number(payload,"sort_order")})
        })
        .collect();
    let shelf_ids = shelves
        .iter()
        .filter_map(|shelf| shelf.get("id").and_then(Value::as_str))
        .collect::<HashSet<_>>();
    let comments: Vec<&Value> = records
        .iter()
        .filter(|(_, kind, payload)| {
            kind == "board_comment"
                && payload
                    .get("deleted_at")
                    .map(Value::is_null)
                    .unwrap_or(true)
        })
        .map(|(_, _, payload)| payload)
        .collect();
    let comment_ids = comments
        .iter()
        .map(|payload| text(payload, "id"))
        .collect::<HashSet<_>>();
    let reactions: Vec<&Value> = records
        .iter()
        .filter(|(_, kind, _)| kind == "board_reaction" || kind == "board_learning_reaction")
        .map(|(_, _, payload)| payload)
        .collect();
    let submissions = records
        .iter()
        .filter(|(_, kind, _)| kind == "board_record_submission")
        .map(|(_, _, payload)| (text(payload, "post_id"), payload))
        .collect::<HashMap<_, _>>();
    let snapshots: Vec<&Value> = records
        .iter()
        .filter(|(_, kind, _)| kind == "board_post_file_snapshot")
        .map(|(_, _, payload)| payload)
        .collect();
    let manifest: Value = serde_json::from_str(&manifest_json).unwrap_or(Value::Null);
    let file_ordinals = manifest
        .get("files")
        .and_then(Value::as_array)
        .map(|files| {
            files
                .iter()
                .filter_map(|file| Some((text(file, "fileId"), file.get("ordinal")?.as_i64()?)))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let mut file_statement = connection
        .prepare("SELECT ordinal,original_name,content_type,byte_size FROM shared_archive_files WHERE archive_id=?1 ORDER BY ordinal")
        .map_err(|e| format!("archive_board_files_prepare_failed:{e}"))?;
    let file_rows = file_statement
        .query_map(params![archive_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                json!({"ordinal":row.get::<_,i64>(0)?,"originalName":row.get::<_,String>(1)?,"contentType":row.get::<_,String>(2)?,"byteSize":row.get::<_,i64>(3)?}),
            ))
        })
        .map_err(|e| format!("archive_board_files_query_failed:{e}"))?
        .collect::<Result<HashMap<_, _>, _>>()
        .map_err(|e| format!("archive_board_files_row_failed:{e}"))?;
    let mut posts: Vec<Value> = Vec::new();
    for (_, _, post) in records.iter().filter(|(_, kind, payload)| {
        kind == "board_post"
            && payload
                .get("deleted_at")
                .map(Value::is_null)
                .unwrap_or(true)
    }) {
        let post_id = text(post, "id");
        let content_revision = number(post, "content_revision").max(1);
        let post_comments: Vec<Value> = comments
            .iter()
            .filter(|comment| text(comment, "post_id") == post_id)
            .map(|comment| {
                let parent = text(comment, "parent_comment_id");
                json!({"id":text(comment,"id"),"authorDisplayName":text(comment,"author_display_name"),"content":text(comment,"content"),
                    "parentCommentId":if parent.is_empty(){Value::Null}else{json!(parent)},"parentUnavailable":!parent.is_empty()&&!comment_ids.contains(&parent),
                    "depth":number(comment,"depth"),"createdAt":number(comment,"created_at")})
            })
            .collect();
        let mut reaction_counts = BTreeMap::<String, i64>::new();
        for reaction in reactions
            .iter()
            .filter(|reaction| text(reaction, "post_id") == post_id)
        {
            *reaction_counts
                .entry(text(reaction, "reaction_type"))
                .or_insert(0) += 1;
        }
        let post_files: Vec<Value> = snapshots
            .iter()
            .filter(|snapshot| {
                text(snapshot, "post_id") == post_id
                    && number(snapshot, "content_revision") == content_revision
            })
            .map(|snapshot| {
                let file_id = text(snapshot, "file_id");
                match file_ordinals
                    .get(&file_id)
                    .and_then(|ordinal| file_rows.get(ordinal))
                {
                    Some(file) => {
                        let mut result = file.clone();
                        result["purpose"] = json!(text(snapshot, "purpose"));
                        result["unavailable"] = json!(false);
                        result
                    }
                    None => json!({"purpose":text(snapshot,"purpose"),"unavailable":true}),
                }
            })
            .collect();
        let shelf_id = text(post, "shelf_id");
        let submission = submissions.get(&post_id);
        let answers = submission
            .and_then(|value| value.get("answers_json"))
            .and_then(Value::as_str)
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .unwrap_or(Value::Null);
        posts.push(json!({
            "id":post_id,"title":text(post,"title"),"content":text(post,"content"),"linkUrl":text(post,"link_url"),
            "authorDisplayName":text(post,"author_display_name"),"status":text(post,"status"),"moderationReason":text(post,"moderation_reason"),
            "backgroundId":text(post,"background_id"),"isPinned":number(post,"is_pinned")==1,
            "shelfId":if shelf_id.is_empty(){Value::Null}else{json!(shelf_id)},"shelfUnavailable":!shelf_id.is_empty()&&!shelf_ids.contains(shelf_id.as_str()),
            "shelfOrder":post.get("shelf_order").cloned().unwrap_or(Value::Null),"createdAt":number(post,"created_at"),"updatedAt":number(post,"updated_at"),
            "comments":post_comments,"reactions":reaction_counts,"attachments":post_files,
            "recordSubmission":submission.map(|value|json!({"formTitle":text(value,"form_title"),"submittedAt":number(value,"submitted_at"),"answers":answers})).unwrap_or(Value::Null)
        }));
    }
    posts.sort_by(|left, right| {
        right
            .get("isPinned")
            .and_then(Value::as_bool)
            .cmp(&left.get("isPinned").and_then(Value::as_bool))
            .then_with(|| number(right, "createdAt").cmp(&number(left, "createdAt")))
    });
    Ok(json!({
        "meta":{"archiveId":archive_id,"tenantId":tenant_id,"title":title,"subject":text(&board,"subject"),
            "studentViewMode":mode,"importedAt":imported_at,"postCount":posts.len(),"fileCount":file_rows.len()},
        "shelves":shelves,"posts":posts
    }))
}

#[cfg(test)]
mod tests {
    use super::archive_board_view_from_connection;
    use rusqlite::{params, Connection};
    use serde_json::json;

    #[test]
    fn board_view_reconstructs_active_content_with_exact_tenant_scope() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                r#"
                CREATE TABLE shared_archives(
                  id TEXT PRIMARY KEY,tenant_id TEXT NOT NULL,source_type TEXT NOT NULL,source_id TEXT NOT NULL,
                  title TEXT NOT NULL,manifest_sha256 TEXT NOT NULL,record_count INTEGER NOT NULL,file_count INTEGER NOT NULL,
                  total_file_bytes INTEGER NOT NULL,source_created_at INTEGER NOT NULL,source_expires_at INTEGER NOT NULL,
                  imported_at INTEGER NOT NULL,manifest_json TEXT NOT NULL
                );
                CREATE TABLE shared_archive_records(
                  archive_id TEXT NOT NULL,ordinal INTEGER NOT NULL,record_type TEXT NOT NULL,payload_json TEXT NOT NULL,
                  payload_sha256 TEXT NOT NULL,PRIMARY KEY(archive_id,ordinal)
                );
                CREATE TABLE shared_archive_files(
                  archive_id TEXT NOT NULL,ordinal INTEGER NOT NULL,original_name TEXT NOT NULL,content_type TEXT NOT NULL,
                  byte_size INTEGER NOT NULL,sha256 TEXT NOT NULL,local_path TEXT NOT NULL,PRIMARY KEY(archive_id,ordinal)
                );
                "#,
            )
            .unwrap();
        let manifest = json!({"files":[{"ordinal":0,"fileId":"file-1"}]}).to_string();
        connection.execute(
            "INSERT INTO shared_archives VALUES (?1,'tenant-a','board','board-1','우리 반 보드','manifest',9,1,128,1,2,3,?2)",
            params!["archive-board-1", manifest],
        ).unwrap();
        let records = [
            (
                0,
                "board",
                json!({"student_view_mode":"shelf","subject":"환경"}),
            ),
            (
                1,
                "board_shelf",
                json!({"id":"shelf-1","name":"실천 기록","sort_order":1}),
            ),
            (
                2,
                "board_post",
                json!({"id":"post-1","title":"텀블러","content":"일회용품을 줄였어요.","author_display_name":"김하늘","status":"approved","is_pinned":1,"shelf_id":"shelf-1","content_revision":2,"created_at":30,"updated_at":31,"link_url":"https://example.com"}),
            ),
            (
                3,
                "board_post",
                json!({"id":"post-deleted","deleted_at":32}),
            ),
            (
                4,
                "board_comment",
                json!({"id":"comment-1","post_id":"post-1","author_display_name":"이서준","content":"좋아요","parent_comment_id":"missing-parent","depth":1,"created_at":33}),
            ),
            (
                5,
                "board_reaction",
                json!({"post_id":"post-1","reaction_type":"like"}),
            ),
            (
                6,
                "board_reaction",
                json!({"post_id":"post-1","reaction_type":"like"}),
            ),
            (
                7,
                "board_post_file_snapshot",
                json!({"post_id":"post-1","content_revision":2,"file_id":"file-1","purpose":"board_post_attachment"}),
            ),
            (
                8,
                "board_record_submission",
                json!({"post_id":"post-1","form_title":"실천 기록","submitted_at":34,"answers_json":"{\"느낀 점\":\"뿌듯해요\"}"}),
            ),
        ];
        for (ordinal, kind, payload) in records {
            connection
                .execute(
                    "INSERT INTO shared_archive_records VALUES (?1,?2,?3,?4,'hash')",
                    params!["archive-board-1", ordinal, kind, payload.to_string()],
                )
                .unwrap();
        }
        connection.execute(
            "INSERT INTO shared_archive_files VALUES (?1,0,'실천사진.jpg','image/jpeg',128,'private-hash','/private/archive/photo.jpg')",
            params!["archive-board-1"],
        ).unwrap();

        assert_eq!(
            archive_board_view_from_connection(&connection, "tenant-b", "archive-board-1")
                .unwrap_err(),
            "archive_board_not_found"
        );
        let board =
            archive_board_view_from_connection(&connection, "tenant-a", "archive-board-1").unwrap();
        assert_eq!(board["meta"]["studentViewMode"], "shelf");
        assert_eq!(board["meta"]["postCount"], 1);
        assert_eq!(board["posts"][0]["comments"][0]["parentUnavailable"], true);
        assert_eq!(board["posts"][0]["reactions"]["like"], 2);
        assert_eq!(
            board["posts"][0]["attachments"][0]["originalName"],
            "실천사진.jpg"
        );
        assert_eq!(
            board["posts"][0]["recordSubmission"]["answers"]["느낀 점"],
            "뿌듯해요"
        );
        let encoded = board.to_string();
        assert!(!encoded.contains("/private/archive"));
        assert!(!encoded.contains("private-hash"));
    }
}
