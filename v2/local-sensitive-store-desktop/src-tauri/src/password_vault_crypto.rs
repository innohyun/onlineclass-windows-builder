use aes_gcm::{
    aead::{Aead, Payload},
    Aes256Gcm, KeyInit, Nonce,
};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hkdf::Hkdf;
use rand::{rngs::OsRng, RngCore};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

const ARGON_MEMORY_KIB: u32 = 19_456;
const ARGON_ITERATIONS: u32 = 2;
const ARGON_LANES: u32 = 1;

pub(crate) fn random_key() -> [u8; 32] {
    let mut key = [0_u8; 32];
    OsRng.fill_bytes(&mut key);
    key
}

pub(crate) fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(random_key())
}

pub(crate) fn encode_key(key: &[u8; 32]) -> String {
    URL_SAFE_NO_PAD.encode(key)
}

pub(crate) fn decode_key(value: &str, error: &str) -> Result<[u8; 32], String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| error.to_string())?;
    bytes.try_into().map_err(|_| error.to_string())
}

pub(crate) fn sha256_hex(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value.as_ref()))
}

pub(crate) fn aad(parts: &[&str]) -> Vec<u8> {
    serde_json::to_vec(parts).unwrap_or_default()
}

pub(crate) fn encrypt_json(
    key: &[u8; 32],
    aad: &[u8],
    plaintext: &Value,
) -> Result<String, String> {
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|_| "password_vault_key_invalid".to_string())?;
    let mut nonce = [0_u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let raw = serde_json::to_vec(plaintext)
        .map_err(|_| "password_vault_plaintext_invalid".to_string())?;
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: &raw, aad })
        .map_err(|_| "password_vault_encrypt_failed".to_string())?;
    serde_json::to_string(&json!({
        "alg": "A256GCM",
        "ciphertext": URL_SAFE_NO_PAD.encode(encrypted),
        "nonce": URL_SAFE_NO_PAD.encode(nonce),
        "v": 1
    }))
    .map_err(|_| "password_vault_ciphertext_encode_failed".to_string())
}

pub(crate) fn decrypt_json(key: &[u8; 32], aad: &[u8], encoded: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(encoded)
        .map_err(|_| "password_vault_ciphertext_invalid".to_string())?;
    if value.get("v").and_then(Value::as_i64) != Some(1)
        || value.get("alg").and_then(Value::as_str) != Some("A256GCM")
    {
        return Err("password_vault_ciphertext_invalid".to_string());
    }
    let nonce = URL_SAFE_NO_PAD
        .decode(value.get("nonce").and_then(Value::as_str).unwrap_or(""))
        .map_err(|_| "password_vault_ciphertext_invalid".to_string())?;
    let nonce: [u8; 12] = nonce
        .try_into()
        .map_err(|_| "password_vault_ciphertext_invalid".to_string())?;
    let ciphertext = URL_SAFE_NO_PAD
        .decode(
            value
                .get("ciphertext")
                .and_then(Value::as_str)
                .unwrap_or(""),
        )
        .map_err(|_| "password_vault_ciphertext_invalid".to_string())?;
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|_| "password_vault_key_invalid".to_string())?;
    let mut plaintext = cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad,
            },
        )
        .map_err(|_| "password_vault_decrypt_failed".to_string())?;
    let decoded = serde_json::from_slice(&plaintext)
        .map_err(|_| "password_vault_plaintext_invalid".to_string());
    plaintext.zeroize();
    decoded
}

pub(crate) fn generate_device_keypair() -> (String, String) {
    let secret = StaticSecret::random_from_rng(OsRng);
    let public = PublicKey::from(&secret);
    (
        URL_SAFE_NO_PAD.encode(secret.to_bytes()),
        URL_SAFE_NO_PAD.encode(public.as_bytes()),
    )
}

fn envelope_key(shared: &[u8; 32], aad: &[u8]) -> Result<[u8; 32], String> {
    let hkdf = Hkdf::<Sha256>::new(Some(b"ClassAiMate school password vault v1"), shared);
    let mut result = [0_u8; 32];
    hkdf.expand(aad, &mut result)
        .map_err(|_| "password_vault_envelope_key_failed".to_string())?;
    Ok(result)
}

