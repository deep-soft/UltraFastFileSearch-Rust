//! Shared-memory transport for bulk search results (D5.0).
//!
//! When a search returns more rows than [`SHMEM_THRESHOLD`], the daemon
//! writes results to a memory-mapped temp file instead of serialising
//! them as inline JSON.  The client then mmaps the same file, reads the
//! rows, and deletes the file.
//!
//! ## Binary layout (little-endian, `repr(C)`)
//!
//! ```text
//! [ShmemHeader: 48 bytes]
//! [ShmemRecord × row_count: 80 bytes each]
//! [String table: concatenated UTF-8 bytes]
//! ```
//!
//! The string table holds all `path` and `name` values back-to-back.
//! Each [`ShmemRecord`] stores an offset + length pair pointing into the
//! table.

use core::sync::atomic::{AtomicU64, Ordering};
use std::io;
use std::path::{Path, PathBuf};

use crate::protocol::{SearchResponse, SearchRow};

/// Result sets larger than this are written to shared memory.
pub const SHMEM_THRESHOLD: usize = 100_000;

/// Magic bytes identifying a UFFS shmem file (`"UFFS"` as `u32` LE).
const MAGIC: u32 = 0x5346_4655; // b"UFFS" LE

/// Current binary format version.
const VERSION: u32 = 1;

// ── On-disk structures ────────────────────────────────────────────────────

/// File header — fixed 48 bytes at offset 0.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct ShmemHeader {
    /// Magic identifier ([`MAGIC`]).
    magic: u32,
    /// Format version ([`VERSION`]).
    version: u32,
    /// Number of result rows.
    row_count: u64,
    /// Byte offset of the string table from file start.
    strings_offset: u64,
    /// Search duration in milliseconds.
    duration_ms: u64,
    /// Total records scanned.
    records_scanned: u64,
    /// Whether the result set was truncated (0 or 1).
    truncated: u32,
    /// Reserved for future use.
    _reserved: u32,
}

/// Per-row fixed-size record — 80 bytes, naturally aligned.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct ShmemRecord {
    /// Drive letter as ASCII byte.
    drive: u8,
    /// 1 = directory, 0 = file.
    is_directory: u8,
    /// Padding for alignment.
    _pad: [u8; 2],
    /// Raw NTFS attribute flags.
    flags: u32,
    /// Logical file size.
    size: u64,
    /// On-disk allocated size.
    allocated: u64,
    /// Last-modified timestamp (Unix µs).
    modified: i64,
    /// Creation timestamp (Unix µs).
    created: i64,
    /// Last-access timestamp (Unix µs).
    accessed: i64,
    /// Descendant count (dirs only).
    descendants: u32,
    /// Padding.
    _pad2: u32,
    /// Subtree total size (dirs only).
    treesize: u64,
    /// Byte offset of the path string in the string table.
    path_off: u32,
    /// Byte length of the path string.
    path_len: u32,
    /// Byte offset of the name string in the string table.
    name_off: u32,
    /// Byte length of the name string.
    name_len: u32,
}

// Compile-time size checks — binary format depends on exact layout.
const _: () = assert!(
    size_of::<ShmemHeader>() == 48,
    "ShmemHeader layout changed — binary format requires exactly 48 bytes"
);
const _: () = assert!(
    size_of::<ShmemRecord>() == 80,
    "ShmemRecord layout changed — binary format requires exactly 80 bytes"
);

// ── Public API ────────────────────────────────────────────────────────────

/// Directory inside the UFFS data folder where shmem files are stored.
const SHMEM_DIR: &str = "shmem";

/// Monotonic counter for unique shmem file names.
static SHMEM_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Return the shmem directory path, creating it if necessary.
fn shmem_dir() -> io::Result<PathBuf> {
    let base = dirs_next::data_local_dir()
        .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { r"C:\temp" } else { "/tmp" }));
    let dir = base.join("uffs").join(SHMEM_DIR);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Generate a unique shmem file path.
