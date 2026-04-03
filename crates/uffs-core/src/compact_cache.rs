//! Compact index cache: serialize/deserialize + encrypted disk I/O.
//!
//! Stores `DriveCompactIndex` as zstd-compressed, AES-256-GCM encrypted
//! `{DRIVE}_compact.uffs` alongside the full `.uffs` `MftIndex` cache.
//!
//! **v6** (current): char-trigram CSR stored on disk (keys `u64[]`, offsets
//! `u32[]`, values `u32[]`).  Zero rebuild on load — saves ~220 ms.
//!
//! **v5**: `names_lower` removed from disk — trigram rebuilt from on-the-fly
//! `CaseFold` lowered names on load.  Still accepted; trigram rebuilt.
//!
//! **v4**: trigram index not stored on disk — rebuilt from `names_lower` on
//! load.  Still accepted on load; `names_lower` is read then dropped.
//!
//! **v3**: adds `source_epoch` (u64) to the header.  Still accepted on load;
//! old byte-trigram CSR is skipped, char-trigram rebuilt.
//!
//! **v2**: old byte-trigram posting lists serialized in CSR format.
//! Accepted on load; `source_epoch` defaults to 0 (always stale).
//!
//! **v1** (legacy): rejected — returns error, caller rebuilds.

use std::path::PathBuf;
use std::time::Instant;

use crate::compact::{ChildrenIndex, CompactRecord, DriveCompactIndex, IndexSource};
use crate::trigram::TrigramIndex;

/// Magic bytes for compact cache files.
const COMPACT_MAGIC: &[u8; 8] = b"UFFSCOM\0";
/// Current compact cache format version (v6 stores char-trigram CSR on disk).
const COMPACT_VERSION: u16 = 6;
/// Bytes per `CompactRecord`.
const RECORD_BYTES: usize = size_of::<CompactRecord>();
/// zstd compression level for compact cache.
const ZSTD_LEVEL: i32 = 3;
/// zstd frame magic bytes (little-endian `0xFD2FB528`).
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Returns the cache file path for a compact index.
#[must_use]
pub fn compact_cache_path(drive_letter: char) -> PathBuf {
    uffs_mft::cache::cache_dir().join(format!("{drive_letter}_compact.uffs"))
}

/// Serializes the compact index (records, names, children, char-trigram CSR).
///
/// **v6**: char-trigram CSR is stored on disk — zero-rebuild on load.
/// Format after children CSR:
///   - `trigram_key_count: u32`
///   - `trigram_keys: u64[key_count]`
///   - `trigram_offsets: u32[key_count + 1]`
///   - `trigram_values_count: u32`
///   - `trigram_values: u32[values_count]`
#[must_use]
pub fn serialize_compact(index: &DriveCompactIndex) -> Vec<u8> {
    let record_count = index.records.len();
    let names_len = index.names.len();

    // Children CSR — already in contiguous layout.
    let (csr_offsets, csr_values) = index.children.as_csr();

    // Trigram CSR.
    let (tri_keys, tri_offsets, tri_values) = index.trigram.as_csr();

    let total = 26 // header: 8 (magic) + 2 (ver) + 4 (rc) + 4 (nl) + 8 (epoch)
        + record_count * RECORD_BYTES
        + names_len
        + csr_offsets.len() * 4
        + csr_values.len() * 4
        + 4                         // trigram_key_count
        + tri_keys.len() * 8        // trigram_keys (u64)
        + tri_offsets.len() * 4     // trigram_offsets (u32)
        + 4                         // trigram_values_count
        + tri_values.len() * 4; // trigram_values (u32)
    let mut buf = Vec::with_capacity(total);

    // Header (26 bytes for v3+)
    buf.extend_from_slice(COMPACT_MAGIC);
    buf.extend_from_slice(&COMPACT_VERSION.to_le_bytes());
    push_u32(&mut buf, record_count);
    push_u32(&mut buf, names_len);
    // v3+: source_epoch
    buf.extend_from_slice(&index.source_epoch.to_le_bytes());

    // Records — single bulk copy via bytemuck (Pod layout = on-disk layout)
    buf.extend_from_slice(bytemuck::cast_slice(&index.records));

    // Names (original case only)
    buf.extend_from_slice(&index.names);

    // Children CSR — bulk cast (u32 slices → &[u8] via bytemuck, zero-copy on LE)
    buf.extend_from_slice(bytemuck::cast_slice(csr_offsets));
    buf.extend_from_slice(bytemuck::cast_slice(csr_values));

    // v6: char-trigram CSR
    push_u32(&mut buf, tri_keys.len());
    buf.extend_from_slice(bytemuck::cast_slice(tri_keys));
    buf.extend_from_slice(bytemuck::cast_slice(tri_offsets));
    push_u32(&mut buf, tri_values.len());
    buf.extend_from_slice(bytemuck::cast_slice(tri_values));

    buf
}

