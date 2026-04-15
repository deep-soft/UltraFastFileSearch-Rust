// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Shared display formatting utilities.
//!
//! Lightweight formatters for numbers, bytes, timestamps, and durations.
//! Used by the thin CLI client and MCP server — no heavy dependencies.
//!
//! These are intentionally duplicated from `uffs-core::format` so the CLI
//! binary does not need to link `uffs-core` (and transitively, polars).

/// Formats a number with comma separators for readability.
///
/// Examples: `1234567` → `"1,234,567"`, `1000` → `"1,000"`
#[must_use]
pub fn format_number_commas(num: u64) -> String {
    let num_str = num.to_string();
    let mut result = String::with_capacity(num_str.len() + num_str.len() / 3);
    for (idx, ch) in num_str.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}

/// Formats a byte count in human-readable form based on magnitude.
///
/// - < 1 KB: `1234 B`
/// - < 1 MB: `123.45 KB`
/// - < 1 GB: `123.45 MB`
/// - < 1 TB: `123.45 GB`
/// - >= 1 TB: `123.45 TB`
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "precision loss acceptable for display"
)]
#[expect(
    clippy::float_arithmetic,
    reason = "floating-point arithmetic required for human-readable byte formatting"
)]
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes:>4} B")
    } else if bytes < 1024 * 1024 {
        format!("{:>7.2} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:>7.2} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes < 1024 * 1024 * 1024 * 1024 {
        format!("{:>7.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else {
        format!(
            "{:>7.2} TB",
            bytes as f64 / (1024.0 * 1024.0 * 1024.0 * 1024.0)
        )
    }
}

/// Formats a duration intelligently based on magnitude.
///
/// - Days+: `2d 3h 5m 10s`
/// - Hours+: `3h 5m 10s`
/// - Minutes+: `5 m 10 s`
/// - Seconds+: `10 s 500 ms`
/// - Milliseconds+: `500 ms 250 μs`
/// - Sub-ms: `250 μs 100 ns`
#[must_use]
pub fn format_duration(duration: core::time::Duration) -> String {
    let total_seconds = duration.as_secs();
    let seconds = total_seconds % 60;
    let minutes = (total_seconds / 60) % 60;
    let hours = (total_seconds / 3600) % 24;
    let days = total_seconds / 86400;
    let milliseconds = duration.subsec_millis();
    let microseconds = duration.subsec_micros() % 1_000;
    let nanoseconds = duration.subsec_nanos() % 1_000;

    if days > 0 {
        format!("{days:>2}d {hours:>2}h {minutes:>2}m {seconds:>2}s")
    } else if hours > 0 {
        format!("{hours:>2}h {minutes:>2}m {seconds:>2}s")
    } else if minutes > 0 {
        format!("{minutes:>3} m  {seconds:>3} s ")
    } else if seconds > 0 {
        format!("{seconds:>3} s  {milliseconds:>3} ms")
    } else if milliseconds > 0 {
        format!("{milliseconds:>3} ms {microseconds:>3} μs")
    } else if microseconds > 0 {
        format!("{microseconds:>3} μs {nanoseconds:>3} ns")
    } else {
        format!("{nanoseconds:>3} ns")
    }
}

/// Convert a non-negative `f64` to `u64`, clamping negatives to 0.
///
/// Used for converting floating-point statistics (averages, etc.) back to
/// integer representations for formatting.
#[inline]
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "centralized f64→u64 conversion; precision loss at u64::MAX boundary is acceptable"
)]
pub fn f64_to_u64(val: f64) -> u64 {
    if val <= 0.0 {
        0
    } else if val >= u64::MAX as f64 {
        u64::MAX
    } else {
        val as u64
    }
}

/// Convert a `u64` to `f64` for display ratios and percentages.
///
/// Precision loss beyond 2^53 is acceptable for display.
#[inline]
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "display-only: sub-unit precision irrelevant for ratios"
)]
pub const fn u64_to_f64(val: u64) -> f64 {
    val as f64
}

