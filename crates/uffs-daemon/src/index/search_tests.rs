// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Unit tests for [`super::build_output_config`].
//!
//! Lifted out of `search.rs` to keep that file under the 800-line
//! policy ceiling.  Re-attached to the original `search::tests` path
//! via `#[path = "search_tests.rs"] mod tests;` in `search.rs`, so the
//! `super::*` glob below continues to resolve against the `search`
//! module's scope.

use uffs_client::protocol::SearchParams;

use super::*;

/// Regression: `build_output_config` must use `OutputConfig` defaults
/// (separator = `,`, quote = `"`) when `SearchParams` output fields
/// are `None`.
///
/// Previously, `from_cli_args` set `output_separator: Some("")` which
/// caused `build_output_config` to call `with_separator("")`, wiping
/// the comma delimiter and producing concatenated output with no field
/// separation.
#[test]
fn build_output_config_preserves_defaults_when_none() {
    let params = SearchParams::default();
    assert!(params.output_separator.is_none());
    assert!(params.output_quote.is_none());
    assert!(params.output_pos.is_none());
    assert!(params.output_neg.is_none());

    let cfg = build_output_config(&params);
    assert_eq!(cfg.separator, ",", "default separator must be comma");
    assert_eq!(cfg.quote, "\"", "default quote must be double-quote");
    assert_eq!(cfg.pos, "1", "default pos must be '1'");
    assert_eq!(cfg.neg, "0", "default neg must be '0'");
    assert!(cfg.header, "default header must be true");
}

/// Guard against the exact bug: passing `Some("")` to
/// `build_output_config` must NOT wipe the separator/quote.
/// The daemon function should skip empty-string overrides.
#[test]
fn build_output_config_some_empty_string_overrides_defaults() {
    // This test documents the current behavior: if Some("") is passed,
    // it DOES override the default.  The fix is in from_cli_args which
    // must never produce Some("") for unset flags.
    let params = SearchParams {
        output_separator: Some(String::new()),
        output_quote: Some(String::new()),
        ..Default::default()
    };
    let cfg = build_output_config(&params);
    // Some("") overrides defaults — this is why from_cli_args must
    // use None, not Some(""), for unset flags.
    assert_eq!(
        cfg.separator, "",
        "Some(\"\") overrides default — from_cli_args must use None"
    );
    assert_eq!(
        cfg.quote, "",
        "Some(\"\") overrides default — from_cli_args must use None"
    );
}

/// Explicit separator and quote values must be forwarded.
#[test]
fn build_output_config_explicit_values_applied() {
    let params = SearchParams {
        output_separator: Some(";".to_owned()),
        output_quote: Some("'".to_owned()),
        output_pos: Some("+".to_owned()),
        output_neg: Some("-".to_owned()),
        output_header: Some(false),
        output_columns: Some("parity".to_owned()),
        output_parity_compat: Some(true),
        output_tz_offset_hours: Some(-7_i32),
        ..Default::default()
    };
    let cfg = build_output_config(&params);
    assert_eq!(cfg.separator, ";");
    assert_eq!(cfg.quote, "'");
    assert_eq!(cfg.pos, "+");
    assert_eq!(cfg.neg, "-");
    assert!(!cfg.header);
    assert!(cfg.columns.is_some(), "parity columns must be set");
    assert!(cfg.parity_compat, "parity_compat must be true");
    assert_eq!(cfg.timezone_offset_secs, -7_i32 * 3_600_i32);
}

/// `--parity-compat` without explicit sep/quote must produce a valid
/// parity `OutputConfig` with default comma + double-quote delimiters.
#[test]
fn build_output_config_parity_compat_uses_defaults() {
    let params = SearchParams {
        output_columns: Some("parity".to_owned()),
        output_parity_compat: Some(true),
        ..Default::default()
    };
    let cfg = build_output_config(&params);
    assert_eq!(
        cfg.separator, ",",
        "parity mode must use comma separator by default"
    );
    assert_eq!(
        cfg.quote, "\"",
        "parity mode must use double-quote by default"
    );
    assert!(cfg.parity_compat);
    assert!(cfg.columns.is_some());
}
