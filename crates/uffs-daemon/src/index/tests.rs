// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Integration tests for aggregation and predicate conversion.
//! Exception: `file_size_policy` — aggregation test suite, shared fixture requires cohesion.

#![expect(
    clippy::indexing_slicing,
    clippy::min_ident_chars,
    clippy::std_instead_of_alloc,
    reason = "test code — relaxed linting for test clarity"
)]

use std::sync::Arc;

use uffs_client::protocol::AggregateSpecWire;
use uffs_core::aggregate::AggregateFilter;
use uffs_core::aggregate::spec::AggregateKind;
use uffs_core::compact::build_compact_index;
use uffs_core::search::backend::DriveIndex;
use uffs_core::search::field::FieldId;
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
    dir.first_name.name =
        IndexNameRef::new(dir_off, uffs_mft::len_to_u16(dir_name.len()), true, dir_ext);
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
        rec.first_name.name = IndexNameRef::new(off, uffs_mft::len_to_u16(name.len()), true, ext);
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
        ..AggregateSpecWire::default()
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
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
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
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &[spec("count")],
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
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
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
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
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
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
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
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
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
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
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
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
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
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
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
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
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
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
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].kind, "buckets");
    assert!(!results[0].buckets.is_empty());
}

// ── Error handling ───────────────────────────────────────────────

#[test]
fn unknown_kind_skipped_gracefully() {
    let index = test_index();
    let specs = [spec("bogus_kind")];
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
    assert!(results.is_empty(), "unknown kind should produce no results");
}

#[test]
fn missing_field_skipped_gracefully() {
    let index = test_index();
    // stats requires a field but none provided
    let specs = [spec("stats")];
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
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
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
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
        preset: Some("overview".to_owned()),
        ..AggregateSpecWire::default()
    }];
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );

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

// ── S2G.13: terms with sample=2 produces sample_rows + drilldown ──

#[test]
fn terms_with_sample_produces_sample_rows_and_drilldown() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        kind: "terms".to_owned(),
        field: Some("extension".to_owned()),
        top: Some(5),
        sample: Some(2),
        sample_sort: None,
        sample_desc: None,
        ..spec("terms")
    }];
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
    assert!(!results.is_empty(), "should have results");

    let bucket_result = results
        .iter()
        .find(|r| r.kind == "buckets")
        .expect("should have a buckets result");
    assert!(
        !bucket_result.buckets.is_empty(),
        "should have at least one bucket"
    );

    // At least one bucket should have sample_rows (our synthetic index
    // has files with extensions, so TopHits should find records).
    let has_samples = bucket_result
        .buckets
        .iter()
        .any(|b| !b.sample_rows.is_empty());
    assert!(
        has_samples,
        "at least one bucket should have sample rows with sample=2"
    );

    // Verify sample row constraints.
    for b in &bucket_result.buckets {
        assert!(
            b.sample_rows.len() <= 2,
            "sample rows should be bounded by sample=2, got {}",
            b.sample_rows.len()
        );
    }

    // Every bucket should have a drilldown predicate for the bucket key.
    for b in &bucket_result.buckets {
        assert!(
            !b.drilldown.is_empty(),
            "bucket '{}' should have drilldown predicates",
            b.key
        );
        let has_key_pred = b.drilldown.iter().any(|d| d.field == "extension");
        assert!(
            has_key_pred,
            "bucket '{}' should have an extension drilldown predicate",
            b.key
        );
    }
}

#[test]
fn terms_without_sample_has_empty_sample_rows() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        kind: "terms".to_owned(),
        field: Some("extension".to_owned()),
        top: Some(5),
        ..spec("terms")
    }];
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
    let bucket_result = results
        .iter()
        .find(|r| r.kind == "buckets")
        .expect("should have buckets");
    // No sample was requested → all sample_rows should be empty.
    for b in &bucket_result.buckets {
        assert!(
            b.sample_rows.is_empty(),
            "bucket '{}' should not have sample rows without sample spec",
            b.key
        );
    }
}

