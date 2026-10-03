//! Resolve an existing owner's personal vault without rewriting its encryption
//! scope. Current class approval remains the authority supplied by the router;
//! this origin principal is used only by personal profile/entry operations.
use super::*;

pub(super) fn resolve(
    store: &SqliteStore,
    current: &BrowserLinkToken,
    school: &str,
) -> Result<BrowserLinkToken, String> {
    crate::class_store::checked_tenant(&current.tenant_id)?;
    opaque(Some(&Value::String(current.uid.clone())), "owner_uid", 1)?;
    let school = school_code(Some(&Value::String(school.to_owned())))?;
    if store.class_tenant.is_some() {
        return Err("password_vault_shared_store_required".into());
    }
    let conn = store.conn.lock().map_err(|_| "db_lock_failed")?;
    let mut statement = conn
        .prepare(
            "SELECT tenant_id FROM password_vault_personal_profiles
         WHERE owner_uid=?1 AND school_code=?2 ORDER BY tenant_id",
        )
        .map_err(|_| "password_vault_profile_read_failed")?;
    let origins = statement
        .query_map(params![current.uid, school], |row| row.get::<_, String>(0))
        .map_err(|_| "password_vault_profile_read_failed")?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "password_vault_profile_read_failed")?;
    let tenant = if origins.iter().any(|tenant| tenant == &current.tenant_id) {
        current.tenant_id.clone()
    } else {
        match origins.as_slice() {
            [] => current.tenant_id.clone(),
            [tenant] => tenant.clone(),
            _ => return Err("password_vault_personal_origin_ambiguous".into()),
        }
    };
    crate::class_store::checked_tenant(&tenant)?;
    let mut origin = current.clone();
    origin.tenant_id = tenant;
    Ok(origin)
}

#[cfg(test)]
#[path = "password_vault_personal_scope_tests.rs"]
mod tests;
