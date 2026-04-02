//! Read the NTFS `$UpCase` table from a live Windows volume.
//!
//! `$UpCase` (FRS 10) stores 128 KB of UTF-16 uppercase mappings.  The
//! actual table data is **non-resident** — it lives in disk clusters,
//! not in the MFT record.  Windows blocks opening `$UpCase` as a
//! regular file, so we read via: open volume → seek to FRS 10 in
//! MFT → parse DATA attribute data runs → read clusters.
//!
//! # Usage
//!
//! Called from `uffs_mft save --drive C --output upcase.bin --upcase`.

use crate::error::{MftError, Result};
#[cfg(windows)]
use crate::ntfs::DataRun;

/// FRS number for `$UpCase`.
#[cfg(windows)]
const UPCASE_FRS: u64 = 10;

/// Expected data size in bytes (65 536 entries × 2).
pub const UPCASE_SIZE_BYTES: usize = 65_536 * 2;

/// Parsed `$UpCase` metadata extracted from FRS 10.
#[cfg(windows)]
#[derive(Debug)]
struct UpcaseDataRuns {
    /// Data run descriptors (VCN → LCN mapping).
    runs: Vec<DataRun>,
    /// Actual data size from the non-resident DATA attribute header.
    data_size: u64,
}

/// Parse a single MFT record's DATA attribute to extract data runs.
///
/// `record_bytes` must be the raw FRS 10 bytes *after* USA fixup.
#[cfg(windows)]
fn parse_data_runs(record_bytes: &[u8]) -> Result<UpcaseDataRuns> {
    use crate::ntfs::{AttributeIterator, AttributeType};

    let attrs = AttributeIterator::new(record_bytes)
        .ok_or_else(|| MftError::InvalidData("FRS 10 ($UpCase): invalid record header".into()))?;

    let data_attr = attrs
        .filter(|a| {
            a.attribute_type() == Some(AttributeType::Data)
                && a.is_non_resident()
                && a.header.name_length == 0
        })
        .next()
        .ok_or_else(|| {
            MftError::InvalidData("FRS 10 ($UpCase): no non-resident unnamed DATA attribute".into())
        })?;

    let nr = data_attr.non_resident_data().ok_or_else(|| {
        MftError::InvalidData("FRS 10 ($UpCase): cannot decode non-resident header".into())
    })?;

    let runs = data_attr.data_runs();
    if runs.is_empty() {
        return Err(MftError::InvalidData(
            "FRS 10 ($UpCase): DATA has no data runs".into(),
        ));
    }

    Ok(UpcaseDataRuns {
        runs,
        data_size: nr.data_size as u64,
    })
}

/// Read the `$UpCase` table from a live NTFS volume.
///
/// Opens the volume, reads FRS 10 from the MFT, parses its data runs,
/// reads the referenced clusters, and returns the 128 KB table.
///
/// # Errors
///
/// Returns [`MftError::PlatformNotSupported`] on non-Windows.
#[cfg(not(windows))]
pub fn read_upcase_table(_drive: char) -> Result<Box<[u16; 65_536]>> {
    Err(MftError::PlatformNotSupported)
}

