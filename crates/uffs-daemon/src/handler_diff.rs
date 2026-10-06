// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Snapshot-diff error mapping for [`super::RequestHandler`], plus the
//! logged `search`-request helpers built on top of
//! [`RequestHandler::search_or_diff`].
//!
//! Lifted out of `handler.rs` to keep that file under the 800-line policy
//! ceiling. Re-attached via `#[path = "handler_diff.rs"] mod diff_handler;`, so
//! every item stays an `impl RequestHandler` method the search handler calls
//! as `self.diff_search_response(...)` / `self.auto_load_missing_drives(...)`
//! / `self.run_search_or_diff(...)`.
//!
//! The diff itself lives in `IndexManager::diff_search` (`crate::index::diff`);
//! this only maps its [`crate::index::diff::DiffError`] setup failures onto the
//! JSON-RPC error envelope.

use uffs_client::protocol::response::SearchResponse;
use uffs_client::protocol::{
    ERR_INTERNAL, ERR_INVALID_PARAMS, ERR_NOT_READY, RpcErrorResponse, SearchParams,
};

use super::RequestHandler;
use crate::index::diff::DiffError;
use crate::index::search::SearchFailure;

impl RequestHandler {
    /// Resolve a search request to its response: a snapshot diff when
    /// `params.diff_baseline` is set (via [`Self::diff_search_response`], which
    /// may yield a JSON-RPC error string), or an ordinary live search.
    pub(super) async fn search_or_diff(
        &self,
        id: u64,
        params: &SearchParams,
    ) -> Result<SearchResponse, String> {
        if params.diff_baseline.is_some() {
            self.diff_search_response(id, params).await
        } else {
            self.index
                .search(params)
                .await
                .map_err(|failure| search_failure_json(id, &failure))
        }
    }

    /// Auto-load `drives` from `data_dir` before a search, timing the
    /// load and warn-logging any drive that couldn't be auto-loaded —
    /// split out of `handler.rs::handle_search` to keep that function's
    /// cognitive complexity down.
    pub(super) async fn auto_load_missing_drives(
        &self,
        drives: &[uffs_mft::platform::DriveLetter],
    ) {
        if drives.is_empty() {
            return;
        }
        let load_started = std::time::Instant::now();
        let missing = self.index.ensure_drives_loaded(drives, false).await;
        tracing::info!(
            ?drives,
            elapsed_ms = load_started.elapsed().as_millis(),
            "search: ensure_drives_loaded complete"
        );
        if !missing.is_empty() {
            tracing::warn!(
                missing_drives = ?missing,
                "Some requested drives could not be auto-loaded"
            );
        }
    }

    /// Run [`Self::search_or_diff`] for `search_params`, logging its
    /// elapsed time and outcome (row count on success) — split out of
    /// `handler.rs::handle_search` to keep that function's cognitive
    /// complexity down. `started` is `handle_search`'s own request-start
    /// timer, so the logged elapsed time covers the drive auto-load step
    /// too, not just this call.
    pub(super) async fn run_search_or_diff(
        &self,
        id: u64,
        search_params: &SearchParams,
        started: std::time::Instant,
    ) -> Result<(SearchResponse, usize), String> {
        match self.search_or_diff(id, search_params).await {
            Ok(response) => {
                let row_count = response.payload.row_count_hint().unwrap_or(0);
                tracing::info!(
                    row_count,
                    elapsed_ms = started.elapsed().as_millis(),
                    "search: search_or_diff complete"
                );
                Ok((response, row_count))
            }
            Err(error_json) => {
                tracing::warn!(
                    elapsed_ms = started.elapsed().as_millis(),
                    "search: search_or_diff failed"
                );
                Err(error_json)
            }
        }
    }

    /// Run a snapshot-diff search, returning the response or a pre-serialized
    /// JSON-RPC error string for the setup failures (no drive / drive not
    /// loaded / baseline unreadable).
    async fn diff_search_response(
        &self,
        id: u64,
        params: &SearchParams,
    ) -> Result<SearchResponse, String> {
        match self.index.diff_search(params).await {
            Ok(response) => Ok(response),
            Err(DiffError::NoDrive) => Err(serde_json::to_string(&RpcErrorResponse::error(
                Some(id),
                ERR_INVALID_PARAMS,
                "diff: `--drive <LETTER>` is required (which live drive to diff against)",
            ))
            .unwrap_or_default()),
            Err(DiffError::DriveNotLoaded(letter)) => {
                Err(serde_json::to_string(&RpcErrorResponse::error(
                    Some(id),
                    ERR_NOT_READY,
                    &format!(
                        "diff: drive {letter} is not loaded; load it first \
                         (`uffs --daemon load --drive {letter}`)"
                    ),
                ))
                .unwrap_or_default())
            }
            Err(DiffError::BaselineLoad { path, source }) => {
                Err(serde_json::to_string(&RpcErrorResponse::error(
                    Some(id),
                    ERR_INTERNAL,
                    &format!("diff: could not load baseline '{path}': {source}"),
                ))
                .unwrap_or_default())
            }
            Err(DiffError::Search(failure)) => Err(search_failure_json(id, &failure)),
        }
    }
}

/// Serialise a [`SearchFailure`] as the JSON-RPC error the client should
/// see: the failure's own code (`ERR_SEARCH_TIMEOUT` / `ERR_SEARCH_BUSY` /
/// `ERR_INTERNAL`) and its operator-facing message.
///
/// Shared by the live search, the snapshot diff, and `facet_values`, so
/// every path that runs a scan reports a non-completion the same way.
pub(crate) fn search_failure_json(id: u64, failure: &SearchFailure) -> String {
    serde_json::to_string(&RpcErrorResponse::error(
        Some(id),
        failure.rpc_code(),
        &failure.to_string(),
    ))
    .unwrap_or_default()
}
