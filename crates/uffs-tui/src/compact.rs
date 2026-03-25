//! Compact in-memory index for the TUI search backend.
//!
//! Replaces the full `MftIndex` (224 bytes/record) with a lean 68-byte
//! `CompactRecord` that covers 100% of sortable/filterable columns.
//! Full metadata (ADS, forensic fields) is resolved on-demand from the
//! `.uffs` cache file.
//!
//! See `docs/architecture/COMPACT_INDEX_DESIGN.md` for the full design.

use std::path::PathBuf;
use std::time::Instant;

use uffs_mft::index::MftIndex;

/// Compact per-record data for in-memory search, filter, and sort.
///
/// 68 bytes per record. Covers every column from the uffs CLI output:
/// Name, Path (via parent chain), Size, Size on Disk, Created, Last Written,
/// Last Accessed, Descendants, Treesize, and all NTFS boolean attributes.
///
/// Only fields NOT included (resolved from `.uffs` on demand):
/// - Alternate Data Streams (ADS)
/// - Reparse tag (`u32` — rare filter target)
/// - Forensic fields (`sequence_number`, LSN, `base_frs`)
/// - `$FILE_NAME` timestamps (`fn_created`, `fn_modified`, etc.)
/// - Internal stream sizes
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct CompactRecord {
    // ── u64 fields first (8-byte aligned) ─────────────────────────
    /// Logical file size in bytes.
    pub size: u64,
    /// Allocated size on disk in bytes ("Size on Disk" column).
    pub allocated: u64,
    /// Sum of logical file sizes in entire subtree.
    pub treesize: u64,
    /// Creation time (Unix microseconds).
    pub created: i64,
    /// Last write time (Unix microseconds).
    pub modified: i64,
    /// Last access time (Unix microseconds).
    pub accessed: i64,

    // ── u32 fields (4-byte aligned, no padding after u64 block) ───
    /// Byte offset into the names blob.
    pub name_offset: u32,
    /// NTFS attribute flags (full `u32` from `$STANDARD_INFORMATION`).
    ///
    /// Bit layout matches NTFS `FILE_ATTRIBUTE_*` constants:
    /// ```text
    ///   bit 0:  read_only        bit 11: compressed
    ///   bit 1:  hidden           bit 12: offline
    ///   bit 2:  system           bit 13: not_content_indexed
    ///   bit 4:  directory        bit 14: encrypted
    ///   bit 5:  archive          bit 15: integrity_stream
    ///   bit 8:  temporary        bit 17: no_scrub_data
    ///   bit 9:  sparse           bit 19: pinned
    ///   bit 10: reparse_point    bit 20: unpinned
    /// ```
    pub flags: u32,
    /// Index into the compact array of the parent directory.
    /// `u32::MAX` = root or orphan.
    pub parent_idx: u32,
    /// Count of all descendants in subtree. 0 for files.
    pub descendants: u32,

    // ── u16 fields (2-byte aligned) ───────────────────────────────
    /// UTF-8 byte length of the filename.
    pub name_len: u16,
    /// Interned extension ID (0 = no extension).
    pub extension_id: u16,
}

impl CompactRecord {
    /// Directory flag bit in the NTFS attributes.
    const DIRECTORY_BIT: u32 = 0x0010;

    /// Returns `true` if this record is a directory.
    #[inline]
    #[must_use]
    pub const fn is_directory(self) -> bool {
        self.flags & Self::DIRECTORY_BIT != 0
    }

    /// Get the name from a names blob.
    #[inline]
    #[must_use]
    pub fn name<'a>(&self, names: &'a [u8]) -> &'a str {
        let start = self.name_offset as usize;
        let end = start + self.name_len as usize;
        names
            .get(start..end)
            .and_then(|bytes| core::str::from_utf8(bytes).ok())
            .unwrap_or("")
    }
}

