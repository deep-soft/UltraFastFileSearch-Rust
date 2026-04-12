//! UFFS MCP Server Library
//!
//! Provides the MCP (Model Context Protocol) server implementation for UFFS.
//! This bridges LLM hosts (Claude Desktop, Cursor, Windsurf, etc.) to the
//! UFFS daemon via the [`rmcp`] SDK.
//!
//! # Architecture
//!
//! ```text
//! LLM Host ──stdio──▶ UffsMcpServer ──UffsClient──▶ uffs-daemon
//! ```
//!
//! The server exposes UFFS tools, resources, and prompts over the MCP protocol.
//! It is **not** in the query data path — it merely bridges MCP framing to the
//! daemon's native protocol.
//!
//! # Usage
//!
//! ```rust,no_run
//! # #[tokio::main]
//! # async fn main() -> anyhow::Result<()> {
//! uffs_mcp::run_mcp_server().await
//! # }
//! ```

// ── MCP tracing initialisation ────────────────────────────────────────

extern crate alloc;

/// Initialise tracing for the MCP server.
///
/// **Stdout is the protocol channel** — all logging MUST go to stderr or
/// to `log_file`.  Behaviour mirrors [`uffs_daemon::init_tracing`]:
///
/// * `UFFS_LOG=trace` sets the filter; default is `"info"`.
/// * `UFFS_LOG_FILE=/tmp/mcp.log` redirects to a file.  When a verbose level
///   (`debug`/`trace`) is active and no file is specified, a default file is
///   used so diagnostic output isn't lost.
///
/// Returns an optional guard that **must** be held for the lifetime of
/// the MCP server — dropping it flushes the non-blocking writer.
#[must_use]
pub fn init_mcp_tracing(
    log_spec: &str,
    log_file: Option<&std::path::Path>,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter = tracing_subscriber::EnvFilter::try_new(log_spec)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    let is_verbose = {
        let lower = log_spec.to_ascii_lowercase();
        lower.contains("debug") || lower.contains("trace")
    };

    let effective_file: Option<std::path::PathBuf> = match log_file {
        Some(path) => {
            let resolved = if path.as_os_str().is_empty() || path == std::path::Path::new("-") {
                default_mcp_log_file()
            } else {
                path.to_path_buf()
            };
            Some(resolved)
        }
        None if is_verbose => Some(default_mcp_log_file()),
        None => None,
    };

    if let Some(resolved) = effective_file {
        if let Some(parent) = resolved.parent() {
            let _ignore = std::fs::create_dir_all(parent);
        }
        let file_appender = tracing_appender::rolling::never(
            resolved
                .parent()
                .unwrap_or_else(|| std::path::Path::new(".")),
            resolved
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("uffs_mcp.log")),
        );
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        let _ignore = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .with_ansi(false)
            .with_writer(non_blocking)
            .try_init();
        Some(guard)
    } else {
        // Default: log to stderr (stdout is the MCP protocol channel).
        let _ignore = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .with_writer(std::io::stderr)
            .try_init();
        None
    }
}

/// Default log file path for MCP diagnostic sessions.
fn default_mcp_log_file() -> std::path::PathBuf {
    dirs_next::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("uffs")
        .join("uffs_mcp.log")
}

/// MCP bridge error types.
pub mod error;
/// MCP [`ServerHandler`](rmcp::ServerHandler) implementation.
pub mod handler;
/// Static and live MCP resource implementations.
pub mod resources;
/// MCP roots mapping policy.
pub(crate) mod roots;
/// Output schema types for `outputSchema` / `structuredContent`.
pub mod schemas;
/// MCP server runtime statistics (lock-free counters).
pub mod stats;
/// Human-readable text formatting for tool responses.
pub mod text;

/// Individual MCP tool handlers.
pub mod tools;

/// Streamable HTTP gateway (feature-gated).
#[cfg(feature = "streamable-http")]
pub mod http;

// tower-service is used by http::tests — suppress unused-crate-dep warning.
use core::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use rmcp::ServiceExt;
#[cfg(feature = "streamable-http")]
use tower_service as _;
use tracing::info;