/// Parse a month/quarter spec into a vector of allowed months (1-12).
///
/// Accepts:
/// - Month names: `january`, `jan`, `february`, `feb`, … , `december`, `dec`
/// - Quarter names: `Q1`, `Q2`, `Q3`, `Q4`
/// - Comma-separated combinations: `jan,feb`, `Q1,Q3`
///
/// ```
/// # use uffs_client::format::parse_month_spec;
/// assert_eq!(parse_month_spec("january"), vec![1]);
/// assert_eq!(parse_month_spec("Q1"), vec![1, 2, 3]);
/// assert_eq!(parse_month_spec("jan,feb"), vec![1, 2]);
/// ```
#[must_use]
pub fn parse_month_spec(spec: &str) -> Vec<u32> {
    let mut months = Vec::new();
    for token in spec.split(',') {
        let lower = token.trim().to_ascii_lowercase();
        match lower.as_str() {
            "january" | "jan" => months.push(1),
            "february" | "feb" => months.push(2),
            "march" | "mar" => months.push(3),
            "april" | "apr" => months.push(4),
            "may" => months.push(5),
            "june" | "jun" => months.push(6),
            "july" | "jul" => months.push(7),
            "august" | "aug" => months.push(8),
            "september" | "sep" => months.push(9),
            "october" | "oct" => months.push(10),
            "november" | "nov" => months.push(11),
            "december" | "dec" => months.push(12),
            "q1" => months.extend_from_slice(&[1, 2, 3]),
            "q2" => months.extend_from_slice(&[4, 5, 6]),
            "q3" => months.extend_from_slice(&[7, 8, 9]),
            "q4" => months.extend_from_slice(&[10, 11, 12]),
            _ => {} // silently ignore unknown tokens
        }
    }
    months.sort_unstable();
    months.dedup();
    months
}

/// Parse a human-readable size spec into bytes.
///
/// Accepts: `1024`, `10KB`, `5MB`, `2GB`, `1TB`, `100B`.
/// Case-insensitive suffixes.
///
/// # Errors
///
/// Returns an error string if the input is empty or not a valid number.
pub fn parse_size(spec: &str) -> Result<u64, String> {
    const SUFFIXES: &[(&str, u64)] = &[
        ("TB", 1024 * 1024 * 1024 * 1024),
        ("GB", 1024 * 1024 * 1024),
        ("MB", 1024 * 1024),
        ("KB", 1024),
        ("B", 1),
    ];

    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err("empty size specification".to_owned());
    }

    let upper = trimmed.to_ascii_uppercase();

    let (digits, multiplier) = SUFFIXES
        .iter()
        .find_map(|(suffix, mult)| upper.strip_suffix(suffix).map(|rest| (rest, *mult)))
        .unwrap_or((upper.as_str(), 1));

    let count: u64 = digits
        .trim()
        .parse()
        .map_err(|_parse_err| format!("invalid size: {spec}"))?;

    Ok(count.saturating_mul(multiplier))
}

/// Check whether a string is a recognized aggregate preset name.
///
/// The full expansion and execution of presets happens on the daemon side;
/// this is a lightweight check so the thin CLI can validate user input
/// before sending it over the wire.
#[must_use]
pub fn is_aggregate_preset(spec: &str) -> bool {
    matches!(
        spec.to_ascii_lowercase().as_str(),
        "overview"
            | "by_type"
            | "bytype"
            | "type"
            | "by_extension"
            | "byextension"
            | "extension"
            | "by_ext"
            | "ext"
            | "by_drive"
            | "bydrive"
            | "drive"
            | "by_size"
            | "bysize"
            | "size"
            | "by_age"
            | "byage"
            | "age"
            | "storage"
            | "activity"
            | "top_folders"
            | "topfolders"
            | "folders"
            | "duplicates"
            | "dups"
            | "media"
            | "cleanup"
    )
}

/// Extract a drive letter from a search pattern, if present.
///
/// Patterns like `c:/*.txt` or `D:\folder` start with a drive prefix.
/// Returns `Some('C')` (uppercased) if found, `None` otherwise.
///
/// ```
/// # use uffs_client::format::extract_drive_letter;
/// assert_eq!(extract_drive_letter("c:/*.txt"), Some('C'));
/// assert_eq!(extract_drive_letter("D:\\folder"), Some('D'));
/// assert_eq!(extract_drive_letter("*.txt"), None);
/// assert_eq!(extract_drive_letter(">regex"), None);
/// ```
#[must_use]
pub fn extract_drive_letter(pattern: &str) -> Option<char> {
    let bytes = pattern.as_bytes();
    let first = *bytes.first()?;
    let second = *bytes.get(1)?;
    (second == b':' && first.is_ascii_alphabetic()).then(|| (first as char).to_ascii_uppercase())
}
