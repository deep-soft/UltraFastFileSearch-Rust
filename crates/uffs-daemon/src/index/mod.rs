// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Index management: load drives, hold compact indices, refresh.
//!
//! The [`IndexManager`] is the daemon's core data structure. It holds
//! the compact search indices for all loaded drives and delegates to
//! `uffs_core::search` for query execution.
//! Exception: `file_size_policy` — single IndexManager impl, splitting hurts
//! readability.

mod aggregation;
mod predicates;
mod projection;
mod search;
#[cfg(test)]
mod tests;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};
use std::path::PathBuf;
use std::time::Instant;

use tokio::sync::{RwLock, Semaphore};
use uffs_client::protocol::response::{DaemonStatus, StatsResponse, StatusResponse};
use uffs_core::search::backend::DriveIndex;

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
/// Concurrent queries clone the inner `Arc<DriveIndex>` under a read lock
/// (< 1 μs), then search the snapshot with no lock held.  Mutations
/// (load, refresh, remove) swap the `Arc` pointer under a write lock
/// (< 1 μs).  In-flight queries keep the old snapshot alive until they
/// finish.
pub(crate) struct IndexManager {
    /// Shared index snapshot: read lock to clone Arc (< 1 μs), write lock
    /// only during load/refresh/remove (pointer swap, < 1 μs).
    index: RwLock<Arc<DriveIndex>>,
    /// Current daemon status.
    status: RwLock<DaemonStatus>,
    /// When the daemon started.
    start_time: Instant,
    /// Data directory for MFT files (Mac/Linux offline mode).
    data_dir: Option<PathBuf>,
    /// Event broadcaster — pushes notifications to all connected clients.
    events: EventSender,
    // ── Concurrency control ────────────────────────────────────────
    /// Limits simultaneous search operations to prevent CPU/memory
    /// exhaustion.  Permits = available parallelism (CPU cores).
    search_semaphore: Semaphore,
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
    pub(crate) fn new(data_dir: Option<PathBuf>, events: EventSender) -> Self {
        let cpus = std::thread::available_parallelism().map_or(4, core::num::NonZeroUsize::get);
        Self {
            index: RwLock::new(Arc::new(DriveIndex::new())),
            status: RwLock::new(DaemonStatus::Loading {
                drives_loaded: 0,
                drives_total: 0,
            }),
            start_time: Instant::now(),
            data_dir,
            events,
            search_semaphore: Semaphore::new(cpus),
            queries_total: AtomicU64::new(0),
            queries_total_us: AtomicU64::new(0),
            startup_duration_us: AtomicU64::new(0),
            drive_timings: RwLock::new(std::collections::HashMap::new()),
        }
    }

    /// Get a reference to the event sender (for IPC and lifecycle integration).
    pub(crate) const fn event_sender(&self) -> &EventSender {
        &self.events
    }

