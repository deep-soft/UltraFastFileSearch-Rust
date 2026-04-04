//! Daemon lifecycle: PID file, idle timeout, auto-retire, shutdown.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use core::time::Duration;
use std::path::PathBuf;

use tokio::sync::watch;

use crate::events::{DaemonEvent, EventSender};

/// Maximum time (seconds) a load phase may run without progress before
/// the daemon force-retires.  Prevents an unkillable zombie when a raw
/// NTFS volume read hangs in kernel-mode I/O.
const LOAD_STALL_TIMEOUT_SECS: u64 = 300; // 5 minutes

/// Handle given to request handlers to control the lifecycle.
#[derive(Clone)]
pub struct LifecycleHandle {
    /// Send `true` to trigger shutdown.
    shutdown_tx: watch::Sender<bool>,
    /// Reset idle timer flag (checked by the timer task).
    idle_reset: Arc<AtomicBool>,
    /// Shutdown nonce — must be provided in the `shutdown` RPC call (S4.4.9).
    shutdown_nonce: Arc<std::sync::Mutex<Option<String>>>,
    /// Active connection count (D2.6.7: don't retire if > 0).
    active_connections: Arc<core::sync::atomic::AtomicUsize>,
    /// Longest session type seen (D2.6.6: TUI/GUI/MCP get 15 min, CLI gets 5
    /// min).
    max_session_tier: Arc<core::sync::atomic::AtomicU8>,
    /// Event broadcaster — connection and lifecycle events.
    events: EventSender,
    /// Load heartbeat — epoch seconds of the last drive-load progress.
    /// Updated by `IndexManager` when each drive finishes loading.
    /// Checked by the idle timer to detect stuck loads.
    load_heartbeat: Arc<AtomicU64>,
}

impl LifecycleHandle {
    /// Signal the daemon to shut down gracefully.
    pub fn request_shutdown(&self) {
        self.events.emit(DaemonEvent::ShuttingDown {
            reason: "shutdown requested via RPC".to_owned(),
        });
        let _ignore = self.shutdown_tx.send(true);
    }

    /// Reset the idle timer (called on every query/keepalive).
    pub fn reset_idle_timer(&self) {
        self.idle_reset.store(true, Ordering::Relaxed);
    }

    /// Increment active connection count and emit event.
    pub fn connection_opened(&self) {
        let active = self.active_connections.fetch_add(1, Ordering::Relaxed) + 1;
        self.events.emit(DaemonEvent::ConnectionChanged { active });
    }

    /// Decrement active connection count and emit event.
    pub fn connection_closed(&self) {
        let active = self
            .active_connections
            .fetch_sub(1, Ordering::Relaxed)
            .saturating_sub(1);
        self.events.emit(DaemonEvent::ConnectionChanged { active });
    }

    /// Get active connection count.
    pub fn active_connections(&self) -> usize {
        self.active_connections.load(Ordering::Relaxed)
    }

    /// Update session type (D2.6.6). Higher tier = longer timeout.
    /// 0 = CLI (5 min), 1 = TUI/GUI/MCP (15 min).
    pub fn set_session_type(&self, session_type: &str) {
        let tier = match session_type {
            "tui" | "gui" | "mcp" => 1,
            _ => 0, // cli or unknown
        };
        // Only upgrade, never downgrade
        self.max_session_tier.fetch_max(tier, Ordering::Relaxed);
    }

    /// Verify a shutdown nonce matches the one in the PID file (S4.4.9).
    ///
    /// If no nonce is set (shouldn't happen), allows shutdown anyway.
    pub fn verify_shutdown_nonce(&self, provided: &str) -> bool {
        let guard = self
            .shutdown_nonce
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.as_deref() == Some(provided) || guard.is_none()
    }

    /// Record load progress — called by `IndexManager` each time a drive
    /// finishes loading.  Updates the heartbeat timestamp so the idle
    /// timer knows the load phase is still making progress.
    #[cfg(windows)]
    pub fn record_load_progress(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |dur| dur.as_secs());
        let prev = self.load_heartbeat.swap(now, Ordering::Relaxed);
        let delta = now.saturating_sub(prev);
        tracing::debug!(heartbeat_delta_secs = delta, "Load heartbeat updated");
    }
}

/// Lifecycle manager: PID file, idle timer, shutdown coordination.
pub struct LifecycleManager {
    /// Shutdown receiver — await this to know when to exit.
    shutdown_rx: watch::Receiver<bool>,
    /// The handle that handlers use to control lifecycle.
    handle: LifecycleHandle,
    /// PID file path.
    pid_path: PathBuf,
    /// Idle timeout duration. `None` = no auto-retire (--no-retire).
    idle_timeout: Option<Duration>,
    /// Shutdown nonce (written to PID file, required for RPC shutdown).
    shutdown_nonce: Option<String>,
}

