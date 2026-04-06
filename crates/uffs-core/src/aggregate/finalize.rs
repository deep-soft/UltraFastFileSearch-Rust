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
}

impl Default for FinalizeOptions {
    fn default() -> Self {
        Self {
            compute_shares: true,
            include_empty_buckets: false,
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
    /// Grouped bucket results (terms, histogram, date_histogram, range).
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
        }
    }
}

/// Finalize accumulated results into a response.
pub(crate) fn finalize(
    accumulators: Vec<GroupAccumulator>,
    _plan: &AggregatePlan,
    drives: &[&DriveCompactIndex],
    options: &FinalizeOptions,
    total_matched: u64,
) -> AggregateResponse {
    let global_total_bytes = compute_global_bytes(&accumulators);

    let results = accumulators
        .into_iter()
        .map(|acc| finalize_one(acc, total_matched, global_total_bytes, options, drives))
        .collect();

    AggregateResponse { results }
}

/// Compute total bytes across all accumulators (for share-of-total).
fn compute_global_bytes(accumulators: &[GroupAccumulator]) -> u64 {
    for acc in accumulators {
        if let AccumulatorKind::Stats { stats, .. } = &acc.kind {
            if acc.field == Some(crate::search::field::FieldId::Size) {
                return stats.sum;
            }
        }
    }
    0
}

/// Finalize a single accumulator into an `AggregateResult`.
fn finalize_one(
    acc: GroupAccumulator,
    total_matched: u64,
    total_bytes: u64,
    options: &FinalizeOptions,
    drives: &[&DriveCompactIndex],
) -> AggregateResult {
    let label = acc.label.clone();
    let field_name = acc
        .field
        .map(|f| f.metadata().canonical_name.to_string())
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

        AccumulatorKind::Terms { groups, top, .. } => {
            let total_groups = groups.len();
            let mut rows: Vec<_> = groups
                .iter()
                .map(|(&key, stats)| {
                    let key_str = resolve_group_key(acc.field, key, drives);
                    BucketRow::from_stats(key_str, stats, total_matched, total_bytes)
                })
                .collect();

            rows.sort_by(|a, b| b.count.cmp(&a.count));

            let limit = top as usize;
            let other_count = if rows.len() > limit {
                let other: u64 = rows[limit..].iter().map(|r| r.count).sum();
                rows.truncate(limit);
                other
            } else {
                0
            };

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
    };

    AggregateResult { label, data }
}

/// Resolve a u64 group key to a display string.
fn resolve_group_key(
    field: Option<crate::search::field::FieldId>,
    key: u64,
    drives: &[&DriveCompactIndex],
) -> String {
    use crate::search::field::FieldId;
    match field {
        Some(FieldId::Extension) => {
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
                "directory".to_string()
            } else {
                "file".to_string()
            }
        }
        Some(f) if f.metadata().field_type == crate::search::field::FieldType::Bool => {
            if key == 1 {
                "true".to_string()
            } else {
                "false".to_string()
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
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
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