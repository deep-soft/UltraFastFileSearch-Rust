//! Search functions for compact-index drives.
//!
//! Per-drive search (trigram, regex, tree) and global top-N collection
//! for match-all queries. Called by `MultiDriveBackend::search()`.

use super::backend::{DisplayRow, FilterMode, SortColumn};
use super::filters::SearchFilters;
use crate::compact::DriveCompactIndex;
use crate::search::tree;

/// Collect the global top-N records across ALL drives for `*` match-all.
#[must_use]
pub fn collect_global_top_n(
    drives: &[DriveCompactIndex],
    limit: usize,
    sort_column: SortColumn,
    sort_desc: bool,
    filter_mode: FilterMode,
    search_filters: &SearchFilters,
) -> Vec<DisplayRow> {
    match sort_column {
        SortColumn::Size
        | SortColumn::SizeOnDisk
        | SortColumn::Created
        | SortColumn::Modified
        | SortColumn::Accessed
        | SortColumn::Drive
        | SortColumn::Descendants => collect_global_top_n_numeric(
            drives,
            limit,
            sort_column,
            sort_desc,
            filter_mode,
            search_filters,
        ),
        SortColumn::Path => {
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
                let volume_prefix = format!("{}:\\", drive.letter);

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
                        &volume_prefix,
                        &mut dir_cache,
                    );
                    path_results.push(make_display_row(drive.letter, rec, name, path));

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
        SortColumn::Name | SortColumn::Extension | SortColumn::Type => {
            collect_global_top_n_numeric(
                drives,
                limit,
                sort_column,
                sort_desc,
                filter_mode,
                search_filters,
            )
        }
    }
}

/// Sort a slice of compact indices by their name in the names blob.
fn sort_indices_by_name(indices: &mut [u32], drive: &DriveCompactIndex, desc: bool) {
    indices.sort_unstable_by(|&idx_a, &idx_b| {
        let name_a = drive
            .records
            .get(idx_a as usize)
            .map_or("", |rec| rec.name(&drive.names_lower));
        let name_b = drive
            .records
            .get(idx_b as usize)
            .map_or("", |rec| rec.name(&drive.names_lower));
        let ord = name_a.cmp(name_b);
        if desc { ord.reverse() } else { ord }
    });
}

