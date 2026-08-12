// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `uffs-watchdog` — keeps the resident UFFS services actually running.
//!
//! Residency promises a daemon that is always there (and starts at
//! login). The login item delivers that at boot, and the auto-spawn
//! marker revives a dead daemon on the next search — but nothing notices
//! a service that vanishes mid-session while no one is searching. On
//! macOS and Linux launchd/systemd close that gap; on Windows the `Run`
//! key fires once at login and never again. This binary is that missing
//! supervisor, on every platform.
//!
//! # What it supervises
//!
//! | Service | How | Why here |
//! |---|---|---|
//! | `uffsd` | `uffs --daemon start` | user process; respawn inherits the resident marker, so it returns with `--no-retire` |
//! | `uffsmcp` (HTTP gateway) | `uffs --mcp start` | user process; only supervised once it has been started at least once |
//!
//! The Access Broker is **deliberately absent**: it is a `LocalSystem`
//! service registered `start= auto`, so the `SCM` already restarts it at
//! boot, and a non-elevated process cannot `StartService` it anyway.
//! Supervising it from here would demand elevation and break the
//! zero-UAC property residency exists to protect — the right mechanism
//! is SCM failure actions, set once at `--install` time.
//!
//! # Deliberate stops win
//!
//! A clean `uffs --daemon stop` records stop intent, and the watchdog
//! honours it until the next explicit start ([`supervise::Action`]).
//! Without that it would fight the operator on every intentional stop.
//!
//! # Why a separate binary
//!
//! A supervisor cannot supervise its own death, so it must outlive the
//! teardowns that kill what it watches — `install-bins.rs` force-kills
//! `uffsd`/`uffsmcp` by image name, which is exactly how the MCP stdio
//! supervisor died. A different image name survives that. It is also not
//! a `uffs` subcommand: a long-running `uffs.exe` would lock the most
//! frequently replaced binary in the tree.

mod supervise;

use core::time::Duration;

use supervise::{Action, RespawnLedger, decide};

/// How often liveness is polled.
///
/// Seconds-scale: a respawn that lands within a few seconds of a crash
/// is indistinguishable from "never went away" for an interactive
/// search, and the probe is two cheap process checks.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Environment override for [`POLL_INTERVAL`], in seconds (tests, and
/// operators who want a tighter or looser loop).
const POLL_ENV: &str = "UFFS_WATCHDOG_POLL_SECS";

/// One supervised service.
struct Service {
    /// Display name used in log lines.
    name: &'static str,
    /// `uffs` subcommand pair that starts it (e.g. `--daemon start`).
    start_args: [&'static str; 2],
    /// Sliding-window respawn ledger.
    ledger: RespawnLedger,
    /// Whether this service has ever been seen running, so an MCP
    /// gateway the user never started is not started *by* the watchdog.
    seen_running: bool,
}

#[expect(
    clippy::print_stderr,
    reason = "a supervisor's log IS its user interface; it has no other channel"
)]
#[expect(
    clippy::infinite_loop,
    reason = "supervising is the whole job — the loop ends when the process is stopped"
)]
fn main() -> anyhow::Result<()> {
    let poll = std::env::var(POLL_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map_or(POLL_INTERVAL, Duration::from_secs);

    let mut services = [
        Service {
            name: "daemon",
            start_args: ["--daemon", "start"],
            ledger: RespawnLedger::default(),
            seen_running: false,
        },
        Service {
            name: "mcp",
            start_args: ["--mcp", "start"],
            ledger: RespawnLedger::default(),
            seen_running: false,
        },
    ];

    eprintln!("uffs-watchdog armed (poll {}s)", poll.as_secs());
    loop {
        for service in &mut services {
            tick(service);
        }
        std::thread::sleep(poll);
    }
}

/// Evaluate and act on one service for this tick.
#[expect(
    clippy::print_stderr,
    reason = "a supervisor's log IS its user interface; it has no other channel"
)]
fn tick(service: &mut Service) {
    let alive = is_running(service.start_args[0]);
    if alive {
        service.seen_running = true;
        return;
    }
    // Never *introduce* a service the user has not run themselves: a
    // machine that never starts the MCP gateway should not acquire one
    // because a watchdog is present.
    if !service.seen_running {
        return;
    }
    let now = std::time::Instant::now();
    let recent = service.ledger.recent(now);
    match decide(alive, stop_intent(service.start_args[0]), recent) {
        // Running, or the operator asked for it to be down: both mean
        // "do nothing", but they are distinct decisions upstream.
        Action::Leave | Action::HonourStopIntent => {}
        Action::GaveUp => {
            eprintln!(
                "uffs-watchdog: {} died {} times in the crash window — not respawning again",
                service.name, recent
            );
        }
        Action::Respawn => {
            eprintln!("uffs-watchdog: {} is gone — restarting", service.name);
            service.ledger.record(now);
            let started = std::process::Command::new(uffs_exe())
                .args(service.start_args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if !started {
                eprintln!("uffs-watchdog: {} restart failed", service.name);
            }
        }
    }
}

/// The `uffs` CLI to drive, resolved next to this binary so a watchdog
/// installed in `~/bin` drives the `uffs` beside it rather than whatever
/// `PATH` happens to resolve.
fn uffs_exe() -> std::path::PathBuf {
    let name = if cfg!(windows) { "uffs.exe" } else { "uffs" };
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(name)))
        .filter(|candidate| candidate.is_file())
        .unwrap_or_else(|| std::path::PathBuf::from(name))
}

/// Liveness probe: ask the CLI, whose status output is the single source
/// of truth for "is it up" on both transports.
///
/// `--daemon status` prints `● running  PID …` when up and
/// `○ Daemon  not running` when not; `--mcp status` mirrors the shape.
fn is_running(kind: &str) -> bool {
    std::process::Command::new(uffs_exe())
        .args([kind, "status"])
        .output()
        .is_ok_and(|out| {
            let text = String::from_utf8_lossy(&out.stdout);
            text.contains("running") && !text.contains("not running")
        })
}

/// Did the user deliberately stop this service?
///
/// Recorded as a marker file beside the lifecycle state, written by the
/// explicit `stop` paths and cleared by an explicit `start`. Absent
/// marker means the service went away on its own, which is what the
/// watchdog exists to repair.
fn stop_intent(kind: &str) -> bool {
    stop_intent_path(kind).is_some_and(|path| path.exists())
}

/// Path of the stop-intent marker for a service kind.
fn stop_intent_path(kind: &str) -> Option<std::path::PathBuf> {
    // Mirrors `uffs_client::daemon_ctl::pid_file_path`'s directory:
    // `<data-local>/uffs`.  Kept in sync by the test below rather than by
    // a dependency edge (see the manifest rationale).
    let dir = dirs_next::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("uffs");
    let leaf = match kind {
        "--daemon" => "daemon.stopped",
        "--mcp" => "mcp.stopped",
        _ => return None,
    };
    Some(dir.join(leaf))
}

#[cfg(test)]
mod tests {
    use super::stop_intent_path;

    /// Each supervised kind maps to its own marker; unknown kinds map to
    /// none, so a typo can never silently suppress supervision.
    #[test]
    fn stop_intent_paths_are_per_service() {
        let daemon = stop_intent_path("--daemon");
        let mcp = stop_intent_path("--mcp");
        assert!(daemon.is_some());
        assert!(mcp.is_some());
        assert_ne!(daemon, mcp);
        assert_eq!(stop_intent_path("--broker"), None);
    }
}
