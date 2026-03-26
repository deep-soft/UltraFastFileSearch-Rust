//! Tree-based path search, glob matching, and path resolution.
//!
//! For patterns containing `\` or `/`, decomposes the pattern into path
//! segments and walks the directory tree instead of flat name search.
//! Also provides glob matching (`*`, `?`, `**`) and path resolution
//! via parent chain traversal.

use crate::compact::DriveCompactIndex;

/// Resolve a record's full path by walking the parent chain in the compact
/// index.
///
/// Returns lowercase path like `c:\users\photos\beach.jpg`.
pub fn resolve_path(drive: &DriveCompactIndex, record_idx: usize, volume_prefix: &str) -> String {
    let mut components = Vec::with_capacity(8);
    let mut current_idx = record_idx;
    let mut depth = 0_u32;

    loop {
        if depth > 256 {
            break; // Prevent infinite loops
        }

        let Some(record) = drive.records.get(current_idx) else {
            break;
        };

        let name = record.name(&drive.names);
        if name.is_empty() || name == "." {
            break;
        }

        components.push(name);

        let parent = record.parent_idx;
        if parent == u32::MAX {
            break;
        }

        current_idx = parent as usize;
        depth += 1;
    }

    // Build path from components (reversed, since we walked child→parent)
    components.reverse();

    let mut path = String::with_capacity(
        volume_prefix.len() + components.iter().map(|comp| comp.len() + 1).sum::<usize>(),
    );
    path.push_str(volume_prefix);
    for (idx, component) in components.iter().enumerate() {
        path.push_str(component);
        if idx < components.len() - 1 {
            path.push('\\');
        }
    }

    path
}

/// Returns `true` if the pattern contains a path separator (`\` or `/`),
/// indicating it should be handled by tree search rather than name trigram.
#[must_use]
#[expect(
    clippy::single_call_fn,
    reason = "public API called from backend::search; separation keeps detection logic isolated"
)]
pub fn is_path_pattern(pattern: &str) -> bool {
    pattern.contains('\\') || pattern.contains('/')
}

/// Search using tree traversal for path patterns like `\photos\*.jpg`.
///
/// Strategy:
/// 1. Split pattern on path separators into segments
/// 2. Find directories matching intermediate segments via trigram + name verify
/// 3. Collect children of those directories
/// 4. Filter leaf matches on the final segment
///
/// Falls back to name search if the pattern can't be decomposed.
#[expect(
    clippy::single_call_fn,
    reason = "public API called from backend; separation keeps tree search isolated"
)]
pub fn tree_search(drive: &DriveCompactIndex, pattern_lower: &str, limit: usize) -> Vec<u32> {
    // Normalize separators to backslash, strip leading separator
    let normalized = pattern_lower.replace('/', "\\");
    let stripped = normalized.strip_prefix('\\').unwrap_or(&normalized);

    let segments: Vec<&str> = stripped.split('\\').filter(|seg| !seg.is_empty()).collect();

    if segments.is_empty() {
        return Vec::new();
    }

    // Single segment = just a name search, no tree walk needed
    let Some(first_segment) = segments.first() else {
        return Vec::new();
    };
    if segments.len() == 1 {
        return name_search(drive, first_segment, limit);
    }

    // Multi-segment path search with ** support.
    //
    // Segments are processed left to right, maintaining a set of candidate
    // directories. Each segment narrows or expands the candidates:
    //   - "**" → expand to ALL descendants (recursive)
    //   - "*"  → direct children directories only
    //   - "name" → direct children matching "name"
    //
    // The last segment is the leaf filter applied to files+dirs in candidates.

    let Some(leaf_pattern) = segments.last() else {
        return Vec::new();
    };
    let dir_segments = segments.get(..segments.len() - 1).unwrap_or(&[]);

    // Start: first segment determines initial candidate dirs
    #[expect(
        clippy::cast_possible_truncation,
        reason = "record count bounded by NTFS limits, fits u32"
    )]
    let mut candidate_dirs: Vec<u32> = if *first_segment == "**" {
        // ** at start = all directories in the drive
        drive
            .records
            .iter()
            .enumerate()
            .filter(|(_, rec)| rec.is_directory() && rec.name_len > 0)
            .map(|(idx, _)| idx as u32)
            .collect()
    } else {
        find_dirs_by_name(drive, first_segment)
    };

    // Walk through intermediate dir segments
    for &segment in dir_segments.get(1..).unwrap_or(&[]) {
        if segment == "**" {
            // ** = collect ALL descendant directories recursively
            let mut all_descendants = Vec::new();
            for &dir_idx in &candidate_dirs {
                collect_descendant_dirs(drive, dir_idx, &mut all_descendants, limit * 10);
            }
            candidate_dirs = all_descendants;
        } else {
            // Regular segment: find matching children directories
            let mut next_dirs = Vec::new();
            for &dir_idx in &candidate_dirs {
                let dir_children = drive
                    .children
                    .get(dir_idx as usize)
                    .map_or(&[][..], Vec::as_slice);
                for &child_idx in dir_children {
                    if let Some(child_rec) = drive.records.get(child_idx as usize) {
                        if child_rec.is_directory() {
                            let child_name = child_rec.name(&drive.names_lower);
                            if segment_matches(child_name, segment) {
                                next_dirs.push(child_idx);
                            }
                        }
                    }
                }
            }
            candidate_dirs = next_dirs;
        }
        if candidate_dirs.is_empty() {
            return Vec::new();
        }
    }

    // Collect results: if leaf is **, collect ALL descendants; otherwise filter
    let mut results = Vec::new();
    if *leaf_pattern == "**" {
        // ** at end = all descendants of matched directories
        for &dir_idx in &candidate_dirs {
            collect_all_descendants(drive, dir_idx, &mut results, limit);
            if results.len() >= limit {
                break;
            }
        }
    } else {
        // Regular leaf: filter children by pattern
        for &dir_idx in &candidate_dirs {
            let dir_children = drive
                .children
                .get(dir_idx as usize)
                .map_or(&[][..], Vec::as_slice);
            for &child_idx in dir_children {
                if let Some(child_rec) = drive.records.get(child_idx as usize) {
                    let child_name = child_rec.name(&drive.names_lower);
                    if name_matches(child_name, leaf_pattern) {
                        results.push(child_idx);
                        if results.len() >= limit {
                            return results;
                        }
                    }
                }
            }
        }
    }

    results
}

