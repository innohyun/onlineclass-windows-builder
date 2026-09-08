use crate::{
    classaimate_mcp_observations, classaimate_mcp_write_jobs, device_sync::DeviceSyncManager,
    local_workspaces, SqliteStore, SERVICE_VERSION,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{ErrorKind, Read};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};
use tungstenite::{client::IntoClientRequest, stream::MaybeTlsStream, Message, WebSocket};
use zeroize::Zeroizing;

const API_PATH: &str = "/api/v3/classaimate-mcp/device";
const MAX_FRAME: usize = 768 * 1024;
const MAX_ASSET: usize = 20 * 1024 * 1024;
const MAX_ASSETS: usize = 100 * 1024 * 1024;
const TICK: Duration = Duration::from_secs(15);
const CAPABILITIES: &[&str] = &[
    "classaimate_public_mcp_local_read_v1",
    "classaimate_public_mcp_write_jobs_v1",
    "classaimate_public_mcp_operations_v1",
    "lesson_observations_mcp_v1",
    "observation_evidence_v1",
    "classaimate_mcp_native_worker_v1",
    "classaimate_mcp_material_assets_v1",
];
static STARTED: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<(&str, &str, i64)> = Mutex::new(("waiting_connection", "", 0));

pub(crate) fn status() -> Value {
    let (state, error, updated_at) =
        STATE
            .lock()
            .map(|value| *value)
            .unwrap_or(("unavailable", "MCP_WORKER_UNAVAILABLE", 0));
    json!({"state":state,"errorCode":if error.is_empty(){Value::Null}else{json!(error)},
        "updatedAt":updated_at,"protocolVersion":1,"capabilities":CAPABILITIES})
}

fn set_state(state: &'static str, error: &'static str) {
    if let Ok(mut value) = STATE.lock() {
        *value = (state, error, chrono::Utc::now().timestamp_millis());
    }
}

// Device secrets stay in native memory and are never returned to the browser.
#[derive(Clone)]
pub(crate) struct WorkerAuthority {
    pub tenant_id: String,
    pub actor_id: String,
    pub device_id: String,
    pub credential: Zeroizing<String>,
    pub origin: String,
}

impl WorkerAuthority {
    fn same(&self, other: &Self) -> bool {
        self.tenant_id == other.tenant_id
            && self.actor_id == other.actor_id
            && self.device_id == other.device_id
            && self.credential.as_str() == other.credential.as_str()
            && self.origin == other.origin
    }
    fn request(&self, method: &str, path: &str) -> ureq::Request {
        ureq::AgentBuilder::new()
            .redirects(0)
            .timeout_connect(Duration::from_secs(5))
            .timeout(Duration::from_secs(12))
            .build()
            .request(method, &format!("{}{API_PATH}/{path}", self.origin))
            .set(
                "Authorization",
                &format!("Bearer {}", self.credential.as_str()),
            )
            .set("X-Local-Store-Device-Id", &self.device_id)
            .set("X-Local-Store-Snapshot-Max", "5")
    }
    fn json(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
        let request = self.request(method, path);
        let result = if let Some(body) = body {
            request.send_json(body)
        } else {
            request.call()
        };
        read_json_response(result)
    }
    fn jobs_page(&self, cursor: Option<&str>) -> Result<Value, String> {
        let mut request = self.request("GET", "write-jobs");
        if let Some(cursor) = cursor {
            request = request.set("X-ClassAimate-Mcp-Jobs-After", cursor);
        }
        read_json_response(request.call())
    }
}

fn read_json_response(result: Result<ureq::Response, ureq::Error>) -> Result<Value, String> {
    let response = result.map_err(http_error)?;
    let bytes = read_bounded(response.into_reader(), MAX_FRAME)?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "MCP_WORKER_RESPONSE_INVALID".to_string())?;
    if value["ok"] != true {
        return Err("MCP_WORKER_RESPONSE_INVALID".to_string());
    }
    value
        .get("data")
        .cloned()
        .ok_or_else(|| "MCP_WORKER_RESPONSE_INVALID".to_string())
}

