//! `uffs mcp {start|status|stats|stop|kill|restart|run}` subcommand handlers.
//!
//! Every lifecycle command targets the **MCP server** process.  The daemon
//! is transparent infrastructure — it auto-starts when the MCP server
//! needs it, and its lifecycle is managed via `uffs daemon …`.
//!
//! The MCP server writes its own PID file (`~/.uffs/mcp-server.pid`) so
//! that `status`, `stop`, `kill`, and `restart` can find it.

use anyhow::{Context, Result};
use uffs_client::connect::UffsClient;

use crate::args::McpAction;

/// Execute an MCP management action.
pub async fn mcp(action: &McpAction) -> Result<()> {
    match action {
        McpAction::Start {
            mft_file,
            data_dir,
            no_cache,
            port,
            bind,
        } => mcp_start(mft_file, data_dir.as_deref(), *no_cache, bind, *port).await,
        McpAction::Status => mcp_status().await,
        McpAction::Stats => mcp_stats().await,
        McpAction::Stop => {
            mcp_stop();
            Ok(())
        }
        McpAction::Kill { port, bind } => {
            mcp_kill(*port, bind);
            Ok(())
        }
        McpAction::Restart => {
            mcp_restart();
            Ok(())
        }
        McpAction::Reload => mcp_reload().await,
        McpAction::Serve {
            port,
            bind,
            data_dir,
            mft_files,
        } => mcp_serve(bind, *port, mft_files, data_dir.as_deref()).await,
        McpAction::Run {
            mft_files,
            data_dir,
            idle_timeout,
        } => mcp_run(mft_files, data_dir.as_deref(), *idle_timeout).await,
    }
}

// ── run (hidden) ────────────────────────────────────────────────────

/// `uffs mcp run` — run the MCP server in-process on stdin/stdout.
///
/// Invoked by AI hosts.  Writes its own PID file, connects to the daemon
/// (auto-starts if needed), and serves MCP over stdio.
async fn mcp_run(
    mft_files: &[std::path::PathBuf],
    data_dir: Option<&std::path::Path>,
    idle_timeout: u64,
) -> Result<()> {
    // MCP uses stderr for logging — stdout is the protocol channel.
    // Honour UFFS_LOG / UFFS_LOG_FILE for diagnostic sessions, matching
    // the daemon pattern:
    //   UFFS_LOG=trace UFFS_LOG_FILE=/tmp/mcp.log uffs mcp run …
    let log_spec = std::env::var("UFFS_LOG").unwrap_or_else(|_| "info".to_owned());
    let log_file = std::env::var("UFFS_LOG_FILE")
        .ok()
        .map(std::path::PathBuf::from);

    // init_mcp_tracing writes to stderr (never stdout) and optionally to a
    // file.  The returned guard must be held until the process exits.
    let _guard = uffs_mcp::init_mcp_tracing(&log_spec, log_file.as_deref());

    let config = uffs_mcp::McpConfig {
        daemon_spawn_args: build_daemon_args(mft_files, data_dir),
        idle_timeout_secs: idle_timeout,
    };

    uffs_mcp::run_mcp_server_with_config(&config)
        .await
        .with_context(|| "MCP server exited with error")
}

// ── serve (hidden — in-process HTTP) ────────────────────────────────

