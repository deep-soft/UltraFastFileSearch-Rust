//! Trigram inverted index — CSR (Compressed Sparse Row) layout.
//!
//! Maps 3-byte sequences to sorted lists of record indices using three
//! contiguous arrays:
//!
//! - `keys`:    sorted `[u8; 3]` trigram keys
//! - `offsets`: CSR offsets into `values` (len = `keys.len()` + 1)
//! - `values`:  flat u32 posting entries
//!
//! Lookup is binary-search on `keys` → slice into `values`.
//! This layout is cache-friendly, allocation-free after construction,
//! and can be serialized/deserialized as three bulk `memcpy`s.
//!
//! ## Build algorithm
//!
//! Two-pass counting sort (same pattern as `ChildrenIndex`):
//!
//! 1. **Pass 1 — count** (parallel): rayon chunks walk `CompactRecord` name
//!    slices from the pre-lowered names blob, deduplicate trigrams per record
//!    via `TinyTriSet`, and increment per-trigram counters in chunk-local
//!    `HashMap`s. Chunk maps are merged into global counts.
//! 2. **Sort keys + prefix sum** → sorted `keys` + CSR `offsets`.
//! 3. **Pass 2 — scatter**: re-iterate all names, write `record_idx` into
//!    `values[write_pos[key_idx]++]` for each unique trigram.
//!
//! Peak memory is only the final CSR arrays (~200MB for 7M records).
//! No intermediate 840MB pairs `Vec`.

use rayon::prelude::*;

use crate::compact::CompactRecord;

/// Flat lookup table size: 256³ = 16,777,216 possible trigram values.
/// At 4 bytes per entry = 64MB. Temporary — freed after build.
const TRIGRAM_LUT_SIZE: usize = 1 << 24;

/// Trigram inverted index in CSR (Compressed Sparse Row) layout.
pub struct TrigramIndex {
    /// Sorted trigram keys (each 3 bytes).
    keys: Vec<[u8; 3]>,
    /// CSR offsets into `values`. Length = `keys.len() + 1`.
    /// Posting list for `keys[i]` is `values[offsets[i]..offsets[i+1]]`.
    offsets: Vec<u32>,
    /// Flat array of all posting values (record indices), sorted per posting
    /// list.
    values: Vec<u32>,
}

/// Pack a 3-byte trigram into a `u32` for sorting (big-endian order so
/// lexicographic sort on the packed value equals byte-order sort on the
/// trigram).
#[inline]
const fn pack_trigram(tri: [u8; 3]) -> u32 {
    (tri[0] as u32) << 16 | (tri[1] as u32) << 8 | (tri[2] as u32)
}

/// Unpack a `u32` back to a 3-byte trigram.
#[inline]
#[expect(clippy::single_call_fn, reason = "pack/unpack are paired helpers")]
#[expect(
    clippy::cast_possible_truncation,
    reason = "right-shift guarantees the value fits in u8"
)]
const fn unpack_trigram(packed: u32) -> [u8; 3] {
    [(packed >> 16) as u8, (packed >> 8) as u8, packed as u8]
}