/// Read the `$UpCase` table from a live NTFS volume (Windows).
#[cfg(windows)]
pub fn read_upcase_table(drive: char) -> Result<Box<[u16; 65_536]>> {
    use crate::parse::apply_fixup;
    use crate::platform::VolumeHandle;

    let handle = VolumeHandle::open(drive)?;
    let vol = handle.volume_data();
    let rs = vol.bytes_per_file_record_segment as usize;
    let mft_offset = handle.mft_byte_offset();
    let frs10_offset = mft_offset + UPCASE_FRS * rs as u64;

    // Read FRS 10 from the MFT on disk.
    let mut record = vec![0u8; rs];
    volume_read_at(handle.raw_handle(), frs10_offset, &mut record)?;
    apply_fixup(&mut record);

    // Parse data runs.
    let info = parse_data_runs(&record)?;
    if info.data_size as usize != UPCASE_SIZE_BYTES {
        return Err(MftError::InvalidData(format!(
            "$UpCase data_size {} != expected {UPCASE_SIZE_BYTES}",
            info.data_size
        )));
    }

    tracing::debug!(
        runs = info.runs.len(),
        data_size = info.data_size,
        "Parsed $UpCase data runs from FRS 10"
    );

    // Read clusters.
    let buf = read_clusters(handle.raw_handle(), &info.runs, vol.bytes_per_cluster)?;

    // Reinterpret as [u16; 65_536].
    let u16_slice: &[u16] = bytemuck::cast_slice(&buf);
    let mut table = Box::new([0u16; 65_536]);
    table.copy_from_slice(u16_slice);

    tracing::info!(
        bytes = UPCASE_SIZE_BYTES,
        "Read $UpCase table from live volume"
    );
    Ok(table)
}

// ── Windows I/O helpers ───────────────────────────────────────────────

/// Seek + read from a raw volume handle.
#[cfg(windows)]
fn volume_read_at(
    handle: windows::Win32::Foundation::HANDLE,
    offset: u64,
    buf: &mut [u8],
) -> Result<()> {
    use windows::Win32::Storage::FileSystem::{FILE_BEGIN, ReadFile, SetFilePointerEx};

    #[expect(
        clippy::cast_possible_wrap,
        reason = "volume offsets fit in i64 for volumes < 8 EB"
    )]
    let seek_pos = offset as i64;

    // SAFETY: SetFilePointerEx is a well-defined Win32 API.
    #[expect(unsafe_code, reason = "FFI: SetFilePointerEx")]
    unsafe {
        SetFilePointerEx(handle, seek_pos, None, FILE_BEGIN).map_err(|e| {
            MftError::InvalidData(format!("$UpCase: seek to offset {offset} failed: {e}"))
        })?;
    }

    let mut bytes_read = 0u32;
    // SAFETY: ReadFile writes into valid writable `buf`.
    #[expect(unsafe_code, reason = "FFI: ReadFile")]
    unsafe {
        ReadFile(handle, Some(buf), Some(&mut bytes_read), None).map_err(|e| {
            MftError::InvalidData(format!("$UpCase: read {} bytes failed: {e}", buf.len()))
        })?;
    }

    if (bytes_read as usize) < buf.len() {
        return Err(MftError::InvalidData(format!(
            "$UpCase: short read: got {bytes_read}/{}",
            buf.len()
        )));
    }
    Ok(())
}

/// Read clusters from data runs into a contiguous buffer.
#[cfg(windows)]
fn read_clusters(
    handle: windows::Win32::Foundation::HANDLE,
    runs: &[DataRun],
    bytes_per_cluster: u32,
) -> Result<Vec<u8>> {
    let bpc = bytes_per_cluster as u64;
    let mut buf = vec![0u8; UPCASE_SIZE_BYTES];
    let mut offset: usize = 0;

    for run in runs {
        if run.lcn == 0 {
            // Sparse — already zeroed.
            offset += (run.cluster_count * bpc) as usize;
            continue;
        }

        let disk_byte = run.lcn * bpc as i64;
        let run_bytes = (run.cluster_count * bpc) as usize;
        let read_len = run_bytes.min(UPCASE_SIZE_BYTES - offset);

        #[expect(clippy::cast_sign_loss, reason = "LCN is positive for non-sparse runs")]
        let disk_offset = disk_byte as u64;
        volume_read_at(handle, disk_offset, &mut buf[offset..offset + read_len])?;
        offset += read_len;
    }

    if offset < UPCASE_SIZE_BYTES {
        return Err(MftError::InvalidData(format!(
            "$UpCase: assembled only {offset}/{UPCASE_SIZE_BYTES} bytes"
        )));
    }
    Ok(buf)
}
