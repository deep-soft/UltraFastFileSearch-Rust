// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Regression tests for the split NTFS module surface.

#![expect(
    clippy::indexing_slicing,
    clippy::min_ident_chars,
    clippy::default_numeric_fallback,
    reason = "test code — relaxed linting for test clarity"
)]

use core::mem::size_of;

use super::*;

fn write_u16_le(buffer: &mut [u8], offset: usize, value: u16) {
    buffer[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32_le(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_i64_le(buffer: &mut [u8], offset: usize, value: i64) {
    buffer[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn test_filetime_conversion() {
    let filetime: i64 = 133_485_408_000_000_000;
    let unix_micros = filetime_to_unix_micros(filetime);
    assert_eq!(unix_micros, 1_704_067_200_000_000);
}

#[test]
fn test_filetime_to_calendar_post_1970() {
    // 2024-01-01 00:00:00 UTC
    let filetime: i64 = 133_485_408_000_000_000;
    let cal = filetime_to_calendar(filetime);
    assert_eq!(cal, Some((2024, 1, 1, 0, 0, 0)));
}

#[test]
fn test_filetime_to_calendar_pre_1970() {
    // 1959-12-02 03:45:50 UTC — the exact case from the parity baseline.
    // From Dec 2, 1959 00:00:00 to Jan 1, 1970 00:00:00:
    //   1960-1969 = 10 years = 7*365 + 3*366 = 3653 days
    //   Dec 2, 1959 to Jan 1, 1960 = 30 days
    //   Total = 3683 days → unix_secs at midnight = -318_211_200
    //   Plus 3h45m50s = 13550s → -318_197_650
    let unix_secs: i64 = -318_197_650;
    let filetime = unix_secs * FILETIME_TICKS_PER_SECOND + FILETIME_UNIX_DIFF;
    let cal = filetime_to_calendar(filetime);
    assert_eq!(cal, Some((1959, 12, 2, 3, 45, 50)));
}

#[test]
fn test_filetime_to_calendar_zero_is_none() {
    assert_eq!(filetime_to_calendar(0), None);
}

// ═══════════════════════════════════════════════════════════════════════════
// filetime_to_unix_micros — edge cases
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_filetime_to_unix_micros_zero_returns_zero() {
    // FILETIME 0 means "unset" — must map to 0, not a 1601 date.
    assert_eq!(filetime_to_unix_micros(0), 0);
}

#[test]
fn test_filetime_to_unix_micros_unix_epoch() {
    // FILETIME at the exact Unix epoch (1970-01-01 00:00:00).
    assert_eq!(filetime_to_unix_micros(FILETIME_UNIX_DIFF), 0);
}

#[test]
fn test_filetime_to_unix_micros_pre_1970() {
    // 1960-01-01 00:00:00 — 10 years before Unix epoch.
    // Days: 1960-01-01 to 1970-01-01 = 3652 days (2 leap years: 1960, 1964, 1968 →
    // 3 leap years) Actually: 1960..1969 inclusive has leap years
    // 1960,1964,1968 → 3*366 + 7*365 = 3653 days Unix µs = -3653 * 86400 *
    // 1_000_000 = -315_619_200_000_000
    let ft_1960 = FILETIME_UNIX_DIFF - 3653 * 86400 * FILETIME_TICKS_PER_SECOND;
    let us = filetime_to_unix_micros(ft_1960);
    assert_eq!(us, -315_619_200_000_000);
    assert!(us < 0, "pre-1970 dates must produce negative unix micros");
}

#[test]
fn test_filetime_to_unix_micros_filetime_epoch() {
    // FILETIME = 1 (one 100ns tick after 1601-01-01 00:00:00).
    // Should produce a large negative Unix µs (roughly -11644473600 seconds).
    let us = filetime_to_unix_micros(1);
    assert!(
        us < -11_000_000_000_000_000,
        "1601 date should be far negative"
    );
}

#[test]
fn test_filetime_to_unix_micros_y2k() {
    // 2000-01-01 00:00:00 — 30 years, well-known reference.
    // Unix timestamp = 946684800 seconds = 946_684_800_000_000 µs
    let expected_us: i64 = 946_684_800_000_000;
    let ft = FILETIME_UNIX_DIFF + expected_us * FILETIME_TICKS_PER_MICROSECOND;
    assert_eq!(filetime_to_unix_micros(ft), expected_us);
}

#[test]
fn test_filetime_to_unix_micros_roundtrip_with_calendar() {
    // Verify that filetime → unix_micros and filetime → calendar agree.
    let ft_2024: i64 = 133_485_408_000_000_000; // 2024-01-01 00:00:00
    let us = filetime_to_unix_micros(ft_2024);
    let cal = filetime_to_calendar(ft_2024);

    // Unix micros → days → should match calendar day
    let days_since_epoch = us / (86_400 * 1_000_000);
    assert_eq!(days_since_epoch, 19723); // 2024-01-01 is day 19723
    assert_eq!(cal, Some((2024, 1, 1, 0, 0, 0)));
}

// ═══════════════════════════════════════════════════════════════════════════
// filetime_to_calendar — additional edge cases
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_filetime_to_calendar_leap_day_2000() {
    // 2000-02-29 12:00:00 — Feb 29 in a century leap year.
    let unix_secs: i64 = 951_825_600; // 2000-02-29 12:00:00 UTC
    let ft = unix_secs * FILETIME_TICKS_PER_SECOND + FILETIME_UNIX_DIFF;
    assert_eq!(filetime_to_calendar(ft), Some((2000, 2, 29, 12, 0, 0)));
}

#[test]
fn test_filetime_to_calendar_leap_day_2024() {
    // 2024-02-29 23:59:59 — last second of a leap day.
    let unix_secs: i64 = 1_709_251_199; // 2024-02-29 23:59:59 UTC
    let ft = unix_secs * FILETIME_TICKS_PER_SECOND + FILETIME_UNIX_DIFF;
    assert_eq!(filetime_to_calendar(ft), Some((2024, 2, 29, 23, 59, 59)));
}

#[test]
fn test_filetime_to_calendar_non_leap_1900() {
    // 1900-02-28 — 1900 is NOT a leap year (divisible by 100 but not 400).
    let unix_secs: i64 = -2_203_977_600; // 1900-02-28 00:00:00 UTC
    let ft = unix_secs * FILETIME_TICKS_PER_SECOND + FILETIME_UNIX_DIFF;
    assert_eq!(filetime_to_calendar(ft), Some((1900, 2, 28, 0, 0, 0)));
}

#[test]
fn test_filetime_to_calendar_year_boundary() {
    // 1999-12-31 23:59:59 → 2000-01-01 00:00:00 boundary
    let ft_dec31 = (946_684_799_i64) * FILETIME_TICKS_PER_SECOND + FILETIME_UNIX_DIFF;
    let ft_jan01 = (946_684_800_i64) * FILETIME_TICKS_PER_SECOND + FILETIME_UNIX_DIFF;
    assert_eq!(
        filetime_to_calendar(ft_dec31),
        Some((1999, 12, 31, 23, 59, 59))
    );
    assert_eq!(filetime_to_calendar(ft_jan01), Some((2000, 1, 1, 0, 0, 0)));
}

#[test]
fn test_filetime_to_calendar_filetime_epoch_itself() {
    // FILETIME = 1 tick → 1601-01-01 00:00:00 (essentially).
    let cal = filetime_to_calendar(1);
    assert_eq!(cal, Some((1601, 1, 1, 0, 0, 0)));
}

#[test]
fn test_filetime_to_calendar_midnight_exact() {
    // Exactly midnight — time components must all be zero.
    let ft = 86400_i64 * FILETIME_TICKS_PER_SECOND; // day 1 since 1601
    let cal = filetime_to_calendar(ft);
    if let Some((_, _, _, h, m, s)) = cal {
        assert_eq!((h, m, s), (0, 0, 0), "midnight should have 00:00:00");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// filetime_with_tz_bias — currently ZERO coverage
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_filetime_with_tz_bias_zero_no_change() {
    let ft: i64 = 133_485_408_000_000_000;
    assert_eq!(filetime_with_tz_bias(ft, 0), ft);
}

#[test]
fn test_filetime_with_tz_bias_positive_east() {
    // UTC+5 (e.g. Yekaterinburg) — 5 hours ahead.
    let ft: i64 = 133_485_408_000_000_000; // 2024-01-01 00:00:00 UTC
    let biased = filetime_with_tz_bias(ft, 5 * 3600);
    let cal = filetime_to_calendar(biased);
    assert_eq!(cal, Some((2024, 1, 1, 5, 0, 0)), "UTC+5 should show 05:00");
}

#[test]
fn test_filetime_with_tz_bias_negative_west() {
    // UTC-8 (US Pacific) — 8 hours behind.
    let ft: i64 = 133_485_408_000_000_000; // 2024-01-01 00:00:00 UTC
    let biased = filetime_with_tz_bias(ft, -8 * 3600);
    let cal = filetime_to_calendar(biased);
    // 00:00 - 8h = previous day 16:00
    assert_eq!(
        cal,
        Some((2023, 12, 31, 16, 0, 0)),
        "UTC-8 should roll back to Dec 31"
    );
}

#[test]
fn test_filetime_with_tz_bias_half_hour() {
    // UTC+5:30 (India) — non-integer hour offset.
    let ft: i64 = 133_485_408_000_000_000; // 2024-01-01 00:00:00 UTC
    let biased = filetime_with_tz_bias(ft, 5 * 3600 + 1800);
    let cal = filetime_to_calendar(biased);
    assert_eq!(
        cal,
        Some((2024, 1, 1, 5, 30, 0)),
        "UTC+5:30 should show 05:30"
    );
}

#[test]
fn test_file_reference_extraction() {
    let file_ref: u64 = (7_u64 << 48) | 0x3039;
    assert_eq!(file_reference_to_frs(file_ref), 12345);
    assert_eq!(file_reference_to_sequence(file_ref), 7);
}

#[test]
fn test_attribute_type_from_u32() {
    assert_eq!(
        AttributeType::from_u32(0x10),
        Some(AttributeType::StandardInformation)
    );
    assert_eq!(AttributeType::from_u32(0x30), Some(AttributeType::FileName));
    assert_eq!(AttributeType::from_u32(0x80), Some(AttributeType::Data));
    assert_eq!(
        AttributeType::from_u32(0xFFFF_FFFF),
        Some(AttributeType::End)
    );
    assert_eq!(AttributeType::from_u32(0x99), None);
}

#[test]
fn test_file_record_flags() {
    let header = FileRecordSegmentHeader {
        multi_sector_header: MultiSectorHeader {
            magic: FILE_RECORD_MAGIC,
            usa_offset: 0,
            usa_count: 0,
        },
        log_file_sequence_number: 0,
        sequence_number: 1,
        link_count: 1,
        first_attribute_offset: 56,
        flags: 0x0003,
        bytes_in_use: 0,
        bytes_allocated: 0,
        base_file_record_segment: 0,
        next_attribute_number: 0,
        reserved: 0,
        segment_number_lower: 0,
    };

    assert!(header.is_in_use());
    assert!(header.is_directory());
    assert!(header.is_base_record());
}

#[test]
fn test_fixup_file_record_applies_usa_from_safe_header_decode() {
    let mut record = vec![0_u8; 1024];
    let usa_offset = 0x30;
    let check_value = 0xABCD;
    let original_first = 0x1234;
    let original_second = 0x5678;

    record[0..4].copy_from_slice(b"FILE");
    write_u16_le(&mut record, 4, crate::len_to_u16(usa_offset));
    write_u16_le(&mut record, 6, 3);
    write_u16_le(&mut record, usa_offset, check_value);
    write_u16_le(&mut record, usa_offset + 2, original_first);
    write_u16_le(&mut record, usa_offset + 4, original_second);
    write_u16_le(&mut record, SECTOR_SIZE - 2, check_value);
    write_u16_le(&mut record, SECTOR_SIZE * 2 - 2, check_value);

    assert!(fixup_file_record(&mut record));
    assert_eq!(
        &record[SECTOR_SIZE - 2..SECTOR_SIZE],
        &original_first.to_le_bytes()
    );
    assert_eq!(
        &record[SECTOR_SIZE * 2 - 2..SECTOR_SIZE * 2],
        &original_second.to_le_bytes()
    );
}

#[test]
fn test_attribute_iterator_reads_resident_attribute_value() {
    let mut record = vec![0_u8; 96];
    let record_len = crate::len_to_u32(record.len());
    let first_attribute_offset = size_of::<FileRecordSegmentHeader>();
    let attr_offset = first_attribute_offset;
    let attr_length = size_of::<AttributeRecordHeader>() + size_of::<ResidentAttributeData>() + 4;
    let end_marker_offset = attr_offset + attr_length;

    record[0..4].copy_from_slice(b"FILE");
    write_u16_le(&mut record, 20, crate::len_to_u16(first_attribute_offset));
    write_u16_le(&mut record, 22, FileRecordFlags::InUse as u16); // enum discriminant is 0x0001
    write_u32_le(
        &mut record,
        24,
        crate::len_to_u32(end_marker_offset + size_of::<AttributeRecordHeader>()),
    );
    write_u32_le(&mut record, 28, record_len);

    write_u32_le(&mut record, attr_offset, AttributeType::DATA_TYPE);
    write_u32_le(&mut record, attr_offset + 4, crate::len_to_u32(attr_length));
    record[attr_offset + 8] = 0;
    record[attr_offset + 9] = 0;
    write_u16_le(&mut record, attr_offset + 10, 0);
    write_u16_le(&mut record, attr_offset + 12, 0);
    write_u16_le(&mut record, attr_offset + 14, 1);

    write_u32_le(&mut record, attr_offset + 16, 4);
    write_u16_le(
        &mut record,
        attr_offset + 20,
        crate::len_to_u16(size_of::<AttributeRecordHeader>() + size_of::<ResidentAttributeData>()),
    );
    write_u16_le(&mut record, attr_offset + 22, 0);
    record[attr_offset + 24..attr_offset + 28].copy_from_slice(&[1, 2, 3, 4]);

    write_u32_le(&mut record, end_marker_offset, AttributeType::END_MARKER);

    let mut iter = AttributeIterator::new(&record).expect("valid record header");
    let attribute = iter.next().expect("resident attribute");

    assert_eq!(attribute.attribute_type(), Some(AttributeType::Data));
    assert_eq!(attribute.resident_value(), Some(&[1, 2, 3, 4][..]));
    assert!(iter.next().is_none());
}

#[test]
fn test_non_resident_attribute_helpers_decode_mapping_pairs() {
    let mut attr =
        vec![0_u8; size_of::<AttributeRecordHeader>() + size_of::<NonResidentAttributeData>() + 4];
    let attr_len = crate::len_to_u32(attr.len());

    write_u32_le(&mut attr, 0, AttributeType::DATA_TYPE);
    write_u32_le(&mut attr, 4, attr_len);
    attr[8] = 1;
    write_u16_le(&mut attr, 12, 0x0001);
    write_u16_le(&mut attr, 14, 2);

    let nr_offset = size_of::<AttributeRecordHeader>();
    write_i64_le(&mut attr, nr_offset, 7);
    write_i64_le(&mut attr, nr_offset + 8, 11);
    write_u16_le(
        &mut attr,
        nr_offset + 16,
        crate::len_to_u16(nr_offset + size_of::<NonResidentAttributeData>()),
    );
    attr[nr_offset + 18] = 0;
    write_i64_le(&mut attr, nr_offset + 24, 40);
    write_i64_le(&mut attr, nr_offset + 32, 20);
    write_i64_le(&mut attr, nr_offset + 40, 20);
    attr[nr_offset + 48..nr_offset + 52].copy_from_slice(&[0x11, 0x05, 0x0A, 0x00]);

    let attribute = AttributeRef {
        data: &attr,
        header: AttributeRecordHeader {
            type_code: AttributeType::DATA_TYPE,
            length: crate::len_to_u32(attr.len()),
            is_non_resident: 1,
            name_length: 0,
            name_offset: 0,
            flags: 0x0001,
            instance: 2,
        },
    };

    let nr_data = attribute.non_resident_data().expect("non-resident header");
    let lowest_vcn = nr_data.lowest_vcn;
    assert_eq!(lowest_vcn, 7);
    assert_eq!(
        nr_data.mapping_pairs_offset as usize,
        nr_offset + size_of::<NonResidentAttributeData>()
    );

    assert_eq!(attribute.data_runs(), vec![DataRun {
        vcn: 7,
        cluster_count: 5,
        lcn: 10,
    }]);
}
