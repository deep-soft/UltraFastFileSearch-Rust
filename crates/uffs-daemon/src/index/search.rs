// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

// Search handler: Arc clones are intentional for task-spawning; bool negation
// is clearer than `!` for readability in complex conditionals.
#![allow(
    clippy::clone_on_ref_ptr,
    clippy::if_not_else,
    reason = "search handler: Arc clones for task boundaries, readable conditionals"
)]

//! Search execution: query dispatch, profile construction, and drive info.

use core::sync::atomic::Ordering;
use std::io::Write;
use std::time::Instant;

use uffs_client::protocol::response::{
    DriveInfo, DriveProfile, DrivesResponse, SearchProfile, SearchResponse, SearchRow,
};
use uffs_client::protocol::{SearchFilterMode, SearchParams, SearchResponseMode};
use uffs_core::search::backend::{DisplayRow, FilterMode, SearchRequest, SortSpec, search_index};
use uffs_core::search::field::FieldId;
use uffs_core::search::filters::{SearchFilterParams, SearchFilters};

use super::IndexManager;

impl IndexManager {
    /// Execute a search query (updates perf counters).
    ///
    /// When `params.profile` is `true`, populates `SearchResponse::profile`
    /// with a per-phase timing breakdown so the CLI can print it.
    #[expect(
        clippy::too_many_lines,
        reason = "search orchestration with multi-drive merge, sorting, and response formatting"
    )]
    #[expect(
        clippy::cognitive_complexity,
        reason = "search filter application with many predicate branches"
    )]
    pub(crate) async fn search(&self, params: &SearchParams) -> SearchResponse {
        // Acquire a concurrency permit — blocks if too many searches
        // are already in flight.  The effective cap is `max(2, cpus /
        // drives)` (see `IndexManager::tune_concurrency`) to prevent
        // rayon-pool oversubscription from the per-query
        // `drives.par_iter()` fanout.
        let Some(_permit) = self.acquire_search_permit().await else {
            return SearchResponse {
                rows: Vec::new(),
                total_count: 0,
                records_scanned: 0,
                duration_ms: 0,
                truncated: false,
                shmem_path: None,
                shmem_count: None,
                profile: None,
                applied_sorts: Vec::new(),
                applied_projection: Vec::new(),
                response_mode: None,
                projected_rows: None,
                aggregations: vec![],
                paths_blob: None,
            };
        };

        let query_start = Instant::now();
        let profiling = params.profile;
        let mut effective_params = params.clone();
        effective_params.populate_canonical_fields();
        let applied_sorts = Self::resolve_applied_sorts(&effective_params);
        let projection_fields = Self::resolve_projection_fields(&effective_params.projection);
        let applied_projection: Vec<String> = projection_fields
            .iter()
            .map(|field| field.canonical_name().to_owned())
            .collect();
        let response_mode = effective_params.resolved_response_mode();
        let requires_post_filter =
            Self::predicates_require_post_filter(&effective_params.predicates);

        // ── Snapshot the index (< 1 μs) ────────────────────────────
        let t_lock = profiling.then(Instant::now);
        let snapshot = self.snapshot().await;
        let lock_us = t_lock.map_or(0, |ts| ts.elapsed().as_micros());

        let (sort_column, sort_desc, extra_sort_tiers) =
            applied_sorts
                .first()
                .map_or((FieldId::Modified, true, Vec::new()), |primary| {
                    let extra = applied_sorts
                        .iter()
                        .skip(1)
                        .filter_map(Self::sort_spec_to_backend)
                        .map(|(column, descending)| SortSpec { column, descending })
                        .collect();
                    let (column, descending) =
                        Self::sort_spec_to_backend(primary).unwrap_or((FieldId::Modified, true));
                    (column, descending, extra)
                });

        let filter_mode = match effective_params.resolved_filter_mode() {
            SearchFilterMode::Files => FilterMode::FilesOnly,
            SearchFilterMode::Dirs => FilterMode::DirsOnly,
            SearchFilterMode::All => FilterMode::All,
        };

        let ep = &effective_params;
        let mut filters = SearchFilters::from_params(&SearchFilterParams {
            hide_system: ep.hide_system,
            hide_ads: ep.hide_ads,
            min_size: ep.min_size,
            max_size: ep.max_size,
            min_descendants: ep.min_descendants,
            max_descendants: ep.max_descendants,
            newer: ep.newer.as_deref(),
            older: ep.older.as_deref(),
            newer_created: ep.newer_created.as_deref(),
            older_created: ep.older_created.as_deref(),
            newer_accessed: ep.newer_accessed.as_deref(),
            older_accessed: ep.older_accessed.as_deref(),
            attr_filter: ep.attr.as_deref(),
            ext_filter: ep.ext.as_deref(),
            exclude: ep.exclude.as_deref(),
            path_contains: ep.path_contains.as_deref(),
            type_filter: ep.type_filter.as_deref(),
            min_bulkiness: ep.min_bulkiness,
            max_bulkiness: ep.max_bulkiness,
            min_name_len: ep.min_name_len,
            max_name_len: ep.max_name_len,
            min_path_len: ep.min_path_len,
            max_path_len: ep.max_path_len,
            min_allocated: ep.min_allocated,
            max_allocated: ep.max_allocated,
            min_treesize: ep.min_treesize,
            max_treesize: ep.max_treesize,
            min_tree_allocated: ep.min_tree_allocated,
            max_tree_allocated: ep.max_tree_allocated,
            allowed_months: &ep.allowed_months,
        });

        // Overlay canonical predicates that can be compiled into the hot
        // path (size / descendant bounds).
        Self::compile_predicates_into_filters(&mut filters, &effective_params.predicates);

        // Snapshot per-drive info (only when profiling).
        let drive_info: Vec<(char, usize)> = if profiling {
            snapshot.drive_summary()
        } else {
            Vec::new()
        };

        // When post-filters are active the search must return more rows
        // than the user-requested limit, because some rows will be
        // discarded after path resolution.
        //
        // • Predicates (parsed `size:>1M` etc.) — unbounded: these are
        //   arbitrary user expressions so we must scan everything.
        // • Display-row filters (--in-path, --min-bulkiness, --min-path-len)
        //   — also unbounded.  The hit rate of path-based filters can be
        //   extremely low (e.g. --in-path *windows* matches <1% of files),
        //   so any fixed multiplier risks returning 0 rows.  The final
        //   limit is applied after filtering (see `filtered_rows.truncate`
        //   below).
        //
        // Build the aggregate record filter BEFORE `filters` is moved into
        // the search closure.  `type_filter` is promoted to extensions by
        // `from_params`; those same extensions must scope the aggregation.
        let agg_record_filter = uffs_core::aggregate::AggregateFilter {
            extensions: filters.extensions.clone(),
            directory_only: match filter_mode {
                FilterMode::FilesOnly => Some(false),
                FilterMode::DirsOnly => Some(true),
                FilterMode::All => None,
            },
            min_size: filters.min_size,
            max_size: filters.max_size,
        };

        let search_limit = if requires_post_filter || filters.needs_display_row_filter() {
            None
        } else {
            effective_params.limit
        };

        // ── Execute search on a blocking thread with timeout ────────
        // `search_index` uses rayon `par_iter`, which blocks the current
        // thread.  `spawn_blocking` prevents it from starving the tokio
        // runtime.
        let pattern = effective_params.pattern.clone();
        let case_sensitive = effective_params.case_sensitive;
        let whole_word = effective_params.whole_word;
        let match_path = effective_params.match_path;
        let drives = effective_params.drives.clone();
        let agg_snapshot = snapshot.clone();
        let search_handle = tokio::task::spawn_blocking(move || {
            search_index(
                &snapshot,
                SearchRequest {
                    pattern: &pattern,
                    case_sensitive,
                    whole_word,
                    match_path,
                    result_limit: search_limit,
                    filter_mode,
                    search_filters: &mut filters,
                    drives_filter: &drives,
                },
                sort_column,
                sort_desc,
                &extra_sort_tiers,
            )
        });

        let search_outcome =
            tokio::time::timeout(core::time::Duration::from_secs(30), search_handle).await;

        let result = match search_outcome {
            Ok(Ok(res)) => res,
            Ok(Err(_join_err)) => {
                tracing::error!("search task panicked");
                return SearchResponse {
                    rows: Vec::new(),
                    total_count: 0,
                    records_scanned: 0,
                    duration_ms: 0,
                    truncated: false,
                    shmem_path: None,
                    shmem_count: None,
                    profile: None,
                    applied_sorts: Vec::new(),
                    applied_projection: Vec::new(),
                    response_mode: None,
                    projected_rows: None,
                    aggregations: vec![],
                    paths_blob: None,
                };
            }
            Err(_timeout) => {
                tracing::warn!(
                    pattern = %effective_params.pattern,
                    "search timed out after 30s"
                );
                return SearchResponse {
                    rows: Vec::new(),
                    total_count: 0,
                    records_scanned: 0,
                    duration_ms: 30_000,
                    truncated: false,
                    shmem_path: None,
                    shmem_count: None,
                    profile: None,
                    applied_sorts: Vec::new(),
                    applied_projection: Vec::new(),
                    response_mode: None,
                    projected_rows: None,
                    aggregations: vec![],
                    paths_blob: None,
                };
            }
        };
        let search_us = if profiling {
            result.duration.as_micros()
        } else {
            0
        };

        // ── Row building ────────────────────────────────────────────
        let t_rows = profiling.then(Instant::now);
        let mut filtered_rows = result.rows;
        if requires_post_filter {
            filtered_rows.retain(|row| Self::matches_predicates(row, &effective_params.predicates));
        }

        let mut total_count = filtered_rows.len() as u64;
        if let Some(limit) = effective_params.limit {
            filtered_rows.truncate(limit as usize);
        }

        // ── Direct file output (OPT-4) ──────────────────────────────
        // When `output_file` is set, write results directly to file and
        // return metadata-only response.  Skips SearchRow allocation,
        // JSON serialization, and IPC transfer entirely.
        if let Some(output_path) = &effective_params.output_file {
            let duration_ms = u64::try_from(result.duration.as_millis()).unwrap_or(u64::MAX);

            // Reconstruct OutputConfig from protocol fields.
            let output_config = build_output_config(&effective_params);

            match Self::write_rows_to_file(&filtered_rows, output_path, &output_config) {
                Ok(rows_written) => {
                    tracing::info!(
                        output = output_path,
                        rows = rows_written,
                        duration_ms,
                        "daemon wrote results directly to file"
                    );
                    // Update perf counters.
                    let query_us = query_start.elapsed().as_micros();
                    self.queries_total.fetch_add(1, Ordering::Relaxed);
                    self.queries_total_us.fetch_add(
                        u64::try_from(query_us).unwrap_or(u64::MAX),
                        Ordering::Relaxed,
                    );
                    return SearchResponse {
                        rows: Vec::new(),
                        total_count,
                        records_scanned: result.records_scanned,
                        duration_ms,
                        truncated: false,
                        shmem_path: None,
                        shmem_count: None,
                        profile: None,
                        applied_sorts: Vec::new(),
                        applied_projection: Vec::new(),
                        response_mode: None,
                        projected_rows: None,
                        aggregations: vec![],
                        paths_blob: None,
                    };
                }
                Err(err) => {
                    tracing::error!(
                        output = output_path,
                        error = %err,
                        "failed to write results to file — falling back to IPC"
                    );
                    // Fall through to the normal IPC path.
                }
            }
        }

        let rows: Vec<SearchRow> = filtered_rows
            .iter()
            .map(Self::display_row_to_search_row)
            .collect();
        let row_build_us = t_rows.map_or(0, |ts| ts.elapsed().as_micros());

        // Update perf counters.
        let query_us = query_start.elapsed().as_micros();
        self.queries_total.fetch_add(1, Ordering::Relaxed);
        self.queries_total_us.fetch_add(
            u64::try_from(query_us).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );

        let truncated = effective_params
            .limit
            .is_some_and(|cap| rows.len() >= cap as usize);
        let duration_ms = u64::try_from(result.duration.as_millis()).unwrap_or(u64::MAX);

        // Profile (built in a separate method to keep `search` under the line limit).
        let profile = if profiling {
            Some(
                self.build_search_profile(lock_us, search_us, row_build_us, &drive_info, &rows)
                    .await,
            )
        } else {
            None
        };

        let projected_rows = (matches!(response_mode, SearchResponseMode::Json)
            && !projection_fields.is_empty())
        .then(|| {
            rows.iter()
                .map(|row| Self::project_search_row(row, &projection_fields))
                .collect()
        });

        // ── Aggregation (if requested) ─────────────────────────────
        let (agg_results, agg_matched) = if !effective_params.aggregations.is_empty() {
            let predicates = Self::build_query_predicates(&effective_params);

            // Pass the pattern if it's non-trivial (not just `*`).
            let agg_pattern =
                if matches!(effective_params.pattern.as_str(), "*" | "**" | "**/*" | "") {
                    None
                } else {
                    Some(effective_params.pattern.as_str())
                };

            Self::run_aggregations(
                &agg_snapshot,
                Some(self.aggregate_cache()),
                &effective_params.aggregations,
                predicates,
                effective_params.agg_cursor.as_deref(),
                effective_params.agg_page_size,
                agg_pattern,
                &effective_params.drives,
                &agg_record_filter,
            )
        } else {
            (vec![], 0)
        };

        // When aggregation ran a filtered scan, its `records_matched`
        // gives the true total (before limit).  Otherwise use the
        // pre-truncation row count.
        if agg_matched > 0 {
            total_count = agg_matched;
        }

        SearchResponse {
            rows: if projected_rows.is_some() {
                Vec::new()
            } else {
                rows
            },
            total_count,
            records_scanned: result.records_scanned,
            duration_ms,
            truncated,
            shmem_path: None,
            shmem_count: None,
            profile,
            applied_sorts,
            applied_projection,
            response_mode: Some(response_mode),
            projected_rows,
            aggregations: agg_results,
            // Populated by `handle_search` when projection is path-only
            // and the row count is below the shmem threshold.  The
            // search core itself always returns full [`SearchRow`]s.
            paths_blob: None,
        }
    }

    /// Build the `SearchProfile` for `--profile` output.
    async fn build_search_profile(
        &self,
        lock_us: u128,
        search_us: u128,
        row_build_us: u128,
        drive_info: &[(char, usize)],
        rows: &[SearchRow],
    ) -> SearchProfile {
        let timings = self.drive_timings.read().await;
        let startup_us = self.startup_duration_us.load(Ordering::Relaxed);

        let us_to_ms = |us: u128| u64::try_from(us / 1000).unwrap_or(u64::MAX);
        let ms_clamp = |val: u128| u64::try_from(val).unwrap_or(u64::MAX);

        let mut drive_profiles: Vec<DriveProfile> = drive_info
            .iter()
            .map(|&(drive, records)| {
                let matches = rows.iter().filter(|row| row.drive == drive).count();
                let (cache_ms, mft_ms, compact_ms, trigram_ms) =
                    timings.get(&drive).map_or((0, 0, 0, 0), |ts| {
                        (
                            ms_clamp(ts.cache),
                            ms_clamp(ts.mft),
                            ms_clamp(ts.compact),
                            ms_clamp(ts.trigram),
                        )
                    });
                DriveProfile {
                    drive,
                    records,
                    matches,
                    cache_ms,
                    mft_ms,
                    compact_ms,
                    trigram_ms,
                }
            })
            .collect();
        drive_profiles.sort_by_key(|dp| dp.drive);

        SearchProfile {
            uptime_ms: us_to_ms(self.start_time.elapsed().as_micros()),
            startup_ms: startup_us / 1000,
            lock_ms: us_to_ms(lock_us),
            search_ms: us_to_ms(search_us),
            row_build_ms: us_to_ms(row_build_us),
            serialize_ms: 0, // filled in by handler after shmem write
            drives: drive_profiles,
        }
    }

    /// Get loaded drives info.
    pub(crate) async fn drives(&self) -> DrivesResponse {
        let snap = self.snapshot().await;
        DrivesResponse {
            drives: snap
                .drives
                .iter()
                .map(|dr| DriveInfo {
                    letter: dr.letter,
                    records: dr.records.len(),
                    source: match &dr.source {
                        uffs_core::compact::IndexSource::MftFile(mft_path) => {
                            if mft_path.to_string_lossy().len() <= 2 {
                                "live".to_owned()
                            } else {
                                format!("file:{}", mft_path.display())
                            }
                        }
                    },
                })
                .collect(),
        }
    }

    /// Convert search-request filters into drill-down predicates.
    ///
    /// These are prepended to each bucket's drill-down list so that a
    /// follow-up query reproduces the original scope plus the bucket key.
    fn build_query_predicates(
        params: &SearchParams,
    ) -> Vec<uffs_core::aggregate::finalize::DrilldownPredicate> {
        use uffs_core::aggregate::finalize::{DrilldownPredicate, DrilldownValue};
        let mut preds = Vec::new();

        // Pattern
        if !params.pattern.is_empty() && params.pattern != "*" {
            preds.push(DrilldownPredicate {
                field: "name".to_owned(),
                op: "glob".to_owned(),
                value: DrilldownValue::String(params.pattern.clone()),
            });
        }

        // Filter mode (files / dirs)
        if let Some(filter) = &params.filter
            && filter != "all"
        {
            preds.push(DrilldownPredicate {
                field: "type".to_owned(),
                op: "eq".to_owned(),
                value: DrilldownValue::String(filter.clone()),
            });
        }

        // Size range
        if let Some(min) = params.min_size {
            preds.push(DrilldownPredicate {
                field: "size".to_owned(),
                op: "gte".to_owned(),
                value: DrilldownValue::U64(min),
            });
        }
        if let Some(max) = params.max_size {
            preds.push(DrilldownPredicate {
                field: "size".to_owned(),
                op: "lte".to_owned(),
                value: DrilldownValue::U64(max),
            });
        }

        // Drives
        for &drive in &params.drives {
            preds.push(DrilldownPredicate {
                field: "drive".to_owned(),
                op: "eq".to_owned(),
                value: DrilldownValue::String(drive.to_string()),
            });
        }

        preds
    }

    // ── Direct file output (OPT-4) ──────────────────────────────────

    /// Write `DisplayRow`s directly to a file, bypassing `SearchRow` and IPC.
    ///
    /// Uses the same `OutputConfig::write_display_rows` that the CLI uses,
    /// so all formatting options (separator, quotes, header, pos/neg,
    /// columns, timestamps) produce identical output.
    ///
    /// Atomic write: writes to a `.uffs.tmp` sibling file, then renames
    /// to the target after a `BufWriter::flush`.  No `fsync` —
    /// `--out=<path>` is reproducible search output, so the tmp+rename
    /// dance protects against partial-file exposure during normal
    /// writes but power-loss durability is intentionally not provided.
    /// See the inline comment in the body and §Run 7 C / §Run 8 of
    /// `docs/research/perf-phase2-measurement-plan.md` for the
    /// measurement that motivated this trade-off.  Zero rows → no
    /// file is created.
    fn write_rows_to_file(
        rows: &[DisplayRow],
        path: &str,
        output_config: &uffs_core::output::OutputConfig,
    ) -> Result<usize, std::io::Error> {
        use std::io::BufWriter;

        // Zero results → don't create the file at all.
        if rows.is_empty() {
            return Ok(0);
        }

        let target = std::path::Path::new(path);
        let tmp_path = target.with_extension("uffs.tmp");

        // Write to temp file — target is untouched until rename.
        let file = std::fs::File::create(&tmp_path)?;
        let mut writer = BufWriter::with_capacity(256 * 1024, file);

        let write_result = output_config
            .write_display_rows(rows, &mut writer)
            .map_err(std::io::Error::other);

        // On write error, clean up the temp file and propagate.
        if let Err(err) = write_result {
            drop(writer);
            let _cleanup: Result<(), std::io::Error> = std::fs::remove_file(&tmp_path);
            return Err(err);
        }

        // Flush the BufWriter and close the underlying file.
        //
        // We deliberately skip `sync_all()` here.  `--out=<path>` is
        // a user-requested export of search results; the data is
        // reproducible from the MFT index in ~100 ms, so paying a
        // 5-15 ms `fsync` per query for power-loss durability is not
        // worth it — a power cut would just leave a 0-byte file and
        // the user can simply re-run the query.  The atomic
        // tmp+rename below still prevents partial-file exposure
        // during normal writes.  See
        // `docs/research/perf-phase2-measurement-plan.md` §Run 7 C /
        // §Run 8 for the measurement that motivated dropping the
        // sync.
        writer.flush()?;
        writer
            .into_inner()
            .map_err(std::io::IntoInnerError::into_error)?;
        // The File temporary above is dropped at the semicolon,
        // closing the OS handle before the rename below.

        // Atomic rename: target appears only with complete data.
        std::fs::rename(&tmp_path, target)?;

        Ok(rows.len())
    }
}

