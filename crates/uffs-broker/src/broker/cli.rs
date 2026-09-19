// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Small CLI helpers for the broker binary: the usage banner and the
//! `--self-test-vss` argument lookup.
//!
//! Split out of `broker.rs` to keep that file under the workspace 800-LOC
//! ceiling, the same reason `service.rs`, `control.rs` and `pipe.rs` were
//! extracted before it.

/// Print CLI usage help to stderr.
///
/// Runs before the `tracing` subscriber is initialised, so uses `eprintln!`
/// directly — the usual logging channel isn't available yet.
#[cfg(windows)]
#[expect(
    clippy::print_stderr,
    reason = "CLI help text written before tracing subscriber init"
)]
pub(super) fn print_usage() {
    eprintln!(
        "uffs-broker: use --install, --uninstall, --repair, --status, --start, --stop, or --run"
    );
    eprintln!("  --install     Install as Windows Service");
    eprintln!("  --uninstall   Remove Windows Service");
    eprintln!("  --repair      Re-apply restart-on-failure settings to an installed service");
    eprintln!("  --status      Show service state, pid, and pipe-serving status");
    eprintln!("  --start       Start the service (waits for RUNNING)");
    eprintln!("  --stop        Stop the service (waits for STOPPED)");
    eprintln!("  --run         Run in foreground (debugging)");
    eprintln!("  --self-test-vss <dir>  Elevated smoke test: real VSS snapshot create/read/delete");
    eprintln!("  --version     Print version (also -V)");
}

/// Return the directory argument following `--self-test-vss`, if present.
#[cfg(windows)]
pub(super) fn self_test_vss_dir(args: &[String]) -> Option<std::path::PathBuf> {
    let flag_index = args.iter().position(|arg| arg == "--self-test-vss")?;
    args.get(flag_index + 1).map(std::path::PathBuf::from)
}
