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

use rayon::prelude::*;
use uffs_mft::index::MftIndex;

use crate::trigram::TrigramIndex;

/// Compact per-record data for in-memory search, filter, and sort.
///
/// 80 bytes per record (76 data + 4 explicit tail padding).
/// Derives `bytemuck::Pod` + `Zeroable` so the entire record array can be
/// serialized/deserialized as a single bulk `memcpy` — no per-field encoding.
#[derive(Debug, Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct CompactRecord {
    // ── u64 fields first (8-byte aligned) ─────────────────────────
    /// Logical file size in bytes.
    pub size: u64,
    /// Allocated size on disk in bytes ("Size on Disk" column).
    pub allocated: u64,
    /// Sum of logical file sizes in entire subtree.
    pub treesize: u64,
    /// Sum of allocated sizes in entire subtree.
    pub tree_allocated: u64,
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

    /// Explicit tail padding for 8-byte struct alignment.
    /// Required by `bytemuck::Pod` — no implicit padding allowed.
    #[expect(
        clippy::pub_underscore_fields,
        reason = "bytemuck Pod requires all fields same visibility"
    )]
    pub _pad: [u8; 4],
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
    size_of::<CompactRecord>() == 80,
    "CompactRecord must be exactly 80 bytes"
);

/// Children index in CSR (Compressed Sparse Row) layout.
///
/// `children(i)` returns the compact indices of record i's children as
/// a contiguous `&[u32]` slice.  The CSR layout avoids per-record `Vec`
/// allocations and enables bulk serialization/deserialization.
pub struct ChildrenIndex {
    /// CSR offsets — one per record + sentinel.  Length = `record_count` + 1.
    /// Children of record `i` are `values[offsets[i]..offsets[i+1]]`.
    offsets: Vec<u32>,
    /// Flat array of all child indices.
    values: Vec<u32>,
}

impl ChildrenIndex {
    /// Build from `CompactRecord::parent_idx` in two passes (count + scatter).
    #[must_use]
    pub fn build(records: &[CompactRecord]) -> Self {
        // Count children per parent
        let mut counts = vec![0_u32; records.len()];
        for rec in records {
            let parent = rec.parent_idx;
            if parent != u32::MAX {
                if let Some(cnt) = counts.get_mut(parent as usize) {
                    *cnt += 1;
                }
            }
        }

        // Prefix-sum → offsets
        let mut offsets = Vec::with_capacity(records.len() + 1);
        let mut running = 0_u32;
        for &cnt in &counts {
            offsets.push(running);
            running = running.saturating_add(cnt);
        }
        offsets.push(running);

        // Scatter children into values
        let mut values = vec![0_u32; running as usize];
        let mut write_pos = offsets.clone();
        for (idx, rec) in records.iter().enumerate() {
            let parent = rec.parent_idx;
            if parent != u32::MAX {
                if let Some(pos) = write_pos.get_mut(parent as usize) {
                    if let Some(slot) = values.get_mut(*pos as usize) {
                        #[expect(
                            clippy::cast_possible_truncation,
                            reason = "record count bounded by NTFS limits, fits u32"
                        )]
                        let child_idx = idx as u32;
                        *slot = child_idx;
                        *pos += 1;
                    }
                }
            }
        }

        Self { offsets, values }
    }

    /// Construct directly from pre-built CSR arrays (cache deserialization).
    #[must_use]
    pub const fn from_csr(offsets: Vec<u32>, values: Vec<u32>) -> Self {
        Self { offsets, values }
    }

    /// Borrow the CSR components for serialization.
    #[must_use]
    pub fn as_csr(&self) -> (&[u32], &[u32]) {
        (&self.offsets, &self.values)
    }

    /// Return the children of record `idx` as a contiguous slice.
    #[must_use]
    pub fn get(&self, idx: usize) -> &[u32] {
        let start = self.offsets.get(idx).copied().unwrap_or(0) as usize;
        let end = self.offsets.get(idx + 1).copied().unwrap_or(0) as usize;
        self.values.get(start..end).unwrap_or(&[])
    }

    /// Total number of child entries across all records.
    #[must_use]
    pub fn total_children(&self) -> usize {
        self.values.len()
    }

    /// Number of records tracked (one slot per record).
    #[must_use]
    pub fn record_count(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    /// Create an empty children index.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            offsets: vec![0],
            values: Vec::new(),
        }
    }
}

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
    /// CSR children index: `children.get(i)` → child indices of record i.
    pub children: ChildrenIndex,
    /// Where this index was loaded from (for future refresh).
    pub source: IndexSource,
    /// `MftIndex.build_epoch` this compact index was built from.
    /// Used as a staleness check when loading from cache.
    pub source_epoch: u64,
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

