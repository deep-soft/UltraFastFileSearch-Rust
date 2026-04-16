// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `uffs daemon {status|stop|kill|restart}` subcommand handlers.

use anyhow::{Context, Result};
use uffs_client::connect_sync::UffsClientSync;
use uffs_client::daemon_ctl::{pid_file_path, socket_path};
use uffs_client::protocol::response::DaemonStatus;

use crate::args::DaemonAction;

/// Execute a daemon management action.
///
/// # Errors
///
/// Returns an error if the operation fails.
pub fn daemon(action: &DaemonAction) -> Result<()> {
    match action {
        DaemonAction::Start {
            mft_file,
            data_dir,
            no_cache,
            log_level,
            log_file,
        } => daemon_start(
            mft_file,
            data_dir.as_deref(),
            *no_cache,
            log_level,
            log_file.as_deref(),
        ),
        DaemonAction::Status => daemon_status(),
        DaemonAction::Stats => daemon_stats(),
        DaemonAction::Stop => daemon_stop(),
        DaemonAction::Kill => {
            daemon_kill();
            Ok(())
        }
        DaemonAction::Restart => daemon_restart(),
    }
}

/// `uffs daemon start` — start the daemon, forwarding data-source flags
/// as-is so the daemon resolves them internally (DRY).
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn daemon_start(
    mft_files: &[std::path::PathBuf],
    data_dir: Option<&std::path::Path>,
    no_cache: bool,
    log_level: &str,
    log_file: Option<&std::path::Path>,
) -> Result<()> {
    // Already running?
    if UffsClientSync::connect_raw().is_ok() {
        println!("Daemon is already running. Use `uffs daemon restart` to reload.");
        return Ok(());
    }

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

    let mut client =
        UffsClientSync::connect_with_args(&spawn_args).with_context(|| "Failed to start daemon")?;

    client
        .await_ready(core::time::Duration::from_mins(2))
        .with_context(|| "Daemon did not become ready in time")?;

    println!("Daemon started and ready.");
    Ok(())
}

/// `uffs daemon status` — show daemon status, PID, loaded drives.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn daemon_status() -> Result<()> {
    let Ok(mut client) = UffsClientSync::connect_raw() else {
        print_not_running();
        return Ok(());
    };

    let Ok(status) = client.status() else {
        print_not_running();
        return Ok(());
    };

    let uptime = core::time::Duration::from_secs(status.uptime_secs);
    println!("Daemon PID:    {}", status.pid);
    println!(
        "Uptime:        {}",
        uffs_client::format::format_duration(uptime)
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

    // Memory info.
    if let Some(heap) = status.index_heap_bytes {
        println!("Index heap:    {} MB", heap / (1024 * 1024));
    }

    // Also show loaded drives.
    let drives = client.drives().with_context(|| "Failed to query drives")?;
    if drives.drives.is_empty() {
        println!("Drives:        (none loaded)");
    } else {
        for dr in &drives.drives {
            // Find memory info for this drive.
            let mem = status.drive_memory.iter().find(|dm| dm.drive == dr.letter);
            if let Some(dm) = mem {
                let mb = |bytes: u64| bytes / (1024 * 1024);
                println!(
                    "  {}: — {:>10} records ({}) — {} MB  [rec={} names={} tri={} ch={} ext={}]",
                    dr.letter,
                    uffs_client::format::format_number_commas(dr.records as u64),
                    dr.source,
                    mb(dm.heap_bytes),
                    mb(dm.records_bytes),
                    mb(dm.names_bytes),
                    mb(dm.trigram_bytes),
                    mb(dm.children_bytes),
                    mb(dm.ext_index_bytes),
                );
            } else {
                println!(
                    "  {}: — {:>10} records ({})",
                    dr.letter,
                    uffs_client::format::format_number_commas(dr.records as u64),
                    dr.source
                );
            }
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
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn daemon_stats() -> Result<()> {
    if let Ok(mut client) = UffsClientSync::connect_raw() {
        let stats = client
            .stats()
            .with_context(|| "Failed to query daemon stats")?;

        let fmt = uffs_client::format::format_duration;
        let uptime = core::time::Duration::from_secs(stats.uptime_secs);
        let startup = core::time::Duration::from_millis(stats.startup_duration_ms);
        let avg_query = core::time::Duration::from_micros(uffs_client::format::f64_to_u64(
            stats.avg_query_time_us,
        ));
        let total_query = core::time::Duration::from_micros(stats.total_query_time_us);

        println!("═══ Daemon Performance Stats ═══");
        println!("Uptime:            {}", fmt(uptime));
        println!("Startup duration:  {}", fmt(startup));
        println!(
            "Total records:     {}",
            uffs_client::format::format_number_commas(stats.total_records as u64)
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
fn daemon_stop() -> Result<()> {
    if let Ok(mut client) = UffsClientSync::connect_raw() {
        client
            .shutdown()
            .with_context(|| "Shutdown RPC failed — try `uffs daemon kill` instead")?;
        println!("Daemon shutdown requested.");
    } else {
        println!("Daemon is not running.");
    }
    Ok(())
}

/// `uffs daemon kill` — hard kill via PID file or socket discovery + cleanup.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn daemon_kill() {
    let pid_path = pid_file_path();

    let mut pid =
        uffs_client::daemon_ctl::parse_pid_file(&pid_path).map(|(file_pid, _, _, _)| file_pid);

    // No PID file → try discovering via live socket.
    if pid.is_none()
        && let Ok(mut client) = UffsClientSync::connect_raw()
        && let Ok(status) = client.status()
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
fn daemon_restart() -> Result<()> {
    let spawn_args = if let Ok(mut client) = UffsClientSync::connect_raw() {
        let drives_resp = client
            .drives()
            .with_context(|| "Failed to query drives before restart")?;

        let mut args = Vec::new();
        for dr in &drives_resp.drives {
            if let Some(path) = dr.source.strip_prefix("file:") {
                args.push("--mft-file".to_owned());
                args.push(path.to_owned());
            }
        }

        let daemon_pid = client.status().map_or(0, |status_resp| status_resp.pid);
        println!("Stopping daemon (PID {daemon_pid})...");

        client.shutdown().with_context(|| {
            format!(
                "Graceful shutdown of PID {daemon_pid} failed.\n\
                 Run `uffs daemon kill` first, then retry."
            )
        })?;

        std::thread::sleep(core::time::Duration::from_secs(1));
        args
    } else {
        println!("Daemon is not running — nothing to restart.");
        return Ok(());
    };

    drop(std::fs::remove_file(pid_file_path()));
    drop(std::fs::remove_file(socket_path()));

    println!(
        "Restarting daemon with {} data source(s)...",
        spawn_args
            .iter()
            .filter(|arg| *arg == "--mft-file" || *arg == "--data-dir")
            .count()
    );

    let mut client = UffsClientSync::connect_with_args(&spawn_args)
        .with_context(|| "Failed to start restarted daemon")?;

    client
        .await_ready(core::time::Duration::from_mins(2))
        .with_context(|| "Restarted daemon did not become ready in time")?;

    let status = client.status();
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
