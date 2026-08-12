#!/usr/bin/env rust-script
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.
//!
//! install-bins.rs — build the workspace and install **every** binary it
//! produces to `~/bin`.
//!
//! The binary list is not hardcoded: after building, the same build is
//! re-run with `--message-format=json` (instant from cache) and every
//! `compiler-artifact` message's `executable` path is installed. A new
//! `[[bin]]` anywhere in the workspace is picked up automatically.
//!
//! Called by `just use-local` as
//! `rust-script scripts/dev/install-bins.rs`. A rust-script on purpose:
//!
//! - a just shebang recipe dies on Windows (just hands bash a raw
//!   `C:\...` temp path whose backslashes bash eats, exit 127);
//! - a plain `bash script.sh` line dies too when `bash` on PATH resolves
//!   to WSL's `System32\bash.exe`, which has no cargo ("cargo: command
//!   not found").
//!
//! rust-script runs identically under just's unix shell and its
//! powershell windows-shell, and spawns `cargo` from the real PATH.
//!
//! Usage:
//!   rust-script scripts/dev/install-bins.rs

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn main() {
    let build_args: &[&str] = &["build", "--release", "--workspace"];

    eprintln!("📦 Build (release) + install UFFS binaries to ~/bin");
    eprintln!("========================================================");

    // Note whether a daemon was serving BEFORE we tear it down, so the
    // install can put it back afterwards (see `restart_daemon`).  Probing
    // after `stop_running_services` would of course always say "no".
    let daemon_was_running = daemon_is_running();
    let mcp_was_running = mcp_is_running();

    // Stop the running daemon + MCP first (best effort), mirroring the
    // previous [unix] bash recipe: the old binaries release their file
    // locks (Windows can't overwrite a running .exe at all) and no stale
    // in-memory index shadows the freshly installed build.
    stop_running_services();

    eprintln!();
    eprintln!("🔨 cargo {}", build_args.join(" "));

    let started = Instant::now();
    let status = Command::new("cargo").args(build_args).status();
    match status {
        Ok(code) if code.success() => {
            eprintln!("  → Built in {}s", started.elapsed().as_secs());
        }
        Ok(code) => {
            eprintln!("❌ build failed with {code:?}");
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("❌ failed to launch cargo: {err}");
            std::process::exit(1);
        }
    }

    // EVERY binary the workspace just built — discovered from cargo, not
    // a hardcoded list, so a new bin target is picked up automatically and
    // nothing is ever silently missed. `--message-format=json` re-emits
    // one `compiler-artifact` message per crate straight from the build
    // cache (instant, since the real build above already ran), and each
    // binary target carries a non-null `"executable"` path. Libraries
    // carry `"executable":null` and are skipped.
    let executables = workspace_executables(build_args);
    if executables.is_empty() {
        eprintln!("❌ cargo reported no workspace binaries to install");
        std::process::exit(1);
    }

    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let Ok(home) = std::env::var(home_var) else {
        eprintln!("❌ {home_var} is not set — nowhere to install to");
        std::process::exit(1);
    };
    let bin_dir = PathBuf::from(&home).join("bin");
    if let Err(err) = std::fs::create_dir_all(&bin_dir) {
        eprintln!("❌ cannot create {}: {err}", bin_dir.display());
        std::process::exit(1);
    }

    let mut installed = 0_u32;
    let mut skipped = 0_u32;
    let mut unchanged = 0_u32;
    eprintln!();
    eprintln!("📦 Installing {} binaries to {}", executables.len(), bin_dir.display());
    for src in &executables {
        let Some(file_name) = src.file_name() else {
            continue;
        };
        let dest = bin_dir.join(file_name);
        let name = file_name.to_string_lossy();
        if !src.is_file() {
            eprintln!("  ⚠️  {name:<28} not found at {} (skipping)", src.display());
            skipped += 1;
            continue;
        }
        // Identical to what is already installed?  Don't touch it.
        //
        // This matters most for `uffs-broker.exe`: it runs as a LocalSystem
        // Windows service, so its image is locked and the copy fails with
        // `os error 32` — which used to fail the whole recipe even when the
        // binary had not changed at all.  The broker's sources move rarely
        // (byte-identical across v0.6.30..v0.6.31), so the common case is a
        // needless copy of an unchanged file.
        if files_identical(src, &dest) {
            eprintln!("  ⏭️  {name:<28} unchanged");
            unchanged += 1;
            continue;
        }
        // Changed AND currently locked by the running service: stop it,
        // copy, restart.  The broker exposes native SCM control for exactly
        // this (`--stop` waits for STOPPED, `--start` waits for RUNNING and
        // for the pipe to actually serve), which is the same quiesce/restore
        // dance `uffs --update` performs.
        let broker_guard = if is_broker(&name) {
            BrokerGuard::stop_for_replace(&dest)
        } else {
            BrokerGuard::inactive()
        };
        // Remove first so the copy gets a fresh inode — overwrites in place
        // share the inode, which lets macOS re-use a path-cached Launch
        // Services deny verdict against an earlier broken copy.
        if dest.is_dir() {
            let _ = std::fs::remove_dir_all(&dest);
        }
        let _ = std::fs::remove_file(&dest);
        match std::fs::copy(src, &dest) {
            Ok(bytes) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    let _ = std::fs::set_permissions(
                        &dest,
                        std::fs::Permissions::from_mode(0o755),
                    );
                }
                eprintln!("  ✅ {name:<28} {:.1} MB", bytes as f64 / 1_048_576.0);
                installed += 1;
            }
            Err(err) => {
                eprintln!("  ❌ {name:<28} copy failed: {err}");
                skipped += 1;
            }
        }
        broker_guard.restart();
    }

    eprintln!();
    if unchanged > 0 {
        eprintln!("✅ Installed {installed} binaries ({unchanged} unchanged, {skipped} skipped)");
    } else {
        eprintln!("✅ Installed {installed} binaries ({skipped} skipped)");
    }
    let on_path = std::env::var("PATH")
        .map(|path| std::env::split_paths(&path).any(|entry| entry == bin_dir))
        .unwrap_or(false);
    if !on_path {
        eprintln!("⚠️  {} is not on PATH", bin_dir.display());
    }
    // Put back what we took down.  Deliberately BEFORE the `skipped`
    // exit: a partially-failed install is exactly the case where leaving
    // the machine daemon-less hurts most.
    if daemon_was_running {
        restart_daemon(&bin_dir);
    }
    if mcp_was_running {
        restart_mcp(&bin_dir);
    }
    if skipped > 0 {
        std::process::exit(1);
    }
}

