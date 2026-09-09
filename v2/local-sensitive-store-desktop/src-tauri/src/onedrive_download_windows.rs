use std::fs::{File, OpenOptions};
use std::os::windows::{fs::OpenOptionsExt, io::{AsRawHandle, FromRawHandle, OwnedHandle}};
use std::path::Path;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_CLOUD_FILE_INVALID_REQUEST,
    ERROR_HANDLE_EOF, ERROR_IO_PENDING, ERROR_TIMEOUT, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::Storage::CloudFilters::{CfHydratePlaceholder, CF_HYDRATE_FLAG_NONE};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_FLAG_OVERLAPPED, FILE_FLAG_SEQUENTIAL_SCAN};
use windows_sys::Win32::System::IO::{CancelIoEx, CancelSynchronousIo, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, GetCurrentThreadId, OpenThread,
    ResetEvent, WaitForSingleObject, THREAD_TERMINATE};

#[derive(Debug)]
struct DownloadError {
    phase: &'static str,
    code: u32,
    read: Option<(u64, u32)>,
    observed_length: Option<u64>,
}

impl DownloadError {
    fn new(phase: &'static str, code: u32) -> Self {
        Self { phase, code, read: None, observed_length: None }
    }
    fn last(phase: &'static str) -> Self { Self::new(phase, unsafe { GetLastError() }) }
    fn permits_read(&self) -> bool {
        self.code == ERROR_CLOUD_FILE_INVALID_REQUEST
            && matches!(self.phase, "hydrate_request" | "hydrate_io")
    }
}

fn hresult_code(result: i32) -> u32 {
    let value = result as u32;
    if value & 0xffff0000 == 0x80070000 { value & 0xffff } else { value }
}

struct DeadlineState {
    finished: bool,
    file: Option<usize>,
    phase: &'static str,
    read: Option<(u64, u32)>,
    observed_length: Option<u64>,
}
struct Deadline { expires: Instant, state: Mutex<DeadlineState>, changed: Condvar }

impl Deadline {
    fn timeout(&self) -> DownloadError {
        let phase = self.state.lock().unwrap().phase;
        DownloadError::new(if phase == "read" { "read_timeout" } else { "hydrate_timeout" }, ERROR_TIMEOUT)
    }
    fn check(&self) -> Result<(), DownloadError> {
        if Instant::now() >= self.expires { Err(self.timeout()) } else { Ok(()) }
    }
    fn remaining_ms(&self) -> u32 {
        let remaining = self.expires.saturating_duration_since(Instant::now());
        if remaining.is_zero() { 0 } else { remaining.as_millis().saturating_add(1).min(u32::MAX as u128) as u32 }
    }
    fn register<'a>(&'a self, file: &'a File) -> ActiveFile<'a> {
        self.state.lock().unwrap().file = Some(file.as_raw_handle() as usize);
        ActiveFile { deadline: self, _file: file }
    }
    fn watch(&self, thread: OwnedHandle) {
        let mut state = self.state.lock().unwrap();
        while !state.finished {
            let remaining = self.expires.saturating_duration_since(Instant::now());
            let wait = if remaining.is_zero() {
                // CreateFile and even a cached ReadFile can block synchronously.
                // The real thread handle cannot be reused by the next queue job:
                // this watchdog is joined before hydrate returns.
                unsafe {
                    CancelSynchronousIo(thread.as_raw_handle());
                    if let Some(file) = state.file { CancelIoEx(file as HANDLE, std::ptr::null()); }
                }
                Duration::from_millis(50)
            } else { remaining };
            state = self.changed.wait_timeout(state, wait).unwrap().0;
        }
    }
}

// Unregister under the same mutex used by the watchdog before the File can drop.
struct ActiveFile<'a> { deadline: &'a Deadline, _file: &'a File }
impl Drop for ActiveFile<'_> {
    fn drop(&mut self) { self.deadline.state.lock().unwrap().file = None; }
}
struct FinishWatch<'a>(&'a Deadline);
impl Drop for FinishWatch<'_> {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().finished = true;
        self.0.changed.notify_all();
    }
}

fn with_deadline(duration: Duration, operation: impl FnOnce(&Deadline) -> Result<(), DownloadError>) -> Result<(), DownloadError> {
    let deadline = Deadline { expires: Instant::now() + duration,
        state: Mutex::new(DeadlineState { finished: false, file: None, phase: "hydrate", read: None, observed_length: None }), changed: Condvar::new() };
    let thread = unsafe { OpenThread(THREAD_TERMINATE, 0, GetCurrentThreadId()) };
    if thread.is_null() { return Err(DownloadError::last("deadline_thread")); }
    let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
    std::thread::scope(|scope| {
        // There is at most one watchdog per occupied bounded download worker.
        let watcher = std::thread::Builder::new().name("onedrive-deadline".into())
            .spawn_scoped(scope, || deadline.watch(thread))
            .map_err(|error| DownloadError::new("deadline_worker", error.raw_os_error().unwrap_or(0) as u32))?;
        let finish = FinishWatch(&deadline);
        let result = operation(&deadline);
        drop(finish);
        watcher.join().expect("onedrive deadline watcher panicked");
        deadline.check().and(result).map_err(|mut error| {
            let state = deadline.state.lock().unwrap();
            error.read = state.read;
            error.observed_length = state.observed_length;
            error
        })
    })
}

