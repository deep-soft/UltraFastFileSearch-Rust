#!/usr/bin/env rust-script
//! Everything Capacity Probe — discover per-drive IPC/OOM limits of Everything (es.exe).
//!
//! Run on Windows (elevated recommended) from the repo root:
//!   rust-script scripts/windows/everything_capacity_probe.rs
//!
//! What it does:
//!   1. Locates Everything.exe and es.exe on disk.
//!   2. Reads Everything.ini to discover configured NTFS volumes.
//!   3. For each drive, isolates it in the ini and restarts Everything.
//!   4. Runs es.exe with progressively heavier column sets (levels 0–7).
//!   5. Detects crashes / OOM / IPC overflow via exit-code + process liveness.
//!   6. Writes a timestamped log to  scripts/windows/everything_probe_<ts>.log
//!
//! Column levels (cumulative):
//!   0  -get-result-count          (count only, no IPC payload)
//!   1  <default output>           (filenames via IPC)
//!   2  -size                      (+ size)
//!   3  -dm                        (+ date-modified)
//!   4  -size -dm                  (size + date-modified)
//!   5  -size -dm -dc              (+ date-created)
//!   6  -size -dm -dc -da          (+ date-accessed)
//!   7  -size -dm -dc -da -a       (+ attributes — full payload)

use std::env;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Column level definitions
// ---------------------------------------------------------------------------

/// Each level is (label, extra args for es.exe).  Level 0 is special (count-only).
const LEVELS: &[(&str, &[&str])] = &[
    ("L0: count-only",            &["-get-result-count"]),
    ("L1: names",                 &[]),
    ("L2: +size",                 &["-size"]),
    ("L3: +date-modified",        &["-dm"]),
    ("L4: size+dm",               &["-size", "-dm"]),
    ("L5: size+dm+dc",            &["-size", "-dm", "-dc"]),
    ("L6: size+dm+dc+da",         &["-size", "-dm", "-dc", "-da"]),
    ("L7: size+dm+dc+da+attribs", &["-size", "-dm", "-dc", "-da", "-a"]),
];

const EVERYTHING_TIMEOUT: Duration = Duration::from_secs(60);

/// How long we let es.exe run before declaring it hung (OOM dialog on the main app).
/// Successful queries on large drives can take 30–90s, so give generous headroom.
/// Level 0 (count-only) should be near-instant, so it gets a shorter timeout.
const ES_TIMEOUT_COUNT: Duration = Duration::from_secs(30);
const ES_TIMEOUT_QUERY: Duration = Duration::from_secs(180);

// ---------------------------------------------------------------------------
// Locators
// ---------------------------------------------------------------------------

fn find_exe(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|p| p.exists()).cloned()
}

fn everything_exe() -> Option<PathBuf> {
    let home = env::var("USERPROFILE").unwrap_or_default();
    let pf = env::var("ProgramFiles").unwrap_or_default();
    let pf86 = env::var("ProgramFiles(x86)").unwrap_or_default();
    find_exe(&[
        PathBuf::from(&pf).join("Everything").join("Everything.exe"),
        PathBuf::from(&pf86).join("Everything").join("Everything.exe"),
        PathBuf::from(&home).join("bin").join("Everything.exe"),
    ])
}

fn es_exe() -> Option<PathBuf> {
    let home = env::var("USERPROFILE").unwrap_or_default();
    let pf = env::var("ProgramFiles").unwrap_or_default();
    let pf86 = env::var("ProgramFiles(x86)").unwrap_or_default();
    find_exe(&[
        PathBuf::from(&home).join("bin").join("es.exe"),
        PathBuf::from(&pf).join("Everything").join("es.exe"),
        PathBuf::from(&pf86).join("Everything").join("es.exe"),
    ])
}

fn everything_ini_path() -> PathBuf {
    let appdata = env::var("APPDATA").unwrap_or_default();
    PathBuf::from(appdata).join("Everything").join("Everything.ini")
}

// ---------------------------------------------------------------------------
// INI helpers
// ---------------------------------------------------------------------------

