# UFFS Cache Security Analysis — Data-at-Rest Protection

> **Status**: Design — RFC  
> **Date**: 2026-03-26  
> **Scope**: Protecting cached MFT index data against malware exfiltration  
> **Author**: Security architecture review

---

## Executive Summary

UFFS caches a parsed MFT index to disk (`.uffs` files) for fast warm-start.
This cache contains a **complete filesystem directory listing** — every filename,
full path, file size, timestamps, and NTFS attributes for every file on a
volume. For a typical 7-drive Windows workstation, this is **25.9 million
records** covering the entire file tree.

This data is a **high-value intelligence target for malware**. An attacker who
exfiltrates the cache learns the exact location of every file on the system —
credentials, crypto wallets, tax documents, source code, browser profiles —
without ever touching those files and without triggering file-access-based
detection heuristics.

Currently, the cache is written as **unencrypted plaintext** to the **system
temp directory** with **default OS permissions**. This document proposes a
layered defense strategy that provides strong protection against malware
exfiltration while maintaining UFFS's core performance characteristics.

**Key constraint**: Encryption must add **<100ms** to cache load and **<200ms**
to cache save for a typical 500 MB cache file. The analysis shows this is
achievable with AES-256-GCM using hardware acceleration (AES-NI / ARM Crypto
Extensions), which delivers 4–8 GB/s throughput on modern hardware.

---

## 1. Threat Model

### 1.1 What We're Protecting

The `.uffs` cache file contains:

| Data | Sensitivity | Why It Matters |
|------|-------------|----------------|
| **Every filename on the volume** | HIGH | Reveals existence of sensitive files (tax returns, crypto keys, medical records, classified projects) |
| **Full directory paths** | HIGH | Reveals organizational structure, usernames, project names, server shares |
| **File sizes** | MEDIUM | Aids identification of specific known files (hash-free fingerprinting) |
| **Timestamps** (created, modified, accessed) | MEDIUM | Activity timeline, pattern-of-life analysis |
| **NTFS attributes** (hidden, encrypted, compressed) | MEDIUM | Reveals which files user considers sensitive (encrypted flag = "look here") |
| **Descendant counts / treesize** | LOW | Directory structure heuristics |
| **Volume serial, USN journal checkpoint** | LOW | System fingerprinting |
| **Alternate Data Streams (ADS)** | MEDIUM | Forensic metadata, zone identifiers, downloaded-from URLs |

A single cache file is a **filesystem census** — equivalent to running
`dir /s /a /q` on an entire volume and saving the output. It is more
comprehensive than what most forensic tools capture in a triage image.

### 1.2 Threat Actors

| Actor | Capability | Goal |
|-------|-----------|------|
| **Info-stealer malware** (Redline, Raccoon, Vidar, Lumma) | User-level file read | Exfiltrate filesystem map to identify and locate high-value files for subsequent theft |
| **Ransomware reconnaissance** | User-level file read | Enumerate all files to maximize encryption coverage; identify backup locations |
| **APT / nation-state** | User-level or elevated | Long-term surveillance, pattern-of-life analysis from timestamp data |
| **Lateral movement tools** | Network + user-level | Identify sensitive files on compromised workstations across an enterprise |
| **Insider threat** | Physical or remote access | Exfiltrate directory listing for competitive intelligence or data theft planning |

### 1.3 What We're NOT Protecting Against

| Threat | Why Not | Mitigation Owner |
|--------|---------|-----------------|
| **Memory forensics** (dump process memory) | Requires elevated access; OS-level protection (PPL, VBS) | OS / endpoint security |
| **Kernel-level rootkits** | Game over — rootkit can read anything | OS / endpoint security |
| **Physical disk forensics** (boot from USB) | Full-disk encryption (BitLocker/FileVault) handles this | User / IT policy |
| **Legitimate admin access** | The user controls the system — this is by design | N/A |
| **DMA attacks** (Thunderbolt, PCIe) | Hardware-level; requires physical access | Firmware / IOMMU |

### 1.4 Attack Surface Summary

```
Current attack surface:

  Malware (user-level)
       │
       ├─→ Read %TEMP%\uffs_index_cache\C_index.uffs  ← UNPROTECTED
       │     (plaintext binary, standard file permissions)
       │
       ├─→ Read %TEMP%\uffs_index_cache\D_index.uffs  ← UNPROTECTED
       │
       └─→ Copy entire directory (~2-3 GB total)       ← TRIVIAL
             Upload to C2 server
             Parse offline at leisure

  Proposed attack surface (with all mitigations):

  Malware (user-level)
       │
       ├─→ Read encrypted .uffs files                  ← USELESS (AES-256-GCM)
       │     Cannot decrypt without DPAPI/Keychain key
       │
       ├─→ Access private directory                    ← BLOCKED (restrictive ACLs)
       │     Owner-only permissions
       │
       ├─→ Tamper with cache files                     ← DETECTED (GCM auth tag)
       │     UFFS refuses to load, rebuilds from MFT
       │
       └─→ Read stale/deleted cache                    ← GONE (secure wipe on TTL)
             Zeroed before unlink
```

---

## 2. Current Vulnerabilities

### V1: No Encryption — Plaintext Binary on Disk

**Location**: `crates/uffs-mft/src/index/storage/file_io.rs`

```rust
// Current: plain write
let data = self.serialize(volume_serial, usn_journal_id, next_usn);
let mut file = std::fs::File::create(path)?;
file.write_all(&data)?;

// Current: plain read
let data = std::fs::read(path)?;
let (index, header) = Self::deserialize(&data)?;
```

The serialized data is a structured binary blob starting with magic bytes
`UFFSIDX\0`. Any process with read access to the file can parse it. The
format is documented in source code and trivially reverse-engineerable from
the magic bytes alone.

**Risk**: HIGH — any user-level malware can read and exfiltrate.

### V2: Cache in System Temp Directory

**Location**: `crates/uffs-mft/src/cache.rs`

```rust
const CACHE_DIR_NAME: &str = "uffs_index_cache";

pub fn cache_dir() -> PathBuf {
    std::env::temp_dir().join(CACHE_DIR_NAME)
}
```

The system temp directory (`%TEMP%` on Windows, `/tmp` on Unix) has relaxed
permissions. On Windows, `%TEMP%` is typically `C:\Users\<user>\AppData\Local\Temp`
which is user-private, but:
- Other processes running as the same user have full access
- The directory is a well-known location that malware specifically targets
- No additional ACLs restrict access beyond default user permissions

**Risk**: MEDIUM-HIGH — well-known location, standard permissions.

### V3: No Integrity Verification

The cache file has a magic number (`UFFSIDX\0`) and version check, but **no
cryptographic integrity verification**. An attacker could:

1. **Modify the cache** to hide files (remove entries for malware artifacts)
2. **Inject fake entries** to mislead forensic analysis
3. **Corrupt selectively** to force a full MFT re-read (timing attack for
   side-channel observation of MFT access patterns)

