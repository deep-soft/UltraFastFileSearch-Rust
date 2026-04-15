// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Daemon-based search: routes CLI search through the UFFS daemon via IPC.
//!
//! This module builds [`SearchParams`](uffs_client::protocol::SearchParams)
//! from CLI arguments and sends the query to the daemon.  Response rows are
//! `SearchRow` — no conversion to `DisplayRow`, no polars, no `uffs-core`.

use anyhow::{Context, Result};
use tracing::info;
use uffs_client::protocol::response::{SearchProfile, SearchRow};
use uffs_client::protocol::{SearchFilterMode, SearchParams, SearchResponseMode};

use super::SearchConfig;

/// Format a number with comma separators (e.g. `1,234,567`).
fn fmt_number(num: usize) -> String {
    let digits = num.to_string();
    let mut result = String::new();
    for (idx, ch) in digits.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}

/// Search via the UFFS daemon.
///
/// Connects to a running daemon (auto-starting if needed), sends the search
/// query, and returns the response `SearchRow`s directly.
///
/// # Data source discovery
///
/// - **Windows:** the daemon auto-discovers live NTFS drives — no args needed.
/// - **Mac/Linux:** the caller must supply `--data-dir` or `--mft-file` via the
///   CLI.  These paths are forwarded to the daemon when auto-starting. If
///   neither is provided, returns a descriptive error immediately.
#[expect(
    clippy::cognitive_complexity,
    reason = "search param building + response formatting with column projection"
)]
/// # Errors
///
/// Returns an error if the operation fails.
pub async fn search_via_daemon(
    config: &SearchConfig<'_>,
) -> Result<(
    Vec<SearchRow>,
    Vec<uffs_client::protocol::AggregateResultWire>,
)> {
    let spawn_args = build_daemon_spawn_args(config)?;
    let params = build_search_params(config);
    let profile = config.profile || config.benchmark;

    info!(
        pattern = %params.pattern,
        case_sensitive = params.case_sensitive,
        limit = ?params.limit,
        "🔌 Searching via daemon"
    );

    let t_connect = std::time::Instant::now();
    let mut client = uffs_client::connect::UffsClient::connect_with_args(&spawn_args)
        .await
        .with_context(|| "Failed to connect to UFFS daemon")?;
    let connect_ms = t_connect.elapsed().as_millis();

    // Wait for the daemon to finish loading indices before searching.
    // First search after daemon auto-start would otherwise hit an empty index.
    let t_ready = std::time::Instant::now();
    client
        .await_ready(core::time::Duration::from_mins(2))
        .await
        .with_context(|| "Daemon did not become ready in time")?;
    let ready_ms = t_ready.elapsed().as_millis();

    // If the CLI was given explicit --mft-file paths, ensure those drives
    // are loaded in the daemon.  This covers the case where the daemon was
    // already running (from a previous invocation) and doesn't have the
    // requested drive.
    if !config.mft_file.is_empty() {
        let mft_strings: Vec<String> = config
            .mft_file
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        match client.load_drive(&mft_strings, config.no_cache).await {
            Ok(resp) => {
                for letter in &resp.loaded {
                    info!(drive = %letter, "Hot-loaded drive into daemon");
                }
                for err in &resp.errors {
                    tracing::warn!(error = %err, "Failed to hot-load drive");
                }
            }
            Err(load_err) => {
                tracing::warn!(
                    error = %load_err,
                    "load_drive RPC failed — search may return incomplete results"
                );
            }
        }
    }

    let t_search = std::time::Instant::now();
    let mut response = client
        .search(&params)
        .await
        .with_context(|| "Daemon search failed")?;
    let ipc_ms = t_search.elapsed().as_millis();
    let daemon_ms = response.duration_ms;

    let records_scanned = response.records_scanned;
    let daemon_profile = response.profile.take();
    let aggregations = core::mem::take(&mut response.aggregations);

    // OPT-4: when the daemon wrote results directly to the output file,
    // `rows` is empty and `total_count` tells us how many rows were written.
    // Skip IPC row conversion entirely — just report success.
    let daemon_wrote_file =
        params.output_file.is_some() && response.rows.is_empty() && response.total_count > 0;

    if daemon_wrote_file {
        info!(
            output = ?params.output_file,
            rows = response.total_count,
            duration_ms = response.duration_ms,
            "🔌 Daemon wrote results directly to file"
        );
    }

    info!(
        rows = response.rows.len(),
        aggregations = aggregations.len(),
        duration_ms = response.duration_ms,
        scanned = records_scanned,
        truncated = response.truncated,
        daemon_wrote_file,
        "🔌 Daemon search complete"
    );

    let rows = response.rows;
    let convert_ms: u128 = 0; // no conversion needed — SearchRow used directly

    // Emit profile breakdown to stderr when --profile or --benchmark is active.
    if profile {
        print_profile(&ClientTiming {
            connect_ms,
            ready_ms,
            ipc_ms,
            daemon_ms,
            convert_ms,
            row_count: rows.len(),
            records_scanned,
            daemon_profile,
        });
    }

    Ok((rows, aggregations))
}

