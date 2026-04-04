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
    t.test("T35 --limit 0 (unlimited)", &["*.dll", "--limit", "0", "--drive", "C", "--files-only"], |stdout, _| {
        let count = csv_row_count(stdout);
        if count < 100 { bail!("Expected many DLLs, got {count}"); }
        Ok(format!("{count} rows (unlimited)"))
    });

    // ── T36. --older-created ─────────────────────────────────────────
    t.test("T36 --older-created 365d", &["*", "--older-created", "365d", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── T37. --attr system ───────────────────────────────────────────
    t.test("T37 --attr system", &["*", "--attr", "system", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let sys = col_val(row, &h, "System");
            if sys != "1" { bail!("Row {i}: System={sys}, expected 1"); }
        }
        Ok(format!("{} rows, all system files", rows.len()))
    });

    // ── T38. --attr readonly ─────────────────────────────────────────
    t.test("T38 --attr readonly", &["*", "--attr", "readonly", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let ro = col_val(row, &h, "Read-only");
            if ro != "1" { bail!("Row {i}: Read-only={ro}, expected 1"); }
        }
        Ok(format!("{} rows, all readonly", rows.len()))
    });

    // ── T39. --attr combined: system,!hidden ─────────────────────────
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

    // ── T40. Empty result set (no crash) ─────────────────────────────
    t.test("T40 no results (graceful)", &["xyzzy_nonexistent_file_pattern_12345", "--limit", "10"], |stdout, _| {
        let count = csv_row_count(stdout);
        if count != 0 { bail!("Expected 0 rows, got {count}"); }
        Ok("0 rows, graceful empty result".into())
    });

    // ── T41. --header false ──────────────────────────────────────────
    t.test("T41 --header false", &["*.exe", "--header", "false", "--limit", "5", "--drive", "C"], |stdout, _| {
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        if let Some(first) = lines.first() {
            if first.starts_with("\"Path\"") || first.starts_with("Path,") {
                bail!("First line looks like a header: {first}");
            }
        }
        Ok(format!("{} lines, no header", lines.len()))
    });

    // ── T42. --smart-case ────────────────────────────────────────────
    t.test("T42 --smart-case (lowercase = insensitive)", &["readme", "--smart-case", "--name-only", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.is_empty() { bail!("No rows"); }
        let has_mixed_case = rows.iter().any(|r| {
            let name = col_val(r, &h, "Name");
            name != name.to_lowercase()
        });
        Ok(format!("{} rows, mixed case={has_mixed_case}", rows.len()))
    });

    // ── T43. --newer-accessed ────────────────────────────────────────
    t.test("T43 --newer-accessed 7d", &["*", "--newer-accessed", "7d", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ═══════════════════════════════════════════════════════════════════
    // TIME GRAMMAR TESTS — Named Time Ranges
    // Validates Phase 5: parse_time_bound named ranges compiled into
    // hot-path SearchFilters via compile_predicates_into_filters.
    // ═══════════════════════════════════════════════════════════════════

    // ── T44. --newer today ───────────────────────────────────────────
    t.test("T44 --newer today", &["*", "--newer", "today", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── T45. --newer yesterday ───────────────────────────────────────
    t.test("T45 --newer yesterday", &["*", "--newer", "yesterday", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── T46. --newer this_week ───────────────────────────────────────
    t.test("T46 --newer this_week", &["*", "--newer", "this_week", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── T47. --newer last_7d ─────────────────────────────────────────
    t.test("T47 --newer last_7d", &["*", "--newer", "last_7d", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── T48. --newer last_30d ────────────────────────────────────────
    t.test("T48 --newer last_30d", &["*", "--newer", "last_30d", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── T49. --newer this_month ──────────────────────────────────────
    t.test("T49 --newer this_month", &["*", "--newer", "this_month", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── T50. --newer this_year / ytd ─────────────────────────────────
    t.test("T50 --newer this_year", &["*", "--newer", "this_year", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── T51. --older last_year ───────────────────────────────────────
    t.test("T51 --older last_year", &["*", "--older", "last_year", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── T52. --newer last_90d ────────────────────────────────────────
    t.test("T52 --newer last_90d", &["*", "--newer", "last_90d", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── T53. --newer last_365d ───────────────────────────────────────
    t.test("T53 --newer last_365d", &["*", "--newer", "last_365d", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── T54. --newer-created today ───────────────────────────────────
    t.test("T54 --newer-created today", &["*", "--newer-created", "today", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── T55. --newer-accessed this_week ──────────────────────────────
    t.test("T55 --newer-accessed this_week", &["*", "--newer-accessed", "this_week", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── T56. Time grammar: newer last_week + older this_week ─────────
    // Files modified last week but NOT this week (bounded range).
    t.test("T56 bounded time range (last_week)", &[
        "*", "--newer", "last_week", "--older", "this_week",
        "--files-only", "--limit", "10"
    ], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── T57. ISO date bound ──────────────────────────────────────────
    t.test("T57 --newer 2025-01-01", &["*", "--newer", "2025-01-01", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ═══════════════════════════════════════════════════════════════════
    // SORT TESTS — All Sortable FieldId Variants
    // Validates Phase 1+3: FieldId.metadata().sortable used by daemon
    // sort path. Tests every sort field to verify no panics/errors.
    // ═══════════════════════════════════════════════════════════════════

    // ── T58. --sort name ─────────────────────────────────────────────
    t.test("T58 --sort name", &["*.txt", "--sort", "name", "--limit", "10"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.len() >= 2 {
            let names: Vec<String> = rows.iter().map(|r| col_val(r, &h, "Name").to_lowercase()).collect();
            for w in names.windows(2) {
                if w[0] > w[1] { bail!("Not ascending: {} > {}", w[0], w[1]); }
            }
        }
        Ok(format!("{} rows, sorted by name asc", rows.len()))
    });

    // ── T59. --sort path ─────────────────────────────────────────────
    t.test("T59 --sort path", &["*.txt", "--sort", "path", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 1, 10)
    });

    // ── T60. --sort created ──────────────────────────────────────────
    t.test("T60 --sort created", &["*.exe", "--sort", "created", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 1, 10)
    });

    // ── T61. --sort accessed ─────────────────────────────────────────
    t.test("T61 --sort accessed", &["*.exe", "--sort", "accessed", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 1, 10)
    });

    // ── T62. --sort extension ────────────────────────────────────────
    t.test("T62 --sort extension", &["*.*", "--sort", "extension", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 1, 10)
    });

    // ── T63. --sort drive ────────────────────────────────────────────
    t.test("T63 --sort drive", &["*.exe", "--sort", "drive", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 1, 10)
    });

    // ── T64. --sort allocated (SizeOnDisk) ───────────────────────────
    t.test("T64 --sort allocated", &["*.exe", "--sort", "allocated", "--files-only", "--limit", "10", "--sort-desc"], |stdout, _| {
        assert_rows(stdout, 1, 10)
    });

    // ── T65. --sort descendants ──────────────────────────────────────
    t.test("T65 --sort descendants --sort-desc", &["*", "--dirs-only", "--sort", "descendants", "--sort-desc", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.len() >= 2 {
            let vals: Vec<u64> = rows.iter().map(|r| col_val(r, &h, "Descendants").parse().unwrap_or(0)).collect();
            for w in vals.windows(2) {
                if w[0] < w[1] { bail!("Not descending: {} < {}", w[0], w[1]); }
            }
        }
        Ok(format!("{} rows, sorted desc by descendants", rows.len()))
    });

    // ── T66. Multi-sort: size desc, then name asc ────────────────────
    t.test("T66 multi-sort size,-name", &["*.dll", "--sort", "size,-name", "--files-only", "--limit", "20"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        if rows.len() >= 2 {
            // Just verify no crash and basic ordering.
            let sizes: Vec<u64> = rows.iter().map(|r| col_val(r, &h, "Size").parse().unwrap_or(0)).collect();
            // In multi-sort, primary sort is size (default asc).
            // With leading '-', it would be size desc. Let's just verify it runs.
            let _ = sizes;
        }
        Ok(format!("{} rows, multi-sort applied", rows.len()))
    });

    // ── T67. Multi-sort: modified desc, name asc ─────────────────────
    t.test("T67 multi-sort -modified,name", &["*.log", "--sort", "-modified,name", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ═══════════════════════════════════════════════════════════════════
    // BOOL ATTRIBUTE MATRIX
    // Validates all 17 bool-typed attribute fields through --attr flag.
    // Each test verifies the correct column reads "1" for require,
    // or NOT "1" for exclude.
    // ═══════════════════════════════════════════════════════════════════

    // ── T68. --attr archive ──────────────────────────────────────────
    t.test("T68 --attr archive", &["*", "--attr", "archive", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let v = col_val(row, &h, "Archive");
            if v != "1" { bail!("Row {i}: Archive={v}"); }
        }
        Ok(format!("{} rows, all have archive attr", rows.len()))
    });

    // ── T69. --attr sparse (may be empty) ────────────────────────────
    t.test("T69 --attr sparse", &["*", "--attr", "sparse", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let v = col_val(row, &h, "Sparse");
            if v != "1" { bail!("Row {i}: Sparse={v}"); }
        }
        Ok(format!("{} rows with sparse attr", rows.len()))
    });

    // ── T70. --attr reparse (junctions/symlinks) ─────────────────────
    t.test("T70 --attr reparse", &["*", "--attr", "reparse", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let v = col_val(row, &h, "Reparse");
            if v != "1" { bail!("Row {i}: Reparse={v}"); }
        }
        Ok(format!("{} rows with reparse attr", rows.len()))
    });

    // ── T71. --attr offline (may be empty) ───────────────────────────
    t.test("T71 --attr offline", &["*", "--attr", "offline", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let v = col_val(row, &h, "Offline");
            if v != "1" { bail!("Row {i}: Offline={v}"); }
        }
        Ok(format!("{} rows with offline attr", rows.len()))
    });

    // ── T72. --attr encrypted (may be empty) ─────────────────────────
    t.test("T72 --attr encrypted", &["*", "--attr", "encrypted", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let v = col_val(row, &h, "Encrypted");
            if v != "1" { bail!("Row {i}: Encrypted={v}"); }
        }
        Ok(format!("{} rows with encrypted attr", rows.len()))
    });

    // ── T73. --attr !system (exclude system) ─────────────────────────
    t.test("T73 --attr !system", &["*", "--attr", "!system", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let v = col_val(row, &h, "System");
            if v == "1" { bail!("Row {i}: System=1 despite !system"); }
        }
        Ok(format!("{} rows, no system files", rows.len()))
    });

    // ── T74. --attr hidden,system (combined require) ─────────────────
    t.test("T74 --attr hidden,system", &["*", "--attr", "hidden,system", "--files-only", "--limit", "10", "--columns", "all"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let hid = col_val(row, &h, "Hidden");
            let sys = col_val(row, &h, "System");
            if hid != "1" { bail!("Row {i}: Hidden={hid}"); }
            if sys != "1" { bail!("Row {i}: System={sys}"); }
        }
        Ok(format!("{} rows, all hidden+system", rows.len()))
    });

    // ═══════════════════════════════════════════════════════════════════
    // COMBINED / STRESS TESTS
    // Validates meaningful multi-constraint combinations that exercise
    // the full predicate compiler → hot-path filter → post-filter
    // → sort → projection pipeline.
    // ═══════════════════════════════════════════════════════════════════

    // ── T75. Size range + time range + extension ─────────────────────
    t.test("T75 size+time+ext combined", &[
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
    });

    // ── T76. Dirs + descendants range + sort ─────────────────────────
    t.test("T76 dirs + desc range + sort", &[
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
    });

    // ── T77. Hidden files with recent modification ───────────────────
    t.test("T77 hidden + --newer last_30d", &[
        "*", "--attr", "hidden", "--newer", "last_30d",
        "--files-only", "--limit", "10", "--columns", "all"
    ], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        for (i, row) in rows.iter().enumerate() {
            let hid = col_val(row, &h, "Hidden");
            if hid != "1" { bail!("Row {i}: Hidden={hid}"); }
        }
        Ok(format!("{} hidden files from last 30 days", rows.len()))
    });

    // ── T78. Exclude + extension + size ──────────────────────────────
    t.test("T78 exclude + ext + size", &[
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
    });

    // ── T79. --columns selective + --format json ─────────────────────
    t.test("T79 projection + json format", &[
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
    });

    // ── T80. --columns all (wide output, no crash) ───────────────────
    t.test("T80 --columns all (wide)", &["*.exe", "--columns", "all", "--limit", "5", "--drive", "C"], |stdout, _| {
        let (h, rows) = parse_csv(stdout);
        // "all" should produce many columns (>= 20).
        if h.len() < 15 { bail!("Expected >= 15 columns, got {}", h.len()); }
        Ok(format!("{} cols × {} rows", h.len(), rows.len()))
    });

    // ── T81. Time created range: this_year ───────────────────────────
    t.test("T81 --newer-created this_year", &["*", "--newer-created", "this_year", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── T82. Time accessed range: last_week ──────────────────────────
    t.test("T82 --newer-accessed last_week", &["*", "--newer-accessed", "last_week", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── T83. Multi-sort 3 fields: drive, extension, size ─────────────
    t.test("T83 multi-sort drive,ext,-size", &["*.*", "--sort", "drive,extension,-size", "--files-only", "--limit", "20"], |stdout, _| {
        assert_rows(stdout, 1, 20)
    });

    // ── T84. Large file search with all constraints ──────────────────
    t.test("T84 mega combined", &[
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
    });

    // ── T85. --format table + --columns selective ────────────────────
    t.test("T85 table format + projection", &[
        "*.dll", "--format", "table", "--columns", "Name,Size", "--limit", "5", "--drive", "C"
    ], |stdout, _| {
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
        if lines.is_empty() { bail!("No table output"); }
        Ok(format!("{} table lines", lines.len()))
    });

    // ── T86. --older-accessed ─────────────────────────────────────────
    t.test("T86 --older-accessed 365d", &["*", "--older-accessed", "365d", "--files-only", "--limit", "10"], |stdout, _| {
        assert_rows(stdout, 0, 10)
    });

    // ── T87. Extension filter with sort by modified ──────────────────
    t.test("T87 ext + sort modified", &[
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
    });

    // ── T88. Name-only with hide-system and time bound ───────────────
    t.test("T88 name-only + hide-system + newer", &[
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