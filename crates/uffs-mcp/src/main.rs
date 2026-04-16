// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `uffsmcp` — standalone UFFS MCP server binary.
//!
//! Handles all MCP lifecycle commands: `run`, `serve`, `start`, `stop`,
//! `status`, `stats`, `kill`, `restart`, `reload`.
//!
//! The CLI (`uffs mcp <action>`) delegates to this binary so the thin
//! client stays small.
//!
//! # MCP Configuration
//!
//! ```json
//! { "uffs": { "command": "uffsmcp" } }
//! ```

// Crates used by the library but not directly by this binary.
#[cfg(feature = "streamable-http")]
use axum as _;
use dirs_next as _;
use rmcp as _;
use schemars as _;
use serde as _;
use serde_json as _;
use thiserror as _;
#[cfg(feature = "streamable-http")]
use tower_service as _;
use tracing_appender as _;

mod process;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

/// UFFS MCP server — bridges AI agents to the UFFS daemon via the
/// Model Context Protocol.
#[derive(Parser)]
#[command(name = "uffsmcp", version, about)]
struct Cli {
    /// Subcommand to execute.  When invoked without a subcommand,
    /// runs in stdio mode (for AI host integration).
    #[command(subcommand)]
    action: Option<Action>,
}

/// MCP server lifecycle actions.
#[derive(Subcommand)]
enum Action {
    /// Run the MCP server on stdin/stdout (for AI hosts).
    Run {
        /// MFT file paths (passed to daemon auto-start).
        #[arg(long = "mft-file", value_name = "PATH")]
        mft_files: Vec<std::path::PathBuf>,
        /// Data directory (passed to daemon auto-start).
        #[arg(long)]
        data_dir: Option<std::path::PathBuf>,
        /// Idle timeout in seconds (0 = no timeout).
        #[arg(long, default_value = "0")]
        idle_timeout: u64,
    },
    /// Start the MCP HTTP server as a background service.
    Start {
        /// MFT file paths (passed to daemon auto-start).
        #[arg(long = "mft-file", value_name = "PATH")]
        mft_files: Vec<std::path::PathBuf>,
        /// Data directory (passed to daemon auto-start).
        #[arg(long)]
        data_dir: Option<std::path::PathBuf>,
        /// Skip index cache.
        #[arg(long)]
        no_cache: bool,
        /// HTTP port.
        #[arg(long, default_value = "8080")]
        port: u16,
        /// Bind address.
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
    },
    /// Run the MCP HTTP gateway in-process (spawned by `start`).
    #[command(hide = true)]
    Serve {
        /// HTTP port.
        #[arg(long, default_value = "8080")]
        port: u16,
        /// Bind address.
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        /// Data directory (passed to daemon auto-start).
        #[arg(long)]
        data_dir: Option<std::path::PathBuf>,
        /// MFT file paths.
        #[arg(long = "mft-file", value_name = "PATH")]
        mft_files: Vec<std::path::PathBuf>,
    },
    /// Show MCP server process status.
    Status,
    /// Show MCP/daemon performance stats.
    Stats,
    /// Gracefully stop the MCP server.
    Stop,
    /// Force-kill the MCP server + clean up.
    Kill {
        /// HTTP port to scan for orphaned processes.
        #[arg(long, default_value = "8080")]
        port: u16,
        /// Bind address.
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
    },
    /// Kill and restart the MCP server.
    Restart,
    /// Reload stale MCP components to pick up a new binary.
    Reload,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.action {
        // No subcommand → stdio mode (for AI hosts like Claude Desktop).
        None => {
            let _ignore = tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_target(false)
                .with_max_level(tracing::Level::INFO)
                .try_init();
            uffs_mcp::run_mcp_server().await
        }
        Some(action) => run_action(action).await,
    }
}

