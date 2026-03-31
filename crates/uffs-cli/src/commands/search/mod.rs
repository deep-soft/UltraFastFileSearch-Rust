//! Search command implementation.
//!
//! All search paths use the unified compact-index pipeline.  This file
//! contains only the public entry point, shared types, and module wiring.

extern crate alloc;

use std::path::PathBuf;

use anyhow::Result;
use tracing::debug;
use uffs_core::output::OutputConfig;

use super::raw_io::QueryFilters;

/// Search dispatch routing and configuration building.
mod dispatch;
/// Pure utility helpers for the search command.
mod util;

/// Full search configuration - all parameters needed for any search path.
#[expect(clippy::struct_excessive_bools, reason = "mirrors CLI parameters")]
struct SearchConfig<'a> {
    /// Search pattern.
    pattern: &'a str,
    /// Single drive letter override.
    single_drive: Option<char>,
    /// Multiple drive letters.
    multi_drives: Option<Vec<char>>,
    /// Index file path.
    index: Option<PathBuf>,
    /// MFT file paths.
    mft_file: Vec<PathBuf>,
    /// Query filters.
    filters: QueryFilters<'a>,
    /// Effective case sensitivity.
    effective_case_sensitive: bool,
    /// Profile mode.
    profile: bool,
    /// Benchmark mode (no output).
    benchmark: bool,
    /// Disable cache.
    no_cache: bool,
    /// Attribute filter string.
    attr_filter: Option<&'a str>,
    /// Date filters.
    newer: Option<&'a str>,
    /// Date filters.
    older: Option<&'a str>,
    /// Date filters.
    newer_created: Option<&'a str>,
    /// Date filters.
    older_created: Option<&'a str>,
    /// Date filters.
    newer_accessed: Option<&'a str>,
    /// Date filters.
    older_accessed: Option<&'a str>,
    /// Exclude patterns.
    exclude: Option<&'a str>,
    /// Sort column.
    sort: Option<&'a str>,
    /// Sort descending.
    sort_desc: bool,
    /// Output format.
    format: &'a str,
    /// Output path.
    out: &'a str,
    /// Output configuration.
    output_config: OutputConfig,
    /// Output targets (drive letters).
    output_targets: Vec<char>,
    /// Start time for profiling.
    start_time: std::time::Instant,
}

/// Search for files matching a pattern.
///
/// Supports:
/// - Drive prefix in pattern: `c:/pro*` extracts drive C
/// - REGEX patterns: `>C:\Temp.*` (starts with `>`)
/// - Glob patterns: `*.txt`, `**/*.rs`
/// - Literal search: `readme` (no wildcards)
/// - Multi-drive search: `--drives C,D,E`
/// - Extension filtering: `--ext pictures,mp4,pdf`
/// - Output customization: `--out`, `--columns`, `--sep`, `--quotes`,
///   `--header`, `--pos`, `--neg`
#[expect(
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools,
    reason = "CLI entry point passes through all parsed args"
)]
#[expect(
    clippy::single_call_fn,
    reason = "public CLI entry point called from main dispatch"
)]
pub fn search(
    pattern: &str,
    single_drive: Option<char>,
    multi_drives: Option<Vec<char>>,
    index: Option<PathBuf>,
    mft_file: Vec<PathBuf>,
    files_only: bool,
    dirs_only: bool,
    hide_system: bool,
    profile: bool,
    benchmark: bool,
    no_cache: bool,
    min_size: Option<u64>,
    max_size: Option<u64>,
    min_descendants: Option<u32>,
    max_descendants: Option<u32>,
    limit: u32,
    format: &str,
    case_sensitive: bool,
    smart_case: bool,
    attr_filter: Option<&str>,
    newer: Option<&str>,
    older: Option<&str>,
    newer_created: Option<&str>,
    older_created: Option<&str>,
    newer_accessed: Option<&str>,
    older_accessed: Option<&str>,
    exclude: Option<&str>,
    word: bool,
    sort: Option<&str>,
    sort_desc: bool,
    ext_filter: Option<&str>,
    out: &str,
    columns: &str,
    sep: &str,
    quotes: &str,
    header: bool,
    pos: &str,
    neg: &str,
    tz_offset: Option<i32>,
) -> Result<()> {
    let start_time = std::time::Instant::now();
    debug!("[TIMING] search() entered at 0ms");

    // Build configuration from CLI parameters.
    let config = dispatch::build_search_config(
        pattern,
        single_drive,
        multi_drives,
        index,
        mft_file,
        files_only,
        dirs_only,
        hide_system,
        profile,
        benchmark,
        no_cache,
        min_size,
        max_size,
        min_descendants,
        max_descendants,
        limit,
        format,
        case_sensitive,
        smart_case,
        attr_filter,
        newer,
        older,
        newer_created,
        older_created,
        newer_accessed,
        older_accessed,
        exclude,
        word,
        sort,
        sort_desc,
        ext_filter,
        out,
        columns,
        sep,
        quotes,
        header,
        pos,
        neg,
        tz_offset,
        start_time,
    )?;

    // Dispatch to unified search pipeline.
    let rows = dispatch::dispatch_search(&config)?;

    // Output.
    dispatch::finalize_output(&rows, &config)
}
