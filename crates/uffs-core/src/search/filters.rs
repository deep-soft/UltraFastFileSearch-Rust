//! Extended post-search filters and NTFS attribute helpers.
//!
//! [`SearchFilters`] holds pre-parsed filter criteria. All parsing (time
//! bounds, attribute bits) happens at construction time so the hot `retain`
//! loop is branch-only.

use super::backend::{DisplayRow, FilterMode};
use crate::compact::CompactRecord;
use crate::search::tree::name_matches;

/// Apply filter mode to a set of display rows.
pub fn apply_filter(rows: &mut Vec<DisplayRow>, filter: FilterMode) {
    match filter {
        FilterMode::All => {}
        FilterMode::FilesOnly => rows.retain(|row| !row.is_directory),
        FilterMode::DirsOnly => rows.retain(|row| row.is_directory),
    }
}

/// Extended post-search filters.
///
/// All fields are pre-parsed so the per-row `retain` loop is branch-only
/// (no parsing).
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
    /// Allowed extensions (lowercase, without dot). Empty = no filter.
    pub extensions: Vec<String>,
    /// Exclude pattern (glob, lowered).
    pub exclude_lower: Option<String>,
}

impl SearchFilters {
    /// Check whether a compact record passes all filters.
    ///
    /// Hot-path predicate used during global top-N scans.
    #[must_use]
    pub fn matches_record(&self, rec: &CompactRecord, names: &[u8]) -> bool {
        if self.hide_system {
            let name = rec.name(names);
            if name.starts_with('$') {
                return false;
            }
        }
        if let Some(min) = self.min_size {
            if rec.size < min {
                return false;
            }
        }
        if let Some(max) = self.max_size {
            if rec.size > max {
                return false;
            }
        }
        if let Some(bound) = self.newer_us {
            if rec.modified < bound {
                return false;
            }
        }
        if let Some(bound) = self.older_us {
            if rec.modified >= bound {
                return false;
            }
        }
        if let Some(bound) = self.newer_created_us {
            if rec.created < bound {
                return false;
            }
        }
        if let Some(bound) = self.older_created_us {
            if rec.created >= bound {
                return false;
            }
        }
        if let Some(bound) = self.newer_accessed_us {
            if rec.accessed < bound {
                return false;
            }
        }
        if let Some(bound) = self.older_accessed_us {
            if rec.accessed >= bound {
                return false;
            }
        }
        if self.attr_require != 0 && (rec.flags & self.attr_require) != self.attr_require {
            return false;
        }
        if self.attr_exclude != 0 && (rec.flags & self.attr_exclude) != 0 {
            return false;
        }
        if let Some(min) = self.min_descendants {
            if rec.descendants < min {
                return false;
            }
        }
        if let Some(max) = self.max_descendants {
            if rec.descendants > max {
                return false;
            }
        }
        if !self.extensions.is_empty() {
            let name = rec.name(names);
            let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
            if !self.extensions.iter().any(|allowed| allowed == &ext) {
                return false;
            }
        }
        if let Some(excl) = &self.exclude_lower {
            let name = rec.name(names);
            let name_lower = name.to_ascii_lowercase();
            if name_matches(&name_lower, excl) {
                return false;
            }
        }
        true
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
            if name_matches(&name_lower, excl) {
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
pub fn now_unix_micros() -> i64 {
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
/// Supports duration (`7d`, `24h`, `30m`, `90s`) and ISO date (`2026-01-15`).
pub fn parse_time_bound(spec: &str, now_us: i64, _is_newer: bool) -> Option<i64> {
    let trimmed = spec.trim();

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

    if trimmed.len() == 10 && trimmed.as_bytes().get(4) == Some(&b'-') {
        let parts: Vec<&str> = trimmed.split('-').collect();
        if let [year_s, month_s, day_s] = parts.as_slice() {
            if let (Ok(year), Ok(month), Ok(day)) = (
                year_s.parse::<i64>(),
                month_s.parse::<i64>(),
                day_s.parse::<i64>(),
            ) {
                let days = (year - 1970) * 365 + (year - 1969) / 4 + month_days(month) + day - 1;
                return Some(days * 86400 * 1_000_000);
            }
        }
    }

    None
}

/// Approximate cumulative days before month `m` (1-indexed, non-leap year).
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
