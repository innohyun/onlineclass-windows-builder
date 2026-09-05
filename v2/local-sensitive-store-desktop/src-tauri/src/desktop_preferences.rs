use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[cfg(target_os = "macos")]
#[path = "macos_autostart.rs"]
mod macos_autostart;

const PREFERENCES_FILE_NAME: &str = "desktop-preferences.json";
#[cfg(windows)]
const AUTOSTART_VALUE_NAME: &str = "OnlineClassLocalSensitiveStore";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DesktopPreferences {
    pub start_with_windows: bool,
    pub keep_running_on_close: bool,
}

impl Default for DesktopPreferences {
    fn default() -> Self {
        Self {
            #[cfg(not(target_os = "macos"))]
            start_with_windows: true,
            #[cfg(target_os = "macos")]
            start_with_windows: false,
            keep_running_on_close: true,
        }
    }
}

pub struct DesktopPreferencesStore {
    path: PathBuf,
    value: Mutex<DesktopPreferences>,
    startup_error: Mutex<Option<String>>,
}

impl DesktopPreferencesStore {
    pub fn open(data_dir: &Path) -> Self {
        let path = data_dir.join(PREFERENCES_FILE_NAME);
        let value = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<DesktopPreferences>(&raw).ok())
            .unwrap_or_default();
        Self {
            path,
            value: Mutex::new(value),
            startup_error: Mutex::new(None),
        }
    }

    pub fn snapshot(&self) -> DesktopPreferences {
        self.value
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default()
    }

    pub fn apply_startup_setting(&self) -> Result<(), String> {
        // Opening an isolated/new Mac store must never change this user's login items.
        #[cfg(target_os = "macos")]
        let result = macos_autostart::verify(self.snapshot().start_with_windows);
        #[cfg(not(target_os = "macos"))]
        let result = apply_start_with_windows(self.snapshot().start_with_windows);
        if let Ok(mut error) = self.startup_error.lock() {
            *error = result.as_ref().err().cloned();
        }
        result
    }

    pub fn startup_error(&self) -> Option<String> {
        self.startup_error
            .lock()
            .ok()
            .and_then(|error| error.clone())
    }

    fn remember_startup_error(&self, error: String) -> String {
        if let Ok(mut stored) = self.startup_error.lock() {
            *stored = Some(error.clone());
        }
        error
    }

    pub fn set(&self, key: &str, enabled: bool) -> Result<DesktopPreferences, String> {
        self.set_with_autostart(key, enabled, apply_start_with_windows)
    }

    fn set_with_autostart(
        &self,
        key: &str,
        enabled: bool,
        apply: impl Fn(bool) -> Result<(), String>,
    ) -> Result<DesktopPreferences, String> {
        let mut value = self
            .value
            .lock()
            .map_err(|_| "desktop_preferences_lock_failed".to_string())?;
        let previous = value.clone();
        let mut next = previous.clone();
        match key {
            "startWithWindows" => next.start_with_windows = enabled,
            "keepRunningOnClose" => next.keep_running_on_close = enabled,
            _ => return Err("desktop_preference_key_invalid".to_string()),
        }

        if key == "startWithWindows" {
            if let Err(error) = apply(enabled) {
                return Err(self.remember_startup_error(error));
            }
        }
        if let Err(error) = self.persist(&next) {
            if key == "startWithWindows" {
                if let Err(rollback_error) = apply(previous.start_with_windows) {
                    return Err(self.remember_startup_error(format!(
                        "{error};autostart_rollback_failed:{rollback_error}"
                    )));
                }
                return Err(self.remember_startup_error(error));
            }
            return Err(error);
        }
        *value = next.clone();
        if key == "startWithWindows" {
            if let Ok(mut error) = self.startup_error.lock() {
                *error = None;
            }
        }
        Ok(next)
    }

    fn persist(&self, value: &DesktopPreferences) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("desktop_preferences_dir_failed:{e}"))?;
        }
        let raw = serde_json::to_string_pretty(value)
            .map_err(|e| format!("desktop_preferences_encode_failed:{e}"))?;
        fs::write(&self.path, raw).map_err(|e| format!("desktop_preferences_write_failed:{e}"))
    }
}

#[cfg(windows)]
fn apply_start_with_windows(enabled: bool) -> Result<(), String> {
    use std::env;
    use std::io::ErrorKind;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey_with_flags(
            "Software\\Microsoft\\Windows\\CurrentVersion\\Run",
            KEY_SET_VALUE,
        )
        .map_err(|e| format!("autostart_key_failed:{e}"))?;
    if enabled {
        let exe = env::current_exe().map_err(|e| format!("autostart_exe_failed:{e}"))?;
        let command = format!("\"{}\" --background", exe.to_string_lossy());
        key.set_value(AUTOSTART_VALUE_NAME, &command)
            .map_err(|e| format!("autostart_set_failed:{e}"))
    } else {
        match key.delete_value(AUTOSTART_VALUE_NAME) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("autostart_remove_failed:{error}")),
        }
    }
}

