//! Separate device DBs and OneDrive views; only the relay controls file arrival.
//! Uses production capture/publish/verify/merge/ACK code. Server SQL authority is
//! independently covered by the real repository + Worker/D1 suites.
use super::*;
use rand::{rngs::StdRng, Rng, SeedableRng};
use rusqlite::params;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tiny_http::{Response, Server};

struct Cloud {
    checkpoint: Mutex<Option<Value>>,
    ack_count: AtomicUsize,
    offline: AtomicBool,
    lose_response: AtomicBool,
    stop: AtomicBool,
}
struct Lab {
    root: PathBuf,
    stores: Vec<Arc<SqliteStore>>,
    sessions: Vec<DeviceSyncSession>,
    managers: Vec<DeviceSyncManager>,
    cloud: Arc<Cloud>,
    server: Option<thread::JoinHandle<()>>,
    trace: Vec<String>,
}
impl Lab {
    fn new(seed: u64) -> Self {
        let root = env::temp_dir().join(format!(
            "classaimate-sync-lab-{seed}-{}",
            crate::random_url_token()
        ));
        fs::create_dir_all(&root).unwrap();
        let server = Server::http("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", server.server_addr());
        let cloud = Arc::new(Cloud {
            checkpoint: Mutex::new(None),
            ack_count: AtomicUsize::new(0),
            offline: AtomicBool::new(false),
            lose_response: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let remote = cloud.clone();
        let worker = thread::spawn(move || {
            while !remote.stop.load(Ordering::SeqCst) {
                let Some(mut request) = server.recv_timeout(Duration::from_millis(20)).unwrap()
                else {
                    continue;
                };
                if remote.offline.load(Ordering::SeqCst) {
                    let _ = request.respond(Response::from_string("{}").with_status_code(503));
                    continue;
                }
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body).unwrap();
                let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                let device = request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("X-Local-Store-Device-Id"))
                    .unwrap()
                    .value
                    .as_str()
                    .to_string();
                let mut cp = remote.checkpoint.lock().unwrap();
                let (data, status) = if request.url() == "/checkpoints" {
                    let base = checkpoint_generation(cp.as_ref());
                    if body["baseGeneration"] != base {
                        (json!({}), 409)
                    } else {
                        *cp = Some(
                            json!({"generation":base+1,"baseGeneration":base,"sourceDeviceId":device,"status":"announced",
                        "artifactSetSha256":body["artifactSetSha256"],"databaseSha256":body["databaseSha256"],"snapshotVersion":body["snapshotVersion"]}),
                        );
                        (cp.clone().unwrap(), 201)
                    }
                } else {
                    let current = cp.as_mut().unwrap();
                    assert_ne!(current["sourceDeviceId"], device);
                    assert_eq!(current["artifactSetSha256"], body["artifactSetSha256"]);
                    assert_eq!(
                        request.url(),
                        format!("/checkpoints/{}/acks", current["generation"])
                    );
                    current["status"] = json!("verified");
                    remote.ack_count.fetch_add(1, Ordering::SeqCst);
                    (current.clone(), 200)
                };
                let payload = if remote.lose_response.swap(false, Ordering::SeqCst) {
                    "response-lost".into()
                } else {
                    json!({"ok":status<400,"data":data,"error":{"code":"qa_conflict"}}).to_string()
                };
                let _ = request.respond(Response::from_string(payload).with_status_code(status));
            }
        });
        let mut stores = Vec::new();
        let mut sessions = Vec::new();
        let mut managers = Vec::new();
        for device in 0..2 {
            let local = root.join(format!("device-{device}"));
            let store = Arc::new(SqliteStore::open(local.join("store.sqlite")).unwrap());
            backup::set_folder(
                &store,
                "qa-lab".into(),
                root.join(format!("view-{device}")).to_string_lossy().into(),
            )
            .unwrap();
            let session = DeviceSyncSession {
                tenant_id: "qa-lab".into(),
                device_id: format!("qa-device-{device}"),
                ..Default::default()
            };
            let mut manager = DeviceSyncManager::new(local, store.clone());
            manager.test_api_root = Some(endpoint.clone());
            manager.test_skip_retry_delay = true;
            stores.push(store);
            sessions.push(session);
            managers.push(manager);
        }
        Self {
            root,
            stores,
            sessions,
            managers,
            cloud,
            server: Some(worker),
            trace: Vec::new(),
        }
    }
    fn edit(&mut self, device: usize, key: usize, value: usize, delete: bool) {
        self.trace.push(format!(
            "edit device={device} key={key} value={value} delete={delete}"
        ));
        let conn = self.stores[device].conn.lock().unwrap();
        if delete {
            conn.execute(
                "DELETE FROM student_private_details WHERE tenant_id='qa-lab' AND student_code=?1",
                params![key.to_string()],
            )
            .unwrap();
        } else {
            conn.execute("INSERT INTO student_private_details VALUES('qa-lab',?1,?2,?3) ON CONFLICT(tenant_id,student_code) DO UPDATE SET payload_json=excluded.payload_json,updated_at_ms=excluded.updated_at_ms",params![key.to_string(),json!({"value":value}).to_string(),value as i64]).unwrap();
        }
    }
    fn cp(&self) -> Option<Value> {
        self.cloud.checkpoint.lock().unwrap().clone()
    }
    fn relay(&self, from: usize, to: usize, partial: bool) {
        let source = self.root.join(format!("view-{from}"));
        let target = self.root.join(format!("view-{to}"));
        fn copy(source: &Path, target: &Path, partial: bool) {
            if !source.exists() {
                return;
            }
            fs::create_dir_all(target).unwrap();
            for item in fs::read_dir(source).unwrap() {
                let item = item.unwrap();
                let name = item.file_name();
                if name.to_string_lossy().starts_with('.') {
                    continue;
                }
                if item.file_type().unwrap().is_dir() {
                    copy(&item.path(), &target.join(name), partial);
                } else if !partial || matches!(name.to_str(), Some("manifest.json" | "commit.json"))
                {
                    fs::copy(item.path(), target.join(name)).unwrap();
                }
            }
        }
        copy(&source, &target, partial);
    }
    fn apply(&self, device: usize, deliver: bool) -> Result<(), String> {
        let Some(cp) = self.cp() else {
            return Ok(());
        };
        let from = if cp["sourceDeviceId"] == "qa-device-0" {
            0
        } else {
            1
        };
        if deliver && from != device {
            self.relay(from, device, false);
        }
        crate::shared_archive::with_test_root(&self.stores[device].data_dir, || {
            self.managers[device].apply_checkpoint(&self.sessions[device], "synthetic-qa-only", &cp)
        })
    }
    fn publish(&self, device: usize) -> Result<(), String> {
        self.apply(device, true)?;
        let cp = self.cp();
        crate::shared_archive::with_test_root(&self.stores[device].data_dir, || {
            self.managers[device].publish(
                &self.sessions[device],
                "synthetic-qa-only",
                checkpoint_generation(cp.as_ref()),
                "announced",
                5,
            )
        })
    }
    fn rows(&self, device: usize) -> Vec<(String, String)> {
        let conn = self.stores[device].conn.lock().unwrap();
        let rows=conn.prepare("SELECT student_code,payload_json FROM student_private_details WHERE tenant_id='qa-lab' ORDER BY student_code").unwrap()
            .query_map([],|r|Ok((r.get(0)?,r.get(1)?))).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
        rows
    }
    fn settle(&mut self) {
        self.cloud.offline.store(false, Ordering::SeqCst);
        self.cloud.lose_response.store(false, Ordering::SeqCst);
        for _ in 0..3 {
            self.publish(0).unwrap();
            self.publish(1).unwrap();
        }
        self.apply(0, true).unwrap();
        self.apply(1, true).unwrap();
        assert_eq!(self.rows(0), self.rows(1), "trace {:?}", self.trace);
        for store in &self.stores {
            let state = backup::local_sync_state(store, "qa-lab").unwrap();
            assert_eq!(
                state.applied_generation,
                checkpoint_generation(self.cp().as_ref())
            );
            assert_eq!(state.first_dirty_at_ms, 0, "trace {:?}", self.trace);
            assert_eq!(
                store
                    .conn
                    .lock()
                    .unwrap()
                    .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                    .unwrap(),
                "ok"
            );
        }
    }
}
impl Drop for Lab {
    fn drop(&mut self) {
        self.cloud.stop.store(true, Ordering::SeqCst);
        if let Some(server) = self.server.take() {
            server.join().unwrap();
        }
        self.managers.clear();
        self.stores.clear();
        if !thread::panicking() {
            fs::remove_dir_all(&self.root).unwrap();
        } else {
            eprintln!(
                "synthetic replay fixture retained: {} trace={:?}",
                self.root.display(),
                self.trace
            );
        }
    }
}

