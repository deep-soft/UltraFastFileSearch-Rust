// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! MCP process management utilities — signal, discover, reload helpers.
//!
//! Extracted from `mcp_mgmt.rs` for file size policy compliance.  These
//! functions handle the OS-level process discovery, signal delivery, and
//! config parsing that the higher-level MCP lifecycle commands depend on.

use anyhow::Result;

/// Try to find and kill any process listening on `port`.
///
/// `skip_pid` is a PID we already killed (to avoid double-killing).
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
pub(super) fn kill_process_on_port(port: u16, skip_pid: u32) {
    #[cfg(unix)]
    {
        // Use lsof to find the PID listening on the port.
        let Ok(output) = std::process::Command::new("lsof")
            .args(["-ti", &format!(":{port}")])
            .output()
        else {
            return;
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if let Ok(pid) = line.trim().parse::<u32>()
                && pid != skip_pid
                && pid != std::process::id()
            {
                println!("  Also killing stale process on port {port} (PID {pid})...");
                signal_pid(pid, true);
            }
        }
    }
    #[cfg(windows)]
    {
        // Use `netstat -ano` to find PIDs listening on the port.
        let Ok(output) = std::process::Command::new("netstat")
            .args(["-ano", "-p", "TCP"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
        else {
            return;
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let port_suffix = format!(":{port}");
        for line in stdout.lines() {
            // Lines look like: "  TCP    127.0.0.1:18080    0.0.0.0:0    LISTENING
            // 12345"
            let trimmed = line.trim();
            if !trimmed.contains("LISTENING") {
                continue;
            }
            let fields: Vec<&str> = trimmed.split_whitespace().collect();
            // fields: [TCP, local_addr, foreign_addr, LISTENING, PID]
            if fields.len() < 5 {
                continue;
            }
            let local_addr = fields[1];
            if !local_addr.ends_with(&port_suffix) {
                continue;
            }
            let Some(pid) = fields[4].parse::<u32>().ok() else {
                continue;
            };
            if pid != skip_pid && pid != std::process::id() && pid != 0 {
                println!("  Also killing stale process on port {port} (PID {pid})...");
                signal_pid(pid, true);
            }
        }
    }
}

// ── restart ─────────────────────────────────────────────────────────

/// `uffs mcp restart` — kill the running MCP server so the AI host
/// respawns it (or the user can run `uffs mcp start` again).
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
pub(super) fn mcp_restart() {
    let Some(pid) = uffs_mcp::is_mcp_server_running() else {
        println!("MCP server is not running — nothing to restart.");
        println!("  Start it with: uffs mcp start");
        return;
    };

    println!("Stopping MCP server (PID {pid})...");
    signal_pid(pid, true);
    drop(std::fs::remove_file(uffs_mcp::mcp_pid_file_path()));
    println!("MCP server killed.");
    println!("  The AI host will respawn it, or run: uffs mcp start");
    println!("  (The daemon continues running — no re-index needed.)");
}

// ── stdio session helpers ───────────────────────────────────────────

/// Check for stale stdio MCP sessions and reload them.
///
/// Compares the modification time of the current binary against each
/// running `uffs mcp run` process's binary.  Sends SIGHUP to stale
/// sessions so their AI hosts respawn with the updated binary.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
pub(super) fn reload_stale_stdio_sessions() {
    let Ok(current_exe) = std::env::current_exe() else {
        return;
    };
    let current_mtime = std::fs::metadata(&current_exe)
        .and_then(|meta| meta.modified())
        .ok();

    let stdio_pids = find_mcp_run_pids();
    if stdio_pids.is_empty() {
        return;
    }

    // A process is stale if it was started before the current binary
    // was last modified (i.e. a rebuild happened after spawn).
    let mut reloaded: u32 = 0;
    for proc_pid in stdio_pids {
        let proc_start = process_start_time(proc_pid);
        let is_stale = match (&current_mtime, proc_start) {
            (Some(bin_mtime), Some(started)) => started < *bin_mtime,
            // Can't determine — assume stale to be safe.
            _ => true,
        };

        if is_stale {
            let parent = resolve_parent_name(proc_pid);
            let host = parent.as_deref().unwrap_or("unknown");
            println!("  Reloading stale stdio session PID {proc_pid} (parent: {host})...");
            signal_pid_hup(proc_pid);
            reloaded += 1;
        }
    }
    if reloaded > 0 {
        println!("  Sent SIGHUP to {reloaded} stale stdio session(s) — hosts will respawn.");
    }
}

/// Get the start time of a process via `ps -p <pid> -o etime=`.
///
/// Returns the approximate `SystemTime` when the process was spawned.
pub(super) fn process_start_time(pid: u32) -> Option<std::time::SystemTime> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "etime="])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let etime_str = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let uptime = parse_ps_etime(&etime_str);
    Some(std::time::SystemTime::now() - uptime)
}

