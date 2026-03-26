//! Platform-native secure key storage.
//!
//! Provides [`get_cache_key`] which returns a 256-bit AES key, generating and
//! storing it on first use via the OS-native secure vault:
//!
//! | Platform | Backend | Key Location |
//! |----------|---------|-------------|
//! | **macOS** | Keychain Services | `com.uffs.cache` / `encryption-key-v1` |
//! | **Windows** | DPAPI (`CryptProtectData`) | `%LOCALAPPDATA%/uffs/key.dpapi` |
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
// Windows: DPAPI (CryptProtectData / CryptUnprotectData)
// ────────────────────────────────────────────────────────────────────────────

/// Windows DPAPI key storage.
///
/// The raw 32-byte key is encrypted with `CryptProtectData` using the
/// entropy string `"uffs-cache-v1"`. The encrypted blob is stored at
/// `%LOCALAPPDATA%/uffs/key.dpapi`. Only the same Windows user account
/// can decrypt it via `CryptUnprotectData`.
#[cfg(target_os = "windows")]
fn platform_get_or_create_key() -> io::Result<[u8; KEY_SIZE]> {
    let key_path = dpapi_key_path()?;

    // Try to read and decrypt existing DPAPI blob
    if key_path.exists() {
        match dpapi_read_key(&key_path) {
            Ok(key) => return Ok(key),
            Err(e) => {
                tracing::warn!(
                    path = %key_path.display(),
                    error = %e,
                    "DPAPI decrypt failed, regenerating key"
                );
                let _ignore = std::fs::remove_file(&key_path);
            }
        }
    }

    // Generate new key
    let key = generate_key()?;

    // Ensure parent dir exists with secure permissions
    if let Some(parent) = key_path.parent() {
        crate::fs::create_secure_dir(parent)?;
    }

    // Encrypt with DPAPI and write
    dpapi_write_key(&key_path, &key)?;

    tracing::info!(path = %key_path.display(), "Generated and stored new encryption key (DPAPI)");
    Ok(key)
}

/// DPAPI key file path: `%LOCALAPPDATA%/uffs/key.dpapi`
#[cfg(target_os = "windows")]
fn dpapi_key_path() -> io::Result<std::path::PathBuf> {
    let base = dirs_next::data_local_dir().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "cannot determine %LOCALAPPDATA%")
    })?;
    Ok(base.join("uffs").join("key.dpapi"))
}

/// Entropy string for DPAPI — binds the encrypted blob to this application.
#[cfg(target_os = "windows")]
const DPAPI_ENTROPY: &[u8] = b"uffs-cache-v1";

/// Encrypt a key with DPAPI and write the blob to disk.
#[cfg(target_os = "windows")]
fn dpapi_write_key(path: &std::path::Path, key: &[u8; KEY_SIZE]) -> io::Result<()> {
    let encrypted = dpapi_protect(key)?;
    std::fs::write(path, &encrypted)?;
    crate::fs::set_file_permissions_owner_only(path)?;
    Ok(())
}

/// Read a DPAPI blob from disk and decrypt it to get the key.
#[cfg(target_os = "windows")]
fn dpapi_read_key(path: &std::path::Path) -> io::Result<[u8; KEY_SIZE]> {
    let blob = std::fs::read(path)?;
    let plaintext = dpapi_unprotect(&blob)?;
    if plaintext.len() != KEY_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("DPAPI decrypted key has wrong size: {}", plaintext.len()),
        ));
    }
    let mut key = [0u8; KEY_SIZE];
    key.copy_from_slice(&plaintext);
    Ok(key)
}

