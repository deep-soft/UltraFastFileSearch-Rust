// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Windows Service registration + control dispatcher for the access broker.
//!
//! Extracted from `broker.rs` (which sits at the 800-LOC file-size ceiling).
//! `install_service` / `uninstall_service` shell out to `sc`; the FU-1
//! dispatcher (`run_as_service`, `service_main`, the control handler) is the
//! `LocalSystem` entry point the SCM invokes at boot — so the broker runs as a
//! real service without the `--run` terminal.

/// Registered service name — the single source of truth shared with the
/// updater + CLI. Used for `sc create`/`delete` and the `service_main`
/// table entry.
#[cfg(windows)]
use uffs_broker_protocol::SERVICE_NAME;

/// Set by the SCM control handler on STOP / SHUTDOWN; polled by the broker's
/// accept loop ([`stop_requested`]) so it exits between connections.
#[cfg(windows)]
static STOP_REQUESTED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// `SERVICE_STATUS_HANDLE` from `RegisterServiceCtrlHandlerW`, stored so the
/// control handler (a C callback with no user context) can report status.
/// Null until `service_main` registers it.
#[cfg(windows)]
static STATUS_HANDLE: core::sync::atomic::AtomicPtr<core::ffi::c_void> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

/// `true` once a service stop has been requested; the accept loop checks this.
#[cfg(windows)]
pub(super) fn stop_requested() -> bool {
    STOP_REQUESTED.load(core::sync::atomic::Ordering::Relaxed)
}

/// Encode `text` as a NUL-terminated UTF-16 buffer for Win32 wide-string APIs.
#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

/// Combine an `sc.exe` invocation's stdout + stderr for an error message.
///
/// `sc.exe` writes most of its diagnostics to **stdout**, not stderr, so a
/// stderr-only message comes out empty (as seen on a failed `sc create`).
#[cfg(windows)]
pub(super) fn sc_output(output: &std::process::Output) -> String {
    // AUDIT-OK(bytes): operator-facing diagnostic text only — the combined
    // output is formatted into an error message, never parsed or matched.
    let stdout = String::from_utf8_lossy(&output.stdout);
    // AUDIT-OK(bytes): same display-only argument as stdout above.
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{} {}", stdout.trim(), stderr.trim())
        .trim()
        .to_owned()
}

/// Run a blocking step (`body`) while animating a braille spinner after
/// `label`, then clear back to `label` so the caller's `println!("ok")` lands
/// cleanly on the same line. Used for `sc create`, which is instant normally
/// but on an AV box (Norton/Defender deep-scanning the registration of a new
/// auto-start service pointing at an UNSIGNED binary) blocks ~40s — a silent
/// 40s reads as a hang.
#[cfg(windows)]
#[expect(
    clippy::print_stdout,
    reason = "CLI admin command — stdout is the user-visible progress channel"
)]
fn spinner_step<T: Send>(label: &str, body: impl FnOnce() -> T + Send) -> T {
    use std::io::Write as _;

    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    std::thread::scope(|scope| {
        let handle = scope.spawn(body);
        let mut frame = 0_usize;
        while !handle.is_finished() {
            let glyph = FRAMES.get(frame % FRAMES.len()).copied().unwrap_or("*");
            print!("\r{label}{glyph} ");
            let _flushed = std::io::stdout().flush();
            std::thread::sleep(core::time::Duration::from_millis(120));
            frame = frame.wrapping_add(1);
        }
        // Redraw the bare label so the caller's verdict ("ok"/"failed") appends.
        print!("\r{label}");
        let _flushed = std::io::stdout().flush();
        handle.join().unwrap_or_else(|_| std::process::abort())
    })
}

