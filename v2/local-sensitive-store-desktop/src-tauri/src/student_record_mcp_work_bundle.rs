use super::protocol::DraftInput;
use super::*;

impl StudentRecordMcpManager {
    fn selection(&self, scope_id: &str, handle: &str) -> Result<(String, String, Value), String> {
        self.purge()?;
        let conn = self
            .db
            .lock()
            .map_err(|_| "student_record_mcp_db_lock_failed".to_string())?;
        let row = conn
            .query_row(
                "SELECT grant_id,tenant_id,payload_json FROM selections WHERE scope_id=?1 AND selection_handle=?2 AND expires_at>?3",
                params![scope_id, handle, now_ms()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("student_record_mcp_selection_query_failed:{error}"))?
            .ok_or_else(|| "SELECTION_EXPIRED".to_string())?;
        Ok((
            row.0,
            row.1,
            json_object(&row.2, "LOCAL_SELECTION_REQUIRED")?,
        ))
    }

    fn authoritative_evidence(
        &self,
        tenant: &str,
        scope: &Value,
        student: &Value,
        identities: &[Value],
    ) -> Result<(Vec<Value>, usize), String> {
        let code = clean(student.get("studentCode"), 80);
        let refs = student
            .get("evidence")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let from = clean(scope.get("fromDate"), 10);
        let to = clean(scope.get("toDate"), 10);
        let mut evidence = Vec::new();
        let mut attendance_count = 0;
        let conn = self
            .store
            .conn
            .lock()
            .map_err(|_| "db_lock_failed".to_string())?;
        for reference in refs {
            let kind = clean(reference.get("kind"), 20);
            let id = clean(reference.get("recordId"), 260);
            if kind == "observation" {
                let raw:Option<String>=conn.query_row("SELECT payload_json FROM lesson_observations WHERE tenant_id=?1 AND doc_id=?2 AND student_code=?3 AND date_key BETWEEN ?4 AND ?5",params![tenant,id,code,from,to],|row|row.get(0)).optional().map_err(|e|format!("student_record_mcp_observation_query_failed:{e}"))?;
                let value = json_object(
                    &raw.ok_or_else(|| "LOCAL_SELECTION_REQUIRED".to_string())?,
                    "LOCAL_SELECTION_REQUIRED",
                )?;
                if !observation_matches(&value, scope) {
                    return Err("LOCAL_SELECTION_REQUIRED".to_string());
                }
                let raw_text = value_text(
                    &value,
                    &["note", "content", "observationText", "memo", "comment"],
                );
                let normalized = sanitize(&raw_text, identities)?;
                if normalized.is_empty() || normalized.chars().count() > MAX_EVIDENCE_ITEM_CHARS {
                    return Err("EVIDENCE_LIMIT_EXCEEDED".to_string());
                }
                let source = if clean(value.get("sourceType"), 80) == "teacherManualSupplement" {
                    "manual_supplement"
                } else {
                    "observation"
                };
                evidence.push(json!({"sourceType":source,"dateKey":source_date(&value),"subject":source_subject(&value),"text":normalized,"_key":format!("observation:{id}")}));
            } else if kind == "evaluation" {
                if scope.get("recordType").and_then(Value::as_str) != Some("subjects") {
                    return Err("LOCAL_SELECTION_REQUIRED".to_string());
                }
                let pair:Option<(String,String)>=conn.query_row("SELECT r.payload_json,a.payload_json FROM eval_results r JOIN eval_assignments a ON a.tenant_id=r.tenant_id AND a.assignment_id=r.assignment_id WHERE r.tenant_id=?1 AND r.result_id=?2 AND r.student_id=?3 AND r.date_key BETWEEN ?4 AND ?5",params![tenant,id,code,from,to],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(|e|format!("student_record_mcp_evaluation_query_failed:{e}"))?;
                let (result_raw, assignment_raw) =
                    pair.ok_or_else(|| "LOCAL_SELECTION_REQUIRED".to_string())?;
                let result = json_object(&result_raw, "LOCAL_SELECTION_REQUIRED")?;
                let assignment = json_object(&assignment_raw, "LOCAL_SELECTION_REQUIRED")?;
                let mut merged = assignment.as_object().cloned().unwrap_or_default();
                if let Some(object) = result.as_object() {
                    for (key, value) in object {
                        merged.insert(key.clone(), value.clone());
                    }
                }
                let value = Value::Object(merged);
                if !evaluation_included(&value, &code)
                    || source_subject(&value) != clean(scope.get("subject"), 160)
                {
                    return Err("LOCAL_SELECTION_REQUIRED".to_string());
                }
                let normalized = sanitize(&evaluation_text(&value), identities)?;
                if normalized.is_empty() || normalized.chars().count() > MAX_EVIDENCE_ITEM_CHARS {
                    return Err("EVIDENCE_LIMIT_EXCEEDED".to_string());
                }
                evidence.push(json!({"sourceType":"evaluation","dateKey":source_date(&value),"subject":clean(scope.get("subject"),160),"text":normalized,"_key":format!("evaluation:{id}")}));
            } else if kind == "attendance" {
                if scope.get("recordType").and_then(Value::as_str) != Some("behavior") {
                    return Err("LOCAL_SELECTION_REQUIRED".to_string());
                }
                let raw:Option<String>=conn.query_row("SELECT payload_json FROM attendance_records WHERE tenant_id=?1 AND record_id=?2 AND student_code=?3 AND date_key BETWEEN ?4 AND ?5",params![tenant,id,code,from,to],|row|row.get(0)).optional().map_err(|e|format!("student_record_mcp_attendance_query_failed:{e}"))?;
                let value = json_object(
                    &raw.ok_or_else(|| "LOCAL_SELECTION_REQUIRED".to_string())?,
                    "LOCAL_SELECTION_REQUIRED",
                )?;
                if clean(value.get("status"), 40) == "present" {
                    return Err("LOCAL_SELECTION_REQUIRED".to_string());
                }
                attendance_count += 1;
            }
        }
        evidence.sort_by(|a, b| {
            clean(a.get("dateKey"), 10)
                .cmp(&clean(b.get("dateKey"), 10))
                .then(clean(a.get("sourceType"), 30).cmp(&clean(b.get("sourceType"), 30)))
                .then(clean(a.get("_key"), 300).cmp(&clean(b.get("_key"), 300)))
        });
        let chars: usize = evidence
            .iter()
            .map(|row| {
                clean(row.get("text"), MAX_EVIDENCE_CHARS + 1)
                    .chars()
                    .count()
            })
            .sum();
        if evidence.len() > MAX_EVIDENCE
            || chars > MAX_EVIDENCE_CHARS
            || (evidence.is_empty() && attendance_count == 0)
        {
            return Err(if chars > MAX_EVIDENCE_CHARS {
                "EVIDENCE_LIMIT_EXCEEDED"
            } else {
                "LOCAL_SELECTION_REQUIRED"
            }
            .to_string());
        }
        for row in &mut evidence {
            if let Some(object) = row.as_object_mut() {
                object.remove("_key");
            }
        }
        Ok((evidence, attendance_count))
    }

    pub(crate) fn list_scopes(&self) -> Result<Value, String> {
        let connection = self.active_connection(None)?;
        self.authorize(&connection, "student_record_list_scopes")?;
        let grant = clean(connection.get("grantId"), 128);
        let conn = self
            .db
            .lock()
            .map_err(|_| "student_record_mcp_db_lock_failed".to_string())?;
        let mut stmt=conn.prepare("SELECT scope_id,selection_handle,payload_json,expires_at FROM selections WHERE grant_id=?1 AND expires_at>?2 ORDER BY created_at DESC").map_err(|e|format!("student_record_mcp_scope_query_failed:{e}"))?;
        let rows = stmt
            .query_map(params![grant, now_ms()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(|e| format!("student_record_mcp_scope_query_failed:{e}"))?;
        let mut scopes = Vec::new();
        for row in rows {
            let (scope_id, handle, raw, expires) =
                row.map_err(|e| format!("student_record_mcp_scope_row_failed:{e}"))?;
            let payload = json_object(&raw, "LOCAL_SELECTION_REQUIRED")?;
            let students = payload
                .get("students")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            scopes.push(json!({"scopeId":scope_id,"selectionHandle":handle,"recordType":payload.get("recordType"),"subject":payload.get("subject"),"creativeArea":payload.get("creativeArea"),"period":{"from":payload.get("fromDate"),"to":payload.get("toDate")},"studentCount":students.len(),"evidenceCount":students.iter().map(|row|row.get("evidence").and_then(Value::as_array).map(Vec::len).unwrap_or(0)).sum::<usize>(),"expiresAt":expires}));
        }
        Ok(
            json!({"connection":{"mode":connection.get("mode"),"expiresAt":connection.get("expiresAt")},"limits":{"maxStudents":MAX_STUDENTS,"maxEvidencePerStudent":MAX_EVIDENCE,"maxEvidenceCharactersPerStudent":MAX_EVIDENCE_CHARS,"maxEvidenceCharacters":MAX_EVIDENCE_ITEM_CHARS,"maxDraftCharacters":MAX_DRAFT_CHARS},"scopes":scopes}),
        )
    }

    pub(crate) fn prepare_context(&self, scope_id: &str, handle: &str) -> Result<Value, String> {
        let (grant, tenant, scope) = self.selection(scope_id, handle)?;
        let connection = self.active_connection(Some(&grant))?;
        self.authorize(&connection, "student_record_prepare_context")?;
        {
            let conn = self
                .db
                .lock()
                .map_err(|_| "student_record_mcp_db_lock_failed".to_string())?;
            if let Some(raw) = conn
                .query_row(
                    "SELECT context_json FROM bundles WHERE selection_handle=?1 AND expires_at>?2",
                    params![handle, now_ms()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|e| format!("student_record_mcp_bundle_query_failed:{e}"))?
            {
                return json_object(&raw, "WORK_BUNDLE_EXPIRED");
            }
        }
        let source = scope
            .get("students")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let identities:Vec<Value>=source.iter().enumerate().map(|(index,row)|json!({"alias":format!("학생-{:02}",index+1),"studentCode":row.get("studentCode"),"studentName":row.get("studentName"),"classNo":row.get("classNo"),"evidence":row.get("evidence")})).collect();
        let aliases: HashMap<String, String> = identities
            .iter()
            .map(|identity| {
                (
                    clean(identity.get("studentCode"), 80),
                    clean(identity.get("alias"), 20),
                )
            })
            .collect();
        let privacy_identities: Vec<Value> = scope
            .get("privacyRoster")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|identity| {
                let code = clean(identity.get("studentCode"), 80);
                json!({
                    "studentCode": code,
                    "studentName": identity.get("studentName"),
                    "alias": aliases.get(&code).cloned().unwrap_or_else(|| "다른 학생".to_string()),
                })
            })
            .collect();
        let mut external = Vec::new();
        let mut mapping = Vec::new();
        let mut evidence_total = 0;
        for identity in &identities {
            let (evidence, attendance) =
                self.authoritative_evidence(&tenant, &scope, identity, &privacy_identities)?;
            evidence_total += evidence.len();
            let code = clean(identity.get("studentCode"), 80);
            let base = self.latest_draft(&tenant, &code, &scope)?;
            mapping.push(json!({"alias":identity.get("alias"),"studentCode":code,"studentName":identity.get("studentName"),"classNo":identity.get("classNo"),"baseDraftDigest":base.digest}));
            external.push(json!({"alias":identity.get("alias"),"evidence":evidence,"attendanceConflictCount":attendance}));
        }
        let now = now_ms();
        let bundle = identifier("srmcp-bundle");
        let context = json!({"workBundleId":bundle,"recordType":scope.get("recordType"),"subject":scope.get("subject"),"creativeArea":scope.get("creativeArea"),"period":{"from":scope.get("fromDate"),"to":scope.get("toDate")},"students":external,"privacy":{"aliasesOnly":true,"excluded":["실제 이름","학생 코드·내부 ID","상담 내용","출결 상세","평가 점수"],"teacherReviewRequired":true},"expiresAt":now+BUNDLE_TTL_MS});
        let mapping_value =
            json!({"scope":scope,"identities":mapping,"privacyIdentities":privacy_identities});
        let conn = self
            .db
            .lock()
            .map_err(|_| "student_record_mcp_db_lock_failed".to_string())?;
        conn.execute("INSERT INTO bundles(bundle_id,selection_handle,grant_id,context_json,mapping_json,created_at,expires_at,state,save_digest,receipt_json) VALUES(?1,?2,?3,?4,?5,?6,?7,'active',NULL,NULL)",params![bundle,handle,grant,serde_json::to_string(&context).unwrap(),serde_json::to_string(&mapping_value).unwrap(),now,now+BUNDLE_TTL_MS]).map_err(|e|format!("student_record_mcp_bundle_insert_failed:{e}"))?;
        drop(conn);
        self.audit(
            "context_prepared",
            &grant,
            scope
                .get("recordType")
                .and_then(Value::as_str)
                .unwrap_or(""),
            identities.len(),
            evidence_total,
        )?;
        Ok(context)
    }

    fn bundle(
        &self,
        id: &str,
    ) -> Result<
        (
            String,
            String,
            Value,
            String,
            Option<String>,
            Option<String>,
        ),
        String,
    > {
        self.purge()?;
        let conn = self
            .db
            .lock()
            .map_err(|_| "student_record_mcp_db_lock_failed".to_string())?;
        conn.query_row("SELECT grant_id,state,mapping_json,context_json,save_digest,receipt_json FROM bundles WHERE bundle_id=?1 AND expires_at>?2",params![id,now_ms()],|row|Ok((row.get(0)?,row.get(1)?,json_object(&row.get::<_,String>(2)?,"WORK_BUNDLE_EXPIRED").map_err(|_|rusqlite::Error::InvalidQuery)?,row.get(3)?,row.get(4)?,row.get(5)?))).optional().map_err(|e|format!("student_record_mcp_bundle_query_failed:{e}"))?.ok_or_else(||"WORK_BUNDLE_EXPIRED".to_string())
    }
    pub(crate) fn get_drafts(&self, bundle_id: &str) -> Result<Value, String> {
        let (grant, _, mapping, _, _, _) = self.bundle(bundle_id)?;
        let connection = self.active_connection(Some(&grant))?;
        self.authorize(&connection, "student_record_get_drafts")?;
        let tenant = clean(connection.get("tenantId"), 128);
        let scope = mapping.get("scope").cloned().unwrap_or_else(|| json!({}));
        let identities = mapping
            .get("identities")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let privacy_identities = mapping
            .get("privacyIdentities")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| identities.clone());
        let mut drafts = Vec::new();
        for identity in &identities {
            let current =
                self.latest_draft(&tenant, &clean(identity.get("studentCode"), 80), &scope)?;
            drafts.push(json!({"alias":identity.get("alias"),"text":sanitize(&current.text,&privacy_identities)?,"status":if current.status.is_empty(){"missing"}else{&current.status},"revision":format!("draft:{}",&current.digest[..24])}));
        }
        Ok(json!({"workBundleId":bundle_id,"drafts":drafts,"teacherReviewRequired":true}))
    }
    pub(crate) fn save_drafts(
        &self,
        bundle_id: &str,
        rows: &[DraftInput],
    ) -> Result<Value, String> {
        let _save_guard = self
            .save_lock
            .lock()
            .map_err(|_| "student_record_mcp_save_lock_failed".to_string())?;
        let _process_save_guard = self.process_save_lock()?;
        let (grant, state, mapping, _, prior_digest, receipt) = self.bundle(bundle_id)?;
        let connection = self.active_connection(Some(&grant))?;
        self.authorize(&connection, "student_record_save_drafts")?;
        if clean(connection.get("mode"), 20) != "read_write" {
            return Err("MCP_SCOPE_DENIED".to_string());
        }
        let tenant = clean(connection.get("tenantId"), 128);
        let scope = mapping.get("scope").cloned().unwrap_or_else(|| json!({}));
        let identities = mapping
            .get("identities")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let privacy_identities = mapping
            .get("privacyIdentities")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| identities.clone());
        if rows.len() != identities.len() {
            return Err("LOCAL_SELECTION_REQUIRED".to_string());
        }
        let mut by_alias = HashMap::new();
        for row in rows {
            if row.alias.is_empty()
                || row.text.trim().is_empty()
                || row.text.chars().count() > MAX_DRAFT_CHARS
                || by_alias
                    .insert(row.alias.clone(), row.text.trim().to_string())
                    .is_some()
            {
                return Err("EVIDENCE_LIMIT_EXCEEDED".to_string());
            }
            reject_draft_pii(&row.text, &privacy_identities)?;
        }
        if identities
            .iter()
            .any(|identity| !by_alias.contains_key(&clean(identity.get("alias"), 20)))
        {
            return Err("LOCAL_SELECTION_REQUIRED".to_string());
        }
        let normalized: Vec<Value> = identities
            .iter()
            .map(|identity| {
                let alias = clean(identity.get("alias"), 20);
                json!({"alias":alias,"text":by_alias.get(&alias)})
            })
            .collect();
        let save_hash = digest(&serde_json::to_string(&normalized).unwrap());
        if state == "saved" {
            if prior_digest.as_deref() != Some(&save_hash) {
                return Err("WORK_BUNDLE_CONSUMED".to_string());
            }
            return receipt
                .and_then(|raw| json_object(&raw, "WORK_BUNDLE_CONSUMED").ok())
                .ok_or_else(|| "WORK_BUNDLE_CONSUMED".to_string());
        }
        if state == "saving" && prior_digest.as_deref() != Some(&save_hash) {
            return Err("WORK_BUNDLE_CONSUMED".to_string());
        }
        let set_id = format!("mcp_{}", &digest(bundle_id)[..24]);
        let mut recovered_saved_at = None;
        if state == "active" {
            for identity in &identities {
                let current =
                    self.latest_draft(&tenant, &clean(identity.get("studentCode"), 80), &scope)?;
                if current.digest != clean(identity.get("baseDraftDigest"), 64) {
                    return Err("DRAFT_CONFLICT".to_string());
                }
            }
            let claimed = self.db.lock().map_err(|_|"student_record_mcp_db_lock_failed".to_string())?.execute("UPDATE bundles SET state='saving',save_digest=?1 WHERE bundle_id=?2 AND state='active'",params![save_hash,bundle_id]).map_err(|e|format!("student_record_mcp_bundle_update_failed:{e}"))?;
            if claimed != 1 {
                return Err("WORK_BUNDLE_CONSUMED".to_string());
            }
        } else {
            recovered_saved_at =
                self.saved_mcp_batch(&tenant, &set_id, &scope, &identities, &by_alias)?;
            if recovered_saved_at.is_none() {
                for identity in &identities {
                    let current = self.latest_draft(
                        &tenant,
                        &clean(identity.get("studentCode"), 80),
                        &scope,
                    )?;
                    if current.digest != clean(identity.get("baseDraftDigest"), 64) {
                        return Err("DRAFT_CONFLICT".to_string());
                    }
                }
            }
        }
        let now = recovered_saved_at.unwrap_or_else(now_ms);
        let record_type = clean(scope.get("recordType"), 20);
        let set = json!({"tenantId":tenant,"draftSetId":set_id,"status":"draft","recordTypes":[record_type],"subject":scope.get("subject"),"creativeArea":scope.get("creativeArea"),"fromDate":scope.get("fromDate"),"toDate":scope.get("toDate"),"sourceType":"studentRecordMcp","sourceLabel":"내 ChatGPT","teacherReviewRequired":true,"createdAtMs":now,"updatedAtMs":now});
        let mut local = Vec::new();
        for identity in &identities {
            let alias = clean(identity.get("alias"), 20);
            let value = by_alias.get(&alias).cloned().unwrap_or_default();
            let code = clean(identity.get("studentCode"), 80);
            local.push(json!({"tenantId":tenant,"draftSetId":set_id,"draftId":format!("{set_id}__{code}"),"studentCode":code,"studentName":identity.get("studentName"),"classNo":identity.get("classNo"),"recordType":record_type,"status":"draft","behaviorComment":if record_type=="behavior"{value.clone()}else{String::new()},"subjectComments":if record_type=="subjects"{json!([{"subject":scope.get("subject"),"comment":value}])}else{json!([])},"creativeComments":if record_type=="creative"{json!([{"area":scope.get("creativeArea"),"comment":value}])}else{json!([])},"sourceType":"studentRecordMcp","sourceLabel":"내 ChatGPT","teacherReviewRequired":true,"createdAtMs":now,"updatedAtMs":now}));
        }
        if recovered_saved_at.is_none() {
            self.store
                .save_student_record_draft_batch(
                    json!({"tenantId":tenant,"draftSet":set,"drafts":local}),
                )
                .map_err(|_| "LOCAL_STORE_WRITE_FAILED".to_string())?;
        }
        let receipt = json!({"workBundleId":bundle_id,"saved":identities.iter().map(|identity|json!({"alias":identity.get("alias"),"status":"draft"})).collect::<Vec<_>>(),"draftSetReference":set_id,"source":"내 ChatGPT","teacherReviewRequired":true,"savedAt":now});
        let changed = self.db.lock().map_err(|_|"student_record_mcp_db_lock_failed".to_string())?.execute("UPDATE bundles SET state='saved',receipt_json=?1 WHERE bundle_id=?2 AND state='saving' AND save_digest=?3",params![serde_json::to_string(&receipt).unwrap(),bundle_id,save_hash]).map_err(|e|format!("student_record_mcp_bundle_update_failed:{e}"))?;
        if changed > 0 {
            self.audit("drafts_saved", &grant, &record_type, identities.len(), 0)?;
            return Ok(receipt);
        }
        let conn = self
            .db
            .lock()
            .map_err(|_| "student_record_mcp_db_lock_failed".to_string())?;
        let settled: Option<(String, String)> = conn
            .query_row(
                "SELECT save_digest,receipt_json FROM bundles WHERE bundle_id=?1 AND state='saved'",
                params![bundle_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| format!("student_record_mcp_bundle_query_failed:{e}"))?;
        match settled {
            Some((digest, raw)) if digest == save_hash => json_object(&raw, "WORK_BUNDLE_CONSUMED"),
            _ => Err("WORK_BUNDLE_CONSUMED".to_string()),
        }
    }
}