/// Recursively collect all descendant DIRECTORY indices from a directory.
fn collect_descendant_dirs(
    drive: &DriveCompactIndex,
    dir_idx: u32,
    out: &mut Vec<u32>,
    max: usize,
) {
    if out.len() >= max {
        return;
    }
    let dir_children = drive
        .children
        .get(dir_idx as usize)
        .map_or(&[][..], Vec::as_slice);
    for &child_idx in dir_children {
        if let Some(child_rec) = drive.records.get(child_idx as usize) {
            if child_rec.is_directory() && child_rec.name_len > 0 {
                out.push(child_idx);
                if out.len() >= max {
                    return;
                }
                collect_descendant_dirs(drive, child_idx, out, max);
            }
        }
    }
}

/// Recursively collect ALL descendants (files + dirs) from a directory.
fn collect_all_descendants(
    drive: &DriveCompactIndex,
    dir_idx: u32,
    out: &mut Vec<u32>,
    max: usize,
) {
    if out.len() >= max {
        return;
    }
    let dir_children = drive
        .children
        .get(dir_idx as usize)
        .map_or(&[][..], Vec::as_slice);
    for &child_idx in dir_children {
        if let Some(child_rec) = drive.records.get(child_idx as usize) {
            if child_rec.name_len > 0 {
                let name = child_rec.name(&drive.names_lower);
                if !name.is_empty() && name != "." {
                    out.push(child_idx);
                    if out.len() >= max {
                        return;
                    }
                }
                if child_rec.is_directory() {
                    collect_all_descendants(drive, child_idx, out, max);
                }
            }
        }
    }
}

/// Find all directory compact indices whose name matches a pattern.
#[expect(
    clippy::single_call_fn,
    reason = "separated for readability; directory search is a distinct concern"
)]
fn find_dirs_by_name(drive: &DriveCompactIndex, pattern: &str) -> Vec<u32> {
    // Try trigram first for 3+ char patterns
    let candidates = drive.trigram.search(pattern);

    if let Some(candidate_indices) = candidates {
        candidate_indices
            .iter()
            .filter(|&&idx| {
                let rec_idx = idx as usize;
                let Some(rec) = drive.records.get(rec_idx) else {
                    return false;
                };
                if !rec.is_directory() {
                    return false;
                }
                let dir_name = rec.name(&drive.names_lower);
                segment_matches(dir_name, pattern)
            })
            .copied()
            .collect()
    } else {
        // Short pattern: linear scan for matching directories
        drive
            .records
            .iter()
            .enumerate()
            .filter(|(_, rec)| {
                if !rec.is_directory() {
                    return false;
                }
                let dir_name = rec.name(&drive.names_lower);
                segment_matches(dir_name, pattern)
            })
            .map(|(idx, _)| {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "record count bounded by NTFS limits"
                )]
                {
                    idx as u32
                }
            })
            .collect()
    }
}

