//! Compact in-memory index for search backends.
//!
//! Replaces the full `MftIndex` (224 bytes/record) with a lean 72-byte
//! `CompactRecord` that covers 100% of sortable/filterable columns.
//! Full metadata (ADS, forensic fields) is resolved on-demand from the
//! `.uffs` cache file.
//!
//! See `docs/architecture/COMPACT_INDEX_DESIGN.md` for the full design.

use std::time::Instant;

use rayon::prelude::*;
use uffs_mft::index::MftIndex;

// Re-export loader types and functions so callers can still use `compact::*`.
#[expect(deprecated, reason = "re-export kept for backward compatibility")]
pub use crate::compact_loader::{
    IndexSource, LoadTiming, MftSource, PatchStats, load_drive, load_mft_file, refresh_drive,
};
#[cfg(windows)]
#[expect(deprecated, reason = "re-export kept for backward compatibility")]
pub use crate::compact_loader::{apply_usn_patch, load_live_drive};
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
    /// Full path length in UTF-8 bytes (e.g. `C:\Windows\System32\cmd.exe` =
    /// 28). Precomputed at index build time via top-down parent-chain walk.
    /// Saturates at `u16::MAX` (65 535) for extremely deep paths.
    pub path_len: u16,

    /// First byte of the filename (e.g. `b'$'` for NTFS metafiles).
    ///
    /// Cached here so that hot-path filters like `--hide-system` (which only
    /// need to check `name.starts_with('$')`) can avoid touching the names
    /// arena entirely — turning a random cache-miss into a sequential field
    /// read from the `CompactRecord` array.
    pub name_first_byte: u8,

    /// Explicit tail padding for 8-byte struct alignment.
    /// Required by `bytemuck::Pod` — no implicit padding allowed.
    #[expect(
        clippy::pub_underscore_fields,
        reason = "bytemuck Pod requires all fields same visibility"
    )]
    pub _pad: [u8; 1],
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

    /// Returns `true` if the filename starts with `$` (NTFS system metafile).
    ///
    /// Uses the cached [`name_first_byte`](Self::name_first_byte) field so the
    /// check is a single byte comparison — no names-arena access required.
    #[inline]
    #[must_use]
    pub const fn is_system_metafile(self) -> bool {
        self.name_first_byte == b'$'
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
            if parent != u32::MAX
                && let Some(cnt) = counts.get_mut(parent as usize)
            {
                *cnt += 1;
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
            if parent != u32::MAX
                && let Some(pos) = write_pos.get_mut(parent as usize)
                && let Some(slot) = values.get_mut(*pos as usize)
            {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "record count bounded by NTFS limits, fits u32"
                )]
                let child_idx = idx as u32;
                *slot = child_idx;
                *pos += 1;
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
    pub const fn total_children(&self) -> usize {
        self.values.len()
    }

    /// Number of records tracked (one slot per record).
    #[must_use]
    pub const fn record_count(&self) -> usize {
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

/// Extension inverted index: `extension_id → &[u32]` (record indices).
///
/// CSR layout identical to `ChildrenIndex`.  Built once at load time in a
/// single O(N) pass so `--ext rs` queries can iterate only matching records
/// instead of scanning all 25M entries.
pub struct ExtensionIndex {
    /// CSR offsets — length = `max_ext_id` + 2 (one per `ext_id` + sentinel).
    offsets: Vec<u32>,
    /// Flat array of record indices, grouped by `extension_id`.
    values: Vec<u32>,
}

impl ExtensionIndex {
    /// Build from compact records in two passes (count + scatter).
    #[must_use]
    pub fn build(records: &[CompactRecord]) -> Self {
        // Find the maximum extension_id to size the offsets array.
        let max_id = records
            .iter()
            .map(|rec| rec.extension_id)
            .max()
            .unwrap_or(0) as usize;

        // Pass 1: count records per extension_id.
        let mut counts = vec![0_u32; max_id + 1];
        for rec in records {
            if rec.name_len == 0 {
                continue;
            }
            if let Some(cnt) = counts.get_mut(rec.extension_id as usize) {
                *cnt += 1;
            }
        }

        // Prefix-sum → offsets.
        let mut offsets = Vec::with_capacity(max_id + 2);
        let mut running = 0_u32;
        for &cnt in &counts {
            offsets.push(running);
            running = running.saturating_add(cnt);
        }
        offsets.push(running);

        // Pass 2: scatter record indices into values.
        let mut values = vec![0_u32; running as usize];
        let mut write_pos = offsets.clone();
        for (idx, rec) in records.iter().enumerate() {
            if rec.name_len == 0 {
                continue;
            }
            let eid = rec.extension_id as usize;
            if let Some(pos) = write_pos.get_mut(eid)
                && let Some(slot) = values.get_mut(*pos as usize)
            {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "record index bounded by NTFS limits"
                )]
                let idx_u32 = idx as u32;
                *slot = idx_u32;
                *pos += 1;
            }
        }

        Self { offsets, values }
    }

    /// Return record indices for the given `extension_id`.
    #[must_use]
    pub fn get(&self, ext_id: u16) -> &[u32] {
        let eid = ext_id as usize;
        let start = self.offsets.get(eid).copied().unwrap_or(0) as usize;
        let end = self.offsets.get(eid + 1).copied().unwrap_or(0) as usize;
        self.values.get(start..end).unwrap_or(&[])
    }

    /// Create an empty extension index.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            offsets: vec![0],
            values: Vec::new(),
        }
    }

    /// Total number of indexed record entries.
    #[must_use]
    pub const fn total_entries(&self) -> usize {
        self.values.len()
    }
}

