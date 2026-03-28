//! File-based persistence wrappers for `MftIndex` snapshots.
//!
//! Cache files are encrypted with AES-256-GCM when a platform key is
//! available. Legacy plaintext files (`UFFSIDX` magic) are auto-migrated
//! to encrypted format on first load.
//!
//! Since v0.4.22 the serialized bytes are zstd-compressed before encryption.
//! On load, the decompressor detects the zstd frame magic (`0xFD2FB528`) and
//! decompresses automatically; older uncompressed caches are still loaded
//! transparently.

use super::IndexHeader;
use crate::index::MftIndex;

/// zstd frame magic bytes (little-endian `0xFD2FB528`).
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Default zstd compression level (3 = good balance of speed vs ratio).
const ZSTD_LEVEL: i32 = 3;

/// Returns `true` if `data` starts with the zstd frame magic.
fn is_zstd_compressed(data: &[u8]) -> bool {
    data.get(..4).is_some_and(|m| m == ZSTD_MAGIC)
}

impl MftIndex {
    /// Saves the index to a file.
    ///
    /// The serialized bytes are zstd-compressed and then encrypted with
    /// AES-256-GCM before writing. Encryption is mandatory — if the key
    /// is unavailable or encryption fails, an error is returned and **no
    /// data is written to disk**.
    ///
    /// # Errors
    ///
    /// Returns an error if compression, encryption, or file writing fails.
    #[expect(clippy::print_stderr, reason = "UFFS_CACHE_PROFILE diagnostic output")]
    pub fn save_to_file(
        &self,
        path: &std::path::Path,
        volume_serial: u64,
        usn_journal_id: u64,
        next_usn: i64,
    ) -> std::io::Result<()> {
        let profile = std::env::var_os("UFFS_CACHE_PROFILE").is_some();

        let serialized = self.serialize(volume_serial, usn_journal_id, next_usn);
        let uncompressed_len = serialized.len();

        // Compress with zstd before encryption
        let t_compress = std::time::Instant::now();
        let compressed = zstd::encode_all(serialized.as_slice(), ZSTD_LEVEL)
            .map_err(|e| std::io::Error::other(format!("zstd compression failed: {e}")))?;
        let compress_ms = t_compress.elapsed().as_millis();
        let compressed_len = compressed.len();

        if profile {
            #[expect(clippy::cast_precision_loss, reason = "display-only MB values")]
            let mb = |b: usize| b as f64 / (1024.0 * 1024.0);
            #[expect(clippy::cast_precision_loss, reason = "display-only ratio")]
            let ratio = uncompressed_len as f64 / compressed_len as f64;
            eprintln!(
                "[CACHE_PROFILE] compress:      {compress_ms:>6} ms  ({:.1} MB → {:.1} MB, {ratio:.1}x)",
                mb(uncompressed_len),
                mb(compressed_len),
            );
        }

        let key = uffs_security::keystore::get_cache_key().map_err(|e| {
            std::io::Error::other(format!("cannot save cache without encryption key: {e}"))
        })?;

        let data = uffs_security::crypto::encrypt_cache(&compressed, &key)?;

        crate::cache::atomic_write(path, &data)
    }

    /// Loads an index from a file.
    ///
    /// Detects the file format automatically:
    /// - **`UFFSENC`**: decrypts with the platform key, then deserializes
    /// - **`UFFSIDX`** (legacy plaintext): deserializes directly, then re-saves
    ///   as encrypted (one-time auto-migration)
    /// - **Unknown**: returns an error
    ///
    /// After decryption, if the plaintext starts with the zstd frame magic
    /// (`0xFD2FB528`), it is decompressed before deserialization. Older
    /// uncompressed caches are loaded transparently.
    ///
    /// If decryption fails (wrong key / tampered), the corrupted file is
    /// deleted and an error is returned so the caller rebuilds from MFT.
    ///
    /// Set `UFFS_CACHE_PROFILE=1` to emit per-phase timing to stderr.
    ///
    /// # Errors
    ///
    /// Returns an error if file reading, decryption, or deserialization fails.
    #[expect(clippy::print_stderr, reason = "UFFS_CACHE_PROFILE diagnostic output")]
    pub fn load_from_file(
        path: &std::path::Path,
    ) -> Result<(Self, IndexHeader), Box<dyn core::error::Error>> {
        use uffs_security::crypto::{CacheFormat, decrypt_cache, detect_format};

        let profile = std::env::var_os("UFFS_CACHE_PROFILE").is_some();
        let t_total = std::time::Instant::now();

        let t0 = std::time::Instant::now();
        let raw = std::fs::read(path)?;
        let read_ms = t0.elapsed().as_millis();
        let raw_len = raw.len();

        let format = detect_format(&raw);

        let t1 = std::time::Instant::now();
        let decrypted = match format {
            CacheFormat::Encrypted => {
                let key = uffs_security::keystore::get_cache_key()
                    .map_err(|e| Box::new(e) as Box<dyn core::error::Error>)?;
                match decrypt_cache(&raw, &key) {
                    Ok(pt) => pt,
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            error = %e,
                            "Cache decryption failed — deleting corrupted file"
                        );
                        let _ignore = std::fs::remove_file(path);
                        return Err(Box::new(e));
                    }
                }
            }
            CacheFormat::LegacyPlaintext => {
                tracing::info!(
                    path = %path.display(),
                    "Loading legacy plaintext cache (will re-encrypt on next save)"
                );
                raw
            }
            CacheFormat::Unknown => {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unknown cache file format: {}", path.display()),
                )));
            }
        };
        let decrypt_ms = t1.elapsed().as_millis();
        let decrypted_len = decrypted.len();

        // Decompress if zstd-compressed (backward compat: old caches skip this)
        let t_decompress = std::time::Instant::now();
        let compressed = is_zstd_compressed(&decrypted);
        let plaintext = if compressed {
            zstd::decode_all(decrypted.as_slice()).map_err(|e| {
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("zstd decompression failed: {e}"),
                )) as Box<dyn core::error::Error>
            })?
        } else {
            decrypted
        };
        let decompress_ms = t_decompress.elapsed().as_millis();
        let plaintext_len = plaintext.len();

        let t2 = std::time::Instant::now();
        let (index, header) = Self::deserialize(&plaintext)?;
        let deser_ms = t2.elapsed().as_millis();

        let total_ms = t_total.elapsed().as_millis();

        if profile {
            #[expect(clippy::cast_precision_loss, reason = "display-only MB values")]
            let mb = |b: usize| b as f64 / (1024.0 * 1024.0);
            eprintln!(
                "[CACHE_PROFILE] file_read:     {read_ms:>6} ms  ({:.1} MB)",
                mb(raw_len)
            );
            eprintln!(
                "[CACHE_PROFILE] decrypt:       {decrypt_ms:>6} ms  ({:.1} MB)",
                mb(decrypted_len)
            );
            if compressed {
                eprintln!(
                    "[CACHE_PROFILE] decompress:    {decompress_ms:>6} ms  ({:.1} MB → {:.1} MB)",
                    mb(decrypted_len),
                    mb(plaintext_len),
                );
            }
            eprintln!(
                "[CACHE_PROFILE] deserialize:   {deser_ms:>6} ms  ({} records)",
                index.len()
            );
            eprintln!("[CACHE_PROFILE] total_load:    {total_ms:>6} ms");
        }

        Ok((index, header))
    }
}