pub(crate) fn wrap_key(
    recipient_public: &str,
    raw_key: &[u8; 32],
    aad: &[u8],
) -> Result<String, String> {
    let public = decode_key(recipient_public, "password_vault_public_key_invalid")?;
    let recipient = PublicKey::from(public);
    let ephemeral = StaticSecret::random_from_rng(OsRng);
    let ephemeral_public = PublicKey::from(&ephemeral);
    let shared = ephemeral.diffie_hellman(&recipient).to_bytes();
    let mut wrapping = envelope_key(&shared, aad)?;
    let cipher = Aes256Gcm::new_from_slice(&wrapping)
        .map_err(|_| "password_vault_envelope_key_failed".to_string())?;
    let mut nonce = [0_u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: raw_key, aad })
        .map_err(|_| "password_vault_envelope_encrypt_failed".to_string())?;
    wrapping.zeroize();
    serde_json::to_string(&json!({
        "alg": "X25519-HKDF-SHA256+A256GCM",
        "ciphertext": URL_SAFE_NO_PAD.encode(encrypted),
        "ephemeralPublicKey": URL_SAFE_NO_PAD.encode(ephemeral_public.as_bytes()),
        "nonce": URL_SAFE_NO_PAD.encode(nonce),
        "v": 1
    }))
    .map_err(|_| "password_vault_envelope_encode_failed".to_string())
}

pub(crate) fn unwrap_key(private_key: &str, encoded: &str, aad: &[u8]) -> Result<[u8; 32], String> {
    let private = decode_key(private_key, "password_vault_private_key_invalid")?;
    let value: Value =
        serde_json::from_str(encoded).map_err(|_| "password_vault_envelope_invalid".to_string())?;
    if value.get("v").and_then(Value::as_i64) != Some(1)
        || value.get("alg").and_then(Value::as_str) != Some("X25519-HKDF-SHA256+A256GCM")
    {
        return Err("password_vault_envelope_invalid".to_string());
    }
    let ephemeral = decode_key(
        value
            .get("ephemeralPublicKey")
            .and_then(Value::as_str)
            .unwrap_or(""),
        "password_vault_envelope_invalid",
    )?;
    let nonce = URL_SAFE_NO_PAD
        .decode(value.get("nonce").and_then(Value::as_str).unwrap_or(""))
        .map_err(|_| "password_vault_envelope_invalid".to_string())?;
    let nonce: [u8; 12] = nonce
        .try_into()
        .map_err(|_| "password_vault_envelope_invalid".to_string())?;
    let ciphertext = URL_SAFE_NO_PAD
        .decode(
            value
                .get("ciphertext")
                .and_then(Value::as_str)
                .unwrap_or(""),
        )
        .map_err(|_| "password_vault_envelope_invalid".to_string())?;
    let secret = StaticSecret::from(private);
    let shared = secret
        .diffie_hellman(&PublicKey::from(ephemeral))
        .to_bytes();
    let mut wrapping = envelope_key(&shared, aad)?;
    let cipher = Aes256Gcm::new_from_slice(&wrapping)
        .map_err(|_| "password_vault_envelope_key_failed".to_string())?;
    let mut plaintext = cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad,
            },
        )
        .map_err(|_| "password_vault_envelope_decrypt_failed".to_string())?;
    wrapping.zeroize();
    let result: Result<[u8; 32], String> = plaintext
        .as_slice()
        .try_into()
        .map_err(|_| "password_vault_envelope_invalid".to_string());
    plaintext.zeroize();
    result
}

fn recovery_key(passphrase: &str, salt: &[u8]) -> Result<[u8; 32], String> {
    if passphrase.chars().count() < 12 || passphrase.chars().count() > 256 {
        return Err("password_vault_recovery_passphrase_invalid".to_string());
    }
    let params = Params::new(ARGON_MEMORY_KIB, ARGON_ITERATIONS, ARGON_LANES, Some(32))
        .map_err(|_| "password_vault_argon_params_invalid".to_string())?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0_u8; 32];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|_| "password_vault_recovery_key_failed".to_string())?;
    Ok(key)
}

pub(crate) fn wrap_personal_key(
    passphrase: &str,
    key: &[u8; 32],
    aad: &[u8],
) -> Result<String, String> {
    let mut salt = [0_u8; 16];
    OsRng.fill_bytes(&mut salt);
    let mut wrapping = recovery_key(passphrase, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&wrapping)
        .map_err(|_| "password_vault_recovery_key_failed".to_string())?;
    let mut nonce = [0_u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: key, aad })
        .map_err(|_| "password_vault_recovery_wrap_failed".to_string())?;
    wrapping.zeroize();
    serde_json::to_string(&json!({
        "alg": "ARGON2ID+A256GCM",
        "ciphertext": URL_SAFE_NO_PAD.encode(encrypted),
        "m": ARGON_MEMORY_KIB,
        "nonce": URL_SAFE_NO_PAD.encode(nonce),
        "p": ARGON_LANES,
        "salt": URL_SAFE_NO_PAD.encode(salt),
        "t": ARGON_ITERATIONS,
        "v": 1
    }))
    .map_err(|_| "password_vault_recovery_wrap_encode_failed".to_string())
}

