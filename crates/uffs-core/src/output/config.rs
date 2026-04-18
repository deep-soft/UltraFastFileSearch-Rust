// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Output configuration and row formatting helpers.

use core::fmt::Write as _;
use std::io::Write;

use uffs_polars::{Column, DataFrame, DataType};

use super::{BASELINE_COLUMN_ORDER, OutputColumn};
use crate::error::Result;

/// Output configuration for customizable formatting.
#[derive(Debug, Clone)]
pub struct OutputConfig {
    /// Columns to output (None = all available).
    pub columns: Option<Vec<OutputColumn>>,
    /// Column separator (default: ",").
    pub separator: String,
    /// Quote character for strings (default: "\"").
    pub quote: String,
    /// Include header row (default: true).
    pub header: bool,
    /// Representation for true/active boolean (default: "1").
    pub pos: String,
    /// Representation for false/inactive boolean (default: "0").
    pub neg: String,
    /// Fixed timezone offset in seconds from UTC (computed once at startup).
    /// This matches established behavior where Windows'
    /// `FileTimeToLocalFileTime()` uses the CURRENT timezone offset for ALL
    /// timestamps, ignoring historical DST.
    pub timezone_offset_secs: i32,
    /// Parity-compat mode: directories get trailing `\` in `Path`,
    /// empty `Name`, self-path in `PathOnly`, and treesize for `Size`.
    pub parity_compat: bool,
    // NOTE: Tripwire was removed from OutputConfig (Fix #1).
    // Tripwire is now logged to stderr/tracing and embedded in binary string table.
    // See TRIPWIRE constant in uffs-cli/src/commands.rs.
}

impl Default for OutputConfig {
    fn default() -> Self {
        // Get current timezone offset once. On Windows,
        // Windows' FileTimeToLocalFileTime() uses the CURRENT offset for all timestamps
        let timezone_offset_secs = chrono::Local::now().offset().local_minus_utc();

        Self {
            columns: None,
            separator: ",".to_owned(),
            quote: "\"".to_owned(),
            header: true,
            pos: "1".to_owned(),
            neg: "0".to_owned(),
            timezone_offset_secs,
            parity_compat: false,
        }
    }
}