/// A loaded drive with compact index.
pub struct DriveCompactIndex {
    /// Drive letter (e.g., 'C').
    pub letter: char,
    /// Compact records — one per MFT file/directory.
    pub records: Vec<CompactRecord>,
    /// All filenames concatenated (UTF-8 bytes, original case).
    pub names: Vec<u8>,
    /// Trigram inverted index built from folded names (char-level, `$UpCase`).
    pub trigram: TrigramIndex,
    /// CSR children index: `children.get(i)` → child indices of record i.
    pub children: ChildrenIndex,
    /// Extension inverted index: `ext_id → record indices`.
    /// Enables O(K) `--ext` queries where K = matching records, not O(N).
    pub ext_index: ExtensionIndex,
    /// NTFS `$UpCase` case folding engine for this volume.
    pub fold: uffs_text::CaseFold,
    /// Extension name table: `ext_names[extension_id]` → lowercase extension
    /// string (e.g. `"rs"`, `"txt"`). Index 0 = no extension.
    /// Used to resolve `--ext` filter strings to `u16` IDs for O(1)
    /// per-record matching instead of per-record string parsing.
    pub ext_names: Vec<Box<str>>,
    /// Where this index was loaded from (for future refresh).
    pub source: IndexSource,
    /// `MftIndex.build_epoch` this compact index was built from.
    /// Used as a staleness check when loading from cache.
    pub source_epoch: u64,
}

impl AsRef<Self> for DriveCompactIndex {
    fn as_ref(&self) -> &Self {
        self
    }
}

impl DriveCompactIndex {
    /// Resolve extension filter strings to their `u16` IDs on this drive.
    ///
    /// Returns a sorted, deduplicated `Vec<u16>` of matching IDs.
    /// Extensions not found on this drive are silently ignored.
    ///
    /// The lookup is a linear scan of `ext_names` (~500–2000 short strings),
    /// which takes < 1 µs.  This runs **once per search per drive**, not per
    /// record.
    #[must_use]
    pub fn resolve_ext_ids(&self, extensions: &[String]) -> Vec<u16> {
        let mut ids = Vec::with_capacity(extensions.len());
        for ext in extensions {
            let normalized = ext.trim().trim_start_matches('.').to_lowercase();
            if normalized.is_empty() {
                continue;
            }
            for (ext_id, name) in (0_u16..).zip(self.ext_names.iter()) {
                if name.as_ref() == normalized {
                    ids.push(ext_id);
                    break;
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

/// Expand alternate data streams (ADS) for a single record, producing the
/// name × stream cross product as extra `CompactRecord` entries.
#[expect(
    clippy::single_call_fn,
    reason = "Extracted to keep expand_links_and_ads under the too_many_lines limit"
)]
fn expand_ads_streams(
    index: &MftIndex,
    record: &uffs_mft::index::FileRecord,
    resolve_parent: &dyn Fn(u64, u64) -> u32,
    names: &mut Vec<u8>,
    extra: &mut Vec<CompactRecord>,
) {
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
                        path_len: 0,
                        name_first_byte: combined.as_bytes().first().copied().unwrap_or(0),
                        _pad: [0; 1],
                    });
                }
            }
        }
        se = stream.next_entry;
    }
}

