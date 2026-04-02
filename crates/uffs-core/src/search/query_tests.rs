//! Tests for compact-index query search: matching, sorting, filtering, limits.

use uffs_mft::index::{IndexNameRef, MftIndex, ROOT_FRS, SizeInfo};

use super::*;
use crate::compact::build_compact_index;
use crate::search::backend::{FilterMode, MultiDriveBackend, SortColumn};
use crate::search::filters::SearchFilters;

/// Build a test fixture with root + files + dir + system metafile.
fn build_test_drive() -> DriveCompactIndex {
    let mut idx = MftIndex::new('C');

    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS);
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = ROOT_FRS;

    let dir_name = "Projects";
    let dir_off = idx.add_name(dir_name);
    let dir_ext = idx.intern_extension(dir_name);
    let dir = idx.get_or_create(100);
    dir.stdinfo.set_directory(true);
    dir.stdinfo.flags = 0x10;
    dir.first_name.name = IndexNameRef::new(
        dir_off,
        u16::try_from(dir_name.len()).expect("name too long"),
        true,
        dir_ext,
    );
    dir.first_name.parent_frs = ROOT_FRS;
    dir.descendants = 2;
    dir.treesize = 700;

    let f1_name = "readme.txt";
    let f1_off = idx.add_name(f1_name);
    let f1_ext = idx.intern_extension(f1_name);
    let f1 = idx.get_or_create(200);
    f1.first_name.name = IndexNameRef::new(
        f1_off,
        u16::try_from(f1_name.len()).expect("name too long"),
        true,
        f1_ext,
    );
    f1.first_name.parent_frs = 100;
    f1.first_stream.size = SizeInfo {
        length: 400,
        allocated: 512,
    };
    f1.stdinfo.flags = 0x20;
    f1.stdinfo.modified = 5_000_000;
    f1.stdinfo.created = 1_000_000;

    let f2_name = "data.csv";
    let f2_off = idx.add_name(f2_name);
    let f2_ext = idx.intern_extension(f2_name);
    let f2 = idx.get_or_create(201);
    f2.first_name.name = IndexNameRef::new(
        f2_off,
        u16::try_from(f2_name.len()).expect("name too long"),
        true,
        f2_ext,
    );
    f2.first_name.parent_frs = 100;
    f2.first_stream.size = SizeInfo {
        length: 300,
        allocated: 512,
    };
    f2.stdinfo.flags = 0x20;
    f2.stdinfo.modified = 3_000_000;

    let sys_name = "$MFT";
    let sys_off = idx.add_name(sys_name);
    let sys_ext = idx.intern_extension(sys_name);
    let sys = idx.get_or_create(0);
    sys.first_name.name = IndexNameRef::new(
        sys_off,
        u16::try_from(sys_name.len()).expect("name too long"),
        true,
        sys_ext,
    );
    sys.first_name.parent_frs = ROOT_FRS;
    sys.first_stream.size = SizeInfo {
        length: 1_000_000,
        allocated: 1_048_576,
    };
    sys.stdinfo.flags = 0x06;

    let (drive, _, _) = build_compact_index('C', &idx);
    drive
}

/// Build a fixture with `count` files under root (for limit tests).
fn build_large_drive(count: usize) -> DriveCompactIndex {
    let mut idx = MftIndex::new('C');
    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS);
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = ROOT_FRS;
    for i in 0..count {
        let frs = (i as u64) + 100;
        let name = format!("f{i:05}.txt");
        let off = idx.add_name(&name);
        let ext = idx.intern_extension(&name);
        let rec = idx.get_or_create(frs);
        rec.first_name.name = IndexNameRef::new(
            off,
            u16::try_from(name.len()).expect("name too long"),
            true,
            ext,
        );
        rec.first_name.parent_frs = ROOT_FRS;
        rec.first_stream.size = SizeInfo {
            length: 100,
            allocated: 512,
        };
        rec.stdinfo.flags = 0x20;
    }
    let (drive, _, _) = build_compact_index('C', &idx);
    drive
}

// ── Search by name ─────────────────────────────────────────────────────

#[test]
fn search_compact_finds_file_by_name() {
    let drive = build_test_drive();
    let rows = search_compact_drive(&drive, "readme", 100, false, false);
    assert!(
        rows.iter().any(|row| row.name() == "readme.txt"),
        "search for 'readme' must find readme.txt"
    );
}

// ── DisplayRow field correctness ───────────────────────────────────────

#[test]
fn display_row_fields_match_source_data() {
    let drive = build_test_drive();
    let rows = search_compact_drive(&drive, "readme", 100, false, false);
    let row = rows
        .iter()
        .find(|row| row.name() == "readme.txt")
        .expect("not found");
    assert_eq!(row.drive, 'C');
    assert_eq!(row.size, 400);
    assert_eq!(row.allocated, 512);
    assert_eq!(row.flags, 0x20);
    assert!(!row.is_directory);
    assert_eq!(row.modified, 5_000_000);
    assert_eq!(row.created, 1_000_000);
}