**Risk**: MEDIUM — cache tampering could aid anti-forensic operations.

### V4: No Secure Deletion

```rust
pub fn remove_cached_index(drive: char) {
    let path = cache_file_path(drive);
    drop(std::fs::remove_file(path));  // Just unlinks — data remains on disk
}
```

Standard `remove_file` only removes the directory entry. The actual data
blocks remain on disk until overwritten by other files. Forensic recovery
tools (or malware with raw disk access) can recover deleted cache files.

**Risk**: LOW-MEDIUM — requires elevated access for raw disk reads, but data
persists indefinitely on SSDs (wear leveling complicates overwrite).

### V5: No File Locking During Write

The cache write is not atomic and has no exclusive locking. A race condition
exists where malware could read a partially-written cache, or where concurrent
UFFS instances could corrupt each other's writes.

**Risk**: LOW — primarily a correctness issue, but partial writes could leak
data in predictable patterns.

---

## 3. Proposed Security Architecture

### Design Principles

1. **Zero user friction — the user never knows it's there.** No prompts, no
   configuration, no passwords, no key management, no opt-in. Security is
   enabled by default on first run and requires zero interaction from the user.
   The encryption key is generated automatically, stored in the OS secure
   vault (DPAPI / Keychain / Secret Service), and retrieved transparently.
   The user's only observable cost is ~80ms of additional cache load time —
   imperceptible in practice. **If the user has to do anything for security
   to work, we have failed.**
2. **Self-contained key management** — keys are generated, stored, and
   retrieved entirely within platform-native secure storage. No config files,
   no environment variables, no master passwords, no key files on disk. The
   OS login credential IS the key (via DPAPI on Windows, Keychain on macOS).
   Key loss is not catastrophic — UFFS rebuilds from the MFT source.
3. **Defense in depth** — multiple independent layers, each valuable alone
4. **Near-zero performance cost** — encryption overhead must be invisible
   relative to existing I/O cost (~80ms on 350ms = unnoticeable)
5. **Transparent to application code** — encrypt/decrypt at the I/O boundary;
   all business logic works on plaintext in memory
6. **Fail-secure** — if decryption fails, rebuild from source (MFT); never
   fall back to unencrypted. The user sees a brief "rebuilding index…" and
   continues working. No error dialogs, no manual recovery steps.
7. **Incremental adoption** — each layer can be implemented independently

### Layer Overview

```
┌────────────────────────────────────────────────────────────┐
│ Layer 4: Secure Lifecycle                                   │
│   • Secure wipe on TTL expiry                              │
│   • Atomic writes (write-to-temp + rename)                 │
│   • File locking during write                              │
├────────────────────────────────────────────────────────────┤
│ Layer 3: Platform Access Control                            │
│   • Private directory (owner-only ACL)                     │
│   • Move out of %TEMP% to app-specific data dir            │
│   • Windows: explicit DACL, deny non-owner                 │
├────────────────────────────────────────────────────────────┤
│ Layer 2: Cryptographic Integrity                            │
│   • AES-256-GCM authentication tag (built into encryption) │
│   • Covers header + all data sections                      │
│   • Reject on tag mismatch → rebuild from MFT              │
├────────────────────────────────────────────────────────────┤
│ Layer 1: Encryption at Rest                                 │
│   • AES-256-GCM (authenticated encryption)                 │
│   • Unique nonce per write                                 │
│   • Key from platform secure storage (DPAPI / Keychain)    │
└────────────────────────────────────────────────────────────┘
```

---

## 4. Layer 1: Encryption at Rest

### 4.1 Algorithm Selection

| Algorithm | Mode | Key Size | Auth | HW Accel | Throughput (AES-NI) | Verdict |
|-----------|------|----------|------|----------|--------------------| --------|
| **AES-256-GCM** | AEAD | 256-bit | Built-in | AES-NI, ARM CE | **4–8 GB/s** | ✅ **Selected** |
| AES-256-CTR + HMAC-SHA256 | Encrypt-then-MAC | 256+256 | Separate HMAC | AES-NI | 3–6 GB/s | ❌ Two-pass, more complex |
| ChaCha20-Poly1305 | AEAD | 256-bit | Built-in | None (software) | 1–2 GB/s | ❌ 3–4× slower without dedicated HW |
| XChaCha20-Poly1305 | AEAD | 256-bit | Built-in | None | 1–2 GB/s | ❌ Same perf issue |

**AES-256-GCM** is the clear winner:
- **Hardware acceleration** on every modern x86 (AES-NI, since ~2010) and ARM
  (Crypto Extensions, since ARMv8/A64) CPU
- **Single-pass** authenticated encryption — encrypt + integrity in one
  operation
- **4–8 GB/s** throughput means a 500 MB cache file encrypts in **60–125ms**
- NIST-approved, FIPS 140-2/3 validated, universally accepted
- Rust ecosystem has mature, audited implementations

### 4.2 Performance Budget

```
Current cache save (500 MB, NVMe):
  Serialize:    ~150ms  (CPU-bound, in-memory)
  Write to disk: ~200ms  (NVMe sequential write)
  Total:         ~350ms

With AES-256-GCM encryption:
  Serialize:    ~150ms  (unchanged)
  Encrypt:       ~80ms  (500 MB @ 6 GB/s, AES-NI)
  Write to disk: ~200ms  (unchanged — same bytes, plus 28 bytes overhead)
  Total:         ~430ms  (+80ms, +23%)

Current cache load (500 MB, NVMe):
  Read from disk: ~150ms  (NVMe sequential read)
  Deserialize:    ~200ms  (CPU-bound, parsing)
  Total:           ~350ms

With AES-256-GCM decryption:
  Read from disk: ~150ms  (unchanged)
  Decrypt:         ~80ms  (500 MB @ 6 GB/s, AES-NI)
  Deserialize:    ~200ms  (unchanged)
  Total:           ~430ms  (+80ms, +23%)
```

**+80ms on a 350ms operation = imperceptible to the user.** The TUI still
loads in ~5s for 7 drives; the CLI still loads in <1s from cache. This is
well within the <100ms budget for cache load.

For HDD-backed systems (200–400 MB/s), the disk I/O dominates:

```
HDD cache load (500 MB):
  Read from disk: ~1500ms  (HDD sequential)
  Decrypt:          ~80ms  (still AES-NI speed)
  Deserialize:     ~200ms
  Total:           ~1780ms  (+80ms is <5% overhead)
```

### 4.3 Encryption Format

