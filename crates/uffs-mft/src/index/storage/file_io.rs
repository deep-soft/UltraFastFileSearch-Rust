//! File-based persistence wrappers for `MftIndex` snapshots.
//!
//! Cache files are encrypted with AES-256-GCM when a platform key is
//! available. Legacy plaintext files (`UFFSIDX` magic) are auto-migrated
//! to encrypted format on first load.

use super::IndexHeader;
use crate::index::MftIndex;

impl MftIndex {
    /// Saves the index to a file.
    ///
    /// The serialized bytes are encrypted with AES-256-GCM before writing.
    /// If the encryption key is unavailable, falls back to plaintext with a
    /// warning (never blocks the user).
    ///
    /// # Errors
    ///
    /// Returns an error if file writing fails.
    pub fn save_to_file(
        &self,
        path: &std::path::Path,
        volume_serial: u64,
        usn_journal_id: u64,
        next_usn: i64,
    ) -> std::io::Result<()> {
        let plaintext = self.serialize(volume_serial, usn_journal_id, next_usn);

        let data = match uffs_security::keystore::get_cache_key() {
            Ok(key) => match uffs_security::crypto::encrypt_cache(&plaintext, &key) {
                Ok(encrypted) => encrypted,
                Err(e) => {
                    tracing::warn!(error = %e, "Encryption failed, saving plaintext");
                    plaintext
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "Key unavailable, saving plaintext");
                plaintext
            }
        };

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
    /// If decryption fails (wrong key / tampered), the corrupted file is
    /// deleted and an error is returned so the caller rebuilds from MFT.
    ///
    /// Set `UFFS_CACHE_PROFILE=1` to emit per-phase timing to stderr.
    ///
    /// # Errors
    ///
    /// Returns an error if file reading, decryption, or deserialization fails.
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
        let plaintext = match format {
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
                "[CACHE_PROFILE] decrypt:       {decrypt_ms:>6} ms  ({:.1} MB plaintext)",
                mb(plaintext_len)
            );
            eprintln!(
                "[CACHE_PROFILE] deserialize:   {deser_ms:>6} ms  ({} records)",
                index.len()
            );
            eprintln!("[CACHE_PROFILE] total_load:    {total_ms:>6} ms");
        }

        Ok((index, header))
    }
}
