// Aggregation handler bridges wire protocol to core aggregate engine.
// Same statistical patterns as uffs-core::aggregate apply here.
#![allow(
    clippy::min_ident_chars,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
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
use uffs_core::search::backend::DriveIndex;

use super::IndexManager;

/// Convert a core [`SampleRow`] to a wire [`SampleRowWire`].
fn sample_row_to_wire(sr: SampleRow) -> SampleRowWire {
    SampleRowWire {
        fields: sr.fields.into_iter().collect(),
        sort_key: Some(sr.sort_key),
    }
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
    pub(crate) fn run_aggregations(
        snapshot: &DriveIndex,
        wire_specs: &[uffs_client::protocol::AggregateSpecWire],
        query_predicates: Vec<DrilldownPredicate>,
        agg_cursor: Option<&str>,
        agg_page_size: Option<u16>,
    ) -> Vec<uffs_client::protocol::AggregateResultWire> {
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
            return vec![];
        }

        // Run aggregation.
        let drive_refs: Vec<&uffs_core::compact::DriveCompactIndex> =
            snapshot.drives.iter().map(|arc| arc.as_ref()).collect();
        let options = FinalizeOptions {
            query_predicates,
            ..FinalizeOptions::default()
        };
        let output = match uffs_core::aggregate::run_aggregate(&drive_refs, &specs, &options) {
            Ok(output) => output,
            Err(_) => return vec![],
        };

        // ── Apply cursor-based pagination (if requested) ────────────
        //
        // When `agg_page_size` is set, paginate bucket/rollup results.
        // When `agg_cursor` is set, it encodes `result_index:offset:page_size`.
        let decoded_cursor = agg_cursor.and_then(AggregateCursor::decode);
        let page_size = decoded_cursor
            .as_ref()
            .map(|c| c.page_size)
            .or_else(|| agg_page_size.map(|ps| ps as usize));

        // Convert results to wire format.
        output
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
                                }
                            })
                            .collect(),
                        None,
                        None,
                        Some(true),
                        None,
                    ),
                    AggregateResultData::Duplicates { result } => (
                        "duplicates".to_owned(),
                        None,
                        Some(result.candidate_files),
                        None,
                        result
                            .groups
                            .into_iter()
                            .take(20)
                            .map(|g| BucketWire {
                                key: format!("{}x{}", g.count, g.file_size),
                                count: g.count,
                                total_bytes: g.total_bytes,
                                total_allocated: Some(g.reclaimable_bytes),
                                avg_size: Some(g.file_size as f64),
                                share_count: None,
                                share_bytes: None,
                                sample_rows: Vec::new(),
                                drilldown: Vec::new(),
                                sub_buckets: Vec::new(),
                            })
                            .collect(),
                        None,
                        Some(result.candidate_groups),
                        None,
                        None,
                    ),
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
            .collect()
    }

    /// Convert a single wire spec into one or more core `AggregateSpec`s.
    ///
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
                        let record_idx = ws.interval.unwrap_or(0) as u32;
                        RollupMode::Ancestor { record_idx }
                    }
                    _ => {
                        let depth = ws.interval.unwrap_or(1) as u32;
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
                Ok(make(AggregateKind::Duplicates {
                    keys,
                    verify: DuplicateVerify::None,
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
