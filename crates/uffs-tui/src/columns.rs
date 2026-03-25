//! TUI column definitions and parsing.
//!
//! Defines [`TuiColumn`] — the set of columns the TUI table can display —
//! along with header labels, width constraints, sort-column mapping, and
//! the `--columns` CLI parser.

use crate::backend::SortColumn;

/// A column that the TUI can display.
///
/// Full CLI-parity with `OutputColumn` in `uffs-core`.  Every column
/// that `DisplayRow` can provide data for is included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiColumn {
    // ── core ────────────────────────────────────────────────────────
    /// Drive letter (TUI-only, not in CLI `OutputColumn`).
    Drive,
    /// Filename (with devicon).
    Name,
    /// Full resolved path.
    Path,
    /// Directory path without filename.
    PathOnly,
    /// File size in bytes.
    Size,
    /// Allocated size on disk ("Size on Disk").
    SizeOnDisk,
    // ── timestamps ──────────────────────────────────────────────────
    /// Creation timestamp.
    Created,
    /// Last modification timestamp.
    Modified,
    /// Last access timestamp.
    Accessed,
    // ── type / extension ────────────────────────────────────────────
    /// File extension (derived from name).
    Extension,
    /// Devicon file type icon.
    Type,
    // ── attributes (formatted) ──────────────────────────────────────
    /// Formatted attribute string (e.g. "HSAR").
    Attributes,
    /// Raw `FILE_ATTRIBUTE_*` flags as decimal number.
    AttributeValue,
    // ── individual attribute booleans ────────────────────────────────
    /// Hidden attribute.
    Hidden,
    /// System attribute.
    System,
    /// Archive attribute.
    Archive,
    /// Read-only attribute.
    ReadOnly,
    /// Compressed attribute.
    Compressed,
    /// Encrypted attribute.
    Encrypted,
    /// Sparse file attribute.
    Sparse,
    /// Reparse point attribute.
    Reparse,
    /// Offline attribute.
    Offline,
    /// Not content-indexed attribute.
    NotIndexed,
    /// Temporary file attribute.
    Temporary,
    /// Virtual file attribute.
    Virtual,
    /// Pinned attribute.
    Pinned,
    /// Unpinned attribute.
    Unpinned,
    /// Integrity stream attribute.
    Integrity,
    /// No-scrub-data attribute.
    NoScrub,
    /// Directory flag (boolean).
    DirectoryFlag,
    // ── tree metrics ────────────────────────────────────────────────
    /// Descendant count (directories only).
    Descendants,
    /// Sum of file sizes in subtree (directories only).
    TreeSize,
}

/// Default column set shown when no `--columns` override is active.
pub const DEFAULT_COLUMNS: &[TuiColumn] = &[
    TuiColumn::Drive,
    TuiColumn::Name,
    TuiColumn::Size,
    TuiColumn::Modified,
    TuiColumn::Path,
];