// ── Directory tree metrics ─────────────────────────────────────────────

#[test]
fn display_row_directory_has_tree_metrics() {
    let drive = build_test_drive();
    let rows = search_compact_drive(&drive, "projects", 100, false, false);
    let row = rows
        .iter()
        .find(|row| row.name() == "Projects")
        .expect("not found");
    assert!(row.is_directory);
    assert_eq!(row.descendants, 2);
    assert_eq!(row.treesize, 700);
}

// ── MultiDriveBackend: filters ─────────────────────────────────────────

#[test]
fn multi_drive_search_applies_filters() {
    let drive = build_test_drive();
    let mut backend = MultiDriveBackend::new();
    backend.drives.push(drive);
    let filters = SearchFilters {
        hide_system: true,
        ..Default::default()
    };
    let result = backend.search("*", false, false, Some(100), FilterMode::All, &filters);
    assert!(
        !result.rows.iter().any(|row| row.name() == "$MFT"),
        "hide_system must filter $MFT"
    );
}

#[test]
fn multi_drive_search_files_only() {
    let drive = build_test_drive();
    let mut backend = MultiDriveBackend::new();
    backend.drives.push(drive);
    let filters = SearchFilters::default();
    let result = backend.search(
        "*",
        false,
        false,
        Some(100),
        FilterMode::FilesOnly,
        &filters,
    );
    assert!(
        !result.rows.iter().any(|row| row.is_directory),
        "FilesOnly must not return dirs"
    );
}

#[test]
fn multi_drive_search_dirs_only() {
    let drive = build_test_drive();
    let mut backend = MultiDriveBackend::new();
    backend.drives.push(drive);
    let filters = SearchFilters::default();
    let result = backend.search("*", false, false, Some(100), FilterMode::DirsOnly, &filters);
    assert!(
        result.rows.iter().all(|row| row.is_directory),
        "DirsOnly must only return dirs"
    );
}

// ── Sort correctness ───────────────────────────────────────────────────

#[test]
fn multi_drive_search_sort_by_size_desc() {
    let drive = build_test_drive();
    let mut backend = MultiDriveBackend::new();
    backend.sort_column = SortColumn::Size;
    backend.sort_desc = true;
    backend.drives.push(drive);
    let filters = SearchFilters {
        hide_system: true,
        ..Default::default()
    };
    let result = backend.search(
        "*",
        false,
        false,
        Some(100),
        FilterMode::FilesOnly,
        &filters,
    );
    for pair in result.rows.windows(2) {
        let left = pair.first().expect("window has first");
        let right = pair.get(1).expect("window has second");
        assert!(left.size >= right.size, "size desc violated");
    }
}

// ── Path resolution ────────────────────────────────────────────────────

#[test]
fn display_row_path_includes_volume_prefix() {
    let drive = build_test_drive();
    let rows = search_compact_drive(&drive, "readme", 100, false, false);
    let row = rows
        .iter()
        .find(|row| row.name() == "readme.txt")
        .expect("not found");
    assert!(
        row.path.starts_with("C:\\"),
        "path must start with C:\\, got: {}",
        row.path
    );
}

// ── Limit semantics ───────────────────────────────────────────────────
// Regression: None = unlimited (no hidden default cap), Some(n) = capped.

#[test]
fn match_all_none_limit_is_unlimited() {
    let drive = build_large_drive(1_500);
    let mut backend = MultiDriveBackend::new();
    backend.drives.push(drive);
    let filters = SearchFilters::default();
    let all = backend.search("*", false, false, None, FilterMode::All, &filters);
    assert!(
        all.rows.len() >= 1_500,
        "None must be unlimited, got {}",
        all.rows.len()
    );
}

#[test]
fn match_all_explicit_limit_caps_results() {
    let drive = build_large_drive(1_500);
    let mut backend = MultiDriveBackend::new();
    backend.drives.push(drive);
    let filters = SearchFilters::default();
    let cap = backend.search("*", false, false, Some(500), FilterMode::All, &filters);
    assert!(
        cap.rows.len() <= 500,
        "Some(500) must cap, got {}",
        cap.rows.len()
    );
}

#[test]
fn unlimited_returns_more_than_capped() {
    let drive = build_large_drive(1_500);
    let mut backend = MultiDriveBackend::new();
    backend.drives.push(drive);
    let filters = SearchFilters::default();
    let all = backend.search("*", false, false, None, FilterMode::All, &filters);
    let cap = backend.search("*", false, false, Some(500), FilterMode::All, &filters);
    assert!(all.rows.len() > cap.rows.len(), "unlimited > capped");
}

// ═══════════════════════════════════════════════════════════════════════
// Regex search (search_compact_drive_regex)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn regex_search_finds_matching_files() {
    let drive = build_test_drive();
    let re = regex::Regex::new("(?i)readme").expect("valid regex");
    let rows = search_compact_drive_regex(&drive, &re, 100);
    assert!(
        rows.iter().any(|row| row.name() == "readme.txt"),
        "regex 'readme' must find readme.txt"
    );
}