/// Client-side timing data collected during a daemon search.
struct ClientTiming {
    /// Time to establish the named-pipe/domain-socket connection (ms).
    connect_ms: u128,
    /// Time waiting for the daemon to report `Ready` status (ms).
    ready_ms: u128,
    /// Total IPC round-trip time including serialization (ms).
    ipc_ms: u128,
    /// Daemon-reported search duration (ms).
    daemon_ms: u64,
    /// Conversion overhead (ms) — zero since `SearchRow` is used directly.
    convert_ms: u128,
    /// Number of result rows returned.
    row_count: usize,
    /// Total records scanned across all drives.
    records_scanned: usize,
    /// Daemon-side profile (populated when `--profile` is active).
    daemon_profile: Option<SearchProfile>,
}

/// Print `--profile` diagnostics to stderr.
///
/// Extracted from `search_via_daemon` to keep that function under the
/// 100-line clippy limit.
#[expect(
    clippy::print_stderr,
    reason = "intentional --profile diagnostic output"
)]
fn print_profile(tm: &ClientTiming) {
    eprintln!("=== PROFILE: Client → Daemon ===");
    eprintln!("  Connect:         {:>6} ms", tm.connect_ms);
    eprintln!("  Await ready:     {:>6} ms", tm.ready_ms);
    eprintln!(
        "  Search (IPC):    {:>6} ms  (daemon: {} ms, transfer: {} ms)",
        tm.ipc_ms,
        tm.daemon_ms,
        tm.ipc_ms.saturating_sub(u128::from(tm.daemon_ms))
    );
    eprintln!(
        "  Convert rows:    {:>6} ms  ({} rows)",
        tm.convert_ms, tm.row_count
    );

    if let Some(prof) = &tm.daemon_profile {
        eprintln!("=== PROFILE: Daemon Internals ===");
        eprintln!("  Uptime:          {:>6} ms", prof.uptime_ms);
        eprintln!(
            "  Startup:         {:>6} ms  (all drives loaded)",
            prof.startup_ms
        );
        eprintln!("  Lock acquire:    {:>6} ms", prof.lock_ms);
        eprintln!(
            "  Search:          {:>6} ms  ({} records scanned)",
            prof.search_ms,
            fmt_number(tm.records_scanned)
        );
        eprintln!(
            "  Row build:       {:>6} ms  ({} → SearchRow)",
            prof.row_build_ms, tm.row_count
        );
        if prof.serialize_ms > 0 {
            eprintln!("  Shmem write:     {:>6} ms", prof.serialize_ms);
        }
        if !prof.drives.is_empty() {
            eprintln!("=== PROFILE: Per-Drive ===");
            eprintln!(
                "  {:>5}  {:>12}  {:>8}  {:>8}  {:>7}  {:>7}  {:>7}",
                "Drive", "Records", "Matches", "Cache", "MFT ms", "Cmpct", "Trigram"
            );
            for dp in &prof.drives {
                eprintln!(
                    "  {:>5}  {:>12}  {:>8}  {:>8}  {:>7}  {:>7}  {:>7}",
                    format!("{}:", dp.drive),
                    fmt_number(dp.records),
                    fmt_number(dp.matches),
                    dp.cache_ms,
                    dp.mft_ms,
                    dp.compact_ms,
                    dp.trigram_ms
                );
            }
            let total_cache: u64 = prof.drives.iter().map(|dp| dp.cache_ms).sum();
            let total_mft: u64 = prof.drives.iter().map(|dp| dp.mft_ms).sum();
            let total_compact: u64 = prof.drives.iter().map(|dp| dp.compact_ms).sum();
            let total_trigram: u64 = prof.drives.iter().map(|dp| dp.trigram_ms).sum();
            eprintln!(
                "  {:>5}  {:>12}  {:>8}  {:>8}  {:>7}  {:>7}  {:>7}",
                "SUM",
                fmt_number(tm.records_scanned),
                "",
                total_cache,
                total_mft,
                total_compact,
                total_trigram
            );
        }
    }
}

