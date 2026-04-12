//! Time-bound parsing: durations, ISO dates, named ranges, and months.

/// Extracts the 1-based month number from a Unix-microsecond timestamp.
#[must_use]
pub fn month_from_unix_micros(us: i64) -> u32 {
    // Convert µs → days since Unix epoch.
    let total_secs = us / 1_000_000;
    // Integer floor-division that rounds towards −∞.
    let day = if total_secs >= 0 {
        total_secs / 86400
    } else {
        (total_secs - 86399) / 86400
    };
    // Civil date from day count (algorithm from Howard Hinnant).
    // <https://howardhinnant.github.io/date_algorithms.html#civil_from_days>
    // All intermediates stay `i64`; only the final month [1,12] is narrowed.
    let z = day + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // day-of-era  [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day-of-year [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    // month is always in [1, 12] — try_from is infallible here.
    u32::try_from(month).unwrap_or(0)
}

/// Parse a month/quarter spec into a vector of allowed months (1-12).
///
/// Accepts:
/// - Month names: `january`, `jan`, `february`, `feb`, … , `december`, `dec`
/// - Quarter names: `Q1`, `Q2`, `Q3`, `Q4`
/// - Comma-separated combinations: `jan,feb`, `Q1,Q3`
///
/// ```
/// # use uffs_core::search::filters::parse_month_spec;
/// assert_eq!(parse_month_spec("january"), vec![1]);
/// assert_eq!(parse_month_spec("Q1"), vec![1, 2, 3]);
/// assert_eq!(parse_month_spec("jan,feb"), vec![1, 2]);
/// assert_eq!(parse_month_spec("Q2,october"), vec![4, 5, 6, 10]);
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

// ═══════════════════════════════════════════════════════════════════════════
// Size parsing helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Parse a human-readable size string into bytes.
///
/// Accepts plain integers (bytes) and suffixes: `B`, `KB`, `MB`, `GB`, `TB`.
/// The suffix is **case-insensitive**.  A bare number with no suffix is
/// treated as bytes.
///
/// # Errors
///
/// Returns `Err` if the spec is empty, contains non-numeric characters
/// (after stripping the suffix), or the result overflows `u64`.
///
/// # Examples
///
/// ```
/// # use uffs_core::search::filters::parse_size;
/// assert_eq!(parse_size("1024"), Ok(1024));
/// assert_eq!(parse_size("1KB"), Ok(1024));
/// assert_eq!(parse_size("10mb"), Ok(10 * 1024 * 1024));
/// assert_eq!(parse_size("1GB"), Ok(1024 * 1024 * 1024));
/// assert_eq!(parse_size("2TB"), Ok(2 * 1024 * 1024 * 1024 * 1024));
/// assert_eq!(parse_size("0"), Ok(0));
/// assert!(parse_size("abc").is_err());
/// ```
pub fn parse_size(spec: &str) -> Result<u64, String> {
    // Suffix table: longest-first to avoid prefix ambiguity.
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

    count
        .checked_mul(multiplier)
        .ok_or_else(|| format!("size overflows u64: {spec}"))
}

// ═══════════════════════════════════════════════════════════════════════════
// Time / attribute parsing helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Current time as Unix microseconds.
#[must_use]
pub fn now_unix_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |dur| uffs_mft::micros_to_i64(dur.as_micros()))
}

