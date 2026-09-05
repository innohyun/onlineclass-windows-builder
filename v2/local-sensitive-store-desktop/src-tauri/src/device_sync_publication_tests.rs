use super::*;
use tiny_http::{Response, Server};

fn fixture() -> (PathBuf, Arc<SqliteStore>, DeviceSyncSession) {
    let root = env::temp_dir().join(format!(
        "classaimate-pending-publication-{}",
        crate::random_url_token()
    ));
    fs::create_dir_all(root.join("local")).unwrap();
    let store = Arc::new(SqliteStore::open(root.join("local/db.sqlite")).unwrap());
    backup::set_folder(
        &store,
        "qa-pending".into(),
        root.join("backups").to_string_lossy().into(),
    )
    .unwrap();
    store
        .upsert_observation(
            json!({"tenantId":"qa-pending","docId":"record","dateKey":"2026-09-05",
        "period":1,"studentCode":"1","observation":"fixture","updatedAtMs":1}),
        )
        .unwrap();
    let session = DeviceSyncSession {
        tenant_id: "qa-pending".into(),
        device_id: "qa-device".into(),
        ..Default::default()
    };
    (root, store, session)
}

fn manager(store: &Arc<SqliteStore>, endpoint: &str) -> DeviceSyncManager {
    let mut manager = DeviceSyncManager::new(store.data_dir.clone(), Arc::clone(store));
    manager.test_api_root = Some(endpoint.into());
    manager
}

#[test]
fn backup_pending_twenty_failures_and_restart_reuse_one_database_then_keep_new_edit_dirty() {
    let (root, store, session) = fixture();
    let server = Server::http("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", server.server_addr());
    let requests = thread::spawn(move || {
        let mut roots = Vec::new();
        for attempt in 0..21 {
            let mut request = server
                .recv_timeout(Duration::from_secs(15))
                .unwrap()
                .expect("publication request");
            assert_eq!(request.url(), "/checkpoints");
            let value: Value = serde_json::from_reader(request.as_reader()).unwrap();
            roots.push(value["artifactSetSha256"].clone());
            let (status, body) = if attempt < 20 {
                (503, json!({"ok":false,"error":"synthetic_unavailable"}))
            } else {
                (200, json!({"ok":true,"data":{"status":"announced"}}))
            };
            request
                .respond(Response::from_string(body.to_string()).with_status_code(status))
                .unwrap();
        }
        roots
    });
    let mut first_snapshot = Value::Null;
    for attempt in 0..20 {
        // Recreate the manager each time; pending authority comes only from SQLite.
        let manager = manager(&store, &endpoint);
        assert!(manager
            .publish(&session, "synthetic", 0, "", 5)
            .unwrap_err()
            .starts_with("device_sync_http_503:"));
        let pending = backup::pending_publication(&store, &session.tenant_id)
            .unwrap()
            .unwrap();
        if attempt == 0 {
            first_snapshot = pending["snapshot"].clone();
            store.upsert_observation(json!({"tenantId":"qa-pending","docId":"record","dateKey":"2026-09-05",
                "period":1,"studentCode":"1","observation":"new edit after capture","updatedAtMs":2})).unwrap();
        } else {
            assert_eq!(pending["snapshot"], first_snapshot);
        }
        let listed = backup::list_backups(&store, session.tenant_id.clone(), 50).unwrap();
        assert_eq!(listed["backups"].as_array().unwrap().len(), 1);
    }
    manager(&store, &endpoint)
        .publish(&session, "synthetic", 0, "", 5)
        .unwrap();
    assert!(backup::pending_publication(&store, &session.tenant_id)
        .unwrap()
        .is_none());
    assert!(
        backup::local_sync_state(&store, &session.tenant_id)
            .unwrap()
            .first_dirty_at_ms
            > 0
    );
    let roots = requests.join().unwrap();
    assert_eq!(roots.len(), 21);
    assert!(roots.iter().all(|value| value == &roots[0]));
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn backup_pending_replaced_after_backup_folder_changes() {
    let (root, store, session) = fixture();
    let server = Server::http("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", server.server_addr());
    let requests = thread::spawn(move || {
        for _ in 0..2 {
            let request = server
                .recv_timeout(Duration::from_secs(15))
                .unwrap()
                .expect("publication request");
            request
                .respond(Response::from_string("synthetic failure").with_status_code(503))
                .unwrap();
        }
    });
    assert!(manager(&store, &endpoint)
        .publish(&session, "synthetic", 0, "", 5)
        .unwrap_err()
        .starts_with("device_sync_http_503:"));
    let old_pending = backup::pending_publication(&store, &session.tenant_id)
        .unwrap()
        .unwrap();
    backup::set_folder(
        &store,
        session.tenant_id.clone(),
        root.join("other-backups").to_string_lossy().into(),
    )
    .unwrap();
    assert!(manager(&store, &endpoint)
        .publish(&session, "synthetic", 0, "", 5)
        .unwrap_err()
        .starts_with("device_sync_http_503:"));
    let pending = backup::pending_publication(&store, &session.tenant_id)
        .unwrap()
        .unwrap();
    let path = Path::new(pending["snapshot"]["manifestPath"].as_str().unwrap());
    assert!(path.starts_with(backup::configured_tenant_dir(&store, &session.tenant_id).unwrap()));
    assert_ne!(
        pending["snapshot"]["manifestPath"],
        old_pending["snapshot"]["manifestPath"]
    );
    assert!(Path::new(old_pending["snapshot"]["manifestPath"].as_str().unwrap()).is_file());
    requests.join().unwrap();
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn backup_lost_publish_response_is_reconciled_by_latest_without_another_snapshot() {
    let (root, store, session) = fixture();
    let server = Server::http("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", server.server_addr());
    let requests = thread::spawn(move || {
        let mut post = server
            .recv_timeout(Duration::from_secs(15))
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_reader(post.as_reader()).unwrap();
        let checkpoint = json!({"generation":1,"sourceDeviceId":"qa-device","status":"announced",
            "artifactSetSha256":value["artifactSetSha256"],"databaseSha256":value["databaseSha256"]});
        // The server accepted the candidate, but its response cannot be decoded.
        post.respond(Response::from_string("truncated response"))
            .unwrap();
        let get = server
            .recv_timeout(Duration::from_secs(15))
            .unwrap()
            .unwrap();
        assert_eq!(get.method(), &tiny_http::Method::Get);
        get.respond(Response::from_string(
            json!({"ok":true,"data":{
                "checkpoint":checkpoint,"latestVerifiedCheckpoint":null,"snapshotVersion":5
            }})
            .to_string(),
        ))
        .unwrap();
    });
    assert!(manager(&store, &endpoint)
        .publish(&session, "synthetic", 0, "", 5)
        .unwrap_err()
        .starts_with("device_sync_decode_failed:"));
    let restarted = manager(&store, &endpoint);
    let (checkpoint, _) = restarted.latest_checkpoint(&session, "synthetic").unwrap();
    restarted
        .apply_checkpoint(&session, "synthetic", &checkpoint.unwrap())
        .unwrap();
    let state = backup::local_sync_state(&store, &session.tenant_id).unwrap();
    assert_eq!(state.applied_generation, 1);
    assert_eq!(state.first_dirty_at_ms, 0);
    assert!(backup::pending_publication(&store, &session.tenant_id)
        .unwrap()
        .is_none());
    assert_eq!(
        backup::list_backups(&store, session.tenant_id.clone(), 50).unwrap()["backups"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    requests.join().unwrap();
    drop(restarted);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}
