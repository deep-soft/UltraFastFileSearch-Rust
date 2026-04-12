// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.
//! Exception: `file_size_policy` — aggregation dispatch, tightly coupled helpers.

// Aggregation handler bridges wire protocol to core aggregate engine.
// Same statistical patterns as uffs-core::aggregate apply here.
#![allow(
    clippy::min_ident_chars,
    clippy::too_many_lines,
    clippy::shadow_reuse,
    clippy::redundant_closure_for_method_calls,
    clippy::option_if_let_else,
    clippy::bool_to_int_with_if,
    clippy::manual_let_else,
    clippy::clone_on_ref_ptr,
    clippy::assigning_clones,
    clippy::single_match_else,
    reason = "aggregation handler: statistical patterns, wire↔core mapping"
)]

//! Aggregation execution: convert wire specs to core specs and run them.

use uffs_client::protocol::{DrilldownWire, SampleRowWire};
use uffs_core::aggregate::TopHitsSpec;
use uffs_core::aggregate::finalize::{DrilldownPredicate, DrilldownValue, SampleRow};
use uffs_core::aggregate::spec::DuplicateVerify;
use uffs_core::aggregate::verify::{DuplicateVerifier, FileReader, VerificationBudget};
use uffs_core::search::backend::DriveIndex;

use super::IndexManager;

// ── Daemon file reader for duplicate verification ───────────────────

/// File reader that resolves `(record_idx, drive_ordinal)` to a file path
/// via the compact index, then reads bytes from disk.
///
/// Only functional on Windows where the resolved paths (e.g. `C:\Users\...`)
/// point to real files. On macOS/Linux (offline mode), reads always fail
/// gracefully — verification is skipped and groups remain unverified.
struct DaemonFileReader<'a> {
    /// Loaded drive indices for path resolution.
    drives: &'a [alloc::sync::Arc<uffs_core::compact::DriveCompactIndex>],
}

impl DaemonFileReader<'_> {
    /// Resolve a record to its full file path.
    fn resolve_path(&self, record_idx: usize, drive_ordinal: u8) -> Option<String> {
        let drive = self.drives.get(usize::from(drive_ordinal))?;
        let volume_prefix = format!("{}:\\", drive.letter);
        Some(uffs_core::search::tree::resolve_path(
            drive,
            record_idx,
            &volume_prefix,
        ))
    }
}

impl FileReader for DaemonFileReader<'_> {
    fn read_first_bytes(
        &self,
        record_idx: usize,
        drive_ordinal: u8,
        count: u32,
    ) -> std::io::Result<Vec<u8>> {
        use std::io::Read;
        let path = self
            .resolve_path(record_idx, drive_ordinal)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "drive ordinal out of range")
            })?;
        let mut file = std::fs::File::open(&path)?;
        let mut buf = vec![0_u8; uffs_mft::u32_as_usize(count)];
        let n = file.read(&mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    fn read_all(&self, record_idx: usize, drive_ordinal: u8) -> std::io::Result<Vec<u8>> {
        let path = self
            .resolve_path(record_idx, drive_ordinal)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "drive ordinal out of range")
            })?;
        std::fs::read(&path)
    }
}

/// Default verification budget: 256 MB total, 10 000 files max.
const DEFAULT_VERIFY_BUDGET_BYTES: u64 = 256 * 1024 * 1024;
/// Default max files for verification budget.
const DEFAULT_VERIFY_BUDGET_FILES: u32 = 10_000;

/// Convert a core [`SampleRow`] to a wire [`SampleRowWire`].
fn sample_row_to_wire(sr: SampleRow) -> SampleRowWire {
    SampleRowWire {
        fields: sr.fields.into_iter().collect(),
        sort_key: Some(sr.sort_key),
    }
}

/// Format bytes as compact human-readable size (e.g. `1.3 MB`).
#[expect(
    clippy::float_arithmetic,
    reason = "float division required for human-readable size formatting"
)]
fn format_size_compact(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    const TB: u64 = 1024 * GB;
    let bytes_f64 = uffs_mft::u64_to_f64(bytes);
    if bytes >= TB {
        format!("{:.2} TB", bytes_f64 / uffs_mft::u64_to_f64(TB))
    } else if bytes >= GB {
        format!("{:.2} GB", bytes_f64 / uffs_mft::u64_to_f64(GB))
    } else if bytes >= MB {
        format!("{:.1} MB", bytes_f64 / uffs_mft::u64_to_f64(MB))
    } else if bytes >= KB {
        format!("{:.1} KB", bytes_f64 / uffs_mft::u64_to_f64(KB))
    } else {
        format!("{bytes} B")
    }
}

