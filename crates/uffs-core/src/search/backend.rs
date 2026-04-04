//! Search backend types: display rows, sort columns, filter modes, and
//! multi-drive search orchestration.

use std::time::Instant;

use rayon::prelude::*;

use crate::compact::DriveCompactIndex;
use crate::search::field::FieldId;

/// Sentinel: no truncation — return every matching record.
const UNLIMITED: usize = usize::MAX;

/// A single displayable search result row.
///
/// The filename is **not** stored separately — it is derived from the `path`
/// field using `name_start` (byte offset where the filename begins within
/// `path`).  This avoids one heap allocation per result row.
#[derive(Debug, Clone, Default)]
#[expect(
    clippy::partial_pub_fields,
    reason = "name_start is private by design — accessed via name() method"
)]
pub struct DisplayRow {
    /// Record index within the compact/cache file.
    pub record_index: u32,
    /// Drive letter this result belongs to.
    pub drive: char,
    /// Full resolved path (e.g., `C:\Users\file.txt`).
    pub path: String,
    /// Byte offset within `path` where the filename begins.
    ///
    /// `self.name()` returns `&self.path[name_start..]`.
    /// Computed once at construction from the last `\` separator.
    name_start: u32,
    /// File size in bytes.
    pub size: u64,
    /// Whether this is a directory.
    pub is_directory: bool,
    /// Last modified time (Unix microseconds).
    pub modified: i64,
    /// Creation time (Unix microseconds).
    pub created: i64,
    /// Last access time (Unix microseconds).
    pub accessed: i64,
    /// Raw NTFS `FILE_ATTRIBUTE_*` flags.
    pub flags: u32,
    /// Allocated size on disk in bytes.
    pub allocated: u64,
    /// Descendant count (directories only).
    pub descendants: u32,
    /// Sum of logical file sizes in entire subtree (directories only).
    pub treesize: u64,
    /// Sum of allocated sizes in entire subtree (directories only).
    pub tree_allocated: u64,
}

impl DisplayRow {
    /// Construct a `DisplayRow`, computing `name_start` from the path.
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "flat struct — all fields are required, no logical grouping"
    )]
    pub fn new(
        record_index: u32,
        drive: char,
        path: String,
        size: u64,
        is_directory: bool,
        modified: i64,
        created: i64,
        accessed: i64,
        flags: u32,
        allocated: u64,
        descendants: u32,
        treesize: u64,
        tree_allocated: u64,
    ) -> Self {
        #[expect(clippy::cast_possible_truncation, reason = "paths < 4GB")]
        let name_start = path.rfind('\\').map_or(0, |pos| pos + 1) as u32;
        Self {
            record_index,
            drive,
            path,
            name_start,
            size,
            is_directory,
            modified,
            created,
            accessed,
            flags,
            allocated,
            descendants,
            treesize,
            tree_allocated,
        }
    }

    /// Filename portion of the path (e.g., `file.txt`).
    ///
    /// Zero-cost: returns a `&str` slice into the owned `path`.
    #[must_use]
    #[inline]
    pub fn name(&self) -> &str {
        self.path.get(self.name_start as usize..).unwrap_or("")
    }

    /// Directory portion of path (up to and including the last `\`).
    ///
    /// Uses `name_start` for zero-cost slicing (no `rfind` needed).
    #[must_use]
    #[inline]
    pub fn path_dir(&self) -> &str {
        self.path
            .get(..self.name_start as usize)
            .unwrap_or(&self.path)
    }
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

/// Legacy type alias — all sort columns are now `FieldId`.
pub type SortColumn = FieldId;

/// Filter mode for file/directory results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterMode {
    /// Show all results.
    #[default]
    All,
    /// Show only files.
    FilesOnly,
    /// Show only directories.
    DirsOnly,
}

/// Parsed sort specification: column + direction.
#[derive(Debug, Clone, Copy)]
pub struct SortSpec {
    /// Which field to sort by.
    pub column: FieldId,
    /// `true` = descending (biggest / newest first).
    pub descending: bool,
}