```
┌─────────────────────────────────────────────────────────────┐
│ Encrypted .uffs File Format                                  │
│                                                              │
│  Offset  Size    Field                                       │
│  ──────  ──────  ────────────────────────────────────────    │
│  0       8       Magic: "UFFSENC\0"                          │
│  8       2       Encryption format version (u16 LE)          │
│  10      1       Algorithm ID (0x01 = AES-256-GCM)           │
│  11      1       KDF ID (0x01=DPAPI, 0x02=Keychain,          │
│                           0x03=Secret Service, 0x04=Raw)     │
│  12      12      Nonce (96-bit, random per write)            │
│  24      4       Plaintext length (u32 LE, for pre-alloc)    │
│  28      N       Ciphertext (encrypted UFFSIDX payload)      │
│  28+N    16      GCM Authentication Tag                      │
│                                                              │
│  Total overhead: 44 bytes (negligible vs 500 MB payload)     │
└─────────────────────────────────────────────────────────────┘
```

**Associated Authenticated Data (AAD)**: The first 28 bytes (magic through
plaintext length) are included as AAD in the GCM computation. This means
tampering with the header (e.g., changing the algorithm ID or nonce) is
detected by the authentication tag, even though the header itself is not
encrypted.

**Nonce handling**: A fresh 96-bit random nonce is generated for every write
using a CSPRNG (`OsRng` / `getrandom`). Since each cache file is a complete
rewrite (never appended to), nonce reuse is impossible as long as the CSPRNG
is functional. The nonce is stored in the clear — this is safe for GCM as
long as it is unique per key+message pair.

### 4.4 Key Management — Platform Secure Storage

The encryption key must **never touch the filesystem** in plaintext. Each
platform provides a hardware-backed or OS-protected key storage facility:

#### Windows: DPAPI (Data Protection API)

```
UFFS generates a random 256-bit key
    │
    └─→ CryptProtectData(key, entropy="uffs-cache-v1")
            │
            └─→ Returns encrypted blob (protected by user's login credentials)
                  │
                  └─→ Store blob in registry: HKCU\Software\UFFS\CacheKey
                       (or %LOCALAPPDATA%\uffs\key.dpapi)

To decrypt:
    Read blob from registry
    │
    └─→ CryptUnprotectData(blob, entropy="uffs-cache-v1")
            │
            └─→ Returns 256-bit key (only succeeds for the logged-in user)
```

**Properties**:
- Key is bound to the Windows user account (derived from login password)
- Cannot be decrypted by other users, even administrators (without impersonation)
- Survives reboots (tied to credential, not session)
- If user changes password via proper Windows UI, DPAPI keys migrate automatically
- If password is forcefully reset (not changed), DPAPI keys are lost → cache
  rebuild from MFT (acceptable — fail-secure)
- ~0.5ms per protect/unprotect call (negligible)

**DPAPI is the gold standard for Windows user-level secret storage.** Used by
Chrome, Edge, Firefox, and virtually all credential managers.

#### macOS: Keychain Services

```
UFFS generates a random 256-bit key
    │
    └─→ SecItemAdd(kSecClassGenericPassword,
                   kSecAttrService="com.uffs.cache",
                   kSecAttrAccount="encryption-key-v1",
                   kSecValueData=key)
            │
            └─→ Key stored in login keychain
                  (encrypted by Keychain master key, backed by Secure Enclave on Apple Silicon)

To retrieve:
    SecItemCopyMatching(service, account)
    │
    └─→ Returns key (only for the current user, may prompt for keychain password)
```

**Properties**:
- Protected by macOS Keychain (hardware-backed on Apple Silicon via SEP)
- Access control: only the UFFS binary (code-signed) can retrieve without prompt
- Survives reboots
- Keychain is unlocked at login; no additional user interaction needed
- If keychain is locked/corrupted → cache rebuild from source (fail-secure)

#### Linux: Secret Service API (GNOME Keyring / KDE Wallet)

```
UFFS generates a random 256-bit key
    │
    └─→ D-Bus: org.freedesktop.secrets.CreateItem(
                   collection="default",
                   label="UFFS Cache Encryption Key",
                   secret=key)

Fallback (headless / no desktop):
    └─→ Derive key from machine-id + user-id via HKDF
         (weaker — but Linux MFT use case is offline files only)
```

#### Key Rotation

The encryption key is **long-lived** (created once, used until lost). This is
acceptable because:

1. Each cache file uses a unique nonce → unique keystream per file
2. Cache files are transient (10-minute TTL) — key material protects
   short-lived data
3. Key loss is not catastrophic — UFFS rebuilds from MFT source
4. Rotation can be triggered manually: `uffs --rotate-cache-key`

### 4.5 Rust Implementation Strategy

**Recommended crate**: `aes-gcm` from RustCrypto

```toml
# Cargo.toml additions
aes-gcm = "0.10"          # AES-256-GCM AEAD (uses aes-ni when available)
rand = "0.8"              # CSPRNG for nonce generation
```

Platform key storage:
```toml
[target.'cfg(windows)'.dependencies]
windows = { version = "0.58", features = ["Win32_Security_Cryptography"] }

[target.'cfg(target_os = "macos")'.dependencies]
security-framework = "2.11"    # Keychain Services bindings

[target.'cfg(target_os = "linux")'.dependencies]
secret-service = "4.0"         # D-Bus Secret Service API
```

**Encryption/decryption is a thin wrapper** at the `file_io.rs` boundary:

```rust
// Pseudocode — encrypt-on-save
pub fn save_to_file_encrypted(
    &self,
    path: &Path,
    volume_serial: u64,
    usn_journal_id: u64,
    next_usn: i64,
) -> io::Result<()> {
    let plaintext = self.serialize(volume_serial, usn_journal_id, next_usn);
    let key = platform::get_or_create_cache_key()?;
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let cipher = Aes256Gcm::new(&key);
    
    let mut file_data = Vec::with_capacity(44 + plaintext.len());
    // Write encrypted header (magic, version, algo, kdf, nonce, plaintext_len)
    file_data.extend_from_slice(b"UFFSENC\0");
    file_data.extend_from_slice(&1u16.to_le_bytes());   // enc version
    file_data.push(0x01);                                // AES-256-GCM
    file_data.push(platform::kdf_id());                  // platform KDF
    file_data.extend_from_slice(nonce.as_slice());       // 12 bytes
    file_data.extend_from_slice(&(plaintext.len() as u32).to_le_bytes());
    
    // AAD = the 28-byte header we just wrote
    let aad = &file_data[..28];
    let ciphertext = cipher.encrypt_with_aad(&nonce, aad, &plaintext)?;
    file_data.extend_from_slice(&ciphertext); // includes GCM tag (appended)
    
    // Atomic write: temp file + rename
    let tmp = path.with_extension("uffs.tmp");
    fs::write(&tmp, &file_data)?;
    fs::rename(&tmp, path)?;
    
    Ok(())
}
```

### 4.6 Backward Compatibility

The encrypted format uses a **different magic** (`UFFSENC\0` vs `UFFSIDX\0`).
On load:

1. Read first 8 bytes
2. If `UFFSIDX\0` → legacy unencrypted format → load normally, then
   **re-save as encrypted** (one-time migration)
3. If `UFFSENC\0` → decrypt, then deserialize as normal
4. If neither → reject as corrupted

This provides seamless upgrade with zero user intervention.

