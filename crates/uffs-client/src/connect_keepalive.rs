// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Background keepalive task + guard for long-lived async clients.
//!
//! This is the **canonical home** of `KeepaliveGuard`.  Import it
//! from `crate::connect_keepalive` (or
//! `uffs_client::connect_keepalive` from outside the crate) — there
//! is intentionally no `pub use` cascade through `connect`.
//! The `start_keepalive` method is re-attached to
//! [`crate::connect::UffsClient`] via a split `impl`.
//!
//! # Why a separate connection
//!
//! The keepalive task must call the daemon from a different tokio
//! task than the owner of the client.  We can't move `&mut self` into
//! a `tokio::spawn`, so the task opens a short-lived blocking
//! connection via [`crate::daemon_ctl::keepalive_send_blocking`]
//! every `interval` and sends a single `keepalive` JSON-RPC message.
//! This is cheap (one socket connect + one line write + close) and
//! keeps the daemon's idle timer reset without touching the owner's
//! client state.

use crate::connect::UffsClient;
use crate::daemon_ctl::{keepalive_send_blocking, socket_path};

/// Guard that stops the background keepalive task when dropped (D3.4.2).
///
/// Constructed by [`UffsClient::start_keepalive`].  The guard holds
/// the `tokio::sync::oneshot::Sender` half of a cancellation
/// channel; dropping it fires the channel and the spawned keepalive
/// task exits on its next loop iteration.
pub struct KeepaliveGuard {
    /// Dropping this sends a cancel signal to the keepalive task.
    _cancel: tokio::sync::oneshot::Sender<()>,
}

impl UffsClient {
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
                        // Unix: std::os::unix::net::UnixStream.
                        // Windows: std::fs::OpenOptions on the named pipe.
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
