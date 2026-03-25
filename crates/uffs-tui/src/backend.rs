//! Search backend: compact-index search for the TUI.
//!
//! Searches `DriveCompactIndex` records (68 bytes each) with trigram
//! name matching, collecting results into `Vec<DisplayRow>` for the UI.
//! Full `MftIndex` is dropped after compact build — key memory savings.

use std::time::Instant;

use rayon::prelude::*;

use crate::compact::{self, DriveCompactIndex};

/// Maximum results returned per search (prevents UI lag on broad patterns).
/// 1K is plenty for a terminal display — keeps search under ~50ms.
const DEFAULT_RESULT_LIMIT: usize = 1_000;

/// Even lower limit for very short patterns (1-2 chars) that match millions.
const SHORT_PATTERN_LIMIT: usize = 200;

/// A single displayable search result row.
#[derive(Debug, Clone)]
pub struct DisplayRow {
    /// Drive letter this result belongs to.
    pub drive: char,
    /// Full resolved path (e.g., `C:\Users\file.txt`).
    pub path: String,
    /// Filename only (e.g., `file.txt`).
    pub name: String,
    /// File size in bytes.
    pub size: u64,
    /// Whether this is a directory (used for --files-only/--dirs-only filter).
    pub is_directory: bool,
    /// Last modified time (Unix microseconds).
    pub modified: i64,
}

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
    #[expect(
        clippy::single_call_fn,
        reason = "public API used by refresh; separation keeps constructor isolated"
    )]
    pub(crate) fn empty() -> Self {
        Self {
            postings: std::collections::HashMap::new(),
        }
    }

    /// Build a trigram index from pre-lowered paths.
    #[expect(
        clippy::single_call_fn,
        reason = "called from compact::build_name_trigram; separation keeps trigram logic isolated"
    )]
    pub(crate) fn build(paths_lower: &[String]) -> Self {
        use rayon::prelude::*;

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
    pub(crate) fn search(&self, needle_lower: &str) -> Option<Vec<u32>> {
        let bytes = needle_lower.as_bytes();
        if bytes.len() < 3 {
            return None; // too short for trigram search
        }

        // Extract trigrams from the query
        // windows(3) guarantees exactly 3 elements per window, so try_into always
        // succeeds
        let trigrams: Vec<[u8; 3]> = bytes
            .windows(3)
            .filter_map(|win| win.try_into().ok())
            .collect();

        // Find the smallest posting list (most selective trigram)
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
        // Safe: we checked lists.is_empty() above, so first() always succeeds
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
#[expect(
    clippy::single_call_fn,
    reason = "called from TrigramIndex::search loop; separation keeps intersection logic isolated"
)]
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

/// Result of a search operation.
pub struct SearchResult {
    /// Matching rows.
    pub rows: Vec<DisplayRow>,
    /// How long the search took.
    pub duration: core::time::Duration,
    /// Total records scanned across all drives.
    pub records_scanned: usize,
}

/// Multi-drive search backend backed by compact indices.
pub struct MultiDriveBackend {
    /// Loaded drives (compact index, ~68 bytes/record).
    pub drives: Vec<DriveCompactIndex>,
    /// Last search results (kept for re-sorting without re-searching).
    pub last_results: Vec<DisplayRow>,
    /// Current sort column.
    pub sort_column: SortColumn,
    /// Sort direction.
    pub sort_desc: bool,
}

/// Columns available for sorting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    /// Sort by filename.
    Name,
    /// Sort by file size.
    Size,
    /// Sort by last modified time.
    Modified,
    /// Sort by full path.
    Path,
    /// Sort by drive letter.
    Drive,
    /// Sort by file extension.
    Extension,
    /// Sort by devicon file type (groups similar types: music, images, code).
    Type,
}

/// Filter mode for file/directory results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterMode {
    /// Show all results.
    All,
    /// Show only files.
    FilesOnly,
    /// Show only directories.
    DirsOnly,
}