// Compile-time size assertion: 6×u64 (48) + 4×u32 (16) + 2×u16 (4) + 4 padding
// = 72 bytes. The 4 bytes tail padding is required by #[repr(C)] to align the
// struct to 8 bytes (its largest member is u64). 72 bytes × 25M records = 1.80
// GB — still excellent.
const _: () = assert!(
    size_of::<CompactRecord>() == 72,
    "CompactRecord must be exactly 72 bytes"
);

/// A loaded drive with compact index for the TUI.
pub struct DriveCompactIndex {
    /// Drive letter (e.g., 'C').
    pub letter: char,
    /// Compact records — one per MFT file/directory.
    pub records: Vec<CompactRecord>,
    /// All filenames concatenated (UTF-8 bytes). Each record references
    /// its name via `(name_offset, name_len)`.
    pub names: Vec<u8>,
    /// Lowercase copy of names for case-insensitive search.
    pub names_lower: Vec<u8>,
    /// Trigram inverted index built on `names_lower`.
    pub trigram: super::backend::TrigramIndex,
    /// Children index: `children[i]` = compact indices of directory i's
    /// children. Empty vec for files. Built from `parent_idx` in a single
    /// pass.
    pub children: Vec<Vec<u32>>,
    /// Where this index was loaded from (for future refresh).
    pub source: IndexSource,
}

/// Where a drive index was loaded from.
pub enum IndexSource {
    /// Raw/IOCP/compressed MFT file.
    MftFile(PathBuf),
}

/// Refresh a drive by reloading from its original source.
///
/// On Windows with `.uffs` cache: reloads cache + applies USN delta.
/// On Mac/Linux with MFT files: re-parses the raw MFT file.
///
/// Returns the new `DriveCompactIndex` with updated data + timing.
#[expect(
    clippy::single_call_fn,
    reason = "public API called from main.rs refresh thread"
)]
pub fn refresh_drive(drive: &DriveCompactIndex) -> anyhow::Result<(DriveCompactIndex, LoadTiming)> {
    match &drive.source {
        IndexSource::MftFile(path) => {
            if path.to_string_lossy().len() <= 2 {
                // Windows live drive (stored as "C:" placeholder) — reload via cache
                #[cfg(windows)]
                {
                    return load_live_drive(drive.letter, false);
                }
                #[cfg(not(windows))]
                {
                    anyhow::bail!("Cannot refresh live drive {}: on non-Windows", drive.letter);
                }
            }
            // MFT file path — re-parse
            load_mft_file(path, Some(drive.letter), false)
        }
    }
}

/// Statistics from in-place USN patching.
#[derive(Debug, Clone, Default)]
#[expect(
    dead_code,
    reason = "Windows-only USN patching; wired into refresh in future"
)]
pub struct PatchStats {
    /// Records marked as deleted (`name_len` zeroed).
    pub deleted: usize,
    /// New records appended.
    pub created: usize,
    /// Records with updated name/parent.
    pub renamed: usize,
    /// Changes skipped (FRS not in index, or no actionable change).
    pub skipped: usize,
}

