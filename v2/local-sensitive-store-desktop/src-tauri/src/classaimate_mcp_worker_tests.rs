use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

fn authority(origin: String) -> WorkerAuthority {
    WorkerAuthority {
        tenant_id: "tenant-a".into(),
        actor_id: "owner-a".into(),
        device_id: "device-a".into(),
        credential: Zeroizing::new("d".repeat(43)),
        origin,
    }
}

#[test]
fn device_identity_changes_invalidate_worker_and_errors_are_safe() {
    let owner = authority("https://t.classaimate.com".into());
    for field in ["tenant", "actor", "device", "credential", "origin"] {
        let mut changed = owner.clone();
        match field {
            "tenant" => changed.tenant_id.push('b'),
            "actor" => changed.actor_id.push('b'),
            "device" => changed.device_id.push('b'),
            "credential" => changed.credential.push('b'),
            _ => changed.origin.push('b'),
        }
        assert!(!owner.same(&changed));
    }
    assert_eq!(
        failure_code("SQLite password=user-note/private-file"),
        "MCP_LOCAL_APPLY_UNKNOWN"
    );
    assert_eq!(
        failure_code("observation_revision_conflict"),
        "OBSERVATION_REVISION_CONFLICT"
    );
    assert_eq!(
        failure_code("MATERIAL_REVISION_CONFLICT"),
        "MCP_MATERIAL_REVISION_CONFLICT"
    );
    // File and readback failures may happen after commit; a raw code cannot prove rollback.
    assert_eq!(
        failure_code("IMAGE_LOCAL_FILE_FAILED"),
        "MCP_LOCAL_APPLY_UNKNOWN"
    );
    assert_eq!(
        failure_code("IMAGE_INTEGRITY_FAILED"),
        "MCP_LOCAL_APPLY_UNKNOWN"
    );
    assert!(read_bounded(&[0u8; 20][..], 10).is_err());
    assert_eq!(ready()["protocolVersion"], 1);
    assert!(ready()["capabilities"]
        .as_array()
        .unwrap()
        .contains(&json!("lesson_observations_mcp_v1")));
    assert!(!status().to_string().contains(owner.credential.as_str()));
}

#[test]
fn readback_failures_never_report_rollback_when_commit_exists_or_cannot_be_checked() {
    for error in [
        "IMAGE_INTEGRITY_FAILED",
        "MATERIAL_REVISION_CONFLICT",
        "IMAGE_LOCAL_FILE_FAILED",
    ] {
        assert_eq!(
            local_failure_code(error, Ok(true)),
            "MCP_LOCAL_APPLY_UNKNOWN"
        );
        assert_eq!(
            local_failure_code(error, Err("db unavailable".into())),
            "MCP_LOCAL_APPLY_UNKNOWN"
        );
    }
    assert_eq!(
        local_failure_code("IMAGE_INTEGRITY_FAILED", Ok(false)),
        "MCP_WORKER_ASSET_DIGEST_MISMATCH"
    );
    assert_eq!(
        local_failure_code("MATERIAL_REVISION_CONFLICT", Ok(false)),
        "MCP_MATERIAL_REVISION_CONFLICT"
    );
    assert_eq!(
        local_failure_code("IMAGE_LOCAL_FILE_FAILED", Ok(false)),
        "MCP_LOCAL_APPLY_UNKNOWN"
    );
}

