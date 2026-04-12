//! Daemon lifecycle helpers: socket path, PID file, exe discovery, spawning.
//!
//! Extracted from `connect.rs` for file-size policy compliance.
//! All items are re-exported from `connect.rs` — callers see no change.

use std::path::PathBuf;

/// Platform-specific socket/pipe path (must match daemon's `ipc::socket_path`).
#[must_use]
pub fn socket_path() -> PathBuf {
    let base = dirs_next::data_local_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    #[cfg(target_os = "macos")]
    {
        base.join("uffs").join("daemon.sock")
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(runtime_dir).join("uffs").join("daemon.sock")
        } else {
            base.join("uffs").join("daemon.sock")
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        base.join("uffs").join("daemon.sock")
    }
}

/// S4.3.4: Verify daemon identity after connecting.
pub(crate) fn verify_daemon_after_connect() {
    let pid_path = pid_file_path();
    if !pid_path.exists() {
        tracing::debug!("No PID file found, skipping daemon identity verification");
        return;
    }
    if !crate::verify::verify_daemon_pid_file(&pid_path) {
        tracing::warn!(
            path = %pid_path.display(),
            "Daemon identity verification failed — proceed with caution"
        );
    }
}

/// Send a keepalive message using blocking std I/O (works on all platforms).
pub(crate) fn keepalive_send_blocking(sock_path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        if let Ok(mut stream) = UnixStream::connect(sock_path) {
            let msg = r#"{"jsonrpc":"2.0","id":0,"method":"keepalive"}"#;
            drop(stream.write_all(msg.as_bytes()));
            drop(stream.write_all(b"\n"));
            drop(stream.flush());
        }
    }
    #[cfg(windows)]
    {
        use std::io::Write;
        use std::os::windows::net::UnixStream;
        if let Ok(mut stream) = UnixStream::connect(sock_path) {
            let msg = r#"{"jsonrpc":"2.0","id":0,"method":"keepalive"}"#;
            drop(stream.write_all(msg.as_bytes()));
            drop(stream.write_all(b"\n"));
            drop(stream.flush());
        }
    }
}

/// PID file path (must match daemon's lifecycle.rs).
#[must_use]
pub fn pid_file_path() -> PathBuf {
    let base = dirs_next::data_local_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("uffs").join("daemon.pid")
}

/// Parse a daemon PID file. Returns `(pid, timestamp, exe_hash, nonce)`.
#[must_use]
pub fn parse_pid_file(path: &std::path::Path) -> Option<(u32, u64, u64, String)> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut lines = content.lines();
    let pid: u32 = lines.next()?.parse().ok()?;
    let ts: u64 = lines.next()?.parse().ok()?;
    let hash: u64 = lines.next()?.parse().ok()?;
    let nonce = lines.next()?.to_owned();
    Some((pid, ts, hash, nonce))
}

/// Find the `uffs` executable (the CLI binary that also embeds the daemon).
#[must_use]
pub fn find_uffs_exe() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let name = exe.file_stem().and_then(|stem| stem.to_str()).unwrap_or("");
        if name == "uffs" {
            return exe;
        }
        if let Some(parent) = exe.parent() {
            let uffs_bin = if cfg!(windows) { "uffs.exe" } else { "uffs" };
            let sibling = parent.join(uffs_bin);
            if sibling.exists() {
                return sibling;
            }
        }
    }
    PathBuf::from("uffs")
}

/// Find the `uffs-daemon` executable (legacy — prefer `find_uffs_exe`).
#[must_use]
pub fn find_daemon_exe() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| {
            let parent = exe.parent()?;
            let unix = parent.join("uffs-daemon");
            let win = parent.join("uffs-daemon.exe");
            if unix.exists() {
                Some(unix)
            } else if win.exists() {
                Some(win)
            } else {
                None
            }
        })
        .unwrap_or_else(|| PathBuf::from("uffs-daemon"))
}

// ── Daemon Spawn ──────────────────────────────────────────────────────────

