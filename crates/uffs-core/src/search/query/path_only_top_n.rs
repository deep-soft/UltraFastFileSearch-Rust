// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Two-phase tree walk for `FieldId::PathOnly` sort.
//!
//! Emits results in `path_only`-sorted order with early termination at
//! `limit`, matching the Windows Explorer "Folder" column convention.
//! The walk produces results in the exact order
//! `sort_rows(.., FieldId::PathOnly, sort_desc, &[])` would — i.e.:
//!
//!   * **Primary key** `path_only` (folded) — lexicographic per `sort_desc`
//!     (ASC or DESC).
//!   * **Tiebreaker** `name` (folded) — *always* ASC, matching the contract
//!     declared in `sort_rows` (the Name tiebreaker is NOT flipped by
//!     `sort_desc`).
//!
//! ## Why not just DFS?
//!
//! The naïve pre-order DFS used for `FieldId::Path` (full-path sort)
//! is equivalent to full-path-ASC, but **not** `path_only`-ASC.  For
//! example, given:
//!
//! ```text
//! C:\
//!   ├── alpha.txt   (path_only = C:\)
//!   ├── beta\       (dir, path_only = C:\)
//!   │   └── a.txt   (path_only = C:\beta\)
//!   └── zeta.txt    (path_only = C:\)
//! ```
//!
//! Pre-order DFS emits `alpha.txt, beta, a.txt, zeta.txt` — but
//! `path_only`-ASC demands `alpha.txt, beta, zeta.txt, a.txt` (all
//! three C:\ entries before C:\beta\ entries).
//!
//! Before this module, the `PathOnly` branch worked around this by
//! walking the **entire** matching record set (`limit = usize::MAX`),
//! resolving every path, then sorting and truncating — catastrophic
//! for queries like `*.exe --sort path_only` (60 k matches × path
//! resolution ≈ 15 s, observed in scenario-T67f).
//!
//! ## The two-phase walk
//!
//! At each directory `D`, we process children in two phases:
//!
//!   1. **Emit** every child of `D` (files AND subdirs alike) in name-ASC
//!      order.  All these children share `path_only = D.path` so name-ASC
//!      tiebreaker is correct within the group.
//!   2. **Recurse** into each subdir child of `D` in name order. Their own
//!      children have `path_only = subdir.path`, strictly greater than `D.path`
//!      in lex order.
//!
//! For `sort_desc = true` the two phases are reversed (subtrees first,
//! then parent's own children) and the recurse order is name-DESC.
//! Children still emit in name-ASC within each group — the tiebreaker
//! does not flip.
//!
//! This walk produces results in the exact target order, so the caller
//! can stop as soon as `limit` rows are collected and no post-sort is
//! required.

use super::super::backend::{self, DisplayRow, FilterMode};
use super::super::field::FieldId;
use super::super::filters::{SearchFilters, row_passes_filters};
use super::super::tree::{self, DirCache, DirCacheExt as _};
use super::numeric_top_n::sort_indices_by_name;
use super::{make_display_row, passes_filter_mode, stack_volume_prefix};
use crate::compact::DriveCompactIndex;

