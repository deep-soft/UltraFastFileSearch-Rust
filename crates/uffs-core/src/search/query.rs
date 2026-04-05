//! Search functions for compact-index drives.
//!
//! Per-drive search (trigram, regex, tree) and global top-N collection
//! for match-all queries. Called by `MultiDriveBackend::search()`.

use alloc::collections::BinaryHeap;
use std::sync::LazyLock;

use super::backend::{DisplayRow, FilterMode};
use super::derived::bulkiness_for_row;
use super::field::FieldId;
use super::filters::SearchFilters;
use crate::compact::{CompactRecord, DriveCompactIndex};
use crate::search::tree::{self, DirCacheExt as _};

/// Whether cache profiling is enabled (`UFFS_CACHE_PROFILE` env var).
///
/// Read once at first access to avoid a syscall per search.
static CACHE_PROFILE: LazyLock<bool> =
    LazyLock::new(|| std::env::var_os("UFFS_CACHE_PROFILE").is_some());

/// Entry for the top-N binary heap used by `collect_global_top_n_numeric`.
#[derive(Eq, PartialEq)]
struct HeapEntry {
    /// Sort key used for ordering.
    sort_key: i64,
    /// Drive index.
    drive_idx: u16,
    /// Record index within the drive.
    rec_idx: u32,
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.sort_key
            .cmp(&other.sort_key)
            .then_with(|| self.drive_idx.cmp(&other.drive_idx))
            .then_with(|| self.rec_idx.cmp(&other.rec_idx))
    }
}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Collect the global top-N records across ALL drives for `*` match-all.
///
/// Dispatches to either tree-walk (Path sort) or numeric sort based on
/// `sort_column`. The exhaustive match contributes most of the line count; no
/// logic to extract.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn collect_global_top_n(
    drives: &[DriveCompactIndex],
    limit: usize,
    sort_column: FieldId,
    sort_desc: bool,
    filter_mode: FilterMode,
    search_filters: &mut SearchFilters,
) -> Vec<DisplayRow> {
    match sort_column {
        FieldId::Path | FieldId::PathOnly => {
            // Hierarchical depth-first tree walk for Path sort
            let mut path_results = Vec::with_capacity(limit);
            let mut drive_order: Vec<usize> = (0..drives.len()).collect();
            #[expect(clippy::indexing_slicing, reason = "indices from 0..len, always valid")]
            drive_order.sort_unstable_by(|&idx_a, &idx_b| {
                let ord = drives[idx_a].letter.cmp(&drives[idx_b].letter);
                if sort_desc { ord.reverse() } else { ord }
            });

            for &drive_idx in &drive_order {
                let Some(drive) = drives.get(drive_idx) else {
                    continue;
                };
                let mut vp_buf = [0_u8; 4];
                let volume_prefix = stack_volume_prefix(&mut vp_buf, drive.letter);

                let mut roots: Vec<u32> = drive
                    .records
                    .iter()
                    .enumerate()
                    .filter(|(_, rec)| rec.parent_idx == u32::MAX && rec.name_len > 0)
                    .map(|(idx, _)| {
                        #[expect(
                            clippy::cast_possible_truncation,
                            reason = "record index bounded by NTFS limits"
                        )]
                        {
                            idx as u32
                        }
                    })
                    .collect();

                sort_indices_by_name(&mut roots, drive, sort_desc);

                let mut dir_cache = tree::DirCache::with_capacity(256);
                let mut stack: Vec<u32> = roots.into_iter().rev().collect();
                while let Some(idx) = stack.pop() {
                    if path_results.len() >= limit {
                        return path_results;
                    }

                    let Some(rec) = drive.records.get(idx as usize) else {
                        continue;
                    };
                    let name = rec.name(&drive.names);
                    if name.is_empty() {
                        continue;
                    }

                    let path = tree::resolve_path_cached(
                        drive,
                        idx as usize,
                        volume_prefix,
                        &mut dir_cache,
                    );
                    path_results.push(make_display_row(idx, drive.letter, rec, name, path));

                    let child_slice = drive.children.get(idx as usize);
                    if !child_slice.is_empty() {
                        let mut sorted_children = child_slice.to_vec();
                        sort_indices_by_name(&mut sorted_children, drive, sort_desc);
                        for &child in sorted_children.iter().rev() {
                            stack.push(child);
                        }
                    }
                }
            }

            path_results
        }
        // All other fields (Size, Name, Extension, Created, Modified, etc.)
        // use the generic numeric sort/collect path.
        FieldId::Size
        | FieldId::SizeOnDisk
        | FieldId::Created
        | FieldId::Modified
        | FieldId::Accessed
        | FieldId::Drive
        | FieldId::Descendants
        | FieldId::TreeAllocated
        | FieldId::Bulkiness
        | FieldId::Name
        | FieldId::Extension
        | FieldId::Type
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
        | FieldId::TreeSize
        | FieldId::Integrity
        | FieldId::NoScrub
        | FieldId::DirectoryFlag
        | FieldId::RecallOnOpen
        | FieldId::RecallOnDataAccess
        | FieldId::ParityAttributes
        | FieldId::NameLength
        | FieldId::PathLength => collect_global_top_n_numeric(
            drives,
            limit,
            sort_column,
            sort_desc,
            filter_mode,
            search_filters,
        ),
    }
}