/// `uffs mcp serve` — run the MCP HTTP gateway in-process.
///
/// This is the entry point spawned by `mcp start`.  It writes the PID
/// file (including transport=http:bind:port), **eagerly starts the daemon**
/// (the backbone of every MCP tool call), then starts the HTTP gateway
/// and blocks until shutdown.
async fn mcp_serve(
    bind: &str,
    port: u16,
    mft_files: &[std::path::PathBuf],
    data_dir: Option<&std::path::Path>,
) -> Result<()> {
    let log_spec = std::env::var("UFFS_LOG").unwrap_or_else(|_| "info".to_owned());
    let log_file = std::env::var("UFFS_LOG_FILE")
        .ok()
        .map(std::path::PathBuf::from);
    let _guard = uffs_mcp::init_mcp_tracing(&log_spec, log_file.as_deref());

    let daemon_args = build_daemon_args(mft_files, data_dir);

    // ── Eagerly start the daemon ────────────────────────────────────
    // The MCP server is useless without the daemon — every tool call
    // routes through it.  Start it now so the first MCP request doesn't
    // have to wait 30+ seconds for daemon boot + index loading.
    tracing::info!("Ensuring daemon is running before HTTP gateway starts...");
    let mut client = UffsClient::connect_with_args(&daemon_args)
        .await
        .with_context(|| "Failed to start daemon")?;

    client
        .await_ready(core::time::Duration::from_mins(2))
        .await
        .with_context(|| "Daemon did not become ready in time")?;

    if let Ok(resp) = client.status().await {
        tracing::info!(pid = resp.pid, "Daemon ready");
    }
    // Drop the bootstrap connection — each MCP session opens its own.
    drop(client);

    // ── Start the HTTP gateway ──────────────────────────────────────
    let transport = format!("http:{bind}:{port}");
    uffs_mcp::write_mcp_pid_file_full(&transport, data_dir, mft_files, false);

    let addr: core::net::SocketAddr = format!("{bind}:{port}")
        .parse()
        .with_context(|| format!("Invalid bind address: {bind}:{port}"))?;

    let config = uffs_mcp::http::HttpGatewayConfig {
        bind_addr: addr,
        auth_token: None, // Local only — no auth needed.
        daemon_spawn_args: daemon_args,
    };

    let result = uffs_mcp::http::run_gateway(config).await;

    // Clean up PID file on exit.
    uffs_mcp::remove_mcp_pid_file();
    result
}

// ── start ───────────────────────────────────────────────────────────

/// `uffs mcp start` — start the MCP HTTP server as a background service.
///
/// Spawns `uffs mcp serve` as a detached child process, then polls
/// the `/health` endpoint until the server is ready.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
async fn mcp_start(
    mft_files: &[std::path::PathBuf],
    data_dir: Option<&std::path::Path>,
    no_cache: bool,
    bind: &str,
    port: u16,
) -> Result<()> {
    let daemon_args = {
        let mut args = build_daemon_args(mft_files, data_dir);
        if no_cache {
            args.push("--no-cache".to_owned());
        }
        args
    };

    if !cfg!(windows) && daemon_args.is_empty() {
        anyhow::bail!(
            "No MFT data sources specified.\n\
             Provide --mft-file <path> or --data-dir <path>."
        );
    }

    // Pre-flight: check if the full stack (gateway + daemon) is already
    // healthy.  If the gateway is alive but the daemon is dead, restart
    // the daemon only — leave the gateway alone.
    let gateway_alive =
        uffs_mcp::is_mcp_server_running().is_some() || port_is_occupied(bind, port).await;

    if gateway_alive && preflight_reclaim_or_reuse(bind, port, &daemon_args).await? {
        return Ok(());
    }

    // Build the `uffs mcp serve` command.
    let exe = std::env::current_exe().with_context(|| "Failed to get current exe path")?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["mcp", "serve", "--bind", bind, "--port", &port.to_string()]);
    for arg in &daemon_args {
        cmd.arg(arg);
    }
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    // Ensure the detached gateway always has a log file — stderr is null,
    // so without UFFS_LOG_FILE all diagnostic output is lost.
    if std::env::var("UFFS_LOG_FILE").is_err() {
        let default_log = dirs_next::data_local_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
            .join("uffs")
            .join("mcp-gateway.log");
        cmd.env("UFFS_LOG_FILE", &default_log);
    }

    println!("Starting MCP HTTP server on {bind}:{port}...");

    let mut child = cmd.spawn().with_context(|| "Failed to spawn MCP server")?;
    let pid = child.id();
    println!("  Spawned (PID {pid})");

    // Poll /health until ready.  Allow up to 3 minutes: `mcp serve`
    // eagerly starts the daemon and waits for index loading before
    // binding the HTTP port, which can take 2+ minutes on cold starts.
    let health_url = format!("http://{bind}:{port}/health");
    let deadline = std::time::Instant::now() + core::time::Duration::from_mins(3);
    let mut ready = false;

    while std::time::Instant::now() < deadline {
        tokio::time::sleep(core::time::Duration::from_millis(250)).await;

        // Check if the child exited (crashed).
        if let Some(exit_status) = child.try_wait().ok().flatten() {
            anyhow::bail!(
                "MCP server process (PID {pid}) exited immediately \
                 (status: {exit_status}).  Run with logging to diagnose:\n\
                 \n\
                 \x20 UFFS_LOG=debug UFFS_LOG_FILE=/tmp/mcp.log uffs mcp serve \
                 --port {port} --data-dir <path>"
            );
        }

        if let Ok(resp) = reqwest_lite_get(&health_url).await
            && resp == "ok"
        {
            ready = true;
            break;
        }
    }

    if ready {
        // Final check: make sure our child is still alive (the health
        // response could have come from a stale process on the same port).
        if child.try_wait().ok().flatten().is_some() {
            anyhow::bail!(
                "Health check passed but spawned process (PID {pid}) is no longer alive.\n\
                 A stale server on port {port} may have answered.  Kill it first."
            );
        }
        println!("  MCP HTTP server ready at http://{bind}:{port}/mcp");
        println!("  Health:  http://{bind}:{port}/health");
        println!("  Status:  http://{bind}:{port}/status");
    } else {
        println!("  ⚠ Server spawned but /health not reachable within 3 minutes.");
        println!("  Check logs: UFFS_LOG=debug UFFS_LOG_FILE=/tmp/mcp.log uffs mcp serve");
    }
    Ok(())
}

