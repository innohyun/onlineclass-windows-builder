use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use tauri::{Emitter, Manager};

const LOG_LIMIT_BYTES: u64 = 128 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DesktopActivationIntent {
    ShowMain,
    QuickObservation,
}

impl DesktopActivationIntent {
    pub(crate) fn from_args(args: impl IntoIterator<Item = String>) -> Self {
        if args.into_iter().any(|arg| arg == "--quick-observation") {
            Self::QuickObservation
        } else {
            Self::ShowMain
        }
    }
}

pub(crate) struct DesktopActivationState {
    pending: Mutex<Option<DesktopActivationIntent>>,
    main_window: Mutex<Option<tauri::WebviewWindow>>,
    log_path: PathBuf,
}

impl DesktopActivationState {
    pub(crate) fn new(
        data_dir: PathBuf,
        initial: DesktopActivationIntent,
        main_window: Option<tauri::WebviewWindow>,
    ) -> Self {
        Self {
            pending: Mutex::new(Some(initial)),
            main_window: Mutex::new(main_window),
            log_path: data_dir.join("desktop-activation.log"),
        }
    }

    pub(crate) fn take(&self) -> Option<DesktopActivationIntent> {
        self.pending.lock().ok().and_then(|mut value| value.take())
    }

    fn set(&self, intent: DesktopActivationIntent) {
        if let Ok(mut value) = self.pending.lock() {
            *value = Some(intent);
        }
    }

    fn main_window(&self) -> Option<tauri::WebviewWindow> {
        self.main_window.lock().ok().and_then(|window| window.clone())
    }

    fn remember_main_window(&self, window: tauri::WebviewWindow) {
        if let Ok(mut value) = self.main_window.lock() {
            *value = Some(window);
        }
    }

    fn log(&self, source: &str, attempt: u8, result: &Result<(), String>) {
        if let Some(parent) = self.log_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if self.log_path.metadata().map(|metadata| metadata.len() >= LOG_LIMIT_BYTES).unwrap_or(false) {
            let rotated = self.log_path.with_extension("log.old");
            let _ = fs::remove_file(&rotated);
            let _ = fs::rename(&self.log_path, rotated);
        }
        let outcome = match result {
            Ok(()) => "ok".to_string(),
            Err(error) => format!("error={}", error.replace(['\r', '\n'], " ")),
        };
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&self.log_path) {
            let _ = writeln!(
                file,
                "{} source={} attempt={} {}",
                Utc::now().to_rfc3339(),
                source,
                attempt,
                outcome
            );
        }
    }
}

fn main_window(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    if let Some(window) = app
        .try_state::<DesktopActivationState>()
        .and_then(|state| state.main_window())
    {
        return Ok(window);
    }
    if let Some(window) = app.get_webview_window("main") {
        if let Some(state) = app.try_state::<DesktopActivationState>() {
            state.remember_main_window(window.clone());
        }
        return Ok(window);
    }
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|config| config.label == "main")
        .cloned()
        .ok_or_else(|| "main_window_config_missing".to_string())?;
    let window = tauri::WebviewWindowBuilder::from_config(app, &config)
        .map_err(|error| format!("main_window_builder_failed:{error}"))?
        .build()
        .map_err(|error| format!("main_window_create_failed:{error}"))?;
    if let Some(state) = app.try_state::<DesktopActivationState>() {
        state.remember_main_window(window.clone());
    }
    Ok(window)
}

#[cfg(target_os = "windows")]
fn native_restore(window: &tauri::WebviewWindow) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetForegroundWindow, SetWindowPos, ShowWindow, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOMOVE,
        SWP_NOSIZE, SWP_SHOWWINDOW, SW_RESTORE,
    };

    let hwnd = window
        .hwnd()
        .map_err(|error| format!("main_window_hwnd_failed:{error}"))?;
    let raw = hwnd.0 as windows_sys::Win32::Foundation::HWND;
    unsafe {
        ShowWindow(raw, SW_RESTORE);
        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW;
        if SetWindowPos(raw, HWND_TOPMOST, 0, 0, 0, 0, flags) == 0 {
            return Err("main_window_native_raise_failed".to_string());
        }
        if SetWindowPos(raw, HWND_NOTOPMOST, 0, 0, 0, 0, flags) == 0 {
            return Err("main_window_native_release_topmost_failed".to_string());
        }
        if SetForegroundWindow(raw) == 0 {
            return Err("main_window_native_focus_failed".to_string());
        }
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn native_restore(_window: &tauri::WebviewWindow) -> Result<(), String> {
    Ok(())
}

fn activate_once(
    app: &tauri::AppHandle,
    intent: DesktopActivationIntent,
) -> Result<(), String> {
    let window = main_window(app)?;
    window
        .unminimize()
        .map_err(|error| format!("main_window_unminimize_failed:{error}"))?;
    window
        .show()
        .map_err(|error| format!("main_window_show_failed:{error}"))?;
    window
        .set_focus()
        .map_err(|error| format!("main_window_focus_failed:{error}"))?;
    native_restore(&window)?;
    let visible = window
        .is_visible()
        .map_err(|error| format!("main_window_visibility_check_failed:{error}"))?;
    let focused = window
        .is_focused()
        .map_err(|error| format!("main_window_focus_check_failed:{error}"))?;
    if !visible || !focused {
        return Err("main_window_not_foreground_after_restore".to_string());
    }
    window.emit("desktop-activation", intent)
        .map_err(|error| format!("desktop_activation_emit_failed:{error}"))?;
    Ok(())
}

fn run_attempt(
    app: tauri::AppHandle,
    intent: DesktopActivationIntent,
    source: String,
    attempt: u8,
) {
    let scheduled_app = app.clone();
    let retry_app = app.clone();
    let retry_source = source.clone();
    let scheduled = scheduled_app.run_on_main_thread(move || {
        let result = activate_once(&app, intent);
        if let Some(state) = app.try_state::<DesktopActivationState>() {
            state.log(&source, attempt, &result);
        }
        if let Err(error) = &result {
            eprintln!("[local-sensitive-store] desktop activation failed: {error}");
        }
        if result.is_err() && attempt == 1 {
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(160));
                run_attempt(retry_app, intent, retry_source, 2);
            });
        }
    });
    if let Err(error) = scheduled {
        eprintln!("[local-sensitive-store] desktop activation scheduling failed: {error}");
    }
}

pub(crate) fn activate(
    app: &tauri::AppHandle,
    intent: DesktopActivationIntent,
    source: &str,
) {
    if let Some(state) = app.try_state::<DesktopActivationState>() {
        state.set(intent);
    }
    run_attempt(app.clone(), intent, source.to_string(), 1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_shortcut_argument_has_a_distinct_intent() {
        assert_eq!(
            DesktopActivationIntent::from_args(["app.exe".to_string(), "--quick-observation".to_string()]),
            DesktopActivationIntent::QuickObservation
        );
        assert_eq!(
            DesktopActivationIntent::from_args(["app.exe".to_string()]),
            DesktopActivationIntent::ShowMain
        );
    }
}
