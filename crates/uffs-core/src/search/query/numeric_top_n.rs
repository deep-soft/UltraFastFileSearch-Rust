//! Internal numeric top-N collection and sort helpers.

use alloc::collections::BinaryHeap;

use super::super::backend::{self, DisplayRow, FilterMode};
use super::super::derived::bulkiness_for_row;
use super::super::field::FieldId;
use super::super::filters::SearchFilters;
use super::super::tree::{self, DirCacheExt as _};
use super::{HeapEntry, heap_push_capped, make_display_row, stack_volume_prefix};
use crate::compact::{CompactRecord, DriveCompactIndex};

/// Sorts result indices by record name, using case-folded comparison.
pub(super) fn sort_indices_by_name(indices: &mut [u32], drive: &DriveCompactIndex, desc: bool) {
    let fold = drive.fold;
    indices.sort_unstable_by(|&idx_a, &idx_b| {
        let name_a = drive
            .records
            .get(idx_a as usize)
            .map_or("", |rec| rec.name(&drive.names));
        let name_b = drive
            .records
            .get(idx_b as usize)
            .map_or("", |rec| rec.name(&drive.names));
        let ord = fold.cmp_str(name_a, name_b);
        if desc { ord.reverse() } else { ord }
    });
}

/// Efficient numeric global top-N using lightweight tuples.
#[expect(clippy::too_many_lines, reason = "linear scan + heap + fallback path")]
#[expect(
    clippy::cognitive_complexity,
    reason = "ext fast-path + full-scan path share sort-key + heap-push logic"
)]
/// Core numeric sort + collect logic for all non-path-sorted fields.
///
/// Extracted from `collect_global_top_n` because inlining 300 lines of sort-key
/// extraction + heap management would make the dispatch function unreadable.
#[allow(clippy::single_call_fn)]
pub(super) fn collect_global_top_n_numeric<D: AsRef<DriveCompactIndex>>(
    drives: &[D],
    limit: usize,
    sort_column: FieldId,
    sort_desc: bool,
    filter_mode: FilterMode,
    search_filters: &mut SearchFilters,
) -> Vec<DisplayRow> {
    let has_filters = !search_filters.is_empty() || !matches!(filter_mode, FilterMode::All);

    // For bounded queries use a BinaryHeap capped at `limit` — O(N log K)
    // instead of O(N log N).  For "unlimited" (limit >= 1M or usize::MAX)
    // fall back to collect-sort-truncate since a heap that large is wasteful.
    let use_heap = limit < 1_000_000;
    let mut heap_desc: BinaryHeap<core::cmp::Reverse<HeapEntry>> = if use_heap && sort_desc {
        BinaryHeap::with_capacity(limit.saturating_add(1))
    } else {
        BinaryHeap::new()
    };
    let mut heap_asc: BinaryHeap<HeapEntry> = if use_heap && !sort_desc {
        BinaryHeap::with_capacity(limit.saturating_add(1))
    } else {
        BinaryHeap::new()
    };
    let mut fallback: Vec<(u16, u32, i64)> = Vec::new();

    // Reusable buffer for on-the-fly CaseFold inside filter matching.
    let mut fold_buf: Vec<u8> = Vec::with_capacity(256);

    // ── Per-record processing closure ──────────────────────────────
    // Shared between the full-scan and ext-index fast paths.
    #[expect(clippy::cast_possible_wrap, reason = "file sizes within i64 range")]
    let mut push_record =
        |drive_idx: usize, rec_idx: usize, rec: &CompactRecord, drive: &DriveCompactIndex| {
            let drive_fold = drive.fold;
            let sort_key = match sort_column {
                FieldId::Size => rec.size as i64,
                #[expect(
                    clippy::cast_possible_wrap,
                    reason = "allocated sizes within i64 range"
                )]
                FieldId::SizeOnDisk => rec.allocated as i64,
                FieldId::Created => rec.created,
                FieldId::Accessed => rec.accessed,
                FieldId::Descendants => i64::from(rec.descendants),
                #[expect(
                    clippy::cast_possible_wrap,
                    reason = "allocated values are expected within i64 range"
                )]
                FieldId::TreeAllocated => {
                    if rec.is_directory() {
                        rec.tree_allocated as i64
                    } else {
                        rec.allocated as i64
                    }
                }
                #[expect(
                    clippy::cast_possible_wrap,
                    reason = "scaled bulkiness metric is expected within i64 range"
                )]
                FieldId::Bulkiness => {
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "record index bounded by NTFS limits (<4B records)"
                    )]
                    let ri = rec_idx as u32;
                    let row = DisplayRow::new(
                        ri,
                        drive.letter,
                        String::new(),
                        rec.size,
                        rec.is_directory(),
                        rec.modified,
                        rec.created,
                        rec.accessed,
                        rec.flags,
                        rec.allocated,
                        rec.descendants,
                        rec.treesize,
                        rec.tree_allocated,
                    );
                    bulkiness_for_row(&row) as i64
                }
                FieldId::Extension | FieldId::Type => i64::from(rec.extension_id),
                FieldId::Name => {
                    let name = rec.name(&drive.names);
                    let mut key = [0_u8; 8];
                    for (dst, ch) in key.iter_mut().zip(name.chars()) {
                        let folded = drive_fold.fold_char(ch);
                        #[expect(clippy::cast_possible_truncation, reason = "sort key prefix")]
                        {
                            *dst = folded as u8;
                        }
                    }
                    i64::from_be_bytes(key)
                }
                FieldId::Drive => {
                    let name = rec.name(&drive.names);
                    let mut key = [0_u8; 8];
                    key[0] = drive.letter as u8;
                    for (dst, ch) in key[1..].iter_mut().zip(name.chars()) {
                        let folded = drive_fold.fold_char(ch);
                        #[expect(clippy::cast_possible_truncation, reason = "sort key prefix")]
                        {
                            *dst = folded as u8;
                        }
                    }
                    i64::from_be_bytes(key)
                }
                #[expect(
                    clippy::cast_possible_wrap,
                    reason = "treesize values within i64 range"
                )]
                FieldId::TreeSize => {
                    if rec.is_directory() {
                        rec.treesize as i64
                    } else {
                        rec.size as i64
                    }
                }
                // Modified is the default; Path/PathOnly handled by tree walk above.
                FieldId::Path
                | FieldId::PathOnly
                | FieldId::Modified
                | FieldId::Attributes
                | FieldId::AttributeValue
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
                | FieldId::ParityAttributes => rec.modified,
                #[expect(clippy::cast_possible_wrap, reason = "filename lengths fit i64")]
                FieldId::NameLength => rec.name(&drive.names).chars().count() as i64,
                #[expect(clippy::cast_possible_wrap, reason = "path lengths fit i64")]
                FieldId::PathLength => {
                    // Use name length as a proxy at the sort-key stage
                    // (full path unavailable here).
                    rec.name(&drive.names).chars().count() as i64
                }
            };

            if use_heap {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "drive index and record index bounded by practical limits"
                )]
                let entry = HeapEntry {
                    sort_key,
                    drive_idx: drive_idx as u16,
                    rec_idx: rec_idx as u32,
                };
                if sort_desc {
                    heap_push_capped(&mut heap_desc, core::cmp::Reverse(entry), limit);
                } else {
                    heap_push_capped(&mut heap_asc, entry, limit);
                }
            } else {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "drive index bounded by practical limits"
                )]
                {
                    fallback.push((drive_idx as u16, rec_idx as u32, sort_key));
                }
            }
        };

    // Check if we can use the extension inverted index (ext-only filter,
    // no other constraints).  This reduces iteration from O(N) where
    // N = all records (~25M) to O(K) where K = matching records (~50K).
    let ext_fast_path = search_filters.is_ext_only()
        && matches!(filter_mode, FilterMode::All | FilterMode::FilesOnly);

    if ext_fast_path {
        tracing::debug!(
            extensions = ?search_filters.extensions,
            "ext fast-path ACTIVE — will use ExtensionIndex"
        );
    }

    for (drive_idx, drive_ref) in drives.iter().enumerate() {
        let drive = drive_ref.as_ref();
        // Resolve extension filter IDs for this drive (once, not per record).
        search_filters.resolve_ext_ids_for_drive(drive);

        if ext_fast_path && !search_filters.resolved_ext_ids.is_empty() {
            // ── Fast path: iterate only ext-index candidates ─────
            tracing::debug!(
                drive = %drive.letter,
                resolved_ids = ?search_filters.resolved_ext_ids,
                "ext fast-path: scanning ext_index candidates"
            );
            for &ext_id in &search_filters.resolved_ext_ids.clone() {
                for &rec_idx_u32 in drive.ext_index.get(ext_id) {
                    let rec_idx = rec_idx_u32 as usize;
                    if let Some(rec) = drive.records.get(rec_idx) {
                        if rec.name_len == 0 {
                            continue;
                        }
                        if matches!(filter_mode, FilterMode::FilesOnly) && rec.is_directory() {
                            continue;
                        }
                        push_record(drive_idx, rec_idx, rec, drive);
                    }
                }
            }
        } else {
            if ext_fast_path && !search_filters.extensions.is_empty() {
                let requested_lower = search_filters
                    .extensions
                    .iter()
                    .map(|ext| ext.to_lowercase())
                    .collect::<Vec<_>>();
                let lowercase_only_hits = requested_lower
                    .iter()
                    .filter(|ext| {
                        drive
                            .ext_names
                            .iter()
                            .any(|name| name.as_ref() == ext.as_str())
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let sample_ext_names = drive
                    .ext_names
                    .iter()
                    .filter(|name| !name.is_empty())
                    .take(8)
                    .map(AsRef::as_ref)
                    .collect::<Vec<_>>();
                tracing::debug!(
                    drive = %drive.letter,
                    requested_extensions = ?search_filters.extensions,
                    requested_lowercase = ?requested_lower,
                    resolved_ext_ids = ?search_filters.resolved_ext_ids,
                    lowercase_only_hits = ?lowercase_only_hits,
                    ext_name_count = drive.ext_names.len(),
                    ext_name_sample = ?sample_ext_names,
                    "ext fast-path FALLBACK — no extension IDs resolved, using full scan"
                );
            }
            // ── Full-scan path ───────────────────────────────────
            let drive_fold = drive.fold;
            for (rec_idx, rec) in drive.records.iter().enumerate() {
                if rec.name_len == 0 {
                    continue;
                }
                if has_filters {
                    match filter_mode {
                        FilterMode::FilesOnly if rec.is_directory() => continue,
                        FilterMode::DirsOnly if !rec.is_directory() => continue,
                        FilterMode::All | FilterMode::FilesOnly | FilterMode::DirsOnly => {}
                    }
                    if !search_filters.matches_record(rec, &drive.names, &mut fold_buf, drive_fold)
                    {
                        continue;
                    }
                }
                push_record(drive_idx, rec_idx, rec, drive);
            }
        }
    }

    // Merge into sorted candidates Vec.
    let mut candidates: Vec<(u16, u32, i64)> = if use_heap {
        if sort_desc {
            heap_desc
                .into_iter()
                .map(|rev| (rev.0.drive_idx, rev.0.rec_idx, rev.0.sort_key))
                .collect()
        } else {
            heap_asc
                .into_iter()
                .map(|he| (he.drive_idx, he.rec_idx, he.sort_key))
                .collect()
        }
    } else {
        fallback
    };
    if sort_desc {
        candidates.sort_unstable_by_key(|entry| core::cmp::Reverse(entry.2));
    } else {
        candidates.sort_unstable_by_key(|entry| entry.2);
    }
    candidates.truncate(limit);

    let mut dir_caches: std::collections::HashMap<u16, tree::DirCache> =
        std::collections::HashMap::new();
    let mut rows: Vec<DisplayRow> = candidates
        .iter()
        .filter_map(|&(drive_idx, rec_idx, _)| {
            let drive = drives.get(drive_idx as usize)?.as_ref();
            let rec = drive.records.get(rec_idx as usize)?;
            let name = rec.name(&drive.names);
            if name.is_empty() {
                return None;
            }
            let mut vp_buf = [0_u8; 4];
            let volume_prefix = stack_volume_prefix(&mut vp_buf, drive.letter);
            let cache = dir_caches
                .entry(drive_idx)
                .or_insert_with(|| tree::DirCache::with_capacity(256));
            let path = tree::resolve_path_cached(drive, rec_idx as usize, volume_prefix, cache);
            Some(make_display_row(rec_idx, drive.letter, rec, name, path))
        })
        .collect();

    backend::sort_rows(&mut rows, sort_column, sort_desc, &[]);
    rows
}
