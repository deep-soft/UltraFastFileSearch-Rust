//! Aggregate specification types.
//!
//! An [`AggregateSpec`] describes a single aggregation operation to perform
//! during a search scan. Multiple specs can be composed to produce a rich
//! statistical profile in a single pass.

use crate::search::field::FieldId;

/// A single aggregation operation to compute during a search scan.
#[derive(Debug, Clone)]
pub struct AggregateSpec {
    /// What kind of aggregation to perform.
    pub kind: AggregateKind,
    /// Optional label for this aggregation in the output.
    pub label: Option<String>,
}

impl AggregateSpec {
    /// Create a new aggregate spec with the given kind.
    #[must_use]
    pub fn new(kind: AggregateKind) -> Self {
        Self { kind, label: None }
    }

    /// Create a new aggregate spec with a label.
    #[must_use]
    pub fn with_label(kind: AggregateKind, label: impl Into<String>) -> Self {
        Self {
            kind,
            label: Some(label.into()),
        }
    }
}

/// The kind of aggregation to compute.
#[derive(Debug, Clone)]
pub enum AggregateKind {
    /// Total count of matching records.
    Count,

    /// Statistical metrics for a numeric or timestamp field.
    Stats {
        /// Which field to compute statistics on.
        field: FieldId,
        /// Which metrics to compute (empty = all applicable).
        metrics: Vec<ScalarMetric>,
    },

    /// Group records by a field's values and compute per-group metrics.
    Terms {
        /// Which field to group by (must be `groupable`).
        field: FieldId,
        /// Maximum number of groups to return.
        top: u16,
        /// Metrics to compute per group (default: count + total_bytes).
        metrics: Vec<BucketMetric>,
    },

    /// Group records into fixed-size numeric buckets.
    Histogram {
        /// Which field to bucket (must have `bucket_support`).
        field: FieldId,
        /// Bucket interval (for numeric fields, in the field's unit).
        interval: u64,
        /// Metrics per bucket.
        metrics: Vec<BucketMetric>,
    },

    /// Group records by calendar-aligned time intervals.
    DateHistogram {
        /// Which timestamp field.
        field: FieldId,
        /// Calendar interval.
        calendar: CalendarInterval,
        /// Metrics per bucket.
        metrics: Vec<BucketMetric>,
    },

    /// Group records into explicit numeric ranges.
    Range {
        /// Which field (must have `bucket_support`).
        field: FieldId,
        /// Range boundaries (N boundaries → N+1 buckets).
        boundaries: Vec<u64>,
        /// Metrics per bucket.
        metrics: Vec<BucketMetric>,
    },

    /// Count records where a field has no value / is zero / is missing.
    Missing {
        /// Which field to check.
        field: FieldId,
    },

    /// Count distinct values for a field.
    Distinct {
        /// Which field.
        field: FieldId,
    },

    /// Rollup: group by path depth or drive, then compute sub-aggregates.
    Rollup {
        /// Rollup mode.
        mode: RollupMode,
        /// Maximum groups to return.
        top: u16,
        /// Metrics per group.
        metrics: Vec<BucketMetric>,
    },

    /// Duplicate candidate detection.
    Duplicates {
        /// Fields to use as composite group key.
        keys: Vec<FieldId>,
        /// Verification mode.
        verify: DuplicateVerify,
        /// Maximum duplicate groups to return.
        top: u16,
        /// Sample rows per group.
        sample: u8,
        /// Maximum groups to track (OOM guard).
        max_groups: u32,
    },
}

/// Rollup mode for path-based or drive-based rollups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RollupMode {
    /// Group by drive letter.
    Drive,
    /// Group by path at a specific depth from drive root.
    Path {
        /// Depth from drive root (1 = top-level folder).
        depth: u8,
    },
}

/// Duplicate verification mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DuplicateVerify {
    /// No verification — candidates only (fastest).
    None,
    /// Compare first N bytes of each file.
    FirstBytes {
        /// Bytes to compare (default: 4096).
        count: u32,
    },
    /// Full SHA-256 hash verification.
    Sha256,
}

/// Specification for sample rows within a bucket.
#[derive(Debug, Clone)]
pub struct TopHitsSpec {
    /// Number of sample rows per bucket (1–5).
    pub count: u8,
    /// Sort sample rows by this field (descending).
    pub sort_field: FieldId,
    /// Fields to include in sample row output.
    pub projection: Vec<FieldId>,
}

/// A scalar metric computed over a set of records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScalarMetric {
    /// Sum of values.
    Sum,
    /// Minimum value.
    Min,
    /// Maximum value.
    Max,
    /// Arithmetic mean.
    Avg,
    /// Count of records with a value for this field.
    ValueCount,
    /// Count of records missing a value for this field.
    MissingCount,
}