/// Build daemon spawn arguments from CLI data sources.
///
/// Forward data-source flags so the daemon loads **only what's needed**:
///
/// - **Windows + `--drive C,D`** → `--drive C --drive D` Daemon loads only C
///   and D (not all 7 drives).
/// - **Windows + no `--drive`** → empty args Daemon auto-discovers all NTFS
///   drives (default).
/// - **Mac/Linux** → `--data-dir` / `--mft-file` forwarded as-is.
fn build_daemon_spawn_args(config: &SearchConfig<'_>) -> Result<Vec<String>> {
    let mut args = Vec::new();

    if cfg!(windows) {
        // Collect the drives the user explicitly asked for.
        let drives: Vec<char> = config
            .multi_drives
            .clone()
            .or_else(|| config.single_drive.map(|ch| vec![ch]))
            .unwrap_or_default();

        if !drives.is_empty() {
            // Tell the daemon to load ONLY these drives — not all 7.
            for &letter in &drives {
                args.push("--drive".to_owned());
                args.push(letter.to_string());
            }
            info!(
                drives = ?drives,
                "Forwarding --drive to daemon spawn (selective load)"
            );
        }
        // No drives specified → daemon auto-discovers all.

        // Also forward --data-dir / --mft-file if provided on Windows
        // (e.g. offline analysis of MFT captures).
        if let Some(dir) = &config.data_dir {
            args.push("--data-dir".to_owned());
            args.push(dir.to_string_lossy().into_owned());
        }
        for mft_path in &config.mft_file {
            args.push("--mft-file".to_owned());
            args.push(mft_path.to_string_lossy().into_owned());
        }
    } else {
        // Forward --data-dir raw — daemon resolves it internally.
        if let Some(dir) = &config.data_dir {
            args.push("--data-dir".to_owned());
            args.push(dir.to_string_lossy().into_owned());
        }

        // Forward explicit --mft-file paths.
        for mft_path in &config.mft_file {
            args.push("--mft-file".to_owned());
            args.push(mft_path.to_string_lossy().into_owned());
        }

        // Non-Windows with no data sources → fail fast.
        if args.is_empty() {
            anyhow::bail!(
                "No MFT data source specified.\n\n\
                 On macOS/Linux, provide MFT files via:\n  \
                 --data-dir <path>   (directory containing *_mft.* files)\n  \
                 --mft-file <path>   (one or more MFT capture files)\n\n\
                 The UFFS daemon needs data to search. On Windows, live NTFS\n\
                 drives are discovered automatically."
            );
        }
    }

    if config.no_cache {
        args.push("--no-cache".to_owned());
    }

    // Forward daemon log configuration from environment variables so that
    // `UFFS_LOG=debug` or `UFFS_LOG_FILE=/tmp/daemon.log` set before a
    // search command automatically propagate to the auto-spawned daemon.
    if let Ok(log_level) = std::env::var("UFFS_LOG") {
        args.push("--log-level".to_owned());
        args.push(log_level);
    }
    if let Ok(log_file) = std::env::var("UFFS_LOG_FILE") {
        args.push("--log-file".to_owned());
        args.push(log_file);
    }

    Ok(args)
}