// ── Stage 2 gap-fill: daemon integration tests ────────────────────

#[test]
fn rollup_drive_via_wire() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        kind: "rollup".to_owned(),
        field: Some("drive".to_owned()),
        top: Some(10),
        ..spec("rollup")
    }];
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
    assert!(!results.is_empty(), "rollup:drive should return results");
    let result = results
        .iter()
        .find(|r| r.kind == "rollup")
        .expect("should have a rollup result");
    assert!(
        !result.buckets.is_empty(),
        "rollup:drive should have buckets"
    );
}

#[test]
fn rollup_path_with_sample_via_wire() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        kind: "rollup".to_owned(),
        field: Some("path".to_owned()),
        top: Some(5),
        sample: Some(2),
        ..spec("rollup")
    }];
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
    assert!(
        !results.is_empty(),
        "rollup:path with sample should return results"
    );
    let result = results
        .iter()
        .find(|r| r.kind == "rollup")
        .expect("should have a rollup result");
    // Sample rows should be bounded.
    for b in &result.buckets {
        assert!(
            b.sample_rows.len() <= 2,
            "rollup bucket '{}' should have ≤2 sample rows, got {}",
            b.key,
            b.sample_rows.len()
        );
    }
}

#[test]
fn convert_wire_spec_terms_with_sample_fields() {
    let ws = AggregateSpecWire {
        kind: "terms".to_owned(),
        field: Some("extension".to_owned()),
        top: Some(5),
        sample: Some(3),
        sample_sort: Some("size".to_owned()),
        sample_desc: Some(true),
        ..spec("terms")
    };
    let converted = IndexManager::convert_wire_spec(&ws).unwrap();
    assert_eq!(converted.len(), 1);
    assert!(
        matches!(&converted[0].kind, AggregateKind::Terms { .. }),
        "expected Terms variant"
    );
    if let AggregateKind::Terms { sample, .. } = &converted[0].kind {
        let top_hits = sample.as_ref().expect("sample should be Some");
        assert_eq!(top_hits.count, 3);
        assert_eq!(top_hits.sort_field, FieldId::Size);
        assert!(top_hits.sort_desc, "sort_desc should be true");
    }
}

#[test]
fn query_predicates_forwarded_to_drilldown() {
    use uffs_core::aggregate::finalize::{DrilldownPredicate, DrilldownValue};
    let index = test_index();
    let specs = [AggregateSpecWire {
        kind: "terms".to_owned(),
        field: Some("extension".to_owned()),
        top: Some(3),
        sample: Some(1),
        ..spec("terms")
    }];
    let predicates = vec![DrilldownPredicate {
        field: "name".to_owned(),
        op: "glob".to_owned(),
        value: DrilldownValue::String("*.rs".to_owned()),
    }];
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        predicates,
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
    let result = results
        .iter()
        .find(|r| r.kind == "buckets")
        .expect("should have buckets");
    // Each bucket's drilldown should include the query predicate for "name".
    for b in &result.buckets {
        let has_name_pred = b
            .drilldown
            .iter()
            .any(|d| d.field == "name" && d.op == "glob");
        assert!(
            has_name_pred,
            "bucket '{}' should have query predicate for 'name' in drilldown",
            b.key
        );
    }
}

#[test]
fn raw_power_syntax_rollup_drive() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        kind: "raw".to_owned(),
        label: Some("rollup:drive,top=5".to_owned()),
        ..spec("raw")
    }];
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
    assert!(
        !results.is_empty(),
        "raw rollup:drive should return results"
    );
}

#[test]
fn raw_power_syntax_hist_size() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        kind: "raw".to_owned(),
        label: Some("hist:size,interval=1048576".to_owned()),
        ..spec("raw")
    }];
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
    assert!(!results.is_empty(), "raw hist:size should return results");
}

