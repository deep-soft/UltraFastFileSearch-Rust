// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! MCP stdio session discovery for `uffs --status`.
//!
//! Enumerates running `uffs --mcp run` processes via `ps`, under a watchdog
//! timeout so a process with a huge argument vector can never hang `--status`.

use super::{Glyph, Palette, section, status_row};

// ── MCP Stdio Sessions ───────────────────────────────────────────────────────

/// Print the MCP stdio session list (running `uffs --mcp run` processes).
#[expect(clippy::print_stdout, reason = "CLI user-facing output")]
pub(super) fn print_mcp_stdio_section(palette: Palette) {
    println!("{}", section(palette, "MCP Stdio Sessions"));
    let Some(sessions) = find_mcp_stdio_processes() else {
        println!("  {}", palette.dim("(process scan timed out — skipped)"));
        return;
    };
    if sessions.is_empty() {
        println!("  {}", palette.dim("(none)"));
        return;
    }
    let mut any_stale = false;
    for (idx, session) in sessions.iter().enumerate() {
        let parent = session
            .parent_name
            .as_deref()
            .map_or(String::new(), |name| format!("  (parent: {name})"));
        let glyph = if session.is_stale {
            any_stale = true;
            Glyph::Warn
        } else {
            Glyph::Up
        };
        let name = format!("{}. PID {}", idx + 1, session.pid);
        let detail = format!(
            "uptime {}{parent}",
            uffs_client::format::format_duration(session.uptime)
        );
        println!("{}", status_row(palette, glyph, &name, &detail));
    }
    if any_stale {
        println!(
            "  {}",
            palette.dim(
                "Serving a superseded binary (renamed aside by an installer, or older \
                 than the one on disk). Still functional; the AI host that launched \
                 them picks up the new one on its next start."
            )
        );
    }
}

/// JSON for the MCP stdio session list.
pub(super) fn mcp_stdio_json() -> serde_json::Value {
    let sessions: Vec<serde_json::Value> = find_mcp_stdio_processes()
        .unwrap_or_default()
        .iter()
        .map(|session| {
            serde_json::json!({
                "pid": session.pid,
                "uptime_secs": session.uptime.as_secs(),
                "parent": session.parent_name,
                "stale": session.is_stale,
            })
        })
        .collect();
    serde_json::Value::Array(sessions)
}

/// Information about a running MCP stdio process.
struct StdioSession {
    /// Process ID.
    pid: u32,
    /// How long the process has been running.
    uptime: core::time::Duration,
    /// Name of the parent process (the AI host), if available.
    parent_name: Option<String>,
    /// True if the process's binary is older than the current binary.
    is_stale: bool,
}

/// Max time to wait for a `ps` invocation before abandoning it.
const PS_TIMEOUT: core::time::Duration = core::time::Duration::from_secs(2);

/// Run `program args…`, capturing stdout, but abandon it — killing the child —
/// if it exceeds `timeout`. A `ps` reading a process with a huge argument
/// vector (or one stuck in uninterruptible I/O) can block for a long time, and
/// `--status` must never hang on it. Returns `None` on spawn failure or
/// timeout.
fn capture_with_timeout(
    program: &str,
    args: &[&str],
    timeout: core::time::Duration,
) -> Option<Vec<u8>> {
    let child = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let pid = child.id();

    // Read the child on a helper thread so the main thread can enforce a
    // deadline via `recv_timeout`.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(tx.send(child.wait_with_output()));
    });

    if let Ok(Ok(output)) = rx.recv_timeout(timeout) {
        Some(output.stdout)
    } else {
        // Timed out (or the child errored): terminate the process so it can't
        // linger, and report the scan as unavailable rather than blocking.
        drop(
            std::process::Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status(),
        );
        None
    }
}

