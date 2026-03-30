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
            extensions: ext_filter
                .map(|ext_list| {
                    ext_list
                        .split(',')
                        .map(|seg| {
                            seg.trim()
                                .to_ascii_lowercase()
                                .trim_start_matches('.')
                                .to_owned()
                        })
                        .filter(|ext| !ext.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            exclude_lower: exclude.map(str::to_ascii_lowercase),
        }
    }

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
/// Supports duration (`7d`, `24h`, `30m`, `90s`) and ISO date (`2026-01-15`).
#[must_use]
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
                // Approximate cumulative days before month (1-indexed, non-leap year)
                let cumulative_month_days = {
                    const CUMULATIVE: [i64; 13] =
                        [0, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
                    let idx = usize::try_from(month).unwrap_or(0);
                    CUMULATIVE.get(idx).copied().unwrap_or(0)
                };
                let days =
                    (year - 1970) * 365 + (year - 1969) / 4 + cumulative_month_days + day - 1;
                return Some(days * 86400 * 1_000_000);
            }
        }
    }

    None
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
// wired into the compact path (OwnedQueryFilters passed None for them).
// See `docs/architecture/2026_03_30_04_12_SEARCH_PIPELINE_REGRESSION_ANALYSIS.
// md` ════════════════════════════════════════════════════════════════════════
#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact::CompactRecord;

    /// Helper: a basic `CompactRecord` with known values.
    fn test_record(name: &str, names: &mut Vec<u8>) -> CompactRecord {
        let offset = u32::try_from(names.len()).expect("offset overflow");
        names.extend_from_slice(name.as_bytes());
        CompactRecord {
            size: 1000,
            allocated: 1024,
            created: 100_000_000,
            modified: 200_000_000,
            accessed: 300_000_000,
            flags: 0x20, // ARCHIVE
            parent_idx: u32::MAX,
            name_offset: offset,
            name_len: u16::try_from(name.len()).expect("name too long"),
            extension_id: 0,
            descendants: 5,
            treesize: 5000,
            tree_allocated: 5120,
            _pad: [0; 4],
        }
    }

    // ── Size filters ──────────────────────────────────────────────────

    #[test]
    fn filter_min_size_rejects_small_files() {
        let mut names = Vec::new();
        let rec = test_record("tiny.txt", &mut names);
        let filters = SearchFilters {
            min_size: Some(2000),
            ..Default::default()
        };
        assert!(
            !filters.matches_record(&rec, &names),
            "file with size=1000 should be rejected by min_size=2000"
        );
    }

    #[test]
    fn filter_max_size_rejects_large_files() {
        let mut names = Vec::new();
        let rec = test_record("big.txt", &mut names);
        let filters = SearchFilters {
            max_size: Some(500),
            ..Default::default()
        };
        assert!(
            !filters.matches_record(&rec, &names),
            "file with size=1000 should be rejected by max_size=500"
        );
    }

    // ── Date filters ──────────────────────────────────────────────────
    // These are the filters that were NOT wired in the v0.4.30 refactor.

    #[test]
    fn filter_newer_modified_rejects_old_files() {
        let mut names = Vec::new();
        let rec = test_record("old.txt", &mut names);
        let filters = SearchFilters {
            newer_us: Some(999_999_999), // modified must be >= this
            ..Default::default()
        };
        assert!(
            !filters.matches_record(&rec, &names),
            "file with modified=200M should be rejected by newer_us=999M"
        );
    }

    #[test]
    fn filter_older_modified_rejects_new_files() {
        let mut names = Vec::new();
        let rec = test_record("new.txt", &mut names);
        let filters = SearchFilters {
            older_us: Some(100_000_000), // modified must be < this
            ..Default::default()
        };
        assert!(
            !filters.matches_record(&rec, &names),
            "file with modified=200M should be rejected by older_us=100M"
        );
    }

    #[test]
    fn filter_newer_created_rejects_old_files() {
        let mut names = Vec::new();
        let rec = test_record("old.txt", &mut names);
        let filters = SearchFilters {
            newer_created_us: Some(999_999_999),
            ..Default::default()
        };
        assert!(
            !filters.matches_record(&rec, &names),
            "file with created=100M should be rejected by newer_created_us=999M"
        );
    }

    #[test]
    fn filter_newer_accessed_rejects_old_files() {
        let mut names = Vec::new();
        let rec = test_record("old.txt", &mut names);
        let filters = SearchFilters {
            newer_accessed_us: Some(999_999_999),
            ..Default::default()
        };
        assert!(
            !filters.matches_record(&rec, &names),
            "file with accessed=300M should be rejected by newer_accessed_us=999M"
        );
    }

    // ── Attribute filters ─────────────────────────────────────────────

    #[test]
    fn filter_attr_require_rejects_missing_bits() {
        let mut names = Vec::new();
        let rec = test_record("file.txt", &mut names);
        // Require HIDDEN (0x02) — but record has ARCHIVE (0x20)
        let filters = SearchFilters {
            attr_require: 0x02,
            ..Default::default()
        };
        assert!(
            !filters.matches_record(&rec, &names),
            "ARCHIVE file should be rejected when HIDDEN is required"
        );
    }

    #[test]
    fn filter_attr_exclude_rejects_matching_bits() {
        let mut names = Vec::new();
        let rec = test_record("file.txt", &mut names);
        // Exclude ARCHIVE (0x20) — record has 0x20
        let filters = SearchFilters {
            attr_exclude: 0x20,
            ..Default::default()
        };
        assert!(
            !filters.matches_record(&rec, &names),
            "ARCHIVE file should be rejected when ARCHIVE is excluded"
        );
    }

    // ── Extension filter ──────────────────────────────────────────────

    #[test]
    fn filter_extension_rejects_wrong_extension() {
        let mut names = Vec::new();
        let rec = test_record("photo.jpg", &mut names);
        let filters = SearchFilters {
            extensions: vec!["txt".to_owned(), "pdf".to_owned()],
            ..Default::default()
        };
        assert!(
            !filters.matches_record(&rec, &names),
            ".jpg should be rejected when only .txt/.pdf are allowed"
        );
    }

    #[test]
    fn filter_extension_accepts_matching_extension() {
        let mut names = Vec::new();
        let rec = test_record("readme.txt", &mut names);
        let filters = SearchFilters {
            extensions: vec!["txt".to_owned()],
            ..Default::default()
        };
        assert!(
            filters.matches_record(&rec, &names),
            ".txt should be accepted when .txt is allowed"
        );
    }

    // ── Exclude pattern ───────────────────────────────────────────────

    #[test]
    fn filter_exclude_rejects_matching_name() {
        let mut names = Vec::new();
        let rec = test_record("thumbs.db", &mut names);
        let filters = SearchFilters {
            exclude_lower: Some("thumbs*".to_owned()),
            ..Default::default()
        };
        assert!(
            !filters.matches_record(&rec, &names),
            "thumbs.db should be rejected by exclude=thumbs*"
        );
    }

    // ── Descendants filter ────────────────────────────────────────────

    #[test]
    fn filter_min_descendants_rejects_low_count() {
        let mut names = Vec::new();
        let rec = test_record("small_dir", &mut names);
        let filters = SearchFilters {
            min_descendants: Some(10),
            ..Default::default()
        };
        assert!(
            !filters.matches_record(&rec, &names),
            "dir with 5 descendants should be rejected by min_descendants=10"
        );
    }

    #[test]
    fn filter_max_descendants_rejects_high_count() {
        let mut names = Vec::new();
        let rec = test_record("big_dir", &mut names);
        let filters = SearchFilters {
            max_descendants: Some(3),
            ..Default::default()
        };
        assert!(
            !filters.matches_record(&rec, &names),
            "dir with 5 descendants should be rejected by max_descendants=3"
        );
    }

    // ── Hide system ───────────────────────────────────────────────────

    #[test]
    fn filter_hide_system_rejects_dollar_prefix() {
        let mut names = Vec::new();
        let rec = test_record("$MFT", &mut names);
        let filters = SearchFilters {
            hide_system: true,
            ..Default::default()
        };
        assert!(
            !filters.matches_record(&rec, &names),
            "$MFT should be rejected by hide_system=true"
        );
    }

    // ── Combined filters ──────────────────────────────────────────────
    // Regression: multiple filters must ALL pass (AND semantics).

    #[test]
    fn filter_combined_all_must_pass() {
        let mut names = Vec::new();
        let rec = test_record("report.txt", &mut names);
        // Size OK (1000 > 500), but modified too old (200M < 999M newer_us)
        let filters = SearchFilters {
            min_size: Some(500),
            newer_us: Some(999_999_999),
            ..Default::default()
        };
        assert!(
            !filters.matches_record(&rec, &names),
            "combined: size passes but date fails → must reject"
        );
    }

    #[test]
    fn filter_all_pass_accepts() {
        let mut names = Vec::new();
        let rec = test_record("report.txt", &mut names);
        let filters = SearchFilters {
            min_size: Some(500),
            max_size: Some(2000),
            newer_us: Some(100_000_000),
            extensions: vec!["txt".to_owned()],
            ..Default::default()
        };
        assert!(
            filters.matches_record(&rec, &names),
            "all filters pass → must accept"
        );
    }

    // ── apply_search_filters on DisplayRow ─────────────────────────
    // Regression: DisplayRow filtering must mirror CompactRecord filtering.

    #[test]
    fn apply_search_filters_matches_compact_behavior() {
        let mut rows = vec![
            DisplayRow {
                drive: 'C',
                path: "C:\\file.txt".to_owned(),
                name: "file.txt".to_owned(),
                size: 1000,
                is_directory: false,
                modified: 200_000_000,
                created: 100_000_000,
                accessed: 300_000_000,
                flags: 0x20,
                allocated: 1024,
                descendants: 0,
                treesize: 0,
                tree_allocated: 0,
            },
            DisplayRow {
                drive: 'C',
                path: "C:\\$MFT".to_owned(),
                name: "$MFT".to_owned(),
                size: 500_000,
                is_directory: false,
                modified: 200_000_000,
                created: 100_000_000,
                accessed: 300_000_000,
                flags: 0x06,
                allocated: 512_000,
                descendants: 0,
                treesize: 0,
                tree_allocated: 0,
            },
        ];

        let filters = SearchFilters {
            hide_system: true,
            ..Default::default()
        };
        apply_search_filters(&mut rows, &filters);
        assert_eq!(rows.len(), 1, "hide_system should remove $MFT");
        let first = rows.first().expect("rows should not be empty");
        assert_eq!(first.name, "file.txt");
    }
}
