//! `uffs daemon {status|stop|kill|restart}` subcommand handlers.

use anyhow::{Context, Result};
use tracing::info;
use uffs_client::connect::{UffsClient, pid_file_path, socket_path};
use uffs_client::protocol::DaemonStatus;

use crate::args::DaemonAction;

/// Execute a daemon management action.
pub async fn daemon(action: &DaemonAction) -> Result<()> {
    match action {
        DaemonAction::Start {
            mft_file,
            data_dir,
            no_cache,
            log_level,
            log_file,
        } => {
            daemon_start(
                mft_file,
                data_dir.as_deref(),
                *no_cache,
                log_level,
                log_file.as_deref(),
            )
            .await
        }
        DaemonAction::Status => daemon_status().await,
        DaemonAction::Stats => daemon_stats().await,
        DaemonAction::Stop => daemon_stop().await,
        DaemonAction::Kill => {
            daemon_kill().await;
            Ok(())
        }
        DaemonAction::Restart => daemon_restart().await,
        DaemonAction::Run {
            mft_files,
            data_dir,
            drives,
            idle_timeout,
            no_retire,
            no_cache,
            log_level,
            log_file,
        } => {
            daemon_run(&DaemonRunParams {
                mft_files,
                data_dir: data_dir.as_ref(),
                drives,
                idle_timeout: *idle_timeout,
                no_retire: *no_retire,
                no_cache: *no_cache,
                log_level,
                log_file: log_file.as_deref(),
            })
            .await
        }
    }
}

/// Parameters for `daemon_run`, grouped to stay under the argument-count limit.
struct DaemonRunParams<'a> {
    /// MFT file paths to load.
    mft_files: &'a [std::path::PathBuf],
    /// Optional data directory override.
    data_dir: Option<&'a std::path::PathBuf>,
    /// Drive letters to scan.
    drives: &'a [char],
    /// Idle timeout in seconds.
    idle_timeout: u64,
    /// Disable retirement of stale indices.
    no_retire: bool,
    /// Disable on-disk caching.
    no_cache: bool,
    /// Log level spec (e.g. `"info"`, `"debug"`).
    log_level: &'a str,
    /// Optional log file path.
    log_file: Option<&'a std::path::Path>,
}

/// `uffs daemon run` — run the daemon in-process (embedded mode).
///
/// This is the same daemon logic as the standalone `uffs-daemon` binary,
/// invoked by the client's auto-start mechanism so only a single `uffs`
/// binary needs to be deployed.
async fn daemon_run(params: &DaemonRunParams<'_>) -> Result<()> {
    // Initialise tracing for the daemon — when launched as a detached
    // background process, no subscriber exists yet.  When called in-process
    // (e.g. `uffs daemon run` from the same CLI binary), a subscriber may
    // already be installed; `try_init` gracefully handles that.
    // UFFS_LOG env var overrides --log-level for diagnostic sessions.
    let log_spec = std::env::var("UFFS_LOG").unwrap_or_else(|_| params.log_level.to_owned());
    let _guard = uffs_daemon::init_tracing(&log_spec, params.log_file);

    uffs_daemon::run_daemon(uffs_daemon::DaemonConfig {
        mft_files: params.mft_files.to_vec(),
        data_dir: params.data_dir.cloned(),
        drives: params.drives.to_vec(),
        idle_timeout: params.idle_timeout,
        no_retire: params.no_retire,
        no_cache: params.no_cache,
        log_level: params.log_level.to_owned(),
        log_file: params.log_file.map(std::path::Path::to_path_buf),
    })
    .await
    .with_context(|| "daemon run failed")
}

