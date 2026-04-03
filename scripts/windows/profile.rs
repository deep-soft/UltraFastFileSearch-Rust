#!/usr/bin/env rust-script
//! UFFS Performance Profiler — 3-phase per-drive timing using `--profile`.
//!
//! For each discovered NTFS drive, runs three caching levels:
//!
//!   COLD       — no daemon, no cache files  (full MFT read + index build)
//!   WARM CACHE — no daemon, cache files exist  (daemon auto-starts from cache)
//!   HOT        — daemon already running  (pure in-memory search)
//!
//! Uses `--profile --limit 100` so output doesn't dominate but we still
//! exercise the full search + path-resolution + serialization pipeline.
//!
//! # Usage (Windows, elevated)
//!
//! ```powershell
//! rust-script scripts\windows\profile.rs
//! rust-script scripts\windows\profile.rs --drives C,D
//! rust-script scripts\windows\profile.rs --bin C:\tools\uffs.exe
//! ```
//!
//! ```cargo
//! [dependencies]
//! ```

use std::env;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// ─── Types ──────────────────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct ProfileTiming {
    total_ms: u64,
    connect_ms: u64,
    ready_ms: u64,
    ipc_ms: u64,
    daemon_search_ms: u64,
    startup_ms: u64,
    records_scanned: String,
    profile_lines: Vec<String>,
}

struct RunResult {
    drive: String,
    phase: String,
    timing: ProfileTiming,
    success: bool,
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn flush() { std::io::stderr().flush().ok(); }

fn kill_daemon(bin: &PathBuf) {
    let _ = Command::new(bin).args(["daemon", "kill"])
        .stdout(Stdio::null()).stderr(Stdio::null()).status();
    std::thread::sleep(Duration::from_secs(2));
}

fn delete_cache() {
    if let Ok(local) = env::var("LOCALAPPDATA") {
        let p = PathBuf::from(&local).join("uffs").join("cache");
        if p.exists() { let _ = std::fs::remove_dir_all(&p); }
    }
    if let Ok(tmp) = env::var("TEMP") {
        let p = PathBuf::from(&tmp).join("uffs_index_cache");
        if p.exists() { let _ = std::fs::remove_dir_all(&p); }
    }
}

fn discover_drives(bin: &PathBuf) -> Vec<String> {
    let _ = Command::new(bin).args(["*", "--limit", "1"])
        .stdout(Stdio::null()).stderr(Stdio::null()).status();
    std::thread::sleep(Duration::from_secs(1));
    let output = Command::new(bin).args(["daemon", "status"])
        .stderr(Stdio::null()).output().ok();
    let stdout = output.map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let mut drives = Vec::new();
    for line in stdout.lines() {
        let t = line.trim();
        if t.contains("records") {
            if let Some(ch) = t.chars().next() {
                if ch.is_ascii_uppercase() { drives.push(ch.to_string()); }
            }
        }
    }
    drives.sort();
    drives
}

fn extract_ms(line: &str, prefix: &str) -> Option<u64> {
    let idx = line.find(prefix)?;
    let after = &line[idx + prefix.len()..];
    let num_start = after.find(|c: char| c.is_ascii_digit())?;
    let num_end = after[num_start..].find(|c: char| !c.is_ascii_digit())
        .map_or(after.len(), |e| num_start + e);
    after[num_start..num_end].parse().ok()
}

fn parse_profile(stderr_lines: &[String]) -> ProfileTiming {
    let mut t = ProfileTiming::default();
    for line in stderr_lines {
        let s = line.trim();
        // Keep profile lines for display.
        if s.starts_with("===") || s.starts_with("Connect:") || s.starts_with("Await")
            || s.starts_with("Search (IPC)") || s.starts_with("Convert")
            || s.starts_with("Uptime") || s.starts_with("Startup")
            || s.starts_with("Lock") || s.starts_with("Search:")
            || s.starts_with("Row build") || s.starts_with("Shmem")
            || s.starts_with("Output") || s.starts_with("Drive")
            || s.starts_with("SUM") || s.contains("Cache")
        {
            t.profile_lines.push(line.clone());
        }
        if let Some(v) = extract_ms(s, "Connect:") { t.connect_ms = v; }
        if let Some(v) = extract_ms(s, "Await ready:") { t.ready_ms = v; }
        if let Some(v) = extract_ms(s, "Search (IPC):") { t.ipc_ms = v; }
        if s.starts_with("Search:") {
            if let Some(v) = extract_ms(s, "Search:") { t.daemon_search_ms = v; }
            if let (Some(a), Some(b)) = (s.find('('), s.find(" records")) {
                t.records_scanned = s[a+1..b].to_string();
            }
        }
        if let Some(v) = extract_ms(s, "Startup:") { t.startup_ms = v; }
        if s.starts_with("=== TOTAL:") {
            if let Some(v) = extract_ms(s, "TOTAL:") { t.total_ms = v; }
        }
    }
    t
}

fn run_profile(bin: &PathBuf, drive: &str, phase: &str) -> RunResult {
    let args = if drive == "ALL" {
        vec!["*", "--profile", "--limit", "100"]
    } else {
        vec!["*", "--profile", "--drive", drive, "--limit", "100"]
    };
    let output = Command::new(bin)
        .args(&args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    match output {
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let lines: Vec<String> = stderr.lines().map(String::from).collect();
            let timing = parse_profile(&lines);
            RunResult { drive: drive.to_string(), phase: phase.to_string(), timing, success: out.status.success() }
        }
        Err(err) => {
            eprintln!("    ERROR: {err}");
            RunResult { drive: drive.to_string(), phase: phase.to_string(),
                        timing: ProfileTiming::default(), success: false }
        }
    }
}

fn parse_args() -> (PathBuf, Vec<String>) {
    let args: Vec<String> = env::args().collect();
    let mut bin = env::var("USERPROFILE")
        .map(|h| PathBuf::from(h).join("bin").join("uffs.exe"))
        .unwrap_or_else(|_| PathBuf::from("uffs.exe"));
    let mut drives: Vec<String> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--drives" | "-d" => {
                i += 1;
                if i < args.len() {
                    drives = args[i].split(',').map(|s| s.trim().to_uppercase()).collect();
                }
            }
            "--bin" => { i += 1; if i < args.len() { bin = PathBuf::from(&args[i]); } }
            "--help" | "-h" => {
                eprintln!("UFFS Performance Profiler (3-phase per-drive)");
                eprintln!("Usage: rust-script scripts\\windows\\profile.rs [OPTIONS]");
                eprintln!("  --drives, -d C,D,E   Drives to profile (default: auto-discover)");
                eprintln!("  --bin PATH            Path to uffs.exe");
                std::process::exit(0);
            }
            other => { eprintln!("Unknown argument: {other}"); std::process::exit(1); }
        }
        i += 1;
    }
    (bin, drives)
}

