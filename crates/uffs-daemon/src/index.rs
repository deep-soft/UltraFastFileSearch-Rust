//! Index management: load drives, hold compact indices, refresh.
//!
//! The [`IndexManager`] is the daemon's core data structure. It holds
//! the compact search indices for all loaded drives and delegates to
//! `uffs_core::search` for query execution.

use core::sync::atomic::{AtomicU64, Ordering};
use std::path::PathBuf;
use std::time::Instant;

use tokio::sync::RwLock;
use uffs_client::protocol::{
    DaemonStatus, DriveInfo, DriveProfile, DrivesResponse, SearchFilterMode, SearchParams,
    SearchPredicate, SearchPredicateOp, SearchPredicateValue, SearchProfile, SearchResponse,
    SearchResponseMode, SearchRow, SearchSortDirection, SearchSortSpec, StatsResponse,
    StatusResponse,
};
use uffs_core::search::backend::{DisplayRow, FilterMode, MultiDriveBackend, SortSpec};
use uffs_core::search::derived::{
    bulkiness_for_row, semantic_type_for_row, tree_allocated_for_row,
};
use uffs_core::search::field::{FieldId, SortDirection};
use uffs_core::search::filters::{SearchFilterParams, SearchFilters};

use crate::events::{DaemonEvent, EventSender};

/// Per-drive load timing stored for profile reporting.
///
/// Field names omit the `_ms` suffix because the unit is documented
/// once here; all values are milliseconds (`u128`).
struct StoredDriveTiming {
    /// Compact-cache deserialization time (milliseconds, 0 if cache miss).
    cache: u128,
    /// MFT read time (milliseconds, 0 if cache hit).
    mft: u128,
    /// Compact index build time (milliseconds, 0 if cache hit).
    compact: u128,
    /// Trigram index build time (milliseconds, 0 if cache hit).
    trigram: u128,
}

/// Manages loaded drive indices and serves queries.
///
/// Thread-safe via `Arc<RwLock<...>>` — multiple readers (search) can
/// run concurrently, writers (load/refresh) get exclusive access.
pub struct IndexManager {
    /// The search backend holding all loaded drives.
    backend: RwLock<MultiDriveBackend>,
    /// Current daemon status.
    status: RwLock<DaemonStatus>,
    /// When the daemon started.
    start_time: Instant,
    /// Data directory for MFT files (Mac/Linux offline mode).
    data_dir: Option<PathBuf>,
    /// Event broadcaster — pushes notifications to all connected clients.
    events: EventSender,
    // ── Performance counters ────────────────────────────────────────
    /// Total search queries served.
    queries_total: AtomicU64,
    /// Cumulative search time in microseconds.
    queries_total_us: AtomicU64,
    /// Duration from daemon start to `Ready` (microseconds, set once).
    startup_duration_us: AtomicU64,
    /// Per-drive load timing for `--profile` reporting.
    drive_timings: RwLock<std::collections::HashMap<char, StoredDriveTiming>>,
}

impl IndexManager {
    /// Create a new empty index manager.
    #[must_use]
    #[expect(clippy::single_call_fn, reason = "constructor — structural separation")]
    pub fn new(data_dir: Option<PathBuf>, events: EventSender) -> Self {
        Self {
            backend: RwLock::new(MultiDriveBackend::new()),
            status: RwLock::new(DaemonStatus::Loading {
                drives_loaded: 0,
                drives_total: 0,
            }),
            start_time: Instant::now(),
            data_dir,
            events,
            queries_total: AtomicU64::new(0),
            queries_total_us: AtomicU64::new(0),
            startup_duration_us: AtomicU64::new(0),
            drive_timings: RwLock::new(std::collections::HashMap::new()),
        }
    }

    /// Get a reference to the event sender (for IPC and lifecycle integration).
    pub const fn event_sender(&self) -> &EventSender {
        &self.events
    }

    /// Load drives from MFT files — **all files in parallel**.
    ///
    /// Each MFT file is loaded on its own blocking thread via `JoinSet`.
    /// Results are collected as they complete (fastest first).
    pub async fn load_from_data_dir(&self, mft_files: &[PathBuf], no_cache: bool) {
        let total = mft_files.len();
        *self.status.write().await = DaemonStatus::Loading {
            drives_loaded: 0,
            drives_total: total,
        };

        // Spawn all file loads in parallel on blocking threads.
        let mut join_set = tokio::task::JoinSet::new();
        for mft_path in mft_files {
            let path = mft_path.clone();
            tracing::info!(path = %path.display(), "Loading MFT file (parallel)");
            join_set.spawn_blocking(move || {
                let source = uffs_core::compact::MftSource::File(path.clone(), None);
                let result = uffs_core::compact::load_drive(&source, no_cache);
                (path, result)
            });
        }

        // Collect results as they complete (fastest first).
        let mut loaded: usize = 0;
        while let Some(join_result) = join_set.join_next().await {
            match join_result {
                Ok((_path, Ok((drive_index, timing)))) => {
                    loaded += 1;
                    let letter = drive_index.letter;
                    let records = drive_index.records.len();
                    tracing::info!(
                        drive = %letter,
                        records,
                        mft_ms = timing.mft,
                        compact_ms = timing.compact,
                        trigram_ms = timing.trigram,
                        loaded,
                        total,
                        "Drive loaded"
                    );
                    self.events.emit(DaemonEvent::DriveLoaded {
                        drive: letter,
                        records,
                        mft_ms: timing.mft,
                        compact_ms: timing.compact,
                        trigram_ms: timing.trigram,
                        drives_loaded: loaded,
                        drives_total: total,
                    });
                    // Store timing for profile reporting.
                    self.drive_timings
                        .write()
                        .await
                        .insert(letter, StoredDriveTiming {
                            cache: timing.cache,
                            mft: timing.mft,
                            compact: timing.compact,
                            trigram: timing.trigram,
                        });
                    let mut backend = self.backend.write().await;
                    backend.drives.push(drive_index);
                    drop(backend);
                }
                Ok((path, Err(load_err))) => {
                    loaded += 1;
                    tracing::error!(path = %path.display(), error = %load_err, "Failed to load MFT file");
                }
                Err(join_err) => {
                    loaded += 1;
                    tracing::error!(error = %join_err, "Task panicked loading MFT");
                }
            }

            let mut progress = self.status.write().await;
            *progress = DaemonStatus::Loading {
                drives_loaded: loaded,
                drives_total: total,
            };
            drop(progress);
        }

        // Mark as ready + record startup duration.
        self.set_ready().await;

        let backend = self.backend.read().await;
        let drive_count = backend.drives.len();
        let total_records = backend.total_records();
        drop(backend);
        tracing::info!(
            drives = drive_count,
            total_records,
            "All drives loaded — daemon ready"
        );
        self.events.emit(DaemonEvent::DaemonReady {
            drives: drive_count,
            total_records,
            startup_ms: self.start_time.elapsed().as_millis(),
        });
    }

    /// Per-drive load timeout.  If a single drive's MFT read takes
    /// longer than this, we skip it rather than blocking the entire
    /// daemon.  Raw NTFS volume reads can hang indefinitely when a
    /// drive is unresponsive (bad sectors, sleep, USB disconnect).
    #[cfg(windows)]
    const DRIVE_LOAD_TIMEOUT: core::time::Duration = core::time::Duration::from_secs(300);

