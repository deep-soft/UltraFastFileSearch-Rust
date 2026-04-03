#!/usr/bin/env rust-script
//! UFFS Performance Profiler — comprehensive timing of cold start, warm search,
//! pattern search, output overhead, and multi-drive parallel performance.
//!
//! # Usage (Windows, elevated)
//!
//! ```powershell
//! # Profile all NTFS drives (auto-discovered)
//! rust-script scripts\windows\profile.rs
//!
//! # Specific drives
//! rust-script scripts\windows\profile.rs --drives C,D,E
//!
//! # Custom pattern, 5 runs, 60s timeout
//! rust-script scripts\windows\profile.rs --drives C --pattern "*.rs" --runs 5 --timeout 60
//!
//! # Skip cold start tests (warm only)
//! rust-script scripts\windows\profile.rs --skip-cold
//!
//! # Skip warm tests (cold only)
//! rust-script scripts\windows\profile.rs --skip-warm
//!
//! # Custom binary path
//! rust-script scripts\windows\profile.rs --bin C:\tools\uffs.exe
//! ```
//!
//! ```cargo
//! [dependencies]
//! ```

use std::env;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// ─── Configuration ───────────────────────────────────────────────────────────

struct Config {
    bin: PathBuf,
    drives: Vec<String>,
    pattern: String,
    runs: u32,
    timeout_secs: u64,
    skip_cold: bool,
    skip_warm: bool,
}

// ─── Measurement result for one test ─────────────────────────────────────────

struct Measurement {
    label: String,
    drive: String,
    times_ms: Vec<f64>,
    records: u64,
    extra: Vec<String>,
    timeouts: u32,
}

impl Measurement {
    fn new(label: &str, drive: &str) -> Self {
        Self { label: label.into(), drive: drive.into(), times_ms: vec![], records: 0, extra: vec![], timeouts: 0 }
    }
    fn avg(&self) -> f64 {
        if self.times_ms.is_empty() { return 0.0; }
        self.times_ms.iter().sum::<f64>() / self.times_ms.len() as f64
    }
    fn min_ms(&self) -> f64 { self.times_ms.iter().cloned().fold(f64::MAX, f64::min) }
    fn max_ms(&self) -> f64 { self.times_ms.iter().cloned().fold(0.0_f64, f64::max) }
}

// ─── Run result ──────────────────────────────────────────────────────────────

struct RunResult {
    elapsed_ms: f64,
    timed_out: bool,
    records: u64,
    stderr_lines: Vec<String>,
    exit_code: Option<i32>,
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn flush() { io::stdout().flush().ok(); }

fn fmt_ms(ms: f64) -> String {
    if ms >= 1000.0 { format!("{:.2}s", ms / 1000.0) }
    else { format!("{:.0}ms", ms) }
}

fn fmt_number(n: u64) -> String {
    let s = n.to_string();
    let mut r = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 { r.push(','); }
        r.push(c);
    }
    r.chars().rev().collect()
}

fn kill_daemon(bin: &PathBuf) {
    let _ = Command::new(bin).args(["daemon", "kill"])
        .stdout(Stdio::null()).stderr(Stdio::null()).status();
    std::thread::sleep(Duration::from_millis(500));
}

fn clear_cache() {
    if let Ok(d) = env::var("LOCALAPPDATA") {
        let _ = std::fs::remove_dir_all(PathBuf::from(&d).join("uffs").join("cache"));
    }
    if let Ok(d) = env::var("TEMP") {
        let _ = std::fs::remove_dir_all(PathBuf::from(&d).join("uffs_index_cache"));
    }
}

fn warmup_daemon(bin: &PathBuf) -> Option<f64> {
    let start = Instant::now();
    let st = Command::new(bin).args(["*", "--limit", "10"])
        .stdout(Stdio::null()).stderr(Stdio::null()).status().ok()?;
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    if st.success() { Some(ms) } else { None }
}

fn daemon_status_lines(bin: &PathBuf) -> Vec<String> {
    Command::new(bin).args(["daemon", "status"]).stderr(Stdio::null())
        .output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect())
        .unwrap_or_default()
}