// ── MCP server PID file ─────────────────────────────────────────────

/// Path to the MCP server PID file.
///
/// Separate from the daemon PID file (`daemon.pid`).
#[must_use]
pub fn mcp_pid_file_path() -> std::path::PathBuf {
    let base = dirs_next::data_local_dir().unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    base.join("uffs").join("mcp-server.pid")
}

/// Write the MCP server PID file with explicit transport info.
///
/// Only called by the HTTP gateway to record `http:bind:port`.
/// Stdio servers do **not** write PID files (multiple can coexist).
///
/// # PID file format
///
/// ```text
/// {pid}
/// {unix_ts}
/// {transport}         e.g. "http:127.0.0.1:8080"
/// data-dir={path}     optional — persisted so `reload` can recover
/// mft-file={path}     optional, repeatable
/// no-cache            optional flag
/// ```
///
/// Lines after the first 3 are optional key=value pairs.
pub fn write_mcp_pid_file_with_transport(transport: &str) {
    write_mcp_pid_file_full(transport, None, &[], false);
}

/// Write the MCP server PID file with transport **and** data source info.
///
/// Called by `mcp start` / `mcp serve` so that `mcp reload` can recover
/// data sources even when the gateway process is already dead.
#[expect(
    clippy::format_push_string,
    reason = "write! requires fmt::Write import; push_str+format is fine here"
)]
pub fn write_mcp_pid_file_full(
    transport: &str,
    data_dir: Option<&std::path::Path>,
    mft_files: &[std::path::PathBuf],
    no_cache: bool,
) {
    let path = mcp_pid_file_path();
    if let Some(parent) = path.parent() {
        drop(std::fs::create_dir_all(parent));
    }
    let pid = std::process::id();
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |dur| dur.as_secs());
    let mut content = format!("{pid}\n{ts}\n{transport}\n");
    if let Some(dir) = data_dir {
        content.push_str(&format!("data-dir={}\n", dir.display()));
    }
    for mft in mft_files {
        content.push_str(&format!("mft-file={}\n", mft.display()));
    }
    if no_cache {
        content.push_str("no-cache\n");
    }
    if let Err(err) = std::fs::write(&path, content) {
        tracing::warn!(path = %path.display(), %err, "Failed to write MCP PID file");
    } else {
        info!(pid, path = %path.display(), transport, "Wrote MCP server PID file");
    }
}

/// Remove the MCP server PID file (best-effort).
pub fn remove_mcp_pid_file() {
    let path = mcp_pid_file_path();
    if path.exists() {
        drop(std::fs::remove_file(&path));
        info!(path = %path.display(), "Removed MCP server PID file");
    }
}

/// Parsed MCP server PID file.
#[derive(Debug, Clone)]
pub struct McpPidInfo {
    /// Process ID.
    pub pid: u32,
    /// Start timestamp (Unix epoch seconds).
    pub start_ts: u64,
    /// Transport: `"stdio"` or `"http:bind:port"`.
    pub transport: String,
    /// Data directory (if persisted).
    pub data_dir: Option<std::path::PathBuf>,
    /// MFT file paths (if persisted).
    pub mft_files: Vec<std::path::PathBuf>,
    /// Whether `--no-cache` was set.
    pub no_cache: bool,
}

impl McpPidInfo {
    /// If HTTP transport, extract `(bind, port)`.
    #[must_use]
    pub fn http_addr(&self) -> Option<(&str, u16)> {
        let rest = self.transport.strip_prefix("http:")?;
        let (bind, port_str) = rest.rsplit_once(':')?;
        let port: u16 = port_str.parse().ok()?;
        Some((bind, port))
    }

    /// Returns `true` if data sources were persisted in the PID file.
    #[must_use]
    pub const fn has_data_sources(&self) -> bool {
        self.data_dir.is_some() || !self.mft_files.is_empty()
    }
}

/// Parse the MCP server PID file.  Returns `(pid, start_timestamp)`.
#[must_use]
pub fn parse_mcp_pid_file() -> Option<(u32, u64)> {
    let info = parse_mcp_pid_file_full()?;
    Some((info.pid, info.start_ts))
}

