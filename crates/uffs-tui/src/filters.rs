//! Extended post-search filters — re-exported from `uffs-core` with
//! TUI-specific `SearchState` builder.

// Re-export everything from uffs-core
pub use uffs_core::search::filters::{
    SearchFilters, now_unix_micros, parse_attr_exclude, parse_attr_require, parse_size,
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
pub fn build_search_filters(state: &SearchState) -> SearchFilters {
    let now_us = now_unix_micros();
    SearchFilters {
        hide_system: state.hide_system,
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
                ext_list
                    .split(',')
                    .map(|segment| {
                        segment
                            .trim()
                            .to_ascii_lowercase()
                            .trim_start_matches('.')
                            .to_owned()
                    })
                    .filter(|ext| !ext.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        resolved_ext_ids: Vec::new(),
        exclude_lower: state.exclude.as_ref().map(|ex| ex.to_ascii_lowercase()),
    }
}