/// True when `src` and `dest` are byte-identical, so the copy can be
/// skipped entirely.  Compares length first (cheap, rejects almost every
/// changed binary) and only then the contents.  A missing or unreadable
/// `dest` is "not identical", so the normal copy path runs.
fn files_identical(src: &std::path::Path, dest: &std::path::Path) -> bool {
    let (Ok(src_meta), Ok(dest_meta)) = (src.metadata(), dest.metadata()) else {
        return false;
    };
    if src_meta.len() != dest_meta.len() {
        return false;
    }
    match (std::fs::read(src), std::fs::read(dest)) {
        (Ok(lhs), Ok(rhs)) => lhs == rhs,
        _ => false,
    }
}

/// Is this the Access Broker binary (the one that runs as a service and
/// therefore holds its own image open)?
fn is_broker(name: &str) -> bool {
    let stem = name.strip_suffix(".exe").unwrap_or(name);
    stem == "uffs-broker"
}

/// Stops the broker service around a replace, and restarts it afterwards.
///
/// `stop_for_replace` is a no-op unless the service is actually running,
/// so a developer box without the broker installed sees no behaviour
/// change.  Restart is best-effort and never fails the install: leaving
/// the new binary in place with the service down is recoverable
/// (`uffs-broker --start`), and is reported loudly.
struct BrokerGuard {
    /// Path of the installed broker binary, when we stopped its service.
    stopped: Option<std::path::PathBuf>,
}

impl BrokerGuard {
    /// A guard that does nothing (non-broker binaries).
    const fn inactive() -> Self {
        Self { stopped: None }
    }

    /// Stop the broker service so its image can be overwritten.
    fn stop_for_replace(installed: &std::path::Path) -> Self {
        if !installed.is_file() {
            return Self::inactive();
        }
        eprintln!("  ⏸️  uffs-broker                 stopping service to replace it");
        let stopped = std::process::Command::new(installed)
            .arg("--stop")
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if stopped {
            Self { stopped: Some(installed.to_path_buf()) }
        } else {
            // Not installed as a service, already stopped, or not
            // elevated — the copy below will report the real error.
            Self::inactive()
        }
    }

    /// Restart the service if this guard stopped it.
    fn restart(self) {
        let Some(path) = self.stopped else {
            return;
        };
        let started = std::process::Command::new(&path)
            .arg("--start")
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if started {
            eprintln!("  ▶️  uffs-broker                 service restarted");
        } else {
            eprintln!(
                "  ⚠️  uffs-broker                 service did NOT restart — run: {} --start",
                path.display()
            );
        }
    }
}

/// Was a daemon serving before we tore everything down?
///
/// `--daemon status` prints `● running  PID …` when up and
/// `○ Daemon  not running` when not, so "contains `running` but not
/// `not running`" is an exact read of both shapes. Any failure to run
/// the probe (no `uffs` on PATH yet on a first install) reads as "not
/// running", which is the safe answer: we then leave things alone.
fn daemon_is_running() -> bool {
    Command::new("uffs")
        .args(["--daemon", "status"])
        .output()
        .map(|out| {
            let text = String::from_utf8_lossy(&out.stdout);
            text.contains("running") && !text.contains("not running")
        })
        .unwrap_or(false)
}