#[test]
fn announced_354_with_receiver_view_only_351_never_applies_or_acks() {
    let mut lab = Lab::new(354);
    lab.edit(0, 1, 1, false);
    let old =
        backup::run_with_kind(&lab.stores[0], "qa-lab".into(), "auto_sync", Some(351)).unwrap();
    lab.relay(0, 1, false);
    let next =
        backup::run_with_kind(&lab.stores[0], "qa-lab".into(), "auto_sync", Some(354)).unwrap();
    *lab.cloud.checkpoint.lock().unwrap() = Some(
        json!({"generation":354,"sourceDeviceId":"qa-device-0","status":"announced",
        "artifactSetSha256":next["artifactSetSha256"],"databaseSha256":next["databaseSha256"]}),
    );
    let before = lab.rows(1);
    assert_eq!(
        lab.apply(1, false).unwrap_err(),
        "onedrive_snapshot_pending"
    );
    assert_eq!(lab.rows(1), before);
    assert_eq!(lab.cloud.ack_count.load(Ordering::SeqCst), 0);
    assert!(Path::new(old["manifestPath"].as_str().unwrap()).exists());
    lab.relay(0, 1, true);
    assert!(lab.apply(1, false).is_err());
    assert_eq!(lab.rows(1), before);
    assert_eq!(lab.cloud.ack_count.load(Ordering::SeqCst), 0);
    lab.relay(0, 1, false);
    let selected = PathBuf::from(next["manifestPath"].as_str().unwrap());
    let source_view = fs::canonicalize(lab.root.join("view-0")).unwrap();
    let relative = selected.strip_prefix(source_view).unwrap();
    let target_db = lab
        .root
        .join("view-1")
        .join(relative)
        .parent()
        .unwrap()
        .join("db/local-sensitive.sqlite");
    fs::write(&target_db, b"corrupt").unwrap();
    assert!(lab.apply(1, false).unwrap_err().contains("digest_mismatch"));
    assert_eq!(lab.cloud.ack_count.load(Ordering::SeqCst), 0);
    lab.apply(1, true).unwrap();
    assert_eq!(lab.rows(1), lab.rows(0));
    assert_eq!(lab.cloud.ack_count.load(Ordering::SeqCst), 1);
}

