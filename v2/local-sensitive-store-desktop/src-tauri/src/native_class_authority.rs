//! Native commands receive a class locator, never class authority. Resolve the
//! registered local identity before opening or migrating a physical class file.
use crate::{AppState, SqliteStore};
use serde_json::Value;
use std::sync::Arc;

const SESSION_REQUIRED: &str = "device_sync_session_required";
const IDENTITY_MISMATCH: &str = "tenant_scope_mismatch";

fn registered_identity<'a>(registry: &'a Value, tenant: &str) -> Result<&'a Value, String> {
    if tenant.is_empty()
        || tenant.len() > 128
        || !tenant
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
        || registry["ok"] != true
    {
        return Err(SESSION_REQUIRED.into());
    }
    let classes = registry["classes"].as_array().ok_or(SESSION_REQUIRED)?;
    let mut matching = classes.iter().filter(|class| class["tenantId"] == tenant);
    let class = matching.next().ok_or(SESSION_REQUIRED)?;
    if matching.next().is_some()
        || ["uid", "deviceId"].iter().any(|key| {
            !class[*key]
                .as_str()
                .is_some_and(|value| !value.trim().is_empty() && value.len() <= 200)
        })
    {
        return Err(SESSION_REQUIRED.into());
    }
    Ok(class)
}

fn resolve_registered_store<T>(
    registry: &Value,
    tenant: &str,
    read_identity: impl FnOnce() -> Result<Value, String>,
    open_store: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    // A caller-provided locator cannot reach credential or file resolution
    // before it is matched to an exact approved local registration.
    let registered = registered_identity(registry, tenant)?;
    let identity = read_identity()?;
    if identity["tenantId"] != tenant
        || identity["actorId"] != registered["uid"]
        || identity["deviceId"] != registered["deviceId"]
    {
        return Err(IDENTITY_MISMATCH.into());
    }
    open_store()
}

/// Control operations such as disconnect keep working while class recovery is
/// blocked. This verifies cached local authority without opening any class file.
pub(crate) fn authorize_native_tenant(state: &AppState, tenant: &str) -> Result<(), String> {
    let manager = state
        .device_sync_manager
        .lock()
        .map_err(|_| "device_sync_unavailable")?
        .clone()
        .ok_or("device_sync_unavailable")?;
    let registry = manager.sessions()?;
    resolve_registered_store(
        &registry,
        tenant,
        // Uses the existing cached local session/keyring, with no network call.
        || manager.native_class_identity_for_tenant(tenant),
        || Ok(()),
    )
}

pub(crate) fn for_native_tenant(
    state: &AppState,
    tenant: &str,
) -> Result<Arc<SqliteStore>, String> {
    authorize_native_tenant(state, tenant)?;
    let root = state
        .store
        .lock()
        .map_err(|_| "db_lock_failed")?
        .clone()
        .ok_or("local_store_unavailable")?;
    root.for_tenant(tenant)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::Cell;

    fn registry() -> Value {
        json!({"ok":true,"selectedTenantId":"class-2027","classes":[
            {"tenantId":"class-2026","uid":"teacher-a","deviceId":"device-2026"},
            {"tenantId":"class-2027","uid":"teacher-a","deviceId":"device-2027"}
        ]})
    }

    #[test]
    fn unknown_class_cannot_call_identity_getter_or_create_a_store() {
        let identities = Cell::new(0);
        let files = Cell::new(0);
        for tenant in ["class-unapproved", "../class-2026", "", "class-2026 "] {
            let result = resolve_registered_store(
                &registry(),
                tenant,
                || {
                    identities.set(identities.get() + 1);
                    Ok(json!({}))
                },
                || {
                    files.set(files.get() + 1);
                    Ok(())
                },
            );
            assert_eq!(result.unwrap_err(), SESSION_REQUIRED);
        }
        assert_eq!(identities.get(), 0);
        assert_eq!(files.get(), 0);
    }

    #[test]
    fn registered_class_requires_the_same_actor_and_device_credential() {
        let files = Cell::new(0);
        for identity in [
            json!({"tenantId":"class-2027","actorId":"teacher-a","deviceId":"device-2026"}),
            json!({"tenantId":"class-2026","actorId":"teacher-b","deviceId":"device-2026"}),
            json!({"tenantId":"class-2026","actorId":"teacher-a","deviceId":"device-2027"}),
            json!({"tenantId":"class-2026","actorId":"teacher-a"}),
        ] {
            assert_eq!(
                resolve_registered_store(
                    &registry(),
                    "class-2026",
                    || Ok(identity),
                    || {
                        files.set(files.get() + 1);
                        Ok(())
                    }
                )
                .unwrap_err(),
                IDENTITY_MISMATCH
            );
        }
        assert_eq!(files.get(), 0);
    }

    #[test]
    fn missing_credential_and_failed_registry_do_not_open_a_store() {
        let files = Cell::new(0);
        assert_eq!(
            resolve_registered_store(
                &registry(),
                "class-2026",
                || { Err("device_sync_credential_missing".into()) },
                || {
                    files.set(files.get() + 1);
                    Ok(())
                }
            )
            .unwrap_err(),
            "device_sync_credential_missing"
        );
        assert_eq!(
            resolve_registered_store(
                &json!({"ok":false,"classes":registry()["classes"]}),
                "class-2026",
                || { panic!("untrusted registry must fail before identity access") },
                || {
                    files.set(files.get() + 1);
                    Ok(())
                }
            )
            .unwrap_err(),
            SESSION_REQUIRED
        );
        assert_eq!(files.get(), 0);
    }

    #[test]
    fn ambiguous_or_incomplete_registration_is_denied_before_resolution() {
        for classes in [
            json!([{ "tenantId":"class-2026", "uid":"teacher-a" }]),
            json!([{ "tenantId":"class-2026", "uid":" ", "deviceId":"device-2026" }]),
            json!([registry()["classes"][0], registry()["classes"][0]]),
        ] {
            assert_eq!(
                resolve_registered_store(
                    &json!({"ok":true,"classes":classes}),
                    "class-2026",
                    || { panic!("ambiguous registration must not resolve a file") },
                    || Ok(())
                )
                .unwrap_err(),
                SESSION_REQUIRED
            );
        }
    }

    #[test]
    fn selected_next_class_does_not_move_an_old_class_request() {
        let requested = "class-2026";
        let pinned = resolve_registered_store(
            &registry(),
            requested,
            || Ok(json!({"tenantId":requested,"actorId":"teacher-a","deviceId":"device-2026"})),
            || Ok(Arc::new(requested.to_owned())),
        )
        .unwrap();
        let mut switched = registry();
        switched["selectedTenantId"] = json!("class-2027");
        assert_eq!(pinned.as_str(), "class-2026");
        assert!(registered_identity(&switched, requested).is_ok());
    }
}