impl MultiDriveBackend {
    /// Create a new empty backend.
    #[must_use]
    #[expect(
        clippy::single_call_fn,
        reason = "constructor called from App::new; separation keeps backend initialization isolated"
    )]
    pub const fn new() -> Self {
        Self {
            drives: Vec::new(),
            last_results: Vec::new(),
            sort_column: SortColumn::Modified,
            sort_desc: true,
        }
    }

    /// Total record count across all loaded drives.
    #[must_use]
    pub fn total_records(&self) -> usize {
        self.drives.iter().map(|dr| dr.records.len()).sum()
    }

    /// List loaded drives with record counts.
    #[must_use]
    pub fn drive_summary(&self) -> Vec<(char, usize)> {
        self.drives
            .iter()
            .map(|dr| (dr.letter, dr.records.len()))
            .collect()
    }

    /// Search across all loaded drives.
    ///
    /// Detects path patterns (containing `\` or `/`) and uses tree search.
    /// Otherwise uses trigram name search. Paths resolved on-demand.
    pub fn search(&mut self, pattern: &str, _name_only: bool) -> SearchResult {
        let start = Instant::now();
        let mut rows = Vec::new();

        // Empty pattern → clear results
        if pattern.is_empty() {
            self.last_results.clear();
            return SearchResult {
                rows: Vec::new(),
                duration: start.elapsed(),
                records_scanned: 0,
            };
        }

        // "*" = show all files (first 1,000)
        let is_match_all = pattern == "*";
        let limit = if is_match_all {
            DEFAULT_RESULT_LIMIT
        } else if pattern.len() <= 2 {
            SHORT_PATTERN_LIMIT
        } else {
            DEFAULT_RESULT_LIMIT
        };

        let needle_lower = pattern.to_ascii_lowercase();
        let is_path = !is_match_all && compact::is_path_pattern(&needle_lower);

        if is_match_all {
            // Global top-N: scan ALL records across ALL drives, pick the
            // best N by sort column, THEN resolve paths only for those.
            rows = collect_global_top_n(&self.drives, limit, self.sort_column, self.sort_desc);
        } else {
            let drive_results: Vec<Vec<DisplayRow>> = self
                .drives
                .par_iter()
                .map(|drive| {
                    if is_path {
                        search_compact_drive_tree(drive, &needle_lower, limit)
                    } else {
                        search_compact_drive(drive, &needle_lower, limit)
                    }
                })
                .collect();
            for drive_rows in drive_results {
                rows.extend(drive_rows);
            }
            sort_rows(&mut rows, self.sort_column, self.sort_desc);
            rows.truncate(limit);
        }
        let scanned = self.drives.iter().map(|dr| dr.records.len()).sum();

        self.last_results.clone_from(&rows);
        SearchResult {
            rows,
            duration: start.elapsed(),
            records_scanned: scanned,
        }
    }

    /// Re-sort the last results by a different column.
    #[expect(
        dead_code,
        reason = "public API for direct sort; currently used via cycle_sort/toggle_sort_direction"
    )]
    pub fn sort(&mut self, column: SortColumn, descending: bool) {
        self.sort_column = column;
        self.sort_desc = descending;
        sort_rows(&mut self.last_results, column, descending);
    }

    /// Cycle to the next sort column with a sensible default direction.
    ///
    /// Each column gets its most useful default sort direction:
    /// - **Name, Path, Drive, Extension, Type** → ascending (A→Z)
    /// - **Size, Modified** → descending (biggest/newest first)
    pub fn cycle_sort(&mut self) {
        let (new_column, new_desc) = match self.sort_column {
            SortColumn::Name => (SortColumn::Size, true), // biggest first
            SortColumn::Size => (SortColumn::Modified, true), // newest first
            SortColumn::Modified => (SortColumn::Path, false), // A→Z
            SortColumn::Path => (SortColumn::Drive, false), // A→Z
            SortColumn::Drive => (SortColumn::Extension, false), // A→Z
            SortColumn::Extension => (SortColumn::Type, false), // A→Z
            SortColumn::Type => (SortColumn::Name, false), // A→Z
        };
        self.sort_column = new_column;
        self.sort_desc = new_desc;
        sort_rows(&mut self.last_results, self.sort_column, self.sort_desc);
    }

    /// Toggle sort direction.
    pub fn toggle_sort_direction(&mut self) {
        self.sort_desc = !self.sort_desc;
        sort_rows(&mut self.last_results, self.sort_column, self.sort_desc);
    }
}

