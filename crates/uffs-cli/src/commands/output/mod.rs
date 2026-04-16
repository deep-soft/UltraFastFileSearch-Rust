// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Output helpers for CLI search commands.
//!
//! Formats `SearchRow` (from the daemon protocol) directly — no polars,
//! no `DisplayRow`, no `DataFrame`.  This is the thin-client output path.

mod parity;

use core::time::Duration;
use std::fs::File;
use std::io::{BufWriter, Write};

use anyhow::{Context, Result};
use parity::{write_legacy_drive_footer, write_parity};
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
        tz_offset_secs: tz_offset.map_or_else(
            || *LOCAL_TZ_OFFSET_SECS,
            |hours| hours.saturating_mul(3_600_i32),
        ),
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

/// Serialise rows as NDJSON (one JSON object per line).
fn write_json<W: Write>(writer: &mut W, rows: &[Value]) -> Result<()> {
    for row in rows {
        serde_json::to_writer(&mut *writer, row)?;
        writeln!(writer)?;
    }
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

// ── Column definition table ─────────────────────────────────────────
//
// Inlined from `uffs-core::FieldId` / `field_metadata` so the CLI stays
// dependency-free (thin-client design).  Keep in sync with FieldId.

/// A column definition: `(canonical_name, &[aliases], display_name)`.
type ColDef = (&'static str, &'static [&'static str], &'static str);

/// Lookup table: canonical name + aliases → display name.
static COL_TABLE: &[ColDef] = &[
    ("name", &[], "Name"),
    ("path", &[], "Path"),
    ("path_only", &["pathonly", "path only"], "Path Only"),
    ("size", &[], "Size"),
    (
        "size_on_disk",
        &["allocated", "allocated_size", "sod"],
        "Size on Disk",
    ),
    ("created", &[], "Created"),
    ("modified", &["written"], "Last Written"),
    ("accessed", &[], "Last Accessed"),
    ("extension", &["ext"], "Extension"),
    ("drive", &["drv"], "Drive"),
    ("type", &["kind"], "Type"),
    ("descendants", &[], "Descendants"),
    ("treesize", &["tree_size"], "Tree Size"),
    ("tree_allocated", &[], "Tree Allocated"),
    ("bulkiness", &[], "Bulkiness"),
    ("name_length", &["namelength", "name length"], "Name Length"),
    ("path_length", &["pathlength", "path length"], "Path Length"),
    // Boolean attribute columns
    ("hidden", &[], "Hidden"),
    ("system", &[], "System"),
    ("archive", &[], "Archive"),
    ("readonly", &["read_only"], "Read-only"),
    ("compressed", &[], "Compressed"),
    ("encrypted", &[], "Encrypted"),
    ("sparse", &[], "Sparse"),
    ("reparse", &[], "Reparse"),
    ("offline", &[], "Offline"),
    (
        "not_indexed",
        &["notindexed", "not indexed"],
        "Not content indexed file",
    ),
    (
        "directory_flag",
        &["directoryflag", "directory flag"],
        "Directory Flag",
    ),
    ("integrity", &[], "Integrity"),
    ("no_scrub", &["noscrub"], "No scrub file"),
    ("pinned", &[], "Pinned"),
    ("unpinned", &[], "Unpinned"),
    ("recall_on_open", &["recallonopen"], "Recall on open"),
    (
        "recall_on_data_access",
        &["recallondataaccess"],
        "Recall on data access",
    ),
    ("temporary", &[], "Temporary"),
    ("virtual", &[], "Virtual"),
    ("attributes", &["parity_attributes"], "Attributes"),
    ("attribute_value", &[], "AttributeValue"),
    ("flags", &[], "Flags"),
];

/// Column order used when `--columns all` is specified (matches
/// `uffs-core::output::column::BASELINE_COLUMN_ORDER`).
static ALL_COLUMNS: &[&str] = &[
    "path",
    "name",
    "path_only",
    "size",
    "size_on_disk",
    "created",
    "modified",
    "accessed",
    "descendants",
    "readonly",
    "hidden",
    "system",
    "directory_flag",
    "archive",
    "sparse",
    "reparse",
    "compressed",
    "offline",
    "not_indexed",
    "encrypted",
    "integrity",
    "no_scrub",
    "recall_on_open",
    "pinned",
    "unpinned",
    "recall_on_data_access",
    "attributes",
    "treesize",
    "tree_allocated",
    "bulkiness",
    "type",
    "extension",
    "name_length",
    "path_length",
];

/// Default column set when none is specified.
static DEFAULT_COLS: &[&str] = &["name", "size", "modified", "path"];

/// Resolve a user column name to its canonical name.
fn resolve_col_name(input: &str) -> Option<&'static str> {
    let lowered = input.to_ascii_lowercase();
    let trimmed = lowered.trim();
    for &(canon, aliases, _display) in COL_TABLE {
        if canon.eq_ignore_ascii_case(trimmed) {
            return Some(canon);
        }
        for &alias in aliases {
            if alias.eq_ignore_ascii_case(trimmed) {
                return Some(canon);
            }
        }
    }
    None
}

/// Get display name for a canonical column name.
fn display_name(canonical: &str) -> &str {
    for &(canon, _, display) in COL_TABLE {
        if canon == canonical {
            return display;
        }
    }
    canonical
}

/// Resolve column specification string to a list of canonical names.
fn resolve_columns(columns: &str) -> Vec<&'static str> {
    if columns.is_empty() {
        DEFAULT_COLS.to_vec()
    } else if columns.eq_ignore_ascii_case("all") {
        ALL_COLUMNS.to_vec()
    } else {
        columns
            .split(',')
            .filter_map(|name| resolve_col_name(name.trim()))
            .collect()
    }
}

