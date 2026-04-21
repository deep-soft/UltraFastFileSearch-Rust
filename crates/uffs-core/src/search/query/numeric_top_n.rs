// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Internal numeric top-N collection and sort helpers.

use alloc::collections::BinaryHeap;

use rayon::prelude::*;

use super::super::backend::{self, DisplayRow, FilterMode, PhaseTimings};
use super::super::derived::bulkiness_for_record;
use super::super::field::FieldId;
use super::super::filters::SearchFilters;
use super::super::tree::{self, DirCacheExt as _};
use super::{HeapEntry, heap_push_capped, make_display_row, stack_volume_prefix};
use crate::compact::{CompactRecord, DriveCompactIndex};

/// Target chunk size for parallel path resolution inside
/// [`collect_global_top_n_numeric`].
///
/// Chosen so each chunk runs for ~1.5 ms of CPU work, well above
/// rayon's per-task dispatch floor (~100 ns to 1 μs).  Smaller chunks
/// would waste time on scheduler overhead; larger chunks would
/// underutilise worker threads on smaller queries.  Measured at
/// ~370 ns per candidate a 4 K chunk runs in ~1.5 ms.
const RESOLVE_CHUNK_SIZE: usize = 4096;

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
pub(super) fn collect_global_top_n_numeric<D: AsRef<DriveCompactIndex> + Sync>(
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
        } else if ext_fast_path && !search_filters.extensions.is_empty() {
            // ── Fast-path short-circuit ──────────────────────────
            //
            // `ext_fast_path` means `is_ext_only()` returned true, so
            // the ONLY filters active are `extensions` plus the cheap
            // inline predicates (`hide_system` / `hide_ads`).  An
            // empty `resolved_ext_ids` means none of the requested
            // extensions are interned on this drive — i.e. zero
            // records can possibly match.  Skip the drive entirely
            // instead of falling through to an O(N) full scan that
            // would uselessly iterate every record (`matches_record`
            // would still reject them at the extension check).
            //
            // Regression root cause (2026-04-21): a query like
            // `*.dbt --hide-system --hide-ads --drive C` on a drive
            // with zero `.dbt` files took **543 ms** (3.5 M record
            // scan) AND returned a spurious match for a directory
            // literally named `dbt` via the buggy
            // `name.rsplit('.').next()` fallback in `matches_record`.
            // The short-circuit here combined with the fallback fix
            // in `filters/mod.rs` closes both issues: the query now
            // returns empty in < 1 ms.  See the `C:ext_rare` row in
            // `@/Users/rnio/Private/Github/UltraFastFileSearch/LOG/Output_cache_new:785`.
            tracing::debug!(
                drive = %drive.letter,
                requested_extensions = ?search_filters.extensions,
                ext_name_count = drive.ext_names.len(),
                "ext fast-path SHORT-CIRCUIT — no matching extension IDs on this drive"
            );
        } else {
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

    // ── Path-resolve phase (parallel): per-candidate parent-chain
    //    walk + `DisplayRow` materialisation.  Candidates are in
    //    MFT order (from the locality re-sort above), so adjacent
    //    items in each chunk share parents and hit the per-chunk
    //    `DirCache` warm.
    //
    // Parallel strategy: rayon `par_chunks` over the candidate
    // slice with one `DirCache` map per rayon worker.  Each chunk
    // accumulates its own deep-profile counters, then everything
    // is reduced into workspace totals so the outer [`PhaseTimings`]
    // keeps the pre-parallel shape.  Chunk size 4 K was chosen to
    // balance scheduler overhead (~1 μs per task) against cache
    // warm-up cost: at ~370 ns per candidate a 4 K chunk runs in
    // ~1.5 ms, >10× the dispatch floor.
    //
    // Cache-hit trade-off: per-thread caches start cold so the
    // first ~20 candidates in each chunk are misses even if they
    // share parents with other chunks.  The MFT-locality re-sort
    // above keeps each chunk's parent working-set small (~1–2 K
    // unique parents in a 4 K chunk), so warm-up pays back within
    // the chunk.  Net effect measured on a 168 K-candidate C:
    // drive: `resolve_fn` drops from ~63 ms sequential to ~17 ms
    // with 8 workers — the 4× speedup reflects the 8-core Windows
    // host minus cache warm-up overhead.
    //
    // Deep profile: split the cost into the `resolve_path_cached`
    // fn itself vs. the `make_display_row` + `Vec::push` work so
    // we can tell whether path-walking or row-building dominates.
    // Counters are summed across chunks; each chunk's contribution
    // is measured on its worker thread.  Rayon reduces the counters
    // alongside the per-chunk row vectors.
    let t_path_resolve = std::time::Instant::now();

    let (mut rows, path_resolve_fn_ns, path_build_row_ns, path_candidates, path_cache_entries) =
        candidates
            .par_chunks(RESOLVE_CHUNK_SIZE)
            .map(|chunk| {
                let mut local_caches: std::collections::HashMap<u16, tree::DirCache> =
                    std::collections::HashMap::new();
                let mut local_rows: Vec<DisplayRow> = Vec::with_capacity(chunk.len());
                let mut local_resolve_ns: u128 = 0;
                let mut local_build_ns: u128 = 0;
                let mut local_candidates: u64 = 0;

                for &(drive_idx, rec_idx, _) in chunk {
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
                    let cache = local_caches
                        .entry(drive_idx)
                        .or_insert_with(|| tree::DirCache::with_capacity(256));
                    let t_resolve = std::time::Instant::now();
                    let path =
                        tree::resolve_path_cached(drive, rec_idx as usize, volume_prefix, cache);
                    local_resolve_ns += t_resolve.elapsed().as_nanos();
                    let t_build = std::time::Instant::now();
                    local_rows.push(make_display_row(rec_idx, drive.letter, rec, name, path));
                    local_build_ns += t_build.elapsed().as_nanos();
                    local_candidates += 1;
                }

                // Sum DirCache entries across this chunk's per-drive
                // caches *before* dropping them.  The total across
                // chunks represents the steady-state miss count
                // observed by the parallel phase — it will exceed
                // the old sequential figure because each chunk
                // pays its own cold-cache warm-up.
                let local_cache_entries: u64 =
                    local_caches.values().map(|cache| cache.len() as u64).sum();
                (
                    local_rows,
                    local_resolve_ns,
                    local_build_ns,
                    local_candidates,
                    local_cache_entries,
                )
            })
            .reduce(
                || (Vec::new(), 0_u128, 0_u128, 0_u64, 0_u64),
                |mut acc, chunk| {
                    let (
                        mut chunk_rows,
                        chunk_resolve_ns,
                        chunk_build_ns,
                        chunk_cands,
                        chunk_entries,
                    ) = chunk;
                    acc.0.append(&mut chunk_rows);
                    acc.1 += chunk_resolve_ns;
                    acc.2 += chunk_build_ns;
                    acc.3 += chunk_cands;
                    acc.4 += chunk_entries;
                    acc
                },
            );

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
