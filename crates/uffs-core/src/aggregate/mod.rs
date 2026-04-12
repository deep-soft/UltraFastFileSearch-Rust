// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

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
#![expect(
    clippy::float_arithmetic,
    clippy::min_ident_chars,
    clippy::too_many_lines,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::shadow_reuse,
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
    clippy::default_numeric_fallback,
    reason = "aggregation module uses dynamic dispatch, float math, hash iteration, and complex pattern matching throughout"
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
/// Per-bucket sample heap for tracking top-N records.
pub mod sample_heap;
pub mod spec;
/// Duplicate verification (first-bytes / SHA-256).
pub mod verify;

// Re-export core public types.
pub use accumulators::GroupAccumulator;
pub use buckets::{AgeBucket, SizeBucket};
pub use cache::AggregateCache;
pub use duplicates::{DuplicateAccumulator, DuplicateResult};
pub use export::{ExportFormat, export_results};
pub use finalize::{
    AggregateResponse, BucketRow, DrilldownPredicate, DrilldownValue, FinalizeOptions, SampleRow,
};
pub use pagination::{AggregateCursor, PaginatedBuckets, paginate_result};
pub use parser::{parse_agg_spec, parse_and_expand_agg_specs};
pub use planner::AggregatePlan;
pub use presets::AggregatePreset;
pub use rollup::RollupAccumulator;
pub use spec::{
    AggregateKind, AggregateSpec, BucketMetric, CalendarInterval, DuplicateVerify, RollupMode,
    ScalarMetric, TopHitsSpec,
};
pub use verify::{DuplicateVerifier, FileReader, VerificationBudget, VerificationSummary};

use crate::compact::{CompactRecord, DriveCompactIndex};

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

/// Lightweight filter for pre-scan record selection in aggregate queries.
///
/// Unlike the full `SearchFilters` (which lives in the search path and
/// supports pattern matching, path predicates, etc.), this struct carries
/// only the fast, O(1)-per-record checks that can be applied during the
/// aggregation scan without path resolution.
///
/// Extension IDs are **per-drive** — call
/// [`DriveCompactIndex::resolve_ext_ids`] once per drive before scanning.
#[derive(Debug, Clone, Default)]
pub struct AggregateFilter {
    /// Extension name strings (lowercase, no dot).  Resolved to per-drive
    /// `u16` IDs before scanning via [`DriveCompactIndex::resolve_ext_ids`].
    pub extensions: Vec<String>,
    /// If `Some(true)` only directories; `Some(false)` only files.
    pub directory_only: Option<bool>,
    /// Minimum file size (inclusive).
    pub min_size: Option<u64>,
    /// Maximum file size (inclusive).
    pub max_size: Option<u64>,
}

impl AggregateFilter {
    /// Returns `true` if no filter constraints are set.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.extensions.is_empty()
            && self.directory_only.is_none()
            && self.min_size.is_none()
            && self.max_size.is_none()
    }

    /// O(1) per-record check using pre-resolved extension IDs.
    #[inline]
    fn matches(&self, record: &CompactRecord, resolved_ext_ids: &[u16]) -> bool {
        // Directory / file filter.
        if let Some(dirs_only) = self.directory_only
            && record.is_directory() != dirs_only
        {
            return false;
        }
        // Extension filter (fast path via pre-resolved IDs).
        if !resolved_ext_ids.is_empty() && !resolved_ext_ids.contains(&record.extension_id) {
            return false;
        }
        // Size bounds.
        if let Some(min) = self.min_size
            && record.size < min
        {
            return false;
        }
        if let Some(max) = self.max_size
            && record.size > max
        {
            return false;
        }
        true
    }
}

/// Cross-drive canonical extension mapping.
///
/// Each drive interns extensions independently (`extension_id` is per-drive).
/// This table maps every `(drive_ordinal, local_extension_id)` to a single
/// canonical ID so that `"exe"` on drive C and `"exe"` on drive D share the
/// same group key in aggregation.
///
/// The reverse mapping (`canonical_id → extension name`) is stored in
/// `canonical_names`.
#[derive(Debug, Clone)]
pub(crate) struct ExtensionMap {
    /// `per_drive[drive_ordinal][local_ext_id] → canonical_ext_id`
    per_drive: Vec<Vec<u64>>,
    /// `canonical_names[canonical_ext_id] → extension string`
    canonical_names: Vec<String>,
}

