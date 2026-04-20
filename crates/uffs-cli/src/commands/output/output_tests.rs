// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Tests for output helpers.
//!
//! All tests exercise the unified `write_native_results` path using
//! `serde_json::Value` inputs — no polars, no typed protocol structs.

use core::time::Duration;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde_json::json;

use super::{
    ConsoleWriteStrategy, MULTICOL_AVG_BYTES_PER_ROW, MULTICOL_BUFFER_CAP_BYTES,
    choose_console_strategy, write_native_results,
};

type TestResult = Result<()>;

fn temp_output_path(extension: &str) -> PathBuf {
    use core::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0_u128, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!(
        "uffs-cli-output-contract-{}-{nanos}-{seq}.{extension}",
        std::process::id()
    ))
}

/// A single row as JSON Value matching the old `sample_df()` content.
fn sample_rows() -> Vec<serde_json::Value> {
    vec![json!({
        "drive": "C",
        "path": "C:\\Temp\\file.txt",
        "name": "file.txt",
        "size": 123,
        "is_directory": false,
        "modified": 1_700_001_100_000_000_i64,
        "created": 1_700_001_000_000_000_i64,
        "accessed": 1_700_001_200_000_000_i64,
        "flags": 0,
        "allocated": 128,
        "descendants": 0,
        "treesize": 0,
        "tree_allocated": 0,
    })]
}

/// 20 000+ rows for testing the slow-scan footer guard.
fn large_sample_rows() -> Vec<serde_json::Value> {
    (0..20_000_u64)
        .map(|idx| {
            json!({
                "drive": "C",
                "path": format!("C:\\Temp\\file{idx}.txt"),
                "name": format!("file{idx}.txt"),
                "size": 100_u64,
                "is_directory": false,
                "modified": 0_i64,
                "created": 0_i64,
                "accessed": 0_i64,
                "flags": 0_u32,
                "allocated": 128_u64,
                "descendants": 0_u32,
                "treesize": 0_u64,
                "tree_allocated": 0_u64,
            })
        })
        .collect()
}

// ===================================================================
// write_native_results contract tests
// ===================================================================

#[test]
fn test_write_native_csv_uses_columns_without_legacy_footer() -> TestResult {
    let path = temp_output_path("csv");
    let rows = sample_rows();

    write_native_results(
        &rows,
        "csv",
        &path.to_string_lossy(),
        "path,name",
        ";",
        "'",
        false,
        "1",
        "0",
        None,
        &['C', 'D'],
        Duration::from_secs(2),
        "*.txt",
    )?;

    let written = fs::read_to_string(&path)?;
    drop(fs::remove_file(&path));

    assert_eq!(written, "'C:\\Temp\\file.txt';'file.txt'\n");
    Ok(())
}

#[test]
fn test_write_native_custom_file_appends_legacy_drive_footer() -> TestResult {
    let path = temp_output_path("txt");
    let rows = sample_rows();

    write_native_results(
        &rows,
        "custom",
        &path.to_string_lossy(),
        "path,name",
        ",",
        "\"",
        false,
        "1",
        "0",
        None,
        &['C', 'D'],
        Duration::from_secs(2),
        "*.txt",
    )?;

    let written = fs::read_to_string(&path)?;
    drop(fs::remove_file(&path));

    // With glob pattern "*.txt", few results is expected — no MMMmmm warning.
    assert_eq!(
        written,
        concat!(
            "\"C:\\Temp\\file.txt\",\"file.txt\"\n",
            "\r\n",
            "\r\n",
            "Drives? \t2\tC:|D:\r\n",
            "\r\n",
        )
    );
    Ok(())
}

#[test]
fn test_write_native_json_file_has_no_footer() -> TestResult {
    let path = temp_output_path("json");
    let rows = sample_rows();

    write_native_results(
        &rows,
        "json",
        &path.to_string_lossy(),
        "path,name",
        "\t",
        "",
        false,
        "1",
        "0",
        None,
        &['C', 'D'],
        Duration::from_secs(2),
        "*.txt",
    )?;

    let written = fs::read_to_string(&path)?;
    drop(fs::remove_file(&path));

    assert!(!written.contains("Drives?"));
    assert!(written.contains("C:\\\\Temp\\\\file.txt"));
    Ok(())
}

// ===================================================================
// Legacy footer tests (via write_native_results in custom format)
// ===================================================================

#[test]
fn test_legacy_footer_includes_fast_scan_message_for_full_scan_pattern() -> TestResult {
    let path = temp_output_path("txt");
    let rows = sample_rows();

    write_native_results(
        &rows,
        "custom",
        &path.to_string_lossy(),
        "path,name",
        ",",
        "\"",
        false,
        "1",
        "0",
        None,
        &['G'],
        Duration::from_millis(999),
        "*",
    )?;

    let written = fs::read_to_string(&path)?;
    drop(fs::remove_file(&path));

    assert!(written.contains("Drives? \t1\tG:"));
    assert!(written.contains("MMMmmm that was FAST"));
    Ok(())
}

#[test]
fn test_legacy_footer_includes_fast_scan_for_transformed_pattern() -> TestResult {
    let path = temp_output_path("txt");
    let rows = sample_rows();

    write_native_results(
        &rows,
        "custom",
        &path.to_string_lossy(),
        "path,name",
        ",",
        "\"",
        false,
        "1",
        "0",
        None,
        &['G'],
        Duration::from_millis(999),
        ">G:.*",
    )?;

    let written = fs::read_to_string(&path)?;
    drop(fs::remove_file(&path));

    assert!(written.contains("Drives? \t1\tG:"));
    assert!(written.contains("MMMmmm that was FAST"));
    Ok(())
}

