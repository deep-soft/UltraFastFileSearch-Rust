// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `uffs mcp {start|status|stats|stop|kill|restart|run}` subcommand handlers.
//!
//! Every lifecycle command targets the **MCP server** process.  The daemon
//! is transparent infrastructure — it auto-starts when the MCP server
//! needs it, and its lifecycle is managed via `uffs daemon …`.
//!
//! The MCP server writes its own PID file (`~/.uffs/mcp-server.pid`) so
//! that `status`, `stop`, `kill`, and `restart` can find it.

mod mcp_process;

use anyhow::{Context, Result};
use mcp_process::{
    build_daemon_args, kill_process_on_port, mcp_reload, mcp_restart, reload_stale_stdio_sessions,
    signal_pid,
};
use uffs_client::connect::UffsClient;

use crate::args::McpAction;

/// Execute an MCP management action.
pub(crate) async fn mcp(action: &McpAction) -> Result<()> {
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
#[expect(clippy::print_stdout, reason = "CLI output")]
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
    let avg_query =
        core::time::Duration::from_micros(uffs_mft::f64_to_u64(stats.avg_query_time_us));
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
/// Sends SIGTERM (Unix) or force-kills via `taskkill /F` (Windows) the
/// MCP server process.  The daemon continues running independently.
///
/// On Windows, `taskkill` without `/F` sends `WM_CLOSE` which only works
/// for GUI apps with a message loop.  The MCP HTTP server is a headless
/// tokio process, so `WM_CLOSE` is silently ignored.  We use `/F` to
/// ensure the process actually terminates.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn mcp_stop() {
    let Some(pid) = uffs_mcp::is_mcp_server_running() else {
        println!("MCP server is not running.");
        return;
    };

    println!("Stopping MCP server (PID {pid})...");
    // On Unix, SIGTERM triggers the graceful shutdown handler.
    // On Windows, force-kill is the only reliable option for headless
    // processes (no message loop → WM_CLOSE is ignored).
    signal_pid(pid, cfg!(windows));
    println!("MCP server stopped.");
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
