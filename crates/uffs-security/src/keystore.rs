//! Platform-native secure key storage.
//!
//! Provides [`get_cache_key`] which returns a 256-bit AES key, generating and
//! storing it on first use via the OS-native secure vault:
//!
//! | Platform | Backend | Key Location |
//! |----------|---------|-------------|
//! | **macOS** | Keychain Services | `com.uffs.cache` / `encryption-key-v1` |
//! | **Windows** | File-based (secure dir + hidden attr) | `%LOCALAPPDATA%/uffs/key.bin` |
//! | **Linux** | File-based (secure dir + 0600 perms) | `~/.local/share/uffs/key.bin` |
//!
//! The user never sees, configures, or manages keys. If the key is lost
//! (keychain corruption, password reset), a new key is generated and old
//! cache files trigger a rebuild from MFT.

use std::io;

/// Size of the AES-256 key in bytes.
const KEY_SIZE: usize = 32;

// ────────────────────────────────────────────────────────────────────────────
// Public API
// ────────────────────────────────────────────────────────────────────────────

/// Returns the 256-bit AES cache encryption key, generating one on first use.
///
/// The key is persisted in the platform's secure storage so it survives
/// across process restarts.
///
/// # Errors
///
/// Returns an error if key generation or storage fails.
pub fn get_cache_key() -> io::Result<[u8; KEY_SIZE]> {
    platform_get_or_create_key()
}

// ────────────────────────────────────────────────────────────────────────────
// macOS: Keychain Services
// ────────────────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn platform_get_or_create_key() -> io::Result<[u8; KEY_SIZE]> {
    use security_framework::passwords::{
        delete_generic_password, get_generic_password, set_generic_password,
    };

    const SERVICE: &str = "com.uffs.cache";
    const ACCOUNT: &str = "encryption-key-v1";

    // Try to retrieve existing key
    match get_generic_password(SERVICE, ACCOUNT) {
        Ok(key_data) => {
            if key_data.len() == KEY_SIZE {
                let mut key = [0u8; KEY_SIZE];
                key.copy_from_slice(&key_data);
                return Ok(key);
            }
            // Wrong size — delete and regenerate
            tracing::warn!(
                len = key_data.len(),
                "Keychain entry has wrong size, regenerating"
            );
            let _ignore = delete_generic_password(SERVICE, ACCOUNT);
        }
        Err(e) => {
            // errSecItemNotFound is the expected "not yet created" case
            let code = e.code();
            if code != -25300 {
                // -25300 = errSecItemNotFound
                tracing::debug!(
                    error_code = code,
                    "Keychain lookup failed (will generate new key)"
                );
            }
        }
    }

    // Generate new key
    let key = generate_key()?;

    // Store in Keychain
    set_generic_password(SERVICE, ACCOUNT, &key).map_err(|e| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("failed to store key in Keychain: {e}"),
        )
    })?;

    tracing::info!("Generated and stored new encryption key in macOS Keychain");
    Ok(key)
}

// ────────────────────────────────────────────────────────────────────────────
// Windows + Linux: file-based key in secure directory
// ────────────────────────────────────────────────────────────────────────────

/// File-based key storage for Windows and Linux.
///
/// The key is a raw 32-byte file stored under the platform's local data dir
/// with owner-only permissions (`0600` on Linux, hidden attribute on Windows).
/// This is protected against casual snooping; combined with cache encryption
/// (S2), the actual file contents are opaque even if the key file is read.
#[cfg(not(target_os = "macos"))]
fn platform_get_or_create_key() -> io::Result<[u8; KEY_SIZE]> {
    let key_path = key_file_path()?;

    // Try to read existing key
    if key_path.exists() {
        let data = std::fs::read(&key_path)?;
        if data.len() == KEY_SIZE {
            let mut key = [0u8; KEY_SIZE];
            key.copy_from_slice(&data);
            return Ok(key);
        }
        // Wrong size — regenerate
        tracing::warn!(
            len = data.len(),
            path = %key_path.display(),
            "Key file has wrong size, regenerating"
        );
    }

    // Generate new key
    let key = generate_key()?;

    // Ensure parent dir exists with secure permissions
    if let Some(parent) = key_path.parent() {
        crate::fs::create_secure_dir(parent)?;
    }

    // Write key file with owner-only permissions
    std::fs::write(&key_path, key)?;
    crate::fs::set_file_permissions_owner_only(&key_path)?;

    tracing::info!(path = %key_path.display(), "Generated and stored new encryption key");
    Ok(key)
}

/// Returns the key file path.
///
/// - **Windows**: `%LOCALAPPDATA%/uffs/key.bin`
/// - **Linux**: `~/.local/share/uffs/key.bin`
#[cfg(not(target_os = "macos"))]
fn key_file_path() -> io::Result<std::path::PathBuf> {
    let base = dirs_next::data_local_dir().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "cannot determine local data directory",
        )
    })?;
    Ok(base.join("uffs").join("key.bin"))
}

// ────────────────────────────────────────────────────────────────────────────
// Shared helpers
// ────────────────────────────────────────────────────────────────────────────

/// Generates a random 256-bit key using the OS CSPRNG.
fn generate_key() -> io::Result<[u8; KEY_SIZE]> {
    use rand::Rng;

    let mut key = [0u8; KEY_SIZE];
    rand::rng().fill_bytes(&mut key);
    Ok(key)
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// S2.2.6: key round-trip — generate, store, retrieve, compare.
    #[test]
    fn key_round_trip() {
        let key1 = get_cache_key().expect("first get_cache_key");
        let key2 = get_cache_key().expect("second get_cache_key");
        assert_eq!(key1, key2, "key should be stable across calls");
    }

    /// Verify generated key is non-zero (not all zeros).
    #[test]
    fn key_is_nonzero() {
        let key = get_cache_key().expect("get_cache_key");
        assert_ne!(key, [0u8; KEY_SIZE], "key should not be all zeros");
    }
}
