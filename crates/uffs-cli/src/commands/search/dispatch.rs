//! Search dispatch routing and configuration building.

extern crate alloc;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use tracing::info;
use uffs_core::output::OutputConfig;
use uffs_core::pattern::ParsedPattern;
use uffs_core::tree::add_tree_columns;

use super::super::output::{can_write_native_results, write_native_results, write_results};
use super::super::raw_io::{QueryFilters, load_and_filter_data, load_and_filter_from_mft_file};
use super::streaming_io::build_record_filter;
use super::util::{compute_output_targets, is_full_scan_query};
use super::{SearchConfig, SearchDispatchResult};

/// Dispatch search to the appropriate execution path.
///
/// Returns `StreamingComplete` if output was written directly (early return),
/// or `DataFrame` if results need standard output processing.
///
/// The `--pipeline` flag controls which execution path is used:
/// - `legacy`  → current streaming + compact paths (stable, default)
/// - `unified` → new unified compact-only path (under development)
pub(super) async fn dispatch_search(config: &SearchConfig<'_>) -> Result<SearchDispatchResult> {
    // ── Pipeline fork ──────────────────────────────────────────────
    if config.pipeline == "unified" {
        info!(pipeline = "unified", "🔀 Using UNIFIED search pipeline");
        return dispatch_unified(config).await;
    }

    // ── [LEGACY_PIPELINE] — all code below is the legacy dispatch ──
    dispatch_legacy(config).await
}

/// **`[LEGACY_PIPELINE]`** Legacy dispatch — streaming + compact + `DataFrame`
/// paths.
///
/// This is the stable, production search pipeline.  All code reachable from
/// this function is tagged `[LEGACY_PIPELINE]` for easy identification and
/// eventual removal once the unified pipeline reaches parity.
async fn dispatch_legacy(config: &SearchConfig<'_>) -> Result<SearchDispatchResult> {
    // [LEGACY_PIPELINE] Multi-file streaming path (cross-platform).
    if config.mft_file.len() > 1
        && !config.benchmark
        && can_write_native_results(config.format, &config.output_config)
    {
        run_multi_file_dispatch(config)?;
        return Ok(SearchDispatchResult::StreamingComplete);
    }

    // [LEGACY_PIPELINE] Single-file streaming path (cross-platform).
    if let Some(mft_path) = config.mft_file.first() {
        if !config.benchmark && can_write_native_results(config.format, &config.output_config) {
            run_single_file_dispatch(config, mft_path)?;
            return Ok(SearchDispatchResult::StreamingComplete);
        }
    }

    // [LEGACY_PIPELINE] Windows LIVE paths.
    #[cfg(windows)]
    {
        if let Some(result) = super::live::dispatch_windows_live(config).await? {
            return Ok(result);
        }
    }

    // [LEGACY_PIPELINE] Fallback: native compact or legacy DataFrame path.
    run_dataframe_search(config).await
}

/// Unified pipeline dispatch — compact-only path.
///
/// Replaces all legacy paths (streaming, `DataFrame`, native compact)
/// with a single compact-index-based flow:
///
/// 1. Load MFT data → `MftIndex` → `DriveCompactIndex`
/// 2. Search via `MultiDriveBackend` (handles pattern, sort, limit)
/// 3. Apply `SearchFilters` (size, date, attr, extension, exclude)
/// 4. Return `NativeRows` for output
async fn dispatch_unified(config: &SearchConfig<'_>) -> Result<SearchDispatchResult> {
    use uffs_core::search::backend::{FilterMode, MultiDriveBackend};
    use uffs_core::search::filters::SearchFilters;

    // ── Escape hatch: explicit `DataFrame` requests fall through to legacy ──
    if config.query_mode == "dataframe" || config.index.is_some() {
        info!(
            pipeline = "unified",
            "⤵ Falling back to legacy for DataFrame/index path"
        );
        return dispatch_legacy(config).await;
    }

    // ── Build search filters (the 14 that were missing in v0.4.30) ───
    let search_filters = SearchFilters::from_params(
        config.filters.hide_system,
        config.filters.min_size,
        config.filters.max_size,
        config.filters.min_descendants,
        config.filters.max_descendants,
        config.newer,
        config.older,
        config.newer_created,
        config.older_created,
        config.newer_accessed,
        config.older_accessed,
        config.attr_filter,
        config.filters.ext_filter,
        config.exclude,
    );

    let filter_mode = if config.filters.files_only {
        FilterMode::FilesOnly
    } else if config.filters.dirs_only {
        FilterMode::DirsOnly
    } else {
        FilterMode::All
    };

    // ── Load drives into compact indices ─────────────────────────────
    let mut backend = MultiDriveBackend::new();
    configure_sort(config, &mut backend);
    load_unified_drives(config, &mut backend)?;

    if backend.drives.is_empty() {
        bail!("No drives loaded — nothing to search");
    }

    // ── Search ────────────────────────────────────────────────────────
    let pattern = config.filters.parsed.pattern();
    // limit=0 → None (unlimited); >0 → Some(n).
    let limit = (config.filters.limit > 0).then_some(config.filters.limit);

    let result = backend.search(
        pattern,
        config.effective_case_sensitive,
        false,
        limit,
        filter_mode,
        &search_filters,
    );

    info!(
        rows = result.rows.len(),
        duration_ms = result.duration.as_millis(),
        scanned = result.records_scanned,
        "🔍 Unified search complete"
    );

    Ok(SearchDispatchResult::NativeRows(result.rows))
}

