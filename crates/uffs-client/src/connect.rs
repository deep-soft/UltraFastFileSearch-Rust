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

use crate::protocol::{DrivesResponse, RpcRequest, SearchParams, SearchResponse, StatusResponse};

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
    /// connection fails, spawns `uffs daemon run` as a detached process
    /// and retries with exponential backoff (up to ~30s).
    ///
    /// On Windows the daemon auto-discovers live NTFS drives so no extra
    /// args are needed.  On Mac/Linux, pass `--data-dir` or `--mft-file`
    /// via [`connect_with_args`] so the daemon knows where to find data.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionFailed` if the daemon cannot be reached after
    /// multiple retries, or `DaemonStartFailed` if auto-start fails.
    pub async fn connect() -> Result<Self, crate::error::ClientError> {
        Self::connect_with_args(&[]).await
    }

    /// Try to connect to an already-running daemon **without** auto-starting.
    ///
    /// # Errors
    ///
    /// Returns `ConnectionFailed` if no daemon is listening.
    pub async fn connect_raw() -> Result<Self, crate::error::ClientError> {
        Self::platform_connect().await.map_err(|conn_err| {
            crate::error::ClientError::ConnectionFailed(format!("No daemon is running: {conn_err}"))
        })
    }

    /// Connect to a running daemon, or auto-start one with extra CLI
    /// arguments.
    ///
    /// `spawn_args` are forwarded to `uffs daemon run` **only** when
    /// the daemon is not already running and must be auto-started.  If
    /// a daemon is already listening, the args are ignored (it already
    /// has its data loaded).
    ///
    /// Typical usage (Mac/Linux):
    /// ```rust,ignore
    /// let args = vec!["--data-dir".into(), "/path/to/uffs_data".into()];
    /// let client = UffsClient::connect_with_args(&args).await?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `ConnectionFailed` or `DaemonStartFailed`.
    #[expect(
        clippy::print_stdout,
        clippy::use_debug,
        reason = "temporary diagnostic output for debugging daemon start"
    )]
    pub async fn connect_with_args(
        spawn_args: &[String],
    ) -> Result<Self, crate::error::ClientError> {
        let sock = socket_path();
        let pid_path = pid_file_path();
        eprintln!("[diag] connect_with_args: socket_path={}", sock.display());
        eprintln!("[diag] connect_with_args: socket exists={}", sock.exists());
        eprintln!("[diag] connect_with_args: pid_file={}", pid_path.display());
        eprintln!(
            "[diag] connect_with_args: pid_file exists={}",
            pid_path.exists()
        );

        // Try connecting directly first — daemon may already be running.
        match Self::platform_connect().await {
            Ok(client) => {
                eprintln!("[diag] connect_with_args: already connected to existing daemon");
                // S4.3.4: Verify daemon identity via PID file
                verify_daemon_after_connect();
                return Ok(client);
            }
            Err(conn_err) => {
                eprintln!("[diag] connect_with_args: initial connect failed: {conn_err}");
                drop(conn_err);
            }
        }

        // Auto-start the daemon using the same binary (`uffs daemon run`)
        tracing::info!("Daemon not running, auto-starting via `uffs daemon run`...");

        let uffs_exe = find_uffs_exe();
        eprintln!("[diag] connect_with_args: uffs_exe={}", uffs_exe.display());
        eprintln!(
            "[diag] connect_with_args: uffs_exe exists={}",
            uffs_exe.exists()
        );

        // Build args: ["daemon", "run", ...spawn_args]
        let mut cmd_args: Vec<&str> = vec!["daemon", "run"];
        for arg in spawn_args {
            cmd_args.push(arg.as_str());
        }
        eprintln!("[diag] connect_with_args: cmd_args={cmd_args:?}");

        // Spawn the daemon process.
        //
        // On Windows, MFT reading requires Administrator privileges. If the
        // current process is not elevated, we use `ShellExecuteW` with the
        // "runas" verb to trigger a UAC consent dialog. If already elevated
        // (or the broker service is available), we spawn normally.
        spawn_daemon(&uffs_exe, &cmd_args)?;
        eprintln!("[diag] connect_with_args: spawn_daemon returned OK");

        // Retry with backoff
        let mut delay_ms = 50_u64;
        let max_attempts = 20_usize;
        for attempt in 1_usize..=max_attempts {
            tokio::time::sleep(core::time::Duration::from_millis(delay_ms)).await;

            let sock_exists = sock.exists();
            let pid_exists = pid_path.exists();
            println!(
                "[diag] connect attempt {attempt}/{max_attempts}: delay={delay_ms}ms, socket_exists={sock_exists}, pid_exists={pid_exists}"
            );

            match Self::platform_connect().await {
                Ok(client) => {
                    println!("[diag] connect_with_args: connected on attempt {attempt}!");
                    tracing::info!(attempt, "Connected to daemon");
                    // S4.3.4: Verify daemon identity via PID file
                    verify_daemon_after_connect();
                    return Ok(client);
                }
                Err(conn_err) if attempt <= 3 || attempt == max_attempts => {
                    println!("[diag] connect attempt {attempt} failed: {conn_err}");
                }
                Err(_) => {}
            }

            delay_ms = (delay_ms * 2).min(2000);
        }

        println!("[diag] connect_with_args: all {max_attempts} attempts exhausted, giving up");
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

        tracing::info!(id, method, "send_request: writing request");
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
        tracing::info!(id, method, "send_request: write+flush done, reading response");

        // Read lines until we get a response with matching id.
        // Notifications (no id) are routed to the notification channel.
        loop {
            let mut line = String::new();
            let read_result = tokio::time::timeout(
                core::time::Duration::from_secs(300),
                self.reader.read_line(&mut line),
            )
            .await
            .map_err(|_timeout_err| {
                tracing::info!(id, method, "send_request: read timed out after 300s");
                crate::error::ClientError::Timeout
            })?
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

            // It's a response — could be success (has `result`) or error (has `error`).
            let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|err| {
                crate::error::ClientError::Protocol(format!("Bad response: {err}"))
            })?;

            // Check for JSON-RPC error response first.
            if let Some(err_obj) = value.get("error") {
                let msg = err_obj
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown RPC error");
                return Err(crate::error::ClientError::Protocol(msg.to_owned()));
            }

            // Success response.
            if let Some(result) = value.get("result") {
                return Ok(result.clone());
            }

            return Err(crate::error::ClientError::Protocol(
                "Response has neither `result` nor `error`".to_owned(),
            ));
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
        let response: SearchResponse = serde_json::from_value(result)
            .map_err(|err| crate::error::ClientError::Protocol(err.to_string()))?;

        // D5.1: transparent shmem reading — if the daemon used shmem,
        // read the file and return a response with inline rows.
        if let Some(path_str) = &response.shmem_path {
            let t_shmem = std::time::Instant::now();
            let path = std::path::Path::new(path_str);
            let shmem_response = crate::shmem::read_search_results(path).map_err(|err| {
                crate::error::ClientError::Protocol(format!("shmem read failed: {err}"))
            })?;
            let shmem_read_ms = t_shmem.elapsed().as_millis();
            let row_count = shmem_response.rows.len();
            tracing::info!(
                rows = row_count,
                shmem_read_ms = shmem_read_ms,
                path = %path_str,
                "🗂️ shmem: read bulk results"
            );
            #[expect(clippy::print_stderr, reason = "UFFS_CACHE_PROFILE diagnostic output")]
            if std::env::var_os("UFFS_CACHE_PROFILE").is_some() {
                eprintln!(
                    "[CACHE_PROFILE] shmem_read:  {shmem_read_ms:>6} ms  ({row_count} rows from shmem)"
                );
            }
            return Ok(shmem_response);
        }

        Ok(response)
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

    /// Query daemon performance statistics (queries, timing, startup).
    ///
    /// # Errors
    ///
    /// Returns a `ClientError` on connection, protocol, or timeout failure.
    pub async fn stats(
        &mut self,
    ) -> Result<crate::protocol::StatsResponse, crate::error::ClientError> {
        let result = self.send_request("stats", None).await?;
        serde_json::from_value(result)
            .map_err(|err| crate::error::ClientError::Protocol(err.to_string()))
    }

    /// Wait until the daemon has finished loading its indices.
    ///
    /// Polls `status()` with exponential backoff (250ms → 2s cap) until the
    /// daemon reports [`DaemonStatus::Ready`].  Times out after
    /// `timeout` and returns an error.
    ///
    /// If multiple consecutive I/O errors occur (e.g. broken pipe from a
    /// stale socket), the client automatically reconnects to the daemon.
    ///
    /// # Errors
    ///
    /// Returns `ClientError` on connection failure or timeout.
    pub async fn await_ready(
        &mut self,
        timeout: core::time::Duration,
    ) -> Result<(), crate::error::ClientError> {
        /// Consecutive I/O errors before attempting a reconnect.
        const RECONNECT_THRESHOLD: u32 = 3;

        let deadline = tokio::time::Instant::now() + timeout;
        let mut delay_ms = 250_u64;
        let mut poll_count = 0_u32;
        let mut consecutive_io_errors = 0_u32;

        loop {
            poll_count += 1;
            tracing::info!(poll_count, delay_ms, "await_ready: sending status poll");
            match self.status().await {
                Ok(resp) => {
                    consecutive_io_errors = 0;
                    tracing::info!(poll_count, status = ?resp.status, "await_ready: got status");
                    if resp.status == crate::protocol::DaemonStatus::Ready {
                        return Ok(());
                    }
                }
                Err(
                    err @ (crate::error::ClientError::Io(_)
                    | crate::error::ClientError::ConnectionClosed),
                ) => {
                    consecutive_io_errors += 1;
                    tracing::info!(
                        poll_count,
                        consecutive_io_errors,
                        error = %err,
                        "await_ready: status poll I/O error"
                    );

                    if consecutive_io_errors >= RECONNECT_THRESHOLD {
                        tracing::info!(
                            consecutive_io_errors,
                            "await_ready: reconnecting to daemon"
                        );
                        match Self::platform_connect().await {
                            Ok(new_client) => {
                                self.reader = new_client.reader;
                                self.writer = new_client.writer;
                                self.next_id = new_client.next_id;
                                self.notification_tx = new_client.notification_tx;
                                self.notification_rx = new_client.notification_rx;
                                consecutive_io_errors = 0;
                                tracing::info!("await_ready: reconnected successfully");
                            }
                            Err(reconn_err) => {
                                tracing::info!(
                                    error = %reconn_err,
                                    "await_ready: reconnect failed, will retry"
                                );
                            }
                        }
                    }
                }
                Err(status_err) => {
                    tracing::info!(poll_count, error = %status_err, "await_ready: status poll failed");
                }
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(crate::error::ClientError::ConnectionFailed(
                    "Timed out waiting for daemon to finish loading".to_owned(),
                ));
            }

            tokio::time::sleep(core::time::Duration::from_millis(delay_ms)).await;
            delay_ms = (delay_ms * 2).min(2000);
        }
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

        // Set a read timeout so the bridge-read thread can detect when the
        // daemon closes the socket and exit promptly instead of blocking forever.
        std_stream
            .set_read_timeout(Some(core::time::Duration::from_secs(5)))
            .map_err(|io_err| crate::error::ClientError::ConnectionFailed(io_err.to_string()))?;

        let std_read = std_stream
            .try_clone()
            .map_err(|io_err| crate::error::ClientError::ConnectionFailed(io_err.to_string()))?;
        let std_write = std_stream;

        // Create async duplex channels that bridge to the blocking socket.
        // 64KB buffer is plenty for JSON-RPC messages.
        let (async_read, mut bridge_write) = tokio::io::duplex(65536);
        let (mut bridge_read, async_write) = tokio::io::duplex(65536);

        // Each bridge thread gets its own dedicated tokio current-thread
        // runtime.  Previous versions shared the caller's runtime via
        // Handle::block_on, which caused subtle waker/scheduling issues —
        // the DuplexStream EOF was delivered prematurely, collapsing the
        // bridge after ~10 RPCs.  Isolated runtimes eliminate that class
        // of bugs entirely.

        // Background thread: std socket → async reader (bridge_write)
        std::thread::spawn(move || {
            tracing::info!("[client-bridge-read] thread started");
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(rt_err) => {
                    tracing::info!(error = %rt_err, "[client-bridge-read] failed to create runtime");
                    return;
                }
            };
            rt.block_on(async move {
                use tokio::io::AsyncWriteExt;
                let mut reader = std::io::BufReader::new(std_read);
                let mut buf = [0_u8; 8192];
                loop {
                    let n = match reader.read(&mut buf) {
                        Ok(0) => {
                            tracing::info!("[client-bridge-read] EOF from socket");
                            break;
                        }
                        Err(ref read_err) if read_err.kind() == std::io::ErrorKind::TimedOut
                            || read_err.kind() == std::io::ErrorKind::WouldBlock =>
                        {
                            // Read timeout — check if bridge_write is still alive
                            // by attempting a zero-byte write.
                            if bridge_write.write_all(&[]).await.is_err() {
                                tracing::info!("[client-bridge-read] bridge closed during timeout, exiting");
                                break;
                            }
                            continue; // retry the read
                        }
                        Err(read_err) => {
                            tracing::info!(
                                error = %read_err,
                                kind = ?read_err.kind(),
                                "[client-bridge-read] read error from socket"
                            );
                            break;
                        }
                        Ok(n) => n,
                    };
                    if bridge_write.write_all(&buf[..n]).await.is_err() {
                        tracing::info!("[client-bridge-read] bridge_write failed");
                        break;
                    }
                }
            });
            tracing::info!("[client-bridge-read] thread exiting");
        });

        // Background thread: async bridge_read → std socket
        std::thread::spawn(move || {
            tracing::info!("[client-bridge-write] thread started");
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(rt_err) => {
                    tracing::info!(error = %rt_err, "[client-bridge-write] failed to create runtime");
                    return;
                }
            };
            let mut writer = std_write;
            rt.block_on(async {
                use tokio::io::AsyncReadExt;
                let mut buf = [0_u8; 8192];
                loop {
                    match bridge_read.read(&mut buf).await {
                        Ok(0) => {
                            tracing::info!("[client-bridge-write] EOF from bridge (async_write dropped)");
                            break;
                        }
                        Err(read_err) => {
                            tracing::info!(
                                error = %read_err,
                                "[client-bridge-write] bridge read error"
                            );
                            break;
                        }
                        Ok(n) => {
                            if writer.write_all(&buf[..n]).is_err() {
                                tracing::info!(
                                    "[client-bridge-write] socket write_all failed"
                                );
                                break;
                            }
                            if writer.flush().is_err() {
                                tracing::info!(
                                    "[client-bridge-write] socket flush failed"
                                );
                                break;
                            }
                        }
                    }
                }
            });
            tracing::info!("[client-bridge-write] thread exiting");
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
///
/// Best-effort: write errors are intentionally ignored because a failed
/// keepalive simply means the connection timed out.
///
/// Kept as a standalone function (rather than inlined) for clarity: it is the
/// blocking closure body passed to `spawn_blocking` and contains platform-
/// specific `#[cfg]` blocks that would clutter the caller.
#[allow(clippy::single_call_fn)] // extracted for readability with multi-cfg blocks
fn keepalive_send_blocking(sock_path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        if let Ok(mut stream) = UnixStream::connect(sock_path) {
            let msg = r#"{"jsonrpc":"2.0","id":0,"method":"keepalive"}"#;
            drop(stream.write_all(msg.as_bytes()));
            drop(stream.write_all(b"\n"));
            drop(stream.flush());
        }
    }
    #[cfg(windows)]
    {
        use std::io::Write;
        use std::os::windows::net::UnixStream;
        if let Ok(mut stream) = UnixStream::connect(sock_path) {
            let msg = r#"{"jsonrpc":"2.0","id":0,"method":"keepalive"}"#;
            drop(stream.write_all(msg.as_bytes()));
            drop(stream.write_all(b"\n"));
            drop(stream.flush());
        }
    }
}

/// PID file path (must match daemon's lifecycle.rs).
#[must_use]
pub fn pid_file_path() -> PathBuf {
    let base = dirs_next::data_local_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("uffs").join("daemon.pid")
}

/// Parse a daemon PID file. Returns `(pid, timestamp, exe_hash, nonce)`.
///
/// Format: `{pid}\n{timestamp}\n{exe_hash}\n{nonce}\n`
#[must_use]
pub fn parse_pid_file(path: &std::path::Path) -> Option<(u32, u64, u64, String)> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut lines = content.lines();
    let pid: u32 = lines.next()?.parse().ok()?;
    let ts: u64 = lines.next()?.parse().ok()?;
    let hash: u64 = lines.next()?.parse().ok()?;
    let nonce = lines.next()?.to_owned();
    Some((pid, ts, hash, nonce))
}

