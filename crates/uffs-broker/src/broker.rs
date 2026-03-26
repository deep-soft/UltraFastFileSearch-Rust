//! Windows Access Broker implementation.
//!
//! Runs as a Windows Service (or foreground process for debugging).
//! Listens on a named pipe, verifies client identity, and provides
//! read-only volume handles for MFT access.
//!
//! # Protocol (binary, over named pipe)
//!
//! Request:  1 byte = drive letter ASCII (e.g., b'C')
//! Response: 1 byte status (0=ok, 1=error) + 8 bytes HANDLE value (little-endian u64)
//!
//! The broker opens `\\.\X:` with `FILE_READ_DATA` + `SeBackupPrivilege`,
//! then `DuplicateHandle`s it into the client process with read-only access.

/// Pipe name for broker communication.
pub const BROKER_PIPE_NAME: &str = r"\\.\pipe\uffs-broker";

/// Run the broker (called from main).
#[cfg(windows)]
pub fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--install") {
        return install_service();
    }
    if args.iter().any(|a| a == "--uninstall") {
        return uninstall_service();
    }
    if args.iter().any(|a| a == "--run") {
        return run_foreground();
    }

    eprintln!("uffs-broker: use --install, --uninstall, or --run");
    eprintln!("  --install     Install as Windows Service");
    eprintln!("  --uninstall   Remove Windows Service");
    eprintln!("  --run         Run in foreground (debugging)");
    Ok(())
}

/// Run the broker in foreground mode.
#[cfg(windows)]
fn run_foreground() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .init();

    tracing::info!(pid = std::process::id(), "uffs-broker starting (foreground mode)");

    if !is_elevated() {
        tracing::warn!("Broker is NOT running elevated — volume access will fail");
        tracing::warn!("Run as Administrator or install as a Windows Service");
    }

    serve_pipe_requests()?;

    tracing::info!("uffs-broker stopped");
    Ok(())
}

/// Serve handle requests on the named pipe.
#[cfg(windows)]
fn serve_pipe_requests() -> anyhow::Result<()> {
    tracing::info!(pipe = BROKER_PIPE_NAME, "Listening for handle requests");

    loop {
        let pipe = create_broker_pipe()?;
        wait_for_client(&pipe)?;

        tracing::debug!("Client connected to broker pipe");

        // D7.4: Verify client identity
        let client_pid = get_pipe_client_pid(&pipe);
        if let Some(pid) = client_pid {
            if !verify_client(pid) {
                tracing::warn!(pid, "Rejected broker client — not uffs-daemon");
                disconnect_and_close(&pipe);
                continue;
            }
            tracing::debug!(pid, "Broker client verified as uffs-daemon");
        } else {
            tracing::warn!("Could not determine client PID — rejecting");
            disconnect_and_close(&pipe);
            continue;
        }

        // D7.5: Handle the request
        if let Err(e) = handle_pipe_request(&pipe, client_pid.unwrap_or(0)) {
            tracing::debug!(error = %e, "Pipe request failed");
        }

        disconnect_and_close(&pipe);
    }
}

// ── Windows Service Install/Uninstall ───────────────────────────────────

#[cfg(windows)]
fn install_service() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let output = std::process::Command::new("sc")
        .args([
            "create", "UffsAccessBroker",
            &format!("binPath= \"{}\"", exe.display()),
            "start=", "demand",
            "DisplayName=", "UFFS Access Broker",
        ])
        .output()?;

    if output.status.success() {
        println!("Service installed. Start with: sc start UffsAccessBroker");
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Install failed: {stderr}");
    }
    Ok(())
}

#[cfg(windows)]
fn uninstall_service() -> anyhow::Result<()> {
    let output = std::process::Command::new("sc")
        .args(["delete", "UffsAccessBroker"])
        .output()?;

    if output.status.success() {
        println!("Service uninstalled.");
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Uninstall failed: {stderr}");
    }
    Ok(())
}

// ── D7.3: Named Pipe Operations ─────────────────────────────────────────

