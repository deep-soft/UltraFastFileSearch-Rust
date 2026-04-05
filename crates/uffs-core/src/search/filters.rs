//! Extended post-search filters and NTFS attribute helpers.
//!
//! [`SearchFilters`] holds pre-parsed filter criteria. All parsing (time
//! bounds, attribute bits) happens at construction time so the hot `retain`
//! loop is branch-only.

use super::backend::{DisplayRow, FilterMode};
use crate::compact::CompactRecord;
use crate::search::tree::name_matches;

/// Lowercase a string into a reusable UTF-8 buffer and return the borrowed
/// string view.
fn lowercase_into<'a>(input: &str, buf: &'a mut Vec<u8>) -> &'a str {
    buf.clear();
    for ch in input.chars() {
        for lower in ch.to_lowercase() {
            let mut char_buf = [0_u8; 4];
            let encoded = lower.encode_utf8(&mut char_buf);
            buf.extend_from_slice(encoded.as_bytes());
        }
    }
    core::str::from_utf8(buf.as_slice()).map_or("", |lowered| lowered)
}

/// Return `true` if a normalized extension matches an allowed filter token.
///
/// The fast/common path compares already-lowercased strings directly. The
/// fallback branch keeps manual test fixtures and any direct struct
/// construction robust if a caller supplied mixed-case extension tokens.
#[must_use]
fn extension_matches_filter(allowed: &str, normalized_extension: &str) -> bool {
    allowed == normalized_extension || allowed.to_lowercase() == normalized_extension
}

/// Apply filter mode to a set of display rows.
pub fn apply_filter(rows: &mut Vec<DisplayRow>, filter: FilterMode) {
    match filter {
        FilterMode::All => {}
        FilterMode::FilesOnly => rows.retain(|row| !row.is_directory),
        FilterMode::DirsOnly => rows.retain(|row| row.is_directory),
    }
}

/// Extended post-search filters.
///
/// All fields are pre-parsed so the per-row `retain` loop is branch-only
/// (no parsing).
#[derive(Debug, Default)]
pub struct SearchFilters {
    /// Hide files whose name starts with `$`.
    pub hide_system: bool,
    /// Hide NTFS Alternate Data Streams (names containing `:`).
    pub hide_ads: bool,
    /// Minimum file size in bytes.
    pub min_size: Option<u64>,
    /// Maximum file size in bytes.
    pub max_size: Option<u64>,
    /// Modified-time lower bound (Unix µs, inclusive).
    pub newer_us: Option<i64>,
    /// Modified-time upper bound (Unix µs, exclusive).
    pub older_us: Option<i64>,
    /// Created-time lower bound (Unix µs, inclusive).
    pub newer_created_us: Option<i64>,
    /// Created-time upper bound (Unix µs, exclusive).
    pub older_created_us: Option<i64>,
    /// Accessed-time lower bound (Unix µs, inclusive).
    pub newer_accessed_us: Option<i64>,
    /// Accessed-time upper bound (Unix µs, exclusive).
    pub older_accessed_us: Option<i64>,
    /// Required attribute bits (all must be set).
    pub attr_require: u32,
    /// Excluded attribute bits (none may be set).
    pub attr_exclude: u32,
    /// Minimum descendant count (inclusive).
    pub min_descendants: Option<u32>,
    /// Maximum descendant count (inclusive).
    pub max_descendants: Option<u32>,
    /// Allowed extensions (lowercase, without dot). Empty = no filter.
    pub extensions: Vec<String>,
    /// Pre-resolved extension IDs for the current drive.
    /// Set via [`resolve_ext_ids_for_drive`](Self::resolve_ext_ids_for_drive)
    /// before the hot loop — enables O(1) `u16` comparison per record
    /// instead of per-record string parsing.
    pub resolved_ext_ids: Vec<u16>,
    /// Exclude pattern (glob, lowered).
    pub exclude_lower: Option<String>,
    /// Directory-path pattern (glob, lowered). Matches against `path_dir()`
    /// only.
    pub path_contains_lower: Option<String>,
    /// File type/category filter (e.g. `"code"`, `"document"`, `"picture"`).
    pub type_filter: Option<String>,
    /// Minimum bulkiness in **per-million** scale.
    ///
    /// `1_000_000` = 100% (perfectly packed).  `2_000_000` = 200%.
    /// CLI percentages must be converted via `from_params` which multiplies
    /// by 10 000.
    pub min_bulkiness: Option<u64>,
    /// Maximum bulkiness in **per-million** scale (see [`Self::min_bulkiness`]).
    pub max_bulkiness: Option<u64>,

    // ── Length filters ──────────────────────────────────────────────
    /// Minimum filename length (characters).
    pub min_name_len: Option<u16>,
    /// Maximum filename length (characters).
    pub max_name_len: Option<u16>,
    /// Minimum full-path length (characters).
    pub min_path_len: Option<u16>,
    /// Maximum full-path length (characters, useful for `MAX_PATH` detection).
    pub max_path_len: Option<u16>,

    // ── Size-on-disk filters ───────────────────────────────────────
    /// Minimum allocated (on-disk) size in bytes.
    pub min_allocated: Option<u64>,
    /// Maximum allocated (on-disk) size in bytes.
    pub max_allocated: Option<u64>,

    // ── Tree metric filters ─────────────────────────────────────────
    /// Minimum subtree logical size in bytes (directories).
    pub min_treesize: Option<u64>,
    /// Maximum subtree logical size in bytes (directories).
    pub max_treesize: Option<u64>,
    /// Minimum subtree allocated (on-disk) size in bytes (directories).
    pub min_tree_allocated: Option<u64>,
    /// Maximum subtree allocated (on-disk) size in bytes (directories).
    pub max_tree_allocated: Option<u64>,

    // ── Month-of-year / quarter filter ─────────────────────────────
    /// Set of allowed months (1-12). Empty = no filter.
    /// Used for "every January" or "Q1" style queries.
    pub allowed_months: Vec<u32>,
}

