//! Index management: load drives, hold compact indices, refresh.
//!
//! The [`IndexManager`] is the daemon's core data structure. It holds
//! the compact search indices for all loaded drives and delegates to
//! `uffs_core::search` for query execution.

use std::path::PathBuf;
use std::time::Instant;

use tokio::sync::RwLock;
use uffs_client::protocol::{
    DaemonStatus, DriveInfo, DrivesResponse, SearchParams, SearchResponse, SearchRow,
    StatusResponse,
};
use uffs_core::search::backend::{DisplayRow, FilterMode, MultiDriveBackend, SortColumn};
use uffs_core::search::filters::SearchFilters;

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
}

impl IndexManager {
    /// Create a new empty index manager.
    #[must_use]
    #[expect(clippy::single_call_fn, reason = "constructor — structural separation")]
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        Self {
            backend: RwLock::new(MultiDriveBackend::new()),
            status: RwLock::new(DaemonStatus::Loading {
                drives_loaded: 0,
                drives_total: 0,
            }),
            start_time: Instant::now(),
            data_dir,
        }
    }

    /// Load drives from MFT files in the data directory.
    ///
    /// Each file matching `*_*.uffs`, `*.bin`, `*.raw`, or `*.iocp` is loaded
    /// as a drive. On Windows, live drives are loaded via the MFT reader.
    pub async fn load_from_data_dir(&self, mft_files: &[PathBuf], no_cache: bool) {
        let total = mft_files.len();
        let mut status_guard = self.status.write().await;
        *status_guard = DaemonStatus::Loading {
            drives_loaded: 0,
            drives_total: total,
        };
        drop(status_guard);

        for (idx, mft_path) in mft_files.iter().enumerate() {
            tracing::info!(path = %mft_path.display(), "Loading MFT file");

            let cloned_path = mft_path.clone();
            let result = tokio::task::spawn_blocking(move || {
                uffs_core::compact::load_mft_file(&cloned_path, None, no_cache)
            })
            .await;

            match result {
                Ok(Ok((drive_index, timing))) => {
                    tracing::info!(
                        drive = %drive_index.letter,
                        records = drive_index.records.len(),
                        mft_ms = timing.mft,
                        compact_ms = timing.compact,
                        trigram_ms = timing.trigram,
                        "Drive loaded"
                    );
                    let mut backend = self.backend.write().await;
                    backend.drives.push(drive_index);
                    drop(backend);
                }
                Ok(Err(load_err)) => {
                    tracing::error!(path = %mft_path.display(), error = %load_err, "Failed to load MFT file");
                }
                Err(join_err) => {
                    tracing::error!(path = %mft_path.display(), error = %join_err, "Task panicked loading MFT");
                }
            }

            // Update progress
            let mut progress = self.status.write().await;
            *progress = DaemonStatus::Loading {
                drives_loaded: idx + 1,
                drives_total: total,
            };
            drop(progress);
        }

        // Mark as ready
        let mut final_status = self.status.write().await;
        *final_status = DaemonStatus::Ready;
        drop(final_status);

        let backend = self.backend.read().await;
        tracing::info!(
            drives = backend.drives.len(),
            total_records = backend.total_records(),
            "All drives loaded — daemon ready"
        );
    }

    /// Load live Windows drives.
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

        for (idx, &letter) in drives.iter().enumerate() {
            tracing::info!(drive = %letter, "Loading live drive");

            let result = tokio::task::spawn_blocking(move || {
                uffs_core::compact::load_live_drive(letter, no_cache)
            })
            .await;

            match result {
                Ok(Ok((drive_index, timing))) => {
                    tracing::info!(
                        drive = %letter,
                        records = drive_index.records.len(),
                        mft_ms = timing.mft,
                        compact_ms = timing.compact,
                        trigram_ms = timing.trigram,
                        "Live drive loaded"
                    );
                    let mut backend = self.backend.write().await;
                    backend.drives.push(drive_index);
                }
                Ok(Err(e)) => {
                    tracing::error!(drive = %letter, error = %e, "Failed to load live drive");
                }
                Err(e) => {
                    tracing::error!(drive = %letter, error = %e, "Task panicked");
                }
            }

            let mut status = self.status.write().await;
            *status = DaemonStatus::Loading {
                drives_loaded: idx + 1,
                drives_total: total,
            };
        }

        let mut status = self.status.write().await;
        *status = DaemonStatus::Ready;
    }

    /// Execute a search query.
    pub async fn search(&self, params: &SearchParams) -> SearchResponse {
        let mut backend = self.backend.write().await;

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

        let filters = SearchFilters::default();

        let result = backend.search(
            &params.pattern,
            params.case_sensitive,
            params.whole_word,
            params.limit,
            filter_mode,
            &filters,
        );
        drop(backend);

        let truncated = params
            .limit
            .is_some_and(|cap| result.rows.len() >= cap as usize);

        let duration_ms = u64::try_from(result.duration.as_millis()).unwrap_or(u64::MAX);

        SearchResponse {
            rows: result
                .rows
                .iter()
                .map(Self::display_row_to_search_row)
                .collect(),
            records_scanned: result.records_scanned,
            duration_ms,
            truncated,
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
                    if mft_path.to_string_lossy().len() <= 2 {
                        #[cfg(windows)]
                        {
                            uffs_core::compact::load_live_drive(letter, false)
                        }
                        #[cfg(not(windows))]
                        {
                            Err(anyhow::anyhow!("Cannot refresh live drive on non-Windows"))
                        }
                    } else {
                        uffs_core::compact::load_mft_file(mft_path, Some(letter), false)
                    }
                }
            })
            .await;

            match result {
                Ok(Ok((new_drive, timing))) => {
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
                        mft_ms = timing.mft,
                        compact_ms = timing.compact,
                        trigram_ms = timing.trigram,
                        "Drive refreshed"
                    );
                }
                Ok(Err(refresh_err)) => {
                    tracing::error!(drive = %letter, error = %refresh_err, "Failed to refresh drive");
                }
                Err(join_err) => {
                    tracing::error!(drive = %letter, error = %join_err, "Task panicked during refresh");
                }
            }
        }

        let mut done_status = self.status.write().await;
        *done_status = DaemonStatus::Ready;
        drop(done_status);
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
            name: row.name.clone(),
            size: row.size,
            is_directory: row.is_directory,
            modified: row.modified,
            created: row.created,
            accessed: row.accessed,
            flags: row.flags,
            allocated: row.allocated,
            descendants: row.descendants,
            treesize: row.treesize,
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