/// Find the `uffs` executable (the CLI binary that also embeds the daemon).
///
/// Strategy:
/// 1. If the calling binary is `uffs` (or `uffs.exe`), use `current_exe()`.
/// 2. Otherwise (e.g. called from `uffs_tui`), look for `uffs` next to the
///    current binary, then fall back to `PATH`.
#[must_use]
pub fn find_uffs_exe() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let name = exe.file_stem().and_then(|stem| stem.to_str()).unwrap_or("");
        if name == "uffs" {
            return exe;
        }
        // Current binary is not `uffs` — look for it alongside the exe.
        if let Some(parent) = exe.parent() {
            let uffs_bin = if cfg!(windows) { "uffs.exe" } else { "uffs" };
            let sibling = parent.join(uffs_bin);
            if sibling.exists() {
                return sibling;
            }
        }
    }
    PathBuf::from("uffs")
}

/// Find the `uffs-daemon` executable (legacy — prefer `find_uffs_exe`).
///
/// Kept for backward compatibility: if a standalone `uffs-daemon` binary
/// exists next to `uffs`, it can still be used.
#[must_use]
pub fn find_daemon_exe() -> PathBuf {
    std::env::current_exe()
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
        .unwrap_or_else(|| PathBuf::from("uffs-daemon"))
}

