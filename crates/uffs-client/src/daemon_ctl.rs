// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Daemon lifecycle helpers: socket path, PID file, exe discovery, spawning.
//!
//! Extracted from `connect.rs` for file-size policy compliance.
//! All items are re-exported from `connect.rs` — callers see no change.

use std::path::PathBuf;

/// Platform-specific socket/pipe path (must match daemon's `ipc::socket_path`).
///
/// On Windows this returns the legacy `AF_UNIX` socket path, which is still
/// served by the daemon as a fallback during the named-pipe transition.
/// New code on Windows should prefer the Windows-only `pipe_name`
/// helper in this module — it avoids the `ws2_32.dll` import
/// (+54 ms launch cost).
#[must_use]
pub fn socket_path() -> PathBuf {
    let base = dirs_next::data_local_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    #[cfg(target_os = "macos")]
    {
        base.join("uffs").join("daemon.sock")
    }
    #[cfg(target_os = "linux")]
    {
        std::env::var("XDG_RUNTIME_DIR").map_or_else(
            |_| base.join("uffs").join("daemon.sock"),
            |runtime_dir| PathBuf::from(runtime_dir).join("uffs").join("daemon.sock"),
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        base.join("uffs").join("daemon.sock")
    }
}

/// Windows named-pipe path (`\\.\pipe\uffs-<hash>`).
///
/// This is the preferred IPC transport on Windows — replaces `AF_UNIX`
/// to avoid the `ws2_32.dll` launch overhead.  The name is deterministic
/// per user (FNV-1a of the user SID); see [`uffs_security::pipe`] for
/// the security model.
///
/// # Errors
///
/// Returns an error if the user SID cannot be resolved, which should
/// only happen on a severely broken Windows session.
#[cfg(windows)]
pub fn pipe_name() -> std::io::Result<String> {
    uffs_security::pipe::pipe_name_for_current_user()
}

/// S4.3.4: Verify daemon identity after connecting.
pub fn verify_daemon_after_connect() {
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
///
/// On Unix, opens the `AF_UNIX` socket at `sock_path`.
/// On Windows, opens the named pipe (no `ws2_32` cost) — `sock_path` is
/// unused but kept for API stability.
pub fn keepalive_send_blocking(sock_path: &std::path::Path) {
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
        use std::fs::OpenOptions;
        use std::io::Write;

        // `sock_path` is unused on Windows — the pipe name is derived
        // from the current user's SID — but we keep the parameter for
        // cross-platform API parity.  Discard explicitly to silence the
        // unused-parameter warning without introducing a suppression.
        _ = sock_path;

        let Ok(name) = pipe_name() else {
            return;
        };
        if let Ok(mut pipe) = OpenOptions::new().read(true).write(true).open(&name) {
            let msg = r#"{"jsonrpc":"2.0","id":0,"method":"keepalive"}"#;
            drop(pipe.write_all(msg.as_bytes()));
            drop(pipe.write_all(b"\n"));
            drop(pipe.flush());
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

/// Find the `uffs` CLI executable.
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

/// Find the `uffsd` daemon executable.
///
/// Search order:
/// 1. If the current binary is already `uffsd`, return it.
/// 2. Look for `uffsd` / `uffsd.exe` next to the current binary.
/// 3. Fall back to bare `uffsd` (rely on `$PATH`).
#[must_use]
pub fn find_daemon_exe() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        let name = exe.file_stem().and_then(|stem| stem.to_str()).unwrap_or("");
        if name == "uffsd" {
            return exe;
        }
        if let Some(parent) = exe.parent() {
            let daemon_bin = if cfg!(windows) { "uffsd.exe" } else { "uffsd" };
            let sibling = parent.join(daemon_bin);
            if sibling.exists() {
                return sibling;
            }
        }
    }
    PathBuf::from("uffsd")
}

// ── Daemon Spawn ──────────────────────────────────────────────────────────

/// Policy for whether `spawn_daemon` may trigger a Windows UAC prompt.
///
/// Before v0.5.36, `spawn_daemon` on Windows unconditionally used
/// `ShellExecuteW("runas")` whenever the current process was not
/// elevated — so any non-admin shell running `uffs <pattern>` with the
/// daemon stopped would get a UAC dialog as a side-effect.  That was
/// surprising and made piping or scripting the CLI fragile.
///
/// The new default is [`ElevationPolicy::RequireExistingElevation`]:
/// the spawn succeeds only if the current process is already elevated;
/// otherwise it returns [`crate::error::ClientError::DaemonNeedsElevation`] and the
/// CLI renders an actionable message.  Callers that actually want the
/// UAC dialog (e.g. `uffs daemon start --elevate`) must opt in with
/// [`ElevationPolicy::AllowUacPrompt`].
///
/// Has no effect on Unix — Unix spawn never triggers UAC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ElevationPolicy {
    /// Spawn only if this process is already elevated.  If not, return
    /// [`crate::error::ClientError::DaemonNeedsElevation`] without
    /// touching the UI.
    ///
    /// This is the default for every implicit auto-spawn path (e.g.
    /// `UffsClient::connect_with_args`).
    #[default]
    RequireExistingElevation,

    /// When not elevated, request a UAC prompt via `ShellExecuteW`
    /// with the `"runas"` verb.  Preserves the pre-v0.5.36 behavior.
    ///
    /// Used by `uffs daemon start --elevate` and by auto-spawn paths
    /// when the environment variable `UFFS_ELEVATE=1` is set.
    AllowUacPrompt,
}

/// Pure policy decision used by [`resolve_elevation_policy`].
///
/// Rules, in priority order:
///
/// 1. If `force_allow` is `true` (e.g. `uffs daemon start --elevate`), return
///    [`ElevationPolicy::AllowUacPrompt`].
/// 2. Otherwise, if `env_value` contains a truthy token (`1`, `true`, `yes`,
///    `on`, case-insensitive — leading/trailing whitespace is trimmed), return
///    [`ElevationPolicy::AllowUacPrompt`].  This is how `UFFS_ELEVATE` is
///    interpreted.
/// 3. Otherwise, return [`ElevationPolicy::RequireExistingElevation`].
///
/// Kept env-free so both the async and sync clients (and tests) can
/// share one decision matrix without racing on real environment state.
#[must_use]
pub fn elevation_policy_from(force_allow: bool, env_value: Option<&str>) -> ElevationPolicy {
    if force_allow {
        return ElevationPolicy::AllowUacPrompt;
    }
    if let Some(raw) = env_value {
        let normalized = raw.trim().to_ascii_lowercase();
        if matches!(normalized.as_str(), "1" | "true" | "yes" | "on") {
            return ElevationPolicy::AllowUacPrompt;
        }
    }
    ElevationPolicy::RequireExistingElevation
}

/// Resolve the effective [`ElevationPolicy`] for an implicit
/// auto-spawn.
///
/// Reads the `UFFS_ELEVATE` environment variable once and feeds the
/// result into [`elevation_policy_from`].  `force_allow = true` from
/// an explicit `--elevate` flag short-circuits the env lookup.
#[must_use]
pub fn resolve_elevation_policy(force_allow: bool) -> ElevationPolicy {
    elevation_policy_from(force_allow, std::env::var("UFFS_ELEVATE").ok().as_deref())
}

/// Spawn the daemon as a detached background process.
///
/// On **Unix**, uses a normal `Command::new` spawn (no elevation needed);
/// the `policy` parameter is ignored.
///
/// On **Windows**, behavior depends on `policy` and the current
/// elevation state:
///
/// | already elevated | policy                        | action                        |
/// |------------------|-------------------------------|-------------------------------|
/// | yes              | any                           | `CreateProcessW` (no UAC)     |
/// | no               | `RequireExistingElevation`    | return `DaemonNeedsElevation` |
/// | no               | `AllowUacPrompt`              | `ShellExecuteW("runas")` + UAC|
///
/// # Errors
///
/// Returns [`crate::error::ClientError::DaemonStartFailed`] if the
/// process creation itself fails, or
/// [`crate::error::ClientError::DaemonNeedsElevation`] if the policy
/// does not allow a UAC prompt in the current elevation state.
#[cfg(unix)]
pub fn spawn_daemon(
    exe: &std::path::Path,
    args: &[&str],
    _policy: ElevationPolicy,
) -> Result<(), crate::error::ClientError> {
    // `policy` is Windows-only; the Unix spawn never prompts for
    // elevation.  The parameter stays in the public signature so
    // callers can pass the same value on every platform.
    spawn_daemon_unix(exe, args)
}

/// Windows implementation of [`spawn_daemon`].
///
/// See the generic doc comment above — behavior is decided by
/// `policy` combined with the current elevation state.
#[cfg(windows)]
pub fn spawn_daemon(
    exe: &std::path::Path,
    args: &[&str],
    policy: ElevationPolicy,
) -> Result<(), crate::error::ClientError> {
    spawn_daemon_windows(exe, args, policy)
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
                "Failed to spawn {}: {spawn_err}",
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
    policy: ElevationPolicy,
) -> Result<(), crate::error::ClientError> {
    let elevated = is_elevated();
    tracing::debug!(
        exe = %exe.display(),
        ?args,
        elevated,
        ?policy,
        "spawn_daemon_windows"
    );

    if elevated {
        tracing::debug!("spawning via CreateProcessW (no handle inheritance)");
        spawn_detached_no_inherit(exe, args)?;
        return Ok(());
    }

    match policy {
        ElevationPolicy::AllowUacPrompt => {
            tracing::debug!("NOT elevated, using ShellExecuteW runas (policy allows UAC)");
            tracing::info!("Not elevated — requesting elevation via UAC prompt");
            shell_execute_elevated(exe, args)?;
            tracing::debug!("ShellExecuteW returned OK");
            Ok(())
        }
        ElevationPolicy::RequireExistingElevation => {
            tracing::info!("Not elevated and policy forbids UAC — returning DaemonNeedsElevation");
            Err(crate::error::ClientError::DaemonNeedsElevation {
                daemon_path: exe.display().to_string(),
            })
        }
    }
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
        cb: u32::try_from(size_of::<STARTUPINFOW>()).unwrap_or(u32::MAX),
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
            core::ptr::from_ref(&si),
            core::ptr::from_mut(&mut pi),
        )
    };

    match result {
        Ok(()) => {
            tracing::debug!(pid = pi.dwProcessId, "spawn_detached_no_inherit: spawned");
            tracing::info!(
                pid = pi.dwProcessId,
                "Daemon spawned (no handle inheritance)"
            );
            // SAFETY: both handles were just returned by CreateProcessW
            // above and are not aliased elsewhere.
            #[expect(unsafe_code, reason = "closing Win32 process handle")]
            let process_close = unsafe { CloseHandle(pi.hProcess) };
            drop(process_close);
            // SAFETY: ditto — thread handle is owned by us.
            #[expect(unsafe_code, reason = "closing Win32 thread handle")]
            let thread_close = unsafe { CloseHandle(pi.hThread) };
            drop(thread_close);
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
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = HANDLE::default();

    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that does not
    // need closing.
    #[expect(unsafe_code, reason = "Win32 pseudo-handle accessor")]
    let current_proc = unsafe { GetCurrentProcess() };
    // SAFETY: `OpenProcessToken` writes a valid token handle into `token`
    // on success; `current_proc` is valid.
    #[expect(unsafe_code, reason = "Win32 token FFI")]
    let open_result =
        unsafe { OpenProcessToken(current_proc, TOKEN_QUERY, core::ptr::from_mut(&mut token)) };
    if open_result.is_err() {
        return false;
    }

    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut size = 0_u32;
    // SAFETY: `token` is a valid token handle; the out-pointer points to
    // a stack-owned `TOKEN_ELEVATION` that lives for the whole call.
    #[expect(unsafe_code, reason = "Win32 token information query")]
    let query_result = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some(core::ptr::from_mut(&mut elevation).cast()),
            u32::try_from(size_of::<TOKEN_ELEVATION>()).unwrap_or(u32::MAX),
            core::ptr::from_mut(&mut size),
        )
    };
    // SAFETY: `token` is owned by this function; no other code references it.
    #[expect(unsafe_code, reason = "CloseHandle for owned Win32 handle")]
    let close_result = unsafe { CloseHandle(token) };
    drop(close_result);

    query_result.is_ok() && elevation.TokenIsElevated != 0
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

