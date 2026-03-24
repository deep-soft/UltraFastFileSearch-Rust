//! On-demand full record lookup from `.uffs` cache files.
//!
//! The compact index holds 72 bytes per record covering all common columns.
//! For rare fields (reparse tag, forensic data, `$FILE_NAME` timestamps),
//! this module reads individual records directly from the `.uffs` cache
//! file without loading the entire `MftIndex`.
//!
//! See `docs/architecture/COMPACT_INDEX_DESIGN.md` Phase 3c.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Extra fields from a full `FileRecord` not present in `CompactRecord`.
///
/// These are the columns visible in "max view" that require on-demand
/// lookup from the `.uffs` cache file.
#[derive(Debug, Clone, Default)]
#[expect(dead_code, reason = "infrastructure for Phase 3c; wired into UI in future wave")]
pub struct ExtraRecordFields {
    /// Reparse tag from `$REPARSE_POINT` (0 if not a reparse point).
    pub reparse_tag: u32,
    /// Sequence number (incremented when FRS is reused).
    pub sequence_number: u16,
    /// Primary filename namespace (0=POSIX, 1=Win32, 2=DOS, 3=Win32+DOS).
    pub namespace: u8,
    /// Forensic flags (bit-packed: deleted, corrupt, extension, etc.).
    pub forensic_flags: u8,
    /// Log File Sequence Number.
    pub lsn: u64,
    /// Base FRS for extension records.
    pub base_frs: u64,
    /// `$STANDARD_INFORMATION` USN (Update Sequence Number).
    pub stdinfo_usn: u64,
    /// `$STANDARD_INFORMATION` security ID.
    pub security_id: u32,
    /// `$STANDARD_INFORMATION` owner ID.
    pub owner_id: u32,
    /// `$FILE_NAME` creation time (Unix microseconds).
    pub fn_created: i64,
    /// `$FILE_NAME` modification time (Unix microseconds).
    pub fn_modified: i64,
    /// `$FILE_NAME` access time (Unix microseconds).
    pub fn_accessed: i64,
    /// `$FILE_NAME` MFT change time (Unix microseconds).
    pub fn_mft_changed: i64,
}

/// Reader for extracting individual records from a `.uffs` cache file.
///
/// Opens the file once, reads the header to determine record layout,
/// then supports seeking to any record by index.
pub struct FullRecordReader {
    /// Path to the `.uffs` cache file.
    path: PathBuf,
    /// Format version (determines record byte size).
    version: u32,
    /// Byte offset where records data starts in the file.
    records_offset: u64,
    /// Byte size of each serialized record.
    record_byte_size: u64,
    /// LRU cache: `compact_idx` → extra fields.
    cache: HashMap<u32, ExtraRecordFields>,
    /// Maximum cache entries before eviction.
    cache_capacity: usize,
}

/// `.uffs` header size: 12 fields, all little-endian.
const HEADER_SIZE: u64 = 96;

