//! Canonical field identifiers and metadata for unified search semantics.

/// Canonical field identifier shared across filter, sort, and projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldId {
    /// Drive letter.
    Drive,
    /// Full resolved path.
    Path,
    /// Filename only.
    Name,
    /// Parent directory path without filename.
    PathOnly,
    /// Logical file size.
    Size,
    /// Allocated size on disk.
    SizeOnDisk,
    /// Creation timestamp.
    Created,
    /// Last-written timestamp.
    Modified,
    /// Last-access timestamp.
    Accessed,
    /// Filename extension.
    Extension,
    /// File category / type.
    Type,
    /// Formatted attribute set.
    Attributes,
    /// Raw attribute value.
    AttributeValue,
    /// Hidden flag.
    Hidden,
    /// System flag.
    System,
    /// Archive flag.
    Archive,
    /// Read-only flag.
    ReadOnly,
    /// Compressed flag.
    Compressed,
    /// Encrypted flag.
    Encrypted,
    /// Sparse flag.
    Sparse,
    /// Reparse-point flag.
    Reparse,
    /// Offline flag.
    Offline,
    /// Not-content-indexed flag.
    NotIndexed,
    /// Temporary flag.
    Temporary,
    /// Virtual flag.
    Virtual,
    /// Pinned flag.
    Pinned,
    /// Unpinned flag.
    Unpinned,
    /// Descendant count.
    Descendants,
    /// Aggregate logical subtree size.
    TreeSize,
    /// Aggregate allocated subtree size.
    TreeAllocated,
    /// Tree allocation / logical size ratio.
    Bulkiness,
    /// Integrity-stream flag.
    Integrity,
    /// No-scrub-data flag.
    NoScrub,
    /// Directory boolean flag.
    DirectoryFlag,
    /// Recall-on-open flag.
    RecallOnOpen,
    /// Recall-on-data-access flag.
    RecallOnDataAccess,
    /// Legacy parity-masked attribute value.
    ParityAttributes,
    /// Filename length in characters.
    NameLength,
    /// Full-path length in characters.
    PathLength,
}

/// Cardinality hint for aggregation planning.
///
/// Tells the aggregation engine what kind of accumulator to expect
/// when this field is used as a group-by key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cardinality {
    /// Tiny, fixed set of values (≤ 26). Use array-indexed accumulator.
    /// Examples: drive letter, bool attrs, directory flag.
    Fixed,
    /// Small value space (≤ ~100). Use small `HashMap`.
    /// Examples: semantic type / `FileCategory` (~24 variants).
    Low,
    /// Medium value space (≤ ~10 000). Use `HashMap`.
    /// Examples: file extensions (~2 000 on a typical system).
    Medium,
    /// Large value space (≤ ~1 000 000). Guard with top-N + `other_count`.
    /// Examples: folder paths, directory names.
    High,
    /// Potentially millions of distinct values. Only aggregate on explicit request.
    /// Examples: full path, file name (for duplicate detection).
    Unbounded,
}

/// Aggregation capability metadata for a single field.
///
/// Populated on every [`FieldId`] and read by the aggregation engine to
/// decide what operations are valid, which accumulator strategy to use,
/// and how to finalize/sort buckets.
///
/// This struct is `Copy` and const-constructable so it can be returned
/// from `FieldId::metadata()` (a `const fn`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AggregateMeta {
    /// Can numeric aggregate functions (sum, min, max, avg) be applied?
    /// True for numeric and timestamp fields.
    pub aggregatable: bool,
    /// Can this field be used as a group-by / terms key?
    /// True for enum, string, and bool fields.
    pub groupable: bool,
    /// Can this field be bucketed into ranges or histograms?
    /// True for numeric and timestamp fields.
    pub bucket_support: bool,
    /// Expected number-of-distinct-values hint.
    pub cardinality: Cardinality,
    /// Default top-N limit when used as a terms aggregation key.
    /// 0 means "not suitable for terms by default — use histogram/range instead".
    pub default_top: u16,
}

/// Canonical field kinds used by predicate compilation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    /// String-like values.
    String,
    /// Numeric values.
    Numeric,
    /// Timestamp values.
    Timestamp,
    /// Boolean values.
    Bool,
    /// Enumerated / categorized values.
    Enum,
    /// Bitmask-style values.
    Bitmask,
}

/// Where a field's value becomes available during execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldAccess {
    /// Available during the existing hot path with no additional
    /// materialization.
    Hot,
    /// Computed from hot data without extra disk I/O.
    Derived,
    /// Requires cold-path materialization from extra record data.
    Cold,
}

