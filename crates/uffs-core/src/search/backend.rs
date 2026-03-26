//! Search backend types: display rows, sort columns, filter modes, and
//! multi-drive search orchestration.

use std::time::Instant;

use rayon::prelude::*;

use crate::compact::DriveCompactIndex;

/// Maximum results returned per search (prevents UI lag on broad patterns).
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    /// Sort by filename.
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
    #[expect(
        clippy::too_many_lines,
        reason = "search dispatch with three modes; splitting would scatter related logic"
    )]
    pub fn search(
        &mut self,
        pattern: &str,
        case_sensitive: bool,
        whole_word: bool,
        result_limit: Option<u32>,
        filter_mode: FilterMode,
        search_filters: &super::filters::SearchFilters,
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

        let is_match_all = pattern == "*";
        let is_regex = pattern.starts_with('>') && pattern.len() > 1;
        let limit = if let Some(n) = result_limit {
            n as usize
        } else if is_match_all {
            DEFAULT_RESULT_LIMIT
        } else if pattern.len() <= 2 {
            SHORT_PATTERN_LIMIT
        } else {
            DEFAULT_RESULT_LIMIT
        };

        let needle = if case_sensitive {
            pattern.to_owned()
        } else {
            pattern.to_ascii_lowercase()
        };
        let is_path =
            !is_match_all && !is_regex && crate::search::tree::is_path_pattern(&needle);

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

        self.last_results.clone_from(&rows);
        SearchResult {
            rows,
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

/// Sort display rows by the given column, then by additional tiers, with a
/// final name-ascending tiebreaker.
pub fn sort_rows(
    rows: &mut [DisplayRow],
    column: SortColumn,
    descending: bool,
    extra_tiers: &[SortSpec],
) {
    rows.sort_unstable_by(|row_a, row_b| {
        let mut ord = compare_by_column(row_a, row_b, column);
        if descending {
            ord = ord.reverse();
        }

        for tier in extra_tiers {
            if ord != core::cmp::Ordering::Equal {
                break;
            }
            ord = compare_by_column(row_a, row_b, tier.column);
            if tier.descending {
                ord = ord.reverse();
            }
        }

        if ord == core::cmp::Ordering::Equal
            && column != SortColumn::Name
            && !extra_tiers
                .iter()
                .any(|tier| tier.column == SortColumn::Name)
        {
            ord = row_a.name.to_lowercase().cmp(&row_b.name.to_lowercase());
        }

        ord
    });
}

/// Compare two rows by a single column (natural / ascending order).
fn compare_by_column(
    row_a: &DisplayRow,
    row_b: &DisplayRow,
    column: SortColumn,
) -> core::cmp::Ordering {
    match column {
        SortColumn::Name => row_a.name.to_lowercase().cmp(&row_b.name.to_lowercase()),
        SortColumn::Size => row_a.size.cmp(&row_b.size),
        SortColumn::SizeOnDisk => row_a.allocated.cmp(&row_b.allocated),
        SortColumn::Created => row_a.created.cmp(&row_b.created),
        SortColumn::Modified => row_a.modified.cmp(&row_b.modified),
        SortColumn::Accessed => row_a.accessed.cmp(&row_b.accessed),
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
        if let Some(column) = parse_sort_column(col_str) {
            let descending = match dir_str {
                Some("desc") => true,
                Some("asc") => false,
                _ => default_sort_direction(column),
            };
            specs.push(SortSpec { column, descending });
        }
    }
    specs
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

/// Map a column name string to a `SortColumn`.
fn parse_sort_column(name: &str) -> Option<SortColumn> {
    match name.to_ascii_lowercase().as_str() {
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
    }
}

/// Sensible default direction for each sort column.
const fn default_sort_direction(column: SortColumn) -> bool {
    match column {
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
    }
}
