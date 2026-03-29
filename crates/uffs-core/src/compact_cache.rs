//! Compact index cache: serialize/deserialize + encrypted disk I/O.
//!
//! Stores `DriveCompactIndex` as zstd-compressed, AES-256-GCM encrypted
//! `{DRIVE}_compact.uffs` alongside the full `.uffs` `MftIndex` cache.
//!
//! **v3** (current): adds `source_epoch` (u64) to the header — the
//! `MftIndex.build_epoch` this compact index was built from.  On load the
//! epoch is compared against the current `MftIndex`; if stale → rebuild.
//!
//! **v2**: trigram posting lists serialized in CSR format (zero rebuild).
//! Accepted on load; `source_epoch` defaults to 0 (always stale).
//!
//! **v1** (legacy): trigram index was rebuilt from `names_lower` on every load.
//! v1 caches are rejected and rebuilt automatically.

use std::path::PathBuf;
use std::time::Instant;

use crate::compact::{ChildrenIndex, CompactRecord, DriveCompactIndex, IndexSource};
use crate::trigram::TrigramIndex;

/// Magic bytes for compact cache files.
const COMPACT_MAGIC: &[u8; 8] = b"UFFSCOM\0";
/// Current compact cache format version (v3 adds `source_epoch`).
const COMPACT_VERSION: u16 = 3;
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

/// Serializes the compact index (records, names, children, trigram postings).
#[must_use]
pub fn serialize_compact(index: &DriveCompactIndex) -> Vec<u8> {
    let record_count = index.records.len();
    let names_len = index.names.len();

    // Children CSR — already in contiguous layout.
    let (csr_offsets, csr_values) = index.children.as_csr();
    let (tri_keys, tri_offsets, tri_values) = index.trigram.as_csr();

    let total = 26 // header: 8 (magic) + 2 (ver) + 4 (rc) + 4 (nl) + 8 (epoch)
        + record_count * RECORD_BYTES
        + names_len * 2
        + csr_offsets.len() * 4
        + csr_values.len() * 4
        + 4
        + tri_keys.len() * 3
        + tri_offsets.len() * 4
        + tri_values.len() * 4;
    let mut buf = Vec::with_capacity(total);

    // Header (26 bytes for v3)
    buf.extend_from_slice(COMPACT_MAGIC);
    buf.extend_from_slice(&COMPACT_VERSION.to_le_bytes());
    push_u32(&mut buf, record_count);
    push_u32(&mut buf, names_len);
    // v3: source_epoch
    buf.extend_from_slice(&index.source_epoch.to_le_bytes());

    // Records — single bulk copy via bytemuck (Pod layout = on-disk layout)
    buf.extend_from_slice(bytemuck::cast_slice(&index.records));

    // Names + names_lower
    buf.extend_from_slice(&index.names);
    buf.extend_from_slice(&index.names_lower);

    // Children CSR — bulk cast (u32 slices → &[u8] via bytemuck, zero-copy on LE)
    buf.extend_from_slice(bytemuck::cast_slice(csr_offsets));
    buf.extend_from_slice(bytemuck::cast_slice(csr_values));

    // ─── v2: Trigram postings CSR ─────────────────────────────────

    #[expect(
        clippy::cast_possible_truncation,
        reason = "trigram count bounded by alphabet³ ≤ 17K"
    )]
    let trigram_count = tri_keys.len() as u32;
    buf.extend_from_slice(&trigram_count.to_le_bytes());

    // Keys (3 bytes each, already sorted) — bulk copy
    buf.extend_from_slice(bytemuck::cast_slice(tri_keys));

    // CSR offsets + posting values — bulk cast
    buf.extend_from_slice(bytemuck::cast_slice(tri_offsets));
    buf.extend_from_slice(bytemuck::cast_slice(tri_values));

    buf
}

