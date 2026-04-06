//! Search command implementation.
//!
//! All searches route through the UFFS daemon via IPC.
//! Output paths converge on `finalize_output`.

extern crate alloc;

use std::path::PathBuf;

use anyhow::Result;
use tracing::debug;
use uffs_core::output::OutputConfig;

/// Daemon-based search via IPC.
mod daemon;
/// Configuration building and output finalization.
mod dispatch;
/// Pure utility helpers for the search command.
mod util;

/// Full search configuration — all parameters needed for daemon search.
#[expect(clippy::struct_excessive_bools, reason = "mirrors CLI parameters")]
pub struct SearchConfig<'a> {
    /// Search pattern.
    pattern: &'a str,
    /// Single drive letter override.
    single_drive: Option<char>,
    /// Multiple drive letters.
    multi_drives: Option<Vec<char>>,
    /// MFT file paths (forwarded to daemon on spawn).
    mft_file: Vec<PathBuf>,
    /// Data directory (`--data-dir`), forwarded to daemon as-is.
    data_dir: Option<PathBuf>,
    /// Effective case sensitivity.
    effective_case_sensitive: bool,
    /// Profile mode.
    profile: bool,
    /// Benchmark mode (no output).
    benchmark: bool,
    /// Disable cache (forwarded to daemon on spawn).
    no_cache: bool,
    /// Only return files (not directories).
    files_only: bool,
    /// Only return directories (not files).
    dirs_only: bool,
    /// Hide system files (files starting with $).
    hide_system: bool,
    /// Hide NTFS Alternate Data Streams from results.
    hide_ads: bool,
    /// Extension filter string (e.g., "pictures,mp4,pdf").
    ext_filter: Option<&'a str>,
    /// Minimum file size filter.
    min_size: Option<u64>,
    /// Maximum file size filter.
    max_size: Option<u64>,
    /// Minimum descendant count filter (directories).
    min_descendants: Option<u32>,
    /// Maximum descendant count filter (directories).
    max_descendants: Option<u32>,
    /// Maximum number of results to return.
    limit: u32,
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
    /// Directory-path pattern (glob, matched against directory portion only).
    in_path: Option<&'a str>,
    /// File type/category filter.
    type_filter: Option<&'a str>,
    /// Minimum bulkiness percentage.
    min_bulkiness: Option<u64>,
    /// Maximum bulkiness percentage.
    max_bulkiness: Option<u64>,
    /// Minimum filename length filter.
    min_name_length: Option<u16>,
    /// Maximum filename length filter.
    max_name_length: Option<u16>,
    /// Minimum path length filter.
    min_path_length: Option<u16>,
    /// Maximum path length filter.
    max_path_length: Option<u16>,
    /// Minimum on-disk size filter.
    min_size_on_disk: Option<u64>,
    /// Maximum on-disk size filter.
    max_size_on_disk: Option<u64>,
    /// Minimum subtree logical size filter.
    min_treesize: Option<u64>,
    /// Maximum subtree logical size filter.
    max_treesize: Option<u64>,
    /// Minimum subtree on-disk size filter.
    min_tree_allocated: Option<u64>,
    /// Maximum subtree on-disk size filter.
    max_tree_allocated: Option<u64>,
    /// Allowed month numbers (1-12), already resolved from CLI spec.
    allowed_months: &'a [u32],
    /// Match pattern against full path (expand directory name matches
    /// to include all descendants).
    match_path: bool,
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
    /// Aggregate specs (from --agg flag).
    agg_specs: Vec<String>,
    /// Force row output even when aggregates are present (--rows flag).
    force_rows: bool,
}

