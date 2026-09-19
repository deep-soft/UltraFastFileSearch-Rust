// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! SCM failure-recovery configuration for the broker service.
//!
//! # Why this exists
//!
//! A service that dies without reporting `SERVICE_STOPPED` is, to the SCM,
//! simply gone: it logs event 7034 ("terminated unexpectedly") and — unless
//! recovery actions are configured — takes **no action at all**.
//!
//! On 2026-09-04 an unrelated automation agent ran
//! `Stop-Process -Name uffs-broker -Force` while cleaning up a stuck
//! scheduled task.  The broker exited with `0xFFFFFFFF` (the code
//! `Stop-Process -Force` produces), the SCM logged 7034, and nothing brought
//! it back.  The broker stayed down for **about seven hours**, until a human
//! restarted it by hand.  Every non-elevated MFT read on the box failed for
//! that whole window, because the broker is what vends the elevated volume
//! handle.
//!
//! Recovery actions fire on exactly that class of death — a kill, a crash, an
//! OOM — so configuring them turns a seven-hour outage into a five-second
//! one.  They are cheap, they are a property of the service registration
//! (not of our code), and nothing about them can make a healthy broker
//! misbehave.
//!
//! # What gets configured
//!
//! * Three escalating restarts (5 s, 10 s, 60 s) then stop trying, so a broker
//!   that is broken rather than merely killed does not spin forever.
//! * A 24-hour reset window: a service that has run cleanly for a day starts
//!   its failure count fresh, so three unlucky kills spread over a month do not
//!   exhaust the ladder.
//! * The **failure flag**, which is the part that is easy to miss: by default
//!   the SCM applies recovery actions only to an *unexpected* termination.  A
//!   service that reports `SERVICE_STOPPED` with a non-zero exit code is
//!   considered to have stopped on purpose and is left alone. Setting the flag
//!   extends recovery to that case too, which is what makes
//!   [`super::service::service_main`]'s non-zero exit on a failed serve loop
//!   actually restart the broker instead of quietly ending it.

/// Failure-count reset window, in seconds (24 h).
///
/// After this much trouble-free uptime the SCM zeroes the failure counter, so
/// the restart ladder below applies to a *burst* of failures rather than to
/// three unrelated incidents spread across weeks.
const RESET_PERIOD_SECS: u32 = 86_400;

/// Delay before the first restart attempt (5 s) — long enough for a killed
/// process's handles to be released, short enough to be invisible in practice.
const FIRST_RESTART_DELAY_MS: u32 = 5_000;

/// Delay before the second restart attempt (10 s).
const SECOND_RESTART_DELAY_MS: u32 = 10_000;

/// Delay before the third restart attempt (60 s) — the "something is actually
/// wrong" backstop, spaced out so a crash-looping broker does not hammer the
/// box or flood the event log.
const THIRD_RESTART_DELAY_MS: u32 = 60_000;

/// Build the `actions=` value for `sc.exe failure`.
///
/// Format is `type/delay-ms` triples joined by `/`, where the final entry also
/// governs every subsequent failure.  We deliberately stop escalating after
/// three restarts rather than appending a `run/` or `reboot/` action: the
/// broker is a convenience layer (its absence means UAC prompts, not data
/// loss), so rebooting the user's machine over it would be wildly
/// disproportionate.
fn restart_actions() -> String {
    format!(
        "restart/{FIRST_RESTART_DELAY_MS}/restart/{SECOND_RESTART_DELAY_MS}/restart/{THIRD_RESTART_DELAY_MS}"
    )
}

/// Build the full argument vector for `sc.exe failure <service> …`.
///
/// Split out from the spawn so the exact argv is unit-testable without a
/// Windows box or an elevated shell — the same reason
/// [`super::service::absent_service_signal`] is a free function.
///
/// `sc.exe` parses `reset=` and `actions=` as option *names* whose values are
/// separate argv elements (the trailing `=` belongs to the name).  This is the
/// same quirk the existing `binPath=` call in `install_service` handles, and
/// getting it wrong yields a cryptic usage dump rather than an error.
fn failure_argv(service: &str) -> Vec<String> {
    vec![
        String::from("failure"),
        service.to_owned(),
        String::from("reset="),
        RESET_PERIOD_SECS.to_string(),
        String::from("actions="),
        restart_actions(),
    ]
}

/// Build the argument vector for `sc.exe failureflag <service> 1`.
///
/// See the module docs for why the flag matters: without it, a non-zero exit
/// that was *reported* to the SCM is treated as a deliberate stop and no
/// recovery action runs.
fn failure_flag_argv(service: &str) -> Vec<String> {
    vec![
        String::from("failureflag"),
        service.to_owned(),
        String::from("1"),
    ]
}

/// Apply the restart ladder + failure flag to an installed service.
///
/// Requires Administrator (the caller checks).  Idempotent: re-applying the
/// same configuration to a service that already has it is a no-op success, so
/// this is safe to call from both `--install` and `--repair`.
///
/// # Errors
///
/// Returns an error if either `sc.exe` invocation cannot be spawned or exits
/// non-zero, with the combined `sc` output for the operator.
#[cfg(windows)]
pub(super) fn configure_recovery_actions(service: &str) -> anyhow::Result<()> {
    for argv in [failure_argv(service), failure_flag_argv(service)] {
        let output = std::process::Command::new("sc.exe").args(&argv).output()?;
        if !output.status.success() {
            // AUDIT-OK(bytes): `sc` output surfaced verbatim to the operator —
            // display only, never parsed or matched on.
            anyhow::bail!(
                "sc.exe {} failed: {}",
                argv.first().map_or("failure", String::as_str),
                super::service::sc_output(&output)
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{failure_argv, failure_flag_argv, restart_actions};

    /// The ladder escalates 5 s → 10 s → 60 s and stops there (three restarts,
    /// no reboot action). Locks in the delays so a well-meaning edit cannot
    /// quietly turn the broker into a crash-loop that hammers the box.
    #[test]
    fn restart_ladder_escalates_and_stops_at_three() {
        assert_eq!(
            restart_actions(),
            "restart/5000/restart/10000/restart/60000"
        );
        assert_eq!(
            restart_actions().matches("restart").count(),
            3,
            "exactly three restart attempts, then give up"
        );
        assert!(
            !restart_actions().contains("reboot"),
            "never reboot the user's machine over a convenience service"
        );
    }

    /// `sc.exe` needs `reset=` / `actions=` as option names with the value in
    /// the NEXT argv element. Passing `reset=86400` as one element makes
    /// `sc` print usage and exit non-zero, so this shape is the contract.
    #[test]
    fn failure_argv_keeps_sc_option_names_and_values_separate() {
        let argv = failure_argv("UffsAccessBroker");
        assert_eq!(argv, vec![
            "failure",
            "UffsAccessBroker",
            "reset=",
            "86400",
            "actions=",
            "restart/5000/restart/10000/restart/60000",
        ]);
    }

    /// The failure flag is what extends recovery to a reported non-zero exit —
    /// without it the serve-loop failure path stays dead. Value must be `1`.
    #[test]
    fn failure_flag_argv_enables_the_flag() {
        assert_eq!(failure_flag_argv("UffsAccessBroker"), vec![
            "failureflag",
            "UffsAccessBroker",
            "1"
        ]);
    }
}
