// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Build script for `uffs-cli`.
//!
//! Emits MSVC `/DELAYLOAD` linker directives for DLLs that are imported
//! transitively but are **not** on the hot path of the thin CLI.  Delay-loading
//! means the DLL is not mapped into the process image table at launch; it is
//! only paged in if a function from it is actually called.  For a launcher
//! whose wall-clock budget is dominated by process creation and DLL loads,
//! this is a real win even for "cheap" system DLLs.
//!
//! # Hot-path DLLs (NEVER delay-load these)
//!
//! - `KERNEL32.dll`, `ntdll.dll`, `VCRUNTIME140.dll`, `api-ms-win-*` — core
//!   runtime, loaded before `main` runs.
//! - `advapi32.dll` — `LookupAccountNameW` / `OpenProcessToken` for deriving
//!   the named-pipe user-SID hash (called on every launch).
//! - `userenv.dll` — `GetUserProfileDirectoryW` via `dirs-next` for resolving
//!   the daemon socket / pipe location.
//! - `shell32.dll` — `SHGetKnownFolderPath` via `dirs-next` for config dir.
//! - `bcryptprimitives.dll` — `getrandom` is called before `main` by several
//!   deps (hashmap seed, etc.).
//!
//! # Safe delay-load candidates (imported but never called)
//!
//! - `combase.dll` — COM runtime.  The `windows` crate exposes COM bindings via
//!   the `Win32_System_Com` feature, but `uffs-cli` never calls
//!   `CoInitializeEx`, `CoTaskMemFree`, or any other COM entry point.  The
//!   import is pulled in by the dependency graph only.
//! - `oleaut32.dll` — OLE Automation (BSTR / VARIANT).  Same story: pulled
//!   transitively, never actually called from the CLI binary.
//!
//! If either of these turns out to be called after all, the delay-load stub
//! will resolve lazily on first call — it will NOT crash.  The cost is
//! simply a one-time per-DLL load at the call site instead of at process
//! start.  See `perf-phase2-measurement-plan.md` §2.4 for A/B results.

fn main() {
    // Re-run only when this file changes.  Nothing else in the source tree
    // affects the linker flags emitted here.
    println!("cargo:rerun-if-changed=build.rs");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    // MSVC-only: `/DELAYLOAD` is an MSVC link.exe feature and requires
    // `delayimp.lib` for the stub resolver.  MinGW / GNU toolchains use a
    // different mechanism that we do not attempt here.
    if target_os == "windows" && target_env == "msvc" {
        for dll in ["combase.dll", "oleaut32.dll"] {
            println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
        }
        // Stub resolver used by /DELAYLOAD.
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }
}
