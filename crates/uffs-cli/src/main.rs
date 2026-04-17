// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! UFFS (Ultra Fast File Search) CLI — thin synchronous client.
//!
//! All heavy lifting (including CLI arg parsing) happens in the daemon.
//! This binary detects subcommands and forwards raw search args via
//! `search_cli` RPC.

// CLI main module uses single-call functions by design
#![expect(
    clippy::single_call_fn,
    reason = "CLI entry point functions are called once from main"
)]

use anyhow::{Context, Result};
#[cfg(test)]
use assert_cmd as _;

pub mod args;
pub mod commands;

/// Run the CLI and return a result.
fn run() -> Result<()> {
    let raw_args: Vec<String> = std::env::args().collect();
    let tokens: Vec<&str> = raw_args.iter().skip(1).map(String::as_str).collect();

    // Fast paths: help / version / no args.
    match tokens.first().copied() {
        None | Some("--help" | "-h" | "help") => {
            args::print_help();
            return Ok(());
        }
        Some("--version" | "-V" | "version") => {
            args::print_version();
            return Ok(());
        }
        _ => {}
    }

    // Detect subcommand as first non-flag token.
    let first = tokens.first().copied().unwrap_or("");
    let subcmd_args = raw_args.get(2..).unwrap_or_default();
    match first {
        "stats" => run_stats(subcmd_args)?,
        "aggregate" | "agg" => run_aggregate(subcmd_args)?,
        "daemon" => run_daemon(subcmd_args)?,
        "mcp" => commands::mcp_mgmt::mcp_from_args(subcmd_args)?,
        "status" => {
            if subcmd_args.iter().any(|arg| arg == "--help" || arg == "-h") {
                args::print_status_help();
            } else {
                commands::system_status::system_status();
            }
        }
        _ => {
            // Default: search — forward ALL args after "uffs" to daemon.
            run_search(raw_args.get(1..).unwrap_or_default())?;
        }
    }

    Ok(())
}

/// Forward raw search args to the daemon via `search_cli` RPC.
fn run_search(args: &[String]) -> Result<()> {
    if args.is_empty() {
        args::print_help();
        return Ok(());
    }

    // Extract daemon-spawn args (--data-dir, --mft-file, --no-cache)
    // from the raw args so we can auto-start the daemon if needed.
    let spawn_args = extract_spawn_args(args);

    let t_connect = std::time::Instant::now();
    let mut client = uffs_client::connect_sync::UffsClientSync::connect_with_args(&spawn_args)
        .with_context(|| "Failed to connect to UFFS daemon")?;
    let connect_ms = t_connect.elapsed().as_millis();

    let t_ready = std::time::Instant::now();
    // 2 minutes — `from_mins` is nightly-only as of 2026-04.
    #[expect(
        clippy::duration_suboptimal_units,
        reason = "Duration::from_mins is nightly-only"
    )]
    let ready_timeout = core::time::Duration::from_secs(120);
    client
        .await_ready(ready_timeout)
        .with_context(|| "Daemon did not become ready in time")?;
    let ready_ms = t_ready.elapsed().as_millis();

    let t_search = std::time::Instant::now();
    // Resolve relative --out paths to absolute using the CLI's cwd, since the
    // daemon process runs in a different working directory.
    let args_owned: Vec<String> = resolve_out_path(args);
    let response = client
        .search_cli_raw(&args_owned)
        .with_context(|| "Daemon search_cli failed")?;
    let ipc_ms = t_search.elapsed().as_millis();

    let aggregations = response
        .get("aggregations")
        .and_then(serde_json::Value::as_array);
    let duration_ms = response
        .get("duration_ms")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0_u64);

    // D5.1: When the daemon used shmem for large result sets, read from
    // the shmem file instead of the (empty) inline `rows` array.
    let shmem_rows: Option<Vec<serde_json::Value>> = response
        .get("shmem_path")
        .and_then(serde_json::Value::as_str)
        .map(|path_str| {
            let shmem_path = std::path::Path::new(path_str);
            let shmem_resp = uffs_client::shmem::read_search_results(shmem_path)
                .with_context(|| format!("Failed to read shmem results from {path_str}"))
                .ok();
            // Best-effort cleanup of the shmem file.
            let _ignored = std::fs::remove_file(shmem_path);
            shmem_resp
                .map(|resp| {
                    resp.rows
                        .iter()
                        .filter_map(|row| serde_json::to_value(row).ok())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        });

    let inline_rows = response.get("rows").and_then(serde_json::Value::as_array);
    let rows: Option<&[serde_json::Value]> = shmem_rows
        .as_deref()
        .or_else(|| inline_rows.map(Vec::as_slice));

    // Path-only single-buffer fast path.  When the daemon decides the
    // projection is path-only and the row count is small enough to
    // inline, it packs every path into `paths_blob` as a newline-
    // terminated UTF-8 buffer.  Writing it to stdout is then a single
    // syscall instead of N per-row format + write calls.
    let paths_blob: Option<&str> = response
        .get("paths_blob")
        .and_then(serde_json::Value::as_str);

    let profile = args
        .iter()
        .any(|arg| arg == "--profile" || arg == "--benchmark");
    if profile {
        #[expect(
            clippy::print_stderr,
            reason = "intentional --profile output to stderr"
        )]
        {
            eprintln!("=== PROFILE: Client → Daemon ===");
            eprintln!("  Connect:         {connect_ms:>6} ms");
            eprintln!("  Await ready:     {ready_ms:>6} ms");
            eprintln!("  Search (IPC):    {ipc_ms:>6} ms  (daemon: {duration_ms} ms)");
            let row_count = paths_blob.map_or_else(
                || rows.map_or(0, <[serde_json::Value]>::len),
                |blob| blob.bytes().filter(|byte| *byte == b'\n').count(),
            );
            eprintln!("  Rows returned:   {row_count:>6}");
            if paths_blob.is_some() {
                eprintln!("  Transport:       paths_blob (single write_all)");
            }
        }
    }

    // OPT-4: When --out is specified, the daemon writes the file directly
    // and returns an empty `rows` array.  Don't overwrite the file.
    // Handles both `--out foo.csv` (separate arg) and `--out=foo.csv` (= form).
    let has_out = args
        .iter()
        .any(|arg| arg == "--out" || arg.starts_with("--out="));
    let daemon_wrote_file =
        has_out && rows.is_none_or(<[serde_json::Value]>::is_empty) && paths_blob.is_none();

    if !daemon_wrote_file {
        if let Some(blob) = paths_blob {
            // Single write_all to stdout — the whole point of the
            // paths_blob transport.  A `BufWriter` is unnecessary: the
            // buffer is already one contiguous slice.
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            std::io::Write::write_all(&mut handle, blob.as_bytes())
                .with_context(|| "Failed to write paths_blob to stdout")?;
        } else if let Some(row_slice) = rows {
            commands::search::dispatch::write_rows(row_slice, args)?;
        }
    }

    // Output aggregations if present.
    if let Some(agg_arr) = aggregations.filter(|arr| !arr.is_empty()) {
        commands::search::dispatch::write_aggregations(agg_arr, args)?;
    }

    Ok(())
}

