// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Configuration building and output finalization for daemon search.

extern crate alloc;

use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing::info;
use uffs_core::output::OutputConfig;
use uffs_core::pattern::ParsedPattern;

use super::super::output::write_native_results;
use super::SearchConfig;
use super::util::compute_output_targets;

/// Build search configuration from CLI parameters.
#[expect(clippy::too_many_arguments, reason = "mirrors CLI parameters")]
#[expect(clippy::fn_params_excessive_bools, reason = "mirrors CLI parameters")]
pub(super) fn build_search_config<'a>(
    pattern: &'a str,
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
    format: &'a str,
    case_sensitive: bool,
    smart_case: bool,
    attr_filter: Option<&'a str>,
    newer: Option<&'a str>,
    older: Option<&'a str>,
    newer_created: Option<&'a str>,
    older_created: Option<&'a str>,
    newer_accessed: Option<&'a str>,
    older_accessed: Option<&'a str>,
    exclude: Option<&'a str>,
    in_path: Option<&'a str>,
    type_filter: Option<&'a str>,
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
    allowed_months: &'a [u32],
    match_path: bool,
    word: bool,
    sort: Option<&'a str>,
    sort_desc: bool,
    ext_filter: Option<&'a str>,
    out: &'a str,
    columns: &'a str,
    sep: &'a str,
    quotes: &'a str,
    header: bool,
    pos: &'a str,
    neg: &'a str,
    tz_offset: Option<i32>,
    agg_specs: Vec<String>,
    force_rows: bool,
    agg_cursor: Option<String>,
    agg_page_size: Option<u16>,
    start_time: std::time::Instant,
) -> Result<SearchConfig<'a>> {
    // Smart case: if enabled and pattern has any uppercase letter,
    // automatically enable case-sensitive matching.
    let effective_case_sensitive =
        case_sensitive || (smart_case && pattern.chars().any(|ch| ch.is_ascii_uppercase()));

    // Whole word: wrap pattern in \b...\b regex.
    let effective_pattern: alloc::borrow::Cow<'_, str> = if word {
        alloc::borrow::Cow::Owned(format!(">\\b{pattern}\\b"))
    } else {
        alloc::borrow::Cow::Borrowed(pattern)
    };

    let parsed = ParsedPattern::parse(&effective_pattern)
        .with_context(|| format!("Invalid pattern: {pattern}"))?
        .with_case_sensitive(effective_case_sensitive);

    // Extract drive letter from pattern (e.g., "c:/*.txt" → Some('C')).
    let pattern_drive = parsed.drive();

    let is_parity = columns.eq_ignore_ascii_case("parity");
    let mut output_config = OutputConfig::new()
        .with_columns(columns)
        .with_separator(sep)
        .with_quote(quotes)
        .with_header(header)
        .with_pos(pos)
        .with_neg(neg)
        .with_parity_compat(is_parity);
    if let Some(hours) = tz_offset {
        output_config = output_config.with_tz_offset_hours(hours);
    }

    let output_targets = compute_output_targets(single_drive, multi_drives.as_ref(), pattern_drive);

    Ok(SearchConfig {
        pattern,
        single_drive,
        multi_drives,
        mft_file,
        data_dir,
        effective_case_sensitive,
        profile,
        benchmark,
        no_cache,
        files_only,
        dirs_only,
        hide_system,
        hide_ads,
        ext_filter,
        min_size,
        max_size,
        min_descendants,
        max_descendants,
        limit,
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
        sort,
        sort_desc,
        format,
        out,
        output_config,
        output_targets,
        start_time,
        agg_specs,
        force_rows,
        agg_cursor,
        agg_page_size,
    })
}

/// Finalize search output — all paths converge here.
///
/// For json/table formats, a small `DataFrame` is created from the result rows
/// only (not the full MFT) to reuse existing Polars serialization.
/// Aggregate results (if any) are printed after the rows.
pub(super) fn finalize_output(
    rows: &[uffs_core::search::backend::DisplayRow],
    aggregations: &[uffs_client::protocol::AggregateResultWire],
    config: &SearchConfig<'_>,
) -> Result<()> {
    let elapsed = config.start_time.elapsed();
    let t_output = std::time::Instant::now();

    // Skip row output when running aggregate-only (unless --rows was given).
    let has_aggs = !aggregations.is_empty();
    let show_rows = !config.benchmark && (!has_aggs || config.force_rows);
    if show_rows {
        write_native_results(
            rows,
            config.format,
            config.out,
            &config.output_config,
            &config.output_targets,
            elapsed,
            config.pattern,
        )?;
    }

    // Print aggregate results (from --agg, --count, --facet, etc.)
    if !aggregations.is_empty() {
        match config.format {
            "json" => {
                let json = serde_json::to_string_pretty(aggregations)?;
                writeln!(std::io::stdout(), "{json}")?;
            }
            "csv" | "tsv" => {
                crate::commands::aggregate::print_csv_results(
                    aggregations,
                    config.format == "tsv",
                )?;
            }
            _ => {
                crate::commands::aggregate::print_table_results(aggregations)?;
            }
        }
    }

    let output_ms = t_output.elapsed().as_millis();
    let wall_ms = config.start_time.elapsed().as_millis();

    tracing::debug!(
        target: "cache_profile",
        output_ms = %output_ms,
        rows = rows.len(),
        wall_ms = %wall_ms,
        "output_wall_total"
    );

    if config.benchmark {
        print_benchmark_stats_native(rows, elapsed);
    } else if config.profile {
        print_profile_stats_native(rows.len(), output_ms, elapsed);
    }

    info!(count = rows.len(), "Search complete");
    Ok(())
}

/// Print profile statistics for native output.
#[expect(
    clippy::print_stderr,
    reason = "intentional user-facing --profile output"
)]
fn print_profile_stats_native(row_count: usize, output_ms: u128, elapsed: core::time::Duration) {
    let total_ms = elapsed.as_millis();
    eprintln!("=== PROFILE: Output ===");
    eprintln!("  Output/write:    {output_ms:>6} ms  ({row_count} rows)");
    eprintln!("=== TOTAL: {total_ms} ms ===");
}

/// Print benchmark statistics for native results.
#[expect(clippy::print_stderr, reason = "intentional user-facing output")]
fn print_benchmark_stats_native(
    rows: &[uffs_core::search::backend::DisplayRow],
    elapsed: core::time::Duration,
) {
    let total_ms = elapsed.as_millis();
    let secs = elapsed.as_secs_f64();
    eprintln!("=== BENCHMARK MODE (no output) ===");
    eprintln!("  Records found:   {:>10}", rows.len());
    eprintln!("  Total time:      {total_ms:>10} ms ({secs:.2} s)");
}