#[test]
fn raw_power_syntax_stats_size() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        kind: "raw".to_owned(),
        label: Some("stats:size".to_owned()),
        ..spec("raw")
    }];
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
    assert!(!results.is_empty(), "raw stats:size should return results");
    let stats = results.iter().find(|r| r.kind == "stats");
    assert!(stats.is_some(), "should have a stats result");
}

// ── Cursor pagination ─────────────────────────────────────────────

#[test]
fn page_size_paginates_terms_buckets() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("extension".to_owned()),
        top: Some(10),
        ..spec("terms")
    }];
    // Request page_size=2 → first page should have ≤2 buckets.
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        Some(2),
        None,
        &[],
        &AggregateFilter::default(),
    );
    let terms = results.iter().find(|r| r.kind == "buckets").unwrap();
    assert!(
        terms.buckets.len() <= 2,
        "expected ≤2 buckets, got {}",
        terms.buckets.len()
    );
    // With 4 extensions and page_size=2, next_cursor should be present.
    assert!(
        terms.next_cursor.is_some(),
        "expected next_cursor for first page of 4 extensions with page_size=2"
    );
}

#[test]
fn cursor_returns_next_page() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("extension".to_owned()),
        top: Some(10),
        ..spec("terms")
    }];

    // First page.
    let (page1, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        Some(2),
        None,
        &[],
        &AggregateFilter::default(),
    );
    let terms1 = page1.iter().find(|r| r.kind == "buckets").unwrap();
    let cursor = terms1
        .next_cursor
        .as_deref()
        .expect("first page should have next_cursor");

    // Second page using cursor from first.
    let (page2, _matched_page2) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        Some(cursor),
        Some(2),
        None,
        &[],
        &AggregateFilter::default(),
    );
    let terms2 = page2.iter().find(|r| r.kind == "buckets").unwrap();

    // Second page should have different keys than first page.
    let keys1: Vec<&str> = terms1.buckets.iter().map(|b| b.key.as_str()).collect();
    let keys2: Vec<&str> = terms2.buckets.iter().map(|b| b.key.as_str()).collect();
    assert!(
        !keys2.is_empty(),
        "second page should have at least 1 bucket"
    );
    for key in &keys2 {
        assert!(
            !keys1.contains(key),
            "second page key `{key}` should not appear in first page"
        );
    }
}

#[test]
fn page_size_does_not_affect_non_bucket_results() {
    let index = test_index();
    let specs = [spec("count")];
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        Some(2),
        None,
        &[],
        &AggregateFilter::default(),
    );
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].kind, "count");
    // Count results should not have next_cursor.
    assert!(
        results[0].next_cursor.is_none(),
        "count results should never have next_cursor"
    );
}

#[test]
fn no_pagination_returns_all_buckets() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("extension".to_owned()),
        top: Some(10),
        ..spec("terms")
    }];
    // Without pagination, all buckets returned.
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        None,
        None,
        &[],
        &AggregateFilter::default(),
    );
    let terms = results.iter().find(|r| r.kind == "buckets").unwrap();
    assert!(
        terms.buckets.len() >= 4,
        "expected ≥4 extension buckets without pagination, got {}",
        terms.buckets.len()
    );
    assert!(
        terms.next_cursor.is_none(),
        "no pagination should mean no next_cursor"
    );
}

#[test]
fn last_page_has_no_next_cursor() {
    let index = test_index();
    let specs = [AggregateSpecWire {
        field: Some("extension".to_owned()),
        top: Some(10),
        ..spec("terms")
    }];
    // Page size of 100 is bigger than our 4 extensions → single page.
    let (results, _matched) = IndexManager::run_aggregations(
        &index,
        &specs,
        Vec::new(),
        None,
        Some(100),
        None,
        &[],
        &AggregateFilter::default(),
    );
    let terms = results.iter().find(|r| r.kind == "buckets").unwrap();
    assert!(
        terms.next_cursor.is_none(),
        "page_size larger than total buckets should produce no next_cursor"
    );
}