/// Materialize duplicate group member indices into wire sample rows.
///
/// For each `(record_idx, drive_ordinal)`, resolve the record name, path,
/// and size from the compact index.
fn materialize_dup_members(
    members: &[(usize, u8)],
    drives: &[&uffs_core::compact::DriveCompactIndex],
) -> Vec<SampleRowWire> {
    members
        .iter()
        .filter_map(|&(rec_idx, drive_ord)| {
            let drive = drives.get(usize::from(drive_ord))?;
            let record = drive.records.get(rec_idx)?;
            let name = record.name(&drive.names).to_owned();
            let volume_prefix = format!("{}:\\", drive.letter);
            let path = uffs_core::search::tree::resolve_path(drive, rec_idx, &volume_prefix);

            let mut fields = std::collections::HashMap::new();
            fields.insert("name".to_owned(), name);
            fields.insert("path".to_owned(), path);
            fields.insert("size".to_owned(), record.size.to_string());

            Some(SampleRowWire {
                fields,
                sort_key: Some(record.size.cast_signed()),
            })
        })
        .collect()
}

/// Convert a core [`DrilldownPredicate`] to a wire [`DrilldownWire`].
fn drilldown_to_wire(dp: DrilldownPredicate) -> DrilldownWire {
    let value = match dp.value {
        DrilldownValue::String(s) => serde_json::Value::String(s),
        DrilldownValue::U64(n) => serde_json::Value::Number(n.into()),
        DrilldownValue::I64(n) => serde_json::Value::Number(n.into()),
        DrilldownValue::Bool(b) => serde_json::Value::Bool(b),
    };
    DrilldownWire {
        field: dp.field,
        op: dp.op,
        value,
    }
}