fn daemon_stats_lines(bin: &PathBuf) -> Vec<String> {
    Command::new(bin).args(["daemon", "stats"]).stderr(Stdio::null())
        .output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect())
        .unwrap_or_default()
}


// ─── Run a benchmark invocation with timeout ─────────────────────────────────

fn run_bench(bin: &PathBuf, args: &[&str], timeout_secs: u64) -> RunResult {
    let start = Instant::now();
    let mut child = match Command::new(bin)
        .args(args)
        .env("UFFS_CACHE_PROFILE", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  ERROR: spawn failed: {e}");
            return RunResult { elapsed_ms: 0.0, timed_out: false, records: 0, stderr_lines: vec![], exit_code: None };
        }
    };

    let deadline = Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                let stderr_lines = child.stderr.take()
                    .map(|s| BufReader::new(s).lines().flatten().collect::<Vec<_>>())
                    .unwrap_or_default();
                let records = parse_records(&stderr_lines);
                return RunResult { elapsed_ms, timed_out: false, records, stderr_lines, exit_code: status.code() };
            }
            Ok(None) if start.elapsed() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return RunResult { elapsed_ms: start.elapsed().as_secs_f64() * 1000.0, timed_out: true, records: 0, stderr_lines: vec![], exit_code: None };
            }
            _ => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

fn parse_records(lines: &[String]) -> u64 {
    for line in lines {
        if line.contains("Records found:") {
            if let Some(n) = line.split(':').last() {
                return n.trim().replace(',', "").parse().unwrap_or(0);
            }
        }
    }
    0
}

fn profile_lines(lines: &[String]) -> Vec<String> {
    lines.iter()
        .filter(|l| l.contains("[CACHE_PROFILE]") || l.contains("[TIMING]")
            || l.contains("BENCHMARK") || l.contains("Records found") || l.contains("Total time"))
        .cloned().collect()
}

// ─── Drive discovery from daemon status ──────────────────────────────────────

fn discover_drives(bin: &PathBuf) -> Vec<String> {
    let _ = warmup_daemon(bin);
    let mut drives = Vec::new();
    for line in daemon_status_lines(bin) {
        let t = line.trim();
        if t.contains("records") && t.len() >= 2 {
            if let Some(c) = t.chars().next() {
                if c.is_ascii_uppercase() { drives.push(c.to_string()); }
            }
        }
    }
    drives.sort();
    drives
}

// ─── CLI arg parsing ─────────────────────────────────────────────────────────

fn parse_args() -> Config {
    let args: Vec<String> = env::args().collect();
    let mut bin = env::var("USERPROFILE")
        .map(|h| PathBuf::from(h).join("bin").join("uffs.exe"))
        .unwrap_or_else(|_| PathBuf::from("uffs.exe"));
    let (mut drives, mut pattern, mut runs, mut timeout_secs) = (vec![], "*".to_string(), 3u32, 180u64);
    let (mut skip_cold, mut skip_warm) = (false, false);

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--drives" | "-d" => { i += 1; if i < args.len() { drives = args[i].split(',').map(|s| s.trim().to_uppercase()).collect(); } }
            "--pattern" | "-p" => { i += 1; if i < args.len() { pattern = args[i].clone(); } }
            "--runs" | "-n" => { i += 1; if i < args.len() { runs = args[i].parse().unwrap_or(3); } }
            "--timeout" | "-t" => { i += 1; if i < args.len() { timeout_secs = args[i].parse().unwrap_or(180); } }
            "--bin" => { i += 1; if i < args.len() { bin = PathBuf::from(&args[i]); } }
            "--skip-cold" => skip_cold = true,
            "--skip-warm" => skip_warm = true,
            "--help" | "-h" => {
                println!("UFFS Performance Profiler\n");
                println!("Usage: rust-script scripts\\windows\\profile.rs [OPTIONS]\n");
                println!("Options:");
                println!("  --drives, -d  C,D,E   Drives to profile (default: auto-discover)");
                println!("  --pattern, -p PATTERN  Search pattern (default: *)");
                println!("  --runs, -n N           Runs per test (default: 3)");
                println!("  --timeout, -t SECS     Per-run timeout (default: 180)");
                println!("  --bin PATH             Path to uffs.exe");
                println!("  --skip-cold            Skip cold start tests");
                println!("  --skip-warm            Skip warm search tests");
                std::process::exit(0);
            }
            other => { eprintln!("Unknown argument: {other}"); std::process::exit(1); }
        }
        i += 1;
    }
    Config { bin, drives, pattern, runs, timeout_secs, skip_cold, skip_warm }
}

