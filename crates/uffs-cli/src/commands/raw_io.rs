//! Raw MFT and data-loading helpers for CLI search commands.

#![expect(
    clippy::single_call_fn,
    reason = "raw MFT/data-loading helpers are orchestrated from the search pipeline"
)]

use anyhow::Result;
use uffs_core::MftQuery;
use uffs_core::extensions::ExtensionFilter;
use uffs_core::pattern::ParsedPattern;

#[cfg(windows)]
#[path = "raw_io_windows.rs"]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{OwnedQueryFilters, load_live_index};

/// Query filter options for the search command.
pub struct QueryFilters<'a> {
    /// Parsed search pattern (glob, regex, or literal).
    pub parsed: &'a ParsedPattern,
    /// Extension filter string (e.g., "pictures,mp4,pdf").
    pub ext_filter: Option<&'a str>,
    /// Only return files (not directories).
    pub files_only: bool,
    /// Only return directories (not files).
    pub dirs_only: bool,
    /// Hide system files (files starting with $).
    pub hide_system: bool,
    /// Minimum file size filter.
    pub min_size: Option<u64>,
    /// Maximum file size filter.
    pub max_size: Option<u64>,
    /// Minimum descendant count filter (directories).
    pub min_descendants: Option<u32>,
    /// Maximum descendant count filter (directories).
    pub max_descendants: Option<u32>,
    /// Maximum number of results to return.
    pub limit: u32,
}

/// Build and execute the MFT query with all filters applied.
#[tracing::instrument(
    level = "debug",
    skip(df, filters),
    fields(
        rows = df.height(),
        columns = df.width(),
        files_only = filters.files_only,
        dirs_only = filters.dirs_only,
        hide_system = filters.hide_system,
        min_size = ?filters.min_size,
        max_size = ?filters.max_size,
        limit = filters.limit,
        has_ext_filter = filters.ext_filter.is_some()
    )
)]
/// Apply query filters to a `DataFrame` (pattern, extension, size, flags,
/// limit).
pub(super) fn execute_query(
    df: uffs_polars::DataFrame,
    filters: &QueryFilters<'_>,
) -> Result<uffs_polars::DataFrame> {
    let mut query = MftQuery::new(df);

    query = query.pattern(filters.parsed)?;

    if let Some(ext_str) = filters.ext_filter {
        let parsed_ext_filter = ExtensionFilter::parse(ext_str)
            .map_err(|err| anyhow::anyhow!("Invalid extension filter: {err}"))?;
        query = query.extension_filter(&parsed_ext_filter);
    }

    if filters.files_only {
        query = query.files_only();
    } else if filters.dirs_only {
        query = query.directories_only();
    }

    if filters.hide_system {
        query = query.hide_system();
    }

    if let Some(min) = filters.min_size {
        query = query.min_size(min);
    }
    if let Some(max) = filters.max_size {
        query = query.max_size(max);
    }

    // Descendant filters are applied by the unified compact pipeline's
    // SearchFilters, not in this DataFrame query path.

    if filters.limit > 0 {
        query = query.limit(filters.limit);
    }
    Ok(query.collect()?)
}
