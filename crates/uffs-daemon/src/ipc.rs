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

/// Handle a single client connection (Unix).
#[cfg(unix)]
async fn handle_connection(
    stream: tokio::net::UnixStream,
    index: &Arc<IndexManager>,
    lifecycle: &LifecycleHandle,
    conn_count: &Arc<AtomicUsize>,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        line.clear();

        // Read with timeout
        let read_result = tokio::time::timeout(
            std::time::Duration::from_secs(READ_TIMEOUT_SECS),
            buf_reader.read_line(&mut line),
        )
        .await;

        let bytes_read = match read_result {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => {
                // Read timeout — disconnect
                tracing::debug!("Read timeout, disconnecting client");
                return Ok(());
            }
        };

        if bytes_read == 0 {
            // EOF — client disconnected
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

        // Reset idle timer on any activity
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

        // Write response + newline
        writer.write_all(response.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
    }
}