/// Efficient numeric global top-N using lightweight tuples.
#[expect(
    clippy::cast_possible_truncation,
    reason = "drive index and record index bounded by practical limits"
)]
fn collect_global_top_n_numeric(
    drives: &[DriveCompactIndex],
    limit: usize,
    sort_column: SortColumn,
    sort_desc: bool,
    filter_mode: FilterMode,
    search_filters: &SearchFilters,
) -> Vec<DisplayRow> {
    let has_filters = !search_filters.is_empty() || !matches!(filter_mode, FilterMode::All);
    let mut candidates: Vec<(u16, u32, i64)> = Vec::new();

    for (drive_idx, drive) in drives.iter().enumerate() {
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
                if !search_filters.matches_record(rec, &drive.names) {
                    continue;
                }
            }
            #[expect(
                clippy::cast_possible_wrap,
                reason = "file sizes within i64 range for practical NTFS volumes"
            )]
            let sort_key = match sort_column {
                SortColumn::Size => rec.size as i64,
                #[expect(
                    clippy::cast_possible_wrap,
                    reason = "allocated sizes within i64 range"
                )]
                SortColumn::SizeOnDisk => rec.allocated as i64,
                SortColumn::Created => rec.created,
                SortColumn::Accessed => rec.accessed,
                SortColumn::Descendants => i64::from(rec.descendants),
                SortColumn::Extension | SortColumn::Type => i64::from(rec.extension_id),
                SortColumn::Name => {
                    let name_bytes = rec.name(&drive.names_lower).as_bytes();
                    let mut key = [0_u8; 8];
                    for (dst, src) in key.iter_mut().zip(name_bytes.iter()) {
                        *dst = *src;
                    }
                    i64::from_be_bytes(key)
                }
                SortColumn::Drive => {
                    let name_bytes = rec.name(&drive.names_lower).as_bytes();
                    let mut key = [0_u8; 8];
                    key[0] = drive.letter as u8;
                    for (dst, src) in key[1..].iter_mut().zip(name_bytes.iter()) {
                        *dst = *src;
                    }
                    i64::from_be_bytes(key)
                }
                SortColumn::Modified | SortColumn::Path => rec.modified,
            };
            candidates.push((drive_idx as u16, rec_idx as u32, sort_key));
        }
    }

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
            let volume_prefix = format!("{}:\\", drive.letter);
            let cache = dir_caches
                .entry(drive_idx)
                .or_insert_with(|| tree::DirCache::with_capacity(256));
            let path = tree::resolve_path_cached(drive, rec_idx as usize, &volume_prefix, cache);
            Some(make_display_row(drive.letter, rec, name, path))
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
    let volume_prefix = format!("{}:\\", drive.letter);
    let profile = std::env::var_os("UFFS_CACHE_PROFILE").is_some();

    let t_match = std::time::Instant::now();
    let match_indices: Vec<usize> = drive
        .records
        .iter()
        .enumerate()
        .filter(|(_, rec)| {
            let name = rec.name(&drive.names);
            !name.is_empty() && compiled_re.is_match(name)
        })
        .take(limit)
        .map(|(idx, _)| idx)
        .collect();
    let match_ms = t_match.elapsed().as_millis();
    let match_count = match_indices.len();

    let t_resolve = std::time::Instant::now();
    let rows = indices_to_rows(drive, &match_indices, &volume_prefix);
    let resolve_ms = t_resolve.elapsed().as_millis();

    #[expect(clippy::print_stderr, reason = "UFFS_CACHE_PROFILE diagnostic output")]
    if profile {
        eprintln!(
            "[CACHE_PROFILE] search_{}: regex_match={match_ms} ms ({match_count} hits from {} scan)  paths={resolve_ms} ms",
            drive.letter,
            drive.records.len(),
        );
    }

    rows
}

/// Search a single drive's compact index (trigram + glob/substring).
#[must_use]
pub fn search_compact_drive(
    drive: &DriveCompactIndex,
    needle: &str,
    limit: usize,
    case_sensitive: bool,
    whole_word: bool,
) -> Vec<DisplayRow> {
    if needle.is_empty() {
        return Vec::new();
    }

    let volume_prefix = format!("{}:\\", drive.letter);
    let is_glob = needle.contains('*') || needle.contains('?');
    let is_or = needle.contains('|');

    let names_blob = if case_sensitive {
        &drive.names
    } else {
        &drive.names_lower
    };

    // Pre-build a SIMD-accelerated substring finder for simple queries.
    // For 1–2 byte needles this is dramatically faster than `str::contains`
    // (memchr uses SSE2/AVX2/NEON vectorised search).
    let simple_substring = !is_glob && !is_or && !whole_word;
    let finder = simple_substring.then(|| memchr::memmem::Finder::new(needle.as_bytes()));
    let matches = |name: &str| -> bool {
        if name.is_empty() || name == "." {
            return false;
        }
        if whole_word {
            if is_glob || is_or {
                tree::name_matches(name, needle)
            } else {
                name == needle
            }
        } else if let Some(fnd) = &finder {
            fnd.find(name.as_bytes()).is_some()
        } else {
            tree::name_matches(name, needle)
        }
    };

    let trigram_needle = if is_or {
        String::new()
    } else if is_glob {
        // Extract the longest literal (non-wildcard) substring for trigram lookup
        needle
            .split(['*', '?'])
            .max_by_key(|seg| seg.len())
            .unwrap_or("")
            .to_owned()
    } else {
        needle.to_owned()
    };

    let profile = std::env::var_os("UFFS_CACHE_PROFILE").is_some();

    let t_tri = std::time::Instant::now();
    let candidates = if !case_sensitive && trigram_needle.len() >= 3 {
        drive.trigram.search(&trigram_needle)
    } else {
        None
    };
    let tri_ms = t_tri.elapsed().as_millis();
    let tri_count = candidates.as_ref().map_or(0, Vec::len);

    let t_match = std::time::Instant::now();
    let match_indices: Vec<usize> = candidates.map_or_else(
        || {
            drive
                .records
                .iter()
                .enumerate()
                .filter(|(_, rec)| {
                    let name = rec.name(names_blob);
                    matches(name)
                })
                .take(limit)
                .map(|(idx, _)| idx)
                .collect()
        },
        |candidate_indices| {
            candidate_indices
                .iter()
                .filter(|&&idx| {
                    let rec_idx = idx as usize;
                    let Some(rec) = drive.records.get(rec_idx) else {
                        return false;
                    };
                    let name = rec.name(names_blob);
                    matches(name)
                })
                .take(limit)
                .map(|&idx| idx as usize)
                .collect()
        },
    );
    let match_ms = t_match.elapsed().as_millis();
    let match_count = match_indices.len();

    let t_resolve = std::time::Instant::now();
    let rows = indices_to_rows(drive, &match_indices, &volume_prefix);
    let resolve_ms = t_resolve.elapsed().as_millis();

    #[expect(clippy::print_stderr, reason = "UFFS_CACHE_PROFILE diagnostic output")]
    if profile {
        let scanned = if tri_count > 0 {
            format!("{tri_count} trigram candidates")
        } else {
            format!("{} full scan", drive.records.len())
        };
        eprintln!(
            "[CACHE_PROFILE] search_{}: trigram={tri_ms} ms  match={match_ms} ms ({match_count} hits from {scanned})  paths={resolve_ms} ms",
            drive.letter,
        );
    }

    rows
}

