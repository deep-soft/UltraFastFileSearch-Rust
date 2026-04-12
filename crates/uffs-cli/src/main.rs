//! UFFS (Ultra Fast File Search) CLI
//!
//! Fast file search from the command line.
//!
//! ## Usage
//!
//! Search is the default action (no subcommand needed):
//! ```bash
//! uffs *.txt              # Find all .txt files
//! uffs c:/pro*            # Find files starting with "pro" on C:
//! uffs --ext=rs,toml      # Find Rust files
//! ```
//!
//! ## Logging
//!
//! Use `-v` / `--verbose` for info-level terminal output:
//! ```bash
//! uffs -v *.txt
//! ```
//!
//! For finer control, use environment variables:
//! - `RUST_LOG`: Terminal log level (default: `error`, or `info` with `-v`)
//! - `RUST_LOG_FILE`: File log level (default: `info`)
//! - `UFFS_LOG_DIR`: Log directory (default: `~/bin/uffs/logs`)
//!
//! Examples:
//! ```bash
//! # Debug mode - verbose terminal output
//! RUST_LOG=debug uffs *.txt
//!
//! # Trace mode - maximum verbosity
//! RUST_LOG=trace RUST_LOG_FILE=trace uffs *.txt
//! ```

// CLI main module uses single-call functions by design
#![expect(
    clippy::single_call_fn,
    reason = "CLI entry point functions are called once from main"
)]

use std::io;
use std::path::PathBuf;

use anyhow::Result;
#[cfg(test)]
use assert_cmd as _;
use chrono as _;
use clap::Parser;
use mimalloc::MiMalloc;
use tracing_subscriber::fmt::time::UtcTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{EnvFilter, Layer};
use uffs_polars as _;

/// Use mimalloc globally - faster than system allocator for our workload:
/// many small allocations (file names, records) + large buffers (MFT,
/// `DataFrame`). Works well on Windows, macOS, and Linux without build
/// complexity.
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

mod args;
mod commands;

use args::{Cli, Commands};

/// Operation label used for CLI-wide shutdown classification.
const CLI_OPERATION: &str = "uffs";

/// Maps spawned CLI task failures onto the approved cancellation taxonomy.
#[must_use]
fn classify_cli_task_error(
    operation: &'static str,
    error: &tokio::task::JoinError,
) -> uffs_mft::MftError {
    if error.is_cancelled() {
        return uffs_mft::MftError::Cancelled {
            operation,
            reason: error.to_string(),
        };
    }

    uffs_mft::MftError::WaitFailed {
        operation,
        reason: error.to_string(),
    }
}

/// Builds the explicit cancellation outcome for a Ctrl+C shutdown request.
#[must_use]
fn shutdown_requested_error(operation: &'static str) -> uffs_mft::MftError {
    uffs_mft::MftError::Cancelled {
        operation,
        reason: "shutdown requested by Ctrl+C".to_owned(),
    }
}