/// Apply USN changes in-place to the compact index (<50ms for typical changes).
///
/// This is the fast path: patches compact records directly without rebuilding
/// the entire index. Handles deletes (zero name_len), creates (append to
/// records + names blob), and renames (append new name, update offset).
///
/// For size/metadata changes, only a full refresh provides updated values
/// since the USN journal doesn't carry the new size — just flags that it
/// changed.
///
/// Trigram index is NOT updated here — stale entries are filtered at verify
/// time. Children index is updated for reparented files.
#[cfg(windows)]
pub fn apply_usn_patch(
    drive: &mut DriveCompactIndex,
    changes: &[uffs_mft::usn::FileChange],
    frs_to_compact: &[u32],
) -> PatchStats {
    let mut stats = PatchStats::default();

    for change in changes {
        let frs_usize = uffs_mft::frs_to_usize(change.frs);

        // Look up compact index for this FRS
        let compact_idx = frs_to_compact.get(frs_usize).copied().unwrap_or(u32::MAX);

        if change.deleted {
            if compact_idx == u32::MAX {
                stats.skipped += 1;
            } else if let Some(rec) = drive.records.get_mut(compact_idx as usize) {
                // Mark as deleted: zero name_len so search skips it
                rec.name_len = 0;
                // Remove from parent's children list
                let parent = rec.parent_idx;
                if parent != u32::MAX {
                    if let Some(children) = drive.children.get_mut(parent as usize) {
                        children.retain(|&child| child != compact_idx);
                    }
                }
                stats.deleted += 1;
            }
        } else if change.created {
            if compact_idx != u32::MAX {
                // FRS already exists (re-create after delete) — just un-delete
                if let Some(rec) = drive.records.get_mut(compact_idx as usize) {
                    if rec.name_len == 0 && !change.filename.is_empty() {
                        // Restore with new name
                        let name_start = drive.names.len();
                        drive.names.extend_from_slice(change.filename.as_bytes());
                        drive
                            .names_lower
                            .extend_from_slice(change.filename.to_ascii_lowercase().as_bytes());
                        #[expect(
                            clippy::cast_possible_truncation,
                            reason = "name offset bounded by names blob size"
                        )]
                        {
                            rec.name_offset = name_start as u32;
                        }
                        rec.name_len = change.filename.len().min(u16::MAX as usize) as u16;
                    }
                }
                stats.skipped += 1;
            } else if !change.filename.is_empty() {
                // Truly new record — append
                let name_start = drive.names.len();
                drive.names.extend_from_slice(change.filename.as_bytes());
                drive
                    .names_lower
                    .extend_from_slice(change.filename.to_ascii_lowercase().as_bytes());

                // Resolve parent compact index
                let parent_frs_usize = uffs_mft::frs_to_usize(change.parent_frs);
                let parent_compact = frs_to_compact
                    .get(parent_frs_usize)
                    .copied()
                    .unwrap_or(u32::MAX);

                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "name offset and record count bounded by NTFS limits"
                )]
                let new_rec = CompactRecord {
                    name_offset: name_start as u32,
                    name_len: change.filename.len().min(u16::MAX as usize) as u16,
                    extension_id: 0, // Unknown until full refresh
                    flags: 0,
                    parent_idx: parent_compact,
                    size: 0,
                    allocated: 0,
                    created: 0,
                    modified: 0,
                    accessed: 0,
                    descendants: 0,
                    treesize: 0,
                };

                let new_idx = drive.records.len() as u32;
                drive.records.push(new_rec);
                drive.children.push(Vec::new());

                // Add to parent's children
                if parent_compact != u32::MAX {
                    if let Some(children) = drive.children.get_mut(parent_compact as usize) {
                        children.push(new_idx);
                    }
                }

                stats.created += 1;
            } else {
                stats.skipped += 1;
            }
        } else if change.renamed {
            if compact_idx == u32::MAX {
                stats.skipped += 1;
            } else if let Some(rec) = drive.records.get_mut(compact_idx as usize) {
                // Update name (append to names blob, old name becomes orphaned)
                if !change.filename.is_empty() {
                    let name_start = drive.names.len();
                    drive.names.extend_from_slice(change.filename.as_bytes());
                    drive
                        .names_lower
                        .extend_from_slice(change.filename.to_ascii_lowercase().as_bytes());
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "name offset bounded by names blob size"
                    )]
                    {
                        rec.name_offset = name_start as u32;
                    }
                    rec.name_len = change.filename.len().min(u16::MAX as usize) as u16;
                }

                // Update parent if changed (reparent)
                let new_parent_frs = uffs_mft::frs_to_usize(change.parent_frs);
                let new_parent_compact = frs_to_compact
                    .get(new_parent_frs)
                    .copied()
                    .unwrap_or(u32::MAX);

                if new_parent_compact != rec.parent_idx {
                    // Remove from old parent's children
                    let old_parent = rec.parent_idx;
                    if old_parent != u32::MAX {
                        if let Some(children) = drive.children.get_mut(old_parent as usize) {
                            children.retain(|&child| child != compact_idx);
                        }
                    }
                    // Add to new parent's children
                    rec.parent_idx = new_parent_compact;
                    if new_parent_compact != u32::MAX {
                        if let Some(children) = drive.children.get_mut(new_parent_compact as usize)
                        {
                            children.push(compact_idx);
                        }
                    }
                }

                stats.renamed += 1;
            }
        } else {
            // Size/metadata changes — USN doesn't carry new values.
            // These are picked up on the next full refresh.
            stats.skipped += 1;
        }
    }

    stats
}