impl LifecycleManager {
    /// Create a new lifecycle manager.
    ///
    /// `idle_timeout`: `None` for `--no-retire`, `Some(duration)` otherwise.
    #[expect(clippy::single_call_fn, reason = "constructor — structural separation")]
    pub fn new(
        data_dir: &std::path::Path,
        idle_timeout: Option<Duration>,
        events: EventSender,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let idle_reset = Arc::new(AtomicBool::new(false));
        let shutdown_nonce_shared = Arc::new(std::sync::Mutex::new(None));
        let active_connections = Arc::new(core::sync::atomic::AtomicUsize::new(0));
        let max_session_tier = Arc::new(core::sync::atomic::AtomicU8::new(0));
        // Seed the load heartbeat with "now" so the stall detector
        // doesn't fire before the first drive even starts loading.
        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |dur| dur.as_secs());
        let load_heartbeat = Arc::new(AtomicU64::new(now_epoch));

        let pid_path = data_dir.join("daemon.pid");

        Self {
            shutdown_rx,
            handle: LifecycleHandle {
                shutdown_tx,
                idle_reset,
                shutdown_nonce: shutdown_nonce_shared,
                active_connections,
                max_session_tier,
                events,
                load_heartbeat,
            },
            pid_path,
            idle_timeout,
            shutdown_nonce: None,
        }
    }

    /// Get a handle for request handlers.
    pub fn handle(&self) -> LifecycleHandle {
        self.handle.clone()
    }

    /// Write the PID file.
    ///
    /// Format: `{pid}\n{start_timestamp}\n{exe_path_hash}\n{shutdown_nonce}\n`
    /// - `exe_path_hash`: FNV-1a hash of the daemon executable path (for
    ///   identity verification)
    /// - `shutdown_nonce`: random token required for the `shutdown` RPC method
    ///   (S4.4.9)
    pub fn write_pid_file(&mut self) -> std::io::Result<()> {
        if let Some(parent) = self.pid_path.parent() {
            uffs_security::fs::create_secure_dir(parent)?;
        }

        let exe_hash = Self::exe_path_hash();
        let nonce = Self::generate_nonce();
        self.shutdown_nonce = Some(nonce.clone());

        // Sync nonce to the shared handle so handlers can verify it
        if let Ok(mut guard) = self.handle.shutdown_nonce.lock() {
            *guard = Some(nonce.clone());
        }

        let content = format!(
            "{}\n{}\n{}\n{}\n",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |dur| dur.as_secs()),
            exe_hash,
            nonce,
        );
        std::fs::write(&self.pid_path, content)?;
        uffs_security::fs::set_file_permissions_owner_only(&self.pid_path)?;
        tracing::info!(path = %self.pid_path.display(), "PID file written");
        Ok(())
    }

    /// Remove the PID file (called on shutdown).
    pub fn remove_pid_file(&self) {
        if self.pid_path.exists() {
            let _ignore = std::fs::remove_file(&self.pid_path);
            tracing::info!(path = %self.pid_path.display(), "PID file removed");
        }
    }

    /// Remove the IPC socket file (called on shutdown).
    ///
    /// Without this, a stale socket file remains after graceful stop and
    /// subsequent `daemon status` connects to it, gets EOF, and reports
    /// "connection closed" instead of "not running".
    #[expect(
        clippy::unused_self,
        reason = "method signature matches remove_pid_file(&self); both called in Drop"
    )]
    pub fn remove_socket_file(&self) {
        let sock_path = crate::ipc::IpcServer::socket_path();
        if sock_path.exists() {
            let _ignore = std::fs::remove_file(&sock_path);
            tracing::info!(path = %sock_path.display(), "Socket file removed");
        }
    }

    /// Check for stale PID file on startup. Returns `true` if safe to proceed.
    ///
    /// Uses [`parse_pid_file`] for structured parsing and validates the exe
    /// hash via [`expected_daemon_exe_hash`] to detect stale files from
    /// different binaries.
    pub fn check_stale_pid(&self) -> bool {
        if !self.pid_path.exists() {
            return true;
        }

        let Some((pid, _ts, exe_hash, _nonce)) = Self::parse_pid_file(&self.pid_path) else {
            // Unparseable — remove and proceed
            let _ignore = std::fs::remove_file(&self.pid_path);
            return true;
        };

        if pid == 0 {
            let _ignore = std::fs::remove_file(&self.pid_path);
            return true;
        }

        // Validate exe hash — if it doesn't match, it's from a different binary
        let expected_hash = Self::expected_daemon_exe_hash();
        if expected_hash != 0 && exe_hash != expected_hash {
            tracing::info!(
                pid,
                "PID file exe hash mismatch (stale from different binary), cleaning up"
            );
            let _ignore = std::fs::remove_file(&self.pid_path);
            return true;
        }

        // Check if the process is still alive
        if Self::is_process_alive(pid) {
            tracing::warn!(
                pid,
                "Another daemon instance is running. Use 'shutdown' to stop it first."
            );
            return false;
        }

        // Stale PID file — process is dead, clean up
        tracing::info!(pid, "Cleaning up stale PID file from dead process");
        let _ignore = std::fs::remove_file(&self.pid_path);
        true
    }

    /// Run the idle timer. Returns when shutdown is requested, idle
    /// timeout fires, **or a stalled load is detected**.
    ///
    /// ## Dual-purpose heartbeat
    ///
    /// During the `Loading` phase, the CLI's `await_ready` polls status
    /// every 2 s, continuously resetting the idle timer.  Without a
    /// separate check, a stuck NTFS volume read would keep the daemon
    /// alive forever.  The **load heartbeat** (`record_load_progress`)
    /// is updated whenever a drive finishes loading.  If no progress is
    /// made for [`LOAD_STALL_TIMEOUT_SECS`], the daemon force-retires
    /// even though IPC activity is still present.
    ///
    /// D2.6.6: Uses differentiated timeouts based on session type:
    /// - CLI sessions (tier 0): use the configured timeout (default 5 min)
    /// - TUI/GUI/MCP sessions (tier 1): use 3× the configured timeout
    ///
    /// D2.6.7: Does NOT retire if active connections exist (Ready state).
    pub async fn run_idle_timer(&mut self) {
        let Some(base_timeout) = self.idle_timeout else {
            // --no-retire: just wait for shutdown signal
            let _shutdown = self.shutdown_rx.wait_for(|&done| done).await;
            return;
        };

        loop {
            // D2.6.6: Compute effective timeout based on highest session tier
            let tier = self.handle.max_session_tier.load(Ordering::Relaxed);
            let effective_timeout = if tier >= 1 {
                base_timeout.saturating_mul(3)
            } else {
                base_timeout
            };

            self.handle.idle_reset.store(false, Ordering::Relaxed);

            // Snapshot the handle's atomics before entering select!,
            // avoiding a borrow conflict with shutdown_rx.
            let handle = self.handle.clone();

            let timed_out = tokio::select! {
                () = tokio::time::sleep(effective_timeout) => true,
                _ = self.shutdown_rx.wait_for(|&done| done) => {
                    tracing::info!("Shutdown requested");
                    return;
                }
            };

            if !timed_out {
                continue;
            }

            // ── Load-stall check (dual-purpose heartbeat) ────────
            // Checked BEFORE the idle_reset gate so it fires even when
            // the CLI's `await_ready` polls keep resetting the timer.
            let stall_secs = LOAD_STALL_TIMEOUT_SECS;
            let last_hb = handle.load_heartbeat.load(Ordering::Relaxed);
            let now_epoch = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |dur| dur.as_secs());
            let hb_age = now_epoch.saturating_sub(last_hb);
            tracing::debug!(
                heartbeat_age_secs = hb_age,
                stall_threshold = stall_secs,
                idle_reset = handle.idle_reset.load(Ordering::Relaxed),
                "Idle timer tick — checking load heartbeat"
            );
            if last_hb > 0 && hb_age >= stall_secs {
                tracing::error!(
                    stall_secs,
                    heartbeat_age_secs = hb_age,
                    "Load stalled — no drive progress, force-retiring"
                );
                handle.events.emit(DaemonEvent::ShuttingDown {
                    reason: format!("load stalled (no progress for {stall_secs}s)"),
                });
                return;
            }

            // Normal idle path — check if IPC activity reset us.
            if handle.idle_reset.load(Ordering::Relaxed) {
                continue;
            }

            // D2.6.7: Don't retire if active connections exist
            let conns = handle.active_connections();
            if conns > 0 {
                tracing::debug!(
                    connections = conns,
                    "Idle timeout but active connections — deferring"
                );
                continue;
            }

            tracing::info!(
                timeout_secs = effective_timeout.as_secs(),
                session_tier = tier,
                "Idle timeout reached — auto-retiring"
            );
            handle.events.emit(DaemonEvent::ShuttingDown {
                reason: format!(
                    "idle timeout ({}s, tier {})",
                    effective_timeout.as_secs(),
                    tier,
                ),
            });
            return;
        }
    }

    /// Get the data directory path (parent of PID file).
    pub fn data_dir(&self) -> &std::path::Path {
        self.pid_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
    }

    // ── Private helpers ───────────────────────────────────────────────

    /// Check if a process with the given PID is alive (Unix).
    #[cfg(unix)]
    #[expect(
        clippy::single_call_fn,
        reason = "platform-specific helper — clarity over inlining"
    )]
    fn is_process_alive(pid: u32) -> bool {
        // pid_t is i32; PIDs above i32::MAX are invalid on POSIX.
        let Ok(target_pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        #[expect(
            unsafe_code,
            reason = "kill(pid, 0) is a standard POSIX liveness check"
        )]
        // SAFETY: `kill(pid, 0)` is a standard POSIX liveness check — it
        // sends no signal, just tests whether the process exists.
        let alive = unsafe { libc::kill(target_pid, 0_i32) == 0_i32 };
        alive
    }

    /// Check if a process with the given PID is alive (Windows).
    #[cfg(windows)]
    fn is_process_alive(pid: u32) -> bool {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

        // SAFETY: `OpenProcess` is a well-defined Win32 API.
        #[expect(unsafe_code, reason = "OpenProcess requires unsafe FFI")]
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
            if let Ok(proc_handle) = handle {
                let _close = CloseHandle(proc_handle);
                true
            } else {
                false
            }
        }
    }

    /// `FNV-1a` hash of the current executable path (S4.3.1).
    #[expect(
        clippy::single_call_fn,
        reason = "hashing helper — clarity over inlining"
    )]
    fn exe_path_hash() -> u64 {
        let exe_path = std::env::current_exe()
            .map(|exe| exe.to_string_lossy().to_string())
            .unwrap_or_default();
        Self::fnv1a_hash(exe_path.as_bytes())
    }

    /// `FNV-1a` 64-bit hash (no external dep needed).
    fn fnv1a_hash(data: &[u8]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for &byte in data {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        hash
    }

    /// Generate a random 16-char hex nonce for shutdown authentication
    /// (S4.4.9).
    #[expect(
        clippy::single_call_fn,
        reason = "nonce generation — clarity over inlining"
    )]
    fn generate_nonce() -> String {
        use rand::Rng;
        let mut nonce_bytes = [0_u8; 8];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let mut nonce_str = String::with_capacity(16_usize);
        for byte in &nonce_bytes {
            use core::fmt::Write;
            let _ignore = write!(nonce_str, "{byte:02x}");
        }
        nonce_str
    }

    /// Parse a PID file and extract its fields.
    ///
    /// Returns `(pid, timestamp, exe_hash, nonce)`.
    #[expect(
        clippy::single_call_fn,
        reason = "PID file parser — public API for clients"
    )]
    pub fn parse_pid_file(path: &std::path::Path) -> Option<(u32, u64, u64, String)> {
        let file_content = std::fs::read_to_string(path).ok()?;
        let mut lines_iter = file_content.lines();
        let pid: u32 = lines_iter.next()?.parse().ok()?;
        let timestamp: u64 = lines_iter.next()?.parse().ok()?;
        let exe_hash: u64 = lines_iter.next()?.parse().ok()?;
        let nonce = lines_iter.next()?.to_owned();
        Some((pid, timestamp, exe_hash, nonce))
    }

    /// Get the expected `exe_path_hash` for daemon identity verification.
    ///
    /// Clients call this to compute what the daemon's exe hash should be,
    /// then compare against the PID file.
    #[expect(
        clippy::single_call_fn,
        reason = "exe hash verifier — public API for clients"
    )]
    pub fn expected_daemon_exe_hash() -> u64 {
        if let Ok(current) = std::env::current_exe()
            && let Some(dir) = current.parent()
        {
            let daemon = dir.join("uffs-daemon");
            if daemon.exists() {
                return Self::fnv1a_hash(daemon.to_string_lossy().as_bytes());
            }
            let daemon_exe = dir.join("uffs-daemon.exe");
            if daemon_exe.exists() {
                return Self::fnv1a_hash(daemon_exe.to_string_lossy().as_bytes());
            }
        }
        0_u64
    }
}

impl Drop for LifecycleManager {
    fn drop(&mut self) {
        self.remove_pid_file();
        self.remove_socket_file();
    }
}