fn http_error(error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(401 | 403, _) => "MCP_WORKER_AUTHORITY_REVOKED",
        ureq::Error::Status(409, _) => "MCP_WORKER_CONFLICT",
        ureq::Error::Status(404 | 410, _) => "MCP_WORKER_JOB_UNAVAILABLE",
        _ => "MCP_WORKER_NETWORK_UNAVAILABLE",
    }
    .to_string()
}

fn read_bounded(reader: impl Read, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "MCP_WORKER_NETWORK_UNAVAILABLE".to_string())?;
    if bytes.len() > limit {
        return Err("MCP_WORKER_RESULT_TOO_LARGE".to_string());
    }
    Ok(bytes)
}

fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || b"._:-".contains(&ch))
}

fn digest(value: &Value) -> Result<String, String> {
    // Use the canonical implementation shared with existing local write receipts.
    crate::sha256_json(value)
}

fn ready() -> Value {
    json!({"type":"ready","protocolVersion":1,"capabilities":CAPABILITIES,
        "appVersion":env!("CARGO_PKG_VERSION"),"serviceVersion":SERVICE_VERSION})
}

fn open_socket(
    authority: &WorkerAuthority,
) -> Result<WebSocket<MaybeTlsStream<TcpStream>>, String> {
    let ticket = authority.json("POST", "relay-ticket", Some(json!({
        "relayKeySha256": format!("{:x}", Sha256::digest(rand::random::<[u8;32]>())),
        "protocolVersion":1,"capabilities":CAPABILITIES,"appVersion":env!("CARGO_PKG_VERSION"),"serviceVersion":SERVICE_VERSION,
    })))?;
    // A server response cannot redirect the bearer credential to another origin.
    if ticket["socketPath"] != format!("{API_PATH}/relay") {
        return Err("MCP_WORKER_RESPONSE_INVALID".to_string());
    }
    let token = ticket["ticket"]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() < 4096)
        .ok_or_else(|| "MCP_WORKER_RESPONSE_INVALID".to_string())?;
    let origin = authority
        .origin
        .replacen("https:", "wss:", 1)
        .replacen("http:", "ws:", 1);
    let mut request = format!("{origin}{API_PATH}/relay")
        .into_client_request()
        .map_err(|_| "MCP_WORKER_CONNECT_FAILED".to_string())?;
    for (key, value) in [
        (
            "authorization",
            format!("Bearer {}", authority.credential.as_str()),
        ),
        ("x-local-store-device-id", authority.device_id.clone()),
        (
            "sec-websocket-protocol",
            format!("classaimate-mcp-relay, {token}"),
        ),
    ] {
        request.headers_mut().insert(
            tungstenite::http::header::HeaderName::from_bytes(key.as_bytes()).unwrap(),
            value
                .parse()
                .map_err(|_| "MCP_WORKER_CONNECT_FAILED".to_string())?,
        );
    }
    let config = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME));
    let host = request
        .uri()
        .host()
        .ok_or_else(|| "MCP_WORKER_CONNECT_FAILED".to_string())?;
    let port = request
        .uri()
        .port_u16()
        .unwrap_or(if authority.origin.starts_with("https:") {
            443
        } else {
            80
        });
    let stream = (host, port)
        .to_socket_addrs()
        .map_err(|_| "MCP_WORKER_CONNECT_FAILED".to_string())?
        .take(2)
        .find_map(|address| TcpStream::connect_timeout(&address, Duration::from_secs(5)).ok())
        .ok_or_else(|| "MCP_WORKER_CONNECT_FAILED".to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|_| "MCP_WORKER_CONNECT_FAILED".to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|_| "MCP_WORKER_CONNECT_FAILED".to_string())?;
    let (mut socket, _) = tungstenite::client_tls_with_config(request, stream, Some(config), None)
        .map_err(|_| "MCP_WORKER_CONNECT_FAILED".to_string())?;
    let stream = match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream,
        MaybeTlsStream::Rustls(stream) => &mut stream.sock,
        _ => return Err("MCP_WORKER_CONNECT_FAILED".to_string()),
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .map_err(|_| "MCP_WORKER_CONNECT_FAILED".to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|_| "MCP_WORKER_CONNECT_FAILED".to_string())?;
    send(&mut socket, ready())?;
    Ok(socket)
}

