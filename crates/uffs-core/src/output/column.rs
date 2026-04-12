//! Output column definitions and mappings.
//!
//! All output columns are now represented by [`FieldId`]. This module
//! provides the legacy column order constants and the type alias.

use crate::search::field::FieldId;

/// Legacy type alias — all output columns are now `FieldId`.
pub type OutputColumn = FieldId;

/// Column order matching C++ output exactly (25 columns).
///
/// Used by `--parity-compat` to produce output that matches the C++ baseline.
pub(crate) const PARITY_COLUMN_ORDER: &[FieldId] = &[
    FieldId::Path,
    FieldId::Name,
    FieldId::PathOnly,
    FieldId::Size,
    FieldId::SizeOnDisk,
    FieldId::Created,
    FieldId::Modified,
    FieldId::Accessed,
    FieldId::Descendants,
    FieldId::ReadOnly,
    FieldId::Archive,
    FieldId::System,
    FieldId::Hidden,
    FieldId::Offline,
    FieldId::NotIndexed,
    FieldId::NoScrub,
    FieldId::Integrity,
    FieldId::Pinned,
    FieldId::Unpinned,
    FieldId::DirectoryFlag,
    FieldId::Compressed,
    FieldId::Encrypted,
    FieldId::Sparse,
    FieldId::Reparse,
    FieldId::ParityAttributes,
];

/// Default column order: data columns + boolean attributes in NTFS flag value
/// order (lowest → highest), matching the NTFS evolution of attribute flags.
///
/// This is the order used when `--columns all` is specified.
pub const CPP_COLUMN_ORDER: &[FieldId] = &[
    // ── Data columns ────────────────────────────────────────────────
    FieldId::Path,
    FieldId::Name,
    FieldId::PathOnly,
    FieldId::Size,
    FieldId::SizeOnDisk,
    FieldId::Created,
    FieldId::Modified,
    FieldId::Accessed,
    FieldId::Descendants,
    // ── Boolean attributes in NTFS flag value order ─────────────────
    FieldId::ReadOnly,           // 0x0001
    FieldId::Hidden,             // 0x0002
    FieldId::System,             // 0x0004
    FieldId::DirectoryFlag,      // 0x0010
    FieldId::Archive,            // 0x0020
    FieldId::Sparse,             // 0x0200
    FieldId::Reparse,            // 0x0400
    FieldId::Compressed,         // 0x0800
    FieldId::Offline,            // 0x1000
    FieldId::NotIndexed,         // 0x2000
    FieldId::Encrypted,          // 0x4000
    FieldId::Integrity,          // 0x8000
    FieldId::NoScrub,            // 0x20000
    FieldId::RecallOnOpen,       // 0x40000
    FieldId::Pinned,             // 0x80000
    FieldId::Unpinned,           // 0x100000
    FieldId::RecallOnDataAccess, // 0x400000
    // ── Raw aggregate ───────────────────────────────────────────────
    FieldId::Attributes,
    // ── Computed / derived columns ──────────────────────────────────
    FieldId::TreeSize,
    FieldId::TreeAllocated,
    FieldId::Bulkiness,
    FieldId::Type,
    FieldId::Extension,
    FieldId::NameLength,
    FieldId::PathLength,
];