fn test_store() -> (std::path::PathBuf, SqliteStore) {
    let directory = std::env::temp_dir().join(format!(
        "classaimate-native-worker-{}",
        crate::random_url_token()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let store = SqliteStore::open(directory.join("store.sqlite")).unwrap();
    (directory, store)
}

fn seed_observations(store: &SqliteStore) {
    let items: Vec<Value> = (1..=22).map(|index| json!({"action":"create","studentCode":format!("STU{index:02}"),
        "docId":format!("obs{index:02}"),"baselineRecords":[],"record":{"note":format!("합성 관찰 {index}")}})).collect();
    classaimate_mcp_write_jobs::apply(store, &json!({"tenantId":"tenant-a","receiptId":"receipt-22",
        "operation":"lesson_observations_manage","requestSha256":"a".repeat(64),
        "data":{"scope":{"date":"2026-09-08","period":1,"subject":"수학"},"mutationId":"mutation-22","items":items}})).unwrap();
}

fn read_request() -> Value {
    json!({"type":"local_read_request","requestId":"trace-22","workspace":"lesson_observations",
        "operation":"observations_list","input":{"date":"2026-09-08","period":1,"subject":"수학","limit":200},
        "deadlineAt":chrono::Utc::now().timestamp_millis()+12_000})
}

#[test]
fn native_read_uses_same_canonical_22_records_and_refuses_cross_tenant_input() {
    let (directory, store) = test_store();
    seed_observations(&store);
    let request = read_request();
    let response = read_frame(
        &store,
        "tenant-a",
        &request,
        chrono::Utc::now().timestamp_millis(),
    )
    .unwrap();
    assert_eq!(response["result"]["records"].as_array().unwrap().len(), 22);
    assert_eq!(response["result"]["complete"], true);
    assert_eq!(response["result"].as_object().unwrap().len(), 3);
    let other = read_frame(
        &store,
        "tenant-b",
        &request,
        chrono::Utc::now().timestamp_millis(),
    )
    .unwrap();
    assert_eq!(other["result"]["records"], json!([]));
    let mut injected = request.clone();
    injected["input"]["tenantId"] = json!("tenant-a");
    assert_eq!(
        read_frame(
            &store,
            "tenant-b",
            &injected,
            chrono::Utc::now().timestamp_millis()
        )
        .unwrap()["status"],
        "error"
    );
    injected = request.clone();
    injected["deadlineAt"] = json!(1);
    assert!(read_frame(&store, "tenant-a", &injected, 2).is_none());
    injected = request;
    injected["operation"] = json!("execute_sql");
    assert_eq!(
        read_frame(
            &store,
            "tenant-a",
            &injected,
            chrono::Utc::now().timestamp_millis()
        )
        .unwrap()["status"],
        "error"
    );
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn credential_https_ticket_and_websocket_read_work_without_browser() {
    let (directory, store) = test_store();
    seed_observations(&store);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let owner = authority(format!("http://{}", listener.local_addr().unwrap()));
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(&mut stream);
        let mut headers = String::new();
        let mut size = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                size = value.trim().parse().unwrap();
            }
            headers.push_str(&line);
        }
        let mut body = vec![0; size];
        reader.read_exact(&mut body).unwrap();
        drop(reader);
        assert!(headers.starts_with(&format!("POST {API_PATH}/relay-ticket ")));
        assert!(headers
            .to_ascii_lowercase()
            .contains("x-local-store-device-id: device-a"));
        assert!(headers.contains(&format!("Bearer {}", "d".repeat(43))));
        assert!(!headers.to_ascii_lowercase().contains("cookie:"));
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["protocolVersion"],
            1
        );
        let body = json!({"ok":true,"data":{"socketPath":format!("{API_PATH}/relay"),"ticket":"test-ticket"}}).to_string();
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        drop(stream);
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut socket = tungstenite::accept_hdr(
            stream,
            |request: &tungstenite::handshake::server::Request,
             mut response: tungstenite::handshake::server::Response| {
                assert_eq!(request.uri().path(), format!("{API_PATH}/relay"));
                assert_eq!(request.headers()["x-local-store-device-id"], "device-a");
                response.headers_mut().insert(
                    "sec-websocket-protocol",
                    "classaimate-mcp-relay".parse().unwrap(),
                );
                Ok(response)
            },
        )
        .unwrap();
        let ready: Value = serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(ready["type"], "ready");
        socket
            .send(Message::Text(read_request().to_string().into()))
            .unwrap();
        let result: Value =
            serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(result["result"]["records"].as_array().unwrap().len(), 22);
        assert_eq!(result["requestId"], "trace-22");
    });
    let mut socket = open_socket(&owner).unwrap();
    let request: Value = serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
    send(
        &mut socket,
        read_frame(
            &store,
            &owner.tenant_id,
            &request,
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap(),
    )
    .unwrap();
    server.join().unwrap();
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn committed_local_job_recovers_lost_ack_without_duplicate_write() {
    let (directory, store) = test_store();
    let data = json!({"scope":{"date":"2026-09-08","period":1,"subject":"수학"},"mutationId":"ack-recovery",
        "items":[{"action":"create","studentCode":"STU01","docId":"recover-obs","baselineRecords":[],"record":{"note":"복구 검증"}}]});
    let job = json!({"receiptId":"recover-receipt","target":{"deviceId":"device-a"},
        "operation":"lesson_observations_manage","requestSha256":"b".repeat(64),"expectedResultSha256":digest(&data).unwrap(),"data":data});
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let owner = authority(format!("http://{}", server.server_addr()));
    let server = thread::spawn(move || {
        for (suffix, status, payload) in [
            (
                "claim",
                200,
                json!({"job":job,"receipt":{"claimRevision":2}}),
            ),
            ("renew", 200, json!({})),
            ("complete", 503, json!({})),
            ("fail", 200, json!({})),
            (
                "claim",
                200,
                json!({"job":job,"receipt":{"claimRevision":3}}),
            ),
            ("renew", 200, json!({})),
            ("complete", 200, json!({"receipt":{"status":"saved"}})),
        ] {
            let mut request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            assert_eq!(
                request.url(),
                format!("{API_PATH}/write-jobs/recover-receipt/{suffix}")
            );
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            let body: Value = serde_json::from_str(&body).unwrap();
            if suffix == "complete" {
                assert_eq!(body["resultSha256"], job["expectedResultSha256"]);
            }
            if suffix == "fail" {
                assert_eq!(body["errorCode"], "MCP_WORKER_NETWORK_UNAVAILABLE");
                assert_eq!(body["claimRevision"], 2);
            }
            let response =
                tiny_http::Response::from_string(json!({"ok":true,"data":payload}).to_string())
                    .with_status_code(status);
            request.respond(response).unwrap();
        }
    });
    let cancelled = AtomicBool::new(false);
    assert_eq!(
        apply_job(&store, &owner, "recover-receipt", &cancelled, || Ok(())).unwrap_err(),
        "MCP_WORKER_NETWORK_UNAVAILABLE"
    );
    apply_job(&store, &owner, "recover-receipt", &cancelled, || Ok(())).unwrap();
    server.join().unwrap();
    let result = read_frame(
        &store,
        "tenant-a",
        &read_request(),
        chrono::Utc::now().timestamp_millis(),
    )
    .unwrap();
    assert_eq!(result["result"]["records"].as_array().unwrap().len(), 1);
    assert_eq!(result["result"]["records"][0]["note"], "복구 검증");
    let receipts: i64 = store
        .conn
        .lock()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM classaimate_mcp_local_write_receipts",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(receipts, 1);
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn changed_canonical_data_after_commit_stays_unknown_and_is_never_reapplied() {
    let (directory, store) = test_store();
    let data = json!({"scope":{"date":"2026-09-08","period":1,"subject":"수학"},"mutationId":"changed-after-commit",
        "items":[{"action":"create","studentCode":"STU01","docId":"changed-obs","baselineRecords":[],"record":{"note":"초기 합성 기록"}}]});
    let input = json!({"tenantId":"tenant-a","receiptId":"changed-receipt","operation":"lesson_observations_manage",
        "requestSha256":"e".repeat(64),"data":data});
    classaimate_mcp_write_jobs::apply(&store, &input).unwrap();
    store.conn.lock().unwrap().execute("UPDATE lesson_observations SET payload_json=json_set(payload_json,'$.note','교사가 고친 합성 기록') WHERE tenant_id='tenant-a' AND doc_id='changed-obs'", []).unwrap();
    let job = json!({"receiptId":"changed-receipt","target":{"deviceId":"device-a"},
        "operation":input["operation"],"requestSha256":input["requestSha256"],"expectedResultSha256":digest(&data).unwrap(),"data":data});
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let owner = authority(format!("http://{}", server.server_addr()));
    let server = thread::spawn(move || {
        let request = server
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert!(request.url().ends_with("/claim"));
        request
            .respond(tiny_http::Response::from_string(
                json!({"ok":true,"data":{"job":job,"receipt":{"claimRevision":3}}}).to_string(),
            ))
            .unwrap();
        let mut request = server
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert!(request.url().ends_with("/fail"));
        let mut body = String::new();
        request.as_reader().read_to_string(&mut body).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["errorCode"],
            "MCP_LOCAL_APPLY_UNKNOWN"
        );
        request
            .respond(tiny_http::Response::from_string(
                json!({"ok":true,"data":{}}).to_string(),
            ))
            .unwrap();
    });
    assert_eq!(
        apply_job(
            &store,
            &owner,
            "changed-receipt",
            &AtomicBool::new(false),
            || Ok(())
        )
        .unwrap_err(),
        "MCP_LOCAL_APPLY_UNKNOWN"
    );
    server.join().unwrap();
    let response = read_frame(
        &store,
        "tenant-a",
        &read_request(),
        chrono::Utc::now().timestamp_millis(),
    )
    .unwrap();
    assert_eq!(response["result"]["records"].as_array().unwrap().len(), 1);
    assert_eq!(
        response["result"]["records"][0]["note"],
        "교사가 고친 합성 기록"
    );
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn assets_use_only_receipt_scoped_path_and_verify_exact_bytes() {
    for mismatch in [false, true] {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let owner = authority(format!("http://{}", server.server_addr()));
        let bytes = b"image-fixture";
        let expected = if mismatch {
            "0".repeat(64)
        } else {
            format!("{:x}", Sha256::digest(bytes))
        };
        let job = json!({"requestSha256":"a".repeat(64),"data":{"attachments":[{"assetId":"asset-a",
            "objectKey":"https://untrusted.example/do-not-fetch", "size":bytes.len(),"sha256":expected}]}});
        let server = thread::spawn(move || {
            let request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            assert_eq!(
                request.url(),
                format!("{API_PATH}/write-jobs/receipt-a/renew")
            );
            request
                .respond(tiny_http::Response::from_string(
                    json!({"ok":true,"data":{}}).to_string(),
                ))
                .unwrap();
            let request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            assert_eq!(
                request.url(),
                format!("{API_PATH}/write-jobs/receipt-a/assets/asset-a")
            );
            request
                .respond(tiny_http::Response::from_data(bytes.to_vec()))
                .unwrap();
        });
        let result = download_assets(
            &owner,
            "receipt-a",
            &job,
            &json!(2),
            &AtomicBool::new(false),
        );
        if mismatch {
            assert_eq!(result.unwrap_err(), "MCP_WORKER_ASSET_DIGEST_MISMATCH");
        } else {
            assert_eq!(result.unwrap()["asset-a"], bytes);
        }
        server.join().unwrap();
    }
}

#[test]
fn job_catch_up_continues_empty_filtered_pages_and_bounds_bad_cursors() {
    for repeated in [false, true] {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let owner = authority(format!("http://{}", server.server_addr()));
        let server = thread::spawn(move || {
            for page in 0..3 {
                let request = server
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap();
                assert_eq!(request.url(), format!("{API_PATH}/write-jobs"));
                let cursor = request
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("x-classaimate-mcp-jobs-after"));
                assert_eq!(
                    cursor.map(|header| header.value.as_str()),
                    match page {
                        0 => None,
                        1 => Some("100:receipt-a"),
                        _ => Some("101:receipt-b"),
                    }
                );
                let data = match page {
                    0 => json!({"jobs":[],"nextCursor":"100:receipt-a"}),
                    1 => {
                        json!({"jobs":[{"receiptId":"wanted-job"}],"nextCursor":if repeated {"100:receipt-a"} else {"101:receipt-b"}})
                    }
                    _ => {
                        json!({"jobs":[{"receiptId":"wanted-job"},{"receiptId":"last-job"}],"nextCursor":null})
                    }
                };
                request
                    .respond(tiny_http::Response::from_string(
                        json!({"ok":true,"data":data}).to_string(),
                    ))
                    .unwrap();
                if repeated && page == 1 {
                    break;
                }
            }
        });
        let result = poll_jobs(&owner, &AtomicBool::new(false));
        if repeated {
            assert!(matches!(result, Err(error) if error == "MCP_WORKER_RESPONSE_INVALID"));
        } else {
            let result = result.unwrap();
            assert_eq!(result.receipts, ["wanted-job", "last-job"]);
            assert!(!result.incomplete);
        }
        server.join().unwrap();
    }
}

#[test]
fn job_catch_up_marks_the_100_page_bound_incomplete_and_cancellation_stops_io() {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let owner = authority(format!("http://{}", server.server_addr()));
    let server = thread::spawn(move || {
        for page in 0..100 {
            let request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            request.respond(tiny_http::Response::from_string(json!({"ok":true,"data":{
                "jobs":[{"receiptId":format!("job-{page}")}],"nextCursor":format!("{page}:receipt-a"),
            }}).to_string())).unwrap();
        }
    });
    let batch = poll_jobs(&owner, &AtomicBool::new(false)).unwrap();
    assert_eq!(batch.receipts.len(), 100);
    assert!(batch.incomplete);
    server.join().unwrap();
    let cancelled = AtomicBool::new(true);
    assert!(
        matches!(poll_jobs(&owner, &cancelled), Err(error) if error == "MCP_WORKER_DISCONNECTED")
    );
    let job = json!({"data":{"attachments":[{"assetId":"asset-a"}]}});
    assert_eq!(
        download_assets(&owner, "receipt-a", &job, &json!(2), &cancelled).unwrap_err(),
        "MCP_WORKER_NETWORK_UNAVAILABLE"
    );
}

fn read_http(stream: &mut TcpStream) -> String {
    let mut reader = BufReader::new(stream);
    let mut headers = String::new();
    let mut size = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(!line.is_empty());
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            size = value.trim().parse().unwrap();
        }
        headers.push_str(&line);
    }
    let mut body = vec![0; size];
    reader.read_exact(&mut body).unwrap();
    headers
}