/// Parse `ps` elapsed time format: `[[dd-]hh:]mm:ss`.
pub(super) fn parse_ps_etime(etime: &str) -> core::time::Duration {
    let mut total_secs: u64 = 0;
    let (days_part, time_part) = if let Some((days, rest)) = etime.split_once('-') {
        (days.parse::<u64>().unwrap_or(0), rest)
    } else {
        (0, etime)
    };
    total_secs += days_part * 86400;
    let mut parts = time_part.rsplit(':');
    if let Some(ss) = parts.next() {
        total_secs += ss.parse::<u64>().unwrap_or(0);
    }
    if let Some(mm) = parts.next() {
        total_secs += mm.parse::<u64>().unwrap_or(0) * 60;
    }
    if let Some(hh) = parts.next() {
        total_secs += hh.parse::<u64>().unwrap_or(0) * 3600;
    }
    core::time::Duration::from_secs(total_secs)
}

// ── reload ──────────────────────────────────────────────────────────

/// `uffs mcp reload` — reload all stale MCP components to pick up
/// the current binary.
///
/// Checks each component's start time against the on-disk binary mtime.
/// Only kills and restarts components that are actually stale:
///
/// 1. **Daemon** — kill + auto-restart on next connection.
/// 2. **HTTP gateway** — read config from process cmdline, kill, restart.
/// 3. **Stdio sessions** — SIGHUP so AI hosts respawn.
///
/// No arguments needed — config is inferred from running processes.
#[expect(
    clippy::print_stdout,
    clippy::too_many_lines,
    reason = "CLI user-facing output; sequential reload pipeline: find daemon → \
              check exe freshness → reload HTTP gateway → signal stdio sessions"
)]
pub(super) async fn mcp_reload() -> Result<()> {
    use uffs_client::connect::{UffsClient, pid_file_path, socket_path};

    let exe_mtime = std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok());

    let Some(bin_mtime) = exe_mtime else {
        anyhow::bail!("Cannot determine current binary mtime.");
    };

    println!("Reloading MCP stack...");
    let mut anything_reloaded = false;

    // ── Pre-flight: read gateway config BEFORE killing anything ────
    // On non-Windows, the daemon needs data sources to restart.
    // Read from the PID file first (persisted at startup), fall back
    // to the process cmdline for backward compatibility.
    let pid_info = uffs_mcp::parse_mcp_pid_file_full();
    let gw_pid = pid_info
        .as_ref()
        .filter(|_info| uffs_mcp::is_mcp_server_running().is_some())
        .map(|info| info.pid);

    let gw_config = {
        // Try PID file data sources first.
        let from_pid_file = pid_info.as_ref().and_then(|info| {
            let (bind, port) = info.http_addr()?;
            Some(GatewayConfig {
                bind: bind.to_owned(),
                port,
                data_dir: info.data_dir.clone(),
                mft_files: info.mft_files.clone(),
                no_cache: info.no_cache,
            })
        });
        // Fall back to process cmdline if PID file had no data sources.
        if from_pid_file
            .as_ref()
            .is_some_and(|cfg| cfg.data_dir.is_some() || !cfg.mft_files.is_empty())
        {
            from_pid_file
        } else {
            gw_pid.and_then(read_gateway_config).or(from_pid_file)
        }
    };

    let can_restart_daemon = cfg!(windows)
        || gw_config
            .as_ref()
            .is_some_and(|cfg| cfg.data_dir.is_some() || !cfg.mft_files.is_empty());

    // ── 1. Daemon ───────────────────────────────────────────────────
    if let Ok(mut client) = UffsClient::connect_raw().await
        && let Ok(status) = client.status().await
    {
        let uptime = core::time::Duration::from_secs(status.uptime_secs);
        let started = std::time::SystemTime::now() - uptime;
        if started < bin_mtime {
            if can_restart_daemon {
                println!("  ✗ Daemon PID {} is stale — killing...", status.pid);
                signal_pid(status.pid, true);
                drop(std::fs::remove_file(pid_file_path()));
                drop(std::fs::remove_file(socket_path()));
                std::thread::sleep(core::time::Duration::from_millis(300));
                anything_reloaded = true;
            } else {
                println!(
                    "  ✗ Daemon PID {} is stale but no data sources found — \
                         skipping kill to avoid losing the running instance.",
                    status.pid
                );
                println!("    Restart manually: uffs mcp start --data-dir <path>");
            }
        } else {
            println!("  ✓ Daemon PID {} is current.", status.pid);
        }
    }

    // ── 2. HTTP gateway ─────────────────────────────────────────────

    if let (Some(pid), Some(config)) = (gw_pid, &gw_config) {
        let gw_start = process_start_time(pid);
        let is_stale = gw_start.is_none_or(|st| st < bin_mtime);
        if is_stale {
            // On non-Windows, the gateway needs data sources to
            // auto-start the daemon.  If none were captured from the
            // process cmdline, refuse to restart — otherwise we'd kill
            // the old gateway and fail to start a new one, leaving
            // everything dead.
            let has_data_sources =
                cfg!(windows) || config.data_dir.is_some() || !config.mft_files.is_empty();
            if has_data_sources {
                println!("  ✗ HTTP gateway PID {pid} is stale — restarting...");
                signal_pid(pid, true);
                drop(std::fs::remove_file(uffs_mcp::mcp_pid_file_path()));
                std::thread::sleep(core::time::Duration::from_millis(500));
                println!(
                    "  ↻ Restarting HTTP gateway on {}:{}...",
                    config.bind, config.port
                );
                super::mcp_start(
                    &config.mft_files,
                    config.data_dir.as_deref(),
                    config.no_cache,
                    &config.bind,
                    config.port,
                )
                .await?;
                anything_reloaded = true;
            } else {
                println!(
                    "  ✗ HTTP gateway PID {pid} is stale but has no data sources — \
                     cannot restart automatically."
                );
                println!(
                    "    Kill it manually and restart with: \
                     uffs mcp start --data-dir <path>"
                );
            }
        } else {
            println!("  ✓ HTTP gateway PID {pid} is current.");
        }
    } else if gw_pid.is_none() {
        println!("  No HTTP gateway running.");
        println!("  Start with: uffs mcp start --data-dir <path>");
    }

    // ── 3. Stdio sessions ───────────────────────────────────────────
    let stdio_pids = find_mcp_run_pids();
    for &proc_pid in &stdio_pids {
        let proc_start = process_start_time(proc_pid);
        let is_stale = proc_start.is_none_or(|st| st < bin_mtime);
        if is_stale {
            let parent = resolve_parent_name(proc_pid);
            let host = parent.as_deref().unwrap_or("unknown");
            println!("  ↻ SIGHUP stale stdio PID {proc_pid} (parent: {host})");
            signal_pid_hup(proc_pid);
            anything_reloaded = true;
        }
    }

    if anything_reloaded {
        println!("Reload complete ✓");
    } else {
        println!("Everything is current — nothing to reload.");
    }
    Ok(())
}

