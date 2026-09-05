pub(crate) fn local_pc_name() -> String {
    #[cfg(target_os = "macos")]
    {
        mac_pc_name(
            std::env::var("COMPUTERNAME").ok().as_deref(),
            std::env::var("HOSTNAME").ok().as_deref(),
            |key| {
                let output = std::process::Command::new("/usr/sbin/scutil")
                    .args(["--get", key])
                    .output()
                    .ok()?;
                output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
            },
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .map(|value| crate::normalize(value, 120))
            .unwrap_or_default()
    }
}

#[cfg(any(target_os = "macos", test))]
fn valid_mac_name(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().any(|ch| ch.is_ascii_control()) {
        return None;
    }
    // Match the server's JavaScript string-length limit, including emoji.
    let mut units = 0;
    Some(value.chars().take_while(|ch| {
        units += ch.len_utf16();
        units <= 120
    }).collect())
}

#[cfg(any(target_os = "macos", test))]
fn mac_pc_name(
    computer_name: Option<&str>,
    hostname: Option<&str>,
    mut read_system_name: impl FnMut(&str) -> Option<String>,
) -> String {
    for value in [computer_name, hostname].into_iter().flatten() {
        if let Some(name) = valid_mac_name(value) { return name; }
    }
    for key in ["ComputerName", "LocalHostName"] {
        if let Some(name) = read_system_name(key).as_deref().and_then(valid_mac_name) {
            return name;
        }
    }
    "Mac".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finder_launch_without_shell_environment_uses_mac_computer_name() {
        assert_eq!(mac_pc_name(None, None, |key| {
            assert_eq!(key, "ComputerName");
            Some("교사용 Mac mini\n".into())
        }), "교사용 Mac mini");
    }

    #[test]
    fn blank_environment_does_not_prevent_system_fallback() {
        assert_eq!(mac_pc_name(Some(" "), Some(""), |key| {
            (key == "LocalHostName").then(|| "teacher-mac-mini".into())
        }), "teacher-mac-mini");
    }

    #[test]
    fn usable_environment_name_preserves_priority() {
        assert_eq!(mac_pc_name(Some(" School Mac "), Some("host"), |_| {
            panic!("do not query OS when environment name is usable")
        }), "School Mac");
    }

    #[test]
    fn missing_or_invalid_system_names_still_produce_a_valid_display_label() {
        assert_eq!(mac_pc_name(None, None, |_| None), "Mac");
        assert_eq!(mac_pc_name(Some("bad\0name"), None, |_| Some("\n".into())), "Mac");
    }

    #[test]
    fn mac_name_obeys_server_utf16_limit() {
        let value = valid_mac_name(&"📚".repeat(100)).unwrap();
        assert_eq!(value.encode_utf16().count(), 120);
        assert_eq!(value.chars().count(), 60);
    }
}