/// Collect the global top-N records across ALL drives for `*` match-all.
///
/// For numeric columns (Size, Modified): scans all 25M compact records with
/// lightweight `(drive_idx, record_idx, sort_key)` tuples, sorts globally,
/// takes top N, then resolves paths only for those N winners.
///
/// For text columns (Name, Path, etc.): samples records from each drive
/// proportionally, builds full `DisplayRow`s, sorts by text, truncates.
#[expect(
    clippy::single_call_fn,
    reason = "called from MultiDriveBackend::search; separation keeps global scan logic isolated"
)]
fn collect_global_top_n(
    drives: &[DriveCompactIndex],
    limit: usize,
    sort_column: SortColumn,
    sort_desc: bool,
) -> Vec<DisplayRow> {
    match sort_column {
        // Numeric columns: efficient lightweight tuple scan of all 25M records
        // Drive: packs drive_letter + name_prefix into i64 for grouping
        SortColumn::Size | SortColumn::Modified | SortColumn::Drive => {
            collect_global_top_n_numeric(drives, limit, sort_column, sort_desc)
        }
        // Path: hierarchical depth-first tree walk — instant, touches ~N records
        SortColumn::Path => collect_path_sorted(drives, limit, sort_desc),
        // Extension/Type: routed to numeric (extension_id) inside collect_name_sorted
        //   Same extension_id = same devicons icon type, so grouping is correct.
        // Name: text comparison on names blob
        SortColumn::Name | SortColumn::Extension | SortColumn::Type => {
            collect_name_sorted(drives, limit, sort_column, sort_desc)
        }
    }
}

/// Hierarchical depth-first tree walk for Path/Drive sort.
///
/// Walks the directory tree in alphabetical order (by name within each level),
/// collecting files as they're encountered. Stops after `limit` results.
/// This is instant — touches only ~N records instead of sorting all 25M.
#[expect(
    clippy::single_call_fn,
    reason = "called from collect_global_top_n; separation keeps tree walk isolated"
)]
fn collect_path_sorted(
    drives: &[DriveCompactIndex],
    limit: usize,
    sort_desc: bool,
) -> Vec<DisplayRow> {
    let mut results = Vec::with_capacity(limit);

    // Sort drives by letter (ascending or descending)
    let mut drive_order: Vec<usize> = (0..drives.len()).collect();
    #[expect(clippy::indexing_slicing, reason = "indices from 0..len, always valid")]
    drive_order.sort_unstable_by(|&idx_a, &idx_b| {
        let ord = drives[idx_a].letter.cmp(&drives[idx_b].letter);
        if sort_desc { ord.reverse() } else { ord }
    });

    for &drive_idx in &drive_order {
        let Some(drive) = drives.get(drive_idx) else {
            continue;
        };
        let volume_prefix = format!("{}:\\", drive.letter);

        // Find root-level records (parent_idx == u32::MAX)
        let mut roots: Vec<u32> = drive
            .records
            .iter()
            .enumerate()
            .filter(|(_, rec)| rec.parent_idx == u32::MAX && rec.name_len > 0)
            .map(|(idx, _)| {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "record index bounded by NTFS limits"
                )]
                {
                    idx as u32
                }
            })
            .collect();

        // Sort roots by name
        sort_indices_by_name(&mut roots, drive, sort_desc);

        // Depth-first walk
        let mut stack: Vec<u32> = roots.into_iter().rev().collect(); // reverse for correct DFS order
        while let Some(idx) = stack.pop() {
            if results.len() >= limit {
                return results;
            }

            let Some(rec) = drive.records.get(idx as usize) else {
                continue;
            };
            let name = rec.name(&drive.names);
            if name.is_empty() || name == "." {
                continue;
            }

            let path = compact::resolve_path(drive, idx as usize, &volume_prefix);
            results.push(DisplayRow {
                drive: drive.letter,
                path,
                name: name.to_owned(),
                size: rec.size,
                is_directory: rec.is_directory(),
                modified: rec.modified,
            });

            // Push children (sorted, reversed for DFS order)
            if let Some(children) = drive.children.get(idx as usize) {
                if !children.is_empty() {
                    let mut sorted_children = children.clone();
                    sort_indices_by_name(&mut sorted_children, drive, sort_desc);
                    for &child in sorted_children.iter().rev() {
                        stack.push(child);
                    }
                }
            }
        }
    }

    results
}

/// Sort a slice of compact indices by their name in the names blob.
fn sort_indices_by_name(indices: &mut [u32], drive: &DriveCompactIndex, desc: bool) {
    indices.sort_unstable_by(|&idx_a, &idx_b| {
        let name_a = drive
            .records
            .get(idx_a as usize)
            .map_or("", |rec| rec.name(&drive.names_lower));
        let name_b = drive
            .records
            .get(idx_b as usize)
            .map_or("", |rec| rec.name(&drive.names_lower));
        let ord = name_a.cmp(name_b);
        if desc { ord.reverse() } else { ord }
    });
}

