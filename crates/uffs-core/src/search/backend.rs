//! Search backend types: display rows, sort columns, filter modes, and
//! multi-drive search orchestration.

use std::time::Instant;

use rayon::prelude::*;

use crate::compact::DriveCompactIndex;

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

/// Columns available for sorting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortColumn {
    /// Sort by filename.
    #[default]
    Name,
    /// Sort by file size.
    Size,
    /// Sort by allocated size on disk.
    SizeOnDisk,
    /// Sort by creation time.
    Created,
    /// Sort by last modified time.
    Modified,
    /// Sort by last access time.
    Accessed,
    /// Sort by full path.
    Path,
    /// Sort by drive letter.
    Drive,
    /// Sort by file extension.
    Extension,
    /// Sort by devicon file type.
    Type,
    /// Sort by descendant count.
    Descendants,
}

impl SortColumn {
    /// Human-readable label for status bar display.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Size => "Size",
            Self::SizeOnDisk => "SizeOnDisk",
            Self::Created => "Created",
            Self::Modified => "Modified",
            Self::Accessed => "Accessed",
            Self::Path => "Path",
            Self::Drive => "Drive",
            Self::Extension => "Extension",
            Self::Type => "Type",
            Self::Descendants => "Descendants",
        }
    }
}

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
    /// Which column to sort by.
    pub column: SortColumn,
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
    pub sort_column: SortColumn,
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
            sort_column: SortColumn::Modified,
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
        search_filters: &super::filters::SearchFilters,
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
        search_filters: &super::filters::SearchFilters,
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

        let needle = if case_sensitive {
            pattern.to_owned()
        } else {
            pattern.to_ascii_lowercase()
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
    pub fn sort(&mut self, column: SortColumn, descending: bool) {
        self.sort_column = column;
        self.sort_desc = descending;
        self.extra_sort_tiers.clear();
        sort_rows(&mut self.last_results, column, descending, &[]);
    }

    /// Cycle to the next sort column with a sensible default direction.
    pub fn cycle_sort(&mut self) {
        let (new_column, new_desc) = match self.sort_column {
            SortColumn::Name => (SortColumn::Size, true),
            SortColumn::Size => (SortColumn::SizeOnDisk, true),
            SortColumn::SizeOnDisk => (SortColumn::Created, true),
            SortColumn::Created => (SortColumn::Modified, true),
            SortColumn::Modified => (SortColumn::Accessed, true),
            SortColumn::Accessed => (SortColumn::Path, false),
            SortColumn::Path => (SortColumn::Drive, false),
            SortColumn::Drive => (SortColumn::Extension, false),
            SortColumn::Extension => (SortColumn::Type, false),
            SortColumn::Type => (SortColumn::Descendants, true),
            SortColumn::Descendants => (SortColumn::Name, false),
        };
        self.sort_column = new_column;
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

/// Pre-computed lowercase sort keys for a single row.
///
/// Stored alongside each `DisplayRow` during sorting (Schwartzian transform)
/// to avoid allocating inside the O(n·log n) comparator.
struct RowSortKey {
    /// Lowercase name.
    name: String,
    /// Lowercase path.
    path: String,
    /// Lowercase extension.
    ext: String,
}

/// Sort display rows by the given column, then by additional tiers, with a
/// final name-ascending tiebreaker.
///
/// String-based columns (Name, Path, Extension) use pre-computed lowercase
/// keys to avoid per-comparison allocation (Schwartzian transform).
pub fn sort_rows(
    rows: &mut [DisplayRow],
    column: SortColumn,
    descending: bool,
    extra_tiers: &[SortSpec],
) {
    if rows.len() <= 1 {
        return;
    }
    // Decorate: zip each row with its pre-computed keys.
    let mut decorated: Vec<(DisplayRow, RowSortKey)> = rows
        .iter_mut()
        .map(|row| {
            let key = RowSortKey {
                name: row.name().to_ascii_lowercase(),
                path: row.path.to_ascii_lowercase(),
                ext: row
                    .name()
                    .rsplit('.')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase(),
            };
            // Take ownership; we'll put it back after sorting.
            (core::mem::take(row), key)
        })
        .collect();

    // Sort the decorated pairs.
    decorated.sort_unstable_by(|(row_a, key_a), (row_b, key_b)| {
        let mut ord = compare_by_column(row_a, key_a, row_b, key_b, column);
        if descending {
            ord = ord.reverse();
        }
        for tier in extra_tiers {
            if ord != core::cmp::Ordering::Equal {
                break;
            }
            ord = compare_by_column(row_a, key_a, row_b, key_b, tier.column);
            if tier.descending {
                ord = ord.reverse();
            }
        }
        // Name tiebreaker.
        if ord == core::cmp::Ordering::Equal
            && column != SortColumn::Name
            && !extra_tiers
                .iter()
                .any(|tier| tier.column == SortColumn::Name)
        {
            ord = key_a.name.cmp(&key_b.name);
        }
        ord
    });

    // Undecorate: move sorted rows back into the slice.
    for (dest, (row, _key)) in rows.iter_mut().zip(decorated) {
        *dest = row;
    }
}

/// Compare two rows by a single column (natural / ascending order).
fn compare_by_column(
    row_a: &DisplayRow,
    key_a: &RowSortKey,
    row_b: &DisplayRow,
    key_b: &RowSortKey,
    column: SortColumn,
) -> core::cmp::Ordering {
    match column {
        SortColumn::Name => key_a.name.cmp(&key_b.name),
        SortColumn::Size => row_a.size.cmp(&row_b.size),
        SortColumn::SizeOnDisk => row_a.allocated.cmp(&row_b.allocated),
        SortColumn::Created => row_a.created.cmp(&row_b.created),
        SortColumn::Modified => row_a.modified.cmp(&row_b.modified),
        SortColumn::Accessed => row_a.accessed.cmp(&row_b.accessed),
        SortColumn::Path => key_a.path.cmp(&key_b.path),
        SortColumn::Drive => row_a.drive.cmp(&row_b.drive),
        SortColumn::Extension => key_a.ext.cmp(&key_b.ext),
        SortColumn::Type => {
            let icon_a = devicons::icon_for_file(row_a.name(), &None).icon;
            let icon_b = devicons::icon_for_file(row_b.name(), &None).icon;
            icon_a.cmp(&icon_b)
        }
        SortColumn::Descendants => row_a.descendants.cmp(&row_b.descendants),
    }
}

/// Parse a `--sort` value like `"name:asc,modified:desc"` into sort specs.
#[must_use]
pub fn parse_sort_spec(sort_str: &str) -> Vec<SortSpec> {
    let mut specs = Vec::new();
    for raw_part in sort_str.split(',') {
        let trimmed = raw_part.trim();
        let (col_str, dir_str) = if let Some((col, dir)) = trimmed.split_once(':') {
            (col.trim(), Some(dir.trim()))
        } else {
            (trimmed, None)
        };
        let parsed_column = match col_str.to_ascii_lowercase().as_str() {
            "name" => Some(SortColumn::Name),
            "size" => Some(SortColumn::Size),
            "sizeondisk" | "allocated" => Some(SortColumn::SizeOnDisk),
            "created" => Some(SortColumn::Created),
            "modified" | "date" | "written" => Some(SortColumn::Modified),
            "accessed" => Some(SortColumn::Accessed),
            "path" => Some(SortColumn::Path),
            "drive" => Some(SortColumn::Drive),
            "ext" | "extension" => Some(SortColumn::Extension),
            "type" => Some(SortColumn::Type),
            "descendants" => Some(SortColumn::Descendants),
            _ => None,
        };
        if let Some(column) = parsed_column {
            let descending = match dir_str {
                Some("desc") => true,
                Some("asc") => false,
                _ => match column {
                    SortColumn::Size
                    | SortColumn::SizeOnDisk
                    | SortColumn::Created
                    | SortColumn::Modified
                    | SortColumn::Accessed
                    | SortColumn::Descendants => true,
                    SortColumn::Name
                    | SortColumn::Path
                    | SortColumn::Drive
                    | SortColumn::Extension
                    | SortColumn::Type => false,
                },
            };
            specs.push(SortSpec { column, descending });
        }
    }
    specs
}

/// Convert `DisplayRow` results to a Polars `DataFrame` with standard MFT
/// column names so existing CLI output formatters can consume it.
///
/// This creates a **small** `DataFrame` (only matching rows, not the full MFT).
///
/// # Errors
///
/// Returns an error if `DataFrame` construction fails.
pub fn display_rows_to_dataframe(
    rows: &[DisplayRow],
) -> uffs_polars::PolarsResult<uffs_polars::DataFrame> {
    use uffs_polars::{Column, DataFrame, columns};

    let names: Vec<&str> = rows.iter().map(DisplayRow::name).collect();
    let paths: Vec<&str> = rows.iter().map(|row| row.path.as_str()).collect();
    let sizes: Vec<u64> = rows.iter().map(|row| row.size).collect();
    let allocated: Vec<u64> = rows.iter().map(|row| row.allocated).collect();
    let created: Vec<i64> = rows.iter().map(|row| row.created).collect();
    let modified: Vec<i64> = rows.iter().map(|row| row.modified).collect();
    let accessed: Vec<i64> = rows.iter().map(|row| row.accessed).collect();
    let flags: Vec<u32> = rows.iter().map(|row| row.flags).collect();
    let drives: Vec<String> = rows.iter().map(|row| format!("{}:", row.drive)).collect();
    let descendants: Vec<u32> = rows.iter().map(|row| row.descendants).collect();
    let treesize: Vec<u64> = rows.iter().map(|row| row.treesize).collect();

    // path_only = directory portion of path (up to and including last backslash).
    // Uses pre-computed name_start offset — zero-cost slice, no rfind needed.
    let path_only: Vec<&str> = rows
        .iter()
        .map(|row| {
            let ns = row.name_start as usize;
            row.path.get(..ns).unwrap_or(&row.path)
        })
        .collect();

    DataFrame::new(
        rows.len(),
        vec![
            Column::new(columns::NAME.into(), &names),
            Column::new(columns::PATH.into(), &paths),
            Column::new("path_only".into(), &path_only),
            Column::new(columns::SIZE.into(), &sizes),
            Column::new("allocated_size".into(), &allocated),
            Column::new(columns::CREATED.into(), &created),
            Column::new(columns::MODIFIED.into(), &modified),
            Column::new(columns::ACCESSED.into(), &accessed),
            Column::new(columns::FLAGS.into(), &flags),
            Column::new("drive".into(), &drives),
            Column::new("descendants".into(), &descendants),
            Column::new("treesize".into(), &treesize),
        ],
    )
}

/// Convert a legacy Polars `DataFrame` into `Vec<DisplayRow>`.
///
/// Handles both "new" column layouts (from `display_rows_to_dataframe`) and
/// legacy MFT layouts (from `results_to_dataframe`). Timestamps may be
/// plain `Int64` or `Datetime(Microseconds)` — both are handled.
///
/// Columns that don't exist get sensible defaults (0 for numbers, empty
/// strings, `'?'` for drive).
///
/// # Errors
///
/// Returns an error if `DataFrame` column extraction fails in an unexpected
/// way.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "u32/u64 downcasts bounded by record counts; i64→u64 safe for MFT sizes"
)]
pub fn dataframe_to_display_rows(
    data_frame: &uffs_polars::DataFrame,
) -> Result<Vec<DisplayRow>, String> {
    let height = data_frame.height();
    if height == 0 {
        return Ok(Vec::new());
    }

    let mut rows = Vec::with_capacity(height);
    for row_idx in 0..height {
        let path = col_str(data_frame, "path", row_idx).unwrap_or_default();
        let drive = col_str(data_frame, "drive", row_idx)
            .and_then(|val| val.chars().next())
            .unwrap_or('?');
        let size = col_u64(data_frame, "size", row_idx);
        let allocated = col_u64(data_frame, "allocated_size", row_idx);
        let flags = col_u64(data_frame, "flags", row_idx) as u32;
        let is_directory = col_bool(data_frame, "is_directory", row_idx);
        let created = col_timestamp(data_frame, "created", row_idx);
        let modified = col_timestamp(data_frame, "modified", row_idx);
        let accessed = col_timestamp(data_frame, "accessed", row_idx);
        let descendants = col_u64(data_frame, "descendants", row_idx) as u32;
        let treesize = col_u64(data_frame, "treesize", row_idx);
        let tree_allocated = col_u64(data_frame, "tree_allocated", row_idx);

        rows.push(DisplayRow::new(
            drive,
            path,
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
        ));
    }
    Ok(rows)
}

