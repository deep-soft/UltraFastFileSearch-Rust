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

/// Formats a Unix-microsecond timestamp as `YYYY-MM-DD HH:MM:SS`.
///
/// Returns `"—"` for zero/invalid timestamps.
///
/// Uses Howard Hinnant's civil calendar algorithm (same as the CLI's
/// `append_datetime`). No external crate dependency.
#[must_use]
pub fn format_timestamp(unix_micros: i64) -> String {
    if unix_micros == 0 {
        return "—".to_owned();
    }
    let adjusted_secs = unix_micros.div_euclid(1_000_000);

    // Civil time decomposition (no leap seconds — matches chrono behavior).
    // rem_euclid is always non-negative → safe to narrow to u32 via try_from.
    let day_secs = u32::try_from(adjusted_secs.rem_euclid(86_400)).unwrap_or(0);
    let days = adjusted_secs.div_euclid(86_400) + 719_468; // shift to 0000-03-01 epoch

    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    // doe is in [0, 146_096] — always fits u32.
    let doe = u32::try_from(days - era * 146_097).unwrap_or(0);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let year_offset = i64::from(yoe) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let month_proxy = (5 * doy + 2) / 153;
    let day = doy - (153 * month_proxy + 2) / 5 + 1;
    let month = if month_proxy < 10 {
        month_proxy + 3
    } else {
        month_proxy - 9
    };
    let year = if month <= 2 {
        year_offset + 1
    } else {
        year_offset
    };

    let hour = day_secs / 3600;
    let minute = (day_secs % 3600) / 60;
    let second = day_secs % 60;

    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
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