/// Collect up to `limit` display rows in `path_only`-sorted order.
///
/// Drives are processed in letter-ASC (or letter-DESC if `sort_desc`)
/// order; within each drive the two-phase walk above produces results
/// directly in `path_only`-sorted order with name-ASC tiebreaker.
///
/// Early termination kicks in the moment `output.len() >= limit`.
///
/// ## Ext-index fast path
///
/// When `search_filters.is_ext_only()` holds and `filter_mode` is
/// `All` or `FilesOnly`, this function short-circuits the full tree
/// walk and uses [`collect_path_only_via_ext_index`] instead —
/// dropping `*.dll --sort path_only` from a C-drive-wide
/// ~3 s traversal to ~250 ms (matches the numeric branch's cost).
/// The tree walk visits every record and resolves every path before
/// applying the extension filter; the ext-index path visits only the
/// ~`N_ext` candidates already bucketed by `ExtensionIndex`.
pub(super) fn collect_path_only_sorted_top_n<D: AsRef<DriveCompactIndex>>(
    drives: &[D],
    limit: usize,
    sort_desc: bool,
    filter_mode: FilterMode,
    search_filters: &mut SearchFilters,
) -> Vec<DisplayRow> {
    if limit == 0 {
        return Vec::new();
    }

    // Fast path: ext-only filter + FilesOnly-or-All mode → skip the
    // full-tree walk entirely.  The tree walk is O(N_total) in the
    // drive record count; the ext-index path is O(N_ext) in the
    // per-extension bucket size.  Empirically ~10× speedup on a
    // 1 M-record C: drive for `*.dll --sort path_only`.
    if search_filters.is_ext_only()
        && matches!(filter_mode, FilterMode::All | FilterMode::FilesOnly)
    {
        return collect_path_only_via_ext_index(
            drives,
            limit,
            sort_desc,
            filter_mode,
            search_filters,
        );
    }

    let mut output: Vec<DisplayRow> = Vec::new();

    // Drive order: letter-ASC or letter-DESC depending on `sort_desc`.
    // Within a drive every `path_only` starts with that drive's
    // `X:\` prefix, so inter-drive ordering is purely by letter.
    let mut drive_order: Vec<usize> = (0..drives.len()).collect();
    drive_order.sort_unstable_by(|&idx_a, &idx_b| {
        let Some(drive_a) = drives.get(idx_a) else {
            return core::cmp::Ordering::Equal;
        };
        let Some(drive_b) = drives.get(idx_b) else {
            return core::cmp::Ordering::Equal;
        };
        let ord = drive_a.as_ref().letter.cmp(&drive_b.as_ref().letter);
        if sort_desc { ord.reverse() } else { ord }
    });

    // Default `$UpCase` fold table — per-drive tables aren't available
    // from the compact snapshot.  Reused across all rows for zero-alloc
    // filter checks.
    let fold = uffs_text::case_fold::CaseFold::default_table();
    let mut fold_buf: Vec<u8> = Vec::with_capacity(256);

    for &drive_idx in &drive_order {
        if output.len() >= limit {
            break;
        }
        let Some(drive_ref) = drives.get(drive_idx) else {
            continue;
        };
        let drive = drive_ref.as_ref();
        let mut vp_buf = [0_u8; 4];
        let volume_prefix = stack_volume_prefix(&mut vp_buf, drive.letter);
        let mut dir_cache = DirCache::with_capacity(256);

        // Roots = records whose parent is `u32::MAX` (typically the
        // drive root "." at FRS 5, though stray orphans are possible).
        // Tiebreaker is ALWAYS name-ASC regardless of `sort_desc`.
        let mut roots: Vec<u32> = drive
            .records
            .iter()
            .enumerate()
            .filter(|(_, rec)| rec.parent_idx == u32::MAX && rec.name_len > 0)
            .map(|(idx, _)| uffs_mft::len_to_u32(idx))
            .collect();
        sort_indices_by_name(&mut roots, drive, false);

        if sort_desc {
            walk_drive_desc(
                drive,
                &roots,
                limit,
                volume_prefix,
                &fold,
                &mut fold_buf,
                &mut dir_cache,
                filter_mode,
                search_filters,
                &mut output,
            );
        } else {
            walk_drive_asc(
                drive,
                &roots,
                limit,
                volume_prefix,
                &fold,
                &mut fold_buf,
                &mut dir_cache,
                filter_mode,
                search_filters,
                &mut output,
            );
        }
    }

    output
}