/// Deserializes a compact index from raw bytes.
///
/// **v3**: trigram postings + `source_epoch`.
/// **v2**: trigram postings, `source_epoch` = 0 (accepted, triggers rebuild).
/// **v1**: rejected — returns an error so the caller rebuilds from `MftIndex`.
///
/// Returns `(DriveCompactIndex, trigram_load_ms)`.
///
/// # Errors
/// Returns an error string if the data is truncated, wrong magic, or v1.
pub fn deserialize_compact(
    data: &[u8],
    drive_letter: char,
) -> Result<(DriveCompactIndex, u128), &'static str> {
    let (source_epoch, body_offset) = parse_compact_header(data)?;

    let record_count = read_u32(data, 10) as usize;
    let names_len = read_u32(data, 14) as usize;
    let records_end = body_offset + record_count * RECORD_BYTES;
    let names_end = records_end + names_len;
    let names_lower_end = names_end + names_len;
    let csr_offsets_end = names_lower_end + (record_count + 1) * 4;
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
    let names_lower = data
        .get(names_end..names_lower_end)
        .ok_or("truncated lower")?
        .to_vec();

    // Children CSR — alignment-safe copy into aligned Vec<u32>
    let offsets_slice = data
        .get(names_lower_end..csr_offsets_end)
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

    // ─── Trigram CSR (v2+) ─────────────────────────────────────────
    let tri_start = Instant::now();
    let tri_hdr = postings_end;
    if data.len() < tri_hdr + 4 {
        return Err("truncated trigram header");
    }
    let tri_count = read_u32(data, tri_hdr) as usize;
    let tri_keys_end = tri_hdr + 4 + tri_count * 3;
    let tri_offs_end = tri_keys_end + (tri_count + 1) * 4;
    if data.len() < tri_offs_end {
        return Err("truncated trigram offsets");
    }
    let tri_post_count = read_u32(data, tri_offs_end - 4) as usize;
    let tri_vals_end = tri_offs_end + tri_post_count * 4;
    if data.len() < tri_vals_end {
        return Err("truncated trigram postings");
    }
    let trigram = TrigramIndex::from_csr(
        aligned_vec_from_bytes(
            data.get(tri_hdr + 4..tri_keys_end)
                .ok_or("trigram keys OOB")?,
        ),
        aligned_vec_from_bytes(
            data.get(tri_keys_end..tri_offs_end)
                .ok_or("trigram offsets OOB")?,
        ),
        aligned_vec_from_bytes(
            data.get(tri_offs_end..tri_vals_end)
                .ok_or("trigram values OOB")?,
        ),
    );
    let tri_ms = tri_start.elapsed().as_millis();

    Ok((
        DriveCompactIndex {
            letter: drive_letter,
            records,
            names,
            names_lower,
            trigram,
            children,
            source: IndexSource::MftFile(PathBuf::from(format!("{drive_letter}:"))),
            source_epoch,
        },
        tri_ms,
    ))
}

/// Validates magic/version and returns `(source_epoch, body_offset)`.
fn parse_compact_header(data: &[u8]) -> Result<(u64, usize), &'static str> {
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
        Ok((epoch, 26))
    } else {
        Ok((0, 18))
    }
}

// ─── Save / Load ────────────────────────────────────────────────────────────

/// Saves a compact index to its cache file (zstd + AES-256-GCM).
///
/// # Errors
/// Returns an error if compression, encryption, or file writing fails.
#[expect(clippy::print_stderr, reason = "UFFS_CACHE_PROFILE diagnostic output")]
pub fn save_compact_cache(index: &DriveCompactIndex) -> std::io::Result<()> {
    let profile = std::env::var_os("UFFS_CACHE_PROFILE").is_some();
    let t_total = Instant::now();
    let t_ser = Instant::now();
    let serialized = serialize_compact(index);
    let ser_ms = t_ser.elapsed().as_millis();
    let uncompressed_len = serialized.len();
    let t_compress = Instant::now();
    let compressed = zstd::encode_all(serialized.as_slice(), ZSTD_LEVEL)
        .map_err(|err| std::io::Error::other(format!("compact zstd failed: {err}")))?;
    let compress_ms = t_compress.elapsed().as_millis();
    let compressed_len = compressed.len();
    let key = uffs_security::keystore::get_cache_key()
        .map_err(|err| std::io::Error::other(format!("key unavailable: {err}")))?;
    let t_encrypt = Instant::now();
    let encrypted = uffs_security::crypto::encrypt_cache(&compressed, &key)?;
    let encrypt_ms = t_encrypt.elapsed().as_millis();
    let path = compact_cache_path(index.letter);
    uffs_mft::cache::create_secure_dir(&path)?;
    let t_write = Instant::now();
    uffs_mft::cache::atomic_write(&path, &encrypted)?;
    let write_ms = t_write.elapsed().as_millis();
    if profile {
        let uncomp_mb = uncompressed_len / (1024 * 1024);
        let comp_mb = compressed_len / (1024 * 1024);
        eprintln!("[CACHE_PROFILE] compact_ser:   {ser_ms:>6} ms  (~{uncomp_mb} MB)");
        eprintln!(
            "[CACHE_PROFILE] compact_zstd:  {compress_ms:>6} ms  (~{uncomp_mb} MB → ~{comp_mb} MB)"
        );
        eprintln!("[CACHE_PROFILE] compact_enc:   {encrypt_ms:>6} ms  (~{comp_mb} MB)");
        eprintln!("[CACHE_PROFILE] compact_write: {write_ms:>6} ms  (~{comp_mb} MB)");
        eprintln!(
            "[CACHE_PROFILE] compact_save:  {:>6} ms  total",
            t_total.elapsed().as_millis()
        );
    }
    Ok(())
}

