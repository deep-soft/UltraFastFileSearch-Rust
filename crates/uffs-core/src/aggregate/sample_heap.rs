// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Per-bucket min-heap for tracking top-N sample records.
//!
//! During the scan phase, each bucket can optionally maintain a bounded
//! min-heap that tracks the top-N records by a caller-chosen sort key.
//! Only record index + drive ordinal + sort key are stored — no paths or
//! names — so the per-record cost is just 16 bytes.
//!
//! After the scan,
//! [`SampleHeap::drain_sorted`](crate::aggregate::sample_heap::SampleHeap::drain_sorted)
//! returns the entries in final display order (descending or ascending)
//! so they can be materialized into [`SampleRow`]s by the finalize step.

use alloc::collections::BinaryHeap;
use std::cmp::Reverse;

use super::spec::TopHitsSpec;
use crate::compact::CompactRecord;
use crate::search::field::FieldId;

/// A single entry stored in the per-bucket sample heap.
///
/// Intentionally small (16 bytes) so that thousands of buckets × 5 entries
/// each remain negligible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleEntry {
    /// Sort key extracted from the record at scan time.
    pub sort_key: i64,
    /// Index of the record within the drive's `records` array.
    pub rec_idx: u32,
    /// Drive ordinal (index into the `drives` Vec).
    pub drive_ordinal: u8,
}

impl PartialOrd for SampleEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SampleEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sort_key
            .cmp(&other.sort_key)
            .then_with(|| self.rec_idx.cmp(&other.rec_idx))
    }
}

/// Bounded min-heap that keeps the top-N records per bucket.
///
/// When `sort_desc` is `true`, we want the N **largest** sort keys.
/// A min-heap (via `BinaryHeap<Reverse<SampleEntry>>`) naturally evicts
/// the smallest key when full — exactly what we need for "top-N largest".
///
/// When `sort_desc` is `false`, we want the N **smallest** sort keys.
/// A max-heap (`BinaryHeap<SampleEntry>`) evicts the largest.
#[derive(Debug, Clone)]
pub struct SampleHeap {
    /// Maximum entries to keep.
    capacity: u8,
    /// Sort field used to extract the key from a record.
    sort_field: FieldId,
    /// `true` = keep largest (desc), `false` = keep smallest (asc).
    sort_desc: bool,
    /// Min-heap for desc mode (keeps largest keys, evicts smallest).
    heap_desc: BinaryHeap<Reverse<SampleEntry>>,
    /// Max-heap for asc mode (keeps smallest keys, evicts largest).
    heap_asc: BinaryHeap<SampleEntry>,
}

impl SampleHeap {
    /// Create a new sample heap from a [`TopHitsSpec`].
    #[must_use]
    pub fn from_spec(spec: &TopHitsSpec) -> Self {
        let cap = spec.count.clamp(1, 5);
        Self {
            capacity: cap,
            sort_field: spec.sort_field,
            sort_desc: spec.sort_desc,
            heap_desc: BinaryHeap::with_capacity(usize::from(cap) + 1),
            heap_asc: BinaryHeap::with_capacity(usize::from(cap) + 1),
        }
    }

    /// Extract the sort key from a record for the configured field.
    #[inline]
    fn extract_sort_key(field: FieldId, record: &CompactRecord) -> i64 {
        match field {
            FieldId::Size => record.size.cast_signed(),
            FieldId::SizeOnDisk => record.allocated.cast_signed(),
            FieldId::Modified => record.modified,
            FieldId::Created => record.created,
            FieldId::Accessed => record.accessed,
            FieldId::NameLength => i64::from(record.name_len),
            FieldId::PathLength => i64::from(record.path_len),
            FieldId::TreeSize => record.treesize.cast_signed(),
            FieldId::TreeAllocated => record.tree_allocated.cast_signed(),
            FieldId::Descendants => i64::from(record.descendants),
            // Boolean flags: 0 or 1.
            FieldId::DirectoryFlag => i64::from(record.flags & 0x0010 != 0),
            FieldId::Hidden => i64::from(record.flags & 0x0002 != 0),
            FieldId::System => i64::from(record.flags & 0x0004 != 0),
            FieldId::ReadOnly => i64::from(record.flags & 0x0001 != 0),
            FieldId::Compressed => i64::from(record.flags & 0x0800 != 0),
            FieldId::Encrypted => i64::from(record.flags & 0x4000 != 0),
            FieldId::Archive => i64::from(record.flags & 0x0020 != 0),
            _ => 0,
        }
    }

    /// Push a record into the heap, evicting if at capacity.
    #[inline]
    pub fn push(&mut self, record: &CompactRecord, rec_idx: u32, drive_ordinal: u8) {
        let sort_key = Self::extract_sort_key(self.sort_field, record);
        let entry = SampleEntry {
            sort_key,
            rec_idx,
            drive_ordinal,
        };
        self.push_entry(entry);
    }

    /// Push a pre-built [`SampleEntry`] into the heap, evicting if at
    /// capacity.  Shared code path for [`Self::push`] and [`Self::merge`].
    #[inline]
    fn push_entry(&mut self, entry: SampleEntry) {
        let cap = usize::from(self.capacity);

        if self.sort_desc {
            if self.heap_desc.len() < cap {
                self.heap_desc.push(Reverse(entry));
            } else if let Some(&Reverse(min)) = self.heap_desc.peek()
                && entry.sort_key > min.sort_key
            {
                self.heap_desc.pop();
                self.heap_desc.push(Reverse(entry));
            }
        } else if self.heap_asc.len() < cap {
            self.heap_asc.push(entry);
        } else if let Some(&max) = self.heap_asc.peek()
            && entry.sort_key < max.sort_key
        {
            self.heap_asc.pop();
            self.heap_asc.push(entry);
        }
    }