/// Parse the full MCP server PID file (pid, timestamp, transport, data
/// sources).
#[must_use]
pub fn parse_mcp_pid_file_full() -> Option<McpPidInfo> {
    let content = std::fs::read_to_string(mcp_pid_file_path()).ok()?;
    let mut lines = content.lines();
    let pid: u32 = lines.next()?.parse().ok()?;
    let ts: u64 = lines.next()?.parse().ok()?;
    let transport = lines.next().unwrap_or("stdio").to_owned();

    let mut data_dir: Option<std::path::PathBuf> = None;
    let mut mft_files: Vec<std::path::PathBuf> = Vec::new();
    let mut no_cache = false;

    for line in lines {
        if let Some(dir) = line.strip_prefix("data-dir=") {
            data_dir = Some(std::path::PathBuf::from(dir));
        } else if let Some(mft) = line.strip_prefix("mft-file=") {
            mft_files.push(std::path::PathBuf::from(mft));
        } else if line == "no-cache" {
            no_cache = true;
        }
    }

    Some(McpPidInfo {
        pid,
        start_ts: ts,
        transport,
        data_dir,
        mft_files,
        no_cache,
    })
}

/// Check if the MCP server process (from the PID file) is still alive.
#[must_use]
pub fn is_mcp_server_running() -> Option<u32> {
    let (pid, _ts) = parse_mcp_pid_file()?;
    is_process_alive(pid).then_some(pid)
}

/// Check if a process is alive (platform-specific).
#[must_use]
fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // /proc/PID/status exists on Linux; on macOS use `kill -0`.
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
    #[cfg(not(unix))]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .map_or(false, |o| {
                String::from_utf8_lossy(&o.stdout).contains(&pid.to_string())
            })
    }
}

// ── Configuration ───────────────────────────────────────────────────

/// Configuration for the MCP server.
#[derive(Debug, Clone)]
pub struct McpConfig {
    /// Extra CLI args forwarded to `uffs daemon run` when auto-starting
    /// (e.g. `["--data-dir", "/path"]`).
    pub daemon_spawn_args: Vec<String>,
    /// Idle timeout in seconds.  The MCP server will auto-exit if no
    /// MCP messages are received within this period.  `0` = no timeout.
    pub idle_timeout_secs: u64,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            daemon_spawn_args: Vec::new(),
            idle_timeout_secs: 7200,
        }
    }
}

// ── Server entry points ─────────────────────────────────────────────

