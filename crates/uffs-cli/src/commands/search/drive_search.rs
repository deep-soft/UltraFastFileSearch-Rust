//! Per-drive search helpers shared by multi-drive command flows.
//!
//! Search uses the **native `CompactIndex`** path by default:
//! `MftIndex → CompactIndex → trigram/regex search → Vec<DisplayRow>`.
//!
//! Only the small result set (~500 rows) is converted to `DataFrame` for
//! output formatting. The full MFT `DataFrame` (7M rows) is **never** created.

use std::sync::Arc;

use anyhow::{Context, Result};
use indicatif::ProgressBar;
use uffs_core::search::backend::DisplayRow;

use crate::commands::raw_io::OwnedQueryFilters;

/// Result from a single drive read operation.
pub(super) struct DriveResult {
    /// Drive letter that was read.
    pub(super) drive: char,
    /// Matching rows from native compact index search.
    pub(super) rows: Vec<DisplayRow>,
    /// Total records read from the MFT.
    pub(super) records_read: usize,
    /// Number of records matching the filters.
    pub(super) matches: usize,
    /// Error message if the drive read failed.
    pub(super) error: Option<String>,
    /// Whether paths were resolved (for logging).
    pub(super) paths_resolved: bool,
}

/// Load, filter, and decorate results for a single drive using **native
/// compact index search** (no full-MFT `DataFrame` created).
pub(super) async fn search_single_drive(
    drive_char: char,
    filters: Arc<OwnedQueryFilters>,
    needs_paths: bool,
    no_bitmap: bool,
    progress: Option<ProgressBar>,
) -> DriveResult {
    _ = no_bitmap;
    _ = needs_paths; // paths are always resolved by CompactIndex search

    let result =
        tokio::task::spawn_blocking(move || search_native_compact(drive_char, &filters)).await;

    if let Some(pb) = progress.as_ref() {
        pb.finish();
    }

    match result {
        Ok(Ok(dr)) => dr,
        Ok(Err(error)) => drive_error(drive_char, 0, 0, error.to_string(), false),
        Err(error) => drive_error(drive_char, 0, 0, error.to_string(), false),
    }
}

/// Native compact search: load `MftIndex` → build `CompactIndex` → search →
/// convert small result set to `DataFrame`.
#[cfg(windows)]
fn search_native_compact(drive: char, filters: &OwnedQueryFilters) -> anyhow::Result<DriveResult> {
    use uffs_mft::cache::load_cached_index;

    // 1. Load MftIndex (cached or fresh)
    let index =
        if let Some((cached, _header)) = load_cached_index(drive, uffs_mft::INDEX_TTL_SECONDS) {
            tracing::info!(drive = %drive, records = cached.records.len(), "📦 MftIndex cache hit");
            cached
        } else {
            tracing::info!(drive = %drive, "📖 MftIndex cache miss — reading MFT");
            let reader = uffs_mft::MftReader::open(drive)?;
            let fresh = reader.read_all_index_sync()?;
            let vol_serial = uffs_mft::VolumeHandle::open(drive)
                .map(|handle| handle.volume_data().volume_serial_number)
                .unwrap_or(0);
            let (usn_jid, usn_next) = uffs_mft::usn::query_usn_journal(drive)
                .map_or((0, 0), |info| (info.journal_id, info.next_usn));
            if let Err(err) =
                uffs_mft::cache::save_to_cache(&fresh, drive, vol_serial, usn_jid, usn_next)
            {
                tracing::warn!(drive = %drive, error = %err, "Failed to save .uffs cache");
            }
            fresh
        };

    // 2. Ensure compact cache is built + saved
    let compact = uffs_core::compact_cache::ensure_compact_cached(drive, &index);
    let records_read = compact.records.len();

    // 3. Search on compact index (native — no full DataFrame)
    let (rows, _search_filters, _filter_mode) = filters.search_compact(compact)?;
    let matches = rows.len();

    Ok(DriveResult {
        drive,
        rows,
        records_read,
        matches,
        error: None,
        paths_resolved: true, // compact index always resolves paths
    })
}

/// Stub for non-Windows — the live drive path is Windows-only.
#[cfg(not(windows))]
fn search_native_compact(
    _drive: char,
    _filters: &OwnedQueryFilters,
) -> anyhow::Result<DriveResult> {
    anyhow::bail!("live drive search requires Windows")
}

/// Reorder a `DataFrame` so the `drive` column appears first.
pub(super) fn reorder_drive_column(df: &uffs_polars::DataFrame) -> Result<uffs_polars::DataFrame> {
    use uffs_core::{IntoLazy, col};

    let column_names: Vec<String> = df
        .get_column_names()
        .into_iter()
        .filter(|name| name.as_str() != "drive")
        .map(|name| name.to_string())
        .collect();
    let columns: Vec<_> = std::iter::once("drive".to_string())
        .chain(column_names)
        .map(|name| col(&name))
        .collect();

    df.clone()
        .lazy()
        .select(columns)
        .collect()
        .context("Failed to reorder columns")
}

/// Build a failed per-drive result.
fn drive_error(
    drive: char,
    records_read: usize,
    matches: usize,
    error: String,
    paths_resolved: bool,
) -> DriveResult {
    DriveResult {
        drive,
        rows: Vec::new(),
        records_read,
        matches,
        error: Some(error),
        paths_resolved,
    }
}