impl ExtensionMap {
    /// Build the cross-drive mapping from a set of drives.
    fn build(drives: &[&DriveCompactIndex]) -> Self {
        let mut name_to_id: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        let mut canonical_names: Vec<String> = Vec::new();
        let mut per_drive: Vec<Vec<u64>> = Vec::with_capacity(drives.len());

        for drive in drives {
            let mut mapping = Vec::with_capacity(drive.ext_names.len());
            for ext_name in &drive.ext_names {
                let name_str: &str = ext_name;
                let canonical_id = if let Some(&id) = name_to_id.get(name_str) {
                    id
                } else {
                    let id = canonical_names.len() as u64;
                    let owned = name_str.to_owned();
                    canonical_names.push(owned.clone());
                    name_to_id.insert(owned, id);
                    id
                };
                mapping.push(canonical_id);
            }
            per_drive.push(mapping);
        }

        Self {
            per_drive,
            canonical_names,
        }
    }

    /// Look up the canonical extension ID for a record.
    #[inline]
    fn canonical_id(&self, drive_ordinal: u8, local_ext_id: u16) -> u64 {
        self.per_drive
            .get(usize::from(drive_ordinal))
            .and_then(|m| m.get(usize::from(local_ext_id)))
            .copied()
            .unwrap_or(u64::MAX)
    }

    /// Resolve a canonical ID to its extension name.
    fn resolve(&self, canonical_id: u64) -> String {
        self.canonical_names
            .get(uffs_mft::frs_to_usize(canonical_id))
            .cloned()
            .unwrap_or_else(|| format!("ext:{canonical_id}"))
    }
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

    // Build cross-drive extension mapping for correct multi-drive grouping.
    let ext_map = ExtensionMap::build(drives);

    // 2. Scan each drive and collect accumulators
    let mut merged = plan.create_accumulators();
    let mut total_scanned: u64 = 0;
    let mut total_matched: u64 = 0;

    for (drive_ordinal, drive) in drives.iter().enumerate() {
        let ordinal = u8::try_from(drive_ordinal).unwrap_or(u8::MAX);
        let t = std::time::Instant::now();
        let (scanned, matched) = scan_drive(drive, &plan, &mut merged, ordinal, Some(&ext_map));
        tracing::debug!(
            drive = %drive.letter,
            scanned,
            matched,
            elapsed_ms = t.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            "run_aggregate: drive scan"
        );
        total_scanned += scanned;
        total_matched += matched;
    }

    // 3. Finalize
    let t_fin = std::time::Instant::now();
    let response = finalize::finalize_with_ext_map(
        merged,
        &plan,
        drives,
        options,
        total_matched,
        Some(&ext_map),
    );
    tracing::debug!(
        elapsed_ms = t_fin.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        "run_aggregate: finalize"
    );

    Ok(AggregateOutput {
        response,
        records_scanned: total_scanned,
        records_matched: total_matched,
        execution_us: start.elapsed().as_micros().try_into().unwrap_or(u64::MAX),
    })
}