/// Raw parameter inputs for constructing [`SearchFilters`].
///
/// All fields default to `None` / `false` / empty — callers only set what
/// they need.  This replaces the former 27-argument positional function
/// signature with a named-field struct that is self-documenting and
/// extensible without touching every call site.
#[derive(Debug, Default)]
pub struct SearchFilterParams<'a> {
    /// Hide system files (names starting with `$`).
    pub hide_system: bool,
    /// Hide NTFS Alternate Data Streams.
    pub hide_ads: bool,
    /// Minimum file size in bytes.
    pub min_size: Option<u64>,
    /// Maximum file size in bytes.
    pub max_size: Option<u64>,
    /// Minimum descendant count (directories).
    pub min_descendants: Option<u32>,
    /// Maximum descendant count (directories).
    pub max_descendants: Option<u32>,
    /// Modified-time lower bound spec (e.g. `"1h"`, `"2024-01-01"`).
    pub newer: Option<&'a str>,
    /// Modified-time upper bound spec.
    pub older: Option<&'a str>,
    /// Created-time lower bound spec.
    pub newer_created: Option<&'a str>,
    /// Created-time upper bound spec.
    pub older_created: Option<&'a str>,
    /// Accessed-time lower bound spec.
    pub newer_accessed: Option<&'a str>,
    /// Accessed-time upper bound spec.
    pub older_accessed: Option<&'a str>,
    /// NTFS attribute filter string (e.g. `"hidden,!system"`).
    pub attr_filter: Option<&'a str>,
    /// Extension filter string (e.g. `"rs,jpg,pictures"`).
    pub ext_filter: Option<&'a str>,
    /// Exclude pattern (glob, e.g. `"backup*"`).
    pub exclude: Option<&'a str>,
    /// Directory-path pattern (glob, matched against dir portion only).
    pub path_contains: Option<&'a str>,
    /// File type/category filter (e.g. `"code"`, `"document"`).
    pub type_filter: Option<&'a str>,
    /// Minimum bulkiness percentage (e.g. `200` = allocated ≥ 2× size).
    pub min_bulkiness: Option<u64>,
    /// Maximum bulkiness percentage.
    pub max_bulkiness: Option<u64>,
    /// Minimum filename length (characters).
    pub min_name_len: Option<u16>,
    /// Maximum filename length (characters).
    pub max_name_len: Option<u16>,
    /// Minimum full-path length (characters).
    pub min_path_len: Option<u16>,
    /// Maximum full-path length (characters).
    pub max_path_len: Option<u16>,
    /// Minimum allocated (on-disk) size in bytes.
    pub min_allocated: Option<u64>,
    /// Maximum allocated (on-disk) size in bytes.
    pub max_allocated: Option<u64>,
    /// Minimum subtree logical size in bytes.
    pub min_treesize: Option<u64>,
    /// Maximum subtree logical size in bytes.
    pub max_treesize: Option<u64>,
    /// Minimum subtree allocated (on-disk) size in bytes.
    pub min_tree_allocated: Option<u64>,
    /// Maximum subtree allocated (on-disk) size in bytes.
    pub max_tree_allocated: Option<u64>,
    /// Allowed month numbers (1-12).
    pub allowed_months: &'a [u32],
}

impl SearchFilters {
    /// Build `SearchFilters` from a [`SearchFilterParams`] struct.
    ///
    /// This is the generic constructor shared by CLI, TUI, daemon, etc.
    /// All time-spec parsing and attribute parsing happens here so the
    /// hot-path `matches_record` loop is branch-only.
    #[must_use]
    pub fn from_params(params: &SearchFilterParams<'_>) -> Self {
        let now_us = now_unix_micros();
        let extensions: Vec<String> = params
            .ext_filter
            .map(|ext_list| {
                let mut exts = Vec::new();
                for segment in ext_list.split(',') {
                    let token = segment.trim().trim_start_matches('.').to_lowercase();
                    if token.is_empty() {
                        continue;
                    }
                    if let Some(collection) = crate::extensions::expand_collection(&token) {
                        exts.extend(collection.iter().map(|ext| (*ext).to_owned()));
                    } else {
                        exts.push(token);
                    }
                }
                exts
            })
            .unwrap_or_default();
        if !extensions.is_empty() {
            tracing::trace!(
                raw_ext_filter = params.ext_filter.unwrap_or_default(),
                normalized_extensions = ?extensions,
                "normalized extension filter strings"
            );
        }
        let fold_table = uffs_text::CaseFold::default_table();
        let exclude_lower = params.exclude.map(|excl| {
            let mut buf = Vec::with_capacity(excl.len());
            fold_table.fold_into(excl, &mut buf).to_owned()
        });
        let path_contains_lower = params.path_contains.map(|pat| {
            let mut buf = Vec::with_capacity(pat.len());
            fold_table.fold_into(pat, &mut buf).to_owned()
        });

        // ── Promote type_filter → extensions for early filtering ─────
        //
        // When the type maps to a known extension list (e.g. "code" →
        // [rs, py, js, …]) we push those extensions into `extensions` so
        // that `matches_record` can filter records during the scan (O(1)
        // per record via ext-index) instead of the expensive post-filter
        // path that requires full path resolution for every candidate.
        //
        // If --ext was also provided, the type list is a superset — we
        // intersect them so only extensions satisfying BOTH constraints
        // survive.
        //
        // Un-mappable types ("directory", "file", "other") stay as
        // `type_filter` for post-filter via `apply_search_filters`.
        let (extensions, type_filter) = if let Some(type_name) = params.type_filter {
            let lower = type_name.to_ascii_lowercase();
            if let Some(type_exts) =
                crate::search::derived::extensions_for_type(&lower)
            {
                let merged = if extensions.is_empty() {
                    // No --ext: use the full type extension list.
                    type_exts.iter().map(|e| (*e).to_owned()).collect()
                } else {
                    // --ext present: intersect (keep only exts that
                    // belong to BOTH the explicit list and the type).
                    extensions
                        .into_iter()
                        .filter(|e| type_exts.contains(&e.as_str()))
                        .collect()
                };
                (merged, None)
            } else {
                // Un-mappable type (directory/file/other) — keep post-filter.
                (extensions, Some(lower))
            }
        } else {
            (extensions, None)
        };

        Self {
            hide_system: params.hide_system,
            hide_ads: params.hide_ads,
            min_size: params.min_size,
            max_size: params.max_size,
            newer_us: params
                .newer
                .and_then(|spec| parse_time_bound(spec, now_us, true)),
            older_us: params
                .older
                .and_then(|spec| parse_time_bound(spec, now_us, false)),
            newer_created_us: params
                .newer_created
                .and_then(|spec| parse_time_bound(spec, now_us, true)),
            older_created_us: params
                .older_created
                .and_then(|spec| parse_time_bound(spec, now_us, false)),
            newer_accessed_us: params
                .newer_accessed
                .and_then(|spec| parse_time_bound(spec, now_us, true)),
            older_accessed_us: params
                .older_accessed
                .and_then(|spec| parse_time_bound(spec, now_us, false)),
            attr_require: parse_attr_require(params.attr_filter.unwrap_or("")),
            attr_exclude: parse_attr_exclude(params.attr_filter.unwrap_or("")),
            min_descendants: params.min_descendants,
            max_descendants: params.max_descendants,
            extensions,
            resolved_ext_ids: Vec::new(),
            exclude_lower,
            path_contains_lower,
            type_filter,
            // CLI bulkiness is a user-facing percentage (200 = 200%).
            // Internal scale is per-million (1_000_000 = 100%).
            // Convert: percentage × 10_000 = per-million.
            min_bulkiness: params.min_bulkiness.map(|p| p.saturating_mul(10_000)),
            max_bulkiness: params.max_bulkiness.map(|p| p.saturating_mul(10_000)),
            min_name_len: params.min_name_len,
            max_name_len: params.max_name_len,
            min_path_len: params.min_path_len,
            max_path_len: params.max_path_len,
            min_allocated: params.min_allocated,
            max_allocated: params.max_allocated,
            min_treesize: params.min_treesize,
            max_treesize: params.max_treesize,
            min_tree_allocated: params.min_tree_allocated,
            max_tree_allocated: params.max_tree_allocated,
            allowed_months: params.allowed_months.to_vec(),
        }
    }

