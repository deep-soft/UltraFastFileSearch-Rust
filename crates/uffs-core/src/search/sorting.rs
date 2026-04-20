// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Sorting and `DataFrame` conversion for search results.
//!
//! Extracted from `backend.rs` for file-size policy compliance.
//! Re-exported via `pub use` in `backend.rs` — callers see no change.

use super::backend::{DisplayRow, SortSpec};
use super::derived::{bulkiness_for_row, semantic_type_for_row, tree_allocated_for_row};
use super::field::FieldId;

/// Pre-computed folded sort keys for a single row.
///
/// Stored alongside each `DisplayRow` during sorting (Schwartzian transform)
/// to avoid allocating inside the O(n·log n) comparator.
///
/// Only fields that `SortKeyNeeds::analyze` marks as required for the active
/// column + tiers are populated; the rest stay as `String::new()` (zero-cost,
/// no heap allocation because `String` has a null pointer representation
/// for empty).  This is a major hot-path optimisation: for a typical
/// numeric sort (Modified-DESC, Size, etc.) only `name` is needed as a
/// tiebreaker, so we save 4 allocations per row — ~170 ms at 168 K rows
/// on Windows based on the v0.5.54 deep-profile data.
struct RowSortKey {
    /// Folded name (always populated — used as the universal tiebreaker).
    name: String,
    /// Folded path.  Populated only when the column/tiers include
    /// `FieldId::Path`.
    path: String,
    /// Folded directory path only.  Populated only when the column/tiers
    /// include `FieldId::PathOnly`.
    path_only: String,
    /// Folded extension.  Populated only when the column/tiers include
    /// `FieldId::Extension`.
    ext: String,
    /// Folded semantic type/category.  Populated only when the column/tiers
    /// include `FieldId::Type`.
    file_type: String,
}

/// Bitmask of which `RowSortKey` fields the active sort needs.
///
/// Computed once upfront from the primary column + `extra_tiers`; the
/// decorate loop then skips folding + allocating keys that no comparator
/// branch will ever touch.  The name key is always needed because
/// `sort_rows_with_fold` uses it as a universal tiebreaker across all
/// numeric / flag columns, so it is not represented here.
///
/// Stored as a `u8` bitfield (not four booleans) to satisfy clippy's
/// `struct_excessive_bools` lint and keep the struct `Copy`-friendly.
#[derive(Debug, Default, Clone, Copy)]
struct SortKeyNeeds(u8);

impl SortKeyNeeds {
    /// Flag bit: the `path` folded key is read by some comparator branch.
    const PATH: u8 = 0b0001;
    /// Flag bit: the `path_only` folded key is read.
    const PATH_ONLY: u8 = 0b0010;
    /// Flag bit: the `ext` folded key is read.
    const EXT: u8 = 0b0100;
    /// Flag bit: the `file_type` folded key is read.
    const FILE_TYPE: u8 = 0b1000;

    /// Walk the primary column + tiers and flag which string keys the
    /// comparator will actually read.  Numeric / boolean-flag columns
    /// never read a folded key, so they contribute no bits.
    fn analyze(column: FieldId, extra_tiers: &[SortSpec]) -> Self {
        let mut bits = Self::bit_for(column);
        for tier in extra_tiers {
            bits |= Self::bit_for(tier.column);
        }
        Self(bits)
    }

    /// Map a single `FieldId` to the key-needs bit it implies.  `FieldId::Name`
    /// and every numeric / flag column map to zero because either the
    /// always-populated `key.name` is sufficient (Name) or no folded key is
    /// consulted at all (numeric / flag).
    const fn bit_for(column: FieldId) -> u8 {
        match column {
            FieldId::Path => Self::PATH,
            FieldId::PathOnly => Self::PATH_ONLY,
            FieldId::Extension => Self::EXT,
            FieldId::Type => Self::FILE_TYPE,
            FieldId::Name
            | FieldId::Size
            | FieldId::SizeOnDisk
            | FieldId::Created
            | FieldId::Modified
            | FieldId::Accessed
            | FieldId::Drive
            | FieldId::Descendants
            | FieldId::TreeSize
            | FieldId::TreeAllocated
            | FieldId::Bulkiness
            | FieldId::NameLength
            | FieldId::PathLength
            | FieldId::Hidden
            | FieldId::System
            | FieldId::Archive
            | FieldId::ReadOnly
            | FieldId::Compressed
            | FieldId::Encrypted
            | FieldId::Sparse
            | FieldId::Reparse
            | FieldId::Offline
            | FieldId::NotIndexed
            | FieldId::Temporary
            | FieldId::Virtual
            | FieldId::Pinned
            | FieldId::Unpinned
            | FieldId::Integrity
            | FieldId::NoScrub
            | FieldId::DirectoryFlag
            | FieldId::RecallOnOpen
            | FieldId::RecallOnDataAccess
            | FieldId::Attributes
            | FieldId::AttributeValue
            | FieldId::ParityAttributes => 0,
        }
    }