    /// Load live Windows drives — **all drives in parallel**.
    ///
    /// Each drive's MFT read runs on its own blocking thread. Results are
    /// collected via `JoinSet` as they complete (fastest drive first), giving
    /// accurate incremental progress and cutting total wall time from
    /// `sum(per-drive)` to `max(per-drive)`.
    ///
    /// Each drive has a [`Self::DRIVE_LOAD_TIMEOUT`] — if exceeded the drive
    /// is skipped and an error is logged.  This prevents a single stuck
    /// volume from making the daemon unkillable.
    #[cfg(windows)]
    pub async fn load_live_drives(
        &self,
        drives: &[char],
        no_cache: bool,
        lifecycle: &crate::lifecycle::LifecycleHandle,
    ) {
        let total = drives.len();
        {
            let mut status = self.status.write().await;
            *status = DaemonStatus::Loading {
                drives_loaded: 0,
                drives_total: total,
            };
        }

        // Spawn all drives in parallel on blocking threads.
        let mut join_set = tokio::task::JoinSet::new();
        for &letter in drives {
            tracing::info!(drive = %letter, "Loading live drive (parallel)");
            join_set.spawn_blocking(move || {
                let result = uffs_core::compact::load_drive(
                    &uffs_core::compact::MftSource::Live(letter),
                    no_cache,
                );
                (letter, result)
            });
        }

        // Collect results as they complete (fastest drive first).
        // Each join_next() gets a per-drive timeout so one stuck
        // volume can't block the entire daemon indefinitely.
        let mut loaded: usize = 0;
        loop {
            let next = tokio::time::timeout(Self::DRIVE_LOAD_TIMEOUT, join_set.join_next()).await;

            let join_result = match next {
                Ok(Some(jr)) => jr,
                Ok(None) => break, // all tasks finished
                Err(_elapsed) => {
                    // Timeout — at least one drive is stuck.
                    let remaining = total.saturating_sub(loaded);
                    tracing::error!(
                        remaining,
                        timeout_secs = Self::DRIVE_LOAD_TIMEOUT.as_secs(),
                        "Drive load timed out — skipping remaining drives"
                    );
                    // Abort the remaining stuck tasks (best-effort;
                    // kernel-mode I/O may not be interruptible, but
                    // process::exit at daemon shutdown will clean up).
                    join_set.abort_all();
                    loaded = total;
                    break;
                }
            };

            match join_result {
                Ok((letter, Ok((drive_index, timing)))) => {
                    loaded += 1;
                    let records = drive_index.records.len();
                    tracing::info!(
                        drive = %letter,
                        records,
                        mft_ms = timing.mft,
                        compact_ms = timing.compact,
                        trigram_ms = timing.trigram,
                        loaded,
                        total,
                        "Live drive loaded"
                    );
                    self.events.emit(DaemonEvent::DriveLoaded {
                        drive: letter,
                        records,
                        mft_ms: timing.mft,
                        compact_ms: timing.compact,
                        trigram_ms: timing.trigram,
                        drives_loaded: loaded,
                        drives_total: total,
                    });
                    // Store timing for profile reporting.
                    self.drive_timings
                        .write()
                        .await
                        .insert(letter, StoredDriveTiming {
                            cache: timing.cache,
                            mft: timing.mft,
                            compact: timing.compact,
                            trigram: timing.trigram,
                        });
                    let mut backend = self.backend.write().await;
                    backend.drives.push(drive_index);
                }
                Ok((letter, Err(err))) => {
                    loaded += 1;
                    tracing::error!(drive = %letter, error = %err, "Failed to load live drive");
                }
                Err(err) => {
                    loaded += 1;
                    tracing::error!(error = %err, "Task panicked loading drive");
                }
            }

            // Update load heartbeat — tells the idle timer we're still
            // making progress, preventing a false stall-timeout.
            lifecycle.record_load_progress();

            let mut status = self.status.write().await;
            *status = DaemonStatus::Loading {
                drives_loaded: loaded,
                drives_total: total,
            };
        }

        self.set_ready().await;

        let backend = self.backend.read().await;
        let drive_count = backend.drives.len();
        let total_records = backend.total_records();
        drop(backend);
        self.events.emit(DaemonEvent::DaemonReady {
            drives: drive_count,
            total_records,
            startup_ms: self.start_time.elapsed().as_millis(),
        });
    }

