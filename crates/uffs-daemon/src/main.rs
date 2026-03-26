//! UFFS Daemon — background service holding MFT indices, serving queries
//! via IPC (Unix domain socket / Windows named pipe).
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

extern crate alloc;

use alloc::sync::Arc;
use std::path::PathBuf;

use clap::Parser;
// Suppress unused crate warnings for deps used in sub-modules behind cfg gates
use dirs_next as _;
use serde as _;
use thiserror as _;
use tracing_appender as _;
use uffs_mft as _;
use uffs_security as _;

mod broker_client;
mod handler;
mod index;
mod ipc;
mod lifecycle;
mod protocol;

/// UFFS background daemon — holds MFT index, serves queries via IPC.
#[derive(Parser)]
#[command(name = "uffs-daemon", about = "UFFS background search daemon")]
struct Cli {
    /// MFT files to load (*.bin, *.raw, *.iocp, *.uffs).
    #[arg(long = "mft-file", value_name = "PATH")]
    mft_files: Vec<PathBuf>,

    /// Live drives to load (Windows only, e.g. C D E).
    #[arg(long = "drive", value_name = "LETTER")]
    drives: Vec<char>,

    /// Idle timeout in seconds before auto-retire (default: 600 = 10 min).
    #[arg(long, default_value = "600")]
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize tracing
    let filter = tracing_subscriber::EnvFilter::try_new(&cli.log_level)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    tracing::info!(
        pid = std::process::id(),
        version = env!("CARGO_PKG_VERSION"),
        broker_available = broker_client::broker_available(),
        "uffs-daemon starting"
    );

    // Determine data directory
    let data_dir = dirs_next::data_local_dir()
        .map_or_else(|| PathBuf::from("/tmp/uffs"), |base| base.join("uffs"));

    // Setup lifecycle manager
    let idle_timeout = if cli.no_retire {
        None
    } else {
        Some(core::time::Duration::from_secs(cli.idle_timeout))
    };
    let mut lifecycle = lifecycle::LifecycleManager::new(&data_dir, idle_timeout);

    tracing::info!(data_dir = %lifecycle.data_dir().display(), "Lifecycle data directory");

    // Check for stale PID / another running instance
    if !lifecycle.check_stale_pid() {
        anyhow::bail!("Another daemon instance is already running");
    }

    // Write PID file
    lifecycle.write_pid_file()?;

    // Create index manager
    let index = Arc::new(index::IndexManager::new(Some(data_dir.clone())));
    tracing::debug!(index_data_dir = ?index.data_dir(), "Index manager created");

    // Load indices in background
    let load_index = Arc::clone(&index);
    let mft_files = cli.mft_files.clone();
    let no_cache = cli.no_cache;

    #[cfg(windows)]
    let drives = cli.drives.clone();

    let broker_is_available = broker_client::broker_available();
    let load_task = tokio::spawn(async move {
        if !mft_files.is_empty() {
            load_index.load_from_data_dir(&mft_files, no_cache).await;
        }
        #[cfg(windows)]
        if !drives.is_empty() {
            // If broker is available, try to get volume handles from it
            if broker_is_available {
                for &drive_letter in &drives {
                    match broker_client::request_volume_handle(drive_letter) {
                        Ok(handle) => {
                            tracing::info!(drive = %drive_letter, handle, "Got broker handle")
                        }
                        Err(broker_err) => {
                            tracing::debug!(drive = %drive_letter, error = %broker_err, "Broker unavailable, using direct access")
                        }
                    }
                }
            }
            load_index.load_live_drives(&drives, no_cache).await;
        }
        // Test broker availability on all platforms (validates stub linkage)
        if broker_is_available {
            // On Windows: attempt to get a handle for first available drive
            // On non-Windows: broker_available() returns false so this is unreachable
            let _handle_result = broker_client::request_volume_handle('C');
        }
    });

    // Start IPC server
    let ipc_index = Arc::clone(&index);
    let ipc_lifecycle = lifecycle.handle();

    let ipc_task = tokio::spawn(async move {
        if let Err(ipc_err) = ipc::run_ipc_server(ipc_index, ipc_lifecycle).await {
            tracing::error!(error = %ipc_err, "IPC server error");
        }
    });

    // Run idle timer (blocks until shutdown or timeout)
    lifecycle.run_idle_timer().await;

    // Graceful shutdown
    tracing::info!("Daemon shutting down");

    // Abort IPC server (it runs forever until cancelled)
    ipc_task.abort();

    // Wait for index loading to finish (if still in progress)
    let _ignore = load_task.await;

    tracing::info!("Daemon stopped");
    Ok(())
}
