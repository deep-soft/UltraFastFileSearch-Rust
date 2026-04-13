// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

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
//! Exception: `file_size_policy` — connection lifecycle is one cohesive flow.

use core::sync::atomic::{AtomicU64, Ordering};

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
    /// via [`Self::connect_with_args`] so the daemon knows where to find data.
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
    pub async fn connect_with_args(
        spawn_args: &[String],
    ) -> Result<Self, crate::error::ClientError> {
        let sock = socket_path();
        let pid_path = pid_file_path();
        tracing::debug!(
            socket_path = %sock.display(),
            socket_exists = sock.exists(),
            pid_file = %pid_path.display(),
            pid_file_exists = pid_path.exists(),
            "connect_with_args: paths"
        );

        // Try connecting directly first — daemon may already be running.
        if let Ok(client) = Self::try_connect_existing().await {
            return Ok(client);
        }

        // Auto-start the daemon using the same binary (`uffs daemon run`)
        Self::auto_start_daemon(spawn_args)?;

        // Retry with exponential backoff until connected.
        Self::retry_connect(&sock, &pid_path).await
    }

    /// Attempt to connect to an already-running daemon.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`](crate::error::ClientError) if no daemon is
    /// running or the connection handshake fails.
    async fn try_connect_existing() -> Result<Self, crate::error::ClientError> {
        match Self::platform_connect().await {
            Ok(client) => {
                tracing::debug!("connect_with_args: already connected to existing daemon");
                verify_daemon_after_connect();
                Ok(client)
            }
            Err(conn_err) => {
                tracing::debug!(%conn_err, "connect_with_args: initial connect failed");
                Err(conn_err)
            }
        }
    }

    /// Spawn the daemon process with the given extra args.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`](crate::error::ClientError) if the daemon
    /// executable cannot be found or the spawn fails.
    fn auto_start_daemon(spawn_args: &[String]) -> Result<(), crate::error::ClientError> {
        tracing::info!("Daemon not running, auto-starting via `uffs daemon run`...");

        let uffs_exe = find_uffs_exe();
        let mut cmd_args: Vec<&str> = vec!["daemon", "run"];
        for arg in spawn_args {
            cmd_args.push(arg.as_str());
        }
        log_spawn_details(&uffs_exe, &cmd_args);

        // On Windows, MFT reading requires Administrator privileges. If the
        // current process is not elevated, we use `ShellExecuteW` with the
        // "runas" verb to trigger a UAC consent dialog. If already elevated
        // (or the broker service is available), we spawn normally.
        spawn_daemon(&uffs_exe, &cmd_args)?;
        tracing::debug!("auto_start_daemon: spawn returned OK");
        Ok(())
    }

    /// Retry connecting to the daemon with exponential backoff.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`](crate::error::ClientError) if all connection
    /// attempts are exhausted.
    async fn retry_connect(
        sock: &std::path::Path,
        pid_path: &std::path::Path,
    ) -> Result<Self, crate::error::ClientError> {
        let mut delay_ms = 50_u64;
        let max_attempts = 20_usize;
        for attempt in 1_usize..=max_attempts {
            tokio::time::sleep(core::time::Duration::from_millis(delay_ms)).await;
            log_connect_attempt(attempt, max_attempts, delay_ms, sock, pid_path);

            match Self::platform_connect().await {
                Ok(client) => {
                    tracing::info!(attempt, "Connected to daemon");
                    verify_daemon_after_connect();
                    return Ok(client);
                }
                Err(conn_err) => {
                    log_connect_error(attempt, max_attempts, &conn_err);
                }
            }

            delay_ms = (delay_ms * 2).min(2000);
        }

        tracing::warn!(max_attempts, "all connect attempts exhausted");
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
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`](crate::error::ClientError) if serialisation,
    /// I/O, or response parsing fails.
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
        tracing::info!(
            id,
            method,
            "send_request: write+flush done, reading response"
        );

        // Read lines until we get a response with matching id.
        // Notifications (no id) are routed to the notification channel.
        loop {
            let mut line = String::new();
            let read_result = tokio::time::timeout(
                core::time::Duration::from_mins(5),
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
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
                && value.get("method").is_some()
                && value.get("id").is_none()
            {
                // It's a notification — route to channel
                if let Ok(notif) = serde_json::from_value::<crate::protocol::RpcNotification>(value)
                {
                    drop(self.notification_tx.send(notif));
                }
                continue; // keep reading for the actual response
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
            tracing::debug!(
                target: "cache_profile",
                shmem_read_ms = %shmem_read_ms,
                row_count,
                "shmem_read"
            );
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
    /// daemon reports [`crate::protocol::DaemonStatus::Ready`].  Times out
    /// after `timeout` and returns an error.
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

            match self.poll_status_once(poll_count).await {
                PollOutcome::Ready => return Ok(()),
                PollOutcome::NotReady => {
                    consecutive_io_errors = 0;
                }
                PollOutcome::IoError => {
                    consecutive_io_errors += 1;
                    if consecutive_io_errors >= RECONNECT_THRESHOLD {
                        self.attempt_reconnect(&mut consecutive_io_errors).await;
                    }
                }
                PollOutcome::OtherError => {}
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

    /// Attempt a reconnect to the daemon, replacing internal reader/writer.
    async fn attempt_reconnect(&mut self, consecutive_io_errors: &mut u32) {
        let error_count = *consecutive_io_errors;
        tracing::info!(error_count, "await_ready: reconnecting to daemon");
        match Self::platform_connect().await {
            Ok(new_client) => {
                self.reader = new_client.reader;
                self.writer = new_client.writer;
                self.next_id = new_client.next_id;
                self.notification_tx = new_client.notification_tx;
                self.notification_rx = new_client.notification_rx;
                *consecutive_io_errors = 0;
                tracing::info!("await_ready: reconnected successfully");
            }
            Err(reconn_err) => {
                tracing::info!(error = %reconn_err, "await_ready: reconnect failed, will retry");
            }
        }
    }

    /// Hot-load MFT files into the running daemon.
    ///
    /// Files whose drive letter is already loaded are skipped.  Returns
    /// which drives were loaded, which were already present, and any errors.
    ///
    /// # Errors
    ///
    /// Returns a `ClientError` on connection, protocol, or timeout failure.
    pub async fn load_drive(
        &mut self,
        mft_files: &[String],
        no_cache: bool,
    ) -> Result<crate::protocol::LoadDriveResponse, crate::error::ClientError> {
        let params = serde_json::json!({
            "mft_files": mft_files,
            "no_cache": no_cache,
        });
        let result = self.send_request("load_drive", Some(params)).await?;
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
        let _: &Self = self; // keepalive uses a separate connection, not &self
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

// ── Readiness polling helpers ────────────────────────────────────────

/// Outcome of a single status poll in [`UffsClient::await_ready`].
enum PollOutcome {
    /// Daemon reports `Ready`.
    Ready,
    /// Daemon responded but is still loading.
    NotReady,
    /// I/O or connection-closed error (may need reconnect).
    IoError,
    /// Non-I/O protocol error.
    OtherError,
}

impl UffsClient {
    /// Poll the daemon status once, returning a classified outcome.
    async fn poll_status_once(&mut self, poll_count: u32) -> PollOutcome {
        match self.status().await {
            Ok(resp) => {
                tracing::info!(poll_count, status = ?resp.status, "await_ready: got status");
                if resp.status == crate::protocol::DaemonStatus::Ready {
                    PollOutcome::Ready
                } else {
                    PollOutcome::NotReady
                }
            }
            Err(
                err @ (crate::error::ClientError::Io(_)
                | crate::error::ClientError::ConnectionClosed),
            ) => {
                tracing::info!(poll_count, error = %err, "await_ready: status poll I/O error");
                PollOutcome::IoError
            }
            Err(status_err) => {
                tracing::info!(
                    poll_count,
                    error = %status_err,
                    "await_ready: status poll failed"
                );
                PollOutcome::OtherError
            }
        }
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
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`](crate::error::ClientError) if the Unix socket
    /// connection fails.
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
                        Err(ref read_err)
                            if read_err.kind() == std::io::ErrorKind::TimedOut
                                || read_err.kind() == std::io::ErrorKind::WouldBlock =>
                        {
                            // Read timeout — check if bridge_write is still alive
                            // by attempting a zero-byte write.
                            if bridge_write.write_all(&[]).await.is_err() {
                                tracing::info!(
                                    "[client-bridge-read] bridge closed during timeout, exiting"
                                );
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
                            tracing::info!(
                                "[client-bridge-write] EOF from bridge (async_write dropped)"
                            );
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
                                tracing::info!("[client-bridge-write] socket write_all failed");
                                break;
                            }
                            if writer.flush().is_err() {
                                tracing::info!("[client-bridge-write] socket flush failed");
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

// ── Daemon lifecycle helpers (extracted to daemon_ctl.rs) ──────────────────
// Re-exported here so callers see no change.
pub use crate::daemon_ctl::{
    find_daemon_exe, find_uffs_exe, parse_pid_file, pid_file_path, socket_path,
};
pub(crate) use crate::daemon_ctl::{
    keepalive_send_blocking, spawn_daemon, verify_daemon_after_connect,
};

// ── Logging helpers ─────────────────────────────────────────────────
// Extracted to keep calling functions under the cognitive-complexity limit.

/// Log daemon spawn details (exe path, existence, command args).
fn log_spawn_details(uffs_exe: &std::path::Path, cmd_args: &[&str]) {
    tracing::debug!(
        uffs_exe = %uffs_exe.display(),
        uffs_exe_exists = uffs_exe.exists(),
        ?cmd_args,
        "auto_start_daemon: resolved exe, spawning"
    );
}

/// Log a connect retry attempt with socket/PID file status.
fn log_connect_attempt(
    attempt: usize,
    max_attempts: usize,
    delay_ms: u64,
    sock: &std::path::Path,
    pid_path: &std::path::Path,
) {
    tracing::debug!(
        attempt,
        max_attempts,
        delay_ms,
        sock_exists = sock.exists(),
        pid_exists = pid_path.exists(),
        "connect attempt"
    );
}

/// Log a failed connect attempt (only for first 3 and final attempts to avoid
/// spam).
fn log_connect_error(attempt: usize, max_attempts: usize, err: &crate::error::ClientError) {
    if attempt <= 3 || attempt == max_attempts {
        tracing::debug!(attempt, %err, "connect attempt failed");
    }
}