// ─── Summary ────────────────────────────────────────────────────────────────

fn print_summary(results: &[RunResult]) {
    eprintln!();
    eprintln!("╔═══════════════════════════════════════════════════════════════════════════════════════════╗");
    eprintln!("║                         PERFORMANCE SUMMARY (--profile)                                  ║");
    eprintln!("╠═══════════════════════════════════════════════════════════════════════════════════════════╣");
    eprintln!("║ {:<6} {:<12} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>14} {:>3} ║",
        "Drive", "Phase", "Total", "Connct", "Ready", "Startup", "Search", "IPC", "Records", "OK");
    eprintln!("╠═══════════════════════════════════════════════════════════════════════════════════════════╣");
    let mut prev_drive = String::new();
    for r in results {
        if !prev_drive.is_empty() && r.drive != prev_drive {
            eprintln!("╟───────────────────────────────────────────────────────────────────────────────────────────╢");
        }
        prev_drive.clone_from(&r.drive);
        let ok = if r.success { "✅" } else { "❌" };
        let t = &r.timing;
        eprintln!("║ {:<6} {:<12} {:>6}ms {:>6}ms {:>6}ms {:>6}ms {:>6}ms {:>6}ms {:>14} {:>3} ║",
            format!("{}:", r.drive), r.phase,
            t.total_ms, t.connect_ms, t.ready_ms, t.startup_ms,
            t.daemon_search_ms, t.ipc_ms, t.records_scanned, ok);
    }
    eprintln!("╚═══════════════════════════════════════════════════════════════════════════════════════════╝");

    // Speedup analysis.
    eprintln!();
    eprintln!("── Speedup ─────────────────────────────────────────────────────────────");
    let mut seen = Vec::new();
    for r in results { if !seen.contains(&r.drive) { seen.push(r.drive.clone()); } }
    for drive in &seen {
        let cold = results.iter().find(|r| r.drive == *drive && r.phase == "COLD");
        let warm = results.iter().find(|r| r.drive == *drive && r.phase == "WARM CACHE");
        let hot  = results.iter().find(|r| r.drive == *drive && r.phase == "HOT");
        if let (Some(c), Some(h)) = (cold, hot) {
            if h.timing.total_ms > 0 {
                let speedup = c.timing.total_ms as f64 / h.timing.total_ms as f64;
                eprint!("  {drive}:  COLD {}ms → HOT {}ms = {speedup:.1}x",
                    c.timing.total_ms, h.timing.total_ms);
                if let Some(w) = warm {
                    eprint!("  (WARM CACHE: {}ms)", w.timing.total_ms);
                }
                eprintln!();
            }
        }
    }
}

// ─── Main ───────────────────────────────────────────────────────────────────