impl TuiColumn {
    /// Parse a column name string (case-insensitive).
    ///
    /// Accepts every name the CLI `--columns` flag recognises, plus
    /// TUI-only `"drive"` / `"drv"`.
    #[must_use]
    #[expect(
        clippy::single_call_fn,
        reason = "standalone parser; keeps column-name mapping isolated from call site"
    )]
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            // core
            "drive" | "drv" => Some(Self::Drive),
            "name" => Some(Self::Name),
            "path" => Some(Self::Path),
            "pathonly" => Some(Self::PathOnly),
            "size" => Some(Self::Size),
            "sizeondisk" | "allocated" => Some(Self::SizeOnDisk),
            // timestamps
            "created" => Some(Self::Created),
            "modified" | "written" | "date" => Some(Self::Modified),
            "accessed" => Some(Self::Accessed),
            // type / extension
            "ext" | "extension" => Some(Self::Extension),
            "type" | "directory" => Some(Self::Type),
            // attributes (formatted)
            "attributes" | "attrs" => Some(Self::Attributes),
            "attributevalue" | "attrval" => Some(Self::AttributeValue),
            // individual booleans
            "hidden" | "h" => Some(Self::Hidden),
            "system" | "s" => Some(Self::System),
            "archive" | "a" => Some(Self::Archive),
            "readonly" | "r" | "read-only" => Some(Self::ReadOnly),
            "compressed" => Some(Self::Compressed),
            "encrypted" => Some(Self::Encrypted),
            "sparse" => Some(Self::Sparse),
            "reparse" => Some(Self::Reparse),
            "offline" | "o" => Some(Self::Offline),
            "notindexed" | "notcontent" => Some(Self::NotIndexed),
            "temporary" | "temp" => Some(Self::Temporary),
            "virtual" => Some(Self::Virtual),
            "pinned" => Some(Self::Pinned),
            "unpinned" => Some(Self::Unpinned),
            "integrity" => Some(Self::Integrity),
            "noscrub" => Some(Self::NoScrub),
            "directoryflag" => Some(Self::DirectoryFlag),
            // tree metrics
            "descendants" | "decendents" => Some(Self::Descendants),
            "treesize" | "tree_size" => Some(Self::TreeSize),
            _ => None,
        }
    }

    /// Short header label for the table.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Drive => "Drv",
            Self::Name => "Name",
            Self::Path => "Path",
            Self::PathOnly => "Dir",
            Self::Size => "Size",
            Self::SizeOnDisk => "OnDisk",
            Self::Created => "Created",
            Self::Modified => "Modified",
            Self::Accessed => "Accessed",
            Self::Extension => "Ext",
            Self::Type => "Type",
            Self::Attributes => "Attrs",
            Self::AttributeValue => "AttrVal",
            Self::Hidden => "H",
            Self::System => "S",
            Self::Archive => "A",
            Self::ReadOnly => "R",
            Self::Compressed => "C",
            Self::Encrypted => "E",
            Self::Sparse => "Sp",
            Self::Reparse => "Rp",
            Self::Offline => "O",
            Self::NotIndexed => "NI",
            Self::Temporary => "T",
            Self::Virtual => "V",
            Self::Pinned => "Pin",
            Self::Unpinned => "Unp",
            Self::Integrity => "Int",
            Self::NoScrub => "NS",
            Self::DirectoryFlag => "Dir?",
            Self::Descendants => "Desc",
            Self::TreeSize => "TreeSz",
        }
    }

    /// Map to the corresponding `SortColumn` (for sort-indicator display).
    #[must_use]
    pub const fn to_sort_column(self) -> SortColumn {
        match self {
            Self::Drive => SortColumn::Drive,
            // Name and all attribute columns fall back to Name sort
            Self::Name
            | Self::Attributes
            | Self::AttributeValue
            | Self::Hidden
            | Self::System
            | Self::Archive
            | Self::ReadOnly
            | Self::Compressed
            | Self::Encrypted
            | Self::Sparse
            | Self::Reparse
            | Self::Offline
            | Self::NotIndexed
            | Self::Temporary
            | Self::Virtual
            | Self::Pinned
            | Self::Unpinned
            | Self::Integrity
            | Self::NoScrub
            | Self::DirectoryFlag => SortColumn::Name,
            Self::Path | Self::PathOnly => SortColumn::Path,
            // Size and TreeSize share the same sort behaviour
            Self::Size | Self::TreeSize => SortColumn::Size,
            Self::SizeOnDisk => SortColumn::SizeOnDisk,
            Self::Created => SortColumn::Created,
            Self::Modified => SortColumn::Modified,
            Self::Accessed => SortColumn::Accessed,
            Self::Extension => SortColumn::Extension,
            Self::Type => SortColumn::Type,
            Self::Descendants => SortColumn::Descendants,
        }
    }

    /// Default width constraint for this column.
    #[must_use]
    pub const fn default_constraint(self) -> ratatui::layout::Constraint {
        use ratatui::layout::Constraint;
        match self {
            Self::Drive => Constraint::Length(3),
            Self::Name => Constraint::Min(20),
            Self::Path => Constraint::Length(62),
            Self::PathOnly => Constraint::Length(52),
            Self::Size | Self::SizeOnDisk | Self::TreeSize => Constraint::Length(12),
            Self::Created | Self::Modified | Self::Accessed => Constraint::Length(19),
            Self::Extension => Constraint::Length(10),
            Self::Type => Constraint::Length(6),
            Self::Attributes | Self::AttributeValue | Self::Descendants => Constraint::Length(8),
            // Boolean attribute columns — narrow
            Self::Hidden
            | Self::System
            | Self::Archive
            | Self::ReadOnly
            | Self::Compressed
            | Self::Encrypted
            | Self::Sparse
            | Self::Reparse
            | Self::Offline
            | Self::NotIndexed
            | Self::Temporary
            | Self::Virtual
            | Self::Pinned
            | Self::Unpinned
            | Self::Integrity
            | Self::NoScrub
            | Self::DirectoryFlag => Constraint::Length(4),
        }
    }
}

/// Parse a `--columns` value like `"name,size,modified,path"` into a
/// `Vec<TuiColumn>`.
///
/// Returns `None` for `"all"` or empty/unrecognised input (meaning: use
/// defaults).
#[must_use]
#[expect(
    clippy::single_call_fn,
    reason = "standalone parser; keeps column-list parsing isolated from call site"
)]
pub fn parse_columns(input: &str) -> Option<Vec<TuiColumn>> {
    let trimmed = input.trim();
    if trimmed.eq_ignore_ascii_case("all") || trimmed.is_empty() {
        return None;
    }
    let cols: Vec<TuiColumn> = trimmed
        .split(',')
        .filter_map(|segment| TuiColumn::parse(segment.trim()))
        .collect();
    if cols.is_empty() { None } else { Some(cols) }
}