    /// Set the minimum filename length filter.
    #[must_use]
    pub const fn with_min_name_len(mut self, len: Option<u16>) -> Self {
        self.min_name_len = len;
        self
    }

    /// Set the maximum filename length filter.
    #[must_use]
    pub const fn with_max_name_len(mut self, len: Option<u16>) -> Self {
        self.max_name_len = len;
        self
    }

    /// Set the minimum full-path length filter.
    #[must_use]
    pub const fn with_min_path_len(mut self, len: Option<u16>) -> Self {
        self.min_path_len = len;
        self
    }

    /// Set the maximum full-path length filter.
    #[must_use]
    pub const fn with_max_path_len(mut self, len: Option<u16>) -> Self {
        self.max_path_len = len;
        self
    }

    /// Set the minimum allocated (on-disk) size filter.
    #[must_use]
    pub const fn with_min_allocated(mut self, size: Option<u64>) -> Self {
        self.min_allocated = size;
        self
    }

    /// Set the maximum allocated (on-disk) size filter.
    #[must_use]
    pub const fn with_max_allocated(mut self, size: Option<u64>) -> Self {
        self.max_allocated = size;
        self
    }

    /// Set the allowed months filter (1-12).
    #[must_use]
    pub fn with_allowed_months(mut self, months: Vec<u32>) -> Self {
        self.allowed_months = months;
        self
    }