/// Parse ntfs_volume_paths from Everything.ini → vec of drive letters.
fn parse_drives_from_ini(ini: &str) -> Vec<char> {
    for line in ini.lines() {
        if let Some(rest) = line.strip_prefix("ntfs_volume_paths=") {
            return rest
                .split(',')
                .filter_map(|s| {
                    let s = s.trim().trim_matches('"');
                    s.chars().next().filter(|c| c.is_ascii_alphabetic())
                })
                .map(|c| c.to_ascii_uppercase())
                .collect();
        }
    }
    Vec::new()
}

/// Rewrite ini so only `target_drive` is included.
fn isolate_drive_in_ini(ini_path: &Path, target_drive: char, all_drives: &[char]) {
    let mut content = fs::read_to_string(ini_path).unwrap_or_default();

    // Build includes mask: 1 for target, 0 for others
    let includes: String = all_drives
        .iter()
        .map(|d| if d.eq_ignore_ascii_case(&target_drive) { "1" } else { "0" })
        .collect::<Vec<_>>()
        .join(",");

    let replacements = [
        ("ntfs_volume_includes=", includes.as_str()),
        ("auto_include_fixed_volumes=", "0"),
        ("auto_include_removable_volumes=", "0"),
    ];
    for (key, val) in &replacements {
        if let Some(pos) = content.find(key) {
            let line_end = content[pos..].find('\n').map_or(content.len(), |i| pos + i);
            let replacement = format!("{key}{val}");
            content.replace_range(pos..line_end, &replacement);
        }
    }
    fs::write(ini_path, &content).ok();
}

fn restore_ini(ini_path: &Path, content: &str) {
    fs::write(ini_path, content).ok();
}

// ---------------------------------------------------------------------------
// Process helpers
// ---------------------------------------------------------------------------