/// Builds a wait failure when the CLI cannot install a Ctrl+C listener.
#[must_use]
fn ctrl_c_listener_error(operation: &'static str, error: &io::Error) -> uffs_mft::MftError {
    uffs_mft::MftError::WaitFailed {
        operation,
        reason: format!("failed to listen for Ctrl+C: {error}"),
    }
}
/// Initialize logging with terminal + file support.
///
/// If `verbose` is true and `RUST_LOG` is not set, uses `info` level for
/// terminal. Otherwise, terminal logging is controlled by `RUST_LOG` (default:
/// `error`). File logging is controlled by `RUST_LOG_FILE` (default: `info`).
/// Log directory is controlled by `UFFS_LOG_DIR` (default: `~/bin/rust`).
///
/// Returns a guard that must be kept alive for the duration of the program.
///
/// # Panics
///
/// Panics if the global tracing subscriber cannot be set (should only happen
/// if called more than once).
// Extracted for clarity and maintainability - logging setup is complex enough
// to warrant its own function even if only called once.
#[expect(
    clippy::single_call_fn,
    reason = "extracted for clarity — logging setup is complex enough to warrant its own function"
)]
fn init_logging(verbose: bool) -> tracing_appender::non_blocking::WorkerGuard {
    use std::fs;

    use tracing_appender::non_blocking::NonBlocking;
    use tracing_appender::rolling::{RollingFileAppender, Rotation};
    use tracing_subscriber::registry::Registry;

    // Get log directory (default: ~/bin/uffs/logs)
    let log_dir = std::env::var("UFFS_LOG_DIR").map_or_else(
        |_| {
            dirs_next::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("bin")
                .join("uffs")
                .join("logs")
        },
        PathBuf::from,
    );

    // Create log directory if it doesn't exist (ignore errors - logging will fail
    // gracefully)
    drop(fs::create_dir_all(&log_dir));

    // Create rolling file appender (daily rotation).
    // Use the builder API which returns Result instead of panicking, and retry
    // briefly to handle transient Windows file-lock races (e.g. previous daemon
    // process still releasing the log file handle).
    let max_attempts = 4_u32;
    let mut file_log_err: Option<String> = None;
    let mut file_log_attempt = 0_u32;
    let (non_blocking, guard): (NonBlocking, _) = {
        let mut last_err = None;
        let mut appender = None;
        for attempt in 0..max_attempts {
            if attempt > 0 {
                std::thread::sleep(core::time::Duration::from_millis(250));
            }
            match RollingFileAppender::builder()
                .rotation(Rotation::DAILY)
                .filename_prefix("uffs_log_")
                .build(&log_dir)
            {
                Ok(file_appender) => {
                    file_log_attempt = attempt;
                    appender = Some(file_appender);
                    break;
                }
                Err(init_err) => last_err = Some(init_err),
            }
        }
        appender.map_or_else(
            || {
                file_log_err = Some(
                    last_err
                        .as_ref()
                        .map_or_else(|| "unknown error".to_owned(), ToString::to_string),
                );
                NonBlocking::new(io::sink())
            },
            NonBlocking::new,
        )
    };

    // Terminal filter: -v sets info if RUST_LOG not explicitly set
    let terminal_default = if verbose { "info" } else { "error" };
    let terminal_filter =
        EnvFilter::new(std::env::var("RUST_LOG").unwrap_or_else(|_| terminal_default.to_owned()));

    // File filter (default: info - more verbose for debugging)
    let file_filter =
        EnvFilter::new(std::env::var("RUST_LOG_FILE").unwrap_or_else(|_| "info".to_owned()));

    // Timer format
    let timer = UtcTime::rfc_3339();

    // Terminal layer (to stderr to avoid corrupting CSV output, with ANSI colors,
    // file/line info, thread IDs)
    let terminal_layer = tracing_subscriber::fmt::layer()
        .with_writer(io::stderr)
        .with_timer(timer.clone())
        .with_ansi(true)
        .with_file(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .with_target(true)
        .with_filter(terminal_filter);

    // File layer (no ANSI colors, but with full context)
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_timer(timer)
        .with_ansi(false)
        .with_file(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .with_target(true)
        .with_filter(file_filter);

    // Combine layers
    let subscriber = Registry::default().with(terminal_layer).with(file_layer);

    // This should only be called once at program startup
    #[expect(
        clippy::expect_used,
        reason = "global subscriber set once at startup; panic is intentional if called twice"
    )]
    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set global tracing subscriber - was init_logging called twice?");

    // Post-init diagnostics: surface file-appender issues through tracing now
    // that the subscriber is active. This ensures problems are visible in both
    // the terminal and (if the file appender recovered) the log file.
    if let Some(err_msg) = &file_log_err {
        tracing::error!(
            log_dir = %log_dir.display(),
            attempts = max_attempts,
            error = %err_msg,
            "File logging DISABLED — log file could not be opened after all retries. \
             All tracing output is terminal-only for this session."
        );
    } else if file_log_attempt > 0 {
        tracing::warn!(
            log_dir = %log_dir.display(),
            retries = file_log_attempt,
            "Log file opened after {file_log_attempt} retries — \
             previous process may have been slow to release the file handle"
        );
    }

    guard
}

