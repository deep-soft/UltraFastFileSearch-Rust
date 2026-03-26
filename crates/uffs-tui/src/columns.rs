//! TUI column definitions — re-exported from `uffs-core` with
//! TUI-specific width constraints (ratatui dependency).

// Re-export all generic column types from uffs-core
pub use uffs_core::search::columns::{DEFAULT_COLUMNS, TuiColumn, parse_columns};

/// Default width constraint for a column (TUI-specific, depends on ratatui).
#[must_use]
#[expect(
    clippy::single_call_fn,
    reason = "layout helper — clarity over inlining into render loop"
)]
pub const fn default_constraint(col: TuiColumn) -> ratatui::layout::Constraint {
    use ratatui::layout::Constraint;
    match col {
        TuiColumn::Drive => Constraint::Length(3),
        TuiColumn::Name => Constraint::Min(20),
        TuiColumn::Path => Constraint::Length(62),
        TuiColumn::PathOnly => Constraint::Length(52),
        TuiColumn::Size | TuiColumn::SizeOnDisk | TuiColumn::TreeSize => Constraint::Length(12),
        TuiColumn::Created | TuiColumn::Modified | TuiColumn::Accessed => Constraint::Length(19),
        TuiColumn::Extension => Constraint::Length(10),
        TuiColumn::Type => Constraint::Length(6),
        TuiColumn::Attributes | TuiColumn::AttributeValue | TuiColumn::Descendants => {
            Constraint::Length(8)
        }
        // Boolean attribute columns — narrow
        TuiColumn::Hidden
        | TuiColumn::System
        | TuiColumn::Archive
        | TuiColumn::ReadOnly
        | TuiColumn::Compressed
        | TuiColumn::Encrypted
        | TuiColumn::Sparse
        | TuiColumn::Reparse
        | TuiColumn::Offline
        | TuiColumn::NotIndexed
        | TuiColumn::Temporary
        | TuiColumn::Virtual
        | TuiColumn::Pinned
        | TuiColumn::Unpinned
        | TuiColumn::Integrity
        | TuiColumn::NoScrub
        | TuiColumn::DirectoryFlag => Constraint::Length(4),
    }
}