/// Config extracted from a running `uffs mcp serve` process cmdline.
pub(super) struct GatewayConfig {
    /// `--bind` value.
    pub(super) bind: String,
    /// `--port` value.
    pub(super) port: u16,
    /// `--data-dir` value (if any).
    pub(super) data_dir: Option<std::path::PathBuf>,
    /// `--mft-file` values.
    pub(super) mft_files: Vec<std::path::PathBuf>,
    /// `--no-cache` flag.
    pub(super) no_cache: bool,
}

/// Read the gateway config from a running process's command line.
///
/// Runs `ps -p <pid> -o args=` and parses out `--port`, `--bind`,
/// `--data-dir`, `--mft-file`, and `--no-cache`.
pub(super) fn read_gateway_config(pid: u32) -> Option<GatewayConfig> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "args="])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let cmdline = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if cmdline.is_empty() {
        return None;
    }

    let args: Vec<&str> = cmdline.split_whitespace().collect();

    let mut bind = "127.0.0.1".to_owned();
    let mut port: u16 = 8080;
    let mut data_dir: Option<std::path::PathBuf> = None;
    let mut mft_files: Vec<std::path::PathBuf> = Vec::new();
    let mut no_cache = false;

    let mut i = 0;
    while i < args.len() {
        match args.get(i).copied() {
            Some("--bind") => {
                if let Some(&val) = args.get(i + 1) {
                    val.clone_into(&mut bind);
                    i += 1;
                }
            }
            Some("--port") => {
                if let Some(&val) = args.get(i + 1) {
                    port = val.parse().unwrap_or(8080);
                    i += 1;
                }
            }
            Some("--data-dir") => {
                if let Some(&val) = args.get(i + 1) {
                    data_dir = Some(std::path::PathBuf::from(val));
                    i += 1;
                }
            }
            Some("--mft-file") => {
                if let Some(&val) = args.get(i + 1) {
                    for part in val.split(',') {
                        mft_files.push(std::path::PathBuf::from(part));
                    }
                    i += 1;
                }
            }
            Some("--no-cache") => {
                no_cache = true;
            }
            _ => {}
        }
        i += 1;
    }

    Some(GatewayConfig {
        bind,
        port,
        data_dir,
        mft_files,
        no_cache,
    })
}

