//! Accumulator types for the aggregation engine.
//!
//! Each [`GroupAccumulator`] tracks statistics for one logical aggregation
//! (count, stats, terms, histogram, etc.). During the scan phase,
//! `feed()` is called for every matching record. After scanning,
//! `finalize()` produces the data needed for the response.

use crate::compact::{CompactRecord, DriveCompactIndex};
use crate::search::field::FieldId;
use super::spec::{AggregateKind, BucketMetric, ScalarMetric};

/// Running statistics for a single group or global scope.
///
/// Tracks count, sum, min, max, and accumulates enough data to
/// compute avg. All values are stored as `u64` (sizes) or `i64`
/// (timestamps). The caller is responsible for interpreting the
/// type based on the source `FieldId`.
#[derive(Debug, Clone)]
pub struct StatsAccumulator {
    /// Number of records in this group.
    pub count: u64,
    /// Sum of values (meaningful for size fields).
    pub sum: u64,
    /// Minimum value seen.
    pub min: u64,
    /// Maximum value seen.
    pub max: u64,
    /// Sum of allocated sizes (for waste calculation).
    pub sum_allocated: u64,
}

impl StatsAccumulator {
    /// Create a new empty stats accumulator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            count: 0,
            sum: 0,
            min: u64::MAX,
            max: 0,
            sum_allocated: 0,
        }
    }

    /// Feed a value from a record.
    #[inline]
    pub fn feed_value(&mut self, value: u64, allocated: u64) {
        self.count += 1;
        self.sum += value;
        if value < self.min {
            self.min = value;
        }
        if value > self.max {
            self.max = value;
        }
        self.sum_allocated += allocated;
    }

    /// Merge another accumulator into this one.
    pub fn merge(&mut self, other: &Self) {
        self.count += other.count;
        self.sum += other.sum;
        if other.min < self.min {
            self.min = other.min;
        }
        if other.max > self.max {
            self.max = other.max;
        }
        self.sum_allocated += other.sum_allocated;
    }

    /// Compute the average value (returns 0 if count is 0).
    #[must_use]
    pub fn avg(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.sum as f64 / self.count as f64
        }
    }

    /// Compute waste bytes: `sum_allocated - sum`.
    #[must_use]
    pub fn waste_bytes(&self) -> u64 {
        self.sum_allocated.saturating_sub(self.sum)
    }

    /// Compute waste percentage.
    #[must_use]
    pub fn waste_pct(&self) -> f64 {
        if self.sum_allocated == 0 {
            0.0
        } else {
            self.waste_bytes() as f64 / self.sum_allocated as f64 * 100.0
        }
    }
}

impl Default for StatsAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

/// A group accumulator tracks statistics for one aggregation spec.
///
/// This is the main workhorse of the aggregation engine. It's
/// constructed from an `AggregateKind` and fed records during scanning.
///
/// Different kinds use different internal strategies:
/// - `Count`: just a counter
/// - `Stats`: a `StatsAccumulator`
/// - `Terms`: a map from key to `StatsAccumulator`
/// - `Histogram`/`DateHistogram`/`Range`: array of `StatsAccumulator`
/// - `Missing`/`Distinct`: specialized counters
#[derive(Debug, Clone)]
pub struct GroupAccumulator {
    /// What this accumulator computes.
    pub kind: AccumulatorKind,
    /// The source field (if applicable).
    pub field: Option<FieldId>,
    /// Label for output.
    pub label: Option<String>,
}

