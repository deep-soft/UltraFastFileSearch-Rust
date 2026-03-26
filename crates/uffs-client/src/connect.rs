//! Daemon connection management: auto-start, connect, reconnect.
//!
//! [`UffsClient`] is the single entry point for all surfaces (CLI, TUI,
//! GUI, MCP) to communicate with the daemon.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::protocol::{
    DrivesResponse, RpcRequest, RpcResponse, SearchParams, SearchResponse, StatusResponse,
};

/// Thin client for the UFFS daemon.
///
/// Holds an open connection to the daemon's Unix domain socket (or named
/// pipe on Windows). All methods are async and return deserialized protocol
/// types.
pub struct UffsClient {
    /// Read half of the socket.
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    /// Write half of the socket.
    writer: tokio::net::unix::OwnedWriteHalf,
    /// Monotonically increasing request ID.
    next_id: AtomicU64,
}

impl UffsClient {
    /// Connect to a running daemon, or auto-start one if not running.
    ///
    /// Tries to connect to the socket. If the socket doesn't exist or
    /// connection fails, spawns `uffs-daemon` as a detached process and
    /// retries with exponential backoff (up to ~30s).
    pub async fn connect() -> Result<Self, crate::error::ClientError> {
        let sock_path = socket_path();

        // Try connecting directly first
        if let Ok(client) = Self::connect_to(&sock_path).await {
            return Ok(client);
        }

        // Auto-start the daemon
        tracing::info!("Daemon not running, auto-starting...");
        Self::spawn_daemon()?;

        // Retry with backoff
        let mut delay_ms = 50_u64;
        let max_attempts = 20;
        for attempt in 1..=max_attempts {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;

            if let Ok(client) = Self::connect_to(&sock_path).await {
                tracing::info!(attempt, "Connected to daemon");
                return Ok(client);
            }

            delay_ms = (delay_ms * 2).min(2000); // cap at 2s
        }

        Err(crate::error::ClientError::ConnectionFailed(
            "Could not connect to daemon after auto-start".to_owned(),
        ))
    }

    /// Connect to a specific socket path.
    async fn connect_to(path: &std::path::Path) -> Result<Self, crate::error::ClientError> {
        let stream = tokio::net::UnixStream::connect(path)
            .await
            .map_err(|e| crate::error::ClientError::ConnectionFailed(e.to_string()))?;

        let (read_half, write_half) = stream.into_split();

        Ok(Self {
            reader: BufReader::new(read_half),
            writer: write_half,
            next_id: AtomicU64::new(1),
        })
    }

    /// Spawn the daemon as a detached background process.
    fn spawn_daemon() -> Result<(), crate::error::ClientError> {
        let daemon_exe = find_daemon_exe()?;

        #[cfg(unix)]
        {
            use std::process::Command;
            Command::new(&daemon_exe)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .stdin(std::process::Stdio::null())
                .spawn()
                .map_err(|e| {
                    crate::error::ClientError::DaemonStartFailed(format!(
                        "Failed to spawn {}: {e}",
                        daemon_exe.display()
                    ))
                })?;
        }

        #[cfg(windows)]
        {
            use std::process::Command;
            Command::new(&daemon_exe)
                .creation_flags(0x0000_0008) // DETACHED_PROCESS
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .stdin(std::process::Stdio::null())
                .spawn()
                .map_err(|e| {
                    crate::error::ClientError::DaemonStartFailed(format!(
                        "Failed to spawn {}: {e}",
                        daemon_exe.display()
                    ))
                })?;
        }

        Ok(())
    }

    /// Send a JSON-RPC request and read the response.
    async fn send_request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, crate::error::ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = RpcRequest::new(id, method, params);

        // Serialize and send
        let json = serde_json::to_string(&req)
            .map_err(|e| crate::error::ClientError::Protocol(e.to_string()))?;

