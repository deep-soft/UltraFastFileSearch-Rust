//! Sorting and DataFrame conversion for search results.
//!
//! Extracted from `backend.rs` for file-size policy compliance.
//! Re-exported via `pub use` in `backend.rs` — callers see no change.

use super::backend::{DisplayRow, SortColumn, SortSpec};

/// Pre-computed folded sort keys for a single row.
///
/// Stored alongside each `DisplayRow` during sorting (Schwartzian transform)
/// to avoid allocating inside the O(n·log n) comparator.
struct RowSortKey {
    /// Folded name.
    name: String,
    /// Folded path.
    path: String,
    /// Folded extension.
    ext: String,
}

/// Sort display rows by the given column, then by additional tiers, with a
/// final name-ascending tiebreaker.
///
/// String-based columns (Name, Path, Extension) use pre-computed folded
/// keys via `CaseFold` to avoid per-comparison allocation (Schwartzian
/// transform).
pub fn sort_rows(
    rows: &mut [DisplayRow],
    column: SortColumn,
    descending: bool,
    extra_tiers: &[SortSpec],
) {
    sort_rows_with_fold(
        rows,
        column,
        descending,
        extra_tiers,
        uffs_text::CaseFold::default_table(),
    );
}

/// Sort display rows using a specific `CaseFold` engine.
pub fn sort_rows_with_fold(
    rows: &mut [DisplayRow],
    column: SortColumn,
    descending: bool,
    extra_tiers: &[SortSpec],
    fold: uffs_text::CaseFold,
) {
    if rows.len() <= 1 {
        return;
    }
    // Decorate: zip each row with pre-computed folded keys.
    let mut fold_buf: Vec<u8> = Vec::with_capacity(256);
    let mut decorated: Vec<(DisplayRow, RowSortKey)> = rows
        .iter_mut()
        .map(|row| {
            let key = RowSortKey {
                name: fold.fold_into(row.name(), &mut fold_buf).to_owned(),
                path: fold.fold_into(&row.path, &mut fold_buf).to_owned(),
                ext: fold
                    .fold_into(row.name().rsplit('.').next().unwrap_or(""), &mut fold_buf)
                    .to_owned(),
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
    let path_only: Vec<&str> = rows.iter().map(|row| row.path_dir()).collect();

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

// ── DataFrame column helpers (private) ────────────────────────────────

/// Extract a `String` from a `DataFrame` column.
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
