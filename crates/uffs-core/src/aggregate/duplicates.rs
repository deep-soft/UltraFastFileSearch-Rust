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
                FieldId::Name => {
                    // Hash the lowercase name for the composite key
                    // (case-insensitive grouping — NTFS is case-preserving
                    // but case-insensitive).
                    let name = record.name(&drive.names);
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    for ch in name.chars() {
                        ch.to_ascii_lowercase().hash(&mut hasher);
                    }
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
            let key = CompositeKey::from_record(record, drive, &self.key_fields);
            if let Some(group) = self.groups.get_mut(&key) {
                group.add(record, idx, self.current_drive);
            }
            return;
        }

        let key = CompositeKey::from_record(record, drive, &self.key_fields);
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
    use uffs_mft::index::{IndexNameRef, MftIndex, ROOT_FRS, SizeInfo};

    use super::*;
    use crate::compact::build_compact_index;

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

    /// Build a synthetic drive with known duplicate files.
    ///
    /// Layout:
    ///   - root (dir)
    ///   - "readme.txt" (FRS 100, 500 bytes) — unique
    ///   - "data.bin"   (FRS 101, 1000 bytes) — duplicate (3 copies)
    ///   - "data.bin"   (FRS 102, 1000 bytes)
    ///   - "data.bin"   (FRS 103, 1000 bytes)
    ///   - "config.ini" (FRS 104, 200 bytes)  — duplicate (2 copies)
    ///   - "config.ini" (FRS 105, 200 bytes)
    fn build_dup_drive() -> DriveCompactIndex {
        let mut idx = MftIndex::new('T');

        // Root directory.
        let root_off = idx.add_name(".");
        let root = idx.get_or_create(ROOT_FRS);
        root.stdinfo.set_directory(true);
        root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
        root.first_name.parent_frs = ROOT_FRS;

        let add_file = |idx: &mut MftIndex, frs: u64, name: &str, size: u64| {
            let off = idx.add_name(name);
            let ext = idx.intern_extension(name);
            let rec = idx.get_or_create(frs);
            rec.first_name.name = IndexNameRef::new(off, name.len() as u16, true, ext);
            rec.first_name.parent_frs = ROOT_FRS;
            rec.first_stream.size = SizeInfo {
                length: size,
                allocated: size,
            };
            rec.stdinfo.flags = 0x20; // archive
        };

        add_file(&mut idx, 100, "readme.txt", 500);
        add_file(&mut idx, 101, "data.bin", 1000);
        add_file(&mut idx, 102, "data.bin", 1000);
        add_file(&mut idx, 103, "data.bin", 1000);
        add_file(&mut idx, 104, "config.ini", 200);
        add_file(&mut idx, 105, "config.ini", 200);

        let (drive, _, _) = build_compact_index('T', &idx);
        drive
    }

    // ── S4E.2: synthetic duplicates — group count + reclaimable ───

    #[test]
    fn synthetic_duplicates_group_count_and_reclaimable() {
        let drive = build_dup_drive();
        let mut acc = DuplicateAccumulator::new(
            vec![FieldId::Size, FieldId::Name],
            DuplicateVerify::None,
            100_000,
            3,
        );

        for (idx, rec) in drive.records.iter().enumerate() {
            acc.feed(rec, &drive, idx);
        }

        let result = acc.finalize(50);

        // Should have exactly 2 duplicate groups:
        //   1. data.bin (3 copies × 1000 bytes)
        //   2. config.ini (2 copies × 200 bytes)
        assert_eq!(result.candidate_groups, 2, "expected 2 duplicate groups");

        // Total duplicate files: 3 + 2 = 5
        assert_eq!(
            result.candidate_files, 5,
            "expected 5 total files across duplicate groups"
        );

        // Reclaimable:
        //   data.bin: 3×1000 - 1000 = 2000
        //   config.ini: 2×200 - 200 = 200
        //   total: 2200
        assert_eq!(
            result.total_reclaimable_bytes, 2200,
            "expected 2200 reclaimable bytes"
        );

        // Groups sorted by reclaimable desc: data.bin first.
        assert_eq!(result.groups.len(), 2);
        assert_eq!(
            result.groups[0].count, 3,
            "first group: data.bin (3 copies)"
        );
        assert_eq!(result.groups[0].file_size, 1000);
        assert_eq!(result.groups[0].reclaimable_bytes, 2000);
        assert_eq!(
            result.groups[1].count, 2,
            "second group: config.ini (2 copies)"
        );
        assert_eq!(result.groups[1].file_size, 200);
        assert_eq!(result.groups[1].reclaimable_bytes, 200);

        // Member indices captured (sample=3).
        assert_eq!(result.groups[0].member_indices.len(), 3);
        assert_eq!(result.groups[1].member_indices.len(), 2);
    }

    // ── S4E.3: singleton elimination ────────────────────────────

    #[test]
    fn singleton_elimination_no_false_duplicates() {
        let mut idx = MftIndex::new('T');

        // Root.
        let root_off = idx.add_name(".");
        let root = idx.get_or_create(ROOT_FRS);
        root.stdinfo.set_directory(true);
        root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
        root.first_name.parent_frs = ROOT_FRS;

        // 10 unique files — all different names and sizes.
        for i in 0..10_u64 {
            let name = format!("file_{i}.dat");
            let off = idx.add_name(&name);
            let ext = idx.intern_extension(&name);
            let rec = idx.get_or_create(100 + i);
            rec.first_name.name = IndexNameRef::new(off, name.len() as u16, true, ext);
            rec.first_name.parent_frs = ROOT_FRS;
            rec.first_stream.size = SizeInfo {
                length: (i + 1) * 100,
                allocated: (i + 1) * 512,
            };
            rec.stdinfo.flags = 0x20;
        }

        let (drive, _, _) = build_compact_index('T', &idx);
        let mut acc = DuplicateAccumulator::new(
            vec![FieldId::Size, FieldId::Name],
            DuplicateVerify::None,
            100_000,
            2,
        );

        for (i, rec) in drive.records.iter().enumerate() {
            acc.feed(rec, &drive, i);
        }

        let result = acc.finalize(50);

        // All files are unique → zero duplicate groups.
        assert_eq!(result.candidate_groups, 0, "no duplicates expected");
        assert_eq!(result.candidate_files, 0);
        assert_eq!(result.total_reclaimable_bytes, 0);
        assert!(result.groups.is_empty());
    }

    #[test]
    fn zero_byte_files_excluded_from_duplicates() {
        let mut idx = MftIndex::new('T');

        let root_off = idx.add_name(".");
        let root = idx.get_or_create(ROOT_FRS);
        root.stdinfo.set_directory(true);
        root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
        root.first_name.parent_frs = ROOT_FRS;

        // Two zero-byte files with same name — should NOT be duplicates.
        for i in 0..2_u64 {
            let name = "empty.txt";
            let off = idx.add_name(name);
            let ext = idx.intern_extension(name);
            let rec = idx.get_or_create(100 + i);
            rec.first_name.name = IndexNameRef::new(off, name.len() as u16, true, ext);
            rec.first_name.parent_frs = ROOT_FRS;
            rec.first_stream.size = SizeInfo {
                length: 0,
                allocated: 0,
            };
            rec.stdinfo.flags = 0x20;
        }

        let (drive, _, _) = build_compact_index('T', &idx);
        let mut acc = DuplicateAccumulator::new(
            vec![FieldId::Size, FieldId::Name],
            DuplicateVerify::None,
            100_000,
            2,
        );

        for (i, rec) in drive.records.iter().enumerate() {
            acc.feed(rec, &drive, i);
        }

        let result = acc.finalize(50);
        assert_eq!(result.candidate_groups, 0, "zero-byte files excluded");
    }

    #[test]
    fn directories_excluded_from_duplicates() {
        let mut idx = MftIndex::new('T');

        let root_off = idx.add_name(".");
        let root = idx.get_or_create(ROOT_FRS);
        root.stdinfo.set_directory(true);
        root.first_name.name = IndexNameRef::new(root_off, 1, true, IndexNameRef::NO_EXTENSION);
        root.first_name.parent_frs = ROOT_FRS;

        // Two directories with same name — should NOT be duplicates.
        for i in 0..2_u64 {
            let name = "subdir";
            let off = idx.add_name(name);
            let ext = idx.intern_extension(name);
            let rec = idx.get_or_create(100 + i);
            rec.stdinfo.set_directory(true);
            rec.stdinfo.flags = 0x10; // directory
            rec.first_name.name = IndexNameRef::new(off, name.len() as u16, true, ext);
            rec.first_name.parent_frs = ROOT_FRS;
            rec.first_stream.size = SizeInfo {
                length: 4096,
                allocated: 4096,
            };
        }

        let (drive, _, _) = build_compact_index('T', &idx);
        let mut acc = DuplicateAccumulator::new(
            vec![FieldId::Size, FieldId::Name],
            DuplicateVerify::None,
            100_000,
            2,
        );

        for (i, rec) in drive.records.iter().enumerate() {
            acc.feed(rec, &drive, i);
        }

        let result = acc.finalize(50);
        assert_eq!(result.candidate_groups, 0, "directories excluded");
    }

    // ── S4E.4: Windows verified duplicates ──────────────────────

    /// Integration test with real MFT data + file-content verification.
    ///
    /// Requires Windows with the test-tree created by
    /// `scripts/windows/create_mft_test_tree.ps1`.
    /// Run with: `cargo test -p uffs-core -- duplicates_verified_windows
    /// --ignored`
    #[test]
    #[ignore = "requires Windows with MFT test tree (create_mft_test_tree.ps1)"]
    #[cfg(windows)]
    fn duplicates_verified_windows() {
        use crate::compact_loader::{MftSource, load_drive};

        // Load C: drive index.
        let source = MftSource::live('C');
        let drive = load_drive(&source, false).expect("failed to load C: drive");

        let mut acc = DuplicateAccumulator::new(
            vec![FieldId::Size, FieldId::Name],
            DuplicateVerify::FirstBytes,
            500_000,
            5,
        );

        for (idx, rec) in drive.records.iter().enumerate() {
            acc.feed(rec, &drive, idx);
        }

        let result = acc.finalize(100);

        // On any real Windows install there should be known duplicates
        // (e.g., DLLs in System32 and SysWOW64 with same name+size).
        assert!(
            result.candidate_groups > 0,
            "a real Windows C: drive should contain duplicate files"
        );
        assert!(
            result.total_reclaimable_bytes > 0,
            "reclaimable bytes should be non-zero"
        );

        // Verify groups are sorted by reclaimable_bytes descending.
        for pair in result.groups.windows(2) {
            assert!(
                pair[0].reclaimable_bytes >= pair[1].reclaimable_bytes,
                "groups should be sorted by reclaimable_bytes desc"
            );
        }

        // Each group must have count ≥ 2.
        for g in &result.groups {
            assert!(g.count >= 2, "each group must have at least 2 files");
            assert!(g.file_size > 0, "zero-byte files should be excluded");
        }
    }
}
