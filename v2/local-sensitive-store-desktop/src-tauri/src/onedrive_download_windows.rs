use std::fs::OpenOptions;
use std::os::windows::{fs::OpenOptionsExt, io::{AsRawHandle, FromRawHandle, OwnedHandle}};
use std::path::Path;
use windows_sys::Win32::Foundation::{GetLastError, ERROR_IO_PENDING, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::Storage::CloudFilters::{CfHydratePlaceholder, CF_HYDRATE_FLAG_NONE};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_OVERLAPPED};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

pub(super) fn hydrate(path: &Path) -> Result<(), String> {
    // A no-access handle is sufficient. Avoid implicit hydration on open and request
    // only this file's missing bytes (CF_EOF), without setting a permanent pin.
    let file = OpenOptions::new().access_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OVERLAPPED)
        .open(path).map_err(|e| format!("onedrive_download_failed:open:{}", e.raw_os_error().unwrap_or(0)))?;
    unsafe {
        let event = CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
        if event.is_null() { return Err(format!("onedrive_download_failed:event:{}", GetLastError())); }
        let event = OwnedHandle::from_raw_handle(event);
        let mut overlapped: OVERLAPPED = std::mem::zeroed();
        overlapped.hEvent = event.as_raw_handle();
        let result = CfHydratePlaceholder(file.as_raw_handle(), 0, -1, CF_HYDRATE_FLAG_NONE, &mut overlapped);
        if result >= 0 { return Ok(()); }
        if result as u32 != (0x80070000 | ERROR_IO_PENDING) {
            return Err(format!("onedrive_download_failed:0x{:08x}", result as u32));
        }
        let wait = WaitForSingleObject(event.as_raw_handle(), 120_000);
        let mut transferred = 0;
        if wait != WAIT_OBJECT_0 {
            // Cancel and drain before dropping OVERLAPPED/event/file. Windows may
            // finish cancellation late; the bounded pool retains ownership meanwhile.
            CancelIoEx(file.as_raw_handle(), &overlapped);
            GetOverlappedResult(file.as_raw_handle(), &overlapped, &mut transferred, 1);
            return Err(if wait == WAIT_TIMEOUT { "onedrive_download_failed:provider_timeout" }
                else { "onedrive_download_failed:provider_wait" }.into());
        }
        if GetOverlappedResult(file.as_raw_handle(), &overlapped, &mut transferred, 0) == 0 {
            return Err(format!("onedrive_download_failed:io:{}", GetLastError()));
        }
    }
    Ok(())
}