/// Find running `uffs --mcp run` processes via `ps`, flagging stale binaries.
///
/// The `ps` calls run under [`PS_TIMEOUT`] (see [`capture_with_timeout`]), so a
/// process with a huge argument vector can never hang `--status`. Returns
/// `None` when the scan could not complete (spawn failed or timed out).
#[cfg(windows)]
fn find_mcp_stdio_processes() -> Option<Vec<StdioSession>> {
    // Windows has no `ps`, so the Unix path below returned nothing and the
    // section rendered "(none)" — indistinguishable from "no sessions" on
    // the one platform UFFS actually ships to.  It reported "(none)" on a
    // box that was demonstrably serving a session.
    //
    // `Get-CimInstance Win32_Process` is the supported enumeration; CSV
    // keeps parsing trivial.  Matching on the image name rather than the
    // command line matters twice over: AI hosts spawn the supervisor as a
    // bare `uffsmcp` with no arguments (so a `--mcp run` cmdline match
    // finds nothing), and after an installer rename-swap a surviving
    // supervisor runs as `uffsmcp.old0` — still serving, and still worth
    // showing.
    let script = "Get-CimInstance Win32_Process -Filter \"Name LIKE 'uffsmcp%'\" | \
         ForEach-Object { \
           $s = $_.CreationDate; \
           $age = if ($s) { [int]((Get-Date) - $s).TotalSeconds } else { 0 }; \
           \"$($_.ProcessId),$($_.ParentProcessId),$age,$($_.Name)\" }";
    let stdout = capture_with_timeout(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", script],
        PS_TIMEOUT,
    )?;
    let text = String::from_utf8_lossy(&stdout);
    let my_pid = std::process::id();
    let current_mtime = std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok());
    // The HTTP gateway is a `uffsmcp` process too, so an image-name
    // filter catches it — but it has its own section above, and listing
    // it here as a "stdio session" is simply wrong.
    let gateway_pid = uffs_client::mcp_pid::is_mcp_server_running();
    // Collect PIDs first so a supervisor's worker child can be told
    // apart from a supervisor: the worker's parent is another `uffsmcp`.
    let rows: Vec<(u32, u32, u64)> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.trim().split(',');
            let pid = fields.next()?.trim().parse::<u32>().ok()?;
            let parent = fields.next()?.trim().parse::<u32>().unwrap_or(0);
            let age = fields.next()?.trim().parse::<u64>().unwrap_or(0);
            Some((pid, parent, age))
        })
        .collect();
    let is_supervisor_pid = |pid: u32| rows.iter().any(|&(other, _, _)| other == pid);

    let mut sessions = Vec::new();
    for line in text.lines() {
        let mut fields = line.trim().split(',');
        let Some(pid) = fields.next().and_then(|val| val.trim().parse::<u32>().ok()) else {
            continue;
        };
        if pid == my_pid || gateway_pid == Some(pid) {
            continue;
        }
        let parent_pid: u32 = fields
            .next()
            .and_then(|val| val.trim().parse().ok())
            .unwrap_or(0);
        let age_secs: u64 = fields
            .next()
            .and_then(|val| val.trim().parse().ok())
            .unwrap_or(0);
        let _image = fields.next().unwrap_or("");
        // A `uffsmcp` whose parent is another `uffsmcp` is a supervisor's
        // worker child, not a session of its own.  Worth showing — a
        // worker younger than its supervisor is the visible proof that a
        // hot-swap happened — but labelled, not counted as a session.
        let is_worker = is_supervisor_pid(parent_pid);
        let parent_name = if is_worker {
            Some(format!("worker of PID {parent_pid}"))
        } else {
            resolve_parent_name(parent_pid)
        };
        // Staleness by *age*, not by image name: Windows keeps reporting
        // the original name after a rename-swap, so the name says nothing.
        // A supervisor older than the installed binary is serving a
        // superseded image — harmless once it has hot-swapped its worker,
        // which is exactly why the worker is listed too.
        let is_stale = !is_worker
            && current_mtime.is_some_and(|bin_mtime| {
                std::time::SystemTime::now()
                    .checked_sub(core::time::Duration::from_secs(age_secs))
                    .is_some_and(|started| started < bin_mtime)
            });
        sessions.push(StdioSession {
            pid,
            uptime: core::time::Duration::from_secs(age_secs),
            parent_name,
            is_stale,
        });
    }
    Some(sessions)
}

