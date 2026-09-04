fn main() {
    // `legacy_win7` is the single cfg every Windows-7-only code path keys off.
    // It is on when the crate is compiled for a Windows 7 target triple
    // (`x86_64-win7-windows-msvc` / `i686-win7-windows-msvc`, whose vendor is
    // `win7`), and — as a preview for clippy/tests on an ordinary toolchain —
    // when the `win7` cargo feature is passed. Dependencies are selected by
    // `target_vendor` in Cargo.toml; this cfg only steers our own source.
    println!("cargo:rustc-check-cfg=cfg(legacy_win7)");

    let win7_target = std::env::var("CARGO_CFG_TARGET_VENDOR").as_deref() == Ok("win7");
    let win7_preview = std::env::var_os("CARGO_FEATURE_WIN7").is_some();
    let legacy_win7 = win7_target || win7_preview;
    if legacy_win7 {
        println!("cargo:rustc-cfg=legacy_win7");
    }

    // The ACL resolver parses EVERY capability file under its pattern before the
    // enabled list in `app.security.capabilities` is applied, and rejects a
    // permission from a plugin that isn't compiled in. `notification:default`
    // in `capabilities/*.json` therefore breaks the Win7 build (no notification
    // plugin there) even when those files are not enabled — so the Win7 build
    // reads its own directory, `capabilities-win7/`, which holds the same two
    // files minus that permission. Keep the two directories in step.
    let attributes = if legacy_win7 {
        tauri_build::Attributes::new().capabilities_path_pattern("./capabilities-win7/*.json")
    } else {
        tauri_build::Attributes::new()
    };

    tauri_build::try_build(attributes).expect("failed to run tauri-build")
}
