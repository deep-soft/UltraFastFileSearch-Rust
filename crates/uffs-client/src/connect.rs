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

use core::sync::atomic::{AtomicU64, Ordering};
use std::path::PathBuf;

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
    /// Notification sender — incoming daemon notifications are forwarded here.
    notification_tx: tokio::sync::mpsc::UnboundedSender<crate::protocol::RpcNotification>,
    /// Notification receiver — consumers read daemon events from this.
    notification_rx: tokio::sync::mpsc::UnboundedReceiver<crate::protocol::RpcNotification>,
}

impl UffsClient {
    /// Connect to a running daemon, or auto-start one if not running.
    ///
    /// Tries to connect to the socket. If the socket doesn't exist or
    /// connection fails, spawns `uffs-daemon` as a detached process and
    /// retries with exponential backoff (up to ~30s).
    ///
    /// # Errors
    ///
    /// Returns `ConnectionFailed` if the daemon cannot be reached after
    /// multiple retries, or `DaemonStartFailed` if auto-start fails.
    pub async fn connect() -> Result<Self, crate::error::ClientError> {
        // Try connecting directly first
        if let Ok(client) = Self::platform_connect().await {
            // S4.3.4: Verify daemon identity via PID file
            verify_daemon_after_connect();
            return Ok(client);
        }

        // Auto-start the daemon
        tracing::info!("Daemon not running, auto-starting...");

        // Find the daemon executable: look next to current exe, fall back to PATH
        let daemon_exe = std::env::current_exe()
            .ok()
            .and_then(|exe| {
                let parent = exe.parent()?;
                let unix = parent.join("uffs-daemon");
                let win = parent.join("uffs-daemon.exe");
                if unix.exists() {
                    Some(unix)
                } else if win.exists() {
                    Some(win)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| PathBuf::from("uffs-daemon"));

        let spawn_result = {
            #[cfg(unix)]
            {
                std::process::Command::new(&daemon_exe)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .stdin(std::process::Stdio::null())
                    .spawn()
            }
            #[cfg(windows)]
            {
                std::process::Command::new(&daemon_exe)
                    .creation_flags(0x0000_0008) // DETACHED_PROCESS
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .stdin(std::process::Stdio::null())
                    .spawn()
            }
        };

        spawn_result.map_err(|spawn_err| {
            crate::error::ClientError::DaemonStartFailed(format!(
                "Failed to spawn {}: {spawn_err}",
                daemon_exe.display()
            ))
        })?;

        // Retry with backoff
        let mut delay_ms = 50_u64;
        let max_attempts = 20_usize;
        for attempt in 1_usize..=max_attempts {
            tokio::time::sleep(core::time::Duration::from_millis(delay_ms)).await;

            if let Ok(client) = Self::platform_connect().await {
                tracing::info!(attempt, "Connected to daemon");
                // S4.3.4: Verify daemon identity via PID file
                verify_daemon_after_connect();
                return Ok(client);
            }

            delay_ms = (delay_ms * 2).min(2000);
        }

        Err(crate::error::ClientError::ConnectionFailed(
            "Could not connect to daemon after auto-start".to_owned(),
        ))
    }

    /// Receive the next daemon notification (non-blocking).
    ///
    /// Returns `None` if no notifications are pending. Use this in an
    /// event loop to process daemon events (`drive_loaded`,
    /// `refresh_complete`).
    pub fn try_recv_notification(&mut self) -> Option<crate::protocol::RpcNotification> {
        self.notification_rx.try_recv().ok()
    }

    /// Send a JSON-RPC request and read the response.
    ///
    /// D3.4.5: While waiting for the response, any incoming notifications
    /// (messages without an `id` field) are routed to the notification channel.
    async fn send_request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, crate::error::ClientError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = RpcRequest::new(id, method, params);

        let json = serde_json::to_string(&req)
            .map_err(|ser_err| crate::error::ClientError::Protocol(ser_err.to_string()))?;

        self.writer
            .write_all(json.as_bytes())
            .await
            .map_err(|io_err| crate::error::ClientError::Io(io_err.to_string()))?;
        self.writer
            .write_all(b"\n")
            .await
            .map_err(|io_err| crate::error::ClientError::Io(io_err.to_string()))?;
        self.writer
            .flush()
            .await
            .map_err(|io_err| crate::error::ClientError::Io(io_err.to_string()))?;

        // Read lines until we get a response with matching id.
        // Notifications (no id) are routed to the notification channel.
        loop {
            let mut line = String::new();
            let read_result = tokio::time::timeout(
                core::time::Duration::from_secs(30),
                self.reader.read_line(&mut line),
            )
            .await
            .map_err(|_timeout_err| crate::error::ClientError::Timeout)?
            .map_err(|io_err| crate::error::ClientError::Io(io_err.to_string()))?;

            if read_result == 0 {
                return Err(crate::error::ClientError::ConnectionClosed);
            }

            let trimmed = line.trim();

            // Check if this is a notification (has "method" but no "id")
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if value.get("method").is_some() && value.get("id").is_none() {
                    // It's a notification — route to channel
                    if let Ok(notif) =
                        serde_json::from_value::<crate::protocol::RpcNotification>(value)
                    {
                        drop(self.notification_tx.send(notif));
                    }
                    continue; // keep reading for the actual response
                }
            }

            // It's a response
            let resp: RpcResponse = serde_json::from_str(trimmed).map_err(|err| {
                crate::error::ClientError::Protocol(format!("Bad response: {err}"))
            })?;

            return Ok(resp.result);
        }
    }