/// Restart the daemon we deliberately stopped, using the freshly
/// installed binary.
///
/// `use-local` kills the daemon + MCP so their images can be replaced,
/// but until now never brought them back — so a routine dev install
/// silently left the machine without a daemon, which is exactly the
/// promise `uffs --daemon resident` makes and breaks. Restoring it here
/// keeps the invariant "use-local leaves the machine as it found it".
///
/// The restart goes through the normal `--daemon start` path, so the
/// resident marker (`resident.args`) is merged in by the client's
/// auto-spawn — a daemon that was resident comes back resident, with
/// `--no-retire`, rather than as a plain ephemeral one.
fn restart_daemon(bin_dir: &std::path::Path) {
    let exe = bin_dir.join(if cfg!(windows) { "uffs.exe" } else { "uffs" });
    if !exe.is_file() {
        return;
    }
    eprintln!();
    eprintln!("🔄 Restarting the daemon (it was running before the install)...");
    let ok = Command::new(&exe)
        .args(["--daemon", "start"])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if ok {
        eprintln!("✅ Daemon restarted.");
    } else {
        eprintln!(
            "⚠️  Daemon did NOT restart — run: {} --daemon start",
            exe.display()
        );
    }
}

/// Was the MCP HTTP gateway serving before the teardown?
///
/// `--mcp status` prints `MCP server:    running (PID …)` when up, and
/// either `not running (no PID file)` or `not running (stale PID file…)`
/// when not — so the same "contains `running` but not `not running`"
/// read works here.
fn mcp_is_running() -> bool {
    Command::new("uffs")
        .args(["--mcp", "status"])
        .output()
        .map(|out| {
            let text = String::from_utf8_lossy(&out.stdout);
            text.contains("running") && !text.contains("not running")
        })
        .unwrap_or(false)
}

/// Restart the MCP HTTP gateway we stopped, using the new binary.
fn restart_mcp(bin_dir: &std::path::Path) {
    let exe = bin_dir.join(if cfg!(windows) { "uffs.exe" } else { "uffs" });
    if !exe.is_file() {
        return;
    }
    eprintln!();
    eprintln!("🔄 Restarting the MCP server (it was running before the install)...");
    let ok = Command::new(&exe)
        .args(["--mcp", "start"])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if ok {
        eprintln!("✅ MCP server restarted.");
    } else {
        eprintln!(
            "⚠️  MCP server did NOT restart — run: {} --mcp start",
            exe.display()
        );
    }
}

/// Best-effort shutdown of the resident daemon + MCP before installing.
///
/// `uffs --daemon kill` is given 10 seconds (a wedged daemon must not
/// hang the install), then any survivors are hard-killed by image name —
/// `pkill -x` on Unix, `taskkill /IM … /F` on Windows. Every step is
/// best-effort: on a machine with nothing running (or no `uffs` on PATH
/// yet), all of this silently no-ops.
fn stop_running_services() {
    eprintln!();
    eprintln!("🔪 Stopping daemon + MCP (best effort)...");
    if let Ok(mut child) = Command::new("uffs")
        .args(["--daemon", "kill"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                _ => {
                    let _ = child.kill();
                    break;
                }
            }
        }
    }
    // Ask the MCP gateway to stop cleanly first.  The `taskkill /F`
    // below is a `/F` by image name: it kills every `uffsmcp` process
    // outright, so the gateway never removes its PID file and the next
    // `--mcp status` reports `not running (stale PID file, PID …)`.
    // A graceful stop leaves no such litter; the force-kill stays as the
    // backstop for a wedged process.
    let _ = Command::new("uffs")
        .args(["--mcp", "stop"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    for name in ["uffsd", "uffsmcp"] {
        let status = if cfg!(windows) {
            Command::new("taskkill")
                .args(["/IM", &format!("{name}.exe"), "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
        } else {
            Command::new("pkill")
                .args(["-x", name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
        };
        let _ = status;
    }
}

/// Every binary the workspace build produced, as absolute paths.
///
/// Re-runs the same build with `--message-format=json` — instant, since
/// the real build already populated the cache — and collects each
/// `compiler-artifact` message's non-null `executable` path. This is the
/// authoritative "what did we build" list: no name guessing, no `.exe`
/// handling, no missed target when a new `[[bin]]` is added.
fn workspace_executables(build_args: &[&str]) -> Vec<PathBuf> {
    let mut args: Vec<&str> = build_args.to_vec();
    args.push("--message-format=json");
    let output = Command::new("cargo")
        .args(&args)
        .stderr(Stdio::inherit())
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    let Ok(text) = String::from_utf8(output.stdout) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for line in text.lines() {
        if let Some(exe) = json_executable(line) {
            paths.push(PathBuf::from(exe));
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

/// The non-null `"executable"` path from one cargo JSON message line, if
/// present. `"executable":null` (a library artifact) yields `None`.
fn json_executable(line: &str) -> Option<String> {
    let key = "\"executable\":";
    let after = &line[line.find(key)? + key.len()..];
    // Value is either `null` or `"<escaped path>"`.
    let rest = after.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(unescape_json_path(&rest[..end]))
}

/// Undo JSON string escaping in a path (`\\` → `\`, `\/` → `/`).
fn unescape_json_path(escaped: &str) -> String {
    let mut out = String::with_capacity(escaped.len());
    let mut chars = escaped.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}