/// Deserializes a compact index from raw bytes.
///
/// **v6**: char-trigram CSR on disk — zero-rebuild.
/// **v5**: no trigram on disk — rebuilt with `CaseFold`.
/// **v4**: `names_lower` on disk, trigram rebuilt.
/// **v3/v2**: legacy byte-trigram / old format — trigram rebuilt.
/// **v1**: rejected — returns an error so the caller rebuilds.
///
/// Returns `(DriveCompactIndex, trigram_load_ms)`.
///
/// # Errors
/// Returns an error string if the data is truncated, wrong magic, or v1.
pub fn deserialize_compact(
    data: &[u8],
    drive_letter: char,
) -> Result<(DriveCompactIndex, u128), &'static str> {
    let (source_epoch, body_offset, version) = parse_compact_header(data)?;

    let record_count = read_u32(data, 10) as usize;
    let names_len = read_u32(data, 14) as usize;
    let records_end = body_offset + record_count * RECORD_BYTES;
    let names_end = records_end + names_len;

    // v4 and earlier stored names_lower (same size as names) on disk.
    // v5+ omits it entirely.
    let csr_start = if version >= 5 {
        names_end
    } else {
        names_end + names_len // skip names_lower
    };
    let csr_offsets_end = csr_start + (record_count + 1) * 4;
    if data.len() < csr_offsets_end {
        return Err("compact cache truncated");
    }

    // Records — alignment-safe copy into properly aligned Vec<CompactRecord>
    let records: Vec<CompactRecord> = aligned_vec_from_bytes(
        data.get(body_offset..records_end)
            .ok_or("truncated records")?,
    );
    let names = data
        .get(records_end..names_end)
        .ok_or("truncated names")?
        .to_vec();

    // Children CSR — alignment-safe copy into aligned Vec<u32>
    let offsets_slice = data
        .get(csr_start..csr_offsets_end)
        .ok_or("truncated CSR")?;
    let total_child_postings = read_u32(offsets_slice, record_count * 4);
    let postings_end = csr_offsets_end + total_child_postings as usize * 4;
    if data.len() < postings_end {
        return Err("truncated CSR postings");
    }
    let child_vals_slice = data.get(csr_offsets_end..postings_end).ok_or("CSR OOB")?;
    let children = ChildrenIndex::from_csr(
        aligned_vec_from_bytes(offsets_slice),
        aligned_vec_from_bytes(child_vals_slice),
    );

    let fold = crate::compact::resolve_case_fold(drive_letter);
    let tri_start = Instant::now();

    // ─── Trigram ──────────────────────────────────────────────────
    let tri_hdr = postings_end;
    if data.len() < tri_hdr + 4 {
        return Err("truncated trigram header");
    }
    let trigram_key_count = read_u32(data, tri_hdr) as usize;

    let trigram = if version >= 6 && trigram_key_count > 0 {
        // v6: char-trigram CSR on disk — zero-rebuild bulk memcpy.
        let tri_keys_start = tri_hdr + 4;
        let tri_keys_end = tri_keys_start + trigram_key_count * 8;
        let tri_offsets_end = tri_keys_end + (trigram_key_count + 1) * 4;
        if data.len() < tri_offsets_end + 4 {
            return Err("truncated trigram CSR (keys/offsets)");
        }
        let tri_keys: Vec<u64> = aligned_vec_from_bytes(
            data.get(tri_keys_start..tri_keys_end)
                .ok_or("truncated trigram keys")?,
        );
        let tri_offsets: Vec<u32> = aligned_vec_from_bytes(
            data.get(tri_keys_end..tri_offsets_end)
                .ok_or("truncated trigram offsets")?,
        );
        let tri_values_count = read_u32(data, tri_offsets_end) as usize;
        let tri_values_start = tri_offsets_end + 4;
        let tri_values_end = tri_values_start + tri_values_count * 4;
        if data.len() < tri_values_end {
            return Err("truncated trigram CSR (values)");
        }
        let tri_values: Vec<u32> = aligned_vec_from_bytes(
            data.get(tri_values_start..tri_values_end)
                .ok_or("truncated trigram values")?,
        );
        TrigramIndex::from_csr(tri_keys, tri_offsets, tri_values)
    } else {
        // v5 and earlier: rebuild char-trigrams from names + CaseFold.
        TrigramIndex::build(&records, &names, fold)
    };

    let tri_ms = tri_start.elapsed().as_millis();

    Ok((
        DriveCompactIndex {
            letter: drive_letter,
            records,
            names,
            trigram,
            children,
            fold,
            source: IndexSource::MftFile(PathBuf::from(format!("{drive_letter}:"))),
            source_epoch,
        },
        tri_ms,
    ))
}

