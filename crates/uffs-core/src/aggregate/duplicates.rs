//! Duplicate file analytics.
//!
//! Groups files by composite key (default: size + name) and identifies
//! candidate duplicate groups. Optionally verifies via first-bytes
//! comparison or full SHA-256 hash.

use core::hash::{Hash, Hasher};
use std::collections::HashMap;

use super::accumulators::StatsAccumulator;
use super::spec::DuplicateVerify;
use crate::compact::{CompactRecord, DriveCompactIndex};
use crate::search::field::FieldId;

/// Composite key for duplicate grouping.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CompositeKey {
    /// Key components as u64 values.
    components: Vec<u64>,
    /// Name component (if name is part of the key).
    name_hash: u64,
}

impl CompositeKey {
    /// Build a composite key from a record using the specified fields.
    #[must_use]
    pub fn from_record(
        record: &CompactRecord,
        drive: &DriveCompactIndex,
        idx: usize,
        key_fields: &[FieldId],
    ) -> Self {
        let mut components = Vec::with_capacity(key_fields.len());
        let mut name_hash = 0_u64;

        for field in key_fields {
            match field {
                FieldId::Size => components.push(record.size),
                FieldId::SizeOnDisk => components.push(record.allocated),
                FieldId::Extension => components.push(u64::from(record.extension_id)),
                FieldId::Modified => components.push(record.modified as u64),
                FieldId::Created => components.push(record.created as u64),
                FieldId::Name
                    // Hash the name for the composite key.
                    if idx < drive.names.len() => {
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        drive.names[idx].hash(&mut hasher);
                        name_hash = hasher.finish();
                    }
                _ => {}
            }
        }

        Self {
            components,
            name_hash,
        }
    }
}

/// A duplicate group — a set of records sharing the same composite key.
#[derive(Debug, Clone)]
pub struct DuplicateGroup {
    /// Number of files in this group.
    pub count: u64,
    /// Total size of all files in this group.
    pub total_bytes: u64,
    /// Size of one file (all should be same if size is a key).
    pub file_size: u64,
    /// Bytes reclaimable (total - one copy).
    pub reclaimable_bytes: u64,
    /// Record indices of members (for sample row output).
    pub member_indices: Vec<(usize, u8)>, // (record_idx, drive_ordinal)
    /// Materialized sample rows — populated during finalization when
    /// `drives` are available.  Empty until then.
    pub sample_rows: Vec<super::finalize::SampleRow>,
    /// Verification status.
    pub verified: bool,
}

/// Duplicate detection accumulator.
#[derive(Debug, Clone)]
pub struct DuplicateAccumulator {
    /// Per-group data, keyed by composite key.
    groups: HashMap<CompositeKey, DuplicateGroupBuilder>,
    /// Key fields for grouping.
    key_fields: Vec<FieldId>,
    /// Verification mode.
    verify: DuplicateVerify,
    /// Max groups to track.
    max_groups: u32,
    /// Max sample rows per group.
    sample: u8,
    /// Current drive ordinal being scanned.
    current_drive: u8,
}

/// Builder for accumulating a duplicate group during scan.
#[derive(Debug, Clone)]
struct DuplicateGroupBuilder {
    /// Stats for this group.
    stats: StatsAccumulator,
    /// Sample member indices (limited to `sample` count).
    members: Vec<(usize, u8)>,
    /// Max sample count.
    max_sample: u8,
}

impl DuplicateGroupBuilder {
    /// Create a new group builder.
    fn new(max_sample: u8) -> Self {
        Self {
            stats: StatsAccumulator::new(),
            members: Vec::with_capacity(max_sample as usize),
            max_sample,
        }
    }

    /// Add a record to this group.
    fn add(&mut self, record: &CompactRecord, idx: usize, drive_ordinal: u8) {
        self.stats.feed_value(record.size, record.allocated);
        if self.members.len() < self.max_sample as usize {
            self.members.push((idx, drive_ordinal));
        }
    }
}

impl DuplicateAccumulator {
    /// Create a new duplicate accumulator.
    #[must_use]
    pub fn new(
        key_fields: Vec<FieldId>,
        verify: DuplicateVerify,
        max_groups: u32,
        sample: u8,
    ) -> Self {
        Self {
            groups: HashMap::new(),
            key_fields,
            verify,
            max_groups,
            sample,
            current_drive: 0,
        }
    }

