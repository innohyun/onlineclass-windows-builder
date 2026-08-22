use keyring::Entry;
use std::path::PathBuf;

#[cfg(target_os = "windows")]
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
#[cfg(target_os = "windows")]
use chrono::Utc;
#[cfg(target_os = "windows")]
use serde::{Deserialize, Serialize};
#[cfg(target_os = "windows")]
use std::fs;

const KEYRING_SERVICE: &str = "OnlineClassLocalSensitiveStore";
#[cfg(target_os = "windows")]
const CREDENTIAL_FILE_NAME: &str = "device-sync-credentials.json";

#[cfg(target_os = "windows")]
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
struct StoredCredential {
    account: String,
    protected_credential: String,
    updated_at_ms: i64,
}

#[cfg(target_os = "windows")]
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
struct CredentialFile {
    version: i64,
    updated_at_ms: i64,
    credentials: Vec<StoredCredential>,
}

pub(crate) struct DeviceSyncCredentialStore {
    #[cfg(target_os = "windows")]
    credential_path: PathBuf,
}

#[cfg(target_os = "windows")]
fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn keyring_entry(account: &str) -> Result<Entry, String> {
    Entry::new(KEYRING_SERVICE, account)
        .map_err(|error| format!("device_sync_keyring_entry_failed:{error}"))
}

fn valid_credential(value: &str) -> bool {
    value.len() == 43
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

#[cfg(target_os = "windows")]
#[link(name = "kernel32")]
extern "system" {
    fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}

#[cfg(target_os = "windows")]
fn protect_credential(credential: &str) -> Result<String, String> {
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};

    let bytes = credential.as_bytes();
    let mut input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &mut input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            0,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(format!(
            "device_sync_dpapi_protect_failed:{}",
            std::io::Error::last_os_error()
        ));
    }
    let encrypted =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData as _);
    }
    Ok(BASE64_STANDARD.encode(encrypted))
}

#[cfg(target_os = "windows")]
fn unprotect_credential(protected_credential: &str) -> Result<String, String> {
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    let encrypted = BASE64_STANDARD
        .decode(protected_credential)
        .map_err(|error| format!("device_sync_dpapi_decode_failed:{error}"))?;
    let mut input = CRYPT_INTEGER_BLOB {
        cbData: encrypted.len() as u32,
        pbData: encrypted.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &mut input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            0,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(format!(
            "device_sync_dpapi_unprotect_failed:{}",
            std::io::Error::last_os_error()
        ));
    }
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData as _);
    }
    String::from_utf8(bytes).map_err(|error| format!("device_sync_dpapi_utf8_failed:{error}"))
}

impl DeviceSyncCredentialStore {
    pub(crate) fn new(data_dir: PathBuf) -> Self {
        #[cfg(not(target_os = "windows"))]
        let _ = data_dir;
        Self {
            #[cfg(target_os = "windows")]
            credential_path: data_dir.join(CREDENTIAL_FILE_NAME),
        }
    }

    #[cfg(target_os = "windows")]
    fn load_file(&self) -> Result<CredentialFile, String> {
        if !self.credential_path.exists() {
            return Ok(CredentialFile::default());
        }
        let raw = fs::read_to_string(&self.credential_path)
            .map_err(|error| format!("device_sync_credential_file_read_failed:{error}"))?;
        serde_json::from_str(&raw)
            .map_err(|error| format!("device_sync_credential_file_decode_failed:{error}"))
    }

