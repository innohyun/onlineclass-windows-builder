//! Bounded, connection-scoped reads. The socket thread never runs SQLite or waits
//! for a restore lock. Each connection owns its deadline/progress handler, so a
//! cancelled read cannot interrupt an unrelated local write.
use super::*;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

const MAX_PENDING: usize = 8;
const RESPONSE_MARGIN_MS: i64 = 1_500;
const READ_CLOCK_SKEW_TOLERANCE_MS: i64 = 30_000;
static DIAGNOSTICS: Mutex<BTreeMap<String, Vec<Value>>> = Mutex::new(BTreeMap::new());

fn bounded_read_remaining(deadline_at: i64, now: i64, budget_ms: i64) -> Result<i64, &'static str> {
    let remaining = deadline_at.checked_sub(now).ok_or("INVALID_LOCAL_READ_REQUEST")?;
    if remaining <= RESPONSE_MARGIN_MS { return Err("LOCAL_DB_QUERY_TIMEOUT"); }
    if remaining > budget_ms + READ_CLOCK_SKEW_TOLERANCE_MS { return Err("INVALID_LOCAL_READ_REQUEST"); }
    // The relay enforces its original deadline. A slower local clock must never
    // turn its absolute timestamp into a longer native execution budget.
    Ok(remaining.min(budget_ms))
}

pub(super) fn diagnostics_for_tenant(tenant: &str) -> Vec<Value> {
    DIAGNOSTICS.lock().ok().and_then(|items| items.get(tenant).cloned()).unwrap_or_default()
}

pub(super) fn trace(frame: &Value, stage: &str, started: Instant, code: &str, store: Option<&SqliteStore>) {
    let request = frame["requestId"].as_str().filter(|value| id(value)).unwrap_or("invalid");
    let correlation = frame["correlationId"].as_str().filter(|value| id(value)).unwrap_or(request);
    let fingerprint = store.map(|store| format!("{:x}", Sha256::digest(store.db_path.to_string_lossy().as_bytes())));
    let result_count = ["records","matches","drafts","chunks","pages"].iter().find_map(|key| frame["result"][key].as_array().map(Vec::len));
    let record = json!({"event":"MCP_LOCAL","requestId":request,"correlationId":correlation,
        "stage":stage,"elapsedMs":started.elapsed().as_millis() as u64,"code":code,
        "resultCount":result_count,
        "dbFingerprint":fingerprint,"at":chrono::Utc::now().timestamp_millis()});
    // No query, payload, tenant/student identifier, raw path, or exception text.
    eprintln!("{record}");
    if let Ok(mut by_tenant) = DIAGNOSTICS.lock() {
        DIAGNOSTIC_TENANT.with(|scope| {
            let items = by_tenant.entry(scope.borrow().clone()).or_default();
            if items.len() >= 96 { items.remove(0); }
            items.push(record);
        });
    }
}

pub(super) fn error(frame: &Value, code: &str) -> Value {
    json!({"type":"local_read_result","requestId":frame["requestId"],
        "correlationId":frame["correlationId"].as_str().filter(|value| id(value)).or(frame["requestId"].as_str()),
        "status":"error","errorCode":code})
}

pub(super) fn wire_response(frame: &Value, mut response: Value) -> Value {
    if let Some(correlation) = frame["correlationId"].as_str().filter(|value| id(value)) {
        response["correlationId"] = json!(correlation);
    }
    // Envelope metadata is part of the wire limit. Oversized reads fail alone,
    // before send() could close the socket and fail unrelated pending requests.
    match serde_json::to_vec(&response) {
        Ok(bytes) if bytes.len() <= outgoing_frame_limit(&response) => response,
        Ok(_) => error(frame, "LOCAL_READ_RESULT_TOO_LARGE"),
        Err(_) => error(frame, "LOCAL_RESPONSE_SERIALIZE_FAILED"),
    }
}