/// Spawn the daemon as a detached background process.
///
/// On **Unix**, uses a normal `Command::new` spawn (no elevation needed).
/// On **Windows**, elevation-aware: uses `CreateProcessW` if already elevated,
/// or `ShellExecuteW("runas")` to trigger a UAC prompt.
///
/// # Errors
///
/// Returns `DaemonStartFailed` if spawning fails.
#[expect(
    clippy::single_call_fn,
    reason = "platform-specific spawn logic — clarity over inlining"
)]
pub(crate) fn spawn_daemon(
    exe: &std::path::Path,
    args: &[&str],
) -> Result<(), crate::error::ClientError> {
    #[cfg(unix)]
    spawn_daemon_unix(exe, args)?;

    #[cfg(windows)]
    spawn_daemon_windows(exe, args)?;

    Ok(())
}

/// Unix daemon spawn: simple detached process.
/// # Errors
///
/// Returns [`ClientError`](crate::error::ClientError) if the daemon process
/// cannot be spawned.
#[cfg(unix)]
#[expect(
    clippy::single_call_fn,
    reason = "platform-specific helper — clarity over inlining"
)]
fn spawn_daemon_unix(
    exe: &std::path::Path,
    args: &[&str],
) -> Result<(), crate::error::ClientError> {
    std::process::Command::new(exe)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|spawn_err| {
            crate::error::ClientError::DaemonStartFailed(format!(
                "Failed to spawn {} daemon run: {spawn_err}",
                exe.display()
            ))
        })?;
    Ok(())
}

/// Windows daemon spawn: elevation-aware.
#[cfg(windows)]
#[expect(
    clippy::single_call_fn,
    reason = "platform-specific helper — clarity over inlining"
)]
fn spawn_daemon_windows(
    exe: &std::path::Path,
    args: &[&str],
) -> Result<(), crate::error::ClientError> {
    let elevated = is_elevated();
    tracing::debug!(exe = %exe.display(), ?args, elevated, "spawn_daemon_windows");

    if elevated {
        tracing::debug!("spawning via CreateProcessW (no handle inheritance)");
        spawn_detached_no_inherit(exe, args)?;
    } else {
        tracing::debug!("NOT elevated, using ShellExecuteW runas");
        tracing::info!("Not elevated — requesting elevation via UAC prompt");
        shell_execute_elevated(exe, args)?;
        tracing::debug!("ShellExecuteW returned OK");
    }
    Ok(())
}

/// Spawn the daemon as a fully detached process with NO handle inheritance.
///
/// Uses `CreateProcessW` directly with `bInheritHandles = FALSE` and
/// `DETACHED_PROCESS` creation flag.
#[cfg(windows)]
fn spawn_detached_no_inherit(
    exe: &std::path::Path,
    args: &[&str],
) -> Result<(), crate::error::ClientError> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        CreateProcessW, DETACHED_PROCESS, PROCESS_INFORMATION, STARTUPINFOW,
    };

    let mut cmd_line = String::new();
    cmd_line.push('"');
    cmd_line.push_str(&exe.to_string_lossy());
    cmd_line.push('"');
    for arg in args {
        cmd_line.push(' ');
        cmd_line.push_str(arg);
    }

    let mut cmd_wide: Vec<u16> = cmd_line.encode_utf16().chain(core::iter::once(0)).collect();

    let si = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut pi = PROCESS_INFORMATION::default();

    // SAFETY: CreateProcessW is a well-defined Win32 API. All pointers are
    // valid: cmd_wide is a mutable null-terminated UTF-16 buffer, si is
    // a zeroed STARTUPINFOW with cb set, pi is zeroed output buffer.
    // We close the returned handles immediately after success.
    #[expect(unsafe_code, reason = "CreateProcessW requires unsafe FFI")]
    let result = unsafe {
        CreateProcessW(
            None,
            Some(windows::core::PWSTR(cmd_wide.as_mut_ptr())),
            None,
            None,
            false, // bInheritHandles = FALSE ← key fix
            DETACHED_PROCESS,
            None,
            None,
            &si,
            &mut pi,
        )
    };

    match result {
        Ok(()) => {
            tracing::debug!(pid = pi.dwProcessId, "spawn_detached_no_inherit: spawned");
            tracing::info!(
                pid = pi.dwProcessId,
                "Daemon spawned (no handle inheritance)"
            );
            // SAFETY: valid handles returned by CreateProcessW.
            #[expect(unsafe_code, reason = "closing Win32 handles from CreateProcessW")]
            unsafe {
                let _ = CloseHandle(pi.hProcess);
                let _ = CloseHandle(pi.hThread);
            }
            Ok(())
        }
        Err(win_err) => {
            tracing::debug!(error = %win_err, "spawn_detached_no_inherit: FAILED");
            Err(crate::error::ClientError::DaemonStartFailed(format!(
                "CreateProcessW failed for {}: {win_err}",
                exe.display()
            )))
        }
    }
}