#[expect(dead_code, reason = "infrastructure for Phase 3c; wired into UI in future wave")]
impl FullRecordReader {
    /// Open a `.uffs` cache file and read its header.
    ///
    /// Returns `None` if the file doesn't exist or the header is invalid.
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "HEADER_SIZE is 96, always fits usize"
    )]
    #[expect(
        clippy::single_call_fn,
        reason = "public API called from open_for_drive; separation keeps file parsing isolated"
    )]
    pub fn open(path: &Path) -> Option<Self> {
        let mut file = std::fs::File::open(path).ok()?;
        let mut header_buf = [0_u8; HEADER_SIZE as usize];
        file.read_exact(&mut header_buf).ok()?;

        // Parse header fields we need
        let magic = &header_buf[0..8];
        if magic != b"UFFSIDX\0" {
            return None;
        }

        let version = u32::from_le_bytes(header_buf[8..12].try_into().ok()?);
        // Read frs_to_idx_len to calculate records offset
        let mut frs_len_buf = [0_u8; 8];
        file.read_exact(&mut frs_len_buf).ok()?;
        let frs_to_idx_len = u64::from_le_bytes(frs_len_buf);

        // records start after: header (96) + frs_to_idx_len (8) + frs_to_idx data
        let records_offset = HEADER_SIZE + 8 + frs_to_idx_len * 4;

        let record_byte_size = match version {
            3 => 121,
            4 => 157,
            5 => 181,
            6 => 185,
            7 => 193,
            8 => 195,
            _ => return None, // Unsupported version
        };

        Some(Self {
            path: path.to_path_buf(),
            version,
            records_offset,
            record_byte_size,
            cache: HashMap::new(),
            cache_capacity: 512,
        })
    }

    /// Try to open a `.uffs` cache file for a drive letter.
    ///
    /// Uses the standard cache path: `{TEMP}/uffs_index_cache/{DRIVE}_index.uffs`.
    #[must_use]
    pub fn open_for_drive(drive_letter: char) -> Option<Self> {
        let path = uffs_mft::cache::cache_file_path(drive_letter);
        if path.exists() {
            Self::open(&path)
        } else {
            None
        }
    }

    /// Read extra fields for a specific record index.
    ///
    /// Returns cached data if available, otherwise reads from disk.
    /// Returns `None` if the record can't be read.
    pub fn get_extra_fields(&mut self, record_idx: u32) -> Option<ExtraRecordFields> {
        // Check cache first
        if let Some(cached) = self.cache.get(&record_idx) {
            return Some(cached.clone());
        }

        // Read from disk
        let fields = self.read_record_from_disk(record_idx)?;

        // Cache the result (simple eviction: clear all when full)
        if self.cache.len() >= self.cache_capacity {
            self.cache.clear();
        }
        self.cache.insert(record_idx, fields.clone());

        Some(fields)
    }

    /// Read a single record from the `.uffs` file and extract extra fields.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "record_idx bounded by NTFS limits, offset arithmetic safe"
    )]
    fn read_record_from_disk(&self, record_idx: u32) -> Option<ExtraRecordFields> {
        let mut file = std::fs::File::open(&self.path).ok()?;

        let offset = self.records_offset + u64::from(record_idx) * self.record_byte_size;
        file.seek(SeekFrom::Start(offset)).ok()?;

        let mut buf = vec![0_u8; self.record_byte_size as usize];
        file.read_exact(&mut buf).ok()?;

        self.parse_extra_fields(&buf)
    }

    /// Parse the extra fields from a raw record buffer.
    ///
    /// Reads only the fields NOT in `CompactRecord`. Skips fields that
    /// are already in the compact record (size, allocated, timestamps, etc.).
    #[expect(
        clippy::cognitive_complexity,
        reason = "version-conditional field reads mirror deserialize.rs; splitting would fragment format logic"
    )]
    fn parse_extra_fields(&self, buf: &[u8]) -> Option<ExtraRecordFields> {
        let mut pos = 0;

        macro_rules! read_u8 {
            () => {{
                let val = *buf.get(pos)?;
                pos += 1;
                val
            }};
        }
        macro_rules! read_u16 {
            () => {{
                let bytes: [u8; 2] = buf.get(pos..pos + 2)?.try_into().ok()?;
                pos += 2;
                u16::from_le_bytes(bytes)
            }};
        }
        macro_rules! read_u32 {
            () => {{
                let bytes: [u8; 4] = buf.get(pos..pos + 4)?.try_into().ok()?;
                pos += 4;
                u32::from_le_bytes(bytes)
            }};
        }
        macro_rules! read_u64 {
            () => {{
                let bytes: [u8; 8] = buf.get(pos..pos + 8)?.try_into().ok()?;
                pos += 8;
                u64::from_le_bytes(bytes)
            }};
        }
        macro_rules! read_i64 {
            () => {{
                let bytes: [u8; 8] = buf.get(pos..pos + 8)?.try_into().ok()?;
                pos += 8;
                i64::from_le_bytes(bytes)
            }};
        }
        macro_rules! skip {
            ($n:expr) => {
                pos += $n;
            };
        }

        // Parse v8 record layout (matching deserialize.rs exactly):
        // frs: u64
        skip!(8);
        // sequence_number: u16 (v4+)
        let sequence_number = if self.version >= 4 { read_u16!() } else { 0 };
        // namespace: u8 (v4+)
        let namespace = if self.version >= 4 { read_u8!() } else { 1 };
        // forensic_flags: u8 (v4+)
        let forensic_flags = if self.version >= 4 { read_u8!() } else { 0 };
        // lsn: u64 (v5+)
        let lsn = if self.version >= 5 { read_u64!() } else { skip!(0); 0 };
        // reparse_tag: u32 (v6+)
        let reparse_tag = if self.version >= 6 { read_u32!() } else { 0 };
        // base_frs: u64 (v7+)
        let base_frs = if self.version >= 7 { read_u64!() } else { 0 };
        // StandardInfo: created, modified, accessed, mft_changed (4×i64), flags (u32)
        skip!(4 * 8 + 4); // skip timestamps + flags (already in compact)
        // usn: u64 (v5+)
        let stdinfo_usn = if self.version >= 5 { read_u64!() } else { 0 };
        // security_id: u32 (v5+)
        let security_id = if self.version >= 5 { read_u32!() } else { 0 };
        // owner_id: u32 (v5+)
        let owner_id = if self.version >= 5 { read_u32!() } else { 0 };
        // name_count: u16, stream_count: u16
        skip!(4);
        // total_stream_count: u16 (v8+)
        if self.version >= 8 {
            skip!(2);
        }
        // first_child: u32
        skip!(4);
        // first_name (LinkInfo): next_entry(4) + name.offset(4) + name.meta(4) + parent_frs(8)
        skip!(20);
        // first_stream: size.length(8) + size.allocated(8) + next_entry(4) + name.offset(4) + name.meta(4) + flags(1)
        skip!(29);
        // tree metrics (v3+): descendants(4) + treesize(8) + tree_allocated(8)
        if self.version >= 3 {
            skip!(20);
        }
        // $FILE_NAME timestamps (v4+)
        let fn_created = if self.version >= 4 { read_i64!() } else { 0 };
        let fn_modified = if self.version >= 4 { read_i64!() } else { 0 };
        let fn_accessed = if self.version >= 4 { read_i64!() } else { 0 };
        let fn_mft_changed = if self.version >= 4 { read_i64!() } else { 0 };
        let _: usize = pos; // suppress unused_assignments on last read

        Some(ExtraRecordFields {
            reparse_tag,
            sequence_number,
            namespace,
            forensic_flags,
            lsn,
            base_frs,
            stdinfo_usn,
            security_id,
            owner_id,
            fn_created,
            fn_modified,
            fn_accessed,
            fn_mft_changed,
        })
    }
}