/// Multi-drive search backend backed by compact indices.
pub struct MultiDriveBackend {
    /// Loaded drives (compact index, ~72 bytes/record).
    pub drives: Vec<DriveCompactIndex>,
    /// Last search results (kept for re-sorting without re-searching).
    pub last_results: Vec<DisplayRow>,
    /// Current (primary) sort column.
    pub sort_column: FieldId,
    /// Primary sort direction (`true` = descending).
    pub sort_desc: bool,
    /// Additional sort tiers beyond the primary.
    pub extra_sort_tiers: Vec<SortSpec>,
}

impl Default for MultiDriveBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MultiDriveBackend {
    /// Create a new empty backend.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            drives: Vec::new(),
            last_results: Vec::new(),
            sort_column: FieldId::Modified,
            sort_desc: true,
            extra_sort_tiers: Vec::new(),
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
    pub fn search(
        &mut self,
        pattern: &str,
        case_sensitive: bool,
        whole_word: bool,
        result_limit: Option<u32>,
        filter_mode: FilterMode,
        search_filters: &mut super::filters::SearchFilters,
    ) -> SearchResult {
        self.search_drives(
            pattern,
            case_sensitive,
            whole_word,
            result_limit,
            filter_mode,
            search_filters,
            &[],
        )
    }