impl TrigramIndex {
    /// Create an empty trigram index (no postings).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            keys: Vec::new(),
            offsets: vec![0],
            values: Vec::new(),
        }
    }

    /// Build a trigram index directly from compact records and a pre-lowered
    /// names blob.
    ///
    /// Uses a **two-pass counting-sort** algorithm (same pattern as
    /// `ChildrenIndex::build`):
    ///
    /// 1. **Pass 1 — count**: parallel scan of names → per-chunk
    ///    `HashMap<packed_trigram, count>`. Merge into global counts.
    /// 2. **Sort keys + prefix sum** → CSR `keys` + `offsets`.
    /// 3. **Pass 2 — scatter**: re-iterate names, write `record_idx` into
    ///    `values` at the correct write position for each trigram.
    ///
    /// **Peak memory**: only the final CSR arrays (~200MB for 7M records).
    /// No intermediate 840MB pairs `Vec`.
    ///
    /// **Zero per-name heap allocations.** Names are accessed as byte slices
    /// from `names_lower`; no `String` or `&str` intermediaries are created.
    #[must_use]
    pub fn build(records: &[CompactRecord], names_lower: &[u8]) -> Self {
        use std::collections::HashMap;

        const CHUNK_SIZE: usize = 64 * 1024;

        if records.is_empty() {
            return Self::empty();
        }

        // ── Pass 1: parallel count ──────────────────────────────────
        // Each chunk produces a local HashMap<packed_trigram, count>.
        // "count" = number of UNIQUE records containing this trigram.
        let chunk_counts: Vec<HashMap<u32, u32>> = records
            .par_chunks(CHUNK_SIZE)
            .map(|chunk| {
                let mut local: HashMap<u32, u32> = HashMap::new();
                // Reuse one TinyTriSet per chunk — clear() resets length
                // without deallocating, eliminating one malloc per record.
                let mut seen = TinyTriSet::new();
                for rec in chunk {
                    let start = rec.name_offset as usize;
                    let end = start + rec.name_len as usize;
                    let bytes = match names_lower.get(start..end) {
                        Some(slice) if slice.len() >= 3 => slice,
                        _ => continue,
                    };
                    seen.clear();
                    for window in bytes.windows(3) {
                        let tri: [u8; 3] = match window.try_into() {
                            Ok(arr) => arr,
                            Err(_) => continue,
                        };
                        let packed = pack_trigram(tri);
                        if seen.insert(packed) {
                            *local.entry(packed).or_insert(0) += 1;
                        }
                    }
                }
                local
            })
            .collect();

        // Merge chunk counts into global counts.
        // Iteration order doesn't matter — global_counts is a HashMap (unordered),
        // and we sort the final keys in the next step.
        let mut global_counts: HashMap<u32, u32> = HashMap::new();
        for chunk_map in chunk_counts {
            #[expect(
                clippy::iter_over_hash_type,
                reason = "merge target is also a HashMap; insertion order is irrelevant — sorted below"
            )]
            for (tri, cnt) in chunk_map {
                *global_counts.entry(tri).or_insert(0) += cnt;
            }
        }

        // ── Sort keys + prefix sum → CSR offsets ────────────────────
        let mut sorted_keys: Vec<(u32, u32)> = global_counts.into_iter().collect();
        sorted_keys.sort_unstable_by_key(|&(packed, _)| packed);

        let trigram_count = sorted_keys.len();
        let mut keys = Vec::with_capacity(trigram_count);
        let mut offsets = Vec::with_capacity(trigram_count + 1);
        let mut running = 0_u32;

        // Flat lookup table: packed_trigram → key_index.
        // 16M entries × 4 bytes = 64MB temporary. O(1) lookup in scatter
        // phase vs HashMap's ~30ns/lookup. Freed after build completes.
        let mut tri_lut = vec![u32::MAX; TRIGRAM_LUT_SIZE];

        for (key_idx, &(packed, count)) in sorted_keys.iter().enumerate() {
            keys.push(unpack_trigram(packed));
            offsets.push(running);
            running = running.saturating_add(count);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "trigram count bounded by alphabet³ ≈ 50K"
            )]
            let ki = key_idx as u32;
            if let Some(slot) = tri_lut.get_mut(packed as usize) {
                *slot = ki;
            }
        }
        offsets.push(running);
        drop(sorted_keys);

        // ── Pass 2: scatter record_idx into CSR values ──────────────
        let values = scatter_postings(records, names_lower, &tri_lut, &offsets, running);
        drop(tri_lut);

        Self {
            keys,
            offsets,
            values,
        }
    }

    /// Construct directly from pre-built CSR arrays (cache deserialization).
    ///
    /// This is a zero-rebuild constructor — the three arrays are bulk-copied
    /// from the cache file, no per-element processing needed.
    #[must_use]
    pub const fn from_csr(keys: Vec<[u8; 3]>, offsets: Vec<u32>, values: Vec<u32>) -> Self {
        Self {
            keys,
            offsets,
            values,
        }
    }

    /// Borrow the CSR components for serialization.
    #[must_use]
    pub fn as_csr(&self) -> (&[[u8; 3]], &[u32], &[u32]) {
        (&self.keys, &self.offsets, &self.values)
    }

    /// Number of unique trigrams in the index.
    #[must_use]
    pub fn posting_count(&self) -> usize {
        self.keys.len()
    }

    /// Look up the posting list for a single trigram key.
    #[must_use]
    fn get_posting(&self, tri: [u8; 3]) -> Option<&[u32]> {
        let idx = self.keys.binary_search(&tri).ok()?;
        let start = *self.offsets.get(idx)? as usize;
        let end = *self.offsets.get(idx + 1)? as usize;
        self.values.get(start..end)
    }

    /// Search: intersect posting lists for query trigrams, return candidate
    /// record indices.
    ///
    /// For queries < 3 chars, returns `None` (caller should fall back to
    /// linear scan).
    #[must_use]
    pub fn search(&self, needle_lower: &str) -> Option<Vec<u32>> {
        let bytes = needle_lower.as_bytes();
        if bytes.len() < 3 {
            return None;
        }

        let trigrams: Vec<[u8; 3]> = bytes
            .windows(3)
            .filter_map(|win| win.try_into().ok())
            .collect();

        let mut lists: Vec<&[u32]> = trigrams
            .iter()
            .filter_map(|tri| self.get_posting(*tri))
            .collect();

        if lists.is_empty() {
            return Some(Vec::new());
        }

        lists.sort_unstable_by_key(|list| list.len());

        let Some(first_list) = lists.first() else {
            return Some(Vec::new());
        };
        let mut result = first_list.to_vec();
        for list in lists.iter().skip(1) {
            result = intersect_sorted(&result, list);
            if result.is_empty() {
                break;
            }
        }

        Some(result)
    }
}