/// Validates magic/version and returns `(source_epoch, body_offset, version)`.
fn parse_compact_header(data: &[u8]) -> Result<(u64, usize, u16), &'static str> {
    if data.len() < 18 {
        return Err("compact cache too short");
    }
    if data.get(..8) != Some(COMPACT_MAGIC.as_slice()) {
        return Err("bad compact magic");
    }
    let version = data
        .get(8..10)
        .and_then(|slice| <[u8; 2]>::try_from(slice).ok())
        .map_or(0, u16::from_le_bytes);
    if version < 2 {
        return Err("stale compact version (v1 → rebuild)");
    }
    if version > COMPACT_VERSION {
        return Err("unsupported compact version (future)");
    }
    if version >= 3 {
        if data.len() < 26 {
            return Err("compact cache truncated (v3 header)");
        }
        let epoch = data
            .get(18..26)
            .and_then(|slice| <[u8; 8]>::try_from(slice).ok())
            .map_or(0, u64::from_le_bytes);
        Ok((epoch, 26, version))
    } else {
        Ok((0, 18, version))
    }
}

// ─── Save / Load ────────────────────────────────────────────────────────────

/// Saves a compact index to its cache file (zstd + AES-256-GCM), blocking.
///
/// Prefer [`save_compact_cache_background`] for non-blocking saves.
///
/// # Errors
/// Returns an error if compression, encryption, or file writing fails.
pub fn save_compact_cache(index: &DriveCompactIndex) -> std::io::Result<()> {
    let profile = std::env::var_os("UFFS_CACHE_PROFILE").is_some();
    let t_total = Instant::now();
    let t_ser = Instant::now();
    let serialized = serialize_compact(index);
    let ser_ms = t_ser.elapsed().as_millis();
    let uncompressed_len = serialized.len();
    let path = compact_cache_path(index.letter);
    if let Some(dir) = path.parent() {
        uffs_mft::cache::create_secure_dir(dir)?;
    }
    uffs_mft::cache::compress_encrypt_write(serialized, &path, ZSTD_LEVEL, profile, "compact")?;
    if profile {
        let uncomp_mb = uncompressed_len / (1024 * 1024);
        tracing::debug!(
            target: "cache_profile",
            ser_ms = %ser_ms,
            uncomp_mb,
            total_ms = %t_total.elapsed().as_millis(),
            "compact_save"
        );
    }
    Ok(())
}

