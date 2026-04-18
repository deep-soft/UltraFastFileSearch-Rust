// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Unit tests for the private helpers in `config.rs`.
//!
//! Extracted from an inline `#[cfg(test)] mod tests { ... }` block to
//! keep `config.rs` under the 800-LOC file-size policy.  The
//! corresponding stub in `config.rs` is:
//!
//! ```ignore
//! #[cfg(test)]
//! #[path = "config_tests.rs"]
//! mod tests;
//! ```
//!
//! so `super::<private_item>` still resolves against the `config`
//! module and keeps tests out of the public API surface.

use super::append_datetime_native;

/// Regression: `append_datetime_native` must interpret its `filetime`
/// argument as a raw FILETIME (100-ns ticks since 1601-01-01), not as
/// Unix microseconds.  Pre-fix, the function ran its inline Hinnant
/// calendar algorithm against the value divided by `1_000_000` as if it
/// were seconds-since-1970, producing year-6220 output for 2026-era
/// timestamps.  Caught by `scripts/verify_parity.rs` — this unit test
/// pins the invariant at the unit level so the next parity regression
/// fails on `cargo test` first, before a parity run is needed.
#[test]
fn append_datetime_native_formats_filetime_as_2024() {
    // 2024-01-01 00:00:00 UTC as raw FILETIME (100-ns ticks since
    // 1601-01-01).  This is the exact constant used by
    // `uffs_time::test_filetime_conversion` and by several of the
    // format/time-parsing tests — keeping the same anchor value here
    // lets any future divergence surface across the whole suite.
    let ft_2024: i64 = 133_485_408_000_000_000;
    let mut buf = String::new();
    append_datetime_native(&mut buf, ft_2024, 0);
    assert_eq!(
        buf, "2024-01-01 00:00:00",
        "v13+ stores timestamps as raw FILETIME — the output writer \
         must interpret them as such, not as Unix microseconds"
    );
}

/// Year-6220 is the distinctive symptom of the pre-fix bug (a 2026
/// FILETIME mis-interpreted as a Unix-µs value lands ~4200 years in
/// the future because of the combined 369-year + 10× unit offset).
/// This test fails LOUDLY on any regression that reintroduces the
/// mis-interpretation, with an error message pointing at the exact
/// bug class.
#[test]
fn append_datetime_native_never_emits_year_6220() {
    // Build a 2026-01-20 00:00:00 UTC FILETIME from the canonical
    // Unix-seconds anchor rather than a hardcoded constant, so any
    // drift in `FILETIME_UNIX_DIFF` / `FILETIME_TICKS_PER_SECOND`
    // surfaces via the same assertion.
    let unix_secs_2026_01_20: i64 = 1_768_867_200; // 2026-01-20 00:00:00 UTC
    let ft_2026 =
        unix_secs_2026_01_20 * uffs_time::FILETIME_TICKS_PER_SECOND + uffs_time::FILETIME_UNIX_DIFF;
    let mut buf = String::new();
    append_datetime_native(&mut buf, ft_2026, 0);
    assert!(
        !buf.starts_with("6220"),
        "FILETIME-as-Unix-micros regression: formatter emitted '{buf}' \
         — this is the year-6220 symptom of interpreting a raw \
         FILETIME as a Unix-µs value.  See the doc comment on \
         `append_datetime_native` for the fix."
    );
    assert_eq!(
        buf, "2026-01-20 00:00:00",
        "expected a 2026-era output from the 2026-01-20 FILETIME anchor"
    );
}

/// Zero FILETIME (unset / null timestamp in NTFS) must surface as
/// the all-zero sentinel.  Catches the case where a regression adds
/// an unconditional offset that would turn zero into a 1601 date or
/// similar.
#[test]
fn append_datetime_native_zero_filetime_is_zero_sentinel() {
    let mut buf = String::new();
    append_datetime_native(&mut buf, 0, 0);
    assert_eq!(buf, "0000-00-00 00:00:00");
}

/// Timezone bias is applied in FILETIME ticks before the calendar
/// decomposition — matches the parity CSV writer.  Same `ft_2024`
/// anchor with a -8h (PST) offset must produce the previous day's
/// afternoon.
#[test]
fn append_datetime_native_tz_bias_pst() {
    let ft_2024: i64 = 133_485_408_000_000_000; // 2024-01-01 00:00:00 UTC
    let pst_offset: i32 = -8 * 3600; // UTC-8
    let mut buf = String::new();
    append_datetime_native(&mut buf, ft_2024, pst_offset);
    assert_eq!(
        buf, "2023-12-31 16:00:00",
        "tz bias must be applied before calendar decomposition"
    );
}