/// Extract daemon-spawn-relevant flags from raw CLI args.
///
/// The daemon auto-start needs `--data-dir`, `--mft-file`, `--no-cache`,
/// `--drive`, and log env vars. Everything else is irrelevant for spawn.
fn extract_spawn_args(args: &[String]) -> Vec<String> {
    let mut spawn = Vec::new();
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        let flag = arg.split('=').next().unwrap_or(arg.as_str());
        match flag {
            "--data-dir" | "--mft-file" | "--drive" | "--drives" | "--log-level" | "--log-file" => {
                spawn.push(arg.clone());
                // If not `--flag=val` form, consume the next token as value.
                if !arg.contains('=')
                    && iter.peek().is_some_and(|peeked| {
                        !peeked.starts_with('-') || flag == "--drive" || flag == "--drives"
                    })
                {
                    // peek() confirmed the value exists, so next() is safe.
                    spawn.push(iter.next().map_or_else(String::new, String::clone));
                }
            }
            "--no-cache" => spawn.push(arg.clone()),
            _ => {}
        }
    }

    // Forward log env vars.
    if let Ok(ll) = std::env::var("UFFS_LOG") {
        spawn.push("--log-level".to_owned());
        spawn.push(ll);
    }
    if let Ok(lf) = std::env::var("UFFS_LOG_FILE") {
        spawn.push("--log-file".to_owned());
        spawn.push(lf);
    }

    spawn
}

/// Resolve a relative `--out` path to absolute using the CLI's working
/// directory.
///
/// The daemon runs in a different working directory, so relative paths in
/// `--out` or `--out=<path>` would resolve against the wrong directory if
/// passed through as-is.
fn resolve_out_path(args: &[String]) -> Vec<String> {
    let mut result: Vec<String> = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(val) = arg.strip_prefix("--out=") {
            // `--out=path` form — resolve inline value.
            let resolved = resolve_to_absolute(val);
            result.push(format!("--out={resolved}"));
        } else if arg == "--out" {
            result.push(arg.clone());
            // Next token is the path value.
            if let Some(val) = iter.next() {
                result.push(resolve_to_absolute(val));
            }
        } else {
            result.push(arg.clone());
        }
    }
    result
}

/// Resolve a potentially relative path to absolute using `current_dir`.
fn resolve_to_absolute(path_str: &str) -> String {
    let path = std::path::Path::new(path_str);
    if path.is_absolute() {
        return path_str.to_owned();
    }
    std::env::current_dir()
        .unwrap_or_default()
        .join(path)
        .to_string_lossy()
        .into_owned()
}