/// Default sort direction for a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    /// Ascending order.
    Ascending,
    /// Descending order.
    Descending,
}

/// Canonical metadata describing one field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldMeta {
    /// The canonical identifier.
    pub id: FieldId,
    /// Canonical wire / config name.
    pub canonical_name: &'static str,
    /// Accepted aliases during parsing.
    pub aliases: &'static [&'static str],
    /// Logical field kind.
    pub field_type: FieldType,
    /// Availability tier for execution planning.
    pub access: FieldAccess,
    /// Whether the field should be sortable in the canonical model.
    pub sortable: bool,
    /// Preferred default sort direction when used in a sort spec.
    pub default_sort_direction: Option<SortDirection>,
    /// Whether the field should be filterable in the canonical model.
    pub filterable: bool,
    /// Whether the field can be projected in results.
    pub projectable: bool,
    /// Short TUI header label (e.g. "Drv", "Name", "Sz").
    pub tui_label: &'static str,
    /// Human-readable display name for CLI output headers.
    pub display_name: &'static str,
    /// Polars `DataFrame` column name (empty if not backed by a DF column).
    pub df_column: &'static str,
    /// Default fallback value when the column is missing from a `DataFrame`.
    pub default_value: &'static str,
    /// Aggregation capability metadata.
    pub aggregate: AggregateMeta,
}

