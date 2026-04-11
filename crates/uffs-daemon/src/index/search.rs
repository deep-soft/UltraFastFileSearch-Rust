// Search handler: Arc clones are intentional for task-spawning; bool negation
// is clearer than `!` for readability in complex conditionals.
#![allow(
    clippy::clone_on_ref_ptr,
    clippy::if_not_else,
    reason = "search handler: Arc clones for task boundaries, readable conditionals"
)]

//! Search execution: query dispatch, profile construction, and drive info.

use core::sync::atomic::Ordering;
use std::time::Instant;

use uffs_client::protocol::{
    DriveInfo, DriveProfile, DrivesResponse, SearchFilterMode, SearchParams, SearchProfile,
    SearchResponse, SearchResponseMode, SearchRow,
};
use uffs_core::search::backend::{FilterMode, SearchRequest, SortSpec, search_index};
use uffs_core::search::field::FieldId;
use uffs_core::search::filters::{SearchFilterParams, SearchFilters};

use super::IndexManager;

impl IndexManager {
    /// Execute a search query (updates perf counters).
    ///
    /// When `params.profile` is `true`, populates `SearchResponse::profile`
    /// with a per-phase timing breakdown so the CLI can print it.
    #[allow(
        clippy::cognitive_complexity,
        reason = "search orchestration with multi-drive merge, sorting, and response formatting"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "search orchestration with multi-drive merge, sorting, and response formatting"
    )]
    pub async fn search(&self, params: &SearchParams) -> SearchResponse {
        // Acquire a concurrency permit — blocks if too many searches
        // are already in flight (prevents CPU/memory exhaustion).
        let _permit = match self.search_semaphore.acquire().await {
            Ok(permit) => permit,
            Err(_closed) => {
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
                };
            }
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
    pub async fn drives(&self) -> DrivesResponse {
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
}