fn respond_http(stream: &mut TcpStream, data: Value) {
    let body = json!({"ok":true,"data":data}).to_string();
    write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
}

#[test]
fn blocked_http_catch_up_does_not_block_websocket_read_or_heartbeat() {
    let (directory, store) = test_store();
    seed_observations(&store);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let owner = authority(format!("http://{}", listener.local_addr().unwrap()));
    let server = thread::spawn(move || {
        let (mut ticket, _) = listener.accept().unwrap();
        ticket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        assert!(read_http(&mut ticket).starts_with(&format!("POST {API_PATH}/relay-ticket ")));
        respond_http(
            &mut ticket,
            json!({"socketPath":format!("{API_PATH}/relay"),"ticket":"test-ticket"}),
        );
        drop(ticket);
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut socket = tungstenite::accept_hdr(
            stream,
            |_: &tungstenite::handshake::server::Request,
             mut response: tungstenite::handshake::server::Response| {
                response.headers_mut().insert(
                    "sec-websocket-protocol",
                    "classaimate-mcp-relay".parse().unwrap(),
                );
                Ok(response)
            },
        )
        .unwrap();
        let ready: Value = serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(ready["type"], "ready");
        let (mut poll, _) = listener.accept().unwrap();
        poll.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        assert!(read_http(&mut poll).starts_with(&format!("GET {API_PATH}/write-jobs ")));
        // Keep the actual HTTP response blocked until a real WS read and ping arrive.
        let started = Instant::now();
        socket
            .send(Message::Text(read_request().to_string().into()))
            .unwrap();
        let mut received_read = false;
        let mut received_ping = false;
        while !received_read || !received_ping {
            let frame: Value =
                serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
            match frame["type"].as_str() {
                Some("ping") => {
                    received_ping = true;
                    socket
                        .send(Message::Text(json!({"type":"pong"}).to_string().into()))
                        .unwrap();
                }
                Some("local_read_result") => {
                    assert_eq!(frame["result"]["records"].as_array().unwrap().len(), 22);
                    received_read = true;
                }
                _ => panic!("unexpected worker frame"),
            }
        }
        assert!(started.elapsed() < Duration::from_secs(2));
        respond_http(&mut poll, json!({"jobs":[],"nextCursor":null}));
        socket.close(None).unwrap();
    });
    let store = Arc::new(store);
    let result = serve_with_validator(Arc::clone(&store), &owner, || Ok(()));
    assert!(result.is_err());
    server.join().unwrap();
    // The HTTP executor exits on the closed channel after serving the blocked response.
    for _ in 0..100 {
        if Arc::strong_count(&store) == 1 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(Arc::strong_count(&store), 1);
    drop(store);
    std::fs::remove_dir_all(directory).unwrap();
}
