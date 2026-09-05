use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const LABEL: &str = "com.classaimate.local-sensitive-store.autostart";
const OWNER: &str = "<!-- ClassAimate managed login launcher v1 -->";
static FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

trait Launchctl {
    fn run(&self, args: &[&OsStr]) -> Result<Output, String>;
}

struct NativeLaunchctl;

impl Launchctl for NativeLaunchctl {
    fn run(&self, args: &[&OsStr]) -> Result<Output, String> {
        Command::new("/bin/launchctl")
            .args(args)
            .output()
            .map_err(|e| format!("macos_autostart_command_failed:{e}"))
    }
}

pub(crate) fn apply(enabled: bool) -> Result<(), String> {
    let (home, executable, uid) = user_context()?;
    apply_for_user(enabled, &home, &executable, uid, &NativeLaunchctl)
}

pub(crate) fn verify(enabled: bool) -> Result<(), String> {
    let (home, executable, uid) = user_context()?;
    verify_for_user(enabled, &home, &executable, uid, &NativeLaunchctl)
}

fn user_context() -> Result<(PathBuf, PathBuf, u32), String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("macos_autostart_home_missing")?;
    let output = Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .map_err(|e| format!("macos_autostart_uid_failed:{e}"))?;
    let uid = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .map_err(|_| "macos_autostart_uid_invalid")?;
    if !output.status.success() || uid == 0 {
        return Err("macos_autostart_user_session_required".into());
    }
    let executable =
        std::env::current_exe().map_err(|e| format!("macos_autostart_exe_failed:{e}"))?;
    Ok((home, executable, uid))
}

fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn app_bundle(executable: &Path) -> Result<PathBuf, String> {
    let executable =
        fs::canonicalize(executable).map_err(|e| format!("macos_autostart_exe_failed:{e}"))?;
    let macos = executable
        .parent()
        .ok_or("macos_autostart_install_required")?;
    let contents = macos.parent().ok_or("macos_autostart_install_required")?;
    let bundle = contents
        .parent()
        .ok_or("macos_autostart_install_required")?;
    if !executable.is_file()
        || fs::metadata(&executable)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode()
            & 0o111
            == 0
        || executable.file_name() != Some(OsStr::new("local-sensitive-store-desktop"))
        || macos.file_name() != Some(OsStr::new("MacOS"))
        || contents.file_name() != Some(OsStr::new("Contents"))
        || bundle.extension() != Some(OsStr::new("app"))
        || !contents.join("Info.plist").is_file()
    {
        return Err("macos_autostart_install_required".into());
    }
    Ok(bundle.to_path_buf())
}

fn plist(executable: &Path) -> Result<String, String> {
    let bundle = app_bundle(executable)?;
    let bundle = bundle.to_str().ok_or("macos_autostart_path_invalid")?;
    if bundle.chars().any(char::is_control) {
        return Err("macos_autostart_path_invalid".into());
    }
    // A one-shot launcher avoids bootout terminating the user's running app.
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
{OWNER}
<plist version="1.0"><dict>
<key>Label</key><string>{LABEL}</string>
<key>ProgramArguments</key><array>
<string>/usr/bin/open</string><string>-g</string><string>-a</string><string>{}</string>
<string>--args</string><string>--background</string>
</array>
<key>RunAtLoad</key><true/>
<key>LimitLoadToSessionType</key><string>Aqua</string>
</dict></plist>
"#,
        xml(bundle)
    ))
}

fn launch_agents(home: &Path, create: bool, uid: u32) -> Result<PathBuf, String> {
    if !home.is_absolute()
        || home
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("macos_autostart_home_invalid".into());
    }
    let mut directory =
        fs::canonicalize(home).map_err(|e| format!("macos_autostart_home_failed:{e}"))?;
    let owner = fs::metadata(&directory).map_err(|e| format!("macos_autostart_home_failed:{e}"))?;
    if owner.uid() != uid || owner.permissions().mode() & 0o022 != 0 {
        return Err("macos_autostart_home_unsafe".into());
    }
    for name in ["Library", "LaunchAgents"] {
        directory.push(name);
        match fs::symlink_metadata(&directory) {
            Ok(metadata)
                if metadata.is_dir()
                    && !metadata.file_type().is_symlink()
                    && metadata.uid() == uid
                    && metadata.permissions().mode() & 0o022 == 0 =>
            {
                ()
            }
            Ok(_) => return Err("macos_autostart_directory_unsafe".into()),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                if create {
                    fs::create_dir(&directory)
                        .map_err(|e| format!("macos_autostart_directory_failed:{e}"))?;
                }
            }
            Err(e) => return Err(format!("macos_autostart_directory_failed:{e}")),
        }
    }
    Ok(directory)
}