/// Run the MCP server on stdin/stdout with the given configuration.
///
/// Writes a PID file on start and removes it on exit.  Connects to the
/// UFFS daemon (auto-starting with forwarded args if needed), creates the
/// [`handler::UffsMcpServer`], and serves MCP over stdio until the client
/// disconnects or the idle timeout fires.
///
/// # Errors
///
/// Returns an error if the daemon connection fails or the MCP transport
/// encounters an I/O error.
#[expect(
    clippy::cognitive_complexity,
    reason = "MCP server startup with daemon connect, idle timer, and transport orchestration"
)]
pub async fn run_mcp_server_with_config(config: &McpConfig) -> anyhow::Result<()> {
    info!(
        idle_timeout = config.idle_timeout_secs,
        daemon_args = ?config.daemon_spawn_args,
        "UFFS MCP server starting (rmcp)…"
    );

    // Stdio servers do NOT write PID files — multiple stdio sessions
    // (one per AI host) can coexist.  Only the HTTP gateway writes a
    // PID file (via `write_mcp_pid_file_with_transport` in http.rs).

    // Connect to the daemon (auto-starts with forwarded args if needed).
    let mut client = uffs_client::connect::UffsClient::connect_with_args(&config.daemon_spawn_args)
        .await
        .context("Failed to connect to UFFS daemon")?;

    // Wait for the daemon to finish loading indices before serving MCP
    // requests.  Without this, tool calls hit empty data when the daemon
    // was just auto-started.
    info!("Connected to daemon, waiting for indices to load…");
    client
        .await_ready(core::time::Duration::from_mins(2))
        .await
        .context("Daemon did not become ready within 120s")?;
    info!("Daemon ready, starting MCP stdio transport…");

    let server = handler::UffsMcpServer::new(client, config.daemon_spawn_args.clone());

    // Capture the activity handle BEFORE `serve` consumes the server.
    // Every MCP tool call / resource read / prompt list calls `touch()`,
    // which stores the current epoch-second into this atomic.  The
    // sliding-window loop below uses it to extend the idle deadline.
    let last_activity = server.last_activity_handle();

    // Serve MCP over stdin/stdout using rmcp's stdio transport.
    let transport = rmcp::transport::io::stdio();
    let service = server.serve(transport).await?;

    if config.idle_timeout_secs > 0 {
        let timeout_secs = config.idle_timeout_secs;
        let ct = service.cancellation_token();

        // Race: transport close (client disconnect) vs sliding-window idle.
        //
        // `service.waiting()` takes ownership, so it cannot be used inside a
        // loop.  Instead the sliding-window logic lives in a self-contained
        // async helper (`wait_for_genuine_idle`) which only resolves once the
        // idle window has truly expired.
        tokio::select! {
            result = service.waiting() => {
                result?;
                info!("MCP server shut down (client disconnected).");
            }
            () = wait_for_genuine_idle(&last_activity, timeout_secs) => {
                info!(
                    timeout_secs,
                    "MCP server idle timeout — shutting down."
                );
                ct.cancel();
            }
        }
    } else {
        // No timeout — wait for client disconnect only.
        service.waiting().await?;
        info!("MCP server shut down cleanly.");
    }

    // PID file removed by _pid_guard drop.
    Ok(())
}

/// Sliding-window idle timer that resolves only on genuine inactivity.
///
/// # Algorithm
///
/// 1. Sleep for the full `timeout_secs` window.
/// 2. On wake, read `last_activity` (epoch seconds set by every MCP request).
/// 3. If activity occurred during the sleep, compute the remaining time until
///    `last_activity + timeout_secs` and sleep again for exactly that long.
///    This avoids both polling and unnecessary wakeups.
/// 4. Repeat until no activity has occurred for a full window.
///
/// Each iteration creates at most one new [`tokio::time::Sleep`], which is
/// negligible overhead for a multi-hour timeout window.
async fn wait_for_genuine_idle(last_activity: &core::sync::atomic::AtomicU64, timeout_secs: u64) {
    let mut remaining = core::time::Duration::from_secs(timeout_secs);
    loop {
        tokio::time::sleep(remaining).await;

        let last_secs = last_activity.load(Ordering::Relaxed);
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |dur| dur.as_secs());
        let elapsed_since_activity = now_secs.saturating_sub(last_secs);

        if elapsed_since_activity >= timeout_secs {
            // Genuinely idle — no MCP request for a full timeout window.
            return;
        }

        // Activity extended the window.  Sleep for the precise remainder.
        remaining = core::time::Duration::from_secs(timeout_secs - elapsed_since_activity);
        info!(
            remaining_secs = remaining.as_secs(),
            "MCP idle deadline extended — activity within window"
        );
    }
}

/// Run the MCP server on stdin/stdout with default configuration.
///
/// Convenience wrapper around [`run_mcp_server_with_config`] using
/// [`McpConfig::default`].
///
/// # Errors
///
/// Returns an error if the daemon connection fails or the MCP transport
/// encounters an I/O error.
pub async fn run_mcp_server() -> anyhow::Result<()> {
    run_mcp_server_with_config(&McpConfig::default()).await
}

#[cfg(test)]
#[expect(
    clippy::default_numeric_fallback,
    clippy::min_ident_chars,
    clippy::indexing_slicing,
    reason = "test code — relaxed for readability"
)]
mod tests {
    // ── error.rs tests ──────────────────────────────────────────────

    /// JSON-RPC 2.0 error code for "Invalid params".
    const INVALID_PARAMS: rmcp::model::ErrorCode = rmcp::model::ErrorCode(-32602);
    /// JSON-RPC 2.0 error code for "Internal error".
    const INTERNAL_ERROR: rmcp::model::ErrorCode = rmcp::model::ErrorCode(-32603);

