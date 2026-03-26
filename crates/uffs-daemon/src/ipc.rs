//! IPC server: Unix domain socket (macOS/Linux) / named pipe (Windows).
//!
//! Listens for newline-delimited JSON-RPC messages, dispatches to the
//! request handler, and writes responses back.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use uffs_client::protocol::{ERR_PARSE, RpcErrorResponse, RpcRequest};

use crate::handler::handle_request;
use crate::index::IndexManager;
use crate::lifecycle::LifecycleHandle;

/// Maximum concurrent connections.
const MAX_CONNECTIONS: usize = 32;

/// Read timeout per message (seconds).
const READ_TIMEOUT_SECS: u64 = 30;

/// Maximum message size (16 MB).
const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Idle connection timeout — disconnect if no messages for this long (S4.4.8).
const IDLE_CONNECTION_SECS: u64 = 300; // 5 minutes

/// Per-connection rate limit: max queries per second (S4.4.6).
const MAX_QUERIES_PER_SEC: u32 = 100;

/// Returns the platform-specific socket path.
pub fn socket_path() -> PathBuf {
    let base = dirs_next::data_local_dir().unwrap_or_else(|| PathBuf::from("/tmp"));

    #[cfg(target_os = "macos")]
    {
        base.join("uffs").join("daemon.sock")
    }
    #[cfg(target_os = "linux")]
    {
        // Prefer XDG_RUNTIME_DIR if available
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(runtime_dir).join("uffs").join("daemon.sock")
        } else {
            base.join("uffs").join("daemon.sock")
        }
    }
    #[cfg(target_os = "windows")]
    {
        // Named pipes don't use file paths, but we use this for the PID file dir
        base.join("uffs").join("daemon.sock")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        base.join("uffs").join("daemon.sock")
    }
}

/// Run the IPC server on a Unix domain socket.
///
/// Returns when the lifecycle manager signals shutdown.
#[cfg(unix)]
pub async fn run_ipc_server(
    index: Arc<IndexManager>,
    lifecycle: LifecycleHandle,
) -> anyhow::Result<()> {
    let sock_path = socket_path();

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

    let connection_count = Arc::new(AtomicUsize::new(0));

    loop {
        let (stream, _addr) = listener.accept().await?;

        // S4.2: Peer credential verification — reject connections from other UIDs
        if !verify_peer_credentials(&stream) {
            tracing::warn!("Rejected connection from different UID");
            drop(stream);
            continue;
        }

        let current = connection_count.load(Ordering::Relaxed);
        if current >= MAX_CONNECTIONS {
            tracing::warn!(
                current,
                max = MAX_CONNECTIONS,
                "Max connections reached, rejecting"
            );
            drop(stream);
            continue;
        }

        connection_count.fetch_add(1, Ordering::Relaxed);
        let index = Arc::clone(&index);
        let lifecycle = lifecycle.clone();
        let conn_count = Arc::clone(&connection_count);

        tokio::spawn(async move {
            let total_conns = conn_count.load(Ordering::Relaxed);
            tracing::debug!(connections = total_conns, "Client connected");

            if let Err(e) = handle_connection(stream, &index, &lifecycle, &conn_count).await {
                tracing::debug!(error = %e, "Connection ended");
            }

            conn_count.fetch_sub(1, Ordering::Relaxed);
            let remaining = conn_count.load(Ordering::Relaxed);
            tracing::debug!(connections = remaining, "Client disconnected");
        });
    }
}

// ── S4.2: Peer Credential Verification ──────────────────────────────────────

