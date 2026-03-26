//! Client error types.

/// Errors that can occur when communicating with the daemon.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Failed to connect to the daemon socket.
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    /// Failed to start the daemon process.
    #[error("daemon start failed: {0}")]
    DaemonStartFailed(String),

    /// I/O error during communication.
    #[error("I/O error: {0}")]
    Io(String),

    /// Protocol error (bad JSON, unexpected response format).
    #[error("protocol error: {0}")]
    Protocol(String),

    /// Response timeout.
    #[error("request timed out")]
    Timeout,

    /// Connection was closed by the daemon.
    #[error("connection closed")]
    ConnectionClosed,

    /// Daemon returned an RPC error.
    #[error("daemon error {code}: {message}")]
    DaemonError {
        /// JSON-RPC error code.
        code: i32,
        /// Error message.
        message: String,
    },
}