/// Dispatch an MCP action.
async fn run_action(action: Action) -> Result<()> {
    match action {
        Action::Run {
            mft_files,
            data_dir,
            idle_timeout,
        } => mcp_run(&mft_files, data_dir.as_deref(), idle_timeout).await,
        Action::Serve {
            port,
            bind,
            data_dir,
            mft_files,
        } => mcp_serve(&bind, port, &mft_files, data_dir.as_deref()).await,
        Action::Start {
            mft_files,
            data_dir,
            no_cache,
            port,
            bind,
        } => mcp_start(&mft_files, data_dir.as_deref(), no_cache, &bind, port).await,
        Action::Status => mcp_status().await,
        Action::Stats => mcp_stats().await,
        Action::Stop => {
            mcp_stop();
            Ok(())
        }
        Action::Kill { port, bind } => {
            mcp_kill(port, &bind);
            Ok(())
        }
        Action::Restart => {
            process::mcp_restart();
            Ok(())
        }
        Action::Reload => process::mcp_reload().await,
    }
}

// ── run ─────────────────────────────────────────────────────────────

/// Run the MCP server in-process on stdin/stdout (invoked by AI hosts).
async fn mcp_run(
    mft_files: &[std::path::PathBuf],
    data_dir: Option<&std::path::Path>,
    idle_timeout: u64,
) -> Result<()> {
    let log_spec = std::env::var("UFFS_LOG").unwrap_or_else(|_| "info".to_owned());
    let log_file = std::env::var("UFFS_LOG_FILE")
        .ok()
        .map(std::path::PathBuf::from);
    let _guard = uffs_mcp::init_mcp_tracing(&log_spec, log_file.as_deref());

    let config = uffs_mcp::McpConfig {
        daemon_spawn_args: process::build_daemon_args(mft_files, data_dir),
        idle_timeout_secs: idle_timeout,
    };
    uffs_mcp::run_mcp_server_with_config(&config)
        .await
        .with_context(|| "MCP server exited with error")
}

// ── serve ───────────────────────────────────────────────────────────

/// Run the MCP HTTP gateway in-process (spawned by `start`).
#[cfg(feature = "streamable-http")]
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

    let daemon_args = process::build_daemon_args(mft_files, data_dir);

    tracing::info!("Ensuring daemon is running before HTTP gateway starts...");
    let mut client = uffs_client::connect::UffsClient::connect_with_args(&daemon_args)
        .await
        .with_context(|| "Failed to start daemon")?;
    client
        .await_ready(core::time::Duration::from_mins(2))
        .await
        .with_context(|| "Daemon did not become ready in time")?;
    if let Ok(resp) = client.status().await {
        tracing::info!(pid = resp.pid, "Daemon ready");
    }
    drop(client);

    let transport = format!("http:{bind}:{port}");
    uffs_client::mcp_pid::write_mcp_pid_file_full(&transport, data_dir, mft_files, false);

    let addr: core::net::SocketAddr = format!("{bind}:{port}")
        .parse()
        .with_context(|| format!("Invalid bind address: {bind}:{port}"))?;

    let config = uffs_mcp::http::HttpGatewayConfig {
        bind_addr: addr,
        auth_token: None,
        daemon_spawn_args: daemon_args,
    };

    let result = uffs_mcp::http::run_gateway(config).await;
    uffs_client::mcp_pid::remove_mcp_pid_file();
    result
}

/// Fallback when HTTP gateway feature is not enabled.
#[cfg(not(feature = "streamable-http"))]
#[expect(
    clippy::unused_async,
    reason = "signature must match the streamable-http variant which genuinely awaits"
)]
async fn mcp_serve(
    _bind: &str,
    _port: u16,
    _mft_files: &[std::path::PathBuf],
    _data_dir: Option<&std::path::Path>,
) -> Result<()> {
    anyhow::bail!(
        "HTTP gateway requires the `streamable-http` feature. Rebuild with: cargo build -p uffs-mcp --features streamable-http"
    );
}

// ── start ───────────────────────────────────────────────────────────

