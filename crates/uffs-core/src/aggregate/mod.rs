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
