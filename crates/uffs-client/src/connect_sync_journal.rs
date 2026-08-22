// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! USN-journal RPC helper for [`crate::connect_sync::UffsClientSync`].
//!
//! Split off [`crate::connect_sync`] so the journal RPC
//! (`changed_since`) lives next to the
//! [`crate::protocol::response::ChangedSinceResponse`] wire types it
//! consumes — the
//! same sibling-module pattern as [`crate::connect_sync_tiering`].
//! Paired with the daemon-side handler in
//! `crates/uffs-daemon/src/handler_journal.rs`.

use crate::connect_sync::UffsClientSync;
use crate::error::ClientError;
use crate::protocol::response::{ChangedSinceParams, ChangedSinceResponse};

impl UffsClientSync {
    /// Ask the daemon which files changed on a drive since a USN
    /// cursor, answered from the volume's NTFS USN journal.
    ///
    /// See [`ChangedSinceResponse`] for the cursor contract: a
    /// bootstrap / stale / wrapped cursor comes back with
    /// `complete == false` and a fresh cursor, and a `truncated == true`
    /// response is continued by calling again from its `next_usn`.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` on I/O, protocol, or timeout failure, and
    /// surfaces the daemon's own errors (invalid params; journal
    /// unsupported on non-NTFS/non-Windows daemons) as
    /// `ClientError::Protocol`.
    pub fn changed_since(
        &mut self,
        params: &ChangedSinceParams,
    ) -> Result<ChangedSinceResponse, ClientError> {
        let payload =
            serde_json::to_value(params).map_err(|err| ClientError::Protocol(err.to_string()))?;
        let result = self.send_request("changed_since", Some(payload))?;
        serde_json::from_value(result).map_err(|err| ClientError::Protocol(err.to_string()))
    }
}