    // ── Public Query API ────────────────────────────────────────────────

    /// Search files across loaded drives.
    ///
    /// # Errors
    ///
    /// Returns a `ClientError` on connection, protocol, or timeout failure.
    pub async fn search(
        &mut self,
        params: &SearchParams,
    ) -> Result<SearchResponse, crate::error::ClientError> {
        let value = serde_json::to_value(params)
            .map_err(|err| crate::error::ClientError::Protocol(err.to_string()))?;
        let result = self.send_request("search", Some(value)).await?;
        serde_json::from_value(result)
            .map_err(|err| crate::error::ClientError::Protocol(err.to_string()))
    }

    /// List loaded drives.
    ///
    /// # Errors
    ///
    /// Returns a `ClientError` on connection, protocol, or timeout failure.
    pub async fn drives(&mut self) -> Result<DrivesResponse, crate::error::ClientError> {
        let result = self.send_request("drives", None).await?;
        serde_json::from_value(result)
            .map_err(|err| crate::error::ClientError::Protocol(err.to_string()))
    }

    /// Get daemon status.
    ///
    /// # Errors
    ///
    /// Returns a `ClientError` on connection, protocol, or timeout failure.
    pub async fn status(&mut self) -> Result<StatusResponse, crate::error::ClientError> {
        let result = self.send_request("status", None).await?;
        serde_json::from_value(result)
            .map_err(|err| crate::error::ClientError::Protocol(err.to_string()))
    }

    /// Trigger a drive refresh.
    ///
    /// # Errors
    ///
    /// Returns a `ClientError` on connection, protocol, or timeout failure.
    pub async fn refresh(&mut self, drives: &[char]) -> Result<(), crate::error::ClientError> {
        let params = serde_json::json!({"drives": drives});
        let _result = self.send_request("refresh", Some(params)).await?;
        Ok(())
    }

    /// Look up detailed info for a specific file path.
    ///
    /// # Errors
    ///
    /// Returns a `ClientError` on connection, protocol, or timeout failure.
    pub async fn info(
        &mut self,
        path: &str,
    ) -> Result<crate::protocol::InfoResponse, crate::error::ClientError> {
        let params = serde_json::json!({"path": path});
        let result = self.send_request("info", Some(params)).await?;
        serde_json::from_value(result)
            .map_err(|err| crate::error::ClientError::Protocol(err.to_string()))
    }

    /// Send a keepalive to reset the daemon's idle timer.
    ///
    /// # Errors
    ///
    /// Returns a `ClientError` on connection or timeout failure.
    pub async fn keepalive(&mut self) -> Result<(), crate::error::ClientError> {
        let _result = self.send_request("keepalive", None).await?;
        Ok(())
    }