fn send(socket: &mut WebSocket<MaybeTlsStream<TcpStream>>, frame: Value) -> Result<(), String> {
    let value = serde_json::to_string(&frame).map_err(|_| "MCP_WORKER_JSON_INVALID".to_string())?;
    if value.len() > MAX_FRAME {
        return Err("MCP_WORKER_RESULT_TOO_LARGE".to_string());
    }
    socket
        .send(Message::Text(value.into()))
        .map_err(|_| "MCP_WORKER_DISCONNECTED".to_string())
}

fn read_local(store: &SqliteStore, tenant: &str, frame: &Value) -> Result<Value, String> {
    let input = frame["input"]
        .as_object()
        .ok_or_else(|| "INVALID_LOCAL_READ_REQUEST".to_string())?;
    let workspace = frame["workspace"].as_str().unwrap_or("");
    let operation = frame["operation"].as_str().unwrap_or("");
    let allowed: &[&str] = match (workspace, operation) {
        ("lesson_observations", "observations_list") => &[
            "date",
            "period",
            "subject",
            "studentCodes",
            "docIds",
            "cursor",
            "limit",
        ],
        ("work_materials" | "lesson_materials" | "student_learning_materials", "search") => {
            &["query", "limit"]
        }
        ("work_materials" | "lesson_materials" | "student_learning_materials", "get_page") => {
            &["pageRef"]
        }
        _ => return Err("INVALID_LOCAL_READ_REQUEST".to_string()),
    };
    if input.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("INVALID_LOCAL_READ_REQUEST".to_string());
    }
    let mut body = Value::Object(input.clone());
    body["tenantId"] = json!(tenant);
    if workspace == "lesson_observations" {
        let result = classaimate_mcp_observations::list(store, &body)?;
        return Ok(
            json!({"records":result["records"],"complete":result["complete"],"nextCursor":result["nextCursor"]}),
        );
    }
    body["workspace"] = json!(workspace);
    if operation == "search" {
        let payload = local_workspaces::mcp_search(store, &body)?;
        let matches: Vec<Value> = payload["pages"].as_array().into_iter().flatten().map(|page| json!({
            "pageRef":page["pageId"],"title":page["title"],"path":page["path"],"snippet":page["snippet"],
        })).collect();
        Ok(json!({"matches":matches}))
    } else {
        body["pageId"] = body["pageRef"].clone();
        let payload = local_workspaces::mcp_page(store, &body)?;
        let page = &payload["page"];
        Ok(
            json!({"page":{"pageRef":page["pageId"],"title":page["title"],"path":page["path"],
            "markdown":page["markdown"],"revision":page["revision"],"blocks":page["blocks"]}}),
        )
    }
}

fn read_frame(store: &SqliteStore, tenant: &str, frame: &Value, now: i64) -> Option<Value> {
    let request_id = frame["requestId"].as_str()?;
    let deadline = frame["deadlineAt"].as_i64()?;
    if !id(request_id)
        || deadline <= now
        || deadline > now + 13_000
        || frame["type"] != "local_read_request"
    {
        return None;
    }
    let response = match read_local(store, tenant, frame) {
        Ok(result) => {
            json!({"type":"local_read_result","requestId":request_id,"status":"ok","result":result})
        }
        Err(error) => {
            let not_found = error == "local_workspace_page_not_found";
            json!({"type":"local_read_result","requestId":request_id,"status":if not_found {"not_found"} else {"error"},
                "errorCode":if not_found {"local_workspace_page_not_found"} else {"LOCAL_READ_FAILED"}})
        }
    };
    if response.to_string().len() <= MAX_FRAME {
        Some(response)
    } else {
        Some(
            json!({"type":"local_read_result","requestId":request_id,"status":"error","errorCode":"LOCAL_READ_RESULT_TOO_LARGE"}),
        )
    }
}