    mod error_tests {
        use rmcp::ErrorData as McpError;

        use crate::error::BridgeError;

        #[test]
        fn missing_param_maps_to_invalid_params() {
            let bridge = BridgeError::MissingParam("pattern");
            let mcp: McpError = McpError::from(bridge);
            assert_eq!(mcp.code, super::INVALID_PARAMS);
            assert!(mcp.message.contains("pattern"));
        }

        #[test]
        fn invalid_param_maps_to_invalid_params() {
            let bridge = BridgeError::InvalidParam {
                name: "limit",
                reason: "must be positive".to_owned(),
            };
            let mcp: McpError = McpError::from(bridge);
            assert_eq!(mcp.code, super::INVALID_PARAMS);
            assert!(mcp.message.contains("limit"));
            assert!(mcp.message.contains("must be positive"));
        }

        #[test]
        fn daemon_error_maps_to_internal() {
            let bridge = BridgeError::Daemon("connection reset".to_owned());
            let mcp: McpError = McpError::from(bridge);
            assert_eq!(mcp.code, super::INTERNAL_ERROR);
            assert!(mcp.message.contains("connection reset"));
        }

        #[test]
        fn serialization_error_maps_to_internal() {
            let serde_err = serde_json::from_str::<serde_json::Value>("{bad}").unwrap_err();
            let bridge: BridgeError = serde_err.into();
            let mcp: McpError = McpError::from(bridge);
            assert_eq!(mcp.code, super::INTERNAL_ERROR);
        }
    }

    // ── text.rs tests ───────────────────────────────────────────────

    mod text_tests {
        use uffs_client::protocol::SearchRow;

        use crate::text::format_search_row;

        fn test_row(name: &str, size: u64, modified: i64, path: &str) -> SearchRow {
            SearchRow {
                drive: 'C',
                name: name.to_owned(),
                size,
                is_directory: false,
                modified,
                created: 0,
                accessed: 0,
                flags: 0x20,
                allocated: size,
                path: path.to_owned(),
                descendants: 0,
                treesize: 0,
                tree_allocated: 0,
            }
        }

        #[test]
        fn format_row_basic() {
            let row = test_row("hello.rs", 1024, 1_705_312_200_000_000, "C:\\src\\hello.rs");
            let formatted = format_search_row(&row);
            assert!(formatted.contains("hello.rs"), "name: {formatted}");
            assert!(formatted.contains("1.0 KB"), "size: {formatted}");
            assert!(formatted.contains("2024-01-15"), "date: {formatted}");
            assert!(formatted.contains("C:\\src\\hello.rs"), "path: {formatted}");
        }

        #[test]
        fn format_row_large_size() {
            let row = test_row(
                "big.bin",
                1_073_741_824,
                1_700_000_000_000_000,
                "D:\\data\\big.bin",
            );
            let formatted = format_search_row(&row);
            assert!(formatted.contains("big.bin"));
            assert!(formatted.contains("1.0 GB"), "size: {formatted}");
        }

        #[test]
        fn format_row_zero_timestamp() {
            let row = test_row("", 0, 0, "");
            let formatted = format_search_row(&row);
            // Should still produce valid markdown table row
            assert!(formatted.starts_with('|'));
            // Zero timestamp renders as "—"
            assert!(formatted.contains('—'), "zero ts: {formatted}");
        }
    }

    // ── handler.rs prompt tests ─────────────────────────────────────

    mod prompt_tests {
        use crate::handler::{build_prompt_messages, str_arg, u64_arg};

        #[test]
        fn str_arg_extracts_string() {
            let mut map = serde_json::Map::new();
            map.insert("key".to_owned(), serde_json::json!("value"));
            assert_eq!(str_arg(&map, "key"), Some("value"));
        }

        #[test]
        fn str_arg_returns_none_for_missing() {
            let map = serde_json::Map::new();
            assert_eq!(str_arg(&map, "missing"), None);
        }

