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
    }

    fn assert_not_running(&self) -> Result<()> {
        // Brief pause to let daemon fully exit after stop/kill.
        std::thread::sleep(std::time::Duration::from_millis(500));
        let out = self.run_ok(&["daemon", "status"])?;
        if !out.contains("not running") {
            bail!("Expected 'not running', got:\n{out}");
        }
        Ok(())
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

// ── Main ─────────────────────────────────────────────────────────────────────

fn default_binary() -> String {
    // Check ~/bin/ first (deployed), then target/release/ (dev build)
    let (home_var, bin_name) = if cfg!(windows) {
        ("USERPROFILE", "uffs.exe")
    } else {
        ("HOME", "uffs")
    };
    if let Ok(home) = std::env::var(home_var) {
        let deployed = std::path::PathBuf::from(&home).join("bin").join(bin_name);
        if deployed.exists() {
            return deployed.to_string_lossy().into_owned();
        }
    }
    let target = std::path::Path::new("target").join("release").join(bin_name);
    target.to_string_lossy().into_owned()
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let binary = cli.binary.unwrap_or_else(|| default_binary());

    let (source_flag, source_path): (Option<&'static str>, String) = match &cli.path {
        Some(path) => {
            let (flag, val) = detect_data_source(path)?;
            (Some(flag), val)
        }
        None => {
            // No path given — Windows live drive mode.
            if !cfg!(windows) {
                bail!(
                    "PATH is required on non-Windows platforms.\n\n\
                     On macOS/Linux, provide a data directory or MFT file:\n  \
                     rust-script scripts/dev/daemon-readiness.rs ~/uffs_data\n  \
                     rust-script scripts/dev/daemon-readiness.rs /path/to/C_mft.iocp\n\n\
                     On Windows, omit PATH to auto-discover live NTFS drives."
                );
            }
            (None, String::new())
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