---

## 5. Layer 2: Cryptographic Integrity

AES-256-GCM provides **authenticated encryption** — integrity verification is
built into the algorithm. The 128-bit GCM authentication tag covers both the
ciphertext and the AAD (header).

On decryption, if the tag verification fails, the `aes-gcm` crate returns
`Err` and does not output any plaintext. This is a hard guarantee — there is
no way to get corrupted or tampered data.

**Fail-secure behavior**: On any decryption/integrity failure:
1. Log a warning: "Cache integrity check failed — rebuilding from MFT"
2. Delete the corrupted cache file (secure wipe)
3. Rebuild from MFT source (live MFT on Windows, offline files on Mac/Linux)
4. Save new encrypted cache

This eliminates the anti-forensic cache-tampering vector entirely.

---

## 6. Layer 3: Platform Access Control

### 6.1 Move Cache Out of Temp Directory

The system temp directory is a shared, well-known location. Move the cache to
an application-specific data directory:

| Platform | Current Location | Proposed Location |
|----------|-----------------|-------------------|
| **Windows** | `%TEMP%\uffs_index_cache\` | `%LOCALAPPDATA%\uffs\cache\` |
| **macOS** | `/tmp/uffs_index_cache/` | `~/Library/Caches/com.uffs/` |
| **Linux** | `/tmp/uffs_index_cache/` | `$XDG_CACHE_HOME/uffs/` (default: `~/.cache/uffs/`) |

**Benefits**:
- Not in a well-known malware-scanning location
- Under user's home directory → inherits home directory permissions
- Survives temp cleanup (Windows Disk Cleanup, `systemd-tmpfiles`, etc.)
- Follows platform conventions (XDG, Apple HIG)

### 6.2 Restrictive Directory Permissions

Create the cache directory with owner-only permissions:

```rust
// Unix (macOS / Linux): mode 0700 (owner: rwx, group: ---, other: ---)
fn create_secure_cache_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

// Windows: explicit DACL with owner-only full control
// Deny access to BUILTIN\Users, allow only the current SID
fn create_secure_cache_dir_windows(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    // Use Windows API to set explicit DACL:
    // - GENERIC_ALL for current user SID
    // - No inherited ACEs
    // - Deny all for Everyone (explicit deny)
    set_owner_only_dacl(path)?;
    Ok(())
}
```

**Note on Windows ACLs**: Default `%LOCALAPPDATA%` permissions allow the user
and SYSTEM. Setting an explicit DACL that removes inherited ACEs and grants
only the current user SID provides the strongest possible user-level isolation.
Malware running as a different user (or a compromised service account) cannot
access the directory.

**Limitation**: Malware running as the **same user** can still access the
files. This is why encryption (Layer 1) is the primary defense — ACLs are
defense-in-depth.

### 6.3 File Permissions on Cache Files

Individual cache files should also be created with restrictive permissions:

```rust
// Unix: mode 0600 (owner: rw-, group: ---, other: ---)
// Windows: inherit from directory DACL (owner-only)
```

---

## 7. Layer 4: Secure Lifecycle

### 7.1 Secure Deletion

When cache files expire (TTL) or are explicitly removed, overwrite before
unlinking:

```rust
fn secure_remove(path: &Path) -> io::Result<()> {
    if let Ok(metadata) = fs::metadata(path) {
        let size = metadata.len();
        if let Ok(mut file) = fs::OpenOptions::new().write(true).open(path) {
            // Single-pass zero overwrite
            // (sufficient for SSDs — wear leveling makes multi-pass pointless)
            let zeros = vec![0u8; 65536]; // 64 KB buffer
            let mut remaining = size;
            while remaining > 0 {
                let chunk = remaining.min(65536);
                file.write_all(&zeros[..chunk as usize])?;
                remaining -= chunk;
            }
            file.sync_all()?; // Ensure zeros hit disk
        }
    }
    fs::remove_file(path)
}
```

**SSD reality check**: On SSDs with wear leveling, overwriting does not
guarantee the original data blocks are erased — the SSD controller may remap
the LBA to a new physical page while the old page enters the garbage collection
pool. For SSDs, the only true guarantee is:
- **TRIM** (signals the SSD to erase the blocks) — but not available via
  standard file APIs
- **Full-disk encryption** (BitLocker/FileVault) — makes recovered blocks
  unreadable

Our zero-overwrite is therefore a **best-effort** measure that:
- Eliminates recovery via filesystem-level tools (recuva, photorec)
- Eliminates recovery on HDDs (which have no wear leveling)
- Provides defense-in-depth on SSDs (combined with encryption making raw
  blocks useless)

### 7.2 Atomic Writes

Prevent partial writes from exposing data:

```rust
fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("uffs.tmp");
    
    // Write to temp file
    let mut file = fs::File::create(&tmp)?;
    file.write_all(data)?;
    file.sync_all()?;  // Ensure data hits persistent storage
    
    // Atomic rename (POSIX guarantee: rename is atomic)
    fs::rename(&tmp, path)?;
    
    Ok(())
}
```

On Windows, `rename` is not atomic if the target exists. Use
`MoveFileEx(MOVEFILE_REPLACE_EXISTING)` or `ReplaceFile()` for atomic
semantics.

### 7.3 File Locking

Prevent concurrent access during writes:

```rust
// Use advisory locks (flock on Unix, LockFileEx on Windows)
fn write_with_lock(path: &Path, data: &[u8]) -> io::Result<()> {
    let lock_path = path.with_extension("lock");
    let lock_file = fs::File::create(&lock_path)?;
    lock_exclusive(&lock_file)?;  // Block until lock acquired
    
    atomic_write(path, data)?;
    
    unlock(&lock_file)?;
    drop(lock_file);
    fs::remove_file(&lock_path).ok();
    Ok(())
}
```

---

## 8. Daemon IPC Security — Deep Dive

The planned daemon architecture (see `DAEMON_SERVICE_ARCHITECTURE.md`)
introduces **inter-process communication** between independent OS processes.
This section provides a rigorous security analysis of every IPC surface,
whether encryption is warranted, and what mitigations are actually needed.

### 8.1 IPC Surface Inventory

The daemon architecture has **four distinct IPC channels**, each with a
different trust model:

```
┌─────────────────────────────────────────────────────────────────────┐
│                     IPC Surface Map                                  │
│                                                                     │
│  ① Client → Daemon (primary data channel)                          │
│     CLI ──┐                                                         │
│     TUI ──┼── uffs-client ──→ Unix socket / Named pipe ──→ Daemon  │
│     GUI ──┤                   (JSON-RPC 2.0)                        │
│     MCP ──┘                                                         │
│                                                                     │
│  ② AI Agent → uffs-mcp (MCP adapter)                               │
│     Claude/Cursor/Windsurf ──→ stdin/stdout ──→ uffs-mcp           │
│                                 (MCP JSON-RPC)                      │
│                                                                     │
│  ③ Daemon → Access Broker (Windows only, optional)                 │
│     uffs-daemon ──→ Named pipe ──→ uffs-broker (SYSTEM service)    │
│                     (handle request/response)                       │
│                                                                     │
│  ④ HTTP/SSE remote transport (Phase 6, future)                     │
│     Remote client ──→ TCP/TLS ──→ Daemon HTTP listener             │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### 8.2 Channel-by-Channel Threat Analysis