/// Create a named pipe with owner-only access.
#[cfg(windows)]
fn create_broker_pipe() -> anyhow::Result<windows::Win32::Foundation::HANDLE> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        CreateNamedPipeW, PIPE_ACCESS_DUPLEX, FILE_FLAG_FIRST_PIPE_INSTANCE,
    };
    use windows::Win32::System::Pipes::{
        PIPE_TYPE_BYTE, PIPE_READMODE_BYTE, PIPE_WAIT,
    };
    use windows::core::PCWSTR;

    let pipe_name: Vec<u16> = std::ffi::OsStr::new(BROKER_PIPE_NAME)
        .encode_wide()
        .chain(Some(0))
        .collect();

    #[expect(unsafe_code, reason = "CreateNamedPipeW requires unsafe FFI")]
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(pipe_name.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            1,      // max instances
            1024,   // out buffer
            1024,   // in buffer
            0,      // default timeout
            None,   // default security (owner-only for elevated process)
        )
    };

    if handle.is_invalid() {
        anyhow::bail!("CreateNamedPipeW failed: {}", std::io::Error::last_os_error());
    }

    Ok(handle)
}

/// Wait for a client to connect to the pipe.
#[cfg(windows)]
fn wait_for_client(pipe: &windows::Win32::Foundation::HANDLE) -> anyhow::Result<()> {
    use windows::Win32::System::Pipes::ConnectNamedPipe;

    #[expect(unsafe_code, reason = "ConnectNamedPipe requires unsafe FFI")]
    let ok = unsafe { ConnectNamedPipe(*pipe, None) };

    if !ok.as_bool() {
        let err = std::io::Error::last_os_error();
        // ERROR_PIPE_CONNECTED (535) means client connected before we called ConnectNamedPipe
        if err.raw_os_error() != Some(535) {
            anyhow::bail!("ConnectNamedPipe failed: {err}");
        }
    }
    Ok(())
}

/// Disconnect client and close pipe handle.
#[cfg(windows)]
fn disconnect_and_close(pipe: &windows::Win32::Foundation::HANDLE) {
    use windows::Win32::System::Pipes::DisconnectNamedPipe;
    use windows::Win32::Foundation::CloseHandle;

    #[expect(unsafe_code, reason = "DisconnectNamedPipe + CloseHandle require unsafe FFI")]
    unsafe {
        let _ = DisconnectNamedPipe(*pipe);
        let _ = CloseHandle(*pipe);
    }
}

// ── D7.4: Client Process Verification ───────────────────────────────────

/// Get the PID of the connected pipe client.
#[cfg(windows)]
fn get_pipe_client_pid(pipe: &windows::Win32::Foundation::HANDLE) -> Option<u32> {
    use windows::Win32::System::Pipes::GetNamedPipeClientProcessId;

    let mut pid: u32 = 0;

    #[expect(unsafe_code, reason = "GetNamedPipeClientProcessId requires unsafe FFI")]
    let ok = unsafe { GetNamedPipeClientProcessId(*pipe, &mut pid) };

    if ok.as_bool() && pid != 0 { Some(pid) } else { None }
}

/// Verify that a client process is a legitimate uffs-daemon.
#[cfg(windows)]
fn verify_client(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    #[expect(unsafe_code, reason = "Win32 process query requires unsafe FFI")]
    let exe_name = unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;

        let mut buf = vec![0u16; 4096];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(handle);

        if !ok.as_bool() || size == 0 {
            return false;
        }
        String::from_utf16_lossy(&buf[..size as usize])
    };

    let name = std::path::Path::new(&exe_name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    name == "uffs-daemon.exe" || name == "uffs-daemon" || name.starts_with("uffs_daemon")
}

// ── D7.5: Handle Brokering ──────────────────────────────────────────────