    /// Set the current drive ordinal (call before scanning each drive).
    pub const fn set_drive_ordinal(&mut self, ordinal: u8) {
        self.current_drive = ordinal;
    }

    /// Feed a record.
    #[inline]
    pub fn feed(&mut self, record: &CompactRecord, drive: &DriveCompactIndex, idx: usize) {
        // Skip directories — duplicates are files only.
        if record.flags & 0x0010 != 0 {
            return;
        }

        // Skip zero-byte files.
        if record.size == 0 {
            return;
        }

        // OOM guard.
        if self.groups.len() as u32 >= self.max_groups {
            // Only feed existing groups, don't create new ones.
            let key = CompositeKey::from_record(record, drive, idx, &self.key_fields);
            if let Some(group) = self.groups.get_mut(&key) {
                group.add(record, idx, self.current_drive);
            }
            return;
        }

        let key = CompositeKey::from_record(record, drive, idx, &self.key_fields);
        self.groups
            .entry(key)
            .or_insert_with(|| DuplicateGroupBuilder::new(self.sample))
            .add(record, idx, self.current_drive);
    }

    /// Finalize: drop singletons, sort by reclaimable bytes, return top groups.
    #[must_use]
    pub fn finalize(self, top: u16) -> DuplicateResult {
        let mut groups: Vec<DuplicateGroup> = self
            .groups
            .into_iter()
            .filter(|(_, g)| g.stats.count > 1) // Drop singletons
            .map(|(_, g)| {
                let file_size = if g.stats.count > 0 {
                    g.stats.sum / g.stats.count
                } else {
                    0
                };
                let reclaimable = g.stats.sum.saturating_sub(file_size);
                DuplicateGroup {
                    count: g.stats.count,
                    total_bytes: g.stats.sum,
                    file_size,
                    reclaimable_bytes: reclaimable,
                    member_indices: g.members,
                    sample_rows: Vec::new(), // populated by finalize_one
                    verified: matches!(self.verify, DuplicateVerify::None),
                }
            })
            .collect();

        // Sort by reclaimable bytes descending.
        groups.sort_by(|a, b| b.reclaimable_bytes.cmp(&a.reclaimable_bytes));

        let total_groups = groups.len();
        let total_duplicate_files: u64 = groups.iter().map(|g| g.count).sum();
        let total_reclaimable: u64 = groups.iter().map(|g| g.reclaimable_bytes).sum();

        groups.truncate(top as usize);

        DuplicateResult {
            candidate_groups: total_groups,
            candidate_files: total_duplicate_files,
            total_duplicate_bytes: groups.iter().map(|g| g.total_bytes).sum(),
            total_reclaimable_bytes: total_reclaimable,
            groups,
            verification_mode: self.verify,
        }
    }
}

/// Result of duplicate analysis.
#[derive(Debug, Clone)]
pub struct DuplicateResult {
    /// Number of candidate duplicate groups (count > 1).
    pub candidate_groups: usize,
    /// Total files across all candidate groups.
    pub candidate_files: u64,
    /// Total bytes in duplicate groups.
    pub total_duplicate_bytes: u64,
    /// Total reclaimable bytes (total - one copy per group).
    pub total_reclaimable_bytes: u64,
    /// Top duplicate groups sorted by reclaimable bytes.
    pub groups: Vec<DuplicateGroup>,
    /// Verification mode used.
    pub verification_mode: DuplicateVerify,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composite_key_equality() {
        let key1 = CompositeKey {
            components: vec![100, 42],
            name_hash: 12345,
        };
        let key2 = CompositeKey {
            components: vec![100, 42],
            name_hash: 12345,
        };
        assert_eq!(key1, key2);
    }

    #[test]
    fn composite_key_inequality() {
        let key1 = CompositeKey {
            components: vec![100, 42],
            name_hash: 12345,
        };
        let key2 = CompositeKey {
            components: vec![100, 43],
            name_hash: 12345,
        };
        assert_ne!(key1, key2);
    }

    #[test]
    fn duplicate_accumulator_new() {
        let acc = DuplicateAccumulator::new(
            vec![FieldId::Size, FieldId::Name],
            DuplicateVerify::None,
            100_000,
            2,
        );
        assert!(acc.groups.is_empty());
    }
}