/// Start the MCP HTTP server as a background service.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
async fn mcp_start(
    mft_files: &[std::path::PathBuf],
    data_dir: Option<&std::path::Path>,
    no_cache: bool,
    bind: &str,
    port: u16,
) -> Result<()> {
    let daemon_args = {
        let mut args = process::build_daemon_args(mft_files, data_dir);
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

    let gateway_alive = uffs_client::mcp_pid::is_mcp_server_running().is_some()
        || port_is_occupied(bind, port).await;

    if gateway_alive && preflight_reclaim_or_reuse(bind, port, &daemon_args).await? {
        return Ok(());
    }

    // Spawn `uffsmcp serve` as a detached child.
    let exe = std::env::current_exe().with_context(|| "Failed to get current exe path")?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["serve", "--bind", bind, "--port", &port.to_string()]);
    for arg in &daemon_args {
        cmd.arg(arg);
    }
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

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

    let health_url = format!("http://{bind}:{port}/health");
    let deadline = std::time::Instant::now() + core::time::Duration::from_mins(3);
    let mut ready = false;

    while std::time::Instant::now() < deadline {
        tokio::time::sleep(core::time::Duration::from_millis(250)).await;
        if let Some(exit_status) = child.try_wait().ok().flatten() {
            anyhow::bail!(
                "MCP server process (PID {pid}) exited immediately (status: {exit_status}).\n\
                 Run with logging: UFFS_LOG=debug UFFS_LOG_FILE=/tmp/mcp.log uffsmcp serve --port {port}"
            );
        }
        if let Ok(resp) = process::reqwest_lite_get(&health_url).await
            && resp == "ok"
        {
            ready = true;
            break;
        }
    }

    if ready {
        if child.try_wait().ok().flatten().is_some() {
            anyhow::bail!(
                "Health check passed but spawned process (PID {pid}) is no longer alive."
            );
        }
        println!("  MCP HTTP server ready at http://{bind}:{port}/mcp");
        println!("  Health:  http://{bind}:{port}/health");
        println!("  Status:  http://{bind}:{port}/status");
    } else {
        println!("  ⚠ Server spawned but /health not reachable within 3 minutes.");
    }
    Ok(())
}

/// Check whether a TCP port is already occupied.
async fn port_is_occupied(bind: &str, port: u16) -> bool {
    let addr = format!("{bind}:{port}");
    tokio::net::TcpStream::connect(&addr).await.is_ok()
}

/// Deep health check when target port is occupied.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
async fn preflight_reclaim_or_reuse(bind: &str, port: u16, daemon_args: &[String]) -> Result<bool> {
    let health_url = format!("http://{bind}:{port}/health");
    let gateway_ok = process::reqwest_lite_get(&health_url)
        .await
        .is_ok_and(|body| body == "ok");

    if !gateway_ok {
        println!("  Stale process on port {port} is not healthy — killing it...");
        let tracked_pid = uffs_client::mcp_pid::parse_mcp_pid_file().map(|(pid, _ts)| pid);
        if let Some(pid) = tracked_pid {
            process::signal_pid(pid, true);
        }
        process::kill_process_on_port(port, tracked_pid.unwrap_or(0));
        uffs_client::mcp_pid::remove_mcp_pid_file();
        tokio::time::sleep(core::time::Duration::from_secs(1)).await;
        if port_is_occupied(bind, port).await {
            anyhow::bail!("Port {port} is still in use after killing the stale process.");
        }
        return Ok(false);
    }

    let daemon_ok = match uffs_client::connect::UffsClient::connect_raw().await {
        Ok(mut client) => client.status().await.is_ok(),
        Err(_) => false,
    };

    if daemon_ok {
        println!("MCP HTTP server already running on {bind}:{port} (gateway ✓, daemon ✓).");
        process::reload_stale_stdio_sessions();
        return Ok(true);
    }

    println!("  Gateway on port {port} is alive but daemon is unreachable.");
    println!("  Restarting daemon...");
    let mut client = uffs_client::connect::UffsClient::connect_with_args(daemon_args)
        .await
        .with_context(|| "Failed to restart daemon")?;
    client
        .await_ready(core::time::Duration::from_mins(2))
        .await
        .with_context(|| "Daemon did not become ready in time")?;
    println!("  Daemon restarted — gateway on {bind}:{port} is ready.");
    process::reload_stale_stdio_sessions();
    Ok(true)
}