/// Build [`SearchParams`] from the CLI [`SearchConfig`].
///
/// Maps every CLI flag to the corresponding `SearchParams` field so the
/// daemon applies the same filters.
#[expect(
    clippy::too_many_lines,
    reason = "maps 20+ CLI flags to SearchParams fields one-to-one — each line is a \
              trivial field assignment; splitting would scatter the CLI→params mapping"
)]
fn build_search_params(config: &SearchConfig<'_>) -> SearchParams {
    let filter = if config.files_only {
        Some("files".to_owned())
    } else if config.dirs_only {
        Some("dirs".to_owned())
    } else {
        None
    };

    // Collect target drives (if any).
    let drives: Vec<char> = config
        .multi_drives
        .clone()
        .or_else(|| config.single_drive.map(|ch| vec![ch]))
        .unwrap_or_default();

    // limit=0 in CLI means unlimited → None for daemon.
    // Exception: when running aggregate-only (--agg without --rows),
    // we don't need rows at all, so set limit=0 to avoid the daemon
    // building and transmitting millions of rows we'd discard anyway.
    let agg_only = !config.agg_specs.is_empty() && !config.force_rows;
    let limit = if agg_only {
        Some(0)
    } else {
        (config.limit > 0).then_some(config.limit)
    };
    let filter_mode = if config.files_only {
        Some(SearchFilterMode::Files)
    } else if config.dirs_only {
        Some(SearchFilterMode::Dirs)
    } else {
        Some(SearchFilterMode::All)
    };
    let sorts = config.sort.map_or_else(Vec::new, |sort| {
        SearchParams::canonicalize_legacy_sort(sort, config.sort_desc)
    });
    let projection: Vec<String> = if config.columns.is_empty() {
        Vec::new()
    } else {
        config
            .columns
            .split(',')
            .map(|col| col.trim().to_owned())
            .collect()
    };

    let mut params = SearchParams {
        pattern: config.pattern.to_owned(),
        case_sensitive: config.effective_case_sensitive,
        whole_word: false, // word wrapping is done in pattern parsing already
        match_path: config.match_path,
        sort: config.sort.map(ToOwned::to_owned),
        sorts,
        sort_desc: config.sort_desc,
        limit,
        filter,
        filter_mode,
        drives,
        projection,
        response_mode: Some(SearchResponseMode::Rows),
        min_size: config.min_size,
        max_size: config.max_size,
        min_descendants: config.min_descendants,
        max_descendants: config.max_descendants,
        newer: config.newer.map(ToOwned::to_owned),
        older: config.older.map(ToOwned::to_owned),
        newer_created: config.newer_created.map(ToOwned::to_owned),
        older_created: config.older_created.map(ToOwned::to_owned),
        newer_accessed: config.newer_accessed.map(ToOwned::to_owned),
        older_accessed: config.older_accessed.map(ToOwned::to_owned),
        attr: config.attr_filter.map(ToOwned::to_owned),
        ext: config.ext_filter.map(ToOwned::to_owned),
        exclude: config.exclude.map(ToOwned::to_owned),
        path_contains: config.in_path.map(ToOwned::to_owned),
        type_filter: config.type_filter.map(ToOwned::to_owned),
        min_bulkiness: config.min_bulkiness,
        max_bulkiness: config.max_bulkiness,
        min_name_len: config.min_name_length,
        max_name_len: config.max_name_length,
        min_path_len: config.min_path_length,
        max_path_len: config.max_path_length,
        min_allocated: config.min_size_on_disk,
        max_allocated: config.max_size_on_disk,
        min_treesize: config.min_treesize,
        max_treesize: config.max_treesize,
        min_tree_allocated: config.min_tree_allocated,
        max_tree_allocated: config.max_tree_allocated,
        allowed_months: config.allowed_months.to_vec(),
        hide_system: config.hide_system,
        hide_ads: config.hide_ads,
        profile: config.profile || config.benchmark,
        predicates: Vec::new(),
        aggregations: config
            .agg_specs
            .iter()
            .map(|spec| {
                let is_preset = uffs_client::format::is_aggregate_preset(spec);
                uffs_client::protocol::AggregateSpecWire {
                    kind: if spec == "count" {
                        "count".to_owned()
                    } else if is_preset {
                        "preset".to_owned()
                    } else {
                        // Raw power syntax → pass as "raw" kind with the
                        // syntax string in the `label` field so the daemon
                        // can parse it via `parse_agg_spec`.
                        "raw".to_owned()
                    },
                    // For "raw" kind, the daemon expects the power
                    // syntax string in the label field.
                    label: (!is_preset && spec != "count").then(|| spec.clone()),
                    preset: is_preset.then(|| spec.clone()),
                    ..uffs_client::protocol::AggregateSpecWire::default()
                }
            })
            .collect(),
        include_rows: config.agg_specs.is_empty() || config.force_rows,
        agg_cursor: config.agg_cursor.clone(),
        agg_page_size: config.agg_page_size,
        // OPT-4: direct file output — daemon writes results to file,
        // bypassing SearchRow, JSON serialization, and IPC transfer.
        // Passes the full OutputConfig so the daemon produces identical
        // output to the CLI (separator, quotes, header, pos/neg, columns).
        output_file: if config.out.is_empty() {
            None
        } else {
            // Resolve to absolute path so the daemon can write to it.
            let path = std::path::Path::new(config.out);
            let abs = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir().unwrap_or_default().join(path)
            };
            Some(abs.to_string_lossy().into_owned())
        },
        output_separator: Some(config.sep.to_owned()),
        output_quote: Some(config.quotes.to_owned()),
        output_header: Some(config.header),
        output_pos: Some(config.pos.to_owned()),
        output_neg: Some(config.neg.to_owned()),
        output_columns: if config.columns.is_empty() {
            None
        } else {
            Some(config.columns.to_owned())
        },
        output_parity_compat: (config.columns == "parity").then_some(true),
        output_tz_offset_hours: config.tz_offset,
    };
    params.populate_canonical_fields();
    params
}