pub(crate) fn unwrap_personal_key(
    passphrase: &str,
    encoded: &str,
    aad: &[u8],
) -> Result<[u8; 32], String> {
    let value: Value = serde_json::from_str(encoded)
        .map_err(|_| "password_vault_recovery_metadata_invalid".to_string())?;
    if value.get("v").and_then(Value::as_i64) != Some(1)
        || value.get("alg").and_then(Value::as_str) != Some("ARGON2ID+A256GCM")
        || value.get("m").and_then(Value::as_u64) != Some(ARGON_MEMORY_KIB as u64)
        || value.get("t").and_then(Value::as_u64) != Some(ARGON_ITERATIONS as u64)
        || value.get("p").and_then(Value::as_u64) != Some(ARGON_LANES as u64)
    {
        return Err("password_vault_recovery_metadata_invalid".to_string());
    }
    let salt = URL_SAFE_NO_PAD
        .decode(value.get("salt").and_then(Value::as_str).unwrap_or(""))
        .map_err(|_| "password_vault_recovery_metadata_invalid".to_string())?;
    let nonce = URL_SAFE_NO_PAD
        .decode(value.get("nonce").and_then(Value::as_str).unwrap_or(""))
        .map_err(|_| "password_vault_recovery_metadata_invalid".to_string())?;
    let nonce: [u8; 12] = nonce
        .try_into()
        .map_err(|_| "password_vault_recovery_metadata_invalid".to_string())?;
    let ciphertext = URL_SAFE_NO_PAD
        .decode(
            value
                .get("ciphertext")
                .and_then(Value::as_str)
                .unwrap_or(""),
        )
        .map_err(|_| "password_vault_recovery_metadata_invalid".to_string())?;
    let mut wrapping = recovery_key(passphrase, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&wrapping)
        .map_err(|_| "password_vault_recovery_key_failed".to_string())?;
    let mut plaintext = cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad,
            },
        )
        .map_err(|_| "password_vault_recovery_passphrase_mismatch".to_string())?;
    wrapping.zeroize();
    let result: Result<[u8; 32], String> = plaintext
        .as_slice()
        .try_into()
        .map_err(|_| "password_vault_recovery_metadata_invalid".to_string());
    plaintext.zeroize();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aes_gcm_binds_ciphertext_to_exact_aad_and_detects_tamper() {
        let key = random_key();
        let first = aad(&["shared", "1234567", "1", "entry", "entry-1", "1"]);
        let second = aad(&["shared", "1234567", "1", "entry", "entry-2", "1"]);
        let encrypted = encrypt_json(&key, &first, &json!({ "password": "secret" })).unwrap();
        assert_eq!(
            decrypt_json(&key, &first, &encrypted).unwrap()["password"],
            "secret"
        );
        assert_eq!(
            decrypt_json(&key, &second, &encrypted).unwrap_err(),
            "password_vault_decrypt_failed"
        );
        let tampered = encrypted.replacen('A', "B", 1);
        assert!(decrypt_json(&key, &first, &tampered).is_err());
    }

    #[test]
    fn argon_recovery_requires_exact_passphrase() {
        let key = random_key();
        let binding = aad(&["personal", "tenant-a", "teacher-a", "1234567"]);
        let wrapped = wrap_personal_key("아주 안전한 복구 암호 1234", &key, &binding).unwrap();
        assert_eq!(
            unwrap_personal_key("아주 안전한 복구 암호 1234", &wrapped, &binding).unwrap(),
            key
        );
        assert_eq!(
            unwrap_personal_key("다른 안전한 복구 암호 5678", &wrapped, &binding).unwrap_err(),
            "password_vault_recovery_passphrase_mismatch"
        );
    }

    #[test]
    fn x25519_envelope_opens_only_with_target_private_key() {
        let (private, public) = generate_device_keypair();
        let (other_private, _) = generate_device_keypair();
        let school_key = random_key();
        let binding = aad(&["envelope", "1234567", "1", "device-a", &public]);
        let envelope = wrap_key(&public, &school_key, &binding).unwrap();
        assert_eq!(
            unwrap_key(&private, &envelope, &binding).unwrap(),
            school_key
        );
        assert!(unwrap_key(&other_private, &envelope, &binding).is_err());
    }
}
