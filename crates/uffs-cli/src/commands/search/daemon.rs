//! Daemon-based search: routes CLI search through the UFFS daemon via IPC.
//!
//! This module builds [`SearchParams`] from CLI arguments and sends the query
//! to the daemon. The response rows are converted to [`DisplayRow`] so the
//! existing output pipeline works unchanged.

use anyhow::{Context, Result};
use tracing::info;
use uffs_client::protocol::{SearchParams, SearchRow};
use uffs_core::search::backend::DisplayRow;

use super::SearchConfig;

/// Search via the UFFS daemon.
///
/// Connects to a running daemon (auto-starting if needed), sends the search
/// query, and converts the response to `Vec<DisplayRow>`.
///
/// # Data source discovery
///
/// - **Windows:** the daemon auto-discovers live NTFS drives — no args needed.
/// - **Mac/Linux:** the caller must supply `--data-dir` or `--mft-file` via the
///   CLI.  These paths are forwarded to the daemon when auto-starting. If
///   neither is provided, returns a descriptive error immediately.
pub(super) async fn search_via_daemon(config: &SearchConfig<'_>) -> Result<Vec<DisplayRow>> {
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
        .await_ready(core::time::Duration::from_secs(120))
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
    let response = client
        .search(&params)
        .await
        .with_context(|| "Daemon search failed")?;
    let ipc_ms = t_search.elapsed().as_millis();
    let daemon_ms = response.duration_ms;

    info!(
        rows = response.rows.len(),
        duration_ms = response.duration_ms,
        scanned = response.records_scanned,
        truncated = response.truncated,
        "🔌 Daemon search complete"
    );

    let t_convert = std::time::Instant::now();
    let rows: Vec<DisplayRow> = response
        .rows
        .into_iter()
        .map(search_row_to_display_row)
        .collect();
    let convert_ms = t_convert.elapsed().as_millis();

    // Emit profile breakdown to stderr when --profile or --benchmark is active.
    #[expect(clippy::print_stderr, reason = "intentional --profile diagnostic output")]
    if profile {
        eprintln!("=== PROFILE: Daemon Round-Trip ===");
        eprintln!("  Connect:         {connect_ms:>6} ms");
        eprintln!("  Await ready:     {ready_ms:>6} ms");
        eprintln!("  Search (IPC):    {ipc_ms:>6} ms  (daemon: {daemon_ms} ms, transfer: {} ms)",
            ipc_ms.saturating_sub(u128::from(daemon_ms)));
        eprintln!("  Convert rows:    {convert_ms:>6} ms  ({} rows)", rows.len());
    }

    Ok(rows)
}

/// Build daemon spawn arguments from CLI data sources.
///
/// On **Windows** this returns an empty list — the daemon auto-discovers
/// live NTFS drives.
///
/// Forward data-source flags to the daemon.
///
/// On **Windows** the daemon auto-discovers live NTFS drives — no args
/// needed.  On **Mac/Linux** we forward `--data-dir` and/or `--mft-file`
/// as-is so the daemon handles discovery internally (DRY).
fn build_daemon_spawn_args(config: &SearchConfig<'_>) -> Result<Vec<String>> {
    // On Windows the daemon discovers live drives automatically.
    if cfg!(windows) {
        return Ok(Vec::new());
    }

    let mut args = Vec::new();

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

    if config.no_cache {
        args.push("--no-cache".to_owned());
    }
    Ok(args)
}

/// Build [`SearchParams`] from the CLI [`SearchConfig`].
///
/// Maps every CLI flag to the corresponding `SearchParams` field so the
/// daemon applies the same filters.
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
    let limit = (config.limit > 0).then_some(config.limit);

    SearchParams {
        pattern: config.pattern.to_owned(),
        case_sensitive: config.effective_case_sensitive,
        whole_word: false, // word wrapping is done in pattern parsing already
        sort: config.sort.map(ToOwned::to_owned),
        sort_desc: config.sort_desc,
        limit,
        filter,
        drives,
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
        hide_system: config.hide_system,
    }
}

/// Convert a daemon [`SearchRow`] to a [`DisplayRow`].
///
/// Both types carry the same fields — this is a mechanical mapping.
fn search_row_to_display_row(row: SearchRow) -> DisplayRow {
    DisplayRow::new(
        row.drive,
        row.path,
        row.size,
        row.is_directory,
        row.modified,
        row.created,
        row.accessed,
        row.flags,
        row.allocated,
        row.descendants,
        row.treesize,
        row.tree_allocated,
    )
}