/// Timing breakdown for the compact index build.
pub struct LoadTiming {
    /// Time to load/read the MFT (milliseconds).
    pub mft: u128,
    /// Time to build compact records from `MftIndex` (milliseconds).
    pub compact: u128,
    /// Time to build trigram index (milliseconds).
    pub trigram: u128,
}

/// Build a `DriveCompactIndex` from a loaded `MftIndex`.
///
/// Extracts the compact record for each file, copies the names blob,
/// builds `parent_idx` mappings, and constructs a trigram index on names.
/// The `MftIndex` can be dropped after this returns (the key memory savings).
///
/// Returns `(DriveCompactIndex, compact_build_ms, trigram_build_ms)`.
#[expect(
    clippy::single_call_fn,
    reason = "called from both load_live_drive and load_mft_file"
)]
pub fn build_compact_index(
    drive_letter: char,
    index: &MftIndex,
) -> (DriveCompactIndex, u128, u128) {
    let compact_start = Instant::now();

    let record_count = index.records.len();

    // Phase 1: Build FRS → compact-index mapping.
    // We need this to resolve parent_frs (a u64 FRS number) into a compact
    // array index (u32). The MftIndex already has frs_to_idx which maps
    // FRS → record index, and record indices ARE our compact indices (1:1).
    //
    // So: parent_frs → frs_to_idx[parent_frs] → compact index.

    // Phase 2: Extract compact records.
    let mut records = Vec::with_capacity(record_count);
    for record in &index.records {
        let name_ref = &record.first_name.name;
        // Resolve parent_idx: parent_frs → compact index via frs_to_idx
        let parent_idx = {
            let parent_frs = record.first_name.parent_frs;
            if parent_frs == record.frs
                || parent_frs == u64::from(uffs_mft::NO_ENTRY)
                || parent_frs == uffs_mft::ROOT_FRS
            {
                u32::MAX
            } else {
                let parent_usize = uffs_mft::frs_to_usize(parent_frs);
                index
                    .frs_to_idx
                    .get(parent_usize)
                    .copied()
                    .filter(|&idx| idx != uffs_mft::NO_ENTRY)
                    .unwrap_or(u32::MAX)
            }
        };

        records.push(CompactRecord {
            name_offset: name_ref.offset,
            name_len: name_ref.length(),
            extension_id: name_ref.extension_id(),
            flags: record.stdinfo.flags,
            parent_idx,
            size: record.first_stream.size.length,
            allocated: record.first_stream.size.allocated,
            created: record.stdinfo.created,
            modified: record.stdinfo.modified,
            accessed: record.stdinfo.accessed,
            descendants: record.descendants,
            treesize: record.treesize,
        });
    }

    // Phase 3: Copy names blob and build lowercase version.
    let names = index.names.as_bytes().to_vec();
    let names_lower: Vec<u8> = index.names.to_ascii_lowercase().into_bytes();

    // Filter out empty/root names for skipping in search.
    // (Records with empty names or "." get name_len=0 from MftIndex.)

    let compact_elapsed = compact_start.elapsed().as_millis();

    // Phase 4: Build trigram index on lowercase names.
    let tri_start = Instant::now();
    let trigram = build_name_trigram(&records, &names_lower);
    let tri_elapsed = tri_start.elapsed().as_millis();

    // Phase 5: Build children index from parent_idx (single pass).
    let mut children: Vec<Vec<u32>> = vec![Vec::new(); records.len()];
    for (idx, rec) in records.iter().enumerate() {
        let parent = rec.parent_idx;
        if parent != u32::MAX {
            if let Some(child_list) = children.get_mut(parent as usize) {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "record count bounded by NTFS limits, always fits u32"
                )]
                {
                    child_list.push(idx as u32);
                }
            }
        }
    }

    (
        DriveCompactIndex {
            letter: drive_letter,
            records,
            names,
            names_lower,
            trigram,
            children,
            source: IndexSource::MftFile(PathBuf::from(format!("{drive_letter}:"))),
        },
        compact_elapsed,
        tri_elapsed,
    )
}