/// Sort a slice of compact indices by their name (case-insensitive).
///
/// Uses `CaseFold::cmp_str` for zero-alloc, per-codepoint fold comparison.
fn sort_indices_by_name(indices: &mut [u32], drive: &DriveCompactIndex, desc: bool) {
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
fn collect_global_top_n_numeric(
    drives: &[DriveCompactIndex],
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
                | FieldId::TreeSize
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

    for (drive_idx, drive) in drives.iter().enumerate() {
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
            let drive = drives.get(drive_idx as usize)?;
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

    super::backend::sort_rows(&mut rows, sort_column, sort_desc, &[]);
    rows
}

/// Search a single drive using regex matching on filenames.
#[must_use]
pub fn search_compact_drive_regex(
    drive: &DriveCompactIndex,
    compiled_re: &regex::Regex,
    limit: usize,
) -> Vec<DisplayRow> {
    let mut vp_buf = [0_u8; 4];
    let volume_prefix = stack_volume_prefix(&mut vp_buf, drive.letter);
    let profile = *CACHE_PROFILE;

    let t_match = std::time::Instant::now();
    #[allow(clippy::cast_possible_truncation)]
    let match_indices: Vec<u32> = drive
        .records
        .iter()
        .enumerate()
        .filter(|(_, rec)| {
            let name = rec.name(&drive.names);
            !name.is_empty() && compiled_re.is_match(name)
        })
        .take(limit)
        .map(|(idx, _)| idx as u32)
        .collect();
    let match_ms = t_match.elapsed().as_millis();
    let match_count = match_indices.len();

    let t_resolve = std::time::Instant::now();
    let rows = indices_to_rows(drive, &match_indices, volume_prefix);
    let resolve_ms = t_resolve.elapsed().as_millis();

    if profile {
        tracing::debug!(
            target: "cache_profile",
            drive = %drive.letter,
            regex_match_ms = %match_ms,
            match_count,
            scanned = drive.records.len(),
            resolve_ms = %resolve_ms,
            "search_regex"
        );
    }

    rows
}

/// Extract the best trigram lookup needle from a search pattern.
///
/// For OR-queries (`|`), returns empty (no trigram lookup).  For globs,
/// extracts the longest literal segment.  For plain substrings, returns
/// the needle as-is.
#[expect(
    clippy::single_call_fn,
    reason = "extracted from search_compact_drive to satisfy too_many_lines lint"
)]
fn extract_trigram_needle(needle: &str, is_glob: bool, is_or: bool) -> String {
    if is_or {
        String::new()
    } else if is_glob {
        needle
            .split(['*', '?'])
            .max_by_key(|seg| seg.len())
            .unwrap_or("")
            .to_owned()
    } else {
        needle.to_owned()
    }
}