/// Configure sort on `MultiDriveBackend` from `SearchConfig`.
fn configure_sort(
    config: &SearchConfig<'_>,
    backend: &mut uffs_core::search::backend::MultiDriveBackend,
) {
    use uffs_core::search::backend::{SortColumn, parse_sort_spec};

    if let Some(sort_str) = config.sort {
        let specs = parse_sort_spec(sort_str);
        if let Some(first) = specs.first() {
            backend.sort_column = first.column;
            backend.sort_desc = first.descending;
            if let Some(rest) = specs.get(1..) {
                backend.extra_sort_tiers = rest.to_vec();
            }
        }
    } else if config.sort_desc {
        backend.sort_column = SortColumn::Size;
        backend.sort_desc = true;
    }
}

/// Load compact indices for the unified pipeline.
///
/// Handles `--mft-file` (cross-platform) and Windows LIVE paths.
fn load_unified_drives(
    config: &SearchConfig<'_>,
    backend: &mut uffs_core::search::backend::MultiDriveBackend,
) -> Result<()> {
    use uffs_core::compact::{MftSource, load_drive};

    if config.mft_file.is_empty() {
        load_live_drives(config, backend)?;
    } else {
        let drive_letters: Vec<Option<char>> = config.multi_drives.as_ref().map_or_else(
            || vec![config.single_drive; config.mft_file.len()],
            |drives| drives.iter().copied().map(Some).collect(),
        );

        for (path, drive_override) in config.mft_file.iter().zip(drive_letters.iter()) {
            let source = MftSource::File(path.clone(), *drive_override);
            let (compact, timing) = load_drive(&source, config.no_cache)
                .with_context(|| format!("Failed to load MFT file: {}", path.display()))?;
            info!(
                drive = %compact.letter,
                records = compact.records.len(),
                mft_ms = timing.mft,
                compact_ms = timing.compact,
                trigram_ms = timing.trigram,
                "📦 Loaded drive (unified pipeline)"
            );
            backend.drives.push(compact);
        }
    }
    Ok(())
}

/// Load live NTFS drives (Windows) or error (non-Windows).
#[cfg(windows)]
fn load_live_drives(
    config: &SearchConfig<'_>,
    backend: &mut uffs_core::search::backend::MultiDriveBackend,
) -> Result<()> {
    use uffs_core::compact::{MftSource, load_drive};

    let drives_to_search = config
        .multi_drives
        .as_ref()
        .map(|d| d.clone())
        .or_else(|| config.single_drive.map(|ch| vec![ch]))
        .or_else(|| config.filters.parsed.drive().map(|ch| vec![ch]))
        .map_or_else(|| resolve_all_ntfs_drives(), Ok)?;

    for &ch in &drives_to_search {
        let source = MftSource::Live(ch);
        let (compact, timing) = load_drive(&source, config.no_cache)
            .with_context(|| format!("Failed to load live drive {ch}:"))?;
        info!(
            drive = %ch,
            records = compact.records.len(),
            mft_ms = timing.mft,
            compact_ms = timing.compact,
            trigram_ms = timing.trigram,
            "📦 Loaded live drive (unified pipeline)"
        );
        backend.drives.push(compact);
    }
    Ok(())
}

