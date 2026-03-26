//! Trigram inverted index: maps 3-byte sequences to sorted lists of record
//! indices.
//!
//! Built once at load time. Search = intersect posting lists for query
//! trigrams, then verify candidates against pre-lowered names. O(matches)
//! not O(n).

use rayon::prelude::*;

/// Trigram inverted index: maps 3-byte sequences to sorted lists of record
/// indices.
///
/// Built once at load time. Search = intersect posting lists for query
/// trigrams, then verify candidates against pre-lowered paths. O(matches) not
/// O(n).
pub struct TrigramIndex {
    /// Trigram → sorted Vec of record indices containing that trigram.
    postings: std::collections::HashMap<[u8; 3], Vec<u32>>,
}

impl TrigramIndex {
    /// Create an empty trigram index (no postings).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            postings: std::collections::HashMap::new(),
        }
    }

    /// Build a trigram index from pre-lowered paths.
    pub fn build(paths_lower: &[String]) -> Self {
        const CHUNK_SIZE: usize = 64 * 1024;

        // Phase 1: parallel — each chunk builds a local postings map
        let chunk_maps: Vec<std::collections::HashMap<[u8; 3], Vec<u32>>> = paths_lower
            .par_chunks(CHUNK_SIZE)
            .enumerate()
            .map(|(chunk_idx, chunk)| {
                let base = chunk_idx * CHUNK_SIZE;
                let mut local: std::collections::HashMap<[u8; 3], Vec<u32>> =
                    std::collections::HashMap::new();

                for (offset, path) in chunk.iter().enumerate() {
                    let bytes = path.as_bytes();
                    if bytes.len() < 3 {
                        continue;
                    }
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "MFT record count bounded by NTFS limits"
                    )]
                    let record_idx = (base + offset) as u32;

                    // Track last pushed idx per trigram to skip consecutive dupes
                    // (cheaper than HashSet — paths have many repeated trigrams)
                    for window in bytes.windows(3) {
                        let tri: [u8; 3] = match <[u8; 3]>::try_from(window) {
                            Ok(arr) => arr,
                            Err(_) => continue,
                        };
                        let list = local.entry(tri).or_default();
                        if list.last() != Some(&record_idx) {
                            list.push(record_idx);
                        }
                    }
                }
                local
            })
            .collect();

        // Phase 2: merge all chunk maps into one (sequential but fast)
        let mut postings: std::collections::HashMap<[u8; 3], Vec<u32>> =
            std::collections::HashMap::new();

        for chunk_map in chunk_maps {
            let mut sorted_entries: Vec<_> = chunk_map.into_iter().collect();
            sorted_entries.sort_unstable_by_key(|(tri, _)| *tri);
            for (tri, indices) in sorted_entries {
                postings.entry(tri).or_default().extend(indices);
            }
        }

        Self { postings }
    }

    /// Number of unique trigrams in the index.
    pub fn posting_count(&self) -> usize {
        self.postings.len()
    }

    /// Search: intersect posting lists for query trigrams, return candidate
    /// record indices.
    ///
    /// For queries < 3 chars, returns None (caller should fall back to linear
    /// scan).
    pub fn search(&self, needle_lower: &str) -> Option<Vec<u32>> {
        let bytes = needle_lower.as_bytes();
        if bytes.len() < 3 {
            return None; // too short for trigram search
        }

        // Extract trigrams from the query
        let trigrams: Vec<[u8; 3]> = bytes
            .windows(3)
            .filter_map(|win| win.try_into().ok())
            .collect();

        // Find posting lists
        let mut lists: Vec<&[u32]> = trigrams
            .iter()
            .filter_map(|tri| self.postings.get(tri).map(Vec::as_slice))
            .collect();

        if lists.is_empty() {
            return Some(Vec::new()); // no trigrams found → no matches
        }

        // Sort by list size (intersect smallest first for efficiency)
        lists.sort_unstable_by_key(|list| list.len());

        // Intersect all posting lists
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

/// Intersect two sorted u32 slices, returning a new sorted Vec of common
/// elements.
fn intersect_sorted(list_a: &[u32], list_b: &[u32]) -> Vec<u32> {
    let mut result = Vec::with_capacity(list_a.len().min(list_b.len()));
    let mut iter_a = list_a.iter().peekable();
    let mut iter_b = list_b.iter().peekable();

    while let (Some(&val_a), Some(&val_b)) = (iter_a.peek(), iter_b.peek()) {
        match val_a.cmp(val_b) {
            core::cmp::Ordering::Equal => {
                result.push(*val_a);
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
    result
}
