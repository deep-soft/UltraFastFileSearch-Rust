//! Rollup aggregations — hierarchical grouping by path or drive.
//!
//! A rollup groups records by a parent/ancestor directory at a given
//! depth from the drive root. This allows "top folders" analysis
//! without resolving full paths for every record.

use std::collections::HashMap;

use super::accumulators::StatsAccumulator;
use super::spec::RollupMode;
use crate::compact::{CompactRecord, DriveCompactIndex};

/// A rollup accumulator — groups records by a key derived from
/// path ancestry or drive letter.
#[derive(Debug, Clone)]
pub struct RollupAccumulator {
    /// Per-group statistics keyed by ancestor record index (or drive ordinal).
    pub groups: HashMap<u32, StatsAccumulator>,
    /// Rollup mode.
    pub mode: RollupMode,
    /// Max groups to track.
    pub top: u16,
}

impl RollupAccumulator {
    /// Create a new rollup accumulator.
    #[must_use]
    pub fn new(mode: RollupMode, top: u16) -> Self {
        Self {
            groups: HashMap::new(),
            mode,
            top,
        }
    }

    /// Feed a record into the rollup.
    #[inline]
    pub fn feed(&mut self, record: &CompactRecord, drive: &DriveCompactIndex, idx: usize) {
        let key = match self.mode {
            RollupMode::Drive => u32::from(drive.letter as u8),
            RollupMode::Path { depth } => ancestor_at_depth(record, drive, idx, depth),
        };

        let stats = self.groups.entry(key).or_default();
        stats.feed_value(record.size, record.allocated);
    }

    /// Merge another rollup accumulator.
    pub fn merge(&mut self, other: &Self) {
        for (&key, other_stats) in &other.groups {
            self.groups
                .entry(key)
                .and_modify(|s| s.merge(other_stats))
                .or_insert_with(|| other_stats.clone());
        }
    }

    /// Finalize: sort by total bytes descending, truncate to top-N.
    /// Returns (key, stats) pairs.
    #[must_use]
    pub fn finalize(&self) -> Vec<(u32, &StatsAccumulator)> {
        let mut entries: Vec<_> = self.groups.iter().map(|(&k, v)| (k, v)).collect();
        entries.sort_by(|a, b| b.1.sum.cmp(&a.1.sum));
        entries.truncate(self.top as usize);
        entries
    }
}

/// Walk the parent chain to find the ancestor at a given depth from root.
///
/// `depth=1` means the immediate child of the drive root.
/// Returns the record index of that ancestor, or `idx` itself if the
/// record is shallower than the requested depth.
fn ancestor_at_depth(
    _record: &CompactRecord,
    drive: &DriveCompactIndex,
    idx: usize,
    target_depth: u8,
) -> u32 {
    // Build the parent chain by walking up.
    let records = &drive.records;
    let mut chain: Vec<u32> = Vec::with_capacity(16);
    let mut current = idx as u32;

    // Walk up to root (parent_idx == 0 or self-referencing means root).
    loop {
        chain.push(current);
        let ci = current as usize;
        if ci >= records.len() {
            break;
        }
        let parent = records[ci].parent_idx;
        if parent == current || parent == 0 {
            break;
        }
        current = parent;
        if chain.len() > 255 {
            break; // Safety: prevent infinite loops
        }
    }

    // chain is [leaf, ..., root]. Reverse to get [root, ..., leaf].
    chain.reverse();

    // depth=1 → index 1 in chain (first child of root).
    let depth_idx = target_depth as usize;
    if depth_idx < chain.len() {
        chain[depth_idx]
    } else {
        // Record is shallower than requested depth — use itself.
        idx as u32
    }
}

/// Resolve a rollup key (record index) to a display name.
///
/// For drive rollups, key is the drive letter ordinal.
/// For path rollups, key is the record index → look up name.
#[must_use]
pub fn resolve_rollup_key(key: u32, mode: &RollupMode, drive: &DriveCompactIndex) -> String {
    match mode {
        RollupMode::Drive => {
            let ch = char::from(key as u8);
            format!("{ch}:")
        }
        RollupMode::Path { .. } => {
            let idx = key as usize;
            if idx < drive.names.len() {
                let name = &drive.names[idx];
                format!("{}:\\{}", drive.letter, name)
            } else {
                format!("record_{key}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollup_accumulator_drive_mode() {
        let acc = RollupAccumulator::new(RollupMode::Drive, 26);
        assert!(acc.groups.is_empty());
        assert_eq!(acc.top, 26);
    }

    #[test]
    fn rollup_accumulator_path_mode() {
        let acc = RollupAccumulator::new(RollupMode::Path { depth: 1 }, 30);
        assert_eq!(acc.mode, RollupMode::Path { depth: 1 });
    }
}
