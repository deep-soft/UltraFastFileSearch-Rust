//! Extended post-search filters and NTFS attribute helpers.
//!
//! [`SearchFilters`] is built once from a [`SearchState`] and then applied
//! per-row via [`apply_search_filters`].  All parsing (time bounds, attribute
//! bits) happens at construction time so the hot `retain` loop is branch-only.

use crate::backend::{DisplayRow, FilterMode};
use crate::history::SearchState;

/// Apply filter mode to a set of display rows.
pub fn apply_filter(rows: &mut Vec<DisplayRow>, filter: FilterMode) {
    match filter {
        FilterMode::All => {} // no-op
        FilterMode::FilesOnly => rows.retain(|row| !row.is_directory),
        FilterMode::DirsOnly => rows.retain(|row| row.is_directory),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Extended search filters (applied after pattern matching)
// ═══════════════════════════════════════════════════════════════════════════

/// Extended post-search filters built from a `SearchState`.
///
/// All fields are pre-parsed at construction time so the per-row
/// `retain` loop is branch-only (no parsing).
#[derive(Debug, Default)]
pub struct SearchFilters {
    /// Hide files whose name starts with `$`.
    pub hide_system: bool,
    /// Minimum file size in bytes.
    pub min_size: Option<u64>,
    /// Maximum file size in bytes.
    pub max_size: Option<u64>,
    /// Modified-time lower bound (Unix µs, inclusive).
    pub newer_us: Option<i64>,
    /// Modified-time upper bound (Unix µs, exclusive).
    pub older_us: Option<i64>,
    /// Created-time lower bound (Unix µs, inclusive).
    pub newer_created_us: Option<i64>,
    /// Created-time upper bound (Unix µs, exclusive).
    pub older_created_us: Option<i64>,
    /// Accessed-time lower bound (Unix µs, inclusive).
    pub newer_accessed_us: Option<i64>,
    /// Accessed-time upper bound (Unix µs, exclusive).
    pub older_accessed_us: Option<i64>,
    /// Required attribute bits (all must be set).
    pub attr_require: u32,
    /// Excluded attribute bits (none may be set).
    pub attr_exclude: u32,
    /// Minimum descendant count (inclusive).
    pub min_descendants: Option<u32>,
    /// Maximum descendant count (inclusive).
    pub max_descendants: Option<u32>,
    /// Allowed extensions (lowercase, without dot).  Empty = no filter.
    pub extensions: Vec<String>,
    /// Exclude pattern (glob, lowered).
    pub exclude_lower: Option<String>,
}

impl SearchFilters {
    /// Build filters from a [`SearchState`].
    ///
    /// All time/attribute parsing happens here so the per-row loop is
    /// branch-only.
    #[must_use]
    #[expect(
        clippy::single_call_fn,
        reason = "standalone constructor; keeps filter-building logic isolated from search dispatch"
    )]
    pub fn from_state(state: &SearchState) -> Self {
        let now_us = now_unix_micros();
        Self {
            hide_system: state.hide_system,
            min_size: state.min_size,
            max_size: state.max_size,
            newer_us: state
                .newer
                .as_deref()
                .and_then(|spec| parse_time_bound(spec, now_us, true)),
            older_us: state
                .older
                .as_deref()
                .and_then(|spec| parse_time_bound(spec, now_us, false)),
            newer_created_us: state
                .newer_created
                .as_deref()
                .and_then(|spec| parse_time_bound(spec, now_us, true)),
            older_created_us: state
                .older_created
                .as_deref()
                .and_then(|spec| parse_time_bound(spec, now_us, false)),
            newer_accessed_us: state
                .newer_accessed
                .as_deref()
                .and_then(|spec| parse_time_bound(spec, now_us, true)),
            older_accessed_us: state
                .older_accessed
                .as_deref()
                .and_then(|spec| parse_time_bound(spec, now_us, false)),
            attr_require: parse_attr_require(state.attr.as_deref().unwrap_or("")),
            attr_exclude: parse_attr_exclude(state.attr.as_deref().unwrap_or("")),
            min_descendants: state.min_descendants,
            max_descendants: state.max_descendants,
            extensions: state
                .ext
                .as_deref()
                .map(|ext_list| {
                    ext_list
                        .split(',')
                        .map(|segment| {
                            segment
                                .trim()
                                .to_ascii_lowercase()
                                .trim_start_matches('.')
                                .to_owned()
                        })
                        .filter(|ext| !ext.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            exclude_lower: state.exclude.as_ref().map(|ex| ex.to_ascii_lowercase()),
        }
    }

    /// Returns `true` if all filters are at their default (no-op) values.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.hide_system
            && self.min_size.is_none()
            && self.max_size.is_none()
            && self.newer_us.is_none()
            && self.older_us.is_none()
            && self.newer_created_us.is_none()
            && self.older_created_us.is_none()
            && self.newer_accessed_us.is_none()
            && self.older_accessed_us.is_none()
            && self.attr_require == 0
            && self.attr_exclude == 0
            && self.min_descendants.is_none()
            && self.max_descendants.is_none()
            && self.extensions.is_empty()
            && self.exclude_lower.is_none()
    }
}

/// Apply extended search filters to display rows (in-place).
pub fn apply_search_filters(rows: &mut Vec<DisplayRow>, filters: &SearchFilters) {
    if filters.is_empty() {
        return;
    }
    rows.retain(|row| {
        if filters.hide_system && row.name.starts_with('$') {
            return false;
        }
        if let Some(min) = filters.min_size {
            if row.size < min {
                return false;
            }
        }
        if let Some(max) = filters.max_size {
            if row.size > max {
                return false;
            }
        }
        if let Some(bound) = filters.newer_us {
            if row.modified < bound {
                return false;
            }
        }
        if let Some(bound) = filters.older_us {
            if row.modified >= bound {
                return false;
            }
        }
        if let Some(bound) = filters.newer_created_us {
            if row.created < bound {
                return false;
            }
        }
        if let Some(bound) = filters.older_created_us {
            if row.created >= bound {
                return false;
            }
        }
        if let Some(bound) = filters.newer_accessed_us {
            if row.accessed < bound {
                return false;
            }
        }
        if let Some(bound) = filters.older_accessed_us {
            if row.accessed >= bound {
                return false;
            }
        }
        if filters.attr_require != 0 && (row.flags & filters.attr_require) != filters.attr_require {
            return false;
        }
        if filters.attr_exclude != 0 && (row.flags & filters.attr_exclude) != 0 {
            return false;
        }
        if let Some(min) = filters.min_descendants {
            if row.descendants < min {
                return false;
            }
        }
        if let Some(max) = filters.max_descendants {
            if row.descendants > max {
                return false;
            }
        }
        if !filters.extensions.is_empty() {
            let ext = row
                .name
                .rsplit('.')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if !filters.extensions.iter().any(|allowed| allowed == &ext) {
                return false;
            }
        }
        if let Some(excl) = &filters.exclude_lower {
            let name_lower = row.name.to_ascii_lowercase();
            if crate::compact::name_matches(&name_lower, excl) {
                return false;
            }
        }
        true
    });
}

// ═══════════════════════════════════════════════════════════════════════════
// Time / attribute parsing helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Current time as Unix microseconds.
#[expect(
    clippy::single_call_fn,
    reason = "standalone helper; isolates system-clock access from filter construction"
)]
fn now_unix_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |dur| {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "micros since epoch fits i64 until year ~292,277"
            )]
            let us = dur.as_micros() as i64;
            us
        })
}

