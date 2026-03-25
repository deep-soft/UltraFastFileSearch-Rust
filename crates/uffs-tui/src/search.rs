//! Search functions for compact-index drives.
//!
//! Per-drive search (trigram, regex, tree) and global top-N collection
//! for match-all queries. Called by `MultiDriveBackend::search()`.

use crate::compact::{self, DriveCompactIndex};
use crate::backend::{DisplayRow, SortColumn};

/// Collect the global top-N records across ALL drives for `*` match-all.
///
/// For numeric columns (Size, Modified): scans all 25M compact records with
/// lightweight `(drive_idx, record_idx, sort_key)` tuples, sorts globally,
/// takes top N, then resolves paths only for those N winners.
///
/// For text columns (Name, Path, etc.): samples records from each drive
/// proportionally, builds full `DisplayRow`s, sorts by text, truncates.
#[expect(
    clippy::single_call_fn,
    reason = "called from MultiDriveBackend::search; separation keeps global scan logic isolated"
)]
pub fn collect_global_top_n(
    drives: &[DriveCompactIndex],
    limit: usize,
    sort_column: SortColumn,
    sort_desc: bool,
) -> Vec<DisplayRow> {
    match sort_column {
        // Numeric columns: efficient lightweight tuple scan of all 25M records
        // Drive: packs drive_letter + name_prefix into i64 for grouping
        SortColumn::Size | SortColumn::Modified | SortColumn::Drive => {
            collect_global_top_n_numeric(drives, limit, sort_column, sort_desc)
        }
        // Path: hierarchical depth-first tree walk — instant, touches ~N records
        SortColumn::Path => collect_path_sorted(drives, limit, sort_desc),
        // Extension/Type: routed to numeric (extension_id) inside collect_name_sorted
        //   Same extension_id = same devicons icon type, so grouping is correct.
        // Name: text comparison on names blob
        SortColumn::Name | SortColumn::Extension | SortColumn::Type => {
            collect_name_sorted(drives, limit, sort_column, sort_desc)
        }
    }
}

/// Hierarchical depth-first tree walk for Path/Drive sort.
///
/// Walks the directory tree in alphabetical order (by name within each level),
/// collecting files as they're encountered. Stops after `limit` results.
/// This is instant — touches only ~N records instead of sorting all 25M.
#[expect(
    clippy::single_call_fn,
    reason = "called from collect_global_top_n; separation keeps tree walk isolated"
)]
fn collect_path_sorted(
    drives: &[DriveCompactIndex],
    limit: usize,
    sort_desc: bool,
) -> Vec<DisplayRow> {
    let mut results = Vec::with_capacity(limit);

    // Sort drives by letter (ascending or descending)
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

        // Find root-level records (parent_idx == u32::MAX)
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

        // Sort roots by name
        sort_indices_by_name(&mut roots, drive, sort_desc);

        // Depth-first walk
        let mut stack: Vec<u32> = roots.into_iter().rev().collect(); // reverse for correct DFS order
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

            let path = compact::resolve_path(drive, idx as usize, &volume_prefix);
            results.push(DisplayRow {
                drive: drive.letter,
                path,
                name: name.to_owned(),
                size: rec.size,
                is_directory: rec.is_directory(),
                modified: rec.modified,
            });

            // Push children (sorted, reversed for DFS order)
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

/// Sort for Name/Extension/Type columns using lightweight numeric keys.
///
/// - **Extension**: sorts by `extension_id` (u16 comparison — instant)
/// - **Name**: sorts by name bytes from names blob (text comparison)
/// - **Type**: sorts by devicons icon character
///
/// Uses `(drive_idx, record_idx, sort_key)` tuples for Extension (numeric),
/// falls back to text comparison for Name/Type.
#[expect(
    clippy::single_call_fn,
    reason = "called from collect_global_top_n; separation keeps text sort isolated"
)]
fn collect_name_sorted(
    drives: &[DriveCompactIndex],
    limit: usize,
    sort_column: SortColumn,
    sort_desc: bool,
) -> Vec<DisplayRow> {
    // All columns now route through the numeric fast path:
    // - Extension/Type: extension_id as sort key
    // - Name: first 8 bytes of lowercase name packed into u64
    collect_global_top_n_numeric(drives, limit, sort_column, sort_desc)
}

/// Efficient numeric global top-N using lightweight tuples.
///
/// Scans all compact records, extracts a numeric sort key, sorts globally,
/// takes top N, then resolves paths only for the winners.
#[expect(
    clippy::cast_possible_truncation,
    reason = "drive index and record index bounded by practical limits"
)]
fn collect_global_top_n_numeric(
    drives: &[DriveCompactIndex],
    limit: usize,
    sort_column: SortColumn,
    sort_desc: bool,
) -> Vec<DisplayRow> {
    let mut candidates: Vec<(u16, u32, i64)> = Vec::new();

    for (drive_idx, drive) in drives.iter().enumerate() {
        for (rec_idx, rec) in drive.records.iter().enumerate() {
            if rec.name_len == 0 {
                continue;
            }
            #[expect(
                clippy::cast_possible_wrap,
                reason = "file sizes within i64 range for practical NTFS volumes"
            )]
            let sort_key = match sort_column {
                SortColumn::Size => rec.size as i64,
                SortColumn::Extension | SortColumn::Type => i64::from(rec.extension_id),
                SortColumn::Name => {
                    // Pack first 8 bytes of lowercase name into u64 (big-endian).
                    // Byte ordering matches alphabetical: "abc" < "abd" as u64.
                    let name_bytes = rec.name(&drive.names_lower).as_bytes();
                    let mut key = [0_u8; 8];
                    for (dst, src) in key.iter_mut().zip(name_bytes.iter()) {
                        *dst = *src;
                    }
                    i64::from_be_bytes(key)
                }
                SortColumn::Drive => {
                    // Pack drive letter (top byte) + first 7 bytes of name.
                    // Groups by drive, then alphabetical by name within drive.
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
            let path = compact::resolve_path(drive, rec_idx as usize, &volume_prefix);
            Some(DisplayRow {
                drive: drive.letter,
                path,
                name: name.to_owned(),
                size: rec.size,
                is_directory: rec.is_directory(),
                modified: rec.modified,
            })
        })
        .collect();

    // Re-sort with name tiebreaker: when multiple results share the same
    // sort key (e.g., same extension, same size), order them by name.
    crate::backend::sort_rows(&mut rows, sort_column, sort_desc);
    rows
}

