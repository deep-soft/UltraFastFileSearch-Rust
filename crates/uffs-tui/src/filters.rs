// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Extended post-search filters — re-exported from `uffs-core` with
//! TUI-specific `SearchState` builder.

// Re-export everything from uffs-core
pub(crate) use uffs_core::search::filters::{
    SearchFilters, now_unix_micros, parse_attr_exclude, parse_attr_require, parse_month_spec,
    parse_time_bound,
};

use crate::history::SearchState;

/// Build [`SearchFilters`] from a TUI [`SearchState`].
///
/// This is the TUI-specific constructor. The generic `SearchFilters` struct
/// lives in `uffs-core`; this function bridges TUI's `SearchState` to it.
#[must_use]
#[expect(
    clippy::single_call_fn,
    reason = "bridge function — structural separation from TUI logic"
)]
pub(crate) fn build_search_filters(state: &SearchState) -> SearchFilters {
    let now_us = now_unix_micros();
    SearchFilters {
        hide_system: state.hide_system,
        hide_ads: false,
        min_size: state.min_size,
        max_size: state.max_size,
        newer_us: state
            .newer
            .as_deref()
            .and_then(|spec| parse_time_bound(spec, now_us, true)),
        older_us: state
            .older
            .as_deref()
            .and_then(|spec| parse_time_bound(spec, now_us, false)),
        newer_created_us: state
            .newer_created
            .as_deref()
            .and_then(|spec| parse_time_bound(spec, now_us, true)),
        older_created_us: state
            .older_created
            .as_deref()
            .and_then(|spec| parse_time_bound(spec, now_us, false)),
        newer_accessed_us: state
            .newer_accessed
            .as_deref()
            .and_then(|spec| parse_time_bound(spec, now_us, true)),
        older_accessed_us: state
            .older_accessed
            .as_deref()
            .and_then(|spec| parse_time_bound(spec, now_us, false)),
        attr_require: parse_attr_require(state.attr.as_deref().unwrap_or("")),
        attr_exclude: parse_attr_exclude(state.attr.as_deref().unwrap_or("")),
        min_descendants: state.min_descendants,
        max_descendants: state.max_descendants,
        extensions: state
            .ext
            .as_deref()
            .map(|ext_list| {
                let mut exts = Vec::new();
                for segment in ext_list.split(',') {
                    let token = segment
                        .trim()
                        .to_ascii_lowercase()
                        .trim_start_matches('.')
                        .to_owned();
                    if token.is_empty() {
                        continue;
                    }
                    if let Some(collection) = uffs_core::extensions::expand_collection(&token) {
                        exts.extend(collection.iter().map(|ext| (*ext).to_owned()));
                    } else {
                        exts.push(token);
                    }
                }
                exts
            })
            .unwrap_or_default(),
        resolved_ext_ids: Vec::new(),
        exclude_lower: state.exclude.as_ref().map(|ex| ex.to_ascii_lowercase()),
        path_contains_lower: None,
        type_filter: None,
        min_bulkiness: None,
        max_bulkiness: None,
        min_name_len: state.min_name_len,
        max_name_len: state.max_name_len,
        min_path_len: state.min_path_len,
        max_path_len: state.max_path_len,
        min_allocated: state.min_allocated,
        max_allocated: state.max_allocated,
        min_treesize: None,
        max_treesize: None,
        min_tree_allocated: None,
        max_tree_allocated: None,
        allowed_months: state
            .month
            .as_deref()
            .map(parse_month_spec)
            .unwrap_or_default(),
    }
}