/// The internal accumulator strategy.
#[derive(Debug, Clone)]
pub enum AccumulatorKind {
    /// Simple record count.
    Count {
        /// Running count.
        count: u64,
    },
    /// Scalar statistics for a single field.
    Stats {
        /// Running statistics.
        stats: StatsAccumulator,
        /// Which metrics were requested.
        metrics: Vec<ScalarMetric>,
    },
    /// Group-by terms: maps key → stats.
    Terms {
        /// Per-group accumulators, keyed by a u64-encoded group key.
        /// For extension_id: key = extension_id as u64.
        /// For drive: key = drive_letter as u64.
        /// For bool: key = 0 or 1.
        /// For type: key = category ordinal.
        groups: std::collections::HashMap<u64, StatsAccumulator>,
        /// Maximum groups to keep.
        top: u16,
        /// Requested metrics.
        metrics: Vec<BucketMetric>,
    },
    /// Fixed-size histogram buckets.
    Histogram {
        /// One accumulator per bucket (sorted by boundary).
        buckets: Vec<StatsAccumulator>,
        /// Bucket boundaries (upper exclusive).
        boundaries: Vec<u64>,
        /// Requested metrics.
        metrics: Vec<BucketMetric>,
    },
    /// Date histogram with calendar intervals.
    DateHistogram {
        /// Maps truncated-timestamp → stats.
        buckets: std::collections::BTreeMap<i64, StatsAccumulator>,
        /// Calendar interval for truncation.
        calendar: super::spec::CalendarInterval,
        /// Requested metrics.
        metrics: Vec<BucketMetric>,
    },
    /// Count of records with missing/zero values.
    Missing {
        /// Count of records with missing value.
        count: u64,
    },
    /// Distinct value count.
    Distinct {
        /// Set of seen values (as u64-encoded keys).
        seen: std::collections::HashSet<u64>,
    },
}

impl GroupAccumulator {
    /// Create a new accumulator for the given aggregate kind.
    #[must_use]
    pub fn from_kind(kind: &AggregateKind, label: Option<String>) -> Self {
        let (acc_kind, field) = match kind {
            AggregateKind::Count => (AccumulatorKind::Count { count: 0 }, None),
            AggregateKind::Stats { field, metrics } => (
                AccumulatorKind::Stats {
                    stats: StatsAccumulator::new(),
                    metrics: metrics.clone(),
                },
                Some(*field),
            ),
            AggregateKind::Terms {
                field,
                top,
                metrics,
            } => (
                AccumulatorKind::Terms {
                    groups: std::collections::HashMap::new(),
                    top: *top,
                    metrics: metrics.clone(),
                },
                Some(*field),
            ),
            AggregateKind::Histogram {
                field,
                interval: _,
                metrics,
            } => {
                // For now, use pre-defined size buckets; interval-based
                // histogram expansion happens in the planner.
                (
                    AccumulatorKind::Histogram {
                        buckets: Vec::new(),
                        boundaries: Vec::new(),
                        metrics: metrics.clone(),
                    },
                    Some(*field),
                )
            }
            AggregateKind::DateHistogram {
                field,
                calendar,
                metrics,
            } => (
                AccumulatorKind::DateHistogram {
                    buckets: std::collections::BTreeMap::new(),
                    calendar: *calendar,
                    metrics: metrics.clone(),
                },
                Some(*field),
            ),
            AggregateKind::Range {
                field,
                boundaries,
                metrics,
            } => {
                let bucket_count = boundaries.len() + 1;
                (
                    AccumulatorKind::Histogram {
                        buckets: (0..bucket_count)
                            .map(|_| StatsAccumulator::new())
                            .collect(),
                        boundaries: boundaries.clone(),
                        metrics: metrics.clone(),
                    },
                    Some(*field),
                )
            }
            AggregateKind::Missing { field } => {
                (AccumulatorKind::Missing { count: 0 }, Some(*field))
            }
            AggregateKind::Distinct { field } => (
                AccumulatorKind::Distinct {
                    seen: std::collections::HashSet::new(),
                },
                Some(*field),
            ),
        };

        Self {
            kind: acc_kind,
            field,
            label,
        }
    }

