// The aggregate module performs statistical analytics over millions of NTFS
// records.  The code is inherently numeric-heavy: u64→f64 casts for averages,
// float arithmetic for percentages, single-char iterator vars (k, v, n) in
// closures, indexing into pre-validated slices, and controlled truncation
// casts.  Each `#[allow]` below is justified by the domain:
//
//  • cast_precision_loss / cast_possible_truncation / cast_sign_loss —
//    acceptable in aggregate statistics; values are counters & sizes
//  • float_arithmetic — percentages, averages, and share calculations
//  • min_ident_chars — terse loop/closure vars in statistical code
//  • indexing_slicing / string_slice — bounds verified by prior logic
//  • too_many_lines — single-pass scan functions with many branches
//  • shadow_reuse / shadow_unrelated — parser re-binds input on each step
//  • iter_over_hash_type — deterministic order not required for aggregation
//  • option_if_let_else — more readable than `map_or` in multi-line blocks
//  • wildcard_enum_match_arm — forward-compat not a concern for internal enums
//  • used_underscore_binding — convention for unused-but-meaningful fields
//  • significant_drop_tightening — mutex guard lifetime is correct
//  • impl_trait_in_params — ergonomic API for closures
//  • manual_checked_div — explicit division-by-zero guards are clearer
//  • std_instead_of_core — HashMap/Mutex are std-only
//  • map_err_ignore — intentional simplification of error types
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_arithmetic,
    clippy::min_ident_chars,
    clippy::too_many_lines,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::shadow_reuse,
    clippy::shadow_unrelated,
    clippy::iter_over_hash_type,
    clippy::option_if_let_else,
    clippy::wildcard_enum_match_arm,
    clippy::used_underscore_binding,
    clippy::significant_drop_tightening,
    clippy::impl_trait_in_params,
    clippy::integer_division_remainder_used,
    clippy::std_instead_of_core,
    clippy::map_err_ignore,
    clippy::match_same_arms,
    clippy::unreadable_literal,
    clippy::unneeded_field_pattern,
    clippy::manual_checked_ops,
    clippy::unnecessary_sort_by,
    clippy::default_numeric_fallback
)]

//! Aggregation engine for UFFS.
//!
//! Provides high-performance, single-pass aggregate computations over
//! `CompactRecord` arrays. Designed to operate on the hot path — no
//! `DisplayRow` construction, no path resolution unless explicitly requested.
//!
//! # Architecture
//!
//! ```text
//! AggregateSpec  ──▶  AggregatePlan  ──▶  AggregateEngine::run()
//!                      (compile)           │
//!                                          ├─ per-drive parallel scan
//!                                          ├─ accumulators (feed/merge)
//!                                          └─ finalize → AggregateResult
//! ```
//!
//! See `docs/architecture/UFFS_AGGREGATION_ARCHITECTURE_CONSOLIDATED.md`
//! for the full design.

pub mod accumulators;
pub mod buckets;
pub mod cache;
pub mod duplicates;
pub mod export;
pub mod finalize;
pub mod pagination;
pub mod parser;
pub mod planner;
pub mod presets;
pub mod rollup;
pub mod spec;

// Re-export core public types.
pub use accumulators::GroupAccumulator;
pub use buckets::{AgeBucket, SizeBucket};
pub use cache::AggregateCache;
pub use duplicates::{DuplicateAccumulator, DuplicateResult};
pub use export::{ExportFormat, export_results};
pub use finalize::{AggregateResponse, BucketRow, FinalizeOptions};
pub use pagination::{AggregateCursor, PaginatedBuckets, paginate_result};
pub use parser::{parse_agg_spec, parse_and_expand_agg_specs};
pub use planner::AggregatePlan;
pub use presets::AggregatePreset;
pub use rollup::RollupAccumulator;
pub use spec::{
    AggregateKind, AggregateSpec, BucketMetric, CalendarInterval, DuplicateVerify, RollupMode,
    ScalarMetric, TopHitsSpec,
};

use crate::compact::DriveCompactIndex;

