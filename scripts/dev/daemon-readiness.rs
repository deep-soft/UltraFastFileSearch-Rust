#!/usr/bin/env rust-script
//! ```cargo
//! [dependencies]
//! anyhow = "1.0"
//! clap = { version = "4.0", features = ["derive"] }
//! colored = "2.0"
//! ```
// =============================================================================
// scripts/dev/daemon-readiness.rs — UFFS Daemon Readiness Verification
// =============================================================================
//
// Exercises ALL meaningful daemon lifecycle combinations:
//
//   Scenario A: Clean lifecycle (start → search → stats → stop)
//   Scenario B: Idempotent operations (stop/kill/status when not running)
//   Scenario C: Double-start (start when already running)
//   Scenario D: Hard kill recovery (start → kill → start)
//   Scenario E: Graceful cycle (start → stop → start)
//   Scenario F: Restart (start → restart → verify)
//   Scenario G: Double restart
//   Scenario H: Stats accumulation across searches
//   Scenario I: Kill running → status shows not running
//   Scenario J: Search auto-starts daemon
//   Scenario K: Startup timing (COLD → WARM → HOT)
//
// Usage:
//   rust-script scripts/dev/daemon-readiness.rs ~/uffs_data          # macOS with offline data
//   rust-script scripts/dev/daemon-readiness.rs                       # Windows (auto-discovers NTFS drives)
//   rust-script scripts/dev/daemon-readiness.rs --binary target/release/uffs

use std::process::{Command, Output};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Parser;
use colored::Colorize;

/// Maximum time any single `uffs` invocation may run before we kill it.
const STEP_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Parser)]
#[command(
    name = "daemon-readiness",
    about = "UFFS daemon lifecycle verification",
    after_help = "EXAMPLES:\n  \
        rust-script scripts/dev/daemon-readiness.rs ~/uffs_data\n  \
        rust-script scripts/dev/daemon-readiness.rs /path/to/C_mft.iocp\n  \
        rust-script scripts/dev/daemon-readiness.rs ~/uffs_data --pattern '*.dll'\n  \
        rust-script scripts/dev/daemon-readiness.rs                  # Windows: auto-discover NTFS drives"
)]
struct Cli {
    /// Path to an MFT file or a data directory containing drive_* subdirs.
    /// Auto-detected: if it's a file → --mft-file, if directory → --data-dir.
    /// On Windows, omit to auto-discover live NTFS drives.
    #[arg(value_name = "PATH")]
    path: Option<String>,

    /// Path to the uffs binary.
    /// Default: ~/bin/uffs first, then target/release/uffs
    #[arg(long)]
    binary: Option<String>,

    /// Search pattern to test with.
    #[arg(long, default_value = "*.rs")]
    pattern: String,
}

/// Detect whether the user passed a file or directory and return the
/// appropriate uffs CLI flag + value.
fn detect_data_source(path: &str) -> Result<(&'static str, String)> {
    let p = std::path::Path::new(path);
    if !p.exists() {
        bail!("Path does not exist: {path}");
    }
    if p.is_file() {
        Ok(("--mft-file", path.to_owned()))
    } else if p.is_dir() {
        Ok(("--data-dir", path.to_owned()))
    } else {
        bail!("Path is neither a file nor a directory: {path}");
    }
}

// ── Test harness ─────────────────────────────────────────────────────────────

struct Runner {
    binary: String,
    /// `"--data-dir"` or `"--mft-file"`, or `None` for Windows live drives.
    source_flag: Option<&'static str>,
    /// The path value for the flag (empty when using live drives).
    source_path: String,
    pattern: String,
    passed: u32,
    failed: u32,
    timings: Vec<(String, u128)>,
}

impl Runner {
    fn new(binary: String, source_flag: Option<&'static str>, source_path: String, pattern: String) -> Self {
        Self { binary, source_flag, source_path, pattern, passed: 0, failed: 0, timings: Vec::new() }
    }

