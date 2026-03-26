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
    let Some(daemon_path) = get_process_exe_path(pid) else {
        tracing::debug!(
            pid,
            "Could not determine daemon exe path, skipping verification"
        );
        return true; // graceful degradation
    };

    let daemon_name = daemon_path
        .file_name()
        .and_then(|name| name.to_str())
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
        return false;
    }

    // S4.3.8: Also verify code signature (graceful — warn but don't block)
    let sig_ok = verify_code_signature(&daemon_path);
    if !sig_ok {
        tracing::warn!(
            pid,
            exe = %daemon_path.display(),
            "Daemon code signature verification failed"
        );
    }

    tracing::debug!(
        pid,
        exe = %daemon_path.display(),
        signed = sig_ok,
        "Daemon identity verified"
    );

    // Return true even if signature fails — graceful degradation
    // (unsigned dev builds should still work)
    is_valid
}

/// Verify daemon identity using the PID file at the given path.
///
/// Reads the PID file, extracts the PID and `exe_path_hash`, then:
/// 1. Checks the PID is alive
/// 2. Gets the exe path of that PID
/// 3. Computes FNV-1a hash of the exe path
/// 4. Compares against the hash in the PID file
///
/// Returns `true` if verification passes.
pub fn verify_daemon_pid_file(pid_path: &std::path::Path) -> bool {
    let Ok(content) = std::fs::read_to_string(pid_path) else {
        return true; // no PID file = can't verify, allow
    };

    let mut lines = content.lines();
    let Some(pid) = lines.next().and_then(|line| line.parse::<u32>().ok()) else {
        return true;
    };
    let _timestamp: u64 = lines.next().and_then(|line| line.parse().ok()).unwrap_or(0);
    let Some(expected_hash) = lines.next().and_then(|line| line.parse::<u64>().ok()) else {
        return true; // old format PID file without hash
    };

    // Skip hash verification if 0 (couldn't determine at write time)
    if expected_hash == 0 {
        return verify_daemon_identity(pid);
    }

    // Get the exe path and compute its hash
    let Some(exe_path) = get_process_exe_path(pid) else {
        return true; // can't get path, allow
    };

    // FNV-1a 64-bit hash (must match uffs-daemon/lifecycle.rs)
    let actual_hash = {
        let data = exe_path.to_string_lossy();
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for &byte in data.as_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        hash
    };

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

// ── Platform-specific exe path lookup ───────────────────────────────────

/// Get the executable path for a running process by PID.
///
/// - **macOS**: `proc_pidpath()`
/// - **Linux**: `/proc/{pid}/exe` readlink
/// - **Windows**: `QueryFullProcessImageNameW()`
///
/// Returns `None` if the process is not found or the API is unavailable.
#[cfg(target_os = "macos")]
#[expect(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "proc_pidpath takes c_int pid and u32 bufsize, returns i32 len — all bounded"
)]
fn get_process_exe_path(pid: u32) -> Option<PathBuf> {
    // MAXPATHLEN on macOS is 1024; proc_pidpath needs at least this
    let mut buf = vec![0_u8; 4096];

    // SAFETY: proc_pidpath is a documented macOS API (libproc.h).
    #[expect(unsafe_code, reason = "proc_pidpath requires unsafe FFI")]
    let len = unsafe {
        libc::proc_pidpath(
            pid as libc::c_int,
            buf.as_mut_ptr().cast::<libc::c_void>(),
            buf.len() as u32,
        )
    };

    if len <= 0_i32 {
        return None;
    }

    let path_str = core::str::from_utf8(buf.get(..len as usize)?).ok()?;
    Some(PathBuf::from(path_str))
}

// ── Linux: /proc/{pid}/exe ──────────────────────────────────────────────

/// Linux: reads `/proc/{pid}/exe` symlink.
#[cfg(target_os = "linux")]
fn get_process_exe_path(pid: u32) -> Option<PathBuf> {
    let proc_path = format!("/proc/{pid}/exe");
    std::fs::read_link(&proc_path).ok()
}

// ── Windows: QueryFullProcessImageNameW ─────────────────────────────────

/// Windows: uses `QueryFullProcessImageNameW()`.
#[cfg(target_os = "windows")]
fn get_process_exe_path(pid: u32) -> Option<PathBuf> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
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

/// Fallback for unknown platforms.
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn get_process_exe_path(_pid: u32) -> Option<PathBuf> {
    None // graceful degradation
}

// ────────────────────────────────────────────────────────────────────────────
// S4.3.8: Code Signature Verification
// ────────────────────────────────────────────────────────────────────────────

/// Verify the code signature of the daemon binary (S4.3.8).
///
/// - **macOS**: uses `codesign --verify` (checks Apple code signature)
/// - **Windows**: uses `Get-AuthenticodeSignature` via PowerShell (checks
///   Authenticode / Microsoft code signature)
/// - **Linux**: no standard code signing — always returns `true`
///
/// Returns `true` if the signature is valid or verification is unavailable.
/// Logs a warning if the signature check fails but does NOT block connection
/// (graceful degradation).
/// macOS: verify via `codesign --verify --strict`.
#[cfg(target_os = "macos")]
pub fn verify_code_signature(exe_path: &std::path::Path) -> bool {
    let output = std::process::Command::new("codesign")
        .args(["--verify", "--strict", "--deep"])
        .arg(exe_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output();

    match output {
        Ok(out) => {
            if out.status.success() {
                tracing::debug!(exe = %exe_path.display(), "Code signature valid");
                true
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr);
                if stderr.contains("not signed") || stderr.contains("code object is not signed") {
                    tracing::debug!(exe = %exe_path.display(), "Binary is not code-signed (acceptable for dev builds)");
                    true
                } else {
                    tracing::warn!(exe = %exe_path.display(), "Code signature INVALID — binary may have been tampered with");
                    false
                }
            }
        }
        Err(_codesign_err) => {
            tracing::debug!("Code signature verification not available on this platform");
            true
        }
    }
}

/// Windows: verify Authenticode signature via PowerShell.
#[cfg(target_os = "windows")]
pub fn verify_code_signature(exe_path: &std::path::Path) -> bool {
    let path_str = exe_path.to_string_lossy();
    let script = format!(
        "(Get-AuthenticodeSignature '{}').Status",
        path_str.replace('\'', "''")
    );

    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            match stdout.as_str() {
                "Valid" => {
                    tracing::debug!(exe = %exe_path.display(), "Code signature valid");
                    true
                }
                "HashMismatch" | "UnknownError" => {
                    tracing::warn!(exe = %exe_path.display(), "Code signature INVALID — binary may have been tampered with");
                    false
                }
                _ => {
                    tracing::debug!(exe = %exe_path.display(), "Binary is not code-signed (acceptable for dev builds)");
                    true
                }
            }
        }
        Err(_ps_err) => {
            tracing::debug!("Code signature verification not available on this platform");
            true
        }
    }
}

/// Linux + other platforms: no standard code signing mechanism.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn verify_code_signature(_exe_path: &std::path::Path) -> bool {
    tracing::debug!("Code signature verification not available on this platform");
    true
}
