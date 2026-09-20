//! Explicit debug-only isolation for synthetic WebView2 QA. Validate before any
//! single-instance activation, OS preferences, service, or webview is initialized.
use std::path::{Component, Path, PathBuf};

const PRODUCTION_IDENTIFIER: &str = "com.onlineclass.local-sensitive-store";

#[derive(Debug)]
pub(crate) struct DesktopQaIsolation {
    active: bool,
}

impl DesktopQaIsolation {
    pub(crate) fn is_active(&self) -> bool {
        self.active
    }
}

pub(crate) fn from_environment(identifier: &str) -> Result<DesktopQaIsolation, String> {
    let requested = match std::env::var("ONLINECLASS_DESKTOP_QA") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(_) => return Err("desktop_qa_flag_invalid".into()),
    };
    let store = std::env::var_os("ONLINECLASS_LOCAL_STORE_DIR").map(PathBuf::from);
    let webview = std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").map(PathBuf::from);
    validate(
        requested.as_deref(),
        cfg!(debug_assertions),
        identifier,
        store.as_deref(),
        webview.as_deref(),
        &std::env::temp_dir(),
    )
}

fn isolated_directory(path: Option<&Path>, temp: &Path) -> Result<PathBuf, String> {
    let path = path.ok_or("desktop_qa_paths_required")?;
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("desktop_qa_path_invalid".into());
    }
    let resolved = std::fs::canonicalize(path).map_err(|_| "desktop_qa_directory_required")?;
    // Requiring existing children of TEMP also rejects a junction into the real
    // profile/store, even when its visible QA path is underneath TEMP.
    if !resolved.is_dir() || resolved == temp || !resolved.starts_with(temp) {
        return Err("desktop_qa_path_not_isolated".into());
    }
    Ok(resolved)
}

fn validate(
    requested: Option<&str>,
    debug_build: bool,
    identifier: &str,
    store: Option<&Path>,
    webview: Option<&Path>,
    temp: &Path,
) -> Result<DesktopQaIsolation, String> {
    let Some(requested) = requested else {
        return Ok(DesktopQaIsolation { active: false });
    };
    if requested != "1" {
        return Err("desktop_qa_flag_invalid".into());
    }
    if !debug_build {
        return Err("desktop_qa_debug_build_required".into());
    }
    if identifier.is_empty() || identifier.eq_ignore_ascii_case(PRODUCTION_IDENTIFIER) {
        return Err("desktop_qa_identifier_required".into());
    }
    let temp = std::fs::canonicalize(temp).map_err(|_| "desktop_qa_temp_directory_invalid")?;
    let store = isolated_directory(store, &temp)?;
    let webview = isolated_directory(webview, &temp)?;
    if store.starts_with(&webview) || webview.starts_with(&store) {
        return Err("desktop_qa_paths_overlap".into());
    }
    Ok(DesktopQaIsolation { active: true })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_qa_requires_debug_flag_identity_and_disjoint_existing_temp_directories() {
        let root = std::env::temp_dir().join(format!(
            "classaimate-qa-guard-{}",
            crate::random_url_token()
        ));
        let store = root.join("store");
        let profile = root.join("profile");
        let missing = root.join("missing");
        let traversal = store.join("../profile");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        let temp = std::env::temp_dir();
        let valid = |flag, debug, id, db, web| validate(flag, debug, id, db, web, &temp);
        assert!(valid(
            Some("1"),
            true,
            "com.onlineclass.local-sensitive-store.qa",
            Some(&store),
            Some(&profile)
        )
        .unwrap()
        .is_active());
        assert!(!valid(None, false, PRODUCTION_IDENTIFIER, None, None)
            .unwrap()
            .is_active());
        assert_eq!(
            valid(Some("1"), false, "qa", Some(&store), Some(&profile)).unwrap_err(),
            "desktop_qa_debug_build_required"
        );
        assert_eq!(
            valid(Some("0"), true, "qa", Some(&store), Some(&profile)).unwrap_err(),
            "desktop_qa_flag_invalid"
        );
        assert_eq!(
            valid(
                Some("1"),
                true,
                PRODUCTION_IDENTIFIER,
                Some(&store),
                Some(&profile)
            )
            .unwrap_err(),
            "desktop_qa_identifier_required"
        );
        assert_eq!(
            valid(Some("1"), true, "qa", None, Some(&profile)).unwrap_err(),
            "desktop_qa_paths_required"
        );
        assert_eq!(
            valid(
                Some("1"),
                true,
                "qa",
                Some(Path::new("relative")),
                Some(&profile)
            )
            .unwrap_err(),
            "desktop_qa_path_invalid"
        );
        assert_eq!(
            valid(
                Some("1"),
                true,
                "qa",
                Some(&missing),
                Some(&profile)
            )
            .unwrap_err(),
            "desktop_qa_directory_required"
        );
        assert_eq!(
            valid(Some("1"), true, "qa", Some(&temp), Some(&profile)).unwrap_err(),
            "desktop_qa_path_not_isolated"
        );
        assert_eq!(
            valid(Some("1"), true, "qa", Some(&store), Some(&store)).unwrap_err(),
            "desktop_qa_paths_overlap"
        );
        assert_eq!(
            valid(Some("1"), true, "qa", Some(&root), Some(&profile)).unwrap_err(),
            "desktop_qa_paths_overlap"
        );
        assert_eq!(
            valid(
                Some("1"),
                true,
                "qa",
                Some(&traversal),
                Some(&profile)
            )
            .unwrap_err(),
            "desktop_qa_path_invalid"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