    /// Pre-resolve extension filter strings to `u16` IDs for a specific
    /// drive.  Call this **once per drive** before the hot record loop.
    pub fn resolve_ext_ids_for_drive(&mut self, drive: &crate::compact::DriveCompactIndex) {
        if self.extensions.is_empty() {
            self.resolved_ext_ids.clear();
            tracing::trace!(drive = %drive.letter, "no extension filter active for drive");
            return;
        }

        self.resolved_ext_ids = drive.resolve_ext_ids(&self.extensions);

        let requested_lower = self
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
            requested_extensions = ?self.extensions,
            requested_lowercase = ?requested_lower,
            resolved_ext_ids = ?self.resolved_ext_ids,
            lowercase_only_hits = ?lowercase_only_hits,
            ext_name_count = drive.ext_names.len(),
            ext_name_sample = ?sample_ext_names,
            "extension filter resolution for drive"
        );
    }

    /// Returns `true` when the only active filter is `extensions` — no
    /// size, date, attr, exclude, descendant, or system-hide constraints.
    /// When this is true and the pattern is match-all (`*`), we can use
    /// the extension inverted index for O(K) iteration instead of O(N).
    #[must_use]
    pub const fn is_ext_only(&self) -> bool {
        !self.extensions.is_empty()
            && !self.hide_system
            && !self.hide_ads
            && self.min_size.is_none()
            && self.max_size.is_none()
            && self.newer_us.is_none()
            && self.older_us.is_none()
            && self.newer_created_us.is_none()
            && self.older_created_us.is_none()
            && self.newer_accessed_us.is_none()
            && self.older_accessed_us.is_none()
            && self.attr_require == 0
            && self.attr_exclude == 0
            && self.min_descendants.is_none()
            && self.max_descendants.is_none()
            && self.exclude_lower.is_none()
            && self.path_contains_lower.is_none()
            && self.type_filter.is_none()
            && self.min_bulkiness.is_none()
            && self.max_bulkiness.is_none()
            && self.min_name_len.is_none()
            && self.max_name_len.is_none()
            && self.min_path_len.is_none()
            && self.max_path_len.is_none()
            && self.min_allocated.is_none()
            && self.max_allocated.is_none()
            && self.min_treesize.is_none()
            && self.max_treesize.is_none()
            && self.min_tree_allocated.is_none()
            && self.max_tree_allocated.is_none()
            && self.allowed_months.is_empty()
    }

    /// Check whether a compact record passes all filters.
    ///
    /// Hot-path predicate used during global top-N scans.
    ///
    /// `fold_buf` is a caller-owned reusable buffer for on-the-fly
    /// `CaseFold` folding (avoids per-record heap allocation for exclude
    /// matching).
    #[must_use]
    pub fn matches_record(
        &self,
        rec: &CompactRecord,
        names: &[u8],
        fold_buf: &mut Vec<u8>,
        fold: uffs_text::CaseFold,
    ) -> bool {
        if self.hide_system || self.hide_ads {
            let name = rec.name(names);
            if self.hide_system && name.starts_with('$') {
                return false;
            }
            if self.hide_ads && memchr::memchr(b':', name.as_bytes()).is_some() {
                return false;
            }
        }
        if let Some(min) = self.min_size
            && rec.size < min
        {
            return false;
        }
        if let Some(max) = self.max_size
            && rec.size > max
        {
            return false;
        }
        if let Some(bound) = self.newer_us
            && rec.modified < bound
        {
            return false;
        }
        if let Some(bound) = self.older_us
            && rec.modified >= bound
        {
            return false;
        }
        if let Some(bound) = self.newer_created_us
            && rec.created < bound
        {
            return false;
        }
        if let Some(bound) = self.older_created_us
            && rec.created >= bound
        {
            return false;
        }
        if let Some(bound) = self.newer_accessed_us
            && rec.accessed < bound
        {
            return false;
        }
        if let Some(bound) = self.older_accessed_us
            && rec.accessed >= bound
        {
            return false;
        }
        if self.attr_require != 0 && (rec.flags & self.attr_require) != self.attr_require {
            return false;
        }
        if self.attr_exclude != 0 && (rec.flags & self.attr_exclude) != 0 {
            return false;
        }
        if let Some(min) = self.min_descendants
            && rec.descendants < min
        {
            return false;
        }
        if let Some(max) = self.max_descendants
            && rec.descendants > max
        {
            return false;
        }
        if !self.resolved_ext_ids.is_empty() {
            // Fast path: compare pre-resolved u16 IDs (O(1) per record).
            if !self.resolved_ext_ids.contains(&rec.extension_id) {
                return false;
            }
        } else if !self.extensions.is_empty() {
            // Fallback for callers that did not call resolve_ext_ids_for_drive.
            let name = rec.name(names);
            let ext = name.rsplit('.').next().unwrap_or("");
            let normalized_ext = lowercase_into(ext, fold_buf);
            if !self
                .extensions
                .iter()
                .any(|allowed| extension_matches_filter(allowed, normalized_ext))
            {
                return false;
            }
        }
        if let Some(excl) = &self.exclude_lower {
            // Zero-alloc via CaseFold: fold the name into a reusable buffer.
            let name = rec.name(names);
            let folded_name = fold.fold_into(name, fold_buf);
            if name_matches(folded_name, excl) {
                return false;
            }
        }
        self.matches_derived(rec, names)
    }

    /// Check derived/computed filters: name length, allocated, tree metrics,
    /// month.
    ///
    /// Split from [`matches_record`] to keep each function under the
    /// `too_many_lines` lint threshold.
    fn matches_derived(&self, rec: &CompactRecord, names: &[u8]) -> bool {
        // ── Name-length filters (chars, not bytes) ─────────────────
        if self.min_name_len.is_some() || self.max_name_len.is_some() {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "filenames on NTFS are ≤255 chars, fits u16"
            )]
            let name_len = rec.name(names).chars().count() as u16;
            if let Some(min) = self.min_name_len
                && name_len < min
            {
                return false;
            }
            if let Some(max) = self.max_name_len
                && name_len > max
            {
                return false;
            }
        }
        // ── Size-on-disk filters ───────────────────────────────────
        if let Some(min) = self.min_allocated
            && rec.allocated < min
        {
            return false;
        }
        if let Some(max) = self.max_allocated
            && rec.allocated > max
        {
            return false;
        }
        // ── Tree metric filters ─────────────────────────────────────
        if let Some(min) = self.min_treesize
            && rec.treesize < min
        {
            return false;
        }
        if let Some(max) = self.max_treesize
            && rec.treesize > max
        {
            return false;
        }
        if let Some(min) = self.min_tree_allocated
            && rec.tree_allocated < min
        {
            return false;
        }
        if let Some(max) = self.max_tree_allocated
            && rec.tree_allocated > max
        {
            return false;
        }
        // ── Bulkiness filters (scan-level, no path needed) ────────
        if self.min_bulkiness.is_some() || self.max_bulkiness.is_some() {
            let (logical, allocated) = if rec.is_directory() {
                (rec.treesize, rec.tree_allocated)
            } else {
                (rec.size, rec.allocated)
            };
            let bulk = if logical == 0 {
                0
            } else {
                allocated.saturating_mul(crate::search::derived::BULKINESS_SCALE) / logical
            };
            if let Some(min) = self.min_bulkiness
                && bulk < min
            {
                return false;
            }
            if let Some(max) = self.max_bulkiness
                && bulk > max
            {
                return false;
            }
        }
        // ── Path-length filters (precomputed on CompactRecord) ────
        if let Some(min) = self.min_path_len
            && rec.path_len < min
        {
            return false;
        }
        if let Some(max) = self.max_path_len
            && rec.path_len > max
        {
            return false;
        }
        // ── Month-of-year filter ───────────────────────────────────
        if !self.allowed_months.is_empty() {
            let month = month_from_unix_micros(rec.modified);
            if !self.allowed_months.contains(&month) {
                return false;
            }
        }
        true
    }

    /// Returns `true` if all filters are at their default (no-op) values.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        !self.hide_system
            && !self.hide_ads
            && self.min_size.is_none()
            && self.max_size.is_none()
            && self.newer_us.is_none()
            && self.older_us.is_none()
            && self.newer_created_us.is_none()
            && self.older_created_us.is_none()
            && self.newer_accessed_us.is_none()
            && self.older_accessed_us.is_none()
            && self.attr_require == 0
            && self.attr_exclude == 0
            && self.min_descendants.is_none()
            && self.max_descendants.is_none()
            && self.extensions.is_empty()
            && self.exclude_lower.is_none()
            && self.path_contains_lower.is_none()
            && self.type_filter.is_none()
            && self.min_bulkiness.is_none()
            && self.max_bulkiness.is_none()
            && self.min_name_len.is_none()
            && self.max_name_len.is_none()
            && self.min_path_len.is_none()
            && self.max_path_len.is_none()
            && self.min_allocated.is_none()
            && self.max_allocated.is_none()
            && self.min_treesize.is_none()
            && self.max_treesize.is_none()
            && self.min_tree_allocated.is_none()
            && self.max_tree_allocated.is_none()
            && self.allowed_months.is_empty()
    }

    /// Returns `true` if any filter requires a resolved `DisplayRow`
    /// (full path, semantic type).
    ///
    /// These cannot be evaluated on a `CompactRecord` because they need
    /// the resolved path string.  The `collect_global_top_n` hot-path
    /// only runs `matches_record`; callers must run
    /// `apply_search_filters` afterwards when this returns `true`.
    ///
    /// Note: bulkiness and path-length are checked at scan level in
    /// `matches_record` (bulkiness uses `size`/`allocated`, path-length
    /// uses the precomputed `path_len` field on `CompactRecord`).
    #[must_use]
    pub const fn needs_display_row_filter(&self) -> bool {
        self.path_contains_lower.is_some() || self.type_filter.is_some()
    }
}