/// Register the broker as an auto-start Windows Service and start it.
///
/// # Why the argv is split the way it is
///
/// `sc create` parses `option= value` where `option=` and the value are
/// **separate command-line tokens** (the space after `=` is the
/// delimiter).  The prior code passed `binPath= "<path>"` as a *single*
/// argument, so the registered `ImagePath` ended up as ` "<path>"` —
/// with a leading space and literal quotes — and the service failed to
/// start with `StartService` error 87 (`ERROR_INVALID_PARAMETER`).  Here
/// `binPath=` and the raw path are distinct argv elements; `std`'s
/// Windows argument quoting wraps the path in quotes only if it contains
/// spaces, producing a valid `ImagePath` in both cases.
#[cfg(windows)]
#[expect(
    clippy::print_stdout,
    reason = "CLI admin command — stdout is the user-visible result channel"
)]
pub(super) fn install_service() -> anyhow::Result<()> {
    if !super::is_elevated() {
        anyhow::bail!(
            "installing the broker service requires Administrator.\n\
             Open an elevated terminal (right-click PowerShell or cmd → \
             \"Run as administrator\") and re-run:\n    uffs-broker --install"
        );
    }

    // Step-by-step narration: the `sc create` below can block ~40s on an AV
    // box (the reason is on that step), and `sc start` waits for the service
    // to warm up (~10s) — either silent wait reads as a hang, so each step
    // spins and says what it is waiting on.
    let exe = std::env::current_exe()?;
    println!("Installing the UFFS Access Broker service...");
    // `sc create` registers an auto-start service pointing at this binary.
    // Instant normally, but security software (Norton / Defender) deep-scans
    // the creation of a new auto-start service on an UNSIGNED binary — measured
    // at ~40s on a Norton box (13ms with it off). So spin, and say why.
    let create = spinner_step(
        "  registering the service (a security scan can take up to a minute)... ",
        || {
            std::process::Command::new("sc.exe")
                .args([
                    "create",
                    SERVICE_NAME,
                    "binPath=",
                    &exe.display().to_string(),
                    "start=",
                    "auto",
                    "DisplayName=",
                    "UFFS Access Broker",
                ])
                .output()
        },
    )?;

    if !create.status.success() {
        println!("failed");
        // AUDIT-OK(bytes): `sc` output surfaced verbatim to the operator —
        // display only, no decision.
        anyhow::bail!(
            "Install failed (sc create): {}\n(If the service already exists, run \
             `uffs-broker --uninstall` first.)",
            sc_output(&create)
        );
    }
    println!("ok");

    // Recovery actions, applied immediately after registration.  Without
    // them the SCM's response to a killed broker is to log event 7034 and
    // do nothing at all — which on 2026-09-04 left the box without a broker
    // for seven hours.  A failure here is reported but NOT fatal: a broker
    // that is installed and running without a restart ladder is strictly
    // better than no broker, and `--repair` can add the ladder later.
    match super::recovery::configure_recovery_actions(SERVICE_NAME) {
        Ok(()) => println!("  restart-on-failure configured (5s / 10s / 60s)"),
        Err(err) => println!("  note: could not set recovery actions: {err:#}"),
    }

    // Start it now so the broker is usable immediately — the whole point
    // is "no future UAC", which only holds once the service is running.
    // `start= auto` also brings it back on every boot.
    // Native SCM start (waits for the service to report RUNNING — the ~10s
    // warmup the plain `sc start` returns before), so the spinner actually
    // covers the wait instead of finishing in 30ms.
    match spinner_step(
        "  starting the service (waiting for it to warm up)... ",
        || uffs_winsvc::start(SERVICE_NAME),
    ) {
        Ok(()) => {
            println!("ok");
            println!(
                "UFFS Access Broker installed and started (auto-start on boot).\n\
                 Non-elevated `uffs` searches will now use the broker for volume \
                 access — no more UAC prompts."
            );
        }
        Err(err) => {
            println!("failed");
            println!(
                "Service installed (auto-start on boot), but starting it failed: \
                 {err:#}\nStart it manually from an elevated shell with:\n    \
                 sc.exe start UffsAccessBroker"
            );
        }
    }
    Ok(())
}