    /// Feed a record into this accumulator.
    ///
    /// The `idx` parameter is the record's index within the drive's
    /// `records` array, used to look up names and other per-record data.
    #[inline]
    pub fn feed(&mut self, record: &CompactRecord, drive: &DriveCompactIndex, _idx: usize) {
        let field = self.field;
        match &mut self.kind {
            AccumulatorKind::Count { count } => {
                *count += 1;
            }
            AccumulatorKind::Stats { stats, .. } => {
                let value = extract_value(field, record);
                stats.feed_value(value, record.allocated);
            }
            AccumulatorKind::Terms { groups, .. } => {
                let key = extract_group_key(field, record, drive);
                let stats = groups
                    .entry(key)
                    .or_insert_with(StatsAccumulator::new);
                stats.feed_value(record.size, record.allocated);
            }
            AccumulatorKind::Histogram {
                buckets,
                boundaries,
                ..
            } => {
                let value = extract_value(field, record);
                let bucket_idx = boundaries.partition_point(|&b| b <= value);
                // Grow buckets if needed.
                while buckets.len() <= bucket_idx {
                    buckets.push(StatsAccumulator::new());
                }
                buckets[bucket_idx].feed_value(record.size, record.allocated);
            }
            AccumulatorKind::DateHistogram {
                buckets, calendar, ..
            } => {
                let ts = extract_timestamp(field, record);
                let truncated = truncate_timestamp(ts, *calendar);
                let stats = buckets
                    .entry(truncated)
                    .or_insert_with(StatsAccumulator::new);
                stats.feed_value(record.size, record.allocated);
            }
            AccumulatorKind::Missing { count } => {
                if is_missing(field, record) {
                    *count += 1;
                }
            }
            AccumulatorKind::Distinct { seen } => {
                let key = extract_group_key(field, record, drive);
                seen.insert(key);
            }
        }
    }

    /// Merge another accumulator into this one (for cross-drive merging).
    pub fn merge(&mut self, other: &Self) {
        match (&mut self.kind, &other.kind) {
            (AccumulatorKind::Count { count: a }, AccumulatorKind::Count { count: b }) => {
                *a += b;
            }
            (
                AccumulatorKind::Stats { stats: a, .. },
                AccumulatorKind::Stats { stats: b, .. },
            ) => {
                a.merge(b);
            }
            (
                AccumulatorKind::Terms { groups: a, .. },
                AccumulatorKind::Terms { groups: b, .. },
            ) => {
                for (key, b_stats) in b {
                    a.entry(*key)
                        .and_modify(|a_stats| a_stats.merge(b_stats))
                        .or_insert_with(|| b_stats.clone());
                }
            }
            (
                AccumulatorKind::Histogram { buckets: a, .. },
                AccumulatorKind::Histogram { buckets: b, .. },
            ) => {
                while a.len() < b.len() {
                    a.push(StatsAccumulator::new());
                }
                for (i, b_stats) in b.iter().enumerate() {
                    a[i].merge(b_stats);
                }
            }
            (
                AccumulatorKind::DateHistogram { buckets: a, .. },
                AccumulatorKind::DateHistogram { buckets: b, .. },
            ) => {
                for (key, b_stats) in b {
                    a.entry(*key)
                        .and_modify(|a_stats| a_stats.merge(b_stats))
                        .or_insert_with(|| b_stats.clone());
                }
            }
            (AccumulatorKind::Missing { count: a }, AccumulatorKind::Missing { count: b }) => {
                *a += b;
            }
            (AccumulatorKind::Distinct { seen: a }, AccumulatorKind::Distinct { seen: b }) => {
                for key in b {
                    a.insert(*key);
                }
            }
            _ => {} // mismatched kinds — should not happen
        }
    }

}

/// Extract a numeric value from a record for stats/histogram.
#[inline]
fn extract_value(field: Option<FieldId>, record: &CompactRecord) -> u64 {
    match field {
        Some(FieldId::Size) => record.size,
        Some(FieldId::SizeOnDisk) => record.allocated,
        Some(FieldId::TreeSize) => record.treesize,
        Some(FieldId::TreeAllocated) => record.tree_allocated,
        Some(FieldId::Descendants) => u64::from(record.descendants),
        Some(FieldId::NameLength) => u64::from(record.name_len),
        Some(FieldId::PathLength) => u64::from(record.path_len),
        Some(FieldId::Created) => record.created as u64,
        Some(FieldId::Modified) => record.modified as u64,
        Some(FieldId::Accessed) => record.accessed as u64,
        _ => 0,
    }
}

/// Extract a timestamp from a record.
#[inline]
fn extract_timestamp(field: Option<FieldId>, record: &CompactRecord) -> i64 {
    match field {
        Some(FieldId::Created) => record.created,
        Some(FieldId::Modified) => record.modified,
        Some(FieldId::Accessed) => record.accessed,
        _ => 0,
    }
}

