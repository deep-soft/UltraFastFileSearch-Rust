// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! UFFS Daemon binary — thin wrapper around [`uffs_daemon::run_daemon`].
//!
//! The actual daemon logic lives in the `uffs-daemon` library crate so it
//! can also be invoked via `uffs daemon run` (single-binary deployment).
//!
//! # Usage
//!
//! ```bash
//! uffs-daemon                          # default settings
//! uffs-daemon --mft-file C.bin D.bin   # load specific MFT files
//! uffs-daemon --idle-timeout 300       # retire after 5 min idle
//! uffs-daemon --no-retire              # stay running indefinitely
//! uffs-daemon --log-level debug        # verbose logging
//! ```

// Enable unstable Windows Unix domain socket support (Windows 10 1803+).
#![cfg_attr(windows, feature(windows_unix_domain_sockets))]

use std::path::PathBuf;

use mimalloc::MiMalloc;

/// Use mimalloc globally — faster than the Windows CRT heap and, critically,
/// `mi_collect(true)` can aggressively decommit freed pages so RSS reflects
/// actual usage after `MftIndex` temporaries are dropped.
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

// Suppress unused-crate-dependency warnings for deps consumed by the
// library crate (lib.rs) rather than the binary.
use anyhow as _;
use clap::Parser;
use dirs_next as _;
#[cfg(unix)]
use libc as _;
use libmimalloc_sys as _;
use rand as _;
use serde as _;
use serde_json as _;
use thiserror as _;
use tokio as _;
use tracing as _;
use tracing_appender as _;
use tracing_subscriber as _;
use uffs_client::connect_sync::UffsClientSync;
use uffs_client::protocol::response::LoadDriveResponse;
use uffs_core as _;
use uffs_mft as _;
use uffs_security as _;
#[cfg(windows)]
use windows as _;

/// UFFS background daemon — holds MFT index, serves queries via IPC.
#[derive(Parser)]
#[command(name = "uffs-daemon", about = "UFFS background search daemon")]
struct Cli {
    /// MFT files to load (*.bin, *.raw, *.iocp, *.uffs).
    #[arg(long = "mft-file", value_name = "PATH")]
    mft_files: Vec<PathBuf>,

    /// Data directory containing `drive_*` subdirectories with MFT files.
    #[arg(long = "data-dir", value_name = "DIR")]
    data_dir_arg: Option<PathBuf>,

    /// Live drives to load (Windows only, e.g. C D E).
    #[arg(long = "drive", value_name = "LETTER")]
    drives: Vec<char>,

    /// Idle timeout in seconds before auto-retire (default: 7200 = 2 hours).
    #[arg(long, default_value = "7200")]
    idle_timeout: u64,

    /// Disable auto-retire (stay running indefinitely).
    #[arg(long)]
    no_retire: bool,

    /// Skip cache when loading (force fresh MFT parse).
    #[arg(long)]
    no_cache: bool,

    /// Log level (error, warn, info, debug, trace).
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Write daemon logs to this file instead of stdout.
    ///
    /// When set, all tracing output is appended to the specified file.
    /// Use `"-"` or omit the path to default to `./uffs_daemon.log`.
    #[arg(long, value_name = "PATH")]
    log_file: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize tracing (standalone binary owns the subscriber).
    // UFFS_LOG env var overrides --log-level for diagnostic sessions.
    let log_spec = std::env::var("UFFS_LOG").unwrap_or_else(|_| cli.log_level.clone());
    let _guard = uffs_daemon::init_tracing(&log_spec, cli.log_file.as_deref());

    // Keep copies for potential IPC forwarding (moved into config below).
    let fwd_drives = cli.drives.clone();
    let fwd_mft_files: Vec<String> = cli
        .mft_files
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    let fwd_no_cache = cli.no_cache;

    let config = uffs_daemon::DaemonConfig {
        mft_files: cli.mft_files,
        data_dir: cli.data_dir_arg,
        drives: cli.drives,
        idle_timeout: cli.idle_timeout,
        no_retire: cli.no_retire,
        no_cache: cli.no_cache,
        log_level: cli.log_level,
        log_file: cli.log_file,
    };

    match uffs_daemon::run_daemon(config).await {
        Ok(()) => Ok(()),
        Err(err) if is_already_running(&err) => {
            // Another daemon is running — forward the load request via IPC.
            forward_to_running_daemon(&fwd_drives, &fwd_mft_files, fwd_no_cache)
        }
        Err(err) => Err(err),
    }
}

/// Check if the error is the "already running" sentinel.
fn is_already_running(err: &anyhow::Error) -> bool {
    format!("{err}").contains("Another daemon instance is already running")
}

/// Log the results of a [`LoadDriveResponse`].
fn log_load_response(resp: &LoadDriveResponse) {
    for letter in &resp.loaded {
        tracing::info!(drive = %letter, "Drive loaded");
    }
    for letter in &resp.already_loaded {
        tracing::info!(drive = %letter, "Drive already loaded");
    }
    for msg in &resp.errors {
        tracing::error!(error = %msg, "Failed to load drive");
    }
}

/// Forward `--drive` / `--mft-file` to the running daemon via IPC.
fn forward_to_running_daemon(
    drives: &[char],
    mft_files: &[String],
    no_cache: bool,
) -> anyhow::Result<()> {
    if drives.is_empty() && mft_files.is_empty() {
        tracing::info!("Daemon is already running. Nothing to load.");
        return Ok(());
    }

    tracing::info!("Daemon is already running — forwarding load request via IPC...");
    let mut client = UffsClientSync::connect()?;

    if !drives.is_empty() {
        log_load_response(&client.load_drive_letters(drives, no_cache)?);
    }
    if !mft_files.is_empty() {
        log_load_response(&client.load_drive(mft_files, no_cache)?);
    }

    Ok(())
}
