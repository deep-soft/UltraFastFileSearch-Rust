//! Tests for `SearchFilters`, `matches_record`, and `apply_search_filters`.

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
        _pad: [0; 4],
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
    let filters = SearchFilters::from_params(
        false,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(" .RS, JPG ,PnG "),
        None,
    );

    assert_eq!(filters.extensions, ["rs", "jpg", "png"]);
}

#[test]
fn resolve_ext_ids_for_drive_accepts_mixed_case_extensions() {
    let drive = test_drive_with_rs_file();
    let mut filters = SearchFilters::from_params(
        false,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some("RS"),
        None,
    );

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
