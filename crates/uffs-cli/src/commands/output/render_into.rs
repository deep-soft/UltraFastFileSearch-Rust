// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! The `--benchmark` output sink: render search rows with the console
//! formatter into any writer.
//!
//! Split from [`super`] for the 800-LOC file-size policy; shares the
//! parent's private `write_formatted` / context builders so the sink
//! renders byte-for-byte what the console would.

use std::io::Write;

use anyhow::Result;
use serde_json::Value;

use super::{CppFooterContext, ParityContext, write_formatted};

/// Render rows exactly as the console path would, into `writer`.
///
/// The `--benchmark` sink: the full formatting pipeline (column
/// resolution, quoting, header / footer, parity timestamps) runs through
/// this into a byte-counting writer, so the measured cost is everything
/// UFFS does to produce the output with only the terminal left out.
/// Same formatter as [`super::write_native_results`]'s console branch; only the
/// destination differs.
///
/// # Errors
///
/// Returns an error if formatting or the write fails.
#[expect(clippy::too_many_arguments, reason = "output config forwarding")]
pub fn render_native_results_into<W: Write>(
    writer: &mut W,
    rows: &[Value],
    format: &str,
    columns: &str,
    separator: &str,
    quote: &str,
    header: bool,
    pos: &str,
    neg: &str,
    tz_offset: Option<i32>,
    output_targets: &[uffs_mft::platform::DriveLetter],
    pattern: &str,
) -> Result<()> {
    let footer_ctx = CppFooterContext {
        output_targets,
        pattern,
        row_count: rows.len(),
    };
    let parity_ctx = ParityContext::new(pos, neg, tz_offset);
    write_formatted(
        writer,
        rows,
        format,
        columns,
        separator,
        quote,
        header,
        &footer_ctx,
        &parity_ctx,
    )?;
    writer.flush()?;
    Ok(())
}