    /// Build the source args (e.g. ["--data-dir", "/path"]) or empty for live drives.
    fn source_args(&self) -> Vec<&str> {
        match self.source_flag {
            Some(flag) => vec![flag, &self.source_path],
            None => vec![],
        }
    }

    /// Run uffs with a hard 120-second timeout.
    ///
    /// Spawns reader threads for stdout/stderr so pipe buffers are
    /// continuously drained.  Without this, a child that writes more
    /// than the OS pipe buffer (4-64 KB) deadlocks because the parent
    /// only reads *after* exit — but exit can't happen while the write
    /// is blocked on a full pipe.
    fn run_raw(&self, args: &[&str]) -> Result<Output> {
        let mut child = Command::new(&self.binary)
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to exec: {} {}", self.binary, args.join(" ")))?;

        // Drain stdout/stderr on background threads so pipes never fill.
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        let stdout_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut r) = stdout_pipe {
                let _ = std::io::Read::read_to_end(&mut r, &mut buf);
            }
            buf
        });
        let stderr_thread = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut r) = stderr_pipe {
                let _ = std::io::Read::read_to_end(&mut r, &mut buf);
            }
            buf
        });

        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let stdout = stdout_thread.join().unwrap_or_default();
                    let stderr = stderr_thread.join().unwrap_or_default();
                    return Ok(Output { status, stdout, stderr });
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait(); // reap zombie
                        bail!(
                            "TIMEOUT after {}s: {} {}",
                            STEP_TIMEOUT.as_secs(),
                            self.binary,
                            args.join(" ")
                        );
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                Err(e) => {
                    bail!("wait error for {} {}: {e}", self.binary, args.join(" "));
                }
            }
        }
    }

    /// Run uffs, require exit 0.
    fn run_ok(&self, args: &[&str]) -> Result<String> {
        let out = self.run_raw(args)?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        if !out.status.success() {
            bail!("exit {}:\n  stdout: {stdout}\n  stderr: {stderr}",
                out.status.code().unwrap_or(-1));
        }
        Ok(stdout)
    }

    fn has_failed(&self) -> bool { self.failed > 0 }

    /// Run a step — announce what we're about to do, then show result.
    fn step(&mut self, name: &str, f: impl FnOnce(&mut Self) -> Result<String>) {
        if self.has_failed() { return; }
        println!("  {name}");
        let t = Instant::now();
        match f(self) {
            Ok(detail) => {
                let ms = t.elapsed().as_millis();
                if detail.is_empty() {
                    println!("    ↳ {} ({ms}ms)", "PASSED".green().bold());
                } else {
                    println!("    ↳ {} ({ms}ms) — {detail}", "PASSED".green().bold());
                }
                self.passed += 1;
                self.timings.push((name.to_owned(), ms));
            }
            Err(e) => {
                println!("    ↳ {}: {e:#}", "FAILED".red().bold());
                self.failed += 1;
                self.ensure_stopped();
            }
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    fn ensure_stopped(&self) {
        let _ = self.run_ok(&["daemon", "kill"]);
        // Poll until the daemon is actually gone (up to 10s).
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(250));
            if let Ok(out) = self.run_ok(&["daemon", "status"]) {
                if out.contains("not running") { return; }
            }
        }
    }

    fn assert_not_running(&self) -> Result<()> {
        // Poll for up to 5s — on Windows, process teardown can be slow.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let out = self.run_ok(&["daemon", "status"])?;
            if out.contains("not running") { return Ok(()); }
            if Instant::now() >= deadline {
                bail!("Expected 'not running', got:\n{out}");
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn assert_ready(&self) -> Result<String> {
        let out = self.run_ok(&["daemon", "status"])?;
        if !out.contains("Ready") {
            bail!("Expected 'Ready', got:\n{out}");
        }
        let drives = out.lines().filter(|l| l.contains("records")).count();
        Ok(format!("[{drives} drives]"))
    }

    fn start_daemon(&self) -> Result<String> {
        let mut args: Vec<&str> = vec!["daemon", "start"];
        args.extend(self.source_args());
        self.run_ok(&args)
    }

    fn search(&self, limit: u32) -> Result<usize> {
        let lim = limit.to_string();
        let mut args: Vec<&str> = vec![&self.pattern];
        args.extend(self.source_args());
        args.extend(["--limit", &lim]);
        let out = self.run_ok(&args)?;
        Ok(out.lines().count())
    }
}

// ── Scenarios ────────────────────────────────────────────────────────────────

fn scenario_a(r: &mut Runner) {
    println!("\n{}", "── Scenario A: Clean lifecycle ──".cyan().bold());

    r.step("A1  Kill stale daemon", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("A2  Verify not running", |r| { r.assert_not_running()?; Ok(String::new()) });
    r.step("A3  Start daemon", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("A4  Verify Ready + drives", |r| r.assert_ready());
    r.step("A5  Search returns results", |r| {
        let n = r.search(100)?;
        if n == 0 { bail!("Zero results"); }
        Ok(format!("[{n} rows]"))
    });
    r.step("A6  Second search (warm)", |r| {
        let n = r.search(100)?;
        if n == 0 { bail!("Zero results"); }
        Ok(format!("[{n} rows]"))
    });
    r.step("A7  Stats show queries", |r| {
        let out = r.run_ok(&["daemon", "stats"])?;
        if !out.contains("Queries served:") { bail!("Missing stats"); }
        let detail: Vec<&str> = out.lines()
            .map(|l| l.trim())
            .filter(|l| l.starts_with("Startup duration:")
                || l.starts_with("Queries served:")
                || l.starts_with("Avg query time:"))
            .collect();
        Ok(detail.join(" | "))
    });
    r.step("A8  Graceful stop", |r| { r.run_ok(&["daemon", "stop"])?; Ok(String::new()) });
    r.step("A9  Verify stopped", |r| { r.assert_not_running()?; Ok(String::new()) });
}

fn scenario_b(r: &mut Runner) {
    println!("\n{}", "── Scenario B: Idempotent ops on stopped daemon ──".cyan().bold());

    r.step("B0  Ensure stopped", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("B1  Status when not running", |r| { r.assert_not_running()?; Ok(String::new()) });
    r.step("B2  Stop when not running", |r| {
        let out = r.run_ok(&["daemon", "stop"])?;
        if !out.contains("not running") { bail!("Expected 'not running', got: {out}"); }
        Ok(String::new())
    });
    r.step("B3  Kill when not running", |r| {
        let out = r.run_ok(&["daemon", "kill"])?;
        if !out.contains("No daemon found") && !out.contains("not running") {
            bail!("Expected no-daemon message, got: {out}");
        }
        Ok(String::new())
    });
    r.step("B4  Restart when not running", |r| {
        let out = r.run_ok(&["daemon", "restart"])?;
        if !out.contains("not running") { bail!("Expected 'not running', got: {out}"); }
        Ok(String::new())
    });
    r.step("B5  Stats when not running", |r| {
        let out = r.run_ok(&["daemon", "stats"])?;
        if !out.contains("not running") { bail!("Expected 'not running', got: {out}"); }
        Ok(String::new())
    });
}

fn scenario_c(r: &mut Runner) {
    println!("\n{}", "── Scenario C: Double start ──".cyan().bold());

    r.step("C0  Ensure stopped", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("C1  Start daemon", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("C2  Start again (already running)", |r| {
        let out = r.start_daemon()?;
        if !out.contains("already running") { bail!("Expected 'already running', got: {out}"); }
        Ok(String::new())
    });
    r.step("C3  Still Ready", |r| r.assert_ready());
    r.step("C4  Cleanup: stop", |r| { r.run_ok(&["daemon", "stop"])?; Ok(String::new()) });
}

fn scenario_d(r: &mut Runner) {
    println!("\n{}", "── Scenario D: Hard kill recovery ──".cyan().bold());

    r.step("D0  Ensure stopped", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("D1  Start daemon", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("D2  Verify Ready", |r| r.assert_ready());
    r.step("D3  Kill -9", |r| { r.run_ok(&["daemon", "kill"])?; Ok(String::new()) });
    r.step("D4  Verify stopped", |r| { r.assert_not_running()?; Ok(String::new()) });
    r.step("D5  Start after kill", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("D6  Verify Ready after kill→start", |r| r.assert_ready());
    r.step("D7  Search works after kill→start", |r| {
        let n = r.search(1000)?;
        if n == 0 { bail!("Zero results after kill→start"); }
        Ok(format!("[{n} rows]"))
    });
    r.step("D8  Cleanup: stop", |r| { r.run_ok(&["daemon", "stop"])?; Ok(String::new()) });
}

fn scenario_e(r: &mut Runner) {
    println!("\n{}", "── Scenario E: Graceful stop → restart cycle ──".cyan().bold());

    r.step("E0  Ensure stopped", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("E1  Start", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("E2  Search", |r| {
        let n = r.search(1000)?;
        if n == 0 { bail!("Zero results"); }
        Ok(format!("[{n} rows]"))
    });
    r.step("E3  Stop", |r| { r.run_ok(&["daemon", "stop"])?; Ok(String::new()) });
    r.step("E4  Verify stopped", |r| { r.assert_not_running()?; Ok(String::new()) });
    r.step("E5  Start again", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("E6  Search after stop→start", |r| {
        let n = r.search(1000)?;
        if n == 0 { bail!("Zero results"); }
        Ok(format!("[{n} rows]"))
    });
    r.step("E7  Cleanup: stop", |r| { r.run_ok(&["daemon", "stop"])?; Ok(String::new()) });
}

fn scenario_f(r: &mut Runner) {
    println!("\n{}", "── Scenario F: Restart preserves data ──".cyan().bold());

    r.step("F0  Ensure stopped", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("F1  Start", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("F2  Verify Ready", |r| r.assert_ready());
    r.step("F3  Search pre-restart", |r| {
        let n = r.search(1000)?;
        if n == 0 { bail!("Zero results"); }
        Ok(format!("[{n} rows]"))
    });
    r.step("F4  Restart", |r| { r.run_ok(&["daemon", "restart"])?; Ok(String::new()) });
    r.step("F5  Verify Ready after restart", |r| r.assert_ready());
    r.step("F6  Search after restart", |r| {
        let n = r.search(1000)?;
        if n == 0 { bail!("Zero results after restart"); }
        Ok(format!("[{n} rows]"))
    });
    r.step("F7  Cleanup: stop", |r| { r.run_ok(&["daemon", "stop"])?; Ok(String::new()) });
}

fn scenario_g(r: &mut Runner) {
    println!("\n{}", "── Scenario G: Double restart ──".cyan().bold());

    r.step("G0  Ensure stopped", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("G1  Start", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("G2  Restart #1", |r| { r.run_ok(&["daemon", "restart"])?; Ok(String::new()) });
    r.step("G3  Verify Ready", |r| r.assert_ready());
    r.step("G4  Restart #2", |r| { r.run_ok(&["daemon", "restart"])?; Ok(String::new()) });
    r.step("G5  Verify Ready", |r| r.assert_ready());
    r.step("G6  Search after 2 restarts", |r| {
        let n = r.search(1000)?;
        if n == 0 { bail!("Zero results"); }
        Ok(format!("[{n} rows]"))
    });
    r.step("G7  Cleanup: stop", |r| { r.run_ok(&["daemon", "stop"])?; Ok(String::new()) });
}

fn scenario_h(r: &mut Runner) {
    println!("\n{}", "── Scenario H: Stats accumulate across searches ──".cyan().bold());

    r.step("H0  Ensure stopped", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("H1  Start", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("H2  Three searches", |r| {
        for i in 1..=3 { r.search(1000)?; print!("[q{i}] "); }
        Ok(String::new())
    });
    r.step("H3  Stats show ≥3 queries", |r| {
        let out = r.run_ok(&["daemon", "stats"])?;
        let count_line = out.lines()
            .find(|l| l.contains("Queries served:"))
            .unwrap_or("");
        let count: u64 = count_line.split_whitespace()
            .filter_map(|w| w.parse().ok())
            .next()
            .unwrap_or(0);
        if count < 3 { bail!("Expected ≥3 queries, got {count}"); }
        Ok(format!("[{count} queries]"))
    });
    r.step("H4  Cleanup: stop", |r| { r.run_ok(&["daemon", "stop"])?; Ok(String::new()) });
}

fn scenario_i(r: &mut Runner) {
    println!("\n{}", "── Scenario I: Kill running → immediate not-running ──".cyan().bold());

    r.step("I0  Ensure stopped", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("I1  Start", |r| { r.start_daemon()?; Ok(String::new()) });
    r.step("I2  Verify Ready", |r| r.assert_ready());
    r.step("I3  Kill", |r| { r.run_ok(&["daemon", "kill"])?; Ok(String::new()) });
    r.step("I4  Status → not running", |r| { r.assert_not_running()?; Ok(String::new()) });
}

fn scenario_j(r: &mut Runner) {
    println!("\n{}", "── Scenario J: Search auto-starts daemon ──".cyan().bold());

    r.step("J0  Ensure stopped", |r| { r.ensure_stopped(); Ok(String::new()) });
    r.step("J1  Verify not running", |r| { r.assert_not_running()?; Ok(String::new()) });
    r.step("J2  Search (should auto-start)", |r| {
        let n = r.search(1000)?;
        if n == 0 { bail!("Zero results from auto-start search"); }
        Ok(format!("[{n} rows, daemon auto-started]"))
    });
    r.step("J3  Verify daemon now running", |r| r.assert_ready());
    r.step("J4  Cleanup: stop", |r| { r.run_ok(&["daemon", "stop"])?; Ok(String::new()) });
}

// ── Startup Timing (COLD → WARM → HOT) ──────────────────────────────────────

/// Delete local MFT index caches so the next startup does a full rebuild.
fn delete_cache() {
    // Windows: %LOCALAPPDATA%\uffs\cache
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let p = std::path::PathBuf::from(&local).join("uffs").join("cache");
        if p.exists() {
            println!("    Deleting cache: {}", p.display());
            let _ = std::fs::remove_dir_all(&p);
        }
    }
    // Windows legacy: %TEMP%\uffs_index_cache
    if let Ok(tmp) = std::env::var("TEMP") {
        let p = std::path::PathBuf::from(&tmp).join("uffs_index_cache");
        if p.exists() {
            println!("    Deleting legacy cache: {}", p.display());
            let _ = std::fs::remove_dir_all(&p);
        }
    }
    // macOS/Linux: XDG cache or ~/Library/Caches
    if let Ok(home) = std::env::var("HOME") {
        for sub in &["Library/Caches/uffs", ".cache/uffs"] {
            let p = std::path::PathBuf::from(&home).join(sub);
            if p.exists() {
                println!("    Deleting cache: {}", p.display());
                let _ = std::fs::remove_dir_all(&p);
            }
        }
    }
}

/// Measure daemon start + first-query timing at a given cache level.
fn measure_startup(r: &Runner, label: &str) -> Result<(u128, u128, usize)> {
    // 1. Start daemon (blocking).
    let mut start_args: Vec<&str> = vec!["daemon", "start"];
    let sa = r.source_args();
    start_args.extend(&sa);
    let t0 = Instant::now();
    let _ = r.run_ok(&start_args);
    let startup_ms = t0.elapsed().as_millis();

    // 2. First query.
    let lim = "1";
    let mut query_args: Vec<&str> = vec![&r.pattern];
    query_args.extend(&sa);
    query_args.extend(["--limit", lim]);
    let t1 = Instant::now();
    let out = r.run_ok(&query_args)?;
    let query_ms = t1.elapsed().as_millis();
    let rows = out.lines().count().saturating_sub(1); // minus header

    println!(
        "    {} startup {}ms + query {}ms = {}ms ({} rows)",
        label, startup_ms, query_ms, startup_ms + query_ms, rows
    );
    Ok((startup_ms, query_ms, rows))
}

fn scenario_k(r: &mut Runner) {
    println!(
        "\n{}",
        "── Scenario K: Startup Timing (COLD → WARM → HOT) ──"
            .cyan()
            .bold()
    );

    // COLD: no daemon, no cache
    r.step("K1  Kill stale daemon", |r| {
        r.ensure_stopped();
        Ok(String::new())
    });
    println!("    Deleting caches for COLD start...");
    delete_cache();

    println!("    COLD (no daemon, no cache)...");
    let cold = match measure_startup(r, "COLD") {
        Ok(v) => v,
        Err(e) => {
            println!("    ↳ {}: {e:#}", "FAILED".red().bold());
            r.failed += 1;
            return;
        }
    };
    r.passed += 1;
    r.timings
        .push(("K2  COLD startup".to_owned(), cold.0 + cold.1));

    // WARM: cache present, no daemon
    r.ensure_stopped();
    std::thread::sleep(Duration::from_secs(1));
    println!("    WARM (cache present, no daemon)...");
    let warm = match measure_startup(r, "WARM") {
        Ok(v) => v,
        Err(e) => {
            println!("    ↳ {}: {e:#}", "FAILED".red().bold());
            r.failed += 1;
            return;
        }
    };
    r.passed += 1;
    r.timings
        .push(("K3  WARM startup".to_owned(), warm.0 + warm.1));

    // HOT: daemon still running from WARM phase
    println!("    HOT  (daemon running)...");
    let hot = match measure_startup(r, "HOT") {
        Ok(v) => v,
        Err(e) => {
            println!("    ↳ {}: {e:#}", "FAILED".red().bold());
            r.failed += 1;
            return;
        }
    };
    r.passed += 1;
    r.timings
        .push(("K4  HOT  startup".to_owned(), hot.0 + hot.1));

    // Summary table
    let cold_total = cold.0 + cold.1;
    let warm_total = warm.0 + warm.1;
    let hot_total = hot.0 + hot.1;
    println!();
    println!("  ┌──────────┬────────────┬────────────┬────────────┬───────────┐");
    println!(
        "  │ {:^8} │ {:>10} │ {:>10} │ {:>10} │ {:>9} │",
        "Phase", "Startup", "Query", "Total", "Speedup"
    );
    println!("  ├──────────┼────────────┼────────────┼────────────┼───────────┤");
    for (label, su, qu, tot) in [
        ("COLD", cold.0, cold.1, cold_total),
        ("WARM", warm.0, warm.1, warm_total),
        ("HOT", hot.0, hot.1, hot_total),
    ] {
        let speedup = if label == "COLD" {
            "—".to_string()
        } else {
            let s = cold_total as f64 / tot.max(1) as f64;
            format!("{s:.1}x")
        };
        println!(
            "  │ {:^8} │ {:>7} ms │ {:>7} ms │ {:>7} ms │ {:>9} │",
            label, su, qu, tot, speedup
        );
    }
    println!("  └──────────┴────────────┴────────────┴────────────┴───────────┘");
    println!();
}

// ── Main ─────────────────────────────────────────────────────────────────────

/// Find the workspace root by walking up from cwd looking for Cargo.toml + .cargo.
fn find_workspace_root() -> std::path::PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut dir = cwd.as_path();
    loop {
        if dir.join("Cargo.toml").exists() && dir.join(".cargo").exists() {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }
    cwd
}

/// Build a fresh release binary and return the path to it (macOS/Linux).
fn ensure_fresh_release_build() -> String {
    let workspace = find_workspace_root();
    let binary_path = workspace.join("target").join("release").join("uffs");

    eprintln!("╔══════════════════════════════════════════════════════════════════╗");
    eprintln!("║  Building fresh release binary...                                ║");
    eprintln!("╚══════════════════════════════════════════════════════════════════╝");
    eprintln!("  Workspace: {}", workspace.display());

    let start = Instant::now();
    let status = Command::new("cargo")
        .args(["build", "--release", "-p", "uffs-cli"])
        .current_dir(&workspace)
        .status();

    match status {
        Ok(s) if s.success() => {
            eprintln!("  ✅ Build completed in {:.1}s", start.elapsed().as_secs_f64());
            eprintln!("  Binary: {}", binary_path.display());
            eprintln!();
        }
        Ok(s) => {
            eprintln!("  ❌ cargo build --release failed (exit {s})");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("  ❌ Failed to run cargo: {e}");
            std::process::exit(1);
        }
    }

    binary_path.to_string_lossy().into_owned()
}

/// On non-Windows, default to ~/uffs_data when no path is given.
fn default_data_dir() -> Option<String> {
    if cfg!(windows) { return None; }
    let home = std::env::var("HOME").ok()?;
    let dir = std::path::PathBuf::from(home).join("uffs_data");
    if dir.is_dir() { Some(dir.to_string_lossy().into_owned()) } else { None }
}

fn default_binary() -> String {
    if cfg!(windows) {
        // Windows: check ~/bin/ first, then target/release/.
        if let Ok(home) = std::env::var("USERPROFILE") {
            let deployed = std::path::PathBuf::from(&home).join("bin").join("uffs.exe");
            if deployed.exists() {
                return deployed.to_string_lossy().into_owned();
            }
        }
        "target\\release\\uffs.exe".to_string()
    } else {
        // Non-Windows: always do a fresh release build so we test the latest code.
        ensure_fresh_release_build()
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let binary = cli.binary.unwrap_or_else(|| default_binary());

    let (source_flag, source_path): (Option<&'static str>, String) = match &cli.path {
        Some(path) => {
            let (flag, val) = detect_data_source(path)?;
            (Some(flag), val)
        }
        None if cfg!(windows) => {
            // Windows: auto-discover live NTFS drives.
            (None, String::new())
        }
        None => {
            // Non-Windows: default to ~/uffs_data if it exists.
            match default_data_dir() {
                Some(dir) => (Some("--data-dir"), dir),
                None => {
                    bail!(
                        "PATH is required on non-Windows platforms.\n\n\
                         On macOS/Linux, provide a data directory or MFT file:\n  \
                         rust-script scripts/dev/daemon-readiness.rs ~/uffs_data\n  \
                         rust-script scripts/dev/daemon-readiness.rs /path/to/C_mft.iocp\n\n\
                         On Windows, omit PATH to auto-discover live NTFS drives."
                    );
                }
            }
        }
    };

    println!("{}", "═══ UFFS Daemon Readiness Verification ═══".bold());
    println!("  binary:    {}", binary);
    match source_flag {
        Some(flag) => println!("  source:    {} {}", flag, source_path),
        None => println!("  source:    live NTFS drives (auto-discover)"),
    }
    println!("  pattern:   {}", cli.pattern);

    let mut r = Runner::new(binary, source_flag, source_path, cli.pattern);

    scenario_a(&mut r);
    scenario_b(&mut r);
    scenario_c(&mut r);
    scenario_d(&mut r);
    scenario_e(&mut r);
    scenario_f(&mut r);
    scenario_g(&mut r);
    scenario_h(&mut r);
    scenario_i(&mut r);
    scenario_j(&mut r);
    scenario_k(&mut r);

    // Final cleanup
    r.ensure_stopped();

    // Summary
    println!();
    println!("─── Timings ───────────────────────────────────────────");
    for (name, ms) in &r.timings {
        println!("  {name:<45} {ms:>6}ms");
    }
    println!();
    let total = r.passed + r.failed;
    if r.failed == 0 {
        println!("{}", format!("══ ALL GOOD ══  {total}/{total} steps passed").green().bold());
    } else {
        println!("{}", format!("══ FAILED ══  {}/{total} steps failed", r.failed).red().bold());
    }

    std::process::exit(if r.failed > 0 { 1 } else { 0 });
}