fn event(phase: &'static str) -> Result<OwnedHandle, DownloadError> {
    let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if handle.is_null() { return Err(DownloadError::last(phase)); }
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn wait_pending(file: &File, overlapped: &mut OVERLAPPED, deadline: &Deadline,
    wait_phase: &'static str, io_phase: &'static str) -> Result<u32, DownloadError> {
    unsafe {
        let wait = WaitForSingleObject(overlapped.hEvent, deadline.remaining_ms());
        let mut transferred = 0;
        if wait != WAIT_OBJECT_0 {
            let error = if wait == WAIT_TIMEOUT { deadline.timeout() } else { DownloadError::last(wait_phase) };
            CancelIoEx(file.as_raw_handle(), overlapped);
            // A cancellation request is not completion. Keep file, event,
            // OVERLAPPED and the caller's read buffer alive until it is drained.
            GetOverlappedResult(file.as_raw_handle(), overlapped, &mut transferred, 1);
            return Err(error);
        }
        if GetOverlappedResult(file.as_raw_handle(), overlapped, &mut transferred, 0) == 0 {
            return Err(DownloadError::last(io_phase));
        }
        Ok(transferred)
    }
}

fn hydrate_placeholder(path: &Path, deadline: &Deadline) -> Result<(), DownloadError> {
    deadline.check()?;
    // No-access + OPEN_REPARSE_POINT avoids implicit hydration on this first open.
    let file = OpenOptions::new().access_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OVERLAPPED).open(path)
        .map_err(|error| DownloadError::new("hydrate_open", error.raw_os_error().unwrap_or(0) as u32))?;
    let _active = deadline.register(&file);
    let observed_length = file.metadata().ok().map(|metadata| metadata.len());
    deadline.state.lock().unwrap().observed_length = observed_length;
    deadline.check()?;
    let event = event("hydrate_event")?;
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.hEvent = event.as_raw_handle();
    let result = unsafe { CfHydratePlaceholder(file.as_raw_handle(), 0, -1, CF_HYDRATE_FLAG_NONE, &mut overlapped) };
    if result >= 0 { return Ok(()); }
    let code = hresult_code(result);
    if code != ERROR_IO_PENDING { return Err(DownloadError::new("hydrate_request", code)); }
    wait_pending(&file, &mut overlapped, deadline, "hydrate_wait", "hydrate_io").map(|_| ())
}

fn read_to_eof(path: &Path, deadline: &Deadline) -> Result<(), DownloadError> {
    deadline.check()?;
    {
        let mut state = deadline.state.lock().unwrap();
        state.phase = "read";
        state.read = Some((0, 0));
    }
    // An ordinary read-only open lets the cloud filter handle this request. Never
    // OPEN_REPARSE_POINT, pin, write, or assume that requesting bytes verifies them.
    let file = OpenOptions::new().read(true)
        .custom_flags(FILE_FLAG_OVERLAPPED | FILE_FLAG_SEQUENTIAL_SCAN).open(path)
        .map_err(|error| DownloadError::new("read_open", error.raw_os_error().unwrap_or(0) as u32))?;
    let _active = deadline.register(&file);
    let observed_length = file.metadata().ok().map(|metadata| metadata.len());
    deadline.state.lock().unwrap().observed_length = observed_length;
    let event = event("read_event")?;
    let mut buffer = [0u8; 64 * 1024];
    let mut offset = 0u64;
    loop {
        deadline.check()?;
        if unsafe { ResetEvent(event.as_raw_handle()) } == 0 { return Err(DownloadError::last("read_event")); }
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = event.as_raw_handle();
        overlapped.Anonymous.Anonymous.Offset = offset as u32;
        overlapped.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
        deadline.state.lock().unwrap().read = Some((offset, buffer.len() as u32));
        let started = unsafe { ReadFile(file.as_raw_handle(), buffer.as_mut_ptr(), buffer.len() as u32,
            std::ptr::null_mut(), &mut overlapped) };
        let result = if started == 0 {
            let code = unsafe { GetLastError() };
            if code == ERROR_IO_PENDING { wait_pending(&file, &mut overlapped, deadline, "read_wait", "read_io") }
            else { Err(DownloadError::new("read_io", code)) }
        } else {
            let mut transferred = 0;
            if unsafe { GetOverlappedResult(file.as_raw_handle(), &overlapped, &mut transferred, 0) } == 0 {
                Err(DownloadError::last("read_io"))
            } else { Ok(transferred) }
        };
        match result {
            Ok(0) => return Ok(()),
            Ok(count) => offset += u64::from(count),
            Err(error) if error.code == ERROR_HANDLE_EOF => return Ok(()),
            Err(error) => return Err(error),
        }
    }
}

pub(super) fn hydrate(path: &Path) -> Result<(), String> {
    with_deadline(Duration::from_secs(120), |deadline| {
        match hydrate_placeholder(path, deadline) {
            Err(error) if error.permits_read() => read_to_eof(path, deadline),
            result => result,
        }
    }).map_err(|error| {
        super::record_failure(path, error.phase, error.code, error.read, error.observed_length);
        format!("onedrive_download_failed:{}:{}", error.phase, error.code)
    })
}

#[cfg(test)]
#[path = "onedrive_download_windows_tests.rs"]
mod tests;
