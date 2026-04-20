// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Search dispatch helpers: pattern-rewrite safety nets and per-branch
//! fan-out functions.
//!
//! Extracted from `backend.rs` to keep that file under the 800-LOC
//! file-size policy.  All symbols here are used only by
//! `MultiDriveBackend::search` and the free `search_index` function in
//! `backend.rs`, so visibility is scoped to `pub(super)`.
//!
//! Two categories:
//!
//! 1. **Pattern-rewrite safety nets** ([`apply_dispatch_safety_nets`]) — mirror
//!    the parse-time rewrites in
//!    `uffs_client::protocol::cli_args::into_search_params`.  Direct JSON-RPC
//!    `search` callers and library users that build `SearchParams` manually
//!    skip the parse-time layer; these catch the two rewrites at dispatch time
//!    so every entry point lands on the same hot paths.  `is_pure_ext_glob` and
//!    `parse_bare_drive_prefix` are internal helpers consumed by
//!    `apply_dispatch_safety_nets` — kept private to this module.
//!
//! 2. **Per-branch dispatchers** ([`dispatch_match_all`], [`dispatch_regex`],
//!    [`dispatch_trigram_or_tree`]) — the three leaf dispatch paths +
//!    [`pick_mode_label`] for tracing.

use rayon::prelude::*;

use super::backend::{DisplayRow, FilterMode, PhaseTimings, SortSpec};
use super::filters::SearchFilters;
use super::sorting::sort_rows;
use crate::compact::DriveCompactIndex;
use crate::search::field::FieldId;

// ─── Pattern-rewrite safety nets ───────────────────────────────────────

/// Return `true` when `s` is exactly `*.<alnum+underscore>+` — a pure
/// extension glob that can be safely promoted to an `ExtensionIndex` lookup.
///
/// Used by the search-dispatch safety net: if a caller (e.g. direct
/// JSON-RPC `search` method) supplies `pattern="*.dll"` without setting
/// the `extensions` filter, we can still route through
/// `numeric_top_n::ext_fast_path` by rewriting to `pattern="*"` +
/// `extensions=["dll"]`.
///
/// Mirror of `uffs_client::protocol::cli_args::is_pure_ext_glob` — keep
/// the two in sync.  See that function's doc for the acceptance matrix.
fn is_pure_ext_glob(pattern: &str) -> bool {
    pattern.strip_prefix("*.").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    })
}

/// Parse a bare drive-letter prefix from a pattern.
///
/// Returns `Some((letter_upper, rest))` when the pattern matches
/// `<letter>:<rest>` where `<letter>` is a single ASCII alphabetic
/// character and `<rest>` is non-empty and does NOT start with `\` or
/// `/` (path-anchored forms like `C:\*.dll` must keep routing through
/// the tree walker).
///
/// Used by the search-dispatch safety net: if a caller (e.g. direct
/// JSON-RPC `search` method) supplies `pattern="C:*.dll"` with an
/// empty `drives_filter`, we can still narrow the search to drive `C`
/// and (via the ext-glob follow-up) route through the `ExtensionIndex`.
///
/// Mirror of `uffs_client::protocol::cli_args::parse_bare_drive_prefix` —
/// keep the two in sync.  See that function's doc for the full
/// acceptance matrix.
fn parse_bare_drive_prefix(pattern: &str) -> Option<(char, &str)> {
    let bytes = pattern.as_bytes();
    let letter = *bytes.first()?;
    if !letter.is_ascii_alphabetic() {
        return None;
    }
    if *bytes.get(1)? != b':' {
        return None;
    }
    let rest = pattern.get(2..)?;
    if rest.is_empty() || rest.starts_with(['\\', '/']) {
        return None;
    }
    Some((letter.to_ascii_uppercase() as char, rest))
}