    /// Merge another sample heap into this one, preserving top-N by the
    /// shared sort key / direction.
    ///
    /// Used by the parallel aggregation reducer when per-drive scans
    /// each produce their own sample heap for a given bucket.  Both
    /// heaps must have been built from the same [`TopHitsSpec`] (same
    /// capacity, sort field, and sort direction); callers arrange this
    /// by constructing heaps from the same spec via [`Self::from_spec`].
    pub fn merge(&mut self, other: &Self) {
        if self.sort_desc {
            for Reverse(entry) in &other.heap_desc {
                self.push_entry(*entry);
            }
        } else {
            for entry in &other.heap_asc {
                self.push_entry(*entry);
            }
        }
    }

    /// Number of entries currently in the heap.
    #[must_use]
    pub fn len(&self) -> usize {
        if self.sort_desc {
            self.heap_desc.len()
        } else {
            self.heap_asc.len()
        }
    }

    /// Whether the heap is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drain all entries in **display order**.
    ///
    /// - `sort_desc=true`  → largest sort key first
    /// - `sort_desc=false` → smallest sort key first
    pub fn drain_sorted(&mut self) -> Vec<SampleEntry> {
        let mut entries: Vec<SampleEntry> = if self.sort_desc {
            self.heap_desc.drain().map(|Reverse(e)| e).collect()
        } else {
            self.heap_asc.drain().collect()
        };
        if self.sort_desc {
            entries.sort_by(|a, b| b.sort_key.cmp(&a.sort_key));
        } else {
            entries.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));
        }
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_spec(count: u8, sort_field: FieldId, sort_desc: bool) -> TopHitsSpec {
        TopHitsSpec::new(count, sort_field, sort_desc, vec![])
    }

    fn make_record(size: u64, flags: u32) -> CompactRecord {
        CompactRecord {
            size,
            allocated: size,
            created: size.cast_signed(),
            modified: size.cast_signed(),
            accessed: size.cast_signed(),
            flags,
            ..CompactRecord::default()
        }
    }

    #[test]
    fn desc_keeps_largest_n() {
        let spec = make_spec(3, FieldId::Size, true);
        let mut heap = SampleHeap::from_spec(&spec);
        for i in 0..10_u64 {
            let rec = make_record(i * 100, 0);
            heap.push(&rec, u32::try_from(i).unwrap_or(u32::MAX), 0);
        }
        assert_eq!(heap.len(), 3);
        let entries = heap.drain_sorted();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].sort_key, 900);
        assert_eq!(entries[1].sort_key, 800);
        assert_eq!(entries[2].sort_key, 700);
    }

    #[test]
    fn asc_keeps_smallest_n() {
        let spec = make_spec(3, FieldId::Size, false);
        let mut heap = SampleHeap::from_spec(&spec);
        for i in 0..10_u64 {
            let rec = make_record(i * 100, 0);
            heap.push(&rec, u32::try_from(i).unwrap_or(u32::MAX), 0);
        }
        assert_eq!(heap.len(), 3);
        let entries = heap.drain_sorted();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].sort_key, 0);
        assert_eq!(entries[1].sort_key, 100);
        assert_eq!(entries[2].sort_key, 200);
    }

    #[test]
    fn empty_heap() {
        let spec = make_spec(3, FieldId::Size, true);
        let mut heap = SampleHeap::from_spec(&spec);
        assert!(heap.is_empty());
        let entries = heap.drain_sorted();
        assert!(entries.is_empty());
    }

    #[test]
    fn fewer_than_capacity() {
        let spec = make_spec(5, FieldId::Size, true);
        let mut heap = SampleHeap::from_spec(&spec);
        let rec = make_record(42, 0);
        heap.push(&rec, 0, 0);
        let rec2 = make_record(99, 0);
        heap.push(&rec2, 1, 0);
        assert_eq!(heap.len(), 2);
        let entries = heap.drain_sorted();
        assert_eq!(entries[0].sort_key, 99);
        assert_eq!(entries[1].sort_key, 42);
    }

    #[test]
    fn boolean_sort_field() {
        let spec = make_spec(2, FieldId::DirectoryFlag, true);
        let mut heap = SampleHeap::from_spec(&spec);
        for i in 0..5 {
            let rec = make_record(100, 0x20); // files
            heap.push(&rec, i, 0);
        }
        for i in 5..10 {
            let rec = make_record(100, 0x10); // dirs
            heap.push(&rec, i, 0);
        }
        let entries = heap.drain_sorted();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].sort_key, 1);
        assert_eq!(entries[1].sort_key, 1);
    }

    #[test]
    fn multi_drive() {
        let spec = make_spec(3, FieldId::Size, true);
        let mut heap = SampleHeap::from_spec(&spec);
        heap.push(&make_record(100, 0), 0, 0);
        heap.push(&make_record(500, 0), 0, 1);
        heap.push(&make_record(300, 0), 0, 2);
        let entries = heap.drain_sorted();
        assert_eq!(entries[0].sort_key, 500);
        assert_eq!(entries[0].drive_ordinal, 1);
        assert_eq!(entries[1].sort_key, 300);
        assert_eq!(entries[1].drive_ordinal, 2);
    }
}
