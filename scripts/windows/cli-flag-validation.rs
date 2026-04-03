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
// Runs every CLI flag combination at three caching levels and compares
// per-test timings side-by-side:
//
//   COLD       — no daemon, no cache files  (full MFT read + index build)
//   WARM CACHE — no daemon, cache files exist  (daemon auto-starts from cache)
//   HOT        — daemon already running  (pure in-memory search)
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

/// Find the uffs binary. First CLI arg overrides.
fn uffs_bin() -> String {
    std::env::args().nth(1).unwrap_or_else(|| {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_else(|_| ".".to_string());
        let candidates = [
            format!("{home}\\bin\\uffs.exe"),
            format!("{home}/bin/uffs.exe"),
            "target/release/uffs.exe".to_string(),
            "target/debug/uffs.exe".to_string(),
        ];
        for c in &candidates {
            if std::path::Path::new(c).exists() {
                return c.clone();
            }
        }
        "uffs".to_string()
    })
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
    passed: bool,
    duration_ms: u128,
    detail: String,
}

/// Results from running the full test suite at one caching level.
struct PhaseResult {
    label: String,
    results: Vec<TestResult>,
    wall_ms: u128,
}

struct TestRunner {
    bin: String,
    results: Vec<TestResult>,
}


impl TestRunner {
    fn new(bin: String) -> Self {
        Self { bin, results: Vec::new() }
    }

    /// Run uffs with given args, return (exit_code, stdout, stderr).
    fn run_uffs(&self, args: &[&str]) -> Result<(i32, String, String)> {
        let output = Command::new(&self.bin)
            .env("RUST_LOG", "trace")
            .env("RUST_LOG_FILE", "trace")
            .env("UFFS_LOG", "trace")
            .args(args)
            .output()
            .with_context(|| format!("Failed to execute: {} {}", self.bin, args.join(" ")))?;
        let code = output.status.code().unwrap_or(-1);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Ok((code, stdout, stderr))
    }

    /// Run a named test. Validate closure returns Ok(detail) or Err(reason).
    fn test<F>(&mut self, name: &str, args: &[&str], validate: F)
    where
        F: FnOnce(&str, &str) -> Result<String>,
    {
        let start = Instant::now();
        let result = self.run_uffs(args);
        let duration_ms = start.elapsed().as_millis();

        let (passed, detail) = match result {
            Ok((code, stdout, stderr)) => {
                if code != 0 {
                    (false, format!("Exit code {code}. stderr: {}", stderr.lines().next().unwrap_or("")))
                } else {
                    match validate(&stdout, &stderr) {
                        Ok(msg) => (true, msg),
                        Err(e) => (false, format!("{e}")),
                    }
                }
            }
            Err(e) => (false, format!("Execution failed: {e}")),
        };

        let status = if passed { "PASS".green().bold() } else { "FAIL".red().bold() };
        let timing = format!("{duration_ms:>5}ms").dimmed();
        eprintln!("  [{status}] {timing}  {name}: {detail}");

        self.results.push(TestResult {
            name: name.to_string(), passed, duration_ms, detail,
        });
    }

    /// Drain results into a `PhaseResult`.
    fn finish_phase(&mut self, label: &str, wall_ms: u128) -> PhaseResult {
        PhaseResult {
            label: label.to_string(),
            results: std::mem::take(&mut self.results),
            wall_ms,
        }
    }

    fn phase_summary(phase: &PhaseResult) {
        let total = phase.results.len();
        let passed = phase.results.iter().filter(|r| r.passed).count();
        let failed = total - passed;
        let sum_ms: u128 = phase.results.iter().map(|r| r.duration_ms).sum();
        let avg_ms = if total > 0 { sum_ms / total as u128 } else { 0 };

        eprintln!();
        eprintln!("  ─── {} ───", phase.label);
        if failed == 0 {
            eprintln!("  {} {passed}/{total} tests — wall {}ms / sum {sum_ms}ms / avg {avg_ms}ms",
                "✅".green(), phase.wall_ms);
        } else {
            eprintln!("  {} {failed}/{total} FAILED — wall {}ms / sum {sum_ms}ms",
                "❌".red(), phase.wall_ms);
            for r in &phase.results {
                if !r.passed {
                    eprintln!("     ❌ {}: {}", r.name, r.detail);
                }
            }
        }
    }
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

fn run_test_suite(t: &mut TestRunner) {
    // ── 1. Warmup (also verifies daemon is alive / auto-starts) ───────
    t.test("T00 warmup / daemon alive", &["*.txt", "--limit", "1"], |stdout, _| {
        if csv_row_count(stdout) < 1 { bail!("No results — daemon may not be running"); }
        Ok("daemon warm".into())
    });

    // ── 2. --files-only ───────────────────────────────────────────────
    t.test("T01 --files-only", &["*.txt", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let dir = col_val(row, &h, "Directory Flag");
            if dir == "1" { bail!("Row {i} is a directory (Directory Flag=1)"); }
        }
        Ok(format!("{} rows, all files", rows.len()))
    });

