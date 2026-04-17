// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

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

    /// Daemon is not running, the current process is not elevated, and
    /// the caller did not opt in to a UAC prompt.
    ///
    /// The CLI is expected to catch this variant and render a
    /// multi-option help message ("run as admin", "pass --elevate",
    /// "install broker service").  The embedded path is the daemon
    /// executable we would have spawned, included so downstream
    /// formatters can reproduce it verbatim.
    #[error(
        "daemon needs admin privileges to read NTFS Master File Tables \
         (would have spawned: {daemon_path})"
    )]
    DaemonNeedsElevation {
        /// Absolute path to the daemon executable that would have been
        /// spawned if elevation had been permitted.
        daemon_path: String,
    },

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

#[cfg(test)]
mod tests {
    use super::ClientError;

    /// `DaemonNeedsElevation` Display output includes the daemon path
    /// verbatim so the CLI formatter and downstream tooling can rely
    /// on it.  Locks the public-facing error message in place.
    #[test]
    fn daemon_needs_elevation_display_includes_path() {
        let err = ClientError::DaemonNeedsElevation {
            daemon_path: r"C:\Program Files\uffs\uffsd.exe".to_owned(),
        };
        let rendered = err.to_string();
        assert!(
            rendered.contains(r"C:\Program Files\uffs\uffsd.exe"),
            "expected daemon path in Display output, got: {rendered}"
        );
        assert!(
            rendered.contains("admin"),
            "expected 'admin' in Display output for discoverability, got: {rendered}"
        );
    }

    /// Sanity-check that adjacent `ClientError` variants still format
    /// distinctly — guards against accidentally collapsing them under
    /// one `#[error]` attribute.
    #[test]
    fn client_error_variants_format_distinctly() {
        let connection = ClientError::ConnectionFailed("pipe closed".to_owned()).to_string();
        let needs_elev = ClientError::DaemonNeedsElevation {
            daemon_path: "uffsd".to_owned(),
        }
        .to_string();
        let start_failed = ClientError::DaemonStartFailed("boom".to_owned()).to_string();
        assert_ne!(connection, needs_elev);
        assert_ne!(connection, start_failed);
        assert_ne!(needs_elev, start_failed);
    }
}