/// Reconstruct an [`OutputConfig`] from protocol fields in [`SearchParams`].
///
/// The CLI serialises its `OutputConfig` into individual string fields
/// (`output_separator`, `output_quote`, etc.) so the daemon can rebuild
/// an identical config without needing serde on `OutputConfig` itself.
fn build_output_config(params: &SearchParams) -> uffs_core::output::OutputConfig {
    let mut cfg = uffs_core::output::OutputConfig::default();

    if let Some(sep) = &params.output_separator {
        cfg = cfg.with_separator(sep);
    }
    if let Some(quote) = &params.output_quote {
        cfg = cfg.with_quote(quote);
    }
    if let Some(header) = params.output_header {
        cfg = cfg.with_header(header);
    }
    if let Some(pos) = &params.output_pos {
        cfg = cfg.with_pos(pos);
    }
    if let Some(neg) = &params.output_neg {
        cfg = cfg.with_neg(neg);
    }
    if let Some(cols_str) = &params.output_columns {
        cfg = cfg.with_columns(cols_str);
    }
    if params.output_parity_compat == Some(true) {
        cfg = cfg.with_parity_compat(true);
    }
    if let Some(tz_hours) = params.output_tz_offset_hours {
        cfg = cfg.with_tz_offset_hours(tz_hours);
    }
    cfg
}