/// Loads a compact index from its cache file if fresh. Returns `None` if
/// cache is missing, stale, corrupt, or built from an older `MftIndex`.
///
/// `mft_build_epoch` is the `build_epoch` of the current `MftIndex`.
/// If the compact cache was built from an older epoch it is considered stale
/// and `None` is returned so the caller rebuilds.
#[must_use]
#[expect(clippy::print_stderr, reason = "UFFS_CACHE_PROFILE diagnostic output")]
pub fn load_compact_cache(
    drive_letter: char,
    ttl_seconds: u64,
    mft_build_epoch: u64,
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
        if let Ok((source_epoch, _)) = parse_compact_header(&plaintext) {
            if source_epoch < mft_build_epoch {
                if profile {
                    eprintln!(
                        "[CACHE_PROFILE] compact: STALE (source_epoch {source_epoch} < mft_epoch {mft_build_epoch})"
                    );
                }
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
        eprintln!("[CACHE_PROFILE] compact_read:  {read_ms:>6} ms  (~{raw_mb} MB)");
        eprintln!("[CACHE_PROFILE] compact_dec:   {decrypt_ms:>6} ms");
        if is_compressed {
            eprintln!("[CACHE_PROFILE] compact_dz:    {decompress_ms:>6} ms  (~{plain_mb} MB)");
        }
        eprintln!(
            "[CACHE_PROFILE] compact_deser: {deser_ms:>6} ms  ({} records, tri_load={tri_ms} ms)",
            index.records.len()
        );
        eprintln!(
            "[CACHE_PROFILE] compact_total: {:>6} ms  (source_epoch={})",
            t_total.elapsed().as_millis(),
            index.source_epoch,
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
/// Always emits `[CACHE_PROFILE]` timing when `UFFS_CACHE_PROFILE` is set.
/// The caller may discard the returned index if only the `MftIndex` is needed.
#[expect(clippy::print_stderr, reason = "UFFS_CACHE_PROFILE diagnostic output")]
pub fn ensure_compact_cached(
    drive_letter: char,
    mft_index: &uffs_mft::MftIndex,
) -> DriveCompactIndex {
    let profile = std::env::var_os("UFFS_CACHE_PROFILE").is_some();

    // Try loading existing compact cache (epoch check catches stale caches)
    if let Some(cached) = load_compact_cache(
        drive_letter,
        super::compact::INDEX_TTL_SECONDS,
        mft_index.build_epoch,
    ) {
        if profile {
            eprintln!(
                "[CACHE_PROFILE] compact: loaded from cache ({} records)",
                cached.records.len()
            );
        }
        return cached;
    }

    // Build from MftIndex
    let t_build = Instant::now();
    let (compact, build_ms, tri_ms) = crate::compact::build_compact_index(drive_letter, mft_index);
    let total_build_ms = t_build.elapsed().as_millis();

    if profile {
        eprintln!(
            "[CACHE_PROFILE] compact_build: {build_ms:>6} ms  ({} records)",
            compact.records.len()
        );
        eprintln!("[CACHE_PROFILE] compact_tri:   {tri_ms:>6} ms");
        eprintln!("[CACHE_PROFILE] compact_total: {total_build_ms:>6} ms  (build+trigram)");
    }

    // Save to disk (best-effort)
    if let Err(err) = save_compact_cache(&compact) {
        tracing::warn!(drive = %drive_letter, error = %err, "Failed to save compact cache");
    } else if profile {
        // Report on-disk size of both caches
        let compact_path = compact_cache_path(drive_letter);
        let mft_path = uffs_mft::cache::cache_file_path(drive_letter);
        let compact_disk = std::fs::metadata(&compact_path).map_or(0, |meta| meta.len());
        let mft_disk = std::fs::metadata(mft_path).map_or(0, |meta| meta.len());
        let compact_disk_mb = compact_disk / (1024 * 1024);
        let mft_disk_mb = mft_disk / (1024 * 1024);
        let total_disk_mb = compact_disk_mb + mft_disk_mb;
        eprintln!("[CACHE_PROFILE] ─── disk summary ───");
        eprintln!("[CACHE_PROFILE] mft_index:     ~{mft_disk_mb} MB on disk");
        eprintln!("[CACHE_PROFILE] compact_index: ~{compact_disk_mb} MB on disk");
        eprintln!("[CACHE_PROFILE] total_cache:   ~{total_disk_mb} MB on disk");
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
