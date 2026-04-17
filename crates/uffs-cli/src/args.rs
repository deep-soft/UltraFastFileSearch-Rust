// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Minimal CLI argument helpers — subcommand detection, help, version.
//!
//! Search-flag parsing is handled by the daemon via `search_cli` RPC
//! (see [`uffs_client::protocol::cli_args`]).  This module only handles
//! subcommands that run client-side (daemon, mcp, stats, aggregate).

use std::path::PathBuf;

/// Parse a drive letter from common CLI input formats.
///
/// Accepts `C`, `c`, `C:`, `c:`.  Returns uppercase drive letter.
///
/// # Errors
///
/// Returns an error if the input is not a valid drive letter.
pub fn parse_drive_letter(input: &str) -> Result<char, String> {
    let trimmed = input.trim();
    let letter_str = trimmed.strip_suffix(':').unwrap_or(trimmed);

    if letter_str.len() != 1 {
        return Err(format!(
            "Invalid drive letter '{input}': expected single letter like 'C' or 'C:'"
        ));
    }

    let ch = letter_str
        .chars()
        .next()
        .ok_or_else(|| format!("Invalid drive letter '{input}'"))?;

    if !ch.is_ascii_alphabetic() {
        return Err(format!("Invalid drive letter '{input}': must be A-Z"));
    }

    Ok(ch.to_ascii_uppercase())
}

// ── Subcommand types ───────────────────────────────────────────────────

/// Available CLI subcommands (for local dispatch only).
pub enum Commands {
    /// Stats subcommand.
    Stats,
    /// Aggregate subcommand.
    Aggregate,
    /// Daemon management.
    Daemon,
    /// MCP management.
    Mcp,
    /// System status.
    SystemStatus,
}

/// Actions for `uffs daemon` subcommand.
pub enum DaemonAction {
    /// Start the daemon.
    Start {
        /// Raw MFT file(s).
        mft_file: Vec<PathBuf>,
        /// Data directory.
        data_dir: Option<PathBuf>,
        /// Drive letter(s) to load (filters `--data-dir` discovery).
        drives: Vec<char>,
        /// Skip file cache.
        no_cache: bool,
        /// Log level.
        log_level: String,
        /// Log file path.
        log_file: Option<PathBuf>,
    },
    /// Show daemon status.
    Status,
    /// Show performance statistics.
    Stats,
    /// Gracefully stop.
    Stop,
    /// Hard kill.
    Kill,
    /// Stop then restart.
    Restart,
    /// Hot-load additional MFT file(s) or drive(s) into a running daemon.
    Load {
        /// Raw MFT file(s) to hot-load.
        mft_file: Vec<PathBuf>,
        /// Data directory — discover and load a specific drive from it.
        data_dir: Option<PathBuf>,
        /// Drive letter(s) to load (Windows live only).
        drives: Vec<char>,
        /// Skip cache when loading.
        no_cache: bool,
    },
}

/// Parse `uffs daemon <action> [flags...]` from raw args.
///
/// # Errors
///
/// Returns an error on invalid action or flags.
pub fn parse_daemon_action(args: &[String]) -> Result<DaemonAction, anyhow::Error> {
    let action = args.first().map_or("status", String::as_str);
    let rest = args.get(1..).unwrap_or_default();
    match action {
        "start" => Ok(parse_daemon_start(rest)),
        "status" => Ok(DaemonAction::Status),
        "stats" => Ok(DaemonAction::Stats),
        "stop" => Ok(DaemonAction::Stop),
        "kill" => Ok(DaemonAction::Kill),
        "restart" => Ok(DaemonAction::Restart),
        "load" => Ok(parse_daemon_load(rest)),
        other => anyhow::bail!(
            "Unknown daemon action: '{other}'. Use: start, status, stats, stop, kill, restart, load"
        ),
    }
}

/// Parse `uffs daemon start [flags...]`.
fn parse_daemon_start(rest: &[String]) -> DaemonAction {
    let mut mft_file = Vec::new();
    let mut data_dir = None;
    let mut drives = Vec::new();
    let mut no_cache = false;
    let mut log_level = "info".to_owned();
    let mut log_file = None;
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--mft-file" => {
                if let Some(val) = iter.next() {
                    mft_file = val
                        .split(',')
                        .map(|part| PathBuf::from(part.trim()))
                        .collect();
                }
            }
            "--data-dir" => {
                if let Some(val) = iter.next() {
                    data_dir = Some(val.into());
                }
            }
            "--drive" => {
                if let Some(val) = iter.next() {
                    for ch in val.chars() {
                        if ch.is_ascii_alphabetic() {
                            drives.push(ch.to_ascii_uppercase());
                        }
                    }
                }
            }
            "--no-cache" => no_cache = true,
            "--log-level" => {
                if let Some(val) = iter.next() {
                    log_level.clone_from(val);
                }
            }
            "--log-file" => {
                if let Some(val) = iter.next() {
                    log_file = Some(val.into());
                }
            }
            _ => {}
        }
    }
    DaemonAction::Start {
        mft_file,
        data_dir,
        drives,
        no_cache,
        log_level,
        log_file,
    }
}

/// Parse `uffs daemon load [flags...]`.
fn parse_daemon_load(rest: &[String]) -> DaemonAction {
    let mut mft_file = Vec::new();
    let mut data_dir = None;
    let mut drives = Vec::new();
    let mut no_cache = false;
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--mft-file" => {
                if let Some(val) = iter.next() {
                    for part in val.split(',') {
                        mft_file.push(PathBuf::from(part.trim()));
                    }
                }
            }
            "--data-dir" => {
                if let Some(val) = iter.next() {
                    data_dir = Some(val.into());
                }
            }
            "--drive" | "-d" => {
                if let Some(val) = iter.next() {
                    for part in val.split(',') {
                        if let Ok(letter) = parse_drive_letter(part) {
                            drives.push(letter);
                        }
                    }
                }
            }
            "--no-cache" => no_cache = true,
            _ => {}
        }
    }
    DaemonAction::Load {
        mft_file,
        data_dir,
        drives,
        no_cache,
    }
}