/// Load live NTFS drives — non-Windows stub that always errors.
#[cfg(not(windows))]
fn load_live_drives(
    _config: &SearchConfig<'_>,
    _backend: &mut uffs_core::search::backend::MultiDriveBackend,
) -> Result<()> {
    bail!(
        "No --mft-file specified. On non-Windows, you must provide an MFT file:\n\n\
         uffs search --mft-file C_drive.raw \"*.txt\""
    );
}

/// Detect all NTFS drives, requiring elevation.
#[cfg(windows)]
fn resolve_all_ntfs_drives() -> Result<Vec<char>> {
    if !uffs_mft::is_elevated() {
        bail!(
            "Administrator privileges required.\n\n\
             UFFS reads the NTFS Master File Table directly, which requires elevated access.\n\n\
             Solutions:\n\
             1. Run PowerShell/Terminal as Administrator\n\
             2. Use a pre-built index: uffs search --index <file.parquet> \"*.txt\""
        );
    }
    let all = uffs_mft::detect_ntfs_drives();
    if all.is_empty() {
        bail!("No NTFS drives found on this system");
    }
    info!(drives = ?all, "No drive specified — searching all NTFS drives");
    Ok(all)
}

/// **`[LEGACY_PIPELINE]`** Dispatch multi-file streaming search.
fn run_multi_file_dispatch(config: &SearchConfig<'_>) -> Result<()> {
    let drive_letters: Vec<char> = if let Some(drives) = &config.multi_drives {
        if drives.len() != config.mft_file.len() {
            bail!(
                "Number of --drives ({}) must match number of --mft-file ({}).",
                drives.len(),
                config.mft_file.len()
            );
        }
        drives.clone()
    } else {
        config
            .mft_file
            .iter()
            .map(|path| super::util::infer_drive_from_filename(path))
            .collect()
    };

    info!(
        files = config.mft_file.len(),
        drives = ?drive_letters,
        "📂 MULTI-FILE STREAMING (cross-platform multi-drive)"
    );

    let compiled_pattern = if config.is_full_scan {
        None
    } else {
        Some(uffs_core::compile_parsed_pattern(config.filters.parsed)?)
    };

    let rec_filter = build_record_filter(
        &config.filters,
        config.attr_filter,
        config.newer,
        config.older,
        config.newer_created,
        config.older_created,
        config.newer_accessed,
        config.older_accessed,
        config.exclude,
        config.sort,
        config.sort_desc,
        config.show_ads,
    );

    let stream_config = super::mft_file::MultiFileStreamConfig {
        mft_files: &config.mft_file,
        drive_letters,
        compiled_pattern,
        format: config.format,
        out: config.out,
        output_config: &config.output_config,
        output_targets: &config.output_targets,
        pattern: config.pattern,
        case_sensitive: config.effective_case_sensitive,
        is_path_pattern: config.filters.parsed.is_path_pattern() && !config.name_only,
        rec_filter,
        debug_tree: config.debug_tree,
        chaos_seed: config.chaos_seed,
        reserved_allocated: config.reserved_allocated,
    };

    let t_output = std::time::Instant::now();
    let total_rows = super::mft_file::run_multi_file_streaming(&stream_config)?;
    let output_ms = t_output.elapsed().as_millis();
    info!(output_ms, total_rows, "📊 multi-file streaming complete");
    Ok(())
}

/// **`[LEGACY_PIPELINE]`** Dispatch single-file streaming search.
fn run_single_file_dispatch(config: &SearchConfig<'_>, mft_path: &std::path::Path) -> Result<()> {
    let stream_config = super::single_file::SingleFileStreamConfig {
        mft_path,
        pattern: config.pattern,
        single_drive: config.single_drive,
        effective_case_sensitive: config.effective_case_sensitive,
        filters: &config.filters,
        attr_filter: config.attr_filter,
        newer: config.newer,
        older: config.older,
        newer_created: config.newer_created,
        older_created: config.older_created,
        newer_accessed: config.older_accessed,
        older_accessed: config.older_accessed,
        exclude: config.exclude,
        sort: config.sort,
        sort_desc: config.sort_desc,
        is_full_scan: config.is_full_scan,
        name_only: config.name_only,
        format: config.format,
        out: config.out,
        output_config: &config.output_config,
        output_targets: &config.output_targets,
        profile: config.profile,
        debug_tree: config.debug_tree,
        show_ads: config.show_ads,
        chaos_seed: config.chaos_seed,
        reserved_allocated: config.reserved_allocated,
        start_time: config.start_time,
    };
    super::single_file::run_single_file_streaming(&stream_config)
}