/// Emit search timing via `tracing::debug!` for a single-drive search.
#[expect(
    clippy::single_call_fn,
    reason = "extracted from search_compact_drive to satisfy too_many_lines lint"
)]
fn log_search_profile(
    letter: char,
    tri_ms: u128,
    match_ms: u128,
    resolve_ms: u128,
    match_count: usize,
    tri_count: usize,
    total_records: usize,
) {
    let scan_mode = if tri_count > 0 { "trigram" } else { "full" };
    let scan_count = if tri_count > 0 {
        tri_count
    } else {
        total_records
    };
    tracing::debug!(
        target: "cache_profile",
        drive = %letter,
        tri_ms = %tri_ms,
        match_ms = %match_ms,
        match_count,
        scan_mode,
        scan_count,
        resolve_ms = %resolve_ms,
        "search_compact"
    );
}

/// Collect record indices that match the name predicate, either from
/// trigram candidates or a full scan, up to `limit` results.
#[expect(
    clippy::single_call_fn,
    reason = "extracted from search_compact_drive to satisfy too_many_lines lint"
)]
fn collect_match_indices(
    drive: &DriveCompactIndex,
    candidates: Option<Vec<u32>>,
    limit: usize,
    lower_buf: &mut Vec<u8>,
    matches: &dyn Fn(&str, &mut Vec<u8>) -> bool,
) -> Vec<u32> {
    match candidates {
        None => {
            let mut out = Vec::new();
            for (idx, rec) in drive.records.iter().enumerate() {
                if out.len() >= limit {
                    break;
                }
                let name = rec.name(&drive.names);
                if matches(name, lower_buf) {
                    #[allow(clippy::cast_possible_truncation)]
                    out.push(idx as u32);
                }
            }
            out
        }
        Some(candidate_indices) => {
            let mut out = Vec::with_capacity(candidate_indices.len().min(limit));
            for &idx in &candidate_indices {
                if out.len() >= limit {
                    break;
                }
                let Some(rec) = drive.records.get(idx as usize) else {
                    continue;
                };
                let name = rec.name(&drive.names);
                if matches(name, lower_buf) {
                    out.push(idx);
                }
            }
            out
        }
    }
}

