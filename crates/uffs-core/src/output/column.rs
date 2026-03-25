//! Output column definitions and mappings.

/// Available output columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputColumn {
    /// Full path including filename.
    Path,
    /// Filename only.
    Name,
    /// Directory path without filename.
    PathOnly,
    /// File size in bytes.
    Size,
    /// Allocated size on disk.
    SizeOnDisk,
    /// Creation timestamp.
    Created,
    /// Last modification timestamp.
    Modified,
    /// Last access timestamp.
    Accessed,
    /// File type/extension.
    Type,
    /// File attributes string.
    Attributes,
    /// Raw attribute flags as number.
    AttributeValue,
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
    /// Not content indexed attribute.
    NotIndexed,
    /// Temporary file attribute.
    Temporary,
    /// Virtual file attribute.
    Virtual,
    /// Pinned attribute.
    Pinned,
    /// Unpinned attribute.
    Unpinned,
    /// Descendant count (for directories).
    Descendants,
    /// Sum of logical file sizes under a directory.
    TreeSize,
    /// Sum of allocated sizes under a directory.
    TreeAllocated,
    /// Fragmentation metric: `tree_allocated` / `treesize` ratio.
    Bulkiness,
    /// Integrity stream attribute (`ReFS`).
    Integrity,
    /// No scrub data attribute.
    NoScrub,
    /// Directory flag (boolean, separate from Type).
    DirectoryFlag,
    /// Recall on open attribute (tiered/cloud storage).
    RecallOnOpen,
    /// Recall on data access attribute (tiered/cloud storage).
    RecallOnDataAccess,
    /// Raw attribute flags masked to 15 C++ baseline bits (for parity).
    ParityAttributes,
}

/// Column order matching C++ output exactly (25 columns).
///
/// Used by `--parity-compat` to produce output that matches the C++ baseline.
/// The Attributes column here outputs `parity_attributes()` (masked to 15 bits).
pub const PARITY_COLUMN_ORDER: &[OutputColumn] = &[
    OutputColumn::Path,
    OutputColumn::Name,
    OutputColumn::PathOnly,
    OutputColumn::Size,
    OutputColumn::SizeOnDisk,
    OutputColumn::Created,
    OutputColumn::Modified,
    OutputColumn::Accessed,
    OutputColumn::Descendants,
    OutputColumn::ReadOnly,
    OutputColumn::Archive,
    OutputColumn::System,
    OutputColumn::Hidden,
    OutputColumn::Offline,
    OutputColumn::NotIndexed,
    OutputColumn::NoScrub,
    OutputColumn::Integrity,
    OutputColumn::Pinned,
    OutputColumn::Unpinned,
    OutputColumn::DirectoryFlag,
    OutputColumn::Compressed,
    OutputColumn::Encrypted,
    OutputColumn::Sparse,
    OutputColumn::Reparse,
    OutputColumn::ParityAttributes,
];

/// Default column order: data columns + boolean attributes in NTFS flag value
/// order (lowest → highest), matching the NTFS evolution of attribute flags.
///
/// This is the order used when `--columns all` is specified.
pub const CPP_COLUMN_ORDER: &[OutputColumn] = &[
    // ── Data columns ────────────────────────────────────────────────
    OutputColumn::Path,
    OutputColumn::Name,
    OutputColumn::PathOnly,
    OutputColumn::Size,
    OutputColumn::SizeOnDisk,
    OutputColumn::Created,
    OutputColumn::Modified,
    OutputColumn::Accessed,
    OutputColumn::Descendants,
    // ── Boolean attributes in NTFS flag value order ─────────────────
    OutputColumn::ReadOnly,          // 0x0001
    OutputColumn::Hidden,            // 0x0002
    OutputColumn::System,            // 0x0004
    OutputColumn::DirectoryFlag,     // 0x0010
    OutputColumn::Archive,           // 0x0020
    OutputColumn::Sparse,            // 0x0200
    OutputColumn::Reparse,           // 0x0400
    OutputColumn::Compressed,        // 0x0800
    OutputColumn::Offline,           // 0x1000
    OutputColumn::NotIndexed,        // 0x2000
    OutputColumn::Encrypted,         // 0x4000
    OutputColumn::Integrity,         // 0x8000
    OutputColumn::NoScrub,           // 0x20000
    OutputColumn::RecallOnOpen,      // 0x40000
    OutputColumn::Pinned,            // 0x80000
    OutputColumn::Unpinned,          // 0x100000
    OutputColumn::RecallOnDataAccess, // 0x400000
    // ── Raw aggregate ───────────────────────────────────────────────
    OutputColumn::Attributes,
];