/// `uffs daemon start` — start the daemon, forwarding data-source flags
/// as-is so the daemon resolves them internally (DRY).
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
async fn daemon_start(
    mft_files: &[std::path::PathBuf],
    data_dir: Option<&std::path::Path>,
    no_cache: bool,
    log_level: &str,
    log_file: Option<&std::path::Path>,
) -> Result<()> {
    // Already running?
    tracing::info!("[daemon_start] checking if daemon is already running");
    if UffsClient::connect_raw().await.is_ok() {
        tracing::info!("[daemon_start] connect_raw succeeded — daemon already running");
        println!("Daemon is already running. Use `uffs daemon restart` to reload.");
        return Ok(());
    }
    tracing::info!("[daemon_start] connect_raw failed — daemon not running, will start");

    // Build spawn args — forward raw, let daemon handle discovery.
    let mut spawn_args = Vec::new();
    if let Some(dir) = data_dir {
        spawn_args.push("--data-dir".to_owned());
        spawn_args.push(dir.to_string_lossy().into_owned());
    }
    for mft_path in mft_files {
        spawn_args.push("--mft-file".to_owned());
        spawn_args.push(mft_path.to_string_lossy().into_owned());
    }
    if no_cache {
        spawn_args.push("--no-cache".to_owned());
    }
    // Forward logging configuration to the spawned daemon.
    if log_level != "info" {
        spawn_args.push("--log-level".to_owned());
        spawn_args.push(log_level.to_owned());
    }
    if let Some(path) = log_file {
        spawn_args.push("--log-file".to_owned());
        spawn_args.push(path.to_string_lossy().into_owned());
    }

    if !cfg!(windows) && spawn_args.is_empty() {
        anyhow::bail!(
            "No MFT data sources specified.\n\
             Provide --mft-file <path> or --data-dir <path>."
        );
    }

    println!("Starting daemon...");

    tracing::info!("[daemon_start] connecting to daemon (auto-start if needed)");
    let mut client = UffsClient::connect_with_args(&spawn_args)
        .await
        .with_context(|| "Failed to start daemon")?;
    tracing::info!("[daemon_start] connected, entering await_ready");

    client
        .await_ready(core::time::Duration::from_mins(2))
        .await
        .with_context(|| "Daemon did not become ready in time")?;

    tracing::info!("[daemon_start] await_ready returned OK, printing result");
    // await_ready already confirmed Ready — no need for a second status call
    // which would risk hanging if the bridge connection tears down.
    println!("Daemon started and ready.");
    tracing::info!("[daemon_start] done, returning");
    Ok(())
}

/// `uffs daemon status` — show daemon status, PID, loaded drives.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
async fn daemon_status() -> Result<()> {
    let Ok(mut client) = UffsClient::connect_raw().await else {
        print_not_running();
        return Ok(());
    };

    // If the socket file is stale (daemon just exited), status() returns
    // ConnectionClosed.  Treat that as "not running" instead of an error.
    let Ok(status) = client.status().await else {
        print_not_running();
        return Ok(());
    };

    let uptime = core::time::Duration::from_secs(status.uptime_secs);
    println!("Daemon PID:    {}", status.pid);
    println!(
        "Uptime:        {}",
        uffs_core::format::format_duration(uptime)
    );
    match &status.status {
        DaemonStatus::Loading {
            drives_loaded,
            drives_total,
        } => {
            println!("Status:        Loading ({drives_loaded}/{drives_total} drives)");
        }
        DaemonStatus::Ready => {
            println!("Status:        Ready");
        }
        DaemonStatus::Refreshing { drives } => {
            let drive_list: String = drives
                .iter()
                .map(|letter| format!("{letter}:"))
                .collect::<Vec<_>>()
                .join(", ");
            println!("Status:        Refreshing ({drive_list})");
        }
    }
    println!("Connections:   {}", status.connections);

    // Also show loaded drives.
    let drives = client
        .drives()
        .await
        .with_context(|| "Failed to query drives")?;
    if drives.drives.is_empty() {
        println!("Drives:        (none loaded)");
    } else {
        for dr in &drives.drives {
            println!(
                "  {}: — {:>10} records ({})",
                dr.letter,
                uffs_core::format::format_number_commas(dr.records as u64),
                dr.source
            );
        }
    }
    Ok(())
}

/// Print the "not running" message with optional stale-PID hint.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn print_not_running() {
    println!("Daemon is not running.");
    let pid_path = pid_file_path();
    if pid_path.exists() {
        println!("  (stale PID file exists at {})", pid_path.display());
    }
}

/// `uffs daemon stats` — show performance metrics.
#[expect(
    clippy::print_stdout,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "CLI user-facing output; f64→u64 truncation acceptable for display"
)]
async fn daemon_stats() -> Result<()> {
    if let Ok(mut client) = UffsClient::connect_raw().await {
        let stats = client
            .stats()
            .await
            .with_context(|| "Failed to query daemon stats")?;

        let fmt = uffs_core::format::format_duration;
        let uptime = core::time::Duration::from_secs(stats.uptime_secs);
        let startup = core::time::Duration::from_millis(stats.startup_duration_ms);
        let avg_query = core::time::Duration::from_micros(stats.avg_query_time_us as u64);
        let total_query = core::time::Duration::from_micros(stats.total_query_time_us);

        println!("═══ Daemon Performance Stats ═══");
        println!("Uptime:            {}", fmt(uptime));
        println!("Startup duration:  {}", fmt(startup));
        println!(
            "Total records:     {}",
            uffs_core::format::format_number_commas(stats.total_records as u64)
        );
        println!("Queries served:    {}", stats.total_queries);
        if stats.total_queries > 0 {
            println!("Avg query time:    {}", fmt(avg_query));
            println!("Total query time:  {}", fmt(total_query));
        }
        println!("Queries/second:    {:.2}", stats.queries_per_second);
    } else {
        println!("Daemon is not running.");
    }
    Ok(())
}

