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

/// Timing + payload summary forwarded to [`print_client_profile`].
///
/// Packaging these into a struct keeps `run_search` under the
/// `clippy::too-many-lines` cap and lets the profile helper take one
/// argument instead of six.
struct ClientProfile<'a> {
    /// Wall-clock time spent in `UffsClientSync::connect_with_args`.
    connect_ms: u128,
    /// Wall-clock time spent in `await_ready` (daemon warm-up).
    ready_ms: u128,
    /// Wall-clock time spent in the `search_cli` IPC round-trip.
    ipc_ms: u128,
    /// Daemon-reported search duration (from the response envelope).
    duration_ms: u64,
    /// Inline `rows` slice from the response (if any).
    rows: Option<&'a [serde_json::Value]>,
    /// Pre-packed `paths_blob` (if the daemon used the path-only
    /// single-buffer fast path).
    paths_blob: Option<&'a str>,
}

/// Print the `--profile` / `--benchmark` client-side timing block to
/// stderr (matches the daemon-side profile formatting).
#[expect(
    clippy::print_stderr,
    reason = "intentional --profile output to stderr"
)]
fn print_client_profile(prof: &ClientProfile<'_>) {
    eprintln!("=== PROFILE: Client → Daemon ===");
    eprintln!("  Connect:         {:>6} ms", prof.connect_ms);
    eprintln!("  Await ready:     {:>6} ms", prof.ready_ms);
    eprintln!(
        "  Search (IPC):    {:>6} ms  (daemon: {} ms)",
        prof.ipc_ms, prof.duration_ms
    );
    let row_count = prof.paths_blob.map_or_else(
        || prof.rows.map_or(0, <[serde_json::Value]>::len),
        |blob| blob.bytes().filter(|byte| *byte == b'\n').count(),
    );
    eprintln!("  Rows returned:   {row_count:>6}");
    if prof.paths_blob.is_some() {
        eprintln!("  Transport:       paths_blob (single write_all)");
    }
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
    // Phase 3.1 NUL fast path: when stdout is redirected to the null
    // device (e.g. `uffs *.dll > NUL`), inject `--no-output` so the
    // daemon skips row materialisation + `paths_blob` construction
    // + IPC row transfer entirely.  Saves ~20-30 ms on medium result
    // sets that would otherwise push 3.5 MB through the pipe just to
    // discard the bytes client-side.
    let args_owned: Vec<String> = inject_no_output_for_null_stdout(resolve_out_path(args));
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

    if args
        .iter()
        .any(|arg| arg == "--profile" || arg == "--benchmark")
    {
        print_client_profile(&ClientProfile {
            connect_ms,
            ready_ms,
            ipc_ms,
            duration_ms,
            rows,
            paths_blob,
        });
    }

    // OPT-4: When --out is specified, the daemon writes the file directly
    // and returns an empty `rows` array.  Don't overwrite the file.
    // Handles both `--out foo.csv` (separate arg) and `--out=foo.csv` (= form).
    let has_out = args
        .iter()
        .any(|arg| arg == "--out" || arg.starts_with("--out="));
    let daemon_wrote_file =
        has_out && rows.is_none_or(<[serde_json::Value]>::is_empty) && paths_blob.is_none();

    // Phase 3.1 NUL fast path: `--no-output` (explicit or auto-injected
    // for NUL stdout) skips every client-side stdout write.
    let suppress_stdout = args_owned.iter().any(|arg| arg == "--no-output");

    if !daemon_wrote_file && !suppress_stdout {
        if let Some(blob) = paths_blob {
            // Single write_all to stdout — the whole point of the
            // paths_blob transport; the buffer is one contiguous slice.
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            std::io::Write::write_all(&mut handle, blob.as_bytes())
                .with_context(|| "Failed to write paths_blob to stdout")?;
        } else if let Some(row_slice) = rows {
            commands::search::dispatch::write_rows(row_slice, args)?;
        }
    }

    if !suppress_stdout && let Some(agg_arr) = aggregations.filter(|arr| !arr.is_empty()) {
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

/// Append `--no-output` to `args` when stdout is redirected to the
/// null device, unless a disqualifying flag is already set.
///
/// Thin wrapper around [`maybe_inject_no_output`] that probes the real
/// stdout via [`uffs_client::stdout_kind::StdoutKind::detect`].  The
/// decision logic itself is in `maybe_inject_no_output` so it can be
/// unit-tested without fighting the test harness's stdout wiring.
fn inject_no_output_for_null_stdout(args: Vec<String>) -> Vec<String> {
    let stdout_is_null = uffs_client::stdout_kind::StdoutKind::detect().is_null();
    maybe_inject_no_output(args, stdout_is_null)
}

/// Pure decision logic for the NUL fast-path injection.
///
/// Returns `args` unchanged when `stdout_is_null == false` or when any
/// disqualifying flag is already present:
///
/// - `--no-output` already set: nothing to add.
/// - `--rows`: the user asked to force rows on even for aggregate queries —
///   honour that intent regardless of where stdout goes.
/// - `--out`: stdout is not the result destination; NUL on stdout is a benign
///   quirk, not the output target.
/// - `--agg` / `--facet` / `--stats` / `--histogram` / `--count`: any
///   aggregation flag already controls `include_rows` via its own sugar; adding
///   `--no-output` would be redundant at best.
fn maybe_inject_no_output(mut args: Vec<String>, stdout_is_null: bool) -> Vec<String> {
    if !stdout_is_null {
        return args;
    }
    let is_aggregate_flag = |flag: &str| {
        matches!(
            flag,
            "--agg" | "--facet" | "--stats" | "--histogram" | "--count"
        )
    };
    let disqualified = args.iter().any(|raw| {
        let flag = raw.split('=').next().unwrap_or(raw.as_str());
        flag == "--no-output" || flag == "--rows" || flag == "--out" || is_aggregate_flag(flag)
    });
    if disqualified {
        return args;
    }
    args.push("--no-output".to_owned());
    args
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
        // Special-case DaemonNeedsElevation: render a multi-option help
        // message instead of the generic `Error: ... Caused by: ...`
        // chain, so a UAC failure reads like advice and not a crash.
        if let Some(needs) = find_needs_elevation(&err) {
            eprintln!("{}", format_elevation_help(needs));
            std::process::exit(1);
        }

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

/// Walk an [`anyhow::Error`] chain looking for
/// [`uffs_client::error::ClientError::DaemonNeedsElevation`].
///
/// Returns the daemon path that would have been spawned, so the
/// formatter can quote it back to the user verbatim.  Returns `None`
/// if no elevation error is present in the chain.
fn find_needs_elevation(err: &anyhow::Error) -> Option<&str> {
    for cause in err.chain() {
        if let Some(uffs_client::error::ClientError::DaemonNeedsElevation { daemon_path }) =
            cause.downcast_ref::<uffs_client::error::ClientError>()
        {
            return Some(daemon_path.as_str());
        }
    }
    None
}

/// Render the "daemon needs admin" help message.
///
/// Lists three independent recovery paths so users can pick whichever
/// fits their workflow — scripted, interactive one-off, or permanent.
fn format_elevation_help(daemon_path: &str) -> String {
    format!(
        "Error: UFFS daemon needs admin privileges to read NTFS Master File Tables.\n\
         \n\
         The daemon is not running, and this shell is not elevated.  To start it, pick one:\n\
         \n  \
         1. Relaunch in an elevated shell (PowerShell/cmd \"Run as administrator\"),\n     \
            then retry the command.\n\
         \n  \
         2. Explicitly request a UAC prompt for this invocation:\n       \
               uffs daemon start --elevate\n     \
            Or set it as the default for the current session:\n       \
               set UFFS_ELEVATE=1     (cmd)\n       \
               $env:UFFS_ELEVATE = '1'  (PowerShell)\n\
         \n  \
         3. Install the broker service — one-time setup, no future UAC prompts:\n       \
               uffs-broker --install\n\
         \n\
         Daemon binary that would have been spawned:\n  \
           {daemon_path}"
    )
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::default_numeric_fallback,
        reason = "test module — relaxed linting"
    )]

    use uffs_client::protocol::SearchParams;

    use super::args::parse_drive_letter;
    use super::{find_needs_elevation, format_elevation_help, maybe_inject_no_output};

    // ── maybe_inject_no_output (Phase 3.1 NUL fast path) ─────────

    /// Helper: run [`maybe_inject_no_output`] with `stdout_is_null = true`
    /// and return the resulting args.
    fn inject_null(args: &[&str]) -> Vec<String> {
        let owned: Vec<String> = args.iter().copied().map(String::from).collect();
        maybe_inject_no_output(owned, true)
    }

    /// Baseline: a plain search with NUL stdout gets `--no-output`
    /// appended.  This is the hot path we want for benchmarks and
    /// `uffs *.dll > NUL`-style invocations.
    #[test]
    fn maybe_inject_no_output_appends_on_null_stdout() {
        let out = inject_null(&["*.dll", "--drive", "D"]);
        assert_eq!(out, ["*.dll", "--drive", "D", "--no-output"]);
    }

    /// Non-null stdout (terminal, pipe, file) must leave args alone,
    /// otherwise the user would see no output on their terminal.
    #[test]
    fn maybe_inject_no_output_unchanged_on_non_null_stdout() {
        let owned: Vec<String> = ["*.dll", "--drive", "D"]
            .into_iter()
            .map(String::from)
            .collect();
        let out = maybe_inject_no_output(owned.clone(), false);
        assert_eq!(out, owned);
    }

    /// Explicit `--no-output` already present: do not double up.
    #[test]
    fn maybe_inject_no_output_skips_when_already_present() {
        let out = inject_null(&["*.dll", "--no-output"]);
        // Exactly one occurrence — no double-injection.
        let count = out
            .iter()
            .filter(|arg| arg.as_str() == "--no-output")
            .count();
        assert_eq!(count, 1, "must not double-inject --no-output, got: {out:?}");
    }

    /// `--rows` forces rows on — auto-injection must not fight it.
    /// Covers the user who wants `uffs *.rs --rows > NUL` for timing
    /// the full round-trip including IPC transport.
    #[test]
    fn maybe_inject_no_output_respects_rows_flag() {
        let out = inject_null(&["*.rs", "--rows"]);
        assert!(
            !out.iter().any(|arg| arg == "--no-output"),
            "--rows must prevent --no-output auto-injection, got: {out:?}"
        );
    }

    /// `--out file` routes results daemon-direct to disk — NUL on
    /// stdout is a benign quirk, not the output destination.
    #[test]
    fn maybe_inject_no_output_respects_out_flag() {
        let out = inject_null(&["*.rs", "--out", "results.csv"]);
        assert!(
            !out.iter().any(|arg| arg == "--no-output"),
            "--out must prevent --no-output auto-injection, got: {out:?}"
        );
    }

    /// Aggregation flags already control row inclusion through their
    /// own sugar; auto-injection would be redundant.  Covers `--count`,
    /// `--agg`, `--facet`, `--stats`, `--histogram`.
    #[test]
    fn maybe_inject_no_output_respects_aggregation_flags() {
        for flag in ["--count", "--agg", "--facet", "--stats", "--histogram"] {
            // `--agg` and friends take a value; the parse-time check
            // keys on the flag name only, so a bare `--agg count` suffix
            // is sufficient to prove the disqualification.
            let args: Vec<&str> = if flag == "--count" {
                vec!["*", flag]
            } else {
                vec!["*", flag, "count"]
            };
            let out = inject_null(&args);
            assert!(
                !out.iter().any(|arg| arg == "--no-output"),
                "{flag} must prevent --no-output auto-injection, got: {out:?}"
            );
        }
    }

    /// `--out=file.csv` (equals form) must also disqualify the
    /// auto-injection.  The parser keys on the flag name before `=`.
    #[test]
    fn maybe_inject_no_output_respects_out_equals_form() {
        let out = inject_null(&["*", "--out=results.csv"]);
        assert!(
            !out.iter().any(|arg| arg == "--no-output"),
            "--out=<path> must prevent --no-output auto-injection, got: {out:?}"
        );
    }

    /// The elevation help must name every recovery path the user has,
    /// so a UAC-blocked invocation becomes actionable advice rather
    /// than a dead-end crash.  Locks the contract in place.
    #[test]
    fn elevation_help_lists_all_recovery_paths() {
        let help = format_elevation_help(r"C:\Program Files\uffs\uffsd.exe");
        assert!(help.contains("admin"), "help must mention admin: {help}");
        assert!(
            help.contains("--elevate"),
            "help must document --elevate: {help}"
        );
        assert!(
            help.contains("UFFS_ELEVATE"),
            "help must document the env var: {help}"
        );
        assert!(
            help.contains("uffs-broker --install"),
            "help must document the broker install path: {help}"
        );
        assert!(
            help.contains(r"C:\Program Files\uffs\uffsd.exe"),
            "help must quote the daemon path: {help}"
        );
    }

    /// `find_needs_elevation` must walk through any `.with_context`
    /// layers that the CLI adds on top of the raw `ClientError`.
    #[test]
    fn find_needs_elevation_walks_anyhow_context() {
        let base = anyhow::Error::from(uffs_client::error::ClientError::DaemonNeedsElevation {
            daemon_path: "uffsd-test".to_owned(),
        });
        let wrapped: anyhow::Error = base.context("while connecting");
        assert_eq!(find_needs_elevation(&wrapped), Some("uffsd-test"));
    }

    /// Unrelated errors must not be mistaken for an elevation problem,
    /// so the default `Error: ... / Caused by:` chain is preserved for
    /// everything else.
    #[test]
    fn find_needs_elevation_returns_none_for_other_errors() {
        let other = anyhow::Error::from(uffs_client::error::ClientError::ConnectionFailed(
            "nope".to_owned(),
        ));
        assert!(find_needs_elevation(&other).is_none());
    }

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
        // `*.rs` is promoted to pattern="*" + ext=Some("rs") so the
        // daemon can route through the ExtensionIndex fast path in
        // `numeric_top_n::ext_fast_path` instead of the trigram + glob
        // path.  See `is_pure_ext_glob` in cli_args.rs for the shape
        // acceptance matrix and `test_from_cli_args_ext_glob_promoted`
        // in uffs-client for the full rewrite semantics.
        assert_eq!(params.pattern, "*");
        assert_eq!(params.ext.as_deref(), Some("rs"));
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
