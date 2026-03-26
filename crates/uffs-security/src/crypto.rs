//! AES-256-GCM authenticated encryption for cache files.
//!
//! # UFFSENC File Format
//!
//! ```text
//! Offset  Size    Field
//! ──────  ──────  ──────────────────────────────
//! 0       8       Magic: b"UFFSENC\0"
//! 8       2       Format version (u16 LE) — currently 1
//! 10      1       Algorithm ID: 0x01 = AES-256-GCM
//! 11      1       KDF ID (0x01=DPAPI, 0x02=Keychain, 0x03=SecretService, 0x04=HKDF)
//! 12      12      Nonce (96-bit, random per write)
//! 24      4       Plaintext length (u32 LE)
//! 28      N       Ciphertext
//! 28+N    16      GCM Authentication Tag
//! ────────────────────────────────────────────────
//! Total overhead: 44 bytes
//! AAD: bytes 0..28 (header, included in GCM auth)
//! ```

use std::io;

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::{AeadInPlace, Aes256Gcm, KeyInit, Nonce};

// ────────────────────────────────────────────────────────────────────────────
// Constants
// ────────────────────────────────────────────────────────────────────────────

/// Magic bytes identifying an encrypted UFFS cache file.
pub const ENCRYPTED_MAGIC: &[u8; 8] = b"UFFSENC\0";

/// Magic bytes identifying a legacy plaintext UFFS cache file.
pub const LEGACY_MAGIC: &[u8; 8] = b"UFFSIDX\0";

/// Current encryption format version.
pub const ENC_FORMAT_VERSION: u16 = 1;

/// Algorithm ID for AES-256-GCM.
pub const ALGO_AES_256_GCM: u8 = 0x01;

/// KDF ID: Windows DPAPI.
pub const KDF_DPAPI: u8 = 0x01;
/// KDF ID: macOS Keychain.
pub const KDF_KEYCHAIN: u8 = 0x02;
/// KDF ID: Linux Secret Service (D-Bus).
pub const KDF_SECRET_SERVICE: u8 = 0x03;
/// KDF ID: HKDF fallback (headless Linux).
pub const KDF_HKDF: u8 = 0x04;

/// Size of the UFFSENC header (before ciphertext).
const HEADER_SIZE: usize = 28;
/// Size of the GCM authentication tag.
const TAG_SIZE: usize = 16;
/// Size of the AES-GCM nonce (96 bits).
const NONCE_SIZE: usize = 12;

// ────────────────────────────────────────────────────────────────────────────
// Format Detection
// ────────────────────────────────────────────────────────────────────────────

/// Detected cache file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheFormat {
    /// Encrypted with UFFSENC header.
    Encrypted,
    /// Legacy plaintext with UFFSIDX header.
    LegacyPlaintext,
    /// Unknown / unrecognised format.
    Unknown,
}

/// Detects the format of a cache file from its first bytes.
///
/// Requires at least 8 bytes to identify the magic.
#[must_use]
pub fn detect_format(data: &[u8]) -> CacheFormat {
    if let Some(magic) = data.get(..8) {
        if *magic == *ENCRYPTED_MAGIC {
            return CacheFormat::Encrypted;
        }
        if *magic == *LEGACY_MAGIC {
            return CacheFormat::LegacyPlaintext;
        }
    }
    CacheFormat::Unknown
}

// ────────────────────────────────────────────────────────────────────────────
// Encrypt
// ────────────────────────────────────────────────────────────────────────────

