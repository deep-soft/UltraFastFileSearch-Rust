// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Transport-error classification for the synchronous client.
//!
//! Split from [`crate::connect_sync`] for the 800-LOC file-size policy.
//! `send_request` maps every failed blocking read / write through
//! `transport_error` (Windows only) or `classify_io_error` (Unix) so the
//! client's own deadline surfaces as [`crate::error::ClientError::Timeout`]
//! and never as a raw transport error.

use std::io;

use crate::error::ClientError;

/// Map a failed blocking read/write to the client error it really is.
///
/// On Windows the per-RPC deadline is enforced by the watchdog's
/// `CancelSynchronousIo`, which makes the blocked call fail with
/// `ERROR_OPERATION_ABORTED` (os error 995).  That code is *also* what
/// a genuine daemon-side transient looks like once flattened across the
/// JSON-RPC boundary, so the raw code alone cannot be trusted: the guard's
/// [`fired`](crate::windows_deadline::WindowsDeadlineGuard::fired) verdict
/// is the discriminator.  A cancelled RPC is reported as
/// [`ClientError::Timeout`]; before this the CLI mistook its own deadline
/// for the "index warming" transient and re-sent the search up to five
/// times, orphaning a still-running scan on the daemon each time.
///
/// A kernel-reported `TimedOut` / `WouldBlock` (the Unix `SO_RCVTIMEO`
/// path, and any Windows transport that enforces its own timeout) is a
/// `Timeout` on every platform.
#[cfg(windows)]
pub(crate) fn transport_error(
    guard: Option<&crate::windows_deadline::WindowsDeadlineGuard>,
    err: &io::Error,
) -> ClientError {
    if guard.is_some_and(crate::windows_deadline::WindowsDeadlineGuard::fired) {
        return ClientError::Timeout;
    }
    classify_io_error(err)
}

/// Classify a failed blocking read/write by its kind alone — the whole
/// story on Unix, where the kernel enforces the deadline via
/// `SO_RCVTIMEO` / `SO_SNDTIMEO`, and the tail of `transport_error` on
/// Windows: a kernel-reported deadline expiry is a
/// [`ClientError::Timeout`]; everything else is an [`ClientError::Io`].
pub(crate) fn classify_io_error(err: &io::Error) -> ClientError {
    if matches!(
        err.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        ClientError::Timeout
    } else {
        ClientError::Io(err.to_string())
    }
}

// Auto-start daemon helpers (`auto_start_daemon`, `is_process_alive`,
// `is_daemon_process`) live in the sibling [`crate::connect_sync_autostart`]
// module to keep this file under the 800-LOC policy ceiling.

#[cfg(test)]
mod tests {
    use std::io;

    use super::classify_io_error;
    use crate::error::ClientError;

    /// A kernel-reported deadline expiry is a `Timeout` on every
    /// platform; any other transport failure stays an `Io` carrying the
    /// original message.
    #[test]
    fn kernel_timeouts_classify_as_timeout_everything_else_as_io() {
        assert!(matches!(
            classify_io_error(&io::Error::from(io::ErrorKind::TimedOut)),
            ClientError::Timeout
        ));
        assert!(matches!(
            classify_io_error(&io::Error::from(io::ErrorKind::WouldBlock)),
            ClientError::Timeout
        ));
        let other = classify_io_error(&io::Error::new(io::ErrorKind::BrokenPipe, "pipe gone"));
        let ClientError::Io(message) = other else {
            panic!("expected Io, got {other:?}");
        };
        assert!(
            message.contains("pipe gone"),
            "message must be preserved; got {message}"
        );
    }
}