/// Re-apply the SCM restart ladder to an **already installed** service.
///
/// `--install` sets recovery actions at registration time, but a broker
/// installed before that existed has none — which is the state the 2026-09-04
/// incident found it in.  Reinstalling would mean a stop/delete/create cycle
/// (and a window with no broker at all) purely to change two SCM settings, so
/// this applies them in place instead.
///
/// Idempotent: running it against a service that already has the ladder is a
/// no-op success.
///
/// # Errors
///
/// Returns an error if the service is not installed, if the caller is not
/// elevated, or if `sc.exe` rejects either configuration call.
#[cfg(windows)]
#[expect(
    clippy::print_stdout,
    reason = "CLI admin command — stdout is the user-visible result channel"
)]
pub(super) fn repair_service() -> anyhow::Result<()> {
    if !uffs_winsvc::is_installed(SERVICE_NAME) {
        anyhow::bail!(
            "the broker service is not installed — run `uffs-broker --install` \
             from an elevated terminal instead."
        );
    }
    if !super::is_elevated() {
        anyhow::bail!(
            "repairing the broker service requires Administrator.\n\
             Open an elevated terminal and re-run:\n    uffs-broker --repair"
        );
    }
    super::recovery::configure_recovery_actions(SERVICE_NAME)?;
    println!(
        "Restart-on-failure configured for the UFFS Access Broker.\n\
         It will now restart automatically (after 5s, then 10s, then 60s) if it \
         is killed or crashes, instead of staying down until someone notices."
    );
    Ok(())
}

/// Deregister the broker Windows Service via `sc delete`.
///
/// See [`install_service`] for why stdout is the output channel.
#[cfg(windows)]
#[expect(
    clippy::print_stdout,
    reason = "CLI admin command — stdout is the user-visible result channel"
)]
pub(super) fn uninstall_service() -> anyhow::Result<()> {
    // Checking existence is a non-elevated SCM query. If the service is already
    // absent, the requested end state holds — a no-op success, and there is no
    // reason to demand Administrator for work that would not happen.
    if !uffs_winsvc::is_installed(SERVICE_NAME) {
        println!("Broker service is not installed — nothing to remove.");
        return Ok(());
    }
    if !super::is_elevated() {
        anyhow::bail!(
            "removing the broker service requires Administrator.\n\
             Open an elevated terminal and re-run:\n    uffs-broker --uninstall"
        );
    }
    // Stop the service before deleting it.  `sc delete` on a RUNNING service
    // only MARKS it for deletion (deferred until it stops), which then makes a
    // later `--install` fail with a cryptic `sc create` error.  Stopping first
    // makes the delete take effect immediately.  Ignore the stop result — the
    // service may already be stopped or not exist.
    let _stopped = std::process::Command::new("sc.exe")
        .args(["stop", SERVICE_NAME])
        .output();
    let output = std::process::Command::new("sc.exe")
        .args(["delete", SERVICE_NAME])
        .output()?;

    if output.status.success() {
        println!("Service uninstalled.");
        return Ok(());
    }

    // The service already not existing IS the requested end state, so treat it
    // as a no-op success rather than a loud failure. `sc delete` on a missing
    // service returns ERROR_SERVICE_DOES_NOT_EXIST (1060).
    if service_already_absent(&output) {
        println!("Broker service is not installed — nothing to remove.");
        return Ok(());
    }

    // AUDIT-OK(bytes): `sc` command output surfaced verbatim in an error
    // message for the operator — display only, no decision.
    anyhow::bail!("Uninstall failed: {}", sc_output(&output));
}

/// Whether an `sc delete` failure is just "the service does not exist"
/// (`ERROR_SERVICE_DOES_NOT_EXIST`, 1060) — i.e. already uninstalled, so the
/// uninstall request is already satisfied.
fn service_already_absent(output: &std::process::Output) -> bool {
    // AUDIT-OK(bytes): the `sc` output is inspected only to classify the "already
    // gone" case; the exit code is the primary signal, the text a fallback.
    absent_service_signal(output.status.code(), &sc_output(output))
}

