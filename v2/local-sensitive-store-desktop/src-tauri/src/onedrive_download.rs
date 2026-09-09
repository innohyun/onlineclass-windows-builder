//! On-demand reads for the selected device-sync snapshot. No folder pinning or recursion.
use std::cell::Cell;
use std::io;
use std::path::Path;

static DIAGNOSTIC_PATH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

pub(crate) fn configure_diagnostics(data_dir: &Path) {
    // Local app data only: never the OneDrive snapshot, relay, or returned error.
    let _ = DIAGNOSTIC_PATH.set(data_dir.join("onedrive-download-diagnostics.log"));
}

#[cfg(any(windows, test))]
const DIAGNOSTIC_LIMIT_BYTES: u64 = 256 * 1024;

#[cfg(any(windows, test))]
fn append_diagnostic(log_path: &Path, entry: &serde_json::Value) {
    use std::io::Write;
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let Ok(_guard) = LOCK.lock() else { return; };
    let Ok(mut bytes) = serde_json::to_vec(entry) else { return; };
    bytes.push(b'\n');
    if bytes.len() as u64 > DIAGNOSTIC_LIMIT_BYTES { return; }
    let current_size = log_path.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    let truncate = current_size.saturating_add(bytes.len() as u64) > DIAGNOSTIC_LIMIT_BYTES;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).write(true)
        .append(!truncate).truncate(truncate).open(log_path) {
        let _ = file.write_all(&bytes);
    }
}

#[cfg(windows)]
fn record_failure(path: &Path, phase: &str, code: u32, read: Option<(u64, u32)>, observed_length: Option<u64>) {
    use std::os::windows::ffi::OsStrExt;
    let Some(log_path) = DIAGNOSTIC_PATH.get() else { return; };
    append_diagnostic(log_path, &serde_json::json!({
        "atMs": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis(),
        "path": path.to_string_lossy(),
        "pathUtf16Length": path.as_os_str().encode_wide().count(),
        "phase": phase,
        "win32": code,
        "readOffset": read.map(|value| value.0),
        "requestedBytes": read.map(|value| value.1),
        "observedLength": observed_length,
    }));
}

thread_local! { static ENABLED: Cell<bool> = const { Cell::new(false) }; }

pub(crate) fn with_downloads<T>(read: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) { ENABLED.set(self.0); }
    }
    let _reset = Reset(ENABLED.replace(true));
    read()
}

pub(crate) fn is_pending(error: &str) -> bool {
    error == "onedrive_snapshot_pending" || error == "onedrive_download_pending" || error.starts_with("onedrive_download_pending:")
}

pub(crate) fn prepare(path: &Path) -> Result<(), String> {
    if !ENABLED.get() { return Ok(()); }
    #[cfg(test)]
    if let Some(result) = tests::prepare_fixture(path) { return result; }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS};
        let metadata = std::fs::metadata(path).map_err(|e| io_error(path, "backup_file_metadata_failed", &e))?;
        if metadata.is_file() && metadata.file_attributes() & (FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS) != 0 {
            return downloads().request(path, native::hydrate);
        }
    }
    let _ = path;
    Ok(())
}

pub(crate) fn io_error(path: &Path, context: &str, error: &io::Error) -> String {
    if ENABLED.get() {
        if error.kind() == io::ErrorKind::NotFound {
            return "onedrive_download_pending:file_not_arrived".into();
        }
        #[cfg(windows)]
        if matches!(error.raw_os_error(), Some(362 | 377 | 380 | 386 | 388 | 389)) {
            // The provider can race the metadata check. Request the exact file again;
            // never continue a partially read hash after the request completes.
            return downloads().request(path, native::hydrate).err()
                .unwrap_or_else(|| "onedrive_download_pending:retry_read".into());
        }
    }
    let _ = path;
    format!("{context}:{error}")
}

#[cfg(any(windows, test))]
mod jobs {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{mpsc, Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct Progress {
        started: Option<Instant>,
        result: Option<(Instant, Result<(), String>)>,
    }
    type Job = Arc<(Mutex<Progress>, Condvar)>;
    type Work = Box<dyn FnOnce() + Send>;
    pub(super) struct Downloads {
        jobs: Mutex<HashMap<PathBuf, Job>>,
        queue: mpsc::SyncSender<Work>,
    }