/// `uffs daemon stop` — graceful shutdown via RPC.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
async fn daemon_stop() -> Result<()> {
    if let Ok(mut client) = UffsClient::connect_raw().await {
        info!("Sending shutdown request to daemon");
        client
            .shutdown()
            .await
            .with_context(|| "Shutdown RPC failed — try `uffs daemon kill` instead")?;
        println!("Daemon shutdown requested.");
    } else {
        println!("Daemon is not running.");
    }
    Ok(())
}

/// `uffs daemon kill` — hard kill via PID file or socket discovery + cleanup.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
async fn daemon_kill() {
    let pid_path = pid_file_path();

    // Try to get PID from PID file first, then from live socket connection.
    let mut pid =
        uffs_client::connect::parse_pid_file(&pid_path).map(|(file_pid, _, _, _)| file_pid);

    // No PID file → try discovering via live socket.
    if pid.is_none()
        && let Ok(mut client) = UffsClient::connect_raw().await
        && let Ok(status) = client.status().await
    {
        pid = Some(status.pid);
    }

    if let Some(target_pid) = pid {
        println!("Killing daemon (PID {target_pid})...");
        kill_pid(target_pid);
    } else {
        println!("No daemon found (no PID file, no socket connection).");
    }

    // Always clean up stale files.
    drop(std::fs::remove_file(&pid_path));
    drop(std::fs::remove_file(socket_path()));
    if pid.is_some() {
        println!("Daemon killed. PID file and socket cleaned up.");
    }
}

/// Send SIGKILL (Unix) or taskkill (Windows) to a process.
fn kill_pid(pid: u32) {
    #[cfg(unix)]
    {
        drop(
            std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .output(),
        );
    }
    #[cfg(windows)]
    {
        drop(
            std::process::Command::new("taskkill")
                .args(["/F", "/PID", &pid.to_string()])
                .output(),
        );
    }
}

/// `uffs daemon restart` — stop, capture data sources, then re-launch.
///
/// If the daemon is running, queries its loaded drives to extract the
/// original `--mft-file` paths, stops it, then re-spawns with the same
/// arguments.  If not running, prints a message and exits.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
async fn daemon_restart() -> Result<()> {
    // 1. Connect to the running daemon and capture its data sources.
    let spawn_args = if let Ok(mut client) = UffsClient::connect_raw().await {
        let drives_resp = client
            .drives()
            .await
            .with_context(|| "Failed to query drives before restart")?;

        // Build --mft-file args from the drive source paths.
        let mut args = Vec::new();
        for dr in &drives_resp.drives {
            if let Some(path) = dr.source.strip_prefix("file:") {
                args.push("--mft-file".to_owned());
                args.push(path.to_owned());
            }
            // "live" sources are auto-discovered on Windows — no arg needed.
        }

        info!(
            drives = drives_resp.drives.len(),
            args_count = args.len(),
            "Captured data sources for restart"
        );

        // 2. Stop the running daemon gracefully.
        let daemon_pid = client
            .status()
            .await
            .map_or(0, |status_resp| status_resp.pid);
        println!("Stopping daemon (PID {daemon_pid})...");

        client.shutdown().await.with_context(|| {
            format!(
                "Graceful shutdown of PID {daemon_pid} failed.\n\
                 Run `uffs daemon kill` first, then retry."
            )
        })?;

        // Wait for it to actually exit.
        tokio::time::sleep(core::time::Duration::from_secs(1)).await;

        args
    } else {
        println!("Daemon is not running — nothing to restart.");
        return Ok(());
    };

    // 3. Clean up stale PID/socket files.
    drop(std::fs::remove_file(pid_file_path()));
    drop(std::fs::remove_file(socket_path()));

    println!(
        "Restarting daemon with {} data source(s)...",
        spawn_args
            .iter()
            .filter(|arg| *arg == "--mft-file" || *arg == "--data-dir")
            .count()
    );

    // 4. Let connect_with_args handle spawning (avoids double-spawn).
    let mut client = UffsClient::connect_with_args(&spawn_args)
        .await
        .with_context(|| "Failed to start restarted daemon")?;

    client
        .await_ready(core::time::Duration::from_mins(2))
        .await
        .with_context(|| "Restarted daemon did not become ready in time")?;

    let status = client.status().await;
    if let Ok(resp) = status {
        let state = match &resp.status {
            DaemonStatus::Loading {
                drives_loaded,
                drives_total,
            } => format!("Loading ({drives_loaded}/{drives_total} drives)"),
            DaemonStatus::Ready => "Ready".to_owned(),
            DaemonStatus::Refreshing { .. } => "Refreshing".to_owned(),
        };
        println!("Daemon restarted (PID {}), status: {state}", resp.pid);
    } else {
        println!("Daemon restarted.");
    }

    Ok(())
}
