// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! JSON-RPC request handler: dispatches methods to [`IndexManager`].

use uffs_client::protocol::response::{
    FacetValuesParams, FacetValuesResponse, LoadDriveParams, LoadDriveResponse, RefreshParams,
    SearchResponse,
};
use uffs_client::protocol::{
    AggregateSpecWire, ERR_INVALID_PARAMS, ERR_METHOD_NOT_FOUND, RpcErrorResponse, RpcRequest,
    RpcResponse, SearchParams,
};

/// Maximum pattern length to prevent regex `DoS` (`S4.4.3`).
const MAX_PATTERN_LENGTH: usize = 4096;

use crate::index::IndexManager;
use crate::lifecycle::LifecycleHandle;

/// Request handler holding shared daemon state.
pub(crate) struct RequestHandler {
    /// Shared index manager.
    pub index: alloc::sync::Arc<IndexManager>,
    /// Lifecycle handle for idle timer, shutdown, connections.
    pub lifecycle: LifecycleHandle,
}

impl RequestHandler {
    /// Handle a single JSON-RPC request and return a JSON response string.
    pub(crate) async fn handle(&self, req: &RpcRequest) -> String {
        // Every incoming request — search, drives, status, keepalive, etc. —
        // extends the daemon's sliding-window idle deadline.  This is the
        // single authoritative call site; individual handlers do not need to
        // repeat it (keepalive still calls it for documentation clarity, but
        // that is idempotent: `notify_one` stores at most one permit).
        self.lifecycle.reset_idle_timer();

        let id = req.id.unwrap_or(0_u64);
        let connections = self.lifecycle.active_connections();

        match req.method.as_str() {
            "search" => self.handle_search(id, req).await,
            "drives" => self.handle_drives(id).await,
            "status" => self.handle_status(id, connections).await,
            "search_cli" => self.handle_search_cli(id, req).await,
            "stats" => self.handle_stats(id).await,
            "info" => self.handle_info(id, req).await,
            "load_drive" => self.handle_load_drive(id, req).await,
            "refresh" => self.handle_refresh(id, req),
            "facet_values" => self.handle_facet_values(id, req).await,
            "keepalive" => self.handle_keepalive(id, req),
            "shutdown" => self.handle_shutdown(id, req),
            _ => serde_json::to_string(&RpcErrorResponse::error(
                Some(id),
                ERR_METHOD_NOT_FOUND,
                &format!("Method not found: {}", req.method),
            ))
            .unwrap_or_default(),
        }
    }