/// Parse a time bound string into Unix microseconds.
///
/// Supports:
/// - **Duration:** `7d`, `24h`, `30m`, `90s`, `2w`
/// - **ISO date:** `2026-01-15`
/// - **Named ranges:** `today`, `yesterday`, `this_week`, `last_week`,
///   `this_month`, `last_month`, `this_year`, `last_year`, `last_7d`,
///   `last_30d`, `last_90d`, `last_365d`, `ytd`
#[must_use]
pub fn parse_time_bound(spec: &str, now_us: i64, is_newer: bool) -> Option<i64> {
    let trimmed = spec.trim();

    // ── Named time ranges ──────────────────────────────────────────
    if let Some(ts) = parse_named_time_range(trimmed, now_us, is_newer) {
        return Some(ts);
    }

    // ── Duration suffix (e.g. "7d", "24h") ─────────────────────────
    if trimmed.len() >= 2 {
        let (num_str, suffix) = trimmed.split_at(trimmed.len() - 1);
        if let Ok(count) = num_str.parse::<i64>() {
            let micros_per_sec: i64 = 1_000_000;
            let delta = match suffix {
                "s" => count * micros_per_sec,
                "m" => count * 60 * micros_per_sec,
                "h" => count * 3600 * micros_per_sec,
                "d" => count * 86400 * micros_per_sec,
                "w" => count * 7 * 86400 * micros_per_sec,
                _ => return None,
            };
            return Some(now_us - delta);
        }
    }

    // ── ISO date (YYYY-MM-DD) ──────────────────────────────────────
    parse_iso_date(trimmed)
}

/// Parse an ISO date string (`YYYY-MM-DD`) into Unix microseconds at midnight.
///
/// Extracted for readability — `parse_time_bound` dispatches to named ranges,
/// duration suffixes, and this ISO parser.
fn parse_iso_date(trimmed: &str) -> Option<i64> {
    if trimmed.len() == 10 && trimmed.as_bytes().get(4) == Some(&b'-') {
        let parts: Vec<&str> = trimmed.split('-').collect();
        if let [year_s, month_s, day_s] = parts.as_slice()
            && let (Ok(year), Ok(month), Ok(day)) = (
                year_s.parse::<i64>(),
                month_s.parse::<i64>(),
                day_s.parse::<i64>(),
            )
        {
            let days = ymd_to_days(year, month, day);
            return Some(days * US_PER_DAY);
        }
    }
    None
}

/// Microseconds per day.
const US_PER_DAY: i64 = 86_400 * 1_000_000;