        #[test]
        fn str_arg_returns_none_for_non_string() {
            let mut map = serde_json::Map::new();
            map.insert("key".to_owned(), serde_json::json!(42));
            assert_eq!(str_arg(&map, "key"), None);
        }

        #[test]
        fn u64_arg_parses_numeric_string() {
            let mut map = serde_json::Map::new();
            map.insert("limit".to_owned(), serde_json::json!("25"));
            assert_eq!(u64_arg(&map, "limit", 50), 25);
        }

        #[test]
        fn u64_arg_uses_default_when_missing() {
            let map = serde_json::Map::new();
            assert_eq!(u64_arg(&map, "limit", 50), 50);
        }

        #[test]
        fn u64_arg_uses_default_when_not_numeric() {
            let mut map = serde_json::Map::new();
            map.insert("limit".to_owned(), serde_json::json!("abc"));
            assert_eq!(u64_arg(&map, "limit", 50), 50);
        }

        // ── build_prompt_messages tests ─────────────────────────────

        #[test]
        fn find_large_files_default_limit() {
            let args = serde_json::Map::new();
            let msgs = build_prompt_messages("find_large_files", &args).unwrap();
            assert_eq!(msgs.len(), 1);
            let text = format!("{:?}", msgs[0]);
            assert!(text.contains("50"), "default limit 50: {text}");
        }

        #[test]
        fn find_large_files_custom_limit() {
            let mut args = serde_json::Map::new();
            args.insert("limit".to_owned(), serde_json::json!("10"));
            let msgs = build_prompt_messages("find_large_files", &args).unwrap();
            let text = format!("{:?}", msgs[0]);
            assert!(text.contains("10"), "custom limit: {text}");
        }

        #[test]
        fn recent_changes_default() {
            let args = serde_json::Map::new();
            let msgs = build_prompt_messages("recent_changes", &args).unwrap();
            let text = format!("{:?}", msgs[0]);
            assert!(text.contains("1 day"), "default 1 day: {text}");
        }

        #[test]
        fn find_by_extension_with_ext() {
            let mut args = serde_json::Map::new();
            args.insert("extension".to_owned(), serde_json::json!("pdf"));
            let msgs = build_prompt_messages("find_by_extension", &args).unwrap();
            let text = format!("{:?}", msgs[0]);
            assert!(text.contains("*.pdf"), "pdf pattern: {text}");
        }

        #[test]
        fn disk_usage_report_with_drive() {
            let mut args = serde_json::Map::new();
            args.insert("drive".to_owned(), serde_json::json!("C"));
            let msgs = build_prompt_messages("disk_usage_report", &args).unwrap();
            let text = format!("{:?}", msgs[0]);
            assert!(text.contains("drive C:"), "drive scope: {text}");
            assert!(text.contains("Step 1"), "multi-step: {text}");
        }

        #[test]
        fn cleanup_report_custom_size() {
            let mut args = serde_json::Map::new();
            args.insert("min_size_mb".to_owned(), serde_json::json!("500"));
            let msgs = build_prompt_messages("cleanup_report", &args).unwrap();
            let text = format!("{:?}", msgs[0]);
            assert!(text.contains("500MB"), "custom min_size: {text}");
        }

        #[test]
        fn duplicate_investigation_with_ext() {
            let mut args = serde_json::Map::new();
            args.insert("extension".to_owned(), serde_json::json!("jpg"));
            let msgs = build_prompt_messages("duplicate_investigation", &args).unwrap();
            let text = format!("{:?}", msgs[0]);
            assert!(text.contains("*.jpg"), "ext filter: {text}");
        }

        #[test]
        fn unknown_prompt_returns_error() {
            let args = serde_json::Map::new();
            let result = build_prompt_messages("nonexistent_prompt", &args);
            result.unwrap_err();
        }