/// Sort for Name/Extension/Type columns using lightweight numeric keys.
///
/// - **Extension**: sorts by `extension_id` (u16 comparison — instant)
/// - **Name**: sorts by name bytes from names blob (text comparison)
/// - **Type**: sorts by devicons icon character
///
/// Uses `(drive_idx, record_idx, sort_key)` tuples for Extension (numeric),
/// falls back to text comparison for Name/Type.
#[expect(
    clippy::single_call_fn,
    reason = "called from collect_global_top_n; separation keeps text sort isolated"
)]
fn collect_name_sorted(
    drives: &[DriveCompactIndex],
    limit: usize,
    sort_column: SortColumn,
    sort_desc: bool,
) -> Vec<DisplayRow> {
    // All columns now route through the numeric fast path:
    // - Extension/Type: extension_id as sort key
    // - Name: first 8 bytes of lowercase name packed into u64
    collect_global_top_n_numeric(drives, limit, sort_column, sort_desc)
}

/// Efficient numeric global top-N using lightweight tuples.
///
/// Scans all compact records, extracts a numeric sort key, sorts globally,
/// takes top N, then resolves paths only for the winners.
#[expect(
    clippy::cast_possible_truncation,
    reason = "drive index and record index bounded by practical limits"
)]
fn collect_global_top_n_numeric(
    drives: &[DriveCompactIndex],
    limit: usize,
    sort_column: SortColumn,
    sort_desc: bool,
) -> Vec<DisplayRow> {
    let mut candidates: Vec<(u16, u32, i64)> = Vec::new();

    for (drive_idx, drive) in drives.iter().enumerate() {
        for (rec_idx, rec) in drive.records.iter().enumerate() {
            if rec.name_len == 0 {
                continue;
            }
            #[expect(
                clippy::cast_possible_wrap,
                reason = "file sizes within i64 range for practical NTFS volumes"
            )]
            let sort_key = match sort_column {
                SortColumn::Size => rec.size as i64,
                SortColumn::Extension | SortColumn::Type => i64::from(rec.extension_id),
                SortColumn::Name => {
                    // Pack first 8 bytes of lowercase name into u64 (big-endian).
                    // Byte ordering matches alphabetical: "abc" < "abd" as u64.
                    let name_bytes = rec.name(&drive.names_lower).as_bytes();
                    let mut key = [0_u8; 8];
                    for (dst, src) in key.iter_mut().zip(name_bytes.iter()) {
                        *dst = *src;
                    }
                    i64::from_be_bytes(key)
                }
                SortColumn::Drive => {
                    // Pack drive letter (top byte) + first 7 bytes of name.
                    // Groups by drive, then alphabetical by name within drive.
                    let name_bytes = rec.name(&drive.names_lower).as_bytes();
                    let mut key = [0_u8; 8];
                    key[0] = drive.letter as u8;
                    for (dst, src) in key[1..].iter_mut().zip(name_bytes.iter()) {
                        *dst = *src;
                    }
                    i64::from_be_bytes(key)
                }
                SortColumn::Modified | SortColumn::Path => rec.modified,
            };
            candidates.push((drive_idx as u16, rec_idx as u32, sort_key));
        }
    }

    if sort_desc {
        candidates.sort_unstable_by_key(|entry| core::cmp::Reverse(entry.2));
    } else {
        candidates.sort_unstable_by_key(|entry| entry.2);
    }
    candidates.truncate(limit);

    let mut rows: Vec<DisplayRow> = candidates
        .iter()
        .filter_map(|&(drive_idx, rec_idx, _)| {
            let drive = drives.get(drive_idx as usize)?;
            let rec = drive.records.get(rec_idx as usize)?;
            let name = rec.name(&drive.names);
            if name.is_empty() || name == "." {
                return None;
            }
            let volume_prefix = format!("{}:\\", drive.letter);
            let path = compact::resolve_path(drive, rec_idx as usize, &volume_prefix);
            Some(DisplayRow {
                drive: drive.letter,
                path,
                name: name.to_owned(),
                size: rec.size,
                is_directory: rec.is_directory(),
                modified: rec.modified,
            })
        })
        .collect();

    // Re-sort with name tiebreaker: when multiple results share the same
    // sort key (e.g., same extension, same size), order them by name.
    sort_rows(&mut rows, sort_column, sort_desc);
    rows
}

