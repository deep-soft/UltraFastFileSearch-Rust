// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Parity-compat 25-column CSV output, timestamp formatting, and legacy footer.

use std::io::Write;

use anyhow::Result;
use serde_json::Value;

use super::{CppFooterContext, ParityContext, parity_flags, vb, vi, vs, vu, vu32};

/// Parity-compat CSV header (25 columns, matching legacy baseline).
pub(super) const PARITY_HEADER: &[&str] = &[
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
pub(super) fn write_parity<W: Write>(
    writer: &mut W,
    rows: &[Value],
    separator: &str,
    quote: &str,
    ctx: &ParityContext<'_>,
) -> Result<()> {
    let mut header = String::with_capacity(512);
    for (idx, col) in PARITY_HEADER.iter().enumerate() {
        if idx > 0 {
            header.push_str(separator);
        }
        header.push_str(quote);
        header.push_str(col);
        header.push_str(quote);
    }
    header.push('\n');
    header.push('\n');
    writer.write_all(header.as_bytes())?;

    let mut buf = String::with_capacity(512);
    for row in rows {
        buf.clear();
        write_parity_row(&mut buf, row, separator, quote, ctx);
        buf.push('\n');
        writer.write_all(buf.as_bytes())?;
    }
    Ok(())
}

/// Write a single parity-compat CSV row.
fn write_parity_row(
    buf: &mut String,
    row: &Value,
    sep: &str,
    quote: &str,
    ctx: &ParityContext<'_>,
) {
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
    buf.push_str(sep);
    buf.push_str(quote);
    if !is_dir {
        buf.push_str(&name);
    }
    buf.push_str(quote);

    // 2: PathOnly (quoted)
    buf.push_str(sep);
    buf.push_str(quote);
    if is_dir {
        buf.push_str(&path);
        if !path.ends_with('\\') {
            buf.push('\\');
        }
    } else if let Some(pos) = path.rfind('\\') {
        if let Some(slice) = path.get(..=pos) {
            buf.push_str(slice);
        }
    } else {
        buf.push_str(&path);
    }
    buf.push_str(quote);

    // 3: Size (treesize for dirs)
    buf.push_str(sep);
    push_u64(
        buf,
        if is_dir {
            vu(row, "treesize")
        } else {
            vu(row, "size")
        },
    );

    // 4: SizeOnDisk (tree_allocated for dirs)
    buf.push_str(sep);
    push_u64(
        buf,
        if is_dir {
            vu(row, "tree_allocated")
        } else {
            vu(row, "allocated")
        },
    );

    // 5-7: Created, Modified, Accessed
    for key in &["created", "modified", "accessed"] {
        buf.push_str(sep);
        append_datetime_tz(buf, vi(row, key), ctx.tz_offset_secs);
    }

    // 8: Descendants
    buf.push_str(sep);
    push_u64(buf, vu(row, "descendants"));

    // 9-23: Boolean flag columns (15 columns)
    for &flag in PARITY_BOOL_FLAGS {
        buf.push_str(sep);
        buf.push_str(if flags & flag != 0 { ctx.pos } else { ctx.neg });
    }

    // 24: ParityAttributes (masked to 15 bits)
    buf.push_str(sep);
    push_u64(buf, u64::from(flags & parity_flags::PARITY_MASK));
}

/// Append a `u64` value to a string buffer without allocation.
fn push_u64(buf: &mut String, value: u64) {
    use core::fmt::Write;
    let _ok = write!(buf, "{value}");
}

/// Format a raw FILETIME with timezone bias directly into `buf`.
///
/// Mirrors C++ `RtlTimeToTimeFields` — applies TZ bias in FILETIME ticks,
/// then decomposes.  No intermediate Unix conversion.
fn append_datetime_tz(buf: &mut String, filetime: i64, tz_offset_secs: i32) {
    use core::fmt::Write;
    let local_ft = uffs_mft::ntfs::filetime_with_tz_bias(filetime, tz_offset_secs);
    if let Some((year, month, day, hour, minute, second)) =
        uffs_mft::ntfs::filetime_to_calendar(local_ft)
    {
        let _ok = write!(
            buf,
            "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
        );
    }
}

/// Append the legacy drive footer for baseline-compatible custom output.
///
/// Uses CRLF line endings (`\r\n`) to match legacy baseline behavior.
pub(super) fn write_legacy_drive_footer<W: Write + ?Sized>(
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
