//! Index-independent, in-memory PDF lookup. Source ownership stays in the parent store.
use super::*;

fn pdf_fallback_sources(store: &SqliteStore, owner: &str, input: &Value) -> Result<Vec<Value>, String> {
    let lessons = input["lessons"].as_array().filter(|v| !v.is_empty() && v.len() <= 100).ok_or("INVALID_LOCAL_READ_REQUEST")?;
    let ids = {
        let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
        let mut stmt = conn.prepare("SELECT s.source_id FROM teaching_sources s WHERE s.owner_uid=?1 AND s.lifecycle_status='active' AND s.content_type='application/pdf' AND s.sha256 IS NOT NULL AND s.local_path IS NOT NULL AND NOT (s.extraction_status='ready' AND COALESCE(s.extractor_version,'')='browser-worker-v3-sparse-pages' AND COALESCE(s.page_count,0)>0 AND EXISTS (SELECT 1 FROM teaching_source_chunks c WHERE c.owner_uid=s.owner_uid AND c.source_id=s.source_id) AND NOT EXISTS (SELECT 1 FROM teaching_source_chunks c WHERE c.owner_uid=s.owner_uid AND c.source_id=s.source_id AND (NOT (length(c.chunk_id)=11 AND substr(c.chunk_id,1,5)='page-' AND substr(c.chunk_id,6) NOT GLOB '*[^0-9]*' AND c.page_start=c.page_end AND c.page_start=CAST(substr(c.chunk_id,6) AS INTEGER) AND c.page_start BETWEEN 1 AND s.page_count AND c.ordinal=c.page_start-1 AND length(c.text) BETWEEN 1 AND 1200) OR c.page_start IS NULL OR c.page_end IS NULL OR NOT EXISTS (SELECT 1 FROM teaching_source_chunks_fts f WHERE f.owner_uid=c.owner_uid AND f.source_id=c.source_id AND f.chunk_id=c.chunk_id)))) ORDER BY s.source_id")
            .map_err(|_| "db_teaching_source_fallback_failed")?;
        let rows = stmt.query_map(params![owner], |row| row.get::<_,String>(0)).map_err(|_| "db_teaching_source_fallback_failed")?;
        rows.collect::<Result<Vec<_>,_>>().map_err(|_| "db_teaching_source_fallback_failed")?
    };
    let types = input["sourceTypes"].as_array();
    let mut sources = Vec::new();
    for id in ids {
        let Some(mut source) = source_row(store, owner, &id)? else { continue; };
        if types.is_some_and(|values| !values.is_empty() && !values.contains(&source["sourceType"])) { continue; }
        let eligible = lessons.iter().filter(|lesson| {
            let grade = lesson["grade"].as_i64().filter(|v| (1..=12).contains(v));
            let subject = lesson["subjectCode"].as_str().filter(|v| !v.is_empty()).or_else(|| lesson["subject"].as_str()).unwrap_or("");
            grade.is_some_and(|g| source["grade"] == g.to_string() || source["grade"] == format!("{g}학년"))
                && !subject.is_empty() && source["subjectCode"] == subject
                && (source["semesterScope"] == "full_year" || source["semesterScope"] == lesson["semester"].as_i64().unwrap_or(0).to_string())
        }).cloned().collect::<Vec<_>>();
        if !eligible.is_empty() { source["lessons"] = json!(eligible); sources.push(source); }
    }
    Ok(sources)
}

fn original_source_current(store: &SqliteStore, owner: &str, source: &str, revision: i64, sha: &str) -> Result<Value, String> {
    let row = source_row(store, owner, source)?.ok_or("teaching_source_stale")?;
    if row["revision"].as_i64() != Some(revision) || row["sha256"] != sha
        || row["lifecycleStatus"] != "active" || row["contentType"] != "application/pdf" {
        return Err("teaching_source_stale".into());
    }
    Ok(row)
}

fn pdf_scan_cursor(source: usize, page: usize, lesson: usize, has_text: bool, failed: bool, digest: &str) -> Value {
    json!({"source":source,"page":page,"lesson":lesson,"hasText":has_text,"failed":failed,"digest":digest})
}

fn fallback_status(source: &Value, status: &str) -> Value {
    let lesson = &source["lessons"][0];
    json!({"sourceRef":source["sourceId"],"sourceRevision":source["revision"],"fileSha256":source["sha256"],
        "title":source["title"],"status":status,
        "lessonRef":{"dateKey":lesson["dateKey"],"period":lesson["period"],"revision":lesson["revision"]}})
}

