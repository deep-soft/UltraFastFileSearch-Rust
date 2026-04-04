//! Tests for `MultiDriveBackend`, sort spec parsing, sort correctness,
//! `display_rows_to_dataframe`, and `format_sort_spec`.

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
    let result = backend.search("*", false, false, None, FilterMode::All, &mut filters);
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
    let result = backend.search("*", false, false, None, FilterMode::FilesOnly, &mut filters);
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
    let result = backend.search("*", false, false, Some(2), FilterMode::All, &mut filters);
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
    let result = backend.search("", false, false, None, FilterMode::All, &mut filters);
    assert!(
        result.rows.is_empty(),
        "empty pattern must return no results"
    );
}
