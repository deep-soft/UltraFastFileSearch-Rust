// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `changed_since` handler for [`super::RequestHandler`]: answers
//! *"which files changed on this drive since my USN cursor?"* straight
//! from the volume's NTFS USN journal.
//!
//! The daemon is the natural owner of this question — it already holds
//! the volume access (broker-adopted handles) and runs the per-shard
//! journal loops — and answering it as an RPC lets external consumers
//! (first: `uffs-content`'s watermark jobs) pay for the *delta* instead
//! of a full index or MFT sweep per pass. The cursor contract lives on
//! [`uffs_client::protocol::response::ChangedSinceResponse`]; this file
//! only maps it onto the `uffs_mft::usn` primitives.
//!
//! Lifted into a sibling file to keep `handler.rs` under the 800-line
//! policy ceiling — same `#[path]` re-attachment pattern as
//! `handler_blob.rs` / `handler_diff.rs`.
//!
//! Cross-platform by construction: the `uffs_mft::usn` entry points
//! exist on every platform and return `ErrorKind::Unsupported` where
//! USN journals don't (macOS/Linux), which this handler surfaces as a
//! JSON-RPC error rather than a panic or a silent empty delta.

use uffs_client::protocol::response::{ChangedSinceParams, ChangedSinceResponse, JournalChange};
use uffs_client::protocol::{
    ERR_INTERNAL, ERR_INVALID_PARAMS, RpcErrorResponse, RpcRequest, RpcResponse,
};

use super::RequestHandler;

/// Hard per-call cap on raw journal records read, and the default when
/// the request carries no `max_records`. One 64 KiB FSCTL batch holds
/// on the order of a thousand records, so this bounds a single call to
/// a few hundred reads and the aggregated JSON response to a few MB —
/// large enough that a day of ordinary desktop churn fits in one call,
/// small enough that a stale-cursor catch-up pages instead of stalling
/// the pipe with one giant response.
const MAX_RECORDS_PER_CALL: usize = 262_144;

impl RequestHandler {
    /// Handle the `changed_since` method.
    ///
    /// Strict on params: a request that doesn't deserialise (above all,
    /// a missing `drive`) is rejected with `ERR_INVALID_PARAMS` rather
    /// than best-effort-defaulted — silently treating a malformed
    /// cursor request as a bootstrap would hand the caller a fresh
    /// cursor and quietly swallow its delta.
    ///
    /// The journal FSCTLs are synchronous blocking I/O, so the read
    /// runs on the blocking pool, keeping the RPC loop responsive.
    pub(super) async fn handle_changed_since(&self, id: u64, req: &RpcRequest) -> String {
        let parsed: Option<ChangedSinceParams> = req
            .params
            .as_ref()
            .and_then(|val| serde_json::from_value(val.clone()).ok());
        let Some(params) = parsed else {
            return serde_json::to_string(&RpcErrorResponse::error(
                Some(id),
                ERR_INVALID_PARAMS,
                "changed_since requires params {drive, journal_id, since_usn[, max_records]}",
            ))
            .unwrap_or_default();
        };

        let outcome =
            tokio::task::spawn_blocking(move || changed_since_from_journal(&params)).await;
        match outcome {
            Ok(Ok(response)) => {
                let result = serde_json::to_value(&response).unwrap_or_default();
                serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
            }
            Ok(Err(err)) => serde_json::to_string(&RpcErrorResponse::error(
                Some(id),
                ERR_INTERNAL,
                &format!("changed_since failed: {err}"),
            ))
            .unwrap_or_default(),
            Err(join_err) => serde_json::to_string(&RpcErrorResponse::error(
                Some(id),
                ERR_INTERNAL,
                &format!("changed_since worker failed: {join_err}"),
            ))
            .unwrap_or_default(),
        }
    }
}

/// Answer a `changed_since` request from the drive's USN journal.
///
/// Implements the cursor contract documented on
/// [`uffs_client::protocol::response::ChangedSinceResponse`]:
///
/// 1. **Bootstrap / stale / wrapped cursor** → `complete == false`, empty
///    changes, fresh cursor at the journal's current position. The delta from
///    the old cursor is unknowable; the caller falls back to its own full pass.
/// 2. **Valid cursor** → bounded read from it, per-FRS aggregation, `complete
///    == true`; `truncated == true` when the per-call record cap stopped the
///    read early (continue from `next_usn`).
///
/// # Errors
///
/// Propagates the underlying journal I/O error — including
/// `ErrorKind::Unsupported` on platforms without USN journals — for the
/// handler to surface as a JSON-RPC error.
fn changed_since_from_journal(
    params: &ChangedSinceParams,
) -> Result<ChangedSinceResponse, std::io::Error> {
    let info = uffs_mft::usn::query_usn_journal(params.drive)?;

    let bootstrap = params.journal_id == 0 || params.since_usn == 0;
    let stale_journal = !bootstrap && params.journal_id != info.journal_id;
    let since = uffs_mft::usn::Usn::new(params.since_usn);
    let wrapped = !bootstrap && !stale_journal && since < info.first_usn;
    if bootstrap || stale_journal || wrapped {
        tracing::info!(
            drive = %params.drive.as_char(),
            bootstrap,
            stale_journal,
            wrapped,
            "changed_since: issuing fresh cursor (delta unknowable)"
        );
        return Ok(ChangedSinceResponse {
            drive: Some(params.drive),
            journal_id: info.journal_id,
            next_usn: info.next_usn.raw(),
            complete: false,
            truncated: false,
            changes: Vec::new(),
        });
    }

    let cap = params
        .max_records
        .map_or(MAX_RECORDS_PER_CALL, |requested| {
            usize::try_from(requested)
                .unwrap_or(MAX_RECORDS_PER_CALL)
                .min(MAX_RECORDS_PER_CALL)
        });
    let (records, next_usn, exhausted) =
        uffs_mft::usn::read_usn_journal_bounded(params.drive, info.journal_id, since, cap)?;
    let aggregated = uffs_mft::usn::aggregate_changes(&records);
    let mut changes: Vec<JournalChange> = aggregated
        .into_values()
        .map(|change| JournalChange {
            frs: change.frs.raw(),
            deleted: change.deleted,
        })
        .collect();
    changes.sort_unstable_by_key(|change| change.frs);

    tracing::info!(
        drive = %params.drive.as_char(),
        raw_records = records.len(),
        changed_files = changes.len(),
        exhausted,
        "changed_since: delta served from the USN journal"
    );
    Ok(ChangedSinceResponse {
        drive: Some(params.drive),
        journal_id: info.journal_id,
        next_usn: next_usn.raw(),
        complete: true,
        truncated: !exhausted,
        changes,
    })
}

// Windows runs the real FSCTL path (exercised by the elevated
// `--ignored` suite); only the non-Windows stub behavior is testable
// here, so the whole module is gated off Windows builds.
#[cfg(test)]
#[cfg(not(windows))]
mod tests {
    use super::*;

    /// On platforms without USN journals the RPC must surface a real
    /// `Unsupported` error — never a silent empty delta a caller could
    /// mistake for "nothing changed".
    #[test]
    fn non_windows_reports_unsupported_not_empty_delta() {
        let params = ChangedSinceParams {
            drive: uffs_mft::platform::DriveLetter::try_from('C').expect("static drive letter"),
            journal_id: 0,
            since_usn: 0,
            max_records: None,
        };
        let err = changed_since_from_journal(&params)
            .expect_err("USN journals do not exist on this platform");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::Unsupported,
            "the platform stub must say Unsupported, got: {err}"
        );
    }
}