#[test]
fn test_legacy_footer_omits_fast_scan_for_real_regex_pattern() -> TestResult {
    let path = temp_output_path("txt");
    let rows = sample_rows();

    write_native_results(
        &rows,
        "custom",
        &path.to_string_lossy(),
        "path,name",
        ",",
        "\"",
        false,
        "1",
        "0",
        None,
        &['G'],
        Duration::from_millis(999),
        r">G:.*\.(jpg|png)",
    )?;

    let written = fs::read_to_string(&path)?;
    drop(fs::remove_file(&path));

    assert!(written.contains("Drives? \t1\tG:"));
    assert!(!written.contains("MMMmmm"));
    Ok(())
}

#[test]
fn test_legacy_footer_omits_fast_scan_message_when_many_results() -> TestResult {
    let path = temp_output_path("txt");
    let rows = large_sample_rows();

    write_native_results(
        &rows,
        "custom",
        &path.to_string_lossy(),
        "path,name",
        ",",
        "\"",
        false,
        "1",
        "0",
        None,
        &['G'],
        Duration::from_secs(2),
        ">G:.*",
    )?;

    let written = fs::read_to_string(&path)?;
    drop(fs::remove_file(&path));

    // Should NOT contain fast-scan message (row_count >= 20,000)
    assert!(written.contains("Drives? \t1\tG:"));
    assert!(!written.contains("MMMmmm"));
    Ok(())
}

// ── Phase 3.2: single-buffer multi-column console render ────────────

/// Tiny row counts fit comfortably under the cap → `SingleBuffer`.
/// This is the common case — any interactive query picks this branch.
#[test]
fn choose_console_strategy_small_result_uses_single_buffer() {
    // 100 rows × 256 B = 25 KB — well under the 50 MB cap.
    assert_eq!(
        choose_console_strategy(100, MULTICOL_BUFFER_CAP_BYTES, MULTICOL_AVG_BYTES_PER_ROW),
        ConsoleWriteStrategy::SingleBuffer
    );
}

/// The 45K-row benchmark baseline stays on the fast path — locks in
/// the expected behaviour for the Phase 2 Run 11 workload.
#[test]
fn choose_console_strategy_benchmark_row_count_uses_single_buffer() {
    // 45_000 × 256 B ≈ 11 MB — still well under 50 MB.
    assert_eq!(
        choose_console_strategy(45_000, MULTICOL_BUFFER_CAP_BYTES, MULTICOL_AVG_BYTES_PER_ROW),
        ConsoleWriteStrategy::SingleBuffer
    );
}

/// Pathologically large result sets flip to streaming so peak RSS
/// stays bounded.  With the production constants, the threshold is
/// roughly 200K rows (50 MB / 256 B).
#[test]
fn choose_console_strategy_huge_result_falls_back_to_streaming() {
    // 1_000_000 × 256 B = 256 MB — 5× over the cap.
    assert_eq!(
        choose_console_strategy(
            1_000_000,
            MULTICOL_BUFFER_CAP_BYTES,
            MULTICOL_AVG_BYTES_PER_ROW
        ),
        ConsoleWriteStrategy::Streaming
    );
}

/// Boundary case: `row_count × est == cap` is inclusive of the buffer
/// path (`<=`).  Exactly-at-cap inputs render in one buffer.
#[test]
fn choose_console_strategy_exactly_at_cap_uses_single_buffer() {
    // row_count chosen so row_count × 256 == 50 MB exactly.
    let exact = MULTICOL_BUFFER_CAP_BYTES / MULTICOL_AVG_BYTES_PER_ROW;
    assert_eq!(
        choose_console_strategy(exact, MULTICOL_BUFFER_CAP_BYTES, MULTICOL_AVG_BYTES_PER_ROW),
        ConsoleWriteStrategy::SingleBuffer
    );

    // One more row tips over — streaming.
    assert_eq!(
        choose_console_strategy(
            exact + 1,
            MULTICOL_BUFFER_CAP_BYTES,
            MULTICOL_AVG_BYTES_PER_ROW
        ),
        ConsoleWriteStrategy::Streaming
    );
}

/// Overflow guard: `usize::MAX × 256` saturates, so the decision must
/// not silently wrap and misclassify as `SingleBuffer`.  A regression
/// here would mean catastrophic allocation attempts for attacker-
/// controlled pagination cursors.
#[test]
fn choose_console_strategy_saturates_on_overflow() {
    assert_eq!(
        choose_console_strategy(
            usize::MAX,
            MULTICOL_BUFFER_CAP_BYTES,
            MULTICOL_AVG_BYTES_PER_ROW
        ),
        ConsoleWriteStrategy::Streaming
    );
}

/// A zero-byte cap forces every non-empty result onto the streaming
/// path — useful for tests that want to exercise the fallback without
/// generating millions of synthetic rows.
#[test]
fn choose_console_strategy_tiny_cap_forces_streaming() {
    // 1 row × 256 B > 0 B → Streaming.
    assert_eq!(
        choose_console_strategy(1, 0, MULTICOL_AVG_BYTES_PER_ROW),
        ConsoleWriteStrategy::Streaming
    );
    // 0 rows stays on SingleBuffer even with a zero cap — `0 <= 0`.
    assert_eq!(
        choose_console_strategy(0, 0, MULTICOL_AVG_BYTES_PER_ROW),
        ConsoleWriteStrategy::SingleBuffer
    );
}