fn normalized_pdf_query(value: &str) -> String {
    value.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

pub(super) fn matches(store: &SqliteStore, tenant: &str, owner: &str, input: &Value) -> Result<Value, String> {
    let sources = pdf_fallback_sources(store, owner, input)?;
    let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&sources).map_err(|_| "teaching_source_snapshot_failed")?));
    if input.get("scan").is_none() {
        let mut indexed = mcp_index_matches(store, tenant, owner, input)?;
        if indexed["complete"] == true && !sources.is_empty() {
            indexed["complete"] = json!(false);
            indexed["nextOffset"] = json!(0);
            indexed["nextScan"] = pdf_scan_cursor(0, 0, 0, false, false, &digest);
        }
        return Ok(indexed);
    }
    let scan = &input["scan"];
    if scan["digest"] != digest { return Ok(json!({"snapshotStale":true})); }
    let mut source_index = scan["source"].as_u64().ok_or("INVALID_LOCAL_READ_REQUEST")? as usize;
    let mut page_index = scan["page"].as_u64().ok_or("INVALID_LOCAL_READ_REQUEST")? as usize;
    let mut lesson_index = scan["lesson"].as_u64().ok_or("INVALID_LOCAL_READ_REQUEST")? as usize;
    let mut has_text = scan["hasText"].as_bool().ok_or("INVALID_LOCAL_READ_REQUEST")?;
    let mut failed = scan["failed"].as_bool().ok_or("INVALID_LOCAL_READ_REQUEST")?;
    if source_index >= sources.len() { return Err("INVALID_LOCAL_READ_REQUEST".into()); }
    let limit = input["limit"].as_u64().unwrap_or(12).clamp(1, MAX_MCP_MATCHES as u64) as usize;
    let stop_at = Utc::now().timestamp_millis() + 20_000;
    let mut matches = Vec::new();
    let mut statuses = Vec::new();
    let mut progressed = false;
    'sources: while source_index < sources.len() {
        if progressed && (Utc::now().timestamp_millis() >= stop_at || matches.len() >= limit || statuses.len() >= 20) { break; }
        let source = &sources[source_index];
        let source_id = source["sourceId"].as_str().ok_or("teaching_source_stale")?;
        let sha = source["sha256"].as_str().ok_or("teaching_source_stale")?;
        let revision = source["revision"].as_i64().ok_or("teaching_source_stale")?;
        original_source_current(store, owner, source_id, revision, sha)?;
        let file = read_verified_managed_file(store, tenant, owner, source_id, sha);
        let pdf = match file {
            Ok(bytes) => match Pdf::new(bytes) { Ok(pdf) => Some(pdf), Err(_) => { statuses.push(fallback_status(source, "file_unavailable")); None } },
            Err(_) => { statuses.push(fallback_status(source, "file_unavailable")); None }
        };
        if let Some(pdf) = pdf {
            if pdf.pages().len() > 2_000 { statuses.push(fallback_status(source, "search_limit")); }
            else {
                let cache = hayro::hayro_interpret::InterpreterCache::new();
                let lessons = source["lessons"].as_array().ok_or("INVALID_LOCAL_READ_REQUEST")?;
                while page_index < pdf.pages().len() {
                    if progressed && (Utc::now().timestamp_millis() >= stop_at || matches.len() >= limit) { break 'sources; }
                    let text = match pdf_text::extract(&pdf.pages()[page_index], &cache) {
                        Ok(text) => text,
                        Err(_) => { failed = true; page_index += 1; lesson_index = 0; progressed = true; continue; }
                    };
                    let searchable = normalized_pdf_query(&text);
                    has_text |= !searchable.is_empty();
                    while lesson_index < lessons.len() {
                        if matches.len() >= limit { break 'sources; }
                        let lesson = &lessons[lesson_index];
                        let context = [lesson["unit"].as_str().unwrap_or(""), lesson["title"].as_str().unwrap_or(""),
                            lesson["curriculumItemLabel"].as_str().unwrap_or("")].join(" ");
                        let query = input["query"].as_str().unwrap_or("");
                        let contextual_hit = context.split(|c: char| !c.is_alphanumeric()).map(normalized_pdf_query)
                            .filter(|term| term.chars().count() >= 2).any(|term| searchable.contains(&term));
                        let query_hit = query.split(|c: char| !c.is_alphanumeric()).map(normalized_pdf_query)
                            .filter(|term| term.chars().count() >= 2 && *term != normalized_pdf_query(lesson["subjectCode"].as_str().unwrap_or("")))
                            .any(|term| searchable.contains(&term));
                        if contextual_hit || context.trim().is_empty() && query_hit {
                            matches.push(json!({"sourceRef":source_id,"sourceRevision":revision,"fileSha256":sha,
                                "chunkRef":format!("page-{:06}",page_index+1),"chunkRevision":null,"readBasis":"original_pdf",
                                "contentType":"application/pdf","extractorVersion":"native-pdf-text-v1",
                                "title":source["title"],"sourceType":source["sourceType"],"grade":source["grade"],
                                "semesterScope":source["semesterScope"],"subjectCode":source["subjectCode"],"publisher":source["publisher"],
                                "pageStart":page_index+1,"pageEnd":page_index+1,"unit":lesson["unit"],"topic":lesson["title"],
                                "snippet":text.chars().take(900).collect::<String>(),"matchKind":"pdf_text_search","linkStatus":"unverified",
                                "matchReason":"로컬 PDF 텍스트층에서 수업 문맥을 검색함. 페이지를 읽어 관련성을 확인하세요.",
                                "lessonRef":{"dateKey":lesson["dateKey"],"period":lesson["period"],"revision":lesson["revision"]}}));
                        }
                        lesson_index += 1;
                        progressed = true;
                    }
                    page_index += 1;
                    lesson_index = 0;
                    progressed = true;
                }
                if failed { statuses.push(fallback_status(source, "text_extraction_failed")); }
                else if !has_text { statuses.push(fallback_status(source, "no_text")); }
            }
        } else if statuses.last().is_none_or(|row| row["sourceRef"] != source["sourceId"]) {
            statuses.push(fallback_status(source, "text_extraction_failed"));
        }
        original_source_current(store, owner, source_id, revision, sha)?;
        source_index += 1; page_index = 0; lesson_index = 0; has_text = false; failed = false; progressed = true;
    }
    // Recheck catalog/revisions even when a page/result budget interrupted this scan.
    let current = pdf_fallback_sources(store, owner, input)?;
    if format!("{:x}", Sha256::digest(serde_json::to_vec(&current).map_err(|_| "teaching_source_snapshot_failed")?)) != digest {
        return Ok(json!({"snapshotStale":true}));
    }
    let complete = source_index >= sources.len();
    Ok(json!({"matches":matches,"diagnostics":[],"diagnosticsTruncated":false,"fallbackSources":statuses,
        "complete":complete,"nextOffset":if complete {Value::Null} else {json!(0)},"snapshotDigest":digest,"snapshotStale":false,
        "strategy":"exact_link_then_lesson_context_fts_then_subject_query_fts",
        "nextScan":if complete { Value::Null } else { pdf_scan_cursor(source_index,page_index,lesson_index,has_text,failed,&digest) }}))
}

