// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Output helpers for CLI search commands.
//!
//! Formats `SearchRow` (from the daemon protocol) directly — no polars,
//! no `DisplayRow`, no `DataFrame`.  This is the thin-client output path.

use core::time::Duration;
use std::fs::File;
use std::io::{BufWriter, Write};

use anyhow::{Context, Result};
use serde_json::Value;

// ── Value extraction helpers ───────────────────────────────────────────

/// Get string field.
fn vs(row: &Value, key: &str) -> String {
    row[key].as_str().unwrap_or("").to_owned()
}

/// Get u64 field.
fn vu(row: &Value, key: &str) -> u64 {
    row[key].as_u64().unwrap_or(0)
}

/// Get i64 field.
fn vi(row: &Value, key: &str) -> i64 {
    row[key].as_i64().unwrap_or(0)
}

/// Get u32 field (clamped to `u32::MAX` on overflow).
fn vu32(row: &Value, key: &str) -> u32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "flags are stored as u32 in NTFS — overflow is not possible in practice"
    )]
    let val = row.get(key).and_then(Value::as_u64).unwrap_or(0) as u32;
    val
}

/// Get bool field.
fn vb(row: &Value, key: &str) -> bool {
    row[key].as_bool().unwrap_or(false)
}

/// Context for legacy baseline-compatible footer formatting.
pub struct CppFooterContext<'a> {
    /// Drive letters to include in the footer (e.g., `['C', 'D']`).
    pub output_targets: &'a [char],
    /// Original search pattern string.
    pub pattern: &'a str,
    /// Total result row count for fast-scan heuristic.
    pub row_count: usize,
}

/// Write `SearchRow` search results to console or file.
///
/// For `json` format: serialises with `serde_json` (no polars).
/// For `csv`/`custom`: writes columnar text directly from `SearchRow` fields.
/// For `table`: formats a fixed-width text table.
///
/// # Errors
///
/// Returns an error if the operation fails.
#[expect(clippy::too_many_arguments, reason = "output config forwarding")]
pub fn write_native_results(
    rows: &[Value],
    format: &str,
    out: &str,
    columns: &str,
    separator: &str,
    quote: &str,
    header: bool,
    pos: &str,
    neg: &str,
    tz_offset: Option<i32>,
    output_targets: &[char],
    _elapsed: Duration,
    pattern: &str,
) -> Result<()> {
    let is_console = out.is_empty()
        || matches!(
            out.to_lowercase().as_str(),
            "console" | "con" | "term" | "terminal"
        );

    let footer_ctx = CppFooterContext {
        output_targets,
        pattern,
        row_count: rows.len(),
    };

    let parity_ctx = ParityContext {
        pos,
        neg,
        tz_offset_secs: tz_offset.map_or(0_i32, |hours| hours * 3_600_i32),
    };

    if is_console {
        let stdout_handle = std::io::stdout();
        let mut stdout = BufWriter::with_capacity(64 * 1024, stdout_handle.lock());
        write_formatted(
            &mut stdout,
            rows,
            format,
            columns,
            separator,
            quote,
            header,
            &footer_ctx,
            &parity_ctx,
        )?;
        stdout.flush()?;
    } else {
        let file =
            File::create(out).with_context(|| format!("Failed to create output file: {out}"))?;
        let mut writer = BufWriter::new(file);
        write_formatted(
            &mut writer,
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
        // Results written to file.
    }

    Ok(())
}

/// Parity formatting context (timezone, boolean flags).
struct ParityContext<'a> {
    /// Positive boolean string (e.g., `"1"`).
    pos: &'a str,
    /// Negative boolean string (e.g., `"0"`).
    neg: &'a str,
    /// Timezone offset in seconds from UTC.
    tz_offset_secs: i32,
}

/// Dispatch to the appropriate formatter.
#[expect(clippy::too_many_arguments, reason = "output config forwarding")]
fn write_formatted<W: Write>(
    writer: &mut W,
    rows: &[Value],
    format: &str,
    columns: &str,
    separator: &str,
    quote: &str,
    header: bool,
    footer_ctx: &CppFooterContext<'_>,
    parity_ctx: &ParityContext<'_>,
) -> Result<()> {
    let is_parity = columns.eq_ignore_ascii_case("parity");
    match format {
        "json" => write_json(writer, rows),
        "custom" => {
            if is_parity {
                write_parity(writer, rows, separator, quote, parity_ctx)?;
            } else {
                write_columnar(writer, rows, columns, separator, quote, header)?;
            }
            write_legacy_drive_footer(writer, footer_ctx)
        }
        "table" => write_table(writer, rows),
        _ => {
            if is_parity {
                write_parity(writer, rows, separator, quote, parity_ctx)
            } else {
                write_columnar(writer, rows, columns, separator, quote, header)
            }
        }
    }
}

