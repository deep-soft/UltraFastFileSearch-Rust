//! UFFS Daemon library — reusable daemon entry point.
//!
//! This crate exposes [`run_daemon`] so the daemon logic can be invoked
//! both from the standalone `uffs-daemon` binary and from the embedded
//! `uffs daemon run` subcommand in the CLI.

// Enable unstable Windows Unix domain socket support (Windows 10 1803+).
#![cfg_attr(windows, feature(windows_unix_domain_sockets))]

extern crate alloc;

use alloc::sync::Arc;
use std::path::PathBuf;

// Suppress unused crate warnings for deps used by sub-modules, the binary, or
// behind cfg gates.
use clap as _;
use dirs_next as _;
use serde as _;
use thiserror as _;
use tracing_appender as _;
use tracing_subscriber as _;
use uffs_mft as _;
use uffs_security as _;

/// Broker client — volume handle requests (Windows) / stubs (other).
mod broker_client;
/// Daemon event broadcasting — push notifications to connected clients.
pub mod events;
/// JSON-RPC request handler.
mod handler;
/// Index manager — loads and queries MFT data.
mod index;
/// IPC server — Unix domain socket / named pipe listener.
mod ipc;
/// Lifecycle manager — PID file, idle timer, shutdown coordination.
mod lifecycle;
/// JSON-RPC protocol types.
mod protocol;

/// Configuration for [`run_daemon`].
pub struct DaemonConfig {
    /// MFT files to load.
    pub mft_files: Vec<PathBuf>,
    /// Data directory containing `drive_*` subdirectories.
    pub data_dir: Option<PathBuf>,
    /// Explicit drive letters (Windows only).
    pub drives: Vec<char>,
    /// Idle timeout in seconds (0 = use default 600s).
    pub idle_timeout: u64,
    /// Disable auto-retire.
    pub no_retire: bool,
    /// Skip cache.
    pub no_cache: bool,
    /// Log level string (e.g. "info", "debug").
    pub log_level: String,
}

/// Bail if the daemon has nothing to serve.
#[expect(clippy::single_call_fn, reason = "extracted for clarity")]
fn validate_data_sources(
    mft_files: &[PathBuf],
    _drives: &[char],
    lifecycle_mgr: &lifecycle::LifecycleManager,
) -> anyhow::Result<()> {
    let has_data = !mft_files.is_empty() || {
        #[cfg(windows)]
        {
            !_drives.is_empty()
        }
        #[cfg(not(windows))]
        {
            false
        }
    };
    if !has_data {
        tracing::error!(
            "No data sources provided. On macOS/Linux pass --mft-file; \
             on Windows, NTFS drives are auto-discovered."
        );
        lifecycle_mgr.remove_pid_file();
        anyhow::bail!(
            "Daemon has no data sources to load. \
             Provide --mft-file <path> (or --data-dir when launching via CLI)."
        );
    }
    Ok(())
}