// ── Daemon Spawn ──────────────────────────────────────────────────────────

/// Spawn the daemon as a detached background process.
///
/// On **Unix**, uses a normal `Command::new` spawn (no elevation needed).
///
/// On **Windows**, MFT reading requires Administrator privileges. If the
/// current process is already elevated, spawns directly with
/// `DETACHED_PROCESS`. Otherwise, uses `ShellExecuteW` with the `"runas"`
/// verb to trigger a UAC consent dialog so the daemon starts elevated.
///
/// # Errors
///
/// Returns `DaemonStartFailed` if spawning fails (Unix), if the UAC prompt
/// is denied, or if `ShellExecuteW` fails (Windows).
#[expect(
    clippy::single_call_fn,
    reason = "platform-specific spawn logic — clarity over inlining"
)]
fn spawn_daemon(exe: &std::path::Path, args: &[&str]) -> Result<(), crate::error::ClientError> {
    #[cfg(unix)]
    spawn_daemon_unix(exe, args)?;

    #[cfg(windows)]
    spawn_daemon_windows(exe, args)?;

    Ok(())
}

/// Unix daemon spawn: simple detached process.
#[cfg(unix)]
#[expect(
    clippy::single_call_fn,
    reason = "platform-specific helper — clarity over inlining"
)]
fn spawn_daemon_unix(
    exe: &std::path::Path,
    args: &[&str],
) -> Result<(), crate::error::ClientError> {
    std::process::Command::new(exe)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|spawn_err| {
            crate::error::ClientError::DaemonStartFailed(format!(
                "Failed to spawn {} daemon run: {spawn_err}",
                exe.display()
            ))
        })?;
    Ok(())
}