/// Serializes the compact index synchronously and spawns a background thread
/// to compress, encrypt, and write the cache file.
///
/// Serialization (~100-250ms) runs on the calling thread; the heavy
/// compress + encrypt + write (~3-5s) runs in a detached background thread.
/// Uses [`atomic_write`](uffs_mft::cache::atomic_write), so partial writes
/// from process exit are safe.
///
/// # Errors
/// Returns an error only if serialization or directory creation fails.
pub fn save_compact_cache_background(index: &DriveCompactIndex) -> std::io::Result<()> {
    let profile = std::env::var_os("UFFS_CACHE_PROFILE").is_some();
    let t_ser = Instant::now();
    let serialized = serialize_compact(index);
    let ser_ms = t_ser.elapsed().as_millis();
    if profile {
        let mb = serialized.len() / (1024 * 1024);
        tracing::debug!(target: "cache_profile", ser_ms = %ser_ms, mb, "compact_ser");
    }
    let path = compact_cache_path(index.letter);
    if let Some(dir) = path.parent() {
        uffs_mft::cache::create_secure_dir(dir)?;
    }
    let drive = index.letter;
    std::thread::Builder::new()
        .name(format!("compact-save-{drive}"))
        .spawn(move || {
            if let Err(err) = uffs_mft::cache::compress_encrypt_write(
                serialized, &path, ZSTD_LEVEL, profile, "compact",
            ) {
                tracing::warn!(
                    drive = %drive,
                    error = %err,
                    "Background compact cache save failed"
                );
            }
        })
        .map_err(|err| std::io::Error::other(format!("spawn failed: {err}")))?;
    Ok(())
}

