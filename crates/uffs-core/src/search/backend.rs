//! Search backend types: display rows, sort columns, filter modes, and
//! multi-drive search orchestration.

use alloc::sync::Arc;
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

/// Parameters for a search operation on [`MultiDriveBackend`].
///
/// Bundles all search-time knobs into a single struct so callers (daemon,
/// CLI, tests) use one consistent API and `search` stays under the
/// `clippy::too_many_arguments` threshold.
#[derive(Debug)]
pub struct SearchRequest<'a> {
    /// The search pattern (glob, substring, regex with `>` prefix, or `*`).
    pub pattern: &'a str,
    /// Whether matching is case-sensitive.
    pub case_sensitive: bool,
    /// Whether to match whole words only.
    pub whole_word: bool,
    /// Whether to match against the full path (not just filename).
    pub match_path: bool,
    /// Maximum number of results to return (`None` = unlimited).
    pub result_limit: Option<u32>,
    /// File / directory filter mode.
    pub filter_mode: FilterMode,
    /// Mutable search filters (extensions, dates, size, etc.).
    pub search_filters: &'a mut super::filters::SearchFilters,
    /// Drive-letter filter: only search drives whose letter is in this
    /// slice.  An empty slice means "search all loaded drives".
    pub drives_filter: &'a [char],
}

impl<'a> SearchRequest<'a> {
    /// Create a minimal request with only the required fields.
    ///
    /// All optional flags default to `false` / `None` / `FilterMode::All`.
    #[must_use]
    pub const fn new(
        pattern: &'a str,
        search_filters: &'a mut super::filters::SearchFilters,
    ) -> Self {
        Self {
            pattern,
            case_sensitive: false,
            whole_word: false,
            match_path: false,
            result_limit: None,
            filter_mode: FilterMode::All,
            search_filters,
            drives_filter: &[],
        }
    }
}

/// Shared, immutable index snapshot for concurrent query access.
///
/// Holds all loaded drives wrapped in per-drive `Arc`s.  Wrapped in an
/// outer `Arc` so concurrent queries can hold cheap references while
/// mutations (load, refresh, remove) atomically swap the pointer.
///
/// Created by the daemon's `IndexManager` — the TUI uses
/// [`MultiDriveBackend`] directly.
pub struct DriveIndex {
    /// Per-drive compact indices, each individually `Arc`-wrapped so
    /// adding/removing a single drive copies only `Arc` pointers (~8
    /// bytes each), not the underlying record data (~250 MB/drive).
    pub drives: Vec<Arc<DriveCompactIndex>>,
}

impl DriveIndex {
    /// Create an empty index with no drives loaded.
    #[must_use]
    pub const fn new() -> Self {
        Self { drives: Vec::new() }
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
}

impl Default for DriveIndex {
    fn default() -> Self {
        Self::new()
    }
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

    /// Search all loaded drives using the given request.
    ///
    /// This is the single search entry point.  Results are sorted by the
    /// backend's current `sort_column` / `sort_desc`, then truncated to
    /// `result_limit`.
    ///
    /// When `drives_filter` is non-empty, only drives whose letter is in
    /// the slice are searched.
    #[expect(
        clippy::too_many_lines,
        reason = "search dispatch with three modes and a drive filter"
    )]
    pub fn search(&mut self, req: SearchRequest<'_>) -> SearchResult {
        let SearchRequest {
            pattern,
            case_sensitive,
            whole_word,
            match_path,
            result_limit,
            filter_mode,
            search_filters,
            drives_filter,
        } = req;

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
            // Post-filters that require resolved paths (type, path_contains,
            // bulkiness, path_length) are not applied inside
            // `collect_global_top_n` — they operate on `DisplayRow`, not
            // `CompactRecord`.  Apply them here, matching the regex and
            // normal-search branches.
            if search_filters.needs_display_row_filter() {
                super::filters::apply_search_filters(&mut rows, search_filters);
            }
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
                            match_path,
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
        sort_rows(&mut self.last_results, self.sort_column, self.sort_desc, &[
        ]);
    }

    /// Toggle sort direction (ascending ↔ descending) and re-sort.
    pub fn toggle_sort_direction(&mut self) {
        self.sort_desc = !self.sort_desc;
        self.extra_sort_tiers.clear();
        sort_rows(&mut self.last_results, self.sort_column, self.sort_desc, &[
        ]);
    }
}

// ── Free-function search for concurrent access ───────────────────────