/// Apply dispatch-time pattern-rewrite safety nets, in canonical order.
///
/// Mirrors the parse-time rewrites in
/// `uffs_client::protocol::cli_args::into_search_params`.  Direct
/// JSON-RPC `search` callers or library users that build `SearchParams`
/// manually skip the parse-time layer; this function catches the two
/// rewrites at dispatch time so both entry points land on the same hot
/// paths.
///
/// # Rewrites (applied in order)
///
/// 1. `<letter>:<rest>` → `drives_filter = [<letter>]`, `pattern = <rest>`.
///    Only fires when the caller's `drives_filter_empty` and not `match_path`.
///    Path-anchored forms (`C:\*.dll`) are excluded by
///    `parse_bare_drive_prefix`.  The promoted letter is pushed into
///    `drive_buf` (which the caller uses as backing storage for a `&[char]`
///    slice that lives for the rest of the dispatch).
///
/// 2. `*.<ext>` → `pattern = "*"`, `extensions += [<ext_lower>]`. Only fires
///    when not `match_path`, not `case_sensitive`, and
///    `search_filters.extensions` is empty.  Uses `is_pure_ext_glob` to reject
///    multi-segment / wildcard / path-anchored shapes.
///
/// The two rewrites **compose**: `C:*.dll` is first stripped to
/// `*.dll` by rewrite #1, and rewrite #2 then promotes it to
/// `pattern="*"` + `extensions=["dll"]`, so the caller ends up with
/// `drive=C` + `ext=dll` + match-all — exactly the shape the
/// `numeric_top_n::ext_fast_path` expects.
pub(super) fn apply_dispatch_safety_nets(
    pattern: &mut &str,
    match_path: bool,
    case_sensitive: bool,
    drives_filter_empty: bool,
    search_filters: &mut SearchFilters,
    drive_buf: &mut Vec<char>,
) {
    if drives_filter_empty
        && !match_path
        && let Some((letter, rest)) = parse_bare_drive_prefix(pattern)
    {
        tracing::debug!(
            original_pattern = *pattern,
            promoted_drive = %letter,
            promoted_rest = rest,
            "promoted <letter>:<rest> to drive filter (dispatch-time safety net)"
        );
        *pattern = rest;
        drive_buf.push(letter);
    }

    if !match_path
        && !case_sensitive
        && search_filters.extensions.is_empty()
        && is_pure_ext_glob(pattern)
    {
        let ext_lower = pattern
            .strip_prefix("*.")
            .unwrap_or_default()
            .to_ascii_lowercase();
        tracing::debug!(
            original_pattern = *pattern,
            promoted_ext = %ext_lower,
            "promoted *.<ext> to ext filter (dispatch-time safety net)"
        );
        search_filters.extensions.push(ext_lower);
        *pattern = "*";
    }
}

// ─── Per-branch dispatchers ────────────────────────────────────────────

/// Dispatch the `pattern == "*"` fast path: global top-N from the ext
/// and size indices, optionally post-filtered by display-row predicates.
///
/// Returns `(rows, phase_timings)`.  `phase_timings` is `Some` when the
/// numeric-sort branch of `collect_global_top_n` ran (i.e. any sort column
/// other than `Path` / `PathOnly`) — that branch calls
/// `collect_global_top_n_numeric`, which populates the scan / sort /
/// `path_resolve` sub-phase breakdown.  The `PathOnly` tree-walk branch
/// produces `None`; callers treat that as "no sub-breakdown available".
pub(super) fn dispatch_match_all(
    active_drives: &[&DriveCompactIndex],
    limit: usize,
    sort_column: FieldId,
    sort_desc: bool,
    filter_mode: FilterMode,
    search_filters: &mut SearchFilters,
) -> (Vec<DisplayRow>, Option<PhaseTimings>) {
    let t_top_n = std::time::Instant::now();
    let (mut rows, phase_timings) = super::query::collect_global_top_n(
        active_drives,
        limit,
        sort_column,
        sort_desc,
        filter_mode,
        search_filters,
    );
    let top_n_ms = t_top_n.elapsed().as_millis();
    tracing::debug!(rows = rows.len(), top_n_ms, "[2] collect_global_top_n done");
    if search_filters.needs_display_row_filter() {
        let t_post = std::time::Instant::now();
        super::filters::apply_search_filters(&mut rows, search_filters);
        tracing::debug!(
            rows_after = rows.len(),
            post_filter_ms = t_post.elapsed().as_millis(),
            "[3] post-filter done"
        );
    }
    (rows, phase_timings)
}

