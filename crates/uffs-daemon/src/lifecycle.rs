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
}

impl LifecycleManager {
    /// Create a new lifecycle manager.
    ///
    /// `idle_timeout`: `None` for `--no-retire`, `Some(duration)` otherwise.
    pub fn new(data_dir: &std::path::Path, idle_timeout: Option<Duration>) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let idle_reset = Arc::new(AtomicBool::new(false));

        let pid_path = data_dir.join("daemon.pid");

        Self {
            shutdown_rx,
            handle: LifecycleHandle {
                shutdown_tx,
                idle_reset,
            },
            pid_path,
            idle_timeout,
        }
    }

    /// Get a handle for request handlers.
    pub fn handle(&self) -> LifecycleHandle {
        self.handle.clone()
    }

    /// Write the PID file.
    pub fn write_pid_file(&self) -> std::io::Result<()> {
        if let Some(parent) = self.pid_path.parent() {
            uffs_security::fs::create_secure_dir(parent)?;
        }
        let content = format!(
            "{}\n{}\n",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
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