/// Windows daemon spawn: elevation-aware.
///
/// If already elevated, spawns directly with `DETACHED_PROCESS`.
/// Otherwise uses `ShellExecuteW("runas", ...)` to trigger a UAC prompt.
#[cfg(windows)]
#[expect(
    clippy::single_call_fn,
    reason = "platform-specific helper — clarity over inlining"
)]
#[expect(
    clippy::print_stdout,
    clippy::use_debug,
    reason = "temporary diagnostic output for debugging daemon start"
)]
fn spawn_daemon_windows(
    exe: &std::path::Path,
    args: &[&str],
) -> Result<(), crate::error::ClientError> {
    let elevated = is_elevated();
    println!("[diag] spawn_daemon_windows: exe={}", exe.display());
    println!("[diag] spawn_daemon_windows: args={args:?}");
    println!("[diag] spawn_daemon_windows: is_elevated={elevated}");

    if elevated {
        // Already elevated — spawn directly via CreateProcessW.
        //
        // We use CreateProcessW instead of std::process::Command because
        // Command always sets bInheritHandles=TRUE.  When the parent
        // process was itself started with pipe redirection (e.g.
        // `Command::output()`), the pipe handles are inheritable and
        // would leak into the daemon.  The caller's `.output()` then
        // blocks until the daemon exits (10-20 min idle timeout).
        //
        // CreateProcessW with bInheritHandles=FALSE prevents all handle
        // inheritance, so the daemon is fully detached from the parent's
        // I/O.
        println!("[diag] spawn_daemon_windows: spawning via CreateProcessW (no handle inheritance)...");
        spawn_detached_no_inherit(exe, args)?;
    } else {
        // Not elevated — use ShellExecuteW "runas" to trigger UAC.
        // ShellExecuteW always creates a fully new process — no handle
        // inheritance issues.
        println!("[diag] spawn_daemon_windows: NOT elevated, using ShellExecuteW runas");
        tracing::info!("Not elevated — requesting elevation via UAC prompt");
        shell_execute_elevated(exe, args)?;
        println!("[diag] spawn_daemon_windows: ShellExecuteW returned OK");
    }
    Ok(())
}