/// Serialise rows as a JSON array.
fn write_json<W: Write>(writer: &mut W, rows: &[Value]) -> Result<()> {
    serde_json::to_writer_pretty(&mut *writer, rows)?;
    writeln!(writer)?;
    Ok(())
}

/// Write a simple aligned text table (name, size, modified, path).
fn write_table<W: Write>(writer: &mut W, rows: &[Value]) -> Result<()> {
    // Header
    writeln!(
        writer,
        "{:<50} {:>12} {:>19} Path",
        "Name", "Size", "Modified"
    )?;
    writeln!(writer, "{}", "─".repeat(120))?;

    for row in rows {
        let size_str = uffs_client::format::format_bytes(vu(row, "size"));
        let time_str = format_unix_us(vi(row, "modified"));
        writeln!(
            writer,
            "{:<50} {:>12} {:>19} {}",
            vs(row, "name"),
            size_str,
            time_str,
            vs(row, "path")
        )?;
    }
    Ok(())
}

/// Default column set when none is specified.
const DEFAULT_COLUMNS: &str = "name,size,modified,path";

/// Write columnar (CSV-style) output from `SearchRow` fields.
///
/// Columns are resolved by name from `SearchRow` fields. Unknown columns
/// are silently ignored.
fn write_columnar<W: Write>(
    writer: &mut W,
    rows: &[Value],
    columns: &str,
    separator: &str,
    quote: &str,
    header: bool,
) -> Result<()> {
    let col_spec = if columns.is_empty() || columns.eq_ignore_ascii_case("parity") {
        DEFAULT_COLUMNS
    } else {
        columns
    };

    let col_names: Vec<&str> = col_spec.split(',').map(str::trim).collect();

    // Header row
    if header {
        let header_line: Vec<&str> = col_names.clone();
        writeln!(writer, "{}", header_line.join(separator))?;
    }

    for row in rows {
        let mut first = true;
        for col in &col_names {
            if !first {
                write!(writer, "{separator}")?;
            }
            first = false;
            let value = extract_column(row, col);
            if quote.is_empty() {
                write!(writer, "{value}")?;
            } else {
                write!(writer, "{quote}{value}{quote}")?;
            }
        }
        writeln!(writer)?;
    }
    Ok(())
}

/// Extract a column value from a JSON row by column name.
fn extract_column(row: &Value, col: &str) -> String {
    match col.to_ascii_lowercase().as_str() {
        "name" => vs(row, "name"),
        "path" => vs(row, "path"),
        "size" => vu(row, "size").to_string(),
        "allocated" | "allocated_size" | "size_on_disk" => vu(row, "allocated").to_string(),
        "modified" | "written" => format_unix_us(vi(row, "modified")),
        "created" => format_unix_us(vi(row, "created")),
        "accessed" => format_unix_us(vi(row, "accessed")),
        "ext" | "extension" => extract_extension(&vs(row, "name")),
        "drive" => vs(row, "drive"),
        "type" | "kind" => {
            if vb(row, "is_directory") {
                "dir".to_owned()
            } else {
                "file".to_owned()
            }
        }
        "flags" => format!("{:#010x}", vu32(row, "flags")),
        "descendants" => vu(row, "descendants").to_string(),
        "treesize" => vu(row, "treesize").to_string(),
        "tree_allocated" => vu(row, "tree_allocated").to_string(),
        _ => String::new(),
    }
}

/// Extract file extension from a filename.
fn extract_extension(name: &str) -> String {
    name.rsplit_once('.')
        .map_or_else(String::new, |(_, ext)| ext.to_owned())
}

// ── Parity-compat output ──────────────────────────────────────────────