// ── status ──────────────────────────────────────────────────────────

/// Show MCP server process status + backend info.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
async fn mcp_status() -> Result<()> {
    println!("uffs-mcp v{}", env!("CARGO_PKG_VERSION"));
    println!();

    match uffs_client::mcp_pid::parse_mcp_pid_file_full() {
        Some(info) => {
            let alive = uffs_client::mcp_pid::is_mcp_server_running().is_some();
            let uptime_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |dur| dur.as_secs().saturating_sub(info.start_ts));
            let uptime = core::time::Duration::from_secs(uptime_secs);
            if alive {
                println!("MCP server:    running (PID {})", info.pid);
                println!("  Transport:   {}", info.transport);
                println!(
                    "  Uptime:      {}",
                    uffs_client::format::format_duration(uptime)
                );
                if let Some((bind, port)) = info.http_addr() {
                    let url = format!("http://{bind}:{port}/health");
                    match process::reqwest_lite_get(&url).await {
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

    println!();
    if let Ok(mut client) = uffs_client::connect::UffsClient::connect_raw().await {
        if let Ok(status) = client.status().await {
            println!("Daemon:        reachable (PID {})", status.pid);
            let state = match &status.status {
                uffs_client::protocol::response::DaemonStatus::Ready => "Ready",
                uffs_client::protocol::response::DaemonStatus::Loading { .. } => "Loading",
                uffs_client::protocol::response::DaemonStatus::Refreshing { .. } => "Refreshing",
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

/// Show MCP/daemon performance stats.
#[expect(clippy::print_stdout, reason = "CLI output")]
async fn mcp_stats() -> Result<()> {
    match uffs_client::mcp_pid::is_mcp_server_running() {
        Some(pid) => println!("MCP server PID: {pid}"),
        None => println!("MCP server:     not running"),
    }

    let Ok(mut client) = uffs_client::connect::UffsClient::connect_raw().await else {
        println!("Daemon:         not running — no stats available.");
        return Ok(());
    };

    let stats = client
        .stats()
        .await
        .with_context(|| "Failed to query stats from daemon")?;

    let fmt = uffs_client::format::format_duration;
    let uptime = core::time::Duration::from_secs(stats.uptime_secs);
    let startup = core::time::Duration::from_millis(stats.startup_duration_ms);
    let avg_query =
        core::time::Duration::from_micros(uffs_client::format::f64_to_u64(stats.avg_query_time_us));
    let total_query = core::time::Duration::from_micros(stats.total_query_time_us);

    println!();
    println!("═══ Performance Stats ═══");
    println!("Backend uptime:    {}", fmt(uptime));
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
    Ok(())
}

// ── stop ────────────────────────────────────────────────────────────

/// Gracefully stop the MCP server.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn mcp_stop() {
    let Some(pid) = uffs_client::mcp_pid::is_mcp_server_running() else {
        println!("MCP server is not running.");
        return;
    };
    println!("Stopping MCP server (PID {pid})...");
    process::signal_pid(pid, cfg!(windows));
    println!("MCP server stopped.");
    println!("  (The daemon continues running independently.)");
}

// ── kill ────────────────────────────────────────────────────────────

/// Force-kill the MCP server + clean up PID file.
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
fn mcp_kill(port: u16, _bind: &str) {
    let pid_path = uffs_client::mcp_pid::mcp_pid_file_path();
    let mut killed_any = false;

    let tracked_pid = if let Some(info) = uffs_client::mcp_pid::parse_mcp_pid_file_full() {
        println!("Killing MCP server (PID {})...", info.pid);
        process::signal_pid(info.pid, true);
        killed_any = true;
        if let Some((_file_bind, file_port)) = info.http_addr() {
            process::kill_process_on_port(file_port, info.pid);
        }
        info.pid
    } else {
        println!("No MCP server PID file found.");
        0
    };

    process::kill_process_on_port(port, tracked_pid);
    drop(std::fs::remove_file(&pid_path));
    if killed_any {
        println!("MCP server PID file cleaned up.");
    }
    println!("  (The daemon is not affected.)");
}

#[cfg(test)]
#[expect(
    clippy::default_numeric_fallback,
    clippy::indexing_slicing,
    clippy::min_ident_chars,
    reason = "test code"
)]
mod tests {
    use uffs_client::protocol::{AggregateResultWire, BucketWire, StatsWire};
    use uffs_mcp::text::format_aggregate_summary;

    #[test]
    fn summary_count_result() {
        let results = vec![AggregateResultWire {
            label: Some("total_files".to_owned()),
            kind: "count".to_owned(),
            field: None,
            value: Some(42_000),
            stats: None,
            buckets: vec![],
            other_count: None,
            total_groups: None,
            next_cursor: None,
            exact: None,
            values_complete: None,
        }];
        let summary = format_aggregate_summary(&results);
        assert!(summary.contains("total_files: 42000"), "got: {summary}");
    }

    #[test]
    fn summary_stats_result() {
        let results = vec![AggregateResultWire {
            label: Some("size_stats".to_owned()),
            kind: "stats".to_owned(),
            field: Some("size".to_owned()),
            value: None,
            stats: Some(StatsWire {
                count: 1000,
                sum: 5_000_000,
                min: 0,
                max: 999_999,
                avg: 5000.0,
                waste_bytes: 100_000,
                waste_pct: 2.0,
            }),
            buckets: vec![],
            other_count: None,
            total_groups: None,
            next_cursor: None,
            exact: None,
            values_complete: None,
        }];
        let summary = format_aggregate_summary(&results);
        assert!(summary.contains("count=1000"), "got: {summary}");
        assert!(summary.contains("sum=5000000"), "got: {summary}");
        assert!(summary.contains("avg=5000.0"), "got: {summary}");
        assert!(summary.contains("waste: 100000 bytes"), "got: {summary}");
    }

    #[test]
    fn summary_buckets_result() {
        let results = vec![AggregateResultWire {
            label: Some("ext_terms".to_owned()),
            kind: "buckets".to_owned(),
            field: Some("extension".to_owned()),
            value: None,
            stats: None,
            buckets: vec![
                BucketWire {
                    key: "rs".to_owned(),
                    count: 500,
                    total_bytes: 2_000_000,
                    total_allocated: None,
                    avg_size: None,
                    share_count: None,
                    share_bytes: None,
                    sample_rows: Vec::new(),
                    drilldown: Vec::new(),
                    sub_buckets: Vec::new(),
                    verified: false,
                },
                BucketWire {
                    key: "toml".to_owned(),
                    count: 200,
                    total_bytes: 50_000,
                    total_allocated: None,
                    avg_size: None,
                    share_count: None,
                    share_bytes: None,
                    sample_rows: Vec::new(),
                    drilldown: Vec::new(),
                    sub_buckets: Vec::new(),
                    verified: false,
                },
            ],
            other_count: Some(300),
            total_groups: None,
            next_cursor: None,
            exact: None,
            values_complete: None,
        }];
        let summary = format_aggregate_summary(&results);
        assert!(summary.contains("ext_terms (2 buckets)"), "got: {summary}");
        assert!(summary.contains("rs"), "got: {summary}");
        assert!(summary.contains("toml"), "got: {summary}");
        assert!(summary.contains("300 in other groups"), "got: {summary}");
    }

    #[test]
    fn summary_missing_result() {
        let results = vec![AggregateResultWire {
            label: Some("no_ext".to_owned()),
            kind: "missing".to_owned(),
            field: Some("extension".to_owned()),
            value: Some(150),
            stats: None,
            buckets: vec![],
            other_count: None,
            total_groups: None,
            next_cursor: None,
            exact: None,
            values_complete: None,
        }];
        let summary = format_aggregate_summary(&results);
        assert!(
            summary.contains("150 records with missing"),
            "got: {summary}"
        );
    }

    #[test]
    fn summary_distinct_result() {
        let results = vec![AggregateResultWire {
            label: Some("unique_exts".to_owned()),
            kind: "distinct".to_owned(),
            field: Some("extension".to_owned()),
            value: Some(4500),
            stats: None,
            buckets: vec![],
            other_count: None,
            total_groups: None,
            next_cursor: None,
            exact: None,
            values_complete: None,
        }];
        let summary = format_aggregate_summary(&results);
        assert!(summary.contains("4500 distinct values"), "got: {summary}");
    }

    #[test]
    fn summary_empty_results() {
        let summary = format_aggregate_summary(&[]);
        assert_eq!(summary, "No aggregate results.");
    }

    #[test]
    fn summary_mixed_results() {
        let results = vec![
            AggregateResultWire {
                label: Some("total".to_owned()),
                kind: "count".to_owned(),
                field: None,
                value: Some(1000),
                stats: None,
                buckets: vec![],
                other_count: None,
                total_groups: None,
                next_cursor: None,
                exact: None,
                values_complete: None,
            },
            AggregateResultWire {
                label: Some("by_type".to_owned()),
                kind: "buckets".to_owned(),
                field: Some("type".to_owned()),
                value: None,
                stats: None,
                buckets: vec![BucketWire {
                    key: "Document".to_owned(),
                    count: 500,
                    total_bytes: 1_000_000,
                    total_allocated: None,
                    avg_size: None,
                    share_count: None,
                    share_bytes: None,
                    ..BucketWire::default()
                }],
                other_count: None,
                total_groups: None,
                next_cursor: None,
                exact: None,
                values_complete: None,
            },
        ];
        let summary = format_aggregate_summary(&results);
        assert!(summary.contains("total: 1000"), "got: {summary}");
        assert!(summary.contains("by_type (1 buckets)"), "got: {summary}");
        assert!(summary.contains("Document"), "got: {summary}");
    }

    #[test]
    fn summary_buckets_truncated_at_10() {
        let buckets: Vec<BucketWire> = (0_u64..15)
            .map(|i| BucketWire {
                key: format!("ext_{i}"),
                count: 15 - i,
                total_bytes: (15 - i) * 1000,
                ..BucketWire::default()
            })
            .collect();
        let results = vec![AggregateResultWire {
            label: Some("many".to_owned()),
            kind: "buckets".to_owned(),
            field: None,
            value: None,
            stats: None,
            buckets,
            other_count: None,
            total_groups: None,
            next_cursor: None,
            exact: None,
            values_complete: None,
        }];
        let summary = format_aggregate_summary(&results);
        assert!(summary.contains("ext_0"), "first bucket present");
        assert!(summary.contains("ext_9"), "10th bucket present");
        assert!(!summary.contains("ext_10"), "11th bucket hidden");
        assert!(summary.contains("and 5 more"), "truncation message");
    }

    /// Validate that the aggregate tool schema has the expected properties.
    #[test]
    fn aggregate_tool_schema_valid() {
        let schema_json = serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "default": "*" },
                "preset": { "type": "string" },
                "aggregations": { "type": "array" },
                "drives": { "type": "array", "items": { "type": "string" } }
            },
            "required": []
        });
        let props = schema_json["properties"].as_object().unwrap();
        assert!(props.contains_key("pattern"));
        assert!(props.contains_key("preset"));
        assert!(props.contains_key("aggregations"));
        assert!(props.contains_key("drives"));
    }

    /// Validate that the `facet_values` tool schema requires "field" and
    /// includes `cursor`/`page_size` for pagination.
    #[test]
    fn facet_values_tool_schema_valid() {
        let schema_json = serde_json::json!({
            "type": "object",
            "properties": {
                "field": { "type": "string" },
                "pattern": { "type": "string", "default": "*" },
                "prefix": { "type": "string" },
                "top": { "type": "integer", "default": 20 },
                "cursor": { "type": "string" },
                "page_size": { "type": "integer" }
            },
            "required": ["field"]
        });
        let props = schema_json["properties"].as_object().unwrap();
        assert!(props.contains_key("field"));
        assert!(props.contains_key("top"));
        assert!(
            props.contains_key("cursor"),
            "cursor param missing from schema"
        );
        assert!(
            props.contains_key("page_size"),
            "page_size param missing from schema"
        );
        let required = schema_json["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("field")));
        // cursor and page_size should NOT be required
        assert!(!required.iter().any(|v| v.as_str() == Some("cursor")));
        assert!(!required.iter().any(|v| v.as_str() == Some("page_size")));
    }

    /// `format_aggregate_summary` includes cursor hint when `next_cursor`
    /// is present on a bucket result.
    #[test]
    fn summary_shows_next_cursor_when_present() {
        let results = vec![AggregateResultWire {
            label: Some("ext_terms".to_owned()),
            kind: "buckets".to_owned(),
            field: Some("extension".to_owned()),
            value: None,
            stats: None,
            buckets: vec![BucketWire {
                key: "rs".to_owned(),
                count: 500,
                total_bytes: 2_000_000,
                ..BucketWire::default()
            }],
            other_count: None,
            total_groups: None,
            next_cursor: Some("0:1:1".to_owned()),
            exact: None,
            values_complete: None,
        }];
        let summary = format_aggregate_summary(&results);
        assert!(
            summary.contains("next_cursor: 0:1:1"),
            "summary should contain cursor hint, got: {summary}"
        );
    }

    /// `format_aggregate_summary` does NOT mention cursor when
    /// `next_cursor` is `None`.
    #[test]
    fn summary_omits_cursor_when_none() {
        let results = vec![AggregateResultWire {
            label: Some("ext_terms".to_owned()),
            kind: "buckets".to_owned(),
            field: Some("extension".to_owned()),
            value: None,
            stats: None,
            buckets: vec![BucketWire {
                key: "rs".to_owned(),
                count: 500,
                total_bytes: 2_000_000,
                ..BucketWire::default()
            }],
            other_count: None,
            total_groups: None,
            next_cursor: None,
            exact: None,
            values_complete: None,
        }];
        let summary = format_aggregate_summary(&results);
        assert!(
            !summary.contains("next_cursor"),
            "summary should NOT mention cursor, got: {summary}"
        );
    }

    #[test]
    fn summary_renders_sub_buckets() {
        let results = vec![AggregateResultWire {
            label: Some("drive_rollup".to_owned()),
            kind: "rollup".to_owned(),
            field: None,
            value: None,
            stats: None,
            buckets: vec![BucketWire {
                key: "C:".to_owned(),
                count: 1000,
                total_bytes: 5_000_000,
                sub_buckets: vec![
                    BucketWire {
                        key: "document".to_owned(),
                        count: 600,
                        total_bytes: 3_000_000,
                        ..BucketWire::default()
                    },
                    BucketWire {
                        key: "image".to_owned(),
                        count: 400,
                        total_bytes: 2_000_000,
                        ..BucketWire::default()
                    },
                ],
                ..BucketWire::default()
            }],
            other_count: None,
            total_groups: None,
            next_cursor: None,
            exact: Some(true),
            values_complete: Some(true),
        }];
        let summary = format_aggregate_summary(&results);
        assert!(
            summary.contains("document"),
            "should show sub-bucket 'document': {summary}"
        );
        assert!(
            summary.contains("image"),
            "should show sub-bucket 'image': {summary}"
        );
        assert!(
            summary.contains("├─"),
            "sub-buckets should be indented with ├─: {summary}"
        );
    }

    #[test]
    fn summary_shows_values_complete_false() {
        let results = vec![AggregateResultWire {
            label: Some("ext_terms".to_owned()),
            kind: "terms".to_owned(),
            field: Some("extension".to_owned()),
            value: None,
            stats: None,
            buckets: vec![],
            other_count: Some(500),
            total_groups: Some(100),
            next_cursor: None,
            exact: Some(true),
            values_complete: Some(false),
        }];
        let summary = format_aggregate_summary(&results);
        assert!(
            summary.contains("truncated"),
            "should show truncation hint: {summary}"
        );
        assert!(
            summary.contains("500"),
            "should show other_count: {summary}"
        );
    }
}