/// Expand hardlinks and ADS into additional `CompactRecord` entries.
///
/// Phase 2 (hardlinks): for each valid record with `name_count > 1`, walks the
/// link chain and creates additional records with alternate name/parent.
///
/// Phase 3 (ADS): delegates to [`expand_ads_streams`] for each valid record
/// with `stream_count > 1`.
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
                    path_len: 0,
                    name_first_byte: names.get(link.name.offset as usize).copied().unwrap_or(0),
                    _pad: [0; 1],
                });
                link_entry = link.next_entry;
            }
        }

        // Phase 3: ADS expansion (name × stream cross product).
        if record.stream_count > 1 {
            expand_ads_streams(index, record, resolve_parent, names, &mut extra);
        }
    }
    extra
}

/// Compute `path_len` (in **characters**, not bytes) for every record
/// via top-down BFS.
///
/// Root entries (`parent_idx == u32::MAX`) get
/// `path_len = 2 + 1 + name_chars` (e.g. `"C:\" + name`), and children
/// accumulate `parent.path_len + 1 (separator) + name_chars`.
/// Saturates at `u16::MAX` (65 535) for extremely deep paths.
///
/// Character counting matches `str::chars().count()` so the precomputed
/// value agrees with the display-row path-length filter.
pub fn compute_path_lengths(records: &mut [CompactRecord], names: &[u8], drive_letter: char) {
    // Drive prefix in characters: the letter (1 char) + colon (1 char) = 2.
    // A `char` is always exactly one Unicode scalar value, so the letter
    // always contributes 1 to the char count regardless of its UTF-8
    // byte length.  Suppress the unused-variable lint by reading the
    // parameter — if this ever becomes `&str` the arithmetic changes.
    // `drive_letter` is read to guarantee callers pass a valid letter;
    // the arithmetic only cares about "1 char + 1 colon".
    debug_assert!(
        drive_letter.is_ascii_alphabetic(),
        "drive_letter must be ASCII A-Z, got {drive_letter:?}"
    );
    let drive_prefix_chars: u32 = 1 /* letter */ + 1 /* ':' */;

    // Build forward adjacency list (parent → children) for top-down BFS.
    let record_count = records.len();
    let mut children_of: Vec<Vec<u32>> = vec![Vec::new(); record_count];
    let mut roots: Vec<u32> = Vec::new();

    for (idx, rec) in records.iter().enumerate() {
        let pi = rec.parent_idx;
        if pi == u32::MAX {
            #[allow(clippy::cast_possible_truncation)] // idx < record_count ≤ u32::MAX by design
            roots.push(idx as u32);
        } else if let Some(siblings) = children_of.get_mut(pi as usize) {
            #[allow(clippy::cast_possible_truncation)] // idx < record_count ≤ u32::MAX by design
            siblings.push(idx as u32);
        }
    }

    // BFS from roots.
    let mut queue = alloc::collections::VecDeque::with_capacity(roots.len());
    for &root in &roots {
        let Some(rec) = records.get(root as usize) else {
            continue;
        };
        let name_chars = name_char_count(rec, names);
        let pl = if name_chars == 0 {
            // Drive root directory: "C:\"
            drive_prefix_chars + 1
        } else {
            // Top-level file/dir: "C:\<name>"
            drive_prefix_chars + 1 + name_chars
        };
        if let Some(slot) = records.get_mut(root as usize) {
            #[allow(clippy::cast_possible_truncation)] // clamped to u16::MAX
            {
                slot.path_len = pl.min(u32::from(u16::MAX)) as u16;
            }
        }
        queue.push_back(root);
    }

    while let Some(idx) = queue.pop_front() {
        let parent_pl = records
            .get(idx as usize)
            .map_or(0, |rec| u32::from(rec.path_len));
        let children: Vec<u32> = children_of
            .get(idx as usize)
            .map_or_else(Vec::new, Clone::clone);
        for &child in &children {
            let child_chars = records
                .get(child as usize)
                .map_or(0, |rec| name_char_count(rec, names));
            // path = parent_path + "\" + name
            let pl = parent_pl.saturating_add(1).saturating_add(child_chars);
            if let Some(slot) = records.get_mut(child as usize) {
                #[allow(clippy::cast_possible_truncation)] // clamped to u16::MAX
                {
                    slot.path_len = pl.min(u32::from(u16::MAX)) as u16;
                }
            }
            queue.push_back(child);
        }
    }
}