#[test]
fn regex_search_no_match_returns_empty() {
    let drive = build_test_drive();
    let re = regex::Regex::new("zzz_no_match[0-9]+").expect("valid regex");
    let rows = search_compact_drive_regex(&drive, &re, 100);
    assert!(rows.is_empty(), "regex with no match must return empty");
}

#[test]
fn regex_search_respects_limit() {
    let drive = build_large_drive(500);
    let re = regex::Regex::new("f[0-9]+").expect("valid regex");
    let rows = search_compact_drive_regex(&drive, &re, 10);
    assert!(
        rows.len() <= 10,
        "regex search must respect limit, got {}",
        rows.len()
    );
}

// ═══════════════════════════════════════════════════════════════════════
// make_display_row ADS logic
// ═══════════════════════════════════════════════════════════════════════

/// Build a fixture with an ADS on a directory.
fn build_ads_on_dir_drive() -> DriveCompactIndex {
    use uffs_mft::index::{
        IndexNameRef, IndexStreamInfo, MftIndex, NO_ENTRY, ROOT_FRS, SizeInfo, StandardInfo,
    };

    let mut idx = MftIndex::new('C');

    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS);
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = ROOT_FRS;

    // A directory with an ADS
    let dir_name = "MyFolder";
    let dir_off = idx.add_name(dir_name);
    let dir_ext = idx.intern_extension(dir_name);
    let dir_rec = idx.get_or_create(100);
    dir_rec.stdinfo.set_directory(true);
    dir_rec.stdinfo.flags |= StandardInfo::IS_ARCHIVE;
    dir_rec.first_name.name = IndexNameRef::new(
        dir_off,
        u16::try_from(dir_name.len()).expect("len"),
        true,
        dir_ext,
    );
    dir_rec.first_name.parent_frs = ROOT_FRS;

    // Add ADS stream
    let stream_name = "metadata";
    let stream_off = idx.add_name(stream_name);
    let stream_ref = IndexNameRef::new(
        stream_off,
        u16::try_from(stream_name.len()).expect("len"),
        true,
        0,
    );
    #[expect(clippy::cast_possible_truncation, reason = "test fixture")]
    let si = idx.streams.len() as u32;
    idx.streams.push(IndexStreamInfo {
        size: SizeInfo {
            length: 42,
            allocated: 64,
        },
        next_entry: NO_ENTRY,
        name: stream_ref,
        flags: 8 << 2,
        _pad0: [0; 3],
    });

    let dir_idx = idx.frs_to_idx_opt(100).expect("dir idx");
    let dir_mut = idx.records.get_mut(dir_idx).expect("dir record");
    dir_mut.first_stream.next_entry = si;
    dir_mut.stream_count = 2;
    dir_mut.total_stream_count = 2;

    let (drive, _, _) = build_compact_index('C', &idx);
    drive
}

#[test]
fn ads_on_directory_display_row_is_not_directory() {
    let drive = build_ads_on_dir_drive();
    // needle must be lowered — search_compact_drive expects pre-lowered for
    // case-insensitive
    let rows = search_compact_drive(&drive, "myfolder:metadata", 100, false, false);
    let ads_row = rows
        .iter()
        .find(|row| row.name().contains(':'))
        .expect("ADS row must exist");
    assert!(
        !ads_row.is_directory,
        "ADS on directory must render as non-directory in DisplayRow"
    );
    assert_eq!(ads_row.size, 42, "ADS must show stream size");
}

#[test]
fn normal_directory_display_row_is_directory() {
    let drive = build_ads_on_dir_drive();
    // needle must be lowered — search_compact_drive expects pre-lowered for
    // case-insensitive
    let rows = search_compact_drive(&drive, "myfolder", 100, false, false);
    let dir_row = rows
        .iter()
        .find(|row| row.name() == "MyFolder")
        .expect("directory row must exist");
    assert!(
        dir_row.is_directory,
        "normal directory must render as directory in DisplayRow"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Case-sensitive and whole-word search
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn case_sensitive_search_misses_wrong_case() {
    let drive = build_test_drive();
    let rows = search_compact_drive(&drive, "README", 100, true, false);
    assert!(
        !rows.iter().any(|row| row.name() == "readme.txt"),
        "case-sensitive 'README' must not match 'readme.txt'"
    );
}

#[test]
fn case_insensitive_search_finds_any_case() {
    let drive = build_test_drive();
    // needle must be pre-lowered for case-insensitive search (caller's
    // responsibility)
    let rows = search_compact_drive(&drive, "readme", 100, false, false);
    assert!(
        rows.iter().any(|row| row.name() == "readme.txt"),
        "case-insensitive 'readme' must match 'readme.txt'"
    );
}

#[test]
fn whole_word_search_exact_match() {
    let drive = build_test_drive();
    // Whole-word with exact name (no extension)
    let rows = search_compact_drive(&drive, "readme.txt", 100, false, true);
    assert!(
        rows.iter().any(|row| row.name() == "readme.txt"),
        "whole-word exact match must find readme.txt"
    );
}
