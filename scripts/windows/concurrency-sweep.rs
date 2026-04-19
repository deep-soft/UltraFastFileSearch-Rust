#!/usr/bin/env rust-script
//! ```cargo
//! [dependencies]
//! anyhow = "1"
//! colored = "2"
//! regex = "1"
//! ```
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.
//!
//! concurrency-sweep.rs — Sweep `UFFS_SEARCH_MAX_CONCURRENCY` values and
//! measure each with the api-validation harness.
//!
//! For each value `N` in the sweep list this script runs the *exact*
//! sequence the user drives manually in PowerShell, one step at a time,
//! waiting for each process to return before moving on:
//!
//! 1. `Remove-Item` the on-disk caches (`%LOCALAPPDATA%\uffs\cache` and
//!    `%TEMP%\uffs_index_cache`) to force a clean start.
//! 2. `uffs mcp kill`
//! 3. `uffs daemon kill`
//! 4. Set `UFFS_SEARCH_MAX_CONCURRENCY=N` and run `uffs daemon start`.
//!    On Windows this call blocks until the daemon reports `Ready`
//!    (typical cold-load time: ~80 s for 26 M records over 7 drives).
//!    No polling is needed — we just wait for the process to return.
//! 5. Read the `search concurrency retuned` line from `uffsd.log` and
//!    verify `source="env"` and `target=N`.
//! 6. Run one warm-up api-validation (populates the agg cache).
//! 7. Run one measured api-validation and parse its Timing Breakdown.
//! 8. Capture `uffs daemon stats` for cache hit-rate and avg query time.
//!
//! A summary table is printed at the end.
//!
//! Usage:
//!   rust-script scripts/windows/concurrency-sweep.rs
//!   rust-script scripts/windows/concurrency-sweep.rs 3 6 12
//!   rust-script scripts/windows/concurrency-sweep.rs --skip-warmup 6 12
//!   rust-script scripts/windows/concurrency-sweep.rs --no-wipe 6 12

use std::env;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use colored::Colorize;
use regex::Regex;

// ── Configuration ────────────────────────────────────────────────────────────

/// Default sweep values when no positional args are supplied.
const DEFAULT_SWEEP: &[usize] = &[3, 6, 8, 12, 16, 24];

/// Seconds to sleep after `daemon kill` so the socket / PID file are gone
/// before we spawn a new daemon.
const KILL_SETTLE_SECS: u64 = 2;

// ── Environment helpers ──────────────────────────────────────────────────────

/// Resolve `~/bin/uffs[.exe]` — the canonical user-installed binary path
/// on both Windows and Unix.
fn uffs_bin() -> PathBuf {
    let home = env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
        .expect("USERPROFILE or HOME must be set");
    if cfg!(windows) {
        home.join("bin").join("uffs.exe")
    } else {
        home.join("bin").join("uffs")
    }
}

/// Path to the daemon's on-disk log file (default location on each OS).
fn uffsd_log_path() -> Option<PathBuf> {
    if cfg!(windows) {
        env::var_os("LOCALAPPDATA")
            .map(|p| PathBuf::from(p).join("uffs").join("logs").join("uffsd.log"))
    } else {
        env::var_os("HOME")
            .map(|p| PathBuf::from(p).join(".uffs").join("logs").join("uffsd.log"))
    }
}

/// Paths that should be wiped before each iteration to force a cold start.
fn cache_paths_to_wipe() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if cfg!(windows) {
        if let Some(p) = env::var_os("LOCALAPPDATA") {
            v.push(PathBuf::from(p).join("uffs").join("cache"));
        }
        if let Some(p) = env::var_os("TEMP") {
            v.push(PathBuf::from(p).join("uffs_index_cache"));
        }
    } else {
        if let Some(p) = env::var_os("HOME") {
            v.push(PathBuf::from(p).join(".uffs").join("cache"));
        }
    }
    v
}

// ── Subprocess helpers ───────────────────────────────────────────────────────

/// Run `uffs <subcmd> kill`, inheriting stdout/stderr so the user sees
/// the same messages they would see running it manually.  Errors are
/// swallowed because an already-dead process is a normal precondition.
fn kill(subcmd: &str) {
    let _ = Command::new(uffs_bin())
        .args([subcmd, "kill"])
        .status();
}