/// Dispatch the regex branch (`>pattern`): compile the regex, fan out
/// a rayon scan across drives, then filter + sort + truncate.  Returns
/// `None` when the regex fails to compile (caller maps this to an empty
/// result so callers can distinguish "no matches" from "bad pattern").
#[expect(clippy::too_many_arguments, reason = "single call site, flat args")]
pub(super) fn dispatch_regex(
    active_drives: &[&DriveCompactIndex],
    needle: &str,
    case_sensitive: bool,
    limit: usize,
    filter_mode: FilterMode,
    search_filters: &SearchFilters,
    sort_column: FieldId,
    sort_desc: bool,
    extra_sort_tiers: &[SortSpec],
) -> Option<Vec<DisplayRow>> {
    let regex_pattern = needle.strip_prefix('>').unwrap_or(needle);
    let compiled_re = regex::RegexBuilder::new(regex_pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .ok()?;
    let drive_results: Vec<Vec<DisplayRow>> = active_drives
        .par_iter()
        .map(|drive| super::query::search_compact_drive_regex(drive, &compiled_re, limit))
        .collect();
    let mut rows: Vec<DisplayRow> = drive_results.into_iter().flatten().collect();
    super::filters::apply_filter(&mut rows, filter_mode);
    super::filters::apply_search_filters(&mut rows, search_filters);
    sort_rows(&mut rows, sort_column, sort_desc, extra_sort_tiers);
    rows.truncate(limit);
    Some(rows)
}

/// Dispatch the default branch: tree-walk for path patterns, trigram
/// for name patterns, both fanned across drives then filtered + sorted
/// + truncated.
#[expect(clippy::too_many_arguments, reason = "single call site, flat args")]
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "the four bools (is_path / case_sensitive / whole_word / match_path) are orthogonal runtime switches, each controlling a distinct aspect of trigram vs tree matching; bundling them into an enum would lose that orthogonality"
)]
pub(super) fn dispatch_trigram_or_tree(
    active_drives: &[&DriveCompactIndex],
    needle: &str,
    is_path: bool,
    case_sensitive: bool,
    whole_word: bool,
    match_path: bool,
    limit: usize,
    filter_mode: FilterMode,
    search_filters: &SearchFilters,
    sort_column: FieldId,
    sort_desc: bool,
    extra_sort_tiers: &[SortSpec],
) -> Vec<DisplayRow> {
    let drive_results: Vec<Vec<DisplayRow>> = active_drives
        .par_iter()
        .map(|drive| {
            if is_path {
                super::query::search_compact_drive_tree(drive, needle, limit)
            } else {
                super::query::search_compact_drive(
                    drive,
                    needle,
                    limit,
                    case_sensitive,
                    whole_word,
                    match_path,
                )
            }
        })
        .collect();
    let mut rows: Vec<DisplayRow> = drive_results.into_iter().flatten().collect();
    super::filters::apply_filter(&mut rows, filter_mode);
    super::filters::apply_search_filters(&mut rows, search_filters);
    sort_rows(&mut rows, sort_column, sort_desc, extra_sort_tiers);
    rows.truncate(limit);
    rows
}

/// Pick the `cache_profile` `mode` tracing label for the chosen
/// dispatch branch.  Pure function — no side effects.
pub(super) const fn pick_mode_label(
    is_match_all: bool,
    is_regex: bool,
    is_path: bool,
) -> &'static str {
    if is_match_all {
        "match-all"
    } else if is_regex {
        "regex"
    } else if is_path {
        "tree"
    } else {
        "trigram"
    }
}