/// Loads a compact index from its cache file if fresh. Returns `None` if
/// cache is missing, stale, corrupt, or built from an older `MftIndex`.
///
/// `mft_build_epoch` is the `build_epoch` of the current `MftIndex`.
/// If the compact cache was built from an older epoch it is considered stale
/// and `None` is returned so the caller rebuilds.
///
/// When `trust_ttl_only` is `true`, the mtime comparison against the
/// `MftIndex` `.uffs` file is skipped — only the TTL age check is used.
/// This is useful for hot-path searches where the caller knows the compact
/// cache was just built or the `MftIndex` hasn't changed.
#[must_use]
pub fn load_compact_cache(
    drive_letter: char,
    ttl_seconds: u64,
    mft_build_epoch: u64,
    trust_ttl_only: bool,
) -> Option<DriveCompactIndex> {
    let path = compact_cache_path(drive_letter);
    let meta = std::fs::metadata(&path).ok()?;
    let compact_mtime = meta.modified().ok()?;
    let age = compact_mtime.elapsed().ok()?.as_secs();
    if age > ttl_seconds {
        return None;
    }

    // Mtime-based staleness: if the MftIndex `.uffs` file is newer than the
    // compact cache, the compact was built from an older MftIndex.
    // This catches cross-process updates (daemon updates MftIndex, TUI has
    // stale compact) with zero I/O — just two stat() calls.
    // Skipped when `trust_ttl_only` — caller trusts the TTL is sufficient.
    if !trust_ttl_only {
        let mft_path = uffs_mft::cache::cache_file_path(drive_letter);
        if let Ok(mft_meta) = std::fs::metadata(&mft_path) {
            if let Ok(mft_mtime) = mft_meta.modified() {
                if mft_mtime > compact_mtime {
                    tracing::debug!(
                        drive = %drive_letter,
                        "Compact cache older than MftIndex cache — rebuilding"
                    );
                    return None;
                }
            }
        }
    }

    let profile = std::env::var_os("UFFS_CACHE_PROFILE").is_some();
    let t_total = Instant::now();

    let t_read = Instant::now();
    let raw = std::fs::read(&path).ok()?;
    let read_ms = t_read.elapsed().as_millis();
    let raw_len = raw.len();

    let key = uffs_security::keystore::get_cache_key().ok()?;
    let t_decrypt = Instant::now();
    let decrypted = uffs_security::crypto::decrypt_cache(&raw, &key).ok()?;
    let decrypt_ms = t_decrypt.elapsed().as_millis();

    let t_decompress = Instant::now();
    let is_compressed = decrypted.get(..4).is_some_and(|magic| magic == ZSTD_MAGIC);
    let plaintext = if is_compressed {
        zstd::decode_all(decrypted.as_slice()).ok()?
    } else {
        decrypted
    };
    let decompress_ms = t_decompress.elapsed().as_millis();
    let plaintext_len = plaintext.len();

    // Early staleness check — inspect header before full deserialization.
    if mft_build_epoch > 0 {
        if let Ok((source_epoch, _, _)) = parse_compact_header(&plaintext) {
            if source_epoch < mft_build_epoch {
                tracing::debug!(
                    target: "cache_profile",
                    source_epoch,
                    mft_build_epoch,
                    "compact: STALE"
                );
                tracing::debug!(
                    drive = %drive_letter,
                    compact_epoch = source_epoch,
                    mft_epoch = mft_build_epoch,
                    "Compact cache stale (source_epoch < mft build_epoch) — rebuilding"
                );
                return None;
            }
        }
    }

    let t_deser = Instant::now();
    let (index, tri_ms) = deserialize_compact(&plaintext, drive_letter).ok()?;
    let deser_ms = t_deser.elapsed().as_millis();

    if profile {
        let raw_mb = raw_len / (1024 * 1024);
        let plain_mb = plaintext_len / (1024 * 1024);
        let tri_label = if tri_ms > 100 {
            "tri_rebuild"
        } else {
            "tri_load"
        };
        tracing::debug!(
            target: "cache_profile",
            read_ms = %read_ms,
            raw_mb,
            decrypt_ms = %decrypt_ms,
            is_compressed,
            decompress_ms = %decompress_ms,
            plain_mb,
            deser_ms = %deser_ms,
            records = index.records.len(),
            tri_label,
            tri_ms = %tri_ms,
            total_ms = %t_total.elapsed().as_millis(),
            source_epoch = index.source_epoch,
            "compact_load"
        );
    }
    Some(index)
}

// ─── Build-or-load + save ────────────────────────────────────────────────────

/// Ensures the compact cache is up-to-date for a given drive.
///
/// - If a fresh compact cache exists on disk → loads and returns it.
/// - Otherwise → builds from the given `MftIndex` → saves → returns.
///
/// Emits `cache_profile` tracing events at `debug` level.
/// The caller may discard the returned index if only the `MftIndex` is needed.
pub fn ensure_compact_cached(
    drive_letter: char,
    mft_index: &uffs_mft::MftIndex,
) -> DriveCompactIndex {
    // Try loading existing compact cache (epoch check catches stale caches).
    // Not TTL-only: we have the MftIndex, so mtime validation is cheap & correct.
    if let Some(cached) = load_compact_cache(
        drive_letter,
        super::compact::INDEX_TTL_SECONDS,
        mft_index.build_epoch,
        false,
    ) {
        tracing::debug!(
            target: "cache_profile",
            records = cached.records.len(),
            "compact: loaded from cache"
        );
        return cached;
    }

    // Build from MftIndex
    let t_build = Instant::now();
    let (compact, build_ms, tri_ms) = crate::compact::build_compact_index(drive_letter, mft_index);
    let total_build_ms = t_build.elapsed().as_millis();

    tracing::debug!(
        target: "cache_profile",
        build_ms = %build_ms,
        records = compact.records.len(),
        tri_ms = %tri_ms,
        total_ms = %total_build_ms,
        "compact_build"
    );

    // Save to disk (best-effort)
    if let Err(err) = save_compact_cache(&compact) {
        tracing::warn!(drive = %drive_letter, error = %err, "Failed to save compact cache");
    } else {
        // Report on-disk size of both caches
        let compact_path = compact_cache_path(drive_letter);
        let mft_path = uffs_mft::cache::cache_file_path(drive_letter);
        let compact_disk = std::fs::metadata(&compact_path).map_or(0, |meta| meta.len());
        let mft_disk = std::fs::metadata(mft_path).map_or(0, |meta| meta.len());
        let compact_disk_mb = compact_disk / (1024 * 1024);
        let mft_disk_mb = mft_disk / (1024 * 1024);
        let total_disk_mb = compact_disk_mb + mft_disk_mb;
        tracing::debug!(
            target: "cache_profile",
            mft_disk_mb,
            compact_disk_mb,
            total_disk_mb,
            "disk_summary"
        );
    }

    compact
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Writes a usize as u32 LE (callers ensure it fits).
#[expect(
    clippy::cast_possible_truncation,
    reason = "callers ensure value fits u32"
)]
fn push_u32(buf: &mut Vec<u8>, value: usize) {
    buf.extend_from_slice(&(value as u32).to_le_bytes());
}

