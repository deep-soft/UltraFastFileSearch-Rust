//! Tests for output helpers.
//!
//! All tests exercise the unified `write_native_results` path using
//! `DisplayRow` inputs — no legacy `DataFrame` or streaming paths.

use core::time::Duration;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use uffs_core::output::OutputConfig;
use uffs_core::search::backend::DisplayRow;

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

/// A single `DisplayRow` matching the old `sample_df()` content.
fn sample_rows() -> Vec<DisplayRow> {
    vec![DisplayRow::new(
        'C',
        "C:\\Temp\\file.txt".to_owned(),
        123,
        false,
        1_700_001_100_000_000,
        1_700_001_000_000_000,
        1_700_001_200_000_000,
        0,
        128,
        0,
        0,
        0,
    )]
}

/// 20 000+ `DisplayRow`s for testing the slow-scan footer guard.
fn large_sample_rows() -> Vec<DisplayRow> {
    (0..20_000_u32)
        .map(|idx| {
            DisplayRow::new(
                'C',
                format!("C:\\Temp\\file{idx}.txt"),
                100,
                false,
                0,
                0,
                0,
                0,
                128,
                0,
                0,
                0,
            )
        })
        .collect()
}

// ===================================================================
// write_native_results contract tests
// ===================================================================

#[test]
fn test_write_native_csv_uses_output_config_without_cpp_footer() -> TestResult {
    let path = temp_output_path("csv");
    let rows = sample_rows();
    let output_config = OutputConfig::new()
        .with_columns("path,name")
        .with_separator(";")
        .with_quote("'")
        .with_header(false);

    write_native_results(
        &rows,
        "csv",
        &path.to_string_lossy(),
        &output_config,
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
fn test_write_native_custom_file_appends_cpp_drive_footer() -> TestResult {
    let path = temp_output_path("txt");
    let rows = sample_rows();
    let output_config = OutputConfig::new()
        .with_columns("path,name")
        .with_header(false);

    write_native_results(
        &rows,
        "custom",
        &path.to_string_lossy(),
        &output_config,
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
    let output_config = OutputConfig::new().with_columns("path,name");

    write_native_results(
        &rows,
        "json",
        &path.to_string_lossy(),
        &output_config,
        &['C', 'D'],
        Duration::from_secs(2),
        "*.txt",
    )?;

    let written = fs::read_to_string(&path)?;
    drop(fs::remove_file(&path));

    assert!(!written.contains("Drives?"));
    assert!(written.contains(r#""C:\\Temp\\file.txt""#));
    Ok(())
}

// ===================================================================
// C++ footer tests (via write_native_results in custom format)
// ===================================================================

#[test]
fn test_cpp_footer_includes_fast_scan_message_for_full_scan_pattern() -> TestResult {
    let path = temp_output_path("txt");
    let rows = sample_rows();
    let output_config = OutputConfig::new()
        .with_columns("path,name")
        .with_header(false);

    write_native_results(
        &rows,
        "custom",
        &path.to_string_lossy(),
        &output_config,
        &['G'],
        Duration::from_millis(999),
        "*",
    )?;

    let written = fs::read_to_string(&path)?;
    drop(fs::remove_file(&path));

    assert_eq!(
        written,
        concat!(
            "\"C:\\Temp\\file.txt\",\"file.txt\"\n",
            "\r\n",
            "\r\n",
            "Drives? \t1\tG:\r\n",
            "\r\n",
            "MMMmmm that was FAST ... maybe your searchstring was wrong?\t*\r\n",
            "Search path. E.g. 'C:/' or 'C:\\Prog**' \r\n"
        )
    );
    Ok(())
}

#[test]
fn test_cpp_footer_includes_fast_scan_for_cpp_transformed_pattern() -> TestResult {
    let path = temp_output_path("txt");
    let rows = sample_rows();
    let output_config = OutputConfig::new()
        .with_columns("path,name")
        .with_header(false);

    write_native_results(
        &rows,
        "custom",
        &path.to_string_lossy(),
        &output_config,
        &['G'],
        Duration::from_millis(999),
        ">G:.*",
    )?;

    let written = fs::read_to_string(&path)?;
    drop(fs::remove_file(&path));

    assert_eq!(
        written,
        concat!(
            "\"C:\\Temp\\file.txt\",\"file.txt\"\n",
            "\r\n",
            "\r\n",
            "Drives? \t1\tG:\r\n",
            "\r\n",
            "MMMmmm that was FAST ... maybe your searchstring was wrong?\t>G:.*\r\n",
            "Search path. E.g. 'C:/' or 'C:\\Prog**' \r\n"
        )
    );
    Ok(())
}

#[test]
fn test_cpp_footer_omits_fast_scan_for_real_regex_pattern() -> TestResult {
    let path = temp_output_path("txt");
    let rows = sample_rows();
    let output_config = OutputConfig::new()
        .with_columns("path,name")
        .with_header(false);

    write_native_results(
        &rows,
        "custom",
        &path.to_string_lossy(),
        &output_config,
        &['G'],
        Duration::from_millis(999),
        r">G:.*\.(jpg|png)",
    )?;

    let written = fs::read_to_string(&path)?;
    drop(fs::remove_file(&path));

    assert_eq!(
        written,
        concat!(
            "\"C:\\Temp\\file.txt\",\"file.txt\"\n",
            "\r\n",
            "\r\n",
            "Drives? \t1\tG:\r\n",
            "\r\n",
        )
    );
    Ok(())
}

#[test]
fn test_cpp_footer_omits_fast_scan_message_when_many_results() -> TestResult {
    let path = temp_output_path("txt");
    let rows = large_sample_rows();
    let output_config = OutputConfig::new()
        .with_columns("path,name")
        .with_header(false);

    write_native_results(
        &rows,
        "custom",
        &path.to_string_lossy(),
        &output_config,
        &['G'],
        Duration::from_secs(2),
        ">G:.*",
    )?;

    let written = fs::read_to_string(&path)?;
    drop(fs::remove_file(&path));

    // Should NOT contain fast-scan message (row_count >= 20,000)
    let lines: Vec<&str> = written.lines().collect();
    let footer_start = lines.len().saturating_sub(4);
    assert_eq!(lines.get(footer_start), Some(&""));
    assert_eq!(lines.get(footer_start + 1), Some(&""));
    assert_eq!(lines.get(footer_start + 2), Some(&"Drives? \t1\tG:"));
    assert_eq!(lines.get(footer_start + 3), Some(&""));
    Ok(())
}
