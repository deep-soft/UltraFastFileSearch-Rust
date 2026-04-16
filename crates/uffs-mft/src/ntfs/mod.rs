// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! NTFS-specific structures and parsing.
//!
//! This module provides low-level NTFS structure definitions for parsing
//! the Master File Table (MFT) directly from disk.
//!
//! # Safety
//!
//! These structures use `#[repr(C, packed)]` to match the on-disk layout.
//! Care must be taken when reading fields due to potential unaligned access.
//!
//! # Reference
//!
//! Based on NTFS on-disk format documentation and the Microsoft NTFS
//! specification.
//!
//! # Platform Support
//!
//! This module is cross-platform - NTFS structures are just byte layouts
//! and can be parsed on any platform.

mod boot_sector;
mod data_runs;
mod metadata;
mod records;
#[cfg(test)]
mod tests;

pub use self::boot_sector::NtfsBootSector;
pub use self::data_runs::{DataRun, extract_data_runs_from_attribute, parse_data_runs};
pub use self::metadata::{
    AttributeListEntry, ExtendedStandardInfo, FileNameAttribute, FileNamespace, IndexHeader,
    IndexRoot, NameInfo, ReparseMountPointBuffer, ReparsePointHeader, ReparseTag,
    STANDARD_INFO_SIZE_V12, STANDARD_INFO_SIZE_V30, StandardInformation,
    StandardInformationExtended, StreamInfo, is_internal_windows_stream,
};
pub use self::records::{
    AttributeIterator, AttributeRecordHeader, AttributeRef, AttributeType, FILE_RECORD_MAGIC,
    FileRecordFlags, FileRecordSegmentHeader, INDX_RECORD_MAGIC, MultiSectorHeader,
    NonResidentAttributeData, ResidentAttributeData, SECTOR_SIZE, apply_usa_fixup,
    fixup_file_record,
};

/// Number of 100-nanosecond intervals per second.
pub const FILETIME_TICKS_PER_SECOND: i64 = 10_000_000;

/// Number of 100-nanosecond intervals per microsecond.
pub const FILETIME_TICKS_PER_MICROSECOND: i64 = 10;

/// Difference between the FILETIME epoch (1601-01-01) and the Unix epoch
/// (1970-01-01), in 100-nanosecond intervals.
pub const FILETIME_UNIX_DIFF: i64 = 116_444_736_000_000_000;

/// Converts a Windows FILETIME (100-nanosecond intervals since 1601-01-01)
/// to Unix timestamp in microseconds.
///
/// **Deprecated path** — prefer storing raw FILETIME values and using
/// [`filetime_to_calendar`] for display.  This function exists only for
/// backward compatibility during migration.
#[must_use]
pub const fn filetime_to_unix_micros(filetime: i64) -> i64 {
    if filetime == 0 {
        return 0;
    }
    (filetime - FILETIME_UNIX_DIFF) / FILETIME_TICKS_PER_MICROSECOND
}

/// Decompose a raw FILETIME into calendar fields `(year, month, day, hour,
/// minute, second)`.
///
/// This mirrors the Windows `RtlTimeToTimeFields` approach — works directly
/// with FILETIME ticks (100-ns intervals since 1601-01-01), no intermediate
/// Unix conversion.  Handles all valid FILETIME values including pre-1970.
///
/// Returns `None` for `filetime == 0` (unset / null timestamp in NTFS).
#[must_use]
#[expect(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "Hinnant algorithm: intermediate values are bounded and non-negative for valid dates"
)]
pub const fn filetime_to_calendar(filetime: i64) -> Option<(i32, u32, u32, u32, u32, u32)> {
    if filetime == 0 {
        return None;
    }
    // Convert to total seconds since 1601-01-01, then split into days
    // and time-of-day using Euclidean division (remainder always ≥ 0).
    let total_secs = filetime / FILETIME_TICKS_PER_SECOND;
    let days_since_1601 = total_secs.div_euclid(86400);
    let day_secs = total_secs.rem_euclid(86400);

    let hour = (day_secs / 3600) as u32;
    let minute = ((day_secs % 3600) / 60) as u32;
    let second = (day_secs % 60) as u32;

    // Hinnant algorithm expects days since 0000-03-01.
    //   719468 (0000-03-01 to 1970-01-01) − 134774 (1601-01-01 to 1970-01-01)
    //   = 584694
    let z = days_since_1601 + 584_694; // days since 0000-03-01
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    Some((year as i32, month, day, hour, minute, second))
}

/// Apply a timezone bias (in seconds) to a raw FILETIME value.
///
/// The bias is added as FILETIME ticks.  This is the FILETIME equivalent of
/// `system_time + time_zone_bias` in the C++ code.
#[must_use]
pub const fn filetime_with_tz_bias(filetime: i64, tz_bias_secs: i32) -> i64 {
    filetime + (tz_bias_secs as i64) * FILETIME_TICKS_PER_SECOND
}

/// Extracts the File Record Segment number from a file reference.
///
/// The lower 48 bits contain the FRS number.
#[must_use]
pub const fn file_reference_to_frs(file_reference: u64) -> u64 {
    file_reference & 0x0000_FFFF_FFFF_FFFF
}

/// Extracts the sequence number from a file reference.
///
/// The upper 16 bits contain the sequence number.
#[must_use]
pub const fn file_reference_to_sequence(file_reference: u64) -> u16 {
    (file_reference >> 48_i32) as u16
}