    /// Load drives from MFT files — **all files in parallel**.
    ///
    /// Each MFT file is loaded on its own blocking thread via `JoinSet`.
    /// Results are collected as they complete (fastest first).
    #[expect(
        clippy::cognitive_complexity,
        reason = "index loading with cache/live/fallback branches"
    )]
    pub(crate) async fn load_from_data_dir(&self, mft_files: &[PathBuf], no_cache: bool) {
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
                    self.add_drive(drive_index).await;
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

            // Return freed pages to the OS after each drive load.
            release_allocator_pages();

            let mut progress = self.status.write().await;
            *progress = DaemonStatus::Loading {
                drives_loaded: loaded,
                drives_total: total,
            };
            drop(progress);
        }

        // Mark as ready + record startup duration.
        self.set_ready().await;

        let snap = self.snapshot().await;
        let drive_count = snap.drives.len();
        let total_records = snap.total_records();
        drop(snap);
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
    #[expect(
        clippy::print_stderr,
        reason = "[diag] diagnostic tracing — remove after D: drive issue is resolved"
    )]
    #[expect(
        clippy::use_debug,
        reason = "[diag] diagnostic tracing — remove after D: drive issue is resolved"
    )]
    pub async fn load_live_drives(
        &self,
        drives: &[char],
        no_cache: bool,
        lifecycle: &crate::lifecycle::LifecycleHandle,
    ) {
        let total = drives.len();
        eprintln!("[diag] load_live_drives: starting — drives={drives:?}  no_cache={no_cache}");
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
            eprintln!("[diag] load_live_drives: spawning thread for drive={letter}");
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
                    eprintln!(
                        "[diag] load_live_drives: TIMEOUT — {remaining} drive(s) stuck after {}s",
                        Self::DRIVE_LOAD_TIMEOUT.as_secs()
                    );
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
                    self.add_drive(drive_index).await;
                }
                Ok((letter, Err(err))) => {
                    loaded += 1;
                    // [diag] This is the key line — print the FULL error for D:.
                    eprintln!("[diag] load_live_drives: FAILED drive={letter}  error={err:#}");
                    tracing::error!(drive = %letter, error = %err, "Failed to load live drive");
                }
                Err(err) => {
                    loaded += 1;
                    eprintln!("[diag] load_live_drives: PANIC in task  error={err}");
                    tracing::error!(error = %err, "Task panicked loading drive");
                }
            }

            // Return freed pages to the OS after each drive load.
            // The MftIndex (~3 GB for large drives) was dropped inside
            // load_drive(), but the system allocator retains those pages
            // as committed virtual memory.  This reclaims them.
            release_allocator_pages();

            // Update load heartbeat — tells the idle timer we're still
            // making progress, preventing a false stall-timeout.
            lifecycle.record_load_progress();

            let mut status = self.status.write().await;
            *status = DaemonStatus::Loading {
                drives_loaded: loaded,
                drives_total: total,
            };
        }

        // Final allocator purge after all drives are loaded.
        release_allocator_pages();

        self.set_ready().await;

        let snap = self.snapshot().await;
        let drive_count = snap.drives.len();
        let total_records = snap.total_records();

        // Log per-drive heap breakdown.
        let mut total_heap: u64 = 0;
        for dr in &snap.drives {
            dr.log_heap_report();
            total_heap += dr.heap_size_bytes().total as u64;
        }
        let heap_mb = total_heap / (1024 * 1024);
        tracing::info!(
            drives = drive_count,
            total_records,
            index_heap_mb = heap_mb,
            "[MEM] All drives loaded: index heap = {} MB",
            heap_mb,
        );
        drop(snap);

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

    // ── Atomic drive mutations ─────────────────────────────────────

    /// Add a drive to the shared index via atomic pointer swap.
    ///
    /// Clones the `Vec` of `Arc` pointers (< 100 bytes), appends the new
    /// drive, and swaps.  In-flight queries keep the old snapshot.
    async fn add_drive(&self, drive: uffs_core::compact::DriveCompactIndex) {
        let mut guard = self.index.write().await;
        let mut drives = guard.drives.clone();
        drives.push(Arc::new(drive));
        *guard = Arc::new(DriveIndex { drives });
    }

    /// Replace a drive by letter (for refresh) via atomic pointer swap.
    ///
    /// Builds a new snapshot with the old drive removed and the new one
    /// appended.  Write lock held for < 1 μs (pointer swap only).
    async fn replace_drive(&self, letter: char, new_drive: uffs_core::compact::DriveCompactIndex) {
        let mut guard = self.index.write().await;
        let mut drives: Vec<Arc<uffs_core::compact::DriveCompactIndex>> = guard
            .drives
            .iter()
            .filter(|drv| !drv.letter.eq_ignore_ascii_case(&letter))
            .cloned()
            .collect();
        drives.push(Arc::new(new_drive));
        *guard = Arc::new(DriveIndex { drives });
    }

    /// Snapshot the current index (< 1 μs).  Callers search the returned
    /// `Arc` with no lock held.
    async fn snapshot(&self) -> Arc<DriveIndex> {
        let guard = self.index.read().await;
        Arc::clone(&guard)
    }

    /// Get daemon performance statistics.
    #[expect(
        clippy::float_arithmetic,
        clippy::default_numeric_fallback,
        reason = "stats are approximate; f64 arithmetic needed for averages"
    )]
    pub(crate) async fn stats(&self) -> StatsResponse {
        let total_queries = self.queries_total.load(Ordering::Relaxed);
        let total_us = self.queries_total_us.load(Ordering::Relaxed);
        let startup_us = self.startup_duration_us.load(Ordering::Relaxed);
        let uptime_secs = self.start_time.elapsed().as_secs();
        let total_records = self.total_records().await;

        let avg_query_us = if total_queries > 0 {
            uffs_mft::u64_to_f64(total_us) / uffs_mft::u64_to_f64(total_queries)
        } else {
            0.0
        };
        let qps = if uptime_secs > 0 {
            uffs_mft::u64_to_f64(total_queries) / uffs_mft::u64_to_f64(uptime_secs)
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
    pub(crate) async fn status(&self, connections: usize) -> StatusResponse {
        let status = self.status.read().await;
        let loaded = self.has_drives().await;
        let records = self.total_records().await;
        tracing::trace!(
            has_drives = loaded,
            total_records = records,
            "Status queried"
        );

        // Collect per-drive memory breakdown.
        let snap = self.snapshot().await;
        let mut drive_memory = Vec::with_capacity(snap.drives.len());
        let mut total_index_heap: u64 = 0;
        for dr in &snap.drives {
            let hr = dr.heap_size_bytes();
            let heap = hr.total as u64;
            total_index_heap += heap;
            drive_memory.push(uffs_client::protocol::response::DriveMemoryInfo {
                drive: dr.letter,
                records: dr.records.len(),
                heap_bytes: heap,
                records_bytes: hr.records as u64,
                names_bytes: hr.names as u64,
                trigram_bytes: hr.trigram as u64,
                children_bytes: hr.children as u64,
                ext_index_bytes: hr.ext_index as u64,
            });
        }
        drop(snap);

        StatusResponse {
            status: status.clone(),
            uptime_secs: self.start_time.elapsed().as_secs(),
            connections,
            pid: std::process::id(),
            rss_bytes: None,
            index_heap_bytes: Some(total_index_heap),
            drive_memory,
        }
    }

    /// Refresh specific drives (or all if empty).
    #[expect(
        clippy::cognitive_complexity,
        reason = "index refresh with multi-stage validation"
    )]
    pub(crate) async fn refresh(&self, drives: &[char]) {
        let drives_to_refresh: Vec<char> = if drives.is_empty() {
            let snap = self.snapshot().await;
            snap.drives.iter().map(|dr| dr.letter).collect()
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
            let snap = self.snapshot().await;
            let drive_source = snap
                .drives
                .iter()
                .find(|dr| dr.letter == letter)
                .map(|dr| dr.source.clone());
            drop(snap);

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
                    self.replace_drive(letter, new_drive).await;
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

            // Reclaim pages freed by the old drive index and MftIndex temporaries.
            release_allocator_pages();
        }

        self.set_ready().await;
        self.events.emit(DaemonEvent::RefreshComplete {
            drives_refreshed: drives_to_refresh.len(),
        });
    }

    /// Look up a file by path and return all available fields (D2.3.7).
    ///
    /// Walks the `children` index top-down in `O(path_depth)` instead of
    /// scanning all records with full path resolution.
    pub(crate) async fn info(
        &self,
        file_path: &str,
    ) -> uffs_client::protocol::response::InfoResponse {
        let snap = self.snapshot().await;

        let found_record = Self::info_tree_lookup(&snap, file_path);

        drop(snap);

        uffs_client::protocol::response::InfoResponse {
            found: found_record.is_some(),
            record: found_record,
        }
    }

    /// Fast tree-walk lookup: parse path → drive letter + segments, then
    /// walk `children` index matching each segment case-insensitively.
    fn info_tree_lookup(snap: &DriveIndex, file_path: &str) -> Option<serde_json::Value> {
        // Parse "C:\Windows\System32\notepad.exe" → ('C', ["Windows", "System32",
        // "notepad.exe"])
        let normalized = file_path.replace('/', "\\");
        let (drive_letter, remainder) = Self::parse_drive_prefix(&normalized)?;

        let segments: Vec<&str> = remainder
            .split('\\')
            .filter(|seg| !seg.is_empty())
            .collect();
        if segments.is_empty() {
            return None;
        }

        // Find the matching drive.
        let drive = snap
            .drives
            .iter()
            .find(|dr| dr.letter.eq_ignore_ascii_case(&drive_letter))?;

        // Find root entries (parent_idx == u32::MAX) as starting candidates.
        let mut candidates: Vec<u32> = Vec::new();
        for (idx, rec) in drive.records.iter().enumerate() {
            if rec.parent_idx == u32::MAX && rec.name_len > 0 {
                candidates.push(uffs_mft::len_to_u32(idx));
            }
        }

        // Walk segments top-down through the children index.
        for (seg_idx, &segment) in segments.iter().enumerate() {
            let seg_lower = segment.to_ascii_lowercase();
            let is_last = seg_idx == segments.len() - 1;

            let mut next_candidates: Vec<u32> = Vec::new();

            if seg_idx == 0 {
                // First segment: match against root entries.
                for &root_idx in &candidates {
                    if let Some(rec) = drive.records.get(uffs_mft::u32_as_usize(root_idx)) {
                        let name = rec.name(&drive.names);
                        if name.to_ascii_lowercase() == seg_lower {
                            if is_last {
                                let volume_prefix = format!("{}:\\", drive.letter);
                                let resolved = uffs_core::search::tree::resolve_path(
                                    drive,
                                    uffs_mft::u32_as_usize(root_idx),
                                    &volume_prefix,
                                );
                                return Some(Self::build_info_json(drive, rec, &resolved));
                            }
                            // Collect children for next segment.
                            next_candidates.extend_from_slice(
                                drive.children.get(uffs_mft::u32_as_usize(root_idx)),
                            );
                        }
                    }
                }
            } else {
                // Subsequent segments: match against children of previous matches.
                for &child_idx in &candidates {
                    if let Some(rec) = drive.records.get(uffs_mft::u32_as_usize(child_idx)) {
                        let name = rec.name(&drive.names);
                        if name.to_ascii_lowercase() == seg_lower {
                            if is_last {
                                let volume_prefix = format!("{}:\\", drive.letter);
                                let resolved = uffs_core::search::tree::resolve_path(
                                    drive,
                                    uffs_mft::u32_as_usize(child_idx),
                                    &volume_prefix,
                                );
                                return Some(Self::build_info_json(drive, rec, &resolved));
                            }
                            next_candidates.extend_from_slice(
                                drive.children.get(uffs_mft::u32_as_usize(child_idx)),
                            );
                        }
                    }
                }
            }

            if next_candidates.is_empty() {
                return None;
            }
            candidates = next_candidates;
        }

        None
    }

    /// Parse `C:\...` or `c:/...` into `(drive_letter, remainder)`.
    fn parse_drive_prefix(path: &str) -> Option<(char, &str)> {
        let mut chars = path.chars();
        let letter = chars.next()?;
        if !letter.is_ascii_alphabetic() {
            return None;
        }
        if chars.next()? != ':' {
            return None;
        }
        // Skip optional separator after ':'
        let after_colon = path.get(2..)?;
        let remainder = after_colon
            .strip_prefix('\\')
            .or_else(|| after_colon.strip_prefix('/'))
            .unwrap_or(after_colon);
        Some((letter, remainder))
    }

    /// Build the JSON value for an info response record.
    fn build_info_json(
        drive: &uffs_core::compact::DriveCompactIndex,
        rec: &uffs_core::compact::CompactRecord,
        resolved_path: &str,
    ) -> serde_json::Value {
        let name = rec.name(&drive.names);
        serde_json::json!({
            "drive": drive.letter.to_string(),
            "path": resolved_path,
            "name": name,
            "size": rec.size,
            "allocated": rec.allocated,
            "treesize": rec.treesize,
            "tree_allocated": rec.tree_allocated,
            "created": rec.created,
            "modified": rec.modified,
            "accessed": rec.accessed,
            "flags": rec.flags,
            "is_directory": rec.is_directory(),
            "descendants": rec.descendants,
            "parent_idx": rec.parent_idx,
            "extension_id": rec.extension_id,
        })
    }

    /// Get the configured data directory, if any.
    #[must_use]
    pub(crate) fn data_dir(&self) -> Option<&std::path::Path> {
        self.data_dir.as_deref()
    }

    /// Check if the daemon has any loaded drives.
    pub(crate) async fn has_drives(&self) -> bool {
        let guard = self.index.read().await;
        !guard.drives.is_empty()
    }

    /// Total records across all drives.
    pub(crate) async fn total_records(&self) -> usize {
        let guard = self.index.read().await;
        guard.total_records()
    }

    /// Return the set of currently loaded drive letters.
    pub(crate) async fn loaded_drive_letters(&self) -> Vec<char> {
        let snap = self.snapshot().await;
        snap.drives.iter().map(|dr| dr.letter).collect()
    }

    /// Hot-load a single MFT file if its drive letter is not already loaded.
    ///
    /// Returns `Ok(Some(letter))` if loaded, `Ok(None)` if already present.
    #[expect(
        clippy::cognitive_complexity,
        reason = "multi-drive search with merge and sort"
    )]
    pub(crate) async fn load_single_mft_file(
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
            let snap = self.snapshot().await;
            if snap.drives.iter().any(|dr| dr.letter == letter) {
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

        // Reclaim pages freed by MftIndex temporaries during load.
        release_allocator_pages();

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
                self.add_drive(drive_index).await;
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

    /// Hot-load a single drive letter into the running daemon.
    ///
    /// On **Windows**, reads the live NTFS MFT directly.
    /// On **non-Windows**, looks in `data_dir` for an offline MFT file.
    ///
    /// If the drive is already loaded, replaces it (re-read).
    ///
    /// Returns `Ok(records)` on success.
    pub(crate) async fn hot_load_drive(
        &self,
        drive_letter: char,
        no_cache: bool,
    ) -> anyhow::Result<usize> {
        let letter = drive_letter.to_ascii_uppercase();

        if self.is_drive_loaded(letter).await {
            tracing::info!(drive = %letter, "Drive already loaded — will hot-swap after re-read");
        }

        let source = self.resolve_drive_source(letter)?;
        tracing::info!(drive = %letter, "Hot-loading drive");

        let (drive_index, timing) = self.blocking_load_drive(source, no_cache).await?;
        let records = drive_index.records.len();

        self.emit_drive_loaded(letter, records, &timing);
        self.store_drive_timing(letter, &timing).await;
        // Atomic swap: old drive (if any) is replaced in a single pointer
        // swap — in-flight queries on the old Arc finish undisturbed, new
        // queries see the fresh data immediately.
        self.replace_drive(letter, drive_index).await;

        Ok(records)
    }

    /// Check whether a drive letter is already in the index.
    async fn is_drive_loaded(&self, letter: char) -> bool {
        let snap = self.snapshot().await;
        snap.drives.iter().any(|dr| dr.letter == letter)
    }

    /// Determine the [`MftSource`] for a drive letter.
    fn resolve_drive_source(&self, letter: char) -> anyhow::Result<uffs_core::compact::MftSource> {
        #[cfg(windows)]
        {
            Ok(uffs_core::compact::MftSource::Live(letter))
        }
        #[cfg(not(windows))]
        {
            let data_dir = self.data_dir.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "No data_dir configured — cannot load drive {letter}: on non-Windows"
                )
            })?;
            let drive_subdir = data_dir.join(format!("drive_{}", letter.to_ascii_lowercase()));
            let mft_path =
                uffs_mft::discovery::find_best_mft_file(&drive_subdir).ok_or_else(|| {
                    anyhow::anyhow!("No MFT file found in {}", drive_subdir.display())
                })?;
            Ok(uffs_core::compact::MftSource::File(mft_path, Some(letter)))
        }
    }

    /// Run `load_drive` on a blocking thread and release allocator pages.
    async fn blocking_load_drive(
        &self,
        source: uffs_core::compact::MftSource,
        no_cache: bool,
    ) -> anyhow::Result<(
        uffs_core::compact::DriveCompactIndex,
        uffs_core::compact::LoadTiming,
    )> {
        let result =
            tokio::task::spawn_blocking(move || uffs_core::compact::load_drive(&source, no_cache))
                .await;

        release_allocator_pages();

        match result {
            Ok(Ok(pair)) => Ok(pair),
            Ok(Err(load_err)) => Err(load_err),
            Err(join_err) => anyhow::bail!("Load task panicked: {join_err}"),
        }
    }

    /// Emit a `DriveLoaded` event for a single hot-loaded drive.
    fn emit_drive_loaded(
        &self,
        letter: char,
        records: usize,
        timing: &uffs_core::compact::LoadTiming,
    ) {
        tracing::info!(
            drive = %letter, records,
            mft_ms = timing.mft, compact_ms = timing.compact, trigram_ms = timing.trigram,
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
    }

    /// Persist per-drive load timing for `--profile` reporting.
    async fn store_drive_timing(&self, letter: char, timing: &uffs_core::compact::LoadTiming) {
        self.drive_timings
            .write()
            .await
            .insert(letter, StoredDriveTiming {
                cache: timing.cache,
                mft: timing.mft,
                compact: timing.compact,
                trigram: timing.trigram,
            });
    }

    /// Discover and load a missing drive from the data directory.
    ///
    /// Returns `Ok(true)` if the drive was discovered and loaded,
    /// `Ok(false)` if no MFT file was found for it, or an error.
    pub(crate) async fn discover_and_load_drive(
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
    #[expect(
        clippy::cognitive_complexity,
        reason = "tree metrics computation with parent chain traversal"
    )]
    pub(crate) async fn ensure_drives_loaded(&self, drives: &[char], no_cache: bool) -> Vec<char> {
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
}

// ── Allocator page release ──────────────────────────────────────────────

/// Ask the system allocator to return freed pages to the OS.
///
/// After a large allocation+free cycle (e.g. `MftIndex` → drop), the
/// system allocator retains committed virtual memory.  This function
/// issues a best-effort request to reclaim those pages so the process
/// RSS reflects actual usage.
///
/// Uses `mi_collect(true)` (mimalloc) which aggressively decommits freed
/// pages.  This replaces the previous `HeapCompact` / `malloc_trim` calls
/// which only work with the system allocator — since we now use mimalloc as
/// `#[global_allocator]`, those had no effect.
fn release_allocator_pages() {
    mi_collect_force();
    tracing::debug!("mi_collect(true) completed");
}

/// Call `mi_collect(true)` to aggressively decommit freed mimalloc segments.
#[expect(unsafe_code, reason = "FFI call to mimalloc's mi_collect")]
fn mi_collect_force() {
    // SAFETY: `mi_collect(true)` only decommits unreferenced mimalloc
    // segments.  No allocated data is affected.
    unsafe {
        libmimalloc_sys::mi_collect(true);
    }
}
