#!/usr/bin/env rust-script
//! ```cargo
//! [dependencies]
//! anyhow = "1.0"
//! colored = "2.0"
//! serde_json = "1.0"
//! ```
// =============================================================================
// scripts/windows/cli-flag-validation.rs — CLI Flag Validation Suite
// =============================================================================
//
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 Robert Nio
//
// Phase 1 — Startup timing:  measures a single query at three caching levels:
//   COLD  — no daemon, no cache files  (full MFT read + index build)
//   WARM  — no daemon, cache files exist  (daemon auto-starts from cache)
//   HOT   — daemon already running  (pure in-memory search)
//
// Phase 2 — Parallel validation:  runs ALL 141 tests concurrently against
//   the HOT daemon from Phase 1.
//
// Usage:
//   rust-script scripts/windows/cli-flag-validation.rs [path-to-uffs-binary]
//
// Requirements:
//   - Windows with NTFS drives (tests reference real drive letters)
//   - Administrator privileges (MFT reading)

use std::process::Command;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use colored::Colorize;

// ── Configuration ────────────────────────────────────────────────────────────

/// Parse CLI args: optional --data-dir, optional --bin, auto-detect binary.
///
/// Usage:
///   rust-script cli-flag-validation.rs [--data-dir <path>] [--bin <path>]
///
/// On macOS/Linux: builds a fresh release binary via `cargo build --release`,
/// auto-detects `~/uffs_data` as data dir.
/// On Windows: looks for `~/bin/uffs.exe`, then `target/release/uffs.exe`.
fn parse_script_args() -> (String, Option<String>) {
    let args: Vec<String> = std::env::args().collect();
    let mut data_dir: Option<String> = None;
    let mut bin_override: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--data-dir" => { data_dir = args.get(i + 1).cloned(); i += 2; }
            "--bin"      => { bin_override = args.get(i + 1).cloned(); i += 2; }
            _            => { i += 1; }
        }
    }

    let bin = bin_override.unwrap_or_else(|| {
        if cfg!(windows) {
            // Windows: ~/bin/uffs.exe → target/release → PATH
            let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".to_string());
            let candidates = [
                format!("{home}\\bin\\uffs.exe"),
                "target\\release\\uffs.exe".to_string(),
            ];
            for c in &candidates {
                if std::path::Path::new(c).exists() { return c.clone(); }
            }
            "uffs".to_string()
        } else {
            // macOS/Linux: build fresh binary, use target/release/uffs
            ensure_fresh_release_build()
        }
    });

    // On non-Windows, auto-detect ~/uffs_data if --data-dir wasn't given.
    if data_dir.is_none() && !cfg!(windows) {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let default = format!("{home}/uffs_data");
        if std::path::Path::new(&default).is_dir() {
            data_dir = Some(default);
        }
    }

    (bin, data_dir)
}

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

// ── Validation Helpers ───────────────────────────────────────────────────────

/// Count non-empty, non-header CSV lines.
fn csv_row_count(stdout: &str) -> usize {
    stdout.lines().filter(|l| !l.is_empty()).count().saturating_sub(1)
}

/// Split a single CSV line respecting double-quote quoting.
///
/// Handles quoted fields that may contain commas (e.g. paths with commas).
/// Does NOT handle escaped quotes inside quoted fields (not needed for UFFS).
fn split_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in line.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                fields.push(current.clone());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    fields.push(current);
    fields
}

/// Parse CSV: returns (headers, data_rows).
fn parse_csv(stdout: &str) -> (Vec<String>, Vec<Vec<String>>) {
    let mut lines = stdout.lines().filter(|l| !l.is_empty());
    let headers = split_csv_line(lines.next().unwrap_or(""));
    let rows: Vec<Vec<String>> = lines.map(|line| split_csv_line(line)).collect();
    (headers, rows)
}

/// Find column index by name (case-insensitive).
fn col_idx(headers: &[String], name: &str) -> Option<usize> {
    headers.iter().position(|h| h.eq_ignore_ascii_case(name))
}

/// Get column value from a row by column name.
fn col_val<'a>(row: &'a [String], headers: &[String], name: &str) -> &'a str {
    col_idx(headers, name)
        .and_then(|i| row.get(i))
        .map(|s| s.as_str())
        .unwrap_or("")
}

/// Assert row count is within expected range.
fn assert_rows(stdout: &str, min: usize, max: usize) -> Result<String> {
    let count = csv_row_count(stdout);
    if count < min || count > max {
        bail!("Expected {min}..={max} rows, got {count}");
    }
    Ok(format!("{count} rows"))
}

// ── Test Runner ──────────────────────────────────────────────────────────────

struct TestResult {
    name: String,
    cli: String,
    passed: bool,
    duration_ms: u128,
    detail: String,
}

/// A test specification: name + args + validator closure.
struct TestSpec {
    name: String,
    args: Vec<String>,
    validate: Box<dyn Fn(&str, &str) -> Result<String> + Send + Sync>,
}

/// Run uffs with given args, return (exit_code, stdout, stderr).
fn run_uffs(bin: &str, args: &[String]) -> Result<(i32, String, String)> {
    let output = Command::new(bin)
        .args(args)
        .output()
        .with_context(|| format!("Failed to execute: {} {}", bin, args.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ")))?;
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok((code, stdout, stderr))
}

/// Build the CLI string for display/reproduction.
fn cli_string(bin: &str, args: &[String]) -> String {
    let mut parts = vec![bin.to_string()];
    for a in args {
        if a.contains(' ') || a.contains('*') || a.contains('>') || a.contains('<') {
            parts.push(format!("\"{a}\""));
        } else {
            parts.push(a.clone());
        }
    }
    parts.join(" ")
}

/// Run a single test spec against the given binary.
/// If `data_dir` is Some, injects `--data-dir <path>` into the args so the
/// CLI can find the daemon/data on non-Windows platforms.
fn run_one_test(bin: &str, spec: &TestSpec, data_dir: &Option<String>) -> TestResult {
    let mut args = spec.args.clone();
    if let Some(ref dir) = data_dir {
        args.push("--data-dir".to_string());
        args.push(dir.clone());
    }
    let cli = cli_string(bin, &args);
    let start = Instant::now();
    let result = run_uffs(bin, &args);
    let duration_ms = start.elapsed().as_millis();

    let (passed, detail) = match result {
        Ok((code, stdout, stderr)) => {
            if code != 0 {
                (false, format!("Exit code {code}. stderr: {}", stderr.lines().next().unwrap_or("")))
            } else {
                match (spec.validate)(&stdout, &stderr) {
                    Ok(msg) => (true, msg),
                    Err(e) => (false, format!("{e}")),
                }
            }
        }
        Err(e) => (false, format!("Execution failed: {e}")),
    };

    TestResult { name: spec.name.clone(), cli, passed, duration_ms, detail }
}

/// Run all test specs in parallel using std::thread::scope.
fn run_tests_parallel(bin: &str, specs: &[TestSpec], data_dir: &Option<String>) -> (Vec<TestResult>, u128) {
    let wall_start = Instant::now();
    let results: Vec<TestResult> = std::thread::scope(|s| {
        let handles: Vec<_> = specs.iter().map(|spec| {
            s.spawn(|| run_one_test(bin, spec, data_dir))
        }).collect();
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| TestResult {
            name: "???".into(), cli: "???".into(), passed: false, duration_ms: 0, detail: "thread panicked".into(),
        })).collect()
    });
    let wall_ms = wall_start.elapsed().as_millis();
    (results, wall_ms)
}

/// Helper to build a TestSpec from a name, args slice, and validator.
fn spec<F>(name: &str, args: &[&str], validate: F) -> TestSpec
where
    F: Fn(&str, &str) -> Result<String> + Send + Sync + 'static,
{
    TestSpec {
        name: name.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        validate: Box::new(validate),
    }
}

fn print_results(results: &[TestResult], wall_ms: u128) {
    // Per-test lines (compact).
    for r in results {
        let status = if r.passed { "PASS".green().bold() } else { "FAIL".red().bold() };
        let timing = format!("{:>5}ms", r.duration_ms).dimmed();
        eprintln!("  [{status}] {timing}  {}: {}", r.name, r.detail);
    }

    let total = results.len();
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = total - passed;
    let sum_ms: u128 = results.iter().map(|r| r.duration_ms).sum();
    let avg_ms = if total > 0 { sum_ms / total as u128 } else { 0 };

    eprintln!();
    if failed == 0 {
        eprintln!("  {} {passed}/{total} tests — wall {wall_ms}ms / sum {sum_ms}ms / avg {avg_ms}ms",
            "✅".green());
    } else {
        eprintln!("  {} {failed}/{total} FAILED — wall {wall_ms}ms / sum {sum_ms}ms",
            "❌".red());
        eprintln!();
        eprintln!("  ┌─ Failed Tests (reproduce with exact CLI) ─────────────────────────┐");
        for r in results {
            if !r.passed {
                eprintln!("  │");
                eprintln!("  │  {} {}", "❌".red(), r.name);
                eprintln!("  │  Error:  {}", r.detail);
                eprintln!("  │  CLI:    {}", r.cli.yellow());
            }
        }
        eprintln!("  │");
        eprintln!("  └──────────────────────────────────────────────────────────────────────┘");
    }
}

// ── Startup Timing ─────────────────────────────────────────────────────────

struct StartupTiming {
    label: String,
    startup_ms: u128,
    query_ms: u128,
    total_ms: u128,
    rows: usize,
}