/// Call `CryptProtectData` to encrypt data with DPAPI.
#[cfg(target_os = "windows")]
fn dpapi_protect(data: &[u8]) -> io::Result<Vec<u8>> {
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN,
    };
    use windows::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB;

    let mut input_blob = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut entropy_blob = CRYPT_INTEGER_BLOB {
        cbData: DPAPI_ENTROPY.len() as u32,
        pbData: DPAPI_ENTROPY.as_ptr() as *mut u8,
    };
    let mut output_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };

    // SAFETY: CryptProtectData is a well-defined Win32 API.
    #[expect(unsafe_code, reason = "DPAPI requires unsafe FFI")]
    let ok = unsafe {
        CryptProtectData(
            &mut input_blob,
            None,                      // description (optional)
            Some(&mut entropy_blob),   // entropy
            None,                      // reserved
            None,                      // prompt struct
            CRYPTPROTECT_UI_FORBIDDEN, // no UI
            &mut output_blob,
        )
    };

    if !ok.as_bool() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "CryptProtectData failed",
        ));
    }

    // Copy output blob to Vec and free the Windows-allocated memory
    let result = unsafe {
        let slice = std::slice::from_raw_parts(
            output_blob.pbData,
            output_blob.cbData as usize,
        );
        let vec = slice.to_vec();
        windows::Win32::System::Memory::LocalFree(
            windows::Win32::Foundation::HLOCAL(output_blob.pbData as _),
        );
        vec
    };

    Ok(result)
}

/// Call `CryptUnprotectData` to decrypt a DPAPI blob.
#[cfg(target_os = "windows")]
fn dpapi_unprotect(blob: &[u8]) -> io::Result<Vec<u8>> {
    use windows::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN,
    };
    use windows::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB;

    let mut input_blob = CRYPT_INTEGER_BLOB {
        cbData: blob.len() as u32,
        pbData: blob.as_ptr() as *mut u8,
    };
    let mut entropy_blob = CRYPT_INTEGER_BLOB {
        cbData: DPAPI_ENTROPY.len() as u32,
        pbData: DPAPI_ENTROPY.as_ptr() as *mut u8,
    };
    let mut output_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };

    // SAFETY: CryptUnprotectData is a well-defined Win32 API.
    #[expect(unsafe_code, reason = "DPAPI requires unsafe FFI")]
    let ok = unsafe {
        CryptUnprotectData(
            &mut input_blob,
            None,                      // description out
            Some(&mut entropy_blob),   // entropy
            None,                      // reserved
            None,                      // prompt struct
            CRYPTPROTECT_UI_FORBIDDEN, // no UI
            &mut output_blob,
        )
    };

    if !ok.as_bool() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "CryptUnprotectData failed (wrong user or corrupted blob)",
        ));
    }

    let result = unsafe {
        let slice = std::slice::from_raw_parts(
            output_blob.pbData,
            output_blob.cbData as usize,
        );
        let vec = slice.to_vec();
        windows::Win32::System::Memory::LocalFree(
            windows::Win32::Foundation::HLOCAL(output_blob.pbData as _),
        );
        vec
    };

    Ok(result)
}

// ────────────────────────────────────────────────────────────────────────────
// Linux: file-based key with 0600 permissions
// ────────────────────────────────────────────────────────────────────────────

/// Linux file-based key storage.
///
/// The key is a raw 32-byte file stored at `~/.local/share/uffs/key.bin`
/// with owner-only permissions (`0600`).
#[cfg(target_os = "linux")]
fn platform_get_or_create_key() -> io::Result<[u8; KEY_SIZE]> {
    let key_path = linux_key_path()?;

    if key_path.exists() {
        let data = std::fs::read(&key_path)?;
        if data.len() == KEY_SIZE {
            let mut key = [0u8; KEY_SIZE];
            key.copy_from_slice(&data);
            return Ok(key);
        }
        tracing::warn!(
            len = data.len(),
            path = %key_path.display(),
            "Key file has wrong size, regenerating"
        );
    }

    let key = generate_key()?;

    if let Some(parent) = key_path.parent() {
        crate::fs::create_secure_dir(parent)?;
    }

    std::fs::write(&key_path, key)?;
    crate::fs::set_file_permissions_owner_only(&key_path)?;

    tracing::info!(path = %key_path.display(), "Generated and stored new encryption key");
    Ok(key)
}

/// Linux key file path: `~/.local/share/uffs/key.bin`
#[cfg(target_os = "linux")]
fn linux_key_path() -> io::Result<std::path::PathBuf> {
    let base = dirs_next::data_local_dir().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "cannot determine local data dir")
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
