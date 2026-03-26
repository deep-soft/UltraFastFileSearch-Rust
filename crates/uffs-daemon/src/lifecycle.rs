//! Daemon lifecycle: PID file, idle timeout, auto-retire, shutdown.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

/// Handle given to request handlers to control the lifecycle.
#[derive(Clone)]
pub struct LifecycleHandle {
    /// Send `true` to trigger shutdown.
    shutdown_tx: watch::Sender<bool>,
    /// Reset idle timer flag (checked by the timer task).
    idle_reset: Arc<AtomicBool>,
    /// Shutdown nonce — must be provided in the `shutdown` RPC call (S4.4.9).
    shutdown_nonce: Arc<std::sync::Mutex<Option<String>>>,
}

impl LifecycleHandle {
    /// Signal the daemon to shut down gracefully.
    pub fn request_shutdown(&self) {
        let _ignore = self.shutdown_tx.send(true);
    }

    /// Reset the idle timer (called on every query/keepalive).
    pub fn reset_idle_timer(&self) {
        self.idle_reset.store(true, Ordering::Relaxed);
    }

    /// Verify a shutdown nonce matches the one in the PID file (S4.4.9).
    ///
    /// If no nonce is set (shouldn't happen), allows shutdown anyway.
    pub fn verify_shutdown_nonce(&self, provided: &str) -> bool {
        let guard = self.shutdown_nonce.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_deref() {
            Some(expected) => expected == provided,
            None => true, // no nonce set — allow (shouldn't happen in practice)
        }
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
    pub fn new(data_dir: &std::path::Path, idle_timeout: Option<Duration>) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let idle_reset = Arc::new(AtomicBool::new(false));
        let shutdown_nonce_shared = Arc::new(std::sync::Mutex::new(None));

        let pid_path = data_dir.join("daemon.pid");

        Self {
            shutdown_rx,
            handle: LifecycleHandle {
                shutdown_tx,
                idle_reset,
                shutdown_nonce: shutdown_nonce_shared,
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
    /// - `exe_path_hash`: FNV-1a hash of the daemon executable path (for identity verification)
    /// - `shutdown_nonce`: random token required for the `shutdown` RPC method (S4.4.9)
    pub fn write_pid_file(&mut self) -> std::io::Result<()> {
        if let Some(parent) = self.pid_path.parent() {
            uffs_security::fs::create_secure_dir(parent)?;
        }

        let exe_hash = exe_path_hash();
        let nonce = generate_nonce();
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
                .map_or(0, |d| d.as_secs()),
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

    /// Check for stale PID file on startup. Returns `true` if safe to proceed.
    pub fn check_stale_pid(&self) -> bool {
        if !self.pid_path.exists() {
            return true;
        }

        let content = match std::fs::read_to_string(&self.pid_path) {
            Ok(c) => c,
            Err(_) => {
                // Can't read — remove and proceed
                let _ignore = std::fs::remove_file(&self.pid_path);
                return true;
            }
        };

        let pid_str = content.lines().next().unwrap_or("");
        let pid: u32 = pid_str.parse().unwrap_or(0);

        if pid == 0 {
            let _ignore = std::fs::remove_file(&self.pid_path);
            return true;
        }

        // Check if the process is still alive
        if is_process_alive(pid) {
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

    /// Run the idle timer. Returns when shutdown is requested or idle timeout fires.
    pub async fn run_idle_timer(&mut self) {
        let Some(timeout) = self.idle_timeout else {
            // --no-retire: just wait for shutdown signal
            let _ = self.shutdown_rx.wait_for(|&v| v).await;
            return;
        };

        loop {
            // Reset the flag
            self.handle.idle_reset.store(false, Ordering::Relaxed);

            tokio::select! {
                // Wait for idle timeout
                () = tokio::time::sleep(timeout) => {
                    // Check if idle was reset during the sleep
                    if self.handle.idle_reset.load(Ordering::Relaxed) {
                        // Activity happened — restart the timer
                        continue;
                    }
                    // No activity — fire idle timeout
                    tracing::info!(
                        timeout_secs = timeout.as_secs(),
                        "Idle timeout reached — auto-retiring"
                    );
                    return;
                }
                // Or shutdown was requested explicitly
                _ = self.shutdown_rx.wait_for(|&v| v) => {
                    tracing::info!("Shutdown requested");
                    return;
                }
            }
        }
    }

    /// Get the data directory path (parent of PID file).
    pub fn data_dir(&self) -> &std::path::Path {
        self.pid_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
    }
}

impl Drop for LifecycleManager {
    fn drop(&mut self) {
        self.remove_pid_file();
    }
}

/// Check if a process with the given PID is alive.
#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    // SAFETY: kill with signal 0 checks if the process exists without sending a signal.
    #[expect(unsafe_code, reason = "kill(pid, 0) is a standard POSIX liveness check")]
    unsafe {
        libc::kill(pid as libc::pid_t, 0) == 0
    }
}

/// Check if a process with the given PID is alive (Windows).
#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    // SAFETY: OpenProcess is a well-defined Win32 API.
    #[expect(unsafe_code, reason = "OpenProcess requires unsafe FFI")]
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
        if let Ok(h) = handle {
            let _ = CloseHandle(h);
            true
        } else {
            false
        }
    }
}

/// FNV-1a hash of the current executable path (S4.3.1).
///
/// Used in the PID file so the client can verify the daemon is the expected binary.
fn exe_path_hash() -> u64 {
    let path = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    fnv1a_hash(path.as_bytes())
}

/// FNV-1a 64-bit hash (no external dep needed).
fn fnv1a_hash(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in data {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Generate a random 16-char hex nonce for shutdown authentication (S4.4.9).
fn generate_nonce() -> String {
    use rand::Rng;
    let mut bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Parse a PID file and extract its fields.
///
/// Returns `(pid, timestamp, exe_hash, nonce)`.
pub fn parse_pid_file(path: &std::path::Path) -> Option<(u32, u64, u64, String)> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut lines = content.lines();
    let pid: u32 = lines.next()?.parse().ok()?;
    let timestamp: u64 = lines.next()?.parse().ok()?;
    let exe_hash: u64 = lines.next()?.parse().ok()?;
    let nonce = lines.next()?.to_owned();
    Some((pid, timestamp, exe_hash, nonce))
}

/// Get the expected exe_path_hash for daemon identity verification.
///
/// Clients call this to compute what the daemon's exe hash should be,
/// then compare against the PID file.
pub fn expected_daemon_exe_hash() -> u64 {
    // Look for uffs-daemon next to current exe
    if let Ok(current) = std::env::current_exe() {
        if let Some(dir) = current.parent() {
            let daemon = dir.join("uffs-daemon");
            if daemon.exists() {
                return fnv1a_hash(daemon.to_string_lossy().as_bytes());
            }
            let daemon_exe = dir.join("uffs-daemon.exe");
            if daemon_exe.exists() {
                return fnv1a_hash(daemon_exe.to_string_lossy().as_bytes());
            }
        }
    }
    0
}