/// Simple name search by substring (used as single-segment fallback).
#[expect(
    clippy::single_call_fn,
    reason = "separated for readability; name search is a distinct concern"
)]
fn name_search(drive: &DriveCompactIndex, needle: &str, limit: usize) -> Vec<u32> {
    let candidates = drive.trigram.search(needle);

    if let Some(candidate_indices) = candidates {
        candidate_indices
            .iter()
            .filter(|&&idx| {
                let rec_idx = idx as usize;
                let Some(rec) = drive.records.get(rec_idx) else {
                    return false;
                };
                let name = rec.name(&drive.names_lower);
                !name.is_empty() && name.contains(needle)
            })
            .take(limit)
            .copied()
            .collect()
    } else {
        drive
            .records
            .iter()
            .enumerate()
            .filter(|(_, rec)| {
                let name = rec.name(&drive.names_lower);
                !name.is_empty() && name != "." && name.contains(needle)
            })
            .take(limit)
            .map(|(idx, _)| {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "record count bounded by NTFS limits"
                )]
                {
                    idx as u32
                }
            })
            .collect()
    }
}

/// Check if a name matches a glob pattern (case-insensitive, both already
/// lowercase).
///
/// Supports:
/// - `*`: matches any sequence of characters (including empty)
/// - `?`: matches exactly one character
/// - Multiple wildcards: `*sex*Ge*` matches "I want your Sex - George Michael"
/// - No wildcards: plain substring match
///
/// Uses a simple iterative algorithm (no regex, no allocations).
pub fn name_matches(name: &str, pattern: &str) -> bool {
    if name.is_empty() || name == "." {
        return false;
    }
    // Fast paths
    if pattern == "*" {
        return true;
    }
    // OR operator: `*.rs|*.py` → match if ANY sub-pattern matches
    if pattern.contains('|') {
        return pattern.split('|').any(|sub| name_matches_single(name, sub));
    }
    name_matches_single(name, pattern)
}

/// Match a single pattern (no `|` alternation) against a filename.
fn name_matches_single(name: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') && !pattern.contains('?') {
        // No wildcards → substring match
        return name.contains(pattern);
    }
    // Full glob match using iterative two-pointer algorithm
    glob_match(name.as_bytes(), pattern.as_bytes())
}

/// Match a path segment exactly against a directory/file name.
///
/// Unlike [`name_matches`] which does substring matching for bare literals
/// (search behaviour), this requires an **exact** match for non-glob segments.
/// `"Projects"` matches only `"projects"`, not `"rustroverprojects"`.
///
/// Glob patterns (`*`, `?`) still use glob matching.
pub fn segment_matches(name: &str, segment: &str) -> bool {
    if name.is_empty() || name == "." {
        return false;
    }
    if segment == "*" || segment == "**" {
        return true;
    }
    if !segment.contains('*') && !segment.contains('?') {
        // No wildcards → exact match (not substring!)
        return name == segment;
    }
    // Glob pattern → full glob match
    glob_match(name.as_bytes(), segment.as_bytes())
}

/// Iterative glob matching: `*` matches any sequence, `?` matches one byte.
///
/// Handles patterns like `*sex*ge*`, `*.jpg`, `photo?.*` correctly.
#[expect(
    clippy::indexing_slicing,
    reason = "all index accesses are bounds-checked by the while/if conditions"
)]
fn glob_match(text: &[u8], pattern: &[u8]) -> bool {
    let mut ti = 0_usize; // text index
    let mut pi = 0_usize; // pattern index
    let mut last_star_p = usize::MAX; // last '*' position in pattern
    let mut last_star_t = 0_usize; // text position when last '*' was hit

    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == b'?' || pattern[pi] == text[ti]) {
            // Character match or '?' wildcard
            ti += 1;
            pi += 1;
        } else if pi < pattern.len() && pattern[pi] == b'*' {
            // '*' wildcard — remember position and try matching zero chars
            last_star_p = pi;
            last_star_t = ti;
            pi += 1;
        } else if last_star_p != usize::MAX {
            // Mismatch — backtrack to last '*' and consume one more text char
            pi = last_star_p + 1;
            last_star_t += 1;
            ti = last_star_t;
        } else {
            return false;
        }
    }

    // Consume trailing '*' in pattern
    while pi < pattern.len() && pattern[pi] == b'*' {
        pi += 1;
    }

    pi == pattern.len()
}
