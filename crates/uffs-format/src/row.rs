// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Row abstraction used by the formatter.
//!
//! `uffs-format` must work against two concrete row types without
//! depending on either crate:
//!
//! - `uffs_core::search::backend::DisplayRow` — held by the daemon, storing the
//!   filename as an offset slice into `path` so the formatter can emit `name`
//!   without an extra allocation.
//! - `uffs_client::protocol::response::SearchRow` — the wire type the CLI
//!   receives over IPC.  Stores filename as a standalone `String` because the
//!   JSON wire format serialises it that way.
//!
//! Both impl [`FormatRow`] locally, and every formatter routine is
//! generic over `R: FormatRow`.  The trait is intentionally dense —
//! the CSV dispatch reads every field on the hot path, so abstracting
//! per-field accessors would trade one virtual call for thirteen.

/// Read-only view of the fields the formatter emits.
///
/// Every accessor must return in O(1) and without allocation — the
/// formatter calls these per row on the hot path and the parallel
/// writer relies on them being cheap enough to inline.
pub trait FormatRow {
    /// Drive letter (e.g. `'C'`).
    fn drive(&self) -> char;
    /// Full resolved path (e.g. `"C:\\Users\\alice\\file.rs"`).
    fn path(&self) -> &str;
    /// Filename slice (the portion of `path` after the final `\`).
    ///
    /// Implementations are expected to return a borrowed slice —
    /// either pre-computed (`DisplayRow::name()`) or stored verbatim
    /// (`SearchRow::name.as_str()`).  Must never allocate.
    fn name(&self) -> &str;
    /// Logical file size in bytes.  Zero for directories.
    fn size(&self) -> u64;
    /// Whether the row represents a directory.
    fn is_directory(&self) -> bool;
    /// Last-modified time (raw NTFS FILETIME — 100-ns ticks since
    /// 1601-01-01).
    fn modified(&self) -> i64;
    /// Creation time (raw NTFS FILETIME).
    fn created(&self) -> i64;
    /// Last-access time (raw NTFS FILETIME).
    fn accessed(&self) -> i64;
    /// Raw NTFS `FILE_ATTRIBUTE_*` bits.
    fn flags(&self) -> u32;
    /// Allocated size on disk in bytes.
    fn allocated(&self) -> u64;
    /// Descendant count (directories only; files always return 0).
    fn descendants(&self) -> u32;
    /// Sum of logical file sizes in the subtree (directories only).
    fn treesize(&self) -> u64;
    /// Sum of allocated sizes in the subtree (directories only).
    fn tree_allocated(&self) -> u64;
}