impl OutputConfig {
    /// Create a new output configuration with defaults.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse columns from a comma-separated string.
    ///
    /// Special value "all" returns None (meaning all columns).
    #[must_use]
    #[expect(
        clippy::shadow_reuse,
        reason = "rebinding input to trimmed+lowered version is clearer than a new name"
    )]
    pub fn parse_columns(input: &str) -> Option<Vec<OutputColumn>> {
        let input = input.trim().to_lowercase();
        if input == "all" {
            return None;
        }
        if input == "parity" {
            return Some(super::column::PARITY_COLUMN_ORDER.to_vec());
        }

        let cols: Vec<OutputColumn> = input
            .split(',')
            .filter_map(|col| OutputColumn::parse(col.trim()))
            .collect();

        if cols.is_empty() { None } else { Some(cols) }
    }

    /// Parse separator with special character handling.
    ///
    /// Supports (case-insensitive):
    /// - TAB → `\t`
    /// - NEWLINE, NEW LINE → `\n`
    /// - SPACE → ` `
    /// - RETURN → `\r`
    /// - DOUBLE → `"`
    /// - SINGLE → `'`
    /// - NULL → `\0`
    #[must_use]
    pub fn parse_separator(input: &str) -> String {
        match input.to_uppercase().as_str() {
            "TAB" => "\t".to_owned(),
            "NEWLINE" | "NEW LINE" => "\n".to_owned(),
            "SPACE" => " ".to_owned(),
            "RETURN" => "\r".to_owned(),
            "DOUBLE" => "\"".to_owned(),
            "SINGLE" => "'".to_owned(),
            "NULL" => "\0".to_owned(),
            _ => input.to_owned(),
        }
    }

    /// Set columns from string.
    #[must_use]
    pub fn with_columns(mut self, columns: &str) -> Self {
        self.columns = Self::parse_columns(columns);
        self
    }

    /// Set separator.
    #[must_use]
    pub fn with_separator(mut self, sep: &str) -> Self {
        self.separator = Self::parse_separator(sep);
        self
    }

    /// Set quote character.
    #[must_use]
    pub fn with_quote(mut self, quote: &str) -> Self {
        quote.clone_into(&mut self.quote);
        self
    }

    /// Set header inclusion.
    #[must_use]
    pub const fn with_header(mut self, header: bool) -> Self {
        self.header = header;
        self
    }

    /// Set positive boolean representation.
    #[must_use]
    pub fn with_pos(mut self, pos: &str) -> Self {
        pos.clone_into(&mut self.pos);
        self
    }

    /// Set negative boolean representation.
    #[must_use]
    pub fn with_neg(mut self, neg: &str) -> Self {
        neg.clone_into(&mut self.neg);
        self
    }

    /// Override the timezone offset used for timestamp display.
    ///
    /// Accepts offset in hours from UTC (e.g., `-8` for PST, `-7` for PDT,
    /// `1` for CET). This overrides the auto-detected local timezone offset.
    ///
    /// Useful for reproducible parity testing when the reference output was
    /// generated in a different DST period than the current one.
    #[must_use]
    pub const fn with_tz_offset_hours(mut self, hours: i32) -> Self {
        self.timezone_offset_secs = hours * 3_600_i32;
        self
    }

    /// Enable parity-compat directory formatting.
    #[must_use]
    pub const fn with_parity_compat(mut self, enabled: bool) -> Self {
        self.parity_compat = enabled;
        self
    }

    // NOTE: with_tripwire() was removed (Fix #1).
    // Tripwire is now logged to stderr/tracing and embedded in binary string table.
    // See TRIPWIRE constant in uffs-cli/src/commands.rs.

    /// Check if the descendants column is requested.
    #[must_use]
    pub fn needs_descendants(&self) -> bool {
        self.columns
            .as_ref()
            .is_some_and(|cols| cols.contains(&OutputColumn::Descendants))
    }

    /// Check if the path column is requested.
    ///
    /// The path column requires resolution from FRS + `parent_frs`.
    /// Returns true when columns is None (meaning "all") since "all" includes
    /// Path.
    #[must_use]
    pub fn needs_path_column(&self) -> bool {
        self.columns.as_ref().is_none_or(|cols| {
            cols.contains(&OutputColumn::Path) || cols.contains(&OutputColumn::PathOnly)
        })
    }

    /// Check if any tree-derived columns are requested.
    /// Note: "all" columns does NOT include tree columns by default (they're
    /// expensive to compute).
    #[must_use]
    pub fn needs_tree_columns(&self) -> bool {
        self.columns
            .as_ref()
            .is_some_and(|cols| cols.iter().any(|col| col.is_tree_field()))
    }

    /// Get the list of requested tree columns.
    #[must_use]
    pub fn get_tree_columns(&self) -> Vec<crate::tree::TreeColumn> {
        self.columns
            .as_ref()
            .map(|cols| cols.iter().filter_map(|col| col.to_tree_column()).collect())
            .unwrap_or_default()
    }

    /// Write `DataFrame` to output with this configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails.
    #[expect(
        clippy::option_if_let_else,
        reason = "if-let-else is clearer for control flow with early return"
    )]
    pub fn write<W: Write>(&self, df: &DataFrame, mut writer: W) -> Result<()> {
        // Determine columns to output - use BASELINE_COLUMN_ORDER when "all" is
        // specified
        let output_cols: &[OutputColumn] = if let Some(cols) = &self.columns {
            cols.as_slice()
        } else {
            BASELINE_COLUMN_ORDER
        };

        let fixed_tz = chrono::FixedOffset::east_opt(self.timezone_offset_secs);

        let resolved_columns: Vec<_> = output_cols
            .iter()
            .map(|col| {
                df.column(col.df_column())
                    .map_or_else(|_| Err(col.default_value()), Ok)
            })
            .collect();

        // NOTE: Tripwire is now logged to stderr/tracing instead of CSV output.
        // This keeps CSV output strict (header + data rows only) for parity analysis.
        // The tripwire is also embedded in the binary string table (see TRIPWIRE
        // constant).

        // Write header if enabled
        if self.header {
            let mut header = String::with_capacity(output_cols.len() * 24);
            for (idx, col) in output_cols.iter().enumerate() {
                if idx > 0 {
                    header.push_str(&self.separator);
                }
                header.push_str(&self.quote);
                header.push_str(col.display_name());
                header.push_str(&self.quote);
            }
            // Header followed by empty line
            header.push('\n');
            header.push('\n');
            writer.write_all(header.as_bytes())?;
        }

        // Write data rows
        let mut row_buffer = String::with_capacity(output_cols.len() * 32);
        for row_idx in 0..df.height() {
            row_buffer.clear();

            for (idx, resolved_column) in resolved_columns.iter().enumerate() {
                if idx > 0 {
                    row_buffer.push_str(&self.separator);
                }

                match resolved_column {
                    Ok(series) => {
                        self.write_value(&mut row_buffer, series, row_idx, fixed_tz.as_ref());
                    }
                    Err(default_value) => {
                        // Column not in DataFrame - use appropriate default.
                        // Numeric columns (like Descendants) should show "0".
                        row_buffer.push_str(default_value);
                    }
                }
            }

            row_buffer.push('\n');
            writer.write_all(row_buffer.as_bytes())?;
        }

        Ok(())
    }

    /// Write `DisplayRow` results directly — **no `DataFrame` involved**.
    ///
    /// Uses the same separator / quote / header / boolean formatting as
    /// [`write`](Self::write) so output is identical.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying writer fails.
    pub fn write_display_rows<W: Write>(
        &self,
        rows: &[crate::search::backend::DisplayRow],
        mut writer: W,
    ) -> Result<()> {
        let output_cols: &[OutputColumn] = self
            .columns
            .as_ref()
            .map_or(BASELINE_COLUMN_ORDER, |cols| cols.as_slice());

        // Header
        if self.header {
            let mut header = String::with_capacity(output_cols.len() * 24);
            for (idx, col) in output_cols.iter().enumerate() {
                if idx > 0 {
                    header.push_str(&self.separator);
                }
                header.push_str(&self.quote);
                header.push_str(col.display_name());
                header.push_str(&self.quote);
            }
            header.push('\n');
            header.push('\n');
            writer.write_all(header.as_bytes())?;
        }

        // Data rows
        let mut buf = String::with_capacity(output_cols.len() * 32);
        let mut itoa_buf = itoa::Buffer::new();
        for row in rows {
            buf.clear();
            write_display_row_columns(&mut buf, &mut itoa_buf, output_cols, self, row);
            buf.push('\n');
            writer.write_all(buf.as_bytes())?;
        }

        Ok(())
    }

    /// Append a single formatted series value to the provided row buffer.
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "intentional catch-all for remaining dtypes"
    )]
    fn write_value(
        &self,
        row_buffer: &mut String,
        series: &Column,
        row_idx: usize,
        fixed_tz: Option<&chrono::FixedOffset>,
    ) {
        use uffs_polars::{AnyValue, TimeUnit};

        let dtype = series.dtype();

        match dtype {
            DataType::Boolean => {
                if let Ok(val) = series.bool() {
                    match val.get(row_idx) {
                        Some(true) => row_buffer.push_str(&self.pos),
                        Some(false) => row_buffer.push_str(&self.neg),
                        None => {}
                    }
                }
            }
            DataType::String => {
                if let Ok(val) = series.str()
                    && let Some(str_val) = val.get(row_idx)
                {
                    row_buffer.push_str(&self.quote);
                    row_buffer.push_str(str_val);
                    row_buffer.push_str(&self.quote);
                }
            }
            DataType::UInt64 => {
                if let Ok(val) = series.u64() {
                    match val.get(row_idx) {
                        Some(number) => {
                            Self::append_display(row_buffer, number);
                        }
                        None => row_buffer.push('0'),
                    }
                } else {
                    row_buffer.push('0');
                }
            }
            DataType::Int64 => {
                if let Ok(val) = series.i64() {
                    match val.get(row_idx) {
                        Some(number) => {
                            Self::append_display(row_buffer, number);
                        }
                        None => row_buffer.push('0'),
                    }
                } else {
                    row_buffer.push('0');
                }
            }
            DataType::UInt32 => {
                if let Ok(val) = series.u32() {
                    match val.get(row_idx) {
                        Some(number) => {
                            Self::append_display(row_buffer, number);
                        }
                        None => row_buffer.push('0'),
                    }
                } else {
                    row_buffer.push('0');
                }
            }
            DataType::Int32 => {
                if let Ok(val) = series.i32() {
                    match val.get(row_idx) {
                        Some(number) => {
                            Self::append_display(row_buffer, number);
                        }
                        None => row_buffer.push('0'),
                    }
                } else {
                    row_buffer.push('0');
                }
            }
            DataType::Datetime(TimeUnit::Microseconds, _) => {
                Self::append_filetime_value(row_buffer, series, row_idx, fixed_tz);
            }
            _ => {
                if let Ok(val) = series.get(row_idx)
                    && !matches!(val, AnyValue::Null)
                {
                    Self::append_display(row_buffer, val);
                }
            }
        }
    }

    /// Format the value at `row_idx` of a `Datetime(Microseconds)` column
    /// as a calendar string, treating the underlying i64 as **raw
    /// FILETIME** (100-ns ticks since 1601-01-01).
    ///
    /// ── v13+ FILETIME semantics ─────────────────────────────────────────
    ///
    /// The `DataFrame` schema declares timestamp columns as
    /// `Datetime(TimeUnit::Microseconds)` for backward compatibility with
    /// Polars analytics (date ops, SQL coercion, Parquet round-trips),
    /// but the underlying i64 values are **raw FILETIME** — see
    /// `uffs-mft::index::dataframe.rs` and
    /// `uffs-mft::reader::dataframe_build.rs`, which push
    /// `rec.stdinfo.created` (FILETIME per the `StandardInfo` doc)
    /// directly into the column with a type cast rather than a value
    /// conversion.
    ///
    /// Formatting therefore has to go through the FILETIME decomposition,
    /// not `chrono::DateTime::from_timestamp` with Unix-micros semantics —
    /// using the latter produces year-6220 output for 2026-era timestamps
    /// (combined ~369-year + 10× unit offset between the two encodings,
    /// same bug class that caused the `append_datetime_native` regression
    /// in this file).
    ///
    /// NOTE: Polars' own `CsvWriter` (used by `uffs load --output *.csv`)
    /// still formats this column as Unix-micros via its built-in
    /// `Datetime` serializer.  Fixing that requires a `DataFrame` schema
    /// change (switch to `Int64` or pre-convert values to Unix micros)
    /// and is tracked as a separate latent bug.
    ///
    /// Polars exposes two variants — `Datetime` (borrowed tz) and
    /// `DatetimeOwned` (owned tz `Arc`) — depending on how the column was
    /// constructed.  Both are matched here.
    fn append_filetime_value(
        row_buffer: &mut String,
        series: &Column,
        row_idx: usize,
        fixed_tz: Option<&chrono::FixedOffset>,
    ) {
        use uffs_polars::{AnyValue, TimeUnit};

        let filetime_opt: Option<i64> = match series.get(row_idx) {
            Ok(
                AnyValue::Datetime(ticks, TimeUnit::Microseconds, _)
                | AnyValue::DatetimeOwned(ticks, TimeUnit::Microseconds, _),
            ) => Some(ticks),
            _ => None,
        };
        let Some(filetime) = filetime_opt else { return };
        let tz_offset_secs: i32 = fixed_tz.map_or(0_i32, chrono::FixedOffset::local_minus_utc);
        let local_ft = uffs_time::filetime_with_tz_bias(filetime, tz_offset_secs);
        if let Some((year, month, day, hour, minute, second)) =
            uffs_time::filetime_to_calendar(local_ft)
        {
            Self::append_display(
                row_buffer,
                format_args!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"),
            );
        }
    }

    /// Append a displayable value to the row buffer without intermediate
    /// allocations.
    fn append_display<T>(row_buffer: &mut String, value: T)
    where
        T: core::fmt::Display,
    {
        if row_buffer.write_fmt(format_args!("{value}")).is_err() {
            row_buffer.push_str(&value.to_string());
        }
    }
}