#### Channel ①: Client → Daemon (Unix Socket / Named Pipe)

This is the primary data channel. **All search results flow through here.**

| Property | Unix Domain Socket (Mac/Linux) | Named Pipe (Windows) |
|----------|-------------------------------|---------------------|
| **Transport** | Kernel-mediated, in-memory | Kernel-mediated, in-memory |
| **Network exposure** | None — never touches network stack | None — local only |
| **Sniffing possible?** | No (`tcpdump`/Wireshark cannot see it) | No (no network) |
| **Man-in-the-middle?** | No (kernel routes directly) | No (kernel routes directly) |
| **Access control** | File permissions on socket file | Security Descriptor / DACL on pipe |
| **Peer identification** | `SO_PEERCRED` (Linux), `getpeereid()` (macOS/BSD) | `GetNamedPipeClientProcessId()` |
| **Data persistence** | Never hits disk — kernel buffers only | Never hits disk |

**Data sensitivity on this channel**:

| Message Type | Direction | Sensitivity | Example |
|-------------|-----------|-------------|---------|
| `search` request | Client → Daemon | LOW | `{"pattern": "*.rs", "files_only": true}` |
| `search` response | Daemon → Client | **HIGH** | Full filenames, paths, sizes, timestamps |
| `drives` / `status` | Both | LOW | Drive letters, record counts, memory usage |
| `info` response | Daemon → Client | **HIGH** | All 25 columns for a specific file |
| `refresh` / `keepalive` / `shutdown` | Client → Daemon | NONE | Control messages |

**The critical question: Does this channel need encryption?**

**Verdict: NO.** Here is the rigorous reasoning:

1. **The data never leaves the kernel.** Unix domain sockets and named pipes
   are kernel-to-kernel data transfers. The bytes move from one process's
   address space to another via kernel buffers. They are never serialized to
   disk, never transmitted over a network interface, and never visible to
   packet-capture tools. There is literally no wire to tap.

2. **The security boundary is "who can connect," not "what's in the
   messages."** If an attacker can connect to the daemon socket, they can
   issue their own `search` queries and get the same data. Encrypting the
   transport doesn't prevent this — the attacker would just query for
   `"pattern": "*"` and get everything. **Access control on the socket IS the
   security mechanism.**

3. **Same-user equivalence.** Any process running as the current user has the
   same filesystem privileges as UFFS itself. On Windows, such a process
   could open the MFT directly (if elevated) or read the `.uffs` cache. On
   Mac/Linux, it could read the offline MFT files. Encrypting the IPC channel
   while the attacker has equivalent filesystem access is security theater.

4. **Performance cost is non-zero.** TLS on a local socket adds ~0.5ms per
   round-trip (handshake amortized) and ~5–10% CPU overhead for
   encrypt/decrypt of result payloads. For search-as-you-type in the TUI
   (queries on every keystroke), this overhead is measurable and provides zero
   additional security.

5. **Industry consensus.** No major local IPC system encrypts local channels:

   | Product | Local IPC | Encrypted? | Rationale |
   |---------|----------|-----------|-----------|
   | **Docker daemon** | Unix socket | No | Socket permissions + group |
   | **PostgreSQL** | Unix socket | No | `peer` auth, optional TLS for TCP only |
   | **SSH agent** | Unix socket | No | Socket permissions |
   | **D-Bus** (system/session bus) | Unix socket | No | Policy-based access control |
   | **VS Code Extension Host** | stdin/stdout | No | Parent-child pipe |
   | **1Password CLI ↔ Desktop** | Unix socket | No | Socket permissions |
   | **gpg-agent** | Unix socket | No | Socket permissions |

**What IS needed for Channel ①:**

| Mitigation | Why | How |
|-----------|-----|-----|
| **Socket/pipe permissions** | Prevent cross-user access | Socket: mode `0600`; Pipe: owner-only DACL |
| **Peer credential check** | Verify connecting process runs as same user | `SO_PEERCRED` / `getpeereid()` / `GetNamedPipeClientProcessId()` |
| **Input validation** | Prevent malformed JSON from crashing daemon | Max message size (16 MB), JSON schema validation, timeout on reads |
| **Rate limiting** | Prevent DoS from runaway client | Max concurrent connections (32), per-client query rate limit |

#### Channel ②: AI Agent → uffs-mcp (stdio)

The MCP adapter communicates with the AI agent via `stdin`/`stdout` pipes.

**Security model**: The agent process spawns `uffs-mcp` as a child process.
The pipes are created by the OS and are exclusive to the parent-child
relationship. No other process can read from or write to these pipes (without
`ptrace` / debugging privileges, which require elevation or explicit
permission).

**Verdict: NO additional protection needed.**

This channel is **inherently isolated** by the OS process model. It is the
most secure IPC mechanism possible — more secure than sockets or named pipes,
because there is no filesystem artifact (socket file, pipe name) that could be
targeted. The only way to intercept this channel is to debug one of the two
processes, which requires elevated privileges.

**Note**: The MCP JSON-RPC protocol itself has no authentication. This is by
design — MCP trusts the stdio transport. The agent and the tool run in the
same security context.

#### Channel ③: Daemon → Access Broker (Windows Named Pipe)

This channel has a **different trust model** than Channel ①. The Access
Broker runs as `SYSTEM` (elevated) and provides privileged volume handles to
the daemon. A rogue process impersonating the daemon could steal elevated
handles.

```
Threat: Rogue process connects to Access Broker
    │
    └─→ Requests elevated volume handle for C:
         └─→ Gets handle with SeBackupPrivilege
              └─→ Can read raw MFT (and any file on the volume)
```

This is a **privilege escalation** vector — a non-elevated process obtains
elevated capabilities by connecting to the broker.

**Verdict: YES — this channel needs authentication.**

| Mitigation | Why | How |
|-----------|-----|-----|
| **Pipe DACL** | Restrict who can connect to broker | Allow only BUILTIN\Administrators + the specific daemon SID |
| **Client process verification** | Ensure connecting process is legitimate UFFS daemon | `GetNamedPipeClientProcessId()` → verify executable path matches signed UFFS binary |
| **Code signing validation** | Prevent renamed/modified binaries | Verify Authenticode signature on the daemon's executable before issuing handles |
| **Request scope limiting** | Minimize blast radius | Broker only issues read-only volume handles; never file handles, never write access |
| **Audit logging** | Detect abuse | Log every handle request with client PID, executable path, and timestamp |

