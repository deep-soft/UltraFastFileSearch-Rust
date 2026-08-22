// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `uffs --daemon changed-since` — operator probe for the daemon's
//! USN-journal delta RPC (`changed_since`).
//!
//! Read-only: connects to the running daemon (never auto-starts one)
//! and prints the cursor + delta the RPC returns, so the journal-delta
//! capability can be exercised and field-validated without writing a
//! client. Two-step usage mirrors the cursor contract
//! ([`uffs_client::protocol::response::ChangedSinceResponse`]): a first
//! call with no cursor bootstraps (prints `journal_id` + `next_usn`), a
//! second call with those values prints every file changed in between.

use anyhow::{Context as _, Result};
use uffs_client::connect_sync::UffsClientSync;
use uffs_client::protocol::response::ChangedSinceParams;
use uffs_mft::platform::DriveLetter;

/// How many changed-file rows are printed before the rest is folded
/// into a `(… N more)` line. Keeps a big catch-up delta readable in a
/// terminal; the full set is what Phase-3 consumers read over the RPC,
/// not this probe.
const MAX_PRINTED_CHANGES: usize = 50;

/// `uffs --daemon changed-since <DRIVE> [--journal-id N] [--since-usn N]
/// [--max-records N]` — run one `changed_since` RPC and print the result.
///
/// # Errors
///
/// Returns an error when the daemon is not running or the RPC fails
/// (including `Unsupported` from a daemon on a platform without USN
/// journals).
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
pub(crate) fn daemon_changed_since(
    drive: DriveLetter,
    journal_id: u64,
    since_usn: i64,
    max_records: Option<u32>,
) -> Result<()> {
    let mut client = UffsClientSync::connect_raw()
        .map_err(|err| anyhow::anyhow!("Daemon is not running: {err}"))?;

    let params = ChangedSinceParams {
        drive,
        journal_id,
        since_usn,
        max_records,
    };
    let response = client
        .changed_since(&params)
        .with_context(|| "changed_since RPC failed")?;

    println!("Drive {}: USN journal delta", drive.as_char());
    println!("  journal_id: {}", response.journal_id);
    println!("  next_usn:   {}", response.next_usn);
    if !response.complete {
        let reason = if journal_id == 0 || since_usn == 0 {
            "no cursor given (bootstrap)"
        } else {
            "cursor stale — journal recreated or wrapped past it"
        };
        println!("  delta:      unavailable — {reason}");
        println!(
            "  Persist journal_id + next_usn above, change some files, then re-run:\n  \
             uffs --daemon changed-since {} --journal-id {} --since-usn {}",
            drive.as_char(),
            response.journal_id,
            response.next_usn
        );
        return Ok(());
    }

    println!(
        "  changed:    {} file(s){}",
        response.changes.len(),
        if response.truncated {
            " (truncated — re-run from next_usn for the remainder)"
        } else {
            ""
        }
    );
    for change in response.changes.iter().take(MAX_PRINTED_CHANGES) {
        println!(
            "    frs {:>12}{}",
            change.frs,
            if change.deleted { "  (deleted)" } else { "" }
        );
    }
    let hidden = response.changes.len().saturating_sub(MAX_PRINTED_CHANGES);
    if hidden > 0 {
        println!("    (… {hidden} more)");
    }
    Ok(())
}