/// Count the number of Unicode characters in a record's filename.
///
/// Falls back to `name_len` (byte count) if the name slice is not valid
/// UTF-8 — this is correct for ASCII names and a safe upper bound
/// otherwise.
fn name_char_count(rec: &CompactRecord, names: &[u8]) -> u32 {
    let start = rec.name_offset as usize;
    let end = start + rec.name_len as usize;
    #[allow(clippy::cast_possible_truncation)] // filenames are always < u32::MAX chars
    names
        .get(start..end)
        .and_then(|slice| core::str::from_utf8(slice).ok())
        .map_or_else(
            || u32::from(rec.name_len),
            |name| name.chars().count() as u32,
        )
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
                path_len: 0,
                name_first_byte: index
                    .names
                    .as_bytes()
                    .get(name_ref.offset as usize)
                    .copied()
                    .unwrap_or(0),
                _pad: [0; 1],
            }
        })
        .collect();

    // Phase 2+3: expand hardlinks and ADS (sequential — rare, <1% of records).
    let mut names = index.names.as_bytes().to_vec();
    let expanded = expand_links_and_ads(index, &resolver, &resolve_parent, &mut names);
    records.extend(expanded);

    // Phase 4: compute path_len (in characters) for every record via
    // top-down BFS.  path_len = char count of "C:\dir\name".
    compute_path_lengths(&mut records, &names, drive_letter);

    let compact_elapsed = compact_start.elapsed().as_millis();

    // Try live $UpCase from the NTFS volume; fall back to compiled-in default.
    let fold = resolve_case_fold(drive_letter);

    let tri_start = Instant::now();
    let trigram = TrigramIndex::build(&records, &names, fold);
    let tri_elapsed = tri_start.elapsed().as_millis();

    // Build children CSR index from parent_idx (two-pass: count + scatter).
    let children = ChildrenIndex::build(&records);

    // Copy extension name table from MftIndex (Arc<str> → Box<str>).
    let ext_names: Vec<Box<str>> = index
        .extensions
        .names
        .iter()
        .map(|arc| Box::from(arc.as_ref()))
        .collect();

    let ext_t0 = Instant::now();
    let ext_index = ExtensionIndex::build(&records);
    let ext_build_ms = ext_t0.elapsed().as_millis();
    tracing::info!(
        drive = %drive_letter,
        entries = ext_index.total_entries(),
        build_ms = ext_build_ms,
        "ExtensionIndex built"
    );

    (
        DriveCompactIndex {
            letter: drive_letter,
            records,
            names,
            trigram,
            children,
            ext_index,
            fold,
            ext_names,
            source: IndexSource::MftFile(std::path::PathBuf::from(format!("{drive_letter}:"))),
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

// ── Live $UpCase resolution ──────────────────────────────────────────

/// Try to read the live `$UpCase` table from the NTFS volume for
/// `drive_letter`. On success, log the result at `INFO` and any diffs
/// from the compiled-in default at `WARN`. On failure, log at `WARN`
/// and fall back to [`CaseFold::default_table()`].
pub(crate) fn resolve_case_fold(drive_letter: char) -> uffs_text::CaseFold {
    match uffs_mft::platform::upcase::read_upcase_table(drive_letter) {
        Ok(live_table) => {
            let default = uffs_text::CaseFold::default_table();

            // Compare live vs default.
            // Leak the box to get a `&'static [u16]` for CaseFold::from_ntfs.
            let live_fold = uffs_text::CaseFold::from_ntfs(Box::leak(live_table));
            let diffs = default.diff(&live_fold);

            if diffs.is_empty() {
                tracing::info!(
                    drive = %drive_letter,
                    "$UpCase loaded from live volume — identical to compiled-in default"
                );
            } else {
                tracing::info!(
                    drive = %drive_letter,
                    diff_count = diffs.len(),
                    "$UpCase loaded from live volume — differs from compiled-in default"
                );
                for diff in &diffs {
                    tracing::warn!(
                        drive = %drive_letter,
                        codepoint = format_args!("U+{:04X}", diff.codepoint),
                        default = format_args!("U+{:04X}", diff.default_maps_to),
                        live = format_args!("U+{:04X}", diff.live_maps_to),
                        "$UpCase diff"
                    );
                }
            }
            live_fold
        }
        Err(err) => {
            tracing::warn!(
                drive = %drive_letter,
                error = %err,
                "$UpCase live read failed — falling back to compiled-in default table"
            );
            uffs_text::CaseFold::default_table()
        }
    }
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