    /// Search with an optional drive-letter filter.
    ///
    /// When `drives_filter` is non-empty, only drives whose letter is in
    /// the slice are searched. An empty slice means "search all loaded
    /// drives".
    #[expect(
        clippy::too_many_lines,
        clippy::too_many_arguments,
        reason = "search dispatch with three modes and a drive filter; bundling params into a
                  struct would change the public API across CLI/TUI/daemon callers"
    )]
    pub fn search_drives(
        &mut self,
        pattern: &str,
        case_sensitive: bool,
        whole_word: bool,
        result_limit: Option<u32>,
        filter_mode: FilterMode,
        search_filters: &mut super::filters::SearchFilters,
        drives_filter: &[char],
    ) -> SearchResult {
        let start = Instant::now();
        let mut rows = Vec::new();

        if pattern.is_empty() {
            self.last_results.clear();
            return SearchResult {
                rows: Vec::new(),
                duration: start.elapsed(),
                records_scanned: 0,
            };
        }

        // When a drive filter is active, temporarily swap out non-matching
        // drives so the rest of the search logic (which uses `self.drives`)
        // only touches the requested subset. We restore afterwards.
        let stashed_drives = if drives_filter.is_empty() {
            None
        } else {
            let all = core::mem::take(&mut self.drives);
            let (keep, rest): (Vec<_>, Vec<_>) = all.into_iter().partition(|dr| {
                drives_filter
                    .iter()
                    .any(|fl| fl.eq_ignore_ascii_case(&dr.letter))
            });
            self.drives = keep;
            Some(rest)
        };

        let is_match_all = pattern == "*";
        let is_regex = pattern.starts_with('>') && pattern.len() > 1;
        let limit = result_limit.map_or(UNLIMITED, |val| val as usize);

        // Fold needle using $UpCase from the first drive (all drives share
        // the same default table; a live volume table would override).
        let fold = self
            .drives
            .first()
            .map_or_else(uffs_text::CaseFold::default_table, |drive| drive.fold);
        let needle = if case_sensitive {
            pattern.to_owned()
        } else {
            let mut buf = Vec::with_capacity(pattern.len());
            fold.fold_into(pattern, &mut buf).to_owned()
        };
        let is_path = !is_match_all && !is_regex && crate::search::tree::is_path_pattern(&needle);

        if is_match_all {
            rows = super::query::collect_global_top_n(
                &self.drives,
                limit,
                self.sort_column,
                self.sort_desc,
                filter_mode,
                search_filters,
            );
        } else if is_regex {
            let regex_pattern = needle.strip_prefix('>').unwrap_or(&needle);
            match regex::RegexBuilder::new(regex_pattern)
                .case_insensitive(!case_sensitive)
                .build()
            {
                Ok(compiled_re) => {
                    let drive_results: Vec<Vec<DisplayRow>> = self
                        .drives
                        .par_iter()
                        .map(|drive| {
                            super::query::search_compact_drive_regex(drive, &compiled_re, limit)
                        })
                        .collect();
                    for drive_rows in drive_results {
                        rows.extend(drive_rows);
                    }
                    super::filters::apply_filter(&mut rows, filter_mode);
                    super::filters::apply_search_filters(&mut rows, search_filters);
                    sort_rows(
                        &mut rows,
                        self.sort_column,
                        self.sort_desc,
                        &self.extra_sort_tiers,
                    );
                    rows.truncate(limit);
                }
                Err(_err) => {
                    // Restore stashed drives before returning.
                    if let Some(rest) = stashed_drives {
                        self.drives.extend(rest);
                    }
                    self.last_results.clear();
                    return SearchResult {
                        rows: Vec::new(),
                        duration: start.elapsed(),
                        records_scanned: 0,
                    };
                }
            }
        } else {
            let drive_results: Vec<Vec<DisplayRow>> = self
                .drives
                .par_iter()
                .map(|drive| {
                    if is_path {
                        super::query::search_compact_drive_tree(drive, &needle, limit)
                    } else {
                        super::query::search_compact_drive(
                            drive,
                            &needle,
                            limit,
                            case_sensitive,
                            whole_word,
                        )
                    }
                })
                .collect();
            for drive_rows in drive_results {
                rows.extend(drive_rows);
            }
            super::filters::apply_filter(&mut rows, filter_mode);
            super::filters::apply_search_filters(&mut rows, search_filters);
            sort_rows(
                &mut rows,
                self.sort_column,
                self.sort_desc,
                &self.extra_sort_tiers,
            );
            rows.truncate(limit);
        }

        let scanned = self.drives.iter().map(|dr| dr.records.len()).sum();

        // Restore stashed drives if we filtered them out.
        if let Some(rest) = stashed_drives {
            self.drives.extend(rest);
        }
        let wall_ms = start.elapsed().as_millis();

        let mode = if is_match_all {
            "match-all"
        } else if is_regex {
            "regex"
        } else if is_path {
            "tree"
        } else {
            "trigram"
        };
        tracing::debug!(
            target: "cache_profile",
            wall_ms = %wall_ms,
            rows = rows.len(),
            scanned,
            mode,
            "search_total"
        );

        // Store results in last_results for TUI re-sort; return the
        // same rows by swapping ownership then cloning back.  This is
        // identical cost to the old clone_from — but callers that never
        // re-sort (CLI / daemon) can ignore last_results entirely.
        // Future optimisation: make SearchResult borrow from last_results.
        self.last_results = rows;
        SearchResult {
            rows: self.last_results.clone(),
            duration: start.elapsed(),
            records_scanned: scanned,
        }
    }

    /// Re-sort the last results by a different column.
    pub fn sort(&mut self, column: FieldId, descending: bool) {
        self.sort_column = column;
        self.sort_desc = descending;
        self.extra_sort_tiers.clear();
        sort_rows(&mut self.last_results, column, descending, &[]);
    }

    /// Cycle to the next sort column with a sensible default direction.
    pub fn cycle_sort(&mut self) {
        let next = self.sort_column.cycle_next();
        let new_desc = matches!(
            next.default_sort_direction(),
            Some(crate::search::field::SortDirection::Descending)
        );
        self.sort_column = next;
        self.sort_desc = new_desc;
        self.extra_sort_tiers.clear();
        sort_rows(
            &mut self.last_results,
            self.sort_column,
            self.sort_desc,
            &[],
        );
    }

    /// Toggle sort direction (ascending ↔ descending) and re-sort.
    pub fn toggle_sort_direction(&mut self) {
        self.sort_desc = !self.sort_desc;
        self.extra_sort_tiers.clear();
        sort_rows(
            &mut self.last_results,
            self.sort_column,
            self.sort_desc,
            &[],
        );
    }
}

// ── Sorting & DataFrame conversion ─────────────────────────────────────
// Extracted into `sorting.rs` for file-size policy compliance.
// Re-exported from `search/mod.rs` → `backend::*` so callers see no change.
pub use super::sorting::{
    dataframe_to_display_rows, display_rows_to_dataframe, format_sort_spec, parse_sort_spec,
    sort_rows, sort_rows_with_fold,
};

#[cfg(test)]
#[path = "backend_tests.rs"]
mod tests;