#[expect(clippy::single_call_fn, reason = "helper extracted for clarity")]
fn unique_shmem_path() -> io::Result<PathBuf> {
    let dir = shmem_dir()?;
    let pid = std::process::id();
    let seq = SHMEM_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(dir.join(format!("search_{pid}_{seq}.bin")))
}

/// Write search results to a shared-memory file.
///
/// Returns the file path on success. The caller should include this path
/// in the `SearchResponse.shmem_path` field so the client can read it.
///
/// # Errors
///
/// Returns `io::Error` on file creation, mmap, or write failure.
#[allow(unsafe_code)]
#[expect(
    clippy::indexing_slicing,
    reason = "mmap is sized to total_size; all slices are within bounds by construction"
)]
pub fn write_search_results(
    rows: &[SearchRow],
    duration_ms: u64,
    records_scanned: u64,
    truncated: bool,
) -> io::Result<PathBuf> {
    let path = unique_shmem_path()?;
    let row_count = rows.len();

    // Build string table and record offsets.
    let mut string_table = Vec::new();
    let mut records: Vec<ShmemRecord> = Vec::with_capacity(row_count);
    for row in rows {
        let path_off = u32::try_from(string_table.len()).unwrap_or(u32::MAX);
        let path_bytes = row.path.as_bytes();
        string_table.extend_from_slice(path_bytes);
        let path_len = u32::try_from(path_bytes.len()).unwrap_or(u32::MAX);

        let name_off = u32::try_from(string_table.len()).unwrap_or(u32::MAX);
        let name_bytes = row.name.as_bytes();
        string_table.extend_from_slice(name_bytes);
        let name_len = u32::try_from(name_bytes.len()).unwrap_or(u32::MAX);

        records.push(ShmemRecord {
            drive: row.drive as u8,
            is_directory: u8::from(row.is_directory),
            _pad: [0; 2],
            flags: row.flags,
            size: row.size,
            allocated: row.allocated,
            modified: row.modified,
            created: row.created,
            accessed: row.accessed,
            descendants: row.descendants,
            _pad2: 0,
            treesize: row.treesize,
            path_off,
            path_len,
            name_off,
            name_len,
        });
    }

    let header_size = size_of::<ShmemHeader>();
    let records_size = row_count * size_of::<ShmemRecord>();
    let strings_offset = header_size + records_size;
    let total_size = strings_offset + string_table.len();

    let header = ShmemHeader {
        magic: MAGIC,
        version: VERSION,
        row_count: row_count as u64,
        strings_offset: strings_offset as u64,
        duration_ms,
        records_scanned,
        truncated: u32::from(truncated),
        _reserved: 0,
    };

    // Create file and set size.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)?;
    file.set_len(total_size as u64)?;

    // Safety: we just created the file exclusively and set its length.
    // The mmap is used only within this function scope, then flushed
    // and dropped before we return the path to readers.
    // Safety: file is freshly created, exclusively owned, and sized.
    let mut mmap = unsafe { memmap2::MmapMut::map_mut(&file)? };

    // Write header.
    // Safety: ShmemHeader is repr(C), Copy, and has no padding-dependent
    // invariants.
    let header_bytes: &[u8] = unsafe {
        core::slice::from_raw_parts(core::ptr::from_ref(&header).cast::<u8>(), header_size)
    };
    mmap[..header_size].copy_from_slice(header_bytes);

    // Write records.
    // Safety: ShmemRecord is repr(C), Copy, and the slice is valid for records_size
    // bytes.
    let records_bytes: &[u8] =
        unsafe { core::slice::from_raw_parts(records.as_ptr().cast::<u8>(), records_size) };
    mmap[header_size..header_size + records_size].copy_from_slice(records_bytes);

    // Write string table.
    mmap[strings_offset..strings_offset + string_table.len()].copy_from_slice(&string_table);

    mmap.flush()?;

    Ok(path)
}