/// Check whether a TCP port is already occupied.
async fn port_is_occupied(bind: &str, port: u16) -> bool {
    let addr = format!("{bind}:{port}");
    tokio::net::TcpStream::connect(&addr).await.is_ok()
}

/// Deep health check when the target port is already occupied.
///
/// Checks both the HTTP gateway (`/health`) **and** the daemon behind it.
/// - **Both healthy** → "already running", returns `Ok(true)`.
/// - **Gateway ✓, daemon ✗** → restarts the daemon only (the gateway reconnects
///   lazily on the next tool call), returns `Ok(true)`.
/// - **Gateway dead / stale port** → kills port occupant, reclaims port,
///   returns `Ok(false)` so the caller spawns a fresh gateway + daemon.
///
/// # Errors
///
/// Returns an error if the port cannot be reclaimed after killing.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
async fn preflight_reclaim_or_reuse(bind: &str, port: u16, daemon_args: &[String]) -> Result<bool> {
    let health_url = format!("http://{bind}:{port}/health");
    let gateway_ok = reqwest_lite_get(&health_url)
        .await
        .is_ok_and(|body| body == "ok");

    if !gateway_ok {
        // Gateway is dead / stale — kill whatever is on the port and let
        // the caller start fresh.
        println!("  Stale process on port {port} is not healthy — killing it...");
        let tracked_pid = uffs_mcp::parse_mcp_pid_file().map(|(pid, _ts)| pid);
        if let Some(pid) = tracked_pid {
            signal_pid(pid, true);
        }
        kill_process_on_port(port, tracked_pid.unwrap_or(0));
        uffs_mcp::remove_mcp_pid_file();
        tokio::time::sleep(core::time::Duration::from_secs(1)).await;

        if port_is_occupied(bind, port).await {
            anyhow::bail!(
                "Port {port} is still in use after killing the stale process.\n\
                 Check manually:  lsof -i :{port}"
            );
        }
        return Ok(false);
    }

    // Gateway is alive — check the daemon.
    let daemon_ok = match UffsClient::connect_raw().await {
        Ok(mut client) => client.status().await.is_ok(),
        Err(_) => false,
    };

    if daemon_ok {
        println!("MCP HTTP server already running on {bind}:{port} (gateway ✓, daemon ✓).");
        // Reload stale stdio sessions so they pick up the current binary.
        reload_stale_stdio_sessions();
        return Ok(true);
    }

    // Gateway is alive but daemon is dead — restart daemon only.
    // The gateway's lazy `ClientSlot` will reconnect on the next tool call.
    println!("  Gateway on port {port} is alive but daemon is unreachable.");
    println!("  Restarting daemon...");
    let mut client = UffsClient::connect_with_args(daemon_args)
        .await
        .with_context(|| "Failed to restart daemon")?;
    client
        .await_ready(core::time::Duration::from_mins(2))
        .await
        .with_context(|| "Daemon did not become ready in time")?;
    println!("  Daemon restarted — gateway on {bind}:{port} is ready.");
    reload_stale_stdio_sessions();
    Ok(true)
}