#[cfg(test)]
mod elevation_policy_tests {
    use super::{ElevationPolicy, elevation_policy_from};

    /// Explicit `force_allow` (e.g. `--elevate`) always wins, even
    /// against an empty or falsy env value.
    #[test]
    fn force_allow_always_permits_uac() {
        assert_eq!(
            elevation_policy_from(true, None),
            ElevationPolicy::AllowUacPrompt,
        );
        assert_eq!(
            elevation_policy_from(true, Some("")),
            ElevationPolicy::AllowUacPrompt,
        );
        assert_eq!(
            elevation_policy_from(true, Some("0")),
            ElevationPolicy::AllowUacPrompt,
        );
    }

    /// Without `force_allow` and without the env var, the default
    /// policy must refuse UAC.  This is the behavioral change v0.5.36
    /// introduces and the linchpin for the whole P7 fix.
    #[test]
    fn missing_env_defaults_to_require_existing_elevation() {
        assert_eq!(
            elevation_policy_from(false, None),
            ElevationPolicy::RequireExistingElevation,
        );
    }

    /// Every documented truthy token must promote to
    /// `AllowUacPrompt`.  Trimming and case-folding are also expected.
    #[test]
    fn truthy_env_values_permit_uac() {
        for token in [
            "1", "true", "TRUE", "True", "yes", "YES", "on", "ON", "  1  ", " yes\n",
        ] {
            assert_eq!(
                elevation_policy_from(false, Some(token)),
                ElevationPolicy::AllowUacPrompt,
                "token {token:?} should enable UAC",
            );
        }
    }

    /// Falsy / unrecognised tokens must keep the conservative default.
    #[test]
    fn falsy_or_unknown_env_values_keep_default() {
        for token in ["0", "false", "no", "off", "", "maybe", "2", "nope"] {
            assert_eq!(
                elevation_policy_from(false, Some(token)),
                ElevationPolicy::RequireExistingElevation,
                "token {token:?} should not enable UAC",
            );
        }
    }

    /// [`ElevationPolicy::default`] must be the safe option.  New
    /// callers that rely on `..Default::default()` must not silently
    /// get the UAC-triggering variant.
    #[test]
    fn default_policy_is_require_existing_elevation() {
        assert_eq!(
            ElevationPolicy::default(),
            ElevationPolicy::RequireExistingElevation,
        );
    }
}
