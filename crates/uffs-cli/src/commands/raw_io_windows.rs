//! Windows-specific multi-drive parallel MFT search helpers.

#![cfg(windows)]

use anyhow::{Context, Result};
use tracing::info;
use uffs_core::extensions::ExtensionFilter;
use uffs_mft::{INDEX_TTL_SECONDS, MftReader};

use crate::commands::raw_io::QueryFilters;

/// Owned version of `QueryFilters` for parallel tasks.
///
/// This struct owns all its data so it can be sent across thread boundaries.
#[derive(Clone)]
pub(crate) struct OwnedQueryFilters {
    /// Parsed search pattern (glob, regex, or literal).
    parsed: uffs_core::pattern::ParsedPattern,
    /// Extension filter string (e.g., "pictures,mp4,pdf").
    ext_filter: Option<String>,
    /// Only return files (not directories).
    files_only: bool,
    /// Only return directories (not files).
    dirs_only: bool,
    /// Hide system files (files starting with $).
    hide_system: bool,
    /// Minimum file size filter.
    min_size: Option<u64>,
    /// Maximum file size filter.
    max_size: Option<u64>,
}

impl OwnedQueryFilters {
    /// Create owned filters from borrowed filters.
    pub(crate) fn from_borrowed(filters: &QueryFilters<'_>) -> Self {
        Self {
            parsed: filters.parsed.clone(),
            ext_filter: filters.ext_filter.map(String::from),
            files_only: filters.files_only,
            dirs_only: filters.dirs_only,
            hide_system: filters.hide_system,
            min_size: filters.min_size,
            max_size: filters.max_size,
        }
    }

    /// Execute query with these filters.
    pub(crate) fn execute(&self, df: uffs_polars::DataFrame) -> Result<uffs_polars::DataFrame> {
        use uffs_core::MftQuery;

        let mut query = MftQuery::new(df);

        query = query.pattern(&self.parsed)?;

        if let Some(ext_str) = &self.ext_filter {
            let parsed_ext_filter = ExtensionFilter::parse(ext_str)
                .map_err(|err| anyhow::anyhow!("Invalid extension filter: {err}"))?;
            query = query.extension_filter(&parsed_ext_filter);
        }

        if self.files_only {
            query = query.files_only();
        } else if self.dirs_only {
            query = query.directories_only();
        }

        if self.hide_system {
            query = query.hide_system();
        }

        if let Some(min) = self.min_size {
            query = query.min_size(min);
        }
        if let Some(max) = self.max_size {
            query = query.max_size(max);
        }

        Ok(query.collect()?)
    }
}

/// Load index only (no query) for Windows LIVE streaming output.
///
/// Returns the `MftIndex` directly for use with `write_index_streaming`.
#[expect(clippy::single_call_fn, reason = "extracted for streaming output path")]
pub(crate) async fn load_live_index(
    drive_letter: char,
    no_cache: bool,
) -> Result<(uffs_mft::MftIndex, u128)> {
    let t_load = std::time::Instant::now();

    let reader = MftReader::open(drive_letter)
        .with_context(|| format!("Failed to open drive {drive_letter}:"))?;

    let index = if no_cache {
        info!(drive = %drive_letter, "🔄 --no-cache: reading MFT fresh (streaming)");
        reader.read_all_index().await?
    } else {
        reader.read_index_cached(INDEX_TTL_SECONDS).await?
    };
    let load_ms = t_load.elapsed().as_millis();
    info!(
        drive = %drive_letter,
        load_ms,
        records = index.len(),
        "📊 Windows LIVE: index loaded for streaming output"
    );

    Ok((index, load_ms))
}

// NOTE: execute_index_query, load_and_filter_data_index, and
// load_and_filter_data_index_multi were removed (zero callers — superseded
// by the streaming search path).  Restore from git history if needed.