**Implementation detail**: The broker should call
`GetNamedPipeClientProcessId()` to get the connecting PID, then
`QueryFullProcessImageNameW()` to get the executable path, then verify the
Authenticode signature matches the expected UFFS publisher certificate. This
is the same pattern used by anti-cheat systems (EasyAntiCheat, BattlEye) and
credential managers (1Password) for privileged broker processes.

#### Channel ④: HTTP/SSE Remote Transport (Phase 6)

**This is the only channel where encryption is mandatory.**

HTTP/SSE traffic traverses the network stack — even when bound to
`127.0.0.1`, it is visible to any process that can capture loopback traffic
(e.g., `tcpdump -i lo`, Wireshark, or malware with raw socket access).

**Verdict: MANDATORY TLS + authentication.**

| Mitigation | Why | How |
|-----------|-----|-----|
| **TLS 1.3** | Encrypt data in transit | Self-signed cert (localhost) or user-provided cert (remote) |
| **Authentication token** | Prevent unauthorized access | Random bearer token generated on daemon start, stored in config |
| **Bind to localhost** | Prevent network exposure by default | `127.0.0.1` / `::1` only; require `--bind 0.0.0.0` for remote |
| **mTLS option** | Strong mutual auth for enterprise | Client cert verification for remote access |
| **CORS restrictions** | Prevent browser-based attacks | No `Access-Control-Allow-Origin: *` |

**Localhost TLS note**: Even on localhost, TLS is justified because:
- Loopback traffic CAN be sniffed by privileged processes (unlike Unix sockets)
- HTTP is a text protocol — credentials/tokens in headers are trivially readable
- Browser-based clients (future GUI?) may use HTTPS APIs
- The overhead is negligible for the HTTP use case (queries are less frequent
  than TUI keystroke-driven search)

### 8.3 Daemon Identity Verification (Anti-Impersonation)

A subtle but important threat: **daemon impersonation**. Malware could:

1. Kill the real daemon (or wait for it to auto-retire)
2. Start a fake daemon that listens on the same socket/pipe
3. Write a fake PID file
4. Intercept all client queries and responses

This gives the attacker:
- A live feed of what the user is searching for (behavioral intelligence)
- The ability to modify search results (hide malware artifacts from the user)
- A persistent position even if the cache files are encrypted

**Mitigations:**

```
Client connection flow (proposed):

1. Read PID file (~/.local/share/uffs/daemon.pid)
2. Verify PID is alive (kill -0 / OpenProcess)
3. Verify process executable path matches expected uffs-daemon location
   ├─ macOS: use proc_pidpath() or sysctl(KERN_PROCARGS2)
   ├─ Linux: read /proc/<pid>/exe symlink
   └─ Windows: QueryFullProcessImageNameW()
4. (Optional) Verify code signature on executable
   ├─ macOS: SecCodeCopySigningInformation() — check team ID
   └─ Windows: WinVerifyTrust() — check Authenticode
5. Connect to socket/pipe
6. (Optional) Verify peer PID matches PID file
   ├─ Unix: SO_PEERCRED / getpeereid()
   └─ Windows: GetNamedPipeServerProcessId()
```

**PID file security**:
- Permissions: `0600` (owner-only read/write)
- Content: PID + daemon start timestamp + executable path hash
- Verification: client checks all three fields, not just PID
- Location: same private directory as cache
  (`~/.local/share/uffs/daemon.pid`)

### 8.4 Socket / Pipe File Security

| Asset | Location | Permissions | Rationale |
|-------|----------|------------|-----------|
| **Unix socket** | `~/.local/share/uffs/daemon.sock` | `0600` | Owner-only connect |
| **PID file** | `~/.local/share/uffs/daemon.pid` | `0600` | Owner-only read/write |
| **Windows pipe** | `\\.\pipe\uffs-daemon-{UserSID}` | DACL: owner-only | Per-user pipe name prevents cross-user collision |
| **Socket directory** | `~/.local/share/uffs/` | `0700` | Owner-only traversal |

**Windows pipe naming**: Including the user's SID in the pipe name
(`\\.\pipe\uffs-daemon-S-1-5-21-...`) prevents a multi-user scenario where
two users' daemons collide on the same pipe name. It also makes it harder for
malware running as a different user to even guess the pipe name.

### 8.5 Input Validation & Resource Limits

The daemon accepts arbitrary JSON-RPC from any connected client. Without
validation, a malicious or buggy client could:

| Attack | Impact | Mitigation |
|--------|--------|-----------|
| **Oversized message** (1 GB JSON string) | OOM crash | Max message size: **16 MB** (reject + disconnect) |
| **Malformed JSON** | Panic in parser | Use `serde_json` with size-limited reader; catch all errors |
| **Regex bomb** (`search` with catastrophic backtracking pattern) | CPU hang | Regex compilation timeout (**100ms**); reject complex patterns |
| **Unlimited `limit`** (request 25M results) | OOM building response | Hard cap on `limit`: **100,000 rows** per response |
| **Rapid-fire queries** (1000 queries/sec) | CPU saturation, starve other clients | Per-client rate limit: **100 queries/sec**; global: **500/sec** |
| **Connection flood** (1000 concurrent connections) | FD exhaustion | Max connections: **32** (more than enough for CLI+TUI+GUI+MCP) |
| **Slowloris** (connect but never send) | Connection slot exhaustion | Read timeout: **30 seconds** per message; idle connection timeout: **5 minutes** |
| **`shutdown` from rogue client** | Daemon killed | Require `shutdown` requests to include a nonce from PID file (or disable over IPC entirely — only honor `SIGTERM`) |

### 8.6 Verdict Summary