/// Build a trigram index from compact records' names.
///
/// Unlike the old approach that built trigrams on full paths (80+ chars each),
/// this builds on filenames only (~15 chars average) — ~5× smaller postings.
#[expect(
    clippy::single_call_fn,
    reason = "separated for readability; trigram build is a distinct concern"
)]
fn build_name_trigram(
    records: &[CompactRecord],
    names_lower: &[u8],
) -> crate::backend::TrigramIndex {
    // Build a Vec<String> of lowercase names for each record, then delegate
    // to the existing TrigramIndex::build which expects &[String].
    // This is slightly wasteful (allocates temporary strings) but reuses
    // the proven parallel trigram builder. We can optimize later.
    let name_strings: Vec<String> = records
        .iter()
        .map(|rec| {
            let start = rec.name_offset as usize;
            let end = start + rec.name_len as usize;
            names_lower
                .get(start..end)
                .and_then(|bytes| core::str::from_utf8(bytes).ok())
                .unwrap_or("")
                .to_owned()
        })
        .collect();

    super::backend::TrigramIndex::build(&name_strings)
}

/// Resolve a record's full path by walking the parent chain in the compact
/// index.
///
/// Returns lowercase path like `c:\users\photos\beach.jpg`.
pub fn resolve_path(drive: &DriveCompactIndex, record_idx: usize, volume_prefix: &str) -> String {
    let mut components = Vec::with_capacity(8);
    let mut current_idx = record_idx;
    let mut depth = 0_u32;

    loop {
        if depth > 256 {
            break; // Prevent infinite loops
        }

        let Some(record) = drive.records.get(current_idx) else {
            break;
        };

        let name = record.name(&drive.names);
        if name.is_empty() || name == "." {
            break;
        }

        components.push(name);

        let parent = record.parent_idx;
        if parent == u32::MAX {
            break;
        }

        current_idx = parent as usize;
        depth += 1;
    }

    // Build path from components (reversed, since we walked child→parent)
    components.reverse();

    let mut path = String::with_capacity(
        volume_prefix.len() + components.iter().map(|comp| comp.len() + 1).sum::<usize>(),
    );
    path.push_str(volume_prefix);
    for (idx, component) in components.iter().enumerate() {
        path.push_str(component);
        if idx < components.len() - 1 {
            path.push('\\');
        }
    }

    path
}

// ============================================================================
// Phase 3b: Tree-based path search
// ============================================================================

/// Returns `true` if the pattern contains a path separator (`\` or `/`),
/// indicating it should be handled by tree search rather than name trigram.
#[must_use]
#[expect(
    clippy::single_call_fn,
    reason = "public API called from backend::search; separation keeps detection logic isolated"
)]
pub fn is_path_pattern(pattern: &str) -> bool {
    pattern.contains('\\') || pattern.contains('/')
}