/// Run `uffs daemon start` with `UFFS_SEARCH_MAX_CONCURRENCY=N` in the
/// environment.  This call inherits stdout/stderr and **blocks until
/// the daemon reports `Ready`** (or the CLI gives up).  No polling —
/// the child `uffs` process does the wait internally and prints
/// "Daemon started and ready." when the MFT load is complete.
///
/// # Errors
/// Returns an error if `uffs daemon start` exits non-zero.
fn start_daemon(n: usize) -> Result<()> {
    let status = Command::new(uffs_bin())
        .args(["daemon", "start"])
        .env("UFFS_SEARCH_MAX_CONCURRENCY", n.to_string())
        .status()
        .context("failed to spawn `uffs daemon start`")?;
    if !status.success() {
        bail!("`uffs daemon start` exited with status {status}");
    }
    Ok(())
}

/// Run the api-validation harness and capture its combined stdout + stderr.
fn run_validation(repo_root: &PathBuf) -> Result<String> {
    let script = repo_root
        .join("scripts")
        .join("windows")
        .join("api-validation.rs");
    let out = Command::new("rust-script")
        .arg(&script)
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("failed to spawn rust-script {}", script.display()))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(text)
}

/// Capture `uffs daemon stats` as plain text.
fn daemon_stats_text() -> String {
    Command::new(uffs_bin())
        .args(["daemon", "stats"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Find the workspace root by walking up from `cwd` looking for a folder
/// that contains both `Cargo.toml` and a `crates/` directory.
fn find_repo_root() -> Result<PathBuf> {
    let start = env::current_dir()?;
    for anc in start.ancestors() {
        if anc.join("Cargo.toml").exists() && anc.join("crates").is_dir() {
            return Ok(anc.to_path_buf());
        }
    }
    bail!(
        "could not find repo root from {} (expected Cargo.toml + crates/)",
        start.display()
    )
}

// ── Output parsing ───────────────────────────────────────────────────────────

#[derive(Default, Debug)]
struct RunMetrics {
    wall_ms: Option<u64>,
    sum_ms: Option<u64>,
    avg_ms: Option<u64>,
    slowest_ms: Option<u64>,
    slowest_name: Option<String>,
    passed: Option<u32>,
    total: Option<u32>,
}

fn parse_metrics(output: &str) -> RunMetrics {
    // These regexes target the "Timing Breakdown" block produced by
    // api-validation.rs — they are stable across Windows/Mac.
    let re_wall = Regex::new(r"Tests wall time:\s+(\d+)ms").unwrap();
    let re_sum = Regex::new(r"Tests sum time:\s+(\d+)ms").unwrap();
    let re_avg = Regex::new(r"Tests avg time:\s+(\d+)ms").unwrap();
    let re_slow = Regex::new(r"Slowest test:\s+(\d+)ms\s+(.+)").unwrap();
    // api-validation prints either "<P>/<T> passed" on success or
    // "<F>/<T> FAILED" on failure — handle both.
    let re_pass = Regex::new(r"(\d+)/(\d+)\s+passed").unwrap();
    let re_fail = Regex::new(r"(\d+)/(\d+)\s+FAILED").unwrap();

    let mut m = RunMetrics::default();
    if let Some(c) = re_wall.captures(output) {
        m.wall_ms = c[1].parse().ok();
    }
    if let Some(c) = re_sum.captures(output) {
        m.sum_ms = c[1].parse().ok();
    }
    if let Some(c) = re_avg.captures(output) {
        m.avg_ms = c[1].parse().ok();
    }
    if let Some(c) = re_slow.captures(output) {
        m.slowest_ms = c[1].parse().ok();
        m.slowest_name = Some(c[2].trim().to_owned());
    }
    if let Some(c) = re_pass.captures(output) {
        m.passed = c[1].parse().ok();
        m.total = c[2].parse().ok();
    } else if let Some(c) = re_fail.captures(output) {
        // Failed path: derive passed as total - failed.
        let failed: Option<u32> = c[1].parse().ok();
        let total: Option<u32> = c[2].parse().ok();
        m.total = total;
        m.passed = match (total, failed) {
            (Some(t), Some(f)) => Some(t.saturating_sub(f)),
            _ => None,
        };
    }
    m
}

fn parse_cache_line(stats_text: &str) -> Option<String> {
    stats_text
        .lines()
        .find(|l| l.contains("Agg cache:"))
        .map(|s| s.trim().to_owned())
}

fn parse_avg_query(stats_text: &str) -> Option<String> {
    stats_text
        .lines()
        .find(|l| l.contains("Avg query time:"))
        .map(|s| s.trim().to_owned())
}

/// Read the last `search concurrency retuned` line from the daemon log.
fn last_tune_line() -> Option<String> {
    let path = uffsd_log_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    text.lines()
        .filter(|l| l.contains("search concurrency retuned"))
        .last()
        .map(|s| s.to_owned())
}

// ── Argument parsing ─────────────────────────────────────────────────────────

#[derive(Debug)]
struct Args {
    values: Vec<usize>,
    skip_warmup: bool,
    wipe: bool,
}

fn parse_args() -> Args {
    let mut raw: Vec<String> = env::args().skip(1).collect();

    let mut skip_warmup = false;
    let mut wipe = true;

    // Extract flags in a small state machine; positional args remain in raw.
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--skip-warmup" => {
                skip_warmup = true;
                raw.remove(i);
            }
            "--no-wipe" => {
                wipe = false;
                raw.remove(i);
            }
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            _ => i += 1,
        }
    }

    let mut values: Vec<usize> = raw.iter().filter_map(|s| s.parse().ok()).collect();
    if values.is_empty() {
        values = DEFAULT_SWEEP.to_vec();
    }

    Args {
        values,
        skip_warmup,
        wipe,
    }
}