/// **`[LEGACY_PIPELINE]`** Execute search fallback path.
///
/// Returns `NativeRows` for multi-drive (compact index search) or
/// `DataFrame` for legacy paths (parquet index, single-drive, mft-file).
async fn run_dataframe_search(config: &SearchConfig<'_>) -> Result<SearchDispatchResult> {
    // For --mft-file: load and query via existing helper (DataFrame path).
    if let Some(mft_path) = config.mft_file.first() {
        let df = load_and_filter_from_mft_file(
            mft_path,
            config.single_drive,
            &config.filters,
            config.output_config.needs_path_column(),
            config.profile,
            config.debug_tree,
            config.chaos_seed,
        )?;
        return Ok(SearchDispatchResult::DataFrame(df));
    }

    // For multi-drive: use native compact index search (no DataFrame).
    #[cfg(windows)]
    if let Some(ref drives) = config.multi_drives {
        let rows = super::multi_drive::search_multi_drive_filtered(
            drives,
            &config.filters,
            config.output_config.needs_path_column(),
            config.no_bitmap,
            config.no_cache,
        )
        .await?;
        return Ok(SearchDispatchResult::NativeRows(rows));
    }

    // For --index (parquet) or explicit single-drive: legacy DataFrame path.
    if config.index.is_some()
        || config.single_drive.is_some()
        || config.filters.parsed.drive().is_some()
    {
        let df = load_and_filter_data(
            config.index.clone(),
            None, // multi_drives already handled above
            config.single_drive,
            &config.filters,
            config.output_config.needs_path_column(),
            config.profile,
            config.no_bitmap,
        )
        .await?;
        return Ok(SearchDispatchResult::DataFrame(df));
    }

    // Auto-detect: no drive, no index → find all NTFS drives and search natively.
    #[cfg(windows)]
    {
        if !uffs_mft::is_elevated() {
            bail!(
                "Administrator privileges required.\n\n\
                 UFFS reads the NTFS Master File Table directly, which requires elevated access.\n\n\
                 Solutions:\n\
                 1. Run PowerShell/Terminal as Administrator\n\
                 2. Use a pre-built index: uffs search --index <file.parquet> \"*.txt\""
            );
        }
        let all_drives = uffs_mft::detect_ntfs_drives();
        if all_drives.is_empty() {
            bail!("No NTFS drives found on this system");
        }
        info!(drives = ?all_drives, count = all_drives.len(), "No drive specified — searching all NTFS drives");
        let rows = super::multi_drive::search_multi_drive_filtered(
            &all_drives,
            &config.filters,
            config.output_config.needs_path_column(),
            config.no_bitmap,
            config.no_cache,
        )
        .await?;
        return Ok(SearchDispatchResult::NativeRows(rows));
    }
    #[cfg(not(windows))]
    {
        bail!(
            "No drive specified. Use --drive, --drives, --index, or include drive in pattern (e.g., c:/pro*)"
        )
    }
}

/// Build search configuration from CLI parameters.
#[expect(clippy::too_many_arguments, reason = "mirrors CLI parameters")]
#[expect(clippy::fn_params_excessive_bools, reason = "mirrors CLI parameters")]
pub(super) fn build_search_config<'a>(
    pattern: &'a str,
    single_drive: Option<char>,
    multi_drives: Option<Vec<char>>,
    index: Option<PathBuf>,
    mft_file: Vec<PathBuf>,
    files_only: bool,
    dirs_only: bool,
    hide_system: bool,
    profile: bool,
    debug_tree: bool,
    benchmark: bool,
    no_bitmap: bool,
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
    name_only: bool,
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
    query_mode: &'a str,
    pipeline: &'a str,
    tz_offset: Option<i32>,
    chaos_seed: Option<u64>,
    show_ads: bool,
    reserved_allocated: Option<u64>,
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

    let filters = QueryFilters {
        parsed: Box::leak(Box::new(parsed)),
        ext_filter,
        files_only,
        dirs_only,
        hide_system,
        min_size,
        max_size,
        min_descendants,
        max_descendants,
        limit,
    };

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
        compute_output_targets(single_drive, multi_drives.as_ref(), filters.parsed.drive());
    let is_full_scan = is_full_scan_query(&filters);

    Ok(SearchConfig {
        pattern,
        single_drive,
        multi_drives,
        index,
        mft_file,
        filters,
        effective_case_sensitive,
        profile,
        debug_tree,
        benchmark,
        no_bitmap,
        no_cache,
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
        is_full_scan,
        name_only,
        query_mode,
        pipeline,
        chaos_seed,
        show_ads,
        reserved_allocated,
        start_time,
    })
}

