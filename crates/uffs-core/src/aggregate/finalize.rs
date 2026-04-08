//! Finalization of aggregate results.
//!
//! After accumulators have been fed all matching records, this module
//! converts raw accumulator state into sorted, truncated, labeled
//! response objects.

use super::accumulators::{AccumulatorKind, GroupAccumulator, StatsAccumulator};
use super::planner::AggregatePlan;
use crate::compact::DriveCompactIndex;

/// Options controlling finalization behavior.
#[derive(Debug, Clone)]
pub struct FinalizeOptions {
    /// Whether to compute share-of-total metrics.
    pub compute_shares: bool,
    /// Whether to include empty buckets in output.
    pub include_empty_buckets: bool,
    /// The original query predicates (if any).  These are prepended to
    /// each bucket's drill-down predicate list so that a follow-up query
    /// reproduces the original scope **plus** the bucket constraint.
    pub query_predicates: Vec<DrilldownPredicate>,
}

impl Default for FinalizeOptions {
    fn default() -> Self {
        Self {
            compute_shares: true,
            include_empty_buckets: false,
            query_predicates: Vec::new(),
        }
    }
}

/// The finalized aggregate response.
#[derive(Debug, Clone)]
pub struct AggregateResponse {
    /// One result per spec in the plan.
    pub results: Vec<AggregateResult>,
}

/// Result of a single aggregation spec.
#[derive(Debug, Clone)]
pub struct AggregateResult {
    /// Label for this result (from the spec).
    pub label: Option<String>,
    /// The kind-specific result data.
    pub data: AggregateResultData,
}

/// The kind-specific result payload.
#[derive(Debug, Clone)]
pub enum AggregateResultData {
    /// Simple count.
    Count {
        /// Total count of matching records.
        value: u64,
    },
    /// Scalar statistics.
    Stats {
        /// Field name.
        field: String,
        /// Computed statistics.
        stats: StatsResult,
    },
    /// Grouped bucket results (terms, histogram, `date_histogram`, range).
    Buckets {
        /// Field name.
        field: String,
        /// Sorted, truncated bucket rows.
        rows: Vec<BucketRow>,
        /// Count of records in buckets beyond top-N (for terms).
        other_count: u64,
        /// Total groups before truncation.
        total_groups: usize,
        /// Whether the values are exact (not approximate).
        exact: bool,
    },
    /// Missing value count.
    Missing {
        /// Field name.
        field: String,
        /// Count of records with missing value.
        count: u64,
    },
    /// Distinct value count.
    Distinct {
        /// Field name.
        field: String,
        /// Number of distinct values seen.
        count: u64,
    },
    /// Rollup result (path/drive grouping).
    Rollup {
        /// Rollup mode description.
        mode: String,
        /// Grouped bucket rows.
        rows: Vec<BucketRow>,
    },
    /// Duplicate detection result.
    Duplicates {
        /// Full duplicate result data.
        result: super::duplicates::DuplicateResult,
    },
}

/// Scalar statistics result.
#[derive(Debug, Clone)]
pub struct StatsResult {
    /// Number of records.
    pub count: u64,
    /// Sum of values.
    pub sum: u64,
    /// Minimum value.
    pub min: u64,
    /// Maximum value.
    pub max: u64,
    /// Average value.
    pub avg: f64,
    /// Waste: allocated - logical.
    pub waste_bytes: u64,
    /// Waste percentage.
    pub waste_pct: f64,
}

/// A single row in a bucket result.
#[derive(Debug, Clone)]
pub struct BucketRow {
    /// Display key for this bucket.
    pub key: String,
    /// Number of records in this bucket.
    pub count: u64,
    /// Total logical bytes.
    pub total_bytes: u64,
    /// Total allocated bytes.
    pub total_allocated: u64,
    /// Average file size.
    pub avg_size: f64,
    /// Minimum file size.
    pub min_size: u64,
    /// Maximum file size.
    pub max_size: u64,
    /// Waste bytes.
    pub waste_bytes: u64,
    /// Waste percentage.
    pub waste_pct: f64,
    /// Share of total count (percentage, 0.0–100.0).
    pub share_of_total_count: f64,
    /// Share of total bytes (percentage, 0.0–100.0).
    pub share_of_total_bytes: f64,
    /// Optional sample rows (top-N records in this bucket).
    pub sample_rows: Vec<SampleRow>,
    /// Drill-down predicates: the original query predicates **plus**
    /// a bucket-key predicate that narrows the result set to exactly
    /// the records in this bucket.
    ///
    /// A client can re-issue a row-level search using these predicates
    /// to retrieve the actual records behind the bucket.
    pub drilldown: Vec<DrilldownPredicate>,
    /// Nested sub-aggregation bucket rows (populated by nested rollups).
    ///
    /// When a rollup spec has `sub`, each top-level bucket finalizes
    /// its per-group sub-accumulator and stores the result here.
    pub sub_buckets: Vec<Self>,
}