/// Handle `uffs stats [path] [--top N] [--data-dir ...] [--mft-file ...]`.
fn run_stats(args: &[String]) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        args::print_stats_help();
        return Ok(());
    }
    // Simple arg extraction for stats subcommand.
    let mut path: Option<std::path::PathBuf> = None;
    let mut top: u32 = 10;
    let mut data_dir: Option<std::path::PathBuf> = None;
    let mut mft_file: Vec<std::path::PathBuf> = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--top" => {
                if let Some(val) = iter.next() {
                    top = val
                        .parse()
                        .map_err(|err| anyhow::anyhow!("Bad --top: {err}"))?;
                }
            }
            "--data-dir" => {
                if let Some(val) = iter.next() {
                    data_dir = Some(val.into());
                }
            }
            "--mft-file" => {
                if let Some(val) = iter.next() {
                    mft_file = val.split(',').map(|part| part.trim().into()).collect();
                }
            }
            other if !other.starts_with('-') && path.is_none() => {
                path = Some(other.into());
            }
            _ => {}
        }
    }

    if let Some(stats_path) = path {
        commands::stats::stats(Some(&stats_path), top)?;
    } else {
        // Synthesise search args for an aggregate-only overview query.
        let mut synth_args = vec![
            "*".to_owned(),
            "--agg".to_owned(),
            "overview".to_owned(),
            "--format".to_owned(),
            "table".to_owned(),
            "--limit".to_owned(),
            "0".to_owned(),
        ];
        if let Some(dir) = data_dir {
            synth_args.extend(["--data-dir".to_owned(), dir.to_string_lossy().into_owned()]);
        }
        for mf in &mft_file {
            synth_args.extend(["--mft-file".to_owned(), mf.to_string_lossy().into_owned()]);
        }
        run_search(&synth_args)?;
    }
    Ok(())
}

/// Handle `uffs aggregate|agg <preset> [--format ...] [--data-dir ...]`.
fn run_aggregate(args: &[String]) -> Result<()> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        args::print_aggregate_help();
        return Ok(());
    }
    // Extract the preset (first positional arg).
    let preset = args
        .iter()
        .find(|arg| !arg.starts_with('-'))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Usage: uffs aggregate <PRESET>\n\
                 Available presets: overview, by_type, by_extension, by_drive, by_size, by_age, count"
            )
        })?;

    // Synthesise search args: `* --agg <preset> --limit 0 [remaining flags]`.
    let mut synth_args = vec![
        "*".to_owned(),
        "--agg".to_owned(),
        preset.clone(),
        "--limit".to_owned(),
        "0".to_owned(),
    ];
    // Default to table format for `uffs agg` unless user specifies --format.
    let has_format = args.iter().any(|arg| arg == "--format" || arg == "-f");
    if !has_format {
        synth_args.extend(["--format".to_owned(), "table".to_owned()]);
    }
    // Forward all flags (skip the preset positional).
    for arg in args {
        if arg == preset {
            continue;
        }
        synth_args.push(arg.clone());
    }
    run_search(&synth_args)
}

/// Handle `uffs daemon <action> [flags...]`.
fn run_daemon(args: &[String]) -> Result<()> {
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        args::print_daemon_help();
        return Ok(());
    }
    let action = args::parse_daemon_action(args)?;
    commands::daemon_mgmt::daemon(&action)
}

/// Entry point — synchronous, no runtime.
#[expect(
    clippy::print_stderr,
    reason = "intentional user-facing error output to stderr"
)]
fn main() {
    if let Err(err) = run() {
        for (idx, cause) in err.chain().enumerate() {
            if idx == 0 {
                eprintln!("Error: {cause}");
            } else {
                eprintln!("  Caused by: {cause}");
            }
        }
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::default_numeric_fallback,
        reason = "test module — relaxed linting"
    )]

    use uffs_client::protocol::SearchParams;

    use super::args::parse_drive_letter;

    #[test]
    fn test_parse_drive_letter_accepts_letter_colon_and_whitespace_variants() {
        assert_eq!(parse_drive_letter("c"), Ok('C'));
        assert_eq!(parse_drive_letter("C:"), Ok('C'));
        assert_eq!(parse_drive_letter(" d: "), Ok('D'));
    }

    #[test]
    fn test_parse_drive_letter_rejects_invalid_values() {
        parse_drive_letter("").unwrap_err();
        parse_drive_letter("12").unwrap_err();
        parse_drive_letter("1:").unwrap_err();
        parse_drive_letter("CD").unwrap_err();
    }

    #[test]
    fn test_from_cli_args_basic_search() {
        let args: Vec<String> = [
            "*.rs",
            "--drive",
            "C",
            "--format",
            "json",
            "--tz-offset",
            "-8",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        let params = SearchParams::from_cli_args(&args).expect("should parse");
        assert_eq!(params.pattern, "*.rs");
        assert_eq!(params.drives, vec!['C']);
        assert_eq!(params.output_tz_offset_hours, Some(-8));
    }

    #[test]
    fn test_from_cli_args_sugar_begins_with() {
        let args: Vec<String> = ["--begins-with", "report"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let params = SearchParams::from_cli_args(&args).expect("should parse");
        assert_eq!(params.pattern, "report*");
    }

    #[test]
    fn test_from_cli_args_sugar_between() {
        let args: Vec<String> = ["*", "--between", "2026-01-01,2026-03-31"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let params = SearchParams::from_cli_args(&args).expect("should parse");
        assert_eq!(params.newer.as_deref(), Some("2026-01-01"));
        assert_eq!(params.older.as_deref(), Some("2026-03-31"));
    }
}