/// Apply extended search filters to display rows (in-place).
pub fn apply_search_filters(rows: &mut Vec<DisplayRow>, filters: &SearchFilters) {
    if filters.is_empty() {
        return;
    }
    let fold = uffs_text::CaseFold::default_table();
    let mut fold_buf: Vec<u8> = Vec::with_capacity(256);
    rows.retain(|row| {
        if filters.hide_system && row.name().starts_with('$') {
            return false;
        }
        if filters.hide_ads && row.name().contains(':') {
            return false;
        }
        if let Some(min) = filters.min_size
            && row.size < min
        {
            return false;
        }
        if let Some(max) = filters.max_size
            && row.size > max
        {
            return false;
        }
        if let Some(bound) = filters.newer_us
            && row.modified < bound
        {
            return false;
        }
        if let Some(bound) = filters.older_us
            && row.modified >= bound
        {
            return false;
        }
        if let Some(bound) = filters.newer_created_us
            && row.created < bound
        {
            return false;
        }
        if let Some(bound) = filters.older_created_us
            && row.created >= bound
        {
            return false;
        }
        if let Some(bound) = filters.newer_accessed_us
            && row.accessed < bound
        {
            return false;
        }
        if let Some(bound) = filters.older_accessed_us
            && row.accessed >= bound
        {
            return false;
        }
        if filters.attr_require != 0 && (row.flags & filters.attr_require) != filters.attr_require {
            return false;
        }
        if filters.attr_exclude != 0 && (row.flags & filters.attr_exclude) != 0 {
            return false;
        }
        if let Some(min) = filters.min_descendants
            && row.descendants < min
        {
            return false;
        }
        if let Some(max) = filters.max_descendants
            && row.descendants > max
        {
            return false;
        }
        if !filters.extensions.is_empty() {
            let ext = row.name().rsplit('.').next().unwrap_or("");
            let normalized_ext = lowercase_into(ext, &mut fold_buf);
            if !filters
                .extensions
                .iter()
                .any(|allowed| extension_matches_filter(allowed, normalized_ext))
            {
                return false;
            }
        }
        if let Some(excl) = &filters.exclude_lower {
            let folded_name = fold.fold_into(row.name(), &mut fold_buf);
            if name_matches(folded_name, excl) {
                return false;
            }
        }
        apply_derived_filters(row, filters)
    });
}

/// Derived / post-filter checks for `apply_search_filters`.
///
/// Extracted to keep the retain closure under the `cognitive_complexity`
/// and `too_many_lines` lint thresholds — a 105-line helper is clearer
/// than inlining into the already-complex `retain` closure.
#[expect(
    clippy::single_call_fn,
    reason = "factored out for cognitive_complexity + too_many_lines"
)]
fn apply_derived_filters(row: &DisplayRow, filters: &SearchFilters) -> bool {
    // ── Name-length filters ────────────────────────────────────
    if filters.min_name_len.is_some() || filters.max_name_len.is_some() {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "NTFS filenames ≤255 chars, fits u16"
        )]
        let name_len = row.name().chars().count() as u16;
        if let Some(min) = filters.min_name_len
            && name_len < min
        {
            return false;
        }
        if let Some(max) = filters.max_name_len
            && name_len > max
        {
            return false;
        }
    }
    // ── Path-length filters ────────────────────────────────────
    // Note: path_len is measured in Unicode characters, consistent with
    // the precomputed `CompactRecord::path_len` used at scan level.
    if filters.min_path_len.is_some() || filters.max_path_len.is_some() {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "path lengths fit u16 for practical NTFS volumes"
        )]
        let path_len = row.path.chars().count() as u16;
        if let Some(min) = filters.min_path_len
            && path_len < min
        {
            return false;
        }
        if let Some(max) = filters.max_path_len
            && path_len > max
        {
            return false;
        }
    }
    // ── Directory-path pattern filter ───────────────────────────
    if let Some(pat) = &filters.path_contains_lower {
        let dir = row.path_dir();
        let dir_lower = dir.to_ascii_lowercase();
        if !name_matches(&dir_lower, pat) {
            return false;
        }
    }
    // ── Type/category filter ────────────────────────────────────
    if let Some(wanted) = &filters.type_filter
        && crate::search::derived::semantic_type_for_row(row) != wanted.as_str()
    {
        return false;
    }
    // ── Bulkiness filters ───────────────────────────────────────
    if filters.min_bulkiness.is_some() || filters.max_bulkiness.is_some() {
        let bulk = crate::search::derived::bulkiness_for_row(row);
        if let Some(min) = filters.min_bulkiness
            && bulk < min
        {
            return false;
        }
        if let Some(max) = filters.max_bulkiness
            && bulk > max
        {
            return false;
        }
    }
    // ── Size-on-disk filters ───────────────────────────────────
    if let Some(min) = filters.min_allocated
        && row.allocated < min
    {
        return false;
    }
    if let Some(max) = filters.max_allocated
        && row.allocated > max
    {
        return false;
    }
    // ── Tree metric filters ─────────────────────────────────────
    if let Some(min) = filters.min_treesize
        && row.treesize < min
    {
        return false;
    }
    if let Some(max) = filters.max_treesize
        && row.treesize > max
    {
        return false;
    }
    if let Some(min) = filters.min_tree_allocated
        && row.tree_allocated < min
    {
        return false;
    }
    if let Some(max) = filters.max_tree_allocated
        && row.tree_allocated > max
    {
        return false;
    }
    // ── Month-of-year filter ───────────────────────────────────
    if !filters.allowed_months.is_empty() {
        let month = month_from_unix_micros(row.modified);
        if !filters.allowed_months.contains(&month) {
            return false;
        }
    }
    true
}

// ═══════════════════════════════════════════════════════════════════════════
// Month extraction
// ═══════════════════════════════════════════════════════════════════════════