pub(super) fn probe(store: &SqliteStore, tenant: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_millis(250);
    let _access = crate::restore_journal::access_pair_until(&store.shared_data_dir, &store.data_dir, deadline)?;
    let reader = read_store(store, deadline, Arc::new(AtomicBool::new(false)))?;
    reader.restore_ready(tenant).map_err(|_| "local_store_unavailable".into())
}

fn sql_error(error: rusqlite::Error) -> String {
    match error.sqlite_error_code() {
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => "LOCAL_DB_LOCKED",
        Some(rusqlite::ErrorCode::OperationInterrupted) => "LOCAL_DB_QUERY_TIMEOUT",
        _ => "LOCAL_DB_QUERY_FAILED",
    }.into()
}

fn read_store(store: &SqliteStore, deadline: Instant, cancelled: Arc<AtomicBool>) -> Result<SqliteStore, String> {
    if store.db_path.as_os_str().is_empty() || !store.db_path.is_file() { return Err("LOCAL_DB_NOT_SELECTED".into()); }
    let conn = Connection::open_with_flags(&store.db_path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(sql_error)?;
    let path = conn.path().ok_or("LOCAL_DB_NOT_SELECTED")?;
    if std::fs::canonicalize(Path::new(path)).ok() != std::fs::canonicalize(&store.db_path).ok() {
        return Err("LOCAL_DB_NOT_SELECTED".into());
    }
    conn.busy_timeout(deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(250))).map_err(sql_error)?;
    conn.progress_handler(1_000, Some(move || cancelled.load(Ordering::SeqCst) || Instant::now() >= deadline));
    conn.execute_batch("PRAGMA query_only=ON; PRAGMA foreign_keys=ON;").map_err(sql_error)?;
    conn.query_row("SELECT count(*) FROM sqlite_schema WHERE name='lesson_observations'", [], |row| row.get::<_, i64>(0))
        .map_err(sql_error).and_then(|count| if count == 1 { Ok(()) } else { Err("LOCAL_DB_QUERY_FAILED".into()) })?;
    Ok(store.with_connection(conn))
}

struct Job { frame: Value, deadline: Instant, started: Instant }
struct Pending { frame: Value, deadline: Instant, started: Instant }

pub(super) struct Executor {
    tx: mpsc::SyncSender<Job>,
    rx: mpsc::Receiver<(String, Value)>,
    pending: HashMap<String, Pending>,
    cancelled: Arc<AtomicBool>,
    completed: VecDeque<String>,
}