/// Search using tree traversal for path patterns like `\photos\*.jpg`.
///
/// Strategy:
/// 1. Split pattern on path separators into segments
/// 2. Find directories matching intermediate segments via trigram + name verify
/// 3. Collect children of those directories
/// 4. Filter leaf matches on the final segment
///
/// Falls back to name search if the pattern can't be decomposed.
#[expect(
    clippy::single_call_fn,
    reason = "public API called from backend; separation keeps tree search isolated"
)]
pub fn tree_search(drive: &DriveCompactIndex, pattern_lower: &str, limit: usize) -> Vec<u32> {
    // Normalize separators to backslash, strip leading separator
    let normalized = pattern_lower.replace('/', "\\");
    let stripped = normalized.strip_prefix('\\').unwrap_or(&normalized);

    let segments: Vec<&str> = stripped.split('\\').filter(|seg| !seg.is_empty()).collect();

    if segments.is_empty() {
        return Vec::new();
    }

    // Single segment = just a name search, no tree walk needed
    let Some(first_segment) = segments.first() else {
        return Vec::new();
    };
    if segments.len() == 1 {
        return name_search(drive, first_segment, limit);
    }

    // Multi-segment: find directories matching all but the last segment,
    // then filter children by the last segment.
    //
    // Example: "photos\*.jpg" → find dirs named "photos", get their children,
    //          filter by "*.jpg" (or substring match)
    let Some(leaf_pattern) = segments.last() else {
        return Vec::new();
    };
    let dir_segments = segments.get(..segments.len() - 1).unwrap_or(&[]);

    // Start with all directories matching the first segment
    let mut candidate_dirs = find_dirs_by_name(drive, first_segment);

    // Walk through intermediate segments: for each candidate dir,
    // find children that are directories matching the next segment
    for &segment in dir_segments.get(1..).unwrap_or(&[]) {
        let mut next_dirs = Vec::new();
        for &dir_idx in &candidate_dirs {
            let dir_children = drive
                .children
                .get(dir_idx as usize)
                .map_or(&[][..], Vec::as_slice);
            for &child_idx in dir_children {
                if let Some(child_rec) = drive.records.get(child_idx as usize) {
                    if child_rec.is_directory() {
                        let child_name = child_rec.name(&drive.names_lower);
                        if name_matches(child_name, segment) {
                            next_dirs.push(child_idx);
                        }
                    }
                }
            }
        }
        candidate_dirs = next_dirs;
        if candidate_dirs.is_empty() {
            return Vec::new();
        }
    }

    // Now collect children of all matched directories, filtering by leaf pattern
    let mut results = Vec::new();
    for &dir_idx in &candidate_dirs {
        let dir_children = drive
            .children
            .get(dir_idx as usize)
            .map_or(&[][..], Vec::as_slice);
        for &child_idx in dir_children {
            if let Some(child_rec) = drive.records.get(child_idx as usize) {
                let child_name = child_rec.name(&drive.names_lower);
                if name_matches(child_name, leaf_pattern) {
                    results.push(child_idx);
                    if results.len() >= limit {
                        return results;
                    }
                }
            }
        }
    }

    results
}

/// Find all directory compact indices whose name matches a pattern.
#[expect(
    clippy::single_call_fn,
    reason = "separated for readability; directory search is a distinct concern"
)]
fn find_dirs_by_name(drive: &DriveCompactIndex, pattern: &str) -> Vec<u32> {
    // Try trigram first for 3+ char patterns
    let candidates = drive.trigram.search(pattern);

    if let Some(candidate_indices) = candidates {
        candidate_indices
            .iter()
            .filter(|&&idx| {
                let rec_idx = idx as usize;
                let Some(rec) = drive.records.get(rec_idx) else {
                    return false;
                };
                if !rec.is_directory() {
                    return false;
                }
                let dir_name = rec.name(&drive.names_lower);
                name_matches(dir_name, pattern)
            })
            .copied()
            .collect()
    } else {
        // Short pattern: linear scan for matching directories
        drive
            .records
            .iter()
            .enumerate()
            .filter(|(_, rec)| {
                if !rec.is_directory() {
                    return false;
                }
                let dir_name = rec.name(&drive.names_lower);
                name_matches(dir_name, pattern)
            })
            .map(|(idx, _)| {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "record count bounded by NTFS limits"
                )]
                {
                    idx as u32
                }
            })
            .collect()
    }
}