impl<'a> SearchConfig<'a> {
    /// Create a minimal config for aggregate-only queries (no rows).
    ///
    /// Uses `"*"` as pattern (match everything), no filters, and the given
    /// aggregate specs.  The `data_dir` / `mft_file` are forwarded to the
    /// daemon for auto-start on macOS/Linux.
    pub(crate) fn aggregate_only(
        pattern: &'a str,
        agg_specs: Vec<String>,
        format: &'a str,
        data_dir: Option<PathBuf>,
        mft_file: Vec<PathBuf>,
    ) -> Self {
        Self {
            pattern,
            single_drive: None,
            multi_drives: None,
            mft_file,
            data_dir,
            effective_case_sensitive: false,
            profile: false,
            benchmark: false,
            no_cache: false,
            files_only: false,
            dirs_only: false,
            hide_system: false,
            hide_ads: false,
            ext_filter: None,
            min_size: None,
            max_size: None,
            min_descendants: None,
            max_descendants: None,
            // Use limit=1 (not 0) because the daemon treats 0/None as
            // unlimited, which would serialize all 25M+ records.  The
            // aggregate engine runs over the full index regardless of
            // limit, so limiting rows to 1 is harmless.
            limit: 1,
            attr_filter: None,
            newer: None,
            older: None,
            newer_created: None,
            older_created: None,
            newer_accessed: None,
            older_accessed: None,
            exclude: None,
            in_path: None,
            type_filter: None,
            min_bulkiness: None,
            max_bulkiness: None,
            min_name_length: None,
            max_name_length: None,
            min_path_length: None,
            max_path_length: None,
            min_size_on_disk: None,
            max_size_on_disk: None,
            min_treesize: None,
            max_treesize: None,
            min_tree_allocated: None,
            max_tree_allocated: None,
            allowed_months: &[],
            match_path: false,
            sort: None,
            sort_desc: false,
            format,
            out: "",
            output_config: OutputConfig::default(),
            output_targets: Vec::new(),
            start_time: std::time::Instant::now(),
            agg_specs,
            force_rows: false,
        }
    }
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
///
/// All searches route through the daemon via IPC.
#[expect(
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools,
    reason = "CLI entry point passes through all parsed args"
)]
#[expect(
    clippy::single_call_fn,
    reason = "public CLI entry point called from main dispatch"
)]
pub async fn search(
    pattern: &str,
    single_drive: Option<char>,
    multi_drives: Option<Vec<char>>,
    mft_file: Vec<PathBuf>,
    data_dir: Option<PathBuf>,
    files_only: bool,
    dirs_only: bool,
    hide_system: bool,
    hide_ads: bool,
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
    in_path: Option<&str>,
    type_filter: Option<&str>,
    min_bulkiness: Option<u64>,
    max_bulkiness: Option<u64>,
    min_name_length: Option<u16>,
    max_name_length: Option<u16>,
    min_path_length: Option<u16>,
    max_path_length: Option<u16>,
    min_size_on_disk: Option<u64>,
    max_size_on_disk: Option<u64>,
    min_treesize: Option<u64>,
    max_treesize: Option<u64>,
    min_tree_allocated: Option<u64>,
    max_tree_allocated: Option<u64>,
    allowed_months: &[u32],
    match_path: bool,
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
    agg_specs: Vec<String>,
    force_rows: bool,
) -> Result<()> {
    let start_time = std::time::Instant::now();
    debug!("[TIMING] search() entered at 0ms");

    let config = dispatch::build_search_config(
        pattern,
        single_drive,
        multi_drives,
        mft_file,
        data_dir,
        files_only,
        dirs_only,
        hide_system,
        hide_ads,
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
        in_path,
        type_filter,
        min_bulkiness,
        max_bulkiness,
        min_name_length,
        max_name_length,
        min_path_length,
        max_path_length,
        min_size_on_disk,
        max_size_on_disk,
        min_treesize,
        max_treesize,
        min_tree_allocated,
        max_tree_allocated,
        allowed_months,
        match_path,
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
        agg_specs,
        force_rows,
        start_time,
    )?;

    run_with_config(&config).await
}

/// Execute a search/aggregate via the daemon using a pre-built config.
///
/// This is the shared core used by both the search command and the
/// aggregate subcommand.
pub async fn run_with_config(config: &SearchConfig<'_>) -> Result<()> {
    let (rows, aggregations) = daemon::search_via_daemon(config).await?;
    dispatch::finalize_output(&rows, &aggregations, config)
}