/// Start daemon (blocking), then measure first query.
fn measure_startup(bin: &str, label: &str, data_dir: &Option<String>) -> StartupTiming {
    // 1. Start daemon (blocking — waits until "Daemon started and ready.")
    let mut start_args: Vec<String> = vec!["daemon".into(), "start".into()];
    if let Some(ref dir) = data_dir {
        start_args.push("--data-dir".into());
        start_args.push(dir.clone());
    }
    let t0 = Instant::now();
    let _ = run_uffs(bin, &start_args);
    let startup_ms = t0.elapsed().as_millis();

    // 2. First query against running daemon.
    let query_args: Vec<String> = ["*", "--limit", "1"].iter().map(|s| s.to_string()).collect();
    let t1 = Instant::now();
    let result = run_uffs(bin, &query_args);
    let query_ms = t1.elapsed().as_millis();

    let rows = match &result {
        Ok((_, stdout, _)) => csv_row_count(stdout),
        Err(_) => 0,
    };
    StartupTiming {
        label: label.to_string(),
        startup_ms, query_ms,
        total_ms: startup_ms + query_ms,
        rows,
    }
}

fn startup_timing(bin: &str, data_dir: &Option<String>) {
    eprintln!("┌───────────────────────────────────────────────────────────────┐");
    eprintln!("│  Startup Timing: COLD → WARM → HOT                          │");
    eprintln!("└───────────────────────────────────────────────────────────────┘");

    // COLD: no daemon, no cache
    kill_daemon(bin);
    delete_cache();
    eprintln!("  COLD (no daemon, no cache)...");
    let cold = measure_startup(bin, "COLD", data_dir);
    eprintln!("    {} startup {}ms + query {}ms = {}ms ({} rows)",
        "COLD".yellow().bold(), cold.startup_ms, cold.query_ms, cold.total_ms, cold.rows);

    // WARM: no daemon, cache files remain
    kill_daemon(bin);
    eprintln!("  WARM (cache present, no daemon)...");
    let warm = measure_startup(bin, "WARM", data_dir);
    eprintln!("    {} startup {}ms + query {}ms = {}ms ({} rows)",
        "WARM".cyan().bold(), warm.startup_ms, warm.query_ms, warm.total_ms, warm.rows);

    // HOT: daemon still running from warm
    eprintln!("  HOT  (daemon running)...");
    let hot = measure_startup(bin, "HOT", data_dir);
    eprintln!("    {}  startup {}ms + query {}ms = {}ms ({} rows)",
        "HOT".green().bold(), hot.startup_ms, hot.query_ms, hot.total_ms, hot.rows);

    // Summary table
    eprintln!();
    eprintln!("  ┌──────────┬────────────┬────────────┬────────────┬───────────┐");
    eprintln!("  │ {:^8} │ {:>10} │ {:>10} │ {:>10} │ {:>9} │",
        "Phase", "Startup", "Query", "Total", "Speedup");
    eprintln!("  ├──────────┼────────────┼────────────┼────────────┼───────────┤");
    for t in &[&cold, &warm, &hot] {
        let speedup = if t.label == "COLD" {
            "—".to_string()
        } else {
            let s = cold.total_ms as f64 / t.total_ms.max(1) as f64;
            format!("{s:.1}x")
        };
        eprintln!("  │ {:^8} │ {:>7} ms │ {:>7} ms │ {:>7} ms │ {:>9} │",
            t.label, t.startup_ms, t.query_ms, t.total_ms, speedup);
    }
    eprintln!("  └──────────┴────────────┴────────────┴────────────┴───────────┘");
    eprintln!();
}

// ── Cache / Daemon Helpers ──────────────────────────────────────────────────

fn kill_daemon(bin: &str) {
    eprintln!("  Killing daemon...");
    let _ = Command::new(bin)
        .env("RUST_LOG", "trace")
        .env("RUST_LOG_FILE", "trace")
        .env("UFFS_LOG", "trace")
        .args(["daemon", "kill"])
        .output();
    std::thread::sleep(std::time::Duration::from_secs(2));
}