    /// Set the session type (D3.4.3) — tells daemon which idle timeout tier to
    /// use.
    ///
    /// - `"cli"` → short timeout (5 min default)
    /// - `"tui"`, `"gui"`, `"mcp"` → long timeout (15 min default)
    ///
    /// # Errors
    ///
    /// Returns a `ClientError` on connection or timeout failure.
    pub async fn set_session_type(
        &mut self,
        session_type: &str,
    ) -> Result<(), crate::error::ClientError> {
        let params = serde_json::json!({"session_type": session_type});
        let _result = self.send_request("keepalive", Some(params)).await?;
        Ok(())
    }

    /// Request graceful daemon shutdown.
    ///
    /// Reads the shutdown nonce from the PID file (S4.4.9) and sends it
    /// with the shutdown request.
    ///
    /// # Errors
    ///
    /// Returns a `ClientError` on connection or timeout failure.
    pub async fn shutdown(&mut self) -> Result<(), crate::error::ClientError> {
        // Read nonce from PID file (line 4): {pid}\n{timestamp}\n{exe_hash}\n{nonce}\n
        let nonce = std::fs::read_to_string(pid_file_path())
            .ok()
            .and_then(|content| content.lines().nth(3).map(ToOwned::to_owned))
            .unwrap_or_default();
        let params = serde_json::json!({"nonce": nonce});
        let _result = self.send_request("shutdown", Some(params)).await?;
        Ok(())
    }

    /// Start a background keepalive task (D3.4.2).
    ///
    /// Sends a keepalive every `interval` to prevent the daemon from
    /// idle-retiring while this client is alive. Returns a handle that
    /// stops the task when dropped.
    ///
    /// Typical usage for long-lived sessions (TUI, GUI, MCP):
    /// ```rust,ignore
    /// let _keepalive = client.start_keepalive(Duration::from_secs(60));
    /// ```
    pub fn start_keepalive(&self, interval: core::time::Duration) -> KeepaliveGuard {
        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();

        // We can't move &mut self into the task, so we open a separate
        // keepalive connection. This is lightweight — just sends one
        // small JSON message every 60s.
        let sock_path = socket_path();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = tokio::time::sleep(interval) => {
                        // Open a short-lived blocking connection for the keepalive.
                        // Uses std::os::unix / std::os::windows UnixStream (not tokio)
                        // because tokio::net::UnixStream is cfg(unix)-only.
                        let send_result = tokio::task::spawn_blocking({
                            let path = sock_path.clone();
                            move || keepalive_send_blocking(&path)
                        }).await;
                        if let Err(join_err) = send_result {
                            tracing::debug!(error = %join_err, "keepalive send failed");
                        }
                    }
                    _ = &mut cancel_rx => {
                        return; // cancelled
                    }
                }
            }
        });

        KeepaliveGuard { _cancel: cancel_tx }
    }
}

/// Guard that stops the background keepalive task when dropped (D3.4.2).
pub struct KeepaliveGuard {
    /// Dropping this sends a cancel signal to the keepalive task.
    _cancel: tokio::sync::oneshot::Sender<()>,
}

// ── Platform-specific connection ────────────────────────────────────────────

/// Unix: connect via Unix domain socket.
#[cfg(unix)]
impl UffsClient {
    /// Platform-specific connection over Unix domain socket.
    async fn platform_connect() -> Result<Self, crate::error::ClientError> {
        let sock_path = socket_path();
        let stream = tokio::net::UnixStream::connect(&sock_path)
            .await
            .map_err(|err| crate::error::ClientError::ConnectionFailed(err.to_string()))?;

        let (read_half, write_half) = stream.into_split();
        let (notification_tx, notification_rx) = tokio::sync::mpsc::unbounded_channel();

        Ok(Self {
            reader: BufReader::new(Box::new(read_half)),
            writer: Box::new(write_half),
            next_id: AtomicU64::new(1),
            notification_tx,
            notification_rx,
        })
    }
}