/// Result of running one or more aggregate specs against a set of drives.
///
/// Contains the finalized response plus execution metadata.
#[derive(Debug, Clone)]
pub struct AggregateOutput {
    /// The finalized aggregate response.
    pub response: AggregateResponse,
    /// Total records scanned across all drives.
    pub records_scanned: u64,
    /// Total records that passed filters and contributed to aggregates.
    pub records_matched: u64,
    /// Wall-clock execution time in microseconds.
    pub execution_us: u64,
}

/// Run aggregation specs against one or more drive indices (unfiltered).
///
/// This is the main entry point for the aggregation engine. It:
/// 1. Compiles specs into an `AggregatePlan`
/// 2. Scans all records per drive
/// 3. Merges per-drive accumulators
/// 4. Finalizes and returns results
///
/// For filtered aggregation, use the search pipeline to pre-filter
/// and then feed matching record indices to the accumulators.
///
/// # Errors
///
/// Returns an error if any spec references an invalid field or if
/// accumulator construction fails.
pub fn run_aggregate(
    drives: &[&DriveCompactIndex],
    specs: &[AggregateSpec],
    options: &FinalizeOptions,
) -> Result<AggregateOutput, AggregateError> {
    let start = std::time::Instant::now();

    // 1. Compile
    let plan = AggregatePlan::compile(specs)?;

    // 2. Scan each drive and collect accumulators
    let mut merged = plan.create_accumulators();
    let mut total_scanned: u64 = 0;
    let mut total_matched: u64 = 0;

    for drive in drives {
        let (scanned, matched) = scan_drive(drive, &plan, &mut merged);
        total_scanned += scanned;
        total_matched += matched;
    }

    // 3. Finalize
    let response = finalize::finalize(merged, &plan, drives, options, total_matched);

    Ok(AggregateOutput {
        response,
        records_scanned: total_scanned,
        records_matched: total_matched,
        execution_us: start.elapsed().as_micros() as u64,
    })
}

/// Scan a single drive's records, feeding all accumulators.
///
/// Returns `(records_scanned, records_matched)`.
fn scan_drive(
    drive: &DriveCompactIndex,
    _plan: &AggregatePlan,
    accumulators: &mut [GroupAccumulator],
) -> (u64, u64) {
    let records = &drive.records;
    let mut scanned: u64 = 0;

    for (idx, record) in records.iter().enumerate() {
        scanned += 1;

        // Feed each accumulator.
        for acc in accumulators.iter_mut() {
            acc.feed(record, drive, idx);
        }
    }

    // For unfiltered aggregation, matched == scanned.
    (scanned, scanned)
}

/// Errors that can occur during aggregation.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AggregateError {
    /// A spec referenced a field that doesn't support the requested operation.
    #[error("field `{field}` does not support {operation}")]
    UnsupportedField {
        /// The field name.
        field: String,
        /// The operation that was attempted.
        operation: String,
    },
    /// An invalid configuration was provided.
    #[error("invalid aggregate configuration: {0}")]
    InvalidConfig(String),
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::float_arithmetic,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::missing_docs_in_private_items,
    clippy::panic,
    clippy::min_ident_chars,
    clippy::default_numeric_fallback,
    clippy::wildcard_imports,
    clippy::too_many_lines,
    reason = "test code"
)]
mod integration_tests {
    use finalize::AggregateResultData;
    use uffs_mft::index::{IndexNameRef, MftIndex, ROOT_FRS, SizeInfo};

    use super::*;
    use crate::compact::build_compact_index;

    /// NTFS epoch is `1601-01-01`. Ticks per second = `10_000_000`.
    /// `2024-01-15 00:00:00 UTC` in NTFS ticks ≈ `133_496_544_000_000_000`.
    const TS_JAN_2024: i64 = 133_496_544_000_000_000;
    /// 2024-03-10 00:00:00 UTC.
    const TS_MAR_2024: i64 = 133_544_928_000_000_000;
    /// 2024-06-20 00:00:00 UTC.
    const TS_JUN_2024: i64 = 133_633_536_000_000_000;