    /// Handle `search` method.
    #[expect(
        clippy::cognitive_complexity,
        reason = "handler dispatch with JSON-RPC routing, error handling"
    )]
    async fn handle_search(&self, id: u64, req: &RpcRequest) -> String {
        let search_params: SearchParams = match req
            .params
            .as_ref()
            .and_then(|val| serde_json::from_value(val.clone()).ok())
        {
            Some(parsed) => parsed,
            None => {
                return serde_json::to_string(&RpcErrorResponse::error(
                    Some(id),
                    ERR_INVALID_PARAMS,
                    "Missing or invalid search params",
                ))
                .unwrap_or_default();
            }
        };

        // S4.4.3: Reject overly long patterns (regex DoS prevention)
        if search_params.pattern.len() > MAX_PATTERN_LENGTH {
            return serde_json::to_string(&RpcErrorResponse::error(
                Some(id),
                ERR_INVALID_PARAMS,
                &format!(
                    "Pattern too long ({} chars, max {MAX_PATTERN_LENGTH})",
                    search_params.pattern.len()
                ),
            ))
            .unwrap_or_default();
        }

        // Auto-load missing drives from data_dir before searching.
        if !search_params.drives.is_empty() {
            let missing = self
                .index
                .ensure_drives_loaded(&search_params.drives, false)
                .await;
            if !missing.is_empty() {
                tracing::warn!(
                    missing_drives = ?missing,
                    "Some requested drives could not be auto-loaded"
                );
            }
        }

        let mut response = self.index.search(&search_params).await;
        let row_count = response.rows.len();

        // Path-only single-buffer fast path (see `try_pack_paths_blob`).
        Self::try_pack_paths_blob(&search_params, &mut response);

        // D5.1: adaptive routing — use shmem for large result sets.
        // Only fires when the path-only fast path did not already
        // claim the payload.
        let mut shmem_ms: u128 = 0;
        if response.paths_blob.is_none() && row_count > uffs_client::shmem::SHMEM_THRESHOLD {
            let t_shmem = std::time::Instant::now();
            match uffs_client::shmem::write_search_results(
                &response.rows,
                response.duration_ms,
                response.records_scanned as u64,
                response.truncated,
            ) {
                Ok(path) => {
                    shmem_ms = t_shmem.elapsed().as_millis();
                    let count = row_count as u64;
                    let path_str = path.to_string_lossy().into_owned();
                    tracing::info!(
                        rows = row_count,
                        shmem_write_ms = shmem_ms,
                        path = %path_str,
                        "🗂️ shmem: wrote bulk results"
                    );
                    response.shmem_path = Some(path_str);
                    response.shmem_count = Some(count);
                    // Clear inline rows — data is in shmem now.
                    response.rows = Vec::new();
                }
                Err(shmem_err) => {
                    shmem_ms = t_shmem.elapsed().as_millis();
                    tracing::warn!(
                        error = %shmem_err,
                        rows = row_count,
                        shmem_write_ms = shmem_ms,
                        "shmem write failed; falling back to inline JSON"
                    );
                    // Fall through — send inline (may be slow for very
                    // large result sets, but at least it works).
                }
            }
        }

        // Back-patch serialize_ms into the profile with shmem write time
        // (the dominant cost). JSON serialization time is measured below
        // but can't be included in the JSON itself (chicken-and-egg).
        if let Some(ref mut prof) = response.profile {
            prof.serialize_ms = u64::try_from(shmem_ms).unwrap_or(u64::MAX);
        }

        let t_serialize = std::time::Instant::now();
        let result = serde_json::to_value(&response).unwrap_or_default();
        let json = serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default();
        let ser_ms = t_serialize.elapsed().as_millis();

        if row_count > 10_000 || ser_ms > 100 {
            tracing::info!(
                rows = row_count,
                serialize_ms = ser_ms,
                json_bytes = json.len(),
                shmem = response.shmem_path.is_some(),
                "🔌 search response serialized"
            );
        }

        json
    }

    /// Pack a path-only search response into a single UTF-8 buffer.
    ///
    /// When the client asked for a path-only projection, replace the
    /// `SearchRow` list with a newline-terminated blob in `paths_blob`.
    /// The CLI then writes the entire buffer with a single `write_all`,
    /// skipping per-row JSON deserialization and format dispatch (both
    /// of which scale linearly with row count).
    ///
    /// This is invisible to the `--out=file` bench (which never
    /// transfers rows), but is a large win for interactive
    /// `uffs *.ext` to stdout or for pipe composition.
    ///
    /// Previously capped at [`uffs_client::shmem::SHMEM_THRESHOLD`]
    /// (100 K rows) on the theory that "a multi-megabyte JSON string
    /// would be the worse choice" above that count, with large
    /// responses routed through shmem instead.  Measured wrong: on a
    /// 168 K-row path-only query the shmem path added ~744 ms vs.
    /// NUL (client-side `write_columnar` re-formatted every row through
    /// `extract_field` → `String::to_owned` + 4× `write!()` per field),
    /// while `paths_blob` plus one `write_all` would cost ~145 ms end to
    /// end.  The JSON-string encode/decode cost (~40 ms for 20 MB of
    /// ASCII) is dwarfed by the per-row format overhead it replaces.
    ///
    /// So: any path-only projection now gets `paths_blob` regardless of
    /// row count.  The shmem path is still used for multi-column
    /// responses above the threshold — those genuinely need binary
    /// `SearchRow` transport because the client still has to format
    /// columns locally.
    fn try_pack_paths_blob(params: &SearchParams, response: &mut SearchResponse) {
        let row_count = response.rows.len();
        if row_count == 0 || !Self::is_path_only_projection(params) {
            return;
        }
        let capacity: usize = response
            .rows
            .iter()
            .map(|row| row.path.len().saturating_add(1))
            .sum();
        let mut blob = String::with_capacity(capacity);
        for row in &response.rows {
            blob.push_str(&row.path);
            blob.push('\n');
        }
        response.paths_blob = Some(blob);
        response.rows = Vec::new();
    }

    /// Return true when the client asked for a single path column.
    ///
    /// Matches the user-facing column aliases `"path"` and `"full path"`
    /// case-insensitively.  Multi-column projections, aggregation
    /// requests, projected-JSON mode, and custom sort clauses all
    /// disqualify the fast path — the response must still carry the
    /// full [`SearchRow`] data for the CLI's row-based formatters.
    fn is_path_only_projection(params: &SearchParams) -> bool {
        if params.projection.len() != 1 {
            return false;
        }
        if !params.aggregations.is_empty() {
            return false;
        }
        if matches!(
            params.response_mode,
            Some(uffs_client::protocol::SearchResponseMode::Json)
        ) {
            return false;
        }
        let Some(col) = params.projection.first() else {
            return false;
        };
        let trimmed = col.trim();
        trimmed.eq_ignore_ascii_case("path") || trimmed.eq_ignore_ascii_case("full path")
    }

    /// Handle `search_cli` method — parse raw CLI args into [`SearchParams`]
    /// and run the standard search.
    ///
    /// The CLI sends its raw `argv` (after subcommand detection) so it
    /// never needs to parse search flags locally.
    async fn handle_search_cli(&self, id: u64, req: &RpcRequest) -> String {
        // Extract the `args` array from the params.
        let args: Vec<String> = match req
            .params
            .as_ref()
            .and_then(|val| val.get("args"))
            .and_then(|val| serde_json::from_value(val.clone()).ok())
        {
            Some(cli_args) => cli_args,
            None => {
                return serde_json::to_string(&RpcErrorResponse::error(
                    Some(id),
                    ERR_INVALID_PARAMS,
                    "Missing or invalid 'args' array",
                ))
                .unwrap_or_default();
            }
        };

        // Parse into SearchParams using the shared CLI parser.
        let search_params = match SearchParams::from_cli_args(&args) {
            Ok(params) => params,
            Err(msg) => {
                return serde_json::to_string(&RpcErrorResponse::error(
                    Some(id),
                    ERR_INVALID_PARAMS,
                    &msg,
                ))
                .unwrap_or_default();
            }
        };

        // Construct a synthetic RpcRequest with the parsed params
        // and delegate to the standard search handler.
        let search_req = RpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: Some(id),
            method: "search".to_owned(),
            params: serde_json::to_value(&search_params).ok(),
        };
        self.handle_search(id, &search_req).await
    }

    /// Handle `stats` method — performance metrics.
    async fn handle_stats(&self, id: u64) -> String {
        let response = self.index.stats().await;
        let result = serde_json::to_value(&response).unwrap_or_default();
        serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
    }

    /// Handle `drives` method.
    async fn handle_drives(&self, id: u64) -> String {
        let response = self.index.drives().await;
        let result = serde_json::to_value(&response).unwrap_or_default();
        serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
    }

    /// Handle `status` method.
    async fn handle_status(&self, id: u64, connections: usize) -> String {
        let response = self.index.status(connections).await;
        let result = serde_json::to_value(&response).unwrap_or_default();
        serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
    }

    /// Handle `info` method — look up a file by path.
    async fn handle_info(&self, id: u64, req: &RpcRequest) -> String {
        let file_path = req
            .params
            .as_ref()
            .and_then(|val| val.get("path"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");

        if file_path.is_empty() {
            return serde_json::to_string(&RpcErrorResponse::error(
                Some(id),
                ERR_INVALID_PARAMS,
                "Missing 'path' parameter",
            ))
            .unwrap_or_default();
        }

        let response = self.index.info(file_path).await;
        let result = serde_json::to_value(&response).unwrap_or_default();
        serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
    }

    /// Handle `facet_values` method — convenience wrapper for distinct
    /// field values with counts.
    ///
    /// Translates to a `search` call with a `terms` aggregation, then
    /// reshapes the response to return just the values and pagination.
    async fn handle_facet_values(&self, id: u64, req: &RpcRequest) -> String {
        let fv_params: FacetValuesParams = req
            .params
            .as_ref()
            .and_then(|val| serde_json::from_value(val.clone()).ok())
            .unwrap_or_default();

        // Build a terms aggregation spec for the requested field.
        // `top` controls how many distinct values the engine computes.
        // We always ask for all values (up to u16::MAX) and rely on
        // `agg_page_size` to paginate the wire response.
        let agg_spec = AggregateSpecWire {
            kind: "terms".to_owned(),
            label: Some(format!("facet_{}", fv_params.field)),
            field: Some(fv_params.field.clone()),
            top: Some(u16::MAX),
            metrics: vec!["count".to_owned(), "total_bytes".to_owned()],
            ..AggregateSpecWire::default()
        };

        let search_params = SearchParams {
            pattern: fv_params.pattern,
            aggregations: vec![agg_spec],
            include_rows: false,
            limit: Some(0),
            agg_cursor: fv_params.cursor,
            agg_page_size: fv_params.page_size,
            ..Default::default()
        };

        let response = self.index.search(&search_params).await;

        // Extract the first aggregation result.
        let (values, next_cursor, total_distinct) = response.aggregations.first().map_or_else(
            || (vec![], None, None),
            |agg| {
                (
                    agg.buckets.clone(),
                    agg.next_cursor.clone(),
                    agg.total_groups,
                )
            },
        );

        let fv_response = FacetValuesResponse {
            field: fv_params.field,
            values,
            total_distinct,
            next_cursor,
        };

        let result = serde_json::to_value(&fv_response).unwrap_or_default();
        serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
    }

    /// Handle `load_drive` method — hot-load MFT files into the daemon.
    async fn handle_load_drive(&self, id: u64, req: &RpcRequest) -> String {
        let params: LoadDriveParams = req
            .params
            .as_ref()
            .and_then(|val| serde_json::from_value(val.clone()).ok())
            .unwrap_or_default();

        let mut loaded: Vec<char> = Vec::new();
        let mut already_loaded: Vec<char> = Vec::new();
        let mut errors: Vec<String> = Vec::new();

        // Hot-load by MFT file path.
        for mft_file in &params.mft_files {
            let path = std::path::PathBuf::from(mft_file);
            match self
                .index
                .load_single_mft_file(&path, params.no_cache)
                .await
            {
                Ok(Some(letter)) => loaded.push(letter),
                Ok(None) => {
                    // Infer the letter for reporting.
                    let letter = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .and_then(|stem| stem.chars().next())
                        .map_or('?', |ch| ch.to_ascii_uppercase());
                    already_loaded.push(letter);
                }
                Err(load_err) => {
                    errors.push(format!("{}: {load_err}", path.display()));
                }
            }
        }

        // Hot-load by drive letter (live NTFS on Windows, data_dir on other
        // platforms).
        for &letter in &params.drives {
            match self.index.hot_load_drive(letter, params.no_cache).await {
                Ok(records) => {
                    tracing::info!(drive = %letter, records, "Drive hot-loaded via RPC");
                    loaded.push(letter.to_ascii_uppercase());
                }
                Err(load_err) => {
                    errors.push(format!("{letter}: {load_err}"));
                }
            }
        }

        let response = LoadDriveResponse {
            loaded,
            already_loaded,
            errors,
        };
        let result = serde_json::to_value(&response).unwrap_or_default();
        serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
    }

    /// Handle `refresh` method — spawns refresh in background, returns
    /// immediate ack.
    fn handle_refresh(&self, id: u64, req: &RpcRequest) -> String {
        let refresh_params: RefreshParams = req
            .params
            .as_ref()
            .and_then(|val| serde_json::from_value(val.clone()).ok())
            .unwrap_or_default();

        let idx_clone = alloc::sync::Arc::clone(&self.index);
        tokio::spawn(async move {
            idx_clone.refresh(&refresh_params.drives).await;
        });

        let result = serde_json::json!({"ok": true, "message": "refresh started"});
        serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
    }

    /// Handle `keepalive` method.
    ///
    /// D3.4.3: Also processes optional `session_type` parameter to set
    /// differentiated idle timeout tier.
    fn handle_keepalive(&self, id: u64, req: &RpcRequest) -> String {
        self.lifecycle.reset_idle_timer();

        // D3.4.3: If session_type is provided, update the timeout tier
        if let Some(session_type) = req
            .params
            .as_ref()
            .and_then(|val| val.get("session_type"))
            .and_then(serde_json::Value::as_str)
        {
            self.lifecycle.set_session_type(session_type);
        }

        let result = serde_json::json!({"ok": true});
        serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
    }

    /// Handle `shutdown` method.
    ///
    /// `S4.4.9`: Requires a `nonce` parameter matching the one in the PID file.
    /// This prevents unauthorized shutdown via the socket.
    fn handle_shutdown(&self, id: u64, req: &RpcRequest) -> String {
        let provided_nonce = req
            .params
            .as_ref()
            .and_then(|val| val.get("nonce"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");

        if !self.lifecycle.verify_shutdown_nonce(provided_nonce) {
            return serde_json::to_string(&RpcErrorResponse::error(
                Some(id),
                ERR_INVALID_PARAMS,
                "Invalid or missing shutdown nonce (read from daemon.pid file)",
            ))
            .unwrap_or_default();
        }

        self.lifecycle.request_shutdown();
        let result = serde_json::json!({"ok": true, "message": "shutting down"});
        serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use uffs_client::protocol::response::{SearchResponse, SearchRow};
    use uffs_client::protocol::{SearchParams, SearchResponseMode};

    use super::RequestHandler;

    /// Build a `SearchRow` with just a path populated — all other
    /// fields are irrelevant to [`RequestHandler::try_pack_paths_blob`]
    /// because the packing loop only reads `row.path`.
    fn path_only_row(path: String) -> SearchRow {
        SearchRow {
            drive: 'C',
            path,
            name: String::new(),
            size: 0,
            is_directory: false,
            modified: 0,
            created: 0,
            accessed: 0,
            flags: 0,
            allocated: 0,
            descendants: 0,
            treesize: 0,
            tree_allocated: 0,
        }
    }

    /// Build a minimal `SearchResponse` carrying `rows` and no side-
    /// channel transports — `shmem_path`, `paths_blob`, and
    /// `projected_rows` start as `None` so the packer's invariants can
    /// be asserted precisely after the call.
    fn bare_response(rows: Vec<SearchRow>) -> SearchResponse {
        let row_count = rows.len();
        SearchResponse {
            rows,
            total_count: u64::try_from(row_count).unwrap_or(u64::MAX),
            records_scanned: row_count,
            duration_ms: 0,
            truncated: false,
            shmem_path: None,
            shmem_count: None,
            profile: None,
            applied_sorts: Vec::new(),
            applied_projection: vec!["path".to_owned()],
            response_mode: Some(SearchResponseMode::Rows),
            projected_rows: None,
            aggregations: Vec::new(),
            paths_blob: None,
        }
    }

    /// Regression: `try_pack_paths_blob` used to bail out for
    /// path-only projections above
    /// `uffs_client::shmem::SHMEM_THRESHOLD` (100 000 rows), which
    /// forced the daemon to fall back to the shmem transport and
    /// made the client re-run `write_columnar` on every row — a
    /// ~6× slowdown vs. `paths_blob` + one `write_all` on the
    /// 168 K-row `C: ext:dll` benchmark.  The cap was removed: any
    /// path-only projection now packs, regardless of count.
    #[test]
    fn try_pack_paths_blob_packs_above_shmem_threshold() {
        // 150 K rows — comfortably above the old 100 K cap.  Kept at
        // this size so the test stays sub-millisecond: each row's
        // path is only ~30 bytes, total blob ~4.5 MB.
        let row_count: usize = 150_000;
        let rows: Vec<SearchRow> = (0..row_count)
            .map(|idx| path_only_row(format!("C:\\dir\\file_{idx}.dll")))
            .collect();
        let mut response = bare_response(rows);

        let params = SearchParams {
            projection: vec!["path".to_owned()],
            ..SearchParams::default()
        };

        RequestHandler::try_pack_paths_blob(&params, &mut response);

        // Use `let-else` so a regression of the cap removal fails
        // with the specific "packed into paths_blob" message rather
        // than a generic `unwrap` panic — keeps the regression
        // signal readable without needing a clippy::unwrap_used allow.
        let Some(blob) = response.paths_blob.as_deref() else {
            panic!(
                "path-only projection above SHMEM_THRESHOLD must still \
                 be packed into paths_blob — regression of the cap \
                 removal in try_pack_paths_blob"
            );
        };
        assert_eq!(
            blob.bytes().filter(|byte| *byte == b'\n').count(),
            row_count,
            "blob must contain one newline per row so the CLI's single \
             write_all emits exactly `row_count` lines"
        );
        assert!(
            blob.ends_with('\n'),
            "blob's last byte must be '\\n' (contract documented on \
             SearchResponse::paths_blob)"
        );
        assert!(
            response.rows.is_empty(),
            "rows must be cleared once paths_blob carries the payload \
             — otherwise the response is doubly-serialised"
        );
    }

    /// Multi-column projections (e.g. `--columns Path,Size`) must not
    /// be packed into `paths_blob`: the client still needs the full
    /// `SearchRow` data to format the size column.  Pins that the
    /// path-only guard (`is_path_only_projection`) is the sole
    /// disqualifier after the cap removal.
    #[test]
    fn try_pack_paths_blob_skips_multi_column_projection() {
        let rows = vec![path_only_row("C:\\a.dll".to_owned())];
        let mut response = bare_response(rows);

        let params = SearchParams {
            projection: vec!["path".to_owned(), "size".to_owned()],
            ..SearchParams::default()
        };

        RequestHandler::try_pack_paths_blob(&params, &mut response);

        assert!(
            response.paths_blob.is_none(),
            "multi-column projection must leave paths_blob unset so \
             the client receives full SearchRow data"
        );
        assert_eq!(
            response.rows.len(),
            1,
            "rows must stay populated for the client-side formatter"
        );
    }

    /// Zero-row responses must short-circuit without allocating an
    /// empty `paths_blob` string — keeps the wire format the same
    /// as before the cap removal.
    #[test]
    fn try_pack_paths_blob_skips_empty_response() {
        let mut response = bare_response(Vec::new());
        let params = SearchParams {
            projection: vec!["path".to_owned()],
            ..SearchParams::default()
        };

        RequestHandler::try_pack_paths_blob(&params, &mut response);

        assert!(
            response.paths_blob.is_none(),
            "empty response must leave paths_blob unset (no \
             `Some(String::new())` allocations)"
        );
    }
}
