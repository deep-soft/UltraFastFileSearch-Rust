//! TUI column definitions — re-exported from `uffs-core` with
//! TUI-specific width constraints (ratatui dependency).

// Re-export column helpers from uffs-core.
pub use uffs_core::search::columns::{DEFAULT_COLUMNS, parse_columns};
use uffs_core::search::field::FieldId;

/// Default width constraint for a column (TUI-specific, depends on ratatui).
#[must_use]
#[expect(
    clippy::single_call_fn,
    reason = "layout helper — clarity over inlining into render loop"
)]
pub const fn default_constraint(col: FieldId) -> ratatui::layout::Constraint {
    use ratatui::layout::Constraint;
    match col {
        FieldId::Drive => Constraint::Length(3),
        FieldId::Name => Constraint::Min(20),
        FieldId::Path => Constraint::Length(62),
        FieldId::PathOnly => Constraint::Length(52),
        FieldId::Size
        | FieldId::SizeOnDisk
        | FieldId::TreeSize
        | FieldId::TreeAllocated
        | FieldId::Bulkiness => Constraint::Length(12),
        FieldId::Created | FieldId::Modified | FieldId::Accessed => Constraint::Length(19),
        FieldId::Extension => Constraint::Length(10),
        FieldId::Type => Constraint::Length(6),
        FieldId::Attributes | FieldId::AttributeValue | FieldId::Descendants => {
            Constraint::Length(8)
        }
        // Boolean attribute columns — narrow
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
        | FieldId::RecallOnDataAccess => Constraint::Length(4),
        FieldId::ParityAttributes => Constraint::Length(8),
        FieldId::NameLength | FieldId::PathLength => Constraint::Length(5),
    }
}
