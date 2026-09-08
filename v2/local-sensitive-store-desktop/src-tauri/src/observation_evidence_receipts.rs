use crate::{now_ms, SqliteStore};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use p256::ecdsa::{signature::Verifier, Signature, VerifyingKey};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;

pub(crate) fn trusted_keys() -> Result<Value, String> {
    let response: Value = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(Duration::from_secs(15))
        .build()
        .get("https://t.classaimate.com/api/v3/observation-evidence/keys")
        .call()
        .map_err(|_| "observation_receipt_keys_unavailable")?
        .into_json()
        .map_err(|_| "observation_receipt_keys_invalid")?;
    Ok(response["data"]["keys"].clone())
}
pub(crate) fn verify(
    receipt: &Value,
    tenant: &str,
    commitment: &str,
    keys: &Value,
) -> Result<(), String> {
    if receipt["algorithm"] != "ES256" {
        return Err("observation_receipt_algorithm_invalid".into());
    }
    let key = keys
        .as_array()
        .and_then(|keys| {
            keys.iter()
                .find(|k| k["keyId"] == receipt["keyId"] && k["algorithm"] == "ES256")
        })
        .ok_or("observation_receipt_untrusted_key")?;
    let jwk = &key["publicKeyJwk"];
    if jwk["kty"] != "EC" || jwk["crv"] != "P-256" || jwk != &receipt["publicKeyJwk"] {
        return Err("observation_receipt_key_invalid".into());
    }
    let thumb = json!({"crv":jwk["crv"],"kty":jwk["kty"],"x":jwk["x"],"y":jwk["y"]});
    if URL_SAFE_NO_PAD.encode(Sha256::digest(thumb.to_string().as_bytes()))
        != receipt["keyId"].as_str().unwrap_or_default()
    {
        return Err("observation_receipt_key_invalid".into());
    }
    let decode = |value: &Value| {
        URL_SAFE_NO_PAD
            .decode(value.as_str().unwrap_or_default())
            .map_err(|_| "observation_receipt_encoding_invalid".to_string())
    };
    let mut point = vec![4u8];
    let x = decode(&jwk["x"])?;
    let y = decode(&jwk["y"])?;
    if x.len() != 32 || y.len() != 32 {
        return Err("observation_receipt_key_invalid".into());
    }
    point.extend(x);
    point.extend(y);
    let verifier =
        VerifyingKey::from_sec1_bytes(&point).map_err(|_| "observation_receipt_key_invalid")?;
    let bytes = decode(&receipt["payloadBase64url"])?;
    let payload: Value =
        serde_json::from_slice(&bytes).map_err(|_| "observation_receipt_payload_invalid")?;
    if payload != receipt["payload"]
        || payload["tenantId"] != tenant
        || payload["commitmentSha256"] != commitment
        || payload["version"] != 1
        || payload["receivedAtMs"].as_i64().unwrap_or(0) <= 0
        || payload["actorUserId"]
            .as_str()
            .unwrap_or_default()
            .is_empty()
    {
        return Err("observation_receipt_payload_mismatch".into());
    }
    let signature = Signature::from_slice(&decode(&receipt["signatureBase64url"])?)
        .map_err(|_| "observation_receipt_signature_invalid")?;
    verifier
        .verify(&bytes, &signature)
        .map_err(|_| "observation_receipt_signature_invalid".into())
}
impl SqliteStore {
    pub(crate) fn evidence_inventory(&self, tenant: &str) -> Result<Value, String> {
        let conn = self.conn.lock().map_err(|_| "db_lock_failed")?;
        let mut stmt=conn.prepare("SELECT receipt_id,commitment_sha256 FROM observation_evidence_batches WHERE tenant_id=?1 ORDER BY created_at_ms,receipt_id").map_err(|e|e.to_string())?;
        let entries=stmt.query_map(params![tenant],|r|Ok(json!({"receiptId":r.get::<_,String>(0)?,"commitmentSha256":r.get::<_,String>(1)?}))).map_err(|e|e.to_string())?.collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())?;
        Ok(json!({"ok":true,"entries":entries}))
    }
    pub(crate) fn evidence_reconcile(
        &self,
        tenant: &str,
        receipts: &Value,
    ) -> Result<Value, String> {
        let keys = trusted_keys()?;
        for receipt in receipts.as_array().ok_or("observation_receipts_required")? {
            verify(
                receipt,
                tenant,
                receipt["payload"]["commitmentSha256"]
                    .as_str()
                    .ok_or("observation_receipt_payload_invalid")?,
                &keys,
            )?;
        }
        let inventory = self.evidence_inventory(tenant)?;
        let entries = inventory["entries"].as_array().unwrap();
        let conn = self.conn.lock().map_err(|_| "db_lock_failed")?;
        let prior=conn.query_row("SELECT payload_json FROM observation_evidence_reconciliation WHERE tenant_id=?1 AND state_id='inventory'",params![tenant],|r|r.get::<_,String>(0)).optional().map_err(|e|e.to_string())?;
        let mut missing = prior
            .and_then(|r| serde_json::from_str::<Value>(&r).ok())
            .and_then(|r| r["missing"].as_array().cloned())
            .unwrap_or_default();
        for receipt in receipts.as_array().unwrap() {
            let entry = json!({"receiptId":receipt["payload"]["receiptId"],"commitmentSha256":receipt["payload"]["commitmentSha256"]});
            if !entries.contains(&entry) && !missing.contains(&entry) {
                missing.push(entry);
            }
        }
        missing.retain(|entry| !entries.contains(entry));
        let now = now_ms();
        let state = json!({"checkedAtMs":now,"missing":missing});
        conn.execute("INSERT INTO observation_evidence_reconciliation VALUES(?1,'inventory',?2,?3) ON CONFLICT(tenant_id,state_id) DO UPDATE SET payload_json=excluded.payload_json,created_at_ms=excluded.created_at_ms",params![tenant,state.to_string(),now]).map_err(|e|e.to_string())?;
        Ok(json!({"ok":true,"reconciliation":state}))
    }
    pub(crate) fn evidence_receipt(&self, tenant: &str, receipt: &Value) -> Result<Value, String> {
        let id = receipt["payload"]["receiptId"]
            .as_str()
            .ok_or("observation_receipt_id_required")?;
        let commitment = {
            let conn = self.conn.lock().map_err(|_| "db_lock_failed")?;
            conn.query_row("SELECT commitment_sha256 FROM observation_evidence_batches WHERE tenant_id=?1 AND receipt_id=?2",params![tenant,id],|r|r.get::<_,String>(0)).optional().map_err(|e|e.to_string())?.ok_or("observation_receipt_batch_missing")?
        };
        let keys = trusted_keys()?;
        verify(receipt, tenant, &commitment, &keys)?;
        let conn = self.conn.lock().map_err(|_| "db_lock_failed")?;
        let prior=conn.query_row("SELECT payload_json FROM observation_evidence_receipts WHERE tenant_id=?1 AND receipt_id=?2",params![tenant,id],|r|r.get::<_,String>(0)).optional().map_err(|e|e.to_string())?;
        if let Some(raw) = prior {
            if serde_json::from_str::<Value>(&raw).ok().as_ref() != Some(receipt) {
                return Err("observation_receipt_conflict".into());
            }
        } else {
            conn.execute(
                "INSERT INTO observation_evidence_receipts VALUES(?1,?2,?3,?4)",
                params![tenant, id, receipt.to_string(), now_ms()],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(json!({"ok":true,"receipt":receipt}))
    }
}
