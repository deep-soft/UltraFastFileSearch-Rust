// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Unit tests for [`crate::connect_sync::UffsClientSync`] that
//! exercise the wire-protocol path without opening a real socket.
//!
//! The tests inject in-memory reader/writer halves via
//! [`UffsClientSync::from_parts_for_test`], pre-populate the reader
//! with canned JSON-RPC responses, and assert that the client
//! interprets them correctly.
//!
//! # Scope
//!
//! * `deep_health_check` happy path (probe succeeds → `Ok(())`).
//! * `deep_health_check` probe-error path (daemon returns a JSON-RPC error →
//!   `ConnectionFailed` with remediation guidance).
//! * `deep_health_check` transport-error path (reader closed before any bytes
//!   arrive → `ConnectionFailed`).

#![cfg(test)]

extern crate alloc;

use alloc::sync::Arc;
use std::io::{BufReader, Cursor, Read, Write};
use std::sync::Mutex;

use crate::connect_sync::UffsClientSync;
use crate::error::ClientError;

/// In-memory writer that records everything written to it.
///
/// We wrap a `Vec<u8>` in `Arc<Mutex<…>>` so the test can inspect
/// the captured bytes *after* handing ownership to
/// [`UffsClientSync::from_parts_for_test`].  Without the `Arc`,
/// the moved writer would be inaccessible.
#[derive(Clone)]
struct CapturingWriter(Arc<Mutex<Vec<u8>>>);

impl CapturingWriter {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    fn take(&self) -> Vec<u8> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Write for CapturingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // Recover from a poisoned mutex rather than propagating a
        // fabricated `io::Error` — the only way the lock can be
        // poisoned in this test is another test-helper panic, which
        // is already surfaced by cargo.  Inline the lock so clippy's
        // `significant_drop_tightening` lint sees the guard released
        // immediately after the single use.
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Build a `UffsClientSync` wired to fixed in-memory halves.
///
/// The `response_body` bytes are pre-loaded into the reader so the
/// first `send_request` call will immediately see a complete
/// JSON-RPC response.  The returned [`CapturingWriter`] snapshots
/// whatever the client writes — tests use it to assert the request
/// shape when that matters.
fn client_with_canned_response(response_body: &[u8]) -> (UffsClientSync, CapturingWriter) {
    let reader: Box<dyn Read + Send> = Box::new(Cursor::new(response_body.to_vec()));
    let writer = CapturingWriter::new();
    let writer_box: Box<dyn Write + Send> = Box::new(writer.clone());
    let client = UffsClientSync::from_parts_for_test(BufReader::new(reader), writer_box);
    (client, writer)
}

/// Happy path: the daemon returns a valid `drives` result, so
/// `deep_health_check` returns `Ok(())` and the caller's retry
/// loop proceeds normally.
///
/// Also verifies that the client actually sent a JSON-RPC request
/// with `method:"drives"` — defence against a regression where the
/// probe silently changes to a different method and gives a false
/// sense of coverage.
#[test]
fn deep_health_check_happy_path() {
    let canned = br#"{"jsonrpc":"2.0","id":1,"result":{"drives":[]}}
"#;
    let (mut client, writer) = client_with_canned_response(canned);

    client.deep_health_check().expect("happy path must succeed");

    let sent = writer.take();
    let sent_str = core::str::from_utf8(&sent).expect("request must be valid UTF-8");
    assert!(
        sent_str.contains(r#""method":"drives""#),
        "the probe must use the `drives` method; saw: {sent_str:?}",
    );
    assert!(
        sent_str.ends_with('\n'),
        "JSON-RPC framing requires a trailing newline; saw: {sent_str:?}",
    );
}

/// Probe-error path: the daemon is reachable (a valid JSON-RPC
/// envelope comes back) but the response carries an `error` object.
/// `deep_health_check` must wrap that into a
/// [`ClientError::ConnectionFailed`] whose message includes the
/// remediation guidance (`uffs daemon kill`, skip-env hint) so the
/// user has an actionable next step.
#[test]
fn deep_health_check_maps_daemon_error_to_connection_failed() {
    let canned = br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"index unavailable"}}
"#;
    let (mut client, _writer) = client_with_canned_response(canned);

    let err = client
        .deep_health_check()
        .expect_err("daemon error must propagate as Err");
    let ClientError::ConnectionFailed(msg) = &err else {
        panic!("expected ConnectionFailed, got {err:?}");
    };
    assert!(
        msg.contains("Deep health check failed"),
        "error must identify itself as a health-check failure: {msg}",
    );
    assert!(
        msg.contains("uffs daemon kill"),
        "error must include the remediation command: {msg}",
    );
    assert!(
        msg.contains("UFFS_CLIENT_SKIP_HEALTH_CHECK"),
        "error must mention the opt-out env var: {msg}",
    );
    assert!(
        msg.contains("index unavailable"),
        "error must preserve the underlying daemon message: {msg}",
    );
}

/// Transport-error path: the reader is empty (EOF on first read).
/// The client observes `ConnectionClosed` from its read loop and
/// `deep_health_check` remaps it into a `ConnectionFailed` — same
/// remediation surface, different underlying cause string.
#[test]
fn deep_health_check_maps_connection_closed_to_connection_failed() {
    let (mut client, _writer) = client_with_canned_response(b"");

    let err = client
        .deep_health_check()
        .expect_err("closed connection must produce Err");
    let ClientError::ConnectionFailed(msg) = &err else {
        panic!("expected ConnectionFailed, got {err:?}");
    };
    assert!(
        msg.contains("Deep health check failed"),
        "error must identify itself as a health-check failure: {msg}",
    );
    // The underlying error is ConnectionClosed — its Display string
    // (from `#[error(...)]`) is substring-matched here so a future
    // error-message tweak is self-documenting.
    assert!(
        msg.to_lowercase().contains("closed") || msg.contains("ConnectionClosed"),
        "wrapped error must reference the closed connection: {msg}",
    );
}