/// Minimal HTTP GET — no external deps needed.
async fn reqwest_lite_get(raw_url: &str) -> Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Parse url to get host:port and path.
    let stripped = raw_url.strip_prefix("http://").unwrap_or(raw_url);
    let (host_port, rel_path) = stripped.split_once('/').unwrap_or((stripped, ""));
    let abs_path = format!("/{rel_path}");

    let stream = tokio::net::TcpStream::connect(host_port).await?;
    let (mut reader, mut writer) = stream.into_split();

    let request =
        format!("GET {abs_path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n");
    writer.write_all(request.as_bytes()).await?;

    let mut response = Vec::new();
    reader.read_to_end(&mut response).await?;

    let text = String::from_utf8_lossy(&response);
    // Extract body after \r\n\r\n.
    Ok(text
        .split_once("\r\n\r\n")
        .map_or_else(|| text.to_string(), |(_, body)| body.trim().to_owned()))
}

// ── status ──────────────────────────────────────────────────────────

/// `uffs mcp status` — show MCP server process status + backend info.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
async fn mcp_status() -> Result<()> {
    println!("uffs-mcp v{}", env!("CARGO_PKG_VERSION"));
    println!();

    // MCP server process status.
    match uffs_mcp::parse_mcp_pid_file_full() {
        Some(info) => {
            let alive = uffs_mcp::is_mcp_server_running().is_some();
            let uptime_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |dur| dur.as_secs().saturating_sub(info.start_ts));
            let uptime = core::time::Duration::from_secs(uptime_secs);
            if alive {
                println!("MCP server:    running (PID {})", info.pid);
                println!("  Transport:   {}", info.transport);
                println!(
                    "  Uptime:      {}",
                    uffs_core::format::format_duration(uptime)
                );

                // If HTTP transport, probe the /health endpoint.
                if let Some((bind, port)) = info.http_addr() {
                    let url = format!("http://{bind}:{port}/health");
                    match reqwest_lite_get(&url).await {
                        Ok(body) if body == "ok" => {
                            println!("  Health:      ✓ (http://{bind}:{port}/health)");
                        }
                        Ok(body) => {
                            println!("  Health:      ⚠ unexpected: {body}");
                        }
                        Err(err) => {
                            println!("  Health:      ✗ unreachable ({err})");
                        }
                    }
                }
            } else {
                println!(
                    "MCP server:    not running (stale PID file, PID {})",
                    info.pid
                );
            }
        }
        None => {
            println!("MCP server:    not running (no PID file)");
        }
    }

    // Backend daemon status.
    println!();
    if let Ok(mut client) = UffsClient::connect_raw().await {
        if let Ok(status) = client.status().await {
            println!("Daemon:        reachable (PID {})", status.pid);
            let state = match &status.status {
                uffs_client::protocol::DaemonStatus::Ready => "Ready",
                uffs_client::protocol::DaemonStatus::Loading { .. } => "Loading",
                uffs_client::protocol::DaemonStatus::Refreshing { .. } => "Refreshing",
            };
            println!("  Status:      {state}");
        } else {
            println!("Daemon:        connected but not responding");
        }
    } else {
        println!("Daemon:        not running");
        println!("  (will auto-start when MCP server connects)");
    }

    Ok(())
}

// ── stats ───────────────────────────────────────────────────────────

/// `uffs mcp stats` — show MCP server performance stats.
///
/// Queries the daemon for metrics (since the MCP server routes all queries
/// through it).  The MCP server itself is stateless — all query stats
/// live in the daemon.
#[expect(
    clippy::print_stdout,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "CLI output; f64→u64 truncation acceptable for display"
)]
async fn mcp_stats() -> Result<()> {
    // MCP server process info.
    match uffs_mcp::is_mcp_server_running() {
        Some(pid) => println!("MCP server PID: {pid}"),
        None => println!("MCP server:     not running"),
    }

    // Query stats from daemon backend.
    let Ok(mut client) = UffsClient::connect_raw().await else {
        println!("Daemon:         not running — no stats available.");
        return Ok(());
    };

    let stats = client
        .stats()
        .await
        .with_context(|| "Failed to query stats from daemon")?;

    let fmt = uffs_core::format::format_duration;
    let uptime = core::time::Duration::from_secs(stats.uptime_secs);
    let startup = core::time::Duration::from_millis(stats.startup_duration_ms);
    let avg_query = core::time::Duration::from_micros(stats.avg_query_time_us as u64);
    let total_query = core::time::Duration::from_micros(stats.total_query_time_us);

    println!();
    println!("═══ Performance Stats ═══");
    println!("Backend uptime:    {}", fmt(uptime));
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
    Ok(())
}

