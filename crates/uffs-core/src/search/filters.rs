//! Extended post-search filters and NTFS attribute helpers.
//!
//! [`SearchFilters`] holds pre-parsed filter criteria. All parsing (time
//! bounds, attribute bits) happens at construction time so the hot `retain`
//! loop is branch-only.

use super::backend::{DisplayRow, FilterMode};
use crate::compact::CompactRecord;
use crate::search::tree::name_matches;

/// Lowercase a string into a reusable UTF-8 buffer and return the borrowed
/// string view.
fn lowercase_into<'a>(input: &str, buf: &'a mut Vec<u8>) -> &'a str {
    buf.clear();
    for ch in input.chars() {
        for lower in ch.to_lowercase() {
            let mut char_buf = [0_u8; 4];
            let encoded = lower.encode_utf8(&mut char_buf);
            buf.extend_from_slice(encoded.as_bytes());
        }
    }
    core::str::from_utf8(buf.as_slice()).map_or("", |lowered| lowered)
}

/// Return `true` if a normalized extension matches an allowed filter token.
///
/// The fast/common path compares already-lowercased strings directly. The
/// fallback branch keeps manual test fixtures and any direct struct
/// construction robust if a caller supplied mixed-case extension tokens.
#[must_use]
fn extension_matches_filter(allowed: &str, normalized_extension: &str) -> bool {
    allowed == normalized_extension || allowed.to_lowercase() == normalized_extension
}

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
    /// Pre-resolved extension IDs for the current drive.
    /// Set via [`resolve_ext_ids_for_drive`](Self::resolve_ext_ids_for_drive)
    /// before the hot loop — enables O(1) `u16` comparison per record
    /// instead of per-record string parsing.
    pub resolved_ext_ids: Vec<u16>,
    /// Exclude pattern (glob, lowered).
    pub exclude_lower: Option<String>,
}