/// Run the UFFS daemon with the given configuration.
///
/// This is the main entry point shared by both the standalone
/// `uffs-daemon` binary and the embedded `uffs daemon run` subcommand.
///
/// **Does not return** until the daemon shuts down (idle timeout,
/// RPC shutdown, or signal).
///
/// # Errors
///
/// Returns an error if another daemon is already running, data sources
/// are missing, or the IPC server fails to bind.
#[expect(
    clippy::too_many_lines,
    reason = "temporary: extra tracing for daemon debugging"
)]
pub async fn run_daemon(config: DaemonConfig) -> anyhow::Result<()> {
    tracing::info!(
        pid = std::process::id(),
        version = env!("CARGO_PKG_VERSION"),
        broker_available = broker_client::broker_available(),
        mft_files = ?config.mft_files,
        drives = ?config.drives,
        data_dir = ?config.data_dir,
        no_cache = config.no_cache,
        no_retire = config.no_retire,
        "uffs-daemon starting"
    );

    // Determine data directory
    let data_dir = dirs_next::data_local_dir()
        .map_or_else(|| PathBuf::from("/tmp/uffs"), |base| base.join("uffs"));

    // Create event broadcast channel — used for push notifications to clients.
    let (event_tx, _event_rx) = events::event_channel();

    // Emit daemon_starting event
    event_tx.emit(events::DaemonEvent::DaemonStarting {
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    });

    // Setup lifecycle manager
    let idle_timeout = if config.no_retire {
        None
    } else {
        Some(core::time::Duration::from_secs(config.idle_timeout))
    };
    let mut lifecycle_mgr =
        lifecycle::LifecycleManager::new(&data_dir, idle_timeout, event_tx.clone());

    tracing::info!(data_dir = %lifecycle_mgr.data_dir().display(), "Lifecycle data directory");

    // Check for stale PID / another running instance
    if !lifecycle_mgr.check_stale_pid() {
        tracing::error!("Another daemon instance is already running");
        anyhow::bail!("Another daemon instance is already running");
    }

    // Write PID file
    lifecycle_mgr.write_pid_file()?;
    tracing::info!("PID file written");

    // D5.0: clean up stale shmem files from previous daemon sessions.
    uffs_client::shmem::cleanup_stale_shmem_files();

    // Create index manager
    let idx = Arc::new(index::IndexManager::new(Some(data_dir.clone()), event_tx));
    tracing::debug!(index_data_dir = ?idx.data_dir(), "Index manager created");

    // Merge --data-dir discovered files into --mft-file list.
    let mut mft_files = config.mft_files;
    if let Some(dir) = &config.data_dir {
        let discovered = uffs_mft::discovery::discover_mft_files(dir);
        tracing::info!(
            data_dir = %dir.display(),
            count = discovered.len(),
            "Discovered MFT files from --data-dir"
        );
        mft_files.extend(discovered);
    }
    let no_cache = config.no_cache;

    // Gather drive letters (Windows only; empty on other platforms).
    //
    // When `--drive C,D` is passed: load ONLY those drives — fast start.
    // When no `--drive` is passed: auto-discover ALL NTFS drives — full index.
    #[cfg(windows)]
    let drives: Vec<char> = {
        let explicit = config.drives;
        if explicit.is_empty() {
            // No drives specified → auto-discover all NTFS drives.
            let auto_drives = uffs_mft::detect_ntfs_drives();
            tracing::info!(
                count = auto_drives.len(),
                drives = ?auto_drives,
                "Auto-discovered NTFS drives (no --drive flag)"
            );
            auto_drives
        } else {
            // Respect the explicit drive list — load only what was asked.
            tracing::info!(
                drives = ?explicit,
                "Loading only requested drives (--drive flag)"
            );
            explicit
        }
    };
    #[cfg(not(windows))]
    let drives: Vec<char> = Vec::new();

    tracing::info!(mft_files = mft_files.len(), drives = ?drives, "Final data sources");

    // Refuse to start with zero data sources — an empty daemon is useless.
    validate_data_sources(&mft_files, &drives, &lifecycle_mgr)?;
    tracing::info!("Data sources validated OK");

    let load_index = Arc::clone(&idx);
    let broker_is_available = broker_client::broker_available();
    let load_task = tokio::spawn(async move {
        tracing::info!(mft_files = mft_files.len(), drives = ?drives, "Load task starting");
        if !mft_files.is_empty() {
            tracing::info!("Loading MFT files from data dir...");
            load_index.load_from_data_dir(&mft_files, no_cache).await;
            tracing::info!("MFT files loaded");
        }
        #[cfg(windows)]
        if !drives.is_empty() {
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
            tracing::info!(drives = ?drives, "Loading live drives...");
            load_index.load_live_drives(&drives, no_cache).await;
            tracing::info!("Live drives loaded");
        }
        if broker_is_available {
            let _handle_result = broker_client::request_volume_handle('C');
        }
        tracing::info!("Load task completed");
    });

    // Start IPC server
    let ipc_index = Arc::clone(&idx);
    let ipc_lifecycle = lifecycle_mgr.handle();

    tracing::info!("Starting IPC server...");
    let ipc_task = tokio::spawn(async move {
        if let Err(ipc_err) = ipc::run_ipc_server(ipc_index, ipc_lifecycle).await {
            tracing::error!(error = %ipc_err, "IPC server error");
        }
    });
    tracing::info!("IPC server task spawned");

    // Spawn periodic stats heartbeat — pushes stats to all connected
    // clients every 30 seconds.
    let stats_index = Arc::clone(&idx);
    let stats_lifecycle = lifecycle_mgr.handle();
    let _stats_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(core::time::Duration::from_secs(30));
        // Skip the first tick (fires immediately).
        interval.tick().await;
        loop {
            interval.tick().await;
            let total_records = stats_index.total_records().await;
            let stats = stats_index.stats().await;
            stats_index
                .event_sender()
                .emit(events::DaemonEvent::StatsHeartbeat {
                    total_queries: stats.total_queries,
                    uptime_secs: stats.uptime_secs,
                    total_records,
                    connections: stats_lifecycle.active_connections(),
                });
        }
    });

    // Run idle timer (blocks until shutdown or timeout)
    lifecycle_mgr.run_idle_timer().await;

    // Graceful shutdown
    tracing::info!("Daemon shutting down");
    ipc_task.abort();
    let _ignore = load_task.await;
    tracing::info!("Daemon stopped");

    // Clean up PID + socket files before exiting.
    drop(lifecycle_mgr);

    // Force-exit the process.  The Windows IPC server uses
    // `std::os::windows::net::UnixListener` with `spawn_blocking(accept)`
    // and per-connection `std::thread::spawn` bridge threads.  These
    // blocking std threads cannot be cancelled by `ipc_task.abort()` and
    // will keep the process alive indefinitely after the daemon logic has
    // finished, turning it into a multi-GB zombie.  `process::exit(0)` is
    // the standard pattern for daemons with uncancellable blocking threads.
    #[expect(
        clippy::exit,
        reason = "daemon has orphaned blocking threads that prevent normal exit"
    )]
    {
        std::process::exit(0);
    }
}