/// Spawn the daemon as a fully detached process with NO handle inheritance.
///
/// Uses `CreateProcessW` directly with `bInheritHandles = FALSE` and
/// `DETACHED_PROCESS` creation flag.  This prevents the daemon from
/// inheriting any of the parent's handles (especially stdout/stderr
/// pipes created by `Command::output()`), which would otherwise keep
/// the calling process alive until the daemon exits.
#[cfg(windows)]
fn spawn_detached_no_inherit(
    exe: &std::path::Path,
    args: &[&str],
) -> Result<(), crate::error::ClientError> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        CreateProcessW, DETACHED_PROCESS, PROCESS_INFORMATION, STARTUPINFOW,
    };

    // Build the command line: "exe" arg1 arg2 ...
    // CreateProcessW wants a single mutable command line string.
    let mut cmd_line = String::new();
    cmd_line.push('"');
    cmd_line.push_str(&exe.to_string_lossy());
    cmd_line.push('"');
    for arg in args {
        cmd_line.push(' ');
        cmd_line.push_str(arg);
    }

    // Convert to wide (UTF-16) null-terminated mutable buffer.
    let mut cmd_wide: Vec<u16> = cmd_line.encode_utf16().chain(core::iter::once(0)).collect();

    let si = STARTUPINFOW {
        cb: core::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();

    // SAFETY: CreateProcessW is a well-defined Win32 API. All pointers are
    // valid: cmd_wide is a mutable null-terminated UTF-16 buffer, si is
    // a zeroed STARTUPINFOW with cb set, pi is zeroed output buffer.
    // We close the returned handles immediately after success.
    #[expect(unsafe_code, reason = "CreateProcessW requires unsafe FFI")]
    let result = unsafe {
        CreateProcessW(
            None,                                    // lpApplicationName (use command line)
            Some(windows::core::PWSTR(cmd_wide.as_mut_ptr())), // lpCommandLine
            None,                                    // lpProcessAttributes
            None,                                    // lpThreadAttributes
            false,                                   // bInheritHandles = FALSE ← key fix
            DETACHED_PROCESS,                        // dwCreationFlags
            None,                                    // lpEnvironment (inherit)
            None,                                    // lpCurrentDirectory (inherit)
            &si,                                     // lpStartupInfo
            &mut pi,                                 // lpProcessInformation
        )
    };

    match result {
        Ok(()) => {
            println!("[diag] spawn_detached_no_inherit: spawned PID={}", pi.dwProcessId);
            tracing::info!(pid = pi.dwProcessId, "Daemon spawned (no handle inheritance)");
            // Close the process and thread handles — we don't need them.
            // SAFETY: valid handles returned by CreateProcessW.
            #[expect(unsafe_code, reason = "closing Win32 handles from CreateProcessW")]
            unsafe {
                let _ = CloseHandle(pi.hProcess);
                let _ = CloseHandle(pi.hThread);
            }
            Ok(())
        }
        Err(win_err) => {
            println!("[diag] spawn_detached_no_inherit: FAILED: {win_err}");
            Err(crate::error::ClientError::DaemonStartFailed(format!(
                "CreateProcessW failed for {}: {win_err}",
                exe.display()
            )))
        }
    }
}