/// Search a single drive's compact index, returning matching `DisplayRow`s.
///
/// Uses trigram index on names for 3+ char patterns, linear scan for shorter.
/// Paths are resolved on-demand only for matched results.
#[expect(
    clippy::single_call_fn,
    reason = "called from MultiDriveBackend::search via rayon; separation keeps per-drive logic isolated"
)]
fn search_compact_drive(
    drive: &DriveCompactIndex,
    needle_lower: &str,
    limit: usize,
) -> Vec<DisplayRow> {
    if needle_lower.is_empty() {
        return Vec::new();
    }

    let volume_prefix = format!("{}:\\", drive.letter);

    // Try trigram search first (3+ chars)
    let candidates = drive.trigram.search(needle_lower);

    let match_indices: Vec<usize> = if let Some(candidate_indices) = candidates {
        // Trigram hit: verify candidates with actual substring check on name
        candidate_indices
            .iter()
            .filter(|&&idx| {
                let rec_idx = idx as usize;
                let Some(rec) = drive.records.get(rec_idx) else {
                    return false;
                };
                let name = rec.name(&drive.names_lower);
                !name.is_empty() && name.contains(needle_lower)
            })
            .take(limit)
            .map(|&idx| idx as usize)
            .collect()
    } else {
        // Short pattern (<3 chars): linear scan on names_lower
        drive
            .records
            .iter()
            .enumerate()
            .filter(|(_, rec)| {
                let name = rec.name(&drive.names_lower);
                !name.is_empty() && name != "." && name.contains(needle_lower)
            })
            .take(limit)
            .map(|(idx, _)| idx)
            .collect()
    };

    // Build DisplayRows: resolve paths on-demand (only for matches)
    match_indices
        .iter()
        .filter_map(|&record_idx| {
            let rec = drive.records.get(record_idx)?;
            let name = rec.name(&drive.names);
            if name.is_empty() || name == "." {
                return None;
            }
            let path = compact::resolve_path(drive, record_idx, &volume_prefix);
            Some(DisplayRow {
                drive: drive.letter,
                path,
                name: name.to_owned(),
                size: rec.size,
                is_directory: rec.is_directory(),
                modified: rec.modified,
            })
        })
        .collect()
}

/// Search a single drive using tree-based path traversal.
///
/// For patterns containing `\` or `/`, decomposes the pattern into path
/// segments and walks the directory tree instead of flat name search.
#[expect(
    clippy::single_call_fn,
    reason = "called from MultiDriveBackend::search via rayon; separation keeps per-drive logic isolated"
)]
fn search_compact_drive_tree(
    drive: &DriveCompactIndex,
    pattern_lower: &str,
    limit: usize,
) -> Vec<DisplayRow> {
    let volume_prefix = format!("{}:\\", drive.letter);

    let match_indices = compact::tree_search(drive, pattern_lower, limit);

    match_indices
        .iter()
        .filter_map(|&record_idx| {
            let rec = drive.records.get(record_idx as usize)?;
            let name = rec.name(&drive.names);
            if name.is_empty() || name == "." {
                return None;
            }
            let path = compact::resolve_path(drive, record_idx as usize, &volume_prefix);
            Some(DisplayRow {
                drive: drive.letter,
                path,
                name: name.to_owned(),
                size: rec.size,
                is_directory: rec.is_directory(),
                modified: rec.modified,
            })
        })
        .collect()
}