/// Run the CLI and return a result.
///
/// This is separated from `main()` to allow custom error handling that
/// doesn't show backtraces for user-facing errors like "file not found".
#[tracing::instrument(level = "info", skip_all)]
async fn run() -> Result<()> {
    let cli = Cli::parse();

    // `uffs daemon run` manages its own tracing subscriber so it can
    // honour --log-level / --log-file.  Skip the CLI's init_logging for
    // that subcommand — otherwise `try_init` in `daemon_run` silently
    // fails because the global subscriber is already installed.
    let is_daemon_run = matches!(
        &cli.command,
        Some(Commands::Daemon {
            action: args::DaemonAction::Run { .. }
        })
    );
    let is_mcp_run = matches!(
        &cli.command,
        Some(Commands::Mcp {
            action: args::McpAction::Run { .. } | args::McpAction::Serve { .. }
        })
    );
    let _guard = if is_daemon_run || is_mcp_run {
        None
    } else {
        let verbose = std::env::args().any(|arg| arg == "-v" || arg == "--verbose");
        Some(init_logging(verbose))
    };

    // Handle subcommands or default search action
    match cli.command {
        Some(Commands::Index {
            output,
            drive,
            drives,
        }) => {
            commands::index(output, drive, drives).await?;
        }
        Some(Commands::Info { path }) => {
            commands::info(&path)?;
        }
        Some(Commands::Stats {
            path,
            top,
            data_dir,
            mft_file,
        }) => {
            if let Some(stats_path) = path {
                commands::stats(Some(&stats_path), top).await?;
            } else {
                // Daemon mode: run overview preset through search path.
                let config = commands::search::SearchConfig::aggregate_only(
                    "*",
                    vec!["overview".to_owned()],
                    "table",
                    data_dir,
                    mft_file,
                    None,
                    None,
                );
                commands::search::run_with_config(&config).await?;
            }
        }
        Some(Commands::Aggregate {
            preset,
            format,
            data_dir,
            mft_file,
            agg_cursor,
            agg_page_size,
        }) => {
            // Route through the standard search path — aggregate-only,
            // no rows returned.  The search daemon lifecycle (auto-start,
            // await_ready, data-dir forwarding) is handled automatically.
            // Pass the preset name directly — `build_search_params`
            // detects known preset names via `AggregatePreset::parse`
            // and sets `kind: "preset"` + `preset: Some(name)` on the
            // wire automatically.
            let agg_spec = preset;
            let config = commands::search::SearchConfig::aggregate_only(
                "*",
                vec![agg_spec],
                &format,
                data_dir,
                mft_file,
                agg_cursor,
                agg_page_size,
            );
            commands::search::run_with_config(&config).await?;
        }
        Some(Commands::Daemon { action }) => {
            commands::daemon(&action).await?;
        }
        Some(Commands::Mcp { action }) => {
            commands::mcp(&action).await?;
        }
        Some(Commands::SystemStatus) => {
            commands::system_status().await?;
        }
        None => {
            run_search(cli).await?;
        }
    }

    Ok(())
}

/// Handle the default search subcommand.
///
/// Extracted from `run()` to stay under the `too_many_lines` lint limit.
async fn run_search(mut cli: Cli) -> Result<()> {
    // Synthesise pattern from --begins-with / --ends-with / --contains (take to
    // avoid partial move)
    let raw_pattern = cli
        .pattern
        .take()
        .or_else(|| cli.begins_with.take().map(|prefix| format!("{prefix}*")))
        .or_else(|| cli.ends_with.take().map(|suffix| format!("*{suffix}")))
        .or_else(|| cli.contains.take().map(|needle| format!("*{needle}*")));

    // Merge --not-contains into --exclude (take to avoid partial move of `cli`)
    let exclude = match (cli.exclude.take(), cli.not_contains.take()) {
        (Some(ex), Some(nc)) => Some(format!("{ex},*{nc}*")),
        (Some(ex), None) => Some(ex),
        (None, Some(nc)) => Some(format!("*{nc}*")),
        (None, None) => None,
    };

    if let Some(resolved) = raw_pattern {
        let (match_path, pattern) = parse_scope_prefix(resolved, &mut cli);
        validate_name_only(&cli, &pattern)?;
        dispatch_search(cli, &pattern, exclude.as_deref(), match_path).await?;
    } else {
        use clap::CommandFactory;
        Cli::command().print_help()?;
    }

    Ok(())
}

/// Parse Everything-style scope prefixes (`path:`, `dir:`, `file:`) from the
/// resolved pattern and update the CLI flags accordingly.
fn parse_scope_prefix(resolved: String, cli: &mut Cli) -> (bool, String) {
    if let Some(rest) = resolved.strip_prefix("path:") {
        (true, rest.to_owned())
    } else if let Some(rest) = resolved.strip_prefix("dir:") {
        cli.dirs_only = true;
        (false, rest.to_owned())
    } else if let Some(rest) = resolved.strip_prefix("file:") {
        cli.files_only = true;
        (false, rest.to_owned())
    } else {
        (false, resolved)
    }
}

/// Validate `--name-only`: incompatible with patterns containing path
/// separators.
fn validate_name_only(cli: &Cli, pattern: &str) -> Result<()> {
    if cli.name_only
        && (pattern.contains('\\') || pattern.contains('/'))
        && !pattern.starts_with('>')
    {
        anyhow::bail!(
            "--name-only cannot be used with path patterns (pattern contains '\\' or '/'). \
             Remove the path from the pattern or drop --name-only."
        );
    }
    Ok(())
}