impl FieldId {
    /// All currently known canonical fields.
    pub const ALL: &'static [Self] = &[
        Self::Drive,
        Self::Path,
        Self::Name,
        Self::PathOnly,
        Self::Size,
        Self::SizeOnDisk,
        Self::Created,
        Self::Modified,
        Self::Accessed,
        Self::Extension,
        Self::Type,
        Self::Attributes,
        Self::AttributeValue,
        Self::Hidden,
        Self::System,
        Self::Archive,
        Self::ReadOnly,
        Self::Compressed,
        Self::Encrypted,
        Self::Sparse,
        Self::Reparse,
        Self::Offline,
        Self::NotIndexed,
        Self::Temporary,
        Self::Virtual,
        Self::Pinned,
        Self::Unpinned,
        Self::Descendants,
        Self::TreeSize,
        Self::TreeAllocated,
        Self::Bulkiness,
        Self::Integrity,
        Self::NoScrub,
        Self::DirectoryFlag,
        Self::RecallOnOpen,
        Self::RecallOnDataAccess,
        Self::ParityAttributes,
        Self::NameLength,
        Self::PathLength,
    ];

    /// Parse a field name or alias into the canonical identifier.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        for &field in Self::ALL {
            let meta = field.metadata();
            if meta.canonical_name.eq_ignore_ascii_case(name)
                || meta
                    .aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(name))
            {
                return Some(field);
            }
        }
        None
    }

    /// Return canonical metadata for this field.
    ///
    /// This is a data-definition function — one arm per `FieldId` variant (35),
    /// each returning a `FieldMeta` struct literal. No logic to extract.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub const fn metadata(self) -> FieldMeta {
        match self {
            Self::Drive => FieldMeta {
                id: self,
                canonical_name: "drive",
                aliases: &["drv"],
                field_type: FieldType::Enum,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Ascending),
                filterable: true,
                projectable: true,
                tui_label: "Drv",
                display_name: "Drive",
                df_column: "",
                default_value: "",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 26,
                },
            },
            Self::Path => FieldMeta {
                id: self,
                canonical_name: "path",
                aliases: &[],
                field_type: FieldType::String,
                access: FieldAccess::Derived,
                sortable: true,
                default_sort_direction: Some(SortDirection::Ascending),
                filterable: true,
                projectable: true,
                tui_label: "Path",
                display_name: "Path",
                df_column: "path",
                default_value: "",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: false,
                    bucket_support: false,
                    cardinality: Cardinality::Unbounded,
                    default_top: 0,
                },
            },
            Self::Name => FieldMeta {
                id: self,
                canonical_name: "name",
                aliases: &[],
                field_type: FieldType::String,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Ascending),
                filterable: true,
                projectable: true,
                tui_label: "Name",
                display_name: "Name",
                df_column: "name",
                default_value: "",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Unbounded,
                    default_top: 100,
                },
            },
            Self::PathOnly => FieldMeta {
                id: self,
                canonical_name: "path_only",
                aliases: &["pathonly", "path only"],
                field_type: FieldType::String,
                access: FieldAccess::Derived,
                sortable: true,
                default_sort_direction: Some(SortDirection::Ascending),
                filterable: true,
                projectable: true,
                tui_label: "Dir",
                display_name: "Path Only",
                df_column: "path_only",
                default_value: "",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Unbounded,
                    default_top: 30,
                },
            },
            Self::Size => FieldMeta {
                id: self,
                canonical_name: "size",
                aliases: &[],
                field_type: FieldType::Numeric,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "Size",
                display_name: "Size",
                df_column: "size",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: true,
                    groupable: false,
                    bucket_support: true,
                    cardinality: Cardinality::Unbounded,
                    default_top: 0,
                },
            },
            Self::SizeOnDisk => FieldMeta {
                id: self,
                canonical_name: "size_on_disk",
                aliases: &["sizeondisk", "size on disk", "allocated"],
                field_type: FieldType::Numeric,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "OnDisk",
                display_name: "Size on Disk",
                df_column: "allocated_size",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: true,
                    groupable: false,
                    bucket_support: true,
                    cardinality: Cardinality::Unbounded,
                    default_top: 0,
                },
            },
            Self::Created => FieldMeta {
                id: self,
                canonical_name: "created",
                aliases: &[],
                field_type: FieldType::Timestamp,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "Created",
                display_name: "Created",
                df_column: "created",
                default_value: "",
                aggregate: AggregateMeta {
                    aggregatable: true,
                    groupable: false,
                    bucket_support: true,
                    cardinality: Cardinality::Unbounded,
                    default_top: 0,
                },
            },
            Self::Modified => FieldMeta {
                id: self,
                canonical_name: "modified",
                aliases: &["written", "date"],
                field_type: FieldType::Timestamp,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "Modified",
                display_name: "Last Written",
                df_column: "modified",
                default_value: "",
                aggregate: AggregateMeta {
                    aggregatable: true,
                    groupable: false,
                    bucket_support: true,
                    cardinality: Cardinality::Unbounded,
                    default_top: 0,
                },
            },
            Self::Accessed => FieldMeta {
                id: self,
                canonical_name: "accessed",
                aliases: &[],
                field_type: FieldType::Timestamp,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "Accessed",
                display_name: "Last Accessed",
                df_column: "accessed",
                default_value: "",
                aggregate: AggregateMeta {
                    aggregatable: true,
                    groupable: false,
                    bucket_support: true,
                    cardinality: Cardinality::Unbounded,
                    default_top: 0,
                },
            },
            Self::Extension => FieldMeta {
                id: self,
                canonical_name: "extension",
                aliases: &["ext"],
                field_type: FieldType::String,
                access: FieldAccess::Derived,
                sortable: true,
                default_sort_direction: Some(SortDirection::Ascending),
                filterable: true,
                projectable: true,
                tui_label: "Ext",
                display_name: "Extension",
                df_column: "",
                default_value: "",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Medium,
                    default_top: 50,
                },
            },
            Self::Type => FieldMeta {
                id: self,
                canonical_name: "type",
                aliases: &["directory"],
                field_type: FieldType::Enum,
                access: FieldAccess::Derived,
                sortable: true,
                default_sort_direction: Some(SortDirection::Ascending),
                filterable: true,
                projectable: true,
                tui_label: "Type",
                display_name: "Type",
                df_column: "type",
                default_value: "",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Low,
                    default_top: 30,
                },
            },
            Self::Attributes => FieldMeta {
                id: self,
                canonical_name: "attributes",
                aliases: &["attrs"],
                field_type: FieldType::Bitmask,
                access: FieldAccess::Derived,
                sortable: false,
                default_sort_direction: None,
                filterable: false,
                projectable: true,
                tui_label: "Attrs",
                display_name: "Attributes",
                df_column: "flags",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: false,
                    bucket_support: false,
                    cardinality: Cardinality::Unbounded,
                    default_top: 0,
                },
            },
            Self::AttributeValue => FieldMeta {
                id: self,
                canonical_name: "attribute_value",
                aliases: &["attributevalue", "attrval"],
                field_type: FieldType::Bitmask,
                access: FieldAccess::Hot,
                sortable: false,
                default_sort_direction: None,
                filterable: true,
                projectable: true,
                tui_label: "AttrVal",
                display_name: "AttributeValue",
                df_column: "flags",
                default_value: "",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: false,
                    bucket_support: false,
                    cardinality: Cardinality::Unbounded,
                    default_top: 0,
                },
            },
            Self::Hidden => FieldMeta {
                id: self,
                canonical_name: "hidden",
                aliases: &["h"],
                field_type: FieldType::Bool,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "H",
                display_name: "Hidden",
                df_column: "is_hidden",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 2,
                },
            },
            Self::System => FieldMeta {
                id: self,
                canonical_name: "system",
                aliases: &["s"],
                field_type: FieldType::Bool,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "S",
                display_name: "System",
                df_column: "is_system",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 2,
                },
            },
            Self::Archive => FieldMeta {
                id: self,
                canonical_name: "archive",
                aliases: &["a"],
                field_type: FieldType::Bool,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "A",
                display_name: "Archive",
                df_column: "is_archive",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 2,
                },
            },
            Self::ReadOnly => FieldMeta {
                id: self,
                canonical_name: "read_only",
                aliases: &["readonly", "read-only", "read only", "r"],
                field_type: FieldType::Bool,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "R",
                display_name: "Read-only",
                df_column: "is_readonly",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 2,
                },
            },
            Self::Compressed => FieldMeta {
                id: self,
                canonical_name: "compressed",
                aliases: &[],
                field_type: FieldType::Bool,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "C",
                display_name: "Compressed",
                df_column: "is_compressed",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 2,
                },
            },
            Self::Encrypted => FieldMeta {
                id: self,
                canonical_name: "encrypted",
                aliases: &[],
                field_type: FieldType::Bool,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "E",
                display_name: "Encrypted",
                df_column: "is_encrypted",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 2,
                },
            },
            Self::Sparse => FieldMeta {
                id: self,
                canonical_name: "sparse",
                aliases: &[],
                field_type: FieldType::Bool,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "Sp",
                display_name: "Sparse",
                df_column: "is_sparse",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 2,
                },
            },
            Self::Reparse => FieldMeta {
                id: self,
                canonical_name: "reparse",
                aliases: &[],
                field_type: FieldType::Bool,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "Rp",
                display_name: "Reparse",
                df_column: "is_reparse",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 2,
                },
            },
            Self::Offline => FieldMeta {
                id: self,
                canonical_name: "offline",
                aliases: &["o"],
                field_type: FieldType::Bool,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "O",
                display_name: "Offline",
                df_column: "is_offline",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 2,
                },
            },
            Self::NotIndexed => FieldMeta {
                id: self,
                canonical_name: "not_indexed",
                aliases: &[
                    "notindexed",
                    "not indexed",
                    "notcontent",
                    "not content indexed",
                    "not content indexed file",
                ],
                field_type: FieldType::Bool,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "NI",
                display_name: "Not content indexed file",
                df_column: "is_not_indexed",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 2,
                },
            },
            Self::Temporary => FieldMeta {
                id: self,
                canonical_name: "temporary",
                aliases: &["temp"],
                field_type: FieldType::Bool,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "T",
                display_name: "Temporary",
                df_column: "is_temporary",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 2,
                },
            },
            Self::Virtual => FieldMeta {
                id: self,
                canonical_name: "virtual",
                aliases: &[],
                field_type: FieldType::Bool,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "V",
                display_name: "Virtual",
                df_column: "is_virtual",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 2,
                },
            },
            Self::Pinned => FieldMeta {
                id: self,
                canonical_name: "pinned",
                aliases: &[],
                field_type: FieldType::Bool,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "Pin",
                display_name: "Pinned",
                df_column: "is_pinned",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 2,
                },
            },
            Self::Unpinned => FieldMeta {
                id: self,
                canonical_name: "unpinned",
                aliases: &[],
                field_type: FieldType::Bool,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "Unp",
                display_name: "Unpinned",
                df_column: "is_unpinned",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 2,
                },
            },
            Self::Descendants => FieldMeta {
                id: self,
                canonical_name: "descendants",
                aliases: &["decendents"],
                field_type: FieldType::Numeric,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "Desc",
                display_name: "Descendants",
                df_column: "descendants",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: true,
                    groupable: false,
                    bucket_support: true,
                    cardinality: Cardinality::Unbounded,
                    default_top: 0,
                },
            },
            Self::TreeSize => FieldMeta {
                id: self,
                canonical_name: "tree_size",
                aliases: &["treesize"],
                field_type: FieldType::Numeric,
                access: FieldAccess::Derived,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "TreeSz",
                display_name: "Tree Size",
                df_column: "treesize",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: true,
                    groupable: false,
                    bucket_support: true,
                    cardinality: Cardinality::Unbounded,
                    default_top: 0,
                },
            },
            Self::TreeAllocated => FieldMeta {
                id: self,
                canonical_name: "tree_allocated",
                aliases: &["treeallocated"],
                field_type: FieldType::Numeric,
                access: FieldAccess::Derived,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "TreeAl",
                display_name: "Tree Allocated",
                df_column: "tree_allocated",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: true,
                    groupable: false,
                    bucket_support: true,
                    cardinality: Cardinality::Unbounded,
                    default_top: 0,
                },
            },
            Self::Bulkiness => FieldMeta {
                id: self,
                canonical_name: "bulkiness",
                aliases: &[],
                field_type: FieldType::Numeric,
                access: FieldAccess::Derived,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "Bulk",
                display_name: "Bulkiness",
                df_column: "bulkiness",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: true,
                    groupable: false,
                    bucket_support: true,
                    cardinality: Cardinality::Unbounded,
                    default_top: 0,
                },
            },
            Self::Integrity => FieldMeta {
                id: self,
                canonical_name: "integrity",
                aliases: &[],
                field_type: FieldType::Bool,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "Int",
                display_name: "Integrity",
                df_column: "is_integrity_stream",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 2,
                },
            },
            Self::NoScrub => FieldMeta {
                id: self,
                canonical_name: "no_scrub",
                aliases: &["noscrub", "no scrub", "no scrub file"],
                field_type: FieldType::Bool,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "NS",
                display_name: "No scrub file",
                df_column: "is_no_scrub_data",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 2,
                },
            },
            Self::DirectoryFlag => FieldMeta {
                id: self,
                canonical_name: "directory_flag",
                aliases: &["directoryflag", "directory flag", "directory", "dir"],
                field_type: FieldType::Bool,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "Dir?",
                display_name: "Directory Flag",
                df_column: "is_directory",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 2,
                },
            },
            Self::RecallOnOpen => FieldMeta {
                id: self,
                canonical_name: "recall_on_open",
                aliases: &["recallonopen", "recall on open"],
                field_type: FieldType::Bool,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "RcO",
                display_name: "Recall on open",
                df_column: "is_recall_on_open",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 2,
                },
            },
            Self::RecallOnDataAccess => FieldMeta {
                id: self,
                canonical_name: "recall_on_data_access",
                aliases: &["recallondataaccess", "recall on data access"],
                field_type: FieldType::Bool,
                access: FieldAccess::Hot,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "RcD",
                display_name: "Recall on data access",
                df_column: "is_recall_on_data_access",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: true,
                    bucket_support: false,
                    cardinality: Cardinality::Fixed,
                    default_top: 2,
                },
            },
            Self::ParityAttributes => FieldMeta {
                id: self,
                canonical_name: "parity_attributes",
                aliases: &["parityattributes"],
                field_type: FieldType::Bitmask,
                access: FieldAccess::Derived,
                sortable: false,
                default_sort_direction: None,
                filterable: false,
                projectable: true,
                tui_label: "PAttr",
                display_name: "Attributes",
                df_column: "parity_flags",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: false,
                    groupable: false,
                    bucket_support: false,
                    cardinality: Cardinality::Unbounded,
                    default_top: 0,
                },
            },
            Self::NameLength => FieldMeta {
                id: self,
                canonical_name: "name_length",
                aliases: &["namelength", "name_len", "namelen"],
                field_type: FieldType::Numeric,
                access: FieldAccess::Derived,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "NLen",
                display_name: "Name Length",
                df_column: "",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: true,
                    groupable: false,
                    bucket_support: true,
                    cardinality: Cardinality::Unbounded,
                    default_top: 0,
                },
            },
            Self::PathLength => FieldMeta {
                id: self,
                canonical_name: "path_length",
                aliases: &["pathlength", "path_len", "pathlen"],
                field_type: FieldType::Numeric,
                access: FieldAccess::Derived,
                sortable: true,
                default_sort_direction: Some(SortDirection::Descending),
                filterable: true,
                projectable: true,
                tui_label: "PLen",
                display_name: "Path Length",
                df_column: "",
                default_value: "0",
                aggregate: AggregateMeta {
                    aggregatable: true,
                    groupable: false,
                    bucket_support: true,
                    cardinality: Cardinality::Unbounded,
                    default_top: 0,
                },
            },
        }
    }

    /// Canonical wire/config name for this field.
    #[must_use]
    pub const fn canonical_name(self) -> &'static str {
        self.metadata().canonical_name
    }

    /// Preferred default sort direction for this field, when sortable.
    #[must_use]
    pub const fn default_sort_direction(self) -> Option<SortDirection> {
        self.metadata().default_sort_direction
    }

    /// Short TUI header label.
    #[must_use]
    pub const fn tui_label(self) -> &'static str {
        self.metadata().tui_label
    }

    /// Human-readable display name for CLI output headers.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        self.metadata().display_name
    }

    /// Polars `DataFrame` column name.
    #[must_use]
    pub const fn df_column(self) -> &'static str {
        self.metadata().df_column
    }

    /// Default fallback value when the column is missing from a DF.
    #[must_use]
    pub const fn default_value(self) -> &'static str {
        self.metadata().default_value
    }

    /// Whether this is a tree-derived metric field.
    #[must_use]
    pub const fn is_tree_field(self) -> bool {
        matches!(
            self,
            Self::Descendants | Self::TreeSize | Self::TreeAllocated | Self::Bulkiness
        )
    }

    /// Convert to a tree column if applicable.
    #[must_use]
    pub const fn to_tree_column(self) -> Option<crate::tree::TreeColumn> {
        match self {
            Self::Descendants => Some(crate::tree::TreeColumn::Descendants),
            Self::TreeSize => Some(crate::tree::TreeColumn::TreeSize),
            Self::TreeAllocated => Some(crate::tree::TreeColumn::TreeAllocated),
            Self::Bulkiness => Some(crate::tree::TreeColumn::Bulkiness),
            Self::Drive
            | Self::Path
            | Self::Name
            | Self::PathOnly
            | Self::Size
            | Self::SizeOnDisk
            | Self::Created
            | Self::Modified
            | Self::Accessed
            | Self::Extension
            | Self::Type
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
            | Self::DirectoryFlag
            | Self::RecallOnOpen
            | Self::RecallOnDataAccess
            | Self::ParityAttributes
            | Self::NameLength
            | Self::PathLength => None,
        }
    }

    /// Sortable fields in TUI cycle order.
    pub const SORT_CYCLE: &'static [Self] = &[
        Self::Name,
        Self::Size,
        Self::SizeOnDisk,
        Self::Created,
        Self::Modified,
        Self::Accessed,
        Self::Path,
        Self::Drive,
        Self::Extension,
        Self::Type,
        Self::Descendants,
        Self::TreeAllocated,
        Self::Bulkiness,
    ];

    /// Return the next sort field in the cycle, wrapping around.
    #[must_use]
    pub fn cycle_next(self) -> Self {
        let mut found = false;
        for &candidate in Self::SORT_CYCLE {
            if found {
                return candidate;
            }
            if candidate == self {
                found = true;
            }
        }
        // Wrap around or fall back to Name for non-cycle fields.
        Self::SORT_CYCLE.first().copied().unwrap_or(Self::Name)
    }

    /// Return the nearest sortable field for a non-sortable field.
    ///
    /// For example, attribute boolean columns map to `Name` sort,
    /// `PathOnly` maps to `Path`, `TreeSize` maps to `Size`.
    #[must_use]
    pub const fn nearest_sort_field(self) -> Self {
        match self {
            Self::Path | Self::PathOnly => Self::Path,
            Self::Size | Self::TreeSize => Self::Size,
            Self::SizeOnDisk => Self::SizeOnDisk,
            Self::Created => Self::Created,
            Self::Modified => Self::Modified,
            Self::Accessed => Self::Accessed,
            Self::Extension => Self::Extension,
            Self::Type => Self::Type,
            Self::Drive => Self::Drive,
            Self::Descendants => Self::Descendants,
            Self::TreeAllocated => Self::TreeAllocated,
            Self::Bulkiness => Self::Bulkiness,
            Self::NameLength => Self::NameLength,
            Self::PathLength => Self::PathLength,
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
            | Self::DirectoryFlag
            | Self::RecallOnOpen
            | Self::RecallOnDataAccess
            | Self::ParityAttributes => Self::Name,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn field_id_parse_accepts_common_aliases() {
        let cases = [
            ("drv", FieldId::Drive),
            ("path only", FieldId::PathOnly),
            ("allocated", FieldId::SizeOnDisk),
            ("written", FieldId::Modified),
            ("ext", FieldId::Extension),
            ("directory", FieldId::Type),
            ("r", FieldId::ReadOnly),
            ("notcontent", FieldId::NotIndexed),
            ("decendents", FieldId::Descendants),
            ("parityattributes", FieldId::ParityAttributes),
        ];

        for (input, expected) in cases {
            assert_eq!(FieldId::parse(input), Some(expected), "alias '{input}'");
        }
    }

    #[test]
    fn field_id_metadata_round_trips_canonical_names() {
        for &field in FieldId::ALL {
            let meta = field.metadata();
            assert_eq!(FieldId::parse(meta.canonical_name), Some(field));
            assert!(meta.projectable || meta.filterable || meta.sortable);
        }
    }

    #[test]
    fn field_id_metadata_captures_default_sort_direction() {
        assert_eq!(
            FieldId::Name.metadata().default_sort_direction,
            Some(SortDirection::Ascending)
        );
        assert_eq!(
            FieldId::Size.metadata().default_sort_direction,
            Some(SortDirection::Descending)
        );
        // Boolean attribute fields default to descending (flagged first).
        assert_eq!(
            FieldId::ReadOnly.metadata().default_sort_direction,
            Some(SortDirection::Descending)
        );
        // Non-sortable fields have no default direction.
        assert_eq!(
            FieldId::ParityAttributes.metadata().default_sort_direction,
            None
        );
    }

    #[test]
    fn field_id_sortable_matches_metadata() {
        assert!(FieldId::Size.metadata().sortable);
        assert!(FieldId::Descendants.metadata().sortable);
        // Boolean attribute fields are sortable (groups true/false via
        // field_to_attr_bit).
        assert!(FieldId::ReadOnly.metadata().sortable);
        assert!(FieldId::Hidden.metadata().sortable);
        assert!(FieldId::System.metadata().sortable);
        assert!(FieldId::Compressed.metadata().sortable);
        assert!(FieldId::DirectoryFlag.metadata().sortable);
        // Non-sortable fields.
        assert!(!FieldId::ParityAttributes.metadata().sortable);
    }

    #[test]
    fn field_id_presentation_fields_non_empty_for_projectable() {
        for &field in FieldId::ALL {
            let meta = field.metadata();
            if meta.projectable {
                assert!(
                    !meta.display_name.is_empty(),
                    "projectable field {field:?} has empty display_name",
                );
                assert!(
                    !meta.tui_label.is_empty(),
                    "projectable field {field:?} has empty tui_label",
                );
            }
        }
    }

    #[test]
    fn field_id_cycle_next_wraps_around() {
        let first = FieldId::SORT_CYCLE[0];
        let last = *FieldId::SORT_CYCLE.last().unwrap();
        assert_eq!(last.cycle_next(), first);
    }

    #[test]
    fn field_id_nearest_sort_maps_non_sortable_to_name() {
        assert_eq!(FieldId::ReadOnly.nearest_sort_field(), FieldId::Name);
        assert_eq!(FieldId::Hidden.nearest_sort_field(), FieldId::Name);
    }

    #[test]
    fn field_id_tree_field_detection() {
        assert!(FieldId::Descendants.is_tree_field());
        assert!(FieldId::TreeSize.is_tree_field());
        assert!(FieldId::TreeAllocated.is_tree_field());
        assert!(FieldId::Bulkiness.is_tree_field());
        assert!(!FieldId::Size.is_tree_field());
    }

    // ── Aggregate capability tests ─────────────────────────────────

    #[test]
    fn aggregate_capability_table() {
        // This test IS the generated capability table. Run with --nocapture
        // to see it. The printed output is the authoritative reference for
        // which fields support which aggregation operations.
        println!();
        println!(
            "{:<22} {:>6} {:>5} {:>6} {:>7} {:>10} {:>3}",
            "Field", "Type", "Agg", "Group", "Bucket", "Cardinality", "Top"
        );
        println!("{}", "-".repeat(65));
        for &field in FieldId::ALL {
            let m = field.metadata();
            let a = &m.aggregate;
            println!(
                "{:<22} {:>6} {:>5} {:>6} {:>7} {:>10} {:>3}",
                m.canonical_name,
                format!("{:?}", m.field_type),
                if a.aggregatable { "yes" } else { "-" },
                if a.groupable { "yes" } else { "-" },
                if a.bucket_support { "yes" } else { "-" },
                format!("{:?}", a.cardinality),
                if a.default_top > 0 {
                    format!("{}", a.default_top)
                } else {
                    "-".to_string()
                },
            );
        }
        println!("{}", "-".repeat(65));

        // Summary counts
        let total = FieldId::ALL.len();
        let agg = FieldId::ALL
            .iter()
            .filter(|f| f.metadata().aggregate.aggregatable)
            .count();
        let grp = FieldId::ALL
            .iter()
            .filter(|f| f.metadata().aggregate.groupable)
            .count();
        let bkt = FieldId::ALL
            .iter()
            .filter(|f| f.metadata().aggregate.bucket_support)
            .count();
        println!("Total: {total}  Aggregatable: {agg}  Groupable: {grp}  Bucketable: {bkt}");
    }

    #[test]
    fn every_field_has_valid_aggregate_meta() {
        for &field in FieldId::ALL {
            let m = field.metadata();
            let a = &m.aggregate;

            // Numeric and Timestamp fields should be aggregatable.
            if matches!(m.field_type, FieldType::Numeric | FieldType::Timestamp) {
                assert!(
                    a.aggregatable,
                    "{:?} is {:?} but not aggregatable",
                    field, m.field_type
                );
            }

            // Bool fields should be groupable (they make ideal facets).
            if m.field_type == FieldType::Bool {
                assert!(
                    a.groupable,
                    "{:?} is Bool but not groupable",
                    field
                );
                assert_eq!(
                    a.cardinality,
                    Cardinality::Fixed,
                    "{:?} is Bool but cardinality is not Fixed",
                    field
                );
            }

            // Groupable fields must have default_top > 0.
            if a.groupable {
                assert!(
                    a.default_top > 0,
                    "{:?} is groupable but default_top is 0",
                    field
                );
            }

            // Non-groupable fields should have default_top == 0.
            if !a.groupable {
                assert_eq!(
                    a.default_top, 0,
                    "{:?} is not groupable but has default_top={}",
                    field, a.default_top
                );
            }

            // Bucket support should only be on numeric/timestamp fields.
            if a.bucket_support {
                assert!(
                    matches!(m.field_type, FieldType::Numeric | FieldType::Timestamp),
                    "{:?} has bucket_support but is {:?}",
                    field, m.field_type
                );
            }

            // Fixed cardinality should only be on Bool/Enum fields.
            if a.cardinality == Cardinality::Fixed {
                assert!(
                    matches!(m.field_type, FieldType::Bool | FieldType::Enum),
                    "{:?} has Fixed cardinality but is {:?}",
                    field, m.field_type
                );
            }
        }
    }

    #[test]
    fn aggregate_bool_fields_are_facets() {
        // All 19 bool attribute fields + DirectoryFlag should be groupable
        // with Fixed cardinality and default_top=2.
        let bool_fields: Vec<_> = FieldId::ALL
            .iter()
            .filter(|f| f.metadata().field_type == FieldType::Bool)
            .collect();

        assert!(
            bool_fields.len() >= 19,
            "Expected at least 19 bool fields, got {}",
            bool_fields.len()
        );

        for &&field in &bool_fields {
            let a = field.metadata().aggregate;
            assert!(a.groupable, "{field:?} is Bool but not groupable");
            assert_eq!(a.cardinality, Cardinality::Fixed, "{field:?}");
            assert_eq!(a.default_top, 2, "{field:?}");
            assert!(!a.aggregatable, "{field:?} Bool should not be aggregatable");
            assert!(!a.bucket_support, "{field:?} Bool should not have bucket support");
        }
    }

    #[test]
    fn aggregate_numeric_fields_are_aggregatable_and_bucketable() {
        let numeric_fields: Vec<_> = FieldId::ALL
            .iter()
            .filter(|f| f.metadata().field_type == FieldType::Numeric)
            .collect();

        assert!(
            numeric_fields.len() >= 8,
            "Expected at least 8 numeric fields, got {}",
            numeric_fields.len()
        );

        for &&field in &numeric_fields {
            let a = field.metadata().aggregate;
            assert!(a.aggregatable, "{field:?} is Numeric but not aggregatable");
            assert!(a.bucket_support, "{field:?} is Numeric but not bucketable");
            assert!(!a.groupable, "{field:?} Numeric should not be groupable");
        }
    }

    #[test]
    fn aggregate_timestamp_fields_are_aggregatable_and_bucketable() {
        let ts_fields = [FieldId::Created, FieldId::Modified, FieldId::Accessed];
        for field in ts_fields {
            let a = field.metadata().aggregate;
            assert!(a.aggregatable, "{field:?}");
            assert!(a.bucket_support, "{field:?}");
            assert!(!a.groupable, "{field:?} Timestamp should not be groupable");
        }
    }

    #[test]
    fn aggregate_key_fields_have_correct_cardinality() {
        assert_eq!(
            FieldId::Drive.metadata().aggregate.cardinality,
            Cardinality::Fixed
        );
        assert_eq!(
            FieldId::Type.metadata().aggregate.cardinality,
            Cardinality::Low
        );
        assert_eq!(
            FieldId::Extension.metadata().aggregate.cardinality,
            Cardinality::Medium
        );
        assert_eq!(
            FieldId::Name.metadata().aggregate.cardinality,
            Cardinality::Unbounded
        );
        assert_eq!(
            FieldId::PathOnly.metadata().aggregate.cardinality,
            Cardinality::Unbounded
        );
    }

    #[test]
    fn aggregate_non_aggregatable_fields() {
        // Path, Attributes, AttributeValue, ParityAttributes should not be
        // aggregatable, groupable, or bucketable.
        let inert = [
            FieldId::Path,
            FieldId::Attributes,
            FieldId::AttributeValue,
            FieldId::ParityAttributes,
        ];
        for field in inert {
            let a = field.metadata().aggregate;
            assert!(!a.aggregatable, "{field:?}");
            assert!(!a.groupable, "{field:?}");
            assert!(!a.bucket_support, "{field:?}");
        }
    }
}