/// Simple name search by substring (used as single-segment fallback).
#[expect(
    clippy::single_call_fn,
    reason = "separated for readability; name search is a distinct concern"
)]
fn name_search(drive: &DriveCompactIndex, needle: &str, limit: usize) -> Vec<u32> {
    let candidates = drive.trigram.search(needle);

    if let Some(candidate_indices) = candidates {
        candidate_indices
            .iter()
            .filter(|&&idx| {
                let rec_idx = idx as usize;
                let Some(rec) = drive.records.get(rec_idx) else {
                    return false;
                };
                let name = rec.name(&drive.names_lower);
                !name.is_empty() && name.contains(needle)
            })
            .take(limit)
            .copied()
            .collect()
    } else {
        drive
            .records
            .iter()
            .enumerate()
            .filter(|(_, rec)| {
                let name = rec.name(&drive.names_lower);
                !name.is_empty() && name != "." && name.contains(needle)
            })
            .take(limit)
            .map(|(idx, _)| {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "record count bounded by NTFS limits"
                )]
                {
                    idx as u32
                }
            })
            .collect()
    }
}

/// Check if a name matches a pattern (substring or simple glob).
///
/// Supports:
/// - Substring: `"photos"` matches any name containing "photos"
/// - `*` prefix/suffix: `"*.jpg"` matches names ending in ".jpg"
/// - Plain `*`: matches everything
fn name_matches(name: &str, pattern: &str) -> bool {
    if name.is_empty() || name == "." {
        return false;
    }
    if pattern == "*" {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        if let Some(prefix) = suffix.strip_suffix('*') {
            // *xxx* → contains
            return name.contains(prefix);
        }
        // *.jpg → ends with
        return name.ends_with(suffix);
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        // photos* → starts with
        return name.starts_with(prefix);
    }
    // Exact substring match
    name.contains(pattern)
}

/// Cache TTL in seconds (10 minutes — same as Windows CLI).
const INDEX_TTL_SECONDS: u64 = 600;

/// Load an MFT file and build a compact index (cross-platform).
///
/// Mirrors the Windows `.uffs` cache flow:
/// 1. Check `.uffs` cache for this drive → if fresh, load from cache (fast)
/// 2. If stale/missing → parse raw MFT → `MftIndex` → **save `.uffs` cache** →
///    compact
/// 3. On subsequent loads, step 1 hits and skips the expensive MFT parse
///
/// This makes the Mac/Linux flow identical to Windows (except the MFT source).
pub fn load_mft_file(
    mft_path: &std::path::Path,
    drive: Option<char>,
    no_cache: bool,
) -> anyhow::Result<(DriveCompactIndex, LoadTiming)> {
    let drive_letter = drive.unwrap_or_else(|| {
        let stem = mft_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("X");
        stem.chars()
            .next()
            .filter(char::is_ascii_alphabetic)
            .map_or('X', |ch| ch.to_ascii_uppercase())
    });

    let mft_start = Instant::now();

    // Step 1: Try loading from .uffs cache (same as Windows), unless --no-cache
    let cached = if no_cache {
        None
    } else {
        uffs_mft::cache::load_cached_index(drive_letter, INDEX_TTL_SECONDS)
    };

    let mft_index = if let Some((cached_index, _header)) = cached {
        tracing::info!(
            drive = %drive_letter,
            records = cached_index.records.len(),
            "📦 Cache hit — loaded .uffs cache"
        );
        cached_index
    } else {
        // Step 2: Cache miss (or --no-cache) — parse raw MFT file
        tracing::info!(
            drive = %drive_letter,
            path = %mft_path.display(),
            "📖 Parsing MFT file"
        );
        let parsed = parse_raw_mft_to_index(mft_path, drive_letter)?;

        // Step 3: Save .uffs cache for next time (mirrors Windows flow)
        if let Err(err) = uffs_mft::cache::save_to_cache(&parsed, drive_letter, 0, 0, 0) {
            tracing::warn!(
                drive = %drive_letter,
                error = %err,
                "Failed to save .uffs cache"
            );
        } else {
            let cache_path = uffs_mft::cache::cache_file_path(drive_letter);
            tracing::info!(
                drive = %drive_letter,
                path = %cache_path.display(),
                "💾 Saved .uffs cache"
            );
        }

        parsed
    };
    let mft_elapsed = mft_start.elapsed().as_millis();

    // Build compact index from MftIndex, then MftIndex is dropped (key savings!)
    let (mut compact, compact_elapsed, tri_elapsed) = build_compact_index(drive_letter, &mft_index);
    compact.source = IndexSource::MftFile(mft_path.to_path_buf());
    // `mft_index` dropped here — frees ~800 MB per drive

    Ok((
        compact,
        LoadTiming {
            mft: mft_elapsed,
            compact: compact_elapsed,
            trigram: tri_elapsed,
        },
    ))
}

