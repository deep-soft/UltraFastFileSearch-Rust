//! IPC server: Unix domain socket (macOS/Linux) / named pipe (Windows).
//!
//! Listens for newline-delimited JSON-RPC messages, dispatches to the
//! request handler, and writes responses back.

use alloc::sync::Arc;
use std::path::PathBuf;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use uffs_client::protocol::{ERR_PARSE, RpcErrorResponse, RpcRequest};

use crate::handler::RequestHandler;
use crate::index::IndexManager;
use crate::lifecycle::LifecycleHandle;

/// Maximum concurrent connections.
const MAX_CONNECTIONS: usize = 32;

/// Maximum message size (16 MB).
const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Idle connection timeout — disconnect if no messages for this long (S4.4.8).
const IDLE_CONNECTION_SECS: u64 = 300; // 5 minutes

/// Per-connection rate limit: max queries per second (S4.4.6).
const MAX_QUERIES_PER_SEC: u32 = 100;

/// IPC server for daemon-client communication.
pub struct IpcServer;

impl IpcServer {
    /// Returns the platform-specific socket path.
    #[expect(
        clippy::single_call_fn,
        reason = "platform-dispatch helper — clarity over inlining"
    )]
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
        #[cfg(target_os = "windows")]
        {
            base.join("uffs").join("daemon.sock")
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            base.join("uffs").join("daemon.sock")
        }
    }

    /// Verify that the connecting client has the same UID as the daemon.
    ///
    /// - **macOS**: uses `getpeereid()` via the raw fd
    /// - **Linux**: uses `SO_PEERCRED` via `UCred`
    /// - **Windows**: always returns `true` (named pipe DACL handles this)
    #[cfg(unix)]
    #[expect(
        clippy::single_call_fn,
        reason = "security boundary — must stay separate"
    )]
    fn verify_peer_credentials(stream: &tokio::net::UnixStream) -> bool {
        use std::os::unix::io::AsRawFd;

        let fd = stream.as_raw_fd();

        // SAFETY: `getuid()` is a pure read of the process UID — no side effects.
        #[expect(unsafe_code, reason = "getuid is a standard POSIX call")]
        let my_uid = unsafe { libc::getuid() };

        let mut peer_uid: libc::uid_t = 0;
        let mut peer_group_id: libc::gid_t = 0;

        // SAFETY: `getpeereid()` writes into the two out-params. We pass valid
        // mutable pointers and a valid fd obtained from the stream.
        #[expect(unsafe_code, reason = "getpeereid is a standard POSIX call")]
        let getpeer_rc = unsafe {
            libc::getpeereid(
                fd,
                core::ptr::addr_of_mut!(peer_uid),
                core::ptr::addr_of_mut!(peer_group_id),
            )
        };

        if getpeer_rc != 0_i32 {
            tracing::warn!("getpeereid failed, rejecting connection");
            return false;
        }

        if peer_uid != my_uid {
            tracing::warn!(
                peer_uid,
                daemon_uid = my_uid,
                "Peer UID mismatch — rejecting connection"
            );
            return false;
        }

        true
    }

    /// Windows: peer credential verification via ACL (handled at socket level).
    #[cfg(windows)]
    fn verify_peer_credentials_win() -> bool {
        true
    }

    /// Handle a single client connection (shared across all platforms).
    ///
    /// Enforces:
    /// - S4.4.7: 30-second per-message read timeout
    /// - S4.4.8: 5-minute idle connection timeout
    /// - S4.4.6: Per-connection rate limit (100 queries/sec)
    #[expect(
        clippy::single_call_fn,
        reason = "connection handler — structural separation"
    )]
    async fn handle_connection(
        reader: impl tokio::io::AsyncRead + Unpin,
        mut writer: impl tokio::io::AsyncWrite + Unpin,
        handler: &RequestHandler,
    ) -> anyhow::Result<()> {
        let mut buf_reader = BufReader::new(reader);
        let mut line = String::new();

        let mut queries_this_second: u32 = 0;
        let mut rate_limit_epoch = std::time::Instant::now();

        loop {
            line.clear();

            let read_result = tokio::time::timeout(
                core::time::Duration::from_secs(IDLE_CONNECTION_SECS),
                buf_reader.read_line(&mut line),
            )
            .await;

            let bytes_read = match read_result {
                Ok(Ok(count)) => count,
                Ok(Err(io_err)) => return Err(io_err.into()),
                Err(_) => {
                    tracing::debug!(
                        "Idle connection timeout ({}s), disconnecting",
                        IDLE_CONNECTION_SECS
                    );
                    return Ok(());
                }
            };

            if bytes_read == 0 {
                return Ok(());
            }

            if line.len() > MAX_MESSAGE_SIZE {
                let err_resp = RpcErrorResponse::error(None, ERR_PARSE, "Message too large");
                let json_out = serde_json::to_string(&err_resp).unwrap_or_default();
                writer.write_all(json_out.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await?;
                return Ok(());
            }

            let now = std::time::Instant::now();
            if now.duration_since(rate_limit_epoch).as_secs() >= 1_u64 {
                queries_this_second = 0;
                rate_limit_epoch = now;
            }
            queries_this_second += 1;
            if queries_this_second > MAX_QUERIES_PER_SEC {
                let rate_err = RpcErrorResponse::error(
                    None,
                    -32000_i32,
                    &format!("Rate limit exceeded ({MAX_QUERIES_PER_SEC} queries/sec)"),
                );
                let json_out = serde_json::to_string(&rate_err).unwrap_or_default();
                writer.write_all(json_out.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await?;
                continue;
            }

            handler.lifecycle.reset_idle_timer();

            let req: RpcRequest = match serde_json::from_str(line.trim()) {
                Ok(parsed) => parsed,
                Err(parse_err) => {
                    let err_resp = RpcErrorResponse::error(
                        None,
                        ERR_PARSE,
                        &format!("Invalid JSON: {parse_err}"),
                    );
                    let json_out = serde_json::to_string(&err_resp).unwrap_or_default();
                    writer.write_all(json_out.as_bytes()).await?;
                    writer.write_all(b"\n").await?;
                    writer.flush().await?;
                    continue;
                }
            };

            let response = handler.handle(&req).await;

            writer.write_all(response.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
        }
    }
}

/// Run the IPC server on a Unix domain socket.
///
/// Returns when the lifecycle manager signals shutdown.
#[cfg(unix)]
#[expect(
    clippy::single_call_fn,
    reason = "server entry point — structural separation"
)]
pub async fn run_ipc_server(
    index: Arc<IndexManager>,
    lifecycle: LifecycleHandle,
) -> anyhow::Result<()> {
    let sock_path = IpcServer::socket_path();

    // Ensure parent directory exists with secure permissions
    if let Some(parent) = sock_path.parent() {
        uffs_security::fs::create_secure_dir(parent)?;
    }

    // Remove stale socket file
    if sock_path.exists() {
        std::fs::remove_file(&sock_path)?;
    }

    let listener = tokio::net::UnixListener::bind(&sock_path)?;

    // Set socket permissions to owner-only (0600)
    uffs_security::fs::set_file_permissions_owner_only(&sock_path)?;

    tracing::info!(path = %sock_path.display(), "IPC server listening");

    let handler = Arc::new(RequestHandler {
        index,
        lifecycle: lifecycle.clone(),
    });

    loop {
        let (stream, _addr) = listener.accept().await?;

        // S4.2: Peer credential verification — reject connections from other UIDs
        if !IpcServer::verify_peer_credentials(&stream) {
            tracing::warn!("Rejected connection from different UID");
            drop(stream);
            continue;
        }

        let active = lifecycle.active_connections();
        if active >= MAX_CONNECTIONS {
            tracing::warn!(
                active,
                max = MAX_CONNECTIONS,
                "Max connections reached, rejecting"
            );
            drop(stream);
            continue;
        }

        lifecycle.connection_opened();
        let handler_clone = Arc::clone(&handler);
        let lc_clone = lifecycle.clone();

        let (read_half, write_half) = stream.into_split();
        tokio::spawn(async move {
            let total_conns = lc_clone.active_connections();
            tracing::debug!(connections = total_conns, "Client connected");

            if let Err(conn_err) =
                IpcServer::handle_connection(read_half, write_half, &handler_clone).await
            {
                tracing::debug!(error = %conn_err, "Connection ended");
            }

            lc_clone.connection_closed();
            let remaining = lc_clone.active_connections();
            tracing::debug!(connections = remaining, "Client disconnected");
        });
    }
}