fn print_usage() {
    println!("Usage: rust-script scripts/windows/concurrency-sweep.rs [FLAGS] [N ...]");
    println!();
    println!("Flags:");
    println!("  --skip-warmup      Skip the warm-up validation run per iteration");
    println!("  --no-wipe          Do not delete the on-disk cache dirs between runs");
    println!("  -h, --help         Print this help and exit");
    println!();
    println!("Positional args are the sweep values (default: {:?}).", DEFAULT_SWEEP);
}

// ── Rendering ────────────────────────────────────────────────────────────────

fn fmt_ms(v: Option<u64>) -> String {
    v.map_or_else(|| "   -".to_owned(), |n| format!("{:>5}", n))
}

fn print_summary(rows: &[(usize, RunMetrics, String, String)]) {
    println!();
    println!("{}", "═══════════════════ SUMMARY ═══════════════════".yellow().bold());
    println!(
        "{:>4}  {:>7}  {:>7}  {:>7}  {:>7}  {:>7}  {}",
        "N".bold(),
        "wall".bold(),
        "sum".bold(),
        "avg".bold(),
        "slow".bold(),
        "pass".bold(),
        "cache".bold(),
    );
    println!("{}", "─".repeat(85));
    for (n, m, cache, _avg_q) in rows {
        let pass = match (m.passed, m.total) {
            (Some(p), Some(t)) => format!("{}/{}", p, t),
            _ => "-".to_owned(),
        };
        println!(
            "{:>4}  {}ms  {}ms  {}ms  {}ms  {:>7}  {}",
            n,
            fmt_ms(m.wall_ms),
            fmt_ms(m.sum_ms),
            fmt_ms(m.avg_ms),
            fmt_ms(m.slowest_ms),
            pass,
            cache.replace("Agg cache:", "").trim(),
        );
    }
    println!("{}", "─".repeat(85));
}