fn kill_everything() {
    Command::new("taskkill")
        .args(["/F", "/IM", "Everything.exe"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok();
    std::thread::sleep(Duration::from_secs(2));
}

fn start_everything(exe: &Path) {
    Command::new(exe)
        .args(["-startup", "-minimized"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok();
}

/// Wait until es.exe -get-result-count returns > 0 (index ready).
fn wait_for_index(es: &Path) -> Option<u64> {
    let deadline = Instant::now() + EVERYTHING_TIMEOUT;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(500));
        if let Ok(out) = Command::new(es)
            .arg("-get-result-count")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
        {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout);
                if let Ok(n) = s.trim().parse::<u64>() {
                    if n > 0 { return Some(n); }
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Probe logic
// ---------------------------------------------------------------------------

/// Check if Everything is showing an error/OOM dialog by inspecting its window
/// title via `tasklist /V`.  The OOM popup changes the window title to include
/// keywords like "Error", "Out of memory", or "memory".
fn detect_everything_oom_dialog() -> Option<String> {
    let out = Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq Everything.exe", "/V", "/FO", "LIST"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        // tasklist /V /FO LIST shows "Window Title:  <title>"
        if let Some(title) = line.strip_prefix("Window Title:") {
            let title = title.trim();
            let lower = title.to_lowercase();
            if lower.contains("error")
                || lower.contains("out of memory")
                || lower.contains("memory")
                || lower.contains("ipc")
            {
                return Some(title.to_string());
            }
        }
    }
    None
}

/// Kill a hung es.exe process (and Everything) after an OOM dialog is detected.
fn kill_es_and_everything() {
    // Kill es.exe first (it's the IPC client hanging on the blocked pipe)
    Command::new("taskkill")
        .args(["/F", "/IM", "es.exe"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok();
    // Then kill Everything (dismisses the OOM dialog)
    kill_everything();
}

/// Result of a single es.exe probe level.
enum ProbeResult {
    /// Query completed successfully: line count, elapsed time.
    Ok { lines: u64, elapsed: Duration },
    /// es.exe timed out — Everything is likely showing an OOM dialog.
    Timeout { elapsed: Duration, dialog: Option<String> },
    /// es.exe exited with an error code.
    Failed { elapsed: Duration, detail: String },
    /// Could not even spawn es.exe.
    SpawnError(String),
}

/// Run a single es.exe probe at the given level.
///
/// Spawns es.exe and polls it with a timeout.  If es.exe hangs (because
/// Everything is showing an OOM dialog and not responding to IPC), we detect
/// it via timeout + dialog-window check, kill both processes, and report.
fn run_es_level(es: &Path, drive: char, level_idx: usize) -> ProbeResult {
    let (_, extra_args) = LEVELS[level_idx];
    let timeout = if level_idx == 0 { ES_TIMEOUT_COUNT } else { ES_TIMEOUT_QUERY };
    let t0 = Instant::now();

    let mut cmd = Command::new(es);
    if level_idx == 0 {
        cmd.arg("-get-result-count");
    } else {
        cmd.arg(format!("{}:", drive));
        for arg in extra_args {
            cmd.arg(arg);
        }
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ProbeResult::SpawnError(format!("{e}")),
    };

    // Poll until es.exe exits or we hit the timeout.
    // While waiting, periodically check for the OOM dialog so we can bail
    // out early rather than waiting the full timeout.
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // es.exe exited — read output
                let elapsed = t0.elapsed();
                let stdout = child.stdout.take().map(|mut s| {
                    let mut buf = Vec::new();
                    std::io::Read::read_to_end(&mut s, &mut buf).ok();
                    buf
                }).unwrap_or_default();
                let stderr = child.stderr.take().map(|mut s| {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut s, &mut buf).ok();
                    buf
                }).unwrap_or_default();

                if status.success() {
                    let count = if level_idx == 0 {
                        String::from_utf8_lossy(&stdout)
                            .trim()
                            .parse::<u64>()
                            .unwrap_or(0)
                    } else {
                        stdout.iter().filter(|&&b| b == b'\n').count() as u64
                    };
                    return ProbeResult::Ok { lines: count, elapsed };
                } else {
                    let code = status.code().unwrap_or(-1);
                    let detail: String = stderr.chars().take(500).collect();
                    return ProbeResult::Failed {
                        elapsed,
                        detail: format!("exit code {code}; {detail}"),
                    };
                }
            }
            Ok(None) => {
                // Still running — check for timeout and OOM dialog
                let elapsed = t0.elapsed();
                if elapsed >= timeout {
                    let dialog = detect_everything_oom_dialog();
                    child.kill().ok();
                    child.wait().ok();
                    kill_es_and_everything();
                    return ProbeResult::Timeout { elapsed, dialog };
                }
                // Check for OOM dialog every 2s (cheap early exit)
                if elapsed.as_secs() > 5 {
                    if let Some(title) = detect_everything_oom_dialog() {
                        let elapsed = t0.elapsed();
                        child.kill().ok();
                        child.wait().ok();
                        kill_es_and_everything();
                        return ProbeResult::Timeout {
                            elapsed,
                            dialog: Some(title),
                        };
                    }
                }
                std::thread::sleep(Duration::from_secs(2));
            }
            Err(e) => {
                child.kill().ok();
                return ProbeResult::SpawnError(format!("wait error: {e}"));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Timestamp (no external crate)
// ---------------------------------------------------------------------------

fn chrono_timestamp() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let secs_per_day: u64 = 86400;
    let days = now / secs_per_day;
    let day_secs = now % secs_per_day;
    let h = day_secs / 3600;
    let m = (day_secs % 3600) / 60;
    let s = day_secs % 60;
    let (y, mo, d) = days_to_ymd(days);
    format!("{y:04}-{mo:02}-{d:02}_{h:02}-{m:02}-{s:02}")
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let days = days + 719_468;
    let era = days / 146_097;
    let doe = days % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    (y, mo, d)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    println!("═══════════════════════════════════════════════════════");
    println!("  Everything Capacity Probe");
    println!("═══════════════════════════════════════════════════════\n");

    // Locate binaries
    let everything = match everything_exe() {
        Some(p) => { println!("✅ Everything.exe: {}", p.display()); p }
        None    => { eprintln!("❌ Everything.exe not found — aborting."); return; }
    };
    let es = match es_exe() {
        Some(p) => { println!("✅ es.exe:          {}", p.display()); p }
        None    => { eprintln!("❌ es.exe not found — aborting."); return; }
    };
    let ini_path = everything_ini_path();
    if !ini_path.exists() {
        eprintln!("❌ Everything.ini not found at {} — aborting.", ini_path.display());
        return;
    }
    println!("✅ Everything.ini:  {}\n", ini_path.display());

    // Parse drives from ini
    let ini_content = fs::read_to_string(&ini_path).unwrap_or_default();
    let drives = parse_drives_from_ini(&ini_content);
    if drives.is_empty() {
        eprintln!("❌ No NTFS volumes found in Everything.ini — aborting.");
        return;
    }
    println!("📁 Detected drives: {:?}\n", drives);

    // Backup ini content for restore at end
    let ini_backup = ini_content.clone();

    // Prepare log buffer
    let ts = chrono_timestamp();
    let log_name = format!("everything_probe_{ts}.log");
    let log_path = PathBuf::from("scripts").join("windows").join(&log_name);
    let mut log = String::new();
    let _ = writeln!(log, "Everything Capacity Probe — {ts}");
    let _ = writeln!(log, "Drives: {:?}", drives);
    let _ = writeln!(log, "{}", "=".repeat(72));

    // ── Probe each drive ──────────────────────────────────────────────────
    for &drive in &drives {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("  DRIVE {}:", drive);
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        let _ = writeln!(log, "\n--- Drive {}: ---", drive);

        // Isolate this drive in ini and restart Everything
        kill_everything();
        isolate_drive_in_ini(&ini_path, drive, &drives);
        start_everything(&everything);

        print!("   ⏳ Waiting for index...");
        std::io::stdout().flush().ok();
        let record_count = match wait_for_index(&es) {
            Some(n) => { println!(" ready ({n} records)"); n }
            None => {
                println!(" TIMEOUT");
                let _ = writeln!(log, "  TIMEOUT waiting for index");
                kill_everything();
                continue;
            }
        };
        let _ = writeln!(log, "  Records: {record_count}");

        // Progressive column probe — increase data load until it breaks
        let mut max_ok_level: Option<usize> = None;
        for (lvl_idx, (label, _)) in LEVELS.iter().enumerate() {
            print!("   {label} … ");
            std::io::stdout().flush().ok();

            match run_es_level(&es, drive, lvl_idx) {
                ProbeResult::Ok { lines, elapsed } => {
                    let secs = elapsed.as_secs_f64();
                    println!("✅  {lines} lines  {secs:.2}s");
                    let _ = writeln!(log, "  {label}: OK  lines={lines}  time={secs:.2}s");
                    max_ok_level = Some(lvl_idx);
                }
                ProbeResult::Timeout { elapsed, dialog } => {
                    let secs = elapsed.as_secs_f64();
                    let dlg_msg = dialog.as_deref().unwrap_or("(no dialog detected)");
                    println!("⏱️  HUNG  {secs:.1}s — OOM dialog: {dlg_msg}");
                    let _ = writeln!(
                        log,
                        "  {label}: HUNG (OOM)  time={secs:.1}s  dialog=\"{dlg_msg}\""
                    );
                    // Everything showed OOM popup, es.exe was hung.
                    // Both are now killed. More columns = more data = worse. Stop.
                    break;
                }
                ProbeResult::Failed { elapsed, detail } => {
                    let secs = elapsed.as_secs_f64();
                    println!("❌  FAIL  {secs:.2}s  {detail}");
                    let _ = writeln!(log, "  {label}: FAIL  time={secs:.2}s  detail={detail}");
                    // es.exe returned an error — IPC broke. Stop.
                    break;
                }
                ProbeResult::SpawnError(err) => {
                    println!("❌  SPAWN ERROR: {err}");
                    let _ = writeln!(log, "  {label}: SPAWN ERROR  {err}");
                    break;
                }
            }
        }

        // Drive summary
        let summary = match max_ok_level {
            Some(lvl) => format!("Max OK level: {} ({})", lvl, LEVELS[lvl].0),
            None => "ALL LEVELS FAILED".to_string(),
        };
        println!("\n   📊 {summary}\n");
        let _ = writeln!(log, "  RESULT: {summary}");

        kill_everything();
    }

    // ── Cleanup ───────────────────────────────────────────────────────────
    restore_ini(&ini_path, &ini_backup);
    println!("✅ Everything.ini restored\n");

    let _ = writeln!(log, "\n{}", "=".repeat(72));
    let _ = writeln!(log, "Probe complete.");
    fs::write(&log_path, &log).ok();
    println!("📄 Log written to: {}", log_path.display());
    println!("\n═══════════════════════════════════════════════════════");
    println!("  Probe complete");
    println!("═══════════════════════════════════════════════════════");
}