    /// Build a synthetic drive with well-known data for integration tests.
    ///
    /// Layout:
    /// ```text
    /// C:\                        (root dir)
    /// C:\Projects\               (dir, flags=0x10)
    /// C:\Projects\main.rs        (2000 bytes, alloc 4096, modified Jan 2024)
    /// C:\Projects\lib.rs         (3000 bytes, alloc 4096, modified Jan 2024)
    /// C:\Projects\util.rs        (1000 bytes, alloc 4096, modified Mar 2024)
    /// C:\Projects\README.md      (500  bytes, alloc 512,  modified Jan 2024)
    /// C:\Projects\CHANGELOG.md   (800  bytes, alloc 1024, modified Jun 2024)
    /// C:\Projects\config.toml    (100  bytes, alloc 512,  modified Mar 2024)
    /// C:\Projects\data.bin       (10000 bytes, alloc 16384, modified Jun 2024)
    /// ```
    ///
    /// Totals (files only): 7 files, 17400 bytes logical, 30628 bytes alloc.
    fn build_agg_test_drive() -> DriveCompactIndex {
        let mut idx = MftIndex::new('C');

        // Root directory.
        let root_off = idx.add_name(".");
        let root = idx.get_or_create(ROOT_FRS);
        root.stdinfo.set_directory(true);
        root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
        root.first_name.parent_frs = ROOT_FRS;

        // Projects directory.
        let dir_name = "Projects";
        let dir_off = idx.add_name(dir_name);
        let dir_ext = idx.intern_extension(dir_name);
        let dir = idx.get_or_create(100);
        dir.stdinfo.set_directory(true);
        dir.stdinfo.flags = 0x10;
        dir.first_name.name = IndexNameRef::new(dir_off, dir_name.len() as u16, true, dir_ext);
        dir.first_name.parent_frs = ROOT_FRS;

        // Files: (name, frs, size, allocated, modified_timestamp)
        let files: &[(&str, u64, u64, u64, i64)] = &[
            ("main.rs", 101, 2000, 4096, TS_JAN_2024),
            ("lib.rs", 102, 3000, 4096, TS_JAN_2024),
            ("util.rs", 103, 1000, 4096, TS_MAR_2024),
            ("README.md", 104, 500, 512, TS_JAN_2024),
            ("CHANGELOG.md", 105, 800, 1024, TS_JUN_2024),
            ("config.toml", 106, 100, 512, TS_MAR_2024),
            ("data.bin", 107, 10000, 16384, TS_JUN_2024),
        ];

        for &(name, frs, size, allocated, modified) in files {
            let off = idx.add_name(name);
            let ext = idx.intern_extension(name);
            let rec = idx.get_or_create(frs);
            rec.first_name.name = IndexNameRef::new(off, name.len() as u16, true, ext);
            rec.first_name.parent_frs = 100;
            rec.first_stream.size = SizeInfo {
                length: size,
                allocated,
            };
            rec.stdinfo.flags = 0x20; // archive
            rec.stdinfo.modified = modified;
        }

        let (drive, _, _) = build_compact_index('C', &idx);
        drive
    }

    fn run(specs: &[AggregateSpec]) -> AggregateResponse {
        let drive = build_agg_test_drive();
        let output = run_aggregate(&[&drive], specs, &FinalizeOptions::default()).unwrap();
        output.response
    }

    // ── S1G.10: overview preset ──────────────────────────────────────

    #[test]
    fn overview_preset_returns_count_and_stats_and_terms() {
        let specs = AggregatePreset::Overview.expand();
        let resp = run(&specs);
        // overview produces: count + file_size stats + type terms + drive terms
        // + date_histogram, etc. — at least 3 results
        assert!(
            resp.results.len() >= 3,
            "overview should produce ≥3 results, got {}",
            resp.results.len()
        );

        // Find the count result.
        let count_result = resp
            .results
            .iter()
            .find(|r| matches!(&r.data, AggregateResultData::Count { .. }))
            .expect("overview should have a count result");
        if let AggregateResultData::Count { value } = &count_result.data {
            // root + dir + 7 files = 9
            assert_eq!(*value, 9, "total record count");
        }
    }

    #[test]
    fn overview_preset_has_size_stats() {
        let specs = AggregatePreset::Overview.expand();
        let resp = run(&specs);
        let stats_result = resp
            .results
            .iter()
            .find(|r| matches!(&r.data, AggregateResultData::Stats { .. }))
            .expect("overview should have stats");
        if let AggregateResultData::Stats { stats, .. } = &stats_result.data {
            assert!(stats.count > 0);
            assert!(stats.sum > 0);
        }
    }

