use super::*;
use std::cell::RefCell;
use std::sync::{atomic::{AtomicUsize, Ordering}, mpsc, Arc};

#[test]
fn onedrive_local_diagnostic_preserves_exact_path_and_stays_bounded() {
    let root = std::env::temp_dir().join(format!("ca-onedrive-diagnostic-{}", crate::random_url_token()));
    std::fs::create_dir_all(&root).unwrap();
    let log = root.join("onedrive-download-diagnostics.log");
    let exact_path = "C:\\Users\\교사\\OneDrive - 학교\\첨부 이름\\수학 관찰기록.docx";
    let entry = serde_json::json!({"path":exact_path,"phase":"read_io","win32":380,
        "readOffset":65536,"requestedBytes":65536,"observedLength":90000});
    append_diagnostic(&log, &entry);
    let readback: serde_json::Value = serde_json::from_slice(&std::fs::read(&log).unwrap()).unwrap();
    assert_eq!(readback, entry);
    // The next failure remains available even after repeated provider errors fill the log.
    std::fs::write(&log, vec![b'x'; DIAGNOSTIC_LIMIT_BYTES as usize]).unwrap();
    append_diagnostic(&log, &entry);
    assert!(log.metadata().unwrap().len() <= DIAGNOSTIC_LIMIT_BYTES);
    let readback: serde_json::Value = serde_json::from_slice(&std::fs::read(&log).unwrap()).unwrap();
    assert_eq!(readback, entry);
    std::fs::remove_dir_all(root).unwrap();
}

type Reader = Box<dyn Fn(&Path) -> Result<(), String>>;
thread_local! { static FIXTURE: RefCell<Option<Reader>> = RefCell::new(None); }

pub(super) fn prepare_fixture(path: &Path) -> Option<Result<(), String>> {
    FIXTURE.with(|fixture| fixture.borrow().as_ref().map(|read| read(path)))
}

pub(crate) fn with_fixture<T>(reader: impl Fn(&Path) -> Result<(), String> + 'static, action: impl FnOnce() -> T) -> T {
    struct Reset(Option<Reader>);
    impl Drop for Reset {
        fn drop(&mut self) { FIXTURE.with(|fixture| { fixture.replace(self.0.take()); }); }
    }
    let _reset = Reset(FIXTURE.with(|fixture| fixture.replace(Some(Box::new(reader)))));
    action()
}

#[test]
fn onedrive_scope_does_not_download_for_inventory_or_normal_local_reads() {
    let reads = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&reads);
    with_fixture(move |_| { seen.fetch_add(1, Ordering::SeqCst); Err("onedrive_download_pending".into()) }, || {
        assert!(prepare(Path::new("ordinary-local-file")).is_ok());
        assert!(is_pending(&with_downloads(|| prepare(Path::new("selected-file"))).unwrap_err()));
        assert!(prepare(Path::new("ordinary-local-file")).is_ok());
    });
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    let error = io::Error::from(io::ErrorKind::NotFound);
    assert!(with_downloads(|| io_error(Path::new("missing"), "read", &error)).starts_with("onedrive_download_pending:"));
    assert!(io_error(Path::new("missing"), "read", &error).starts_with("read:"));
    assert!(!is_pending("backup_artifact_digest_mismatch"));
    assert!(!is_pending("onedrive_download_failed:0x8007017c"));
}

#[test]
fn onedrive_requests_coalesce_and_do_not_block_or_spawn_unbounded_workers() {
    let downloads = jobs::Downloads::default();
    let (first_done, first_wait) = mpsc::channel();
    let (second_done, second_wait) = mpsc::channel();
    let (third_done, third_wait) = mpsc::channel();
    let first = Path::new("selected-db.sqlite");
    let second = Path::new("selected-attachment");
    assert!(is_pending(&downloads.request(first, move |_| { first_wait.recv().unwrap(); Ok(()) }).unwrap_err()));
    assert!(is_pending(&downloads.request(first, |_| panic!("same path must not start twice")).unwrap_err()));
    assert!(is_pending(&downloads.request(second, move |_| { second_wait.recv().unwrap(); Ok(()) }).unwrap_err()));
    assert!(is_pending(&downloads.request(Path::new("queued-file"), move |_| { third_done.send(()).unwrap(); Ok(()) }).unwrap_err()));
    assert!(third_wait.try_recv().is_err(), "only two workers may run concurrently");
    first_done.send(()).unwrap();
    second_done.send(()).unwrap();
    third_wait.recv_timeout(std::time::Duration::from_secs(1)).expect("queued file starts without a sync retry");
    assert!(downloads.request(first, |_| panic!("consume existing request")).is_ok());
    assert!(downloads.request(second, |_| panic!("consume existing request")).is_ok());
}

#[test]
fn onedrive_provider_error_is_reported_and_cached_without_paths() {
    let downloads = jobs::Downloads::default();
    let path = Path::new("private/teacher/backup.sqlite");
    let result = downloads.request(path, |_| Err("onedrive_download_failed:0x8007017c".into()));
    assert_eq!(result.unwrap_err(), "onedrive_download_failed:0x8007017c");
    assert_eq!(downloads.request(path, |_| panic!("do not spin on provider failure")).unwrap_err(), "onedrive_download_failed:0x8007017c");
}

#[cfg(windows)]
#[test]
fn onedrive_plain_windows_file_remains_readable_without_provider_or_pin_changes() {
    let root = std::env::temp_dir().join(format!("classaimate-cloud-read-{}", crate::random_url_token()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("plain.txt");
    std::fs::write(&path, b"local-only").unwrap();
    with_downloads(|| {
        prepare(&path).unwrap();
        assert_eq!(crate::backup::sha256_file(&path).unwrap().0, 10);
    });
    std::fs::remove_dir_all(root).unwrap();
}