```
┌────────────────────────────────────────────────────────────────────┐
│                   IPC Security Verdict                              │
│                                                                    │
│  Channel ① Client → Daemon (socket/pipe)                          │
│  ┌──────────────────────────────────────────────────────────┐     │
│  │  Encryption needed?           NO                          │     │
│  │  Reason: kernel-mediated, no network, same-user equiv.   │     │
│  │                                                           │     │
│  │  What IS needed:                                          │     │
│  │   ✓ Socket/pipe permissions (owner-only)                  │     │
│  │   ✓ Peer credential verification                          │     │
│  │   ✓ Input validation + resource limits                    │     │
│  │   ✓ Daemon identity verification by clients               │     │
│  └──────────────────────────────────────────────────────────┘     │
│                                                                    │
│  Channel ② Agent → MCP adapter (stdio)                            │
│  ┌──────────────────────────────────────────────────────────┐     │
│  │  Encryption needed?           NO                          │     │
│  │  Reason: parent-child pipe, OS-isolated, no artifact      │     │
│  │  Additional mitigations:      NONE                        │     │
│  └──────────────────────────────────────────────────────────┘     │
│                                                                    │
│  Channel ③ Daemon → Access Broker (named pipe, Windows)           │
│  ┌──────────────────────────────────────────────────────────┐     │
│  │  Encryption needed?           NO (local pipe)             │     │
│  │  Authentication needed?       YES (privilege escalation)  │     │
│  │                                                           │     │
│  │  What IS needed:                                          │     │
│  │   ✓ Pipe DACL (Administrators + daemon SID only)          │     │
│  │   ✓ Client process verification (PID → exe path → sig)   │     │
│  │   ✓ Read-only handle scope                                │     │
│  │   ✓ Audit logging                                         │     │
│  └──────────────────────────────────────────────────────────┘     │
│                                                                    │
│  Channel ④ HTTP/SSE (Phase 6, remote)                             │
│  ┌──────────────────────────────────────────────────────────┐     │
│  │  Encryption needed?           YES — MANDATORY TLS 1.3     │     │
│  │  Authentication needed?       YES — bearer token or mTLS  │     │
│  │  Bind to localhost by default                             │     │
│  └──────────────────────────────────────────────────────────┘     │
│                                                                    │
│  BOTTOM LINE:                                                      │
│  Local IPC does NOT need encryption. The OS kernel provides the    │
│  isolation. Invest in access control, identity verification, and   │
│  input validation instead — these address the actual threats.      │
│  Reserve TLS for the network transport (Phase 6).                  │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

---

## 9. Implementation Roadmap

### Phase S1: Quick Wins (1–2 days)

These can be implemented immediately with no new dependencies:

| Task | Impact | Effort |
|------|--------|--------|
| Move cache dir out of `%TEMP%` to `%LOCALAPPDATA%\uffs\cache\` (Win) / `~/Library/Caches/com.uffs/` (Mac) | Reduces discoverability | ~30 min |
| Set directory permissions to owner-only (0700 / explicit DACL) | Blocks cross-user access | ~1 hour |
| Set file permissions to 0600 on cache files | Blocks cross-user read | ~30 min |
| Atomic writes (temp + rename) | Prevents partial-write exposure | ~1 hour |
| Backward compat: check old `%TEMP%` location, migrate if found | Seamless upgrade | ~30 min |

### Phase S2: Encryption (2–3 days)

| Task | Impact | Effort |
|------|--------|--------|
| Add `aes-gcm` + `rand` dependencies | — | ~10 min |
| Implement `encrypt_cache()` / `decrypt_cache()` in `file_io.rs` | PRIMARY DEFENSE | ~4 hours |
| Windows: DPAPI key storage (CryptProtectData/Unprotect) | Key management | ~4 hours |
| macOS: Keychain key storage (security-framework) | Key management | ~4 hours |
| Linux: Secret Service + HKDF fallback | Key management | ~4 hours |
| Encrypted file format (UFFSENC header, nonce, GCM tag) | Format design | ~2 hours |
| Backward compat: detect UFFSIDX magic → migrate to encrypted | Seamless upgrade | ~1 hour |
| Unit tests: encrypt/decrypt round-trip, tamper detection, format migration | Quality | ~2 hours |

### Phase S3: Secure Lifecycle (1 day)

| Task | Impact | Effort |
|------|--------|--------|
| Secure wipe on cache removal (zero-overwrite) | Prevents recovery | ~2 hours |
| File locking (advisory locks) | Prevents race conditions | ~2 hours |
| Audit all `fs::remove_file` calls → replace with `secure_remove` | Completeness | ~1 hour |

### Phase S4: Daemon IPC Hardening (with daemon implementation)

| Task | Impact | Effort |
|------|--------|--------|
| Unix socket with mode 0600 | Cross-user isolation | ~30 min |
| Windows named pipe DACL (owner-only, SID-scoped name) | Cross-user isolation | ~2 hours |
| Peer credential check on every connection (`SO_PEERCRED` / `getpeereid`) | Client identity | ~2 hours |
| Daemon identity verification in `uffs-client` (PID → exe path) | Anti-impersonation | ~3 hours |
| PID file: permissions 0600, include exe path hash + start timestamp | Anti-hijack | ~1 hour |
| Input validation: max message size (16 MB), JSON schema check | Crash prevention | ~2 hours |
| Resource limits: max connections (32), rate limit (100 q/s/client) | DoS prevention | ~2 hours |
| Regex compilation timeout (100ms) | CPU bomb prevention | ~1 hour |
| Response limit cap (100K rows) | OOM prevention | ~30 min |
| `shutdown` method: require nonce or restrict to SIGTERM only | Anti-abuse | ~1 hour |

### Phase S5: Access Broker Hardening (with broker implementation, Windows)

| Task | Impact | Effort |
|------|--------|--------|
| Broker pipe DACL (Administrators + daemon SID) | Privilege escalation prevention | ~2 hours |
| Client process verification (PID → exe path → Authenticode sig) | Rogue daemon prevention | ~4 hours |
| Read-only handle scope (never issue write handles) | Blast radius reduction | ~1 hour |
| Audit logging (all handle requests to Windows Event Log) | Detection | ~2 hours |

### Phase S6: Network Transport Security (Phase 6 of daemon roadmap)

| Task | Impact | Effort |
|------|--------|--------|
| TLS 1.3 on HTTP listener (rustls / native-tls) | Mandatory encryption | ~4 hours |
| Auto-generated self-signed cert for localhost | Zero-config local use | ~2 hours |
| Bearer token authentication (generated on daemon start) | Access control | ~2 hours |
| Bind to localhost by default; `--bind` flag for remote | Network exposure control | ~1 hour |
| mTLS option for enterprise/remote use | Strong mutual auth | ~4 hours |
| CORS headers (deny all by default) | Browser attack prevention | ~30 min |

---

## 10. Performance Validation Plan

Before and after measurements for each phase:

```
Benchmark: Cache Save (C: drive, 3.4M records, ~450 MB)
  Metric: wall-clock time from serialize-start to file-closed
  Baseline: current plaintext write
  Target: <200ms overhead

Benchmark: Cache Load (C: drive, 3.4M records, ~450 MB)  
  Metric: wall-clock time from file-open to deserialized MftIndex
  Baseline: current plaintext read
  Target: <100ms overhead

Benchmark: End-to-End TUI Startup (7 drives, all cached)
  Metric: wall-clock time from launch to "ready" state
  Baseline: current ~5s
  Target: <6s (≤20% regression)

Benchmark: IPC Round-Trip Latency (daemon mode)
  Metric: client query → daemon response → client receives
  Baseline: N/A (new)
  Target: <15ms per query (search-as-you-type usable)

Platform matrix:
  - Windows 11, NVMe (x86_64, AES-NI)
  - Windows 11, HDD (x86_64, AES-NI)
  - macOS M4 (ARM, Crypto Extensions)
