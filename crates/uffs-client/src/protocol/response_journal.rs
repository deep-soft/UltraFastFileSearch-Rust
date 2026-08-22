// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! USN-journal RPC wire types: `changed_since`.
//!
//! The daemon owns live volume access (broker handles, journal loops);
//! this RPC lets any client ask it the incremental question directly —
//! *"which files changed on this drive since my cursor?"* — answered
//! from the NTFS USN journal instead of a full index sweep. First
//! consumer: `uffs-content`'s watermark jobs, which today pay a full
//! snapshot-MFT parse per pass to find a delta the journal already
//! knows. The capability is first-class daemon surface, not shaped
//! around any one consumer.
//!
//! Split into this sibling file to keep [`super::response`] under the
//! workspace 800-LOC policy ceiling — same precedent as
//! [`super::response_status`] and [`super::response_tiering`].
//!
//! ## Cursor contract
//!
//! A cursor is the pair (`journal_id`, `usn`). `journal_id == 0` (or
//! `since_usn == 0`) means *bootstrap*: the caller has no prior cursor
//! and the daemon replies with the journal's **current** position
//! (`complete == false`, empty `changes`) for the caller to persist —
//! its first real delta comes on the next call. A cursor whose
//! `journal_id` no longer matches the volume's, or whose USN has been
//! overwritten by journal wrap, is likewise answered with
//! `complete == false` and a fresh cursor: the delta between the old
//! cursor and now is **unknowable**, and the caller must fall back to
//! whatever full pass its use case defines before trusting deltas
//! again.
//!
//! All types serialise to / deserialise from JSON with `snake_case`
//! field names, matching every other RPC cluster.

use serde::{Deserialize, Serialize};

/// Parameters for the `changed_since` method.
///
/// `drive` is mandatory (a malformed or drive-less request is rejected
/// with `ERR_INVALID_PARAMS`, never silently treated as a bootstrap);
/// the cursor fields default to the bootstrap sentinel `0`.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangedSinceParams {
    /// Drive whose journal to read.
    pub drive: uffs_mft::platform::DriveLetter,
    /// Journal identity the cursor belongs to. `0` = bootstrap (no
    /// prior cursor).
    #[serde(default)]
    pub journal_id: u64,
    /// USN cursor: read changes strictly after this journal position.
    /// Meaningful only alongside a non-zero [`Self::journal_id`].
    #[serde(default)]
    pub since_usn: i64,
    /// Upper bound on raw journal records read in this one call.
    /// `None` ⇒ the daemon's own cap; values above that cap are
    /// clamped. When the bound stops the read early the response says
    /// `truncated == true` and the caller continues from its
    /// `next_usn`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_records: Option<u32>,
}

/// One changed file in a [`ChangedSinceResponse`].
///
/// Deliberately minimal: the journal authoritatively knows *which* FRS
/// changed and whether the file is now gone; every richer per-file fact
/// (sizes, timestamps, `$STANDARD_INFORMATION` `ChangeTime`, security
/// ids) is the caller's to resolve against whatever record source its
/// consistency model requires — the live MFT, a VSS snapshot, or the
/// daemon's own index.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct JournalChange {
    /// File Record Segment number (48-bit FRS index, sequence masked
    /// off — the same convention as the daemon's index `frs` column).
    pub frs: u64,
    /// The aggregated change stream for this FRS ends in a delete.
    #[serde(default)]
    pub deleted: bool,
}

/// Response for the `changed_since` method.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangedSinceResponse {
    /// Drive the response describes (echo of the request).
    #[serde(default)]
    pub drive: Option<uffs_mft::platform::DriveLetter>,
    /// The volume's current journal identity. Persist alongside
    /// [`Self::next_usn`] — a cursor is only meaningful under the
    /// journal id it was issued for.
    #[serde(default)]
    pub journal_id: u64,
    /// The caller's next cursor position.
    #[serde(default)]
    pub next_usn: i64,
    /// `true` ⇒ [`Self::changes`] is the complete delta from the
    /// caller's cursor up to [`Self::next_usn`]. `false` ⇒ the delta
    /// was unknowable (bootstrap, journal-id change, or journal wrap):
    /// `changes` is empty, `next_usn`/`journal_id` are a fresh cursor,
    /// and the caller must run its own full pass before trusting
    /// deltas again.
    #[serde(default)]
    pub complete: bool,
    /// `true` ⇒ the per-call record bound stopped the read early;
    /// call again from [`Self::next_usn`] for the remainder. Only ever
    /// `true` alongside `complete == true`.
    #[serde(default)]
    pub truncated: bool,
    /// The changed files, one entry per FRS (multiple journal records
    /// for one file are aggregated), sorted by FRS for a deterministic
    /// wire order.
    #[serde(default)]
    pub changes: Vec<JournalChange>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The params wire shape: `drive` is mandatory, the cursor fields
    /// default to the bootstrap sentinel, and `max_records` stays off
    /// the wire when unset.
    #[test]
    fn params_defaults_and_required_drive() {
        let minimal: ChangedSinceParams =
            serde_json::from_str(r#"{"drive":"C"}"#).expect("drive alone is a valid request");
        assert_eq!(
            minimal.journal_id, 0,
            "journal_id must default to bootstrap"
        );
        assert_eq!(minimal.since_usn, 0, "since_usn must default to bootstrap");
        assert_eq!(
            minimal.max_records, None,
            "max_records must default to None"
        );

        let missing_drive = serde_json::from_str::<ChangedSinceParams>(r#"{"journal_id":7}"#);
        assert!(
            missing_drive.is_err(),
            "a request without a drive must be rejected, not defaulted"
        );

        let encoded = serde_json::to_string(&minimal).expect("params serialise");
        assert!(
            !encoded.contains("max_records"),
            "unset max_records must stay off the wire, got: {encoded}"
        );
    }

    /// Response round-trip: every field survives, and the `changes`
    /// entries keep their per-file shape.
    #[test]
    fn response_round_trips() {
        let response = ChangedSinceResponse {
            drive: Some(
                uffs_mft::platform::DriveLetter::try_from('D').expect("static drive letter"),
            ),
            journal_id: 0xDEAD_BEEF,
            next_usn: 123_456_789,
            complete: true,
            truncated: true,
            changes: vec![
                JournalChange {
                    frs: 42,
                    deleted: false,
                },
                JournalChange {
                    frs: 4096,
                    deleted: true,
                },
            ],
        };
        let encoded = serde_json::to_string(&response).expect("response serialises");
        let decoded: ChangedSinceResponse =
            serde_json::from_str(&encoded).expect("response deserialises");
        assert_eq!(decoded, response, "wire round-trip must be lossless");
    }

    /// A bootstrap-style response deserialises from the minimal JSON a
    /// future daemon might send — every field is defaulted, so adding
    /// fields later cannot break old clients.
    #[test]
    fn response_tolerates_minimal_json() {
        let decoded: ChangedSinceResponse =
            serde_json::from_str("{}").expect("all response fields must be defaultable");
        assert!(!decoded.complete, "complete must default to false");
        assert!(decoded.changes.is_empty(), "changes must default to empty");
    }
}