/// Intersect two sorted `u32` slices, returning a new sorted `Vec<u32>`.
#[expect(
    clippy::single_call_fn,
    reason = "separated for clarity — hot-path intersection logic"
)]
fn intersect_sorted(list_a: &[u32], list_b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(list_a.len().min(list_b.len()));
    let mut iter_a = list_a.iter().peekable();
    let mut iter_b = list_b.iter().peekable();
    while let (Some(&val_a), Some(&val_b)) = (iter_a.peek(), iter_b.peek()) {
        match val_a.cmp(val_b) {
            core::cmp::Ordering::Equal => {
                out.push(*val_a);
                iter_a.next();
                iter_b.next();
            }
            core::cmp::Ordering::Less => {
                iter_a.next();
            }
            core::cmp::Ordering::Greater => {
                iter_b.next();
            }
        }
    }
    out
}

/// Pass 2 of the counting-sort trigram build: scatter `record_idx` values
/// into the pre-allocated CSR `values` array.
///
/// Records are visited in order (0, 1, 2, …), so each posting list is
/// automatically sorted by record index.
///
/// `tri_lut` is a flat lookup table of size `TRIGRAM_LUT_SIZE` mapping
/// `packed_trigram → key_index`. `u32::MAX` means "trigram not present".
/// O(1) lookup — no hashing overhead.
#[expect(
    clippy::single_call_fn,
    reason = "extracted to keep build() under line limit"
)]
fn scatter_postings(
    records: &[CompactRecord],
    names_lower: &[u8],
    tri_lut: &[u32],
    offsets: &[u32],
    total_postings: u32,
) -> Vec<u32> {
    let mut values = vec![0_u32; total_postings as usize];
    let mut write_pos: Vec<u32> = offsets.to_vec();
    // Single TinyTriSet reused across all records — one allocation total.
    let mut seen = TinyTriSet::new();

    for (record_idx, rec) in records.iter().enumerate() {
        let start = rec.name_offset as usize;
        let end = start + rec.name_len as usize;
        let bytes = match names_lower.get(start..end) {
            Some(slice) if slice.len() >= 3 => slice,
            _ => continue,
        };
        #[expect(
            clippy::cast_possible_truncation,
            reason = "MFT record count bounded by NTFS limits"
        )]
        let rec_idx = record_idx as u32;

        seen.clear();
        for window in bytes.windows(3) {
            let tri: [u8; 3] = match window.try_into() {
                Ok(arr) => arr,
                Err(_) => continue,
            };
            let packed = pack_trigram(tri);
            if !seen.insert(packed) {
                continue;
            }
            // Flat LUT: O(1) lookup, no hash
            let key_idx = match tri_lut.get(packed as usize).copied() {
                Some(ki) if ki != u32::MAX => ki,
                _ => continue,
            };
            if let Some(pos) = write_pos.get_mut(key_idx as usize) {
                if let Some(slot) = values.get_mut(*pos as usize) {
                    *slot = rec_idx;
                    *pos += 1;
                }
            }
        }
    }

    values
}

/// Tiny inline set for deduplicating packed trigram values within a single
/// filename.
///
/// NTFS filenames are at most 255 chars → at most 253 trigrams. We use a
/// small `Vec` with linear scan. For ≤253 elements this is faster than
/// hashing (no hash computation, cache-hot sequential scan).
struct TinyTriSet {
    /// Packed trigram values seen so far.
    seen: Vec<u32>,
}

impl TinyTriSet {
    /// Create a new empty set.
    fn new() -> Self {
        Self {
            seen: Vec::with_capacity(32),
        }
    }

    /// Reset for the next record without deallocating.
    ///
    /// The underlying `Vec` keeps its capacity, so the next record
    /// reuses the same heap allocation — eliminating one malloc/free
    /// per record (14M total across Pass 1 + Pass 2).
    fn clear(&mut self) {
        self.seen.clear();
    }

    /// Insert a packed trigram. Returns `true` if it was NOT already present.
    fn insert(&mut self, packed: u32) -> bool {
        if self.seen.contains(&packed) {
            return false;
        }
        self.seen.push(packed);
        true
    }
}