/// Extract a string value from a `DataFrame` column at `row_idx`.
fn col_str(data_frame: &uffs_polars::DataFrame, col_name: &str, row_idx: usize) -> Option<String> {
    data_frame
        .column(col_name)
        .ok()
        .and_then(|column| column.str().ok())
        .and_then(|chunked| chunked.get(row_idx).map(String::from))
}

/// Extract a `u64` value from a `DataFrame` column (handles `UInt64`,
/// `Int64`, `UInt32` dtype).
#[allow(clippy::cast_sign_loss)]
fn col_u64(data_frame: &uffs_polars::DataFrame, col_name: &str, row_idx: usize) -> u64 {
    data_frame
        .column(col_name)
        .ok()
        .and_then(|column| {
            column
                .u64()
                .ok()
                .and_then(|arr| arr.get(row_idx))
                .or_else(|| {
                    column
                        .i64()
                        .ok()
                        .and_then(|arr| arr.get(row_idx).map(|val| val as u64))
                })
                .or_else(|| {
                    column
                        .u32()
                        .ok()
                        .and_then(|arr| arr.get(row_idx).map(u64::from))
                })
        })
        .unwrap_or(0)
}

/// Extract a boolean value from a `DataFrame` column.
#[allow(clippy::single_call_fn)]
fn col_bool(data_frame: &uffs_polars::DataFrame, col_name: &str, row_idx: usize) -> bool {
    data_frame
        .column(col_name)
        .ok()
        .and_then(|column| column.bool().ok())
        .and_then(|chunked| chunked.get(row_idx))
        .unwrap_or(false)
}

