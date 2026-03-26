//! Compact in-memory index for search backends.
//!
//! Replaces the full `MftIndex` (224 bytes/record) with a lean 72-byte
//! `CompactRecord` that covers 100% of sortable/filterable columns.
//! Full metadata (ADS, forensic fields) is resolved on-demand from the
//! `.uffs` cache file.
//!
//! See `docs/architecture/COMPACT_INDEX_DESIGN.md` for the full design.

use std::path::PathBuf;
use std::time::Instant;

use uffs_mft::index::MftIndex;

use crate::trigram::TrigramIndex;

/// Compact per-record data for in-memory search, filter, and sort.
///
/// 72 bytes per record (68 data + 4 tail padding for `#[repr(C)]` alignment).
/// Covers every column from the uffs CLI output.
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

    // ── u32 fields (4-byte aligned) ───────────────────────────────
    /// Byte offset into the names blob.
    pub name_offset: u32,
    /// Raw NTFS `FILE_ATTRIBUTE_*` flags.
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
    /// Directory flag bit in raw NTFS `FILE_ATTRIBUTE_DIRECTORY`.
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

// Compile-time size assertion.
const _: () = assert!(
    size_of::<CompactRecord>() == 72,
    "CompactRecord must be exactly 72 bytes"
);

/// A loaded drive with compact index.
pub struct DriveCompactIndex {
    /// Drive letter (e.g., 'C').
    pub letter: char,
    /// Compact records — one per MFT file/directory.
    pub records: Vec<CompactRecord>,
    /// All filenames concatenated (UTF-8 bytes).
    pub names: Vec<u8>,
    /// Lowercase copy of names for case-insensitive search.
    pub names_lower: Vec<u8>,
    /// Trigram inverted index built on `names_lower`.
    pub trigram: TrigramIndex,
    /// Children index: `children[i]` = compact indices of directory i's children.
    pub children: Vec<Vec<u32>>,
    /// Where this index was loaded from (for future refresh).
    pub source: IndexSource,
}

/// Where a drive index was loaded from.
#[derive(Clone)]
pub enum IndexSource {
    /// Raw/IOCP/compressed MFT file.
    MftFile(PathBuf),
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

/// Statistics from in-place USN patching.
#[derive(Debug, Clone, Default)]
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

/// Refresh a drive by reloading from its original source.
pub fn refresh_drive(drive: &DriveCompactIndex) -> anyhow::Result<(DriveCompactIndex, LoadTiming)> {
    match &drive.source {
        IndexSource::MftFile(path) => {
            if path.to_string_lossy().len() <= 2 {
                #[cfg(windows)]
                {
                    return load_live_drive(drive.letter, false);
                }
                #[cfg(not(windows))]
                {
                    anyhow::bail!("Cannot refresh live drive {}: on non-Windows", drive.letter);
                }
            }
            load_mft_file(path, Some(drive.letter), false)
        }
    }
}

/// Build a `DriveCompactIndex` from a loaded `MftIndex`.
///
/// Returns `(DriveCompactIndex, compact_build_ms, trigram_build_ms)`.
pub fn build_compact_index(
    drive_letter: char,
    index: &MftIndex,
) -> (DriveCompactIndex, u128, u128) {
    let compact_start = Instant::now();
    let record_count = index.records.len();

    let mut records = Vec::with_capacity(record_count);
    for record in &index.records {
        let name_ref = &record.first_name.name;
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

    let names = index.names.as_bytes().to_vec();
    let names_lower: Vec<u8> = index.names.to_ascii_lowercase().into_bytes();
    let compact_elapsed = compact_start.elapsed().as_millis();

    let tri_start = Instant::now();
    let trigram = build_name_trigram(&records, &names_lower);
    let tri_elapsed = tri_start.elapsed().as_millis();

    // Build children index from parent_idx (single pass).
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
fn build_name_trigram(records: &[CompactRecord], names_lower: &[u8]) -> TrigramIndex {
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

    TrigramIndex::build(&name_strings)
}

/// Cache TTL in seconds (10 minutes — same as Windows CLI).
const INDEX_TTL_SECONDS: u64 = 600;

/// Load an MFT file and build a compact index (cross-platform).
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
        tracing::info!(
            drive = %drive_letter,
            path = %mft_path.display(),
            "📖 Parsing MFT file"
        );
        let parsed = parse_raw_mft_to_index(mft_path, drive_letter)?;

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

    let (mut compact, compact_elapsed, tri_elapsed) = build_compact_index(drive_letter, &mft_index);
    compact.source = IndexSource::MftFile(mft_path.to_path_buf());

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

    Ok((
        compact,
        LoadTiming {
            mft: mft_elapsed,
            compact: compact_elapsed,
            trigram: tri_elapsed,
        },
    ))
}

/// Apply USN changes in-place to the compact index (<50ms for typical changes).
#[cfg(windows)]
pub fn apply_usn_patch(
    drive: &mut DriveCompactIndex,
    changes: &[uffs_mft::usn::FileChange],
    frs_to_compact: &[u32],
) -> PatchStats {
    let mut stats = PatchStats::default();

    for change in changes {
        let frs_usize = uffs_mft::frs_to_usize(change.frs);
        let compact_idx = frs_to_compact.get(frs_usize).copied().unwrap_or(u32::MAX);

        if change.deleted {
            if compact_idx == u32::MAX {
                stats.skipped += 1;
            } else if let Some(rec) = drive.records.get_mut(compact_idx as usize) {
                rec.name_len = 0;
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
                if let Some(rec) = drive.records.get_mut(compact_idx as usize) {
                    if rec.name_len == 0 && !change.filename.is_empty() {
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
                let name_start = drive.names.len();
                drive.names.extend_from_slice(change.filename.as_bytes());
                drive
                    .names_lower
                    .extend_from_slice(change.filename.to_ascii_lowercase().as_bytes());

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
                    extension_id: 0,
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

                let new_parent_frs = uffs_mft::frs_to_usize(change.parent_frs);
                let new_parent_compact = frs_to_compact
                    .get(new_parent_frs)
                    .copied()
                    .unwrap_or(u32::MAX);

                if new_parent_compact != rec.parent_idx {
                    let old_parent = rec.parent_idx;
                    if old_parent != u32::MAX {
                        if let Some(children) = drive.children.get_mut(old_parent as usize) {
                            children.retain(|&child| child != compact_idx);
                        }
                    }
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
            stats.skipped += 1;
        }
    }

    stats
}