fn read_owned(path: &Path, uid: u32) -> Result<Option<String>, String> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("macos_autostart_read_failed:{e}")),
        Ok(metadata)
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.uid() != uid
                || metadata.permissions().mode() & 0o022 != 0 =>
        {
            return Err("macos_autostart_file_unsafe".into());
        }
        Ok(_) => (),
    }
    let value = fs::read_to_string(path).map_err(|e| format!("macos_autostart_read_failed:{e}"))?;
    if !value.contains(OWNER)
        || !value.contains(&format!("<key>Label</key><string>{LABEL}</string>"))
    {
        return Err("macos_autostart_file_not_owned".into());
    }
    Ok(Some(value))
}

fn write_atomic(path: &Path, value: &str) -> Result<(), String> {
    let temporary = path.with_extension(format!(
        "plist.{}.{}.tmp",
        std::process::id(),
        FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| format!("macos_autostart_write_failed:{e}"))?;
    let result = file
        .write_all(value.as_bytes())
        .and_then(|_| file.sync_all())
        .and_then(|_| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|e| format!("macos_autostart_write_failed:{e}"))
}

fn loaded(runner: &impl Launchctl, service: &str) -> Result<bool, String> {
    let output = runner.run(&[OsStr::new("print"), OsStr::new(service)])?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(113) => Ok(false),
        code => Err(format!("macos_autostart_status_failed:{code:?}")),
    }
}

fn checked(runner: &impl Launchctl, args: &[&OsStr]) -> Result<(), String> {
    let output = runner.run(args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "macos_autostart_{}_failed:{:?}",
            args[0].to_string_lossy(),
            output.status.code()
        ))
    }
}

fn disabled(runner: &impl Launchctl, domain: &str) -> Result<bool, String> {
    let output = runner.run(&[OsStr::new("print-disabled"), OsStr::new(domain)])?;
    if !output.status.success() {
        return Err("macos_autostart_disabled_status_failed".into());
    }
    let state = String::from_utf8_lossy(&output.stdout);
    Ok(state
        .lines()
        .any(|line| line.trim().starts_with(&format!("\"{LABEL}\" => true"))))
}

fn verify_for_user(
    enabled: bool,
    home: &Path,
    executable: &Path,
    uid: u32,
    runner: &impl Launchctl,
) -> Result<(), String> {
    let path = launch_agents(home, false, uid)?.join(format!("{LABEL}.plist"));
    let existing = read_owned(&path, uid)?;
    if !enabled {
        return if existing.is_none() {
            Ok(())
        } else {
            Err("macos_autostart_preferences_mismatch".into())
        };
    }
    if existing.as_deref() != Some(plist(executable)?.as_str()) {
        return Err("macos_autostart_preferences_mismatch".into());
    }
    let domain = format!("gui/{uid}");
    if !loaded(runner, &format!("{domain}/{LABEL}"))? {
        return Err("macos_autostart_registration_unverified".into());
    }
    if disabled(runner, &domain)? {
        return Err("macos_autostart_disabled_in_system_settings".into());
    }
    Ok(())
}

