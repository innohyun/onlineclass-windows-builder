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
        let origin = personal_scope::resolve(store, principal, &school)?;
        personal_status(store, &origin, &school)?
    } else if request.method() == &Method::Post && path == "/v1/password-vault/personal/setup" {
        let input = body(request)?;
        let school = school_code(input.get("schoolCode"))?;
        let origin = personal_scope::resolve(store, principal, &school)?;
        let data = setup_personal(store, &origin, &input)?;
        crate::backup::mark_external_sync_dirty(store, &origin.tenant_id)?;
        data
    } else if request.method() == &Method::Post && path == "/v1/password-vault/personal/recovery" {
        let input = body(request)?;
        let school = school_code(input.get("schoolCode"))?;
        let origin = personal_scope::resolve(store, principal, &school)?;
        recover_personal(store, &origin, &input)?
    } else if request.method() == &Method::Get && path == "/v1/password-vault/personal/entries" {
        let school = school_code(Some(&Value::String(crate::query(url, "schoolCode"))))?;
        let origin = personal_scope::resolve(store, principal, &school)?;
        list_personal_entries(store, &origin, &school)?
    } else if request.method() == &Method::Put && path == "/v1/password-vault/personal/entries" {
        let input = body(request)?;
        let school = school_code(input.get("schoolCode"))?;
        let origin = personal_scope::resolve(store, principal, &school)?;
        let data = save_personal_entry(store, &origin, &input)?;
        crate::backup::mark_external_sync_dirty(store, &origin.tenant_id)?;
        data
    } else if request.method() == &Method::Post && path == "/v1/password-vault/personal/reveal" {
        let input = body(request)?;
        let school = school_code(input.get("schoolCode"))?;
        let origin = personal_scope::resolve(store, principal, &school)?;
        reveal_personal_entry(store, &origin, &input)?
    } else if request.method() == &Method::Delete
        && path.starts_with("/v1/password-vault/personal/entries/")
    {
        let entry_id = path.trim_start_matches("/v1/password-vault/personal/entries/");
        if entry_id.contains('/') {
            return Err("password_vault_entry_id_invalid".to_string());
        }
        let input = body(request)?;
        let school = school_code(input.get("schoolCode"))?;
        let origin = personal_scope::resolve(store, principal, &school)?;
        let data = delete_personal_entry(store, &origin, &input, entry_id)?;
        crate::backup::mark_external_sync_dirty(store, &origin.tenant_id)?;
        data
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