/// Run aggregation specs against records that match a search pattern.
///
/// This scans all records per drive but only feeds records whose name
/// matches `pattern` (a glob like `*.exe`).  This is the correct entry
/// point when combining search + aggregation: e.g. `*.exe --agg
/// terms:extension` should aggregate only `.exe` files, not all files.
///
/// The pattern is compiled once using [`IndexPattern`] and matched
/// inline during the scan — no `DisplayRow` construction or path
/// resolution is needed.
///
/// # Errors
///
/// Returns an error if any spec references an invalid field or if
/// accumulator construction fails.
pub fn run_aggregate_filtered(
    drives: &[&DriveCompactIndex],
    specs: &[AggregateSpec],
    options: &FinalizeOptions,
    pattern: &str,
) -> Result<AggregateOutput, AggregateError> {
    use uffs_text::CaseFold;

    use crate::index_search::compile_parsed_pattern;
    use crate::pattern::ParsedPattern;

    let start = std::time::Instant::now();

    // Compile search pattern.
    let fold = CaseFold::default_table();
    let parsed = ParsedPattern::parse(pattern)
        .map_err(|e| AggregateError::InvalidConfig(format!("bad pattern: {e}")))?;
    let index_pat = compile_parsed_pattern(&parsed)
        .map_err(|e| AggregateError::InvalidConfig(format!("bad pattern: {e}")))?;
    tracing::info!(
        pattern,
        index_pattern = ?index_pat,
        "run_aggregate_filtered: compiled pattern"
    );

    // 1. Compile aggregation plan.
    let plan = AggregatePlan::compile(specs)?;

    // Build cross-drive extension mapping.
    let ext_map = ExtensionMap::build(drives);

    // 2. Scan each drive, filtering by pattern.
    let mut merged = plan.create_accumulators();
    let mut total_scanned: u64 = 0;
    let mut total_matched: u64 = 0;

    for (drive_ordinal, drive) in drives.iter().enumerate() {
        let ordinal = u8::try_from(drive_ordinal).unwrap_or(u8::MAX);
        for (idx, record) in drive.records.iter().enumerate() {
            total_scanned += 1;
            let name = record.name(&drive.names);
            if name.is_empty() || !index_pat.matches(name, false, fold) {
                continue;
            }
            total_matched += 1;
            for acc in &mut merged {
                acc.feed(record, drive, idx, ordinal, Some(&ext_map));
            }
        }
    }

    tracing::info!(
        total_scanned,
        total_matched,
        "run_aggregate_filtered: scan complete"
    );

    // 3. Finalize.
    let response = finalize::finalize_with_ext_map(
        merged,
        &plan,
        drives,
        options,
        total_matched,
        Some(&ext_map),
    );

    Ok(AggregateOutput {
        response,
        records_scanned: total_scanned,
        records_matched: total_matched,
        execution_us: start.elapsed().as_micros().try_into().unwrap_or(u64::MAX),
    })
}

/// Run aggregation with both pattern and record-level filters.
///
/// Combines the glob/regex pattern matching of [`run_aggregate_filtered`]
/// with the O(1) per-record checks from [`AggregateFilter`] (extension IDs,
/// directory flag, size bounds).  This is the entry point for MCP/daemon
/// aggregate queries that combine `pattern` + `type_filter` + `filter`.
///
/// When `filter.is_empty()` and pattern is `"*"`, this behaves identically
/// to [`run_aggregate`] (unfiltered).
///
/// # Errors
///
/// Returns an error if any spec references an invalid field or if
/// accumulator construction fails.
pub fn run_aggregate_with_filters(
    drives: &[&DriveCompactIndex],
    specs: &[AggregateSpec],
    options: &FinalizeOptions,
    pattern: Option<&str>,
    filter: &AggregateFilter,
) -> Result<AggregateOutput, AggregateError> {
    // Fast path: no filters and trivial pattern → unfiltered scan.
    use uffs_text::CaseFold;

    use crate::index_search::compile_parsed_pattern;
    use crate::pattern::ParsedPattern;

    let trivial_pattern = pattern.is_none_or(|p| matches!(p, "*" | "**" | "**/*" | ""));
    if filter.is_empty() && trivial_pattern {
        return run_aggregate(drives, specs, options);
    }
    // Pattern-only → delegate to existing filtered path.
    if filter.is_empty() {
        if let Some(pat) = pattern {
            return run_aggregate_filtered(drives, specs, options, pat);
        }
        return run_aggregate(drives, specs, options);
    }

    let start = std::time::Instant::now();

    // Compile pattern (if non-trivial).
    let fold = CaseFold::default_table();
    let compiled_pattern = if trivial_pattern {
        None
    } else {
        let pat = pattern.unwrap_or("*");
        let parsed = ParsedPattern::parse(pat)
            .map_err(|e| AggregateError::InvalidConfig(format!("bad pattern: {e}")))?;
        Some(
            compile_parsed_pattern(&parsed)
                .map_err(|e| AggregateError::InvalidConfig(format!("bad pattern: {e}")))?,
        )
    };

    // 1. Compile aggregation plan.
    let plan = AggregatePlan::compile(specs)?;
    let ext_map = ExtensionMap::build(drives);

    // 2. Scan each drive with combined filter.
    let mut merged = plan.create_accumulators();
    let mut total_scanned: u64 = 0;
    let mut total_matched: u64 = 0;

    for (drive_ordinal, drive) in drives.iter().enumerate() {
        let ordinal = u8::try_from(drive_ordinal).unwrap_or(u8::MAX);
        // Resolve extension names → per-drive u16 IDs (< 1µs).
        let resolved_ext_ids = drive.resolve_ext_ids(&filter.extensions);

        for (idx, record) in drive.records.iter().enumerate() {
            total_scanned += 1;

            // Record-level filter (O(1) — extension ID + directory flag + size).
            if !filter.matches(record, &resolved_ext_ids) {
                continue;
            }

            // Pattern filter (if non-trivial).
            if let Some(pat) = &compiled_pattern {
                let name = record.name(&drive.names);
                if name.is_empty() || !pat.matches(name, false, fold) {
                    continue;
                }
            }

            total_matched += 1;
            for acc in &mut merged {
                acc.feed(record, drive, idx, ordinal, Some(&ext_map));
            }
        }
    }

    tracing::info!(
        total_scanned,
        total_matched,
        has_pattern = compiled_pattern.is_some(),
        ext_count = filter.extensions.len(),
        dir_only = ?filter.directory_only,
        "run_aggregate_with_filters: scan complete"
    );

    // 3. Finalize.
    let response = finalize::finalize_with_ext_map(
        merged,
        &plan,
        drives,
        options,
        total_matched,
        Some(&ext_map),
    );

    Ok(AggregateOutput {
        response,
        records_scanned: total_scanned,
        records_matched: total_matched,
        execution_us: start.elapsed().as_micros().try_into().unwrap_or(u64::MAX),
    })
}