// ─── Run a measurement (N runs of the same command) ──────────────────────────

fn measure(cfg: &Config, label: &str, drive: &str, args: Vec<&str>) -> Measurement {
    let mut m = Measurement::new(label, drive);
    for run in 1..=cfg.runs {
        let r = run_bench(&cfg.bin, &args, cfg.timeout_secs);
        if r.timed_out {
            m.timeouts += 1;
            println!("     Run {run}: TIMEOUT (>{}s)", cfg.timeout_secs);
            kill_daemon(&cfg.bin);
        } else {
            m.times_ms.push(r.elapsed_ms);
            if r.records > 0 { m.records = r.records; }
            let pl = profile_lines(&r.stderr_lines);
            if run == 1 { m.extra = pl.clone(); }
            let exit = r.exit_code.map_or(String::new(), |c| if c != 0 { format!(" [exit={c}]") } else { String::new() });
            println!("     Run {run}: {}{}", fmt_ms(r.elapsed_ms), exit);
            for l in &pl { println!("       {l}"); }
        }
    }
    m
}

// ─── Phase runners ───────────────────────────────────────────────────────────

fn phase_cold(cfg: &Config, drives: &[String]) -> Vec<Measurement> {
    println!("\n╔══════════════════════════════════════════════════╗");
    println!("║  PHASE 1: Cold Start (daemon kill + cache clear) ║");
    println!("╚══════════════════════════════════════════════════╝");

    let mut results = Vec::new();
    for drive in drives {
        println!("\n  ◆ Drive {drive}: cold start");
        let label = format!("Cold {drive}:");
        // Before each cold run: kill daemon + clear cache
        let mut m = Measurement::new(&label, drive);
        for run in 1..=cfg.runs {
            kill_daemon(&cfg.bin);
            clear_cache();
            let args = vec![&cfg.pattern as &str, "--drive", drive, "--benchmark"];
            let r = run_bench(&cfg.bin, &args, cfg.timeout_secs);
            if r.timed_out {
                m.timeouts += 1;
                println!("     Run {run}: TIMEOUT (>{}s) - killed", cfg.timeout_secs);
                kill_daemon(&cfg.bin);
            } else {
                m.times_ms.push(r.elapsed_ms);
                if r.records > 0 { m.records = r.records; }
                let pl = profile_lines(&r.stderr_lines);
                if run == 1 { m.extra = pl.clone(); }
                let exit = r.exit_code.map_or(String::new(), |c| if c != 0 { format!(" [exit={c}]") } else { String::new() });
                println!("     Run {run}: {}{}", fmt_ms(r.elapsed_ms), exit);
                for l in &pl { println!("       {l}"); }
            }
        }
        results.push(m);
    }
    // Kill daemon after cold phase
    kill_daemon(&cfg.bin);
    results
}

fn phase_warm(cfg: &Config, drives: &[String]) -> Vec<Measurement> {
    println!("\n╔══════════════════════════════════════════════════╗");
    println!("║  PHASE 2: Warm Search (daemon pre-loaded)        ║");
    println!("╚══════════════════════════════════════════════════╝");

    // Warm up daemon (loads all drives)
    print!("  Warming up daemon...");
    flush();
    if let Some(ms) = warmup_daemon(&cfg.bin) {
        println!(" ready ({})", fmt_ms(ms));
    } else {
        println!(" FAILED");
        return vec![];
    }

    let mut results = Vec::new();

    // Per-drive warm search
    for drive in drives {
        println!("\n  ◆ Drive {drive}: warm search");
        let args = vec![&cfg.pattern as &str, "--drive", drive, "--benchmark"];
        let m = measure(cfg, &format!("Warm {drive}:"), drive, args);
        results.push(m);
    }

    // All-drives parallel
    if drives.len() > 1 {
        println!("\n  ◆ All drives: warm parallel search");
        let args = vec![&cfg.pattern as &str, "--benchmark"];
        let m = measure(cfg, "Warm ALL:", "ALL", args);
        results.push(m);
    }

    results
}