        #[test]
        fn all_7_prompts_are_defined() {
            let defs = crate::handler::prompt_definitions();
            assert_eq!(defs.len(), 7, "expected 7 prompts, got {}", defs.len());

            let names: Vec<_> = defs.iter().map(|p| p.name.as_ref()).collect();
            assert!(names.contains(&"find_large_files"));
            assert!(names.contains(&"recent_changes"));
            assert!(names.contains(&"find_by_extension"));
            assert!(names.contains(&"find_duplicates_by_name"));
            assert!(names.contains(&"disk_usage_report"));
            assert!(names.contains(&"cleanup_report"));
            assert!(names.contains(&"duplicate_investigation"));
        }
    }

    // ── tool definition tests ───────────────────────────────────────

    mod tool_def_tests {
        use crate::handler::tool_definitions;

        #[test]
        fn six_tools_defined() {
            let tools = tool_definitions();
            assert_eq!(tools.len(), 6, "expected 6 tools, got {}", tools.len());
        }

        #[test]
        fn tool_names_are_namespaced() {
            let tools = tool_definitions();
            for tool in &tools {
                assert!(
                    tool.name.starts_with("uffs_"),
                    "tool '{}' should be namespaced",
                    tool.name
                );
            }
        }

        #[test]
        fn expected_tools_present() {
            let tools = tool_definitions();
            let names: Vec<_> = tools.iter().map(|t| t.name.as_ref()).collect();
            for expected in &[
                "uffs_search",
                "uffs_info",
                "uffs_drives",
                "uffs_status",
                "uffs_aggregate",
                "uffs_facet_values",
            ] {
                assert!(names.contains(expected), "missing tool: {expected}");
            }
        }

        #[test]
        fn all_tools_have_descriptions() {
            let tools = tool_definitions();
            for tool in &tools {
                let desc = tool.description.as_deref().unwrap_or("");
                assert!(
                    !desc.is_empty(),
                    "tool '{}' has empty description",
                    tool.name
                );
            }
        }
    }

    // ── tool args deserialization tests ──────────────────────────────

    mod tool_args_tests {
        use crate::tools::aggregate::AggregateArgs;
        use crate::tools::facet_values::FacetValuesArgs;
        use crate::tools::info::InfoArgs;
        use crate::tools::search::SearchArgs;

        #[test]
        fn search_args_defaults() {
            let args: SearchArgs = serde_json::from_value(serde_json::json!({
                "pattern": "*.rs"
            }))
            .unwrap();
            assert_eq!(args.pattern, "*.rs");
            assert!(!args.case_sensitive);
            assert_eq!(args.sort, "modified");
            assert!(
                !args.sort_desc,
                "sort_desc defaults to false (ascending), matching CLI"
            );
            assert_eq!(args.limit, 50);
            assert_eq!(args.filter, "all");
        }

        #[test]
        fn search_args_custom_values() {
            let args: SearchArgs = serde_json::from_value(serde_json::json!({
                "pattern": ">report_[0-9]+",
                "case_sensitive": true,
                "sort": "size",
                "sort_desc": false,
                "limit": 25,
                "filter": "files"
            }))
            .unwrap();
            assert_eq!(args.pattern, ">report_[0-9]+");
            assert!(args.case_sensitive);
            assert_eq!(args.sort, "size");
            assert!(!args.sort_desc);
            assert_eq!(args.limit, 25);
            assert_eq!(args.filter, "files");
        }

        #[test]
        fn aggregate_args_defaults() {
            let args: AggregateArgs = serde_json::from_value(serde_json::json!({})).unwrap();
            assert_eq!(args.pattern, "*");
            assert!(args.preset.is_none());
            assert!(args.aggregations.is_empty());
            assert!(args.drives.is_empty());
        }

        #[test]
        fn aggregate_args_with_preset() {
            let args: AggregateArgs = serde_json::from_value(serde_json::json!({
                "preset": "by_extension",
                "drives": ["C", "D"]
            }))
            .unwrap();
            assert_eq!(args.preset.as_deref(), Some("by_extension"));
            assert_eq!(args.drives, vec!["C", "D"]);
        }

        #[test]
        fn facet_values_args_defaults() {
            let args: FacetValuesArgs = serde_json::from_value(serde_json::json!({
                "field": "extension"
            }))
            .unwrap();
            assert_eq!(args.field, "extension");
            assert_eq!(args.top, 20);
            assert!(args.prefix.is_none());
        }