pub(super) fn page_refs(store: &SqliteStore, tenant: &str, owner: &str, input: &Value) -> Result<Value, String> {
    let source = safe_id(input["sourceRef"].as_str().unwrap_or(""), 160).ok_or("INVALID_LOCAL_READ_REQUEST")?;
    let revision = input["sourceRevision"].as_i64().filter(|v| *v > 0).ok_or("INVALID_LOCAL_READ_REQUEST")?;
    let sha = input["fileSha256"].as_str().ok_or("INVALID_LOCAL_READ_REQUEST")?;
    let start = input["pageStart"].as_u64().filter(|v| *v > 0).ok_or("INVALID_LOCAL_READ_REQUEST")?;
    let end = input["pageEnd"].as_u64().filter(|v| *v >= start && *v - start < 4 && *v <= 2_000).ok_or("INVALID_LOCAL_READ_REQUEST")?;
    original_source_current(store, owner, &source, revision, sha)?;
    let pdf = Pdf::new(read_verified_managed_file(store, tenant, owner, &source, sha)?).map_err(|_| "teaching_source_page_render_failed")?;
    if end > pdf.pages().len() as u64 { return Err("teaching_source_page_range_invalid".into()); }
    original_source_current(store, owner, &source, revision, sha)?;
    Ok(json!({"pageNumbers":(start..=end).collect::<Vec<_>>(),"complete":true}))
}
