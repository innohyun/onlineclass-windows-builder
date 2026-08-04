use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
            start_with_windows: true,
            keep_running_on_close: true,
        }
    }
}

pub struct DesktopPreferencesStore {
    path: PathBuf,
    value: Mutex<DesktopPreferences>,
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
        }
    }

    pub fn snapshot(&self) -> DesktopPreferences {
        self.value
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default()
    }

    pub fn apply_startup_setting(&self) -> Result<(), String> {
        apply_start_with_windows(self.snapshot().start_with_windows)
    }

    pub fn set(&self, key: &str, enabled: bool) -> Result<DesktopPreferences, String> {
        let previous = self.snapshot();
        let mut next = previous.clone();
        match key {
            "startWithWindows" => next.start_with_windows = enabled,
            "keepRunningOnClose" => next.keep_running_on_close = enabled,
            _ => return Err("desktop_preference_key_invalid".to_string()),
        }

        if key == "startWithWindows" {
            apply_start_with_windows(enabled)?;
        }
        if let Err(error) = self.persist(&next) {
            if key == "startWithWindows" {
                let _ = apply_start_with_windows(previous.start_with_windows);
            }
            return Err(error);
        }
        let mut value = self
            .value
            .lock()
            .map_err(|_| "desktop_preferences_lock_failed".to_string())?;
        *value = next.clone();
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

#[cfg(not(windows))]
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
        assert!(value.start_with_windows);
        assert!(value.keep_running_on_close);
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
            Some(&serde_json::Value::Bool(true))
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
}
