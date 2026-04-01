//! Configuration building and output finalization for daemon search.

extern crate alloc;

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

    let output_targets =
        compute_output_targets(single_drive, multi_drives.as_ref(), pattern_drive);

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
        sort,
        sort_desc,
        format,
        out,
        output_config,
        output_targets,
        start_time,
    })
}

/// Finalize search output — all paths converge here.
///
/// For json/table formats, a small `DataFrame` is created from the result rows
/// only (not the full MFT) to reuse existing Polars serialization.
pub(super) fn finalize_output(
    rows: &[uffs_core::search::backend::DisplayRow],
    config: &SearchConfig<'_>,
) -> Result<()> {
    let elapsed = config.start_time.elapsed();
    let t_output = std::time::Instant::now();

    if !config.benchmark {
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
    let output_ms = t_output.elapsed().as_millis();
    let wall_ms = config.start_time.elapsed().as_millis();

    #[expect(clippy::print_stderr, reason = "UFFS_CACHE_PROFILE diagnostic output")]
    if std::env::var_os("UFFS_CACHE_PROFILE").is_some() {
        eprintln!(
            "[CACHE_PROFILE] output_total:  {output_ms:>6} ms  ({} rows)",
            rows.len(),
        );
        eprintln!("[CACHE_PROFILE] wall_total:    {wall_ms:>6} ms");
    }

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
