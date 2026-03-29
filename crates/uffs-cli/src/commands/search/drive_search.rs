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

/// Native compact search: compact-first, MftIndex only on cache miss.
///
/// Uses [`uffs_core::compact::load_live_drive`] which:
/// 1. Tries compact cache first (no MftIndex loaded)
/// 2. On miss → loads MftIndex (with USN update) → builds compact → saves
///    compact cache → drops MftIndex
///
/// The MftIndex is never held after the compact index is built.
#[cfg(windows)]
fn search_native_compact(drive: char, filters: &OwnedQueryFilters) -> anyhow::Result<DriveResult> {
    let source = uffs_core::compact::MftSource::Live(drive);
    let (compact, _timing) = uffs_core::compact::load_drive(&source, false)?;
    let records_read = compact.records.len();

    let (rows, _search_filters, _filter_mode) = filters.search_compact(compact)?;
    let matches = rows.len();

    Ok(DriveResult {
        drive,
        rows,
        records_read,
        matches,
        error: None,
        paths_resolved: true,
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