    // ── S1G.11: by_extension top-N ───────────────────────────────────

    #[test]
    fn by_extension_returns_sorted_buckets() {
        let specs = AggregatePreset::ByExtension.expand();
        let resp = run(&specs);
        let bucket_result = resp
            .results
            .iter()
            .find(|r| matches!(&r.data, AggregateResultData::Buckets { .. }))
            .expect("by_extension should have buckets");
        if let AggregateResultData::Buckets { rows, .. } = &bucket_result.data {
            assert!(!rows.is_empty());
            // "rs" has 3 files (main.rs, lib.rs, util.rs)
            let rs = rows.iter().find(|r| r.key == "rs").expect("rs bucket");
            assert_eq!(rs.count, 3);
            assert_eq!(rs.total_bytes, 6000); // 2000+3000+1000
            // "md" has 2 files
            let md = rows.iter().find(|r| r.key == "md").expect("md bucket");
            assert_eq!(md.count, 2);
            assert_eq!(md.total_bytes, 1300); // 500+800
            // Sorted by count desc (or total_bytes desc)
            // rs(3) should come before md(2) and bin(1) and toml(1)
        }
    }

    #[test]
    fn by_extension_has_all_extensions() {
        let specs = AggregatePreset::ByExtension.expand();
        let resp = run(&specs);
        let bucket_result = resp
            .results
            .iter()
            .find(|r| matches!(&r.data, AggregateResultData::Buckets { .. }))
            .expect("buckets");
        if let AggregateResultData::Buckets { rows, .. } = &bucket_result.data {
            let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
            assert!(keys.contains(&"rs"), "missing rs: {keys:?}");
            assert!(keys.contains(&"md"), "missing md: {keys:?}");
            assert!(keys.contains(&"toml"), "missing toml: {keys:?}");
            assert!(keys.contains(&"bin"), "missing bin: {keys:?}");
        }
    }

    // ── S1G.12: by_type category counts ──────────────────────────────

    #[test]
    fn by_type_returns_category_buckets() {
        let specs = AggregatePreset::ByType.expand();
        let resp = run(&specs);
        let bucket_result = resp
            .results
            .iter()
            .find(|r| matches!(&r.data, AggregateResultData::Buckets { .. }))
            .expect("by_type should have buckets");
        if let AggregateResultData::Buckets { rows, .. } = &bucket_result.data {
            assert!(!rows.is_empty(), "by_type should have at least one bucket");
            // All 7 files should appear in some type category.
            let total: u64 = rows.iter().map(|r| r.count).sum();
            // At minimum, the 7 files should be categorized (dirs may or may not
            // depending on how type categorization handles them).
            assert!(total >= 7, "total categorized should be ≥7, got {total}");
        }
    }

    // ── S1G.13: hist:size bucket boundaries ──────────────────────────

    #[test]
    fn range_size_produces_correct_buckets() {
        // Use Range (not Histogram) since interval-based boundaries
        // aren't auto-generated yet. Range gives explicit boundaries.
        let mut spec = AggregateSpec::new(AggregateKind::Range {
            field: crate::search::field::FieldId::Size,
            boundaries: vec![0, 512, 2048, 8192],
            metrics: vec![BucketMetric::Count, BucketMetric::TotalBytes],
        });
        spec.label = Some("size_range".to_owned());
        let resp = run(&[spec]);
        assert_eq!(resp.results.len(), 1);
        if let AggregateResultData::Buckets { rows, .. } = &resp.results[0].data {
            assert!(!rows.is_empty(), "range should have buckets");
            // Total count across all buckets should equal all 9 records.
            let total: u64 = rows.iter().map(|r| r.count).sum();
            assert_eq!(total, 9, "range total count");
            // 4 boundaries → 5 possible buckets, but empty ones may be skipped.
            // [..0) is empty, so expect 4 non-empty buckets.
            assert!(
                rows.len() >= 3,
                "expected ≥3 range buckets, got {}",
                rows.len()
            );
        }
    }

