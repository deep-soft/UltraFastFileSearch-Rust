//! Tests for `SearchFilters`, `matches_record`, and `apply_search_filters`.
//!
//! Exception: integration test suite for filter pipeline; splitting further
//! would scatter related test fixtures and reduce cohesion.

use uffs_mft::index::{IndexNameRef, MftIndex, ROOT_FRS};

use super::*;
use crate::compact::{CompactRecord, DriveCompactIndex, build_compact_index};

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
        path_len: 0,
        name_first_byte: name.as_bytes().first().copied().unwrap_or(0),
        _pad: [0; 1],
    }
}

/// Helper: build a compact drive with a single `readme.rs` file.
fn test_drive_with_rs_file() -> DriveCompactIndex {
    let mut idx = MftIndex::new('C');

    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS);
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = ROOT_FRS;

    let name = "readme.rs";
    let off = idx.add_name(name);
    let ext = idx.intern_extension(name);
    let rec = idx.get_or_create(100);
    rec.first_name.name = IndexNameRef::new(
        off,
        u16::try_from(name.len()).expect("name too long"),
        true,
        ext,
    );
    rec.first_name.parent_frs = ROOT_FRS;
    rec.stdinfo.flags = 0x20;

    let (drive, _, _) = build_compact_index('C', &idx);
    drive
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
        !filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
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
        !filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
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
        !filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
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
        !filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
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
        !filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
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
        !filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
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
        !filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
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
        !filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
        "ARCHIVE file should be rejected when ARCHIVE is excluded"
    );
}

// ── Extension filter ──────────────────────────────────────────────

#[test]
fn filter_extension_rejects_wrong_extension() {
    let mut names = Vec::new();
    let rec = test_record("photo.jpg", &mut names);
    let filters = SearchFilters {
        extensions: vec!["TXT".to_owned(), "PDF".to_owned()],
        ..Default::default()
    };
    assert!(
        !filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
        ".jpg should be rejected when only .txt/.pdf are allowed"
    );
}

#[test]
fn filter_extension_accepts_matching_extension() {
    let mut names = Vec::new();
    let rec = test_record("readme.txt", &mut names);
    let filters = SearchFilters {
        extensions: vec!["TXT".to_owned()],
        ..Default::default()
    };
    assert!(
        filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
        ".txt should be accepted when .txt is allowed"
    );
}

#[test]
fn from_params_normalizes_extensions_to_lowercase_without_dot() {
    let filters = SearchFilters::from_params(&SearchFilterParams {
        ext_filter: Some(" .RS, JPG ,PnG "),
        ..Default::default()
    });

    assert_eq!(filters.extensions, ["rs", "jpg", "png"]);
}

#[test]
fn resolve_ext_ids_for_drive_accepts_mixed_case_extensions() {
    let drive = test_drive_with_rs_file();
    let mut filters = SearchFilters::from_params(&SearchFilterParams {
        ext_filter: Some("RS"),
        ..Default::default()
    });

    filters.resolve_ext_ids_for_drive(&drive);

    assert_eq!(
        filters.resolved_ext_ids.len(),
        1,
        "must resolve one extension id"
    );
    let resolved_id = filters.resolved_ext_ids.first().copied();
    let resolved_name = resolved_id.and_then(|id| drive.ext_names.get(usize::from(id)));
    assert_eq!(resolved_name.map(AsRef::as_ref), Some("rs"));
}

#[test]
fn resolve_ext_ids_for_drive_is_robust_to_manual_uppercase_filters() {
    let drive = test_drive_with_rs_file();
    let mut filters = SearchFilters {
        extensions: vec!["RS".to_owned()],
        ..Default::default()
    };

    filters.resolve_ext_ids_for_drive(&drive);

    assert_eq!(
        filters.resolved_ext_ids.len(),
        1,
        "uppercase filter must still resolve"
    );
    let resolved_id = filters.resolved_ext_ids.first().copied();
    let resolved_name = resolved_id.and_then(|id| drive.ext_names.get(usize::from(id)));
    assert_eq!(resolved_name.map(AsRef::as_ref), Some("rs"));
}