/// Search a single drive using tree-based path traversal.
#[must_use]
pub fn search_compact_drive_tree(
    drive: &DriveCompactIndex,
    pattern_lower: &str,
    limit: usize,
) -> Vec<DisplayRow> {
    let volume_prefix = format!("{}:\\", drive.letter);
    let profile = std::env::var_os("UFFS_CACHE_PROFILE").is_some();

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
                &volume_prefix,
                &mut dir_cache,
            );
            Some(make_display_row(drive.letter, rec, name, path))
        })
        .collect();
    let resolve_ms = t_resolve.elapsed().as_millis();

    #[expect(clippy::print_stderr, reason = "UFFS_CACHE_PROFILE diagnostic output")]
    if profile {
        eprintln!(
            "[CACHE_PROFILE] search_{}: tree_walk={tree_ms} ms ({match_count} hits)  paths={resolve_ms} ms",
            drive.letter,
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
    drive_letter: char,
    rec: &crate::compact::CompactRecord,
    name: &str,
    path: String,
) -> DisplayRow {
    // ADS entries on directories must not render as directories
    // (no trailing backslash, name shown, stream size used).
    let is_ads = name.contains(':');
    DisplayRow {
        drive: drive_letter,
        path,
        name: name.to_owned(),
        size: rec.size,
        is_directory: rec.is_directory() && !is_ads,
        modified: rec.modified,
        created: rec.created,
        accessed: rec.accessed,
        flags: rec.flags,
        allocated: rec.allocated,
        descendants: rec.descendants,
        treesize: rec.treesize,
        tree_allocated: rec.tree_allocated,
    }
}

/// Convert a list of record indices into `DisplayRow`s with resolved paths.
fn indices_to_rows(
    drive: &DriveCompactIndex,
    indices: &[usize],
    volume_prefix: &str,
) -> Vec<DisplayRow> {
    let mut dir_cache = tree::DirCache::with_capacity(256);
    indices
        .iter()
        .filter_map(|&record_idx| {
            let rec = drive.records.get(record_idx)?;
            let name = rec.name(&drive.names);
            if name.is_empty() {
                return None;
            }
            let path = tree::resolve_path_cached(drive, record_idx, volume_prefix, &mut dir_cache);
            Some(make_display_row(drive.letter, rec, name, path))
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