fn phase_pattern(cfg: &Config, drives: &[String]) -> Vec<Measurement> {
    if cfg.pattern == "*" {
        return vec![];
    }
    println!("\n╔══════════════════════════════════════════════════╗");
    println!("║  PHASE 3: Pattern Search ('{}')  ║", cfg.pattern);
    println!("╚══════════════════════════════════════════════════╝");

    let _ = warmup_daemon(&cfg.bin);
    let mut results = Vec::new();
    for drive in drives {
        println!("\n  ◆ Drive {drive}: pattern search");
        let args = vec![&cfg.pattern as &str, "--drive", drive, "--benchmark"];
        let m = measure(cfg, &format!("Pat {drive}:"), drive, args);
        results.push(m);
    }
    results
}

fn phase_output(cfg: &Config, drives: &[String]) -> Vec<Measurement> {
    println!("\n╔══════════════════════════════════════════════════╗");
    println!("║  PHASE 4: Output Overhead (--profile)            ║");
    println!("╚══════════════════════════════════════════════════╝");
    println!("  Measures: search + path resolution + CSV serialization + I/O");

    let _ = warmup_daemon(&cfg.bin);
    let mut results = Vec::new();

    // 4a. Per-drive: write to file (full output pipeline)
    for drive in drives {
        let out_file = env::var("TEMP")
            .map(|t| format!("{}\\uffs_profile_{}.csv", t, drive))
            .unwrap_or_else(|_| format!("uffs_profile_{}.csv", drive));

        println!("\n  ◆ Drive {drive}: output to file ({out_file})");
        let args = vec![&cfg.pattern as &str, "--drive", drive, "--out", &out_file, "--profile"];
        let m = measure(cfg, &format!("File {drive}:"), drive, args);
        // Clean up temp file
        let _ = std::fs::remove_file(&out_file);
        results.push(m);
    }

    // 4b. Per-drive: stdout to NUL (console output pipeline, discarded)
    for drive in drives {
        println!("\n  ◆ Drive {drive}: output to stdout (piped to /dev/null)");
        let m = measure_stdout_null(cfg, &format!("Stdout {drive}:"), drive);
        results.push(m);
    }

    // 4c. All drives: write to file
    if drives.len() > 1 {
        let out_file = env::var("TEMP")
            .map(|t| format!("{}\\uffs_profile_ALL.csv", t))
            .unwrap_or_else(|_| "uffs_profile_ALL.csv".to_string());

        println!("\n  ◆ All drives: output to file ({out_file})");
        let args = vec![&cfg.pattern as &str, "--out", &out_file, "--profile"];
        let m = measure(cfg, "File ALL:", "ALL", args);
        let _ = std::fs::remove_file(&out_file);
        results.push(m);
    }

    results
}

