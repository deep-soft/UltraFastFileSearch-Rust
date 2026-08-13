// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! The pure supervision policy: what to do about a service that is not
//! running, and how often we are willing to do it.
//!
//! Kept free of process spawning and clocks-by-default so the decisions
//! are unit-testable on every platform — the same reason
//! `uffs-daemon::cache::policy` keeps `next_state_for_idle` pure.

use core::time::Duration;

/// Respawns tolerated inside [`CRASH_WINDOW`] before the watchdog stops
/// trying and says so.
///
/// Mirrors the MCP supervisor's crash hatch: a service that dies
/// immediately on every start is broken in a way respawning cannot fix,
/// and an unbounded retry loop turns that into a fork bomb that also
/// buries the real error in log noise.
pub(crate) const RESPAWN_LIMIT: usize = 3;

/// The window [`RESPAWN_LIMIT`] applies over.
pub(crate) const CRASH_WINDOW: Duration = Duration::from_secs(60);

/// What the watchdog should do about one service this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// Running (or not configured): do nothing.
    Leave,
    /// Gone, and the user did not ask for that: start it again.
    Respawn,
    /// Gone because the user deliberately stopped it: honour that.
    ///
    /// Mirrors launchd's `KeepAlive.SuccessfulExit = false` — a clean
    /// `uffs --daemon stop` must stick, or the watchdog fights the
    /// operator every time they stop something on purpose.
    HonourStopIntent,
    /// Gone, but it has already been respawned too many times in the
    /// window: give up loudly rather than crash-loop.
    GaveUp,
}

/// Decide what to do about one service.
///
/// `alive` is the liveness probe result, `stop_intent` whether the user
/// asked for it to be down, and `recent_respawns` how many times it has
/// been restarted inside [`CRASH_WINDOW`].
pub(crate) const fn decide(alive: bool, stop_intent: bool, recent_respawns: usize) -> Action {
    if alive {
        return Action::Leave;
    }
    if stop_intent {
        return Action::HonourStopIntent;
    }
    if recent_respawns >= RESPAWN_LIMIT {
        return Action::GaveUp;
    }
    Action::Respawn
}

/// Sliding-window respawn counter for a single service.
///
/// Holds the instants of recent respawns and prunes those older than
/// [`CRASH_WINDOW`], so a service that dies once an hour is restarted
/// every time while one that dies in a tight loop is abandoned.
#[derive(Debug, Default)]
pub(crate) struct RespawnLedger {
    /// Respawn timestamps still inside the window.
    events: Vec<std::time::Instant>,
}

impl RespawnLedger {
    /// Drop entries older than the window and report how many remain.
    pub(crate) fn recent(&mut self, now: std::time::Instant) -> usize {
        self.events
            .retain(|at| now.duration_since(*at) < CRASH_WINDOW);
        self.events.len()
    }

    /// Record a respawn that just happened.
    pub(crate) fn record(&mut self, now: std::time::Instant) {
        self.events.push(now);
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, CRASH_WINDOW, RESPAWN_LIMIT, RespawnLedger, decide};

    /// A live service is never touched, whatever else is true.
    #[test]
    fn alive_is_always_left_alone() {
        assert_eq!(decide(true, false, 0), Action::Leave);
        assert_eq!(decide(true, true, RESPAWN_LIMIT + 5), Action::Leave);
    }

    /// A deliberate stop outranks respawning — this is the property that
    /// keeps the watchdog from fighting the operator.
    #[test]
    fn deliberate_stop_is_honoured_over_respawn() {
        assert_eq!(decide(false, true, 0), Action::HonourStopIntent);
    }

    /// A vanished service with no stop intent comes back, until the
    /// window limit is reached.
    #[test]
    fn respawns_until_the_window_limit() {
        assert_eq!(decide(false, false, 0), Action::Respawn);
        assert_eq!(decide(false, false, RESPAWN_LIMIT - 1), Action::Respawn);
        assert_eq!(decide(false, false, RESPAWN_LIMIT), Action::GaveUp);
    }

    /// The ledger forgets respawns once they age out, so a service that
    /// dies rarely is always restarted.
    #[test]
    fn ledger_prunes_outside_the_window() {
        let mut ledger = RespawnLedger::default();
        let start = std::time::Instant::now();
        for _ in 0..RESPAWN_LIMIT {
            ledger.record(start);
        }
        assert_eq!(ledger.recent(start), RESPAWN_LIMIT, "all inside the window");

        let later = start
            .checked_add(CRASH_WINDOW)
            .and_then(|at| at.checked_add(core::time::Duration::from_secs(1)))
            .unwrap_or(start);
        assert_eq!(ledger.recent(later), 0, "all aged out");
    }
}