/// ASC walk: emit each directory's children (name-ASC), then recurse
/// into subdir-children (name-ASC).
///
/// Iterative; stack holds directories whose children still need to be
/// emitted.  Subdirs are pushed in reverse name order so the
/// first-name subdir is popped and processed before its later
/// siblings — depth-first recursion in name-ASC.
#[expect(
    clippy::too_many_arguments,
    reason = "shared state between walk and emit: drive, roots, fold, filters, output"
)]
fn walk_drive_asc(
    drive: &DriveCompactIndex,
    roots: &[u32],
    limit: usize,
    volume_prefix: &str,
    fold: &uffs_text::case_fold::CaseFold,
    fold_buf: &mut Vec<u8>,
    dir_cache: &mut DirCache,
    filter_mode: FilterMode,
    search_filters: &SearchFilters,
    output: &mut Vec<DisplayRow>,
) {
    // Phase 1 for the drive-root "level": emit each root record.
    // Roots share `path_only = X:\` so name-ASC is the natural order.
    for &root_idx in roots {
        if output.len() >= limit {
            return;
        }
        emit_if_passes(
            drive,
            root_idx,
            volume_prefix,
            filter_mode,
            search_filters,
            fold,
            fold_buf,
            dir_cache,
            output,
        );
    }

    // Phase 2 for the drive-root level: push subdir-roots onto the
    // stack in reverse name order so the first-name root's subtree is
    // traversed first.
    let mut stack: Vec<u32> = Vec::new();
    for &root_idx in roots.iter().rev() {
        if is_dir(drive, root_idx) {
            stack.push(root_idx);
        }
    }

    while let Some(dir_idx) = stack.pop() {
        if output.len() >= limit {
            return;
        }
        let child_slice = drive.children.get(dir_idx as usize);
        if child_slice.is_empty() {
            continue;
        }
        let mut sorted: Vec<u32> = child_slice.to_vec();
        sort_indices_by_name(&mut sorted, drive, false);

        // Phase 1: emit every child in name-ASC order.  They all share
        // `path_only = dir_idx.path`.
        for &child_idx in &sorted {
            if output.len() >= limit {
                return;
            }
            emit_if_passes(
                drive,
                child_idx,
                volume_prefix,
                filter_mode,
                search_filters,
                fold,
                fold_buf,
                dir_cache,
                output,
            );
        }

        // Phase 2: push subdir-children in reverse name order so the
        // first-name subdir pops next (depth-first, name-ASC).
        for &child_idx in sorted.iter().rev() {
            if is_dir(drive, child_idx) {
                stack.push(child_idx);
            }
        }
    }
}

/// Task type for the DESC walker's explicit stack.
///
/// DESC requires "recurse before emit" at each level; an iterative
/// walk can't do that with a plain dir-index stack, so we tag each
/// task explicitly.
enum DescTask {
    /// Emit this record (post-filter) — used for emitting a parent
    /// AFTER its subtree has been processed.
    Emit(u32),
    /// Expand this directory: push emits for its children plus
    /// recurse-tasks for its subdir children.
    Recurse(u32),
}

/// DESC walk: recurse into subdirs (name-DESC) FIRST, then emit each
/// directory's children (name-ASC).
///
/// Name tiebreaker stays ASC even when the primary key is DESC —
/// matches the contract declared in `sort_rows`.  Iterative, with
/// explicit `Task::Emit` / `Task::Recurse` entries on a single stack
/// to encode the "recurse-then-emit" phase ordering.
#[expect(
    clippy::too_many_arguments,
    reason = "shared state between walk and emit: drive, roots, fold, filters, output"
)]
fn walk_drive_desc(
    drive: &DriveCompactIndex,
    roots: &[u32],
    limit: usize,
    volume_prefix: &str,
    fold: &uffs_text::case_fold::CaseFold,
    fold_buf: &mut Vec<u8>,
    dir_cache: &mut DirCache,
    filter_mode: FilterMode,
    search_filters: &SearchFilters,
    output: &mut Vec<DisplayRow>,
) {
    // Seed stack.  Goal (top-to-bottom on stack):
    //     Recurse(root_N), ..., Recurse(root_1),
    //     Emit(root_1),    ..., Emit(root_N)
    // Pop order: Recurse(root_N) first (its subtree is emitted before
    // any of the drive-root emits), then Recurse(root_{N-1}), etc.,
    // then Emit(root_1), Emit(root_2), ..., Emit(root_N) — i.e. emits
    // in name-ASC (tiebreaker does not flip).
    let mut stack: Vec<DescTask> = Vec::new();
    // Push emits in reverse name order so they pop in name-ASC.
    for &root_idx in roots.iter().rev() {
        stack.push(DescTask::Emit(root_idx));
    }
    // Push recurse-tasks in forward name order so they pop in
    // reverse name order (name-DESC recursion).
    for &root_idx in roots {
        if is_dir(drive, root_idx) {
            stack.push(DescTask::Recurse(root_idx));
        }
    }

    while let Some(task) = stack.pop() {
        if output.len() >= limit {
            return;
        }
        match task {
            DescTask::Emit(idx) => {
                emit_if_passes(
                    drive,
                    idx,
                    volume_prefix,
                    filter_mode,
                    search_filters,
                    fold,
                    fold_buf,
                    dir_cache,
                    output,
                );
            }
            DescTask::Recurse(dir_idx) => {
                let child_slice = drive.children.get(dir_idx as usize);
                if child_slice.is_empty() {
                    continue;
                }
                let mut sorted: Vec<u32> = child_slice.to_vec();
                sort_indices_by_name(&mut sorted, drive, false);

                // Push emits in reverse name order so emits pop in
                // name-ASC (the tiebreaker).
                for &child_idx in sorted.iter().rev() {
                    stack.push(DescTask::Emit(child_idx));
                }
                // Push recurse-tasks in forward name order so they
                // pop in reverse name order — processing the
                // largest-name subdir's subtree before its
                // earlier-named siblings.
                for &child_idx in &sorted {
                    if is_dir(drive, child_idx) {
                        stack.push(DescTask::Recurse(child_idx));
                    }
                }
            }
        }
    }
}