impl Executor {
    pub(super) fn new<F>(store: Arc<SqliteStore>, authority: WorkerAuthority, validate: F) -> Self
    where F: Fn() -> Result<(), String> + Send + 'static {
        let (tx, jobs) = mpsc::sync_channel::<Job>(MAX_PENDING);
        let (results, rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&cancelled);
        thread::spawn(move || {
            diagnostic_tenant(&authority.tenant_id);
            while let Ok(job) = jobs.recv() {
                if stopping.load(Ordering::SeqCst) { break; }
                let request_id = job.frame["requestId"].as_str().unwrap_or_default().to_string();
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if Instant::now() >= job.deadline { return error(&job.frame, "LOCAL_DB_QUERY_TIMEOUT"); }
                    let result = (|| {
                        validate().map_err(|_| "MCP_RELAY_OFFLINE".to_string())?;
                        // Personal source documents retain their account-owned common DB.
                        let selected_store = if job.frame["workspace"] == "teaching_sources" { store.shared_store()? } else { Arc::clone(&store) };
                        trace(&job.frame, "db_lock_wait", job.started, "START", Some(&selected_store));
                        let _access = crate::restore_journal::access_pair_until(&selected_store.shared_data_dir, &selected_store.data_dir,
                            job.deadline.min(Instant::now() + Duration::from_millis(250)))?;
                        let reader = read_store(&selected_store, job.deadline, Arc::clone(&stopping))?;
                        reader.restore_ready(&authority.tenant_id).map_err(|_| "local_store_unavailable".to_string())?;
                        trace(&job.frame, "db_ready", job.started, "OK", Some(&reader));
                        trace(&job.frame, "query_start", job.started, "START", None);
                        let response = read_frame_for_owner(&reader, &authority.tenant_id, &authority.actor_id, &job.frame,
                            chrono::Utc::now().timestamp_millis()).ok_or("INVALID_LOCAL_READ_REQUEST")?;
                        let mut diagnostic = response.clone();
                        diagnostic["correlationId"] = job.frame["correlationId"].clone();
                        trace(&diagnostic, "query_complete", job.started, response["errorCode"].as_str().unwrap_or("OK"), None);
                        validate().map_err(|_| "MCP_RELAY_OFFLINE".to_string())?;
                        if Instant::now() >= job.deadline { return Err("LOCAL_DB_QUERY_TIMEOUT".to_string()); }
                        Ok(response)
                    })();
                    result.unwrap_or_else(|code| error(&job.frame, safe_error(&code)))
                })).unwrap_or_else(|_| error(&job.frame, "LOCAL_DB_QUERY_FAILED"));
                let outcome = wire_response(&job.frame, outcome);
                trace(&outcome, "serialized", job.started, outcome["errorCode"].as_str().unwrap_or("OK"), None);
                if stopping.load(Ordering::SeqCst) || results.send((request_id, outcome)).is_err() { break; }
            }
        });
        Self { tx, rx, pending: HashMap::new(), cancelled, completed: VecDeque::new() }
    }

    pub(super) fn submit(&mut self, mut frame: Value) -> Option<Value> {
        let request_id = frame["requestId"].as_str().filter(|value| id(value))?.to_string();
        // An identical socket delivery is still the same pending operation.
        if self.pending.contains_key(&request_id) || self.completed.contains(&request_id) { return None; }
        let started = Instant::now();
        trace(&frame, "app_received", started, "START", None);
        let now = chrono::Utc::now().timestamp_millis();
        let budget = if frame["workspace"] == "teaching_sources" && frame["operation"] != "chunks" { 30_000 } else { 12_000 };
        let remaining = if frame["type"] != "local_read_request" { Err("INVALID_LOCAL_READ_REQUEST") }
            else { frame["deadlineAt"].as_i64().ok_or("INVALID_LOCAL_READ_REQUEST")
                .and_then(|deadline| bounded_read_remaining(deadline, now, budget)) };
        let remaining = match remaining {
            Ok(remaining) if self.pending.len() < MAX_PENDING => remaining,
            Ok(_) => {
                trace(&frame, "request_rejected", started, "MCP_RELAY_BUSY", None);
                self.remember(request_id);
                return Some(error(&frame, "MCP_RELAY_BUSY"));
            }
            Err(code) => {
                trace(&frame, "request_rejected", started, code, None);
                self.remember(request_id);
                return Some(error(&frame, code));
            }
        };
        // Downstream PDF checks use local UTC; normalize only the private native
        // frame while Instant still bounds SQLite, queued work and returned results.
        frame["deadlineAt"] = json!(now + remaining);
        let deadline = started + Duration::from_millis((remaining - RESPONSE_MARGIN_MS) as u64);
        if self.tx.try_send(Job { frame: frame.clone(), deadline, started }).is_err() {
            trace(&frame, "request_rejected", started, "MCP_RELAY_BUSY", None);
            self.remember(request_id);
            return Some(error(&frame, "MCP_RELAY_BUSY"));
        }
        self.pending.insert(request_id, Pending { frame, deadline, started });
        None
    }

    pub(super) fn drain(&mut self) -> Vec<Value> {
        let mut ready = Vec::new();
        while let Ok((id, response)) = self.rx.try_recv() {
            if let Some(pending) = self.pending.remove(&id) {
                let response = if Instant::now() >= pending.deadline { error(&pending.frame, "LOCAL_DB_QUERY_TIMEOUT") } else { response };
                self.finish(id, &pending, &response);
                ready.push(response);
            }
        }
        let expired: Vec<_> = self.pending.iter().filter(|(_, value)| Instant::now() >= value.deadline).map(|(id, _)| id.clone()).collect();
        for id in expired {
            if let Some(pending) = self.pending.remove(&id) {
                let response = error(&pending.frame, "LOCAL_DB_QUERY_TIMEOUT");
                self.finish(id, &pending, &response);
                ready.push(response);
            }
        }
        ready
    }

    fn finish(&mut self, id: String, pending: &Pending, response: &Value) {
        trace(&pending.frame, "response_ready", pending.started, response["errorCode"].as_str().unwrap_or("OK"), None);
        self.remember(id);
    }

    fn remember(&mut self, id: String) {
        self.completed.push_back(id);
        if self.completed.len() > 256 { self.completed.pop_front(); }
    }
}