/// Where to read MFT data from.
#[derive(Debug, Clone)]
pub enum MftSource {
    /// Offline MFT file (`.uffs`, `.raw`, `.iocp` capture).
    /// Second field is an optional drive-letter override.
    File(PathBuf, Option<char>),
    /// Live Windows NTFS volume (e.g., `'C'`).
    #[cfg(windows)]
    Live(char),
}

impl MftSource {
    /// Returns the file path if this is a `File` source.
    #[must_use]
    pub fn file_path(&self) -> Option<&std::path::Path> {
        match self {
            Self::File(path, _) => Some(path),
            #[cfg(windows)]
            Self::Live(_) => None,
        }
    }
}

/// Unified entry point: load MFT data from any source and build a compact
/// index.
///
/// Handles compact cache → MFT cache → cold read → save caches,
/// with `[CACHE_PROFILE]` profiling when `UFFS_CACHE_PROFILE=1`.
///
/// # Errors
///
/// Returns an error if the MFT data cannot be read or parsed.
#[expect(clippy::print_stderr, reason = "UFFS_CACHE_PROFILE diagnostic output")]
pub fn load_drive(
    source: &MftSource,
    no_cache: bool,
) -> anyhow::Result<(DriveCompactIndex, LoadTiming)> {
    let drive_letter = match source {
        MftSource::File(path, drive_override) => drive_override.unwrap_or_else(|| {
            let stem = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("X");
            stem.chars()
                .next()
                .filter(char::is_ascii_alphabetic)
                .map_or('X', |ch| ch.to_ascii_uppercase())
        }),
        #[cfg(windows)]
        MftSource::Live(ch) => *ch,
    };

    // ── Fast path: compact cache hit ───────────────────────────────
    if !no_cache {
        if let Some(mut compact) =
            crate::compact_cache::load_compact_cache(drive_letter, INDEX_TTL_SECONDS, 0)
        {
            if let Some(path) = source.file_path() {
                compact.source = IndexSource::MftFile(path.to_path_buf());
            }
            tracing::info!(
                drive = %drive_letter,
                records = compact.records.len(),
                "📦 Cache hit — loaded compact cache"
            );
            return Ok((
                compact,
                LoadTiming {
                    mft: 0,
                    compact: 0,
                    trigram: 0,
                },
            ));
        }
    }

    // ── Load MftIndex (cache or cold) ──────────────────────────────
    let mft_start = Instant::now();
    let mft_index = match source {
        MftSource::File(path, _) => load_mft_index_from_file(path, drive_letter, no_cache)?,
        #[cfg(windows)]
        MftSource::Live(ch) => load_mft_index_live(*ch, no_cache)?,
    };
    let mft_elapsed = mft_start.elapsed().as_millis();

    // ── Build compact index ────────────────────────────────────────
    let (mut compact, compact_elapsed, tri_elapsed) = build_compact_index(drive_letter, &mft_index);
    if let Some(path) = source.file_path() {
        compact.source = IndexSource::MftFile(path.to_path_buf());
    }

    // ── Save compact cache (background, best-effort) ────────────────
    if !no_cache {
        let t_compact_save = Instant::now();
        if let Err(err) = crate::compact_cache::save_compact_cache_background(&compact) {
            tracing::warn!(drive = %drive_letter, error = %err, "Failed to start compact cache save");
        }
        let compact_save_ms = t_compact_save.elapsed().as_millis();
        if std::env::var_os("UFFS_CACHE_PROFILE").is_some() {
            eprintln!(
                "[CACHE_PROFILE] compact_save_submit: {compact_save_ms:>4} ms  (serialized, bg thread spawned)"
            );
        }
    }

    Ok((
        compact,
        LoadTiming {
            mft: mft_elapsed,
            compact: compact_elapsed,
            trigram: tri_elapsed,
        },
    ))
}