#[cfg(target_os = "macos")]
fn apply_start_with_windows(enabled: bool) -> Result<(), String> {
    macos_autostart::apply(enabled)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn apply_start_with_windows(_enabled: bool) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn test_dir(label: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "onlineclass-desktop-preferences-{label}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn defaults_keep_background_safety_enabled() {
        let directory = test_dir("defaults");
        let _ = fs::remove_dir_all(&directory);
        let store = DesktopPreferencesStore::open(&directory);
        let value = store.snapshot();
        assert_eq!(value.start_with_windows, !cfg!(target_os = "macos"));
        assert!(value.keep_running_on_close);
        fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn preference_changes_persist_and_invalid_keys_fail() {
        let directory = test_dir("persist");
        let _ = fs::remove_dir_all(&directory);
        let store = DesktopPreferencesStore::open(&directory);
        let changed = store
            .set("keepRunningOnClose", false)
            .expect("save close preference");
        assert!(!changed.keep_running_on_close);
        let persisted: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(directory.join(PREFERENCES_FILE_NAME)).expect("read preferences"),
        )
        .expect("parse preferences");
        assert_eq!(persisted.as_object().expect("preference object").len(), 2);
        assert_eq!(
            persisted.get("startWithWindows"),
            Some(&serde_json::Value::Bool(!cfg!(target_os = "macos")))
        );
        assert_eq!(
            persisted.get("keepRunningOnClose"),
            Some(&serde_json::Value::Bool(false))
        );
        assert_eq!(
            store.set("unknown", false).unwrap_err(),
            "desktop_preference_key_invalid"
        );
        assert!(
            !DesktopPreferencesStore::open(&directory)
                .snapshot()
                .keep_running_on_close
        );
        fs::remove_dir_all(&directory).expect("remove test directory");
    }

    #[test]
    fn registration_failure_preserves_preferences_without_real_os_changes() {
        let directory = test_dir("registration-failure");
        let store = DesktopPreferencesStore::open(&directory);
        let before = store.snapshot().start_with_windows;
        assert_eq!(
            store
                .set_with_autostart("startWithWindows", !before, |_| Err(
                    "injected_failure".into()
                ))
                .unwrap_err(),
            "injected_failure"
        );
        assert_eq!(store.snapshot().start_with_windows, before);
        assert!(!directory.join(PREFERENCES_FILE_NAME).exists());
        assert_eq!(store.startup_error().as_deref(), Some("injected_failure"));
        store
            .set_with_autostart("startWithWindows", before, |_| Ok(()))
            .unwrap();
        assert!(store.startup_error().is_none());
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn persistence_and_rollback_failures_remain_visible_to_later_reads() {
        use std::cell::Cell;
        for rollback_fails in [false, true] {
            let directory = test_dir(if rollback_fails {
                "rollback-failure"
            } else {
                "persist-failure"
            });
            fs::create_dir_all(directory.join(PREFERENCES_FILE_NAME)).unwrap();
            let store = DesktopPreferencesStore::open(&directory);
            let before = store.snapshot().start_with_windows;
            let calls = Cell::new(0);
            let error = store
                .set_with_autostart("startWithWindows", !before, |enabled| {
                    calls.set(calls.get() + 1);
                    if calls.get() == 1 {
                        assert_eq!(enabled, !before);
                        Ok(())
                    } else {
                        assert_eq!(enabled, before);
                        if rollback_fails {
                            Err("injected_rollback_failure".into())
                        } else {
                            Ok(())
                        }
                    }
                })
                .unwrap_err();
            assert!(error.starts_with("desktop_preferences_write_failed"));
            assert_eq!(error.contains("autostart_rollback_failed"), rollback_fails);
            assert_eq!(store.startup_error().as_deref(), Some(error.as_str()));
            assert_eq!(store.snapshot().start_with_windows, before);
            assert_eq!(calls.get(), 2);
            fs::remove_dir_all(&directory).unwrap();
        }
    }

    #[test]
    fn existing_json_choice_survives_platform_default_change() {
        for enabled in [true, false] {
            let value: DesktopPreferences = serde_json::from_str(&format!(
                "{{\"startWithWindows\":{enabled},\"keepRunningOnClose\":false}}"
            ))
            .unwrap();
            assert_eq!(value.start_with_windows, enabled);
            assert!(!value.keep_running_on_close);
        }
    }
}