        self.writer
            .write_all(json.as_bytes())
            .await
            .map_err(|e| crate::error::ClientError::Io(e.to_string()))?;
        self.writer
            .write_all(b"\n")
            .await
            .map_err(|e| crate::error::ClientError::Io(e.to_string()))?;
        self.writer
            .flush()
            .await
            .map_err(|e| crate::error::ClientError::Io(e.to_string()))?;

        // Read response with timeout
        let mut line = String::new();
        let read_result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.reader.read_line(&mut line),
        )
        .await
        .map_err(|_| crate::error::ClientError::Timeout)?
        .map_err(|e| crate::error::ClientError::Io(e.to_string()))?;

        if read_result == 0 {
            return Err(crate::error::ClientError::ConnectionClosed);
        }

        // Parse response
        let resp: RpcResponse = serde_json::from_str(line.trim())
            .map_err(|e| crate::error::ClientError::Protocol(format!("Bad response: {e}")))?;

        Ok(resp.result)
    }

    // ── Public Query API ────────────────────────────────────────────────

    /// Search files across loaded drives.
    pub async fn search(
        &mut self,
        params: &SearchParams,
    ) -> Result<SearchResponse, crate::error::ClientError> {
        let value = serde_json::to_value(params)
            .map_err(|e| crate::error::ClientError::Protocol(e.to_string()))?;
        let result = self.send_request("search", Some(value)).await?;
        serde_json::from_value(result)
            .map_err(|e| crate::error::ClientError::Protocol(e.to_string()))
    }

    /// List loaded drives.
    pub async fn drives(&mut self) -> Result<DrivesResponse, crate::error::ClientError> {
        let result = self.send_request("drives", None).await?;
        serde_json::from_value(result)
            .map_err(|e| crate::error::ClientError::Protocol(e.to_string()))
    }

    /// Get daemon status.
    pub async fn status(&mut self) -> Result<StatusResponse, crate::error::ClientError> {
        let result = self.send_request("status", None).await?;
        serde_json::from_value(result)
            .map_err(|e| crate::error::ClientError::Protocol(e.to_string()))
    }

    /// Trigger a drive refresh. Returns immediately; daemon refreshes in background.
    pub async fn refresh(
        &mut self,
        drives: &[char],
    ) -> Result<(), crate::error::ClientError> {
        let params = serde_json::json!({"drives": drives});
        let _result = self.send_request("refresh", Some(params)).await?;
        Ok(())
    }

    /// Send a keepalive to reset the daemon's idle timer.
    pub async fn keepalive(&mut self) -> Result<(), crate::error::ClientError> {
        let _result = self.send_request("keepalive", None).await?;
        Ok(())
    }

    /// Request graceful daemon shutdown.
    pub async fn shutdown(&mut self) -> Result<(), crate::error::ClientError> {
        let _result = self.send_request("shutdown", None).await?;
        Ok(())
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Platform-specific socket path (must match daemon's ipc::socket_path).
fn socket_path() -> PathBuf {
    let base = dirs_next::data_local_dir().unwrap_or_else(|| PathBuf::from("/tmp"));

    #[cfg(target_os = "macos")]
    {
        base.join("uffs").join("daemon.sock")
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(runtime_dir).join("uffs").join("daemon.sock")
        } else {
            base.join("uffs").join("daemon.sock")
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        base.join("uffs").join("daemon.sock")
    }
}

/// Find the `uffs-daemon` executable.
///
/// Looks for it next to the current executable first, then in PATH.
fn find_daemon_exe() -> Result<PathBuf, crate::error::ClientError> {
    // Check next to current executable
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let candidate = dir.join("uffs-daemon");
            if candidate.exists() {
                return Ok(candidate);
            }
            // Windows
            let candidate_exe = dir.join("uffs-daemon.exe");
            if candidate_exe.exists() {
                return Ok(candidate_exe);
            }
        }
    }

    // Fall back to PATH
    Ok(PathBuf::from("uffs-daemon"))
}