/// Load `MftIndex` from an offline file (cache → cold parse).
#[expect(
    clippy::single_call_fn,
    reason = "extracted for readability from load_drive"
)]
fn load_mft_index_from_file(
    mft_path: &std::path::Path,
    drive_letter: char,
    no_cache: bool,
) -> anyhow::Result<MftIndex> {
    let cached = if no_cache {
        None
    } else {
        uffs_mft::cache::load_cached_index(drive_letter, INDEX_TTL_SECONDS)
    };

    if let Some((cached_index, _header)) = cached {
        tracing::info!(
            drive = %drive_letter,
            records = cached_index.records.len(),
            "📦 Cache hit — loaded .uffs cache"
        );
        return Ok(cached_index);
    }

    tracing::info!(
        drive = %drive_letter,
        path = %mft_path.display(),
        "📖 Parsing MFT file (delegating to uffs-mft)"
    );

    // IOCP captures must use `load_iocp_to_index` (unified `process_record`
    // path) which mirrors the Windows LIVE inline parser exactly.  The generic
    // `load_raw_to_index_with_options` dispatches IOCP to
    // `load_iocp_capture_to_index` (MftRecordMerger multi-pass path) which
    // produces different `total_stream_count` values and therefore different
    // tree metrics (descendants, treesize) — a known parity divergence.
    let is_iocp = uffs_mft::is_iocp_capture(mft_path).unwrap_or(false);
    let parsed = if is_iocp {
        tracing::info!(
            drive = %drive_letter,
            "📼 IOCP capture detected — using unified process_record parser for parity"
        );
        uffs_mft::load_iocp_to_index(mft_path)?
    } else {
        let options = uffs_mft::raw::LoadRawOptions {
            header_only: false,
            volume_letter: Some(drive_letter),
            forensic: false,
        };
        uffs_mft::MftReader::load_raw_to_index_with_options(mft_path, &options)?
    };

    // Background save: serialize sync (~500ms), compress/encrypt/write in bg
    // thread.
    if let Err(err) = uffs_mft::cache::save_to_cache_background(&parsed, drive_letter, 0, 0, 0) {
        tracing::warn!(drive = %drive_letter, error = %err, "Failed to start .uffs cache save");
    } else {
        tracing::info!(drive = %drive_letter, "💾 MFT cache save started (background)");
    }

    Ok(parsed)
}

/// Load `MftIndex` from a live Windows volume (cache → cold read via IOCP).
#[cfg(windows)]
#[expect(
    clippy::single_call_fn,
    reason = "extracted for readability from load_drive"
)]
fn load_mft_index_live(drive_letter: char, no_cache: bool) -> anyhow::Result<MftIndex> {
    use anyhow::Context;

    let read_index = async {
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
    };

    // If we are already inside a Tokio runtime (e.g. CLI `#[tokio::main]`),
    // creating a new `Runtime` would panic with "Cannot start a runtime from
    // within a runtime".  Use `block_in_place` + the current handle instead.
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(read_index))
    } else {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(read_index)
    }
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
///
/// # Errors
///
/// Returns an error if the drive source cannot be reloaded.
pub fn refresh_drive(drive: &DriveCompactIndex) -> anyhow::Result<(DriveCompactIndex, LoadTiming)> {
    match &drive.source {
        IndexSource::MftFile(path) => {
            let source = if path.to_string_lossy().len() <= 2 {
                #[cfg(windows)]
                {
                    MftSource::Live(drive.letter)
                }
                #[cfg(not(windows))]
                {
                    anyhow::bail!("Cannot refresh live drive {}: on non-Windows", drive.letter);
                }
            } else {
                MftSource::File(path.clone(), Some(drive.letter))
            };
            load_drive(&source, false)
        }
    }
}

