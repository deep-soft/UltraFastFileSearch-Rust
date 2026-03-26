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
use uffs_core::search::backend::{
    DisplayRow, FilterMode, MultiDriveBackend, SortColumn,
};
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
        {
            let mut status = self.status.write().await;
            *status = DaemonStatus::Loading {
                drives_loaded: 0,
                drives_total: total,
            };
        }

        for (idx, path) in mft_files.iter().enumerate() {
            tracing::info!(path = %path.display(), "Loading MFT file");

            let result = tokio::task::spawn_blocking({
                let path = path.clone();
                move || uffs_core::compact::load_mft_file(&path, None, no_cache)
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
                }
                Ok(Err(e)) => {
                    tracing::error!(path = %path.display(), error = %e, "Failed to load MFT file");
                }
                Err(e) => {
                    tracing::error!(path = %path.display(), error = %e, "Task panicked loading MFT");
                }
            }

            // Update progress
            let mut status = self.status.write().await;
            *status = DaemonStatus::Loading {
                drives_loaded: idx + 1,
                drives_total: total,
            };
        }

        // Mark as ready
        let mut status = self.status.write().await;
        *status = DaemonStatus::Ready;

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

        // Parse sort column
        let sort_column = params
            .sort
            .as_deref()
            .and_then(parse_sort_column)
            .unwrap_or(SortColumn::Modified);
        backend.sort_column = sort_column;
        backend.sort_desc = params.sort_desc;

        // Parse filter mode
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

        let truncated = params
            .limit
            .is_some_and(|limit| result.rows.len() >= limit as usize);

        SearchResponse {
            rows: result.rows.iter().map(display_row_to_search_row).collect(),
            records_scanned: result.records_scanned,
            duration_ms: result.duration.as_millis() as u64,
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
                        uffs_core::compact::IndexSource::MftFile(p) => {
                            if p.to_string_lossy().len() <= 2 {
                                "live".to_owned()
                            } else {
                                format!("file:{}", p.display())
                            }
                        }
                    },
                })
                .collect(),
        }
    }

    /// Get current daemon status.
    pub async fn status(&self, connections: usize) -> StatusResponse {
        let status = self.status.read().await;
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

        {
            let mut status = self.status.write().await;
            *status = DaemonStatus::Refreshing {
                drives: drives_to_refresh.clone(),
            };
        }

        // Refresh each drive sequentially
        for &letter in &drives_to_refresh {
            // Find the drive index and take it for refresh
            let drive_data = {
                let backend = self.backend.read().await;
                backend
                    .drives
                    .iter()
                    .find(|dr| dr.letter == letter)
                    .map(|dr| dr.source.clone())
            };

            let Some(source) = drive_data else {
                tracing::warn!(drive = %letter, "Drive not found for refresh");
                continue;
            };

            let result = tokio::task::spawn_blocking(move || {
                // Create a temporary DriveCompactIndex with just the source info
                // for the refresh function
                match &source {
                    uffs_core::compact::IndexSource::MftFile(path) => {
                        if path.to_string_lossy().len() <= 2 {
                            #[cfg(windows)]
                            {
                                uffs_core::compact::load_live_drive(letter, false)
                            }
                            #[cfg(not(windows))]
                            {
                                Err(anyhow::anyhow!("Cannot refresh live drive on non-Windows"))
                            }
                        } else {
                            uffs_core::compact::load_mft_file(path, Some(letter), false)
                        }
                    }
                }
            })
            .await;

            match result {
                Ok(Ok((new_drive, timing))) => {
                    let mut backend = self.backend.write().await;
                    // Replace the old drive with the new one
                    if let Some(pos) = backend.drives.iter().position(|dr| dr.letter == letter) {
                        backend.drives[pos] = new_drive;
                    } else {
                        backend.drives.push(new_drive);
                    }
                    tracing::info!(
                        drive = %letter,
                        mft_ms = timing.mft,
                        compact_ms = timing.compact,
                        trigram_ms = timing.trigram,
                        "Drive refreshed"
                    );
                }
                Ok(Err(e)) => {
                    tracing::error!(drive = %letter, error = %e, "Failed to refresh drive");
                }
                Err(e) => {
                    tracing::error!(drive = %letter, error = %e, "Task panicked during refresh");
                }
            }
        }

        let mut status = self.status.write().await;
        *status = DaemonStatus::Ready;
    }

    /// Look up a file by path and return all available fields (D2.3.7).
    pub async fn info(&self, path: &str) -> uffs_client::protocol::InfoResponse {
        let backend = self.backend.read().await;
        let path_lower = path.to_ascii_lowercase();

        // Search all drives for a matching path
        for drive in &backend.drives {
            let volume_prefix = format!("{}:\\", drive.letter);
            for (idx, rec) in drive.records.iter().enumerate() {
                if rec.name_len == 0 {
                    continue;
                }
                let resolved = uffs_core::search::tree::resolve_path(drive, idx, &volume_prefix);
                if resolved.to_ascii_lowercase() == path_lower {
                    let name = rec.name(&drive.names);
                    let record_json = serde_json::json!({
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
                    });
                    return uffs_client::protocol::InfoResponse {
                        found: true,
                        record: Some(record_json),
                    };
                }
            }
        }

        uffs_client::protocol::InfoResponse {
            found: false,
            record: None,
        }
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
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Convert a `DisplayRow` to a protocol `SearchRow`.
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