/// Pure classifier: `sc delete` reports a missing service as
/// `ERROR_SERVICE_DOES_NOT_EXIST` (1060) — via the process exit code (primary)
/// or its text (fallback). Split out so the "already gone → success" decision
/// is unit-testable without spawning `sc`.
fn absent_service_signal(exit_code: Option<i32>, sc_text: &str) -> bool {
    exit_code == Some(1060) || sc_text.contains("1060")
}

// ── FU-1: Windows Service control dispatcher ────────────────────────────────

/// Hand control to the SCM via `StartServiceCtrlDispatcherW`; blocks until the
/// service stops.
///
/// When the binary is launched **interactively** (no SCM), the dispatcher fails
/// with `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT` (1063) — in that case we
/// print usage, so a bare `uffs-broker` is still helpful instead of hanging.
#[cfg(windows)]
#[expect(unsafe_code, reason = "FFI: StartServiceCtrlDispatcherW")]
pub(super) fn run_as_service() -> anyhow::Result<()> {
    use windows::Win32::Foundation::ERROR_FAILED_SERVICE_CONTROLLER_CONNECT;
    use windows::Win32::System::Services::{SERVICE_TABLE_ENTRYW, StartServiceCtrlDispatcherW};
    use windows::core::PWSTR;

    let mut name = wide(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(name.as_mut_ptr()),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(), // NULL terminator
    ];

    // SAFETY: `table` is NULL-terminated and, with `name`, outlives the
    // (blocking) dispatcher call.
    match unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } {
        Ok(()) => Ok(()),
        Err(err) if err.code() == ERROR_FAILED_SERVICE_CONTROLLER_CONNECT.to_hresult() => {
            // Not launched by the SCM → interactive invocation.
            super::print_usage();
            Ok(())
        }
        Err(err) => Err(anyhow::anyhow!("StartServiceCtrlDispatcherW failed: {err}")),
    }
}

/// SCM service entry point: register the control handler, report RUNNING, serve
/// until a stop is requested, then report STOPPED.
#[cfg(windows)]
#[expect(unsafe_code, reason = "FFI: RegisterServiceCtrlHandlerW")]
extern "system" fn service_main(_argc: u32, _argv: *mut windows::core::PWSTR) {
    use core::sync::atomic::Ordering;

    use windows::Win32::System::Services::{
        RegisterServiceCtrlHandlerW, SERVICE_RUNNING, SERVICE_START_PENDING,
    };
    use windows::core::PCWSTR;

    let name = wide(SERVICE_NAME);
    // SAFETY: `name` is a NUL-terminated wide string valid for the call.
    let Ok(handle) =
        (unsafe { RegisterServiceCtrlHandlerW(PCWSTR(name.as_ptr()), Some(service_ctrl_handler)) })
    else {
        return;
    };
    STATUS_HANDLE.store(handle.0, Ordering::Relaxed);

    report_status(SERVICE_START_PENDING, 0);
    // File logging, not stdout: a service has no console, so the previous
    // `super::init_tracing()` wrote every line into the void.  When the
    // broker was killed on 2026-09-04 its own side of the story did not
    // exist — the whole timeline had to be reconstructed from the Windows
    // Security log.  Hold the guard until `service_main` returns so the
    // non-blocking writer is flushed on the way out.
    let _log_guard = init_service_tracing();
    tracing::info!(
        pid = std::process::id(),
        version = %uffs_version::version_short!("uffs-broker"),
        "uffs-broker starting (service mode)"
    );
    report_status(SERVICE_RUNNING, accepted_controls());

    // A serve loop that returns `Err` used to be logged and then reported as
    // a CLEAN stop, which tells the SCM the service ended on purpose — so no
    // recovery action ran and the broker stayed down until a human noticed.
    // Report the failure as a service-specific error instead; together with
    // the failure flag set by `recovery::configure_recovery_actions`, that is
    // what actually gets the broker restarted.
    let serve_result = super::serve_pipe_requests();
    let failed = match serve_result {
        Ok(()) => false,
        Err(err) => {
            tracing::error!(error = %err, "broker serve loop exited with error");
            true
        }
    };
    tracing::info!(failed, "uffs-broker stopped (service mode)");
    report_stopped(failed);
}