/// Expand hardlinks and ADS into additional `CompactRecord` entries.
///
/// Phase 2 (hardlinks): for each valid record with `name_count > 1`, walks the
/// link chain and creates additional records with alternate name/parent.
///
/// Phase 3 (ADS): for each valid record with `stream_count > 1`, creates
/// `CompactRecord`s for every `(name × stream)` combination. This includes
/// both primary and hardlink names — matching C++ baseline behavior.
#[expect(
    clippy::single_call_fn,
    reason = "Extracted to keep build_compact_index under the too_many_lines limit"
)]
fn expand_links_and_ads(
    index: &MftIndex,
    resolver: &uffs_mft::index::PathResolver,
    resolve_parent: &dyn Fn(u64, u64) -> u32,
    names: &mut Vec<u8>,
) -> Vec<CompactRecord> {
    let mut extra: Vec<CompactRecord> = Vec::new();

    for (idx, record) in index.records.iter().enumerate() {
        if !resolver.is_valid_idx(idx) {
            continue;
        }

        // Phase 2: hardlink expansion.
        if record.name_count > 1 {
            let mut link_entry = record.first_name.next_entry;
            while link_entry != uffs_mft::NO_ENTRY {
                let Some(link) = index.links.get(link_entry as usize) else {
                    break;
                };
                let link_parent = resolve_parent(link.parent_frs, record.frs);
                extra.push(CompactRecord {
                    name_offset: link.name.offset,
                    name_len: link.name.length(),
                    extension_id: link.name.extension_id(),
                    flags: record.stdinfo.flags,
                    parent_idx: link_parent,
                    size: record.first_stream.size.length,
                    allocated: record.first_stream.size.allocated,
                    created: record.stdinfo.created,
                    modified: record.stdinfo.modified,
                    accessed: record.stdinfo.accessed,
                    descendants: record.descendants,
                    treesize: record.treesize,
                    tree_allocated: record.tree_allocated,
                    _pad: [0; 4],
                });
                link_entry = link.next_entry;
            }
        }

        // Phase 3: ADS expansion (name × stream cross product).
        if record.stream_count <= 1 {
            continue;
        }

        // Collect all names for this record (primary + hardlinks).
        let mut all_names: Vec<(&str, u32)> = Vec::new();
        let primary_name = index.get_name(&record.first_name.name);
        if !primary_name.is_empty() {
            let pid = resolve_parent(record.first_name.parent_frs, record.frs);
            all_names.push((primary_name, pid));
        }
        if record.name_count > 1 {
            let mut le = record.first_name.next_entry;
            while le != uffs_mft::NO_ENTRY {
                let Some(lnk) = index.links.get(le as usize) else {
                    break;
                };
                let ln = index.get_name(&lnk.name);
                if !ln.is_empty() {
                    let lp = resolve_parent(lnk.parent_frs, record.frs);
                    all_names.push((ln, lp));
                }
                le = lnk.next_entry;
            }
        }

        // Walk output streams (skip default $DATA at head of chain).
        let mut se = record.first_stream.next_entry;
        while se != uffs_mft::NO_ENTRY {
            let Some(stream) = index.streams.get(se as usize) else {
                break;
            };
            if stream.is_output_stream() {
                let sn = index.stream_name(stream);
                if !sn.is_empty() {
                    for &(base_name, parent_idx) in &all_names {
                        let combined = format!("{base_name}:{sn}");
                        #[expect(
                            clippy::cast_possible_truncation,
                            reason = "names buffer < 4GB for any real volume"
                        )]
                        let name_offset = names.len() as u32;
                        #[expect(
                            clippy::cast_possible_truncation,
                            reason = "combined name length < 65535 chars"
                        )]
                        let name_len = combined.len() as u16;
                        names.extend_from_slice(combined.as_bytes());

                        extra.push(CompactRecord {
                            name_offset,
                            name_len,
                            extension_id: 0,
                            flags: record.stdinfo.flags,
                            parent_idx,
                            size: stream.size.length,
                            allocated: stream.size.allocated,
                            created: record.stdinfo.created,
                            modified: record.stdinfo.modified,
                            accessed: record.stdinfo.accessed,
                            descendants: 0,
                            treesize: 0,
                            tree_allocated: 0,
                            _pad: [0; 4],
                        });
                    }
                }
            }
            se = stream.next_entry;
        }
    }
    extra
}