/// Extract a timestamp (microseconds `i64`) from a `DataFrame` column.
///
/// Handles both plain `Int64` and `Datetime(Microseconds)` dtypes.
fn col_timestamp(data_frame: &uffs_polars::DataFrame, col_name: &str, row_idx: usize) -> i64 {
    data_frame
        .column(col_name)
        .ok()
        .and_then(|column| {
            // Try direct i64 first (from display_rows_to_dataframe).
            column
                .i64()
                .ok()
                .and_then(|arr| arr.get(row_idx))
                .or_else(|| {
                    // Try Datetime(Microseconds) (from legacy MftIndex DataFrames).
                    // `.phys` gives the underlying Int64 chunked array.
                    column.datetime().ok().and_then(|dt| dt.phys.get(row_idx))
                })
        })
        .unwrap_or(0)
}

/// Format the current sort state back into a CLI-compatible sort string.
#[must_use]
pub fn format_sort_spec(primary: SortColumn, primary_desc: bool, extra: &[SortSpec]) -> String {
    let mut parts = Vec::with_capacity(1 + extra.len());
    let dir = |desc: bool| if desc { "desc" } else { "asc" };
    parts.push(format!(
        "{}:{}",
        primary.label().to_ascii_lowercase(),
        dir(primary_desc)
    ));
    for spec in extra {
        parts.push(format!(
            "{}:{}",
            spec.column.label().to_ascii_lowercase(),
            dir(spec.descending)
        ));
    }
    parts.join(",")
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod tests;