/// Encrypts plaintext using AES-256-GCM and wraps it in the UFFSENC format.
///
/// # Errors
///
/// Returns an error if encryption fails (should not happen with valid key).
pub fn encrypt_cache(plaintext: &[u8], key: &[u8; 32]) -> io::Result<Vec<u8>> {
    use rand::Rng;

    // Validate plaintext length fits in u32
    let plaintext_len: u32 = u32::try_from(plaintext.len()).map_err(|_convert_err| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "plaintext too large for UFFSENC format (max 4 GB)",
        )
    })?;

    // Generate random 96-bit nonce
    let mut nonce_bytes = [0_u8; NONCE_SIZE];
    rand::rng().fill_bytes(&mut nonce_bytes);

    // KDF ID for the current platform
    #[cfg(target_os = "windows")]
    let kdf_id = KDF_DPAPI;
    #[cfg(target_os = "macos")]
    let kdf_id = KDF_KEYCHAIN;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let kdf_id = KDF_SECRET_SERVICE;

    // Build header (28 bytes)
    let mut output = Vec::with_capacity(HEADER_SIZE + plaintext.len() + TAG_SIZE);
    output.extend_from_slice(ENCRYPTED_MAGIC); // 0..8
    output.extend_from_slice(&ENC_FORMAT_VERSION.to_le_bytes()); // 8..10
    output.push(ALGO_AES_256_GCM); // 10
    output.push(kdf_id); // 11
    output.extend_from_slice(&nonce_bytes); // 12..24
    output.extend_from_slice(&plaintext_len.to_le_bytes()); // 24..28

    // AAD = header bytes 0..28
    let aad = output
        .get(..HEADER_SIZE)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "header shorter than expected"))?
        .to_vec();

    // Append plaintext (will be encrypted in-place)
    let ciphertext_start = output.len();
    output.extend_from_slice(plaintext);

    // Encrypt in-place
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key));
    let nonce = Nonce::from_slice(&nonce_bytes);
    let tag = cipher
        .encrypt_in_place_detached(
            nonce,
            &aad,
            output.get_mut(ciphertext_start..).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "ciphertext offset out of bounds",
                )
            })?,
        )
        .map_err(|enc_err| io::Error::other(format!("AES-GCM encrypt failed: {enc_err}")))?;

    // Append 16-byte GCM tag
    output.extend_from_slice(&tag);

    Ok(output)
}

// ────────────────────────────────────────────────────────────────────────────
// Decrypt
// ────────────────────────────────────────────────────────────────────────────