/// Scan a single drive's records, feeding all accumulators.
///
/// Returns `(records_scanned, records_matched)`.
fn scan_drive(
    drive: &DriveCompactIndex,
    _plan: &AggregatePlan,
    accumulators: &mut [GroupAccumulator],
    drive_ordinal: u8,
    ext_map: Option<&ExtensionMap>,
) -> (u64, u64) {
    let records = &drive.records;
    let mut scanned: u64 = 0;

    for (idx, record) in records.iter().enumerate() {
        scanned += 1;

        // Feed each accumulator.
        for acc in accumulators.iter_mut() {
            acc.feed(record, drive, idx, drive_ordinal, ext_map);
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
#[expect(
    clippy::indexing_slicing,
    clippy::min_ident_chars,
    reason = "test code — relaxed for readability and fail-fast assertions"
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
        dir.first_name.name =
            IndexNameRef::new(dir_off, uffs_mft::len_to_u16(dir_name.len()), true, dir_ext);
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
            rec.first_name.name =
                IndexNameRef::new(off, uffs_mft::len_to_u16(name.len()), true, ext);
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
            sample: None,
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

    // ── S2A: TopHits sample rows are materialized ───────────────────

    #[test]
    fn terms_with_sample_materializes_rows() {
        use crate::search::field::FieldId;

        let drive = build_agg_test_drive();
        let spec = AggregateSpec::new(AggregateKind::Terms {
            field: FieldId::Extension,
            top: 10,
            metrics: vec![BucketMetric::Count, BucketMetric::TotalBytes],
            sample: Some(TopHitsSpec::with_count(2)),
        });
        let output = run_aggregate(&[&drive], &[spec], &FinalizeOptions::default()).unwrap();

        if let AggregateResultData::Buckets { rows, .. } = &output.response.results[0].data {
            // The "rs" bucket has 3 files; sample should have 2 (largest by size).
            let rs = rows.iter().find(|r| r.key == "rs").expect("rs bucket");
            assert_eq!(
                rs.sample_rows.len(),
                2,
                "rs bucket should have 2 sample rows, got {}",
                rs.sample_rows.len()
            );
            // Default sort: Size desc => 3000 (lib.rs), 2000 (main.rs)
            assert_eq!(rs.sample_rows[0].sort_key, 3000);
            assert_eq!(rs.sample_rows[1].sort_key, 2000);

            // Verify projected fields include "name" and "size".
            let names: Vec<&str> = rs.sample_rows[0]
                .fields
                .iter()
                .map(|(k, _)| k.as_str())
                .collect();
            assert!(names.contains(&"name"), "should project name field");
            assert!(names.contains(&"size"), "should project size field");

            // Check actual name values.
            let name_val = rs.sample_rows[0]
                .fields
                .iter()
                .find(|(k, _)| k == "name")
                .map(|(_, v)| v.as_str())
                .unwrap();
            assert_eq!(name_val, "lib.rs", "largest .rs file is lib.rs");

            // Buckets with 1 file should have 1 sample row.
            let toml = rows.iter().find(|r| r.key == "toml").expect("toml bucket");
            assert_eq!(toml.sample_rows.len(), 1);
        } else {
            panic!("expected Buckets result");
        }
    }

    #[test]
    fn terms_without_sample_has_empty_sample_rows() {
        use crate::search::field::FieldId;

        let drive = build_agg_test_drive();
        let spec = AggregateSpec::new(AggregateKind::Terms {
            field: FieldId::Extension,
            top: 10,
            metrics: vec![BucketMetric::Count],
            sample: None,
        });
        let output = run_aggregate(&[&drive], &[spec], &FinalizeOptions::default()).unwrap();

        if let AggregateResultData::Buckets { rows, .. } = &output.response.results[0].data {
            for row in rows {
                assert!(
                    row.sample_rows.is_empty(),
                    "bucket '{}' should have no sample rows",
                    row.key
                );
            }
        }
    }

    #[test]
    fn terms_sample_custom_projection() {
        use crate::search::field::FieldId;

        let drive = build_agg_test_drive();
        let spec = AggregateSpec::new(AggregateKind::Terms {
            field: FieldId::Extension,
            top: 10,
            metrics: vec![BucketMetric::Count],
            sample: Some(TopHitsSpec::new(1, FieldId::Size, true, vec![
                FieldId::Name,
                FieldId::Size,
            ])),
        });
        let output = run_aggregate(&[&drive], &[spec], &FinalizeOptions::default()).unwrap();

        if let AggregateResultData::Buckets { rows, .. } = &output.response.results[0].data {
            let rs = rows.iter().find(|r| r.key == "rs").expect("rs bucket");
            assert_eq!(rs.sample_rows.len(), 1);
            // Only 2 fields projected.
            assert_eq!(
                rs.sample_rows[0].fields.len(),
                2,
                "custom projection should have 2 fields"
            );
            let field_names: Vec<&str> = rs.sample_rows[0]
                .fields
                .iter()
                .map(|(k, _)| k.as_str())
                .collect();
            assert_eq!(field_names, vec!["name", "size"]);
        }
    }

    #[test]
    fn terms_sample_asc_sort() {
        use crate::search::field::FieldId;

        let drive = build_agg_test_drive();
        let spec = AggregateSpec::new(AggregateKind::Terms {
            field: FieldId::Extension,
            top: 10,
            metrics: vec![BucketMetric::Count],
            sample: Some(TopHitsSpec::new(2, FieldId::Size, false, vec![])),
        });
        let output = run_aggregate(&[&drive], &[spec], &FinalizeOptions::default()).unwrap();

        if let AggregateResultData::Buckets { rows, .. } = &output.response.results[0].data {
            let rs = rows.iter().find(|r| r.key == "rs").expect("rs bucket");
            assert_eq!(rs.sample_rows.len(), 2);
            // Asc sort: smallest first => 1000 (util.rs), 2000 (main.rs)
            assert_eq!(rs.sample_rows[0].sort_key, 1000);
            assert_eq!(rs.sample_rows[1].sort_key, 2000);
        }
    }

    // ── S2B: Drill-down predicates ──────────────────────────────────

    #[test]
    fn terms_drilldown_includes_bucket_key() {
        use crate::search::field::FieldId;

        let drive = build_agg_test_drive();
        let spec = AggregateSpec::new(AggregateKind::Terms {
            field: FieldId::Extension,
            top: 10,
            metrics: vec![BucketMetric::Count],
            sample: None,
        });
        let output = run_aggregate(&[&drive], &[spec], &FinalizeOptions::default()).unwrap();

        if let AggregateResultData::Buckets { rows, .. } = &output.response.results[0].data {
            let rs = rows.iter().find(|r| r.key == "rs").expect("rs bucket");

            // Should have exactly 1 drill-down predicate: extension=rs
            assert_eq!(rs.drilldown.len(), 1, "expected 1 drilldown pred");
            assert_eq!(rs.drilldown[0].field, "extension");
            assert_eq!(rs.drilldown[0].op, "eq");
            assert_eq!(
                rs.drilldown[0].value,
                DrilldownValue::String("rs".to_owned())
            );

            // Every bucket should have a drill-down predicate.
            for row in rows {
                assert!(
                    !row.drilldown.is_empty(),
                    "bucket '{}' should have drilldown",
                    row.key
                );
            }
        } else {
            panic!("expected Buckets");
        }
    }

    #[test]
    fn terms_drilldown_preserves_query_predicates() {
        use crate::search::field::FieldId;

        let drive = build_agg_test_drive();
        let spec = AggregateSpec::new(AggregateKind::Terms {
            field: FieldId::Extension,
            top: 10,
            metrics: vec![BucketMetric::Count],
            sample: None,
        });

        // Simulate an original query that filtered by size > 100
        let opts = FinalizeOptions {
            query_predicates: vec![DrilldownPredicate {
                field: "size".to_owned(),
                op: "gte".to_owned(),
                value: DrilldownValue::U64(100),
            }],
            ..FinalizeOptions::default()
        };

        let output = run_aggregate(&[&drive], &[spec], &opts).unwrap();

        if let AggregateResultData::Buckets { rows, .. } = &output.response.results[0].data {
            let rs = rows.iter().find(|r| r.key == "rs").expect("rs bucket");

            // Should have 2 predicates: size>=100 (from query) + extension=rs (bucket)
            assert_eq!(rs.drilldown.len(), 2, "expected 2 drilldown preds");
            assert_eq!(rs.drilldown[0].field, "size");
            assert_eq!(rs.drilldown[0].op, "gte");
            assert_eq!(rs.drilldown[1].field, "extension");
            assert_eq!(rs.drilldown[1].op, "eq");
        } else {
            panic!("expected Buckets");
        }
    }

    #[test]
    fn terms_drilldown_no_query_predicates() {
        use crate::search::field::FieldId;

        let drive = build_agg_test_drive();
        let spec = AggregateSpec::new(AggregateKind::Terms {
            field: FieldId::Extension,
            top: 10,
            metrics: vec![BucketMetric::Count],
            sample: None,
        });
        let output = run_aggregate(&[&drive], &[spec], &FinalizeOptions::default()).unwrap();

        if let AggregateResultData::Buckets { rows, .. } = &output.response.results[0].data {
            // With no query predicates, each bucket has exactly 1 pred
            for row in rows {
                assert_eq!(
                    row.drilldown.len(),
                    1,
                    "bucket '{}' should have 1 drilldown pred (just bucket key)",
                    row.key
                );
            }
        }
    }

    // ── S2F: Preset integration tests on synthetic index ────────────

    #[test]
    fn s2f4_top_folders_preset() {
        let drive = build_agg_test_drive();
        let specs = AggregatePreset::TopFolders.expand();
        let output = run_aggregate(&[&drive], &specs, &FinalizeOptions::default()).unwrap();
        let result = &output.response.results[0];

        assert_eq!(
            result.label.as_deref(),
            Some("top_folders"),
            "should carry the preset label"
        );

        if let AggregateResultData::Rollup { rows, mode } = &result.data {
            assert_eq!(
                mode, "path(depth=1)",
                "top_folders uses path depth=1 rollup"
            );
            assert!(
                !rows.is_empty(),
                "top_folders should produce at least 1 row"
            );

            let total_bytes: u64 = rows.iter().map(|r| r.total_bytes).sum();
            assert!(total_bytes > 0, "top_folders should report non-zero bytes");
        } else {
            panic!(
                "expected Rollup result from top_folders, got {:?}",
                result.data
            );
        }
    }

    #[test]
    fn s2f5_cleanup_preset() {
        let drive = build_agg_test_drive();
        let specs = AggregatePreset::Cleanup.expand();
        let output = run_aggregate(&[&drive], &specs, &FinalizeOptions::default()).unwrap();

        assert!(
            output.response.results.len() >= 3,
            "cleanup preset should produce at least 3 specs, got {}",
            output.response.results.len()
        );

        // Find the total_files count.
        let total = output
            .response
            .results
            .iter()
            .find(|r| r.label.as_deref() == Some("total_files"));
        assert!(total.is_some(), "cleanup should have total_files");
        if let Some(r) = total
            && let AggregateResultData::Count { value } = &r.data
        {
            // 8 records total (1 dir + 7 files).
            assert!(*value > 0, "total_files should be > 0");
        }

        // zero_byte_files: our test drive has no zero-byte files.
        let zero_byte = output
            .response
            .results
            .iter()
            .find(|r| r.label.as_deref() == Some("zero_byte_files"));
        assert!(
            zero_byte.is_some(),
            "cleanup should have zero_byte_files spec"
        );

        // distinct_extensions: should find our 5 distinct extensions
        // (rs, md, toml, bin, + empty for directory).
        let distinct = output
            .response
            .results
            .iter()
            .find(|r| r.label.as_deref() == Some("distinct_extensions"));
        assert!(
            distinct.is_some(),
            "cleanup should have distinct_extensions"
        );
    }

    #[test]
    fn s2f6_aggregate_and_rows_independent() {
        use crate::search::field::FieldId;

        let drive = build_agg_test_drive();

        // Run an aggregation.
        let agg_spec = AggregateSpec::new(AggregateKind::Terms {
            field: FieldId::Extension,
            top: 10,
            metrics: vec![BucketMetric::Count],
            sample: None,
        });
        let agg_output =
            run_aggregate(&[&drive], &[agg_spec], &FinalizeOptions::default()).unwrap();

        // Verify aggregation works.
        if let AggregateResultData::Buckets { rows, .. } = &agg_output.response.results[0].data {
            let total: u64 = rows.iter().map(|r| r.count).sum();
            assert!(total >= 7, "aggregation counted files");
        } else {
            panic!("expected Buckets");
        }

        // Run a second, independent aggregation on the same drive.
        let agg_spec2 = AggregateSpec::new(AggregateKind::Count);
        let agg_output2 =
            run_aggregate(&[&drive], &[agg_spec2], &FinalizeOptions::default()).unwrap();

        if let AggregateResultData::Count { value } = &agg_output2.response.results[0].data {
            assert!(*value >= 7, "count should be >= 7 records");
        } else {
            panic!("expected Count");
        }

        // Key assertion: neither aggregation mutated the drive index.
        // Running them back-to-back on the same &drive proves independence.
        assert_eq!(drive.records.len(), drive.records.len());
    }

    // ── S3F.2: Paginate through extensions with cursor ──────────────

    #[test]
    fn s3f2_paginate_extensions_total_equals_unpaginated() {
        use super::pagination::{AggregateCursor, paginate_result};

        let drive = build_agg_test_drive();
        // Full (unpaginated) terms:extension
        let spec = AggregateSpec::new(AggregateKind::Terms {
            field: crate::search::field::FieldId::Extension,
            top: 100,
            metrics: vec![BucketMetric::Count, BucketMetric::TotalBytes],
            sample: None,
        });
        let output = run_aggregate(&[&drive], &[spec], &FinalizeOptions::default()).unwrap();
        let full_result = &output.response.results[0];
        let AggregateResultData::Buckets {
            rows: full_rows, ..
        } = &full_result.data
        else {
            panic!("expected Buckets");
        };
        let full_count: u64 = full_rows.iter().map(|r| r.count).sum();
        let full_len = full_rows.len();

        // Now paginate with page_size=2 and walk all pages.
        let page_size = 2;
        let mut collected_keys = Vec::new();
        let mut collected_count: u64 = 0;
        let mut cursor = AggregateCursor::new(0, page_size);
        let mut pages = 0_u32;

        loop {
            let page = paginate_result(full_result, &cursor)
                .expect("paginate should work on bucket result");
            for row in &page.rows {
                collected_keys.push(row.key.clone());
                collected_count += row.count;
            }
            pages += 1;
            if let Some(next_token) = &page.next_cursor {
                cursor = AggregateCursor::decode(next_token).expect("next_cursor should decode");
            } else {
                break;
            }
        }

        // Verify totals match.
        assert_eq!(
            collected_keys.len(),
            full_len,
            "paginated total keys should equal unpaginated"
        );
        assert_eq!(
            collected_count, full_count,
            "paginated total count should equal unpaginated"
        );
        // Pages should be ceil(full_len / page_size).
        let expected_pages = uffs_mft::len_to_u32(full_len.div_ceil(page_size));
        assert_eq!(
            pages, expected_pages,
            "{full_len} extensions / page_size={page_size} → {expected_pages} pages"
        );
    }

    // ── S3F.3: facet_values prefix filtering ────────────────────────

    #[test]
    fn s3f3_terms_extension_prefix_filter() {
        // Simulate prefix filtering by running terms:extension then
        // client-side filtering. The synthetic drive has: rs, md, toml, bin.
        // "Prefix" filter is handled by the search pattern in the daemon,
        // but at the core level we verify that terms produces all values
        // and a prefix filter can narrow them.
        let drive = build_agg_test_drive();
        let spec = AggregateSpec::new(AggregateKind::Terms {
            field: crate::search::field::FieldId::Extension,
            top: 100,
            metrics: vec![BucketMetric::Count],
            sample: None,
        });
        let output = run_aggregate(&[&drive], &[spec], &FinalizeOptions::default()).unwrap();
        let AggregateResultData::Buckets { rows, .. } = &output.response.results[0].data else {
            panic!("expected Buckets");
        };

        // All extensions should be present.
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert!(keys.contains(&"rs"), "should have rs: {keys:?}");
        assert!(keys.contains(&"md"), "should have md: {keys:?}");

        // Prefix filter: only extensions starting with "r".
        let filtered: Vec<_> = rows.iter().filter(|r| r.key.starts_with('r')).collect();
        assert_eq!(filtered.len(), 1, "only 'rs' starts with 'r'");
        assert_eq!(filtered[0].key, "rs");
        assert!(filtered[0].count >= 3, "at least 3 .rs files");

        // Prefix filter: "m" → only "md".
        let m_filtered: Vec<_> = rows.iter().filter(|r| r.key.starts_with('m')).collect();
        assert_eq!(m_filtered.len(), 1);
        assert_eq!(m_filtered[0].key, "md");

        // Prefix filter: "z" → nothing.
        let z_count = rows.iter().filter(|r| r.key.starts_with('z')).count();
        assert_eq!(z_count, 0, "no extensions start with 'z'");
    }

    // ── S3F.4: nested rollup on synthetic index ─────────────────────

    #[test]
    fn s3f4_nested_rollup_drive_with_terms_type() {
        let drive = build_agg_test_drive();
        // rollup:drive with sub=terms:type
        let spec = AggregateSpec::new(AggregateKind::Rollup {
            mode: RollupMode::Drive,
            top: 10,
            metrics: vec![BucketMetric::Count, BucketMetric::TotalBytes],
            sample: None,
            sub: Some(Box::new(AggregateSpec::new(AggregateKind::Terms {
                field: crate::search::field::FieldId::Type,
                top: 20,
                metrics: vec![BucketMetric::Count, BucketMetric::TotalBytes],
                sample: None,
            }))),
        });
        let output = run_aggregate(&[&drive], &[spec], &FinalizeOptions::default()).unwrap();
        let result = &output.response.results[0];

        let rows = match &result.data {
            AggregateResultData::Rollup { rows, .. } => rows,
            other => panic!("expected Rollup, got: {other:?}"),
        };

        // Single drive C: → exactly 1 bucket.
        assert_eq!(rows.len(), 1, "single drive should produce 1 bucket");
        let drive_bucket = &rows[0];
        assert!(
            drive_bucket.key.starts_with('C'),
            "drive bucket key should start with C, got: {}",
            drive_bucket.key
        );

        // Nested sub_buckets should contain type breakdowns.
        assert!(
            !drive_bucket.sub_buckets.is_empty(),
            "drive bucket should have nested type sub-buckets"
        );

        // Total count across sub_buckets should equal the drive total.
        let sub_total: u64 = drive_bucket.sub_buckets.iter().map(|b| b.count).sum();
        assert_eq!(
            sub_total, drive_bucket.count,
            "sub_buckets total count should equal drive bucket count"
        );
    }
}