/// Search a single drive using regex matching on filenames.
///
/// Linear scan — regex can't leverage trigram index. But typically still
/// fast (<500ms for 25M) because regex matching is optimized by the `regex`
/// crate.
#[expect(
    clippy::single_call_fn,
    reason = "called from MultiDriveBackend::search via rayon; separation keeps regex logic isolated"
)]
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

    match_indices
        .iter()
        .filter_map(|&record_idx| {
            let rec = drive.records.get(record_idx)?;
            let name = rec.name(&drive.names);
            if name.is_empty() || name == "." {
                return None;
            }
            let path = compact::resolve_path(drive, record_idx, &volume_prefix);
            Some(DisplayRow {
                drive: drive.letter,
                path,
                name: name.to_owned(),
                size: rec.size,
                is_directory: rec.is_directory(),
                modified: rec.modified,
            })
        })
        .collect()
}

/// Extract the longest literal (non-wildcard) substring from a glob pattern.
///
/// Used to find a trigram-searchable needle within glob patterns:
/// - `*sex*ge*` → `"sex"` (longest literal segment)
/// - `*.jpg` → `".jpg"`
/// - `photo?.*` → `"photo"`
#[expect(
    clippy::single_call_fn,
    reason = "separated for readability; literal extraction is a distinct concern"
)]
fn longest_literal(pattern: &str) -> String {
    pattern
        .split(['*', '?'])
        .max_by_key(|seg| seg.len())
        .unwrap_or("")
        .to_owned()
}

/// Search a single drive's compact index, returning matching `DisplayRow`s.
///
/// Uses trigram index on names for 3+ char patterns, linear scan for shorter.
/// Paths are resolved on-demand only for matched results.
#[expect(
    clippy::single_call_fn,
    reason = "called from MultiDriveBackend::search via rayon; separation keeps per-drive logic isolated"
)]
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

    // Choose names blob: case-sensitive uses original case, otherwise lowercase
    let names_blob = if case_sensitive {
        &drive.names
    } else {
        &drive.names_lower
    };

    // Match function: whole_word requires exact name match, otherwise
    // glob/substring
    let matches = |name: &str| -> bool {
        if whole_word {
            if is_glob {
                compact::name_matches(name, needle)
            } else {
                name == needle
            }
        } else {
            compact::name_matches(name, needle)
        }
    };

    // For glob patterns, extract the longest literal substring for trigram lookup.
    let trigram_needle = if is_glob {
        longest_literal(needle)
    } else {
        needle.to_owned()
    };

    // Trigram index only works on names_lower — skip for case-sensitive mode
    let candidates = if !case_sensitive && trigram_needle.len() >= 3 {
        drive.trigram.search(&trigram_needle)
    } else {
        None
    };

    let match_indices: Vec<usize> = if let Some(candidate_indices) = candidates {
        // Trigram hit: verify candidates
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
        // Linear scan (case-sensitive, short pattern, or no trigram-able literals)
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

    // Build DisplayRows: resolve paths on-demand (only for matches)
    match_indices
        .iter()
        .filter_map(|&record_idx| {
            let rec = drive.records.get(record_idx)?;
            let name = rec.name(&drive.names);
            if name.is_empty() || name == "." {
                return None;
            }
            let path = compact::resolve_path(drive, record_idx, &volume_prefix);
            Some(DisplayRow {
                drive: drive.letter,
                path,
                name: name.to_owned(),
                size: rec.size,
                is_directory: rec.is_directory(),
                modified: rec.modified,
            })
        })
        .collect()
}

/// Search a single drive using tree-based path traversal.
///
/// For patterns containing `\` or `/`, decomposes the pattern into path
/// segments and walks the directory tree instead of flat name search.
#[expect(
    clippy::single_call_fn,
    reason = "called from MultiDriveBackend::search via rayon; separation keeps per-drive logic isolated"
)]
pub fn search_compact_drive_tree(
    drive: &DriveCompactIndex,
    pattern_lower: &str,
    limit: usize,
) -> Vec<DisplayRow> {
    let volume_prefix = format!("{}:\\", drive.letter);

    let match_indices = compact::tree_search(drive, pattern_lower, limit);

    match_indices
        .iter()
        .filter_map(|&record_idx| {
            let rec = drive.records.get(record_idx as usize)?;
            let name = rec.name(&drive.names);
            if name.is_empty() || name == "." {
                return None;
            }
            let path = compact::resolve_path(drive, record_idx as usize, &volume_prefix);
            Some(DisplayRow {
                drive: drive.letter,
                path,
                name: name.to_owned(),
                size: rec.size,
                is_directory: rec.is_directory(),
                modified: rec.modified,
            })
        })
        .collect()
}