/// A lightweight predicate for drill-down follow-up queries.
///
/// Mirrors the wire-protocol `SearchPredicate` shape but lives in
/// `uffs-core` so the aggregate module has no dependency on the client
/// crate.  The daemon / CLI can convert these to wire predicates
/// trivially.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrilldownPredicate {
    /// Canonical field name (e.g. `"extension"`, `"drive"`, `"type"`).
    pub field: String,
    /// Comparison operator (e.g. `"eq"`, `"in"`).
    pub op: String,
    /// Predicate value(s).
    pub value: DrilldownValue,
}

/// Value for a [`DrilldownPredicate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrilldownValue {
    /// A single string value.
    String(String),
    /// A single unsigned integer.
    U64(u64),
    /// A single signed integer.
    I64(i64),
    /// A boolean value.
    Bool(bool),
}

/// A materialized sample row — one record's projected fields.
///
/// Produced during finalization from the per-bucket `SampleHeap` entries.
/// Only surviving (top-N) buckets have their samples materialized.
#[derive(Debug, Clone)]
pub struct SampleRow {
    /// Key-value pairs of projected fields.
    ///
    /// Each entry is `(field_name, display_value)`.  The set of fields
    /// is determined by [`TopHitsSpec::effective_projection`].
    pub fields: Vec<(String, String)>,
    /// The sort key that determined this row's position.
    pub sort_key: i64,
}

impl BucketRow {
    /// Create a bucket row from a stats accumulator and context.
    fn from_stats(
        key: String,
        stats: &StatsAccumulator,
        total_matched: u64,
        total_bytes: u64,
    ) -> Self {
        let share_count = if total_matched == 0 {
            0.0
        } else {
            stats.count as f64 / total_matched as f64 * 100.0
        };
        let share_bytes = if total_bytes == 0 {
            0.0
        } else {
            stats.sum as f64 / total_bytes as f64 * 100.0
        };
        Self {
            key,
            count: stats.count,
            total_bytes: stats.sum,
            total_allocated: stats.sum_allocated,
            avg_size: stats.avg(),
            min_size: if stats.min == u64::MAX { 0 } else { stats.min },
            max_size: stats.max,
            waste_bytes: stats.waste_bytes(),
            waste_pct: stats.waste_pct(),
            share_of_total_count: share_count,
            share_of_total_bytes: share_bytes,
            sample_rows: Vec::new(),
            drilldown: Vec::new(),
            sub_buckets: Vec::new(),
        }
    }
}

/// Finalize accumulated results into a response.
/// Finalize aggregate results, optionally using a cross-drive
/// [`ExtensionMap`] for correct extension key resolution.
pub(crate) fn finalize_with_ext_map(
    accumulators: Vec<GroupAccumulator>,
    _plan: &AggregatePlan,
    drives: &[&DriveCompactIndex],
    options: &FinalizeOptions,
    total_matched: u64,
    ext_map: Option<&super::ExtensionMap>,
) -> AggregateResponse {
    let global_total_bytes = compute_global_bytes(&accumulators);

    let results = accumulators
        .into_iter()
        .map(|acc| {
            finalize_one(
                acc,
                total_matched,
                global_total_bytes,
                options,
                drives,
                ext_map,
            )
        })
        .collect();

    AggregateResponse { results }
}

/// Compute total bytes across all accumulators (for share-of-total).
fn compute_global_bytes(accumulators: &[GroupAccumulator]) -> u64 {
    for acc in accumulators {
        if let AccumulatorKind::Stats { stats, .. } = &acc.kind
            && acc.field == Some(crate::search::field::FieldId::Size)
        {
            return stats.sum;
        }
    }
    0
}

/// Finalize a standalone accumulator (used by nested rollups).
///
/// Wraps `finalize_one` with default options.
fn finalize_accumulator(
    acc: GroupAccumulator,
    total_matched: u64,
    total_bytes: u64,
    drives: &[&DriveCompactIndex],
    _predicates: &[DrilldownPredicate],
) -> AggregateResult {
    finalize_one(
        acc,
        total_matched,
        total_bytes,
        &FinalizeOptions::default(),
        drives,
        None,
    )
}

