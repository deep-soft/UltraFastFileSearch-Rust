//! Integration tests for aggregation and predicate conversion.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::float_arithmetic,
    clippy::missing_docs_in_private_items,
    clippy::cast_possible_truncation,
    clippy::min_ident_chars,
    clippy::too_many_lines,
    clippy::wildcard_imports,
    clippy::default_numeric_fallback,
    clippy::std_instead_of_alloc
)]

use std::sync::Arc;

use uffs_client::protocol::AggregateSpecWire;
use uffs_core::compact::build_compact_index;
use uffs_core::search::backend::DriveIndex;
use uffs_mft::index::{IndexNameRef, MftIndex, ROOT_FRS, SizeInfo};

use super::IndexManager;

/// Build a synthetic drive with root + 1 dir + 5 files of varied
/// sizes/extensions.
fn build_test_drive() -> uffs_core::compact::DriveCompactIndex {
    let mut idx = MftIndex::new('C');

    // Root directory
    let root_off = idx.add_name(".");
    let root = idx.get_or_create(ROOT_FRS);
    root.stdinfo.set_directory(true);
    root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
    root.first_name.parent_frs = ROOT_FRS;

    // Subdirectory "Projects"
    let dir_name = "Projects";
    let dir_off = idx.add_name(dir_name);
    let dir_ext = idx.intern_extension(dir_name);
    let dir = idx.get_or_create(100);
    dir.stdinfo.set_directory(true);
    dir.stdinfo.flags = 0x10; // directory flag
    dir.first_name.name = IndexNameRef::new(dir_off, dir_name.len() as u16, true, dir_ext);
    dir.first_name.parent_frs = ROOT_FRS;

    // Files with different extensions and sizes
    let files: &[(&str, u64, u64, u64)] = &[
        ("readme.md", 101, 500, 512),
        ("main.rs", 102, 2000, 4096),
        ("lib.rs", 103, 3000, 4096),
        ("config.toml", 104, 100, 512),
        ("data.bin", 105, 10_000, 16_384),
    ];

    for &(name, frs, size, allocated) in files {
        let off = idx.add_name(name);
        let ext = idx.intern_extension(name);
        let rec = idx.get_or_create(frs);
        rec.first_name.name = IndexNameRef::new(off, name.len() as u16, true, ext);
        rec.first_name.parent_frs = 100; // under Projects
        rec.first_stream.size = SizeInfo {
            length: size,
            allocated,
        };
        rec.stdinfo.flags = 0x20; // archive
        rec.stdinfo.modified = 1_000_000;
    }

    let (drive, _, _) = build_compact_index('C', &idx);
    drive
}

fn test_index() -> DriveIndex {
    DriveIndex {
        drives: vec![Arc::new(build_test_drive())],
    }
}

fn spec(kind: &str) -> AggregateSpecWire {
    AggregateSpecWire {
        kind: kind.to_owned(),
        label: None,
        field: None,
        top: None,
        interval: None,
        calendar: None,
        boundaries: vec![],
        metrics: vec![],
        preset: None,
    }
}

// ── Preset round-trip ────────────────────────────────────────────

#[test]
fn preset_overview_returns_multiple_results() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        preset: Some("overview".to_owned()),
        ..spec("preset")
    }];
    let results = IndexManager::run_aggregations(&index, &specs);
    // overview preset expands to count + stats + terms etc.
    assert!(
        results.len() >= 3,
        "overview should produce ≥3 results, got {}",
        results.len()
    );
    // First result is typically count
    let count = results.iter().find(|r| r.kind == "count").unwrap();
    // 5 files + 1 dir + root = 7 records total
    assert!(count.value.unwrap() >= 5, "count should be ≥5 files");
}

// ── Count ────────────────────────────────────────────────────────

#[test]
fn count_returns_total_records() {
    let index = test_index();
    let results = IndexManager::run_aggregations(&index, &[spec("count")]);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].kind, "count");
    // root + Projects dir + 5 files = 7
    assert_eq!(results[0].value, Some(7));
}

// ── Stats ────────────────────────────────────────────────────────

#[test]
fn stats_size_returns_metrics() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("size".to_owned()),
        ..spec("stats")
    }];
    let results = IndexManager::run_aggregations(&index, &specs);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].kind, "stats");
    let stats = results[0].stats.as_ref().unwrap();
    assert!(stats.count > 0);
    // Total of 500+2000+3000+100+10000 = 15600 for files; dirs have size 0
    assert!(stats.sum > 0);
    assert!(stats.min <= stats.max);
}

// ── Terms ────────────────────────────────────────────────────────

#[test]
fn terms_extension_returns_buckets() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("extension".to_owned()),
        top: Some(10),
        ..spec("terms")
    }];
    let results = IndexManager::run_aggregations(&index, &specs);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].kind, "buckets");
    // We have rs, md, toml, bin extensions
    assert!(
        results[0].buckets.len() >= 3,
        "expected ≥3 ext buckets, got {}",
        results[0].buckets.len()
    );
    // "rs" should have 2 files (main.rs, lib.rs)
    let rs_bucket = results[0].buckets.iter().find(|b| b.key == "rs");
    assert!(rs_bucket.is_some(), "should have 'rs' bucket");
    assert_eq!(rs_bucket.unwrap().count, 2);
}