// ── Windows Elevation Helpers ─────────────────────────────────────────────

/// Check if the current process is running with Administrator privileges.
#[cfg(windows)]
fn is_elevated() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // SAFETY: Win32 token query APIs are well-defined and we close the handle.
    #[expect(
        unsafe_code,
        reason = "Win32 token elevation check requires unsafe FFI"
    )]
    unsafe {
        let mut token = windows::Win32::Foundation::HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut size = 0_u32;
        let result = GetTokenInformation(
            token,
            TokenElevation,
            Some(core::ptr::from_mut(&mut elevation).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        );
        let _ = CloseHandle(token);
        result.is_ok() && elevation.TokenIsElevated != 0
    }
}

/// Launch a process elevated via `ShellExecuteW` with the `"runas"` verb.
///
/// This triggers the Windows UAC consent dialog. If the user clicks "Yes",
/// the process starts elevated; if they click "No" or dismiss the dialog,
/// an error is returned.
#[cfg(windows)]
fn shell_execute_elevated(
    exe: &std::path::Path,
    args: &[&str],
) -> Result<(), crate::error::ClientError> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::core::PCWSTR;

    let verb: Vec<u16> = "runas\0".encode_utf16().collect();
    let exe_str = exe.to_string_lossy();
    let file: Vec<u16> = format!("{exe_str}\0").encode_utf16().collect();
    let params_str = args.join(" ");
    let params: Vec<u16> = format!("{params_str}\0").encode_utf16().collect();

    tracing::debug!(
        verb = "runas",
        file = %exe_str,
        params = %params_str,
        "ShellExecuteW"
    );

    // SAFETY: ShellExecuteW is a well-defined Win32 Shell API.
    // All PCWSTR pointers are valid null-terminated UTF-16 buffers
    // that outlive the call (stack-allocated Vecs above).
    #[expect(unsafe_code, reason = "ShellExecuteW requires unsafe FFI")]
    let hinst = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR(params.as_ptr()),
            PCWSTR::null(),
            windows::Win32::UI::WindowsAndMessaging::SW_HIDE,
        )
    };

    // ShellExecuteW returns HINSTANCE — values > 32 indicate success.
    let code = hinst.0 as isize;
    if code > 32 {
        tracing::debug!(code, "ShellExecuteW succeeded");
        Ok(())
    } else {
        let msg = match code {
            0 => "The OS is out of memory or resources",
            2 => "Executable not found (ERROR_FILE_NOT_FOUND)",
            3 => "Path not found (ERROR_PATH_NOT_FOUND)",
            5 => "Access denied (ERROR_ACCESS_DENIED)",
            _ => "Unknown ShellExecuteW error",
        };
        tracing::debug!(code, msg, "ShellExecuteW failed");
        Err(crate::error::ClientError::DaemonStartFailed(format!(
            "ShellExecuteW(runas) failed for {}: code={code} — {msg}",
            exe.display()
        )))
    }
}