/// Initialise tracing for the SCM-dispatched service, writing to
/// `<log-dir>/uffs-broker.log`.
///
/// Mirrors the daemon's `log_init`, with one deliberate difference: the
/// broker rotates **daily and keeps a bounded number of files**.  `uffsd`
/// uses `rolling::never` and one production log reached 305 MB; a service
/// that nobody ever looks at until an incident must not be the thing that
/// fills the disk.
///
/// Returns the writer guard, which the caller must hold for the lifetime of
/// the service — dropping it flushes buffered lines.  Returns `None` when the
/// log directory cannot be created, in which case the broker degrades to a
/// stdout subscriber (harmless, just invisible) rather than failing to start.
#[cfg(windows)]
fn init_service_tracing() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    /// Log files kept by the daily rotation before the oldest is pruned.
    const KEPT_LOG_FILES: usize = 7;

    let dir = uffs_security::log_dir::log_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        // No console to complain to; fall back to a stdout subscriber so a
        // foreground/debug invocation still shows something.
        let fallback = tracing_subscriber::fmt()
            .with_target(false)
            .with_max_level(tracing::Level::INFO)
            .try_init();
        drop(fallback);
        return None;
    }
    let built = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("uffs-broker")
        .filename_suffix("log")
        .max_log_files(KEPT_LOG_FILES)
        .build(&dir);
    let Ok(appender) = built else {
        let fallback = tracing_subscriber::fmt()
            .with_target(false)
            .with_max_level(tracing::Level::INFO)
            .try_init();
        drop(fallback);
        return None;
    };
    let (non_blocking, guard) = tracing_appender::non_blocking(appender);
    let init_result = tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(non_blocking)
        .try_init();
    drop(init_result);
    Some(guard)
}

/// Report `SERVICE_STOPPED`, carrying a service-specific error when the serve
/// loop failed so the SCM's recovery actions apply.
///
/// `ERROR_SERVICE_SPECIFIC_ERROR` (1066) in `dwWin32ExitCode` is the
/// documented way to say "this service failed for its own reason"; the detail
/// goes in `dwServiceSpecificExitCode`.
#[cfg(windows)]
#[expect(unsafe_code, reason = "FFI: SetServiceStatus")]
fn report_stopped(failed: bool) {
    use core::sync::atomic::Ordering;

    use windows::Win32::System::Services::{
        SERVICE_STATUS, SERVICE_STATUS_HANDLE, SERVICE_STOPPED, SERVICE_WIN32_OWN_PROCESS,
        SetServiceStatus,
    };

    /// `ERROR_SERVICE_SPECIFIC_ERROR` — "the service has returned a
    /// service-specific error code".
    const ERROR_SERVICE_SPECIFIC_ERROR: u32 = 1066;
    /// Our one service-specific code: the serve loop exited with an error.
    const SERVE_LOOP_FAILED: u32 = 1;

    let raw = STATUS_HANDLE.load(Ordering::Relaxed);
    if raw.is_null() {
        return;
    }
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: SERVICE_STOPPED,
        dwControlsAccepted: 0,
        dwWin32ExitCode: if failed {
            ERROR_SERVICE_SPECIFIC_ERROR
        } else {
            0
        },
        dwServiceSpecificExitCode: if failed { SERVE_LOOP_FAILED } else { 0 },
        dwCheckPoint: 0,
        dwWaitHint: 0,
    };
    // SAFETY: `raw` is the handle stored by `service_main`; `status` is fully
    // initialised and valid for the duration of the call.
    if let Err(err) = unsafe { SetServiceStatus(SERVICE_STATUS_HANDLE(raw), &raw const status) } {
        tracing::debug!(err = ?err, "SetServiceStatus(STOPPED) failed");
    }
}