/// NTFS attribute flag constants (for parity boolean columns).
mod parity_flags {
    /// Read-only attribute.
    pub(super) const READONLY: u32 = 0x0001;
    /// Hidden attribute.
    pub(super) const HIDDEN: u32 = 0x0002;
    /// System attribute.
    pub(super) const SYSTEM: u32 = 0x0004;
    /// Directory attribute.
    pub(super) const DIRECTORY: u32 = 0x0010;
    /// Archive attribute.
    pub(super) const ARCHIVE: u32 = 0x0020;
    /// Sparse file attribute.
    pub(super) const SPARSE: u32 = 0x0200;
    /// Reparse point attribute.
    pub(super) const REPARSE: u32 = 0x0400;
    /// Compressed attribute.
    pub(super) const COMPRESSED: u32 = 0x0800;
    /// Offline attribute.
    pub(super) const OFFLINE: u32 = 0x1000;
    /// Not content-indexed attribute.
    pub(super) const NOT_INDEXED: u32 = 0x2000;
    /// Encrypted attribute.
    pub(super) const ENCRYPTED: u32 = 0x4000;
    /// Integrity stream attribute.
    pub(super) const INTEGRITY: u32 = 0x8000;
    /// No-scrub-data attribute.
    pub(super) const NO_SCRUB: u32 = 0x0002_0000;
    /// Pinned attribute.
    pub(super) const PINNED: u32 = 0x0008_0000;
    /// Unpinned attribute.
    pub(super) const UNPINNED: u32 = 0x0010_0000;
    /// Parity mask — the 15 attribute bits tracked by the legacy baseline.
    pub(super) const PARITY_MASK: u32 = READONLY
        | HIDDEN
        | SYSTEM
        | DIRECTORY
        | ARCHIVE
        | SPARSE
        | REPARSE
        | COMPRESSED
        | OFFLINE
        | NOT_INDEXED
        | ENCRYPTED
        | INTEGRITY
        | NO_SCRUB
        | PINNED
        | UNPINNED;
}

/// Parity-compat CSV header (25 columns, matching legacy baseline).
const PARITY_HEADER: &[&str] = &[
    "Path",
    "Name",
    "Path Only",
    "Size",
    "Size on Disk",
    "Created",
    "Last Written",
    "Last Accessed",
    "Descendants",
    "Read-only",
    "Archive",
    "System",
    "Hidden",
    "Offline",
    "Not content indexed file",
    "No scrub file",
    "Integrity",
    "Pinned",
    "Unpinned",
    "Directory Flag",
    "Compressed",
    "Encrypted",
    "Sparse",
    "Reparse",
    "Attributes",
];

/// Boolean columns in parity order (matches `PARITY_HEADER[9..25]`).
const PARITY_BOOL_FLAGS: &[u32] = &[
    parity_flags::READONLY,
    parity_flags::ARCHIVE,
    parity_flags::SYSTEM,
    parity_flags::HIDDEN,
    parity_flags::OFFLINE,
    parity_flags::NOT_INDEXED,
    parity_flags::NO_SCRUB,
    parity_flags::INTEGRITY,
    parity_flags::PINNED,
    parity_flags::UNPINNED,
    parity_flags::DIRECTORY,
    parity_flags::COMPRESSED,
    parity_flags::ENCRYPTED,
    parity_flags::SPARSE,
    parity_flags::REPARSE,
];

/// Write parity-compat 25-column CSV output from `SearchRow` data.
///
/// This mirrors the daemon-side `write_display_row_columns` output exactly:
/// - Directories: trailing `\` on path, empty name, `path_only` = path, size =
///   `treesize`
/// - Timestamps: adjusted by `tz_offset_secs`
/// - Booleans: `pos`/`neg` strings
/// - Last column: raw attributes masked to parity 15 bits
fn write_parity<W: Write>(
    writer: &mut W,
    rows: &[Value],
    separator: &str,
    quote: &str,
    ctx: &ParityContext<'_>,
) -> Result<()> {
    // Header
    let mut header = String::with_capacity(512);
    for (idx, col) in PARITY_HEADER.iter().enumerate() {
        if idx > 0 {
            header.push_str(separator);
        }
        header.push_str(quote);
        header.push_str(col);
        header.push_str(quote);
    }
    // C++ baseline: header followed by empty line
    header.push('\n');
    header.push('\n');
    writer.write_all(header.as_bytes())?;

    let mut buf = String::with_capacity(512);

    for row in rows {
        buf.clear();
        let is_dir = vb(row, "is_directory");
        let flags = vu32(row, "flags");
        let path = vs(row, "path");
        let name = vs(row, "name");

        // 0: Path (quoted, trailing \ for dirs)
        buf.push_str(quote);
        buf.push_str(&path);
        if is_dir && !path.ends_with('\\') {
            buf.push('\\');
        }
        buf.push_str(quote);

        // 1: Name (quoted, empty for dirs)
        buf.push_str(separator);
        buf.push_str(quote);
        if !is_dir {
            buf.push_str(&name);
        }
        buf.push_str(quote);

        // 2: PathOnly (quoted)
        buf.push_str(separator);
        buf.push_str(quote);
        if is_dir {
            buf.push_str(&path);
            if !path.ends_with('\\') {
                buf.push('\\');
            }
        } else if let Some(bslash) = path.rfind('\\') {
            if let Some(slice) = path.get(..=bslash) {
                buf.push_str(slice);
            }
        } else {
            buf.push_str(&path);
        }
        buf.push_str(quote);

        // 3: Size (treesize for dirs)
        buf.push_str(separator);
        let size = if is_dir {
            vu(row, "treesize")
        } else {
            vu(row, "size")
        };
        push_u64(&mut buf, size);

        // 4: SizeOnDisk (tree_allocated for dirs)
        buf.push_str(separator);
        let alloc = if is_dir {
            vu(row, "tree_allocated")
        } else {
            vu(row, "allocated")
        };
        push_u64(&mut buf, alloc);

        // 5-7: Created, Modified, Accessed
        for key in &["created", "modified", "accessed"] {
            buf.push_str(separator);
            append_datetime_tz(&mut buf, vi(row, key), ctx.tz_offset_secs);
        }

        // 8: Descendants
        buf.push_str(separator);
        push_u64(&mut buf, vu(row, "descendants"));

        // 9-23: Boolean flag columns (15 columns)
        for &flag in PARITY_BOOL_FLAGS {
            buf.push_str(separator);
            buf.push_str(if flags & flag != 0 { ctx.pos } else { ctx.neg });
        }

        // 24: ParityAttributes (masked to 15 bits)
        buf.push_str(separator);
        push_u64(&mut buf, u64::from(flags & parity_flags::PARITY_MASK));

        buf.push('\n');
        writer.write_all(buf.as_bytes())?;
    }

    Ok(())
}

