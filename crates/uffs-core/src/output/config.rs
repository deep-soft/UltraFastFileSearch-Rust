//! Output configuration and row formatting helpers.

use core::fmt::Write as _;
use std::io::Write;

use uffs_polars::{Column, DataFrame, DataType};

use super::{CPP_COLUMN_ORDER, OutputColumn};
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
    /// C++ parity-compat mode: directories get trailing `\` in `Path`,
    /// empty `Name`, self-path in `PathOnly`, and treesize for `Size`.
    pub parity_compat: bool,
    // NOTE: Tripwire was removed from OutputConfig (Fix #1).
    // Tripwire is now logged to stderr/tracing and embedded in binary string table.
    // See TRIPWIRE constant in uffs-cli/src/commands.rs.
}

impl Default for OutputConfig {
    fn default() -> Self {
        // Get current timezone offset once, matching C++ behavior where
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

    /// Enable C++ parity-compat directory formatting.
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
        // Determine columns to output - use CPP_COLUMN_ORDER when "all" is specified
        let output_cols: &[OutputColumn] = if let Some(cols) = &self.columns {
            cols.as_slice()
        } else {
            CPP_COLUMN_ORDER
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
            // C++ outputs header followed by empty line
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
                        // Numeric columns (like Descendants) should show "0" to match C++.
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
            .map_or(CPP_COLUMN_ORDER, |cols| cols.as_slice());

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
                // Convert UTC timestamp to local time using FIXED offset (matching C++ output).
                // C++ uses Windows' FileTimeToLocalFileTime() which applies the CURRENT
                // timezone offset to ALL timestamps, ignoring historical DST transitions.
                // We match this by using a fixed offset computed once at startup.
                if let Ok(AnyValue::Datetime(ts, TimeUnit::Microseconds, _)) = series.get(row_idx)
                    && let Some(utc_dt) = {
                        // Use div_euclid/rem_euclid for correct handling of negative timestamps.
                        // rem_euclid(1_000_000) always returns [0, 999_999] for any i64 input.
                        let secs = ts.div_euclid(1_000_000);
                        let micros_i64 = ts.rem_euclid(1_000_000);
                        // Safe: rem_euclid(1_000_000) is always in [0, 999_999], fits in u32
                        let micros = u32::try_from(micros_i64).unwrap_or(0);
                        chrono::DateTime::from_timestamp(secs, micros * 1000)
                    }
                {
                    // Apply fixed timezone offset (computed once at startup)
                    // This matches established behavior: same offset for all timestamps
                    if let Some(timezone_offset) = fixed_tz {
                        let local_dt = utc_dt.with_timezone(timezone_offset);
                        // Format WITHOUT subseconds to match C++ output exactly
                        Self::append_display(row_buffer, local_dt.format("%Y-%m-%d %H:%M:%S"));
                    } else {
                        // Fallback: format as UTC if offset is invalid
                        Self::append_display(row_buffer, utc_dt.format("%Y-%m-%d %H:%M:%S"));
                    }
                }
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
    pub const READONLY: u32 = 0x0001;
    /// Hidden.
    pub const HIDDEN: u32 = 0x0002;
    /// System.
    pub const SYSTEM: u32 = 0x0004;
    /// Directory.
    pub const DIRECTORY: u32 = 0x0010;
    /// Archive.
    pub const ARCHIVE: u32 = 0x0020;
    /// Temporary.
    pub const TEMPORARY: u32 = 0x0100;
    /// Sparse.
    pub const SPARSE: u32 = 0x0200;
    /// Reparse point.
    pub const REPARSE: u32 = 0x0400;
    /// Compressed.
    pub const COMPRESSED: u32 = 0x0800;
    /// Offline.
    pub const OFFLINE: u32 = 0x1000;
    /// Not content indexed.
    pub const NOT_INDEXED: u32 = 0x2000;
    /// Encrypted.
    pub const ENCRYPTED: u32 = 0x4000;
    /// Integrity stream.
    pub const INTEGRITY: u32 = 0x8000;
    /// Virtual.
    pub const VIRTUAL: u32 = 0x0001_0000;
    /// No scrub data.
    pub const NO_SCRUB: u32 = 0x0002_0000;
    /// Recall on open.
    pub const RECALL_ON_OPEN: u32 = 0x0004_0000;
    /// Pinned.
    pub const PINNED: u32 = 0x0008_0000;
    /// Unpinned.
    pub const UNPINNED: u32 = 0x0010_0000;
    /// Recall on data access.
    pub const RECALL_ON_DATA: u32 = 0x0040_0000;
    /// Parity-compat mask — must match `StandardInfo::parity_attributes()`.
    ///
    /// Includes the 15 attribute bits the C++ baseline tracks:
    /// `READONLY` | `HIDDEN` | `SYSTEM` | `DIRECTORY` | `ARCHIVE` | `SPARSE` |
    /// `REPARSE` | `COMPRESSED` | `OFFLINE` | `NOT_INDEXED` | `ENCRYPTED` |
    /// `INTEGRITY` | `NO_SCRUB` | `PINNED` | `UNPINNED`.
    ///
    /// Note: excludes `TEMPORARY` (0x100) and `VIRTUAL` (0x10000) which are
    /// NOT part of the parity contract.
    pub const PARITY_MASK: u32 = READONLY
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
    clippy::single_call_fn,
    reason = "separated for readability of 30-arm match"
)]
#[expect(
    clippy::too_many_lines,
    reason = "column dispatch — flat match arms, splitting hurts readability"
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
                // tree_allocated not in DisplayRow — fall back to allocated
                buf.push_str(itoa_buf.format(row.allocated));
            }
            OutputColumn::Type => {
                // Extract extension from name
                buf.push_str(&cfg.quote);
                if let Some(dot) = row.name().rfind('.') {
                    buf.push_str(row.name().get(dot + 1..).unwrap_or(""));
                }
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
                buf.push_str(OutputColumn::Bulkiness.default_value());
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
            // Any other FieldId variant not in the output set — skip.
            #[expect(
                unreachable_patterns,
                reason = "forward-compat for new FieldId variants"
            )]
            _ => {}
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

/// Append `YYYY-MM-DD HH:MM:SS` from Unix microseconds with timezone offset.
///
/// Same algorithm as `row_writer::append_datetime` — no `chrono` overhead.
#[expect(
    clippy::cast_sign_loss,
    reason = "rem_euclid always returns non-negative"
)]
#[expect(
    clippy::cast_possible_truncation,
    reason = "day_secs/doe bounded within u32"
)]
fn append_datetime_native(buf: &mut String, timestamp_micros: i64, tz_offset_secs: i32) {
    use core::fmt::Write;

    let adjusted_secs = timestamp_micros.div_euclid(1_000_000) + i64::from(tz_offset_secs);
    let day_secs = adjusted_secs.rem_euclid(86_400) as u32;
    let days = adjusted_secs.div_euclid(86_400) + 719_468;

    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let doe = (days - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let year_offset = i64::from(yoe) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let month_proxy = (5 * doy + 2) / 153;
    let day = doy - (153 * month_proxy + 2) / 5 + 1;
    let month = if month_proxy < 10 {
        month_proxy + 3
    } else {
        month_proxy - 9
    };
    let year = if month <= 2 {
        year_offset + 1
    } else {
        year_offset
    };
    let hour = day_secs / 3600;
    let minute = (day_secs % 3600) / 60;
    let second = day_secs % 60;

    #[expect(
        clippy::let_underscore_must_use,
        reason = "String::write_fmt never fails"
    )]
    let _ = write!(
        buf,
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
    );
}
