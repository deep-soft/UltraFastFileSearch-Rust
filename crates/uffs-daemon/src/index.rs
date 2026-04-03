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
    DaemonStatus, DriveInfo, DriveProfile, DrivesResponse, SearchParams, SearchProfile,
    SearchResponse, SearchRow, StatsResponse, StatusResponse,
};
use uffs_core::search::backend::{DisplayRow, FilterMode, MultiDriveBackend, SortColumn};
use uffs_core::search::filters::SearchFilters;

use crate::events::{DaemonEvent, EventSender};

/// Per-drive load timing stored for profile reporting.
///
/// Field names omit the `_ms` suffix because the unit is documented
/// once here; all values are milliseconds (`u128`).
struct StoredDriveTiming {
    /// MFT read time (milliseconds).
    mft: u128,
    /// Compact index build time (milliseconds).
    compact: u128,
    /// Trigram index build time (milliseconds).
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
                    self.drive_timings.write().await.insert(
                        letter,
                        StoredDriveTiming {
                            mft: timing.mft,
                            compact: timing.compact,
                            trigram: timing.trigram,
                        },
                    );
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

    /// Load live Windows drives — **all drives in parallel**.
    ///
    /// Each drive's MFT read runs on its own blocking thread. Results are
    /// collected via `JoinSet` as they complete (fastest drive first), giving
    /// accurate incremental progress and cutting total wall time from
    /// `sum(per-drive)` to `max(per-drive)`.
    #[cfg(windows)]
    pub async fn load_live_drives(&self, drives: &[char], no_cache: bool) {
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
        let mut loaded: usize = 0;
        while let Some(join_result) = join_set.join_next().await {
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
                    self.drive_timings.write().await.insert(
                        letter,
                        StoredDriveTiming {
                            mft: timing.mft,
                            compact: timing.compact,
                            trigram: timing.trigram,
                        },
                    );
                    let mut backend = self.backend.write().await;
                    backend.drives.push(drive_index);
                }
                Ok((letter, Err(e))) => {
                    loaded += 1;
                    tracing::error!(drive = %letter, error = %e, "Failed to load live drive");
                }
                Err(e) => {
                    loaded += 1;
                    tracing::error!(error = %e, "Task panicked loading drive");
                }
            }

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
    pub async fn search(&self, params: &SearchParams) -> SearchResponse {
        let query_start = Instant::now();
        let profiling = params.profile;

        // ── Lock acquisition ────────────────────────────────────────
        let t_lock = profiling.then(Instant::now);
        let mut backend = self.backend.write().await;
        let lock_us = t_lock.map_or(0, |ts| ts.elapsed().as_micros());

        let sort_column = params
            .sort
            .as_deref()
            .and_then(Self::parse_sort_column)
            .unwrap_or(SortColumn::Modified);
        backend.sort_column = sort_column;
        backend.sort_desc = params.sort_desc;

        let filter_mode = match params.filter.as_deref() {
            Some("files") => FilterMode::FilesOnly,
            Some("dirs") => FilterMode::DirsOnly,
            _ => FilterMode::All,
        };

        let filters = SearchFilters::from_params(
            params.hide_system,
            params.min_size,
            params.max_size,
            params.min_descendants,
            params.max_descendants,
            params.newer.as_deref(),
            params.older.as_deref(),
            params.newer_created.as_deref(),
            params.older_created.as_deref(),
            params.newer_accessed.as_deref(),
            params.older_accessed.as_deref(),
            params.attr.as_deref(),
            params.ext.as_deref(),
            params.exclude.as_deref(),
        );

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
            &params.pattern,
            params.case_sensitive,
            params.whole_word,
            params.limit,
            filter_mode,
            &filters,
            &params.drives,
        );
        let search_us = if profiling {
            result.duration.as_micros()
        } else {
            0
        };

        drop(backend);

        // ── Row building ────────────────────────────────────────────
        let t_rows = profiling.then(Instant::now);
        let rows: Vec<SearchRow> = result
            .rows
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

        let truncated = params.limit.is_some_and(|cap| rows.len() >= cap as usize);
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

        SearchResponse {
            rows,
            records_scanned: result.records_scanned,
            duration_ms,
            truncated,
            shmem_path: None,
            shmem_count: None,
            profile,
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
                let (mft_ms, compact_ms, trigram_ms) =
                    timings.get(&drive).map_or((0, 0, 0), |ts| {
                        (ms_clamp(ts.mft), ms_clamp(ts.compact), ms_clamp(ts.trigram))
                    });
                DriveProfile {
                    drive,
                    records,
                    matches,
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

    /// Parse a sort column name string.
    #[expect(
        clippy::single_call_fn,
        reason = "parsing helper — clarity over inlining"
    )]
    fn parse_sort_column(name: &str) -> Option<SortColumn> {
        match name.to_ascii_lowercase().as_str() {
            "name" => Some(SortColumn::Name),
            "size" => Some(SortColumn::Size),
            "sizeondisk" | "allocated" => Some(SortColumn::SizeOnDisk),
            "created" => Some(SortColumn::Created),
            "modified" | "date" | "written" => Some(SortColumn::Modified),
            "accessed" => Some(SortColumn::Accessed),
            "path" => Some(SortColumn::Path),
            "drive" => Some(SortColumn::Drive),
            "ext" | "extension" => Some(SortColumn::Extension),
            "type" => Some(SortColumn::Type),
            "descendants" => Some(SortColumn::Descendants),
            _ => None,
        }
    }
}