/// Parse a time bound string into Unix microseconds.
///
/// Supports:
/// - Duration: `7d`, `24h`, `30m`, `90s`
/// - ISO date: `2026-01-15` (midnight UTC)
///
/// `is_newer`: if `true`, returns `now - duration` (lower bound);
/// if `false`, returns `now - duration` (upper bound, same value).
fn parse_time_bound(spec: &str, now_us: i64, _is_newer: bool) -> Option<i64> {
    let trimmed = spec.trim();

    // Duration format: "7d", "24h", "30m", "90s"
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

    // ISO date: "2026-01-15" → midnight UTC as micros
    // Simple parse: YYYY-MM-DD
    if trimmed.len() == 10 && trimmed.as_bytes().get(4) == Some(&b'-') {
        let parts: Vec<&str> = trimmed.split('-').collect();
        if let [year_s, month_s, day_s] = parts.as_slice() {
            if let (Ok(year), Ok(month), Ok(day)) = (
                year_s.parse::<i64>(),
                month_s.parse::<i64>(),
                day_s.parse::<i64>(),
            ) {
                // Rough epoch calculation (good enough for filtering)
                let days = (year - 1970) * 365 + (year - 1969) / 4 + month_days(month) + day - 1;
                return Some(days * 86400 * 1_000_000);
            }
        }
    }

    None
}

/// Approximate cumulative days before month `m` (1-indexed, non-leap year).
#[expect(
    clippy::single_call_fn,
    reason = "standalone helper; keeps date-arithmetic lookup isolated from parse_time_bound"
)]
fn month_days(month: i64) -> i64 {
    const CUMULATIVE: [i64; 13] = [0, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let idx = usize::try_from(month).unwrap_or(0);
    CUMULATIVE.get(idx).copied().unwrap_or(0)
}

/// NTFS attribute name → bit value.
pub fn attr_bit(name: &str) -> u32 {
    match name {
        "readonly" | "read-only" | "r" => 0x0001,
        "hidden" | "h" => 0x0002,
        "system" | "s" => 0x0004,
        "directory" | "dir" | "d" => 0x0010,
        "archive" | "a" => 0x0020,
        "device" => 0x0040,
        "normal" => 0x0080,
        "temporary" | "temp" | "t" => 0x0100,
        "sparse" => 0x0200,
        "reparse" => 0x0400,
        "compressed" | "c" => 0x0800,
        "offline" | "o" => 0x1000,
        "notindexed" | "notcontent" | "n" => 0x2000,
        "encrypted" | "e" => 0x4000,
        "integrity" | "i" => 0x8000,
        "virtual" | "v" => 0x0001_0000,
        "noscrub" | "no_scrub_data" | "x" => 0x0002_0000,
        "pinned" | "p" => 0x0008_0000,
        "unpinned" | "u" => 0x0010_0000,
        _ => 0,
    }
}

/// Parse required attribute bits from an attr spec like `"hidden,compressed"`.
#[expect(
    clippy::single_call_fn,
    reason = "standalone parser; keeps require-vs-exclude attribute logic separated"
)]
pub fn parse_attr_require(spec: &str) -> u32 {
    let mut bits = 0_u32;
    for raw_part in spec.split(',') {
        let lowered = raw_part.trim().to_ascii_lowercase();
        if !lowered.starts_with('!') {
            bits |= attr_bit(&lowered);
        }
    }
    bits
}

/// Parse excluded attribute bits from an attr spec like `"!system,!hidden"`.
#[expect(
    clippy::single_call_fn,
    reason = "standalone parser; keeps require-vs-exclude attribute logic separated"
)]
pub fn parse_attr_exclude(spec: &str) -> u32 {
    let mut bits = 0_u32;
    for raw_part in spec.split(',') {
        let lowered = raw_part.trim().to_ascii_lowercase();
        if let Some(name) = lowered.strip_prefix('!') {
            bits |= attr_bit(name);
        }
    }
    bits
}
