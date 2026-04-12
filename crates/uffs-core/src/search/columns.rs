// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Column definitions and parsing.
//!
//! All columns are now represented by [`FieldId`]. This module provides
//! the default column set and the `--columns` parser.

use super::field::FieldId;

/// Legacy type alias — all TUI columns are now `FieldId`.
pub type TuiColumn = FieldId;

/// Default column set shown when no `--columns` override is active.
pub const DEFAULT_COLUMNS: &[FieldId] = &[
    FieldId::Drive,
    FieldId::Name,
    FieldId::Size,
    FieldId::Modified,
    FieldId::Path,
];

/// Parse a `--columns` value like `"name,size,modified,path"` into a
/// `Vec<FieldId>`.
///
/// Returns `None` for `"all"` or empty/unrecognised input (meaning: use
/// defaults).
#[must_use]
pub fn parse_columns(input: &str) -> Option<Vec<FieldId>> {
    let trimmed = input.trim();
    if trimmed.eq_ignore_ascii_case("all") || trimmed.is_empty() {
        return None;
    }
    let cols: Vec<FieldId> = trimmed
        .split(',')
        .filter_map(|segment| FieldId::parse(segment.trim()))
        .collect();
    if cols.is_empty() { None } else { Some(cols) }
}