/// Read a little-endian u32 from `data` at `offset`.
fn read_u32(data: &[u8], offset: usize) -> u32 {
    data.get(offset..offset + 4)
        .and_then(|slice| <[u8; 4]>::try_from(slice).ok())
        .map_or(0, u32::from_le_bytes)
}

/// Alignment-safe bulk copy from a `&[u8]` slice into a properly aligned
/// `Vec<T>`.
///
/// Unlike `bytemuck::cast_slice`, this works regardless of the source
/// pointer's alignment. It allocates a `Vec<T>` (which the allocator
/// guarantees to be `align_of::<T>()`-aligned), then copies the raw bytes
/// in via `copy_from_slice`.
///
/// # Panics
///
/// Panics if `bytes.len()` is not an exact multiple of `size_of::<T>()`.
fn aligned_vec_from_bytes<T: bytemuck::Pod>(bytes: &[u8]) -> Vec<T> {
    let elem_size = size_of::<T>();
    assert!(
        elem_size > 0 && bytes.len() % elem_size == 0,
        "byte slice length {} is not a multiple of element size {}",
        bytes.len(),
        elem_size,
    );
    let count = bytes.len() / elem_size;
    let mut vec = vec![T::zeroed(); count];
    // The Vec<T> is guaranteed aligned by the allocator. Copy raw bytes in.
    let dst = bytemuck::cast_slice_mut::<T, u8>(&mut vec);
    dst.copy_from_slice(bytes);
    vec
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal `DriveCompactIndex` with 3 records for testing.
    fn make_test_index() -> DriveCompactIndex {
        let names = b"foobarbaz".to_vec(); // "foo" [0..3], "bar" [3..6], "baz" [6..9]
        let records = vec![
            CompactRecord {
                name_offset: 0,
                name_len: 3,
                parent_idx: u32::MAX,
                flags: 0x0010, // directory
                ..CompactRecord::default()
            },
            CompactRecord {
                name_offset: 3,
                name_len: 3,
                parent_idx: 0,
                ..CompactRecord::default()
            },
            CompactRecord {
                name_offset: 6,
                name_len: 3,
                parent_idx: 0,
                ..CompactRecord::default()
            },
        ];
        let fold = uffs_text::CaseFold::default_table();
        let trigram = TrigramIndex::build(&records, &names, fold);
        let children = ChildrenIndex::build(&records);
        DriveCompactIndex {
            letter: 'T',
            records,
            names,
            trigram,
            children,
            fold,
            source: IndexSource::MftFile(PathBuf::from("T:")),
            source_epoch: 42,
        }
    }

    #[test]
    fn v6_round_trip_preserves_trigram() {
        let index = make_test_index();
        let (tri_keys, tri_offsets, tri_values) = index.trigram.as_csr();
        let original_key_count = tri_keys.len();
        assert!(original_key_count > 0, "test index should have trigrams");

        let serialized = serialize_compact(&index);
        let (loaded, tri_ms) = deserialize_compact(&serialized, 'T').unwrap();

        // Trigram loaded from disk — should be fast (< 10ms on any hardware).
        assert!(
            tri_ms < 500,
            "trigram took {tri_ms}ms — should be near-zero for cached CSR"
        );

        // Verify trigram CSR is identical.
        let (loaded_keys, loaded_offsets, loaded_values) = loaded.trigram.as_csr();
        assert_eq!(loaded_keys, tri_keys, "trigram keys mismatch");
        assert_eq!(loaded_offsets, tri_offsets, "trigram offsets mismatch");
        assert_eq!(loaded_values, tri_values, "trigram values mismatch");

        // Verify other fields survived.
        assert_eq!(loaded.letter, 'T');
        assert_eq!(loaded.records.len(), 3);
        assert_eq!(loaded.names, b"foobarbaz");
        assert_eq!(loaded.source_epoch, 42);
    }

    #[test]
    fn v5_backward_compat_rebuilds_trigram() {
        // Serialize a v6 index, then patch the version to v5 and replace
        // the trigram section with the v5 sentinel (trigram_count = 0).
        let index = make_test_index();
        let mut serialized = serialize_compact(&index);

        // Patch version to 5.
        serialized
            .get_mut(8..10)
            .expect("buffer too short for version")
            .copy_from_slice(&5_u16.to_le_bytes());

        // Find the trigram section: after children CSR.
        // Children CSR starts after names, offsets are (records+1)*4, then values.
        let record_count = index.records.len();
        let names_len = index.names.len();
        let records_end = 26 + record_count * RECORD_BYTES;
        let names_end = records_end + names_len;
        let csr_offsets_end = names_end + (record_count + 1) * 4;
        let total_children = index.children.total_children();
        let postings_end = csr_offsets_end + total_children * 4;

        // Truncate at postings_end + 4 (v5 sentinel: trigram_count = 0).
        serialized.truncate(postings_end + 4);
        serialized
            .get_mut(postings_end..postings_end + 4)
            .expect("buffer too short for trigram sentinel")
            .copy_from_slice(&0_u32.to_le_bytes());

        let (loaded, _tri_ms) = deserialize_compact(&serialized, 'T').unwrap();

        // Trigram was rebuilt — should match the original.
        let (orig_keys, orig_offsets, orig_values) = index.trigram.as_csr();
        let (loaded_keys, loaded_offsets, loaded_values) = loaded.trigram.as_csr();
        assert_eq!(loaded_keys, orig_keys, "rebuilt trigram keys mismatch");
        assert_eq!(
            loaded_offsets, orig_offsets,
            "rebuilt trigram offsets mismatch"
        );
        assert_eq!(
            loaded_values, orig_values,
            "rebuilt trigram values mismatch"
        );
    }

    #[test]
    fn v6_header_version() {
        let index = make_test_index();
        let serialized = serialize_compact(&index);
        let b8 = *serialized.get(8).expect("missing byte 8");
        let b9 = *serialized.get(9).expect("missing byte 9");
        let version = u16::from_le_bytes([b8, b9]);
        assert_eq!(version, 6);
    }

    #[test]
    fn v1_rejected() {
        let mut data = vec![0_u8; 64];
        data.get_mut(..8)
            .expect("buffer too short for magic")
            .copy_from_slice(COMPACT_MAGIC);
        data.get_mut(8..10)
            .expect("buffer too short for version")
            .copy_from_slice(&1_u16.to_le_bytes());
        assert!(deserialize_compact(&data, 'X').is_err());
    }

    #[test]
    fn truncated_data_rejected() {
        assert!(deserialize_compact(b"short", 'X').is_err());
    }
}