/// Write columnar (CSV-style) output from `SearchRow` fields.
///
/// Columns are resolved through the inline column table so display
/// names, flag decomposition, and derived columns (Path Only, Bulkiness,
/// etc.) work correctly.
fn write_columnar<W: Write>(
    writer: &mut W,
    rows: &[Value],
    columns: &str,
    separator: &str,
    quote: &str,
    header: bool,
) -> Result<()> {
    let fields = resolve_columns(columns);

    // Header row — use display_name() for Title-Case headers.
    if header {
        for (idx, field) in fields.iter().enumerate() {
            if idx > 0 {
                write!(writer, "{separator}")?;
            }
            let name = display_name(field);
            if quote.is_empty() {
                write!(writer, "{name}")?;
            } else {
                write!(writer, "{quote}{name}{quote}")?;
            }
        }
        writeln!(writer)?;
    }

    for row in rows {
        for (idx, field) in fields.iter().enumerate() {
            if idx > 0 {
                write!(writer, "{separator}")?;
            }
            let value = extract_field(row, field);
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

/// Extract a field value from a JSON row by canonical column name.
///
/// Handles flag decomposition, path derivation, and computed columns.
fn extract_field(row: &Value, field: &str) -> String {
    let flags = vu32(row, "flags");
    match field {
        "name" => vs(row, "name"),
        "path" => vs(row, "path"),
        "path_only" => {
            let path = vs(row, "path");
            if vb(row, "is_directory") {
                path
            } else if let Some(pos) = path.rfind('\\') {
                path.get(..=pos).unwrap_or(&path).to_owned()
            } else {
                path
            }
        }
        "size" => vu(row, "size").to_string(),
        "size_on_disk" => vu(row, "allocated").to_string(),
        "created" => format_unix_us(vi(row, "created")),
        "modified" => format_unix_us(vi(row, "modified")),
        "accessed" => format_unix_us(vi(row, "accessed")),
        "extension" => extract_extension(&vs(row, "name")),
        "drive" => vs(row, "drive"),
        "type" => if vb(row, "is_directory") {
            "dir"
        } else {
            "file"
        }
        .to_owned(),
        "descendants" => vu(row, "descendants").to_string(),
        "treesize" => vu(row, "treesize").to_string(),
        "tree_allocated" => vu(row, "tree_allocated").to_string(),
        "bulkiness" => {
            let is_dir = vb(row, "is_directory");
            let (logical, alloc) = if is_dir {
                (vu(row, "treesize"), vu(row, "tree_allocated"))
            } else {
                (vu(row, "size"), vu(row, "allocated"))
            };
            alloc
                .checked_mul(100)
                .and_then(|numerator| numerator.checked_div(logical))
                .unwrap_or(0)
                .to_string()
        }
        "name_length" => vs(row, "name").len().to_string(),
        "path_length" => vs(row, "path").len().to_string(),
        // Boolean flag columns
        "hidden" => flag_bit(flags, parity_flags::HIDDEN),
        "system" => flag_bit(flags, parity_flags::SYSTEM),
        "archive" => flag_bit(flags, parity_flags::ARCHIVE),
        "readonly" => flag_bit(flags, parity_flags::READONLY),
        "compressed" => flag_bit(flags, parity_flags::COMPRESSED),
        "encrypted" => flag_bit(flags, parity_flags::ENCRYPTED),
        "sparse" => flag_bit(flags, parity_flags::SPARSE),
        "reparse" => flag_bit(flags, parity_flags::REPARSE),
        "offline" => flag_bit(flags, parity_flags::OFFLINE),
        "not_indexed" => flag_bit(flags, parity_flags::NOT_INDEXED),
        "directory_flag" => flag_bit(flags, parity_flags::DIRECTORY),
        "integrity" => flag_bit(flags, parity_flags::INTEGRITY),
        "no_scrub" => flag_bit(flags, parity_flags::NO_SCRUB),
        "pinned" => flag_bit(flags, parity_flags::PINNED),
        "unpinned" => flag_bit(flags, parity_flags::UNPINNED),
        "recall_on_open" => flag_bit(flags, 0x0004_0000),
        "recall_on_data_access" => flag_bit(flags, 0x0040_0000),
        "temporary" => flag_bit(flags, 0x0100),
        "virtual" => flag_bit(flags, 0x0001_0000),
        "attributes" | "parity_attributes" => (flags & parity_flags::PARITY_MASK).to_string(),
        "attribute_value" | "flags" => flags.to_string(),
        _ => String::new(),
    }
}

/// Format a flag bit as "1" or "0".
fn flag_bit(flags: u32, bit: u32) -> String {
    if flags & bit != 0 { "1" } else { "0" }.to_owned()
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

/// Local timezone offset in seconds, computed once at startup.
///
/// Matches C++ behavior where `FileTimeToLocalFileTime()` uses the
/// CURRENT timezone offset for ALL timestamps, ignoring historical
/// DST transitions.
///
/// Uses platform APIs (no chrono dependency) via `uffs-client`.
static LOCAL_TZ_OFFSET_SECS: std::sync::LazyLock<i32> =
    std::sync::LazyLock::new(uffs_client::format::local_utc_offset_secs);

/// Format a Unix-microsecond timestamp into `YYYY-MM-DD HH:MM:SS` local time.
///
/// Applies the fixed local timezone offset captured at startup.
#[expect(
    clippy::cast_sign_loss,
    reason = "timestamp values are non-negative in practice"
)]
fn format_unix_us(unix_us: i64) -> String {
    if unix_us <= 0 {
        return String::new();
    }
    let secs = unix_us / 1_000_000;
    let adjusted = secs + i64::from(*LOCAL_TZ_OFFSET_SECS);
    if adjusted < 0 {
        return String::new();
    }
    format_unix_timestamp(adjusted as u64)
}

/// Format a Unix timestamp as `YYYY-MM-DD HH:MM:SS`.
///
/// Minimal implementation — no leap-second handling.
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

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
