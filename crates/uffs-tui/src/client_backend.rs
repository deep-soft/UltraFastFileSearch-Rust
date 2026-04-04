//! Daemon client backend for the TUI.
//!
//! Wraps [`UffsClient`] behind a synchronous API so the TUI event loop
//! (which is single-threaded / no tokio) can call `search()` without
//! changing its overall architecture.
//!
//! A dedicated single-threaded tokio [`Runtime`] is created once at
//! startup and kept alive for the TUI's lifetime.

use uffs_client::connect::UffsClient;
use uffs_client::protocol::{SearchParams, SearchResponse, StatusResponse};
use uffs_core::search::backend::DisplayRow;

/// Synchronous wrapper around [`UffsClient`] for TUI use.
///
/// Owns a private tokio `Runtime` that powers the async IPC calls.
pub struct DaemonBackend {
    /// Tokio runtime dedicated to IPC calls.
    rt: tokio::runtime::Runtime,
    /// Connected client (lazily established on first call).
    client: Option<UffsClient>,
    /// Arguments forwarded to `uffs-daemon` when auto-starting.
    ///
    /// On Windows this is empty (daemon auto-discovers live drives).
    /// On Mac/Linux this carries `--mft-file` / `--no-cache` flags.
    spawn_args: Vec<String>,
}

impl DaemonBackend {
    /// Create a new daemon backend.
    ///
    /// `spawn_args` are forwarded to `uffs-daemon` only when the daemon
    /// must be auto-started.
    ///
    /// # Panics
    ///
    /// Panics if the tokio runtime cannot be created (system resource
    /// exhaustion).
    #[must_use]
    #[expect(clippy::single_call_fn, reason = "constructor called once from main")]
    pub fn new(spawn_args: Vec<String>) -> Self {
        #[expect(
            clippy::expect_used,
            reason = "tokio runtime creation at TUI startup; failure is unrecoverable"
        )]
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to create tokio runtime for daemon IPC");

        Self {
            rt,
            client: None,
            spawn_args,
        }
    }

    /// Establish the daemon connection (auto-starts if needed).
    ///
    /// Blocks until the daemon is connected **and** has finished loading
    /// its indices (up to 2 minutes).  Safe to call multiple times —
    /// reconnects if the previous connection was lost.
    pub fn connect(&mut self) -> Result<(), String> {
        let result = self
            .rt
            .block_on(UffsClient::connect_with_args(&self.spawn_args));
        match result {
            Ok(mut client) => {
                // Wait for daemon to finish loading before returning.
                let ready = self
                    .rt
                    .block_on(client.await_ready(core::time::Duration::from_secs(120)));
                if let Err(ready_err) = ready {
                    return Err(format!("Daemon not ready: {ready_err}"));
                }
                self.client = Some(client);
                Ok(())
            }
            Err(err) => Err(err.to_string()),
        }
    }

    /// Query the daemon's current status.
    #[expect(dead_code, reason = "will be wired into TUI status bar in a follow-up")]
    pub fn status(&mut self) -> Result<StatusResponse, String> {
        self.ensure_connected()?;
        let client = self
            .client
            .as_mut()
            .ok_or_else(|| "not connected".to_owned())?;
        self.rt
            .block_on(client.status())
            .map_err(|err| err.to_string())
    }

    /// Send a search to the daemon and return `DisplayRow`s.
    pub fn search(&mut self, params: &SearchParams) -> Result<DaemonSearchResult, String> {
        self.ensure_connected()?;
        let client = self
            .client
            .as_mut()
            .ok_or_else(|| "not connected".to_owned())?;
        let response: SearchResponse = self
            .rt
            .block_on(client.search(params))
            .map_err(|err| err.to_string())?;

        let rows: Vec<DisplayRow> = response
            .rows
            .into_iter()
            .map(|row| {
                DisplayRow::new(
                    0,
                    row.drive,
                    row.path,
                    row.size,
                    row.is_directory,
                    row.modified,
                    row.created,
                    row.accessed,
                    row.flags,
                    row.allocated,
                    row.descendants,
                    row.treesize,
                    row.tree_allocated,
                )
            })
            .collect();

        Ok(DaemonSearchResult {
            rows,
            duration_ms: response.duration_ms,
            records_scanned: response.records_scanned,
            truncated: response.truncated,
        })
    }

    /// Set session type to TUI (gives longer idle timeout).
    pub fn set_session_tui(&mut self) {
        if self.ensure_connected().is_ok() {
            if let Some(client) = self.client.as_mut() {
                drop(self.rt.block_on(client.set_session_type("tui")));
            }
        }
    }

    /// Ensure we have a live connection, reconnecting if needed.
    fn ensure_connected(&mut self) -> Result<(), String> {
        if self.client.is_none() {
            self.connect()?;
        }
        Ok(())
    }
}

/// Result from a daemon search — mirrors the information in
/// `SearchResponse` but carries `DisplayRow` instead of `SearchRow`.
pub struct DaemonSearchResult {
    /// Matched rows.
    pub rows: Vec<DisplayRow>,
    /// Search duration on the daemon side (milliseconds).
    pub duration_ms: u64,
    /// Total records scanned.
    pub records_scanned: usize,
    /// Whether the result set was truncated by the limit.
    pub truncated: bool,
}