// ── Help & version ─────────────────────────────────────────────────────

/// Short help text.
const HELP: &str = "\
uffs - Ultra Fast File Search

USAGE:  uffs [OPTIONS] <PATTERN>
        uffs <SUBCOMMAND> [OPTIONS]

Search is the default action: pass a pattern with no subcommand.

EXAMPLES:
  uffs '*.txt'                        Find all .txt files
  uffs '>.*\\.log$' --drive C          Regex search on C:
  uffs '*' --mft-file C.bin            Offline MFT search
  uffs --ext rs,toml                   Find Rust project files
  uffs --type picture --min-size 10MB  Large images

SUBCOMMANDS:
  stats             Show filesystem statistics
  aggregate|agg     Run aggregate analytics
  daemon            Manage the UFFS daemon (start/stop/load/status)
  mcp               Manage the UFFS MCP server
  status            Show combined system status

COMMON OPTIONS:
  -v, --verbose           Verbose output
  -d, --drive <LETTER>    Drive letter (e.g. C or C:)
  --drives <A,B,...>      Multiple drive letters
  --mft-file <PATH>       Raw MFT file(s), comma-separated
  --data-dir <PATH>       Data directory with drive_* subdirs
  --files-only            Show only files
  --dirs-only             Show only directories
  --ext <EXT>             Filter by extension(s)
  --type <CATEGORY>       Filter by type: code, picture, video, etc.
  -n, --limit <N>         Max results (0 = unlimited, default: 0)
  -f, --format <FMT>      Output: csv (default), json, table
  --sort <COL>            Sort by column, prefix - for desc
  --out <FILE>            Write to file instead of console
  --columns <COLS>        Columns to output (default: all)
  --newer <SPEC>          Modified after date/duration
  --older <SPEC>          Modified before date/duration
  --min-size <SIZE>       Minimum file size (e.g. 100KB, 10MB)
  --max-size <SIZE>       Maximum file size
  --profile               Show timing breakdown
  --benchmark             Measure only, skip output
  --help                  Print this help
  --version               Print version
";

/// Print help and exit.
#[expect(clippy::print_stdout, reason = "intentional help output")]
pub fn print_help() {
    print!("{HELP}");
}

/// Print version and exit.
#[expect(clippy::print_stdout, reason = "intentional version output")]
pub fn print_version() {
    println!("uffs {}", env!("CARGO_PKG_VERSION"));
}

// ── Subcommand help texts ─────────────────────────────────────────────

/// Help text for `uffs daemon`.
const DAEMON_HELP: &str = "\
uffs daemon — Manage the UFFS background daemon

USAGE:  uffs daemon <ACTION> [OPTIONS]

ACTIONS:
  start              Start the daemon
    --data-dir PATH    Data directory with drive_* subdirs
    --mft-file PATH    Raw MFT file(s), comma-separated
    --no-cache         Skip cached index, re-parse MFT
  status             Show daemon status (running, drives, PID)
  stats              Show performance statistics
  stop               Gracefully stop the daemon
  kill               Hard kill + remove PID/socket files
  restart            Stop then restart (re-loads all indices)
  load               Hot-load additional MFT file(s) into running daemon
    --mft-file PATH    Raw MFT file(s) to load
    --data-dir PATH    Data directory with drive_* subdirs
    --drive LETTER     Drive letter(s) to load from data-dir
    --no-cache         Skip cache when loading
";

/// Print daemon help.
#[expect(clippy::print_stdout, reason = "intentional help output")]
pub fn print_daemon_help() {
    print!("{DAEMON_HELP}");
}

/// Help text for `uffs stats`.
const STATS_HELP: &str = "\
uffs stats — Show filesystem statistics

USAGE:  uffs stats [PATH] [OPTIONS]

ARGUMENTS:
  [PATH]               Index file path (optional; omit to query daemon)

OPTIONS:
  --top <N>            Show top N largest files (default: 10)
  --data-dir <PATH>    Data directory with drive_* subdirs
  --mft-file <PATH>    Raw MFT file(s)
";

/// Print stats help.
#[expect(clippy::print_stdout, reason = "intentional help output")]
pub fn print_stats_help() {
    print!("{STATS_HELP}");
}

/// Help text for `uffs aggregate`.
const AGGREGATE_HELP: &str = "\
uffs aggregate — Run aggregate analytics on the filesystem index

USAGE:  uffs aggregate <PRESET> [OPTIONS]

ARGUMENTS:
  <PRESET>             overview, by_type, by_extension, by_drive,
                       by_size, by_age, count

OPTIONS:
  --format <FMT>       Output format: table (default), csv, json
  --data-dir <PATH>    Data directory with drive_* subdirs
  --mft-file <PATH>    Raw MFT file(s)
  --agg-cursor <TOK>   Continue from previous page
  --agg-page-size <N>  Max buckets per page
";

/// Print aggregate help.
#[expect(clippy::print_stdout, reason = "intentional help output")]
pub fn print_aggregate_help() {
    print!("{AGGREGATE_HELP}");
}

/// Help text for `uffs status`.
const STATUS_HELP: &str = "\
uffs status — Show combined system status (daemon + MCP HTTP server)

USAGE:  uffs status
";

/// Print status help.
#[expect(clippy::print_stdout, reason = "intentional help output")]
pub fn print_status_help() {
    print!("{STATUS_HELP}");
}