fn main() {
    let (bin, mut drives) = parse_args();

    if !bin.exists() {
        eprintln!("ERROR: uffs binary not found at: {}", bin.display());
        eprintln!("Use --bin to specify the correct path.");
        std::process::exit(1);
    }

    // Version.
    let version = Command::new(&bin).arg("--version").output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());

    // Discover drives if not specified.
    if drives.is_empty() {
        eprint!("  Auto-discovering drives... ");
        flush();
        drives = discover_drives(&bin);
        if drives.is_empty() {
            eprintln!("FAILED (no drives found). Use --drives C,D to specify.");
            std::process::exit(1);
        }
        eprintln!("found: {}", drives.join(", "));
    }

    eprintln!();
    eprintln!("╔══════════════════════════════════════════════════════════════╗");
    eprintln!("║          UFFS Performance Profiler (3-Phase)                 ║");
    eprintln!("╠══════════════════════════════════════════════════════════════╣");
    eprintln!("║  Binary:   {:<48}║", version);
    eprintln!("║  Drives:   {:<48}║", drives.join(", "));
    eprintln!("║  Pattern:  {:<48}║", "*");
    eprintln!("║  Limit:    {:<48}║", "100 rows");
    eprintln!("║  Phases:   {:<48}║", "COLD → WARM CACHE → HOT");
    eprintln!("╚══════════════════════════════════════════════════════════════╝");

    let total_start = Instant::now();
    let mut all_results: Vec<RunResult> = Vec::new();

    for drive in &drives {
        eprintln!();
        eprintln!("━━━ Drive {drive}: ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

        // ── COLD: kill daemon + delete cache ────────────────────────────
        eprintln!("  [COLD] Killing daemon + deleting cache...");
        kill_daemon(&bin);
        delete_cache();
        eprintln!("  [COLD] Running: uffs \"*\" --profile --drive {drive} --limit 100");
        let cold = run_profile(&bin, drive, "COLD");
        for line in &cold.timing.profile_lines { eprintln!("    {line}"); }
        eprintln!("  [COLD] Total: {}ms  {}", cold.timing.total_ms,
            if cold.success { "✅" } else { "❌" });
        all_results.push(cold);

        // ── WARM CACHE: kill daemon, cache files remain ─────────────────
        eprintln!();
        eprintln!("  [WARM CACHE] Killing daemon (cache files remain)...");
        kill_daemon(&bin);
        eprintln!("  [WARM CACHE] Running: uffs \"*\" --profile --drive {drive} --limit 100");
        let warm = run_profile(&bin, drive, "WARM CACHE");
        for line in &warm.timing.profile_lines { eprintln!("    {line}"); }
        eprintln!("  [WARM CACHE] Total: {}ms  {}", warm.timing.total_ms,
            if warm.success { "✅" } else { "❌" });
        all_results.push(warm);

        // ── HOT: daemon still running from warm cache run ───────────────
        eprintln!();
        eprintln!("  [HOT] Daemon still running from WARM CACHE phase...");
        eprintln!("  [HOT] Running: uffs \"*\" --profile --drive {drive} --limit 100");
        let hot = run_profile(&bin, drive, "HOT");
        for line in &hot.timing.profile_lines { eprintln!("    {line}"); }
        eprintln!("  [HOT] Total: {}ms  {}", hot.timing.total_ms,
            if hot.success { "✅" } else { "❌" });
        all_results.push(hot);
    }

    // ── ALL drives: COLD → WARM CACHE → HOT ───────────────────────────
    if drives.len() > 1 {
        eprintln!();
        eprintln!("━━━ ALL drives: ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

        // COLD ALL: kill daemon + delete cache
        eprintln!("  [COLD] Killing daemon + deleting cache...");
        kill_daemon(&bin);
        delete_cache();
        eprintln!("  [COLD] Running: uffs \"*\" --profile --limit 100");
        let cold_all = run_profile(&bin, "ALL", "COLD");
        for line in &cold_all.timing.profile_lines { eprintln!("    {line}"); }
        eprintln!("  [COLD] Total: {}ms  {}", cold_all.timing.total_ms,
            if cold_all.success { "✅" } else { "❌" });
        all_results.push(cold_all);

        // WARM CACHE ALL: kill daemon, cache files remain
        eprintln!();
        eprintln!("  [WARM CACHE] Killing daemon (cache files remain)...");
        kill_daemon(&bin);
        eprintln!("  [WARM CACHE] Running: uffs \"*\" --profile --limit 100");
        let warm_all = run_profile(&bin, "ALL", "WARM CACHE");
        for line in &warm_all.timing.profile_lines { eprintln!("    {line}"); }
        eprintln!("  [WARM CACHE] Total: {}ms  {}", warm_all.timing.total_ms,
            if warm_all.success { "✅" } else { "❌" });
        all_results.push(warm_all);

        // HOT ALL: daemon still running
        eprintln!();
        eprintln!("  [HOT] Daemon still running from WARM CACHE phase...");
        eprintln!("  [HOT] Running: uffs \"*\" --profile --limit 100");
        let hot_all = run_profile(&bin, "ALL", "HOT");
        for line in &hot_all.timing.profile_lines { eprintln!("    {line}"); }
        eprintln!("  [HOT] Total: {}ms  {}", hot_all.timing.total_ms,
            if hot_all.success { "✅" } else { "❌" });
        all_results.push(hot_all);
    }

    // ── Summary ─────────────────────────────────────────────────────────
    print_summary(&all_results);

    let total_secs = total_start.elapsed().as_secs();
    eprintln!();
    eprintln!("Total profiling time: {}m {}s", total_secs / 60, total_secs % 60);

    // Cleanup.
    kill_daemon(&bin);
}