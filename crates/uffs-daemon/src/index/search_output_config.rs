// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Lift a protocol-level [`SearchParams`] into a fully-configured
//! [`uffs_core::output::OutputConfig`] the daemon's CSV sinks
//! (`--out=file` and [`crate::handler::RequestHandler::try_pack_csv_blob`])
//! consume.
//!
//! Lifted out of `search.rs` to keep that file under the 800-line
//! policy ceiling.  Re-attached via
//! `#[path = "search_output_config.rs"] mod output_config;` in
//! `search.rs`, so `build_output_config` stays addressable as
//! `crate::index::search::build_output_config(...)` at every call
//! site (the file-sink path in `search()` and the fast-path dispatch
//! in `handler::RequestHandler::try_pack_csv_blob`).

use uffs_client::protocol::SearchParams;

/// Reconstruct an [`uffs_core::output::OutputConfig`] from protocol
/// fields in [`SearchParams`].
///
/// The CLI serialises its `OutputConfig` into individual string fields
/// (`output_separator`, `output_quote`, etc.) so the daemon can rebuild
/// an identical config without needing serde on `OutputConfig` itself.
///
/// `pub(crate)` so [`crate::handler::RequestHandler::try_pack_csv_blob`]
/// can build the same config the file-sink path uses without
/// duplicating the field-by-field dispatch.
pub(crate) fn build_output_config(params: &SearchParams) -> uffs_core::output::OutputConfig {
    let mut cfg = uffs_core::output::OutputConfig::default();

    if let Some(sep) = &params.output_separator {
        cfg = cfg.with_separator(sep);
    }
    if let Some(quote) = &params.output_quote {
        cfg = cfg.with_quote(quote);
    }
    if let Some(header) = params.output_header {
        cfg = cfg.with_header(header);
    }
    if let Some(pos) = &params.output_pos {
        cfg = cfg.with_pos(pos);
    }
    if let Some(neg) = &params.output_neg {
        cfg = cfg.with_neg(neg);
    }
    let parity_columns = params
        .output_columns
        .as_deref()
        .is_some_and(|cols| cols.eq_ignore_ascii_case("parity"));
    if let Some(cols_str) = &params.output_columns {
        cfg = cfg.with_columns(cols_str);
    }
    // `--columns parity` implies the parity-compat directory rewrites
    // (trailing `\`, empty Name, treesize for Size, etc.).  The CLI's
    // hand-rolled `write_parity` applies these unconditionally
    // regardless of `--parity-compat`, so the daemon must too —
    // otherwise `--columns parity` without `--parity-compat` would
    // drift between CLI stdout (rewrites on) and `--out=file`
    // (rewrites off).
    if parity_columns || params.output_parity_compat == Some(true) {
        cfg = cfg.with_parity_compat(true);
    }
    if let Some(tz_hours) = params.output_tz_offset_hours {
        cfg = cfg.with_tz_offset_hours(tz_hours);
    }
    cfg
}