```

If AES-NI / ARM CE is not available (very old hardware), fall back to software
AES with a warning. The performance impact on software-only AES is ~10×
slower (400–800 MB/s) — still acceptable for a 500 MB cache (~700ms overhead).

**IPC performance note**: No encryption on local IPC means **zero crypto
overhead** on the hot path (search-as-you-type). The only IPC costs are
JSON serialization (~0.5ms) and kernel socket transfer (~0.1ms). This keeps
the daemon query path well under the 15ms budget.

---

## 11. Threat Mitigation Matrix

| Threat | V1 (No Enc) | V2 (Temp Dir) | V3 (No Auth) | V4 (No Wipe) | V5 (No Lock) | V6 (IPC) |
|--------|:-----------:|:-------------:|:------------:|:------------:|:------------:|:--------:|
| **Layer 1**: AES-256-GCM encryption | ✅ FIXED | — | ✅ FIXED (GCM auth) | — | — | — |
| **Layer 2**: GCM integrity tag | — | — | ✅ FIXED | — | — | — |
| **Layer 3**: Private dir + ACLs | — | ✅ FIXED | — | — | — | — |
| **Layer 4**: Secure wipe + atomic write + lock | — | — | — | ✅ FIXED | ✅ FIXED | — |
| **Layer 5**: IPC hardening (permissions, identity, input validation) | — | — | — | — | — | ✅ FIXED |

All six identified vulnerability classes are addressed.

---

## 12. Comparison with Industry Practice

### Data-at-Rest

| Product | MFT/Index Cache | Encryption | Key Storage | Integrity |
|---------|----------------|-----------|-------------|-----------|
| **Everything** (voidtools) | None (in-memory only, or .efu export) | No | N/A | No |
| **WizFile** | In-memory only | N/A | N/A | N/A |
| **Chrome** (history/cookies) | SQLite files | AES-256-GCM | DPAPI (Win), Keychain (Mac) | No |
| **Firefox** (logins.json) | JSON files | AES-256 via NSS | Master password or OS keystore | HMAC |
| **1Password** | SQLite vault | AES-256-GCM | SRP + HKDF | GCM tag |
| **UFFS (proposed)** | Binary `.uffs` files | **AES-256-GCM** | **DPAPI / Keychain / Secret Service** | **GCM tag** |

### Local IPC

| Product | IPC Mechanism | Encrypted? | Authentication |
|---------|-------------|-----------|----------------|
| **Docker daemon** | Unix socket | No | Socket permissions + docker group |
| **PostgreSQL** | Unix socket | No | `peer` auth (UID check) |
| **SSH agent** | Unix socket | No | Socket permissions (0600) |
| **D-Bus** | Unix socket | No | Policy files + UID |
| **gpg-agent** | Unix socket | No | Socket permissions |
| **1Password CLI ↔ Desktop** | Unix socket | No | Socket permissions |
| **VS Code ↔ Extensions** | stdin/stdout | No | Parent-child pipe |
| **UFFS (proposed)** | Unix socket / Named pipe | **No** | **Socket perms + peer cred + daemon identity** |

UFFS's proposed approach matches industry standard for local IPC (no
encryption, strong access control) while exceeding most products on daemon
identity verification (exe path + optional code signature check).

---

## 13. Risk Acceptance Notes

### Risks accepted (by design)

1. **Same-user malware (cache)**: If malware runs as the current user AND has
   the ability to call DPAPI/Keychain, it can decrypt the cache key. This is
   inherent to user-level security — the same malware could also just call
   UFFS or read the MFT directly. Encryption raises the bar from "read a
   file" to "call platform crypto APIs + understand our format."

2. **Same-user malware (IPC)**: Malware running as the current user can
   connect to the daemon socket and issue queries. This is equivalent to the
   malware running UFFS itself — encrypting the IPC channel would not help
   because the malware could just use `uffs-client` directly. The daemon
   treats all same-user connections as trusted. This matches the security
   model of Docker, PostgreSQL, SSH agent, and every other local IPC system.

3. **Memory-resident data**: While UFFS is running, the decrypted index is in
   process memory (~7 GiB). A memory dump would expose it. Mitigation is
   OS-level (PPL, VBS, kernel ASLR) — not our responsibility.

4. **SSD wear-leveling residual**: Deleted cache blocks may persist in SSD
   flash until garbage collection. Full-disk encryption (BitLocker/FileVault)
   is the only complete mitigation. Our zero-overwrite is best-effort.

5. **Key recovery after forced password reset**: Windows DPAPI keys become
   irrecoverable if the user's password is forcefully reset (not changed via
   UI). This means the cache key is lost → cache is rebuilt from MFT. This is
   acceptable (fail-secure).

6. **Unencrypted local IPC data**: Search results (filenames, paths) travel
   in plaintext over local socket/pipe. This is accepted because the kernel
   provides isolation, no network is involved, and any attacker who can read
   the socket can achieve the same result by other means. TLS is reserved for
   the network transport (Phase 6) where the threat model changes.

### Risks NOT accepted (must implement)

1. **Plaintext cache on disk** → Must encrypt (Layer 1)
2. **World-readable cache directory** → Must restrict permissions (Layer 3)
3. **No integrity check** → Must use authenticated encryption (Layer 2)
4. **Unprotected daemon socket** → Must set restrictive permissions + peer
   verification (Layer 5)
5. **Unauthenticated Access Broker** → Must verify client identity before
   issuing elevated handles (Phase S5)
6. **Unencrypted HTTP/SSE** → Must use TLS 1.3 + authentication (Phase S6)

---

## 14. Summary

| Property | Current | Proposed |
|----------|---------|----------|
| **Cache encryption** | None | AES-256-GCM |
| **Key storage** | N/A | DPAPI / Keychain / Secret Service |
| **Integrity check** | Magic bytes only | 128-bit GCM authentication tag |
| **Cache location** | System temp dir | App-specific data dir |
| **Directory permissions** | Default (inherited) | Owner-only (0700 / explicit DACL) |
| **File permissions** | Default | Owner-only (0600) |
| **Deletion** | Simple unlink | Zero-overwrite + unlink |
| **Write safety** | Direct overwrite | Atomic (temp + rename) + file lock |
| **IPC encryption** | N/A | Not needed (kernel-isolated) |
| **IPC access control** | N/A | Socket perms + peer cred + daemon identity |
| **IPC input validation** | N/A | Size limits, rate limits, regex timeout |
| **Access Broker auth** | N/A | PID + exe path + Authenticode signature |
| **HTTP/SSE security** | N/A | TLS 1.3 + bearer token + localhost bind |
| **Performance overhead** | 0 | ~80ms per 500 MB cache (AES-NI); 0 for IPC |
| **Backward compat** | N/A | Auto-detect magic → migrate on first load |

The MFT cache is a **filesystem census** that deserves the same protection as
browser credentials or password vaults. The proposed architecture provides
**defense in depth** through encryption, access control, integrity
verification, and secure lifecycle management — all while maintaining UFFS's
core promise of speed.

Local IPC does not need encryption — the OS kernel already provides the
isolation. The security investment goes where the actual threats are: cache
encryption at rest, access control on sockets/pipes, daemon identity
verification, input validation, and TLS for the network transport.

---

*Document Version: 1.1*  
*Last Updated: 2026-03-26*  
*Classification: Internal — Security Architecture*