/// Convert a sub-aggregation result into drilldown bucket rows.
///
/// Extracts `BucketRow`s from bucket-like result variants; non-bucket
/// results (count, stats, missing, distinct) produce an empty vec.
fn sub_result_to_bucket_rows(result: &AggregateResult) -> Vec<BucketRow> {
    match &result.data {
        AggregateResultData::Buckets { rows, .. } | AggregateResultData::Rollup { rows, .. } => {
            rows.clone()
        }
        _ => Vec::new(),
    }
}

/// Finalize a single accumulator into an `AggregateResult`.
fn finalize_one(
    acc: GroupAccumulator,
    total_matched: u64,
    total_bytes: u64,
    options: &FinalizeOptions,
    drives: &[&DriveCompactIndex],
    ext_map: Option<&super::ExtensionMap>,
) -> AggregateResult {
    let label = acc.label.clone();
    let field_name = acc
        .field
        .map(|f| f.metadata().canonical_name.to_owned())
        .unwrap_or_default();

    let data = match acc.kind {
        AccumulatorKind::Count { count } => AggregateResultData::Count { value: count },

        AccumulatorKind::Stats { stats, .. } => AggregateResultData::Stats {
            field: field_name,
            stats: StatsResult {
                count: stats.count,
                sum: stats.sum,
                min: if stats.min == u64::MAX { 0 } else { stats.min },
                max: stats.max,
                avg: stats.avg(),
                waste_bytes: stats.waste_bytes(),
                waste_pct: stats.waste_pct(),
            },
        },

        AccumulatorKind::Terms {
            groups,
            top,
            mut sample_heaps,
            sample_spec,
            ..
        } => {
            let total_groups = groups.len();

            // Build (group_key, BucketRow) pairs so we can correlate
            // surviving keys with their sample heaps.
            let mut keyed_rows: Vec<(u64, BucketRow)> = groups
                .iter()
                .map(|(&key, stats)| {
                    let key_str = resolve_group_key(acc.field, key, drives, ext_map);
                    (
                        key,
                        BucketRow::from_stats(key_str, stats, total_matched, total_bytes),
                    )
                })
                .collect();

            keyed_rows.sort_by(|a, b| b.1.count.cmp(&a.1.count));

            let limit = top as usize;
            let other_count = if keyed_rows.len() > limit {
                let other: u64 = keyed_rows[limit..].iter().map(|r| r.1.count).sum();
                keyed_rows.truncate(limit);
                other
            } else {
                0
            };

            // Materialize sample rows for surviving buckets only.
            if let Some(ref mut heaps) = sample_heaps {
                let projection = sample_spec
                    .as_ref()
                    .map(super::spec::TopHitsSpec::effective_projection)
                    .unwrap_or_default();
                for (group_key, row) in &mut keyed_rows {
                    if let Some(heap) = heaps.get_mut(group_key) {
                        let entries = heap.drain_sorted();
                        row.sample_rows = entries
                            .iter()
                            .map(|entry| materialize_sample_entry(entry, projection, drives))
                            .collect();
                    }
                }
            }

            // Attach drill-down predicates: original query preds + bucket key.
            let bucket_field = acc.field;
            for (group_key, row) in &mut keyed_rows {
                row.drilldown = build_drilldown(
                    &options.query_predicates,
                    bucket_field,
                    *group_key,
                    &row.key,
                    drives,
                );
            }

            let rows: Vec<BucketRow> = keyed_rows.into_iter().map(|(_, row)| row).collect();

            AggregateResultData::Buckets {
                field: field_name,
                rows,
                other_count,
                total_groups,
                exact: true,
            }
        }

        AccumulatorKind::Histogram {
            buckets,
            boundaries,
            ..
        } => {
            let rows: Vec<_> = buckets
                .iter()
                .enumerate()
                .filter(|(_, s)| options.include_empty_buckets || s.count > 0)
                .map(|(i, stats)| {
                    let key = format_range_key(i, &boundaries);
                    BucketRow::from_stats(key, stats, total_matched, total_bytes)
                })
                .collect();

            AggregateResultData::Buckets {
                field: field_name,
                rows,
                other_count: 0,
                total_groups: buckets.len(),
                exact: true,
            }
        }

        AccumulatorKind::DateHistogram { buckets, .. } => {
            let rows: Vec<_> = buckets
                .iter()
                .filter(|(_, s)| options.include_empty_buckets || s.count > 0)
                .map(|(&ts, stats)| {
                    let key = format_timestamp_key(ts);
                    BucketRow::from_stats(key, stats, total_matched, total_bytes)
                })
                .collect();

            AggregateResultData::Buckets {
                field: field_name,
                rows,
                other_count: 0,
                total_groups: buckets.len(),
                exact: true,
            }
        }

        AccumulatorKind::Missing { count } => AggregateResultData::Missing {
            field: field_name,
            count,
        },

        AccumulatorKind::Distinct { seen } => AggregateResultData::Distinct {
            field: field_name,
            count: seen.len() as u64,
        },

        AccumulatorKind::Rollup {
            inner,
            mut sub_accumulators,
            ..
        } => {
            let mode_str = match inner.mode {
                super::spec::RollupMode::Drive => "drive".to_owned(),
                super::spec::RollupMode::Path { depth } => format!("path(depth={depth})"),
                super::spec::RollupMode::Ancestor { record_idx } => {
                    format!("ancestor(record={record_idx})")
                }
            };
            let entries = inner.finalize();
            let rows: Vec<_> = entries
                .into_iter()
                .map(|(key, stats)| {
                    let key_str = if drives.is_empty() {
                        format!("{key}")
                    } else {
                        super::rollup::resolve_rollup_key(key, &inner.mode, drives[0])
                    };
                    let mut row = BucketRow::from_stats(key_str, stats, total_matched, total_bytes);

                    // Attach sub-aggregation from nested sub-accumulator.
                    if let Some(sub_acc) = sub_accumulators.as_mut().and_then(|m| m.remove(&key)) {
                        let sub_result =
                            finalize_accumulator(sub_acc, total_matched, total_bytes, drives, &[]);
                        row.sub_buckets = sub_result_to_bucket_rows(&sub_result);
                    }

                    row
                })
                .collect();
            AggregateResultData::Rollup {
                mode: mode_str,
                rows,
            }
        }

        AccumulatorKind::Duplicates { inner, sample_spec } => {
            let dup_top = 100; // default
            let mut result = inner.finalize(dup_top);
            // Materialize member_indices → SampleRows for each group.
            let spec = sample_spec.unwrap_or_default();
            let projection = spec.effective_projection();
            for group in &mut result.groups {
                group.sample_rows = group
                    .member_indices
                    .iter()
                    .map(|&(rec_idx, drive_ord)| {
                        let entry = super::sample_heap::SampleEntry {
                            sort_key: 0,
                            rec_idx: rec_idx as u32,
                            drive_ordinal: drive_ord,
                        };
                        materialize_sample_entry(&entry, projection, drives)
                    })
                    .collect();
            }
            AggregateResultData::Duplicates { result }
        }
    };

    AggregateResult { label, data }
}