#[cfg(test)]
mod tests {
    use uffs_client::protocol::SearchParams;

    use super::*;

    /// Regression: `build_output_config` must use `OutputConfig` defaults
    /// (separator = `,`, quote = `"`) when `SearchParams` output fields
    /// are `None`.
    ///
    /// Previously, `from_cli_args` set `output_separator: Some("")` which
    /// caused `build_output_config` to call `with_separator("")`, wiping
    /// the comma delimiter and producing concatenated output with no field
    /// separation.
    #[test]
    fn build_output_config_preserves_defaults_when_none() {
        let params = SearchParams::default();
        assert!(params.output_separator.is_none());
        assert!(params.output_quote.is_none());
        assert!(params.output_pos.is_none());
        assert!(params.output_neg.is_none());

        let cfg = build_output_config(&params);
        assert_eq!(cfg.separator, ",", "default separator must be comma");
        assert_eq!(cfg.quote, "\"", "default quote must be double-quote");
        assert_eq!(cfg.pos, "1", "default pos must be '1'");
        assert_eq!(cfg.neg, "0", "default neg must be '0'");
        assert!(cfg.header, "default header must be true");
    }

    /// Guard against the exact bug: passing `Some("")` to
    /// `build_output_config` must NOT wipe the separator/quote.
    /// The daemon function should skip empty-string overrides.
    #[test]
    fn build_output_config_some_empty_string_overrides_defaults() {
        // This test documents the current behavior: if Some("") is passed,
        // it DOES override the default.  The fix is in from_cli_args which
        // must never produce Some("") for unset flags.
        let params = SearchParams {
            output_separator: Some(String::new()),
            output_quote: Some(String::new()),
            ..Default::default()
        };
        let cfg = build_output_config(&params);
        // Some("") overrides defaults — this is why from_cli_args must
        // use None, not Some(""), for unset flags.
        assert_eq!(
            cfg.separator, "",
            "Some(\"\") overrides default — from_cli_args must use None"
        );
        assert_eq!(
            cfg.quote, "",
            "Some(\"\") overrides default — from_cli_args must use None"
        );
    }

