fn main() {
    println!("cargo:rerun-if-env-changed=CLASSAIMATE_WINDOWS_TEST_MANIFEST");
    let test_manifest = std::env::var("CLASSAIMATE_WINDOWS_TEST_MANIFEST").as_deref() == Ok("1")
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if test_manifest {
        // Cargo's unit-test executable needs the same Common Controls v6
        // dependency as the GUI. Keep this workaround outside installer builds.
        tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(
            tauri_build::WindowsAttributes::new_without_app_manifest(),
        ))
        .expect("failed to build test resources");
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/windows-app-manifest.xml");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    } else {
        tauri_build::build()
    }
}