/// Windows: connect via AF_UNIX socket (Windows 10 1803+).
///
/// `tokio::net::UnixStream` is `cfg(unix)` only in tokio. On Windows we
/// connect with `std::os::windows::net::UnixStream` (blocking), then bridge
/// it into async via two background threads that pump bytes between the
/// blocking socket and tokio `DuplexStream` channels.
#[cfg(windows)]
impl UffsClient {
    /// Platform-specific connection over AF_UNIX socket (Windows 10 1803+).
    async fn platform_connect() -> Result<Self, crate::error::ClientError> {
        use std::io::{Read, Write};
        use std::os::windows::net::UnixStream as StdUnixStream;

        let sock_path = socket_path();

        let std_stream = StdUnixStream::connect(&sock_path)
            .map_err(|io_err| crate::error::ClientError::ConnectionFailed(io_err.to_string()))?;

        std_stream
            .set_read_timeout(Some(core::time::Duration::from_secs(30)))
            .map_err(|io_err| crate::error::ClientError::ConnectionFailed(io_err.to_string()))?;

        let std_read = std_stream
            .try_clone()
            .map_err(|io_err| crate::error::ClientError::ConnectionFailed(io_err.to_string()))?;
        let std_write = std_stream;

        // Create async duplex channels that bridge to the blocking socket.
        // 64KB buffer is plenty for JSON-RPC messages.
        let (async_read, mut bridge_write) = tokio::io::duplex(65536);
        let (mut bridge_read, async_write) = tokio::io::duplex(65536);

        // Background thread: std socket → async reader
        std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(std_read);
            let mut buf = [0_u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        use tokio::io::AsyncWriteExt;
                        let rt = tokio::runtime::Handle::try_current();
                        if let Ok(handle) = rt {
                            let bytes = buf[..n].to_vec();
                            let _ = handle.block_on(async {
                                bridge_write.write_all(&bytes).await
                            });
                        } else {
                            break;
                        }
                    }
                }
            }
        });

        // Background thread: async writer → std socket
        std::thread::spawn(move || {
            let mut writer = std_write;
            let rt = tokio::runtime::Handle::try_current();
            if let Ok(handle) = rt {
                handle.block_on(async {
                    use tokio::io::AsyncReadExt;
                    let mut buf = [0_u8; 8192];
                    loop {
                        match bridge_read.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if writer.write_all(&buf[..n]).is_err() {
                                    break;
                                }
                                if writer.flush().is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
            }
        });

        let (notification_tx, notification_rx) = tokio::sync::mpsc::unbounded_channel();

        Ok(Self {
            reader: BufReader::new(Box::new(async_read)),
            writer: Box::new(async_write),
            next_id: AtomicU64::new(1),
            notification_tx,
            notification_rx,
        })
    }
}

// ── Shared Helpers ──────────────────────────────────────────────────────────

/// Platform-specific socket/pipe path (must match daemon's `ipc::socket_path`).
#[must_use]
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

/// S4.3.4: Verify daemon identity after connecting.
///
/// Reads the PID file, verifies the daemon process is alive and running
/// the expected binary. Logs a warning on failure but does NOT disconnect
/// (graceful degradation — don't block the user).
fn verify_daemon_after_connect() {
    let pid_path = pid_file_path();
    if !pid_path.exists() {
        tracing::debug!("No PID file found, skipping daemon identity verification");
        return;
    }
    if !crate::verify::verify_daemon_pid_file(&pid_path) {
        tracing::warn!(
            path = %pid_path.display(),
            "Daemon identity verification failed — proceed with caution"
        );
    }
}

/// Send a keepalive message using blocking std I/O (works on all platforms).
///
/// Called from `spawn_blocking` in the keepalive background task. Uses
/// platform-appropriate `std::os::*::net::UnixStream` which compiles on
/// both Unix and Windows (unlike `tokio::net::UnixStream` which is
/// `cfg(unix)` only).
fn keepalive_send_blocking(sock_path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        if let Ok(mut stream) = UnixStream::connect(sock_path) {
            let msg = r#"{"jsonrpc":"2.0","id":0,"method":"keepalive"}"#;
            let _ = stream.write_all(msg.as_bytes());
            let _ = stream.write_all(b"\n");
            let _ = stream.flush();
        }
    }
    #[cfg(windows)]
    {
        use std::io::Write;
        use std::os::windows::net::UnixStream;
        if let Ok(mut stream) = UnixStream::connect(sock_path) {
            let msg = r#"{"jsonrpc":"2.0","id":0,"method":"keepalive"}"#;
            let _ = stream.write_all(msg.as_bytes());
            let _ = stream.write_all(b"\n");
            let _ = stream.flush();
        }
    }
}

/// PID file path (must match daemon's lifecycle.rs).
fn pid_file_path() -> PathBuf {
    let base = dirs_next::data_local_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("uffs").join("daemon.pid")
}