/// SCM control handler: on STOP / SHUTDOWN flag the accept loop to exit and
/// nudge it awake (it blocks in `ConnectNamedPipe`).
#[cfg(windows)]
extern "system" fn service_ctrl_handler(control: u32) {
    use core::sync::atomic::Ordering;

    use windows::Win32::System::Services::{
        SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP,
        SERVICE_RUNNING, SERVICE_STOP_PENDING,
    };

    if control == SERVICE_CONTROL_STOP || control == SERVICE_CONTROL_SHUTDOWN {
        STOP_REQUESTED.store(true, Ordering::Relaxed);
        report_status(SERVICE_STOP_PENDING, 0);
        signal_stop();
    } else if control == SERVICE_CONTROL_INTERROGATE {
        report_status(SERVICE_RUNNING, accepted_controls());
    }
}

/// Controls the broker accepts while RUNNING (operator STOP + system SHUTDOWN).
#[cfg(windows)]
const fn accepted_controls() -> u32 {
    use windows::Win32::System::Services::{SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP};
    SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
}

/// Report a service-status transition to the SCM (no-op before registration).
#[cfg(windows)]
#[expect(unsafe_code, reason = "FFI: SetServiceStatus")]
fn report_status(
    state: windows::Win32::System::Services::SERVICE_STATUS_CURRENT_STATE,
    controls_accepted: u32,
) {
    use core::sync::atomic::Ordering;

    use windows::Win32::System::Services::{
        SERVICE_STATUS, SERVICE_STATUS_HANDLE, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus,
    };

    let raw = STATUS_HANDLE.load(Ordering::Relaxed);
    if raw.is_null() {
        return;
    }
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: controls_accepted,
        dwWin32ExitCode: 0,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: 0,
    };
    // SAFETY: `raw` is the handle stored by `service_main`; `status` is fully
    // initialised and valid for the duration of the call.
    if let Err(err) = unsafe { SetServiceStatus(SERVICE_STATUS_HANDLE(raw), &raw const status) } {
        tracing::debug!(err = ?err, "SetServiceStatus failed");
    }
}

/// Wake the accept loop's blocking `ConnectNamedPipe` by opening the pipe as a
/// throwaway client, so a stop takes effect promptly.  The loop then sees
/// [`stop_requested`] and exits without serving this connection.
#[cfg(windows)]
#[expect(unsafe_code, reason = "FFI: CreateFileW dummy connect + CloseHandle")]
fn signal_stop() {
    use uffs_broker_protocol::PIPE_NAME;
    use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE, OPEN_EXISTING,
    };
    use windows::core::PCWSTR;

    let name = wide(PIPE_NAME);
    // SAFETY: `name` is a NUL-terminated wide path valid for the call; any
    // returned handle is closed immediately below.
    let opened = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_MODE(0),
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    };
    if let Ok(handle) = opened {
        // SAFETY: `handle` is a freshly opened, valid handle owned here.
        if let Err(err) = unsafe { CloseHandle(handle) } {
            tracing::debug!(err = ?err, "CloseHandle failed for stop-signal pipe");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::absent_service_signal;

    #[test]
    fn absent_when_sc_reports_1060_by_code_or_text() {
        // Exit code is the primary signal.
        assert!(absent_service_signal(Some(1060_i32), ""));
        // Text is the fallback when the code is not surfaced.
        assert!(absent_service_signal(
            None,
            "[SC] OpenService FAILED 1060:\n\nThe specified service does not exist"
        ));
    }

    #[test]
    fn other_failures_are_not_treated_as_absent() {
        // Access denied (5) is a real failure, not "already gone".
        assert!(!absent_service_signal(
            Some(5_i32),
            "[SC] DeleteService FAILED 5:\n\nAccess is denied."
        ));
        assert!(!absent_service_signal(None, "some unrelated error"));
    }
}
