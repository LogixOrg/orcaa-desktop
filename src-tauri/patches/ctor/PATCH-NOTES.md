# Vendored `ctor` 0.8.0 — Windows 7 target fix

This directory is the crates.io release of `ctor` **0.8.0** (Apache-2.0 OR MIT, by Matt
Mastracci), copied verbatim except for **one change** in `src/macros/mod.rs`:

- the constructor link-section selector accepted Windows only as
  `target_vendor = "pc"`; it now also accepts `target_vendor = "win7"`, which is what the
  tier-3 `x86_64-win7-windows-msvc` / `i686-win7-windows-msvc` targets report. Same
  `.CRT$XCU` section, same behaviour — Windows 7 links constructors exactly like Windows 10.

Why it exists: `tauri-utils` depends on `ctor = "0.8"` for its cached starting-binary path,
and 0.8.0 is the only 0.8.x release. Without this, the Windows 7 lane fails at compile time
with `#[ctor]/#[dtor] is not supported on the current target`.

Wired from `src-tauri/Cargo.toml` via `[patch.crates-io] ctor = { path = "patches/ctor" }`.
Because it resolves to the identical version, every other platform compiles unchanged code.

Remove this directory and the `[patch.crates-io]` block once tauri-utils depends on a `ctor`
release that gates Windows on `target_os = "windows"` (or lists the `win7` vendor).