/// Append a `u64` value to a string buffer without allocation.
fn push_u64(buf: &mut String, value: u64) {
    use core::fmt::Write;
    let _ok = write!(buf, "{value}");
}

/// Format a Unix-microsecond timestamp with timezone offset into `buf`.
#[expect(
    clippy::cast_sign_loss,
    reason = "timestamp values are non-negative in practice"
)]
fn append_datetime_tz(buf: &mut String, unix_us: i64, tz_offset_secs: i32) {
    use core::fmt::Write;

    if unix_us <= 0 {
        return;
    }
    let secs = unix_us / 1_000_000;
    let adjusted = secs + i64::from(tz_offset_secs);
    if adjusted < 0 {
        return;
    }
    let total = adjusted as u64;
    let seconds = total % 60;
    let minutes = (total / 60) % 60;
    let hours = (total / 3600) % 24;
    let days = total / 86400;
    let (year, month, day) = days_to_ymd(days);
    let _ok = write!(
        buf,
        "{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02}:{seconds:02}"
    );
}

/// Format a Unix-microsecond timestamp into `YYYY-MM-DD HH:MM:SS` UTC.
#[expect(
    clippy::cast_sign_loss,
    reason = "timestamp values are non-negative in practice"
)]
fn format_unix_us(unix_us: i64) -> String {
    if unix_us <= 0 {
        return String::new();
    }
    let secs = (unix_us / 1_000_000) as u64;
    format_unix_timestamp(secs)
}

/// Format a Unix timestamp as `YYYY-MM-DD HH:MM:SS` UTC.
///
/// Minimal implementation — no timezone, no leap-second handling.
/// Sufficient for file timestamps.
fn format_unix_timestamp(secs: u64) -> String {
    // Days since Unix epoch
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02}:{seconds:02}")
}

/// Convert days since Unix epoch to (year, month, day).
///
/// Uses the civil calendar algorithm from Howard Hinnant.
#[expect(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "calendar arithmetic requires signed intermediates and truncation is safe for valid dates"
)]
fn days_to_ymd(days: u64) -> (u32, u32, u32) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    let z = days as i64 + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = i64::from(yoe) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (year as u32, month, day)
}

/// Append the legacy drive footer for baseline-compatible custom output.
///
/// Uses CRLF line endings (`\r\n`) to match legacy baseline behavior.
fn write_legacy_drive_footer<W: Write + ?Sized>(
    writer: &mut W,
    ctx: &CppFooterContext<'_>,
) -> Result<()> {
    if ctx.output_targets.is_empty() {
        return Ok(());
    }

    write!(writer, "\r\n\r\n")?;
    write!(
        writer,
        "Drives? \t{}\t{}\r\n",
        ctx.output_targets.len(),
        format_legacy_drive_letters(ctx.output_targets)
    )?;
    write!(writer, "\r\n")?;

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

/// Format drive letters using the legacy footer style.
#[must_use]
fn format_legacy_drive_letters(output_targets: &[char]) -> String {
    output_targets
        .iter()
        .map(|drive| format!("{}:", drive.to_ascii_uppercase()))
        .collect::<Vec<_>>()
        .join("|")
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