// ── Windows Elevation Helpers ─────────────────────────────────────────────

/// Check if the current process is running with Administrator privileges.
///
/// Uses `OpenProcessToken` + `GetTokenInformation(TokenElevation)`.
#[cfg(windows)]
fn is_elevated() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // SAFETY: Win32 token query APIs are well-defined and we close the handle.
    #[expect(
        unsafe_code,
        reason = "Win32 token elevation check requires unsafe FFI"
    )]
    unsafe {
        let mut token = windows::Win32::Foundation::HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut size = 0_u32;
        let result = GetTokenInformation(
            token,
            TokenElevation,
            Some(core::ptr::from_mut(&mut elevation).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        );
        let _ = CloseHandle(token);
        result.is_ok() && elevation.TokenIsElevated != 0
    }
}

/// Spawn a process with elevation via `ShellExecuteW("runas", ...)`.
///
/// This triggers a Windows UAC consent dialog. The daemon process starts
/// elevated and can read the NTFS MFT directly.
///
/// # Errors
///
/// Returns `DaemonStartFailed` if the user denies the UAC prompt or if the
/// Win32 call fails.
#[cfg(windows)]
fn shell_execute_elevated(
    exe: &std::path::Path,
    args: &[&str],
) -> Result<(), crate::error::ClientError> {
    use std::os::windows::ffi::OsStrExt;

    // Build the arguments as a single space-separated string.
    let params_string = args.join(" ");

    // Convert strings to wide (UTF-16) null-terminated.
    let verb: Vec<u16> = "runas\0".encode_utf16().collect();
    let file: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
    let params: Vec<u16> = params_string.encode_utf16().chain(Some(0)).collect();

    // ShellExecuteW returns HINSTANCE; values > 32 indicate success.
    // SAFETY: ShellExecuteW is a well-defined Win32 Shell API. All pointers
    // are valid null-terminated UTF-16 strings allocated above.
    #[expect(unsafe_code, reason = "ShellExecuteW requires unsafe FFI")]
    let result = unsafe {
        // ShellExecuteW is in shell32.dll — use raw FFI to avoid adding
        // Win32_UI_Shell feature to the windows crate.
        #[link(name = "shell32")]
        unsafe extern "system" {
            fn ShellExecuteW(
                hwnd: *mut core::ffi::c_void,
                operation: *const u16,
                file: *const u16,
                parameters: *const u16,
                directory: *const u16,
                show_cmd: i32,
            ) -> isize;
        }

        ShellExecuteW(
            core::ptr::null_mut(), // hwnd
            verb.as_ptr(),         // "runas"
            file.as_ptr(),         // exe path
            params.as_ptr(),       // arguments
            core::ptr::null(),     // directory (inherit)
            0,                     // SW_HIDE
        )
    };

    // HINSTANCE > 32 means success.
    if result > 32 {
        tracing::info!("Daemon spawned with elevation (ShellExecuteW returned {result})");
        Ok(())
    } else {
        // Common error codes:
        // SE_ERR_ACCESSDENIED (5) — user denied UAC
        // 0 — out of memory
        Err(crate::error::ClientError::DaemonStartFailed(format!(
            "UAC elevation failed (ShellExecuteW returned {result}). \
             Run your terminal as Administrator, or install the uffs-broker service."
        )))
    }
}
