//! Real Windows Cloud Files tests, not a OneDrive account or mocked hydrate result.
//! A missing Cloud Files driver/NTFS provider is a failing release gate, never a skip.
use std::fs::{self, OpenOptions};
use std::mem::{offset_of, size_of, zeroed};
use std::os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle};
use std::path::{Path, PathBuf};
use std::sync::{atomic::{AtomicUsize, Ordering}, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use windows_sys::Win32::{
    Foundation::{
        STATUS_CLOUD_FILE_ACCESS_DENIED, STATUS_CLOUD_FILE_AUTHENTICATION_FAILED,
        ERROR_TIMEOUT, STATUS_CLOUD_FILE_INVALID_REQUEST, STATUS_CLOUD_FILE_NETWORK_UNAVAILABLE,
        STATUS_PENDING, STATUS_SUCCESS,
    },
    Storage::{
        CloudFilters::*,
        FileSystem::{FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES},
    },
};

#[derive(Clone, Copy)]
enum Response {
    ReadAfter380,
    StallReadAfter380,
    Reject(i32),
}

#[derive(Clone, Copy, Debug)]
struct Fetch {
    explicit: bool,
    status: i32,
    offset: i64,
    length: i64,
    result: i32,
}

struct Provider {
    bytes: Vec<u8>,
    response: Response,
    fetches: Mutex<Vec<Fetch>>,
    cancellations: AtomicUsize,
}

unsafe extern "system" fn fetch_data(
    callback_info: *const CF_CALLBACK_INFO,
    callback_parameters: *const CF_CALLBACK_PARAMETERS,
) {
    let info = &*callback_info;
    let request = (*callback_parameters).Anonymous.FetchData;
    let provider = &*(info.CallbackContext as *const Provider);
    let explicit = request.Flags & CF_CALLBACK_FETCH_DATA_FLAG_EXPLICIT_HYDRATION != 0;
    // Antivirus/indexer reads must not pre-hydrate the fixture or count as app requests.
    let own_request = !info.ProcessInfo.is_null()
        && (*info.ProcessInfo).ProcessId == std::process::id();
    let selected = info.FileIdentityLength == 1 && *(info.FileIdentity as *const u8) == 1;
    let status = if !own_request || !selected {
        STATUS_CLOUD_FILE_ACCESS_DENIED
    } else {
        match provider.response {
            Response::ReadAfter380 | Response::StallReadAfter380 if explicit => STATUS_CLOUD_FILE_INVALID_REQUEST,
            Response::ReadAfter380 => STATUS_SUCCESS,
            Response::StallReadAfter380 => STATUS_PENDING,
            Response::Reject(status) => status,
        }
    };
    let offset = request.RequiredFileOffset;
    let length = request.RequiredLength.min(provider.bytes.len() as i64 - offset);
    if status == STATUS_PENDING {
        provider.fetches.lock().unwrap_or_else(|error| error.into_inner()).push(Fetch {
            explicit, status, offset, length, result: 0,
        });
        // Keep the native I/O pending until the production deadline cancels it.
        return;
    }
    let buffer = if status == STATUS_SUCCESS {
        provider.bytes.as_ptr().add(offset as usize).cast()
    } else {
        std::ptr::null()
    };
    let operation = CF_OPERATION_INFO {
        StructSize: size_of::<CF_OPERATION_INFO>() as u32,
        Type: CF_OPERATION_TYPE_TRANSFER_DATA,
        ConnectionKey: info.ConnectionKey,
        TransferKey: info.TransferKey,
        CorrelationVector: info.CorrelationVector,
        SyncStatus: std::ptr::null(),
        RequestKey: info.RequestKey,
    };
    let mut parameters: CF_OPERATION_PARAMETERS = zeroed();
    parameters.ParamSize = (offset_of!(CF_OPERATION_PARAMETERS, Anonymous)
        + size_of::<CF_OPERATION_PARAMETERS_0_6>()) as u32;
    parameters.Anonymous.TransferData = CF_OPERATION_PARAMETERS_0_6 {
        Flags: CF_OPERATION_TRANSFER_DATA_FLAG_NONE,
        CompletionStatus: status,
        Buffer: buffer,
        Offset: offset,
        Length: length,
    };
    let result = CfExecute(&operation, &mut parameters);
    if own_request {
        provider.fetches.lock().unwrap_or_else(|error| error.into_inner()).push(Fetch {
            explicit, status, offset, length, result,
        });
    }
}

unsafe extern "system" fn cancel_fetch_data(
    callback_info: *const CF_CALLBACK_INFO,
    _callback_parameters: *const CF_CALLBACK_PARAMETERS,
) {
    let provider = &*((*callback_info).CallbackContext as *const Provider);
    provider.cancellations.fetch_add(1, Ordering::SeqCst);
}

struct CloudFixture {
    root: PathBuf,
    registered: bool,
    connection: Option<CF_CONNECTION_KEY>,
    // Box keeps CallbackContext stable until CfDisconnectSyncRoot has drained callbacks.
    provider: Option<Box<Provider>>,
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn require_success(result: i32, operation: &str) {
    assert!(result >= 0, "Cloud Files native release gate: {operation}: 0x{:08x}; requires Windows Cloud Files on NTFS", result as u32);
}

impl CloudFixture {
    fn new(response: Response) -> Self {
        Self::with_policy(response, CF_HYDRATION_POLICY_PARTIAL)
    }

    fn with_policy(response: Response, hydration: CF_HYDRATION_POLICY_PRIMARY) -> Self {
        let root = std::env::temp_dir().join(format!(
            "classaimate-cf-native-{}-{:032x}", std::process::id(), rand::random::<u128>()
        ));
        fs::create_dir(&root).expect("create unique Cloud Files fixture directory");
        let mut fixture = Self {
            root,
            registered: false,
            connection: None,
            provider: Some(Box::new(Provider {
                // More than three 64 KiB reads, including a non-page-aligned EOF.
                bytes: (0..(3 * 65_536 + 37)).map(|index| (index % 251) as u8).collect(),
                response,
                fetches: Mutex::new(Vec::new()),
                cancellations: AtomicUsize::new(0),
            })),
        };
        let root_wide = wide(&fixture.root);
        let name = wide(Path::new("ClassAiMate isolated native regression provider"));
        let version = wide(Path::new("1.0"));
        unsafe {
            let mut registration: CF_SYNC_REGISTRATION = zeroed();
            registration.StructSize = size_of::<CF_SYNC_REGISTRATION>() as u32;
            registration.ProviderName = name.as_ptr();
            registration.ProviderVersion = version.as_ptr();
            let mut policies: CF_SYNC_POLICIES = zeroed();
            policies.StructSize = size_of::<CF_SYNC_POLICIES>() as u32;
            // PARTIAL proves EOF reads; FULL also exercises implicit hydration on open.
            policies.Hydration.Primary = hydration;
            policies.Population.Primary = CF_POPULATION_POLICY_ALWAYS_FULL;
            require_success(CfRegisterSyncRoot(root_wide.as_ptr(), &registration, &policies,
                CF_REGISTER_FLAG_MARK_IN_SYNC_ON_ROOT), "CfRegisterSyncRoot");
            fixture.registered = true;
            let callbacks = [
                CF_CALLBACK_REGISTRATION { Type: CF_CALLBACK_TYPE_FETCH_DATA, Callback: Some(fetch_data) },
                CF_CALLBACK_REGISTRATION { Type: CF_CALLBACK_TYPE_CANCEL_FETCH_DATA, Callback: Some(cancel_fetch_data) },
                CF_CALLBACK_REGISTRATION { Type: CF_CALLBACK_TYPE_NONE, Callback: None },
            ];
            let mut connection = 0;
            require_success(CfConnectSyncRoot(root_wide.as_ptr(), callbacks.as_ptr(),
                (&**fixture.provider.as_ref().unwrap() as *const Provider).cast(),
                CF_CONNECT_FLAG_REQUIRE_PROCESS_INFO, &mut connection), "CfConnectSyncRoot");
            fixture.connection = Some(connection);
        }
        fixture.create_placeholder("selected.bin", 1);
        fixture.create_placeholder("unselected.bin", 2);
        fixture
    }

    fn create_placeholder(&self, name: &str, identity: u8) {
        let root = wide(&self.root);
        let name = wide(Path::new(name));
        unsafe {
            let mut placeholder: CF_PLACEHOLDER_CREATE_INFO = zeroed();
            placeholder.RelativeFileName = name.as_ptr();
            placeholder.FsMetadata.BasicInfo.FileAttributes = FILE_ATTRIBUTE_NORMAL;
            placeholder.FsMetadata.FileSize = self.provider.as_ref().unwrap().bytes.len() as i64;
            placeholder.FileIdentity = (&identity as *const u8).cast();
            placeholder.FileIdentityLength = 1;
            placeholder.Flags = CF_PLACEHOLDER_CREATE_FLAG_MARK_IN_SYNC;
            let mut processed = 0;
            require_success(CfCreatePlaceholders(root.as_ptr(), &mut placeholder, 1,
                CF_CREATE_FLAG_STOP_ON_ERROR, &mut processed), "CfCreatePlaceholders");
            require_success(placeholder.Result, "placeholder.Result");
            assert_eq!(processed, 1);
        }
    }

    fn info(&self, name: &str) -> CF_PLACEHOLDER_STANDARD_INFO {
        let file = OpenOptions::new().access_mode(FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(self.root.join(name)).unwrap();
        unsafe {
            // A one-byte identity fits the SDK's trailing FileIdentity[1] field.
            let mut info: CF_PLACEHOLDER_STANDARD_INFO = zeroed();
            require_success(CfGetPlaceholderInfo(file.as_raw_handle(), CF_PLACEHOLDER_INFO_STANDARD,
                (&mut info as *mut CF_PLACEHOLDER_STANDARD_INFO).cast(), size_of::<CF_PLACEHOLDER_STANDARD_INFO>() as u32,
                std::ptr::null_mut()), "CfGetPlaceholderInfo");
            info
        }
    }

    fn fetches(&self) -> Vec<Fetch> {
        self.provider.as_ref().unwrap().fetches.lock().unwrap().clone()
    }

    fn disconnect(&mut self) {
        if let Some(connection) = self.connection {
            require_success(unsafe { CfDisconnectSyncRoot(connection) }, "CfDisconnectSyncRoot");
            self.connection = None;
        }
    }
}

impl Drop for CloudFixture {
    fn drop(&mut self) {
        let result = if let Some(connection) = self.connection.take() {
            unsafe { CfDisconnectSyncRoot(connection) }
        } else { 0 };
        if result < 0 {
            // A failed disconnect must not free memory still reachable by a callback.
            Box::leak(self.provider.take().unwrap());
            eprintln!("Cloud Files fixture cleanup blocked at {:?}: disconnect 0x{:08x}", self.root, result as u32);
        } else if self.registered {
            let unregister = unsafe { CfUnregisterSyncRoot(wide(&self.root).as_ptr()) };
            if unregister >= 0 {
                self.registered = false;
            } else {
                eprintln!("Cloud Files fixture cleanup blocked at {:?}: unregister 0x{:08x}", self.root, unregister as u32);
                if !std::thread::panicking() { require_success(unregister, "CfUnregisterSyncRoot cleanup"); }
            }
        }
        if result >= 0 && !self.registered {
            let cleanup = fs::remove_dir_all(&self.root);
            if !std::thread::panicking() { cleanup.expect("remove owned Cloud Files fixture"); }
        } else if !std::thread::panicking() {
            require_success(result, "CfDisconnectSyncRoot cleanup");
        }
    }
}

#[test]
fn onedrive_cloud_files_380_falls_back_to_real_reads_through_eof_without_pinning() {
    let mut fixture = CloudFixture::new(Response::ReadAfter380);
    let before = fixture.info("selected.bin");
    let unselected = fixture.info("unselected.bin");
    assert_eq!(before.OnDiskDataSize, 0, "test starts with a genuinely dehydrated placeholder");
    assert_eq!(unselected.OnDiskDataSize, 0);
    super::hydrate(&fixture.root.join("selected.bin")).expect("380 fallback hydrates actual bytes");
    // Freeze provider traffic before readback: verification itself must not fetch missing data.
    fixture.disconnect();
    let fetches = fixture.fetches();
    assert_eq!(fetches.iter().filter(|fetch| fetch.explicit).count(), 1, "one explicit attempt: {fetches:?}");
    assert_eq!(fetches[0].status, STATUS_CLOUD_FILE_INVALID_REQUEST);
    assert!(fetches.iter().any(|fetch| !fetch.explicit && fetch.status == STATUS_SUCCESS), "real normal-read hydration: {fetches:?}");
    assert!(fetches.iter().all(|fetch| fetch.result >= 0 && fetch.offset >= 0 && fetch.length > 0), "provider transfer results: {fetches:?}");
    let after = fixture.info("selected.bin");
    assert_eq!(after.PinState, before.PinState, "read hydration must not pin the file");
    let unselected_after = fixture.info("unselected.bin");
    assert_eq!(unselected_after.OnDiskDataSize, 0, "only selected file may be downloaded");
    assert_eq!(unselected_after.PinState, unselected.PinState);
    let actual = fs::read(fixture.root.join("selected.bin")).expect("fully hydrated file remains readable with provider disconnected");
    let expected = &fixture.provider.as_ref().unwrap().bytes;
    assert_eq!(actual.len(), expected.len());
    assert_eq!(Sha256::digest(&actual), Sha256::digest(expected));
    assert_eq!(&actual, expected);
}

#[test]
fn onedrive_cloud_files_non_380_refusal_does_not_attempt_normal_reads() {
    for (status, win32) in [
        (STATUS_CLOUD_FILE_AUTHENTICATION_FAILED, 386),
        (STATUS_CLOUD_FILE_NETWORK_UNAVAILABLE, 388),
        (STATUS_CLOUD_FILE_ACCESS_DENIED, 395),
    ] {
        let mut fixture = CloudFixture::new(Response::Reject(status));
        let error = super::hydrate(&fixture.root.join("selected.bin")).unwrap_err();
        fixture.disconnect();
        let fetches = fixture.fetches();
        assert!(error.starts_with("onedrive_download_failed:"));
        assert!(error.contains(&win32.to_string()) || error.contains(&format!("0x{:08x}", 0x80070000u32 | win32)), "numeric cause preserved: {error}");
        assert_eq!(fetches.len(), 1, "no fallback for {win32}: {fetches:?}");
        assert!(fetches[0].explicit);
        assert_eq!(fetches[0].status, status);
        assert!(fetches[0].result >= 0);
        assert_eq!(fixture.info("selected.bin").OnDiskDataSize, 0);
    }
}

#[test]
fn onedrive_cloud_files_380_read_refusal_is_failure_not_retry_or_success() {
    let mut fixture = CloudFixture::new(Response::Reject(STATUS_CLOUD_FILE_INVALID_REQUEST));
    let error = super::hydrate(&fixture.root.join("selected.bin")).unwrap_err();
    fixture.disconnect();
    let fetches = fixture.fetches();
    assert!(error.starts_with("onedrive_download_failed:"));
    assert!(error.contains("read") && error.contains("380"), "fallback phase and numeric failure: {error}");
    assert_eq!(fetches.iter().filter(|fetch| fetch.explicit).count(), 1, "no recursive hydrate: {fetches:?}");
    assert_eq!(fetches.iter().filter(|fetch| !fetch.explicit).count(), 1, "one normal-read attempt: {fetches:?}");
    assert!(fetches.iter().all(|fetch| fetch.status == STATUS_CLOUD_FILE_INVALID_REQUEST && fetch.result >= 0));
    assert_eq!(fixture.info("selected.bin").OnDiskDataSize, 0);
}

#[test]
fn onedrive_native_read_failure_retains_offset_request_size_and_observed_length() {
    let mut fixture = CloudFixture::new(Response::Reject(STATUS_CLOUD_FILE_INVALID_REQUEST));
    let error = super::with_deadline(Duration::from_secs(120), |deadline| {
        super::read_to_eof(&fixture.root.join("selected.bin"), deadline)
    }).unwrap_err();
    fixture.disconnect();
    assert_eq!(error.phase, "read_io");
    assert_eq!(error.code, 380);
    assert_eq!(error.read, Some((0, 65_536)));
    assert_eq!(error.observed_length, Some(fixture.provider.as_ref().unwrap().bytes.len() as u64));
    assert_eq!(fixture.info("selected.bin").OnDiskDataSize, 0);
}

#[test]
fn onedrive_native_failure_retains_nonzero_offset_and_timeout_context() {
    for expires in [false, true] {
        let error = super::with_deadline(Duration::from_secs(1), |deadline| {
            {
                let mut state = deadline.state.lock().unwrap();
                state.phase = "read";
                state.read = Some((65_536, 65_536));
                state.observed_length = Some(90_000);
            }
            if expires { std::thread::sleep(Duration::from_millis(1100)); }
            Err(super::DownloadError::new("read_io", 380))
        }).unwrap_err();
        assert_eq!(error.phase, if expires { "read_timeout" } else { "read_io" });
        assert_eq!(error.code, if expires { ERROR_TIMEOUT } else { 380 });
        assert_eq!(error.read, Some((65_536, 65_536)));
        assert_eq!(error.observed_length, Some(90_000));
    }
}

#[test]
fn onedrive_native_hresult_and_async_380_share_only_primary_fallback_eligibility() {
    let immediate = super::hresult_code(0x8007017cu32 as i32);
    assert_eq!(immediate, 380);
    assert!(super::DownloadError::new("hydrate_request", immediate).permits_read());
    assert!(super::DownloadError::new("hydrate_io", 380).permits_read());
    for phase in ["hydrate_open", "hydrate_event", "hydrate_wait", "read_open", "read_io"] {
        assert!(!super::DownloadError::new(phase, 380).permits_read(), "{phase}");
    }
    for code in [5, 362, 377, 386, 388, 395, 1460] {
        assert!(!super::DownloadError::new("hydrate_io", code).permits_read(), "{code}");
    }
}

#[test]
fn onedrive_cloud_files_stalled_read_is_cancelled_and_drained_with_shared_deadline() {
    assert_cancelled_and_drained(CF_HYDRATION_POLICY_PARTIAL);
}

#[test]
fn onedrive_cloud_files_full_policy_stall_is_cancelled_and_drained_with_shared_deadline() {
    assert_cancelled_and_drained(CF_HYDRATION_POLICY_FULL);
}

fn assert_cancelled_and_drained(hydration: CF_HYDRATION_POLICY_PRIMARY) {
    let mut fixture = CloudFixture::with_policy(Response::StallReadAfter380, hydration);
    let path = fixture.root.join("selected.bin");
    let started = Instant::now();
    let error = super::with_deadline(Duration::from_secs(2), |deadline| {
        let original_deadline = deadline.expires;
        let refused = super::hydrate_placeholder(&path, deadline).unwrap_err();
        assert!(refused.permits_read(), "primary request is the observed 380: {refused:?}");
        assert_eq!(deadline.expires, original_deadline);
        super::read_to_eof(&path, deadline)
    }).unwrap_err();
    assert_eq!(error.phase, "read_timeout");
    assert_eq!(error.code, ERROR_TIMEOUT);
    assert!(started.elapsed() < Duration::from_secs(10), "must not wait for Cloud Files' provider timeout");
    fixture.disconnect();
    let fetches = fixture.fetches();
    assert_eq!(fetches.iter().filter(|fetch| fetch.explicit).count(), 1, "{fetches:?}");
    assert!(fetches.iter().any(|fetch| !fetch.explicit && fetch.status == STATUS_PENDING));
    assert!(fixture.provider.as_ref().unwrap().cancellations.load(Ordering::SeqCst) > 0,
        "Cloud Files must observe cancellation before fixture context can be freed");
    assert_eq!(fixture.info("selected.bin").OnDiskDataSize, 0);
    // A completed watchdog cannot cancel a later job on the same worker thread.
    let plain = fixture.root.join("plain-after-cancel.bin");
    fs::write(&plain, b"still-readable").unwrap();
    super::with_deadline(Duration::from_secs(2), |deadline| super::read_to_eof(&plain, deadline)).unwrap();
    assert_eq!(fs::read(plain).unwrap(), b"still-readable");
}