// ── Exclude pattern ───────────────────────────────────────────────

#[test]
fn filter_exclude_rejects_matching_name() {
    let mut names = Vec::new();
    let rec = test_record("Thumbs.DB", &mut names);
    let mut lower_buf = Vec::new();
    let filters = SearchFilters {
        exclude_lower: Some("THUMBS*".to_owned()),
        ..Default::default()
    };
    assert!(
        !filters.matches_record(
            &rec,
            &names,
            &mut lower_buf,
            uffs_text::CaseFold::default_table()
        ),
        "Thumbs.DB should be rejected by exclude=thumbs* (case-insensitive via lower_buf)"
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
        !filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
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
        !filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
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
        !filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
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
        !filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
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
        extensions: vec!["TXT".to_owned()],
        ..Default::default()
    };
    assert!(
        filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
        "all filters pass → must accept"
    );
}
// ── apply_search_filters on DisplayRow ─────────────────────────
// Regression: DisplayRow filtering must mirror CompactRecord filtering.

#[test]
fn apply_search_filters_matches_compact_behavior() {
    let mut rows = vec![
        DisplayRow::new(
            0,
            'C',
            "C:\\file.txt".to_owned(),
            1000,
            false,
            200_000_000,
            100_000_000,
            300_000_000,
            0x20,
            1024,
            0,
            0,
            0,
        ),
        DisplayRow::new(
            0,
            'C',
            "C:\\$MFT".to_owned(),
            500_000,
            false,
            200_000_000,
            100_000_000,
            300_000_000,
            0x06,
            512_000,
            0,
            0,
            0,
        ),
    ];

    let filters = SearchFilters {
        hide_system: true,
        ..Default::default()
    };
    apply_search_filters(&mut rows, &filters);
    assert_eq!(rows.len(), 1, "hide_system should remove $MFT");
    let first = rows.first().expect("rows should not be empty");
    assert_eq!(first.name(), "file.txt");
}

// ── Older-created / older-accessed filters ───────────────────────
// Regression: only newer_* directions were tested. older_* must also work.

#[test]
fn filter_older_created_rejects_new_files() {
    let mut names = Vec::new();
    let rec = test_record("new.txt", &mut names);
    // created=100M, older_created_us=50M → file is NEWER than cutoff → reject
    let filters = SearchFilters {
        older_created_us: Some(50_000_000),
        ..Default::default()
    };
    assert!(
        !filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
        "file with created=100M should be rejected by older_created_us=50M"
    );
}

#[test]
fn filter_older_created_accepts_old_files() {
    let mut names = Vec::new();
    let rec = test_record("old.txt", &mut names);
    // created=100M, older_created_us=999M → file IS older than cutoff → accept
    let filters = SearchFilters {
        older_created_us: Some(999_000_000),
        ..Default::default()
    };
    assert!(
        filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
        "file with created=100M should be accepted by older_created_us=999M"
    );
}

#[test]
fn filter_older_accessed_rejects_new_files() {
    let mut names = Vec::new();
    let rec = test_record("new.txt", &mut names);
    // accessed=300M, older_accessed_us=100M → file is NEWER than cutoff → reject
    let filters = SearchFilters {
        older_accessed_us: Some(100_000_000),
        ..Default::default()
    };
    assert!(
        !filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
        "file with accessed=300M should be rejected by older_accessed_us=100M"
    );
}

#[test]
fn filter_older_accessed_accepts_old_files() {
    let mut names = Vec::new();
    let rec = test_record("old.txt", &mut names);
    // accessed=300M, older_accessed_us=999M → file IS older than cutoff → accept
    let filters = SearchFilters {
        older_accessed_us: Some(999_000_000),
        ..Default::default()
    };
    assert!(
        filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
        "file with accessed=300M should be accepted by older_accessed_us=999M"
    );
}

