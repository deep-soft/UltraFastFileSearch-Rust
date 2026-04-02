//! Output helpers for CLI search commands.
//!
//! Delegates to submodules for types, filter logic, and row writing.
//! This file keeps the public API surface, `DataFrame` conversion, and
//! results-writing logic.

use core::time::Duration;
use std::fs::File;
use std::io::{BufWriter, Write};

use anyhow::{Context, Result};
use tracing::info;
use uffs_core::output::OutputConfig;
use uffs_core::{export_json, export_table};

// Legacy streaming submodules (filter, row_writer, streaming, types) removed
// in Step 4.  See git history for reference.

/// Context for C++ baseline-compatible footer formatting.
pub(super) struct CppFooterContext<'a> {
    /// Drive letters to include in the footer (e.g., `['C', 'D']`).
    pub(super) output_targets: &'a [char],
    /// Original search pattern string.
    pub(super) pattern: &'a str,
    /// Total result row count for fast-scan heuristic.
    pub(super) row_count: usize,
}

/// Write `DisplayRow` search results to console or file.
///
/// For `json` and `table` formats, converts `DisplayRow`s to a `DataFrame`
/// first (Polars serialisation).  For `csv` and `custom`, writes directly.
pub(super) fn write_native_results(
    rows: &[uffs_core::search::backend::DisplayRow],
    format: &str,
    out: &str,
    output_config: &OutputConfig,
    output_targets: &[char],
    _elapsed: Duration,
    pattern: &str,
) -> Result<()> {
    let profile = std::env::var_os("UFFS_CACHE_PROFILE").is_some();
    let is_console = matches!(
        out.to_lowercase().as_str(),
        "console" | "con" | "term" | "terminal"
    );

    let footer_ctx = CppFooterContext {
        output_targets,
        pattern,
        row_count: rows.len(),
    };

    // ── Phase 1: convert DisplayRows → DataFrame (json/table only) ──
    let t_convert = std::time::Instant::now();
    let needs_df = matches!(format, "json" | "table");
    let converted_df = if needs_df {
        Some(
            uffs_core::search::backend::display_rows_to_dataframe(rows)
                .map_err(|err| anyhow::anyhow!("Failed to build result DataFrame: {err}"))?,
        )
    } else {
        None
    };
    let convert_ms = t_convert.elapsed().as_millis();

    // ── Phase 2: format + write ─────────────────────────────────────
    let t_write = std::time::Instant::now();
    if is_console {
        let stdout_handle = std::io::stdout();
        let mut stdout = BufWriter::with_capacity(64 * 1024, stdout_handle.lock());
        match format {
            "json" => export_json(converted_df.as_ref().unwrap_or(&EMPTY_DF), &mut stdout)?,
            "csv" => output_config.write_display_rows(rows, &mut stdout)?,
            "custom" => {
                output_config.write_display_rows(rows, &mut stdout)?;
                write_cpp_drive_footer(&mut stdout, &footer_ctx)?;
            }
            _ => export_table(converted_df.as_ref().unwrap_or(&EMPTY_DF), &mut stdout)?,
        }
        stdout.flush()?;
    } else {
        let file =
            File::create(out).with_context(|| format!("Failed to create output file: {out}"))?;
        let mut writer = BufWriter::new(file);

        match format {
            "json" => export_json(converted_df.as_ref().unwrap_or(&EMPTY_DF), &mut writer)?,
            "custom" => {
                output_config.write_display_rows(rows, &mut writer)?;
                write_cpp_drive_footer(&mut writer, &footer_ctx)?;
            }
            _ => output_config.write_display_rows(rows, &mut writer)?,
        }
        writer.flush()?;

        info!(file = out, "Results written to file");
    }
    let write_ms = t_write.elapsed().as_millis();

    if profile {
        tracing::debug!(
            target: "cache_profile",
            convert_ms = %convert_ms,
            needs_df,
            rows = rows.len(),
            write_ms = %write_ms,
            format,
            "output_fmt_io"
        );
    }

    Ok(())
}

/// Empty `DataFrame` sentinel — avoids `unwrap` when the branch is unreachable.
static EMPTY_DF: std::sync::LazyLock<uffs_polars::DataFrame> =
    std::sync::LazyLock::new(uffs_polars::DataFrame::empty);

/// Append the legacy C++ drive footer for baseline-compatible custom output.
///
/// Uses CRLF line endings (`\r\n`) to match C++ baseline behavior.
/// When `row_count` is < 20,000, appends the fast-scan message.
fn write_cpp_drive_footer<W: Write + ?Sized>(
    writer: &mut W,
    ctx: &CppFooterContext<'_>,
) -> Result<()> {
    if ctx.output_targets.is_empty() {
        return Ok(());
    }

    write!(writer, "\r\n")?;
    write!(writer, "\r\n")?;
    write!(
        writer,
        "Drives? \t{}\t{}\r\n",
        ctx.output_targets.len(),
        format_cpp_drive_letters(ctx.output_targets)
    )?;
    write!(writer, "\r\n")?;

    // Only show the "too few results" warning for full-scan patterns (* or empty).
    // Filtered/regex/glob queries naturally return few results — that's not an
    // error.
    // Also recognize cpp-transformed full-scan patterns like ">G:.*" or
    // ">C:.*|D:.*" which are the regex equivalents of "*".
    let is_full_scan = matches!(ctx.pattern, "" | "*" | "**" | "**/*")
        || ctx.pattern.strip_prefix('>').is_some_and(|rest| {
            rest.split('|')
                .all(|seg| seg.ends_with(".*") && seg.len() <= 4)
        });
    if ctx.row_count < 20_000 && is_full_scan {
        write!(
            writer,
            "MMMmmm that was FAST ... maybe your searchstring was wrong?\t{pattern}\r\n",
            pattern = ctx.pattern
        )?;
        write!(writer, "Search path. E.g. 'C:/' or 'C:\\Prog**' \r\n")?;
    }

    Ok(())
}

/// Format drive letters using the legacy C++ footer style (for example `D:` or
/// `C:|D:`).
#[must_use]
fn format_cpp_drive_letters(output_targets: &[char]) -> String {
    output_targets
        .iter()
        .map(|drive| format!("{}:", drive.to_ascii_uppercase()))
        .collect::<Vec<_>>()
        .join("|")
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