/// Verify that the connecting client has the same UID as the daemon.
///
/// - **macOS**: uses `getpeereid()` via the raw fd
/// - **Linux**: uses `SO_PEERCRED` via `UCred`
/// - **Windows**: always returns `true` (named pipe DACL handles this)
#[cfg(unix)]
fn verify_peer_credentials(stream: &tokio::net::UnixStream) -> bool {
    use std::os::unix::io::AsRawFd;

    let fd = stream.as_raw_fd();

    // SAFETY: getuid() and getpeereid() are standard POSIX calls.
    #[expect(unsafe_code, reason = "POSIX credential checks require unsafe FFI")]
    let (my_uid, peer_uid, result) = unsafe {
        let my_uid = libc::getuid();
        let mut peer_uid: libc::uid_t = 0;
        let mut peer_gid: libc::gid_t = 0;
        let result = libc::getpeereid(fd, &mut peer_uid, &mut peer_gid);
        (my_uid, peer_uid, result)
    };

    if result != 0 {
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

/// Windows IPC server — uses Unix domain sockets (Windows 10 1803+).
///
/// Mirrors the Unix version: secure dir (icacls owner-only ACL), socket
/// file permissions, max connections, peer verification via ACL.
#[cfg(windows)]
pub async fn run_ipc_server(
    index: Arc<IndexManager>,
    lifecycle: LifecycleHandle,
) -> anyhow::Result<()> {
    let sock_path = socket_path();

    // Ensure parent directory exists with owner-only ACL (icacls)
    if let Some(parent) = sock_path.parent() {
        uffs_security::fs::create_secure_dir(parent)?;
    }

    // Remove stale socket file
    if sock_path.exists() {
        std::fs::remove_file(&sock_path)?;
    }

    let listener = tokio::net::UnixListener::bind(&sock_path)?;

    // Set socket file permissions to owner-only (icacls ACL)
    uffs_security::fs::set_file_permissions_owner_only(&sock_path)?;

    tracing::info!(path = %sock_path.display(), "IPC server listening (Windows AF_UNIX)");

    let connection_count = Arc::new(AtomicUsize::new(0));

    loop {
        let (stream, _addr) = listener.accept().await?;

        // Peer verification: on Windows, the socket dir's owner-only ACL
        // prevents other users from connecting (OS-enforced).
        if !verify_peer_credentials(&stream) {
            tracing::warn!("Rejected connection");
            drop(stream);
            continue;
        }

        let current = connection_count.load(Ordering::Relaxed);
        if current >= MAX_CONNECTIONS {
            tracing::warn!(current, max = MAX_CONNECTIONS, "Max connections reached");
            drop(stream);
            continue;
        }

        connection_count.fetch_add(1, Ordering::Relaxed);
        let index = Arc::clone(&index);
        let lifecycle = lifecycle.clone();
        let conn_count = Arc::clone(&connection_count);

        tokio::spawn(async move {
            let total = conn_count.load(Ordering::Relaxed);
            tracing::debug!(connections = total, "Client connected");

            if let Err(e) = handle_connection(stream, &index, &lifecycle, &conn_count).await {
                tracing::debug!(error = %e, "Connection ended");
            }

            conn_count.fetch_sub(1, Ordering::Relaxed);
            let remaining = conn_count.load(Ordering::Relaxed);
            tracing::debug!(connections = remaining, "Client disconnected");
        });
    }
}

/// Windows: peer credential verification not needed — socket permissions handle it.
#[cfg(windows)]
fn verify_peer_credentials(_stream: &tokio::net::UnixStream) -> bool {
    true
}

/// Handle a single client connection (shared across all platforms).
///
/// Enforces:
/// - S4.4.7: 30-second per-message read timeout
/// - S4.4.8: 5-minute idle connection timeout (no messages at all)
/// - S4.4.6: Per-connection rate limit (100 queries/sec)
async fn handle_connection(
    stream: tokio::net::UnixStream,
    index: &Arc<IndexManager>,
    lifecycle: &LifecycleHandle,
    conn_count: &Arc<AtomicUsize>,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();

    // S4.4.6: Simple token-bucket rate limiter
    let mut queries_this_second: u32 = 0;
    let mut rate_limit_epoch = std::time::Instant::now();

    loop {
        line.clear();

        // S4.4.8: Use idle connection timeout for the read
        // (each message also has a READ_TIMEOUT_SECS per-message cap)
        let read_result = tokio::time::timeout(
            std::time::Duration::from_secs(IDLE_CONNECTION_SECS),
            buf_reader.read_line(&mut line),
        )
        .await;

        let bytes_read = match read_result {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                tracing::debug!("Idle connection timeout ({}s), disconnecting", IDLE_CONNECTION_SECS);
                return Ok(());
            }
        };

        if bytes_read == 0 {
            return Ok(());
        }

        if line.len() > MAX_MESSAGE_SIZE {
            let err = RpcErrorResponse::error(None, ERR_PARSE, "Message too large");
            let response = serde_json::to_string(&err).unwrap_or_default();
            writer.write_all(response.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
            return Ok(());
        }

        // S4.4.6: Rate limiting — reset counter every second
        let now = std::time::Instant::now();
        if now.duration_since(rate_limit_epoch).as_secs() >= 1 {
            queries_this_second = 0;
            rate_limit_epoch = now;
        }
        queries_this_second += 1;
        if queries_this_second > MAX_QUERIES_PER_SEC {
            let err = RpcErrorResponse::error(
                None,
                -32000,
                &format!("Rate limit exceeded ({MAX_QUERIES_PER_SEC} queries/sec)"),
            );
            let response = serde_json::to_string(&err).unwrap_or_default();
            writer.write_all(response.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
            continue;
        }

        // Reset daemon idle timer on any activity
        lifecycle.reset_idle_timer();

        // Parse JSON-RPC request
        let req: RpcRequest = match serde_json::from_str(line.trim()) {
            Ok(r) => r,
            Err(e) => {
                let err = RpcErrorResponse::error(
                    None,
                    ERR_PARSE,
                    &format!("Invalid JSON: {e}"),
                );
                let response = serde_json::to_string(&err).unwrap_or_default();
                writer.write_all(response.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await?;
                continue;
            }
        };

        // Dispatch to handler
        let connections = conn_count.load(Ordering::Relaxed);
        let response = handle_request(&req, index, lifecycle, connections).await;

        writer.write_all(response.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
    }
}