/// Search a single drive's compact index (trigram + glob/substring).
#[must_use]
pub fn search_compact_drive(
    drive: &DriveCompactIndex,
    needle: &str,
    limit: usize,
    case_sensitive: bool,
    whole_word: bool,
    match_path: bool,
) -> Vec<DisplayRow> {
    if needle.is_empty() {
        return Vec::new();
    }

    let mut vp_buf = [0_u8; 4];
    let volume_prefix = stack_volume_prefix(&mut vp_buf, drive.letter);
    let is_glob = needle.contains('*') || needle.contains('?');
    let is_or = needle.contains('|');

    // $UpCase case folding engine — zero-alloc comparisons, buffer-reuse fold.
    let fold = drive.fold;

    // Pre-fold the needle for case-insensitive matching.
    let mut needle_fold_buf: Vec<u8> = Vec::with_capacity(needle.len());
    let needle_folded = if case_sensitive {
        needle.to_owned()
    } else {
        fold.fold_into(needle, &mut needle_fold_buf).to_owned()
    };

    // Pre-build a SIMD-accelerated substring finder for simple queries.
    // For 1–2 byte needles this is dramatically faster than `str::contains`
    // (memchr uses SSE2/AVX2/NEON vectorised search).
    let simple_substring = !is_glob && !is_or && !whole_word && !case_sensitive;
    let finder = simple_substring.then(|| memchr::memmem::Finder::new(needle_folded.as_bytes()));
    // Reusable buffer for on-the-fly CaseFold (avoids per-record heap alloc).
    let mut fold_buf: Vec<u8> = Vec::with_capacity(256);
    let matches = |name: &str, buf: &mut Vec<u8>| -> bool {
        if name.is_empty() || name == "." {
            return false;
        }
        if whole_word {
            if case_sensitive {
                if is_glob || is_or {
                    tree::name_matches(name, needle)
                } else {
                    name == needle
                }
            } else {
                let folded = fold.fold_into(name, buf);
                if is_glob || is_or {
                    tree::name_matches(folded, &needle_folded)
                } else {
                    folded == needle_folded
                }
            }
        } else if let Some(fnd) = &finder {
            buf.clear();
            let folded = fold.fold_into(name, buf);
            fnd.find(folded.as_bytes()).is_some()
        } else if case_sensitive {
            tree::name_matches(name, needle)
        } else {
            let folded = fold.fold_into(name, buf);
            tree::name_matches(folded, &needle_folded)
        }
    };

    let trigram_needle = extract_trigram_needle(needle, is_glob, is_or);
    let profile = *CACHE_PROFILE;

    let t_tri = std::time::Instant::now();
    let candidates = if !case_sensitive && trigram_needle.len() >= 3 {
        drive.trigram.search(&trigram_needle, fold)
    } else {
        None
    };
    let tri_ms = t_tri.elapsed().as_millis();
    let tri_count = candidates.as_ref().map_or(0, Vec::len);

    let t_match = std::time::Instant::now();
    let mut match_indices =
        collect_match_indices(drive, candidates, limit, &mut fold_buf, &matches);
    let match_ms = t_match.elapsed().as_millis();
    let match_count = match_indices.len();

    // ── path mode: expand matching directories to include all descendants ──
    if match_path && !match_indices.is_empty() {
        expand_directory_descendants(drive, &mut match_indices);
    }

    let t_resolve = std::time::Instant::now();
    let rows = indices_to_rows(drive, &match_indices, volume_prefix);
    let resolve_ms = t_resolve.elapsed().as_millis();

    if profile {
        log_search_profile(
            drive.letter,
            tri_ms,
            match_ms,
            resolve_ms,
            match_count,
            tri_count,
            drive.records.len(),
        );
    }

    rows
}

/// DFS expansion: for every matching directory, collect all descendant indices.
///
/// Extracted from `search_compact_drive` to stay under the `too_many_lines`
/// lint limit (the caller was 103/100 before extraction).
#[expect(
    clippy::single_call_fn,
    reason = "factored out to keep search_compact_drive under too_many_lines"
)]
fn expand_directory_descendants(drive: &DriveCompactIndex, indices: &mut Vec<u32>) {
    let mut extra: Vec<u32> = Vec::new();
    let mut stack: Vec<u32> = Vec::new();
    for &idx in indices.iter() {
        if let Some(rec) = drive.records.get(idx as usize)
            && rec.is_directory()
        {
            stack.push(idx);
            while let Some(dir_idx) = stack.pop() {
                for &child_idx in drive.children.get(dir_idx as usize) {
                    extra.push(child_idx);
                    if let Some(child_rec) = drive.records.get(child_idx as usize)
                        && child_rec.is_directory()
                    {
                        stack.push(child_idx);
                    }
                }
            }
        }
    }
    if !extra.is_empty() {
        indices.extend(extra);
        indices.sort_unstable();
        indices.dedup();
    }
}

/// Search a single drive using tree-based path traversal.
#[must_use]
pub fn search_compact_drive_tree(
    drive: &DriveCompactIndex,
    pattern_lower: &str,
    limit: usize,
) -> Vec<DisplayRow> {
    let mut vp_buf = [0_u8; 4];
    let volume_prefix = stack_volume_prefix(&mut vp_buf, drive.letter);
    let profile = *CACHE_PROFILE;

    let t_tree = std::time::Instant::now();
    let match_indices = tree::tree_search(drive, pattern_lower, limit);
    let tree_ms = t_tree.elapsed().as_millis();
    let match_count = match_indices.len();

    let t_resolve = std::time::Instant::now();
    let mut dir_cache = tree::DirCache::with_capacity(256);
    let rows: Vec<DisplayRow> = match_indices
        .iter()
        .filter_map(|&record_idx| {
            let rec = drive.records.get(record_idx as usize)?;
            let name = rec.name(&drive.names);
            if name.is_empty() {
                return None;
            }
            let path = tree::resolve_path_cached(
                drive,
                record_idx as usize,
                volume_prefix,
                &mut dir_cache,
            );
            Some(make_display_row(record_idx, drive.letter, rec, name, path))
        })
        .collect();
    let resolve_ms = t_resolve.elapsed().as_millis();

    if profile {
        tracing::debug!(
            target: "cache_profile",
            drive = %drive.letter,
            tree_ms = %tree_ms,
            match_count,
            resolve_ms = %resolve_ms,
            "search_tree"
        );
    }

    rows
}