/// Measure stdout output overhead: run with full output piped to /dev/null.
/// Captures stderr for CACHE_PROFILE/TIMING lines. Stdout is consumed and discarded.
fn measure_stdout_null(cfg: &Config, label: &str, drive: &str) -> Measurement {
    let mut m = Measurement::new(label, drive);
    for run in 1..=cfg.runs {
        let start = Instant::now();
        let mut child = match Command::new(&cfg.bin)
            .args([&cfg.pattern as &str, "--drive", drive, "--profile"])
            .env("UFFS_CACHE_PROFILE", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => { eprintln!("  ERROR: spawn failed: {e}"); continue; }
        };
        let deadline = Duration::from_secs(cfg.timeout_secs);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                    let stderr_lines = child.stderr.take()
                        .map(|s| BufReader::new(s).lines().flatten().collect::<Vec<_>>())
                        .unwrap_or_default();
                    let records = parse_records(&stderr_lines);
                    m.times_ms.push(elapsed_ms);
                    if records > 0 { m.records = records; }
                    let pl = profile_lines(&stderr_lines);
                    if run == 1 { m.extra = pl.clone(); }
                    let exit = status.code().map_or(String::new(), |c| if c != 0 { format!(" [exit={c}]") } else { String::new() });
                    println!("     Run {run}: {}{}", fmt_ms(elapsed_ms), exit);
                    for l in &pl { println!("       {l}"); }
                    break;
                }
                Ok(None) if start.elapsed() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    m.timeouts += 1;
                    println!("     Run {run}: TIMEOUT (>{}s) - killed", cfg.timeout_secs);
                    kill_daemon(&cfg.bin);
                    break;
                }
                _ => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }
    m
}

// ─── Summary table ───────────────────────────────────────────────────────────

fn print_summary(all: &[Measurement]) {
    println!("\n╔═══════════════════════════════════════════════════════════════════════════╗");
    println!("║                         PERFORMANCE SUMMARY                              ║");
    println!("╠═══════════════════════════════════════════════════════════════════════════╣");
    println!("║ {:<18} {:>10} {:>10} {:>10} {:>14} {:>5} ║", "Test", "Avg", "Min", "Max", "Records", "OK");
    println!("╠═══════════════════════════════════════════════════════════════════════════╣");

    let mut prev_phase = "";
    for m in all {
        // Separator between phases (Cold→Warm→Pat→File→Stdout)
        let phase = m.label.split_whitespace().next().unwrap_or("");
        if !prev_phase.is_empty() && phase != prev_phase {
            println!("╟───────────────────────────────────────────────────────────────────────────╢");
        }
        prev_phase = phase;

        if m.times_ms.is_empty() && m.timeouts > 0 {
            println!("║ {:<18} {:>10} {:>10} {:>10} {:>14} {:>4}! ║",
                m.label, "TIMEOUT", "-", "-", "-", m.timeouts);
        } else if !m.times_ms.is_empty() {
            let ok_str = if m.timeouts > 0 {
                format!("{}/{}!", m.times_ms.len(), m.times_ms.len() as u32 + m.timeouts)
            } else {
                format!("{}/{}", m.times_ms.len(), m.times_ms.len())
            };
            println!("║ {:<18} {:>10} {:>10} {:>10} {:>14} {:>5} ║",
                m.label, fmt_ms(m.avg()), fmt_ms(m.min_ms()), fmt_ms(m.max_ms()),
                fmt_number(m.records), ok_str);
        }
    }
    println!("╚═══════════════════════════════════════════════════════════════════════════╝");

    // Throughput analysis
    println!("\n── Throughput ──────────────────────────────────────────────────────────");
    for m in all {
        if !m.times_ms.is_empty() && m.records > 0 {
            let avg_secs = m.avg() / 1000.0;
            let rps = m.records as f64 / avg_secs;
            println!("  {:<18}  {:>12} records/sec  ({} in {})",
                m.label, fmt_number(rps as u64), fmt_number(m.records), fmt_ms(m.avg()));
        }
    }

    // Bottleneck analysis: compare warm vs file vs stdout to show where time goes
    println!("\n── Bottleneck Analysis ─────────────────────────────────────────────────");
    let find_avg = |prefix: &str| -> Option<(String, f64, u64)> {
        all.iter()
            .find(|m| m.label.starts_with(prefix) && !m.times_ms.is_empty())
            .map(|m| (m.drive.clone(), m.avg(), m.records))
    };

    // For the first drive that has all three measurements
    for m in all {
        if !m.label.starts_with("Warm ") || m.drive == "ALL" { continue; }
        let d = &m.drive;
        let warm = find_avg(&format!("Warm {d}:"));
        let cold = find_avg(&format!("Cold {d}:"));
        let file = find_avg(&format!("File {d}:"));
        let stdout = find_avg(&format!("Stdout {d}:"));

        if let Some((_, warm_ms, recs)) = warm {
            println!("\n  Drive {d}: ({} records)", fmt_number(recs));
            if let Some((_, cold_ms, _)) = cold {
                let startup_ms = cold_ms - warm_ms;
                println!("    Daemon startup + MFT load:  {:>10}  ({:.0}% of cold)",
                    fmt_ms(startup_ms), startup_ms / cold_ms * 100.0);
                println!("    Search (warm):              {:>10}  ({:.0}% of cold)",
                    fmt_ms(warm_ms), warm_ms / cold_ms * 100.0);
            }
            if let Some((_, file_ms, _)) = file {
                let output_ms = file_ms - warm_ms;
                println!("    Output to file overhead:    {:>10}  (search {} + write {})",
                    fmt_ms(output_ms), fmt_ms(warm_ms), fmt_ms(output_ms));
            }
            if let Some((_, stdout_ms, _)) = stdout {
                let output_ms = stdout_ms - warm_ms;
                println!("    Stdout output overhead:     {:>10}  (search {} + write {})",
                    fmt_ms(output_ms), fmt_ms(warm_ms), fmt_ms(output_ms));
            }
        }
    }
}

