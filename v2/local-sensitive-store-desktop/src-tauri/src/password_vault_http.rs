use super::*;
use tiny_http::{Method, Request};
use url::Url;

fn body(request: &mut Request) -> Result<Value, String> {
    crate::read_body(request)
}

pub(crate) fn handle_http(
    request: &mut Request,
    store: &SqliteStore,
    principal: &BrowserLinkToken,
    url: &Url,
) -> Result<(u16, Value), String> {
    let path = url.path();
    let data = if request.method() == &Method::Get && path == "/v1/password-vault/personal/status" {
        let school = school_code(Some(&Value::String(crate::query(url, "schoolCode"))))?;
        personal_status(store, principal, &school)?
    } else if request.method() == &Method::Post && path == "/v1/password-vault/personal/setup" {
        setup_personal(store, principal, &body(request)?)?
    } else if request.method() == &Method::Post && path == "/v1/password-vault/personal/recovery" {
        recover_personal(store, principal, &body(request)?)?
    } else if request.method() == &Method::Get && path == "/v1/password-vault/personal/entries" {
        let school = school_code(Some(&Value::String(crate::query(url, "schoolCode"))))?;
        list_personal_entries(store, principal, &school)?
    } else if request.method() == &Method::Put && path == "/v1/password-vault/personal/entries" {
        save_personal_entry(store, principal, &body(request)?)?
    } else if request.method() == &Method::Post && path == "/v1/password-vault/personal/reveal" {
        reveal_personal_entry(store, principal, &body(request)?)?
    } else if request.method() == &Method::Delete
        && path.starts_with("/v1/password-vault/personal/entries/")
    {
        let entry_id = path.trim_start_matches("/v1/password-vault/personal/entries/");
        if entry_id.contains('/') {
            return Err("password_vault_entry_id_invalid".to_string());
        }
        delete_personal_entry(store, principal, &body(request)?, entry_id)?
    } else if request.method() == &Method::Get && path == "/v1/password-vault/shared/device" {
        let school = school_code(Some(&Value::String(crate::query(url, "schoolCode"))))?;
        let current = device_status(store, principal, &school)?;
        if current.get("status").and_then(Value::as_str) == Some("missing") {
            ensure_device(store, principal, &school, false)?
        } else {
            current
        }
    } else if request.method() == &Method::Post && path == "/v1/password-vault/shared/bootstrap" {
        bootstrap_shared(store, principal, &body(request)?)?
    } else if request.method() == &Method::Post
        && path == "/v1/password-vault/shared/approve-device"
    {
        approve_device(store, principal, &body(request)?)?
    } else if request.method() == &Method::Post
        && path == "/v1/password-vault/shared/accept-envelope"
    {
        accept_envelope(store, principal, &body(request)?)?
    } else if request.method() == &Method::Post && path == "/v1/password-vault/shared/encrypt" {
        encrypt_shared(store, principal, &body(request)?)?
    } else if request.method() == &Method::Post && path == "/v1/password-vault/shared/decrypt" {
        decrypt_shared(store, principal, &body(request)?)?
    } else if request.method() == &Method::Post && path == "/v1/password-vault/shared/recover" {
        recover_shared(store, principal, &body(request)?)?
    } else {
        return Ok((
            404,
            json!({ "ok": false, "error": "password_vault_route_not_found" }),
        ));
    };
    Ok((200, json!({ "ok": true, "data": data })))
}