impl OutputColumn {
    /// Parse column name from string.
    ///
    /// Supports both full names and short aliases for CPP compatibility:
    /// - `r` → readonly
    /// - `a` → archive
    /// - `s` → system
    /// - `h` → hidden
    /// - `o` → offline
    /// - `directory` → `is_directory` (mapped to Type)
    /// - `notcontent` → notindexed
    /// - `written` → modified
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "path" => Some(Self::Path),
            "name" => Some(Self::Name),
            "pathonly" => Some(Self::PathOnly),
            "size" => Some(Self::Size),
            "sizeondisk" => Some(Self::SizeOnDisk),
            "created" => Some(Self::Created),
            // CPP uses "written", Rust uses "modified" - support both
            "modified" | "written" => Some(Self::Modified),
            "accessed" => Some(Self::Accessed),
            // "directory" maps to Type (which shows file/directory)
            "type" | "directory" => Some(Self::Type),
            "attributes" => Some(Self::Attributes),
            "attributevalue" => Some(Self::AttributeValue),
            // Short aliases for CPP compatibility
            "hidden" | "h" => Some(Self::Hidden),
            "system" | "s" => Some(Self::System),
            "archive" | "a" => Some(Self::Archive),
            "readonly" | "r" => Some(Self::ReadOnly),
            "compressed" => Some(Self::Compressed),
            "encrypted" => Some(Self::Encrypted),
            "sparse" => Some(Self::Sparse),
            "reparse" => Some(Self::Reparse),
            "offline" | "o" => Some(Self::Offline),
            // CPP uses "notcontent", Rust uses "notindexed" - support both
            "notindexed" | "notcontent" => Some(Self::NotIndexed),
            "temporary" => Some(Self::Temporary),
            "virtual" => Some(Self::Virtual),
            "pinned" => Some(Self::Pinned),
            "unpinned" => Some(Self::Unpinned),
            // CPP typo "decendents" supported for compatibility
            "descendants" | "decendents" => Some(Self::Descendants),
            "treesize" | "tree_size" => Some(Self::TreeSize),
            "treeallocated" | "tree_allocated" => Some(Self::TreeAllocated),
            "bulkiness" => Some(Self::Bulkiness),
            // New columns for legacy-output parity
            "integrity" => Some(Self::Integrity),
            "noscrub" => Some(Self::NoScrub),
            "directoryflag" => Some(Self::DirectoryFlag),
            "recallonopen" | "recall_on_open" => Some(Self::RecallOnOpen),
            "recallondataaccess" | "recall_on_data_access" => Some(Self::RecallOnDataAccess),
            "parityattributes" | "parity_attributes" => Some(Self::ParityAttributes),
            _ => None,
        }
    }

    /// Get the `DataFrame` column name.
    #[must_use]
    pub const fn df_column(&self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Name => "name",
            Self::PathOnly => "path_only",
            Self::Size => "size",
            Self::SizeOnDisk => "allocated_size",
            Self::Created => "created",
            Self::Modified => "modified",
            Self::Accessed => "accessed",
            Self::Type => "type",
            // Both Attributes and AttributeValue map to the raw flags column
            // C++ outputs the numeric value in the "Attributes" column
            Self::Attributes | Self::AttributeValue => "flags",
            // MFT reader uses is_ prefix for boolean flags
            Self::Hidden => "is_hidden",
            Self::System => "is_system",
            Self::Archive => "is_archive",
            Self::ReadOnly => "is_readonly",
            Self::Compressed => "is_compressed",
            Self::Encrypted => "is_encrypted",
            Self::Sparse => "is_sparse",
            Self::Reparse => "is_reparse",
            Self::Offline => "is_offline",
            Self::NotIndexed => "is_not_indexed",
            Self::Temporary => "is_temporary",
            Self::Virtual => "is_virtual",
            Self::Pinned => "is_pinned",
            Self::Unpinned => "is_unpinned",
            // Tree columns (computed on-demand)
            Self::Descendants => "descendants",
            Self::TreeSize => "treesize",
            Self::TreeAllocated => "tree_allocated",
            Self::Bulkiness => "bulkiness",
            // New columns for legacy-output parity
            Self::Integrity => "is_integrity_stream",
            Self::NoScrub => "is_no_scrub_data",
            Self::DirectoryFlag => "is_directory",
            Self::RecallOnOpen => "is_recall_on_open",
            Self::RecallOnDataAccess => "is_recall_on_data_access",
            Self::ParityAttributes => "parity_flags",
        }
    }

    /// Get the display name for headers (matches expected output exactly).
    #[must_use]
    pub const fn display_name(&self) -> &'static str {
        match self {
            Self::Path => "Path",
            Self::Name => "Name",
            Self::PathOnly => "Path Only",
            Self::Size => "Size",
            Self::SizeOnDisk => "Size on Disk",
            Self::Created => "Created",
            Self::Modified => "Last Written",
            Self::Accessed => "Last Accessed",
            Self::Type => "Type",
            Self::Attributes => "Attributes",
            Self::AttributeValue => "AttributeValue",
            Self::Hidden => "Hidden",
            Self::System => "System",
            Self::Archive => "Archive",
            Self::ReadOnly => "Read-only",
            Self::Compressed => "Compressed",
            Self::Encrypted => "Encrypted",
            Self::Sparse => "Sparse",
            Self::Reparse => "Reparse",
            Self::Offline => "Offline",
            Self::NotIndexed => "Not content indexed file",
            Self::Temporary => "Temporary",
            Self::Virtual => "Virtual",
            Self::Pinned => "Pinned",
            Self::Unpinned => "Unpinned",
            Self::Descendants => "Descendants",
            Self::TreeSize => "TreeSize",
            Self::TreeAllocated => "TreeAllocated",
            Self::Bulkiness => "Bulkiness",
            Self::Integrity => "Integrity",
            Self::NoScrub => "No scrub file",
            Self::DirectoryFlag => "Directory Flag",
            Self::RecallOnOpen => "Recall on open",
            Self::RecallOnDataAccess => "Recall on data access",
            Self::ParityAttributes => "Attributes",
        }
    }

    /// Check if this column is a tree-derived column.
    #[must_use]
    pub const fn is_tree_column(&self) -> bool {
        matches!(
            self,
            Self::Descendants | Self::TreeSize | Self::TreeAllocated | Self::Bulkiness
        )
    }

    /// Convert to a tree column if applicable.
    #[must_use]
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "intentional: only tree columns convert"
    )]
    pub const fn to_tree_column(&self) -> Option<crate::tree::TreeColumn> {
        match self {
            Self::Descendants => Some(crate::tree::TreeColumn::Descendants),
            Self::TreeSize => Some(crate::tree::TreeColumn::TreeSize),
            Self::TreeAllocated => Some(crate::tree::TreeColumn::TreeAllocated),
            Self::Bulkiness => Some(crate::tree::TreeColumn::Bulkiness),
            _ => None,
        }
    }

    /// Get the default value for this column when it's missing from the
    /// `DataFrame`.
    ///
    /// Numeric and boolean columns return "0" to match C++ output behavior.
    /// String and timestamp columns return empty string.
    #[must_use]
    pub const fn default_value(&self) -> &'static str {
        match self {
            // Numeric columns default to "0"
            // Boolean columns default to "0" (false)
            Self::Size
            | Self::SizeOnDisk
            | Self::Descendants
            | Self::TreeSize
            | Self::TreeAllocated
            | Self::Bulkiness
            | Self::Attributes
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
            | Self::DirectoryFlag
            | Self::Temporary
            | Self::Virtual
            | Self::Pinned
            | Self::Unpinned
            | Self::Integrity
            | Self::NoScrub
            | Self::RecallOnOpen
            | Self::RecallOnDataAccess
            | Self::ParityAttributes => "0",
            // String and timestamp columns default to empty
            Self::Path
            | Self::Name
            | Self::PathOnly
            | Self::Type
            | Self::AttributeValue
            | Self::Created
            | Self::Modified
            | Self::Accessed => "",
        }
    }
}