    /// Returns `true` if the folded `path` key is referenced by any
    /// comparator branch for the active sort.
    const fn path(self) -> bool {
        self.0 & Self::PATH != 0
    }

    /// Returns `true` if the folded `path_only` key is referenced.
    const fn path_only(self) -> bool {
        self.0 & Self::PATH_ONLY != 0
    }

    /// Returns `true` if the folded `ext` key is referenced.
    const fn ext(self) -> bool {
        self.0 & Self::EXT != 0
    }

    /// Returns `true` if the folded `file_type` key is referenced.
    const fn file_type(self) -> bool {
        self.0 & Self::FILE_TYPE != 0
    }
}

/// Sort display rows by the given column, then by additional tiers, with a
/// final name-ascending tiebreaker.
///
/// String-based columns (Name, Path, Extension) use pre-computed folded
/// keys via `CaseFold` to avoid per-comparison allocation (Schwartzian
/// transform).
pub fn sort_rows(
    rows: &mut [DisplayRow],
    column: FieldId,
    descending: bool,
    extra_tiers: &[SortSpec],
) {
    sort_rows_with_fold(
        rows,
        column,
        descending,
        extra_tiers,
        uffs_text::case_fold::CaseFold::default_table(),
    );
}

/// Sort display rows using a specific `CaseFold` engine.
pub fn sort_rows_with_fold(
    rows: &mut [DisplayRow],
    column: FieldId,
    descending: bool,
    extra_tiers: &[SortSpec],
    fold: uffs_text::case_fold::CaseFold,
) {
    if rows.len() <= 1 {
        return;
    }
    // Determine which folded-key fields the active sort column + tiers will
    // actually read.  For numeric / boolean-flag sorts this leaves the four
    // directional keys empty, saving 4 heap allocations per row.  The name
    // key is always populated (used as the universal tiebreaker).
    let needs = SortKeyNeeds::analyze(column, extra_tiers);

    // Decorate: zip each row with pre-computed folded keys.
    let mut fold_buf: Vec<u8> = Vec::with_capacity(256);
    let mut decorated: Vec<(DisplayRow, RowSortKey)> = rows
        .iter_mut()
        .map(|row| {
            let key = RowSortKey {
                name: fold.fold_into(row.name(), &mut fold_buf).to_owned(),
                path: if needs.path() {
                    fold.fold_into(&row.path, &mut fold_buf).to_owned()
                } else {
                    String::new()
                },
                path_only: if needs.path_only() {
                    fold.fold_into(row.path_dir(), &mut fold_buf).to_owned()
                } else {
                    String::new()
                },
                ext: if needs.ext() {
                    fold.fold_into(row.name().rsplit('.').next().unwrap_or(""), &mut fold_buf)
                        .to_owned()
                } else {
                    String::new()
                },
                file_type: if needs.file_type() {
                    fold.fold_into(semantic_type_for_row(row), &mut fold_buf)
                        .to_owned()
                } else {
                    String::new()
                },
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
        // Name tiebreaker (case-folded, then raw for determinism).
        if ord == core::cmp::Ordering::Equal
            && column != FieldId::Name
            && !extra_tiers.iter().any(|tier| tier.column == FieldId::Name)
        {
            ord = key_a
                .name
                .cmp(&key_b.name)
                .then_with(|| row_a.name().cmp(row_b.name()));
        }
        ord
    });

    // Undecorate: move sorted rows back into the slice.
    for (dest, (row, _key)) in rows.iter_mut().zip(decorated) {
        *dest = row;
    }
}

/// Compare two rows by a single column (natural / ascending order).
///
/// String-based columns use a **two-phase comparison** for deterministic
/// ordering:
///   1. Case-folded keys (groups variants together: `TEXT` ≈ `text`)
///   2. Unicode codepoint tiebreaker (deterministic within the group: `TEXT` <
///      `text`)
///
/// This ensures stable, reproducible sort order regardless of the
/// underlying `sort_unstable_by` implementation.
fn compare_by_column(
    row_a: &DisplayRow,
    key_a: &RowSortKey,
    row_b: &DisplayRow,
    key_b: &RowSortKey,
    column: FieldId,
) -> core::cmp::Ordering {
    match column {
        FieldId::Size => row_a.size.cmp(&row_b.size),
        FieldId::SizeOnDisk => row_a.allocated.cmp(&row_b.allocated),
        FieldId::Created => row_a.created.cmp(&row_b.created),
        FieldId::Modified => row_a.modified.cmp(&row_b.modified),
        FieldId::Accessed => row_a.accessed.cmp(&row_b.accessed),
        FieldId::Path => key_a
            .path
            .cmp(&key_b.path)
            .then_with(|| row_a.path.cmp(&row_b.path)),
        FieldId::PathOnly => key_a
            .path_only
            .cmp(&key_b.path_only)
            .then_with(|| row_a.path_dir().cmp(row_b.path_dir())),
        FieldId::Drive => row_a.drive.cmp(&row_b.drive),
        FieldId::Extension => key_a.ext.cmp(&key_b.ext).then_with(|| {
            let ext_a = row_a.name().rsplit('.').next().unwrap_or("");
            let ext_b = row_b.name().rsplit('.').next().unwrap_or("");
            ext_a.cmp(ext_b)
        }),
        FieldId::Type => key_a
            .file_type
            .cmp(&key_b.file_type)
            .then_with(|| semantic_type_for_row(row_a).cmp(semantic_type_for_row(row_b))),
        FieldId::Descendants => row_a.descendants.cmp(&row_b.descendants),
        FieldId::TreeSize => row_a.treesize.cmp(&row_b.treesize),
        FieldId::TreeAllocated => tree_allocated_for_row(row_a).cmp(&tree_allocated_for_row(row_b)),
        FieldId::Bulkiness => bulkiness_for_row(row_a).cmp(&bulkiness_for_row(row_b)),
        FieldId::NameLength => row_a
            .name()
            .chars()
            .count()
            .cmp(&row_b.name().chars().count()),
        FieldId::PathLength => row_a.path.chars().count().cmp(&row_b.path.chars().count()),
        // ── Boolean attribute fields: sort by flag bit, tiebreak on name ──
        FieldId::Hidden
        | FieldId::System
        | FieldId::Archive
        | FieldId::ReadOnly
        | FieldId::Compressed
        | FieldId::Encrypted
        | FieldId::Sparse
        | FieldId::Reparse
        | FieldId::Offline
        | FieldId::NotIndexed
        | FieldId::Temporary
        | FieldId::Virtual
        | FieldId::Pinned
        | FieldId::Unpinned
        | FieldId::Integrity
        | FieldId::NoScrub
        | FieldId::DirectoryFlag
        | FieldId::RecallOnOpen
        | FieldId::RecallOnDataAccess => {
            let mask = field_to_attr_bit(column);
            let a_set = row_a.flags & mask != 0;
            let b_set = row_b.flags & mask != 0;
            // true > false so that desc puts flagged files first
            a_set
                .cmp(&b_set)
                .then_with(|| key_a.name.cmp(&key_b.name))
                .then_with(|| row_a.name().cmp(row_b.name()))
        }
        // ── Remaining non-sortable fields: name tiebreaker ──
        FieldId::Name
        | FieldId::Attributes
        | FieldId::AttributeValue
        | FieldId::ParityAttributes => key_a
            .name
            .cmp(&key_b.name)
            .then_with(|| row_a.name().cmp(row_b.name())),
    }
}

/// Map a boolean-attribute `FieldId` to its NTFS `FILE_ATTRIBUTE_*` bitmask.
///
/// Non-boolean fields return `0` — the caller skips attribute-based sorting.
///
/// Kept as a separate function (rather than inlined into `compare_by_column`)
/// because inlining 42 match arms into a nested closure harms readability.
pub(crate) const fn field_to_attr_bit(field: FieldId) -> u32 {
    match field {
        FieldId::Hidden => 0x0002,
        FieldId::System => 0x0004,
        FieldId::Archive => 0x0020,
        FieldId::ReadOnly => 0x0001,
        FieldId::Compressed => 0x0800,
        FieldId::Encrypted => 0x4000,
        FieldId::Sparse => 0x0200,
        FieldId::Reparse => 0x0400,
        FieldId::Offline => 0x1000,
        FieldId::NotIndexed => 0x2000,
        FieldId::Temporary => 0x0100,
        FieldId::Virtual => 0x0001_0000,
        FieldId::Pinned => 0x0008_0000,
        FieldId::Unpinned => 0x0010_0000,
        FieldId::Integrity => 0x8000,
        FieldId::NoScrub => 0x0002_0000,
        FieldId::DirectoryFlag => 0x0010,
        FieldId::RecallOnOpen => 0x0004_0000,
        FieldId::RecallOnDataAccess => 0x0040_0000,
        // Non-boolean fields — no attribute bit.
        FieldId::Drive
        | FieldId::Path
        | FieldId::Name
        | FieldId::PathOnly
        | FieldId::Size
        | FieldId::SizeOnDisk
        | FieldId::Created
        | FieldId::Modified
        | FieldId::Accessed
        | FieldId::Extension
        | FieldId::Type
        | FieldId::Attributes
        | FieldId::AttributeValue
        | FieldId::Descendants
        | FieldId::TreeSize
        | FieldId::TreeAllocated
        | FieldId::Bulkiness
        | FieldId::ParityAttributes
        | FieldId::NameLength
        | FieldId::PathLength => 0,
    }
}

/// Parse a `--sort` value like `"name:asc,modified:desc"` into sort specs.
///
/// Supports three direction syntaxes:
/// - Prefix: `-size` means descending, bare `size` means ascending
/// - Suffix: `size:desc` or `size:asc` (explicit)
///
/// Without any direction hint, the field-type default is used.
///
/// Any field recognised by `FieldId::parse` that is also sortable is accepted.
#[must_use]
pub fn parse_sort_spec(sort_str: &str) -> Vec<SortSpec> {
    let mut specs = Vec::new();
    for raw_part in sort_str.split(',') {
        let trimmed = raw_part.trim();

        // Check for `-` prefix (e.g. "-modified" → descending).
        let (has_dash_prefix, after_dash) = trimmed
            .strip_prefix('-')
            .map_or((false, trimmed), |rest| (true, rest));

        let (col_str, dir_str) = if let Some((col, dir)) = after_dash.split_once(':') {
            (col.trim(), Some(dir.trim()))
        } else {
            (after_dash, None)
        };
        let Some(field) = FieldId::parse(col_str) else {
            continue;
        };
        if !field.metadata().sortable {
            continue;
        }
        let descending = match dir_str {
            Some("desc") => true,
            Some("asc") => false,
            _ if has_dash_prefix => true,
            _ => matches!(
                field.default_sort_direction(),
                Some(super::field::SortDirection::Descending)
            ),
        };
        specs.push(SortSpec {
            column: field,
            descending,
        });
    }
    specs
}

/// Format the current sort state back into a CLI-compatible sort string.
#[must_use]
pub fn format_sort_spec(primary: FieldId, primary_desc: bool, extra: &[SortSpec]) -> String {
    let mut parts = Vec::with_capacity(1 + extra.len());
    let dir = |desc: bool| if desc { "desc" } else { "asc" };
    parts.push(format!(
        "{}:{}",
        primary.canonical_name(),
        dir(primary_desc)
    ));
    for spec in extra {
        parts.push(format!(
            "{}:{}",
            spec.column.canonical_name(),
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
    let tree_allocated: Vec<u64> = rows.iter().map(tree_allocated_for_row).collect();
    let bulkiness: Vec<u64> = rows.iter().map(bulkiness_for_row).collect();

    // path_only = directory portion of path (up to and including last backslash).
    let path_only: Vec<&str> = rows.iter().map(DisplayRow::path_dir).collect();

    DataFrame::new(rows.len(), vec![
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
        Column::new("tree_allocated".into(), &tree_allocated),
        Column::new("bulkiness".into(), &bulkiness),
    ])
}

/// Convert a legacy Polars `DataFrame` into `Vec<DisplayRow>`.
///
/// Handles both "new" column layouts (from `display_rows_to_dataframe`) and
/// legacy MFT layouts (from `results_to_dataframe`). Timestamps may be
/// plain `Int64` or `Datetime(Microseconds)` — both are handled.
///
/// Columns that don't exist get sensible defaults (0 for numbers, empty
/// strings, `'?'` for drive).
/// Converts a `DataFrame` into a `Vec<DisplayRow>` for rendering.
#[must_use]
pub fn dataframe_to_display_rows(data_frame: &uffs_polars::DataFrame) -> Vec<DisplayRow> {
    let height = data_frame.height();
    if height == 0 {
        return Vec::new();
    }

    let mut rows = Vec::with_capacity(height);
    for row_idx in 0..height {
        let path = col_str(data_frame, "path", row_idx).unwrap_or_default();
        let drive = col_str(data_frame, "drive", row_idx)
            .and_then(|val| val.chars().next())
            .unwrap_or('?');
        let size = col_u64(data_frame, "size", row_idx);
        let allocated = col_u64(data_frame, "allocated_size", row_idx);
        let flags = u32::try_from(col_u64(data_frame, "flags", row_idx)).unwrap_or(u32::MAX);
        let is_directory = col_bool(data_frame, "is_directory", row_idx);
        let created = col_timestamp(data_frame, "created", row_idx);
        let modified = col_timestamp(data_frame, "modified", row_idx);
        let accessed = col_timestamp(data_frame, "accessed", row_idx);
        let descendants =
            u32::try_from(col_u64(data_frame, "descendants", row_idx)).unwrap_or(u32::MAX);
        let treesize = col_u64(data_frame, "treesize", row_idx);
        let tree_allocated = col_u64(data_frame, "tree_allocated", row_idx);

        rows.push(DisplayRow::new(
            uffs_mft::len_to_u32(row_idx),
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
    rows
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
                        .and_then(|arr| arr.get(row_idx).map(uffs_mft::nonneg_to_u64))
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