/// Extract the month (1-12) from a Unix-microsecond timestamp.
///
/// Uses civil-time decomposition without any external crate.
///
/// ```
/// # use uffs_core::search::filters::month_from_unix_micros;
/// // 2026-01-15 00:00:00 UTC → January
/// assert_eq!(month_from_unix_micros(1_768_435_200_000_000), 1);
/// // 2026-07-01 00:00:00 UTC → July
/// assert_eq!(month_from_unix_micros(1_782_864_000_000_000), 7);
/// ```
#[must_use]
pub const fn month_from_unix_micros(us: i64) -> u32 {
    // Convert µs → days since Unix epoch.
    let total_secs = us / 1_000_000;
    // Integer floor-division that rounds towards −∞.
    let day = if total_secs >= 0 {
        total_secs / 86400
    } else {
        (total_secs - 86399) / 86400
    };
    // Civil date from day count (algorithm from Howard Hinnant).
    // <https://howardhinnant.github.io/date_algorithms.html#civil_from_days>
    let z = day + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // day-of-era  [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day-of-year [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let civil_month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "month is always 1-12"
    )]
    let month = civil_month as u32;
    month
}

/// Parse a month/quarter spec into a vector of allowed months (1-12).
///
/// Accepts:
/// - Month names: `january`, `jan`, `february`, `feb`, … , `december`, `dec`
/// - Quarter names: `Q1`, `Q2`, `Q3`, `Q4`
/// - Comma-separated combinations: `jan,feb`, `Q1,Q3`
///
/// ```
/// # use uffs_core::search::filters::parse_month_spec;
/// assert_eq!(parse_month_spec("january"), vec![1]);
/// assert_eq!(parse_month_spec("Q1"), vec![1, 2, 3]);
/// assert_eq!(parse_month_spec("jan,feb"), vec![1, 2]);
/// assert_eq!(parse_month_spec("Q2,october"), vec![4, 5, 6, 10]);
/// ```
#[must_use]
pub fn parse_month_spec(spec: &str) -> Vec<u32> {
    let mut months = Vec::new();
    for token in spec.split(',') {
        let lower = token.trim().to_ascii_lowercase();
        match lower.as_str() {
            "january" | "jan" => months.push(1),
            "february" | "feb" => months.push(2),
            "march" | "mar" => months.push(3),
            "april" | "apr" => months.push(4),
            "may" => months.push(5),
            "june" | "jun" => months.push(6),
            "july" | "jul" => months.push(7),
            "august" | "aug" => months.push(8),
            "september" | "sep" => months.push(9),
            "october" | "oct" => months.push(10),
            "november" | "nov" => months.push(11),
            "december" | "dec" => months.push(12),
            "q1" => months.extend_from_slice(&[1, 2, 3]),
            "q2" => months.extend_from_slice(&[4, 5, 6]),
            "q3" => months.extend_from_slice(&[7, 8, 9]),
            "q4" => months.extend_from_slice(&[10, 11, 12]),
            _ => {} // silently ignore unknown tokens
        }
    }
    months.sort_unstable();
    months.dedup();
    months
}

// ═══════════════════════════════════════════════════════════════════════════
// Size parsing helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Parse a human-readable size string into bytes.
///
/// Accepts plain integers (bytes) and suffixes: `B`, `KB`, `MB`, `GB`, `TB`.
/// The suffix is **case-insensitive**.  A bare number with no suffix is
/// treated as bytes.
///
/// # Errors
///
/// Returns `Err` if the spec is empty, contains non-numeric characters
/// (after stripping the suffix), or the result overflows `u64`.
///
/// # Examples
///
/// ```
/// # use uffs_core::search::filters::parse_size;
/// assert_eq!(parse_size("1024"), Ok(1024));
/// assert_eq!(parse_size("1KB"), Ok(1024));
/// assert_eq!(parse_size("10mb"), Ok(10 * 1024 * 1024));
/// assert_eq!(parse_size("1GB"), Ok(1024 * 1024 * 1024));
/// assert_eq!(parse_size("2TB"), Ok(2 * 1024 * 1024 * 1024 * 1024));
/// assert_eq!(parse_size("0"), Ok(0));
/// assert!(parse_size("abc").is_err());
/// ```
pub fn parse_size(spec: &str) -> Result<u64, String> {
    // Suffix table: longest-first to avoid prefix ambiguity.
    const SUFFIXES: &[(&str, u64)] = &[
        ("TB", 1024 * 1024 * 1024 * 1024),
        ("GB", 1024 * 1024 * 1024),
        ("MB", 1024 * 1024),
        ("KB", 1024),
        ("B", 1),
    ];

    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err("empty size specification".to_owned());
    }

    let upper = trimmed.to_ascii_uppercase();

    let (digits, multiplier) = SUFFIXES
        .iter()
        .find_map(|(suffix, mult)| upper.strip_suffix(suffix).map(|rest| (rest, *mult)))
        .unwrap_or((upper.as_str(), 1));

    let count: u64 = digits
        .trim()
        .parse()
        .map_err(|_parse_err| format!("invalid size: {spec}"))?;

    count
        .checked_mul(multiplier)
        .ok_or_else(|| format!("size overflows u64: {spec}"))
}

// ═══════════════════════════════════════════════════════════════════════════
// Time / attribute parsing helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Current time as Unix microseconds.
#[must_use]
pub fn now_unix_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |dur| {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "micros since epoch fits i64 until year ~292,277"
            )]
            let us = dur.as_micros() as i64;
            us
        })
}

/// Parse a time bound string into Unix microseconds.
///
/// Supports:
/// - **Duration:** `7d`, `24h`, `30m`, `90s`, `2w`
/// - **ISO date:** `2026-01-15`
/// - **Named ranges:** `today`, `yesterday`, `this_week`, `last_week`,
///   `this_month`, `last_month`, `this_year`, `last_year`, `last_7d`,
///   `last_30d`, `last_90d`, `last_365d`, `ytd`
#[must_use]
pub fn parse_time_bound(spec: &str, now_us: i64, is_newer: bool) -> Option<i64> {
    let trimmed = spec.trim();

    // ── Named time ranges ──────────────────────────────────────────
    if let Some(ts) = parse_named_time_range(trimmed, now_us, is_newer) {
        return Some(ts);
    }

    // ── Duration suffix (e.g. "7d", "24h") ─────────────────────────
    if trimmed.len() >= 2 {
        let (num_str, suffix) = trimmed.split_at(trimmed.len() - 1);
        if let Ok(count) = num_str.parse::<i64>() {
            let micros_per_sec: i64 = 1_000_000;
            let delta = match suffix {
                "s" => count * micros_per_sec,
                "m" => count * 60 * micros_per_sec,
                "h" => count * 3600 * micros_per_sec,
                "d" => count * 86400 * micros_per_sec,
                "w" => count * 7 * 86400 * micros_per_sec,
                _ => return None,
            };
            return Some(now_us - delta);
        }
    }

    // ── ISO date (YYYY-MM-DD) ──────────────────────────────────────
    parse_iso_date(trimmed)
}