/// Read search results from a shared-memory file and delete it.
///
/// Returns a fully populated [`SearchResponse`] with inline `rows`.
/// The shmem file is removed after successful reading.
///
/// # Errors
///
/// Returns `io::Error` on mmap failure, format mismatch, or invalid UTF-8.
#[allow(unsafe_code)]
#[expect(
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "validated: bounds-checked before indexing; 32-bit truncation is acceptable for shmem files"
)]
pub fn read_search_results(path: &Path) -> io::Result<SearchResponse> {
    let file = std::fs::File::open(path)?;

    // Safety: the file was written by our daemon using the same binary
    // layout. We validate magic + version before interpreting data.
    let mmap = unsafe { memmap2::Mmap::map(&file)? };

    let header_size = size_of::<ShmemHeader>();
    if mmap.len() < header_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "shmem file too small",
        ));
    }

    // Read header.
    // Safety: mmap is at least header_size bytes; read_unaligned handles any
    // alignment.
    let header: ShmemHeader =
        unsafe { core::ptr::read_unaligned(mmap.as_ptr().cast::<ShmemHeader>()) };

    if header.magic != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("bad shmem magic: {:#x}", header.magic),
        ));
    }
    if header.version != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported shmem version: {}", header.version),
        ));
    }

    let row_count = header.row_count as usize;
    let record_size = size_of::<ShmemRecord>();
    let records_end = header_size + row_count * record_size;
    let strings_offset = header.strings_offset as usize;

    if mmap.len() < records_end || mmap.len() < strings_offset {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "shmem file truncated",
        ));
    }

    let string_table = &mmap[strings_offset..];
    let mut rows = Vec::with_capacity(row_count);

    for i in 0..row_count {
        let offset = header_size + i * record_size;
        // Safety: offset is within mmap bounds (checked via records_end above).
        let rec_ptr = unsafe { mmap.as_ptr().add(offset) };
        // Safety: rec_ptr points to at least record_size valid bytes.
        let rec: ShmemRecord = unsafe { core::ptr::read_unaligned(rec_ptr.cast::<ShmemRecord>()) };

        let path_start = rec.path_off as usize;
        let path_end = path_start + rec.path_len as usize;
        let name_start = rec.name_off as usize;
        let name_end = name_start + rec.name_len as usize;

        if path_end > string_table.len() || name_end > string_table.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("string offset out of bounds at row {i}"),
            ));
        }

        let path_str = core::str::from_utf8(&string_table[path_start..path_end])
            .map_err(|utf8_err| io::Error::new(io::ErrorKind::InvalidData, utf8_err))?;
        let name_str = core::str::from_utf8(&string_table[name_start..name_end])
            .map_err(|utf8_err| io::Error::new(io::ErrorKind::InvalidData, utf8_err))?;

        rows.push(SearchRow {
            drive: char::from(rec.drive),
            path: path_str.to_owned(),
            name: name_str.to_owned(),
            size: rec.size,
            is_directory: rec.is_directory != 0,
            modified: rec.modified,
            created: rec.created,
            accessed: rec.accessed,
            flags: rec.flags,
            allocated: rec.allocated,
            descendants: rec.descendants,
            treesize: rec.treesize,
        });
    }

    // Unmap before deleting.
    drop(mmap);
    drop(file);

    // Best-effort cleanup — don't fail the read if delete fails.
    drop(std::fs::remove_file(path));

    Ok(SearchResponse {
        rows,
        records_scanned: header.records_scanned as usize,
        duration_ms: header.duration_ms,
        truncated: header.truncated != 0,
        shmem_path: None,
        shmem_count: None,
    })
}

/// Remove any leftover shmem files (GC).
///
/// Called on daemon startup to clean stale files from previous sessions.
pub fn cleanup_stale_shmem_files() {
    if let Ok(dir) = shmem_dir() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "bin") {
                    drop(std::fs::remove_file(&path));
                }
            }
        }
    }
}