// ── Shared helpers ──────────────────────────────────────────────────────────

/// Build a `DisplayRow` from a compact record.
///
/// ADS entries (name contains `:`) are always rendered as file-like rows
/// even when the underlying MFT record is a directory.  The raw `flags`
/// field preserves the NTFS ground truth — only the `is_directory`
/// display hint is adjusted.
fn make_display_row(
    record_index: u32,
    drive_letter: char,
    rec: &CompactRecord,
    name: &str,
    path: String,
) -> DisplayRow {
    // ADS entries on directories must not render as directories
    // (no trailing backslash, name shown, stream size used).
    let is_ads = name.contains(':');
    DisplayRow::new(
        record_index,
        drive_letter,
        path,
        rec.size,
        rec.is_directory() && !is_ads,
        rec.modified,
        rec.created,
        rec.accessed,
        rec.flags,
        rec.allocated,
        rec.descendants,
        rec.treesize,
        rec.tree_allocated,
    )
}

/// Build a `"X:\\"` volume prefix on the stack.
///
/// Returns a 3-byte `&str` without heap allocation.  Uses safe
/// `from_utf8` with a fallback — the bytes are always valid ASCII.
#[inline]
fn stack_volume_prefix(buf: &mut [u8; 4], letter: char) -> &str {
    buf[0] = letter.to_ascii_uppercase() as u8;
    buf[1] = b':';
    buf[2] = b'\\';
    core::str::from_utf8(buf.get(..3).unwrap_or(b"?:\\")).unwrap_or("?:\\")
}

/// Push an element into a `BinaryHeap` capped at `limit`.
///
/// If the heap is below capacity, always push.  If at capacity, only push
/// if the new element would displace the current top (and pop the old top).
/// This keeps the heap at most `limit` entries.
#[inline]
fn heap_push_capped<T: Ord>(heap: &mut BinaryHeap<T>, entry: T, limit: usize) {
    if heap.len() < limit {
        heap.push(entry);
    } else if let Some(top) = heap.peek()
        && entry < *top
    {
        // New entry is "better" — displace the worst.
        // (For Reverse<T> this means the underlying T is *larger*.)
        drop(heap.pop());
        heap.push(entry);
    }
}

/// Convert a list of record indices into `DisplayRow`s with resolved paths.
fn indices_to_rows(
    drive: &DriveCompactIndex,
    indices: &[u32],
    volume_prefix: &str,
) -> Vec<DisplayRow> {
    let mut dir_cache = tree::DirCache::with_capacity(256);
    indices
        .iter()
        .filter_map(|&record_idx| {
            let rec = drive.records.get(record_idx as usize)?;
            let name = rec.name(&drive.names);
            if name.is_empty() {
                return None;
            }
            let path = tree::resolve_path_cached(
                drive,
                record_idx as usize,
                volume_prefix,
                &mut dir_cache,
            );
            Some(make_display_row(record_idx, drive.letter, rec, name, path))
        })
        .collect()
}

// ════════════════════════════════════════════════════════════════════════
// REGRESSION TESTS — End-to-End Compact Search Parity
//
// These tests build a synthetic MftIndex → compact index → search and
// verify DisplayRow correctness. They protect against field mapping,
// filter wiring, and system metafile handling regressions.
// See `docs/architecture/2026_03_30_04_12_SEARCH_PIPELINE_REGRESSION_ANALYSIS.
// md` ════════════════════════════════════════════════════════════════════════
#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