fn renew(
    authority: &WorkerAuthority,
    receipt: &str,
    job: &Value,
    claim_revision: &Value,
) -> Result<(), String> {
    authority.json(
        "POST",
        &format!("write-jobs/{receipt}/renew"),
        Some(json!({
            "requestSha256":job["requestSha256"],"claimRevision":claim_revision,
        })),
    )?;
    Ok(())
}

fn download_assets(
    authority: &WorkerAuthority,
    receipt: &str,
    job: &Value,
    claim_revision: &Value,
    cancelled: &AtomicBool,
) -> Result<HashMap<String, Vec<u8>>, String> {
    let mut assets = HashMap::new();
    let mut total = 0usize;
    for asset in job
        .pointer("/data/attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if cancelled.load(Ordering::SeqCst) {
            return Err("MCP_WORKER_NETWORK_UNAVAILABLE".to_string());
        }
        renew(authority, receipt, job, claim_revision)?;
        let asset_id = asset["assetId"]
            .as_str()
            .filter(|value| id(value))
            .ok_or_else(|| "MCP_WORKER_ASSET_INVALID".to_string())?;
        let size = asset["size"]
            .as_u64()
            .filter(|value| *value > 0 && *value <= MAX_ASSET as u64)
            .ok_or_else(|| "MCP_WORKER_ASSET_INVALID".to_string())? as usize;
        total = total
            .checked_add(size)
            .ok_or_else(|| "MCP_WORKER_ASSET_INVALID".to_string())?;
        if total > MAX_ASSETS || assets.contains_key(asset_id) {
            return Err("MCP_WORKER_ASSET_INVALID".to_string());
        }
        let response = authority
            .request("GET", &format!("write-jobs/{receipt}/assets/{asset_id}"))
            .call()
            .map_err(http_error)?;
        let bytes = read_bounded(response.into_reader(), size)?;
        if bytes.len() != size
            || asset["sha256"].as_str() != Some(format!("{:x}", Sha256::digest(&bytes)).as_str())
        {
            return Err("MCP_WORKER_ASSET_DIGEST_MISMATCH".to_string());
        }
        assets.insert(asset_id.to_string(), bytes);
    }
    Ok(assets)
}

fn ensure_current(manager: &DeviceSyncManager, authority: &WorkerAuthority) -> Result<(), String> {
    if !authority.same(&manager.mcp_worker_authority()?) {
        return Err("MCP_WORKER_AUTHORITY_CHANGED".to_string());
    }
    Ok(())
}

fn failure_code(error: &str) -> &str {
    match error {
        "observation_revision_conflict" => "OBSERVATION_REVISION_CONFLICT",
        "local_workspace_page_revision_conflict"
        | "MCP_MATERIAL_DRAFT_CONFLICT"
        | "MATERIAL_REVISION_CONFLICT"
        | "REVISION_CONFLICT" => "MCP_MATERIAL_REVISION_CONFLICT",
        "MCP_WORKER_NETWORK_UNAVAILABLE"
        | "MCP_WORKER_ASSET_DIGEST_MISMATCH"
        | "MCP_WORKER_ASSET_INVALID"
        | "MCP_WRITE_DEVICE_MISMATCH"
        | "MCP_WRITE_RESULT_CONFLICT"
        | "MCP_WORKER_AUTHORITY_CHANGED" => error,
        _ => "MCP_LOCAL_APPLY_UNKNOWN",
    }
}

fn local_failure_code(error: &str, committed: Result<bool, String>) -> &str {
    // An on-disk receipt or an unreadable DB means a failed readback is not rollback.
    if !matches!(committed, Ok(false)) {
        return "MCP_LOCAL_APPLY_UNKNOWN";
    }
    if error == "IMAGE_INTEGRITY_FAILED" {
        "MCP_WORKER_ASSET_DIGEST_MISMATCH"
    } else {
        failure_code(error)
    }
}

