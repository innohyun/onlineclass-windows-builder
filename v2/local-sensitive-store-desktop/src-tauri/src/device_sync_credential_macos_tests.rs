use super::{keyring_entry, DeviceSyncCredentialStore};
use rand::{distributions::Alphanumeric, Rng};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use zeroize::Zeroizing;

const CHILD_TEST: &str = "device_sync_credential::macos_tests::keychain_child";

#[test]
fn mac_port_never_resumes_legacy_cloud_sync() {
    let root = std::env::temp_dir().join(format!("classaimate-mac-legacy-qa-{}", crate::random_url_token()));
    std::fs::create_dir_all(&root).unwrap();
    let store = std::sync::Arc::new(crate::SqliteStore::open(root.join("fixture.sqlite")).unwrap());
    let manager = crate::cloud_sync::CloudSyncManager::new(root.clone(), store);
    assert_eq!(manager.connect(serde_json::json!({})).unwrap_err(), "legacy_cloud_sync_disabled");
    assert_eq!(manager.run_once().unwrap_err(), "legacy_cloud_sync_disabled");
    assert!(!root.join("cloud-sync-session.json").exists());
    assert!(!root.join("cloud-sync-credentials.json").exists());
    let legacy_path = root.join("cloud-sync-session.json");
    std::fs::write(&legacy_path, b"unreadable legacy session fixture").unwrap();
    let status = manager.status().unwrap();
    assert_eq!(status["connected"], false);
    assert_eq!(status["disabledReason"], "legacy_cloud_sync_disabled");
    assert_eq!(std::fs::read(&legacy_path).unwrap(), b"unreadable legacy session fixture");
    drop(manager);
    std::fs::remove_dir_all(root).unwrap();
}

struct TestAccount(String);

impl Drop for TestAccount {
    fn drop(&mut self) {
        DeviceSyncCredentialStore::new(std::env::temp_dir()).delete(&self.0);
    }
}

#[test]
#[ignore = "writes a unique synthetic credential to the real macOS Keychain"]
fn native_keychain_survives_process_restart() {
    let random: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(24)
        .map(char::from)
        .collect();
    let account = TestAccount(format!("macos-keychain-qa:{}:{random}", std::process::id()));
    let credential = Zeroizing::new(
        rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(43)
            .map(char::from)
            .collect::<String>(),
    );
    // Every phase runs after the previous process has completely exited.
    for phase in ["missing", "store", "read", "invalid", "read", "delete", "missing"] {
        let mut child = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", CHILD_TEST, "--ignored", "--test-threads=1"])
            .env("CLASSAIMATE_KEYCHAIN_QA_ACCOUNT", &account.0)
            .env("CLASSAIMATE_KEYCHAIN_QA_PHASE", phase)
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .spawn()
            .expect("start isolated Keychain phase");
        child.stdin.take().expect("child stdin")
            .write_all(credential.as_bytes()).expect("send synthetic credential");
        assert!(child.wait().expect("wait for phase").success(), "Keychain phase failed: {phase}");
    }
}

#[test]
#[ignore = "only invoked by native_keychain_survives_process_restart"]
fn keychain_child() {
    let Ok(account) = std::env::var("CLASSAIMATE_KEYCHAIN_QA_ACCOUNT") else { return };
    assert!(account.starts_with("macos-keychain-qa:"));
    let phase = std::env::var("CLASSAIMATE_KEYCHAIN_QA_PHASE").expect("phase");
    let mut credential = Zeroizing::new(String::new());
    std::io::stdin().read_to_string(&mut credential).expect("synthetic credential input");
    let store = DeviceSyncCredentialStore::new(std::env::temp_dir());
    match phase.as_str() {
        "store" => assert_eq!(store.store(&account, &credential).expect("native store"), "macos_keychain"),
        "read" => {
            let actual = Zeroizing::new(store.read(&account).expect("native read"));
            assert!(actual.as_str() == credential.as_str(), "credential mismatch");
        }
        "invalid" => assert!(store.store(&account, "invalid").is_err()),
        "delete" => store.delete(&account),
        "missing" => {
            assert!(matches!(keyring_entry(&account).expect("native entry").get_password(), Err(keyring::Error::NoEntry)));
            assert!(store.read(&account).is_err());
        }
        _ => panic!("unknown Keychain phase"),
    }
}