/// Build drill-down predicates for a bucket row.
///
/// Returns the original query predicates **plus** a bucket key predicate
/// that narrows to the specific group.
fn build_drilldown(
    query_predicates: &[DrilldownPredicate],
    bucket_field: Option<crate::search::field::FieldId>,
    _group_key: u64,
    display_key: &str,
    _drives: &[&DriveCompactIndex],
) -> Vec<DrilldownPredicate> {
    let mut preds = query_predicates.to_vec();

    // Add the bucket key predicate.
    if let Some(field) = bucket_field {
        let field_name = field.metadata().canonical_name.to_owned();
        preds.push(DrilldownPredicate {
            field: field_name,
            op: "eq".to_owned(),
            value: DrilldownValue::String(display_key.to_owned()),
        });
    }

    preds
}

/// Materialize a single sample entry into a [`SampleRow`].
///
/// Looks up the record by `(drive_ordinal, rec_idx)` in `drives`,
/// then projects the requested fields into key-value pairs.
fn materialize_sample_entry(
    entry: &super::sample_heap::SampleEntry,
    projection: &[crate::search::field::FieldId],
    drives: &[&DriveCompactIndex],
) -> SampleRow {
    let drive = drives.get(usize::from(entry.drive_ordinal));
    let record = drive.and_then(|d| d.records.get(entry.rec_idx as usize));

    let fields: Vec<(String, String)> = projection
        .iter()
        .map(|fid| {
            let name = fid.metadata().canonical_name.to_owned();
            let value = match (record, drive) {
                (Some(rec), Some(drv)) => format_field(*fid, rec, drv),
                _ => String::new(),
            };
            (name, value)
        })
        .collect();

    SampleRow {
        fields,
        sort_key: entry.sort_key,
    }
}