    /// Transition to `Ready` and record startup duration (idempotent).
    async fn set_ready(&self) {
        let mut status = self.status.write().await;
        *status = DaemonStatus::Ready;
        drop(status);
        // Record only the first transition.
        let elapsed_us = u64::try_from(self.start_time.elapsed().as_micros()).unwrap_or(u64::MAX);
        // Only record the first transition; ignore the result.
        let _already_set = self.startup_duration_us.compare_exchange(
            0,
            elapsed_us,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    /// Execute a search query (updates perf counters).
    ///
    /// When `params.profile` is `true`, populates `SearchResponse::profile`
    /// with a per-phase timing breakdown so the CLI can print it.
    #[allow(clippy::too_many_lines)]
    pub async fn search(&self, params: &SearchParams) -> SearchResponse {
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

        // ── Lock acquisition ────────────────────────────────────────
        let t_lock = profiling.then(Instant::now);
        let mut backend = self.backend.write().await;
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
        backend.sort_column = sort_column;
        backend.sort_desc = sort_desc;
        backend.extra_sort_tiers = extra_sort_tiers;

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
            backend
                .drives
                .iter()
                .map(|dr| (dr.letter, dr.records.len()))
                .collect()
        } else {
            Vec::new()
        };

        let result = backend.search_drives(
            &effective_params.pattern,
            effective_params.case_sensitive,
            effective_params.whole_word,
            effective_params.match_path,
            if requires_post_filter {
                None
            } else {
                effective_params.limit
            },
            filter_mode,
            &mut filters,
            &effective_params.drives,
        );
        let search_us = if profiling {
            result.duration.as_micros()
        } else {
            0
        };

        drop(backend);

        // ── Row building ────────────────────────────────────────────
        let t_rows = profiling.then(Instant::now);
        let mut filtered_rows = result.rows;
        if requires_post_filter {
            filtered_rows.retain(|row| Self::matches_predicates(row, &effective_params.predicates));
        }
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

        SearchResponse {
            rows: if projected_rows.is_some() {
                Vec::new()
            } else {
                rows
            },
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
        let backend = self.backend.read().await;
        DrivesResponse {
            drives: backend
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

    /// Get daemon performance statistics.
    #[expect(
        clippy::cast_precision_loss,
        clippy::float_arithmetic,
        clippy::default_numeric_fallback,
        reason = "stats are approximate; f64 precision is fine for monitoring"
    )]
    pub async fn stats(&self) -> StatsResponse {
        let total_queries = self.queries_total.load(Ordering::Relaxed);
        let total_us = self.queries_total_us.load(Ordering::Relaxed);
        let startup_us = self.startup_duration_us.load(Ordering::Relaxed);
        let uptime_secs = self.start_time.elapsed().as_secs();
        let total_records = self.total_records().await;

        let avg_query_us = if total_queries > 0 {
            total_us as f64 / total_queries as f64
        } else {
            0.0
        };
        let qps = if uptime_secs > 0 {
            total_queries as f64 / uptime_secs as f64
        } else {
            0.0
        };

        StatsResponse {
            total_queries,
            total_query_time_us: total_us,
            avg_query_time_us: avg_query_us,
            startup_duration_ms: startup_us / 1000,
            uptime_secs,
            total_records,
            queries_per_second: qps,
        }
    }

    /// Get current daemon status.
    ///
    /// Includes `has_drives` and `total_records` for completeness.
    pub async fn status(&self, connections: usize) -> StatusResponse {
        let status = self.status.read().await;
        let loaded = self.has_drives().await;
        let records = self.total_records().await;
        tracing::trace!(
            has_drives = loaded,
            total_records = records,
            "Status queried"
        );
        StatusResponse {
            status: status.clone(),
            uptime_secs: self.start_time.elapsed().as_secs(),
            connections,
            pid: std::process::id(),
        }
    }

    /// Refresh specific drives (or all if empty).
    pub async fn refresh(&self, drives: &[char]) {
        let drives_to_refresh: Vec<char> = if drives.is_empty() {
            let backend = self.backend.read().await;
            backend.drives.iter().map(|dr| dr.letter).collect()
        } else {
            drives.to_vec()
        };

        self.events.emit(DaemonEvent::RefreshStarted {
            drives: drives_to_refresh.clone(),
        });

        let mut refresh_status = self.status.write().await;
        *refresh_status = DaemonStatus::Refreshing {
            drives: drives_to_refresh.clone(),
        };
        drop(refresh_status);

        // Refresh each drive sequentially
        for &letter in &drives_to_refresh {
            // Find the drive source to reload
            let backend_snap = self.backend.read().await;
            let drive_source = backend_snap
                .drives
                .iter()
                .find(|dr| dr.letter == letter)
                .map(|dr| dr.source.clone());
            drop(backend_snap);

            let Some(source) = drive_source else {
                tracing::warn!(drive = %letter, "Drive not found for refresh");
                continue;
            };

            let result = tokio::task::spawn_blocking(move || match &source {
                uffs_core::compact::IndexSource::MftFile(mft_path) => {
                    let mft_source = if mft_path.to_string_lossy().len() <= 2 {
                        #[cfg(windows)]
                        {
                            uffs_core::compact::MftSource::Live(letter)
                        }
                        #[cfg(not(windows))]
                        {
                            return Err(anyhow::anyhow!(
                                "Cannot refresh live drive on non-Windows"
                            ));
                        }
                    } else {
                        uffs_core::compact::MftSource::File(mft_path.clone(), Some(letter))
                    };
                    uffs_core::compact::load_drive(&mft_source, false)
                }
            })
            .await;

            match result {
                Ok(Ok((new_drive, timing))) => {
                    let records = new_drive.records.len();
                    let mut backend_wr = self.backend.write().await;
                    if let Some(pos) = backend_wr.drives.iter().position(|dr| dr.letter == letter) {
                        // `pos` was just validated by `position()`.
                        #[expect(clippy::indexing_slicing, reason = "pos from position()")]
                        {
                            backend_wr.drives[pos] = new_drive;
                        }
                    } else {
                        backend_wr.drives.push(new_drive);
                    }
                    drop(backend_wr);
                    tracing::info!(
                        drive = %letter,
                        records,
                        mft_ms = timing.mft,
                        compact_ms = timing.compact,
                        trigram_ms = timing.trigram,
                        "Drive refreshed"
                    );
                    self.events.emit(DaemonEvent::DriveRefreshed {
                        drive: letter,
                        records,
                        mft_ms: timing.mft,
                        compact_ms: timing.compact,
                        trigram_ms: timing.trigram,
                    });
                }
                Ok(Err(refresh_err)) => {
                    tracing::error!(drive = %letter, error = %refresh_err, "Failed to refresh drive");
                }
                Err(join_err) => {
                    tracing::error!(drive = %letter, error = %join_err, "Task panicked during refresh");
                }
            }
        }

        self.set_ready().await;
        self.events.emit(DaemonEvent::RefreshComplete {
            drives_refreshed: drives_to_refresh.len(),
        });
    }

    /// Look up a file by path and return all available fields (D2.3.7).
    pub async fn info(&self, file_path: &str) -> uffs_client::protocol::InfoResponse {
        let backend = self.backend.read().await;
        let path_lower = file_path.to_ascii_lowercase();

        let mut found_record = None;

        // Search all drives for a matching path
        'outer: for drive in &backend.drives {
            let volume_prefix = format!("{}:\\", drive.letter);
            for (idx, rec) in drive.records.iter().enumerate() {
                if rec.name_len == 0 {
                    continue;
                }
                let resolved = uffs_core::search::tree::resolve_path(drive, idx, &volume_prefix);
                if resolved.to_ascii_lowercase() == path_lower {
                    let name = rec.name(&drive.names);
                    found_record = Some(serde_json::json!({
                        "drive": drive.letter.to_string(),
                        "path": resolved,
                        "name": name,
                        "size": rec.size,
                        "allocated": rec.allocated,
                        "treesize": rec.treesize,
                        "created": rec.created,
                        "modified": rec.modified,
                        "accessed": rec.accessed,
                        "flags": rec.flags,
                        "is_directory": rec.is_directory(),
                        "descendants": rec.descendants,
                        "parent_idx": rec.parent_idx,
                        "extension_id": rec.extension_id,
                    }));
                    break 'outer;
                }
            }
        }

        drop(backend);

        uffs_client::protocol::InfoResponse {
            found: found_record.is_some(),
            record: found_record,
        }
    }

    /// Get the configured data directory, if any.
    #[must_use]
    pub fn data_dir(&self) -> Option<&std::path::Path> {
        self.data_dir.as_deref()
    }

    /// Check if the daemon has any loaded drives.
    pub async fn has_drives(&self) -> bool {
        let backend = self.backend.read().await;
        !backend.drives.is_empty()
    }

    /// Total records across all drives.
    pub async fn total_records(&self) -> usize {
        let backend = self.backend.read().await;
        backend.total_records()
    }

    /// Return the set of currently loaded drive letters.
    pub async fn loaded_drive_letters(&self) -> Vec<char> {
        let backend = self.backend.read().await;
        backend.drives.iter().map(|dr| dr.letter).collect()
    }

    /// Hot-load a single MFT file if its drive letter is not already loaded.
    ///
    /// Returns `Ok(Some(letter))` if loaded, `Ok(None)` if already present.
    pub async fn load_single_mft_file(
        &self,
        mft_path: &std::path::Path,
        no_cache: bool,
    ) -> anyhow::Result<Option<char>> {
        // Infer drive letter from filename (e.g. G_mft.iocp → 'G').
        let letter = {
            let stem = mft_path.file_name().and_then(|n| n.to_str()).unwrap_or("X");
            stem.chars()
                .next()
                .filter(char::is_ascii_alphabetic)
                .map_or('X', |ch| ch.to_ascii_uppercase())
        };

        // Skip if already loaded.
        {
            let backend = self.backend.read().await;
            if backend.drives.iter().any(|dr| dr.letter == letter) {
                tracing::debug!(drive = %letter, "Drive already loaded, skipping");
                return Ok(None);
            }
        }

        tracing::info!(
            drive = %letter,
            path = %mft_path.display(),
            "Hot-loading MFT file"
        );

        let cloned_path = mft_path.to_path_buf();
        let source = uffs_core::compact::MftSource::File(cloned_path, None);
        let result =
            tokio::task::spawn_blocking(move || uffs_core::compact::load_drive(&source, no_cache))
                .await;

        match result {
            Ok(Ok((drive_index, timing))) => {
                let records = drive_index.records.len();
                tracing::info!(
                    drive = %letter,
                    records,
                    mft_ms = timing.mft,
                    compact_ms = timing.compact,
                    trigram_ms = timing.trigram,
                    "Drive hot-loaded"
                );
                self.events.emit(DaemonEvent::DriveLoaded {
                    drive: letter,
                    records,
                    mft_ms: timing.mft,
                    compact_ms: timing.compact,
                    trigram_ms: timing.trigram,
                    drives_loaded: 1,
                    drives_total: 1,
                });
                let mut backend = self.backend.write().await;
                backend.drives.push(drive_index);
                drop(backend);
                Ok(Some(letter))
            }
            Ok(Err(load_err)) => {
                tracing::error!(
                    path = %mft_path.display(),
                    error = %load_err,
                    "Failed to hot-load MFT file"
                );
                Err(load_err)
            }
            Err(join_err) => {
                tracing::error!(
                    path = %mft_path.display(),
                    error = %join_err,
                    "Task panicked hot-loading MFT"
                );
                anyhow::bail!("Task panicked: {join_err}")
            }
        }
    }

    /// Discover and load a missing drive from the data directory.
    ///
    /// Returns `Ok(true)` if the drive was discovered and loaded,
    /// `Ok(false)` if no MFT file was found for it, or an error.
    pub async fn discover_and_load_drive(
        &self,
        drive_letter: char,
        no_cache: bool,
    ) -> anyhow::Result<bool> {
        let Some(data_dir) = &self.data_dir else {
            return Ok(false);
        };

        let drive_lower = drive_letter.to_ascii_lowercase();
        let drive_subdir = data_dir.join(format!("drive_{drive_lower}"));

        if !drive_subdir.is_dir() {
            tracing::debug!(
                drive = %drive_letter,
                path = %drive_subdir.display(),
                "No drive_X directory found in data_dir"
            );
            return Ok(false);
        }

        let Some(mft_path) = uffs_mft::discovery::find_best_mft_file(&drive_subdir) else {
            tracing::debug!(
                drive = %drive_letter,
                path = %drive_subdir.display(),
                "No MFT file found in drive directory"
            );
            return Ok(false);
        };

        // Whether Some (freshly loaded) or None (already present), the
        // drive is now available.
        let _loaded = self.load_single_mft_file(&mft_path, no_cache).await?;
        Ok(true)
    }

    /// Ensure all requested drives are loaded, auto-discovering from
    /// `data_dir` if available.
    ///
    /// Returns a list of drive letters that could NOT be loaded (no data
    /// source found).
    pub async fn ensure_drives_loaded(&self, drives: &[char], no_cache: bool) -> Vec<char> {
        if drives.is_empty() {
            return Vec::new();
        }

        let loaded = self.loaded_drive_letters().await;
        let mut missing: Vec<char> = Vec::new();

        for &letter in drives {
            let upper = letter.to_ascii_uppercase();
            if loaded.contains(&upper) {
                continue;
            }

            // Try to auto-discover from data_dir.
            match self.discover_and_load_drive(upper, no_cache).await {
                Ok(true) => {
                    tracing::info!(drive = %upper, "Auto-discovered and loaded missing drive");
                }
                Ok(false) => {
                    tracing::warn!(
                        drive = %upper,
                        "Drive not loaded and not discoverable from data_dir"
                    );
                    missing.push(upper);
                }
                Err(load_err) => {
                    tracing::error!(
                        drive = %upper,
                        error = %load_err,
                        "Failed to auto-load drive"
                    );
                    missing.push(upper);
                }
            }
        }

        missing
    }

    // ── Private helpers ─────────────────────────────────────────────

    /// Convert a [`DisplayRow`] to a protocol [`SearchRow`].
    #[expect(
        clippy::single_call_fn,
        reason = "type-conversion helper — clarity over inlining"
    )]
    fn display_row_to_search_row(row: &DisplayRow) -> SearchRow {
        SearchRow {
            drive: row.drive,
            path: row.path.clone(),
            name: row.name().to_owned(),
            size: row.size,
            is_directory: row.is_directory,
            modified: row.modified,
            created: row.created,
            accessed: row.accessed,
            flags: row.flags,
            allocated: row.allocated,
            descendants: row.descendants,
            treesize: row.treesize,
            tree_allocated: row.tree_allocated,
        }
    }

    /// Normalize the effective canonical sort clauses supported by the daemon.
    #[must_use]
    #[allow(clippy::single_call_fn)]
    fn resolve_applied_sorts(params: &SearchParams) -> Vec<SearchSortSpec> {
        params
            .resolved_sorts()
            .into_iter()
            .filter_map(|spec| {
                let field = FieldId::parse(&spec.field)?;
                if !field.metadata().sortable {
                    return None;
                }
                let direction = spec.direction.or_else(|| {
                    Some(
                        if matches!(
                            field.default_sort_direction(),
                            Some(SortDirection::Descending)
                        ) {
                            SearchSortDirection::Desc
                        } else {
                            SearchSortDirection::Asc
                        },
                    )
                });
                Some(SearchSortSpec {
                    field: field.canonical_name().to_owned(),
                    direction,
                })
            })
            .collect()
    }

    /// Convert a canonical sort clause to backend sorting state.
    #[must_use]
    fn sort_spec_to_backend(spec: &SearchSortSpec) -> Option<(FieldId, bool)> {
        let field = FieldId::parse(&spec.field)?;
        if !field.metadata().sortable {
            return None;
        }
        let descending =
            spec.direction.unwrap_or(SearchSortDirection::Asc) == SearchSortDirection::Desc;
        Some((field, descending))
    }

    /// Normalize the effective projection fields supported by the daemon.
    #[must_use]
    #[allow(clippy::single_call_fn)]
    fn resolve_projection_fields(projection: &[String]) -> Vec<FieldId> {
        let mut resolved = Vec::new();
        for raw in projection {
            if let Some(field) = FieldId::parse(raw)
                && !resolved.contains(&field)
            {
                resolved.push(field);
            }
        }
        resolved
    }

    /// Build one projected JSON object from a `SearchRow`.
    #[must_use]
    #[allow(clippy::single_call_fn)]
    fn project_search_row(
        row: &SearchRow,
        projection: &[FieldId],
    ) -> serde_json::Map<String, serde_json::Value> {
        projection
            .iter()
            .map(|&field| {
                (
                    field.canonical_name().to_owned(),
                    Self::projected_value(row, field),
                )
            })
            .collect()
    }

    /// Convert one canonical field from a `SearchRow` into JSON.
    ///
    /// Kept as a named helper (54-line match) for readability — the caller
    /// is already a nested iterator.
    #[must_use]
    #[expect(
        clippy::single_call_fn,
        reason = "54-arm match is clearer as a named helper"
    )]
    fn projected_value(row: &SearchRow, field: FieldId) -> serde_json::Value {
        match field {
            FieldId::Drive => serde_json::Value::String(row.drive.to_string()),
            FieldId::Path => serde_json::Value::String(row.path.clone()),
            FieldId::Name => serde_json::Value::String(row.name.clone()),
            FieldId::PathOnly => serde_json::Value::String(
                row.path
                    .rsplit_once('\\')
                    .map_or_else(String::new, |(path_only, _)| path_only.to_owned()),
            ),
            FieldId::Size => serde_json::Value::from(row.size),
            FieldId::SizeOnDisk => serde_json::Value::from(row.allocated),
            FieldId::Created => serde_json::Value::from(row.created),
            FieldId::Modified => serde_json::Value::from(row.modified),
            FieldId::Accessed => serde_json::Value::from(row.accessed),
            FieldId::Extension => {
                serde_json::Value::String(Self::search_row_extension(row).to_owned())
            }
            FieldId::Type => serde_json::Value::String(Self::search_row_type(row).to_owned()),
            FieldId::Attributes | FieldId::AttributeValue => serde_json::Value::from(row.flags),
            FieldId::Hidden => serde_json::Value::from(Self::flag_set(row.flags, "hidden")),
            FieldId::System => serde_json::Value::from(Self::flag_set(row.flags, "system")),
            FieldId::Archive => serde_json::Value::from(Self::flag_set(row.flags, "archive")),
            FieldId::ReadOnly => serde_json::Value::from(Self::flag_set(row.flags, "readonly")),
            FieldId::Compressed => serde_json::Value::from(Self::flag_set(row.flags, "compressed")),
            FieldId::Encrypted => serde_json::Value::from(Self::flag_set(row.flags, "encrypted")),
            FieldId::Sparse => serde_json::Value::from(Self::flag_set(row.flags, "sparse")),
            FieldId::Reparse => serde_json::Value::from(Self::flag_set(row.flags, "reparse")),
            FieldId::Offline => serde_json::Value::from(Self::flag_set(row.flags, "offline")),
            FieldId::NotIndexed => serde_json::Value::from(Self::flag_set(row.flags, "notindexed")),
            FieldId::Temporary => serde_json::Value::from(Self::flag_set(row.flags, "temporary")),
            FieldId::Virtual => serde_json::Value::from(Self::flag_set(row.flags, "virtual")),
            FieldId::Pinned => serde_json::Value::from(Self::flag_set(row.flags, "pinned")),
            FieldId::Unpinned => serde_json::Value::from(Self::flag_set(row.flags, "unpinned")),
            FieldId::Descendants => serde_json::Value::from(row.descendants),
            FieldId::TreeSize => serde_json::Value::from(row.treesize),
            FieldId::TreeAllocated => serde_json::Value::from(Self::search_row_tree_allocated(row)),
            FieldId::Bulkiness => serde_json::Value::from(Self::search_row_bulkiness(row)),
            FieldId::Integrity => serde_json::Value::from(Self::flag_set(row.flags, "integrity")),
            FieldId::NoScrub => serde_json::Value::from(Self::flag_set(row.flags, "noscrub")),
            FieldId::DirectoryFlag => serde_json::Value::from(row.is_directory),
            FieldId::RecallOnOpen => {
                serde_json::Value::from(row.flags & Self::FLAG_RECALL_ON_OPEN != 0)
            }
            FieldId::RecallOnDataAccess => {
                serde_json::Value::from(row.flags & Self::FLAG_RECALL_ON_DATA_ACCESS != 0)
            }
            FieldId::ParityAttributes => {
                serde_json::Value::from(row.flags & Self::PARITY_FLAG_MASK)
            }
            FieldId::NameLength => serde_json::Value::from(row.name.chars().count()),
            FieldId::PathLength => serde_json::Value::from(row.path.chars().count()),
        }
    }

    /// Return whether any canonical predicates require daemon-side
    /// post-filtering.
    ///
    /// A predicate is "hot" (handled by `SearchFilters` without post-filter)
    /// when its field has `FieldAccess::Hot` and the hot-path filter pipeline
    /// already covers its operator.  Everything else needs post-filtering
    /// against the materialised `DisplayRow`.
    #[must_use]
    #[allow(clippy::single_call_fn)]
    fn predicates_require_post_filter(predicates: &[SearchPredicate]) -> bool {
        predicates.iter().any(|predicate| {
            let Some(field) = FieldId::parse(&predicate.field) else {
                return true;
            };
            // The hot-path `compile_predicates_into_filters` compiles these
            // field+op combinations into `SearchFilters` so they run inside the
            // compact record loop.  Anything not listed here needs post-filter.
            let compiled_to_hot_path = match field {
                // Size: Gte/Lte/Gt/Lt compiled into min_size/max_size.
                FieldId::Size => matches!(
                    predicate.op,
                    SearchPredicateOp::Gte
                        | SearchPredicateOp::Lte
                        | SearchPredicateOp::Gt
                        | SearchPredicateOp::Lt
                ),
                // Descendants: Gte/Lte/Gt/Lt compiled into min/max_descendants.
                FieldId::Descendants => matches!(
                    predicate.op,
                    SearchPredicateOp::Gte
                        | SearchPredicateOp::Lte
                        | SearchPredicateOp::Gt
                        | SearchPredicateOp::Lt
                ),
                // Timestamps: Gte/Lt compiled into newer_*/older_* bounds.
                FieldId::Modified | FieldId::Created | FieldId::Accessed => {
                    matches!(predicate.op, SearchPredicateOp::Gte | SearchPredicateOp::Lt)
                }
                // Extension: In compiled into extensions list.
                FieldId::Extension => predicate.op == SearchPredicateOp::In,
                // Attributes: HasAll/HasNone compiled into attr_require/exclude.
                FieldId::Attributes => matches!(
                    predicate.op,
                    SearchPredicateOp::HasAll | SearchPredicateOp::HasNone
                ),
                // Name: NotMatch compiled into exclude_lower glob.
                FieldId::Name => predicate.op == SearchPredicateOp::NotMatch,
                FieldId::Drive
                | FieldId::Path
                | FieldId::PathOnly
                | FieldId::SizeOnDisk
                | FieldId::Type
                | FieldId::AttributeValue
                | FieldId::Hidden
                | FieldId::System
                | FieldId::Archive
                | FieldId::ReadOnly
                | FieldId::Compressed
                | FieldId::Encrypted
                | FieldId::Sparse
                | FieldId::Reparse
                | FieldId::Offline
                | FieldId::NotIndexed
                | FieldId::Temporary
                | FieldId::Virtual
                | FieldId::Pinned
                | FieldId::Unpinned
                | FieldId::TreeSize
                | FieldId::TreeAllocated
                | FieldId::Bulkiness
                | FieldId::Integrity
                | FieldId::NoScrub
                | FieldId::DirectoryFlag
                | FieldId::RecallOnOpen
                | FieldId::RecallOnDataAccess
                | FieldId::ParityAttributes => false,
                // Length predicates are compiled into hot-path min/max filters.
                FieldId::NameLength | FieldId::PathLength => {
                    matches!(
                        predicate.op,
                        SearchPredicateOp::Gte
                            | SearchPredicateOp::Lte
                            | SearchPredicateOp::Gt
                            | SearchPredicateOp::Lt
                            | SearchPredicateOp::Eq
                    ) && matches!(predicate.value, SearchPredicateValue::U64(_))
                }
            };
            !compiled_to_hot_path
        })
    }

    /// Overlay canonical predicates onto an existing `SearchFilters`.
    ///
    /// This compiles hot-path predicates into the compiled filter fields
    /// so they are evaluated during the fast record loop rather than in
    /// the slower post-filter pass.  Predicates that cannot be compiled
    /// into the hot path are silently skipped — they will be handled by
    /// `matches_predicate` during post-filtering.
    #[allow(
        clippy::single_call_fn,
        clippy::wildcard_enum_match_arm,
        clippy::too_many_lines
    )]
    fn compile_predicates_into_filters(
        filters: &mut SearchFilters,
        predicates: &[SearchPredicate],
    ) {
        for predicate in predicates {
            let Some(field) = FieldId::parse(&predicate.field) else {
                continue;
            };
            match field {
                FieldId::Size => {
                    if let SearchPredicateValue::U64(val) = &predicate.value {
                        match predicate.op {
                            SearchPredicateOp::Gte => {
                                let merged = filters.min_size.map_or(*val, |cur| cur.max(*val));
                                filters.min_size = Some(merged);
                            }
                            SearchPredicateOp::Lte => {
                                let merged = filters.max_size.map_or(*val, |cur| cur.min(*val));
                                filters.max_size = Some(merged);
                            }
                            SearchPredicateOp::Gt => {
                                let lower = val.saturating_add(1);
                                let merged = filters.min_size.map_or(lower, |cur| cur.max(lower));
                                filters.min_size = Some(merged);
                            }
                            SearchPredicateOp::Lt => {
                                let upper = val.saturating_sub(1);
                                let merged = filters.max_size.map_or(upper, |cur| cur.min(upper));
                                filters.max_size = Some(merged);
                            }
                            SearchPredicateOp::Eq
                            | SearchPredicateOp::Ne
                            | SearchPredicateOp::In
                            | SearchPredicateOp::NotIn
                            | SearchPredicateOp::HasAll
                            | SearchPredicateOp::HasAny
                            | SearchPredicateOp::HasNone
                            | SearchPredicateOp::Match
                            | SearchPredicateOp::NotMatch
                            | SearchPredicateOp::Contains
                            | SearchPredicateOp::StartsWith
                            | SearchPredicateOp::EndsWith => {}
                        }
                    }
                }
                FieldId::Descendants => {
                    if let SearchPredicateValue::U64(val) = &predicate.value {
                        let val32 = u32::try_from(*val).unwrap_or(u32::MAX);
                        match predicate.op {
                            SearchPredicateOp::Gte => {
                                let merged =
                                    filters.min_descendants.map_or(val32, |cur| cur.max(val32));
                                filters.min_descendants = Some(merged);
                            }
                            SearchPredicateOp::Lte => {
                                let merged =
                                    filters.max_descendants.map_or(val32, |cur| cur.min(val32));
                                filters.max_descendants = Some(merged);
                            }
                            SearchPredicateOp::Gt => {
                                let lower = val32.saturating_add(1);
                                let merged =
                                    filters.min_descendants.map_or(lower, |cur| cur.max(lower));
                                filters.min_descendants = Some(merged);
                            }
                            SearchPredicateOp::Lt => {
                                let upper = val32.saturating_sub(1);
                                let merged =
                                    filters.max_descendants.map_or(upper, |cur| cur.min(upper));
                                filters.max_descendants = Some(merged);
                            }
                            SearchPredicateOp::Eq
                            | SearchPredicateOp::Ne
                            | SearchPredicateOp::In
                            | SearchPredicateOp::NotIn
                            | SearchPredicateOp::HasAll
                            | SearchPredicateOp::HasAny
                            | SearchPredicateOp::HasNone
                            | SearchPredicateOp::Match
                            | SearchPredicateOp::NotMatch
                            | SearchPredicateOp::Contains
                            | SearchPredicateOp::StartsWith
                            | SearchPredicateOp::EndsWith => {}
                        }
                    }
                }
                // ── Timestamp predicates (string time specs → i64 µs) ──
                FieldId::Modified | FieldId::Created | FieldId::Accessed => {
                    if let SearchPredicateValue::String(spec) = &predicate.value {
                        let now_us = uffs_core::search::filters::now_unix_micros();
                        let is_newer =
                            matches!(predicate.op, SearchPredicateOp::Gte | SearchPredicateOp::Gt);
                        if let Some(bound) =
                            uffs_core::search::filters::parse_time_bound(spec, now_us, is_newer)
                        {
                            match (field, &predicate.op) {
                                (FieldId::Modified, SearchPredicateOp::Gte) => {
                                    let merged =
                                        filters.newer_us.map_or(bound, |cur| cur.max(bound));
                                    filters.newer_us = Some(merged);
                                }
                                (FieldId::Modified, SearchPredicateOp::Lt) => {
                                    let merged =
                                        filters.older_us.map_or(bound, |cur| cur.min(bound));
                                    filters.older_us = Some(merged);
                                }
                                (FieldId::Created, SearchPredicateOp::Gte) => {
                                    let merged = filters
                                        .newer_created_us
                                        .map_or(bound, |cur| cur.max(bound));
                                    filters.newer_created_us = Some(merged);
                                }
                                (FieldId::Created, SearchPredicateOp::Lt) => {
                                    let merged = filters
                                        .older_created_us
                                        .map_or(bound, |cur| cur.min(bound));
                                    filters.older_created_us = Some(merged);
                                }
                                (FieldId::Accessed, SearchPredicateOp::Gte) => {
                                    let merged = filters
                                        .newer_accessed_us
                                        .map_or(bound, |cur| cur.max(bound));
                                    filters.newer_accessed_us = Some(merged);
                                }
                                (FieldId::Accessed, SearchPredicateOp::Lt) => {
                                    let merged = filters
                                        .older_accessed_us
                                        .map_or(bound, |cur| cur.min(bound));
                                    filters.older_accessed_us = Some(merged);
                                }
                                _ => {}
                            }
                        }
                    } else if let SearchPredicateValue::I64(val) = &predicate.value {
                        // Direct i64 timestamp µs value.
                        match (field, &predicate.op) {
                            (FieldId::Modified, SearchPredicateOp::Gte) => {
                                let merged = filters.newer_us.map_or(*val, |cur| cur.max(*val));
                                filters.newer_us = Some(merged);
                            }
                            (FieldId::Modified, SearchPredicateOp::Lt) => {
                                let merged = filters.older_us.map_or(*val, |cur| cur.min(*val));
                                filters.older_us = Some(merged);
                            }
                            (FieldId::Created, SearchPredicateOp::Gte) => {
                                let merged =
                                    filters.newer_created_us.map_or(*val, |cur| cur.max(*val));
                                filters.newer_created_us = Some(merged);
                            }
                            (FieldId::Created, SearchPredicateOp::Lt) => {
                                let merged =
                                    filters.older_created_us.map_or(*val, |cur| cur.min(*val));
                                filters.older_created_us = Some(merged);
                            }
                            (FieldId::Accessed, SearchPredicateOp::Gte) => {
                                let merged =
                                    filters.newer_accessed_us.map_or(*val, |cur| cur.max(*val));
                                filters.newer_accessed_us = Some(merged);
                            }
                            (FieldId::Accessed, SearchPredicateOp::Lt) => {
                                let merged =
                                    filters.older_accessed_us.map_or(*val, |cur| cur.min(*val));
                                filters.older_accessed_us = Some(merged);
                            }
                            _ => {}
                        }
                    }
                }
                // ── Extension predicate → hot-path ext filter ──────────
                FieldId::Extension if predicate.op == SearchPredicateOp::In => {
                    if let SearchPredicateValue::StringList(values) = &predicate.value {
                        filters.extensions.extend(values.iter().cloned());
                    }
                }
                // ── Attribute predicates → hot-path attr bitmask ───────
                FieldId::Attributes => {
                    if let SearchPredicateValue::StringList(values) = &predicate.value {
                        match predicate.op {
                            SearchPredicateOp::HasAll => {
                                for name in values {
                                    filters.attr_require |=
                                        uffs_core::search::filters::attr_bit(name);
                                }
                            }
                            SearchPredicateOp::HasNone => {
                                for name in values {
                                    filters.attr_exclude |=
                                        uffs_core::search::filters::attr_bit(name);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                // ── Exclude pattern → hot-path exclude glob ────────────
                FieldId::Name if predicate.op == SearchPredicateOp::NotMatch => {
                    if let SearchPredicateValue::String(pattern) = &predicate.value {
                        filters.exclude_lower = Some(pattern.to_ascii_lowercase());
                    }
                }
                // ── Name/path length → hot-path length filters ──────────
                FieldId::NameLength => {
                    if let SearchPredicateValue::U64(val) = &predicate.value {
                        let val16 = u16::try_from(*val).unwrap_or(u16::MAX);
                        match predicate.op {
                            SearchPredicateOp::Gte => {
                                let merged =
                                    filters.min_name_len.map_or(val16, |cur| cur.max(val16));
                                filters.min_name_len = Some(merged);
                            }
                            SearchPredicateOp::Lte => {
                                let merged =
                                    filters.max_name_len.map_or(val16, |cur| cur.min(val16));
                                filters.max_name_len = Some(merged);
                            }
                            SearchPredicateOp::Gt => {
                                let lower = val16.saturating_add(1);
                                let merged =
                                    filters.min_name_len.map_or(lower, |cur| cur.max(lower));
                                filters.min_name_len = Some(merged);
                            }
                            SearchPredicateOp::Lt => {
                                let upper = val16.saturating_sub(1);
                                let merged =
                                    filters.max_name_len.map_or(upper, |cur| cur.min(upper));
                                filters.max_name_len = Some(merged);
                            }
                            SearchPredicateOp::Eq => {
                                filters.min_name_len = Some(val16);
                                filters.max_name_len = Some(val16);
                            }
                            _ => {}
                        }
                    }
                }
                FieldId::PathLength => {
                    if let SearchPredicateValue::U64(val) = &predicate.value {
                        let val16 = u16::try_from(*val).unwrap_or(u16::MAX);
                        match predicate.op {
                            SearchPredicateOp::Gte => {
                                let merged =
                                    filters.min_path_len.map_or(val16, |cur| cur.max(val16));
                                filters.min_path_len = Some(merged);
                            }
                            SearchPredicateOp::Lte => {
                                let merged =
                                    filters.max_path_len.map_or(val16, |cur| cur.min(val16));
                                filters.max_path_len = Some(merged);
                            }
                            SearchPredicateOp::Gt => {
                                let lower = val16.saturating_add(1);
                                let merged =
                                    filters.min_path_len.map_or(lower, |cur| cur.max(lower));
                                filters.min_path_len = Some(merged);
                            }
                            SearchPredicateOp::Lt => {
                                let upper = val16.saturating_sub(1);
                                let merged =
                                    filters.max_path_len.map_or(upper, |cur| cur.min(upper));
                                filters.max_path_len = Some(merged);
                            }
                            SearchPredicateOp::Eq => {
                                filters.min_path_len = Some(val16);
                                filters.max_path_len = Some(val16);
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Apply canonical predicates against a materialized display row.
    #[must_use]
    #[allow(clippy::single_call_fn)]
    fn matches_predicates(row: &DisplayRow, predicates: &[SearchPredicate]) -> bool {
        predicates
            .iter()
            .all(|predicate| Self::matches_predicate(row, predicate))
    }

    /// Apply a single canonical predicate.
    #[must_use]
    #[allow(clippy::single_call_fn)]
    fn matches_predicate(row: &DisplayRow, predicate: &SearchPredicate) -> bool {
        let Some(field) = FieldId::parse(&predicate.field) else {
            return true;
        };

        match field {
            FieldId::PathOnly => Self::match_string(row.path_dir(), predicate),
            FieldId::Path => Self::match_string(&row.path, predicate),
            FieldId::Name => Self::match_string(row.name(), predicate),
            FieldId::Drive => Self::match_string(&row.drive.to_string(), predicate),
            FieldId::Extension => Self::match_string(
                row.name().rsplit_once('.').map_or("", |(_, ext)| ext),
                predicate,
            ),
            FieldId::Type => Self::match_string(semantic_type_for_row(row), predicate),
            FieldId::Size => Self::match_u64(row.size, predicate),
            FieldId::SizeOnDisk => Self::match_u64(row.allocated, predicate),
            FieldId::Created => Self::match_i64(row.created, predicate),
            FieldId::Modified => Self::match_i64(row.modified, predicate),
            FieldId::Accessed => Self::match_i64(row.accessed, predicate),
            FieldId::Descendants => Self::match_u64(u64::from(row.descendants), predicate),
            FieldId::TreeSize => Self::match_u64(row.treesize, predicate),
            FieldId::TreeAllocated => Self::match_u64(tree_allocated_for_row(row), predicate),
            FieldId::Bulkiness => Self::match_u64(bulkiness_for_row(row), predicate),
            FieldId::Attributes | FieldId::AttributeValue => {
                Self::match_attributes(row.flags, predicate)
            }
            // ── Bool-typed attribute fields ─────────────────────────
            FieldId::Hidden => Self::match_bool(row.flags & 0x02 != 0, predicate),
            FieldId::System => Self::match_bool(row.flags & 0x04 != 0, predicate),
            FieldId::Archive => Self::match_bool(row.flags & 0x20 != 0, predicate),
            FieldId::ReadOnly => Self::match_bool(row.flags & 0x01 != 0, predicate),
            FieldId::Compressed => Self::match_bool(row.flags & 0x800 != 0, predicate),
            FieldId::Encrypted => Self::match_bool(row.flags & 0x4000 != 0, predicate),
            FieldId::Sparse => Self::match_bool(row.flags & 0x200 != 0, predicate),
            FieldId::Reparse => Self::match_bool(row.flags & 0x400 != 0, predicate),
            FieldId::Offline => Self::match_bool(row.flags & 0x1000 != 0, predicate),
            FieldId::NotIndexed => Self::match_bool(row.flags & 0x2000 != 0, predicate),
            FieldId::Temporary => Self::match_bool(row.flags & 0x100 != 0, predicate),
            FieldId::Virtual => Self::match_bool(row.flags & 0x0001_0000 != 0, predicate),
            FieldId::Pinned => Self::match_bool(row.flags & 0x0008_0000 != 0, predicate),
            FieldId::Unpinned => Self::match_bool(row.flags & 0x0010_0000 != 0, predicate),
            FieldId::Integrity => Self::match_bool(row.flags & 0x8000 != 0, predicate),
            FieldId::NoScrub => Self::match_bool(row.flags & 0x0002_0000 != 0, predicate),
            FieldId::DirectoryFlag => Self::match_bool(row.is_directory, predicate),
            FieldId::RecallOnOpen => {
                Self::match_bool(row.flags & Self::FLAG_RECALL_ON_OPEN != 0, predicate)
            }
            FieldId::RecallOnDataAccess => {
                Self::match_bool(row.flags & Self::FLAG_RECALL_ON_DATA_ACCESS != 0, predicate)
            }
            FieldId::ParityAttributes => {
                Self::match_u64(u64::from(row.flags & Self::PARITY_FLAG_MASK), predicate)
            }
            FieldId::NameLength => Self::match_u64(row.name().chars().count() as u64, predicate),
            FieldId::PathLength => Self::match_u64(row.path.chars().count() as u64, predicate),
        }
    }

    /// Return the extension shown to direct daemon callers.
    #[must_use]
    #[allow(clippy::single_call_fn)]
    fn search_row_extension(row: &SearchRow) -> &str {
        row.name.rsplit_once('.').map_or("", |(_, ext)| ext)
    }

    /// Return the semantic type shown to direct daemon callers.
    #[must_use]
    #[allow(clippy::single_call_fn)]
    fn search_row_type(row: &SearchRow) -> &'static str {
        if row.is_directory {
            "directory"
        } else {
            let temp = DisplayRow::new(
                0,
                row.drive,
                row.path.clone(),
                row.size,
                row.is_directory,
                row.modified,
                row.created,
                row.accessed,
                row.flags,
                row.allocated,
                row.descendants,
                row.treesize,
                row.tree_allocated,
            );
            semantic_type_for_row(&temp)
        }
    }

    /// Return the tree allocated value shown to direct daemon callers.
    #[must_use]
    const fn search_row_tree_allocated(row: &SearchRow) -> u64 {
        if row.is_directory {
            row.tree_allocated
        } else {
            row.allocated
        }
    }

    /// Return the fixed-point bulkiness metric shown to direct daemon callers.
    #[must_use]
    #[allow(clippy::single_call_fn)]
    fn search_row_bulkiness(row: &SearchRow) -> u64 {
        let logical = if row.is_directory {
            row.treesize
        } else {
            row.size
        };
        let allocated = Self::search_row_tree_allocated(row);
        allocated
            .saturating_mul(1_000_000)
            .checked_div(logical)
            .unwrap_or(0)
    }

    /// Match a string predicate.
    #[must_use]
    fn match_string(actual: &str, predicate: &SearchPredicate) -> bool {
        match (&predicate.op, &predicate.value) {
            (SearchPredicateOp::Eq, SearchPredicateValue::String(expected)) => {
                actual.eq_ignore_ascii_case(expected)
            }
            (SearchPredicateOp::Ne, SearchPredicateValue::String(expected)) => {
                !actual.eq_ignore_ascii_case(expected)
            }
            (SearchPredicateOp::In, SearchPredicateValue::StringList(values)) => values
                .iter()
                .any(|value| actual.eq_ignore_ascii_case(value)),
            (SearchPredicateOp::NotIn, SearchPredicateValue::StringList(values)) => values
                .iter()
                .all(|value| !actual.eq_ignore_ascii_case(value)),
            (SearchPredicateOp::Match, SearchPredicateValue::String(pattern)) => {
                Self::wildcard_match(actual, pattern)
            }
            (SearchPredicateOp::NotMatch, SearchPredicateValue::String(pattern)) => {
                !Self::wildcard_match(actual, pattern)
            }
            // Substring containment ops — case-insensitive.
            (SearchPredicateOp::HasAll, SearchPredicateValue::StringList(values)) => {
                let lower = actual.to_ascii_lowercase();
                values
                    .iter()
                    .all(|val| lower.contains(&*val.to_ascii_lowercase()))
            }
            (SearchPredicateOp::HasAny, SearchPredicateValue::StringList(values)) => {
                let lower = actual.to_ascii_lowercase();
                values
                    .iter()
                    .any(|val| lower.contains(&*val.to_ascii_lowercase()))
            }
            (SearchPredicateOp::HasNone, SearchPredicateValue::StringList(values)) => {
                let lower = actual.to_ascii_lowercase();
                values
                    .iter()
                    .all(|val| !lower.contains(&*val.to_ascii_lowercase()))
            }
            // Substring / prefix / suffix ops.
            (SearchPredicateOp::Contains, SearchPredicateValue::String(needle)) => actual
                .to_ascii_lowercase()
                .contains(&*needle.to_ascii_lowercase()),
            (SearchPredicateOp::StartsWith, SearchPredicateValue::String(prefix)) => actual
                .to_ascii_lowercase()
                .starts_with(&*prefix.to_ascii_lowercase()),
            (SearchPredicateOp::EndsWith, SearchPredicateValue::String(suffix)) => actual
                .to_ascii_lowercase()
                .ends_with(&*suffix.to_ascii_lowercase()),
            _ => true,
        }
    }

    /// Case-insensitive wildcard match supporting `*` and `?`.
    #[must_use]
    #[allow(clippy::indexing_slicing)]
    fn wildcard_match(actual_str: &str, pattern_str: &str) -> bool {
        let actual_bytes = actual_str.to_ascii_lowercase().into_bytes();
        let pattern_bytes = pattern_str.to_ascii_lowercase().into_bytes();
        let mut dp = vec![false; actual_bytes.len() + 1];
        dp[0] = true;
        for token in pattern_bytes {
            match token {
                b'*' => {
                    let mut seen = false;
                    for slot in &mut dp {
                        seen |= *slot;
                        *slot = seen;
                    }
                }
                b'?' => {
                    for idx in (1..dp.len()).rev() {
                        dp[idx] = dp[idx - 1];
                    }
                    dp[0] = false;
                }
                byte => {
                    for idx in (1..dp.len()).rev() {
                        dp[idx] = dp[idx - 1] && actual_bytes[idx - 1] == byte;
                    }
                    dp[0] = false;
                }
            }
        }
        dp[actual_bytes.len()]
    }

    /// Match a boolean predicate.
    #[must_use]
    const fn match_bool(actual: bool, predicate: &SearchPredicate) -> bool {
        match (&predicate.op, &predicate.value) {
            (SearchPredicateOp::Eq, SearchPredicateValue::Bool(expected)) => actual == *expected,
            (SearchPredicateOp::Ne, SearchPredicateValue::Bool(expected)) => actual != *expected,
            _ => true,
        }
    }

    /// Match an unsigned numeric predicate.
    #[must_use]
    const fn match_u64(actual: u64, predicate: &SearchPredicate) -> bool {
        match (&predicate.op, &predicate.value) {
            (SearchPredicateOp::Eq, SearchPredicateValue::U64(expected)) => actual == *expected,
            (SearchPredicateOp::Ne, SearchPredicateValue::U64(expected)) => actual != *expected,
            (SearchPredicateOp::Lt, SearchPredicateValue::U64(expected)) => actual < *expected,
            (SearchPredicateOp::Lte, SearchPredicateValue::U64(expected)) => actual <= *expected,
            (SearchPredicateOp::Gt, SearchPredicateValue::U64(expected)) => actual > *expected,
            (SearchPredicateOp::Gte, SearchPredicateValue::U64(expected)) => actual >= *expected,
            _ => true,
        }
    }

    /// Match a signed numeric predicate.
    #[must_use]
    const fn match_i64(actual: i64, predicate: &SearchPredicate) -> bool {
        match (&predicate.op, &predicate.value) {
            (SearchPredicateOp::Eq, SearchPredicateValue::I64(expected)) => actual == *expected,
            (SearchPredicateOp::Ne, SearchPredicateValue::I64(expected)) => actual != *expected,
            (SearchPredicateOp::Lt, SearchPredicateValue::I64(expected)) => actual < *expected,
            (SearchPredicateOp::Lte, SearchPredicateValue::I64(expected)) => actual <= *expected,
            (SearchPredicateOp::Gt, SearchPredicateValue::I64(expected)) => actual > *expected,
            (SearchPredicateOp::Gte, SearchPredicateValue::I64(expected)) => actual >= *expected,
            _ => true,
        }
    }

    /// Match an attribute-list predicate against raw NTFS flags.
    #[must_use]
    #[allow(clippy::single_call_fn)]
    fn match_attributes(flags: u32, predicate: &SearchPredicate) -> bool {
        let SearchPredicateValue::StringList(values) = &predicate.value else {
            return true;
        };
        match predicate.op {
            SearchPredicateOp::HasAll => values.iter().all(|name| Self::flag_set(flags, name)),
            SearchPredicateOp::HasAny => values.iter().any(|name| Self::flag_set(flags, name)),
            SearchPredicateOp::HasNone => values.iter().all(|name| !Self::flag_set(flags, name)),
            SearchPredicateOp::Eq
            | SearchPredicateOp::Ne
            | SearchPredicateOp::Lt
            | SearchPredicateOp::Lte
            | SearchPredicateOp::Gt
            | SearchPredicateOp::Gte
            | SearchPredicateOp::In
            | SearchPredicateOp::NotIn
            | SearchPredicateOp::Match
            | SearchPredicateOp::NotMatch
            | SearchPredicateOp::Contains
            | SearchPredicateOp::StartsWith
            | SearchPredicateOp::EndsWith => true,
        }
    }

    /// Test whether one named NTFS attribute bit is set in the raw flags.
    #[must_use]
    fn flag_set(flags: u32, name: &str) -> bool {
        flags & uffs_core::search::filters::attr_bit(name) != 0
    }

    /// `FILE_ATTRIBUTE_RECALL_ON_OPEN` raw NTFS bit.
    const FLAG_RECALL_ON_OPEN: u32 = 0x0004_0000;

    /// `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` raw NTFS bit.
    const FLAG_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

    /// C++ parity mask over the raw NTFS attribute flags.
    const PARITY_FLAG_MASK: u32 = 0x001A_EE37;
}