// ─── Native DisplayRow output ───────────────────────────────────────────────

/// NTFS attribute flag constants for bit-testing `DisplayRow::flags`.
mod attr {
    /// Read-only.
    pub(super) const READONLY: u32 = 0x0001;
    /// Hidden.
    pub(super) const HIDDEN: u32 = 0x0002;
    /// System.
    pub(super) const SYSTEM: u32 = 0x0004;
    /// Directory.
    pub(super) const DIRECTORY: u32 = 0x0010;
    /// Archive.
    pub(super) const ARCHIVE: u32 = 0x0020;
    /// Temporary.
    pub(super) const TEMPORARY: u32 = 0x0100;
    /// Sparse.
    pub(super) const SPARSE: u32 = 0x0200;
    /// Reparse point.
    pub(super) const REPARSE: u32 = 0x0400;
    /// Compressed.
    pub(super) const COMPRESSED: u32 = 0x0800;
    /// Offline.
    pub(super) const OFFLINE: u32 = 0x1000;
    /// Not content indexed.
    pub(super) const NOT_INDEXED: u32 = 0x2000;
    /// Encrypted.
    pub(super) const ENCRYPTED: u32 = 0x4000;
    /// Integrity stream.
    pub(super) const INTEGRITY: u32 = 0x8000;
    /// Virtual.
    pub(super) const VIRTUAL: u32 = 0x0001_0000;
    /// No scrub data.
    pub(super) const NO_SCRUB: u32 = 0x0002_0000;
    /// Recall on open.
    pub(super) const RECALL_ON_OPEN: u32 = 0x0004_0000;
    /// Pinned.
    pub(super) const PINNED: u32 = 0x0008_0000;
    /// Unpinned.
    pub(super) const UNPINNED: u32 = 0x0010_0000;
    /// Recall on data access.
    pub(super) const RECALL_ON_DATA: u32 = 0x0040_0000;
    /// Parity-compat mask — must match `StandardInfo::parity_attributes()`.
    ///
    /// Includes the 15 attribute bits the legacy baseline tracks:
    /// `READONLY` | `HIDDEN` | `SYSTEM` | `DIRECTORY` | `ARCHIVE` | `SPARSE` |
    /// `REPARSE` | `COMPRESSED` | `OFFLINE` | `NOT_INDEXED` | `ENCRYPTED` |
    /// `INTEGRITY` | `NO_SCRUB` | `PINNED` | `UNPINNED`.
    ///
    /// Note: excludes `TEMPORARY` (0x100) and `VIRTUAL` (0x10000) which are
    /// NOT part of the parity contract.
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

/// Write one `DisplayRow` into `buf` using the configured columns.
///
/// Extracted as a standalone function for readability — the column match has
/// ~30 arms mirroring all `OutputColumn` variants.
#[expect(
    clippy::too_many_lines,
    reason = "exhaustive match over ~30 OutputColumn variants; each arm is 1–8 lines \
              of formatting — splitting would scatter the column→text dispatch table"
)]
fn write_display_row_columns(
    buf: &mut String,
    itoa_buf: &mut itoa::Buffer,
    output_cols: &[OutputColumn],
    cfg: &OutputConfig,
    row: &crate::search::backend::DisplayRow,
) {
    let flags = row.flags;
    // Parity-compat: directories get trailing `\`, empty name, self-path.
    let parity_dir = cfg.parity_compat && row.is_directory;

    for (idx, col) in output_cols.iter().enumerate() {
        if idx > 0 {
            buf.push_str(&cfg.separator);
        }
        match col {
            OutputColumn::Path => {
                buf.push_str(&cfg.quote);
                buf.push_str(&row.path);
                if parity_dir && !row.path.ends_with('\\') {
                    buf.push('\\');
                }
                buf.push_str(&cfg.quote);
            }
            OutputColumn::Name => {
                buf.push_str(&cfg.quote);
                if !parity_dir {
                    buf.push_str(row.name());
                }
                // parity dirs: empty name (just quotes)
                buf.push_str(&cfg.quote);
            }
            OutputColumn::PathOnly => {
                buf.push_str(&cfg.quote);
                if parity_dir {
                    // Legacy: PathOnly = full path with trailing `\`
                    buf.push_str(&row.path);
                    if !row.path.ends_with('\\') {
                        buf.push('\\');
                    }
                } else if let Some(pos) = row.path.rfind('\\') {
                    buf.push_str(row.path.get(..=pos).unwrap_or(&row.path));
                } else {
                    buf.push_str(&row.path);
                }
                buf.push_str(&cfg.quote);
            }
            OutputColumn::Size => {
                if parity_dir {
                    buf.push_str(itoa_buf.format(row.treesize));
                } else {
                    buf.push_str(itoa_buf.format(row.size));
                }
            }
            OutputColumn::SizeOnDisk => {
                if parity_dir {
                    buf.push_str(itoa_buf.format(row.tree_allocated));
                } else {
                    buf.push_str(itoa_buf.format(row.allocated));
                }
            }
            OutputColumn::Created => {
                append_datetime_native(buf, row.created, cfg.timezone_offset_secs);
            }
            OutputColumn::Modified => {
                append_datetime_native(buf, row.modified, cfg.timezone_offset_secs);
            }
            OutputColumn::Accessed => {
                append_datetime_native(buf, row.accessed, cfg.timezone_offset_secs);
            }
            OutputColumn::Descendants => {
                buf.push_str(itoa_buf.format(row.descendants));
            }
            OutputColumn::TreeSize => {
                buf.push_str(itoa_buf.format(row.treesize));
            }
            OutputColumn::TreeAllocated => {
                buf.push_str(itoa_buf.format(row.tree_allocated));
            }
            OutputColumn::Type => {
                buf.push_str(&cfg.quote);
                buf.push_str(crate::search::derived::semantic_type_for_row(row));
                buf.push_str(&cfg.quote);
            }
            OutputColumn::Attributes | OutputColumn::AttributeValue => {
                buf.push_str(itoa_buf.format(flags));
            }
            OutputColumn::ParityAttributes => {
                buf.push_str(itoa_buf.format(flags & attr::PARITY_MASK));
            }
            OutputColumn::Hidden => push_flag(buf, cfg, flags, attr::HIDDEN),
            OutputColumn::System => push_flag(buf, cfg, flags, attr::SYSTEM),
            OutputColumn::Archive => push_flag(buf, cfg, flags, attr::ARCHIVE),
            OutputColumn::ReadOnly => push_flag(buf, cfg, flags, attr::READONLY),
            OutputColumn::Compressed => push_flag(buf, cfg, flags, attr::COMPRESSED),
            OutputColumn::Encrypted => push_flag(buf, cfg, flags, attr::ENCRYPTED),
            OutputColumn::Sparse => push_flag(buf, cfg, flags, attr::SPARSE),
            OutputColumn::Reparse => push_flag(buf, cfg, flags, attr::REPARSE),
            OutputColumn::Offline => push_flag(buf, cfg, flags, attr::OFFLINE),
            OutputColumn::NotIndexed => push_flag(buf, cfg, flags, attr::NOT_INDEXED),
            OutputColumn::Temporary => push_flag(buf, cfg, flags, attr::TEMPORARY),
            OutputColumn::Virtual => push_flag(buf, cfg, flags, attr::VIRTUAL),
            OutputColumn::Pinned => push_flag(buf, cfg, flags, attr::PINNED),
            OutputColumn::Unpinned => push_flag(buf, cfg, flags, attr::UNPINNED),
            OutputColumn::DirectoryFlag => push_flag(buf, cfg, flags, attr::DIRECTORY),
            OutputColumn::Integrity => push_flag(buf, cfg, flags, attr::INTEGRITY),
            OutputColumn::NoScrub => push_flag(buf, cfg, flags, attr::NO_SCRUB),
            OutputColumn::RecallOnOpen => push_flag(buf, cfg, flags, attr::RECALL_ON_OPEN),
            OutputColumn::RecallOnDataAccess => push_flag(buf, cfg, flags, attr::RECALL_ON_DATA),
            OutputColumn::Bulkiness => {
                // Allocated-to-logical ratio × 100 (integer percentage).
                // 100 = perfectly packed. >100 = cluster slack / waste.
                // 0 for zero-byte files. Directories use tree metrics.
                let per_million = crate::search::derived::bulkiness_for_row(row);
                // Convert per-million → percentage (÷ 10_000).
                let pct = per_million / 10_000;
                buf.push_str(itoa_buf.format(pct));
            }
            OutputColumn::Drive => {
                buf.push(row.drive);
            }
            OutputColumn::Extension => {
                buf.push_str(&cfg.quote);
                if let Some(dot) = row.name().rfind('.') {
                    buf.push_str(row.name().get(dot + 1..).unwrap_or(""));
                }
                buf.push_str(&cfg.quote);
            }
            // Newly added columns that have no dedicated text formatter yet.
            OutputColumn::NameLength => {
                let len = uffs_mft::len_to_u16(row.name().chars().count());
                buf.push_str(itoa_buf.format(len));
            }
            OutputColumn::PathLength => {
                let len = uffs_mft::len_to_u16(row.path.chars().count());
                buf.push_str(itoa_buf.format(len));
            }
        }
    }
}