#[test]
fn filter_older_modified_accepts_old_files() {
    let mut names = Vec::new();
    let rec = test_record("old.txt", &mut names);
    // modified=200M, older_us=999M → file IS older → accept
    let filters = SearchFilters {
        older_us: Some(999_000_000),
        ..Default::default()
    };
    assert!(
        filters.matches_record(
            &rec,
            &names,
            &mut Vec::new(),
            uffs_text::CaseFold::default_table()
        ),
        "file with modified=200M should be accepted by older_us=999M"
    );
}

// ════════════════════════════════════════════════════════════════════════
// TIME GRAMMAR TESTS — Named Time Ranges
// ════════════════════════════════════════════════════════════════════════

/// Microseconds per day constant for tests.
const US_PER_DAY: i64 = 86_400 * 1_000_000;

#[test]
fn parse_time_bound_duration_7d() {
    let now = 100 * US_PER_DAY;
    let result = parse_time_bound("7d", now, true).unwrap();
    assert_eq!(result, now - 7 * US_PER_DAY);
}

#[test]
fn parse_time_bound_duration_24h() {
    let now = 100 * US_PER_DAY;
    let result = parse_time_bound("24h", now, true).unwrap();
    assert_eq!(result, now - 24 * 3600 * 1_000_000);
}

#[test]
fn parse_time_bound_iso_date() {
    let result = parse_time_bound("1970-01-02", 0, true).unwrap();
    assert_eq!(result, US_PER_DAY); // Jan 2 1970 = 1 day from epoch
}

#[test]
fn parse_time_bound_today() {
    let now = 100 * US_PER_DAY + 42_000_000; // 100 days + offset
    let result = parse_time_bound("today", now, true).unwrap();
    assert_eq!(result, 100 * US_PER_DAY); // midnight of day 100
}

#[test]
fn parse_time_bound_yesterday_newer() {
    let now = 100 * US_PER_DAY + 42_000_000;
    let result = parse_time_bound("yesterday", now, true).unwrap();
    assert_eq!(result, 99 * US_PER_DAY);
}

#[test]
fn parse_time_bound_yesterday_older() {
    let now = 100 * US_PER_DAY + 42_000_000;
    let result = parse_time_bound("yesterday", now, false).unwrap();
    assert_eq!(result, 100 * US_PER_DAY); // today midnight = end of yesterday
}

#[test]
fn parse_time_bound_last_7d() {
    let now = 100 * US_PER_DAY;
    let result = parse_time_bound("last_7d", now, true).unwrap();
    assert_eq!(result, now - 7 * US_PER_DAY);
}

#[test]
fn parse_time_bound_last_30d() {
    let now = 100 * US_PER_DAY;
    let result = parse_time_bound("last_30d", now, true).unwrap();
    assert_eq!(result, now - 30 * US_PER_DAY);
}

#[test]
fn parse_time_bound_this_year() {
    // 2026-04-04 ≈ day 20548 from epoch. Jan 1 2026 ≈ day 20454.
    let now_us = 20548 * US_PER_DAY;
    let result = parse_time_bound("this_year", now_us, true).unwrap();
    // Should be Jan 1 of current year.
    let jan1_days = (2026 - 1970) * 365 + (2026 - 1969) / 4;
    assert_eq!(result, jan1_days * US_PER_DAY);
}

#[test]
fn parse_time_bound_this_month() {
    // Day 100 from epoch = April 11, 1970. Day 1 of April = day 90.
    let now_us = 100 * US_PER_DAY;
    let result = parse_time_bound("this_month", now_us, true).unwrap();
    // Should be start of current month.
    assert!(result <= now_us);
    assert!(result >= now_us - 31 * US_PER_DAY);
}

#[test]
fn parse_time_bound_last_year_newer() {
    let now_us = 20548 * US_PER_DAY;
    let result = parse_time_bound("last_year", now_us, true).unwrap();
    let jan1_2025 = (2025 - 1970) * 365 + (2025 - 1969) / 4;
    assert_eq!(result, jan1_2025 * US_PER_DAY);
}