/// Build a `DriveCompactIndex` from a loaded `MftIndex`.
///
/// Returns `(DriveCompactIndex, compact_build_ms, trigram_build_ms)`.
#[must_use]
pub fn build_compact_index(
    drive_letter: char,
    index: &MftIndex,
) -> (DriveCompactIndex, u128, u128) {
    use uffs_mft::index::PathResolver;

    let compact_start = Instant::now();

    // Build path resolver to determine which records are valid.
    // This filters out system metafiles (FRS 0-15 except root) and
    // propagates invalidity to descendants (e.g., $Extend children).
    let resolver = PathResolver::build(index, false);

    // Helper: resolve parent_frs → compact index.
    let resolve_parent = |parent_frs: u64, own_frs: u64| -> u32 {
        if parent_frs == own_frs
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

    // Phase 1: build primary compact records (parallel).
    let mut records: Vec<CompactRecord> = index
        .records
        .par_iter()
        .enumerate()
        .map(|(idx, record)| {
            // Skip invalid records (system metafiles + descendants).
            if !resolver.is_valid_idx(idx) {
                return CompactRecord::default();
            }

            let name_ref = &record.first_name.name;
            let parent_idx = resolve_parent(record.first_name.parent_frs, record.frs);

            CompactRecord {
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
                tree_allocated: record.tree_allocated,
                _pad: [0; 4],
            }
        })
        .collect();

    // Phase 2+3: expand hardlinks and ADS (sequential — rare, <1% of records).
    let mut names = index.names.as_bytes().to_vec();
    let expanded = expand_links_and_ads(index, &resolver, &resolve_parent, &mut names);
    records.extend(expanded);

    // Clone-then-lowercase avoids the intermediate `String` allocation that
    // `to_ascii_lowercase().into_bytes()` would create (~150MB saved).
    let mut names_lower = names.clone();
    names_lower.make_ascii_lowercase();
    let compact_elapsed = compact_start.elapsed().as_millis();

    let tri_start = Instant::now();
    let trigram = TrigramIndex::build(&records, &names_lower);
    let tri_elapsed = tri_start.elapsed().as_millis();

    // Build children CSR index from parent_idx (two-pass: count + scatter).
    let children = ChildrenIndex::build(&records);

    (
        DriveCompactIndex {
            letter: drive_letter,
            records,
            names,
            names_lower,
            trigram,
            children,
            source: IndexSource::MftFile(PathBuf::from(format!("{drive_letter}:"))),
            source_epoch: index.build_epoch,
        },
        compact_elapsed,
        tri_elapsed,
    )
}

/// Cache TTL in seconds (4 hours — same as Windows CLI).
///
/// USN Journal handles incremental freshness; this is a safety-net full rescan.
pub(crate) const INDEX_TTL_SECONDS: u64 = 14400;

/// Load an MFT file and build a compact index (cross-platform).
///
/// **Deprecated:** Use [`load_drive`] with [`MftSource::File`] instead.
///
/// # Errors
///
/// Returns an error if the MFT file cannot be read or parsed.
#[deprecated(note = "Use load_drive(MftSource::File(...)) instead")]
pub fn load_mft_file(
    mft_path: &std::path::Path,
    drive: Option<char>,
    no_cache: bool,
) -> anyhow::Result<(DriveCompactIndex, LoadTiming)> {
    load_drive(&MftSource::File(mft_path.to_path_buf(), drive), no_cache)
}

/// Load a live NTFS drive and build a compact index (Windows only).
///
/// **Deprecated:** Use [`load_drive`] with [`MftSource::Live`] instead.
///
/// # Errors
///
/// Returns an error if the drive cannot be read.
#[cfg(windows)]
#[deprecated(note = "Use load_drive(MftSource::Live(...)) instead")]
pub fn load_live_drive(
    drive_letter: char,
    no_cache: bool,
) -> anyhow::Result<(DriveCompactIndex, LoadTiming)> {
    load_drive(&MftSource::Live(drive_letter), no_cache)
}

/// Apply USN changes in-place to the compact index.
///
/// Mutates records (parent_idx, names, flags) then rebuilds the children CSR
/// once at the end.  Typical cost: <5ms for record mutations + ~100ms for CSR
/// rebuild on a 7M-record drive.
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
                // Clear parent so CSR rebuild excludes this record.
                rec.parent_idx = u32::MAX;
                stats.deleted += 1;
            }
        } else if change.created {
            if compact_idx != u32::MAX {
                // Re-animate a previously deleted slot.
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
                    tree_allocated: 0,
                    _pad: [0; 4],
                };

                drive.records.push(new_rec);
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

                // Update parent_idx — CSR rebuild picks this up.
                rec.parent_idx = new_parent_compact;
                stats.renamed += 1;
            }
        } else {
            stats.skipped += 1;
        }
    }

    // Rebuild derived structures from updated records + names.
    // Children CSR: ~100ms for 7M records. Trigram: ~500ms for 7M records.
    // Both are necessary so newly created/renamed files appear in tree
    // traversal AND trigram search.
    drive.children = ChildrenIndex::build(&drive.records);
    drive.trigram = TrigramIndex::build(&drive.records, &drive.names_lower);

    stats
}

// ════════════════════════════════════════════════════════════════════════
// REGRESSION TESTS — Search Pipeline Parity Guards
//
// These tests protect critical behaviors that broke during the v0.4.30
// refactor attempt.  They run on synthetic data (no Windows/MFT needed).
// See `docs/architecture/2026_03_30_04_12_SEARCH_PIPELINE_REGRESSION_ANALYSIS.
// md` ════════════════════════════════════════════════════════════════════════
#[cfg(test)]
#[path = "compact_tests.rs"]
mod tests;