// ── Histogram ────────────────────────────────────────────────────

#[test]
fn histogram_size_returns_buckets() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("size".to_owned()),
        ..spec("histogram")
    }];
    let results = IndexManager::run_aggregations(&index, &specs);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].kind, "buckets");
    // Should have at least 1 bucket covering the file sizes
    assert!(!results[0].buckets.is_empty());
}

// ── Date Histogram ───────────────────────────────────────────────

#[test]
fn date_histogram_returns_buckets() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("modified".to_owned()),
        calendar: Some("month".to_owned()),
        ..spec("datehist")
    }];
    let results = IndexManager::run_aggregations(&index, &specs);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].kind, "buckets");
}

// ── Missing ──────────────────────────────────────────────────────

#[test]
fn missing_extension_counts_records_without_ext() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("extension".to_owned()),
        ..spec("missing")
    }];
    let results = IndexManager::run_aggregations(&index, &specs);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].kind, "missing");
    // Root "." and dir "Projects" have no extension → ≥2 missing
    assert!(results[0].value.unwrap() >= 2);
}

// ── Distinct ─────────────────────────────────────────────────────

#[test]
fn distinct_extension_counts_unique_values() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("extension".to_owned()),
        ..spec("distinct")
    }];
    let results = IndexManager::run_aggregations(&index, &specs);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].kind, "distinct");
    // rs, md, toml, bin → 4 distinct extensions
    assert!(results[0].value.unwrap() >= 4);
}

// ── Rollup ───────────────────────────────────────────────────────

#[test]
fn rollup_drive_returns_buckets() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("drive".to_owned()),
        top: Some(10),
        ..spec("rollup")
    }];
    let results = IndexManager::run_aggregations(&index, &specs);
    assert_eq!(results.len(), 1);
    // Rollup → buckets or rollup kind
    assert!(!results[0].buckets.is_empty() || results[0].value.is_some());
}

// ── Duplicates ───────────────────────────────────────────────────

#[test]
fn duplicates_returns_result() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        top: Some(10),
        ..spec("duplicates")
    }];
    let results = IndexManager::run_aggregations(&index, &specs);
    // Should return exactly 1 result (even if 0 duplicates)
    assert_eq!(results.len(), 1);
}

// ── Raw power syntax ─────────────────────────────────────────────

#[test]
fn raw_power_syntax_terms_works() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        label: Some("terms:extension,top=5".to_owned()),
        ..spec("raw")
    }];
    let results = IndexManager::run_aggregations(&index, &specs);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].kind, "buckets");
    assert!(!results[0].buckets.is_empty());
}

// ── Error handling ───────────────────────────────────────────────

#[test]
fn unknown_kind_skipped_gracefully() {
    let index = test_index();
    let specs = [spec("bogus_kind")];
    let results = IndexManager::run_aggregations(&index, &specs);
    assert!(results.is_empty(), "unknown kind should produce no results");
}

#[test]
fn missing_field_skipped_gracefully() {
    let index = test_index();
    // stats requires a field but none provided
    let specs = [spec("stats")];
    let results = IndexManager::run_aggregations(&index, &specs);
    assert!(
        results.is_empty(),
        "missing field should produce no results"
    );
}

// ── Multiple specs in one call ───────────────────────────────────

#[test]
fn multiple_specs_return_multiple_results() {
    let index = test_index();
    let specs = [
        spec("count"),
        AggregateSpecWire {
            field: Some("size".to_owned()),
            ..spec("stats")
        },
        AggregateSpecWire {
            field: Some("extension".to_owned()),
            top: Some(5),
            ..spec("terms")
        },
    ];
    let results = IndexManager::run_aggregations(&index, &specs);
    assert_eq!(results.len(), 3, "should return one result per spec");
    assert_eq!(results[0].kind, "count");
    assert_eq!(results[1].kind, "stats");
    assert_eq!(results[2].kind, "buckets");
}

// ── S1H.2: uffs stats daemon-path parity ─────────────────────────

#[test]
fn stats_overview_preset_wire_roundtrip() {
    // Simulate the exact wire spec that `uffs stats` (no path)
    // sends to the daemon, and verify it produces correct results.
    let index = test_index();
    let specs = [AggregateSpecWire {
        kind: "preset".to_owned(),
        label: None,
        field: None,
        top: None,
        interval: None,
        calendar: None,
        boundaries: vec![],
        metrics: vec![],
        preset: Some("overview".to_owned()),
    }];
    let results = IndexManager::run_aggregations(&index, &specs);

    // Overview preset expands to multiple results.
    assert!(
        results.len() >= 3,
        "overview should produce ≥3 results, got {}",
        results.len()
    );

    // Must include a count result.
    let count = results.iter().find(|r| r.kind == "count").unwrap();
    assert_eq!(count.value, Some(7)); // root + dir + 5 files

    // Must include a stats result with valid metrics.
    let stats = results.iter().find(|r| r.kind == "stats").unwrap();
    let s = stats.stats.as_ref().unwrap();
    assert!(s.count > 0);
    assert!(s.sum > 0);

    // Must include a buckets result (extension or type terms).
    let has_buckets = results.iter().any(|r| r.kind == "buckets");
    assert!(has_buckets, "overview should include bucket results");
}
