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
//! # Liveness is read from JSON, never from prose
//!
//! Probing used to be `uffs --<service> status` scanned for the
//! substring `running` minus `not running`. That is wrong twice over,
//! and both bugs were observed in the field:
//!
//! * `uffs --mcp status` reports the daemon too, so a *stopped daemon* put
//!   `Daemon:  not running` in the *MCP* report and the watchdog concluded the
//!   healthy gateway had died. It then ran `uffs --mcp start`, whose preflight
//!   sees "gateway up, daemon down" and helpfully restarts the daemon —
//!   resurrecting the very daemon the operator had just stopped on purpose. The
//!   watchdog's own log showed `HonourStopIntent` throughout, because it never
//!   touched the daemon: it defeated the stop through the MCP.
//! * `◐ loading (3/7 drives)` contains neither string, so a daemon still
//!   reading the MFT read as dead and was liable to be respawned on top of
//!   itself.
//!
//! Liveness now comes from `uffs --status --json`, which reports every
//! service under its own key, so one service's state can never be
//! mistaken for another's. An unreadable probe means *unknown*, and
//! unknown is always left alone — a supervisor that restarts things
//! because it could not see them is worse than none.
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
/// search, and the probe is one cheap status call covering every
/// supervised service.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Environment override for [`POLL_INTERVAL`], in seconds (tests, and
/// operators who want a tighter or looser loop).
const POLL_ENV: &str = "UFFS_WATCHDOG_POLL_SECS";

/// One supervised service.
struct Service {
    /// Display name used in log lines.
    name: &'static str,
    /// Key under which `uffs --status --json` reports this service.
    /// Reading a named field is what keeps one service's state from
    /// being mistaken for another's (see the module docs).
    status_key: &'static str,
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
            status_key: "daemon",
            start_args: ["--daemon", "start"],
            ledger: RespawnLedger::default(),
            seen_running: false,
        },
        Service {
            name: "mcp",
            status_key: "mcp_http",
            start_args: ["--mcp", "start"],
            ledger: RespawnLedger::default(),
            seen_running: false,
        },
    ];

    eprintln!("uffs-watchdog armed (poll {}s)", poll.as_secs());
    loop {
        // One snapshot per tick, shared by every service: the probe is a
        // single subprocess rather than one per service, and every
        // decision in a tick is taken against the same instant.
        let snapshot = status_snapshot();
        for service in &mut services {
            tick(service, snapshot.as_deref());
        }
        std::thread::sleep(poll);
    }
}