#[test]
fn parse_time_bound_last_year_older() {
    let now_us = 20548 * US_PER_DAY;
    let result = parse_time_bound("last_year", now_us, false).unwrap();
    let jan1_2026 = (2026 - 1970) * 365 + (2026 - 1969) / 4;
    assert_eq!(result, jan1_2026 * US_PER_DAY);
}

#[test]
fn parse_time_bound_unknown_returns_none() {
    assert!(parse_time_bound("foobar", 100 * US_PER_DAY, true).is_none());
}

#[test]
fn parse_time_bound_this_week() {
    // Thursday epoch + 7 days = another Thursday.
    let now_us = 7 * US_PER_DAY;
    let result = parse_time_bound("this_week", now_us, true).unwrap();
    // Should be Monday = day 4 from epoch (epoch was Thursday).
    assert_eq!(result, 4 * US_PER_DAY);
}

// ═══════════════════════════════════════════════════════════════════════════
// parse_size tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn parse_size_plain_bytes() {
    assert_eq!(parse_size("0").unwrap(), 0);
    assert_eq!(parse_size("1024").unwrap(), 1024);
    assert_eq!(parse_size("999999").unwrap(), 999_999);
}

#[test]
fn parse_size_b_suffix() {
    assert_eq!(parse_size("512B").unwrap(), 512);
    assert_eq!(parse_size("512b").unwrap(), 512);
}

#[test]
fn parse_size_kb() {
    assert_eq!(parse_size("1KB").unwrap(), 1024);
    assert_eq!(parse_size("1kb").unwrap(), 1024);
    assert_eq!(parse_size("100KB").unwrap(), 100 * 1024);
}

#[test]
fn parse_size_mb() {
    assert_eq!(parse_size("1MB").unwrap(), 1024 * 1024);
    assert_eq!(parse_size("10mb").unwrap(), 10 * 1024 * 1024);
    assert_eq!(parse_size("100Mb").unwrap(), 100 * 1024 * 1024);
}

#[test]
fn parse_size_gb() {
    assert_eq!(parse_size("1GB").unwrap(), 1024 * 1024 * 1024);
    assert_eq!(parse_size("2gb").unwrap(), 2 * 1024 * 1024 * 1024);
}

#[test]
fn parse_size_tb() {
    assert_eq!(parse_size("1TB").unwrap(), 1024_u64 * 1024 * 1024 * 1024);
    assert_eq!(
        parse_size("2tb").unwrap(),
        2 * 1024_u64 * 1024 * 1024 * 1024
    );
}

#[test]
fn parse_size_whitespace() {
    assert_eq!(parse_size("  1MB  ").unwrap(), 1024 * 1024);
}

#[test]
fn parse_size_invalid() {
    assert!(parse_size("").is_err());
    assert!(parse_size("abc").is_err());
    assert!(parse_size("MB").is_err());
    assert!(parse_size("-1KB").is_err());
}

// ═══════════════════════════════════════════════════════════════════════════
// Month extraction + parsing
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn month_from_unix_micros_epoch() {
    // 1970-01-01 00:00:00 UTC → January
    assert_eq!(month_from_unix_micros(0), 1);
}

#[test]
fn month_from_unix_micros_december() {
    // 2025-12-15 00:00:00 UTC → December
    // Dec 15 2025 = roughly 20437 days
    let us = 1_765_756_800_000_000_i64; // 2025-12-15 00:00 UTC
    assert_eq!(month_from_unix_micros(us), 12);
}

#[test]
fn parse_month_spec_single_month() {
    assert_eq!(parse_month_spec("january"), vec![1]);
    assert_eq!(parse_month_spec("Jan"), vec![1]);
    assert_eq!(parse_month_spec("dec"), vec![12]);
}

#[test]
fn parse_month_spec_quarter() {
    assert_eq!(parse_month_spec("Q1"), vec![1, 2, 3]);
    assert_eq!(parse_month_spec("q4"), vec![10, 11, 12]);
}