/// Parse an ISO date string (`YYYY-MM-DD`) into Unix microseconds at midnight.
///
/// Extracted for readability — `parse_time_bound` dispatches to named ranges,
/// duration suffixes, and this ISO parser.
#[allow(clippy::single_call_fn)]
fn parse_iso_date(trimmed: &str) -> Option<i64> {
    if trimmed.len() == 10 && trimmed.as_bytes().get(4) == Some(&b'-') {
        let parts: Vec<&str> = trimmed.split('-').collect();
        if let [year_s, month_s, day_s] = parts.as_slice()
            && let (Ok(year), Ok(month), Ok(day)) = (
                year_s.parse::<i64>(),
                month_s.parse::<i64>(),
                day_s.parse::<i64>(),
            )
        {
            let days = ymd_to_days(year, month, day);
            return Some(days * US_PER_DAY);
        }
    }
    None
}

/// Microseconds per day.
const US_PER_DAY: i64 = 86_400 * 1_000_000;

/// Resolve a named time range to Unix microseconds.
///
/// For `is_newer = true`, returns the start of the range (lower bound).
/// For `is_newer = false`, returns the end of the range (upper bound).
///
/// Extracted for readability — the 15 named range cases would make
/// `parse_time_bound` exceed the `too_many_lines` threshold.
#[allow(clippy::single_call_fn, clippy::too_many_lines)]
fn parse_named_time_range(name: &str, now_us: i64, is_newer: bool) -> Option<i64> {
    let today_start = now_us - (now_us % US_PER_DAY);

    match name.to_ascii_lowercase().as_str() {
        "today" => Some(today_start),
        "yesterday" => {
            if is_newer {
                Some(today_start - US_PER_DAY)
            } else {
                Some(today_start)
            }
        }
        "this_week" | "thisweek" => {
            // Go back to most recent Monday (Unix epoch was Thursday).
            let days_since_epoch = today_start / US_PER_DAY;
            let dow = (days_since_epoch + 3) % 7; // 0=Mon, 6=Sun
            Some(today_start - dow * US_PER_DAY)
        }
        "last_week" | "lastweek" => {
            let days_since_epoch = today_start / US_PER_DAY;
            let dow = (days_since_epoch + 3) % 7;
            let this_monday = today_start - dow * US_PER_DAY;
            if is_newer {
                Some(this_monday - 7 * US_PER_DAY)
            } else {
                Some(this_monday)
            }
        }
        "this_month" | "thismonth" => {
            let days_since_epoch = today_start / US_PER_DAY;
            let (_, _, day) = days_to_ymd(days_since_epoch);
            Some(today_start - (day - 1) * US_PER_DAY)
        }
        "last_month" | "lastmonth" => {
            let days_since_epoch = today_start / US_PER_DAY;
            let (year, month, day) = days_to_ymd(days_since_epoch);
            let this_month_start = today_start - (day - 1) * US_PER_DAY;
            if is_newer {
                let (prev_year, prev_month) = if month == 1 {
                    (year - 1, 12)
                } else {
                    (year, month - 1)
                };
                let prev_days = days_in_month(prev_year, prev_month);
                Some(this_month_start - prev_days * US_PER_DAY)
            } else {
                Some(this_month_start)
            }
        }
        "this_year" | "thisyear" | "ytd" => {
            let days_since_epoch = today_start / US_PER_DAY;
            let (year, _, _) = days_to_ymd(days_since_epoch);
            Some(ymd_to_days(year, 1, 1) * US_PER_DAY)
        }
        "last_year" | "lastyear" => {
            let days_since_epoch = today_start / US_PER_DAY;
            let (year, _, _) = days_to_ymd(days_since_epoch);
            if is_newer {
                Some(ymd_to_days(year - 1, 1, 1) * US_PER_DAY)
            } else {
                Some(ymd_to_days(year, 1, 1) * US_PER_DAY)
            }
        }
        // last_Nd shortcuts
        "last_7d" | "last7d" => Some(now_us - 7 * US_PER_DAY),
        "last_30d" | "last30d" => Some(now_us - 30 * US_PER_DAY),
        "last_90d" | "last90d" => Some(now_us - 90 * US_PER_DAY),
        "last_365d" | "last365d" => Some(now_us - 365 * US_PER_DAY),
        // next_* periods — for finding files with future timestamps
        // (clock skew, timezone issues, scheduled items).
        "next_day" | "nextday" | "tomorrow" => {
            if is_newer {
                Some(today_start + US_PER_DAY)
            } else {
                Some(today_start + 2 * US_PER_DAY)
            }
        }
        "next_week" | "nextweek" => {
            let days_since_epoch = today_start / US_PER_DAY;
            let dow = (days_since_epoch + 3) % 7;
            let this_monday = today_start - dow * US_PER_DAY;
            let next_monday = this_monday + 7 * US_PER_DAY;
            if is_newer {
                Some(next_monday)
            } else {
                Some(next_monday + 7 * US_PER_DAY)
            }
        }
        "next_month" | "nextmonth" => {
            let days_since_epoch = today_start / US_PER_DAY;
            let (year, month, day) = days_to_ymd(days_since_epoch);
            let this_month_start = today_start - (day - 1) * US_PER_DAY;
            let days = days_in_month(year, month);
            let next_month_start = this_month_start + days * US_PER_DAY;
            if is_newer {
                Some(next_month_start)
            } else {
                let (ny, nm) = if month == 12 {
                    (year + 1, 1)
                } else {
                    (year, month + 1)
                };
                Some(next_month_start + days_in_month(ny, nm) * US_PER_DAY)
            }
        }
        "next_year" | "nextyear" => {
            let days_since_epoch = today_start / US_PER_DAY;
            let (year, _, _) = days_to_ymd(days_since_epoch);
            if is_newer {
                Some(ymd_to_days(year + 1, 1, 1) * US_PER_DAY)
            } else {
                Some(ymd_to_days(year + 2, 1, 1) * US_PER_DAY)
            }
        }
        _ => None,
    }
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(total_days: i64) -> (i64, i64, i64) {
    let mut y = 1970 + total_days / 365;
    let mut remaining = total_days - (y - 1970) * 365 - (y - 1969) / 4;
    if remaining < 0 {
        y -= 1;
        remaining = total_days - (y - 1970) * 365 - (y - 1969) / 4;
    }
    let is_leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let month_lengths: [i64; 12] = [
        31,
        if is_leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month_idx = 1_i64;
    for &ml in &month_lengths {
        if remaining < ml {
            break;
        }
        remaining -= ml;
        month_idx += 1;
    }
    (y, month_idx, remaining + 1)
}

/// Days in a given month (1-indexed).
///
/// Only used by `parse_named_time_range` for `last_month` calculation;
/// extracted for clarity.
#[allow(clippy::single_call_fn)]
const fn days_in_month(year: i64, month: i64) -> i64 {
    let is_leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        2 => {
            if is_leap {
                29
            } else {
                28
            }
        }
        // All other months (4,6,9,11 and invalid) default to 30.
        _ => 30,
    }
}

/// Convert (year, month, day) to days since Unix epoch.
fn ymd_to_days(year: i64, month: i64, day: i64) -> i64 {
    const CUMULATIVE: [i64; 13] = [0, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let idx = usize::try_from(month).unwrap_or(0);
    let month_offset = CUMULATIVE.get(idx).copied().unwrap_or(0);
    (year - 1970) * 365 + (year - 1969) / 4 + month_offset + day - 1
}

/// NTFS attribute name → bit value.
#[must_use]
pub fn attr_bit(name: &str) -> u32 {
    match name {
        "readonly" | "read-only" | "r" => 0x0001,
        "hidden" | "h" => 0x0002,
        "system" | "s" => 0x0004,
        "directory" | "dir" | "d" => 0x0010,
        "archive" | "a" => 0x0020,
        "device" => 0x0040,
        "normal" => 0x0080,
        "temporary" | "temp" | "t" => 0x0100,
        "sparse" => 0x0200,
        "reparse" => 0x0400,
        "compressed" | "c" => 0x0800,
        "offline" | "o" => 0x1000,
        "notindexed" | "notcontent" | "n" => 0x2000,
        "encrypted" | "e" => 0x4000,
        "integrity" | "i" => 0x8000,
        "virtual" | "v" => 0x0001_0000,
        "noscrub" | "no_scrub_data" | "x" => 0x0002_0000,
        "pinned" | "p" => 0x0008_0000,
        "unpinned" | "u" => 0x0010_0000,
        _ => 0,
    }
}

/// Expand an attribute preset name into its raw spec string.
///
/// Returns `None` if the token is not a known preset.
fn expand_attr_preset(token: &str) -> Option<&'static str> {
    match token {
        "system-files" | "system_files" | "sysfiles" => Some("hidden,system"),
        "user-files" | "user_files" | "userfiles" => Some("!hidden,!system"),
        "compressed-encrypted" | "compressed_encrypted" | "compenc" => Some("compressed,encrypted"),
        _ => None,
    }
}

/// Expand all preset aliases in a comma-separated attribute spec.
///
/// Tokens that are known presets (e.g. `"system-files"`) are replaced with
/// their primitive equivalents.  Other tokens pass through unchanged.
///
/// ```
/// # use uffs_core::search::filters::expand_attr_spec;
/// assert_eq!(expand_attr_spec("system-files"), "hidden,system");
/// assert_eq!(expand_attr_spec("user-files"), "!hidden,!system");
/// assert_eq!(expand_attr_spec("hidden,readonly"), "hidden,readonly");
/// assert_eq!(
///     expand_attr_spec("compressed-encrypted,readonly"),
///     "compressed,encrypted,readonly",
/// );
/// ```
#[must_use]
pub fn expand_attr_spec(spec: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for raw in spec.split(',') {
        let token = raw.trim();
        let lower = token.to_ascii_lowercase();
        // Check for negated presets: "!system-files" → "!hidden,!system"
        if let Some(inner) = lower.strip_prefix('!')
            && let Some(expanded) = expand_attr_preset(inner)
        {
            // Expanded preset may contain its own '!' prefixes.
            for sub in expanded.split(',') {
                out.push(sub);
            }
            continue;
        }
        if let Some(expanded) = expand_attr_preset(&lower) {
            for sub in expanded.split(',') {
                out.push(sub);
            }
        } else {
            out.push(token);
        }
    }
    out.join(",")
}

/// Parse required attribute bits from an attr spec like `"hidden,compressed"`.
///
/// Supports preset aliases: `system-files` → `hidden,system`,
/// `user-files` → `!hidden,!system`.
#[must_use]
pub fn parse_attr_require(spec: &str) -> u32 {
    let mut bits = 0_u32;
    for raw_part in spec.split(',') {
        let lowered = raw_part.trim().to_ascii_lowercase();
        if lowered.starts_with('!') {
            continue;
        }
        // Check for presets first.
        if let Some(expanded) = expand_attr_preset(&lowered) {
            bits |= parse_attr_require(expanded);
        } else {
            bits |= attr_bit(&lowered);
        }
    }
    bits
}

/// Parse excluded attribute bits from an attr spec like `"!system,!hidden"`.
///
/// Supports preset aliases: `user-files` → `!hidden,!system`.
#[must_use]
pub fn parse_attr_exclude(spec: &str) -> u32 {
    let mut bits = 0_u32;
    for raw_part in spec.split(',') {
        let lowered = raw_part.trim().to_ascii_lowercase();
        if let Some(name) = lowered.strip_prefix('!') {
            if let Some(expanded) = expand_attr_preset(name) {
                bits |= parse_attr_exclude(expanded);
            } else {
                bits |= attr_bit(name);
            }
        }
        // Also check if the whole token (without !) is a preset that
        // contains exclusion rules.
        if !lowered.starts_with('!')
            && let Some(expanded) = expand_attr_preset(&lowered)
        {
            bits |= parse_attr_exclude(expanded);
        }
    }
    bits
}

// ════════════════════════════════════════════════════════════════════════
// REGRESSION TESTS — Search Filters Parity Guards
//
// These tests verify that SearchFilters.matches_record covers ALL filter
// types.  During the v0.4.30 refactor, 14 filter parameters were not
// wired into the compact search path (they were all passed as None).
// See `docs/architecture/2026_03_30_04_12_SEARCH_PIPELINE_REGRESSION_ANALYSIS.
// md` ════════════════════════════════════════════════════════════════════════
#[cfg(test)]
#[path = "filters_tests.rs"]
mod tests;