/// Execute a search against a shared [`DriveIndex`] snapshot.
///
/// All per-query state (sort, filters, limit) is passed as parameters —
/// this function **never mutates the index**, so it is safe to call from
/// multiple threads/tasks simultaneously.
///
/// This is the daemon-facing entry point.  The TUI continues to use
/// [`MultiDriveBackend::search()`] which wraps its own per-query state.
#[expect(
    clippy::too_many_lines,
    reason = "search dispatch with three modes and a drive filter — mirrors MultiDriveBackend::search"
)]
#[allow(
    clippy::cognitive_complexity,
    reason = "search dispatch with pattern/regex/glob modes, sort, and drive filter"
)]
pub fn search_index(
    index: &DriveIndex,
    req: SearchRequest<'_>,
    sort_column: FieldId,
    sort_desc: bool,
    extra_sort_tiers: &[SortSpec],
) -> SearchResult {
    let SearchRequest {
        pattern,
        case_sensitive,
        whole_word,
        match_path,
        result_limit,
        filter_mode,
        search_filters,
        drives_filter,
    } = req;

    let start = Instant::now();

    if pattern.is_empty() {
        return SearchResult {
            rows: Vec::new(),
            duration: start.elapsed(),
            records_scanned: 0,
        };
    }

    // Filter drives without mutation — just skip non-matching ones.
    let active_drives: Vec<&DriveCompactIndex> = index
        .drives
        .iter()
        .filter(|dr| {
            drives_filter.is_empty()
                || drives_filter
                    .iter()
                    .any(|fl| fl.eq_ignore_ascii_case(&dr.letter))
        })
        .map(Arc::as_ref)
        .collect();

    let is_match_all = pattern == "*";
    let is_regex = pattern.starts_with('>') && pattern.len() > 1;
    let limit = result_limit.map_or(UNLIMITED, |val| val as usize);

    // Fold needle using $UpCase from the first drive.
    let fold = active_drives
        .first()
        .map_or_else(uffs_text::CaseFold::default_table, |drive| drive.fold);
    let needle = if case_sensitive {
        pattern.to_owned()
    } else {
        let mut buf = Vec::with_capacity(pattern.len());
        fold.fold_into(pattern, &mut buf).to_owned()
    };
    let is_path = !is_match_all && !is_regex && crate::search::tree::is_path_pattern(&needle);

    let mut rows = Vec::new();

    tracing::debug!(
        pattern,
        sort_column = ?sort_column,
        sort_desc,
        limit,
        is_match_all,
        hide_system = search_filters.hide_system,
        filters_empty = search_filters.is_empty(),
        "[1] search_index entry"
    );

    if is_match_all {
        let t_top_n = Instant::now();
        rows = super::query::collect_global_top_n(
            &active_drives,
            limit,
            sort_column,
            sort_desc,
            filter_mode,
            search_filters,
        );
        let top_n_ms = t_top_n.elapsed().as_millis();
        tracing::debug!(rows = rows.len(), top_n_ms, "[2] collect_global_top_n done");
        if search_filters.needs_display_row_filter() {
            let t_post = Instant::now();
            super::filters::apply_search_filters(&mut rows, search_filters);
            tracing::debug!(
                rows_after = rows.len(),
                post_filter_ms = t_post.elapsed().as_millis(),
                "[3] post-filter done"
            );
        }
    } else if is_regex {
        let regex_pattern = needle.strip_prefix('>').unwrap_or(&needle);
        match regex::RegexBuilder::new(regex_pattern)
            .case_insensitive(!case_sensitive)
            .build()
        {
            Ok(compiled_re) => {
                let drive_results: Vec<Vec<DisplayRow>> = active_drives
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
                sort_rows(&mut rows, sort_column, sort_desc, extra_sort_tiers);
                rows.truncate(limit);
            }
            Err(_err) => {
                return SearchResult {
                    rows: Vec::new(),
                    duration: start.elapsed(),
                    records_scanned: 0,
                };
            }
        }
    } else {
        let drive_results: Vec<Vec<DisplayRow>> = active_drives
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
                        match_path,
                    )
                }
            })
            .collect();
        for drive_rows in drive_results {
            rows.extend(drive_rows);
        }
        super::filters::apply_filter(&mut rows, filter_mode);
        super::filters::apply_search_filters(&mut rows, search_filters);
        sort_rows(&mut rows, sort_column, sort_desc, extra_sort_tiers);
        rows.truncate(limit);
    }

    let scanned = active_drives.iter().map(|dr| dr.records.len()).sum();
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
        "search_index_total"
    );

    SearchResult {
        rows,
        duration: start.elapsed(),
        records_scanned: scanned,
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
