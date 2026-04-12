// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Tests for `MultiDriveBackend`, sort spec parsing, sort correctness,
//! `display_rows_to_dataframe`, and `format_sort_spec`.
//! Exception: `file_size_policy` — backend test suite, shared fixtures require
//! cohesion.

use super::*;

// ── Helpers ──────────────────────────────────────────────────────────

/// Build a minimal `DisplayRow` for sort / aggregation tests.
fn row(name: &str, drive: char, size: u64, modified: i64, created: i64) -> DisplayRow {
    DisplayRow::new(
        0,
        drive,
        format!("{drive}:\\{name}"),
        size,
        false,
        modified,
        created,
        0,
        0x20,
        size.next_multiple_of(512),
        0,
        0,
        0,
    )
}

fn dir_row(name: &str, drive: char, descendants: u32, treesize: u64) -> DisplayRow {
    DisplayRow::new(
        0,
        drive,
        format!("{drive}:\\{name}"),
        0,
        true,
        0,
        0,
        0,
        0x10,
        0,
        descendants,
        treesize,
        treesize,
    )
}

// ═══════════════════════════════════════════════════════════════════════
// parse_sort_spec
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn parse_sort_spec_single_column_with_direction() {
    let specs = parse_sort_spec("name:asc");
    assert_eq!(specs.len(), 1);
    let first = specs.first().expect("must have one spec");
    assert_eq!(first.column, SortColumn::Name);
    assert!(!first.descending);
}

#[test]
fn parse_sort_spec_default_direction_for_size_is_desc() {
    let specs = parse_sort_spec("size");
    let first = specs.first().expect("must have one spec");
    assert_eq!(first.column, SortColumn::Size);
    assert!(first.descending, "size default direction must be desc");
}

#[test]
fn parse_sort_spec_default_direction_for_name_is_asc() {
    let specs = parse_sort_spec("name");
    let first = specs.first().expect("must have one spec");
    assert_eq!(first.column, SortColumn::Name);
    assert!(!first.descending, "name default direction must be asc");
}

#[test]
fn parse_sort_spec_all_column_aliases() {
    let cases = [
        ("name", SortColumn::Name),
        ("size", SortColumn::Size),
        ("sizeondisk", SortColumn::SizeOnDisk),
        ("allocated", SortColumn::SizeOnDisk),
        ("created", SortColumn::Created),
        ("modified", SortColumn::Modified),
        ("date", SortColumn::Modified),
        ("written", SortColumn::Modified),
        ("accessed", SortColumn::Accessed),
        ("path", SortColumn::Path),
        ("drive", SortColumn::Drive),
        ("ext", SortColumn::Extension),
        ("extension", SortColumn::Extension),
        ("type", SortColumn::Type),
        ("descendants", SortColumn::Descendants),
    ];
    for (input, expected) in cases {
        let specs = parse_sort_spec(input);
        assert_eq!(
            specs.first().map(|spec| spec.column),
            Some(expected),
            "alias '{input}' must map to {expected:?}"
        );
    }
}

#[test]
fn parse_sort_spec_multi_tier() {
    let specs = parse_sort_spec("size:desc,name:asc");
    assert_eq!(specs.len(), 2);
    let first = specs.first().expect("first");
    let second = specs.get(1).expect("second");
    assert_eq!(first.column, SortColumn::Size);
    assert!(first.descending);
    assert_eq!(second.column, SortColumn::Name);
    assert!(!second.descending);
}

#[test]
fn parse_sort_spec_unknown_column_ignored() {
    let specs = parse_sort_spec("bogus,size:desc");
    assert_eq!(specs.len(), 1, "unknown column must be skipped");
    let first = specs.first().expect("first");
    assert_eq!(first.column, SortColumn::Size);
}

#[test]
fn parse_sort_spec_empty_string() {
    let specs = parse_sort_spec("");
    assert!(specs.is_empty());
}

