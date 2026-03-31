//! Search dispatch routing and configuration building.
//!
//! All search paths converge on a single unified compact-index pipeline.
//! The only escape hatch is `--index` (parquet), which loads a pre-built
//! `DataFrame` and converts it to `DisplayRow`s.

extern crate alloc;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use tracing::info;
use uffs_core::output::OutputConfig;
use uffs_core::pattern::ParsedPattern;

use super::super::output::write_native_results;
use super::super::raw_io::QueryFilters;
use super::SearchConfig;
use super::util::compute_output_targets;

/// Dispatch search — unified compact-index pipeline.
///
/// Returns `Vec<DisplayRow>` for all paths. The only escape hatch is
/// `--index` (parquet), which is handled inline.
///
/// 1. Load MFT data → `MftIndex` → `DriveCompactIndex`
/// 2. Search via `MultiDriveBackend` (handles pattern, sort, limit)
/// 3. Apply `SearchFilters` (size, date, attr, extension, exclude)
/// 4. Return `Vec<DisplayRow>` for output
pub(super) fn dispatch_search(
    config: &SearchConfig<'_>,
) -> Result<Vec<uffs_core::search::backend::DisplayRow>> {
    use uffs_core::search::backend::{FilterMode, MultiDriveBackend};
    use uffs_core::search::filters::SearchFilters;

    // ── Escape hatch: --index (parquet) ──────────────────────────────
    if let Some(index_path) = &config.index {
        return load_parquet_search(index_path, config);
    }

    // ── Build search filters ─────────────────────────────────────────
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
        "🔍 Search complete"
    );

    Ok(result.rows)
}

/// Load a parquet index file, apply query filters, convert to `DisplayRow`s.
fn load_parquet_search(
    index_path: &std::path::Path,
    config: &SearchConfig<'_>,
) -> Result<Vec<uffs_core::search::backend::DisplayRow>> {
    info!(index = %index_path.display(), "📦 Loading parquet index");
    let df = uffs_mft::MftReader::load_parquet(index_path)
        .with_context(|| format!("Failed to load index: {}", index_path.display()))?;

    let filtered = super::super::raw_io::execute_query(df, &config.filters)?;
    let rows = uffs_core::search::backend::dataframe_to_display_rows(&filtered)
        .map_err(|err| anyhow::anyhow!("DataFrame→DisplayRow conversion failed: {err}"))?;

    info!(rows = rows.len(), "📦 Parquet search complete");
    Ok(rows)
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

    // Resolve --data-dir into concrete MFT file paths (standalone only).
    let mut resolved_files = config.mft_file.clone();
    if let Some(dir) = &config.data_dir {
        resolved_files.extend(uffs_mft::discovery::discover_mft_files(dir));
    }

    if resolved_files.is_empty() {
        load_live_drives(config, backend)?;
    } else {
        let drive_letters: Vec<Option<char>> = config.multi_drives.as_ref().map_or_else(
            || vec![config.single_drive; resolved_files.len()],
            |drives| drives.iter().copied().map(Some).collect(),
        );

        for (path, drive_override) in resolved_files.iter().zip(drive_letters.iter()) {
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

/// Build search configuration from CLI parameters.
#[expect(clippy::too_many_arguments, reason = "mirrors CLI parameters")]
#[expect(clippy::fn_params_excessive_bools, reason = "mirrors CLI parameters")]
pub(super) fn build_search_config<'a>(
    pattern: &'a str,
    single_drive: Option<char>,
    multi_drives: Option<Vec<char>>,
    index: Option<PathBuf>,
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

    Ok(SearchConfig {
        pattern,
        single_drive,
        multi_drives,
        index,
        mft_file,
        data_dir,
        filters,
        effective_case_sensitive,
        profile,
        benchmark,
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
