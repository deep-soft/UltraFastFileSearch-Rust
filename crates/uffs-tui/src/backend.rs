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
    /// Build a trigram index from pre-lowered paths.
    #[expect(
        clippy::single_call_fn,
        reason = "constructor called once per drive load; separation improves readability"
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
    fn search(&self, needle_lower: &str) -> Option<Vec<u32>> {
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
            sort_column: SortColumn::Name,
            sort_desc: false,
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
    /// Searches compact index names via trigram, resolves paths on-demand
    /// only for matched results.
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

        let limit = if pattern.len() <= 2 {
            SHORT_PATTERN_LIMIT
        } else {
            DEFAULT_RESULT_LIMIT
        };

        let needle_lower = pattern.to_ascii_lowercase();

        let drive_results: Vec<Vec<DisplayRow>> = self
            .drives
            .par_iter()
            .map(|drive| search_compact_drive(drive, &needle_lower, limit))
            .collect();
        for drive_rows in drive_results {
            rows.extend(drive_rows);
        }
        rows.truncate(limit);
        let scanned = self.drives.iter().map(|dr| dr.records.len()).sum();

        sort_rows(&mut rows, self.sort_column, self.sort_desc);

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

    /// Cycle to the next sort column.
    pub fn cycle_sort(&mut self) {
        self.sort_column = match self.sort_column {
            SortColumn::Name => SortColumn::Size,
            SortColumn::Size => SortColumn::Modified,
            SortColumn::Modified => SortColumn::Path,
            SortColumn::Path => SortColumn::Drive,
            SortColumn::Drive => SortColumn::Extension,
            SortColumn::Extension => SortColumn::Type,
            SortColumn::Type => SortColumn::Name,
        };
        sort_rows(&mut self.last_results, self.sort_column, self.sort_desc);
    }

    /// Toggle sort direction.
    pub fn toggle_sort_direction(&mut self) {
        self.sort_desc = !self.sort_desc;
        sort_rows(&mut self.last_results, self.sort_column, self.sort_desc);
    }
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