struct JobBatch {
    receipts: Vec<String>,
    incomplete: bool,
}

fn poll_jobs(authority: &WorkerAuthority, cancelled: &AtomicBool) -> Result<JobBatch, String> {
    let mut receipts = Vec::new();
    let mut seen = HashSet::new();
    let mut cursors = HashSet::new();
    let mut cursor: Option<String> = None;
    for _ in 0..100 {
        if cancelled.load(Ordering::SeqCst) {
            return Err("MCP_WORKER_DISCONNECTED".to_string());
        }
        let page = authority.jobs_page(cursor.as_deref())?;
        if page
            .as_object()
            .map(|object| {
                object
                    .keys()
                    .any(|key| key != "jobs" && key != "nextCursor")
            })
            .unwrap_or(true)
        {
            return Err("MCP_WORKER_RESPONSE_INVALID".to_string());
        }
        let jobs = page["jobs"]
            .as_array()
            .filter(|rows| rows.len() <= 50)
            .ok_or_else(|| "MCP_WORKER_RESPONSE_INVALID".to_string())?;
        for job in jobs {
            let receipt = job["receiptId"]
                .as_str()
                .filter(|value| id(value))
                .ok_or_else(|| "MCP_WORKER_RESPONSE_INVALID".to_string())?;
            if seen.insert(receipt.to_string()) {
                receipts.push(receipt.to_string());
            }
        }
        match page.get("nextCursor") {
            None | Some(Value::Null) => {
                return Ok(JobBatch {
                    receipts,
                    incomplete: false,
                })
            }
            Some(Value::String(next)) => {
                let valid = next
                    .split_once(':')
                    .map(|(time, receipt)| {
                        !time.is_empty()
                            && time.bytes().all(|byte| byte.is_ascii_digit())
                            && time
                                .parse::<u64>()
                                .map(|value| value <= 9_007_199_254_740_991)
                                .unwrap_or(false)
                            && id(receipt)
                    })
                    .unwrap_or(false);
                if !valid || !cursors.insert(next.clone()) {
                    return Err("MCP_WORKER_RESPONSE_INVALID".to_string());
                }
                cursor = Some(next.clone());
            }
            _ => return Err("MCP_WORKER_RESPONSE_INVALID".to_string()),
        }
    }
    Ok(JobBatch {
        receipts,
        incomplete: true,
    })
}

enum WorkerEvent {
    Jobs(Result<JobBatch, String>),
    Applied(String, Result<(), String>),
}