/// Emit a single record as a `DisplayRow` if it passes `filter_mode`
/// and every `search_filters` predicate.
///
/// Returns `true` if the record was pushed (caller may want to
/// re-check the limit immediately).  Records with empty names are
/// skipped silently — they carry no user-visible content.
#[expect(
    clippy::too_many_arguments,
    reason = "borrowed per-walk state: volume_prefix, fold, fold_buf, dir_cache, output"
)]
fn emit_if_passes(
    drive: &DriveCompactIndex,
    idx: u32,
    volume_prefix: &str,
    filter_mode: FilterMode,
    search_filters: &SearchFilters,
    fold: &uffs_text::case_fold::CaseFold,
    fold_buf: &mut Vec<u8>,
    dir_cache: &mut DirCache,
    output: &mut Vec<DisplayRow>,
) -> bool {
    let Some(rec) = drive.records.get(idx as usize) else {
        return false;
    };
    let name = rec.name(&drive.names);
    if name.is_empty() {
        return false;
    }
    if !passes_filter_mode(rec.is_directory(), filter_mode) {
        return false;
    }
    let path = tree::resolve_path_cached(drive, idx as usize, volume_prefix, dir_cache);
    let row = make_display_row(idx, drive.letter, rec, name, path);
    if !row_passes_filters(&row, search_filters, fold, fold_buf) {
        return false;
    }
    output.push(row);
    true
}

/// Fast "is this record a directory" lookup by index.
#[inline]
fn is_dir(drive: &DriveCompactIndex, idx: u32) -> bool {
    drive
        .records
        .get(idx as usize)
        .is_some_and(|rec| rec.is_directory())
}