// ─── Main ────────────────────────────────────────────────────────────────────

fn main() {
    let mut cfg = parse_args();

    // Verify binary exists
    if !cfg.bin.exists() {
        eprintln!("ERROR: uffs binary not found at: {}", cfg.bin.display());
        eprintln!("Use --bin to specify the correct path.");
        std::process::exit(1);
    }

    // Version info
    let version = Command::new(&cfg.bin).arg("--version").output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());

    // Auto-discover drives if not specified
    if cfg.drives.is_empty() {
        print!("Auto-discovering drives... ");
        flush();
        cfg.drives = discover_drives(&cfg.bin);
        if cfg.drives.is_empty() {
            println!("FAILED (no drives found)");
            eprintln!("Use --drives to specify drives manually.");
            std::process::exit(1);
        }
        println!("found: {}", cfg.drives.join(", "));
    }

    // Print header
    println!();
    println!("╔══════════════════════════════════════════════════╗");
    println!("║          UFFS Performance Profiler               ║");
    println!("╠══════════════════════════════════════════════════╣");
    println!("║  Binary:   {:<38}║", version);
    println!("║  Drives:   {:<38}║", cfg.drives.join(", "));
    println!("║  Pattern:  {:<38}║", cfg.pattern);
    println!("║  Runs:     {:<38}║", cfg.runs);
    println!("║  Timeout:  {:<38}║", format!("{}s", cfg.timeout_secs));
    println!("╚══════════════════════════════════════════════════╝");

    let total_start = Instant::now();
    let mut all_measurements: Vec<Measurement> = Vec::new();

    // Phase 1: Cold start
    if !cfg.skip_cold {
        let cold = phase_cold(&cfg, &cfg.drives.clone());
        all_measurements.extend(cold);
    }

    // Phase 2: Warm search
    if !cfg.skip_warm {
        let warm = phase_warm(&cfg, &cfg.drives.clone());
        all_measurements.extend(warm);
    }

    // Phase 3: Pattern search (only if non-default pattern)
    let pat = phase_pattern(&cfg, &cfg.drives.clone());
    all_measurements.extend(pat);

    // Phase 4: Output overhead (file write, stdout, --profile breakdown)
    if !cfg.skip_warm {
        let output = phase_output(&cfg, &cfg.drives.clone());
        all_measurements.extend(output);
    }

    // Daemon stats after all tests
    println!("\n── Daemon Stats ────────────────────────────────────────────────────");
    for line in daemon_stats_lines(&cfg.bin) {
        println!("  {line}");
    }

    // Summary table
    print_summary(&all_measurements);

    // Total time
    let total_ms = total_start.elapsed().as_secs_f64() * 1000.0;
    println!("\nTotal profiling time: {}", fmt_ms(total_ms));

    // Final cleanup
    kill_daemon(&cfg.bin);
}