#[test]
fn parse_month_spec_combo() {
    assert_eq!(parse_month_spec("jan,feb"), vec![1, 2]);
    assert_eq!(parse_month_spec("Q1,october"), vec![1, 2, 3, 10]);
}

#[test]
fn parse_month_spec_dedup() {
    // Q1 includes jan; jan should not appear twice
    assert_eq!(parse_month_spec("Q1,jan"), vec![1, 2, 3]);
}

#[test]
fn parse_month_spec_unknown_ignored() {
    assert_eq!(parse_month_spec("foo"), Vec::<u32>::new());
}

// ═══════════════════════════════════════════════════════════════════════════
// Extension collection expansion
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn from_params_expands_extension_collections() {
    let filters = SearchFilters::from_params(&SearchFilterParams {
        ext_filter: Some("executables"),
        ..Default::default()
    });
    assert!(filters.extensions.contains(&"exe".to_owned()));
    assert!(filters.extensions.contains(&"bat".to_owned()));
    assert!(filters.extensions.contains(&"ps1".to_owned()));
}

#[test]
fn from_params_expands_documents_collection() {
    let filters = SearchFilters::from_params(&SearchFilterParams {
        ext_filter: Some("documents,rs"),
        ..Default::default()
    });
    // Should contain both expanded docs and the literal "rs"
    assert!(filters.extensions.contains(&"pdf".to_owned()));
    assert!(filters.extensions.contains(&"docx".to_owned()));
    assert!(filters.extensions.contains(&"rs".to_owned()));
}

/// Regression: `from_params` must convert CLI percentage to per-million scale.
/// `--min-bulkiness 200` (200%) → internal `2_000_000`.
#[test]
fn from_params_converts_bulkiness_percentage_to_per_million() {
    let filters = SearchFilters::from_params(&SearchFilterParams {
        min_bulkiness: Some(200),
        max_bulkiness: Some(500),
        ..Default::default()
    });
    assert_eq!(
        filters.min_bulkiness,
        Some(2_000_000),
        "200% → 2_000_000 per-million"
    );
    assert_eq!(
        filters.max_bulkiness,
        Some(5_000_000),
        "500% → 5_000_000 per-million"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// path_contains separator normalization
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn normalize_path_separators_collapses_double_backslash() {
    assert_eq!(normalize_path_separators("users\\\\rnio"), "users\\rnio");
}

#[test]
fn normalize_path_separators_replaces_forward_slash() {
    assert_eq!(normalize_path_separators("users/rnio"), "users\\rnio");
}

#[test]
fn normalize_path_separators_mixed_separators() {
    assert_eq!(
        normalize_path_separators("github//ultra\\\\fast/search"),
        "github\\ultra\\fast\\search"
    );
}

#[test]
fn normalize_path_separators_single_backslash_unchanged() {
    assert_eq!(normalize_path_separators("users\\rnio"), "users\\rnio");
}

#[test]
fn normalize_path_separators_no_separators() {
    assert_eq!(normalize_path_separators("rnio"), "rnio");
}

#[test]
fn from_params_path_contains_normalizes_separators() {
    let filters = SearchFilters::from_params(&SearchFilterParams {
        path_contains: Some("Users\\\\rnio\\\\GitHub"),
        ..Default::default()
    });
    assert_eq!(
        filters.path_contains_lower.as_deref(),
        Some("users\\rnio\\github"),
        "double backslashes should be collapsed to single"
    );
}

#[test]
fn from_params_path_contains_normalizes_forward_slashes() {
    let filters = SearchFilters::from_params(&SearchFilterParams {
        path_contains: Some("Users/rnio/GitHub"),
        ..Default::default()
    });
    assert_eq!(
        filters.path_contains_lower.as_deref(),
        Some("users\\rnio\\github"),
        "forward slashes should be converted to backslashes"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Attribute presets
// ═══════════════════════════════════════════════════════════════════════════

#[path = "tests_ext.rs"]
mod tests_ext;