/// Finalize `DataFrame` output with tree columns and output writing.
pub(super) fn finalize_dataframe_output(
    mut results: uffs_polars::DataFrame,
    config: &SearchConfig<'_>,
) -> Result<()> {
    let t_tree = std::time::Instant::now();
    if !config.benchmark && config.output_config.needs_tree_columns() {
        let tree_cols = config.output_config.get_tree_columns();
        let missing_cols: Vec<_> = tree_cols
            .iter()
            .filter(|col| results.column(col.column_name()).is_err())
            .copied()
            .collect();

        if !missing_cols.is_empty() {
            info!(columns = missing_cols.len(), "Computing tree metrics");
            results = add_tree_columns(&results, &missing_cols)
                .context("Failed to compute tree columns")?;
        }
    }
    let tree_ms = t_tree.elapsed().as_millis();

    let elapsed = config.start_time.elapsed();
    let t_output = std::time::Instant::now();
    if !config.benchmark {
        write_results(
            &results,
            config.format,
            config.out,
            &config.output_config,
            &config.output_targets,
            elapsed,
            config.pattern,
        )?;
    }
    let output_ms = t_output.elapsed().as_millis();

    if config.benchmark {
        print_benchmark_stats(&results, elapsed);
    } else if config.profile {
        print_profile_stats(&results, tree_ms, output_ms, elapsed);
    }

    info!(count = results.height(), "Search complete");
    Ok(())
}

/// Finalize **native** `DisplayRow` output — no full-MFT `DataFrame` involved.
///
/// For json/table formats, a small `DataFrame` is created from the result rows
/// only (not the full MFT) to reuse existing Polars serialization.
pub(super) fn finalize_native_output(
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
            "[CACHE_PROFILE] output_write:  {output_ms:>6} ms  ({} rows)",
            rows.len(),
        );
        eprintln!("[CACHE_PROFILE] wall_total:    {wall_ms:>6} ms");
    }

    if config.benchmark {
        print_benchmark_stats_native(rows, elapsed);
    } else if config.profile {
        print_profile_stats_native(rows.len(), output_ms, elapsed);
    }

    info!(count = rows.len(), "Search complete (native)");
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

/// Print benchmark statistics.
#[expect(clippy::print_stderr, reason = "intentional user-facing output")]
fn print_benchmark_stats(results: &uffs_polars::DataFrame, elapsed: core::time::Duration) {
    let row_count = results.height();
    let total_ms = elapsed.as_millis();
    let secs = elapsed.as_secs_f64();
    eprintln!("=== BENCHMARK MODE (no output) ===");
    eprintln!("  Records found:   {row_count:>10}");
    eprintln!("  Total time:      {total_ms:>10} ms ({secs:.2} s)");
    #[expect(
        clippy::cast_precision_loss,
        reason = "row_count as f64 is fine for display"
    )]
    #[expect(
        clippy::float_arithmetic,
        reason = "throughput calculation for display"
    )]
    let throughput = row_count as f64 / secs;
    eprintln!("  Throughput:      {throughput:>10.0} records/sec");
}

/// Print profiling statistics.
#[expect(clippy::print_stderr, reason = "intentional user-facing output")]
fn print_profile_stats(
    results: &uffs_polars::DataFrame,
    tree_ms: u128,
    output_ms: u128,
    elapsed: core::time::Duration,
) {
    let row_count = results.height();
    let total_ms = elapsed.as_millis();
    eprintln!("=== PROFILE: Output ===");
    eprintln!("  Tree columns:    {tree_ms:>6} ms");
    eprintln!("  Output/write:    {output_ms:>6} ms  ({row_count} rows)");
    eprintln!("=== TOTAL: {total_ms} ms ===");
}