/// Format a single field value for sample row output.
fn format_field(
    field: crate::search::field::FieldId,
    record: &crate::compact::CompactRecord,
    drive: &DriveCompactIndex,
) -> String {
    use crate::search::field::FieldId;
    match field {
        FieldId::Name => record.name(&drive.names).to_owned(),
        FieldId::Size => record.size.to_string(),
        FieldId::SizeOnDisk => record.allocated.to_string(),
        FieldId::Modified => format_timestamp_key(record.modified),
        FieldId::Created => format_timestamp_key(record.created),
        FieldId::Accessed => format_timestamp_key(record.accessed),
        FieldId::Extension => {
            let ext_id = record.extension_id as usize;
            drive
                .ext_names
                .get(ext_id)
                .map(ToString::to_string)
                .unwrap_or_default()
        }
        FieldId::Path | FieldId::PathOnly => {
            // Full path resolution is expensive — return parent index
            // as a placeholder.  Callers needing full paths should use
            // the search pipeline instead.
            format!("parent_idx:{}", record.parent_idx)
        }
        FieldId::DirectoryFlag => {
            if record.flags & 0x0010 != 0 {
                "directory".to_owned()
            } else {
                "file".to_owned()
            }
        }
        FieldId::Hidden => format!("{}", record.flags & 0x0002 != 0),
        FieldId::System => format!("{}", record.flags & 0x0004 != 0),
        FieldId::ReadOnly => format!("{}", record.flags & 0x0001 != 0),
        FieldId::TreeSize => record.treesize.to_string(),
        FieldId::Descendants => record.descendants.to_string(),
        _ => String::new(),
    }
}

/// Resolve a u64 group key to a display string.
///
/// For `Extension`, the key encodes `(drive_ordinal << 16) | extension_id`.
/// The extension is resolved using the specific drive's intern table.
fn resolve_group_key(
    field: Option<crate::search::field::FieldId>,
    key: u64,
    drives: &[&DriveCompactIndex],
    ext_map: Option<&super::ExtensionMap>,
) -> String {
    use crate::search::field::FieldId;
    match field {
        Some(FieldId::Extension) => {
            // When an ExtensionMap is available, group keys are canonical
            // IDs that can be resolved directly.
            if let Some(map) = ext_map {
                return map.resolve(key);
            }
            // Legacy fallback: raw extension_id, first-drive lookup.
            let ext_id = key as u16;
            for drive in drives {
                if let Some(name) = drive.ext_names.get(usize::from(ext_id)) {
                    return name.to_string();
                }
            }
            format!("ext:{ext_id}")
        }
        Some(FieldId::Drive) => {
            let ch = char::from(key as u8);
            format!("{ch}:")
        }
        Some(FieldId::Type) => {
            format!("type:{key}")
        }
        Some(FieldId::DirectoryFlag) => {
            if key == 1 {
                "directory".to_owned()
            } else {
                "file".to_owned()
            }
        }
        Some(f) if f.metadata().field_type == crate::search::field::FieldType::Bool => {
            if key == 1 {
                "true".to_owned()
            } else {
                "false".to_owned()
            }
        }
        _ => format!("{key}"),
    }
}

/// Format a range bucket key.
fn format_range_key(index: usize, boundaries: &[u64]) -> String {
    if boundaries.is_empty() {
        return format!("bucket_{index}");
    }
    if index == 0 {
        format!("< {}", boundaries[0])
    } else if index >= boundaries.len() {
        format!(">= {}", boundaries[boundaries.len() - 1])
    } else {
        format!("{} - {}", boundaries[index - 1], boundaries[index])
    }
}

/// Format a timestamp key (epoch microseconds to ISO-ish date).
fn format_timestamp_key(ts_us: i64) -> String {
    let secs = ts_us / 1_000_000;
    let days = secs / 86400;
    let mut y = 1970_i64;
    let mut remaining = days;

    loop {
        let year_days = if is_leap(y) { 366 } else { 365 };
        if remaining < year_days {
            break;
        }
        remaining -= year_days;
        y += 1;
    }

    let leap = is_leap(y);
    let month_days: [i64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 1_u32;
    for &md in &month_days {
        if remaining < md {
            break;
        }
        remaining -= md;
        m += 1;
    }
    let d = remaining + 1;
    format!("{y:04}-{m:02}-{d:02}")
}

/// Check if a year is a leap year.
const fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}
