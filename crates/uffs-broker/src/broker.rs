//! Windows Access Broker implementation.
//!
//! Runs as a Windows Service (or foreground process for debugging).
//! Listens on a named pipe, verifies client identity, and provides
//! read-only volume handles for MFT access.
//!
//! # Architecture
//!
//! ```text
//! uffs-daemon (normal user)
//!     │ named pipe request: "open C:"
//!     ▼
//! uffs-broker (elevated / Windows Service)
//!     │ verify client PID → exe path → Authenticode
//!     │ open volume with SeBackupPrivilege
//!     ▼
//! return read-only HANDLE via DuplicateHandle
//! ```

/// Pipe name for broker communication.
#[cfg(windows)]
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

    // Default: try to run as Windows Service
    eprintln!("uffs-broker: use --install, --uninstall, or --run");
    eprintln!("  --install     Install as Windows Service");
    eprintln!("  --uninstall   Remove Windows Service");
    eprintln!("  --run         Run in foreground (debugging)");
    Ok(())
}

/// Run the broker in foreground mode (for development/debugging).
#[cfg(windows)]
fn run_foreground() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .init();

    tracing::info!(pid = std::process::id(), "uffs-broker starting (foreground mode)");

    // Check if we're running elevated
    if !is_elevated() {
        tracing::warn!("Broker is NOT running elevated — volume access will fail");
        tracing::warn!("Run as Administrator or install as a Windows Service");
    }

    // Create named pipe and serve requests
    serve_pipe_requests()?;

    tracing::info!("uffs-broker stopped");
    Ok(())
}

/// Serve handle requests on the named pipe.
#[cfg(windows)]
fn serve_pipe_requests() -> anyhow::Result<()> {
    use std::io::{BufRead, BufReader, Write};

    tracing::info!(pipe = BROKER_PIPE_NAME, "Listening for handle requests");

    // Simple synchronous loop for now — broker handles are rare (one per drive load)
    loop {
        // Create a new pipe instance for each connection
        let pipe = create_broker_pipe()?;

        // Wait for a client to connect
        connect_pipe(&pipe)?;

        tracing::debug!("Client connected to broker pipe");

        // Verify client identity
        let client_pid = get_pipe_client_pid(&pipe);
        if let Some(pid) = client_pid {
            if !verify_client(pid) {
                tracing::warn!(pid, "Rejected broker client — identity verification failed");
                let _ = disconnect_pipe(&pipe);
                continue;
            }
        }

        // Handle the request (read drive letter, return handle)
        if let Err(e) = handle_pipe_request(&pipe) {
            tracing::debug!(error = %e, "Pipe request failed");
        }

        let _ = disconnect_pipe(&pipe);
    }
}

// ── Windows Service Install/Uninstall ───────────────────────────────────

/// Install the broker as a Windows Service.
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
        println!("Service installed successfully.");
        println!("Start with: sc start UffsAccessBroker");
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to install service: {stderr}");
    }
    Ok(())
}

/// Uninstall the broker Windows Service.
#[cfg(windows)]
fn uninstall_service() -> anyhow::Result<()> {
    let output = std::process::Command::new("sc")
        .args(["delete", "UffsAccessBroker"])
        .output()?;

    if output.status.success() {
        println!("Service uninstalled successfully.");
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to uninstall service: {stderr}");
    }
    Ok(())
}

// ── Pipe Operations ─────────────────────────────────────────────────────

/// Placeholder: create a named pipe with Administrators-only DACL.
#[cfg(windows)]
fn create_broker_pipe() -> anyhow::Result<std::os::windows::io::RawHandle> {
    // TODO: CreateNamedPipeW with proper security descriptor
    // For now, return an error since we can't implement this on macOS
    anyhow::bail!("Named pipe creation requires Windows runtime")
}

/// Placeholder: wait for pipe client connection.
#[cfg(windows)]
fn connect_pipe(_pipe: &std::os::windows::io::RawHandle) -> anyhow::Result<()> {
    anyhow::bail!("Pipe connection requires Windows runtime")
}

/// Placeholder: disconnect pipe client.
#[cfg(windows)]
fn disconnect_pipe(_pipe: &std::os::windows::io::RawHandle) -> anyhow::Result<()> {
    Ok(())
}

/// Placeholder: get the PID of the connected pipe client.
#[cfg(windows)]
fn get_pipe_client_pid(_pipe: &std::os::windows::io::RawHandle) -> Option<u32> {
    // TODO: GetNamedPipeClientProcessId
    None
}

/// Verify that a client process is a legitimate uffs-daemon.
#[cfg(windows)]
fn verify_client(pid: u32) -> bool {
    // Reuse the same verification logic as the client
    // 1. Get exe path via QueryFullProcessImageNameW
    // 2. Check it's uffs-daemon
    // 3. Optionally verify Authenticode signature
    tracing::debug!(pid, "Verifying broker client identity");

    // For now, accept all clients (proper verification in S5)
    true
}

/// Handle a single pipe request: read drive letter, open volume, return handle.
#[cfg(windows)]
fn handle_pipe_request(_pipe: &std::os::windows::io::RawHandle) -> anyhow::Result<()> {
    // Protocol:
    // 1. Read: single byte = drive letter (e.g., 'C')
    // 2. Open: \\.\C: with FILE_READ_DATA + SeBackupPrivilege
    // 3. DuplicateHandle into the client process
    // 4. Write: 8 bytes = HANDLE value for the client
    anyhow::bail!("Handle brokering requires Windows runtime")
}

// ── Elevation Check ─────────────────────────────────────────────────────

/// Check if the current process is running elevated (Administrator).
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

/// Non-Windows: broker is not supported.
#[cfg(not(windows))]
pub fn run() -> anyhow::Result<()> {
    anyhow::bail!("uffs-broker is a Windows-only component")
}