/// Parse a raw MFT file into an `MftIndex`.
///
/// Handles raw, IOCP capture, and compressed formats.
#[expect(
    clippy::single_call_fn,
    reason = "separated for readability; MFT parsing is a distinct concern from cache logic"
)]
fn parse_raw_mft_to_index(
    mft_path: &std::path::Path,
    drive_letter: char,
) -> anyhow::Result<MftIndex> {
    use uffs_mft::parse::{MftRecordMerger, apply_fixup, parse_record_full};

    let options = uffs_mft::raw::LoadRawOptions::default();
    let raw = uffs_mft::raw::load_raw_mft(mft_path, &options)?;
    let capacity = uffs_mft::frs_to_usize(raw.header.record_count);
    let mut merger = MftRecordMerger::with_capacity(capacity);

    for (frs, record_data) in raw.iter_records() {
        let mut record_buf = record_data.to_vec();
        if !apply_fixup(&mut record_buf) {
            continue;
        }
        merger.add_result(parse_record_full(&record_buf, frs));
    }

    let records = merger.merge();
    Ok(MftIndex::from_parsed_records(drive_letter, records))
}

/// Load a live NTFS drive and build a compact index (Windows only).
#[cfg(windows)]
pub fn load_live_drive(
    drive_letter: char,
    no_cache: bool,
) -> anyhow::Result<(DriveCompactIndex, LoadTiming)> {
    use anyhow::Context;

    const INDEX_TTL_SECONDS: u64 = 600;

    let mft_start = Instant::now();
    let rt = tokio::runtime::Runtime::new()?;
    let index = rt.block_on(async {
        let reader = uffs_mft::MftReader::open(drive_letter)
            .with_context(|| format!("Failed to open drive {drive_letter}:"))?;
        if no_cache {
            reader
                .read_all_index()
                .await
                .with_context(|| format!("Failed to read MFT fresh for drive {drive_letter}:"))
        } else {
            reader
                .read_index_cached(INDEX_TTL_SECONDS)
                .await
                .with_context(|| format!("Failed to read MFT for drive {drive_letter}:"))
        }
    })?;
    let mft_elapsed = mft_start.elapsed().as_millis();

    let (compact, compact_elapsed, tri_elapsed) = build_compact_index(drive_letter, &index);
    // `index` dropped here — frees ~800 MB per drive

    Ok((
        compact,
        LoadTiming {
            mft: mft_elapsed,
            compact: compact_elapsed,
            trigram: tri_elapsed,
        },
    ))
}