/// Resolve a named time range to Unix microseconds.
///
/// For `is_newer = true`, returns the start of the range (lower bound).
/// For `is_newer = false`, returns the end of the range (upper bound).
///
/// Extracted for readability — the 15 named range cases would make
/// `parse_time_bound` exceed the `too_many_lines` threshold.
fn parse_named_time_range(name: &str, now_us: i64, is_newer: bool) -> Option<i64> {
    let today_start = now_us - (now_us % US_PER_DAY);

    match name.to_ascii_lowercase().as_str() {
        "today" => Some(today_start),
        "yesterday" => {
            if is_newer {
                Some(today_start - US_PER_DAY)
            } else {
                Some(today_start)
            }
        }
        "this_week" | "thisweek" => {
            // Go back to most recent Monday (Unix epoch was Thursday).
            let days_since_epoch = today_start / US_PER_DAY;
            let dow = (days_since_epoch + 3) % 7; // 0=Mon, 6=Sun
            Some(today_start - dow * US_PER_DAY)
        }
        "last_week" | "lastweek" => {
            let days_since_epoch = today_start / US_PER_DAY;
            let dow = (days_since_epoch + 3) % 7;
            let this_monday = today_start - dow * US_PER_DAY;
            if is_newer {
                Some(this_monday - 7 * US_PER_DAY)
            } else {
                Some(this_monday)
            }
        }
        "this_month" | "thismonth" => {
            let days_since_epoch = today_start / US_PER_DAY;
            let (_, _, day) = days_to_ymd(days_since_epoch);
            Some(today_start - (day - 1) * US_PER_DAY)
        }
        "last_month" | "lastmonth" => {
            let days_since_epoch = today_start / US_PER_DAY;
            let (year, month, day) = days_to_ymd(days_since_epoch);
            let this_month_start = today_start - (day - 1) * US_PER_DAY;
            if is_newer {
                let (prev_year, prev_month) = if month == 1 {
                    (year - 1, 12)
                } else {
                    (year, month - 1)
                };
                let prev_days = days_in_month(prev_year, prev_month);
                Some(this_month_start - prev_days * US_PER_DAY)
            } else {
                Some(this_month_start)
            }
        }
        "this_year" | "thisyear" | "ytd" => {
            let days_since_epoch = today_start / US_PER_DAY;
            let (year, _, _) = days_to_ymd(days_since_epoch);
            Some(ymd_to_days(year, 1, 1) * US_PER_DAY)
        }
        "last_year" | "lastyear" => {
            let days_since_epoch = today_start / US_PER_DAY;
            let (year, _, _) = days_to_ymd(days_since_epoch);
            if is_newer {
                Some(ymd_to_days(year - 1, 1, 1) * US_PER_DAY)
            } else {
                Some(ymd_to_days(year, 1, 1) * US_PER_DAY)
            }
        }
        // last_Nd shortcuts
        "last_7d" | "last7d" => Some(now_us - 7 * US_PER_DAY),
        "last_30d" | "last30d" => Some(now_us - 30 * US_PER_DAY),
        "last_90d" | "last90d" => Some(now_us - 90 * US_PER_DAY),
        "last_365d" | "last365d" => Some(now_us - 365 * US_PER_DAY),
        // next_* periods — for finding files with future timestamps
        // (clock skew, timezone issues, scheduled items).
        "next_day" | "nextday" | "tomorrow" => {
            if is_newer {
                Some(today_start + US_PER_DAY)
            } else {
                Some(today_start + 2 * US_PER_DAY)
            }
        }
        "next_week" | "nextweek" => {
            let days_since_epoch = today_start / US_PER_DAY;
            let dow = (days_since_epoch + 3) % 7;
            let this_monday = today_start - dow * US_PER_DAY;
            let next_monday = this_monday + 7 * US_PER_DAY;
            if is_newer {
                Some(next_monday)
            } else {
                Some(next_monday + 7 * US_PER_DAY)
            }
        }
        "next_month" | "nextmonth" => {
            let days_since_epoch = today_start / US_PER_DAY;
            let (year, month, day) = days_to_ymd(days_since_epoch);
            let this_month_start = today_start - (day - 1) * US_PER_DAY;
            let days = days_in_month(year, month);
            let next_month_start = this_month_start + days * US_PER_DAY;
            if is_newer {
                Some(next_month_start)
            } else {
                let (ny, nm) = if month == 12 {
                    (year + 1, 1)
                } else {
                    (year, month + 1)
                };
                Some(next_month_start + days_in_month(ny, nm) * US_PER_DAY)
            }
        }
        "next_year" | "nextyear" => {
            let days_since_epoch = today_start / US_PER_DAY;
            let (year, _, _) = days_to_ymd(days_since_epoch);
            if is_newer {
                Some(ymd_to_days(year + 1, 1, 1) * US_PER_DAY)
            } else {
                Some(ymd_to_days(year + 2, 1, 1) * US_PER_DAY)
            }
        }
        _ => None,
    }
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(total_days: i64) -> (i64, i64, i64) {
    let mut y = 1970 + total_days / 365;
    let mut remaining = total_days - (y - 1970) * 365 - (y - 1969) / 4;
    if remaining < 0 {
        y -= 1;
        remaining = total_days - (y - 1970) * 365 - (y - 1969) / 4;
    }
    let is_leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let month_lengths: [i64; 12] = [
        31,
        if is_leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month_idx = 1_i64;
    for &ml in &month_lengths {
        if remaining < ml {
            break;
        }
        remaining -= ml;
        month_idx += 1;
    }
    (y, month_idx, remaining + 1)
}

/// Days in a given month (1-indexed).
///
/// Only used by `parse_named_time_range` for `last_month` calculation;
/// extracted for clarity.
const fn days_in_month(year: i64, month: i64) -> i64 {
    let is_leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        2 => {
            if is_leap {
                29
            } else {
                28
            }
        }
        // All other months (4,6,9,11 and invalid) default to 30.
        _ => 30,
    }
}

/// Convert (year, month, day) to days since Unix epoch.
fn ymd_to_days(year: i64, month: i64, day: i64) -> i64 {
    const CUMULATIVE: [i64; 13] = [0, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let idx = usize::try_from(month).unwrap_or(0);
    let month_offset = CUMULATIVE.get(idx).copied().unwrap_or(0);
    (year - 1970) * 365 + (year - 1969) / 4 + month_offset + day - 1
}