// ── stop ────────────────────────────────────────────────────────────

/// `uffs mcp stop` — gracefully stop the MCP server.
///
/// Sends SIGTERM (Unix) or taskkill (Windows) to the MCP server process.
/// The daemon continues running independently.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn mcp_stop() {
    let Some(pid) = uffs_mcp::is_mcp_server_running() else {
        println!("MCP server is not running.");
        return;
    };

    println!("Stopping MCP server (PID {pid})...");
    signal_pid(pid, false);
    println!("MCP server stop signal sent.");
    println!("  (The daemon continues running independently.)");
}

// ── kill ────────────────────────────────────────────────────────────

/// `uffs mcp kill` — force-kill the MCP server + clean up PID file.
///
/// Kills the PID-file-tracked process (if any) **and** scans `port`
/// for orphaned gateway processes that outlived their PID file.
/// The daemon is NOT affected.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn mcp_kill(port: u16, _bind: &str) {
    let pid_path = uffs_mcp::mcp_pid_file_path();
    let mut killed_any = false;

    // 1. Kill the process tracked in the PID file.
    let tracked_pid = if let Some(info) = uffs_mcp::parse_mcp_pid_file_full() {
        println!("Killing MCP server (PID {})...", info.pid);
        signal_pid(info.pid, true);
        killed_any = true;

        // Also kill whatever the PID file says about the HTTP transport.
        if let Some((_file_bind, file_port)) = info.http_addr() {
            kill_process_on_port(file_port, info.pid);
        }
        info.pid
    } else {
        println!("No MCP server PID file found.");
        0
    };

    // 2. Always scan the specified port for orphaned processes.
    kill_process_on_port(port, tracked_pid);

    // Always clean up stale PID file.
    drop(std::fs::remove_file(&pid_path));
    if killed_any {
        println!("MCP server PID file cleaned up.");
    }
    println!("  (The daemon is not affected.)");
}

/// Try to find and kill any process listening on `port`.
///
/// `skip_pid` is a PID we already killed (to avoid double-killing).
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn kill_process_on_port(port: u16, skip_pid: u32) {
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
    #[cfg(not(unix))]
    {
        let _ = (port, skip_pid);
    }
}

// ── restart ─────────────────────────────────────────────────────────

/// `uffs mcp restart` — kill the running MCP server so the AI host
/// respawns it (or the user can run `uffs mcp start` again).
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn mcp_restart() {
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
fn reload_stale_stdio_sessions() {
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
fn process_start_time(pid: u32) -> Option<std::time::SystemTime> {
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
fn parse_ps_etime(etime: &str) -> core::time::Duration {
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
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
#[expect(
    clippy::too_many_lines,
    reason = "multi-component reload with daemon + gateway branches"
)]
async fn mcp_reload() -> Result<()> {
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
                mcp_start(
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
struct GatewayConfig {
    /// `--bind` value.
    bind: String,
    /// `--port` value.
    port: u16,
    /// `--data-dir` value (if any).
    data_dir: Option<std::path::PathBuf>,
    /// `--mft-file` values.
    mft_files: Vec<std::path::PathBuf>,
    /// `--no-cache` flag.
    no_cache: bool,
}

/// Read the gateway config from a running process's command line.
///
/// Runs `ps -p <pid> -o args=` and parses out `--port`, `--bind`,
/// `--data-dir`, `--mft-file`, and `--no-cache`.
fn read_gateway_config(pid: u32) -> Option<GatewayConfig> {
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
fn find_mcp_run_pids() -> Vec<u32> {
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
fn resolve_parent_name(child_pid: u32) -> Option<String> {
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
fn build_daemon_args(
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
fn signal_pid_hup(pid: u32) {
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
fn signal_pid(pid: u32, force: bool) {
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