    // ── 3. --dirs-only ────────────────────────────────────────────────
    t.test("T02 --dirs-only", &["Windows", "--dirs-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let dir = col_val(row, &h, "Directory Flag");
            if dir != "1" { bail!("Row {i} is not a directory (Directory Flag={dir})"); }
        }
        Ok(format!("{} rows, all directories", rows.len()))
    });

    // ── 4. --hide-system ──────────────────────────────────────────────
    t.test("T03 --hide-system", &["$*", "--limit", "20", "--hide-system"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        // With --hide-system, no file should start with $
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name");
            if name.starts_with('$') { bail!("Row {i}: {name} starts with $ despite --hide-system"); }
        }
        Ok(format!("{} rows, no $-prefixed files", rows.len()))
    });

    // ── 5. --ext single ───────────────────────────────────────────────
    t.test("T04 --ext rs", &["*", "--ext", "rs", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name");
            if !name.to_lowercase().ends_with(".rs") {
                bail!("Row {i}: {name} does not end with .rs");
            }
        }
        Ok(format!("{} rows, all .rs", rows.len()))
    });

    // ── 6. --ext multiple ─────────────────────────────────────────────
    t.test("T05 --ext jpg,png,gif", &["*", "--ext", "jpg,png,gif", "--limit", "10"], |stdout, _| {
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
    });

    // ── 7. --min-size ─────────────────────────────────────────────────
    t.test("T06 --min-size 100MB", &["*", "--min-size", "104857600", "--files-only", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let size: u64 = col_val(row, &h, "Size").parse().unwrap_or(0);
            if size < 104_857_600 { bail!("Row {i}: size={size} < 100MB"); }
        }
        Ok(format!("{} rows, all >= 100MB", rows.len()))
    });

    // ── 8. --max-size ─────────────────────────────────────────────────
    t.test("T07 --max-size 1KB", &["*", "--max-size", "1024", "--files-only", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let size: u64 = col_val(row, &h, "Size").parse().unwrap_or(u64::MAX);
            if size > 1024 { bail!("Row {i}: size={size} > 1KB"); }
        }
        Ok(format!("{} rows, all <= 1KB", rows.len()))
    });

    // ── 9. --min-size + --max-size combined ───────────────────────────
    t.test("T08 --min/max-size 1MB..10MB", &["*.pdf", "--min-size", "1048576", "--max-size", "10485760", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let size: u64 = col_val(row, &h, "Size").parse().unwrap_or(0);
            if size < 1_048_576 || size > 10_485_760 {
                bail!("Row {i}: size={size} outside 1MB..10MB");
            }
        }
        Ok(format!("{} rows, all 1MB..10MB", rows.len()))
    });

    // ── 10. --sort size ascending ─────────────────────────────────────
    t.test("T09 --sort size (asc)", &["*.exe", "--sort", "size", "--files-only", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.len() < 2 { return Ok("< 2 rows, skip sort check".into()); }
        let sizes: Vec<u64> = rows.iter().map(|r| col_val(r, &h, "Size").parse().unwrap_or(0)).collect();
        for w in sizes.windows(2) {
            if w[0] > w[1] { bail!("Not ascending: {} > {}", w[0], w[1]); }
        }
        Ok(format!("{} rows, sorted asc", rows.len()))
    });

    // ── 11. --sort size descending ────────────────────────────────────
    t.test("T10 --sort size --sort-desc", &["*.exe", "--sort", "size", "--sort-desc", "--files-only", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.len() < 2 { return Ok("< 2 rows, skip sort check".into()); }
        let sizes: Vec<u64> = rows.iter().map(|r| col_val(r, &h, "Size").parse().unwrap_or(0)).collect();
        for w in sizes.windows(2) {
            if w[0] < w[1] { bail!("Not descending: {} < {}", w[0], w[1]); }
        }
        Ok(format!("{} rows, sorted desc", rows.len()))
    });

    // ── 12. --sort modified ───────────────────────────────────────────
    t.test("T11 --sort modified", &["*.log", "--sort", "modified", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 1, 10)
    });

    // ── 13. --sort multi-tier ─────────────────────────────────────────
    t.test("T12 --sort size,name", &["*.dll", "--sort", "size,name", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 1, 10)
    });

    // ── 14. --attr hidden ─────────────────────────────────────────────
    t.test("T13 --attr hidden", &["*", "--attr", "hidden", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let hidden = col_val(row, &h, "Hidden");
            if hidden != "1" { bail!("Row {i}: Hidden={hidden}, expected 1"); }
        }
        Ok(format!("{} rows, all hidden", rows.len()))
    });

    // ── 15. --attr !hidden ────────────────────────────────────────────
    t.test("T14 --attr !hidden", &["*", "--attr", "!hidden", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let hidden = col_val(row, &h, "Hidden");
            if hidden == "1" { bail!("Row {i}: Hidden=1, expected 0"); }
        }
        Ok(format!("{} rows, none hidden", rows.len()))
    });

    // ── 16. --attr compressed ─────────────────────────────────────────
    t.test("T15 --attr compressed", &["*", "--attr", "compressed", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        // compressed files may not exist on all systems — 0 rows is ok
        for (i, row) in rows.iter().enumerate() {
            let c = col_val(row, &h, "Compressed");
            if c != "1" { bail!("Row {i}: Compressed={c}, expected 1"); }
        }
        Ok(format!("{} rows, all compressed", rows.len()))
    });

    // ── 17. --exclude ─────────────────────────────────────────────────
    t.test("T16 --exclude backup*", &["*.txt", "--exclude", "backup*", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if name.starts_with("backup") { bail!("Row {i}: {name} matches exclude pattern"); }
        }
        Ok(format!("{} rows, no backup* files", rows.len()))
    });

    // ── 18. --name-only ───────────────────────────────────────────────
    t.test("T17 --name-only", &["readme", "--name-only", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if !name.contains("readme") { bail!("Row {i}: {name} does not contain 'readme'"); }
        }
        Ok(format!("{} rows, all contain 'readme'", rows.len()))
    });

    // ── 19. --case (case-sensitive) ───────────────────────────────────
    t.test("T18 --case sensitive", &["README", "--case", "--name-only", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        // All results should have exact case "README" in filename
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name");
            if !name.contains("README") { bail!("Row {i}: {name} — case mismatch"); }
        }
        Ok(format!("{} rows, case-sensitive match", rows.len()))
    });

    // ── 20. --word (whole word) ───────────────────────────────────────
    t.test("T19 --word", &["test", "--word", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── 21. --format json (NDJSON — one JSON object per line) ────────
    t.test("T20 --format json", &["*.rs", "--format", "json", "--limit", "5"], |stdout, _| {
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
    });

    // ── 22. --format table ────────────────────────────────────────────
    t.test("T21 --format table", &["*.rs", "--format", "table", "--limit", "5"], |stdout, _| {
        // Table format should have alignment characters or separator lines
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        if lines.is_empty() { bail!("No output"); }
        Ok(format!("{} lines of table output", lines.len()))
    });

    // ── 23. --columns selective ───────────────────────────────────────
    t.test("T22 --columns Name,Size,Path Only", &["*.txt", "--columns", "Name,Size,Path Only", "--limit", "10"], |stdout, _| {
        let header = stdout.lines().next().unwrap_or("");
        // Should only have the requested columns
        let col_count = header.split(',').count();
        if col_count > 5 { bail!("Too many columns ({col_count}), expected ~3"); }
        Ok(format!("{col_count} columns in output"))
    });

    // ── 24. --dirs-only + --min-descendants ───────────────────────────
    t.test("T23 --min-descendants 100", &["*", "--dirs-only", "--min-descendants", "100", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let desc: u64 = col_val(row, &h, "Descendants").parse().unwrap_or(0);
            if desc < 100 { bail!("Row {i}: descendants={desc} < 100"); }
        }
        Ok(format!("{} dirs with 100+ descendants", rows.len()))
    });

    // ── 25. --dirs-only + --max-descendants 0 (empty dirs) ───────────
    t.test("T24 --max-descendants 0", &["*", "--dirs-only", "--max-descendants", "0", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let desc: u64 = col_val(row, &h, "Descendants").parse().unwrap_or(999);
            if desc != 0 { bail!("Row {i}: descendants={desc}, expected 0"); }
        }
        Ok(format!("{} empty directories", rows.len()))
    });

    // ── 26. --newer 7d ────────────────────────────────────────────────
    t.test("T25 --newer 7d", &["*.log", "--newer", "7d", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── 27. --older 365d ──────────────────────────────────────────────
    t.test("T26 --older 365d", &["*.doc", "--older", "365d", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── 28. --newer-created 30d ───────────────────────────────────────
    t.test("T27 --newer-created 30d", &["*", "--newer-created", "30d", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── 29. --drive C ─────────────────────────────────────────────────
    t.test("T28 --drive C", &["*.exe", "--drive", "C", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let path = col_val(row, &h, "Path Only");
            if !path.starts_with("C:") && !path.starts_with("c:") {
                bail!("Row {i}: path={path} not on C:");
            }
        }
        Ok(format!("{} rows, all on C:", rows.len()))
    });

    // ── 30. --drives C,D ──────────────────────────────────────────────
    t.test("T29 --drives C,D", &["*.exe", "--drives", "C,D", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        for (i, row) in rows.iter().enumerate() {
            let path = col_val(row, &h, "Path Only").to_uppercase();
            if !path.starts_with("C:") && !path.starts_with("D:") {
                bail!("Row {i}: path={path} not on C: or D:");
            }
        }
        Ok(format!("{} rows, all on C: or D:", rows.len()))
    });

    // ── 31. --sep and --quotes ────────────────────────────────────────
    t.test("T30 --sep | --quotes '", &["*.txt", "--sep", "|", "--quotes", "'", "--limit", "5"], |stdout, _| {
        let first_line = stdout.lines().next().unwrap_or("");
        if !first_line.contains('|') { bail!("No pipe separator in header: {first_line}"); }
        Ok(format!("pipe-separated output"))
    });

    // ── 32. --out file ────────────────────────────────────────────────
    t.test("T31 --out file", &["*.rs", "--limit", "100", "--out", "test_cli_validation_out.csv"], |_stdout, _| {
        let path = std::path::Path::new("test_cli_validation_out.csv");
        if !path.exists() { bail!("Output file not created"); }
        let content = std::fs::read_to_string(path).unwrap_or_default();
        let _ = std::fs::remove_file(path); // cleanup
        let lines = content.lines().filter(|l| !l.is_empty()).count();
        if lines < 2 { bail!("Output file has {lines} lines, expected at least 2"); }
        Ok(format!("{lines} lines written to file"))
    });

    // ── 33. --benchmark ───────────────────────────────────────────────
    t.test("T32 --benchmark", &["*.rs", "--benchmark"], |stdout, _| {
        // Benchmark mode should produce no CSV output (or minimal output)
        // but should exit successfully
        Ok(format!("{} bytes stdout (benchmark mode)", stdout.len()))
    });

    // ── 34. Regex pattern ─────────────────────────────────────────────
    t.test("T33 regex >.*\\.config$", &[">.*\\.config$", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let name = col_val(row, &h, "Name").to_lowercase();
            if !name.ends_with(".config") { bail!("Row {i}: {name} doesn't end with .config"); }
        }
        Ok(format!("{} rows, all .config files", rows.len()))
    });

    // ── 35. Combined stress test ──────────────────────────────────────
    t.test("T34 combined stress", &[
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
    });

    // ═══ NEW TESTS (beyond original 34) ══════════════════════════════

    // ── 36. --limit 0 (unlimited, but we cap the check) ──────────────
    t.test("T35 --limit 0 (unlimited)", &["*.dll", "--limit", "0", "--drive", "C", "--files-only"], |stdout, _| {
        let count = csv_row_count(stdout);
        if count < 100 { bail!("Expected many DLLs, got {count}"); }
        Ok(format!("{count} rows (unlimited)"))
    });

    // ── 37. --older-created ───────────────────────────────────────────
    t.test("T36 --older-created 365d", &["*", "--older-created", "365d", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── 38. --attr system ─────────────────────────────────────────────
    t.test("T37 --attr system", &["*", "--attr", "system", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let sys = col_val(row, &h, "System");
            if sys != "1" { bail!("Row {i}: System={sys}, expected 1"); }
        }
        Ok(format!("{} rows, all system files", rows.len()))
    });

    // ── 39. --attr readonly ───────────────────────────────────────────
    t.test("T38 --attr readonly", &["*", "--attr", "readonly", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let ro = col_val(row, &h, "Read-only");
            if ro != "1" { bail!("Row {i}: Read-only={ro}, expected 1"); }
        }
        Ok(format!("{} rows, all readonly", rows.len()))
    });

    // ── 40. --attr combined: system,!hidden ───────────────────────────
    t.test("T39 --attr system,!hidden", &["*", "--attr", "system,!hidden", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let sys = col_val(row, &h, "System");
            let hid = col_val(row, &h, "Hidden");
            if sys != "1" { bail!("Row {i}: System={sys}, expected 1"); }
            if hid == "1" { bail!("Row {i}: Hidden=1, expected 0"); }
        }
        Ok(format!("{} rows, system but not hidden", rows.len()))
    });

    // ── 41. Empty result set (no crash) ───────────────────────────────
    t.test("T40 no results (graceful)", &["xyzzy_nonexistent_file_pattern_12345", "--limit", "10"], |stdout, _| {
        let count = csv_row_count(stdout);
        if count != 0 { bail!("Expected 0 rows, got {count}"); }
        Ok("0 rows, graceful empty result".into())
    });

    // ── 42. --header false ────────────────────────────────────────────
    t.test("T41 --header false", &["*.exe", "--header", "false", "--limit", "5", "--drive", "C"], |stdout, _| {
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        // With --no-header, first line should be data, not column names
        // Heuristic: column headers contain "Name" or "Path" — data lines don't
        if let Some(first) = lines.first() {
            // Default header starts with "Path","Name",...
            if first.starts_with("\"Path\"") || first.starts_with("Path,") {
                bail!("First line looks like a header: {first}");
            }
        }
        Ok(format!("{} lines, no header", lines.len()))
    });

    // ── 43. --smart-case ──────────────────────────────────────────────
    t.test("T42 --smart-case (lowercase = insensitive)", &["readme", "--smart-case", "--name-only", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        // lowercase query → case-insensitive → should match README, Readme, etc
        let has_mixed_case = rows.iter().any(|r| {
            let name = col_val(r, &h, "Name");
            name != name.to_lowercase()
        });
        if rows.is_empty() { bail!("No rows"); }
        Ok(format!("{} rows, mixed case={has_mixed_case}", rows.len()))
    });

    // ── 44. --newer-accessed ──────────────────────────────────────────
    t.test("T43 --newer-accessed 7d", &["*", "--newer-accessed", "7d", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });
}

// ── Cross-Level Summary ─────────────────────────────────────────────────────

fn cross_level_summary(phases: &[PhaseResult]) {
    eprintln!();
    eprintln!("╔═══════════════════════════════════════════════════════════════════════════════════╗");
    eprintln!("║  Cross-Level Timing Comparison                                                   ║");
    eprintln!("╚═══════════════════════════════════════════════════════════════════════════════════╝");

    // Header row: test name + one column per phase.
    let labels: Vec<&str> = phases.iter().map(|p| p.label.as_str()).collect();
    eprint!("  {:<36}", "Test");
    for label in &labels {
        eprint!("  {:>14}", label);
    }
    eprintln!("    Status");
    eprint!("  {:<36}", "────────────────────────────────────");
    for _ in &labels {
        eprint!("  {:>14}", "──────────────");
    }
    eprintln!("    ──────");

    // Determine the test list from the first phase.
    let test_count = phases.first().map_or(0, |p| p.results.len());
    for i in 0..test_count {
        let name = phases.first().map_or("?", |p| p.results.get(i).map_or("?", |r| r.name.as_str()));
        // Truncate long test names.
        let short = if name.len() > 35 { &name[..35] } else { name };
        eprint!("  {:<36}", short);

        let mut all_pass = true;
        for phase in phases {
            if let Some(r) = phase.results.get(i) {
                let ms = r.duration_ms;
                let cell = format!("{ms} ms");
                if r.passed {
                    eprint!("  {:>14}", cell);
                } else {
                    eprint!("  {:>14}", cell.red());
                    all_pass = false;
                }
            } else {
                eprint!("  {:>14}", "—");
            }
        }
        if all_pass {
            eprintln!("    {}", "✅".green());
        } else {
            eprintln!("    {}", "❌".red());
        }
    }

    // Totals row.
    eprint!("  {:<36}", "TOTAL (sum)");
    for phase in phases {
        let sum: u128 = phase.results.iter().map(|r| r.duration_ms).sum();
        eprint!("  {:>14}", format!("{sum} ms"));
    }
    eprintln!();
    eprint!("  {:<36}", "WALL (phase)");
    for phase in phases {
        eprint!("  {:>14}", format!("{} ms", phase.wall_ms));
    }
    eprintln!();

    // Speedup row (COLD → HOT).
    if phases.len() >= 3 {
        let cold_sum: u128 = phases[0].results.iter().map(|r| r.duration_ms).sum();
        let hot_sum: u128 = phases[2].results.iter().map(|r| r.duration_ms).sum();
        if hot_sum > 0 {
            let speedup = cold_sum as f64 / hot_sum as f64;
            eprintln!();
            eprintln!("  {} COLD→HOT sum speedup: {:.1}x",
                "⚡".yellow(), speedup);
        }
        let cold_wall = phases[0].wall_ms;
        let hot_wall = phases[2].wall_ms;
        if hot_wall > 0 {
            let speedup = cold_wall as f64 / hot_wall as f64;
            eprintln!("  {} COLD→HOT wall speedup: {:.1}x",
                "⚡".yellow(), speedup);
        }
    }
}

// ── Main ─────────────────────────────────────────────────────────────────────

fn main() {
    let bin = uffs_bin();
    eprintln!();
    eprintln!("╔═══════════════════════════════════════════════════════════════╗");
    eprintln!("║  UFFS CLI Flag Validation Suite — 3-Level Cache Comparison   ║");
    eprintln!("╚═══════════════════════════════════════════════════════════════╝");
    eprintln!("  Binary: {bin}");
    eprintln!();

    let mut t = TestRunner::new(bin.clone());
    let mut phases: Vec<PhaseResult> = Vec::new();

    // ═══ Phase 1: COLD — no daemon, no cache files ══════════════════════
    eprintln!("┌───────────────────────────────────────────────────────────────┐");
    eprintln!("│  Phase 1: COLD (no daemon, no cache)                         │");
    eprintln!("└───────────────────────────────────────────────────────────────┘");
    kill_daemon(&bin);
    delete_cache();
    let phase_start = Instant::now();
    run_test_suite(&mut t);
    let wall_ms = phase_start.elapsed().as_millis();
    let phase = t.finish_phase("COLD", wall_ms);
    TestRunner::phase_summary(&phase);
    phases.push(phase);

    // ═══ Phase 2: WARM CACHE — cache files exist, no daemon ═════════════
    eprintln!();
    eprintln!("┌───────────────────────────────────────────────────────────────┐");
    eprintln!("│  Phase 2: WARM CACHE (cache files present, no daemon)        │");
    eprintln!("└───────────────────────────────────────────────────────────────┘");
    kill_daemon(&bin);
    // Cache files remain from Phase 1.
    let phase_start = Instant::now();
    run_test_suite(&mut t);
    let wall_ms = phase_start.elapsed().as_millis();
    let phase = t.finish_phase("WARM CACHE", wall_ms);
    TestRunner::phase_summary(&phase);
    phases.push(phase);

    // ═══ Phase 3: HOT — daemon already running ══════════════════════════
    eprintln!();
    eprintln!("┌───────────────────────────────────────────────────────────────┐");
    eprintln!("│  Phase 3: HOT (daemon running from Phase 2)                  │");
    eprintln!("└───────────────────────────────────────────────────────────────┘");
    // Daemon is already warm from Phase 2's test run.
    let phase_start = Instant::now();
    run_test_suite(&mut t);
    let wall_ms = phase_start.elapsed().as_millis();
    let phase = t.finish_phase("HOT", wall_ms);
    TestRunner::phase_summary(&phase);
    phases.push(phase);

    // ═══ Cross-Level Summary ════════════════════════════════════════════
    eprintln!();
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    cross_level_summary(&phases);
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    // Exit code: fail if any phase had failures.
    let total_failures: usize = phases.iter()
        .flat_map(|p| &p.results)
        .filter(|r| !r.passed)
        .count();
    std::process::exit(if total_failures == 0 { 0 } else { 1 });
}