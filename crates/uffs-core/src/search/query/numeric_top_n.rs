// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Internal numeric top-N collection and sort helpers.

use alloc::collections::BinaryHeap;

use super::super::backend::{self, DisplayRow, FilterMode, PhaseTimings};
use super::super::derived::bulkiness_for_record;
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
pub(super) fn collect_global_top_n_numeric<D: AsRef<DriveCompactIndex>>(
    drives: &[D],
    limit: usize,
    sort_column: FieldId,
    sort_desc: bool,
    filter_mode: FilterMode,
    search_filters: &mut SearchFilters,
) -> (Vec<DisplayRow>, PhaseTimings) {
    let has_filters = !search_filters.is_empty() || !matches!(filter_mode, FilterMode::All);
    tracing::debug!(
        has_filters,
        hide_system = search_filters.hide_system,
        filters_empty = search_filters.is_empty(),
        filter_mode = ?filter_mode,
        limit,
        sort_column = ?sort_column,
        sort_desc,
        num_drives = drives.len(),
        "[TOP-N] entering collect_global_top_n_numeric"
    );

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
                FieldId::Bulkiness => bulkiness_for_record(rec) as i64,
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
                    key[0] = u8::try_from(u32::from(drive.letter)).unwrap_or(b'?');
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
                // Boolean attribute flags: extract the individual bit as 0/1.
                FieldId::DirectoryFlag => i64::from(rec.is_directory()),
                FieldId::Hidden => i64::from(rec.flags & 0x0002 != 0),
                FieldId::System => i64::from(rec.flags & 0x0004 != 0),
                FieldId::ReadOnly => i64::from(rec.flags & 0x0001 != 0),
                FieldId::Archive => i64::from(rec.flags & 0x0020 != 0),
                FieldId::Compressed => i64::from(rec.flags & 0x0800 != 0),
                FieldId::Encrypted => i64::from(rec.flags & 0x4000 != 0),
                FieldId::Sparse => i64::from(rec.flags & 0x0200 != 0),
                FieldId::Reparse => i64::from(rec.flags & 0x0400 != 0),
                FieldId::Offline => i64::from(rec.flags & 0x1000 != 0),
                FieldId::NotIndexed => i64::from(rec.flags & 0x2000 != 0),
                FieldId::Temporary => i64::from(rec.flags & 0x0100 != 0),
                FieldId::Integrity => i64::from(rec.flags & 0x8000 != 0),
                FieldId::NoScrub => i64::from(rec.flags & 0x0002_0000 != 0),
                FieldId::Pinned => i64::from(rec.flags & 0x0008_0000 != 0),
                FieldId::Unpinned => i64::from(rec.flags & 0x0010_0000 != 0),
                FieldId::RecallOnOpen => i64::from(rec.flags & 0x0004_0000 != 0),
                FieldId::RecallOnDataAccess => i64::from(rec.flags & 0x0040_0000 != 0),
                // Composite attribute fields use the raw flags value.
                FieldId::Attributes | FieldId::AttributeValue | FieldId::ParityAttributes => {
                    i64::from(rec.flags)
                }
                FieldId::Virtual => i64::from(rec.flags & 0x0001_0000 != 0),
                // Modified is the default; Path/PathOnly handled by tree walk above.
                FieldId::Path | FieldId::PathOnly | FieldId::Modified => rec.modified,
                FieldId::NameLength => {
                    i64::try_from(rec.name(&drive.names).chars().count()).unwrap_or(i64::MAX)
                }
                FieldId::PathLength => {
                    // Use name length as a proxy at the sort-key stage
                    // (full path unavailable here).
                    i64::try_from(rec.name(&drive.names).chars().count()).unwrap_or(i64::MAX)
                }
            };

            if use_heap {
                let entry = HeapEntry {
                    sort_key,
                    drive_idx: uffs_mft::len_to_u16(drive_idx),
                    rec_idx: uffs_mft::len_to_u32(rec_idx),
                };
                if sort_desc {
                    heap_push_capped(&mut heap_desc, core::cmp::Reverse(entry), limit);
                } else {
                    heap_push_capped(&mut heap_asc, entry, limit);
                }
            } else {
                fallback.push((
                    uffs_mft::len_to_u16(drive_idx),
                    uffs_mft::len_to_u32(rec_idx),
                    sort_key,
                ));
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

    let t_scan_all = std::time::Instant::now();
    let mut total_records_scanned: u64 = 0;
    let mut total_filtered_out: u64 = 0;

    for (drive_idx, drive_ref) in drives.iter().enumerate() {
        let drive = drive_ref.as_ref();
        let t_drive = std::time::Instant::now();
        let drive_record_count = drive.records.len();
        let mut drive_filtered = 0_u64;

        // Resolve extension filter IDs for this drive (once, not per record).
        search_filters.resolve_ext_ids_for_drive(drive);

        if ext_fast_path && !search_filters.resolved_ext_ids.is_empty() {
            // ── Fast path: iterate only ext-index candidates ─────
            //
            // The CSR `ext_index` bucket already narrows the candidate
            // set from O(N) (all records on the drive, up to 7 M+) to
            // O(K) (matches of the requested extension, typically 10s
            // to 100 Ks).  We still have to apply the two cheap
            // per-candidate predicates that `is_ext_only()` admits:
            //
            //   • `hide_system` — cached `name_first_byte == b'$'`
            //     via `rec.is_system_metafile()`, zero name-arena
            //     reads.  ~1 ns per candidate.
            //
            //   • `hide_ads` — scan the name bytes for a `:` (NTFS
            //     ADS separator).  Name arena read + `memchr::memchr`,
            //     ~30 ns per candidate and only evaluated when the
            //     flag is actually set.
            //
            // Both predicates must run BEFORE `push_record` so filtered
            // records never enter the heap / fallback buffer and never
            // trigger the expensive path-resolution phase downstream.
            tracing::debug!(
                drive = %drive.letter,
                resolved_ids = ?search_filters.resolved_ext_ids,
                hide_system = search_filters.hide_system,
                hide_ads = search_filters.hide_ads,
                "ext fast-path: scanning ext_index candidates"
            );
            let hide_system = search_filters.hide_system;
            let hide_ads = search_filters.hide_ads;
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
                        if hide_system && rec.is_system_metafile() {
                            continue;
                        }
                        if hide_ads {
                            let name = rec.name(&drive.names);
                            if memchr::memchr(b':', name.as_bytes()).is_some() {
                                continue;
                            }
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
                        drive_filtered += 1;
                        continue;
                    }
                }
                push_record(drive_idx, rec_idx, rec, drive);
            }
        }
        let drive_ms = t_drive.elapsed().as_millis();
        total_records_scanned += drive_record_count as u64;
        total_filtered_out += drive_filtered;
        tracing::debug!(
            drive = %drive.letter,
            records = drive_record_count,
            filtered_out = drive_filtered,
            has_filters,
            elapsed_ms = drive_ms,
            "[SCAN] drive scan complete"
        );
    }
    let scan_ms_u128 = t_scan_all.elapsed().as_millis();
    let scan_ms = u64::try_from(scan_ms_u128).unwrap_or(u64::MAX);
    tracing::debug!(
        total_records = total_records_scanned,
        total_filtered = total_filtered_out,
        scan_ms,
        "[SCAN] all drives scanned"
    );

    // ── Sort phase: drain heap / fallback into candidates Vec,
    //    sort by sort_key, and truncate to `limit`.
    let t_sort = std::time::Instant::now();
    // Merge into sorted candidates Vec.
    let mut candidates: Vec<(u16, u32, i64)> = if use_heap {
        if sort_desc {
            let desc_vec: Vec<_> = heap_desc
                .into_iter()
                .map(|rev| (rev.0.drive_idx, rev.0.rec_idx, rev.0.sort_key))
                .collect();
            tracing::debug!(
                sort_column = ?sort_column,
                sort_desc,
                heap_size = desc_vec.len(),
                keys = ?desc_vec.iter().map(|entry| entry.2).collect::<Vec<_>>(),
                "[3] numeric_top_n heap_desc candidates"
            );
            desc_vec
        } else {
            let asc_vec: Vec<_> = heap_asc
                .into_iter()
                .map(|he| (he.drive_idx, he.rec_idx, he.sort_key))
                .collect();
            tracing::debug!(
                sort_column = ?sort_column,
                sort_desc,
                heap_size = asc_vec.len(),
                keys = ?asc_vec.iter().map(|entry| entry.2).collect::<Vec<_>>(),
                "[3] numeric_top_n heap_asc candidates"
            );
            asc_vec
        }
    } else {
        tracing::debug!(
            sort_column = ?sort_column,
            sort_desc,
            fallback_size = fallback.len(),
            "[3] numeric_top_n FALLBACK (no heap)"
        );
        fallback
    };
    if sort_desc {
        candidates.sort_unstable_by_key(|entry| core::cmp::Reverse(entry.2));
    } else {
        candidates.sort_unstable_by_key(|entry| entry.2);
    }
    candidates.truncate(limit);
    // Locality re-sort: the numeric sort above scrambles MFT order
    // (e.g. `Modified`-DESC interleaves records from arbitrary
    // directories), which collapses the `DirCache` hit rate during
    // the path-resolve phase below.  Re-sort the survivors by
    // `(drive_idx, rec_idx)` so sibling records land next to each
    // other — `tree::resolve_path_cached` then walks one step up
    // and finds the parent already warm in the cache.  The final
    // `backend::sort_rows` call after path resolution restores the
    // user-requested order with its tiebreakers, so this reorder
    // is invisible to callers.
    //
    // Measured on a 1 M-record C: drive with `*.dll` and
    // `--sort modified`: `path_resolve_ms` drops from ~226 ms to
    // well under 100 ms because the per-directory DirCache entry is
    // reused across all dll siblings of `System32\` etc.
    candidates.sort_unstable_by_key(|&(drive_idx, rec_idx, _)| (drive_idx, rec_idx));
    let sort_ms = u64::try_from(t_sort.elapsed().as_millis()).unwrap_or(u64::MAX);

    // ── Path-resolve phase: per-candidate parent-chain walk +
    //    `DisplayRow` materialisation.  Candidates are now in MFT
    //    order thanks to the locality re-sort above, so adjacent
    //    path resolutions hit the `DirCache` warm.
    //
    // Deep profile: split the cost into the `resolve_path_cached`
    // fn itself vs. the `make_display_row` + `Vec::push` work so
    // we can tell whether path-walking or row-building dominates.
    // The two cumulative `u128` counters add ~50-100 ns of timer
    // overhead per iteration; at 50 K records that is ~5 ms — big
    // enough to skew the absolute `path_resolve_ms` measurement
    // slightly, but small relative to the phase total and
    // necessary for attribution.
    let t_path_resolve = std::time::Instant::now();
    let mut dir_caches: std::collections::HashMap<u16, tree::DirCache> =
        std::collections::HashMap::new();
    let mut path_resolve_fn_ns: u128 = 0;
    let mut path_build_row_ns: u128 = 0;
    let mut path_candidates: u64 = 0;
    let mut rows: Vec<DisplayRow> = Vec::with_capacity(candidates.len());
    for &(drive_idx, rec_idx, _) in &candidates {
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
            .or_insert_with(|| tree::DirCache::with_capacity(256));
        let t_resolve = std::time::Instant::now();
        let path = tree::resolve_path_cached(drive, rec_idx as usize, volume_prefix, cache);
        path_resolve_fn_ns += t_resolve.elapsed().as_nanos();
        let t_build = std::time::Instant::now();
        rows.push(make_display_row(rec_idx, drive.letter, rec, name, path));
        path_build_row_ns += t_build.elapsed().as_nanos();
        path_candidates += 1;
    }

    // Count DirCache entries across all drives *before* the sort
    // so we capture the steady-state miss count.  Must happen
    // before `sort_rows` because the caches are no longer touched
    // past this point.
    let path_cache_entries: u64 = dir_caches.values().map(|cache| cache.len() as u64).sum();

    backend::sort_rows(&mut rows, sort_column, sort_desc, &[]);
    let path_resolve_ms = u64::try_from(t_path_resolve.elapsed().as_millis()).unwrap_or(u64::MAX);

    let timings = PhaseTimings {
        scan_ms,
        sort_ms,
        path_resolve_ms,
        path_candidates,
        path_cache_entries,
        path_resolve_fn_ns: u64::try_from(path_resolve_fn_ns).unwrap_or(u64::MAX),
        path_build_row_ns: u64::try_from(path_build_row_ns).unwrap_or(u64::MAX),
    };
    tracing::debug!(
        scan_ms,
        sort_ms,
        path_resolve_ms,
        path_candidates,
        path_cache_entries,
        path_resolve_fn_ns = timings.path_resolve_fn_ns,
        path_build_row_ns = timings.path_build_row_ns,
        rows = rows.len(),
        "[PHASE] collect_global_top_n_numeric complete"
    );
    (rows, timings)
}