impl IndexManager {
    /// Run aggregation specs from wire format against loaded drives.
    ///
    /// `query_predicates` are the original search-scope filters (if any),
    /// forwarded into `FinalizeOptions` so drill-down predicates on each
    /// bucket include the full query context.
    ///
    /// `pattern` applies glob/regex name matching; `record_filter` applies
    /// O(1)-per-record constraints (extension IDs, directory flag, size
    /// bounds).  Both are optional and compose: a record must pass *both*
    /// to be fed to accumulators.
    #[expect(
        clippy::cognitive_complexity,
        reason = "multi-spec aggregation engine with predicate filtering and accumulation"
    )]
    #[expect(
        clippy::too_many_arguments,
        reason = "aggregation engine needs snapshot, specs, predicates, pagination, pattern, drives, and filter"
    )]
    pub(crate) fn run_aggregations(
        snapshot: &DriveIndex,
        wire_specs: &[uffs_client::protocol::AggregateSpecWire],
        query_predicates: Vec<DrilldownPredicate>,
        agg_cursor: Option<&str>,
        agg_page_size: Option<u16>,
        pattern: Option<&str>,
        drives_filter: &[char],
        record_filter: &uffs_core::aggregate::AggregateFilter,
    ) -> (Vec<uffs_client::protocol::AggregateResultWire>, u64) {
        use uffs_client::protocol::{AggregateResultWire, BucketWire, StatsWire};
        use uffs_core::aggregate::finalize::{AggregateResultData, FinalizeOptions};
        use uffs_core::aggregate::pagination::{AggregateCursor, paginate_result};
        use uffs_core::aggregate::spec::AggregateSpec;

        // Convert wire specs to core specs.
        let mut specs: Vec<AggregateSpec> = Vec::new();
        for ws in wire_specs {
            match Self::convert_wire_spec(ws) {
                Ok(converted) => specs.extend(converted),
                Err(e) => {
                    tracing::warn!(kind = %ws.kind, "skipping malformed aggregate spec: {e}");
                }
            }
        }

        if specs.is_empty() {
            return (vec![], 0);
        }

        // Apply drive filter: if non-empty, only include matching drives.
        let drive_refs: Vec<&uffs_core::compact::DriveCompactIndex> = snapshot
            .drives
            .iter()
            .filter(|arc| {
                drives_filter.is_empty()
                    || drives_filter
                        .iter()
                        .any(|f| f.eq_ignore_ascii_case(&arc.letter))
            })
            .map(|arc| arc.as_ref())
            .collect();
        let options = FinalizeOptions {
            query_predicates,
            ..FinalizeOptions::default()
        };

        // Dispatch to the appropriate core aggregate function.
        //
        // `run_aggregate_with_filters` handles all three cases internally:
        //   - pattern + record_filter → combined filtered scan
        //   - pattern only → delegates to `run_aggregate_filtered`
        //   - no filters → delegates to `run_aggregate` (unfiltered)
        tracing::info!(
            pattern = ?pattern,
            ext_count = record_filter.extensions.len(),
            dir_only = ?record_filter.directory_only,
            "running aggregation"
        );
        let mut output = match uffs_core::aggregate::run_aggregate_with_filters(
            &drive_refs,
            &specs,
            &options,
            pattern,
            record_filter,
        ) {
            Ok(output) => {
                tracing::info!(
                    scanned = output.records_scanned,
                    matched = output.records_matched,
                    "aggregation complete"
                );
                output
            }
            Err(e) => {
                tracing::error!(error = %e, "aggregation failed");
                return (vec![], 0);
            }
        };

        // ── Duplicate verification (if any spec has verify != None) ──
        Self::run_duplicate_verification(&specs, &mut output, &snapshot.drives);
        let records_matched = output.records_matched;

        // ── Apply cursor-based pagination (if requested) ────────────
        //
        // When `agg_page_size` is set, paginate bucket/rollup results.
        // When `agg_cursor` is set, it encodes `result_index:offset:page_size`.
        let decoded_cursor = agg_cursor.and_then(AggregateCursor::decode);
        let page_size = decoded_cursor
            .as_ref()
            .map(|c| c.page_size)
            .or_else(|| agg_page_size.map(usize::from));

        // Convert results to wire format.
        let wire_results = output
            .response
            .results
            .into_iter()
            .enumerate()
            .map(|(idx, result)| {
                // If pagination is active, check if this result should be paginated.
                let pagination = page_size.and_then(|ps| {
                    let cursor = decoded_cursor
                        .as_ref()
                        .filter(|c| c.result_index == idx)
                        .cloned()
                        .unwrap_or_else(|| AggregateCursor::new(idx, ps));
                    paginate_result(&result, &cursor)
                });

                let (
                    kind,
                    field,
                    value,
                    stats,
                    buckets,
                    other_count,
                    total_groups,
                    exact,
                    values_complete,
                ) = match result.data {
                    AggregateResultData::Count { value } => (
                        "count".to_owned(),
                        None,
                        Some(value),
                        None,
                        vec![],
                        None,
                        None,
                        None,
                        None,
                    ),
                    AggregateResultData::Stats { field, stats } => (
                        "stats".to_owned(),
                        Some(field),
                        None,
                        Some(StatsWire {
                            count: stats.count,
                            sum: stats.sum,
                            min: stats.min,
                            max: stats.max,
                            avg: stats.avg,
                            waste_bytes: stats.waste_bytes,
                            waste_pct: stats.waste_pct,
                        }),
                        vec![],
                        None,
                        None,
                        None,
                        None,
                    ),
                    AggregateResultData::Buckets {
                        field,
                        rows,
                        other_count,
                        total_groups,
                        exact,
                    } => (
                        "buckets".to_owned(),
                        Some(field),
                        None,
                        None,
                        rows.into_iter()
                            .map(|r| {
                                let samples =
                                    r.sample_rows.into_iter().map(sample_row_to_wire).collect();
                                let drills =
                                    r.drilldown.into_iter().map(drilldown_to_wire).collect();
                                BucketWire {
                                    key: r.key,
                                    count: r.count,
                                    total_bytes: r.total_bytes,
                                    total_allocated: Some(r.total_allocated),
                                    avg_size: Some(r.avg_size),
                                    share_count: Some(r.share_of_total_count),
                                    share_bytes: Some(r.share_of_total_bytes),
                                    sample_rows: samples,
                                    drilldown: drills,
                                    sub_buckets: Vec::new(),
                                    verified: false,
                                }
                            })
                            .collect(),
                        Some(other_count),
                        Some(total_groups),
                        Some(exact),
                        Some(other_count == 0),
                    ),
                    AggregateResultData::Missing { field, count } => (
                        "missing".to_owned(),
                        Some(field),
                        Some(count),
                        None,
                        vec![],
                        None,
                        None,
                        None,
                        None,
                    ),
                    AggregateResultData::Distinct { field, count } => (
                        "distinct".to_owned(),
                        Some(field),
                        Some(count),
                        None,
                        vec![],
                        None,
                        None,
                        None,
                        None,
                    ),
                    AggregateResultData::Rollup { mode, rows } => (
                        "rollup".to_owned(),
                        Some(mode),
                        None,
                        None,
                        rows.into_iter()
                            .map(|r| {
                                let samples =
                                    r.sample_rows.into_iter().map(sample_row_to_wire).collect();
                                let drills =
                                    r.drilldown.into_iter().map(drilldown_to_wire).collect();
                                let subs = r
                                    .sub_buckets
                                    .into_iter()
                                    .map(|sub| BucketWire {
                                        key: sub.key,
                                        count: sub.count,
                                        total_bytes: sub.total_bytes,
                                        total_allocated: Some(sub.total_allocated),
                                        avg_size: Some(sub.avg_size),
                                        share_count: Some(sub.share_of_total_count),
                                        share_bytes: Some(sub.share_of_total_bytes),
                                        sample_rows: Vec::new(),
                                        drilldown: Vec::new(),
                                        sub_buckets: Vec::new(),
                                        verified: false,
                                    })
                                    .collect();
                                BucketWire {
                                    key: r.key,
                                    count: r.count,
                                    total_bytes: r.total_bytes,
                                    total_allocated: Some(r.total_allocated),
                                    avg_size: Some(r.avg_size),
                                    share_count: Some(r.share_of_total_count),
                                    share_bytes: Some(r.share_of_total_bytes),
                                    sample_rows: samples,
                                    drilldown: drills,
                                    sub_buckets: subs,
                                    verified: false,
                                }
                            })
                            .collect(),
                        None,
                        None,
                        Some(true),
                        None,
                    ),
                    AggregateResultData::Duplicates { result } => {
                        let total_groups = result.candidate_groups;
                        let total_reclaimable = result.total_reclaimable_bytes;
                        let dup_drive_refs: Vec<&uffs_core::compact::DriveCompactIndex> =
                            snapshot.drives.iter().map(|d| d.as_ref()).collect();
                        let buckets: Vec<BucketWire> = result
                            .groups
                            .into_iter()
                            .map(|g| {
                                // Materialize sample rows from member_indices.
                                let samples: Vec<SampleRowWire> =
                                    materialize_dup_members(&g.member_indices, &dup_drive_refs);
                                // Derive human-readable key from first sample
                                // row's name field, falling back to file size.
                                let display_name = samples
                                    .first()
                                    .and_then(|s| s.fields.get("name").cloned())
                                    .filter(|n| !n.is_empty())
                                    .unwrap_or_else(|| format_size_compact(g.file_size));
                                let key = format!(
                                    "{} ({}, {} copies)",
                                    display_name,
                                    format_size_compact(g.file_size),
                                    g.count,
                                );
                                BucketWire {
                                    key,
                                    count: g.count,
                                    total_bytes: g.total_bytes,
                                    total_allocated: Some(g.reclaimable_bytes),
                                    avg_size: Some(uffs_mft::u64_to_f64(g.file_size)),
                                    share_count: None,
                                    share_bytes: None,
                                    sample_rows: samples,
                                    drilldown: Vec::new(),
                                    sub_buckets: Vec::new(),
                                    verified: g.verified,
                                }
                            })
                            .collect();

                        // Build stats with summary: total reclaimable in
                        // waste_bytes, total candidate files in count.
                        #[expect(
                            clippy::float_arithmetic,
                            reason = "percentage calculation for waste_pct"
                        )]
                        let summary = StatsWire {
                            count: result.candidate_files,
                            sum: result.total_duplicate_bytes,
                            min: 0,
                            max: 0,
                            avg: 0.0,
                            waste_bytes: total_reclaimable,
                            waste_pct: if result.total_duplicate_bytes > 0 {
                                (uffs_mft::u64_to_f64(total_reclaimable)
                                    / uffs_mft::u64_to_f64(result.total_duplicate_bytes))
                                    * 100.0
                            } else {
                                0.0
                            },
                        };

                        (
                            "duplicates".to_owned(),
                            None,
                            Some(result.candidate_files),
                            Some(summary),
                            buckets,
                            None,
                            Some(total_groups),
                            None,
                            None,
                        )
                    }
                };

                // Apply pagination: replace full bucket list with the
                // current page and attach `next_cursor` for the caller.
                let (buckets, next_cursor) = if let Some(pg) = &pagination {
                    let start = pg.offset.min(buckets.len());
                    let end = (start + pg.rows.len()).min(buckets.len());
                    let page = buckets.get(start..end).map_or_else(Vec::new, <[_]>::to_vec);
                    (page, pg.next_cursor.clone())
                } else {
                    (buckets, None)
                };

                AggregateResultWire {
                    label: result.label,
                    kind,
                    field,
                    value,
                    stats,
                    buckets,
                    other_count,
                    total_groups,
                    next_cursor,
                    exact,
                    values_complete,
                }
            })
            .collect();
        (wire_results, records_matched)
    }

    /// Convert a single wire spec into one or more core `AggregateSpec`s.
    ///
    /// Run duplicate verification on any `Duplicates` result that has
    /// `verify != None`.
    ///
    /// Extracts the verify mode from the original specs, builds a
    /// [`DaemonFileReader`], and calls [`DuplicateVerifier::verify`].
    /// Results are mutated in place.
    fn run_duplicate_verification(
        specs: &[uffs_core::aggregate::spec::AggregateSpec],
        output: &mut uffs_core::aggregate::AggregateOutput,
        drives: &[alloc::sync::Arc<uffs_core::compact::DriveCompactIndex>],
    ) {
        use uffs_core::aggregate::finalize::AggregateResultData;
        use uffs_core::aggregate::spec::AggregateKind;

        // Collect verify modes from specs (parallel to results).
        let verify_modes: Vec<DuplicateVerify> = specs
            .iter()
            .map(|s| match &s.kind {
                AggregateKind::Duplicates { verify, .. } => *verify,
                AggregateKind::Count
                | AggregateKind::Stats { .. }
                | AggregateKind::Terms { .. }
                | AggregateKind::Histogram { .. }
                | AggregateKind::DateHistogram { .. }
                | AggregateKind::Range { .. }
                | AggregateKind::Missing { .. }
                | AggregateKind::Distinct { .. }
                | AggregateKind::Rollup { .. } => DuplicateVerify::None,
            })
            .collect();

        // Short-circuit: no verification needed.
        if verify_modes
            .iter()
            .all(|m| matches!(m, DuplicateVerify::None))
        {
            return;
        }

        let reader = DaemonFileReader { drives };
        let budget =
            VerificationBudget::new(DEFAULT_VERIFY_BUDGET_BYTES, DEFAULT_VERIFY_BUDGET_FILES);

        // Walk results in parallel with specs. Each result corresponds to a
        // spec at the same index.
        for (result, mode) in output.response.results.iter_mut().zip(verify_modes.iter()) {
            if matches!(mode, DuplicateVerify::None) {
                continue;
            }
            if let AggregateResultData::Duplicates { result: dup_result } = &mut result.data {
                let mut verifier = DuplicateVerifier::new(*mode, budget);
                // Move current result out, verify, and put it back.
                let placeholder = uffs_core::aggregate::duplicates::DuplicateResult {
                    candidate_groups: 0,
                    candidate_files: 0,
                    total_duplicate_bytes: 0,
                    total_reclaimable_bytes: 0,
                    groups: Vec::new(),
                    verification_mode: DuplicateVerify::None,
                };
                let current = core::mem::replace(dup_result, placeholder);
                let (vfy_result, summary) = verifier.verify(current, &reader);
                *dup_result = vfy_result;

                tracing::info!(
                    mode = ?mode,
                    groups_verified = summary.groups_verified,
                    groups_rejected = summary.groups_rejected,
                    groups_skipped = summary.groups_skipped,
                    groups_errored = summary.groups_errored,
                    bytes_read = summary.bytes_read,
                    budget_exhausted = summary.budget_exhausted,
                    "duplicate verification complete"
                );
            }
        }
    }

    /// Presets expand to multiple specs; all other kinds produce exactly one.
    #[expect(
        clippy::too_many_lines,
        reason = "straightforward match arms — one per wire kind"
    )]
    pub(crate) fn convert_wire_spec(
        ws: &uffs_client::protocol::AggregateSpecWire,
    ) -> Result<Vec<uffs_core::aggregate::spec::AggregateSpec>, String> {
        use uffs_core::aggregate::parser::parse_agg_spec;
        use uffs_core::aggregate::presets::AggregatePreset;
        use uffs_core::aggregate::spec::{
            AggregateKind, AggregateSpec, CalendarInterval, DuplicateVerify, RollupMode,
        };
        use uffs_core::search::field::FieldId;

        /// Helper: parse optional wire field name to `FieldId`.
        fn require_field(ws: &uffs_client::protocol::AggregateSpecWire) -> Result<FieldId, String> {
            let name = ws
                .field
                .as_deref()
                .ok_or_else(|| "missing 'field'".to_owned())?;
            FieldId::parse(name).ok_or_else(|| format!("unknown field: `{name}`"))
        }

        /// Helper: parse wire metric strings to `BucketMetric`s, with defaults.
        fn parse_bucket_metrics(wire: &[String]) -> Vec<uffs_core::aggregate::spec::BucketMetric> {
            use uffs_core::aggregate::spec::BucketMetric;
            if wire.is_empty() {
                return vec![BucketMetric::Count, BucketMetric::TotalBytes];
            }
            wire.iter()
                .filter_map(|m| match m.as_str() {
                    "count" => Some(BucketMetric::Count),
                    "total_bytes" | "bytes" | "size" => Some(BucketMetric::TotalBytes),
                    "total_allocated" | "allocated" => Some(BucketMetric::TotalAllocated),
                    "waste_bytes" | "waste" => Some(BucketMetric::WasteBytes),
                    "waste_pct" | "waste_percent" => Some(BucketMetric::WastePct),
                    "avg_size" | "avg" => Some(BucketMetric::AvgSize),
                    "min_size" | "min" => Some(BucketMetric::MinSize),
                    "max_size" | "max" => Some(BucketMetric::MaxSize),
                    "share_count" | "share_of_count" => Some(BucketMetric::ShareOfTotalCount),
                    "share_bytes" | "share_of_bytes" => Some(BucketMetric::ShareOfTotalBytes),
                    _ => None,
                })
                .collect()
        }

        /// Helper: parse wire metric strings to `ScalarMetric`s, with defaults.
        fn parse_scalar_metrics(wire: &[String]) -> Vec<uffs_core::aggregate::spec::ScalarMetric> {
            use uffs_core::aggregate::spec::ScalarMetric;
            if wire.is_empty() {
                return vec![
                    ScalarMetric::Sum,
                    ScalarMetric::Min,
                    ScalarMetric::Max,
                    ScalarMetric::Avg,
                ];
            }
            wire.iter()
                .filter_map(|m| match m.as_str() {
                    "sum" => Some(ScalarMetric::Sum),
                    "min" => Some(ScalarMetric::Min),
                    "max" => Some(ScalarMetric::Max),
                    "avg" | "mean" => Some(ScalarMetric::Avg),
                    "value_count" | "count" => Some(ScalarMetric::ValueCount),
                    "missing_count" | "missing" => Some(ScalarMetric::MissingCount),
                    _ => None,
                })
                .collect()
        }

        /// Build `Option<TopHitsSpec>` from the wire spec's sample fields.
        fn build_sample(ws: &uffs_client::protocol::AggregateSpecWire) -> Option<TopHitsSpec> {
            ws.sample.map(|n| {
                let mut th = TopHitsSpec::with_count(n);
                if let Some(field) = &ws.sample_sort
                    && let Some(fid) = FieldId::parse(field)
                {
                    th.sort_field = fid;
                }
                if let Some(desc) = ws.sample_desc {
                    th.sort_desc = desc;
                }
                th
            })
        }

        let make = |kind: AggregateKind| -> Vec<AggregateSpec> {
            let mut spec = AggregateSpec::new(kind);
            spec.label = ws.label.clone();
            vec![spec]
        };

        match ws.kind.as_str() {
            "preset" => {
                let name = ws
                    .preset
                    .as_deref()
                    .ok_or_else(|| "preset kind requires 'preset' field".to_owned())?;
                let preset = AggregatePreset::parse(name)
                    .ok_or_else(|| format!("unknown preset: `{name}`"))?;
                Ok(preset.expand())
            }
            "count" => Ok(make(AggregateKind::Count)),
            "stats" => {
                let field = require_field(ws)?;
                let metrics = parse_scalar_metrics(&ws.metrics);
                Ok(make(AggregateKind::Stats { field, metrics }))
            }
            "terms" | "facet" => {
                let field = require_field(ws)?;
                let top = ws.top.unwrap_or(20);
                let metrics = parse_bucket_metrics(&ws.metrics);
                Ok(make(AggregateKind::Terms {
                    field,
                    top,
                    metrics,
                    sample: build_sample(ws),
                }))
            }
            "histogram" | "hist" => {
                let field = require_field(ws)?;
                let interval = ws.interval.unwrap_or(1_048_576);
                let metrics = parse_bucket_metrics(&ws.metrics);
                Ok(make(AggregateKind::Histogram {
                    field,
                    interval,
                    metrics,
                }))
            }
            "date_histogram" | "datehist" => {
                let field = require_field(ws)?;
                let cal_str = ws.calendar.as_deref().unwrap_or("month");
                let calendar = CalendarInterval::parse(cal_str)
                    .ok_or_else(|| format!("unknown calendar interval: `{cal_str}`"))?;
                let metrics = parse_bucket_metrics(&ws.metrics);
                Ok(make(AggregateKind::DateHistogram {
                    field,
                    calendar,
                    metrics,
                }))
            }
            "range" => {
                let field = require_field(ws)?;
                let metrics = parse_bucket_metrics(&ws.metrics);
                Ok(make(AggregateKind::Range {
                    field,
                    boundaries: ws.boundaries.clone(),
                    metrics,
                }))
            }
            "missing" => {
                let field = require_field(ws)?;
                Ok(make(AggregateKind::Missing { field }))
            }
            "distinct" => {
                let field = require_field(ws)?;
                Ok(make(AggregateKind::Distinct { field }))
            }
            "rollup" => {
                let mode_str = ws.field.as_deref().unwrap_or("path");
                let mode = match mode_str {
                    "drive" => RollupMode::Drive,
                    "ancestor" | "drilldown" => {
                        // Use interval field as the record index.
                        let record_idx = u32::try_from(ws.interval.unwrap_or(0)).unwrap_or(0);
                        RollupMode::Ancestor { record_idx }
                    }
                    _ => {
                        let depth = u32::try_from(ws.interval.unwrap_or(1)).unwrap_or(1);
                        RollupMode::Path { depth }
                    }
                };
                let top = ws.top.unwrap_or(30);
                let metrics = parse_bucket_metrics(&ws.metrics);
                Ok(make(AggregateKind::Rollup {
                    mode,
                    top,
                    metrics,
                    sample: build_sample(ws),
                    sub: None, // TODO: wire sub-agg from wire type
                }))
            }
            "duplicates" | "dups" => {
                let keys: Vec<FieldId> = ws
                    .metrics
                    .iter()
                    .filter_map(|m| FieldId::parse(m))
                    .collect();
                let keys = if keys.is_empty() {
                    vec![FieldId::Size, FieldId::Name]
                } else {
                    keys
                };
                let top = ws.top.unwrap_or(100);
                let verify = match ws.verify.as_deref() {
                    Some("first_bytes") => DuplicateVerify::FirstBytes {
                        count: ws.verify_bytes.unwrap_or(4096),
                    },
                    Some("sha256") => DuplicateVerify::Sha256,
                    _ => DuplicateVerify::None,
                };
                Ok(make(AggregateKind::Duplicates {
                    keys,
                    verify,
                    top,
                    sample: Some(build_sample(ws).unwrap_or_else(|| TopHitsSpec::with_count(2))),
                    max_groups: 500_000,
                }))
            }
            "raw" => {
                let syntax = ws
                    .label
                    .as_deref()
                    .ok_or_else(|| "raw kind requires syntax in 'label' field".to_owned())?;
                let spec = parse_agg_spec(syntax)?;
                Ok(vec![spec])
            }
            other => Err(format!("unknown aggregate kind: `{other}`")),
        }
    }
}
