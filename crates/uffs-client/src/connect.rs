//! Daemon connection management: auto-start, connect, reconnect.
//!
//! [`UffsClient`] is the single entry point for all surfaces (CLI, TUI,
//! GUI, MCP) to communicate with the daemon.
//!
//! # Platform Support
//!
//! | Platform | IPC Transport |
//! |----------|--------------|
//! | **macOS** | Unix domain socket (`~/Library/Application Support/uffs/daemon.sock`) |
//! | **Linux** | Unix domain socket (`$XDG_RUNTIME_DIR/uffs/daemon.sock`) |
//! | **Windows** | Unix domain socket (`%LOCALAPPDATA%/uffs/daemon.sock`) — named pipe planned |

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::protocol::{
    DrivesResponse, RpcRequest, RpcResponse, SearchParams, SearchResponse, StatusResponse,
};

/// Thin client for the UFFS daemon.
///
/// Uses boxed async I/O so the same struct works with Unix domain sockets
/// (macOS/Linux) and named pipes (Windows) without generics leaking into
/// the public API.
pub struct UffsClient {
    /// Buffered reader for the IPC connection.
    reader: BufReader<Box<dyn tokio::io::AsyncRead + Unpin + Send>>,
    /// Writer for the IPC connection.
    writer: Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
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
        // Try connecting directly first
        if let Ok(client) = Self::platform_connect().await {
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

            if let Ok(client) = Self::platform_connect().await {
                tracing::info!(attempt, "Connected to daemon");
                return Ok(client);
            }

            delay_ms = (delay_ms * 2).min(2000);
        }

        Err(crate::error::ClientError::ConnectionFailed(
            "Could not connect to daemon after auto-start".to_owned(),
        ))
    }

    /// Spawn the daemon as a detached background process.
    fn spawn_daemon() -> Result<(), crate::error::ClientError> {
        let daemon_exe = find_daemon_exe()?;

        #[cfg(unix)]
        {
            std::process::Command::new(&daemon_exe)
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
            std::process::Command::new(&daemon_exe)
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

    /// Trigger a drive refresh.
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

// ── Platform-specific connection ────────────────────────────────────────────

/// Unix: connect via Unix domain socket.
#[cfg(unix)]
impl UffsClient {
    async fn platform_connect() -> Result<Self, crate::error::ClientError> {
        let sock_path = socket_path();
        let stream = tokio::net::UnixStream::connect(&sock_path)
            .await
            .map_err(|e| crate::error::ClientError::ConnectionFailed(e.to_string()))?;

        let (read_half, write_half) = stream.into_split();

        Ok(Self {
            reader: BufReader::new(Box::new(read_half)),
            writer: Box::new(write_half),
            next_id: AtomicU64::new(1),
        })
    }
}

/// Windows: connect via Unix domain socket (named pipe support planned).
///
/// Windows 10 1803+ supports Unix domain sockets via `AF_UNIX`. We use
/// this for now; native named pipe support can be added later for older
/// Windows versions.
#[cfg(windows)]
impl UffsClient {
    async fn platform_connect() -> Result<Self, crate::error::ClientError> {
        let sock_path = socket_path();
        let stream = tokio::net::UnixStream::connect(&sock_path)
            .await
            .map_err(|e| crate::error::ClientError::ConnectionFailed(e.to_string()))?;

        let (read_half, write_half) = stream.into_split();

        Ok(Self {
            reader: BufReader::new(Box::new(read_half)),
            writer: Box::new(write_half),
            next_id: AtomicU64::new(1),
        })
    }
}

// ── Shared Helpers ──────────────────────────────────────────────────────────

/// Platform-specific socket/pipe path (must match daemon's ipc::socket_path).
pub fn socket_path() -> PathBuf {
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
fn find_daemon_exe() -> Result<PathBuf, crate::error::ClientError> {
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let candidate = dir.join("uffs-daemon");
            if candidate.exists() {
                return Ok(candidate);
            }
            let candidate_exe = dir.join("uffs-daemon.exe");
            if candidate_exe.exists() {
                return Ok(candidate_exe);
            }
        }
    }
    Ok(PathBuf::from("uffs-daemon"))
}