    impl Default for Downloads {
        fn default() -> Self {
            let (queue, receiver) = mpsc::sync_channel::<Work>(64);
            let receiver = Arc::new(Mutex::new(receiver));
            for _ in 0..2 {
                let receiver = Arc::clone(&receiver);
                // A fixed pool continues with the next file without waiting for a sync retry.
                let _ = std::thread::Builder::new().name("onedrive-download".into()).spawn(move || loop {
                    let work = match receiver.lock() {
                        Ok(receiver) => receiver.recv(),
                        Err(_) => return,
                    };
                    match work { Ok(work) => work(), Err(_) => return }
                });
            }
            Self { jobs: Mutex::new(HashMap::new()), queue }
        }
    }

    impl Downloads {
        pub(super) fn request(&self, path: &Path, hydrate: impl FnOnce(&Path) -> Result<(), String> + Send + 'static) -> Result<(), String> {
            let mut jobs = self.jobs.lock().map_err(|_| "onedrive_download_failed:lock")?;
            jobs.retain(|_, job| job.0.lock().map(|progress| progress.result.as_ref().map(|(at, _)| at.elapsed() < Duration::from_secs(30)).unwrap_or(true)).unwrap_or(false));
            let stalled = jobs.values().filter(|job| job.0.lock().map(|progress| {
                progress.result.is_none() && progress.started.is_some_and(|at| at.elapsed() > Duration::from_secs(130))
            }).unwrap_or(true)).count();
            if stalled >= 2 { return Err("onedrive_download_failed:provider_timeout".into()); }
            let job = if let Some(job) = jobs.get(path) {
                Arc::clone(job)
            } else {
                if jobs.len() >= 128 { return Err("onedrive_download_pending:queued".into()); }
                let job: Job = Arc::new((Mutex::new(Progress::default()), Condvar::new()));
                jobs.insert(path.to_path_buf(), Arc::clone(&job));
                let worker = Arc::clone(&job);
                let target = path.to_path_buf();
                let work: Work = Box::new(move || {
                    if let Ok(mut progress) = worker.0.lock() { progress.started = Some(Instant::now()); }
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| hydrate(&target)))
                        .unwrap_or_else(|_| Err("onedrive_download_failed:worker".into()));
                    if let Ok(mut progress) = worker.0.lock() { progress.result = Some((Instant::now(), result)); }
                    worker.1.notify_all();
                });
                if let Err(error) = self.queue.try_send(work) {
                    jobs.remove(path);
                    return Err(match error {
                        mpsc::TrySendError::Full(_) => "onedrive_download_pending:queued",
                        mpsc::TrySendError::Disconnected(_) => "onedrive_download_failed:worker_start",
                    }.into());
                }
                job
            };
            drop(jobs);
            let done = job.0.lock().map_err(|_| "onedrive_download_failed:lock")?;
            let (done, _) = job.1.wait_timeout_while(done, Duration::from_millis(20), |done| done.result.is_none())
                .map_err(|_| "onedrive_download_failed:lock")?;
            if done.result.is_none() && done.started.is_some_and(|at| at.elapsed() > Duration::from_secs(130)) {
                return Err("onedrive_download_failed:provider_timeout".into());
            }
            done.result.as_ref().map(|(_, result)| result.clone())
                .unwrap_or_else(|| Err("onedrive_download_pending".into()))
        }
    }

    #[test]
    fn onedrive_stalled_provider_is_reported_for_a_new_folder_too() {
        let downloads = Downloads::default();
        for path in ["old-folder/a", "old-folder/b"] {
            downloads.jobs.lock().unwrap().insert(PathBuf::from(path), Arc::new((Mutex::new(Progress {
                started: Some(Instant::now() - Duration::from_secs(131)), result: None,
            }), Condvar::new())));
        }
        assert_eq!(downloads.request(Path::new("new-folder/db"), |_| panic!("no extra worker")).unwrap_err(),
            "onedrive_download_failed:provider_timeout");
    }
}

#[cfg(windows)]
fn downloads() -> &'static jobs::Downloads {
    static DOWNLOADS: std::sync::OnceLock<jobs::Downloads> = std::sync::OnceLock::new();
    DOWNLOADS.get_or_init(jobs::Downloads::default)
}

#[cfg(windows)]
#[path = "onedrive_download_windows.rs"]
mod native;

#[cfg(test)]
#[path = "onedrive_download_tests.rs"]
pub(crate) mod tests;