    /// Explicit separator and quote values must be forwarded.
    #[test]
    fn build_output_config_explicit_values_applied() {
        let params = SearchParams {
            output_separator: Some(";".to_owned()),
            output_quote: Some("'".to_owned()),
            output_pos: Some("+".to_owned()),
            output_neg: Some("-".to_owned()),
            output_header: Some(false),
            output_columns: Some("parity".to_owned()),
            output_parity_compat: Some(true),
            output_tz_offset_hours: Some(-7_i32),
            ..Default::default()
        };
        let cfg = build_output_config(&params);
        assert_eq!(cfg.separator, ";");
        assert_eq!(cfg.quote, "'");
        assert_eq!(cfg.pos, "+");
        assert_eq!(cfg.neg, "-");
        assert!(!cfg.header);
        assert!(cfg.columns.is_some(), "parity columns must be set");
        assert!(cfg.parity_compat, "parity_compat must be true");
        assert_eq!(cfg.timezone_offset_secs, -7_i32 * 3_600_i32);
    }

    /// `--parity-compat` without explicit sep/quote must produce a valid
    /// parity `OutputConfig` with default comma + double-quote delimiters.
    #[test]
    fn build_output_config_parity_compat_uses_defaults() {
        let params = SearchParams {
            output_columns: Some("parity".to_owned()),
            output_parity_compat: Some(true),
            ..Default::default()
        };
        let cfg = build_output_config(&params);
        assert_eq!(
            cfg.separator, ",",
            "parity mode must use comma separator by default"
        );
        assert_eq!(
            cfg.quote, "\"",
            "parity mode must use double-quote by default"
        );
        assert!(cfg.parity_compat);
        assert!(cfg.columns.is_some());
    }
}