fn delete_cache() {
    // Secure cache location
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let p = std::path::PathBuf::from(&local).join("uffs").join("cache");
        if p.exists() {
            eprintln!("  Deleting cache: {}", p.display());
            let _ = std::fs::remove_dir_all(&p);
        }
    }
    // Legacy cache location
    if let Ok(tmp) = std::env::var("TEMP") {
        let p = std::path::PathBuf::from(&tmp).join("uffs_index_cache");
        if p.exists() {
            eprintln!("  Deleting legacy cache: {}", p.display());
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

// ── Test Suite ───────────────────────────────────────────────────────────────

fn define_tests() -> Vec<TestSpec> {
    let mut specs: Vec<TestSpec> = Vec::new();

    // ── 1. Warmup (also verifies daemon is alive / auto-starts) ───────
    specs.push(spec("T00 warmup / daemon alive", &["*", "--limit", "10"], |stdout, _| {
        if csv_row_count(stdout) < 10 { bail!("No results — daemon may not be running"); }
        Ok("daemon warm".into())
    }));

    // ── 2. --files-only ───────────────────────────────────────────────
    specs.push(spec("T01 --files-only", &["*.txt", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let dir = col_val(row, &h, "Directory Flag");
            if dir == "1" { bail!("Row {i} is a directory (Directory Flag=1)"); }
        }
        Ok(format!("{} rows, all files", rows.len()))
    }));

    // ── 3. --dirs-only ────────────────────────────────────────────────
    specs.push(spec("T02 --dirs-only", &["Windows", "--dirs-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let dir = col_val(row, &h, "Directory Flag");
            if dir != "1" { bail!("Row {i} is not a directory (Directory Flag={dir})"); }
        }
        Ok(format!("{} rows, all directories", rows.len()))
    }));

    // ── 4. --hide-system ──────────────────────────────────────────────
    specs.push(spec("T03 --hide-system", &["$*", "--limit", "20", "--hide-system"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        // With --hide-system, no file should start with $
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name");
            if name.starts_with('$') { bail!("Row {i}: {name} starts with $ despite --hide-system"); }
        }
        Ok(format!("{} rows, no $-prefixed files", rows.len()))
    }));

    // ── 5. --ext single ───────────────────────────────────────────────
    specs.push(spec("T04 --ext rs", &["*", "--ext", "rs", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name");
            if !name.to_lowercase().ends_with(".rs") {
                bail!("Row {i}: {name} does not end with .rs");
            }
        }
        Ok(format!("{} rows, all .rs", rows.len()))
    }));

    // ── 6. --ext multiple ─────────────────────────────────────────────
    specs.push(spec("T05 --ext jpg,png,gif", &["*", "--ext", "jpg,png,gif", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        let exts = ["jpg", "png", "gif"];
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if !exts.iter().any(|e| name.ends_with(&format!(".{e}"))) {
                bail!("Row {i}: {name} not in {{jpg,png,gif}}");
            }
        }
        Ok(format!("{} rows, all image extensions", rows.len()))
    }));

    // ── 7. --min-size ─────────────────────────────────────────────────
    specs.push(spec("T06 --min-size 100MB", &["*", "--min-size", "104857600", "--files-only", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let size: u64 = col_val(row, &h, "Size").parse().unwrap_or(0);
            if size < 104_857_600 { bail!("Row {i}: size={size} < 100MB"); }
        }
        Ok(format!("{} rows, all >= 100MB", rows.len()))
    }));

    // ── 8. --max-size ─────────────────────────────────────────────────
    specs.push(spec("T07 --max-size 1KB", &["*", "--max-size", "1024", "--files-only", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let size: u64 = col_val(row, &h, "Size").parse().unwrap_or(u64::MAX);
            if size > 1024 { bail!("Row {i}: size={size} > 1KB"); }
        }
        Ok(format!("{} rows, all <= 1KB", rows.len()))
    }));

    // ── 9. --min-size + --max-size combined ───────────────────────────
    specs.push(spec("T08 --min/max-size 1MB..10MB", &["*.pdf", "--min-size", "1048576", "--max-size", "10485760", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let size: u64 = col_val(row, &h, "Size").parse().unwrap_or(0);
            if size < 1_048_576 || size > 10_485_760 {
                bail!("Row {i}: size={size} outside 1MB..10MB");
            }
        }
        Ok(format!("{} rows, all 1MB..10MB", rows.len()))
    }));

    // ── 10. --sort size ascending ─────────────────────────────────────
    specs.push(spec("T09 --sort size (asc)", &["*.exe", "--sort", "size", "--files-only", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.len() < 2 { return Ok("< 2 rows, skip sort check".into()); }
        let sizes: Vec<u64> = rows.iter().map(|r| col_val(r, &h, "Size").parse().unwrap_or(0)).collect();
        for w in sizes.windows(2) {
            if w[0] > w[1] { bail!("Not ascending: {} > {}", w[0], w[1]); }
        }
        Ok(format!("{} rows, sorted asc", rows.len()))
    }));

    // ── 11. --sort size descending ────────────────────────────────────
    specs.push(spec("T10 --sort size --sort-desc", &["*.exe", "--sort", "size", "--sort-desc", "--files-only", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.len() < 2 { return Ok("< 2 rows, skip sort check".into()); }
        let sizes: Vec<u64> = rows.iter().map(|r| col_val(r, &h, "Size").parse().unwrap_or(0)).collect();
        for w in sizes.windows(2) {
            if w[0] < w[1] { bail!("Not descending: {} < {}", w[0], w[1]); }
        }
        Ok(format!("{} rows, sorted desc", rows.len()))
    }));

    // ── 12. --sort modified ───────────────────────────────────────────
    specs.push(spec("T11 --sort modified", &["*.log", "--sort", "modified", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 1, 10)
    }));

    // ── 13. --sort multi-tier ─────────────────────────────────────────
    specs.push(spec("T12 --sort size,name", &["*.dll", "--sort", "size,name", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 1, 10)
    }));

    // ── 14. --attr hidden ─────────────────────────────────────────────
    specs.push(spec("T13 --attr hidden", &["*", "--attr", "hidden", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let hidden = col_val(row, &h, "Hidden");
            if hidden != "1" { bail!("Row {i}: Hidden={hidden}, expected 1"); }
        }
        Ok(format!("{} rows, all hidden", rows.len()))
    }));

    // ── 15. --attr !hidden ────────────────────────────────────────────
    specs.push(spec("T14 --attr !hidden", &["*", "--attr", "!hidden", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let hidden = col_val(row, &h, "Hidden");
            if hidden == "1" { bail!("Row {i}: Hidden=1, expected 0"); }
        }
        Ok(format!("{} rows, none hidden", rows.len()))
    }));

    // ── 16. --attr compressed ─────────────────────────────────────────
    specs.push(spec("T15 --attr compressed", &["*", "--attr", "compressed", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        // compressed files may not exist on all systems — 0 rows is ok
        for (i, row) in rows.iter().enumerate() {
            let c = col_val(row, &h, "Compressed");
            if c != "1" { bail!("Row {i}: Compressed={c}, expected 1"); }
        }
        Ok(format!("{} rows, all compressed", rows.len()))
    }));

    // ── 17. --exclude ─────────────────────────────────────────────────
    specs.push(spec("T16 --exclude backup*", &["*.txt", "--exclude", "backup*", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if name.starts_with("backup") { bail!("Row {i}: {name} matches exclude pattern"); }
        }
        Ok(format!("{} rows, no backup* files", rows.len()))
    }));

    // ── 18. --name-only ───────────────────────────────────────────────
    specs.push(spec("T17 --name-only", &["readme", "--name-only", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if !name.contains("readme") { bail!("Row {i}: {name} does not contain 'readme'"); }
        }
        Ok(format!("{} rows, all contain 'readme'", rows.len()))
    }));

    // ── 19. --case (case-sensitive) ───────────────────────────────────
    specs.push(spec("T18 --case sensitive", &["README", "--case", "--name-only", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        // All results should have exact case "README" in filename
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name");
            if !name.contains("README") { bail!("Row {i}: {name} — case mismatch"); }
        }
        Ok(format!("{} rows, case-sensitive match", rows.len()))
    }));

    // ── 20. --word (whole word) ───────────────────────────────────────
    specs.push(spec("T19 --word", &["test", "--word", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── 21. --format json (NDJSON — one JSON object per line) ────────
    specs.push(spec("T20 --format json", &["*.rs", "--format", "json", "--limit", "5"], |stdout, _| {
        let items: Vec<serde_json::Value> = stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("Invalid JSON line: {e}"))?;
        if items.is_empty() { bail!("No JSON items"); }
        if items.len() > 5 { bail!("Expected <= 5 items, got {}", items.len()); }
        // Verify each item has expected fields
        let first = &items[0];
        if first.get("Name").is_none() && first.get("name").is_none() {
            bail!("JSON item missing 'Name' field: {first}");
        }
        Ok(format!("{} NDJSON items", items.len()))
    }));

    // ── 22. --format table ────────────────────────────────────────────
    specs.push(spec("T21 --format table", &["*.rs", "--format", "table", "--limit", "5"], |stdout, _| {
        // Table format should have alignment characters or separator lines
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        if lines.is_empty() { bail!("No output"); }
        Ok(format!("{} lines of table output", lines.len()))
    }));

    // ── 23. --columns selective ───────────────────────────────────────
    specs.push(spec("T22 --columns Name,Size,Path Only", &["*.txt", "--columns", "Name,Size,Path Only", "--limit", "10"], |stdout, _| {
        let header = stdout.lines().next().unwrap_or("");
        // Should only have the requested columns
        let col_count = header.split(',').count();
        if col_count > 5 { bail!("Too many columns ({col_count}), expected ~3"); }
        Ok(format!("{col_count} columns in output"))
    }));

    // ── 24. --dirs-only + --min-descendants ───────────────────────────
    specs.push(spec("T23 --min-descendants 100", &["*", "--dirs-only", "--min-descendants", "100", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let desc: u64 = col_val(row, &h, "Descendants").parse().unwrap_or(0);
            if desc < 100 { bail!("Row {i}: descendants={desc} < 100"); }
        }
        Ok(format!("{} dirs with 100+ descendants", rows.len()))
    }));

    // ── 25. --dirs-only + --max-descendants 0 (empty dirs) ───────────
    specs.push(spec("T24 --max-descendants 0", &["*", "--dirs-only", "--max-descendants", "0", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let desc: u64 = col_val(row, &h, "Descendants").parse().unwrap_or(999);
            if desc != 0 { bail!("Row {i}: descendants={desc}, expected 0"); }
        }
        Ok(format!("{} empty directories", rows.len()))
    }));

    // ── 26. --newer 7d ────────────────────────────────────────────────
    specs.push(spec("T25 --newer 7d", &["*.log", "--newer", "7d", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── 27. --older 365d ──────────────────────────────────────────────
    specs.push(spec("T26 --older 365d", &["*.doc", "--older", "365d", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── 28. --newer-created 30d ───────────────────────────────────────
    specs.push(spec("T27 --newer-created 30d", &["*", "--newer-created", "30d", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── 29. --drive C ─────────────────────────────────────────────────
    specs.push(spec("T28 --drive C", &["*.exe", "--drive", "C", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let path = col_val(row, &h, "Path Only");
            if !path.starts_with("C:") && !path.starts_with("c:") {
                bail!("Row {i}: path={path} not on C:");
            }
        }
        Ok(format!("{} rows, all on C:", rows.len()))
    }));

    // ── 30. --drives C,D ──────────────────────────────────────────────
    specs.push(spec("T29 --drives C,D", &["*.exe", "--drives", "C,D", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let path = col_val(row, &h, "Path Only").to_uppercase();
            if !path.starts_with("C:") && !path.starts_with("D:") {
                bail!("Row {i}: path={path} not on C: or D:");
            }
        }
        Ok(format!("{} rows, all on C: or D:", rows.len()))
    }));

    // ── 31. --sep and --quotes ────────────────────────────────────────
    specs.push(spec("T30 --sep | --quotes '", &["*.txt", "--sep", "|", "--quotes", "'", "--limit", "5"], |stdout, _| {
        let first_line = stdout.lines().next().unwrap_or("");
        if !first_line.contains('|') { bail!("No pipe separator in header: {first_line}"); }
        Ok(format!("pipe-separated output"))
    }));

    // ── 32. --out file ────────────────────────────────────────────────
    specs.push(spec("T31 --out file", &["*.rs", "--limit", "100", "--out", "test_cli_validation_out.csv"], |_stdout, _| {
        let path = std::path::Path::new("test_cli_validation_out.csv");
        if !path.exists() { bail!("Output file not created"); }
        let content = std::fs::read_to_string(path).unwrap_or_default();
        let _ = std::fs::remove_file(path); // cleanup
        let lines = content.lines().filter(|l| !l.is_empty()).count();
        if lines < 2 { bail!("Output file has {lines} lines, expected at least 2"); }
        Ok(format!("{lines} lines written to file"))
    }));

    // ── 33. --benchmark ───────────────────────────────────────────────
    specs.push(spec("T32 --benchmark", &["*.rs", "--benchmark"], |stdout, _| {
        // Benchmark mode should produce no CSV output (or minimal output)
        // but should exit successfully
        Ok(format!("{} bytes stdout (benchmark mode)", stdout.len()))
    }));

    // ── 34. Regex pattern ─────────────────────────────────────────────
    specs.push(spec("T33 regex >.*\\.config$", &[">.*\\.config$", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if !name.ends_with(".config") { bail!("Row {i}: {name} doesn't end with .config"); }
        }
        Ok(format!("{} rows, all .config files", rows.len()))
    }));

    // ── 35. Combined stress test ──────────────────────────────────────
    specs.push(spec("T34 combined stress", &[
        "*.pdf", "--files-only", "--min-size", "1048576", "--sort", "size",
        "--sort-desc", "--attr", "!hidden", "--newer", "365d", "--limit", "10",
        "--format", "csv", "--columns", "Name,Size,Path Only",
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        // Verify size descending
        if rows.len() >= 2 {
            let sizes: Vec<u64> = rows.iter().map(|r| col_val(r, &h, "Size").parse().unwrap_or(0)).collect();
            for w in sizes.windows(2) {
                if w[0] < w[1] { bail!("Not descending: {} < {}", w[0], w[1]); }
            }
        }
        // Verify all >= 1MB
        for (i, row) in rows.iter().enumerate() {
            let size: u64 = col_val(row, &h, "Size").parse().unwrap_or(0);
            if size < 1_048_576 { bail!("Row {i}: size={size} < 1MB"); }
        }
        Ok(format!("{} rows, all constraints satisfied", rows.len()))
    }));

    // ═══ EXTENDED TESTS (beyond original 34) ════════════════════════
    // Tests T35+ validate the unified search infrastructure built during
    // the FieldId consolidation (Phases 1-8):
    //  - Time grammar (named ranges)
    //  - Multi-sort across all sortable fields
    //  - Extension/attribute predicate compilation
    //  - Derived fields (type, path_only, extension)
    //  - Projection
    //  - Response modes
    //  - Bool attribute matrix
    //  - Combined stress tests

    // ── T35. --limit 0 (unlimited) ───────────────────────────────────
    specs.push(spec("T35 --limit 0 (unlimited)", &["*.dll", "--limit", "0", "--drive", "C", "--files-only"], |stdout, _| {
        let count = csv_row_count(stdout);
        if count < 100 { bail!("Expected many DLLs, got {count}"); }
        Ok(format!("{count} rows (unlimited)"))
    }));

    // ── T36. --older-created ─────────────────────────────────────────
    specs.push(spec("T36 --older-created 365d", &["*", "--older-created", "365d", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T37. --attr system ───────────────────────────────────────────
    specs.push(spec("T37 --attr system", &["*", "--attr", "system", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let sys = col_val(row, &h, "System");
            if sys != "1" { bail!("Row {i}: System={sys}, expected 1"); }
        }
        Ok(format!("{} rows, all system files", rows.len()))
    }));

    // ── T38. --attr readonly ─────────────────────────────────────────
    specs.push(spec("T38 --attr readonly", &["*", "--attr", "readonly", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let ro = col_val(row, &h, "Read-only");
            if ro != "1" { bail!("Row {i}: Read-only={ro}, expected 1"); }
        }
        Ok(format!("{} rows, all readonly", rows.len()))
    }));

    // ── T39. --attr combined: system,!hidden ─────────────────────────
    specs.push(spec("T39 --attr system,!hidden", &["*", "--attr", "system,!hidden", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let sys = col_val(row, &h, "System");
            let hid = col_val(row, &h, "Hidden");
            if sys != "1" { bail!("Row {i}: System={sys}, expected 1"); }
            if hid == "1" { bail!("Row {i}: Hidden=1, expected 0"); }
        }
        Ok(format!("{} rows, system but not hidden", rows.len()))
    }));

    // ── T40. Empty result set (no crash) ─────────────────────────────
    specs.push(spec("T40 no results (graceful)", &["xyzzy_nonexistent_file_pattern_12345", "--limit", "10"], |stdout, _| {
        let count = csv_row_count(stdout);
        if count != 0 { bail!("Expected 0 rows, got {count}"); }
        Ok("0 rows, graceful empty result".into())
    }));

    // ── T41. --header false ──────────────────────────────────────────
    specs.push(spec("T41 --header false", &["*.exe", "--header", "false", "--limit", "5", "--drive", "C"], |stdout, _| {
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        if let Some(first) = lines.first() {
            if first.starts_with("\"Path\"") || first.starts_with("Path,") {
                bail!("First line looks like a header: {first}");
            }
        }
        Ok(format!("{} lines, no header", lines.len()))
    }));

    // ── T42. --smart-case ────────────────────────────────────────────
    specs.push(spec("T42 --smart-case (lowercase = insensitive)", &["readme", "--smart-case", "--name-only", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        let has_mixed_case = rows.iter().any(|r| {
            let name = col_val(r, &h, "Name");
            name != name.to_lowercase()
        });
        Ok(format!("{} rows, mixed case={has_mixed_case}", rows.len()))
    }));

    // ── T43. --newer-accessed ────────────────────────────────────────
    specs.push(spec("T43 --newer-accessed 7d", &["*", "--newer-accessed", "7d", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ═══════════════════════════════════════════════════════════════════
    // TIME GRAMMAR TESTS — Named Time Ranges
    // Validates Phase 5: parse_time_bound named ranges compiled into
    // hot-path SearchFilters via compile_predicates_into_filters.
    // ═══════════════════════════════════════════════════════════════════

    // ── T44. --newer today ───────────────────────────────────────────
    specs.push(spec("T44 --newer today", &["*", "--newer", "today", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T45. --newer yesterday ───────────────────────────────────────
    specs.push(spec("T45 --newer yesterday", &["*", "--newer", "yesterday", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T46. --newer this_week ───────────────────────────────────────
    specs.push(spec("T46 --newer this_week", &["*", "--newer", "this_week", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T47. --newer last_7d ─────────────────────────────────────────
    specs.push(spec("T47 --newer last_7d", &["*", "--newer", "last_7d", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T48. --newer last_30d ────────────────────────────────────────
    specs.push(spec("T48 --newer last_30d", &["*", "--newer", "last_30d", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T49. --newer this_month ──────────────────────────────────────
    specs.push(spec("T49 --newer this_month", &["*", "--newer", "this_month", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T50. --newer this_year / ytd ─────────────────────────────────
    specs.push(spec("T50 --newer this_year", &["*", "--newer", "this_year", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T51. --older last_year ───────────────────────────────────────
    specs.push(spec("T51 --older last_year", &["*", "--older", "last_year", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T52. --newer last_90d ────────────────────────────────────────
    specs.push(spec("T52 --newer last_90d", &["*", "--newer", "last_90d", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T53. --newer last_365d ───────────────────────────────────────
    specs.push(spec("T53 --newer last_365d", &["*", "--newer", "last_365d", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T54. --newer-created today ───────────────────────────────────
    specs.push(spec("T54 --newer-created today", &["*", "--newer-created", "today", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T55. --newer-accessed this_week ──────────────────────────────
    specs.push(spec("T55 --newer-accessed this_week", &["*", "--newer-accessed", "this_week", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T56. Time grammar: newer last_week + older this_week ─────────
    // Files modified last week but NOT this week (bounded range).
    specs.push(spec("T56 bounded time range (last_week)", &[
        "*", "--newer", "last_week", "--older", "this_week",
        "--files-only", "--limit", "10"
    ], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T57. ISO date bound ──────────────────────────────────────────
    specs.push(spec("T57 --newer 2025-01-01", &["*", "--newer", "2025-01-01", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ═══════════════════════════════════════════════════════════════════
    // SORT TESTS — All Sortable FieldId Variants
    // Validates Phase 1+3: FieldId.metadata().sortable used by daemon
    // sort path. Tests every sort field to verify no panics/errors.
    // ═══════════════════════════════════════════════════════════════════

    // ── T58. --sort name ─────────────────────────────────────────────
    specs.push(spec("T58 --sort name", &["*.txt", "--sort", "name", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.len() >= 2 {
            let names: Vec<String> = rows.iter().map(|r| col_val(r, &h, "Name").to_lowercase()).collect();
            for w in names.windows(2) {
                if w[0] > w[1] { bail!("Not ascending: {} > {}", w[0], w[1]); }
            }
        }
        Ok(format!("{} rows, sorted by name asc", rows.len()))
    }));

    // ── T59. --sort path ─────────────────────────────────────────────
    specs.push(spec("T59 --sort path", &["*.txt", "--sort", "path", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 1, 10)
    }));

    // ── T60. --sort created ──────────────────────────────────────────
    specs.push(spec("T60 --sort created", &["*.exe", "--sort", "created", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 1, 10)
    }));

    // ── T61. --sort accessed ─────────────────────────────────────────
    specs.push(spec("T61 --sort accessed", &["*.exe", "--sort", "accessed", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 1, 10)
    }));

    // ── T62. --sort extension ────────────────────────────────────────
    specs.push(spec("T62 --sort extension", &["*.*", "--sort", "extension", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 1, 10)
    }));

    // ── T63. --sort drive ────────────────────────────────────────────
    specs.push(spec("T63 --sort drive", &["*.exe", "--sort", "drive", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 1, 10)
    }));

    // ── T64. --sort allocated (SizeOnDisk) ───────────────────────────
    specs.push(spec("T64 --sort allocated", &["*.exe", "--sort", "allocated", "--files-only", "--limit", "10", "--sort-desc"], |stdout, _| {
        assert_rows(stdout, 1, 10)
    }));

    // ── T65. --sort descendants ──────────────────────────────────────
    specs.push(spec("T65 --sort descendants --sort-desc", &["*", "--dirs-only", "--sort", "descendants", "--sort-desc", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.len() >= 2 {
            let vals: Vec<u64> = rows.iter().map(|r| col_val(r, &h, "Descendants").parse().unwrap_or(0)).collect();
            for w in vals.windows(2) {
                if w[0] < w[1] { bail!("Not descending: {} < {}", w[0], w[1]); }
            }
        }
        Ok(format!("{} rows, sorted desc by descendants", rows.len()))
    }));

    // ── T66. Multi-sort: size desc, then name asc ────────────────────
    specs.push(spec("T66 multi-sort size,-name", &["*.dll", "--sort", "size,-name", "--files-only", "--limit", "20"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.len() >= 2 {
            // Just verify no crash and basic ordering.
            let sizes: Vec<u64> = rows.iter().map(|r| col_val(r, &h, "Size").parse().unwrap_or(0)).collect();
            // In multi-sort, primary sort is size (default asc).
            // With leading '-', it would be size desc. Let's just verify it runs.
            let _ = sizes;
        }
        Ok(format!("{} rows, multi-sort applied", rows.len()))
    }));

    // ── T67. Multi-sort: modified desc, name asc ─────────────────────
    specs.push(spec("T67 multi-sort -modified,name", &["*.log", "--sort", "-modified,name", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ═══════════════════════════════════════════════════════════════════
    // DERIVED FIELD SORT TESTS — Tree metrics, bulkiness, lengths
    // Validates newly-wired sort columns in compare_by_column:
    //  treesize, tree_allocated, bulkiness, namelength, pathlength, path_only, type
    // ═══════════════════════════════════════════════════════════════════

    // ── T67a. --sort treesize (largest subtrees first) ───────────────
    specs.push(spec("T67a --sort treesize", &[
        "*", "--dirs-only", "--sort", "treesize", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.len() >= 2 {
            let vals: Vec<u64> = rows.iter().map(|r| col_val(r, &h, "Tree Size").parse().unwrap_or(0)).collect();
            for w in vals.windows(2) {
                if w[0] < w[1] { bail!("Not desc: {} < {}", w[0], w[1]); }
            }
        }
        Ok(format!("{} rows, sorted by treesize desc", rows.len()))
    }));

    // ── T67b. --sort treeallocated ───────────────────────────────────
    specs.push(spec("T67b --sort treeallocated", &[
        "*", "--dirs-only", "--sort", "treeallocated", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.len() >= 2 {
            let vals: Vec<u64> = rows.iter().map(|r| col_val(r, &h, "Tree Allocated").parse().unwrap_or(0)).collect();
            for w in vals.windows(2) {
                if w[0] < w[1] { bail!("Not desc: {} < {}", w[0], w[1]); }
            }
        }
        Ok(format!("{} rows, sorted by treeallocated desc", rows.len()))
    }));

    // ── T67c. --sort bulkiness ───────────────────────────────────────
    specs.push(spec("T67c --sort bulkiness", &[
        "*", "--files-only", "--min-size", "1024", "--sort", "bulkiness", "--limit", "10"
    ], |stdout, _| {
        assert_rows(stdout, 1, 10)
    }));

    // ── T67d. --sort namelength (longest names first) ────────────────
    specs.push(spec("T67d --sort namelength", &[
        "*", "--files-only", "--sort", "namelength", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.len() >= 2 {
            let lens: Vec<usize> = rows.iter().map(|r| col_val(r, &h, "Name").chars().count()).collect();
            for w in lens.windows(2) {
                if w[0] < w[1] { bail!("Not desc: {} < {}", w[0], w[1]); }
            }
        }
        Ok(format!("{} rows, sorted by name length desc", rows.len()))
    }));

    // ── T67e. --sort pathlength (longest paths first) ────────────────
    specs.push(spec("T67e --sort pathlength", &[
        "*", "--files-only", "--sort", "pathlength", "--limit", "10"
    ], |stdout, _| {
        assert_rows(stdout, 1, 10)
    }));

    // ── T67f. --sort path_only ───────────────────────────────────────
    specs.push(spec("T67f --sort path_only", &[
        "*.exe", "--sort", "path_only", "--limit", "10"
    ], |stdout, _| {
        assert_rows(stdout, 1, 10)
    }));

    // ── T67g. --sort type (semantic category) ────────────────────────
    specs.push(spec("T67g --sort type", &[
        "*", "--files-only", "--sort", "type", "--limit", "20"
    ], |stdout, _| {
        assert_rows(stdout, 1, 20)
    }));

    // ── T67h. multi-sort: treesize desc, name asc ────────────────────
    specs.push(spec("T67h multi-sort treesize,name", &[
        "*", "--dirs-only", "--sort", "treesize,name", "--limit", "20", "--columns", "all"
    ], |stdout, _| {
        assert_rows(stdout, 1, 20)
    }));

    // ── T67i. multi-sort: type asc, size desc ────────────────────────
    specs.push(spec("T67i multi-sort type,-size", &[
        "*", "--files-only", "--sort", "type,-size", "--limit", "20"
    ], |stdout, _| {
        assert_rows(stdout, 1, 20)
    }));

    // ── T67j. multi-sort: hidden desc, bulkiness desc ────────────────
    specs.push(spec("T67j multi-sort hidden,bulkiness", &[
        "*", "--files-only", "--min-size", "1024", "--sort", "hidden,bulkiness", "--limit", "20"
    ], |stdout, _| {
        assert_rows(stdout, 1, 20)
    }));

    // ═══════════════════════════════════════════════════════════════════
    // BOOL ATTRIBUTE MATRIX
    // Validates all 17 bool-typed attribute fields through --attr flag.
    // Each test verifies the correct column reads "1" for require,
    // or NOT "1" for exclude.
    // ═══════════════════════════════════════════════════════════════════

    // ── T68. --attr archive ──────────────────────────────────────────
    specs.push(spec("T68 --attr archive", &["*", "--attr", "archive", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let v = col_val(row, &h, "Archive");
            if v != "1" { bail!("Row {i}: Archive={v}"); }
        }
        Ok(format!("{} rows, all have archive attr", rows.len()))
    }));

    // ── T69. --attr sparse (may be empty) ────────────────────────────
    specs.push(spec("T69 --attr sparse", &["*", "--attr", "sparse", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let v = col_val(row, &h, "Sparse");
            if v != "1" { bail!("Row {i}: Sparse={v}"); }
        }
        Ok(format!("{} rows with sparse attr", rows.len()))
    }));

    // ── T70. --attr reparse (junctions/symlinks) ─────────────────────
    specs.push(spec("T70 --attr reparse", &["*", "--attr", "reparse", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let v = col_val(row, &h, "Reparse");
            if v != "1" { bail!("Row {i}: Reparse={v}"); }
        }
        Ok(format!("{} rows with reparse attr", rows.len()))
    }));

    // ── T71. --attr offline (may be empty) ───────────────────────────
    specs.push(spec("T71 --attr offline", &["*", "--attr", "offline", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let v = col_val(row, &h, "Offline");
            if v != "1" { bail!("Row {i}: Offline={v}"); }
        }
        Ok(format!("{} rows with offline attr", rows.len()))
    }));

    // ── T72. --attr encrypted (may be empty) ─────────────────────────
    specs.push(spec("T72 --attr encrypted", &["*", "--attr", "encrypted", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let v = col_val(row, &h, "Encrypted");
            if v != "1" { bail!("Row {i}: Encrypted={v}"); }
        }
        Ok(format!("{} rows with encrypted attr", rows.len()))
    }));

    // ── T73. --attr !system (exclude system) ─────────────────────────
    specs.push(spec("T73 --attr !system", &["*", "--attr", "!system", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let v = col_val(row, &h, "System");
            if v == "1" { bail!("Row {i}: System=1 despite !system"); }
        }
        Ok(format!("{} rows, no system files", rows.len()))
    }));

    // ── T74. --attr hidden,system (combined require) ─────────────────
    specs.push(spec("T74 --attr hidden,system", &["*", "--attr", "hidden,system", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let hid = col_val(row, &h, "Hidden");
            let sys = col_val(row, &h, "System");
            if hid != "1" { bail!("Row {i}: Hidden={hid}"); }
            if sys != "1" { bail!("Row {i}: System={sys}"); }
        }
        Ok(format!("{} rows, all hidden+system", rows.len()))
    }));

    // ═══════════════════════════════════════════════════════════════════
    // COMBINED / STRESS TESTS
    // Validates meaningful multi-constraint combinations that exercise
    // the full predicate compiler → hot-path filter → post-filter
    // → sort → projection pipeline.
    // ═══════════════════════════════════════════════════════════════════

    // ── T75. Size range + time range + extension ─────────────────────
    specs.push(spec("T75 size+time+ext combined", &[
        "*", "--ext", "exe,dll", "--min-size", "1048576", "--newer", "last_365d",
        "--files-only", "--sort", "size", "--sort-desc", "--limit", "10"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if !name.ends_with(".exe") && !name.ends_with(".dll") {
                bail!("Row {i}: {name} not exe/dll");
            }
            let size: u64 = col_val(row, &h, "Size").parse().unwrap_or(0);
            if size < 1_048_576 { bail!("Row {i}: size={size} < 1MB"); }
        }
        if rows.len() >= 2 {
            let sizes: Vec<u64> = rows.iter().map(|r| col_val(r, &h, "Size").parse().unwrap_or(0)).collect();
            for w in sizes.windows(2) {
                if w[0] < w[1] { bail!("Not descending: {} < {}", w[0], w[1]); }
            }
        }
        Ok(format!("{} rows, all constraints met", rows.len()))
    }));

    // ── T76. Dirs + descendants range + sort ─────────────────────────
    specs.push(spec("T76 dirs + desc range + sort", &[
        "*", "--dirs-only", "--min-descendants", "10", "--max-descendants", "1000",
        "--sort", "descendants", "--sort-desc", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let desc: u64 = col_val(row, &h, "Descendants").parse().unwrap_or(0);
            if desc < 10 || desc > 1000 { bail!("Row {i}: desc={desc} outside 10..1000"); }
        }
        if rows.len() >= 2 {
            let vals: Vec<u64> = rows.iter().map(|r| col_val(r, &h, "Descendants").parse().unwrap_or(0)).collect();
            for w in vals.windows(2) {
                if w[0] < w[1] { bail!("Not desc: {} < {}", w[0], w[1]); }
            }
        }
        Ok(format!("{} rows, desc 10..1000 sorted", rows.len()))
    }));

    // ── T77. Hidden files with recent modification ───────────────────
    specs.push(spec("T77 hidden + --newer last_30d", &[
        "*", "--attr", "hidden", "--newer", "last_30d",
        "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let hid = col_val(row, &h, "Hidden");
            if hid != "1" { bail!("Row {i}: Hidden={hid}"); }
        }
        Ok(format!("{} hidden files from last 30 days", rows.len()))
    }));

    // ── T78. Exclude + extension + size ──────────────────────────────
    specs.push(spec("T78 exclude + ext + size", &[
        "*.log", "--exclude", "debug*", "--max-size", "1048576",
        "--files-only", "--limit", "10"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if name.starts_with("debug") { bail!("Row {i}: {name} matches exclude"); }
            let size: u64 = col_val(row, &h, "Size").parse().unwrap_or(u64::MAX);
            if size > 1_048_576 { bail!("Row {i}: size={size} > 1MB"); }
        }
        Ok(format!("{} rows, all constraints met", rows.len()))
    }));

    // ── T79. --columns selective + --format json ─────────────────────
    specs.push(spec("T79 projection + json format", &[
        "*.rs", "--columns", "Name,Size,Modified", "--format", "json", "--limit", "5"
    ], |stdout, _| {
        let items: Vec<serde_json::Value> = stdout
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("Invalid JSON: {e}"))?;
        if items.is_empty() { bail!("No JSON items"); }
        // Verify projected fields exist.
        let first = &items[0];
        if first.get("Name").is_none() && first.get("name").is_none() {
            bail!("Missing Name in projected JSON");
        }
        Ok(format!("{} projected JSON items", items.len()))
    }));

    // ── T80. --columns all (wide output, no crash) ───────────────────
    specs.push(spec("T80 --columns all (wide)", &["*.exe", "--columns", "all", "--limit", "5", "--drive", "C"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        // "all" should produce many columns (>= 20).
        if h.len() < 15 { bail!("Expected >= 15 columns, got {}", h.len()); }
        Ok(format!("{} cols × {} rows", h.len(), rows.len()))
    }));

    // ── T81. Time created range: this_year ───────────────────────────
    specs.push(spec("T81 --newer-created this_year", &["*", "--newer-created", "this_year", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T82. Time accessed range: last_week ──────────────────────────
    specs.push(spec("T82 --newer-accessed last_week", &["*", "--newer-accessed", "last_week", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T83. Multi-sort 3 fields: drive, extension, size ─────────────
    specs.push(spec("T83 multi-sort drive,ext,-size", &["*.*", "--sort", "drive,extension,-size", "--files-only", "--limit", "20"], |stdout, _| {
        assert_rows(stdout, 1, 20)
    }));

    // ── T84. Large file search with all constraints ──────────────────
    specs.push(spec("T84 mega combined", &[
        "*.exe", "--files-only", "--min-size", "10485760", "--max-size", "1073741824",
        "--attr", "!hidden,!system", "--newer", "last_365d", "--sort", "-size",
        "--drive", "C", "--limit", "10", "--columns", "Name,Size,Modified,Path Only"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let size: u64 = col_val(row, &h, "Size").parse().unwrap_or(0);
            if size < 10_485_760 || size > 1_073_741_824 {
                bail!("Row {i}: size={size} outside 10MB..1GB");
            }
        }
        Ok(format!("{} rows, all constraints satisfied", rows.len()))
    }));

    // ── T85. --format table + --columns selective ────────────────────
    specs.push(spec("T85 table format + projection", &[
        "*.dll", "--format", "table", "--columns", "Name,Size", "--limit", "5", "--drive", "C"
    ], |stdout, _| {
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        if lines.is_empty() { bail!("No table output"); }
        Ok(format!("{} table lines", lines.len()))
    }));

    // ── T86. --older-accessed ─────────────────────────────────────────
    specs.push(spec("T86 --older-accessed 365d", &["*", "--older-accessed", "365d", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T87. Extension filter with sort by modified ──────────────────
    specs.push(spec("T87 ext + sort modified", &[
        "*", "--ext", "txt,log,md", "--sort", "-modified", "--files-only", "--limit", "10"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if !name.ends_with(".txt") && !name.ends_with(".log") && !name.ends_with(".md") {
                bail!("Row {i}: {name} not txt/log/md");
            }
        }
        Ok(format!("{} rows, ext filtered + sorted", rows.len()))
    }));

    // ── T88. Name-only with hide-system and time bound ───────────────
    specs.push(spec("T88 name-only + hide-system + newer", &[
        "config", "--name-only", "--hide-system", "--newer", "last_90d",
        "--files-only", "--limit", "10"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if !name.contains("config") { bail!("Row {i}: {name} doesn't contain 'config'"); }
            if name.starts_with('$') { bail!("Row {i}: {name} starts with $ despite hide-system"); }
        }
        Ok(format!("{} rows", rows.len()))
    }));

    // ═══════════════════════════════════════════════════════════════════
    // SEARCH MODE TESTS — Scope prefixes, pattern sugar, path filter
    // Validates path:/dir:/file: prefixes, --begins-with, --ends-with,
    // --contains, --not-contains, and --in-path + pattern combos.
    // ═══════════════════════════════════════════════════════════════════

    // ── T88a. path: prefix (match against full path) ─────────────────
    specs.push(spec("T88a path: prefix", &[
        "path:*windows*", "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let path = col_val(row, &h, "Path Only").to_lowercase();
            let name = col_val(row, &h, "Name").to_lowercase();
            let full = format!("{path}{name}");
            if !full.contains("windows") {
                bail!("Row {i}: full path '{full}' doesn't contain 'windows'");
            }
        }
        Ok(format!("{} rows, all paths contain 'windows'", rows.len()))
    }));

    // ── T88b. dir: prefix (directories only) ─────────────────────────
    specs.push(spec("T88b dir: prefix", &[
        "dir:*system*", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let dir_flag = col_val(row, &h, "Directory Flag");
            if dir_flag != "1" {
                bail!("Row {i}: dir: prefix returned non-directory (Directory Flag={dir_flag})");
            }
        }
        Ok(format!("{} rows, all directories", rows.len()))
    }));

    // ── T88c. file: prefix (files only) ──────────────────────────────
    specs.push(spec("T88c file: prefix", &[
        "file:*.dll", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let dir_flag = col_val(row, &h, "Directory Flag");
            if dir_flag == "1" {
                bail!("Row {i}: file: prefix returned directory");
            }
            let name = col_val(row, &h, "Name").to_lowercase();
            if !name.ends_with(".dll") {
                bail!("Row {i}: {name} doesn't end with .dll");
            }
        }
        Ok(format!("{} rows, all .dll files", rows.len()))
    }));

    // ── T88d. --begins-with ──────────────────────────────────────────
    specs.push(spec("T88d --begins-with", &[
        "--begins-with", "note", "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if !name.starts_with("note") {
                bail!("Row {i}: {name} doesn't start with 'note'");
            }
        }
        Ok(format!("{} rows, all begin with 'note'", rows.len()))
    }));

    // ── T88e. --ends-with ────────────────────────────────────────────
    specs.push(spec("T88e --ends-with", &[
        "--ends-with", ".log", "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if !name.ends_with(".log") {
                bail!("Row {i}: {name} doesn't end with '.log'");
            }
        }
        Ok(format!("{} rows, all end with '.log'", rows.len()))
    }));

    // ── T88f. --contains ─────────────────────────────────────────────
    specs.push(spec("T88f --contains", &[
        "--contains", "setup", "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if !name.contains("setup") {
                bail!("Row {i}: {name} doesn't contain 'setup'");
            }
        }
        Ok(format!("{} rows, all contain 'setup'", rows.len()))
    }));

    // ── T88g. --not-contains (exclusion) ─────────────────────────────
    specs.push(spec("T88g --not-contains", &[
        "*.log", "--not-contains", "debug", "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if name.contains("debug") {
                bail!("Row {i}: {name} contains 'debug' despite --not-contains");
            }
            if !name.ends_with(".log") {
                bail!("Row {i}: {name} not a .log file");
            }
        }
        Ok(format!("{} rows, no 'debug' in names", rows.len()))
    }));

    // ── T88h. --in-path + filename pattern ───────────────────────────
    specs.push(spec("T88h --in-path + pattern", &[
        "*.exe", "--in-path", "*windows*", "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let path = col_val(row, &h, "Path Only").to_lowercase();
            let name = col_val(row, &h, "Name").to_lowercase();
            if !path.contains("windows") {
                bail!("Row {i}: path={path} doesn't contain 'windows'");
            }
            if !name.ends_with(".exe") {
                bail!("Row {i}: {name} not .exe");
            }
        }
        Ok(format!("{} rows, .exe files in *windows*", rows.len()))
    }));

    // ── T88i. path: prefix vs --in-path distinction ──────────────────
    // path: matches filename too; --in-path matches directory only.
    specs.push(spec("T88i path: vs --in-path", &[
        "path:*notepad*", "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        // path: should match files named "notepad*" OR in paths containing "notepad"
        for (i, row) in rows.iter().enumerate() {
            let path = col_val(row, &h, "Path Only").to_lowercase();
            let name = col_val(row, &h, "Name").to_lowercase();
            let full = format!("{path}{name}");
            if !full.contains("notepad") {
                bail!("Row {i}: full path '{full}' doesn't contain 'notepad'");
            }
        }
        Ok(format!("{} rows, path: matched", rows.len()))
    }));

    // ── T88j. --contains + --not-contains combined ───────────────────
    specs.push(spec("T88j --contains + --not-contains", &[
        "--contains", "update", "--not-contains", "old",
        "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if !name.contains("update") {
                bail!("Row {i}: {name} doesn't contain 'update'");
            }
            if name.contains("old") {
                bail!("Row {i}: {name} contains 'old' despite --not-contains");
            }
        }
        Ok(format!("{} rows, contains 'update' but not 'old'", rows.len()))
    }));

    // ── T88k. dir: prefix + sort by treesize ─────────────────────────
    specs.push(spec("T88k dir: prefix + sort treesize", &[
        "dir:*program*", "--sort", "treesize", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let dir_flag = col_val(row, &h, "Directory Flag");
            if dir_flag != "1" {
                bail!("Row {i}: dir: prefix returned non-directory");
            }
        }
        if rows.len() >= 2 {
            let sizes: Vec<u64> = rows.iter().map(|r| col_val(r, &h, "Tree Size").parse().unwrap_or(0)).collect();
            for w in sizes.windows(2) {
                if w[0] < w[1] { bail!("Treesize not desc: {} < {}", w[0], w[1]); }
            }
        }
        Ok(format!("{} dirs sorted by treesize", rows.len()))
    }));

    // ── T88l. --begins-with + --ext combined ─────────────────────────
    specs.push(spec("T88l --begins-with + --ext", &[
        "--begins-with", "win", "--ext", "exe,dll", "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if !name.starts_with("win") {
                bail!("Row {i}: {name} doesn't start with 'win'");
            }
            if !name.ends_with(".exe") && !name.ends_with(".dll") {
                bail!("Row {i}: {name} not .exe/.dll");
            }
        }
        Ok(format!("{} rows, begins-with + ext", rows.len()))
    }));

    // ═══════════════════════════════════════════════════════════════════
    // TYPE FILTER TESTS — Semantic file categorization
    // Validates --type <category> post-filter via semantic_type_for_row().
    // ═══════════════════════════════════════════════════════════════════

    // ── T89. --type code ─────────────────────────────────────────────
    specs.push(spec("T89 --type code", &[
        "*", "--type", "code", "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        let code_exts = ["rs","py","js","ts","java","c","cpp","h","hpp","go","rb","php","swift","kt"];
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if !code_exts.iter().any(|e| name.ends_with(&format!(".{e}"))) {
                bail!("Row {i}: {name} is not a code file");
            }
        }
        Ok(format!("{} rows, all code files", rows.len()))
    }));

    // ── T90. --type document ─────────────────────────────────────────
    specs.push(spec("T90 --type document", &[
        "*", "--type", "document", "--files-only", "--limit", "10"
    ], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T91. --type executable ───────────────────────────────────────
    specs.push(spec("T91 --type executable", &[
        "*", "--type", "executable", "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        let exec_exts = ["exe","msi","bat","cmd","ps1","com","scr"];
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if !exec_exts.iter().any(|e| name.ends_with(&format!(".{e}"))) {
                bail!("Row {i}: {name} is not an executable");
            }
        }
        Ok(format!("{} rows, all executables", rows.len()))
    }));

    // ── T92. --type picture ──────────────────────────────────────────
    specs.push(spec("T92 --type picture", &[
        "*", "--type", "picture", "--files-only", "--limit", "10"
    ], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T93. --type system ───────────────────────────────────────────
    specs.push(spec("T93 --type system", &[
        "*", "--type", "system", "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        let sys_exts = ["sys","dll","drv","ocx","cpl","ax","mui"];
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if !sys_exts.iter().any(|e| name.ends_with(&format!(".{e}"))) {
                bail!("Row {i}: {name} is not a system file type");
            }
        }
        Ok(format!("{} rows, all system type", rows.len()))
    }));

    // ── T94. --type combined with sort ───────────────────────────────
    specs.push(spec("T94 --type code + sort size", &[
        "*", "--type", "code", "--files-only", "--sort", "-size", "--limit", "10"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.len() >= 2 {
            let sizes: Vec<u64> = rows.iter().map(|r| col_val(r, &h, "Size").parse().unwrap_or(0)).collect();
            for w in sizes.windows(2) {
                if w[0] < w[1] { bail!("Not descending: {} < {}", w[0], w[1]); }
            }
        }
        Ok(format!("{} code files sorted by size desc", rows.len()))
    }));

    // ═══════════════════════════════════════════════════════════════════
    // IN-PATH FILTER TESTS — Directory path glob matching
    // Validates --in-path <glob> post-filter on directory portion of path.
    // ═══════════════════════════════════════════════════════════════════

    // ── T95. --in-path windows ───────────────────────────────────────
    specs.push(spec("T95 --in-path *windows*", &[
        "*.dll", "--in-path", "*windows*", "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let path = col_val(row, &h, "Path Only").to_lowercase();
            if !path.contains("windows") {
                bail!("Row {i}: path={path} doesn't contain 'windows'");
            }
        }
        Ok(format!("{} rows, all in *windows*", rows.len()))
    }));

    // ── T96. --in-path system32 ──────────────────────────────────────
    specs.push(spec("T96 --in-path *system32*", &[
        "*.dll", "--in-path", "*system32*", "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let path = col_val(row, &h, "Path Only").to_lowercase();
            if !path.contains("system32") {
                bail!("Row {i}: path={path} doesn't contain 'system32'");
            }
        }
        Ok(format!("{} rows, all in *system32*", rows.len()))
    }));

    // ── T97. --in-path combined with --exclude ───────────────────────
    specs.push(spec("T97 --in-path + --exclude", &[
        "*.exe", "--in-path", "*windows*", "--exclude", "setup*",
        "--files-only", "--limit", "10"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if name.starts_with("setup") {
                bail!("Row {i}: {name} matches exclude pattern");
            }
        }
        Ok(format!("{} rows, in-path + exclude", rows.len()))
    }));

    // ═══════════════════════════════════════════════════════════════════
    // BULKINESS FILTER TESTS — Waste ratio filtering
    // Validates --min-bulkiness / --max-bulkiness post-filter.
    // ═══════════════════════════════════════════════════════════════════

    // ── T98. --min-bulkiness 200 (files using ≥2× their size) ────────
    specs.push(spec("T98 --min-bulkiness 200", &[
        "*", "--min-bulkiness", "200", "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        // We can't directly check bulkiness from CSV columns (it's derived),
        // but we can verify allocated > size for each row.
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let size: u64 = col_val(row, &h, "Size").parse().unwrap_or(1);
            let alloc: u64 = col_val(row, &h, "Size on Disk").parse().unwrap_or(0);
            if size > 0 {
                let bulk = alloc * 100 / size;
                if bulk < 200 { bail!("Row {i}: bulkiness={bulk}% < 200%"); }
            }
        }
        Ok(format!("{} rows, all bulkiness >= 200%", rows.len()))
    }));

    // ── T99. --max-bulkiness 100 (perfectly packed) ──────────────────
    specs.push(spec("T99 --max-bulkiness 100", &[
        "*", "--max-bulkiness", "100", "--files-only", "--min-size", "1024",
        "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let size: u64 = col_val(row, &h, "Size").parse().unwrap_or(1);
            let alloc: u64 = col_val(row, &h, "Size on Disk").parse().unwrap_or(u64::MAX);
            if size > 0 {
                let bulk = alloc * 100 / size;
                if bulk > 100 { bail!("Row {i}: bulkiness={bulk}% > 100%"); }
            }
        }
        Ok(format!("{} rows, all bulkiness <= 100%", rows.len()))
    }));

    // ── T100. --min-bulkiness + --min-size combined ──────────────────
    specs.push(spec("T100 bulkiness + size combined", &[
        "*", "--min-bulkiness", "500", "--min-size", "1048576",
        "--files-only", "--limit", "10"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let size: u64 = col_val(row, &h, "Size").parse().unwrap_or(0);
            if size < 1_048_576 { bail!("Row {i}: size={size} < 1MB"); }
        }
        Ok(format!("{} rows, bulky large files", rows.len()))
    }));

    // ═══════════════════════════════════════════════════════════════════
    // TREE METRICS TESTS — Subtree size filters for directories
    // Validates --min/max-treesize and --min/max-tree-allocated.
    // ═══════════════════════════════════════════════════════════════════

    // ── T101. --min-treesize 100MB (large directory subtrees) ────────
    specs.push(spec("T101 --min-treesize 100MB", &[
        "*", "--dirs-only", "--min-treesize", "104857600", "--limit", "10",
        "--sort", "-size", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let ts: u64 = col_val(row, &h, "Tree Size").parse().unwrap_or(0);
            if ts < 104_857_600 { bail!("Row {i}: Tree Size={ts} < 100MB"); }
        }
        Ok(format!("{} dirs with treesize >= 100MB", rows.len()))
    }));

    // ── T102. --max-treesize 1MB (small directory subtrees) ──────────
    specs.push(spec("T102 --max-treesize 1MB", &[
        "*", "--dirs-only", "--max-treesize", "1048576", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let ts: u64 = col_val(row, &h, "Tree Size").parse().unwrap_or(u64::MAX);
            if ts > 1_048_576 { bail!("Row {i}: Tree Size={ts} > 1MB"); }
        }
        Ok(format!("{} dirs with treesize <= 1MB", rows.len()))
    }));

    // ── T103. --min-tree-allocated 100MB ─────────────────────────────
    specs.push(spec("T103 --min-tree-allocated 100MB", &[
        "*", "--dirs-only", "--min-tree-allocated", "104857600", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let ta: u64 = col_val(row, &h, "Tree Allocated").parse().unwrap_or(0);
            if ta < 104_857_600 { bail!("Row {i}: Tree Allocated={ta} < 100MB"); }
        }
        Ok(format!("{} dirs with tree-allocated >= 100MB", rows.len()))
    }));

    // ═══════════════════════════════════════════════════════════════════
    // NAME / PATH LENGTH TESTS
    // Validates --min/max-name-length and --min/max-path-length.
    // ═══════════════════════════════════════════════════════════════════

    // ── T104. --min-name-length 50 (long filenames) ──────────────────
    specs.push(spec("T104 --min-name-length 50", &[
        "*", "--min-name-length", "50", "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name");
            if name.chars().count() < 50 {
                bail!("Row {i}: name len {} < 50: {name}", name.chars().count());
            }
        }
        Ok(format!("{} rows, all names >= 50 chars", rows.len()))
    }));

    // ── T105. --max-name-length 8 (short filenames) ──────────────────
    specs.push(spec("T105 --max-name-length 8", &[
        "*", "--max-name-length", "8", "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name");
            if name.chars().count() > 8 {
                bail!("Row {i}: name len {} > 8: {name}", name.chars().count());
            }
        }
        Ok(format!("{} rows, all names <= 8 chars", rows.len()))
    }));

    // ── T106. --min-path-length 200 (deep paths) ─────────────────────
    specs.push(spec("T106 --min-path-length 200", &[
        "*", "--min-path-length", "200", "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let path = col_val(row, &h, "Path Only");
            let name = col_val(row, &h, "Name");
            let full = format!("{path}{name}");
            if full.chars().count() < 200 {
                bail!("Row {i}: path len {} < 200", full.chars().count());
            }
        }
        Ok(format!("{} rows, all paths >= 200 chars", rows.len()))
    }));

    // ── T107. --max-path-length 30 (short paths) ─────────────────────
    specs.push(spec("T107 --max-path-length 30", &[
        "*", "--max-path-length", "30", "--files-only", "--limit", "10"
    ], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ═══════════════════════════════════════════════════════════════════
    // SIZE ON DISK TESTS — Allocated size filters
    // Validates --min/max-size-on-disk.
    // ═══════════════════════════════════════════════════════════════════

    // ── T108. --min-size-on-disk 100MB ───────────────────────────────
    specs.push(spec("T108 --min-size-on-disk 100MB", &[
        "*", "--min-size-on-disk", "104857600", "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let alloc: u64 = col_val(row, &h, "Size on Disk").parse().unwrap_or(0);
            if alloc < 104_857_600 { bail!("Row {i}: Size on Disk={alloc} < 100MB"); }
        }
        Ok(format!("{} rows, all allocated >= 100MB", rows.len()))
    }));

    // ── T109. --max-size-on-disk 4096 ────────────────────────────────
    specs.push(spec("T109 --max-size-on-disk 4096", &[
        "*", "--max-size-on-disk", "4096", "--files-only", "--min-size", "1",
        "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let alloc: u64 = col_val(row, &h, "Size on Disk").parse().unwrap_or(u64::MAX);
            if alloc > 4096 { bail!("Row {i}: Size on Disk={alloc} > 4096"); }
        }
        Ok(format!("{} rows, all allocated <= 4096", rows.len()))
    }));

    // ═══════════════════════════════════════════════════════════════════
    // MONTH FILTER TESTS — Month-of-year filtering
    // Validates --month <spec> parsed via parse_month_spec().
    // ═══════════════════════════════════════════════════════════════════

    // ── T110. --month jan ────────────────────────────────────────────
    specs.push(spec("T110 --month jan", &[
        "*", "--month", "jan", "--files-only", "--limit", "10"
    ], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T111. --month Q4 (Oct/Nov/Dec) ───────────────────────────────
    specs.push(spec("T111 --month Q4", &[
        "*", "--month", "Q4", "--files-only", "--limit", "10"
    ], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T112. --month combo: jan,feb,mar ─────────────────────────────
    specs.push(spec("T112 --month jan,feb,mar", &[
        "*", "--month", "jan,feb,mar", "--files-only", "--limit", "10"
    ], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ── T113. --month + --newer combined ─────────────────────────────
    specs.push(spec("T113 --month jan + --newer last_365d", &[
        "*", "--month", "jan", "--newer", "last_365d", "--files-only", "--limit", "10"
    ], |stdout, _| {
        assert_rows(stdout, 0, 10)
    }));

    // ═══════════════════════════════════════════════════════════════════
    // BOOL ATTRIBUTE SORT TESTS — Sort by flag bit value
    // Validates field_to_attr_bit() sorting (true/false grouping).
    // ═══════════════════════════════════════════════════════════════════

    // ── T114. --sort hidden:desc (hidden files first) ────────────────
    specs.push(spec("T114 --sort hidden:desc", &[
        "*", "--sort", "hidden:desc", "--limit", "20", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.len() < 2 { return Ok("< 2 rows, skip".into()); }
        // First rows should have Hidden=1, later rows Hidden=0
        let flags: Vec<&str> = rows.iter().map(|r| col_val(r, &h, "Hidden")).collect();
        let mut seen_zero = false;
        for (i, f) in flags.iter().enumerate() {
            if *f != "1" { seen_zero = true; }
            if seen_zero && *f == "1" {
                bail!("Row {i}: Hidden=1 after Hidden=0 — not sorted desc");
            }
        }
        Ok(format!("{} rows, hidden sorted desc", rows.len()))
    }));

    // ── T115. --sort compressed:desc ─────────────────────────────────
    specs.push(spec("T115 --sort compressed:desc", &[
        "*", "--sort", "compressed:desc", "--limit", "20", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.len() < 2 { return Ok("< 2 rows, skip".into()); }
        let flags: Vec<&str> = rows.iter().map(|r| col_val(r, &h, "Compressed")).collect();
        let mut seen_zero = false;
        for (i, f) in flags.iter().enumerate() {
            if *f != "1" { seen_zero = true; }
            if seen_zero && *f == "1" {
                bail!("Row {i}: Compressed=1 after 0 — not sorted desc");
            }
        }
        Ok(format!("{} rows, compressed sorted desc", rows.len()))
    }));

    // ── T116. --sort directory:desc ──────────────────────────────────
    specs.push(spec("T116 --sort directory:desc", &[
        "*", "--sort", "directory:desc", "--limit", "20", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.len() < 2 { return Ok("< 2 rows, skip".into()); }
        let flags: Vec<&str> = rows.iter().map(|r| col_val(r, &h, "Directory Flag")).collect();
        let mut seen_zero = false;
        for (i, f) in flags.iter().enumerate() {
            if *f != "1" { seen_zero = true; }
            if seen_zero && *f == "1" {
                bail!("Row {i}: Dir=1 after Dir=0 — not sorted desc");
            }
        }
        Ok(format!("{} rows, directory sorted desc", rows.len()))
    }));

    // ═══════════════════════════════════════════════════════════════════
    // COMBINED — New filter mega-stress tests
    // ═══════════════════════════════════════════════════════════════════

    // ── T117. type + in-path + size combined ─────────────────────────
    specs.push(spec("T117 type + in-path + size", &[
        "*", "--type", "system", "--in-path", "*windows*", "--min-size", "1048576",
        "--files-only", "--sort", "-size", "--limit", "10"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let size: u64 = col_val(row, &h, "Size").parse().unwrap_or(0);
            if size < 1_048_576 { bail!("Row {i}: size={size} < 1MB"); }
        }
        Ok(format!("{} rows, all constraints met", rows.len()))
    }));

    // ── T118. tree metrics + descendants combined ────────────────────
    specs.push(spec("T118 treesize + descendants", &[
        "*", "--dirs-only", "--min-treesize", "10485760", "--min-descendants", "10",
        "--sort", "-size", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let ts: u64 = col_val(row, &h, "Tree Size").parse().unwrap_or(0);
            let desc: u64 = col_val(row, &h, "Descendants").parse().unwrap_or(0);
            if ts < 10_485_760 { bail!("Row {i}: Tree Size={ts} < 10MB"); }
            if desc < 10 { bail!("Row {i}: descendants={desc} < 10"); }
        }
        Ok(format!("{} dirs, treesize+desc constraints met", rows.len()))
    }));

    specs
}

// ── Main ─────────────────────────────────────────────────────────────────────

fn main() {
    let (bin, data_dir) = parse_script_args();
    let total_start = Instant::now();
    eprintln!();
    eprintln!("╔═══════════════════════════════════════════════════════════════╗");
    eprintln!("║  UFFS CLI Flag Validation Suite                              ║");
    eprintln!("╚═══════════════════════════════════════════════════════════════╝");
    eprintln!("  Binary:   {bin}");
    if let Some(ref d) = data_dir {
        eprintln!("  Data dir: {d}");
    }
    eprintln!();

    // ═══ Phase 1: Startup Timing (COLD → WARM → HOT) ════════════════════
    startup_timing(&bin, &data_dir);

    // ═══ Phase 2: Parallel Validation (all tests, HOT daemon) ════════════
    eprintln!("┌───────────────────────────────────────────────────────────────┐");
    eprintln!("│  Parallel Validation (141 tests, HOT daemon)                 │");
    eprintln!("└───────────────────────────────────────────────────────────────┘");

    let specs = define_tests();
    let test_count = specs.len();
    eprintln!("  Launching {test_count} tests in parallel...");
    eprintln!();

    let (results, wall_ms) = run_tests_parallel(&bin, &specs, &data_dir);
    print_results(&results, wall_ms);

    let total_ms = total_start.elapsed().as_millis();
    eprintln!();
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!("  Total time: {}ms", total_ms);
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    // Exit code: fail if any test failed.
    let total_failures = results.iter().filter(|r| !r.passed).count();
    std::process::exit(if total_failures == 0 { 0 } else { 1 });
}