    #[cfg(target_os = "windows")]
    fn save_file(&self, credential_file: &CredentialFile) -> Result<(), String> {
        if let Some(parent) = self.credential_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("device_sync_credential_file_dir_failed:{error}"))?;
        }
        let raw = serde_json::to_string_pretty(credential_file)
            .map_err(|error| format!("device_sync_credential_file_encode_failed:{error}"))?;
        fs::write(&self.credential_path, format!("{raw}\n"))
            .map_err(|error| format!("device_sync_credential_file_write_failed:{error}"))
    }

    #[cfg(target_os = "windows")]
    fn save_fallback(&self, account: &str, credential: &str) -> Result<(), String> {
        let protected_credential = protect_credential(credential)?;
        let mut credential_file = self.load_file()?;
        credential_file
            .credentials
            .retain(|item| item.account != account);
        credential_file.credentials.push(StoredCredential {
            account: account.to_string(),
            protected_credential,
            updated_at_ms: now_ms(),
        });
        credential_file.version = 1;
        credential_file.updated_at_ms = now_ms();
        self.save_file(&credential_file)
    }

    #[cfg(target_os = "windows")]
    fn read_fallback(&self, account: &str) -> Result<String, String> {
        let credential_file = self.load_file()?;
        let protected = credential_file
            .credentials
            .iter()
            .find(|item| item.account == account)
            .map(|item| item.protected_credential.as_str())
            .unwrap_or_default();
        if protected.is_empty() {
            return Err("device_sync_fallback_credential_missing".to_string());
        }
        unprotect_credential(protected)
    }

    #[cfg(target_os = "windows")]
    fn delete_fallback(&self, account: &str) -> Result<(), String> {
        if !self.credential_path.exists() {
            return Ok(());
        }
        let mut credential_file = self.load_file()?;
        let before = credential_file.credentials.len();
        credential_file
            .credentials
            .retain(|item| item.account != account);
        if before == credential_file.credentials.len() {
            return Ok(());
        }
        credential_file.updated_at_ms = now_ms();
        self.save_file(&credential_file)
    }

    pub(crate) fn store(&self, account: &str, credential: &str) -> Result<String, String> {
        if !valid_credential(credential) {
            return Err("device_sync_credential_invalid".to_string());
        }
        let keyring_result = (|| -> Result<(), String> {
            let entry = keyring_entry(account)?;
            entry
                .set_password(credential)
                .map_err(|error| format!("device_sync_credential_set_failed:{error}"))?;
            let verified = entry
                .get_password()
                .map_err(|error| format!("device_sync_credential_get_failed:{error}"))?;
            if verified != credential {
                return Err("device_sync_credential_verify_failed".to_string());
            }
            Ok(())
        })();

        #[cfg(target_os = "windows")]
        {
            let fallback_result = self.save_fallback(account, credential).and_then(|_| {
                let verified = self.read_fallback(account)?;
                if verified != credential {
                    return Err("device_sync_fallback_credential_verify_failed".to_string());
                }
                Ok(())
            });
            if let Err(fallback_error) = fallback_result {
                if let Ok(entry) = keyring_entry(account) {
                    let _ = entry.delete_credential();
                }
                let keyring_error = keyring_result
                    .as_ref()
                    .err()
                    .cloned()
                    .unwrap_or_else(|| "device_sync_keyring_available".to_string());
                return Err(format!("{keyring_error};{fallback_error}"));
            }
            return Ok(if keyring_result.is_ok() {
                "keyring+windows_dpapi_file".to_string()
            } else {
                "windows_dpapi_file".to_string()
            });
        }

        #[cfg(not(target_os = "windows"))]
        {
            keyring_result?;
            #[cfg(target_os = "macos")]
            return Ok("macos_keychain".to_string());
            #[cfg(not(target_os = "macos"))]
            return Ok("os_keyring".to_string());
        }
    }

    pub(crate) fn read(&self, account: &str) -> Result<String, String> {
        let keyring_result = keyring_entry(account).and_then(|entry| {
            entry
                .get_password()
                .map_err(|error| format!("device_sync_credential_get_failed:{error}"))
        });
        if let Ok(credential) = keyring_result.as_ref() {
            if valid_credential(credential) {
                #[cfg(target_os = "windows")]
                if self.read_fallback(account).as_deref() != Ok(credential.as_str()) {
                    let _ = self.save_fallback(account, credential);
                }
                return Ok(credential.clone());
            }
        }

        #[cfg(target_os = "windows")]
        {
            let keyring_error = keyring_result
                .err()
                .unwrap_or_else(|| "device_sync_credential_invalid".to_string());
            let credential = self
                .read_fallback(account)
                .map_err(|fallback_error| format!("{keyring_error};{fallback_error}"))?;
            if !valid_credential(&credential) {
                return Err("device_sync_credential_invalid".to_string());
            }
            return Ok(credential);
        }

        #[cfg(not(target_os = "windows"))]
        {
            Err(keyring_result
                .err()
                .unwrap_or_else(|| "device_sync_credential_invalid".to_string()))
        }
    }

    pub(crate) fn delete(&self, account: &str) {
        if let Ok(entry) = keyring_entry(account) {
            let _ = entry.delete_credential();
        }
        #[cfg(target_os = "windows")]
        let _ = self.delete_fallback(account);
    }
}