/// Take one machine-readable snapshot of every service's liveness.
///
/// `uffs --status --json` connects with `connect_raw`, which never
/// auto-spawns anything, so probing has no side effects — an important
/// property for something that runs every few seconds forever.
fn status_snapshot() -> Option<String> {
    let out = std::process::Command::new(uffs_exe())
        .args(["--status", "--json"])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Is the service reported under `key` running, per a `--status --json`
/// document?
///
/// `None` means *unknown* — malformed JSON, a missing key, or a `uffs`
/// too old to report that service. Callers must treat unknown as "leave
/// it alone": absence of evidence is not evidence of death, and a
/// supervisor that respawns on a failed probe manufactures the outage
/// it exists to prevent.
fn running(doc: &str, key: &str) -> Option<bool> {
    serde_json::from_str::<serde_json::Value>(doc)
        .ok()?
        .get(key)?
        .get("running")?
        .as_bool()
}

/// Append one line to the watchdog log, beside the lifecycle state.
///
/// `resident on` spawns the watchdog with its stdio discarded, so
/// `eprintln!` alone leaves the supervisor's decisions invisible — which
/// made a stop-intent bug undiagnosable from the outside and cost two
/// wrong guesses before this existed. Every decision is now recorded
/// with the inputs that produced it.
fn log_line(message: &str) {
    let path = lifecycle_dir().join("watchdog.log");
    if let Some(parent) = path.parent() {
        let _ensure = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write as _;
        let _best_effort = writeln!(file, "{message}");
    }
}

/// The per-user lifecycle directory holding the PID file and markers.
fn lifecycle_dir() -> std::path::PathBuf {
    dirs_next::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("uffs")
}

/// Evaluate and act on one service for this tick.
#[expect(
    clippy::print_stderr,
    reason = "a supervisor's log IS its user interface; it has no other channel"
)]
fn tick(service: &mut Service, snapshot: Option<&str>) {
    // Unknown liveness is not death: leave the service exactly as it is
    // and try again next tick.
    let Some(alive) = snapshot.and_then(|doc| running(doc, service.status_key)) else {
        return;
    };
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
    let intent = stop_intent(service.start_args[0]);
    let action = decide(alive, intent, recent);
    log_line(&format!(
        "{} down: stop_intent={} (marker {}) recent_respawns={} -> {:?}",
        service.name,
        intent,
        stop_intent_path(service.start_args[0])
            .map_or_else(|| "?".to_owned(), |path| path.display().to_string()),
        recent,
        action,
    ));
    match action {
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
                // Tell the CLI this start is the supervisor's, not the
                // operator's, so it leaves any stop-intent marker alone.
                .env("UFFS_SUPERVISED_RESTART", "1")
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
    let dir = lifecycle_dir();
    let leaf = match kind {
        "--daemon" => "daemon.stopped",
        "--mcp" => "mcp.stopped",
        _ => return None,
    };
    Some(dir.join(leaf))
}

#[cfg(test)]
mod tests {
    use super::{running, stop_intent_path};

    /// A `uffs --status --json` document shaped like the real one: a
    /// live gateway while the daemon is deliberately stopped.
    const GATEWAY_UP_DAEMON_DOWN: &str = r#"{
        "daemon": { "running": false },
        "broker": { "running": true },
        "mcp_http": { "running": true, "pid": 2912, "endpoint": "http://127.0.0.1:8080/mcp" },
        "mcp_stdio": { "sessions": [] }
    }"#;

    /// Each service is read from its own key, so a stopped daemon can
    /// never be mistaken for a stopped gateway.
    ///
    /// Regression: the old probe scanned `uffs --mcp status` prose for
    /// `running` minus `not running`. That report names the daemon too,
    /// so stopping the daemon made the healthy gateway read as dead;
    /// the watchdog "restarted" the gateway, and the gateway's own
    /// preflight restarted the daemon — silently undoing a deliberate
    /// `uffs --daemon stop`.
    #[test]
    fn services_are_read_from_their_own_key() {
        assert_eq!(running(GATEWAY_UP_DAEMON_DOWN, "daemon"), Some(false));
        assert_eq!(running(GATEWAY_UP_DAEMON_DOWN, "mcp_http"), Some(true));
    }

    /// A daemon still loading its drives is alive: `--status --json`
    /// reports `running: true` from the moment it answers RPCs, so the
    /// watchdog cannot respawn a daemon on top of a starting one.
    #[test]
    fn a_loading_daemon_counts_as_running() {
        let loading = r#"{ "daemon": { "running": true,
            "status": { "status": { "Loading": { "loaded": 3, "total": 7 } } } } }"#;
        assert_eq!(running(loading, "daemon"), Some(true));
    }

    /// Anything unreadable is *unknown*, never "down" — the caller
    /// leaves unknown services alone rather than respawning them.
    #[test]
    fn unreadable_probes_are_unknown_not_dead() {
        assert_eq!(running("not json at all", "daemon"), None);
        assert_eq!(running("{}", "daemon"), None, "missing key");
        assert_eq!(running(r#"{"daemon":{}}"#, "daemon"), None, "missing field");
        assert_eq!(
            running(r#"{"daemon":{"running":"yes"}}"#, "daemon"),
            None,
            "non-boolean"
        );
    }

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