/// A metric computed per bucket/group in a terms, histogram, or range aggregation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BucketMetric {
    /// Number of records in the bucket.
    Count,
    /// Total logical size (sum of `size`).
    TotalBytes,
    /// Total allocated size (sum of `allocated`).
    TotalAllocated,
    /// Waste: `total_allocated - total_bytes`.
    WasteBytes,
    /// Waste percentage: `waste / total_allocated * 100`.
    WastePct,
    /// Average file size in this bucket.
    AvgSize,
    /// Minimum file size in this bucket.
    MinSize,
    /// Maximum file size in this bucket.
    MaxSize,
    /// Share of total record count (percentage).
    ShareOfTotalCount,
    /// Share of total bytes (percentage).
    ShareOfTotalBytes,
}


/// Calendar-aligned time intervals for date histogram aggregation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CalendarInterval {
    /// One hour.
    Hour,
    /// One day.
    Day,
    /// One ISO week (Monday-based).
    Week,
    /// One calendar month.
    Month,
    /// One calendar quarter (3 months).
    Quarter,
    /// One calendar year.
    Year,
}

impl CalendarInterval {
    /// Parse a calendar interval from a string.
    ///
    /// # Errors
    ///
    /// Returns `None` if the string is not a recognized interval.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "hour" | "h" | "hourly" => Some(Self::Hour),
            "day" | "d" | "daily" => Some(Self::Day),
            "week" | "w" | "weekly" => Some(Self::Week),
            "month" | "m" | "monthly" => Some(Self::Month),
            "quarter" | "q" | "quarterly" => Some(Self::Quarter),
            "year" | "y" | "yearly" | "annual" => Some(Self::Year),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_spec() {
        let spec = AggregateSpec::new(AggregateKind::Count);
        assert!(spec.label.is_none());
        assert!(matches!(spec.kind, AggregateKind::Count));
    }

    #[test]
    fn stats_spec() {
        let spec = AggregateSpec::with_label(
            AggregateKind::Stats {
                field: FieldId::Size,
                metrics: vec![ScalarMetric::Sum, ScalarMetric::Avg],
            },
            "size_stats",
        );
        assert_eq!(spec.label.as_deref(), Some("size_stats"));
    }

    #[test]
    fn terms_spec() {
        let spec = AggregateSpec::new(AggregateKind::Terms {
            field: FieldId::Extension,
            top: 50,
            metrics: vec![BucketMetric::Count, BucketMetric::TotalBytes],
        });
        if let AggregateKind::Terms { field, top, .. } = &spec.kind {
            assert_eq!(*field, FieldId::Extension);
            assert_eq!(*top, 50);
        } else {
            panic!("expected Terms");
        }
    }

    #[test]
    fn calendar_interval_parse() {
        assert_eq!(CalendarInterval::parse("month"), Some(CalendarInterval::Month));
        assert_eq!(CalendarInterval::parse("M"), Some(CalendarInterval::Month));
        assert_eq!(CalendarInterval::parse("yearly"), Some(CalendarInterval::Year));
        assert_eq!(CalendarInterval::parse("hourly"), Some(CalendarInterval::Hour));
        assert_eq!(CalendarInterval::parse("weekly"), Some(CalendarInterval::Week));
        assert_eq!(CalendarInterval::parse("quarterly"), Some(CalendarInterval::Quarter));
        assert_eq!(CalendarInterval::parse("daily"), Some(CalendarInterval::Day));
        assert!(CalendarInterval::parse("millennium").is_none());
    }

    #[test]
    fn all_scalar_metrics() {
        // Ensure all variants are distinct.
        let all = [
            ScalarMetric::Sum,
            ScalarMetric::Min,
            ScalarMetric::Max,
            ScalarMetric::Avg,
            ScalarMetric::ValueCount,
            ScalarMetric::MissingCount,
        ];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                assert_eq!(i == j, a == b);
            }
        }
    }

    #[test]
    fn all_bucket_metrics() {
        let all = [
            BucketMetric::Count,
            BucketMetric::TotalBytes,
            BucketMetric::TotalAllocated,
            BucketMetric::WasteBytes,
            BucketMetric::WastePct,
            BucketMetric::AvgSize,
            BucketMetric::MinSize,
            BucketMetric::MaxSize,
            BucketMetric::ShareOfTotalCount,
            BucketMetric::ShareOfTotalBytes,
        ];
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate() {
                assert_eq!(i == j, a == b);
            }
        }
    }

    #[test]
    fn range_spec() {
        let spec = AggregateSpec::new(AggregateKind::Range {
            field: FieldId::Size,
            boundaries: vec![1024, 1_048_576, 1_073_741_824],
            metrics: vec![BucketMetric::Count],
        });
        if let AggregateKind::Range { boundaries, .. } = &spec.kind {
            assert_eq!(boundaries.len(), 3);
        } else {
            panic!("expected Range");
        }
    }

    #[test]
    fn missing_and_distinct_specs() {
        let missing = AggregateSpec::new(AggregateKind::Missing {
            field: FieldId::Extension,
        });
        assert!(matches!(missing.kind, AggregateKind::Missing { .. }));

        let distinct = AggregateSpec::new(AggregateKind::Distinct {
            field: FieldId::Type,
        });
        assert!(matches!(distinct.kind, AggregateKind::Distinct { .. }));
    }
}