/// Decrypts an UFFSENC-formatted buffer, returning the original plaintext.
///
/// Validates the header, algorithm ID, and GCM authentication tag.
///
/// # Errors
///
/// Returns an error if:
/// - The data is too short or has wrong magic
/// - The algorithm or version is unsupported
/// - GCM authentication fails (tampered data or wrong key)
pub fn decrypt_cache(data: &[u8], key: &[u8; 32]) -> io::Result<Vec<u8>> {
    // Minimum size: header (28) + tag (16) = 44 bytes (for 0-byte plaintext)
    if data.len() < HEADER_SIZE + TAG_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "encrypted cache too short: {} bytes (minimum {})",
                data.len(),
                HEADER_SIZE + TAG_SIZE
            ),
        ));
    }

    // We need at least HEADER_SIZE (28) bytes for the header fields.
    // Extract all header bytes via safe `.get()` accessors.
    let header = data
        .get(..HEADER_SIZE)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "data too short for header"))?;

    // Validate magic
    let magic = header
        .get(..8)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "data too short for magic"))?;
    if *magic != *ENCRYPTED_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not an encrypted UFFS cache file (bad magic)",
        ));
    }

    // Parse version from header[8..10]
    let ver_hi = *header
        .get(8)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing version byte"))?;
    let ver_lo = *header
        .get(9)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing version byte"))?;
    let version = u16::from_le_bytes([ver_hi, ver_lo]);
    if version != ENC_FORMAT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported encryption format version: {version}"),
        ));
    }

    let algo = *header
        .get(10)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing algorithm byte"))?;
    if algo != ALGO_AES_256_GCM {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported encryption algorithm: 0x{algo:02x}"),
        ));
    }

    // KDF ID at header[11] — informational, not validated during decrypt

    let nonce_bytes: &[u8; NONCE_SIZE] = header
        .get(12..24)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "data too short for nonce"))?
        .try_into()
        .map_err(|_nonce_err| io::Error::new(io::ErrorKind::InvalidData, "invalid nonce"))?;

    let len_bytes: &[u8; 4] = header
        .get(24..28)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing plaintext length"))?
        .try_into()
        .map_err(|_len_err| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid plaintext length bytes")
        })?;
    let plaintext_len = u32::from_le_bytes(*len_bytes) as usize;

    // Validate lengths
    let expected_total = HEADER_SIZE + plaintext_len + TAG_SIZE;
    if data.len() < expected_total {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "encrypted cache truncated: have {} bytes, expected {}",
                data.len(),
                expected_total
            ),
        ));
    }

    // Extract components — bounds guaranteed by expected_total check
    let aad = data
        .get(..HEADER_SIZE)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "header slice out of bounds"))?;
    let ciphertext = data
        .get(HEADER_SIZE..HEADER_SIZE + plaintext_len)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "ciphertext slice out of bounds")
        })?;
    let tag = data
        .get(HEADER_SIZE + plaintext_len..HEADER_SIZE + plaintext_len + TAG_SIZE)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "tag slice out of bounds"))?;

    // Decrypt
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key));
    let nonce = Nonce::from_slice(nonce_bytes);
    let tag_arr = GenericArray::from_slice(tag);

    let mut plaintext = ciphertext.to_vec();
    cipher
        .decrypt_in_place_detached(nonce, aad, &mut plaintext, tag_arr)
        .map_err(|_dec_err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "AES-GCM authentication failed (wrong key or tampered data)",
            )
        })?;

    Ok(plaintext)
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// S2.3.5: encrypt → decrypt round-trip, various sizes.
    #[test]
    fn round_trip_empty() {
        let key = [0x42_u8; 32];
        let plaintext = b"";
        let encrypted = encrypt_cache(plaintext, &key).expect("encrypt");
        let decrypted = decrypt_cache(&encrypted, &key).expect("decrypt");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn round_trip_1_byte() {
        let key = [0xAB_u8; 32];
        let plaintext = b"X";
        let encrypted = encrypt_cache(plaintext, &key).expect("encrypt");
        let decrypted = decrypt_cache(&encrypted, &key).expect("decrypt");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn round_trip_1mb() {
        let key = [0xCD_u8; 32];
        let plaintext = vec![0x55_u8; 1024 * 1024];
        let encrypted = encrypt_cache(&plaintext, &key).expect("encrypt");
        let decrypted = decrypt_cache(&encrypted, &key).expect("decrypt");
        assert_eq!(decrypted, plaintext);
    }

    /// S2.3.6: tampered ciphertext → decrypt fails.
    #[test]
    fn tampered_ciphertext() {
        let key = [0x11_u8; 32];
        let plaintext = b"hello world";
        let mut encrypted = encrypt_cache(plaintext, &key).expect("encrypt");
        // Flip a byte in the ciphertext region
        if let Some(byte) = encrypted.get_mut(HEADER_SIZE) {
            *byte ^= 0xFF;
        }
        assert!(decrypt_cache(&encrypted, &key).is_err());
    }

    /// S2.3.7: tampered header → decrypt fails (AAD mismatch).
    #[test]
    fn tampered_header_nonce() {
        let key = [0x22_u8; 32];
        let plaintext = b"hello world";
        let mut encrypted = encrypt_cache(plaintext, &key).expect("encrypt");
        // Flip a nonce byte
        if let Some(byte) = encrypted.get_mut(14) {
            *byte ^= 0xFF;
        }
        assert!(decrypt_cache(&encrypted, &key).is_err());
    }

    #[test]
    fn tampered_header_algo() {
        let key = [0x33_u8; 32];
        let plaintext = b"hello world";
        let mut encrypted = encrypt_cache(plaintext, &key).expect("encrypt");
        // Change algo ID
        if let Some(byte) = encrypted.get_mut(10) {
            *byte = 0xFF;
        }
        assert!(decrypt_cache(&encrypted, &key).is_err());
    }

    /// S2.3.8: truncated file → decrypt fails.
    #[test]
    fn truncated_file() {
        let key = [0x44_u8; 32];
        let plaintext = b"hello world";
        let encrypted = encrypt_cache(plaintext, &key).expect("encrypt");
        // Truncate to just the header
        let header_only = encrypted.get(..HEADER_SIZE).expect("header slice");
        assert!(decrypt_cache(header_only, &key).is_err());
        // Truncate mid-ciphertext
        let partial = encrypted.get(..HEADER_SIZE + 5).expect("partial slice");
        assert!(decrypt_cache(partial, &key).is_err());
    }

    /// S2.3.9: legacy UFFSIDX magic → `detect_format` returns
    /// `LegacyPlaintext`.
    #[test]
    fn detect_legacy() {
        let mut data = vec![0_u8; 64];
        data.get_mut(..8)
            .expect("data slice")
            .copy_from_slice(LEGACY_MAGIC);
        assert_eq!(detect_format(&data), CacheFormat::LegacyPlaintext);
    }

    #[test]
    fn detect_encrypted() {
        let mut data = vec![0_u8; 64];
        data.get_mut(..8)
            .expect("data slice")
            .copy_from_slice(ENCRYPTED_MAGIC);
        assert_eq!(detect_format(&data), CacheFormat::Encrypted);
    }

    #[test]
    fn detect_unknown() {
        assert_eq!(detect_format(b"RANDOM"), CacheFormat::Unknown);
        assert_eq!(detect_format(b""), CacheFormat::Unknown);
    }

    /// Wrong key → decrypt fails.
    #[test]
    fn wrong_key() {
        let key1 = [0x11_u8; 32];
        let key2 = [0x22_u8; 32];
        let plaintext = b"secret data";
        let encrypted = encrypt_cache(plaintext, &key1).expect("encrypt");
        assert!(decrypt_cache(&encrypted, &key2).is_err());
    }
}