impl Drop for Executor {
    fn drop(&mut self) { self.cancelled.store(true, Ordering::SeqCst); }
}

pub(super) fn safe_error(code: &str) -> &str {
    match code {
        "LOCAL_DB_NOT_SELECTED" | "LOCAL_DB_LOCKED" | "LOCAL_DB_QUERY_TIMEOUT" | "LOCAL_DB_QUERY_FAILED"
        | "LOCAL_HANDLER_NOT_FOUND" | "LOCAL_RESPONSE_SERIALIZE_FAILED" | "MCP_RELAY_OFFLINE"
        | "INVALID_LOCAL_READ_REQUEST" | "local_store_unavailable" => code,
        _ if code.contains("database is locked") || code.contains("database table is locked") => "LOCAL_DB_LOCKED",
        _ if code.contains("interrupted") => "LOCAL_DB_QUERY_TIMEOUT",
        _ => "LOCAL_DB_QUERY_FAILED",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn server_clock_skew_does_not_extend_native_operation_budgets() {
        let now = 1_000_000;
        for budget in [12_000, 30_000] {
            // SONG's local clock was behind by more than the old one-second allowance.
            assert_eq!(bounded_read_remaining(now + budget + 2_000, now, budget), Ok(budget));
            assert_eq!(bounded_read_remaining(now + budget + READ_CLOCK_SKEW_TOLERANCE_MS, now, budget), Ok(budget));
            assert_eq!(bounded_read_remaining(now + budget + READ_CLOCK_SKEW_TOLERANCE_MS + 1, now, budget),
                Err("INVALID_LOCAL_READ_REQUEST"));
            assert_eq!(bounded_read_remaining(now + 3_000, now, budget), Ok(3_000));
            assert_eq!(bounded_read_remaining(now + RESPONSE_MARGIN_MS, now, budget), Err("LOCAL_DB_QUERY_TIMEOUT"));
            assert_eq!(bounded_read_remaining(now - 1, now, budget), Err("LOCAL_DB_QUERY_TIMEOUT"));
            assert_eq!(bounded_read_remaining(i64::MIN, now, budget), Err("INVALID_LOCAL_READ_REQUEST"));
            assert_eq!(bounded_read_remaining(i64::MAX, now, budget), Err("INVALID_LOCAL_READ_REQUEST"));
        }
    }

    #[test]
    fn skewed_material_and_teaching_source_frames_reach_actual_sqlite_reads() {
        let (directory, store) = super::super::tests::test_store();
        let store = Arc::new(store);
        let owner = WorkerAuthority { tenant_id: "tenant-a".into(), actor_id: "owner-a".into(),
            device_id: "device-a".into(), credential: Zeroizing::new("d".repeat(43)), origin: "http://localhost".into() };
        let mut executor = Executor::new(Arc::clone(&store), owner, || Ok(()));
        for (workspace, operation, input, budget) in [
            ("work_materials", "search", json!({"query":"고려","limit":10}), 12_000),
            ("teaching_sources", "matches", json!({"lessons":[{"schoolYear":2026,"semester":2,
                "curriculumSourceKind":"class","curriculumSourceScopeId":"scope-a","curriculumSourceRevision":1,
                "curriculumItemId":"lesson-a","curriculumStatus":"qualified"}],"limit":10}), 30_000),
        ] {
            let request_id = format!("clock-skew-{workspace}");
            let now = chrono::Utc::now().timestamp_millis();
            let frame = json!({"type":"local_read_request","requestId":request_id,"workspace":workspace,
                "operation":operation,"input":input,"deadlineAt":now+budget+2_000});
            assert!(executor.submit(frame).is_none());
            let pending = executor.pending.get(&request_id).unwrap();
            assert_eq!(pending.deadline.duration_since(pending.started), Duration::from_millis((budget-RESPONSE_MARGIN_MS) as u64));
            let normalized = pending.frame["deadlineAt"].as_i64().unwrap();
            assert!((now+budget..=chrono::Utc::now().timestamp_millis()+budget).contains(&normalized));
            let until = Instant::now()+Duration::from_secs(3);
            loop {
                let ready = executor.drain();
                if let Some(response) = ready.first() { assert_eq!(response["status"], "ok", "{response}"); break; }
                assert!(Instant::now()<until, "skewed read did not reach its SQLite handler");
                thread::sleep(Duration::from_millis(5));
            }
        }
        drop(executor);
        for _ in 0..100 { if Arc::strong_count(&store)==1 { break; } thread::sleep(Duration::from_millis(10)); }
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn invalid_or_expired_frames_report_safe_rejection_diagnostics() {
        let (directory, store) = super::super::tests::test_store();
        let store = Arc::new(store);
        let owner = WorkerAuthority { tenant_id: "tenant-a".into(), actor_id: "owner-a".into(),
            device_id: "device-a".into(), credential: Zeroizing::new("d".repeat(43)), origin: "http://localhost".into() };
        let mut executor = Executor::new(Arc::clone(&store), owner, || Ok(()));
        diagnostic_tenant("clock-skew-rejections");
        let now = chrono::Utc::now().timestamp_millis();
        for (request_id, deadline, expected) in [
            ("clock-skew-rejected", json!(now+12_000+READ_CLOCK_SKEW_TOLERANCE_MS+1_000), "INVALID_LOCAL_READ_REQUEST"),
            ("clock-skew-expired", json!(0), "LOCAL_DB_QUERY_TIMEOUT"),
            ("clock-skew-malformed", json!("not-a-timestamp"), "INVALID_LOCAL_READ_REQUEST"),
        ] {
            let frame = json!({"type":"local_read_request","requestId":request_id,"workspace":"work_materials",
                "operation":"search","input":{"query":"private-query"},"deadlineAt":deadline});
            assert_eq!(executor.submit(frame).unwrap()["errorCode"], expected);
            assert!(diagnostics_for_tenant("clock-skew-rejections").iter()
                .any(|entry| entry["requestId"]==request_id && entry["stage"]=="request_rejected" && entry["code"]==expected));
        }
        assert!(!serde_json::to_string(&diagnostics_for_tenant("clock-skew-rejections")).unwrap().contains("private-query"));
        drop(executor);
        for _ in 0..100 { if Arc::strong_count(&store)==1 { break; } thread::sleep(Duration::from_millis(10)); }
        drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn private_read_connection_interrupts_sql_and_rejects_writes() {
        let (directory, store) = super::super::tests::test_store();
        let reader = read_store(&store, Instant::now()+Duration::from_secs(1), Arc::new(AtomicBool::new(false))).unwrap();
        let conn = reader.conn.lock().unwrap();
        assert!(conn.execute("DELETE FROM lesson_observations", []).is_err());
        let error = conn.query_row("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000000) SELECT sum(x) FROM n", [], |row| row.get::<_, i64>(0)).unwrap_err();
        assert_eq!(sql_error(error), "LOCAL_DB_QUERY_TIMEOUT");
        drop(conn); drop(reader); drop(store);
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn missing_selected_database_never_creates_an_empty_file() {
        let (directory, mut store) = super::super::tests::test_store();
        store.db_path = directory.join("missing.sqlite");
        assert!(matches!(read_store(&store, Instant::now()+Duration::from_secs(1), Arc::new(AtomicBool::new(false))),
            Err(code) if code == "LOCAL_DB_NOT_SELECTED"));
        assert!(!store.db_path.exists());
        drop(store); std::fs::remove_dir_all(directory).unwrap();
    }
}