/// Handle a single pipe request: read drive letter, open volume, DuplicateHandle.
#[cfg(windows)]
fn handle_pipe_request(
    pipe: &windows::Win32::Foundation::HANDLE,
    client_pid: u32,
) -> anyhow::Result<()> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_GENERIC_READ, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_DUP_HANDLE,
    };

    // 1. Read drive letter (1 byte)
    let mut drive_buf = [0u8; 1];
    read_pipe(pipe, &mut drive_buf)?;
    let drive_letter = drive_buf[0] as char;

    if !drive_letter.is_ascii_alphabetic() {
        write_pipe(pipe, &[1u8; 1])?; // error status
        anyhow::bail!("Invalid drive letter: {drive_letter}");
    }

    tracing::info!(drive = %drive_letter, client_pid, "Opening volume for client");

    // 2. Open volume with backup semantics (requires SeBackupPrivilege)
    let volume_path = format!("\\\\.\\{}:", drive_letter);
    let wide_path: Vec<u16> = volume_path.encode_utf16().chain(Some(0)).collect();

    #[expect(unsafe_code, reason = "CreateFileW + DuplicateHandle require unsafe FFI")]
    unsafe {
        let volume_handle = CreateFileW(
            windows::core::PCWSTR(wide_path.as_ptr()),
            FILE_GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        );

        let volume_handle = match volume_handle {
            Ok(h) => h,
            Err(e) => {
                write_pipe(pipe, &[1u8; 1])?; // error
                anyhow::bail!("CreateFileW failed for {volume_path}: {e}");
            }
        };

        // 3. Open client process for handle duplication
        let client_process = match OpenProcess(PROCESS_DUP_HANDLE, false, client_pid) {
            Ok(h) => h,
            Err(e) => {
                let _ = CloseHandle(volume_handle);
                write_pipe(pipe, &[1u8; 1])?;
                anyhow::bail!("OpenProcess for client {client_pid} failed: {e}");
            }
        };

        // 4. Duplicate handle into client process (read-only)
        let mut client_handle = HANDLE::default();
        let dup_ok = windows::Win32::Foundation::DuplicateHandle(
            windows::Win32::System::Threading::GetCurrentProcess(),
            volume_handle,
            client_process,
            &mut client_handle,
            FILE_GENERIC_READ.0,
            false,
            windows::Win32::Foundation::DUPLICATE_HANDLE_OPTIONS(0),
        );

        let _ = CloseHandle(volume_handle);
        let _ = CloseHandle(client_process);

        if !dup_ok.as_bool() {
            write_pipe(pipe, &[1u8; 1])?;
            anyhow::bail!("DuplicateHandle failed");
        }

        // 5. Send success (1 byte) + handle value (8 bytes LE)
        let handle_value = client_handle.0 as u64;
        let mut response = [0u8; 9];
        response[0] = 0; // success
        response[1..9].copy_from_slice(&handle_value.to_le_bytes());
        write_pipe(pipe, &response)?;

        tracing::info!(
            drive = %drive_letter,
            client_pid,
            handle = handle_value,
            "Volume handle brokered successfully"
        );
    }

    Ok(())
}

/// Read exact bytes from the pipe.
#[cfg(windows)]
fn read_pipe(pipe: &windows::Win32::Foundation::HANDLE, buf: &mut [u8]) -> anyhow::Result<()> {
    use windows::Win32::Storage::FileSystem::ReadFile;

    let mut bytes_read = 0u32;

    #[expect(unsafe_code, reason = "ReadFile requires unsafe FFI")]
    let ok = unsafe {
        ReadFile(*pipe, Some(buf), Some(&mut bytes_read), None)
    };

    if !ok.as_bool() {
        anyhow::bail!("ReadFile failed: {}", std::io::Error::last_os_error());
    }
    if (bytes_read as usize) < buf.len() {
        anyhow::bail!("Short read: got {bytes_read}, expected {}", buf.len());
    }
    Ok(())
}

/// Write bytes to the pipe.
#[cfg(windows)]
fn write_pipe(pipe: &windows::Win32::Foundation::HANDLE, buf: &[u8]) -> anyhow::Result<()> {
    use windows::Win32::Storage::FileSystem::WriteFile;

    let mut bytes_written = 0u32;

    #[expect(unsafe_code, reason = "WriteFile requires unsafe FFI")]
    let ok = unsafe {
        WriteFile(*pipe, Some(buf), Some(&mut bytes_written), None)
    };

    if !ok.as_bool() {
        anyhow::bail!("WriteFile failed: {}", std::io::Error::last_os_error());
    }
    Ok(())
}

// ── Elevation Check ─────────────────────────────────────────────────────

#[cfg(windows)]
fn is_elevated() -> bool {
    use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::Win32::Foundation::HANDLE;

    #[expect(unsafe_code, reason = "Win32 token query requires unsafe FFI")]
    unsafe {
        let mut token = HANDLE::default();
        if !OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).as_bool() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut size = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut TOKEN_ELEVATION as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        );
        let _ = windows::Win32::Foundation::CloseHandle(token);
        ok.as_bool() && elevation.TokenIsElevated != 0
    }
}

// ── Non-Windows stub ────────────────────────────────────────────────────

#[cfg(not(windows))]
pub fn run() -> anyhow::Result<()> {
    anyhow::bail!("uffs-broker is a Windows-only component")
}