/// Expand filter aliases and dispatch to the actual search command.
async fn dispatch_search(
    cli: Cli,
    pattern: &str,
    exclude: Option<&str>,
    match_path: bool,
) -> Result<()> {
    // Expand collection aliases / presets
    let ext_expanded = cli
        .ext
        .as_deref()
        .map(uffs_core::extensions::expand_ext_spec);
    let attr_expanded = cli
        .attr
        .as_deref()
        .map(uffs_core::search::filters::expand_attr_spec);
    let month_expanded: Vec<u32> = cli
        .month
        .as_deref()
        .map(uffs_core::search::filters::parse_month_spec)
        .unwrap_or_default();

    // Merge --exact-size / --exact-descendants
    let (min_size, max_size) = (
        cli.exact_size.or(cli.min_size),
        cli.exact_size.or(cli.max_size),
    );
    let (min_desc, max_desc) = (
        cli.exact_descendants.or(cli.min_descendants),
        cli.exact_descendants.or(cli.max_descendants),
    );

    // Merge --between START,END → newer + older
    let (bn, bo) = cli.between.as_ref().map_or((None, None), |between| {
        let mut parts = between.splitn(2, ',');
        (
            parts.next().map(String::from),
            parts.next().map(String::from),
        )
    });
    let (newer, older) = (
        cli.newer.as_deref().or(bn.as_deref()),
        cli.older.as_deref().or(bo.as_deref()),
    );

    commands::search(
        pattern,
        cli.drive,
        cli.drives,
        cli.mft_file,
        cli.data_dir,
        cli.files_only,
        cli.dirs_only,
        cli.hide_system,
        cli.hide_ads,
        cli.profile,
        cli.benchmark,
        cli.no_cache,
        min_size,
        max_size,
        min_desc,
        max_desc,
        cli.limit,
        &cli.format,
        cli.case,
        cli.smart_case,
        attr_expanded.as_deref(),
        newer,
        older,
        cli.newer_created.as_deref(),
        cli.older_created.as_deref(),
        cli.newer_accessed.as_deref(),
        cli.older_accessed.as_deref(),
        exclude,
        cli.in_path.as_deref(),
        cli.type_filter.as_deref(),
        cli.min_bulkiness,
        cli.max_bulkiness,
        cli.min_name_length,
        cli.max_name_length,
        cli.min_path_length,
        cli.max_path_length,
        cli.exact_size_on_disk.or(cli.min_size_on_disk),
        cli.exact_size_on_disk.or(cli.max_size_on_disk),
        cli.min_treesize,
        cli.max_treesize,
        cli.min_tree_allocated,
        cli.max_tree_allocated,
        &month_expanded,
        match_path,
        cli.word,
        cli.sort.as_deref(),
        cli.sort_desc,
        ext_expanded.as_deref(),
        &cli.out,
        if cli.parity_compat {
            "parity"
        } else {
            &cli.columns
        },
        &cli.sep,
        &cli.quotes,
        cli.header,
        &cli.pos,
        &cli.neg,
        cli.tz_offset,
        {
            let mut agg = cli.agg;
            if cli.count && !agg.iter().any(|item| item == "count") {
                agg.push("count".to_owned());
            }
            for facet in &cli.facet {
                if let Some((field, top)) = facet.split_once(':') {
                    agg.push(format!("terms:{field},top={top}"));
                } else {
                    agg.push(format!("terms:{facet},top=20"));
                }
            }
            for stat in &cli.stats {
                agg.push(format!("stats:{stat}"));
            }
            for hist in &cli.histogram {
                if let Some((field, interval)) = hist.split_once(':') {
                    agg.push(format!("hist:{field},interval={interval}"));
                } else {
                    agg.push(format!("hist:{hist}"));
                }
            }
            agg
        },
        cli.rows,
        cli.agg_cursor,
        cli.agg_page_size,
    )
    .await
}

/// Runs the CLI while listening for Ctrl+C so shutdown reaches long-running
/// command flows started from the binary entrypoint.
#[expect(
    clippy::single_call_fn,
    reason = "entrypoint wrapper exists solely to propagate shutdown into the spawned command task"
)]
#[tracing::instrument(level = "debug", skip_all, fields(operation = CLI_OPERATION))]
async fn run_until_shutdown() -> Result<()> {
    let mut run_task = tokio::spawn(run());

    tokio::select! {
        result = &mut run_task => {
            match result {
                Ok(outcome) => outcome,
                Err(error) => Err(classify_cli_task_error(CLI_OPERATION, &error).into()),
            }
        }
        signal = tokio::signal::ctrl_c() => {
            run_task.abort();

            match signal {
                Ok(()) => Err(shutdown_requested_error(CLI_OPERATION).into()),
                Err(error) => Err(ctrl_c_listener_error(CLI_OPERATION, &error).into()),
            }
        }
    }
}