/// Ext-index fast path for `PathOnly` sort.
///
/// Called from [`collect_path_only_sorted_top_n`] when
/// `search_filters.is_ext_only()` holds and `filter_mode` is `All` or
/// `FilesOnly`.  Mirrors the numeric branch's ext fast-path shape
/// (see `numeric_top_n::collect_global_top_n_numeric`):
///
///   1. Iterate `drive.ext_index[ext_id]` for every drive and every resolved
///      extension id.  This narrows the candidate set from `O(N_total)` to
///      `O(N_ext)`.
///   2. Apply the two cheap per-candidate predicates `is_ext_only()` admits —
///      `hide_system` (`$`-prefix byte check) and `hide_ads` (`memchr(b':')` on
///      the name arena slice).  Both run before path resolution.
///   3. Resolve each survivor's path via `tree::resolve_path_cached`. Because
///      the `ext_index` bucket is stored in FRN order
///      (`compact::ExtensionIndex` sorts by FRS), sibling records land next to
///      each other in the iteration, so the `DirCache` stays warm across the
///      loop.
///   4. Sort the materialised `DisplayRow`s via `backend::sort_rows` with
///      `FieldId::PathOnly` to apply the name-ASC tiebreaker, then truncate to
///      `limit`.
///
/// Contrast with the two-phase tree walk above: the tree walk has to
/// visit every record on the drive (including directories and
/// non-matching files) and resolve every path before the extension
/// filter has a chance to reject it.  For `*.dll` on a 1 M-record
/// C: drive the tree walk costs ~3 s versus ~250 ms here.
fn collect_path_only_via_ext_index<D: AsRef<DriveCompactIndex>>(
    drives: &[D],
    limit: usize,
    sort_desc: bool,
    filter_mode: FilterMode,
    search_filters: &mut SearchFilters,
) -> Vec<DisplayRow> {
    // `hide_system` / `hide_ads` are captured once — the rest of the
    // filter set is guaranteed empty by the `is_ext_only()` caller
    // gate (min/max size, date, attribute, type, path filters all
    // disqualify the fast path; see `SearchFilters::is_ext_only`).
    let hide_system = search_filters.hide_system;
    let hide_ads = search_filters.hide_ads;

    // Collect (drive_idx, rec_idx) pairs for every candidate that
    // survives the per-record predicates.  We do NOT bound this
    // by `limit` here: `PathOnly` ordering isn't known until paths
    // are resolved, so early termination on the pre-resolve stream
    // would be wrong.  The candidate set size is bounded by the
    // per-extension bucket, not by drive cardinality, so carrying
    // all ~N_ext survivors is cheap.
    let mut candidates: Vec<(u16, u32)> = Vec::new();
    for (drive_idx, drive_ref) in drives.iter().enumerate() {
        let drive = drive_ref.as_ref();
        search_filters.resolve_ext_ids_for_drive(drive);
        if search_filters.resolved_ext_ids.is_empty() {
            // Extension not present on this drive — skip.
            continue;
        }
        let drive_idx_u16 = uffs_mft::len_to_u16(drive_idx);
        // Clone the resolved ids so we can reborrow `search_filters`
        // later if a future filter pushes more predicates.
        for &ext_id in &search_filters.resolved_ext_ids.clone() {
            for &rec_idx_u32 in drive.ext_index.get(ext_id) {
                let rec_idx = rec_idx_u32 as usize;
                let Some(rec) = drive.records.get(rec_idx) else {
                    continue;
                };
                if rec.name_len == 0 {
                    continue;
                }
                if matches!(filter_mode, FilterMode::FilesOnly) && rec.is_directory() {
                    continue;
                }
                if hide_system && rec.is_system_metafile() {
                    continue;
                }
                if hide_ads {
                    let name = rec.name(&drive.names);
                    if memchr::memchr(b':', name.as_bytes()).is_some() {
                        continue;
                    }
                }
                candidates.push((drive_idx_u16, rec_idx_u32));
            }
        }
    }

    // Path-resolve every survivor.  Candidates arrive in ext-bucket
    // order (FRN-ascending per drive), so adjacent `resolve_path_cached`
    // calls share parent directories and the `DirCache` stays warm.
    // A per-drive cache map keeps the invariant that `DirCache` only
    // contains entries from the same drive.
    let mut dir_caches: std::collections::HashMap<u16, DirCache> = std::collections::HashMap::new();
    let mut rows: Vec<DisplayRow> = Vec::with_capacity(candidates.len());
    for &(drive_idx, rec_idx) in &candidates {
        let Some(drive_ref) = drives.get(drive_idx as usize) else {
            continue;
        };
        let drive = drive_ref.as_ref();
        let Some(rec) = drive.records.get(rec_idx as usize) else {
            continue;
        };
        let name = rec.name(&drive.names);
        if name.is_empty() {
            continue;
        }
        let mut vp_buf = [0_u8; 4];
        let volume_prefix = stack_volume_prefix(&mut vp_buf, drive.letter);
        let cache = dir_caches
            .entry(drive_idx)
            .or_insert_with(|| DirCache::with_capacity(256));
        let path = tree::resolve_path_cached(drive, rec_idx as usize, volume_prefix, cache);
        rows.push(make_display_row(rec_idx, drive.letter, rec, name, path));
    }

    // Sort by `PathOnly` with the name-ASC tiebreaker (matches the
    // tree walk's intra-folder order convention), then truncate.
    backend::sort_rows(&mut rows, FieldId::PathOnly, sort_desc, &[]);
    rows.truncate(limit);
    rows
}
