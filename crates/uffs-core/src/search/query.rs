//! Search functions for compact-index drives.
//!
//! Per-drive search (trigram, regex, tree) and global top-N collection
//! for match-all queries. Called by `MultiDriveBackend::search()`.

use super::backend::{DisplayRow, FilterMode, SortColumn};
use super::filters::SearchFilters;
use crate::compact::DriveCompactIndex;
use crate::search::tree;

/// Collect the global top-N records across ALL drives for `*` match-all.
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
        SortColumn::Path => collect_path_sorted(drives, limit, sort_desc),
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

/// Hierarchical depth-first tree walk for Path sort.
fn collect_path_sorted(
    drives: &[DriveCompactIndex],
    limit: usize,
    sort_desc: bool,
) -> Vec<DisplayRow> {
    let mut results = Vec::with_capacity(limit);

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

        let mut stack: Vec<u32> = roots.into_iter().rev().collect();
        while let Some(idx) = stack.pop() {
            if results.len() >= limit {
                return results;
            }

            let Some(rec) = drive.records.get(idx as usize) else {
                continue;
            };
            let name = rec.name(&drive.names);
            if name.is_empty() || name == "." {
                continue;
            }

            let path = tree::resolve_path(drive, idx as usize, &volume_prefix);
            results.push(make_display_row(drive.letter, rec, name, path));

            if let Some(children) = drive.children.get(idx as usize) {
                if !children.is_empty() {
                    let mut sorted_children = children.clone();
                    sort_indices_by_name(&mut sorted_children, drive, sort_desc);
                    for &child in sorted_children.iter().rev() {
                        stack.push(child);
                    }
                }
            }
        }
    }

    results
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

    let mut rows: Vec<DisplayRow> = candidates
        .iter()
        .filter_map(|&(drive_idx, rec_idx, _)| {
            let drive = drives.get(drive_idx as usize)?;
            let rec = drive.records.get(rec_idx as usize)?;
            let name = rec.name(&drive.names);
            if name.is_empty() || name == "." {
                return None;
            }
            let volume_prefix = format!("{}:\\", drive.letter);
            let path = tree::resolve_path(drive, rec_idx as usize, &volume_prefix);
            Some(make_display_row(drive.letter, rec, name, path))
        })
        .collect();

    super::backend::sort_rows(&mut rows, sort_column, sort_desc, &[]);
    rows
}

/// Search a single drive using regex matching on filenames.
pub fn search_compact_drive_regex(
    drive: &DriveCompactIndex,
    compiled_re: &regex::Regex,
    limit: usize,
) -> Vec<DisplayRow> {
    let volume_prefix = format!("{}:\\", drive.letter);

    let match_indices: Vec<usize> = drive
        .records
        .iter()
        .enumerate()
        .filter(|(_, rec)| {
            let name = rec.name(&drive.names);
            !name.is_empty() && name != "." && compiled_re.is_match(name)
        })
        .take(limit)
        .map(|(idx, _)| idx)
        .collect();

    indices_to_rows(drive, &match_indices, &volume_prefix)
}

/// Search a single drive's compact index (trigram + glob/substring).
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

    let matches = |name: &str| -> bool {
        if whole_word {
            if is_glob || is_or {
                tree::name_matches(name, needle)
            } else {
                name == needle
            }
        } else {
            tree::name_matches(name, needle)
        }
    };

    let trigram_needle = if is_or {
        String::new()
    } else if is_glob {
        longest_literal(needle)
    } else {
        needle.to_owned()
    };

    let candidates = if !case_sensitive && trigram_needle.len() >= 3 {
        drive.trigram.search(&trigram_needle)
    } else {
        None
    };

    let match_indices: Vec<usize> = if let Some(candidate_indices) = candidates {
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
    } else {
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
    };

    indices_to_rows(drive, &match_indices, &volume_prefix)
}

/// Search a single drive using tree-based path traversal.
pub fn search_compact_drive_tree(
    drive: &DriveCompactIndex,
    pattern_lower: &str,
    limit: usize,
) -> Vec<DisplayRow> {
    let volume_prefix = format!("{}:\\", drive.letter);
    let match_indices = tree::tree_search(drive, pattern_lower, limit);

    match_indices
        .iter()
        .filter_map(|&record_idx| {
            let rec = drive.records.get(record_idx as usize)?;
            let name = rec.name(&drive.names);
            if name.is_empty() || name == "." {
                return None;
            }
            let path = tree::resolve_path(drive, record_idx as usize, &volume_prefix);
            Some(make_display_row(drive.letter, rec, name, path))
        })
        .collect()
}

// ── Shared helpers ──────────────────────────────────────────────────────────

/// Extract the longest literal (non-wildcard) substring from a glob pattern.
fn longest_literal(pattern: &str) -> String {
    pattern
        .split(['*', '?'])
        .max_by_key(|seg| seg.len())
        .unwrap_or("")
        .to_owned()
}

/// Build a `DisplayRow` from a compact record.
fn make_display_row(
    drive_letter: char,
    rec: &crate::compact::CompactRecord,
    name: &str,
    path: String,
) -> DisplayRow {
    DisplayRow {
        drive: drive_letter,
        path,
        name: name.to_owned(),
        size: rec.size,
        is_directory: rec.is_directory(),
        modified: rec.modified,
        created: rec.created,
        accessed: rec.accessed,
        flags: rec.flags,
        allocated: rec.allocated,
        descendants: rec.descendants,
        treesize: rec.treesize,
    }
}

/// Convert a list of record indices into `DisplayRow`s with resolved paths.
fn indices_to_rows(
    drive: &DriveCompactIndex,
    indices: &[usize],
    volume_prefix: &str,
) -> Vec<DisplayRow> {
    indices
        .iter()
        .filter_map(|&record_idx| {
            let rec = drive.records.get(record_idx)?;
            let name = rec.name(&drive.names);
            if name.is_empty() || name == "." {
                return None;
            }
            let path = tree::resolve_path(drive, record_idx, volume_prefix);
            Some(make_display_row(drive.letter, rec, name, path))
        })
        .collect()
}