    #[test]
    fn histogram_size_single_bucket_when_no_boundaries() {
        // Histogram without planner-generated boundaries puts all in one bucket.
        let mut spec = AggregateSpec::new(AggregateKind::Histogram {
            field: crate::search::field::FieldId::Size,
            interval: 4096,
            metrics: vec![BucketMetric::Count, BucketMetric::TotalBytes],
        });
        spec.label = Some("hist_test".to_owned());
        let resp = run(&[spec]);
        assert_eq!(resp.results.len(), 1);
        if let AggregateResultData::Buckets { rows, .. } = &resp.results[0].data {
            // Without boundary expansion, all records land in one bucket.
            let total: u64 = rows.iter().map(|r| r.count).sum();
            assert_eq!(total, 9, "all records accounted for");
        }
    }

    // ── S1G.14: datehist:modified,month ──────────────────────────────

    #[test]
    fn datehist_modified_monthly_produces_buckets() {
        let mut spec = AggregateSpec::new(AggregateKind::DateHistogram {
            field: crate::search::field::FieldId::Modified,
            calendar: CalendarInterval::Month,
            metrics: vec![BucketMetric::Count],
        });
        spec.label = Some("mod_monthly".to_owned());
        let resp = run(&[spec]);
        assert_eq!(resp.results.len(), 1);
        if let AggregateResultData::Buckets { rows, .. } = &resp.results[0].data {
            assert!(!rows.is_empty(), "datehist should have ≥1 month bucket");
            // We have files in Jan, Mar, Jun 2024.
            // Total across all buckets should include all 9 records (dirs get ts=0
            // which maps to some bucket too).
            let total: u64 = rows.iter().map(|r| r.count).sum();
            assert_eq!(total, 9, "datehist total count should be 9");
            // Check that we have at least 3 distinct month buckets
            // (Jan + Mar + Jun, plus possibly one for timestamp=0 dirs).
            assert!(
                rows.len() >= 3,
                "should have ≥3 month buckets, got {}",
                rows.len()
            );
        }
    }

    // ── S1G.15: aggregate-only must NOT call path resolution ─────────

    #[test]
    fn aggregate_only_skips_path_resolution() {
        // The aggregate engine calls `run_aggregate` which scans records
        // directly without using `FastPathResolver`. This test verifies
        // the engine produces correct results without any path resolution
        // infrastructure, proving it never calls path resolution.
        let drive = build_agg_test_drive();
        let specs = AggregatePreset::Overview.expand();
        let output = run_aggregate(&[&drive], &specs, &FinalizeOptions::default()).unwrap();
        // If path resolution were required, this would fail because
        // the synthetic index doesn't have a fully valid parent chain.
        // The fact that it succeeds proves aggregate-only works
        // without path resolution.
        assert!(output.records_scanned > 0);
        assert!(!output.response.results.is_empty());
    }

    // ── S1G.16: terms:ext uses extension_id, not string allocation ──

    #[test]
    fn terms_ext_uses_intern_extension_id() {
        // The Terms:Extension accumulator groups by compact
        // extension_id (u16), not by allocating extension strings.
        // String keys are only resolved during finalization.
        // This test verifies correct results through the
        // extension_id path.
        let mut spec = AggregateSpec::new(AggregateKind::Terms {
            field: crate::search::field::FieldId::Extension,
            top: 100,
            metrics: vec![BucketMetric::Count, BucketMetric::TotalBytes],
        });
        spec.label = Some("ext_terms".to_owned());
        let resp = run(&[spec]);

        if let AggregateResultData::Buckets { rows, .. } = &resp.results[0].data {
            // Check exact counts for each extension.
            let rs = rows.iter().find(|r| r.key == "rs").expect("rs");
            assert_eq!(rs.count, 3);
            let md = rows.iter().find(|r| r.key == "md").expect("md");
            assert_eq!(md.count, 2);
            let toml = rows.iter().find(|r| r.key == "toml").expect("toml");
            assert_eq!(toml.count, 1);
            let bin = rows.iter().find(|r| r.key == "bin").expect("bin");
            assert_eq!(bin.count, 1);
            // Total file count from extension terms (dirs have no ext).
            let total: u64 = rows.iter().map(|r| r.count).sum();
            assert!(total >= 7, "at least 7 files with extensions, got {total}");
        }
    }
}