        #[test]
        fn info_args_basic() {
            let args: InfoArgs = serde_json::from_value(serde_json::json!({
                "path": "C:\\Windows\\System32\\notepad.exe"
            }))
            .unwrap();
            assert_eq!(args.path, "C:\\Windows\\System32\\notepad.exe");
        }
    }

    // ── percent encode/decode round-trip ────────────────────────────

    mod percent_encode_tests {
        use crate::handler::percent_decode_path;
        use crate::tools::search::percent_encode_path;

        #[test]
        fn round_trip_simple_path() {
            let path = r"C:\Users\me\project\file.rs";
            let encoded = percent_encode_path(path);
            assert_eq!(encoded, "C:/Users/me/project/file.rs");
            let decoded = percent_decode_path(&encoded);
            // Decode gives forward slashes; the handler normalises back.
            assert_eq!(decoded, "C:/Users/me/project/file.rs");
        }

        #[test]
        fn round_trip_path_with_spaces() {
            let path = r"C:\Program Files\My App\data.txt";
            let encoded = percent_encode_path(path);
            assert!(
                encoded.contains("%20"),
                "spaces should be encoded: {encoded}"
            );
            let decoded = percent_decode_path(&encoded);
            assert_eq!(decoded, "C:/Program Files/My App/data.txt");
        }

        #[test]
        fn round_trip_path_with_unicode() {
            let path = r"D:\文档\报告.pdf";
            let encoded = percent_encode_path(path);
            let decoded = percent_decode_path(&encoded);
            assert_eq!(decoded, "D:/文档/报告.pdf");
        }

        #[test]
        fn decode_passthrough_for_unencoded() {
            assert_eq!(percent_decode_path("C:/simple/path"), "C:/simple/path");
        }

        #[test]
        fn decode_handles_percent_at_end() {
            // Truncated percent sequence should be passed through.
            assert_eq!(percent_decode_path("foo%2"), "foo%2");
            assert_eq!(percent_decode_path("foo%"), "foo%");
        }
    }

    // ── idle timeout (sliding-window) tests ─────────────────────────────

    mod idle_timeout_tests {
        use crate::wait_for_genuine_idle;
        extern crate alloc;

        use alloc::sync::Arc;
        use core::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};

        fn now_epoch() -> u64 {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs())
        }

        #[tokio::test]
        async fn genuine_idle_returns_promptly() {
            let last_activity = AtomicU64::new(now_epoch());
            // 100 ms timeout, no further activity → should return in ~100 ms.
            let start = tokio::time::Instant::now();
            wait_for_genuine_idle(&last_activity, 1).await;
            // It must have waited at least ~1 s (the timeout).
            // With epoch-second resolution, accept ≥ 900 ms.
            assert!(
                start.elapsed() >= core::time::Duration::from_millis(900),
                "should wait for the full timeout window"
            );
        }

        #[tokio::test]
        async fn activity_extends_deadline() {
            // `wait_for_genuine_idle` works in epoch-second resolution, so
            // we must use a 2 s timeout and poke at ~1 s to ensure the
            // epoch second value actually advances between the initial
            // snapshot and the update.
            let last_activity = Arc::new(AtomicU64::new(now_epoch()));
            let la = Arc::clone(&last_activity);

            // 2 s timeout. At ~1.2 s, poke activity (ensures epoch second advances).
            let updater = tokio::spawn(async move {
                tokio::time::sleep(core::time::Duration::from_millis(1200)).await;
                la.store(now_epoch(), Ordering::Relaxed);
            });

            let start = tokio::time::Instant::now();
            wait_for_genuine_idle(&last_activity, 2).await;
            updater.await.unwrap();

            // The activity at ~1.2 s should push the deadline to ~3.2 s.
            // Total wall time must exceed 2 s (the base timeout).
            assert!(
                start.elapsed() >= core::time::Duration::from_millis(2800),
                "activity at 1.2 s should extend total wait beyond 2 s; actual: {:?}",
                start.elapsed()
            );
        }
    }
}