/// Maximally distinct color palettes for 1–10 drives.
///
/// Each sub-palette is hand-tuned so that N drives get N maximally
/// distinguishable colors on a dark terminal background.
const PALETTES: &[&[(u8, u8, u8)]] = &[
    // 1 drive
    &[(255, 255, 255)],
    // 2 drives
    &[(100, 180, 255), (255, 150, 50)],
    // 3 drives
    &[(100, 180, 255), (80, 220, 80), (255, 150, 50)],
    // 4 drives
    &[
        (100, 180, 255),
        (80, 220, 80),
        (255, 150, 50),
        (200, 100, 255),
    ],
    // 5 drives
    &[
        (100, 180, 255),
        (80, 220, 80),
        (255, 150, 50),
        (200, 100, 255),
        (255, 255, 80),
    ],
    // 6 drives
    &[
        (100, 180, 255),
        (80, 220, 80),
        (255, 150, 50),
        (200, 100, 255),
        (255, 255, 80),
        (255, 100, 100),
    ],
    // 7 drives
    &[
        (100, 180, 255),
        (80, 220, 80),
        (255, 150, 50),
        (200, 100, 255),
        (255, 255, 80),
        (255, 100, 100),
        (100, 255, 220),
    ],
    // 8 drives
    &[
        (100, 180, 255),
        (80, 220, 80),
        (255, 150, 50),
        (200, 100, 255),
        (255, 255, 80),
        (255, 100, 100),
        (100, 255, 220),
        (255, 130, 200),
    ],
    // 9 drives
    &[
        (100, 180, 255),
        (80, 220, 80),
        (255, 150, 50),
        (200, 100, 255),
        (255, 255, 80),
        (255, 100, 100),
        (100, 255, 220),
        (255, 130, 200),
        (255, 255, 255),
    ],
    // 10 drives
    &[
        (100, 180, 255),
        (80, 220, 80),
        (255, 150, 50),
        (200, 100, 255),
        (255, 255, 80),
        (255, 100, 100),
        (100, 255, 220),
        (255, 130, 200),
        (255, 255, 255),
        (180, 140, 100),
    ],
];

/// Build a drive-letter → color mapping for the currently loaded drives.
///
/// Assigns colors from the optimal palette for the given number of drives.
/// Drives are sorted alphabetically so the mapping is deterministic.
#[must_use]
#[expect(
    clippy::single_call_fn,
    reason = "public API: intentionally a standalone function for reuse and clarity"
)]
pub fn build_drive_colors(
    drives: &[DriveCompactIndex],
) -> std::collections::HashMap<char, ratatui::style::Color> {
    use ratatui::style::Color;

    let mut letters: Vec<char> = drives.iter().map(|dr| dr.letter).collect();
    letters.sort_unstable();
    letters.dedup();

    let count = letters.len();
    let palette_idx = count
        .saturating_sub(1)
        .min(PALETTES.len().saturating_sub(1));
    let default_palette: &[(u8, u8, u8)] = &[(255, 255, 255)];
    let palette = PALETTES.get(palette_idx).unwrap_or(&default_palette);

    letters
        .into_iter()
        .enumerate()
        .map(|(idx, letter)| {
            let &(red, green, blue) = palette
                .get(idx % palette.len().max(1))
                .unwrap_or(&(255, 255, 255));
            (letter, Color::Rgb(red, green, blue))
        })
        .collect()
}

/// Sort display rows by the given column with name as secondary tiebreaker.
fn sort_rows(rows: &mut [DisplayRow], column: SortColumn, descending: bool) {
    rows.sort_unstable_by(|row_a, row_b| {
        let primary = match column {
            SortColumn::Name => row_a.name.to_lowercase().cmp(&row_b.name.to_lowercase()),
            SortColumn::Size => row_a.size.cmp(&row_b.size),
            SortColumn::Modified => row_a.modified.cmp(&row_b.modified),
            SortColumn::Path => row_a.path.to_lowercase().cmp(&row_b.path.to_lowercase()),
            SortColumn::Drive => row_a.drive.cmp(&row_b.drive),
            SortColumn::Extension => {
                let ext_a = row_a.name.rsplit('.').next().unwrap_or("").to_lowercase();
                let ext_b = row_b.name.rsplit('.').next().unwrap_or("").to_lowercase();
                ext_a.cmp(&ext_b)
            }
            SortColumn::Type => {
                let icon_a = devicons::icon_for_file(&row_a.name, &None).icon;
                let icon_b = devicons::icon_for_file(&row_b.name, &None).icon;
                icon_a.cmp(&icon_b)
            }
        };
        // Multi-tier: if primary column is equal, break ties by name (ascending)
        let ord = if primary == core::cmp::Ordering::Equal && column != SortColumn::Name {
            row_a.name.to_lowercase().cmp(&row_b.name.to_lowercase())
        } else {
            primary
        };
        if descending { ord.reverse() } else { ord }
    });
}

/// Apply filter mode to a set of display rows.
pub fn apply_filter(rows: &mut Vec<DisplayRow>, filter: FilterMode) {
    match filter {
        FilterMode::All => {} // no-op
        FilterMode::FilesOnly => rows.retain(|row| !row.is_directory),
        FilterMode::DirsOnly => rows.retain(|row| row.is_directory),
    }
}