impl SearchFilters {
    /// Build `SearchFilters` from individual CLI-style parameter strings.
    ///
    /// This is the generic constructor shared by CLI, TUI, daemon, etc.
    /// All time-spec parsing and attribute parsing happens here so the
    /// hot-path `matches_record` loop is branch-only.
    #[must_use]
    #[expect(clippy::too_many_arguments, reason = "mirrors CLI parameter surface")]
    pub fn from_params(
        hide_system: bool,
        min_size: Option<u64>,
        max_size: Option<u64>,
        min_descendants: Option<u32>,
        max_descendants: Option<u32>,
        newer: Option<&str>,
        older: Option<&str>,
        newer_created: Option<&str>,
        older_created: Option<&str>,
        newer_accessed: Option<&str>,
        older_accessed: Option<&str>,
        attr_filter: Option<&str>,
        ext_filter: Option<&str>,
        exclude: Option<&str>,
    ) -> Self {
        let now_us = now_unix_micros();
        let extensions: Vec<String> = ext_filter
            .map(|ext_list| {
                ext_list
                    .split(',')
                    .map(|segment| segment.trim().trim_start_matches('.').to_lowercase())
                    .filter(|ext| !ext.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if !extensions.is_empty() {
            tracing::trace!(
                raw_ext_filter = ext_filter.unwrap_or_default(),
                normalized_extensions = ?extensions,
                "normalized extension filter strings"
            );
        }
        let exclude_lower = exclude.map(|excl| {
            let fold = uffs_text::CaseFold::default_table();
            let mut buf = Vec::with_capacity(excl.len());
            fold.fold_into(excl, &mut buf).to_owned()
        });
        Self {
            hide_system,
            min_size,
            max_size,
            newer_us: newer.and_then(|spec| parse_time_bound(spec, now_us, true)),
            older_us: older.and_then(|spec| parse_time_bound(spec, now_us, false)),
            newer_created_us: newer_created.and_then(|spec| parse_time_bound(spec, now_us, true)),
            older_created_us: older_created.and_then(|spec| parse_time_bound(spec, now_us, false)),
            newer_accessed_us: newer_accessed.and_then(|spec| parse_time_bound(spec, now_us, true)),
            older_accessed_us: older_accessed
                .and_then(|spec| parse_time_bound(spec, now_us, false)),
            attr_require: parse_attr_require(attr_filter.unwrap_or("")),
            attr_exclude: parse_attr_exclude(attr_filter.unwrap_or("")),
            min_descendants,
            max_descendants,
            extensions,
            resolved_ext_ids: Vec::new(),
            exclude_lower,
        }
    }

    /// Pre-resolve extension filter strings to `u16` IDs for a specific
    /// drive.  Call this **once per drive** before the hot record loop.
    pub fn resolve_ext_ids_for_drive(&mut self, drive: &crate::compact::DriveCompactIndex) {
        if self.extensions.is_empty() {
            self.resolved_ext_ids.clear();
            tracing::trace!(drive = %drive.letter, "no extension filter active for drive");
            return;
        }

        self.resolved_ext_ids = drive.resolve_ext_ids(&self.extensions);

        let requested_lower = self
            .extensions
            .iter()
            .map(|ext| ext.to_lowercase())
            .collect::<Vec<_>>();
        let lowercase_only_hits = requested_lower
            .iter()
            .filter(|ext| {
                drive
                    .ext_names
                    .iter()
                    .any(|name| name.as_ref() == ext.as_str())
            })
            .cloned()
            .collect::<Vec<_>>();
        let sample_ext_names = drive
            .ext_names
            .iter()
            .filter(|name| !name.is_empty())
            .take(8)
            .map(AsRef::as_ref)
            .collect::<Vec<_>>();

        tracing::debug!(
            drive = %drive.letter,
            requested_extensions = ?self.extensions,
            requested_lowercase = ?requested_lower,
            resolved_ext_ids = ?self.resolved_ext_ids,
            lowercase_only_hits = ?lowercase_only_hits,
            ext_name_count = drive.ext_names.len(),
            ext_name_sample = ?sample_ext_names,
            "extension filter resolution for drive"
        );
    }

    /// Returns `true` when the only active filter is `extensions` — no
    /// size, date, attr, exclude, descendant, or system-hide constraints.
    /// When this is true and the pattern is match-all (`*`), we can use
    /// the extension inverted index for O(K) iteration instead of O(N).
    #[must_use]
    pub const fn is_ext_only(&self) -> bool {
        !self.extensions.is_empty()
            && !self.hide_system
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
            && self.exclude_lower.is_none()
    }

    /// Check whether a compact record passes all filters.
    ///
    /// Hot-path predicate used during global top-N scans.
    ///
    /// `fold_buf` is a caller-owned reusable buffer for on-the-fly
    /// `CaseFold` folding (avoids per-record heap allocation for exclude
    /// matching).
    #[must_use]
    pub fn matches_record(
        &self,
        rec: &CompactRecord,
        names: &[u8],
        fold_buf: &mut Vec<u8>,
        fold: uffs_text::CaseFold,
    ) -> bool {
        if self.hide_system {
            let name = rec.name(names);
            if name.starts_with('$') {
                return false;
            }
        }
        if let Some(min) = self.min_size
            && rec.size < min
        {
            return false;
        }
        if let Some(max) = self.max_size
            && rec.size > max
        {
            return false;
        }
        if let Some(bound) = self.newer_us
            && rec.modified < bound
        {
            return false;
        }
        if let Some(bound) = self.older_us
            && rec.modified >= bound
        {
            return false;
        }
        if let Some(bound) = self.newer_created_us
            && rec.created < bound
        {
            return false;
        }
        if let Some(bound) = self.older_created_us
            && rec.created >= bound
        {
            return false;
        }
        if let Some(bound) = self.newer_accessed_us
            && rec.accessed < bound
        {
            return false;
        }
        if let Some(bound) = self.older_accessed_us
            && rec.accessed >= bound
        {
            return false;
        }
        if self.attr_require != 0 && (rec.flags & self.attr_require) != self.attr_require {
            return false;
        }
        if self.attr_exclude != 0 && (rec.flags & self.attr_exclude) != 0 {
            return false;
        }
        if let Some(min) = self.min_descendants
            && rec.descendants < min
        {
            return false;
        }
        if let Some(max) = self.max_descendants
            && rec.descendants > max
        {
            return false;
        }
        if !self.resolved_ext_ids.is_empty() {
            // Fast path: compare pre-resolved u16 IDs (O(1) per record).
            if !self.resolved_ext_ids.contains(&rec.extension_id) {
                return false;
            }
        } else if !self.extensions.is_empty() {
            // Fallback for callers that did not call resolve_ext_ids_for_drive.
            let name = rec.name(names);
            let ext = name.rsplit('.').next().unwrap_or("");
            let normalized_ext = lowercase_into(ext, fold_buf);
            if !self
                .extensions
                .iter()
                .any(|allowed| extension_matches_filter(allowed, normalized_ext))
            {
                return false;
            }
        }
        if let Some(excl) = &self.exclude_lower {
            // Zero-alloc via CaseFold: fold the name into a reusable buffer.
            let name = rec.name(names);
            let folded_name = fold.fold_into(name, fold_buf);
            if name_matches(folded_name, excl) {
                return false;
            }
        }
        true
    }

    /// Returns `true` if all filters are at their default (no-op) values.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
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
    let fold = uffs_text::CaseFold::default_table();
    let mut fold_buf: Vec<u8> = Vec::with_capacity(256);
    rows.retain(|row| {
        if filters.hide_system && row.name().starts_with('$') {
            return false;
        }
        if let Some(min) = filters.min_size
            && row.size < min
        {
            return false;
        }
        if let Some(max) = filters.max_size
            && row.size > max
        {
            return false;
        }
        if let Some(bound) = filters.newer_us
            && row.modified < bound
        {
            return false;
        }
        if let Some(bound) = filters.older_us
            && row.modified >= bound
        {
            return false;
        }
        if let Some(bound) = filters.newer_created_us
            && row.created < bound
        {
            return false;
        }
        if let Some(bound) = filters.older_created_us
            && row.created >= bound
        {
            return false;
        }
        if let Some(bound) = filters.newer_accessed_us
            && row.accessed < bound
        {
            return false;
        }
        if let Some(bound) = filters.older_accessed_us
            && row.accessed >= bound
        {
            return false;
        }
        if filters.attr_require != 0 && (row.flags & filters.attr_require) != filters.attr_require {
            return false;
        }
        if filters.attr_exclude != 0 && (row.flags & filters.attr_exclude) != 0 {
            return false;
        }
        if let Some(min) = filters.min_descendants
            && row.descendants < min
        {
            return false;
        }
        if let Some(max) = filters.max_descendants
            && row.descendants > max
        {
            return false;
        }
        if !filters.extensions.is_empty() {
            let ext = row.name().rsplit('.').next().unwrap_or("");
            let normalized_ext = lowercase_into(ext, &mut fold_buf);
            if !filters
                .extensions
                .iter()
                .any(|allowed| extension_matches_filter(allowed, normalized_ext))
            {
                return false;
            }
        }
        if let Some(excl) = &filters.exclude_lower {
            let folded_name = fold.fold_into(row.name(), &mut fold_buf);
            if name_matches(folded_name, excl) {
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
#[must_use]
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
#[allow(clippy::single_call_fn)]
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
#[allow(clippy::single_call_fn, clippy::too_many_lines)]
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
#[allow(clippy::single_call_fn)]
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

/// NTFS attribute name → bit value.
#[must_use]
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
#[must_use]
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
#[must_use]
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

// ════════════════════════════════════════════════════════════════════════
// REGRESSION TESTS — Search Filters Parity Guards
//
// These tests verify that SearchFilters.matches_record covers ALL filter
// types.  During the v0.4.30 refactor, 14 filter parameters were not
// wired into the compact search path (they were all passed as None).
// See `docs/architecture/2026_03_30_04_12_SEARCH_PIPELINE_REGRESSION_ANALYSIS.
// md` ════════════════════════════════════════════════════════════════════════
#[cfg(test)]
#[path = "filters_tests.rs"]
mod tests;