/// Regression: `-size` prefix means descending (T84, T87).
#[test]
fn parse_sort_spec_dash_prefix_forces_descending() {
    let specs = parse_sort_spec("-size");
    assert_eq!(specs.len(), 1);
    let first = specs.first().expect("must have one spec");
    assert_eq!(first.column, SortColumn::Size);
    assert!(first.descending, "dash prefix must force descending");
}

/// Regression: `-modified,name` → modified desc, name asc (T67).
#[test]
fn parse_sort_spec_dash_prefix_multi_tier() {
    let specs = parse_sort_spec("-modified,name");
    assert_eq!(specs.len(), 2);
    let first = specs.first().expect("first");
    assert_eq!(first.column, SortColumn::Modified);
    assert!(first.descending, "dash prefix must force descending");
    let second = specs.get(1).expect("second");
    assert_eq!(second.column, SortColumn::Name);
    assert!(!second.descending, "name without dash should be asc");
}

/// Regression: dash prefix mixed with colon-suffix direction.
#[test]
fn parse_sort_spec_dash_prefix_with_colon_suffix() {
    // Colon suffix takes precedence over dash prefix.
    let specs = parse_sort_spec("-size:asc");
    assert_eq!(specs.len(), 1);
    let first = specs.first().expect("first");
    assert_eq!(first.column, SortColumn::Size);
    assert!(
        !first.descending,
        "explicit :asc suffix must override dash prefix"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// format_sort_spec
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn format_sort_spec_primary_only() {
    let result = format_sort_spec(SortColumn::Size, true, &[]);
    assert_eq!(result, "size:desc");
}

#[test]
fn format_sort_spec_with_extra_tiers() {
    let extra = vec![SortSpec {
        column: SortColumn::Name,
        descending: false,
    }];
    let result = format_sort_spec(SortColumn::Modified, true, &extra);
    assert_eq!(result, "modified:desc,name:asc");
}

// ═══════════════════════════════════════════════════════════════════════
// sort_rows — all column variants
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn sort_by_name_ascending() {
    let mut rows = vec![
        row("zebra.txt", 'C', 100, 0, 0),
        row("alpha.txt", 'C', 200, 0, 0),
    ];
    sort_rows(&mut rows, SortColumn::Name, false, &[]);
    assert_eq!(rows.first().expect("first").name(), "alpha.txt");
}

#[test]
fn sort_by_modified_descending() {
    let mut rows = vec![
        row("old.txt", 'C', 100, 1000, 0),
        row("new.txt", 'C', 100, 9000, 0),
    ];
    sort_rows(&mut rows, SortColumn::Modified, true, &[]);
    assert_eq!(rows.first().expect("first").name(), "new.txt");
}

#[test]
fn sort_by_created_descending() {
    let mut rows = vec![
        row("old.txt", 'C', 100, 0, 1000),
        row("new.txt", 'C', 100, 0, 9000),
    ];
    sort_rows(&mut rows, SortColumn::Created, true, &[]);
    assert_eq!(rows.first().expect("first").name(), "new.txt");
}

#[test]
fn sort_by_path_ascending() {
    let mut rows = vec![row("z.txt", 'D', 100, 0, 0), row("a.txt", 'C', 100, 0, 0)];
    sort_rows(&mut rows, SortColumn::Path, false, &[]);
    assert_eq!(rows.first().expect("first").name(), "a.txt");
}

#[test]
fn sort_by_extension_ascending() {
    let mut rows = vec![
        row("file.zip", 'C', 100, 0, 0),
        row("file.abc", 'C', 100, 0, 0),
    ];
    sort_rows(&mut rows, SortColumn::Extension, false, &[]);
    assert_eq!(rows.first().expect("first").name(), "file.abc");
}

#[test]
fn sort_by_descendants_descending() {
    let mut rows = vec![
        dir_row("small", 'C', 5, 1000),
        dir_row("big", 'C', 500, 50_000),
    ];
    sort_rows(&mut rows, SortColumn::Descendants, true, &[]);
    assert_eq!(rows.first().expect("first").name(), "big");
}

#[test]
fn sort_by_drive_ascending() {
    let mut rows = vec![
        row("file.txt", 'D', 100, 0, 0),
        row("file.txt", 'C', 100, 0, 0),
    ];
    sort_rows(&mut rows, SortColumn::Drive, false, &[]);
    assert_eq!(rows.first().expect("first").drive, 'C');
}

#[test]
fn sort_by_size_on_disk_descending() {
    let mut rows = vec![
        row("small.txt", 'C', 100, 0, 0),
        row("big.txt", 'C', 5000, 0, 0),
    ];
    sort_rows(&mut rows, SortColumn::SizeOnDisk, true, &[]);
    assert_eq!(rows.first().expect("first").name(), "big.txt");
}

#[test]
fn sort_multi_tier_size_then_name() {
    let mut rows = vec![
        row("beta.txt", 'C', 100, 0, 0),
        row("alpha.txt", 'C', 100, 0, 0),
        row("gamma.txt", 'C', 200, 0, 0),
    ];
    let tiers = vec![SortSpec {
        column: SortColumn::Name,
        descending: false,
    }];
    sort_rows(&mut rows, SortColumn::Size, true, &tiers);
    // gamma (200) first, then alpha (100), then beta (100) — name tiebreaker
    assert_eq!(rows.first().expect("first").name(), "gamma.txt");
    assert_eq!(rows.get(1).expect("second").name(), "alpha.txt");
    assert_eq!(rows.get(2).expect("third").name(), "beta.txt");
}

#[test]
fn sort_by_type_groups_by_semantic_category() {
    // .rs → code, .zip → archive, .jpg → picture
    // ascending: archive < code < picture
    let mut rows = vec![
        row("photo.jpg", 'C', 100, 0, 0),
        row("main.rs", 'C', 200, 0, 0),
        row("backup.zip", 'C', 50, 0, 0),
    ];
    sort_rows(&mut rows, SortColumn::Type, false, &[]);
    assert_eq!(rows.first().expect("first").name(), "backup.zip"); // archive
    assert_eq!(rows.get(1).expect("second").name(), "main.rs"); // code
    assert_eq!(rows.get(2).expect("third").name(), "photo.jpg"); // picture
}

#[test]
fn sort_by_type_descending() {
    let mut rows = vec![
        row("photo.jpg", 'C', 100, 0, 0),
        row("main.rs", 'C', 200, 0, 0),
        row("backup.zip", 'C', 50, 0, 0),
    ];
    sort_rows(&mut rows, SortColumn::Type, true, &[]);
    assert_eq!(rows.first().expect("first").name(), "photo.jpg"); // picture
    assert_eq!(rows.get(1).expect("second").name(), "main.rs"); // code
    assert_eq!(rows.get(2).expect("third").name(), "backup.zip"); // archive
}

#[test]
fn sort_by_type_then_size() {
    // Two code files with different sizes
    let mut rows = vec![
        row("big.rs", 'C', 5000, 0, 0),
        row("small.rs", 'C', 100, 0, 0),
        row("photo.jpg", 'C', 300, 0, 0),
    ];
    let tiers = vec![SortSpec {
        column: SortColumn::Size,
        descending: true,
    }];
    sort_rows(&mut rows, SortColumn::Type, false, &tiers);
    // code first (asc), then picture; within code, big before small (desc)
    assert_eq!(rows.first().expect("first").name(), "big.rs");
    assert_eq!(rows.get(1).expect("second").name(), "small.rs");
    assert_eq!(rows.get(2).expect("third").name(), "photo.jpg");
}

#[test]
fn sort_by_descendants_in_dir() {
    let mut rows = vec![
        dir_row("few", 'C', 5, 100),
        dir_row("many", 'C', 50, 500),
        dir_row("empty", 'C', 0, 0),
    ];
    sort_rows(&mut rows, SortColumn::Descendants, true, &[]);
    assert_eq!(rows.first().expect("first").name(), "many");
    assert_eq!(rows.get(1).expect("second").name(), "few");
    assert_eq!(rows.get(2).expect("third").name(), "empty");
}

// ═══════════════════════════════════════════════════════════════════════
// Multi-drive aggregation
// ═══════════════════════════════════════════════════════════════════════

/// Build a minimal `DriveCompactIndex` from the `query_tests` helpers.
fn build_two_drive_backend() -> MultiDriveBackend {
    use uffs_mft::index::{IndexNameRef, MftIndex, ROOT_FRS, SizeInfo};

    use crate::compact::build_compact_index;

    let mut backend = MultiDriveBackend::new();

    for (letter, file_name, file_size) in [('C', "report.txt", 400_u64), ('D', "data.csv", 800)] {
        let mut idx = MftIndex::new(letter);
        let root_off = idx.add_name(".");
        let root = idx.get_or_create(ROOT_FRS);
        root.stdinfo.set_directory(true);
        root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
        root.first_name.parent_frs = ROOT_FRS;

        let f_off = idx.add_name(file_name);
        let f_ext = idx.intern_extension(file_name);
        let file_rec = idx.get_or_create(200);
        file_rec.first_name.name = IndexNameRef::new(
            f_off,
            u16::try_from(file_name.len()).expect("name too long"),
            true,
            f_ext,
        );
        file_rec.first_name.parent_frs = ROOT_FRS;
        file_rec.first_stream.size = SizeInfo {
            length: file_size,
            allocated: file_size.next_multiple_of(512),
        };
        file_rec.stdinfo.flags = 0x20;

        let (drive, _, _) = build_compact_index(letter, &idx);
        backend.drives.push(drive);
    }
    backend
}

#[test]
fn multi_drive_merges_results_from_both_drives() {
    let mut backend = build_two_drive_backend();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = backend.search(SearchRequest::new("*", &mut filters));
    let drives: std::collections::HashSet<char> = result.rows.iter().map(|row| row.drive).collect();
    assert!(drives.contains(&'C'), "must include drive C results");
    assert!(drives.contains(&'D'), "must include drive D results");
}

#[test]
fn multi_drive_sort_across_drives() {
    let mut backend = build_two_drive_backend();
    backend.sort_column = SortColumn::Size;
    backend.sort_desc = true;
    let mut filters = super::super::filters::SearchFilters::default();
    let result = backend.search(SearchRequest {
        filter_mode: FilterMode::FilesOnly,
        ..SearchRequest::new("*", &mut filters)
    });
    // data.csv (800) on D should come before report.txt (400) on C
    let first = result.rows.first().expect("first");
    assert_eq!(
        first.name(),
        "data.csv",
        "largest file across drives must be first"
    );
    assert_eq!(first.drive, 'D');
}

#[test]
fn multi_drive_limit_caps_total() {
    let mut backend = build_two_drive_backend();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = backend.search(SearchRequest {
        result_limit: Some(2),
        ..SearchRequest::new("*", &mut filters)
    });
    assert!(
        result.rows.len() <= 2,
        "limit must cap across drives, got {}",
        result.rows.len()
    );
}

// ═══════════════════════════════════════════════════════════════════════
// display_rows_to_dataframe
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn display_rows_to_dataframe_column_count_and_height() {
    let rows = vec![
        row("file.txt", 'C', 1024, 5_000_000, 1_000_000),
        row("data.csv", 'D', 2048, 3_000_000, 2_000_000),
    ];
    let df = display_rows_to_dataframe(&rows).expect("DataFrame creation must not fail");
    assert_eq!(df.height(), 2, "row count");
    assert_eq!(df.width(), 14, "column count must be 14");
}

#[test]
fn display_rows_to_dataframe_values_correct() {
    let rows = vec![row("test.rs", 'C', 4096, 9_000_000, 1_000_000)];
    let df = display_rows_to_dataframe(&rows).expect("DataFrame creation must not fail");

    // Check name column
    let name_col = df.column("name").expect("name column");
    let name_val = name_col
        .str()
        .expect("str chunked")
        .get(0)
        .expect("first value");
    assert_eq!(name_val, "test.rs");

    // Check size column
    let size_col = df.column("size").expect("size column");
    let size_val = size_col
        .u64()
        .expect("u64 chunked")
        .get(0)
        .expect("first value");
    assert_eq!(size_val, 4096);

    // Check drive column (formatted as "C:")
    let drive_col = df.column("drive").expect("drive column");
    let drive_val = drive_col
        .str()
        .expect("str chunked")
        .get(0)
        .expect("first value");
    assert_eq!(drive_val, "C:");
}

#[test]
fn display_rows_to_dataframe_path_only_extracts_directory() {
    let rows = vec![DisplayRow::new(
        0,
        'C',
        "C:\\Users\\john\\file.txt".to_owned(),
        100,
        false,
        0,
        0,
        0,
        0x20,
        512,
        0,
        0,
        0,
    )];
    let df = display_rows_to_dataframe(&rows).expect("DataFrame creation must not fail");

    let path_only_col = df.column("path_only").expect("path_only column");
    let val = path_only_col
        .str()
        .expect("str chunked")
        .get(0)
        .expect("first value");
    assert_eq!(
        val, "C:\\Users\\john\\",
        "path_only must be directory portion including trailing backslash"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Empty pattern returns empty
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn search_empty_pattern_returns_empty() {
    let mut backend = build_two_drive_backend();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = backend.search(SearchRequest::new("", &mut filters));
    assert!(
        result.rows.is_empty(),
        "empty pattern must return no results"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Regression: TreeSize sort must use treesize, not modified time (T67a)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn sort_by_treesize_uses_treesize_not_modified() {
    let mut rows = vec![
        dir_row("small", 'C', 5, 1_000),
        dir_row("large", 'C', 10, 1_000_000),
        dir_row("medium", 'C', 7, 100_000),
    ];
    sort_rows(&mut rows, FieldId::TreeSize, true, &[]);
    let sizes: Vec<u64> = rows.iter().map(|row| row.treesize).collect();
    assert_eq!(
        sizes,
        vec![1_000_000, 100_000, 1_000],
        "treesize desc: largest first"
    );
}

#[test]
fn sort_by_treesize_ascending() {
    let mut rows = vec![
        dir_row("large", 'C', 10, 1_000_000),
        dir_row("small", 'C', 5, 1_000),
        dir_row("medium", 'C', 7, 100_000),
    ];
    sort_rows(&mut rows, FieldId::TreeSize, false, &[]);
    let sizes: Vec<u64> = rows.iter().map(|row| row.treesize).collect();
    assert_eq!(
        sizes,
        vec![1_000, 100_000, 1_000_000],
        "treesize asc: smallest first"
    );
}

// ═══════════════════════════════════════════════════════════════════════
// search_index() free function — concurrent-safe API
// ═══════════════════════════════════════════════════════════════════════

/// Build a `DriveIndex` with two drives (C: and D:) for `search_index` tests.
fn build_two_drive_index() -> DriveIndex {
    use alloc::sync::Arc;

    use uffs_mft::index::{IndexNameRef, MftIndex, ROOT_FRS, SizeInfo};

    use crate::compact::build_compact_index;

    let mut drives = Vec::new();
    for (letter, file_name, file_size) in [('C', "report.txt", 400_u64), ('D', "data.csv", 800)] {
        let mut idx = MftIndex::new(letter);
        let root_off = idx.add_name(".");
        let root = idx.get_or_create(ROOT_FRS);
        root.stdinfo.set_directory(true);
        root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
        root.first_name.parent_frs = ROOT_FRS;

        let f_off = idx.add_name(file_name);
        let f_ext = idx.intern_extension(file_name);
        let file_rec = idx.get_or_create(200);
        file_rec.first_name.name = IndexNameRef::new(
            f_off,
            u16::try_from(file_name.len()).expect("name too long"),
            true,
            f_ext,
        );
        file_rec.first_name.parent_frs = ROOT_FRS;
        file_rec.first_stream.size = SizeInfo {
            length: file_size,
            allocated: file_size.next_multiple_of(512),
        };
        file_rec.stdinfo.flags = 0x20;

        let (drive, _, _) = build_compact_index(letter, &idx);
        drives.push(Arc::new(drive));
    }
    DriveIndex { drives }
}

#[test]
fn search_index_returns_results_from_both_drives() {
    let index = build_two_drive_index();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest::new("*", &mut filters),
        FieldId::Modified,
        true,
        &[],
    );
    let drives: std::collections::HashSet<char> = result.rows.iter().map(|row| row.drive).collect();
    assert!(drives.contains(&'C'), "must include C: results");
    assert!(drives.contains(&'D'), "must include D: results");
}

#[test]
fn search_index_drives_filter_excludes_non_matching() {
    let index = build_two_drive_index();
    let mut filters = super::super::filters::SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest {
            drives_filter: &['C'],
            ..SearchRequest::new("*", &mut filters)
        },
        FieldId::Modified,
        true,
        &[],
    );
    assert!(
        result.rows.iter().all(|row| row.drive == 'C'),
        "drive filter must exclude D: results"
    );
    assert!(!result.rows.is_empty(), "must have at least one C: result");
}

#[test]
fn search_index_concurrent_calls_do_not_interfere() {
    use alloc::sync::Arc;

    let index = Arc::new(build_two_drive_index());
    let idx1 = Arc::clone(&index);
    let idx2 = Arc::clone(&index);

    let (r1, r2) = rayon::join(
        || {
            let mut filters = super::super::filters::SearchFilters::default();
            search_index(
                &idx1,
                SearchRequest::new("*", &mut filters),
                FieldId::Size,
                true,
                &[],
            )
        },
        || {
            let mut filters = super::super::filters::SearchFilters::default();
            search_index(
                &idx2,
                SearchRequest::new("*", &mut filters),
                FieldId::Modified,
                false,
                &[],
            )
        },
    );
    assert!(!r1.rows.is_empty(), "concurrent search 1 must return rows");
    assert!(!r2.rows.is_empty(), "concurrent search 2 must return rows");
}

// ═══════════════════════════════════════════════════════════════════════
// Boolean flag sorting — compare_by_column, sort_rows, field_to_attr_bit
// ═══════════════════════════════════════════════════════════════════════

/// Build a `DisplayRow` with explicit NTFS flags.
///
/// `is_directory` is derived from `flags & 0x0010`.  All rows share
/// identical timestamps (`modified = 5000`) so any sort that falls back
/// to `modified` instead of the flag value will produce unstable order.
fn flagged_row(name: &str, flags: u32) -> DisplayRow {
    DisplayRow::new(
        0,
        'C',
        format!("C:\\{name}"),
        100,
        flags & 0x0010 != 0,
        5000,
        5000,
        5000,
        flags,
        512,
        0,
        0,
        0,
    )
}

// ── field_to_attr_bit ────────────────────────────────────────────────

/// Verify every boolean `FieldId` maps to the documented NTFS
/// `FILE_ATTRIBUTE_*` constant.  A wrong value here silently breaks
/// sorting for that flag.
#[test]
fn field_to_attr_bit_matches_ntfs_constants() {
    use crate::search::sorting::field_to_attr_bit;

    let expected: &[(FieldId, u32)] = &[
        (FieldId::ReadOnly, 0x0001),
        (FieldId::Hidden, 0x0002),
        (FieldId::System, 0x0004),
        (FieldId::DirectoryFlag, 0x0010),
        (FieldId::Archive, 0x0020),
        (FieldId::Temporary, 0x0100),
        (FieldId::Sparse, 0x0200),
        (FieldId::Reparse, 0x0400),
        (FieldId::Compressed, 0x0800),
        (FieldId::Offline, 0x1000),
        (FieldId::NotIndexed, 0x2000),
        (FieldId::Encrypted, 0x4000),
        (FieldId::Integrity, 0x8000),
        (FieldId::Virtual, 0x0001_0000),
        (FieldId::NoScrub, 0x0002_0000),
        (FieldId::RecallOnOpen, 0x0004_0000),
        (FieldId::Pinned, 0x0008_0000),
        (FieldId::Unpinned, 0x0010_0000),
        (FieldId::RecallOnDataAccess, 0x0040_0000),
    ];
    for &(field, ntfs_bit) in expected {
        assert_eq!(
            field_to_attr_bit(field),
            ntfs_bit,
            "{field:?}: expected 0x{ntfs_bit:08X}, got 0x{:08X}",
            field_to_attr_bit(field)
        );
    }
}

/// Non-boolean fields must return 0 (no attribute bit).
#[test]
fn field_to_attr_bit_returns_zero_for_non_boolean_fields() {
    use crate::search::sorting::field_to_attr_bit;

    let non_boolean = [
        FieldId::Name,
        FieldId::Path,
        FieldId::PathOnly,
        FieldId::Size,
        FieldId::SizeOnDisk,
        FieldId::Created,
        FieldId::Modified,
        FieldId::Accessed,
        FieldId::Extension,
        FieldId::Type,
        FieldId::Descendants,
        FieldId::TreeSize,
    ];
    for field in non_boolean {
        assert_eq!(
            field_to_attr_bit(field),
            0,
            "{field:?} should return 0 (non-boolean)"
        );
    }
}

// ── sort_rows: boolean flags ─────────────────────────────────────────

/// Parametric helper: build two rows that differ only by a single flag
/// bit, verify `sort_rows` puts the flagged row first (desc) / last (asc).
///
/// Both rows have identical timestamps to catch any fallback to `modified`.
fn assert_sort_rows_boolean(field: SortColumn, flag_bit: u32) {
    // Use a base of 0 (no flags) so that `plain` never has the tested bit.
    // Add archive (0x20) only when NOT testing archive itself.
    let base = if flag_bit == 0x0020 { 0_u32 } else { 0x20 };
    let flagged = flagged_row("flagged.dat", flag_bit | base);
    let plain = flagged_row("plain.dat", base);

    // ── Descending: flagged first ──
    let mut rows_desc = vec![plain.clone(), flagged.clone()]; // wrong order
    sort_rows(&mut rows_desc, field, true, &[]);
    assert_eq!(
        rows_desc.first().expect("first").name(),
        "flagged.dat",
        "{field:?} desc: flagged row must sort first"
    );
    assert_eq!(
        rows_desc.last().expect("last").name(),
        "plain.dat",
        "{field:?} desc: plain row must sort last"
    );

    // ── Ascending: flagged last ──
    let mut rows_asc = vec![flagged, plain];
    sort_rows(&mut rows_asc, field, false, &[]);
    assert_eq!(
        rows_asc.first().expect("first").name(),
        "plain.dat",
        "{field:?} asc: plain row must sort first"
    );
    assert_eq!(
        rows_asc.last().expect("last").name(),
        "flagged.dat",
        "{field:?} asc: flagged row must sort last"
    );
}

#[test]
fn sort_rows_directory_flag() {
    // DirectoryFlag needs special handling: is_directory derives from flags.
    let dir = flagged_row("mydir", 0x0010);
    let file = flagged_row("myfile.dat", 0x0020);

    let mut rows_desc = vec![file.clone(), dir.clone()];
    sort_rows(&mut rows_desc, SortColumn::DirectoryFlag, true, &[]);
    assert_eq!(
        rows_desc.first().expect("first").name(),
        "mydir",
        "DirectoryFlag desc: directory must sort first"
    );
    assert_eq!(
        rows_desc.last().expect("last").name(),
        "myfile.dat",
        "DirectoryFlag desc: file must sort last"
    );

    let mut rows_asc = vec![dir, file];
    sort_rows(&mut rows_asc, SortColumn::DirectoryFlag, false, &[]);
    assert_eq!(
        rows_asc.first().expect("first").name(),
        "myfile.dat",
        "DirectoryFlag asc: file must sort first"
    );
    assert_eq!(
        rows_asc.last().expect("last").name(),
        "mydir",
        "DirectoryFlag asc: directory must sort last"
    );
}

#[test]
fn sort_rows_hidden_flag() {
    assert_sort_rows_boolean(SortColumn::Hidden, 0x0002);
}
#[test]
fn sort_rows_system_flag() {
    assert_sort_rows_boolean(SortColumn::System, 0x0004);
}
#[test]
fn sort_rows_readonly_flag() {
    assert_sort_rows_boolean(SortColumn::ReadOnly, 0x0001);
}
#[test]
fn sort_rows_archive_flag() {
    assert_sort_rows_boolean(SortColumn::Archive, 0x0020);
}
#[test]
fn sort_rows_compressed_flag() {
    assert_sort_rows_boolean(SortColumn::Compressed, 0x0800);
}
#[test]
fn sort_rows_encrypted_flag() {
    assert_sort_rows_boolean(SortColumn::Encrypted, 0x4000);
}
#[test]
fn sort_rows_sparse_flag() {
    assert_sort_rows_boolean(SortColumn::Sparse, 0x0200);
}
#[test]
fn sort_rows_reparse_flag() {
    assert_sort_rows_boolean(SortColumn::Reparse, 0x0400);
}
#[test]
fn sort_rows_offline_flag() {
    assert_sort_rows_boolean(SortColumn::Offline, 0x1000);
}
#[test]
fn sort_rows_not_indexed_flag() {
    assert_sort_rows_boolean(SortColumn::NotIndexed, 0x2000);
}
#[test]
fn sort_rows_integrity_flag() {
    assert_sort_rows_boolean(SortColumn::Integrity, 0x8000);
}
#[test]
fn sort_rows_no_scrub_flag() {
    assert_sort_rows_boolean(SortColumn::NoScrub, 0x0002_0000);
}
#[test]
fn sort_rows_pinned_flag() {
    assert_sort_rows_boolean(SortColumn::Pinned, 0x0008_0000);
}
#[test]
fn sort_rows_unpinned_flag() {
    assert_sort_rows_boolean(SortColumn::Unpinned, 0x0010_0000);
}

// ── sort_rows: multi-row boolean stability ───────────────────────────

/// With 5 rows (3 flagged, 2 unflagged), verify sort produces a clean
/// partition: all flagged records in one block, all unflagged in the other,
/// with name-based tiebreaking within each block.
#[test]
fn sort_rows_directory_flag_multi_row_stability() {
    let mut rows = vec![
        flagged_row("delta.txt", 0x0020),   // file
        flagged_row("alpha_dir", 0x0010),   // directory
        flagged_row("gamma.txt", 0x0020),   // file
        flagged_row("beta_dir", 0x0010),    // directory
        flagged_row("epsilon_dir", 0x0010), // directory
    ];

    sort_rows(&mut rows, SortColumn::DirectoryFlag, true, &[]);
    let names: Vec<&str> = rows.iter().map(DisplayRow::name).collect();

    // Desc: all dirs first, then all files.
    // Within each group: name tiebreaker is also reversed (desc),
    // so names appear Z→A within each flag partition.
    assert_eq!(
        names,
        &[
            "epsilon_dir",
            "beta_dir",
            "alpha_dir",
            "gamma.txt",
            "delta.txt",
        ],
        "DirectoryFlag desc: dirs first, then files, name-desc within each"
    );
}

/// Ascending: files first, then directories, alphabetical within each block.
#[test]
fn sort_rows_directory_flag_multi_row_ascending() {
    let mut rows = vec![
        flagged_row("delta.txt", 0x0020), // file
        flagged_row("alpha_dir", 0x0010), // directory
        flagged_row("gamma.txt", 0x0020), // file
        flagged_row("beta_dir", 0x0010),  // directory
    ];

    sort_rows(&mut rows, SortColumn::DirectoryFlag, false, &[]);
    let names: Vec<&str> = rows.iter().map(DisplayRow::name).collect();

    assert_eq!(
        names,
        &["delta.txt", "gamma.txt", "alpha_dir", "beta_dir",],
        "DirectoryFlag asc: files first, then dirs, alphabetical within each"
    );
}