// ── Main sweep loop ──────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let args = parse_args();
    let repo_root = find_repo_root()?;

    println!("{}", "UFFS concurrency sweep".bold().cyan());
    println!("  Binary       : {}", uffs_bin().display());
    println!("  Repo root    : {}", repo_root.display());
    println!("  Sweep values : {:?}", args.values);
    println!("  Wipe caches  : {}", args.wipe);
    println!("  Skip warm-up : {}", args.skip_warmup);
    if let Some(p) = uffsd_log_path() {
        println!("  Daemon log   : {}", p.display());
    }

    let mut rows: Vec<(usize, RunMetrics, String, String)> = Vec::new();

    for (idx, &n) in args.values.iter().enumerate() {
        println!();
        println!(
            "{}",
            format!(
                "═══════════════ N={n} ({}/{}) ═══════════════",
                idx + 1,
                args.values.len()
            )
            .cyan()
            .bold()
        );

        // 1. Wipe caches.
        if args.wipe {
            for p in cache_paths_to_wipe() {
                let _ = std::fs::remove_dir_all(&p);
                println!("  wiped  : {}", p.display());
            }
        } else {
            println!("  wipe   : skipped (--no-wipe)");
        }

        // 2. Kill mcp + daemon (ignore errors; may already be down).
        kill("mcp");
        kill("daemon");
        thread::sleep(Duration::from_secs(KILL_SETTLE_SECS));

        // 3. Start daemon with UFFS_SEARCH_MAX_CONCURRENCY=N.
        //    `uffs daemon start` blocks until the daemon reports Ready
        //    (or gives up), so we just wait for the child process to
        //    return — no polling loop required.
        println!("  start  : UFFS_SEARCH_MAX_CONCURRENCY={n}");
        let t0 = Instant::now();
        if let Err(err) = start_daemon(n) {
            println!("  {}", format!("FAILED: {err}").red());
            println!("  {} N={n}", "skipping".yellow());
            continue;
        }
        println!(
            "  {} ({:.1}s)",
            "daemon Ready".green(),
            t0.elapsed().as_secs_f64()
        );

        // 4. Confirm the env override landed in the daemon.
        if let Some(tune) = last_tune_line() {
            println!("  tune   : {}", tune.trim().dimmed());
            let env_ok = tune.contains("source=\"env\"") && tune.contains(&format!("target={n}"));
            if !env_ok {
                println!(
                    "  {}",
                    "WARNING: tune log does not confirm env override — result may not reflect N".yellow()
                );
            }
        } else {
            println!("  {}", "tune   : (log line not found — env confirmation skipped)".yellow());
        }

        // 5. Warm-up run (populates agg cache).
        if !args.skip_warmup {
            print!("  warm-up: ");
            std::io::Write::flush(&mut std::io::stdout()).ok();
            let t0 = Instant::now();
            match run_validation(&repo_root) {
                Ok(text) => {
                    let m = parse_metrics(&text);
                    println!(
                        "done in {:.1}s (wall={}ms)",
                        t0.elapsed().as_secs_f64(),
                        m.wall_ms.map_or_else(|| "?".to_owned(), |v| v.to_string())
                    );
                }
                Err(err) => {
                    println!("{}", format!("FAILED: {err}").red());
                    continue;
                }
            }
        }

        // 6. Measured run.
        print!("  measure: ");
        std::io::Write::flush(&mut std::io::stdout()).ok();
        let t0 = Instant::now();
        let output = match run_validation(&repo_root) {
            Ok(o) => o,
            Err(err) => {
                println!("{}", format!("FAILED: {err}").red());
                continue;
            }
        };
        let metrics = parse_metrics(&output);
        println!(
            "done in {:.1}s",
            t0.elapsed().as_secs_f64()
        );

        // 7. Stats.
        let stats_text = daemon_stats_text();
        let cache = parse_cache_line(&stats_text).unwrap_or_default();
        let avg_q = parse_avg_query(&stats_text).unwrap_or_default();

        if let (Some(w), Some(a), Some(s), Some(name)) = (
            metrics.wall_ms,
            metrics.avg_ms,
            metrics.slowest_ms,
            metrics.slowest_name.as_ref(),
        ) {
            println!(
                "  result : wall={}ms  avg={}ms  slowest={}ms ({})",
                w, a, s, name
            );
        }
        if !avg_q.is_empty() {
            println!("  {}", avg_q.dimmed());
        }
        if !cache.is_empty() {
            println!("  {}", cache.dimmed());
        }

        rows.push((n, metrics, cache, avg_q));
    }

    print_summary(&rows);
    Ok(())
}