fn apply_job(
    store: &SqliteStore,
    authority: &WorkerAuthority,
    receipt: &str,
    cancelled: &AtomicBool,
    validate_authority: impl Fn() -> Result<(), String>,
) -> Result<(), String> {
    if !id(receipt) {
        return Err("MCP_WRITE_JOB_INVALID".to_string());
    }
    let claimed = authority.json(
        "POST",
        &format!("write-jobs/{receipt}/claim"),
        Some(json!({})),
    )?;
    let job = &claimed["job"];
    let claim_revision = &claimed["receipt"]["claimRevision"];
    let result = (|| {
        if job["receiptId"] != receipt
            || job.pointer("/target/deviceId") != Some(&json!(authority.device_id))
        {
            return Err("MCP_WRITE_DEVICE_MISMATCH".to_string());
        }
        if !claim_revision.is_u64() {
            return Err("MCP_WORKER_RESPONSE_INVALID".to_string());
        }
        let input = json!({"tenantId":authority.tenant_id,"receiptId":receipt,"operation":job["operation"],
            "requestSha256":job["requestSha256"],"data":job["data"]});
        validate_authority()?;
        let classify = |error: String| {
            local_failure_code(
                &error,
                classaimate_mcp_write_jobs::committed_receipt_exists(store, &input),
            )
            .to_string()
        };
        let replay =
            classaimate_mcp_write_jobs::verified_replay(store, &input).map_err(classify)?;
        let assets = if replay.is_none() {
            download_assets(authority, receipt, job, claim_revision, cancelled)?
        } else {
            HashMap::new()
        };
        renew(authority, receipt, job, claim_revision)?;
        validate_authority()?;
        if cancelled.load(Ordering::SeqCst) {
            return Err("MCP_WORKER_NETWORK_UNAVAILABLE".to_string());
        }
        let saved = match replay {
            Some(saved) => saved,
            None => classaimate_mcp_write_jobs::apply_with_assets(store, &input, &assets)
                .map_err(classify)?,
        };
        let result_digest = digest(&saved["result"])?;
        if job["expectedResultSha256"] != result_digest {
            return Err("MCP_WRITE_RESULT_CONFLICT".to_string());
        }
        let completed = authority.json("POST", &format!("write-jobs/{receipt}/complete"), Some(json!({
            "requestSha256":job["requestSha256"],"resultSha256":result_digest,"localRef":saved["localRef"],"claimRevision":claim_revision,
        })))?;
        if completed["receipt"]["status"] != "saved" {
            return Err("MCP_WORKER_RESPONSE_INVALID".to_string());
        }
        Ok(())
    })();
    if let Err(error) = &result {
        if claim_revision.is_u64() {
            let _ = authority.json("POST", &format!("write-jobs/{receipt}/fail"), Some(json!({
                "requestSha256":job["requestSha256"],"claimRevision":claim_revision,"errorCode":failure_code(error),
            })));
        }
    }
    result
}

fn serve(
    store: Arc<SqliteStore>,
    manager: Arc<DeviceSyncManager>,
    authority: &WorkerAuthority,
) -> Result<(), String> {
    let checked_authority = authority.clone();
    serve_with_validator(store, authority, move || {
        ensure_current(&manager, &checked_authority)
    })
}