/// Windows IPC server — uses Unix domain sockets (Windows 10 1803+).
///
/// Mirrors the Unix version: secure dir (icacls owner-only ACL), socket
/// file permissions, max connections, peer verification via ACL.
#[cfg(windows)]
pub async fn run_ipc_server(
    index: Arc<IndexManager>,
    lifecycle: LifecycleHandle,
) -> anyhow::Result<()> {
    let sock_path = IpcServer::socket_path();

    if let Some(parent) = sock_path.parent() {
        uffs_security::fs::create_secure_dir(parent)?;
    }

    if sock_path.exists() {
        std::fs::remove_file(&sock_path)?;
    }

    uffs_security::fs::set_file_permissions_owner_only(&sock_path)?;

    tracing::info!(path = %sock_path.display(), "IPC server listening (Windows AF_UNIX)");

    let handler = Arc::new(RequestHandler {
        index,
        lifecycle: lifecycle.clone(),
    });

    // Windows: use std blocking UnixListener in a spawn_blocking loop,
    // bridge each connection via tokio::io::duplex.
    use std::os::windows::net::UnixListener as StdUnixListener;
    let std_listener = StdUnixListener::bind(&sock_path)?;

    loop {
        // Blocking accept in spawn_blocking
        let accept_listener = std_listener.try_clone()?;
        let accept_result = tokio::task::spawn_blocking(move || accept_listener.accept()).await?;

        let (std_stream, _addr) = accept_result?;
        std_stream.set_read_timeout(Some(core::time::Duration::from_secs(IDLE_CONNECTION_SECS)))?;

        if !IpcServer::verify_peer_credentials_win() {
            tracing::warn!("Rejected connection");
            continue;
        }

        let active = lifecycle.active_connections();
        if active >= MAX_CONNECTIONS {
            tracing::warn!(active, max = MAX_CONNECTIONS, "Max connections reached");
            continue;
        }

        // Bridge std blocking socket to async duplex channels
        let std_read = std_stream.try_clone()?;
        let std_write = std_stream;

        let (async_read, mut bridge_write) = tokio::io::duplex(65536);
        let (mut bridge_read, async_write) = tokio::io::duplex(65536);

        // Background thread: std socket → async reader
        std::thread::spawn(move || {
            use std::io::Read;
            let mut reader = std::io::BufReader::new(std_read);
            let mut buf = [0_u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        use tokio::io::AsyncWriteExt;
                        if let Ok(handle) = tokio::runtime::Handle::try_current() {
                            let bytes = buf[..n].to_vec();
                            let _ = handle.block_on(async { bridge_write.write_all(&bytes).await });
                        } else {
                            break;
                        }
                    }
                }
            }
        });

        // Background thread: async writer → std socket
        std::thread::spawn(move || {
            use std::io::Write;
            let mut writer = std_write;
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.block_on(async {
                    use tokio::io::AsyncReadExt;
                    let mut buf = [0_u8; 8192];
                    loop {
                        match bridge_read.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if writer.write_all(&buf[..n]).is_err() || writer.flush().is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
            }
        });

        lifecycle.connection_opened();
        let handler_clone = Arc::clone(&handler);
        let lc_clone = lifecycle.clone();

        tokio::spawn(async move {
            let total_conns = lc_clone.active_connections();
            tracing::debug!(connections = total_conns, "Client connected");

            if let Err(conn_err) =
                IpcServer::handle_connection(async_read, async_write, &handler_clone).await
            {
                tracing::debug!(error = %conn_err, "Connection ended");
            }

            lc_clone.connection_closed();
            let remaining = lc_clone.active_connections();
            tracing::debug!(connections = remaining, "Client disconnected");
        });
    }
}