/// Find PIDs of running `uffs mcp run` processes.
pub(super) fn find_mcp_run_pids() -> Vec<u32> {
    let Ok(raw_output) = std::process::Command::new("ps")
        .args(["-eo", "pid,args"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&raw_output.stdout);
    let my_pid = std::process::id();

    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let proc_pid: u32 = fields.next()?.parse().ok()?;
            if proc_pid == my_pid {
                return None;
            }
            let cmdline: String = fields.collect::<Vec<_>>().join(" ");
            let is_mcp_run = cmdline.contains("mcp")
                && cmdline.contains("run")
                && !cmdline.contains("serve")
                && !cmdline.contains("start")
                && !cmdline.contains("kill");
            is_mcp_run.then_some(proc_pid)
        })
        .collect()
}

/// Resolve the name of a parent process by PID.
pub(super) fn resolve_parent_name(child_pid: u32) -> Option<String> {
    // Get the PPID first, then resolve its name.
    let ppid_output = std::process::Command::new("ps")
        .args(["-p", &child_pid.to_string(), "-o", "ppid="])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let ppid: u32 = String::from_utf8_lossy(&ppid_output.stdout)
        .trim()
        .parse()
        .ok()?;
    if ppid == 0 {
        return None;
    }
    let comm_output = std::process::Command::new("ps")
        .args(["-p", &ppid.to_string(), "-o", "comm="])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&comm_output.stdout)
        .trim()
        .to_owned();
    if name.is_empty() {
        return None;
    }
    Some(name.rsplit('/').next().unwrap_or(&name).to_owned())
}

// ── helpers ─────────────────────────────────────────────────────────

/// Build `--mft-file` / `--data-dir` args for daemon auto-start.
pub(super) fn build_daemon_args(
    mft_files: &[std::path::PathBuf],
    data_dir: Option<&std::path::Path>,
) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(dir) = data_dir {
        args.push("--data-dir".to_owned());
        args.push(dir.to_string_lossy().into_owned());
    }
    for path in mft_files {
        args.push("--mft-file".to_owned());
        args.push(path.to_string_lossy().into_owned());
    }
    args
}

/// Send SIGHUP to a process (Unix only; no-op on Windows).
///
/// Used to signal stdio MCP sessions to exit so their host can respawn.
pub(super) fn signal_pid_hup(pid: u32) {
    #[cfg(unix)]
    {
        drop(
            std::process::Command::new("kill")
                .args(["-1", &pid.to_string()])
                .output(),
        );
    }
    #[cfg(not(unix))]
    {
        // Windows: no SIGHUP, fall back to SIGTERM equivalent.
        signal_pid(pid, false);
    }
}

/// Send a signal to a process.
///
/// When `force` is true: SIGKILL (Unix) / `/F` (Windows).
/// When `force` is false: SIGTERM (Unix) / normal taskkill (Windows).
pub(super) fn signal_pid(pid: u32, force: bool) {
    #[cfg(unix)]
    {
        let sig = if force { "-9" } else { "-15" };
        drop(
            std::process::Command::new("kill")
                .args([sig, &pid.to_string()])
                .output(),
        );
    }
    #[cfg(not(unix))]
    {
        let mut cmd = std::process::Command::new("taskkill");
        if force {
            cmd.arg("/F");
        }
        cmd.args(["/PID", &pid.to_string()]);
        drop(cmd.output());
    }
}
