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

use super::write_native_results;

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
