// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Shared display formatting utilities.
//!
//! Human-readable formatters for numbers, bytes, timestamps, booleans, and
//! durations. Used by CLI, TUI, GUI, and diagnostic surfaces.

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
    clippy::float_arithmetic,
    reason = "floating-point arithmetic required for human-readable byte formatting"
)]
pub fn format_bytes(bytes: u64) -> String {
    let bytes_f64 = uffs_mft::u64_to_f64(bytes);
    if bytes < 1024 {
        format!("{bytes:>4} B")
    } else if bytes < 1024 * 1024 {
        format!("{:>7.2} KB", bytes_f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:>7.2} MB", bytes_f64 / (1024.0 * 1024.0))
    } else if bytes < 1024 * 1024 * 1024 * 1024 {
        format!("{:>7.2} GB", bytes_f64 / (1024.0 * 1024.0 * 1024.0))
    } else {
        format!(
            "{:>7.2} TB",
            bytes_f64 / (1024.0 * 1024.0 * 1024.0 * 1024.0)
        )
    }
}

/// Formats a Windows FILETIME as `YYYY-MM-DD HH:MM:SS`.
///
/// Returns `"—"` for zero/invalid timestamps.
///
/// Uses Howard Hinnant's civil calendar algorithm (same as the CLI's
/// `append_datetime`). No external crate dependency.
#[must_use]
pub fn format_timestamp(filetime: i64) -> String {
    match uffs_time::filetime_to_calendar(filetime) {
        Some((year, month, day, hour, minute, second)) => {
            format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
        }
        None => "—".to_owned(),
    }
}

/// Formats a boolean as a filled or hollow circle glyph.
///
/// - `true` → `"●"` (full moon / filled circle)
/// - `false` → `"○"` (hollow moon / empty circle)
///
/// Intended for NTFS boolean attribute columns (Read-only, Hidden, etc.)
/// where a compact visual indicator is clearer than `1` / `0`.
#[must_use]
pub const fn format_bool(value: bool) -> &'static str {
    if value { "●" } else { "○" }
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

#[cfg(test)]
mod tests {
    use uffs_time::{FILETIME_TICKS_PER_SECOND, FILETIME_UNIX_DIFF};

    use super::*;

    // ═══════════════════════════════════════════════════════════════════
    // format_timestamp
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn format_timestamp_zero_returns_dash() {
        assert_eq!(format_timestamp(0), "—");
    }

    #[test]
    fn format_timestamp_post_1970() {
        // 2024-01-01 00:00:00 UTC
        let ft: i64 = 133_485_408_000_000_000;
        assert_eq!(format_timestamp(ft), "2024-01-01 00:00:00");
    }

    #[test]
    fn format_timestamp_pre_1970() {
        // 1959-12-02 03:45:50 — the parity baseline case.
        let unix_secs: i64 = -318_197_650;
        let ft = unix_secs * FILETIME_TICKS_PER_SECOND + FILETIME_UNIX_DIFF;
        assert_eq!(format_timestamp(ft), "1959-12-02 03:45:50");
    }

    #[test]
    fn format_timestamp_leap_day() {
        // 2000-02-29 12:00:00 — century leap year.
        let unix_secs: i64 = 951_825_600;
        let ft = unix_secs * FILETIME_TICKS_PER_SECOND + FILETIME_UNIX_DIFF;
        assert_eq!(format_timestamp(ft), "2000-02-29 12:00:00");
    }

    #[test]
    fn format_timestamp_unix_epoch() {
        // 1970-01-01 00:00:00 — boundary between positive/negative unix time.
        assert_eq!(format_timestamp(FILETIME_UNIX_DIFF), "1970-01-01 00:00:00");
    }

    // ═══════════════════════════════════════════════════════════════════
    // format_number_commas
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn format_number_commas_small() {
        assert_eq!(format_number_commas(0), "0");
        assert_eq!(format_number_commas(999), "999");
    }

    #[test]
    fn format_number_commas_thousands() {
        assert_eq!(format_number_commas(1_000), "1,000");
        assert_eq!(format_number_commas(1_234_567), "1,234,567");
    }

    // ═══════════════════════════════════════════════════════════════════
    // format_bytes
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn format_bytes_under_1kb() {
        assert_eq!(format_bytes(512), " 512 B");
    }

    #[test]
    fn format_bytes_megabytes() {
        let result = format_bytes(10 * 1024 * 1024);
        assert!(result.contains("MB"), "expected MB in '{result}'");
    }

    // ═══════════════════════════════════════════════════════════════════
    // format_bool
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn format_bool_values() {
        assert_eq!(format_bool(true), "●");
        assert_eq!(format_bool(false), "○");
    }
}