/// Append a boolean flag test result.
fn push_flag(buf: &mut String, cfg: &OutputConfig, flags: u32, mask: u32) {
    if flags & mask != 0 {
        buf.push_str(&cfg.pos);
    } else {
        buf.push_str(&cfg.neg);
    }
}

/// Append `YYYY-MM-DD HH:MM:SS` from a raw FILETIME (100-ns ticks since
/// 1601-01-01) with timezone offset.
///
/// v13+ of the compact index stores timestamps as **raw FILETIME** (matching
/// the C++ NTFS baseline), not Unix microseconds.  Callers in this file
/// pass `row.modified` / `row.created` / `row.accessed` which are FILETIME
/// values — previously this function mis-interpreted them as Unix
/// microseconds and produced year-6220 output for 2026-era timestamps (the
/// ~369-year + 10× unit offset between the two encodings).
///
/// Delegates to `uffs_time::filetime_with_tz_bias` + `filetime_to_calendar`
/// for the canonical Hinnant civil-calendar decomposition — same helpers
/// used by the parity-compat CSV writer in
/// `uffs_cli::commands::output::parity::append_datetime_tz`.
///
/// Regression-pinned by `append_datetime_native_formats_filetime_as_2024`
/// in this module's `tests` submodule.
fn append_datetime_native(buf: &mut String, filetime: i64, tz_offset_secs: i32) {
    use core::fmt::Write;

    let local_ft = uffs_time::filetime_with_tz_bias(filetime, tz_offset_secs);
    if let Some((year, month, day, hour, minute, second)) =
        uffs_time::filetime_to_calendar(local_ft)
    {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "String::write_fmt never fails"
        )]
        let _ = write!(
            buf,
            "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
        );
    } else {
        // `filetime == 0` (unset / null) — surface as the zero sentinel.
        buf.push_str("0000-00-00 00:00:00");
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