/// Extract a group key (encoded as u64) from a record.
#[inline]
fn extract_group_key(field: Option<FieldId>, record: &CompactRecord, drive: &DriveCompactIndex) -> u64 {
    match field {
        Some(FieldId::Extension) => u64::from(record.extension_id),
        Some(FieldId::Drive) => u64::from(drive.letter as u32),
        Some(FieldId::DirectoryFlag) => {
            if record.flags & 0x0010 != 0 { 1 } else { 0 }
        }
        Some(FieldId::Hidden) => if record.flags & 0x0002 != 0 { 1 } else { 0 },
        Some(FieldId::System) => if record.flags & 0x0004 != 0 { 1 } else { 0 },
        Some(FieldId::ReadOnly) => if record.flags & 0x0001 != 0 { 1 } else { 0 },
        Some(FieldId::Compressed) => if record.flags & 0x0800 != 0 { 1 } else { 0 },
        Some(FieldId::Encrypted) => if record.flags & 0x4000 != 0 { 1 } else { 0 },
        Some(FieldId::Archive) => if record.flags & 0x0020 != 0 { 1 } else { 0 },
        Some(FieldId::Sparse) => if record.flags & 0x0200 != 0 { 1 } else { 0 },
        Some(FieldId::Reparse) => if record.flags & 0x0400 != 0 { 1 } else { 0 },
        Some(FieldId::Temporary) => if record.flags & 0x0100 != 0 { 1 } else { 0 },
        Some(FieldId::Offline) => if record.flags & 0x1000 != 0 { 1 } else { 0 },
        Some(FieldId::NotIndexed) => if record.flags & 0x2000 != 0 { 1 } else { 0 },
        Some(FieldId::Virtual) => if record.flags & 0x10000 != 0 { 1 } else { 0 },
        Some(FieldId::Integrity) => if record.flags & 0x8000 != 0 { 1 } else { 0 },
        Some(FieldId::NoScrub) => if record.flags & 0x20000 != 0 { 1 } else { 0 },
        Some(FieldId::Pinned) => if record.flags & 0x80000 != 0 { 1 } else { 0 },
        Some(FieldId::Unpinned) => if record.flags & 0x100000 != 0 { 1 } else { 0 },
        Some(FieldId::RecallOnOpen) => if record.flags & 0x40000 != 0 { 1 } else { 0 },
        Some(FieldId::RecallOnDataAccess) => {
            if record.flags & 0x400000 != 0 { 1 } else { 0 }
        }
        _ => 0,
    }
}

/// Check if a field has a "missing" value for this record.
#[inline]
fn is_missing(field: Option<FieldId>, record: &CompactRecord) -> bool {
    match field {
        Some(FieldId::Extension) => record.extension_id == 0,
        Some(FieldId::Size) => record.size == 0,
        Some(FieldId::SizeOnDisk) => record.allocated == 0,
        Some(FieldId::Created) => record.created == 0,
        Some(FieldId::Modified) => record.modified == 0,
        Some(FieldId::Accessed) => record.accessed == 0,
        _ => false,
    }
}

/// Truncate a microsecond timestamp to a calendar interval boundary.
fn truncate_timestamp(ts_us: i64, calendar: super::spec::CalendarInterval) -> i64 {
    use super::spec::CalendarInterval;
    let secs = ts_us / 1_000_000;
    let truncated_secs = match calendar {
        CalendarInterval::Hour => (secs / 3600) * 3600,
        CalendarInterval::Day => (secs / 86400) * 86400,
        CalendarInterval::Week => {
            // Align to Monday (Unix epoch 1970-01-01 was Thursday, day 4).
            let days = secs / 86400;
            let day_of_week = (days + 3) % 7; // Mon=0
            (days - day_of_week) * 86400
        }
        CalendarInterval::Month => {
            // Approximate: 30-day months. For exact calendar alignment,
            // use chrono in a later stage.
            (secs / 2_592_000) * 2_592_000
        }
        CalendarInterval::Quarter => {
            (secs / 7_776_000) * 7_776_000
        }
        CalendarInterval::Year => {
            (secs / 31_536_000) * 31_536_000
        }
    };
    truncated_secs * 1_000_000
}