#[tokio::main]
#[expect(
    clippy::print_stderr,
    reason = "intentional user-facing error output to stderr"
)]
async fn main() {
    if let Err(err) = run_until_shutdown().await {
        // Print error without backtrace for clean user-facing output
        // Use anyhow's chain() to iterate through the error chain
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

    use std::path::PathBuf;

    use clap::{CommandFactory, Parser};

    use super::args::{Cli, Commands, parse_drive_letter};
    use super::{classify_cli_task_error, ctrl_c_listener_error, shutdown_requested_error};

    fn render_long_help(mut command: clap::Command) -> String {
        let mut buffer = Vec::new();
        command
            .write_long_help(&mut buffer)
            .expect("CLI help should render successfully");
        String::from_utf8(buffer).expect("CLI help should be valid UTF-8")
    }

    fn parse_cli(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(args)
    }

    #[test]
    fn test_cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn test_top_level_help_includes_examples_and_default_search_guidance() {
        let help = render_long_help(Cli::command());

        assert!(help.contains("Search is the default action"));
        assert!(help.contains("uffs '*.txt'"));
        assert!(help.contains("uffs '>.*\\.log$' --drive C"));
        assert!(help.contains("uffs '*' --mft-file G_mft.bin --drive G"));
        assert!(help.contains("uffs index -d C index.parquet"));
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
    fn test_default_search_parses_offline_mft_mode_and_common_options() {
        let cli = parse_cli(&[
            "uffs",
            "*.rs",
            "--mft-file",
            "raw.bin",
            "--drive",
            "g:",
            "--format",
            "json",
            "--tz-offset",
            "-8",
        ])
        .expect("default search args should parse");

        assert!(cli.command.is_none());
        assert_eq!(cli.pattern.as_deref(), Some("*.rs"));
        assert_eq!(cli.drive, Some('G'));
        assert_eq!(cli.drives, None);
        assert_eq!(cli.mft_file.as_slice(), &[PathBuf::from("raw.bin")]);
        assert_eq!(cli.format, "json");
        assert_eq!(cli.tz_offset, Some(-8));
    }

    #[test]
    fn test_index_subcommand_normalizes_multi_drive_input() {
        let cli = parse_cli(&["uffs", "index", "out.parquet", "--drives", "c:,d,e:"])
            .expect("index args should parse");

        match cli.command {
            Some(Commands::Index {
                output,
                drive,
                drives,
            }) => {
                assert_eq!(output, PathBuf::from("out.parquet"));
                assert_eq!(drive, None);
                assert_eq!(drives, Some(vec!['C', 'D', 'E']));
            }
            _ => panic!("expected index subcommand"),
        }
    }

    #[test]
    fn test_index_help_includes_examples_and_multi_drive_guidance() {
        let mut command = Cli::command();
        let help = render_long_help(
            command
                .find_subcommand_mut("index")
                .expect("index subcommand should exist")
                .clone(),
        );

        assert!(help.contains("By default, indexes ALL available NTFS drives"));
        assert!(help.contains("uffs index -d C index.parquet"));
        assert!(help.contains("uffs index --drives C,D,E out.parquet"));
        assert!(help.contains("Creates myindex.parquet"));
    }

    #[tokio::test]
    async fn test_classify_cli_task_error_maps_cancelled_joins() {
        let handle = tokio::spawn(async {
            core::future::pending::<()>().await;
        });
        handle.abort();

        let outcome = handle.await;
        assert!(outcome.is_err(), "aborted task unexpectedly completed");
        let Err(join_error) = outcome else {
            return;
        };

        let error = classify_cli_task_error("uffs", &join_error);

        assert!(matches!(error, uffs_mft::MftError::Cancelled {
            operation: "uffs",
            ..
        }));
    }

    #[test]
    fn test_shutdown_requested_error_is_cancelled() {
        let error = shutdown_requested_error("uffs");

        assert!(matches!(error, uffs_mft::MftError::Cancelled {
            operation: "uffs",
            ..
        }));
    }

    #[test]
    fn test_ctrl_c_listener_error_is_wait_failed() {
        let error = ctrl_c_listener_error("uffs", &std::io::Error::other("listener unavailable"));

        assert!(matches!(error, uffs_mft::MftError::WaitFailed {
            operation: "uffs",
            ..
        }));
    }
}
