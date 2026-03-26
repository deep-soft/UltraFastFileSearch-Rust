//! Daemon identity verification (S4.3.4-7).
//!
//! After connecting to the daemon socket, the client reads the PID file
//! and verifies:
//! 1. The PID in the file is alive
//! 2. The exe path of that PID matches the expected `uffs-daemon` binary
//!
//! This prevents a rogue process from impersonating the daemon by placing
//! a fake socket file.

use std::path::PathBuf;

/// Verify that the daemon process identified by `pid` is running the
/// expected `uffs-daemon` binary.
///
/// Returns `true` if verification passes or cannot be performed (graceful
/// degradation — don't block the user if the OS API isn't available).
pub fn verify_daemon_identity(pid: u32) -> bool {
    let daemon_path = match get_process_exe_path(pid) {
        Some(p) => p,
        None => {
            tracing::debug!(pid, "Could not determine daemon exe path, skipping verification");
            return true; // graceful degradation
        }
    };

    let daemon_name = daemon_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    // Check that the process is actually uffs-daemon (not something else)
    let is_valid = daemon_name == "uffs-daemon"
        || daemon_name == "uffs-daemon.exe"
        || daemon_name.starts_with("uffs-daemon") // covers uffs-daemon-v0.4.14 etc.
        || daemon_name.starts_with("uffs_daemon"); // covers cargo build output

    if !is_valid {
        tracing::warn!(
            pid,
            exe = %daemon_path.display(),
            "Daemon identity verification FAILED — process is not uffs-daemon"
        );
    } else {
        tracing::debug!(
            pid,
            exe = %daemon_path.display(),
            "Daemon identity verified"
        );
    }

    is_valid
}

/// Verify daemon identity using the PID file at the given path.
///
/// Reads the PID file, extracts the PID and exe_path_hash, then:
/// 1. Checks the PID is alive
/// 2. Gets the exe path of that PID
/// 3. Computes FNV-1a hash of the exe path
/// 4. Compares against the hash in the PID file
///
/// Returns `true` if verification passes.
pub fn verify_daemon_pid_file(pid_path: &std::path::Path) -> bool {
    let content = match std::fs::read_to_string(pid_path) {
        Ok(c) => c,
        Err(_) => return true, // no PID file = can't verify, allow
    };

    let mut lines = content.lines();
    let pid: u32 = match lines.next().and_then(|s| s.parse().ok()) {
        Some(p) => p,
        None => return true,
    };
    let _timestamp: u64 = lines.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let expected_hash: u64 = match lines.next().and_then(|s| s.parse().ok()) {
        Some(h) => h,
        None => return true, // old format PID file without hash
    };

    // Skip hash verification if 0 (couldn't determine at write time)
    if expected_hash == 0 {
        return verify_daemon_identity(pid);
    }

    // Get the exe path and compute its hash
    let exe_path = match get_process_exe_path(pid) {
        Some(p) => p,
        None => return true, // can't get path, allow
    };

    let actual_hash = fnv1a_hash(exe_path.to_string_lossy().as_bytes());

    if actual_hash != expected_hash {
        tracing::warn!(
            pid,
            exe = %exe_path.display(),
            expected_hash,
            actual_hash,
            "Daemon exe_path_hash mismatch — possible impersonation"
        );
        return false;
    }

    true
}

/// FNV-1a 64-bit hash (must match the one in uffs-daemon/lifecycle.rs).
fn fnv1a_hash(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in data {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

// ── Platform-specific exe path lookup ───────────────────────────────────

/// Get the executable path for a running process by PID.
///
/// - **macOS**: `proc_pidpath()`
/// - **Linux**: `/proc/{pid}/exe` readlink
/// - **Windows**: `QueryFullProcessImageNameW()`
fn get_process_exe_path(pid: u32) -> Option<PathBuf> {
    platform_get_exe_path(pid)
}

// ── macOS: proc_pidpath ─────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn platform_get_exe_path(pid: u32) -> Option<PathBuf> {
    // MAXPATHLEN on macOS is 1024; proc_pidpath needs at least this
    let mut buf = vec![0u8; 4096];

    // SAFETY: proc_pidpath is a documented macOS API (libproc.h).
    #[expect(unsafe_code, reason = "proc_pidpath requires unsafe FFI")]
    let len = unsafe {
        libc::proc_pidpath(
            pid as libc::c_int,
            buf.as_mut_ptr().cast::<libc::c_void>(),
            buf.len() as u32,
        )
    };

    if len <= 0 {
        return None;
    }

    let path_str = std::str::from_utf8(&buf[..len as usize]).ok()?;
    Some(PathBuf::from(path_str))
}

// ── Linux: /proc/{pid}/exe ──────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn platform_get_exe_path(pid: u32) -> Option<PathBuf> {
    let proc_path = format!("/proc/{pid}/exe");
    std::fs::read_link(&proc_path).ok()
}

// ── Windows: QueryFullProcessImageNameW ─────────────────────────────────

#[cfg(target_os = "windows")]
fn platform_get_exe_path(pid: u32) -> Option<PathBuf> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: OpenProcess + QueryFullProcessImageNameW are well-defined Win32 APIs.
    #[expect(unsafe_code, reason = "Win32 process query requires unsafe FFI")]
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;

        let mut buf = vec![0u16; 4096];
        let mut size = buf.len() as u32;

        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0), // Win32 path format
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        );

        let _ = CloseHandle(handle);

        if !ok.as_bool() || size == 0 {
            return None;
        }

        let path = String::from_utf16_lossy(&buf[..size as usize]);
        Some(PathBuf::from(path))
    }
}

// ── Fallback for other platforms ────────────────────────────────────────

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn platform_get_exe_path(_pid: u32) -> Option<PathBuf> {
    None // graceful degradation
}