/// Unix: enumerate via `ps`, matching the `uffs --mcp run` command line.
#[cfg(not(windows))]
fn find_mcp_stdio_processes() -> Option<Vec<StdioSession>> {
    let stdout = capture_with_timeout("ps", &["-eo", "pid,ppid,etime,args"], PS_TIMEOUT)?;

    // AUDIT-OK(bytes): per-line PID scan of process-list output; each line's
    // PID parses-or-skips (fail-safe). Whole-buffer strict decode would drop
    // the whole list on one bad byte. (WI-4.3 follow-up)
    let text = String::from_utf8_lossy(&stdout);
    let my_pid = std::process::id();
    let current_mtime = std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::metadata(path).ok())
        .and_then(|meta| meta.modified().ok());

    let mut sessions = Vec::new();
    for line in text.lines().skip(1) {
        if let Some(session) = parse_stdio_line(line, my_pid, current_mtime) {
            sessions.push(session);
        }
    }
    Some(sessions)
}

/// Parse one `ps` line into a [`StdioSession`] if it is an MCP stdio process.
#[cfg(not(windows))]
fn parse_stdio_line(
    line: &str,
    my_pid: u32,
    current_mtime: Option<std::time::SystemTime>,
) -> Option<StdioSession> {
    let mut fields = line.split_whitespace();
    let proc_pid = fields.next()?.parse::<u32>().ok()?;
    if proc_pid == my_pid {
        return None;
    }
    let parent_pid: u32 = fields.next().and_then(|val| val.parse().ok()).unwrap_or(0);
    let elapsed_time = fields.next()?;
    let cmdline: String = fields.collect::<Vec<_>>().join(" ");

    // Match the actual `uffs --mcp run` invocation. Requiring the `--mcp` flag
    // (not a bare "mcp" substring) excludes build tooling — cargo / sccache /
    // rustc compiling the `uffs-mcp` crate carry "uffs-mcp" but never "--mcp".
    if !cmdline.contains("--mcp") || !cmdline.contains("run") {
        return None;
    }
    if cmdline.contains("serve") || cmdline.contains("start") || cmdline.contains("kill") {
        return None;
    }

    let uptime = parse_ps_etime(elapsed_time);
    let is_stale = current_mtime.is_some_and(|bin_mtime| {
        let proc_started = std::time::SystemTime::now() - uptime;
        proc_started < bin_mtime
    });
    Some(StdioSession {
        pid: proc_pid,
        uptime,
        parent_name: resolve_parent_name(parent_pid),
        is_stale,
    })
}

/// Parse `ps` elapsed time format: `[[dd-]hh:]mm:ss`.
#[cfg(not(windows))]
fn parse_ps_etime(etime: &str) -> core::time::Duration {
    let mut total_secs: u64 = 0;
    let (days_part, time_part) = if let Some((days, rest)) = etime.split_once('-') {
        (days.parse::<u64>().unwrap_or(0), rest)
    } else {
        (0, etime)
    };
    total_secs += days_part * 86400;
    let mut parts = time_part.rsplit(':');
    if let Some(sec) = parts.next() {
        total_secs += sec.parse::<u64>().unwrap_or(0);
    }
    if let Some(min) = parts.next() {
        total_secs += min.parse::<u64>().unwrap_or(0) * 60;
    }
    if let Some(hour) = parts.next() {
        total_secs += hour.parse::<u64>().unwrap_or(0) * 3600;
    }
    core::time::Duration::from_secs(total_secs)
}

/// Resolve the name of a parent process by PID.
fn resolve_parent_name(ppid: u32) -> Option<String> {
    if ppid == 0 {
        return None;
    }
    let ppid_str = ppid.to_string();
    // Windows: `ps` does not exist, so this silently returned `None` and
    // every session rendered without the "(parent: …)" tag that tells you
    // WHICH host owns it — the most useful field in the list.
    #[cfg(windows)]
    let stdout = {
        let script =
            format!("(Get-CimInstance Win32_Process -Filter 'ProcessId = {ppid_str}').Name");
        capture_with_timeout(
            "powershell",
            &["-NoProfile", "-NonInteractive", "-Command", &script],
            PS_TIMEOUT,
        )?
    };
    #[cfg(not(windows))]
    let stdout = capture_with_timeout("ps", &["-p", &ppid_str, "-o", "comm="], PS_TIMEOUT)?;
    // Strict decode: this process name is returned and used for a
    // comparison/targeting decision, so invalid UTF-8 fails closed (None)
    // rather than yielding a U+FFFD-mangled name. (WI-4.3 follow-up)
    let name = core::str::from_utf8(&stdout).ok()?.trim().to_owned();
    if name.is_empty() {
        return None;
    }
    let short = name.rsplit('/').next().unwrap_or(&name).to_owned();
    Some(short)
}