fn serve_with_validator<F>(
    store: Arc<SqliteStore>,
    authority: &WorkerAuthority,
    validate_authority: F,
) -> Result<(), String>
where
    F: Fn() -> Result<(), String> + Send + Clone + 'static,
{
    set_state("connecting", "");
    let mut socket = open_socket(authority)?;
    set_state("ready", "");
    let (job_tx, job_rx) = mpsc::sync_channel::<String>(1);
    let (done_tx, done_rx) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let worker_store = Arc::clone(&store);
    let worker_authority = authority.clone();
    let worker_validate = validate_authority.clone();
    thread::spawn(move || {
        let mut last_poll = Instant::now() - TICK;
        loop {
            if worker_cancelled.load(Ordering::SeqCst) {
                break;
            }
            // Catch-up and asset HTTP never block the WebSocket reader/heartbeat.
            if last_poll.elapsed() >= TICK {
                let result = poll_jobs(&worker_authority, &worker_cancelled);
                let failed = result.is_err();
                if done_tx.send(WorkerEvent::Jobs(result)).is_err() || failed {
                    break;
                }
                last_poll = Instant::now();
            }
            let receipt = match job_rx.recv_timeout(TICK.saturating_sub(last_poll.elapsed())) {
                Ok(receipt) => receipt,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if worker_cancelled.load(Ordering::SeqCst) {
                break;
            }
            let result = apply_job(
                &worker_store,
                &worker_authority,
                &receipt,
                &worker_cancelled,
                &worker_validate,
            );
            if done_tx.send(WorkerEvent::Applied(receipt, result)).is_err() {
                break;
            }
        }
    });
    let mut pending = VecDeque::new();
    let mut queued = HashSet::new();
    let mut reads = VecDeque::new();
    let mut last_tick = Instant::now() - TICK;
    let mut last_pong = Instant::now();
    let mut busy = false;
    let result = (|| loop {
        if last_pong.elapsed() > Duration::from_secs(45) {
            return Err("MCP_WORKER_DISCONNECTED".to_string());
        }
        if last_tick.elapsed() >= TICK {
            validate_authority()?;
            send(&mut socket, json!({"type":"ping"}))?;
            last_tick = Instant::now();
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                if let Ok(frame) = serde_json::from_str::<Value>(&text) {
                    if frame["type"] == "pong" {
                        last_pong = Instant::now();
                    } else if frame["type"] == "write_job_available" {
                        if let Some(receipt) = frame["receiptId"].as_str().filter(|value| id(value))
                        {
                            if queued.insert(receipt.to_string()) {
                                pending.push_back(receipt.to_string());
                            }
                        }
                    } else if let Some(request_id) = frame["requestId"]
                        .as_str()
                        .filter(|id| !reads.iter().any(|value| value == id))
                    {
                        let request_id = request_id.to_string();
                        validate_authority()?;
                        if let Some(response) = read_frame(
                            &store,
                            &authority.tenant_id,
                            &frame,
                            chrono::Utc::now().timestamp_millis(),
                        ) {
                            send(&mut socket, response)?;
                            reads.push_back(request_id);
                            if reads.len() > 256 {
                                reads.pop_front();
                            }
                        }
                    }
                }
            }
            Ok(Message::Close(_)) => return Err("MCP_WORKER_DISCONNECTED".to_string()),
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => return Err("MCP_WORKER_DISCONNECTED".to_string()),
        }
        while let Ok(event) = done_rx.try_recv() {
            match event {
                WorkerEvent::Jobs(result) => {
                    let batch = result?;
                    set_state(
                        "ready",
                        if batch.incomplete {
                            "MCP_WORKER_JOBS_INCOMPLETE"
                        } else {
                            ""
                        },
                    );
                    for receipt in batch.receipts {
                        if queued.insert(receipt.clone()) {
                            pending.push_back(receipt);
                        }
                    }
                }
                WorkerEvent::Applied(receipt, result) => {
                    busy = false;
                    queued.remove(&receipt);
                    if let Err(error) = result {
                        if [
                            "MCP_WORKER_AUTHORITY_REVOKED",
                            "MCP_WORKER_AUTHORITY_CHANGED",
                        ]
                        .contains(&error.as_str())
                        {
                            return Err(error);
                        }
                    }
                }
            }
        }
        if !busy {
            if let Some(receipt) = pending.pop_front() {
                job_tx
                    .send(receipt)
                    .map_err(|_| "MCP_WORKER_DISCONNECTED".to_string())?;
                busy = true;
            }
        }
    })();
    cancelled.store(true, Ordering::SeqCst);
    result
}

pub(crate) fn start(store: Arc<SqliteStore>, manager: Arc<DeviceSyncManager>) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    thread::spawn(move || {
        let mut failures = 0u32;
        loop {
            let started = Instant::now();
            if let Ok(authority) = manager.mcp_worker_authority() {
                let result = serve(Arc::clone(&store), Arc::clone(&manager), &authority);
                set_state(
                    "reconnecting",
                    match result.as_ref().err().map(String::as_str) {
                        Some("MCP_WORKER_AUTHORITY_REVOKED") => "MCP_WORKER_AUTHORITY_REVOKED",
                        Some("MCP_WORKER_AUTHORITY_CHANGED") => "MCP_WORKER_AUTHORITY_CHANGED",
                        Some("MCP_WORKER_RESPONSE_INVALID") => "MCP_WORKER_RESPONSE_INVALID",
                        _ => "MCP_WORKER_NETWORK_UNAVAILABLE",
                    },
                );
            } else {
                set_state("waiting_connection", "MCP_WORKER_CREDENTIAL_UNAVAILABLE");
            }
            failures = if started.elapsed() >= Duration::from_secs(60) {
                0
            } else {
                failures.saturating_add(1)
            };
            let delay = (1u64 << failures.min(5)).min(30);
            thread::sleep(Duration::from_secs(delay));
        }
    });
}

#[cfg(test)]
#[path = "classaimate_mcp_worker_tests.rs"]
mod tests;