fn apply_for_user(
    enabled: bool,
    home: &Path,
    executable: &Path,
    uid: u32,
    runner: &impl Launchctl,
) -> Result<(), String> {
    let desired = if enabled {
        Some(plist(executable)?)
    } else {
        None
    };
    let path = launch_agents(home, enabled, uid)?.join(format!("{LABEL}.plist"));
    let previous = read_owned(&path, uid)?;
    if !enabled && previous.is_none() {
        return Ok(());
    }
    let domain = format!("gui/{uid}");
    let service = format!("{domain}/{LABEL}");
    let was_loaded = loaded(runner, &service)?;
    let was_disabled = if enabled {
        disabled(runner, &domain)?
    } else {
        false
    };
    let bootstrap = [
        OsStr::new("bootstrap"),
        OsStr::new(&domain),
        path.as_os_str(),
    ];
    let bootout = [OsStr::new("bootout"), OsStr::new(&service)];
    let mut enable_attempted = false;
    let result = (|| {
        if enabled {
            write_atomic(&path, desired.as_deref().unwrap())?;
            enable_attempted = true;
            checked(runner, &[OsStr::new("enable"), OsStr::new(&service)])?;
            if was_loaded && previous != desired {
                checked(runner, &bootout)?;
            }
            if !was_loaded || previous != desired {
                checked(runner, &bootstrap)?;
            }
            if !loaded(runner, &service)? {
                return Err("macos_autostart_registration_unverified".into());
            }
        } else {
            if was_loaded {
                checked(runner, &bootout)?;
            }
            if loaded(runner, &service)? {
                return Err("macos_autostart_removal_unverified".into());
            }
            fs::remove_file(&path).map_err(|e| format!("macos_autostart_remove_failed:{e}"))?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let rollback = (|| {
            if loaded(runner, &service)? {
                checked(runner, &bootout)?;
            }
            if let Some(value) = previous {
                write_atomic(&path, &value)?;
                if was_loaded {
                    checked(runner, &bootstrap)?;
                }
            } else if path.exists() {
                fs::remove_file(&path).map_err(|e| format!("macos_autostart_remove_failed:{e}"))?;
            }
            Ok::<_, String>(())
        })();
        // Restore the user's OS override even when plist/service rollback failed.
        let disabled_rollback = if enable_attempted {
            checked(
                runner,
                &[
                    OsStr::new(if was_disabled { "disable" } else { "enable" }),
                    OsStr::new(&service),
                ],
            )
        } else {
            Ok(())
        };
        let rollback_errors: Vec<String> = [rollback.err(), disabled_rollback.err()]
            .into_iter()
            .flatten()
            .collect();
        return Err(if rollback_errors.is_empty() {
            error
        } else {
            format!("{error};rollback_failed:{}", rollback_errors.join(";"))
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::os::unix::{fs::symlink, process::ExitStatusExt};

    #[derive(Default)]
    struct FakeLaunchctl {
        loaded: Cell<bool>,
        disabled: Cell<bool>,
        fail_once: RefCell<Option<String>>,
        calls: RefCell<Vec<Vec<String>>>,
    }
    impl Launchctl for FakeLaunchctl {
        fn run(&self, args: &[&OsStr]) -> Result<Output, String> {
            let args: Vec<String> = args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            self.calls.borrow_mut().push(args.clone());
            let fail = self.fail_once.borrow().as_deref() == Some(&args[0]);
            let code = if fail {
                self.fail_once.take();
                5
            } else {
                match args[0].as_str() {
                    "print" => {
                        if self.loaded.get() {
                            0
                        } else {
                            113
                        }
                    }
                    "bootstrap" => {
                        self.loaded.set(true);
                        0
                    }
                    "bootout" => {
                        self.loaded.set(false);
                        0
                    }
                    "enable" => {
                        self.disabled.set(false);
                        0
                    }
                    "disable" => {
                        self.disabled.set(true);
                        0
                    }
                    "print-disabled" => 0,
                    _ => panic!("unexpected command"),
                }
            };
            Ok(Output {
                status: std::process::ExitStatus::from_raw(code << 8),
                stdout: if args[0] == "print-disabled" {
                    format!(
                        "disabled services = {{\n\t\"{LABEL}\" => {}\n}}",
                        self.disabled.get()
                    )
                    .into_bytes()
                } else {
                    vec![]
                },
                stderr: vec![],
            })
        }
    }
    struct Fixture {
        home: PathBuf,
        executable: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let home = std::env::temp_dir().join(format!(
                "classaimate-autostart-{}-{}",
                std::process::id(),
                FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&home).unwrap();
            let contents = home.join("Applications/교사 & <테스트> '$` 앱.app/Contents");
            fs::create_dir_all(contents.join("MacOS")).unwrap();
            fs::write(contents.join("Info.plist"), "fixture").unwrap();
            let executable = contents.join("MacOS/local-sensitive-store-desktop");
            fs::write(&executable, "fixture").unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            Self { home, executable }
        }
        fn path(&self) -> PathBuf {
            self.home
                .join(format!("Library/LaunchAgents/{LABEL}.plist"))
        }
        fn apply(&self, enabled: bool, runner: &FakeLaunchctl) -> Result<(), String> {
            apply_for_user(
                enabled,
                &self.home,
                &self.executable,
                fs::metadata(&self.home).unwrap().uid(),
                runner,
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.home).unwrap();
        }
    }

    #[test]
    fn enable_registers_exact_escaped_bundle_and_disable_only_removes_owned_launcher() {
        let fixture = Fixture::new();
        let runner = FakeLaunchctl::default();
        fixture.apply(true, &runner).unwrap();
        let value = fs::read_to_string(fixture.path()).unwrap();
        assert!(value.contains("교사 &amp; &lt;테스트&gt; &apos;$` 앱.app"));
        assert!(value.contains("<string>/usr/bin/open</string>"));
        assert!(value.contains("<string>--background</string>"));
        assert!(!value.contains("KeepAlive"));
        assert_eq!(
            fs::metadata(fixture.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(runner.loaded.get());
        let unrelated = fixture.path().parent().unwrap().join("unrelated.plist");
        fs::write(&unrelated, "preserved").unwrap();
        fixture.apply(true, &runner).unwrap();
        assert_eq!(
            runner
                .calls
                .borrow()
                .iter()
                .filter(|args| args[0] == "bootstrap")
                .count(),
            1
        );
        fixture.apply(false, &runner).unwrap();
        assert!(!runner.loaded.get());
        assert!(!fixture.path().exists());
        assert_eq!(fs::read_to_string(unrelated).unwrap(), "preserved");
        fixture.apply(false, &runner).unwrap();
    }

    #[test]
    fn bootstrap_failure_rolls_back_file_and_does_not_claim_success() {
        let fixture = Fixture::new();
        let runner = FakeLaunchctl::default();
        runner.fail_once.replace(Some("bootstrap".into()));
        assert!(fixture
            .apply(true, &runner)
            .unwrap_err()
            .starts_with("macos_autostart_bootstrap_failed"));
        assert!(!fixture.path().exists());
        assert!(!runner.loaded.get());
    }

    #[test]
    fn failed_enable_restores_existing_os_disabled_override_and_plist() {
        for has_previous_plist in [false, true] {
            let fixture = Fixture::new();
            let runner = FakeLaunchctl::default();
            let previous = if has_previous_plist {
                fs::create_dir_all(fixture.path().parent().unwrap()).unwrap();
                let value = plist(&fixture.executable).unwrap();
                fs::write(fixture.path(), &value).unwrap();
                Some(value)
            } else {
                None
            };
            runner.disabled.set(true);
            runner.fail_once.replace(Some("bootstrap".into()));
            assert!(fixture
                .apply(true, &runner)
                .unwrap_err()
                .starts_with("macos_autostart_bootstrap_failed"));
            assert_eq!(fs::read_to_string(fixture.path()).ok(), previous);
            assert!(!runner.loaded.get());
            assert!(runner.disabled.get());
            let calls = runner.calls.borrow();
            let snapshot = calls
                .iter()
                .position(|args| args[0] == "print-disabled")
                .unwrap();
            let enable = calls.iter().position(|args| args[0] == "enable").unwrap();
            assert!(snapshot < enable);
            assert_eq!(calls.last().unwrap()[0], "disable");
        }
    }

    #[test]
    fn disabled_status_read_failure_does_not_mutate_registration() {
        let fixture = Fixture::new();
        let runner = FakeLaunchctl::default();
        runner.disabled.set(true);
        runner.fail_once.replace(Some("print-disabled".into()));
        assert_eq!(
            fixture.apply(true, &runner).unwrap_err(),
            "macos_autostart_disabled_status_failed"
        );
        assert!(runner.disabled.get());
        assert!(!fixture.path().exists());
        assert!(runner
            .calls
            .borrow()
            .iter()
            .all(|args| ["print", "print-disabled"].contains(&args[0].as_str())));
    }

    #[test]
    fn bootout_failure_preserves_previous_registration() {
        let fixture = Fixture::new();
        let runner = FakeLaunchctl::default();
        fixture.apply(true, &runner).unwrap();
        let original = fs::read_to_string(fixture.path()).unwrap();
        runner.fail_once.replace(Some("bootout".into()));
        assert!(fixture
            .apply(false, &runner)
            .unwrap_err()
            .starts_with("macos_autostart_bootout_failed"));
        assert_eq!(fs::read_to_string(fixture.path()).unwrap(), original);
        assert!(runner.loaded.get());
    }

    #[test]
    fn read_failure_is_not_treated_as_an_absent_service() {
        let fixture = Fixture::new();
        let runner = FakeLaunchctl::default();
        runner.fail_once.replace(Some("print".into()));
        assert!(fixture
            .apply(true, &runner)
            .unwrap_err()
            .starts_with("macos_autostart_status_failed"));
        assert!(!fixture.path().exists());
        assert_eq!(runner.calls.borrow().len(), 1);
    }

    #[test]
    fn unmanaged_file_and_symlinks_are_never_replaced() {
        let fixture = Fixture::new();
        let runner = FakeLaunchctl::default();
        fs::create_dir_all(fixture.path().parent().unwrap()).unwrap();
        fs::write(fixture.path(), "unrelated file").unwrap();
        assert_eq!(
            fixture.apply(true, &runner).unwrap_err(),
            "macos_autostart_file_not_owned"
        );
        assert_eq!(
            fixture.apply(false, &runner).unwrap_err(),
            "macos_autostart_file_not_owned"
        );
        fs::remove_file(fixture.path()).unwrap();
        symlink(&fixture.executable, fixture.path()).unwrap();
        assert_eq!(
            fixture.apply(true, &runner).unwrap_err(),
            "macos_autostart_file_unsafe"
        );
        assert!(runner.calls.borrow().is_empty());
    }

    #[test]
    fn symlinked_launch_agents_and_uninstalled_binaries_are_rejected() {
        let fixture = Fixture::new();
        let runner = FakeLaunchctl::default();
        fs::create_dir(fixture.home.join("Library")).unwrap();
        symlink(
            fixture.home.join("Applications"),
            fixture.home.join("Library/LaunchAgents"),
        )
        .unwrap();
        assert_eq!(
            fixture.apply(true, &runner).unwrap_err(),
            "macos_autostart_directory_unsafe"
        );
        let debug_exe = fixture.home.join("debug-binary");
        fs::write(&debug_exe, "fixture").unwrap();
        assert_eq!(
            plist(&debug_exe).unwrap_err(),
            "macos_autostart_install_required"
        );
        assert!(runner.calls.borrow().is_empty());
    }

    #[test]
    fn default_off_does_not_create_files_or_execute_launchctl() {
        let fixture = Fixture::new();
        let runner = FakeLaunchctl::default();
        fixture.apply(false, &runner).unwrap();
        assert!(!fixture.home.join("Library").exists());
        assert!(runner.calls.borrow().is_empty());
    }

    #[test]
    fn startup_is_read_only_even_with_different_or_stale_saved_preferences() {
        let fixture = Fixture::new();
        let runner = FakeLaunchctl::default();
        let uid = fs::metadata(&fixture.home).unwrap().uid();
        verify_for_user(false, &fixture.home, &fixture.executable, uid, &runner).unwrap();
        assert!(!fixture.home.join("Library").exists());
        fixture.apply(true, &runner).unwrap();
        runner.calls.borrow_mut().clear();
        assert_eq!(
            verify_for_user(false, &fixture.home, &fixture.executable, uid, &runner).unwrap_err(),
            "macos_autostart_preferences_mismatch"
        );
        assert!(runner.calls.borrow().is_empty());
        verify_for_user(true, &fixture.home, &fixture.executable, uid, &runner).unwrap();
        assert!(runner
            .calls
            .borrow()
            .iter()
            .all(|args| ["print", "print-disabled"].contains(&args[0].as_str())));
        assert!(fixture.path().is_file());
        runner.disabled.set(true);
        assert_eq!(
            verify_for_user(true, &fixture.home, &fixture.executable, uid, &runner).unwrap_err(),
            "macos_autostart_disabled_in_system_settings"
        );
        assert!(runner.disabled.get());
        runner.loaded.set(false);
        assert_eq!(
            verify_for_user(true, &fixture.home, &fixture.executable, uid, &runner).unwrap_err(),
            "macos_autostart_registration_unverified"
        );
    }

    #[test]
    fn foreign_owner_and_shared_writable_paths_are_rejected() {
        let fixture = Fixture::new();
        let runner = FakeLaunchctl::default();
        let uid = fs::metadata(&fixture.home).unwrap().uid();
        assert_eq!(
            apply_for_user(true, &fixture.home, &fixture.executable, uid + 1, &runner).unwrap_err(),
            "macos_autostart_home_unsafe"
        );
        fs::create_dir_all(fixture.path().parent().unwrap()).unwrap();
        fs::set_permissions(
            fixture.path().parent().unwrap(),
            fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        assert_eq!(
            fixture.apply(true, &runner).unwrap_err(),
            "macos_autostart_directory_unsafe"
        );
        fs::set_permissions(
            fixture.path().parent().unwrap(),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        fs::write(fixture.path(), plist(&fixture.executable).unwrap()).unwrap();
        fs::set_permissions(fixture.path(), fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(
            fixture.apply(false, &runner).unwrap_err(),
            "macos_autostart_file_unsafe"
        );
        assert!(runner.calls.borrow().is_empty());
    }
}