#[test]
fn dirty_delete_is_preserved_and_lost_ack_can_be_replayed() {
    let mut lab = Lab::new(91);
    lab.edit(0, 1, 1, false);
    lab.publish(0).unwrap();
    lab.apply(1, true).unwrap();
    lab.edit(1, 1, 2, true);
    lab.edit(0, 1, 3, false);
    lab.publish(0).unwrap();
    lab.cloud.lose_response.store(true, Ordering::SeqCst);
    assert!(lab.apply(1, true).is_err());
    let conn = lab.stores[1].conn.lock().unwrap();
    assert!(conn.query_row("SELECT EXISTS(SELECT 1 FROM local_store_device_sync_conflicts WHERE tenant_id='qa-lab' AND json_extract(payload_json,'$.tombstone')=1)",[],|r|r.get::<_,bool>(0)).unwrap());
    drop(conn);
    lab.apply(1, true).unwrap();
    lab.settle();
}

#[test]
#[ignore = "bounded seeded suite is run explicitly by CI/runner"]
fn seeded_convergence() {
    let number = |name: &str, default: usize| {
        env::var(name)
            .ok()
            .map(|v| v.parse().expect("numeric seed option"))
            .unwrap_or(default)
    };
    let count = number("CLASSAIMATE_QA_SEEDS", 100);
    let events = number("CLASSAIMATE_QA_EVENTS", 50);
    let start = number("CLASSAIMATE_QA_SEED_START", 0);
    assert!(count > 0 && count <= 1000 && events > 0 && events <= 200);
    for seed in start..start + count {
        let mut lab = Lab::new(seed as u64);
        let mut rng = StdRng::seed_from_u64(seed as u64);
        let mut generation = 0;
        for event in 1..=events {
            let device = rng.gen_range(0..2);
            let action = rng.gen_range(0..10);
            lab.trace.push(format!(
                "seed={seed} event={event} device={device} action={action}"
            ));
            match action {
                0..=4 => lab.edit(device, rng.gen_range(0..6), event, action == 4),
                5 => {
                    lab.cloud.offline.store(true, Ordering::SeqCst);
                    let _ = lab.publish(device);
                    lab.cloud.offline.store(false, Ordering::SeqCst);
                }
                6 => {
                    lab.cloud.lose_response.store(true, Ordering::SeqCst);
                    let _ = lab.publish(device);
                    lab.cloud.lose_response.store(false, Ordering::SeqCst);
                }
                7 => {
                    if let Some(cp) = lab.cp() {
                        let from = if cp["sourceDeviceId"] == "qa-device-0" {
                            0
                        } else {
                            1
                        };
                        if from != device {
                            lab.relay(from, device, true);
                        }
                    }
                    let _ = lab.apply(device, false);
                }
                _ => lab.publish(device).unwrap(),
            }
            let latest = checkpoint_generation(lab.cp().as_ref());
            assert!(latest >= generation, "seed={seed} trace={:?}", lab.trace);
            generation = latest;
        }
        lab.settle();
        eprintln!("sync replay passed seed={seed} events={events}");
    }
}
