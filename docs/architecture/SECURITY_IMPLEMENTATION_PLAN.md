# UFFS Security Implementation Plan

> **Status**: Active  
> **Date**: 2026-03-26  
> **Reference**: `CACHE_SECURITY_ANALYSIS.md` (threat model & design)  
> **Principle**: Zero user friction — security is invisible, self-contained,
> automatic. The user is never prompted, never configures, never manages keys.

---

## Overview

This document is the **actionable implementation plan** for the security
architecture defined in `CACHE_SECURITY_ANALYSIS.md`. It breaks work into
phases and waves with specific tasks, file paths, function signatures,
acceptance criteria, and a tracking section.

### Phase Map

| Phase | Name | Depends On | Effort | Core Deliverable |
|-------|------|-----------|--------|-----------------|
| **S1** | Secure Foundation | Nothing | 1–2 days | Private cache dir, permissions, atomic writes |
| **S2** | Encryption at Rest | S1 | 2–3 days | AES-256-GCM cache encryption, platform key storage |
| **S3** | Secure Lifecycle | S1 | 1 day | Secure wipe, file locking |
| **S4** | Daemon IPC Hardening | Daemon Phase 1 | 2–3 days | Socket/pipe permissions, identity, input validation |
| **S5** | Access Broker Hardening | Daemon Phase 5 | 1–2 days | Broker auth, code signing verification |
| **S6** | Network Transport | Daemon Phase 6 | 2 days | TLS 1.3, bearer token, mTLS |

S1, S2, S3 are **independent of the daemon** and can start immediately.
S4–S6 are implemented alongside their respective daemon phases.

---

## Phase S1: Secure Foundation

> **Goal**: Move cache out of temp, lock down permissions, atomic writes.  
> **Dependencies**: None  
> **Effort**: 1–2 days  
> **New crates/deps**: None

### Wave S1.1 — Cache Directory Relocation

**What**: Replace `std::env::temp_dir()` with platform-appropriate app data dir.

| Platform | Old Location | New Location |
|----------|-------------|-------------|
| Windows | `%TEMP%\uffs_index_cache\` | `%LOCALAPPDATA%\uffs\cache\` |
| macOS | `/tmp/uffs_index_cache/` | `~/Library/Caches/com.uffs/` |
| Linux | `/tmp/uffs_index_cache/` | `$XDG_CACHE_HOME/uffs/` (`~/.cache/uffs/`) |

#### Tasks

| ID | Task | File | Status |
|----|------|------|--------|
| S1.1.1 | Add `dirs-next` dependency (or reuse existing) for platform dirs | `Cargo.toml` (workspace + uffs-mft) | ✅ DONE (already existed) |
| S1.1.2 | Create `fn secure_cache_dir() -> PathBuf` — returns new platform-specific cache dir | `crates/uffs-mft/src/cache.rs` | ✅ DONE |
| S1.1.3 | Update `cache_dir()` to call `secure_cache_dir()` | `crates/uffs-mft/src/cache.rs` | ✅ DONE |
| S1.1.4 | Add migration: on startup, check old `temp_dir()` location; if `.uffs` files exist, move them to new dir | `crates/uffs-mft/src/cache.rs` (new fn `migrate_legacy_cache()`) | ✅ DONE |
| S1.1.5 | Call `migrate_legacy_cache()` from `cache_dir()` on first call (or from each surface's init) | `crates/uffs-mft/src/cache.rs` | ✅ DONE |
| S1.1.6 | Update doc comments referencing `{TEMP}/uffs_index_cache/` | `crates/uffs-mft/src/cache.rs:1-18` | ✅ DONE |

**Acceptance criteria**:
- `cache_dir()` returns platform-appropriate path on all 3 OSes
- Old cache files in `%TEMP%` are automatically moved on first run
- All existing callers work unchanged (they all go through `cache_dir()` / `cache_file_path()`)

**Call sites that auto-inherit** (no changes needed):
- `cache_file_path()` → calls `cache_dir()`
- `save_to_cache()` → calls `cache_dir()` + `cache_file_path()`
- `load_cached_index()` → calls `cache_file_path()`
- `check_cache_status()` → calls `cache_file_path()`
- `remove_cached_index()` → calls `cache_file_path()`
- `remove_all_cached_indices()` → calls `cache_dir()`
- `list_cached_drives()` → calls `cache_dir()`
- `reader/index_cache.rs:259-265` → calls `cache_dir()` + `cache_file_path()`
- `uffs-tui/src/compact.rs:569` → calls `cache_file_path()`
- `uffs-tui/src/full_record.rs:137` → calls `cache_file_path()`

### Wave S1.2 — Directory & File Permissions

**What**: Create cache dir with owner-only permissions. Set file permissions on write.

#### Tasks

| ID | Task | File | Status |
|----|------|------|--------|
| S1.2.1 | Create `fn create_secure_dir(path: &Path) -> io::Result<()>` — creates dir with 0700 (Unix) or owner-only ACL via icacls (Windows) | `crates/uffs-security/src/fs.rs` | ✅ DONE |
| S1.2.2 | Replace `std::fs::create_dir_all(&dir)` in `save_to_cache()` with `create_secure_dir()` | `crates/uffs-mft/src/cache.rs` | ✅ DONE |
| S1.2.3 | Replace `std::fs::create_dir_all(&dir)` in `read_and_cache_index()` with `create_secure_dir()` | `crates/uffs-mft/src/reader/index_cache.rs` | ✅ DONE |
| S1.2.4 | Create `fn set_file_permissions_owner_only(path: &Path) -> io::Result<()>` — sets 0600 (Unix) | `crates/uffs-mft/src/cache.rs` (new fn) | ✅ DONE |
| S1.2.5 | Call `set_file_permissions_owner_only()` after every cache file write (both `save_to_file` and raw `fs::write` paths) | `crates/uffs-mft/src/cache.rs` (called from `atomic_write`) | ✅ DONE |
| S1.2.6 | Windows: owner-only ACL via `icacls /inheritance:r /grant:r %USERNAME%:(OI)(CI)F` | `crates/uffs-security/src/fs.rs` | ✅ DONE |

**Acceptance criteria**:
- `ls -la` on cache dir shows `drwx------` (macOS/Linux)
- `ls -la` on cache files shows `-rw-------`
- Windows: `icacls` shows only current user has access
- Existing callers work unchanged

### Wave S1.3 — Atomic Writes

**What**: Write to `.tmp` then rename, preventing partial-write exposure.

#### Tasks

| ID | Task | File | Status |
|----|------|------|--------|
| S1.3.1 | Create `fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()>` — write to `.uffs.tmp`, `sync_all()`, rename | `crates/uffs-mft/src/cache.rs` (new fn) | ✅ DONE |
| S1.3.2 | Update `save_to_file()` to use `atomic_write()` instead of `File::create` + `write_all` | `crates/uffs-mft/src/index/storage/file_io.rs` | ✅ DONE |
| S1.3.3 | Update `read_and_cache_index()` raw write path to use `atomic_write()` | `crates/uffs-mft/src/reader/index_cache.rs` | ✅ DONE |
| S1.3.4 | Windows: use `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` for atomic rename | `crates/uffs-mft/src/cache.rs` | ✅ DONE (`std::fs::rename` uses this on Windows) |

**Acceptance criteria**:
- Kill process mid-write → old cache file intact (not corrupted)
- No `.uffs.tmp` files left behind on normal operation
- On crash, stale `.uffs.tmp` cleaned up on next write

---

## Phase S2: Encryption at Rest

> **Goal**: AES-256-GCM encryption with platform-native key storage.  
> **Dependencies**: S1 (secure dir + atomic writes)  
> **Effort**: 2–3 days  
> **New crates/deps**: `aes-gcm`, `rand`, platform key storage crates

### Wave S2.1 — Dependencies & Crypto Module Scaffold

#### Tasks

| ID | Task | File | Status |
|----|------|------|--------|
| S2.1.1 | Add workspace deps: `aes-gcm = "0.10"`, `rand = "0.9.2"`, `security-framework = "3.2"` | `Cargo.toml` (workspace) | ✅ DONE |
| S2.1.2 | Add `aes-gcm`, `rand` to `uffs-security` dependencies | `crates/uffs-security/Cargo.toml` | ✅ DONE (in uffs-security, not uffs-mft) |
| S2.1.3 | Add platform key deps: `security-framework` (macOS) | `crates/uffs-security/Cargo.toml` | ✅ DONE |
| S2.1.4 | Create crypto module — encrypt/decrypt functions | `crates/uffs-security/src/crypto.rs` | ✅ DONE |
| S2.1.5 | Create keystore module — platform key storage abstraction | `crates/uffs-security/src/keystore.rs` | ✅ DONE |
| S2.1.6 | Refactor: crypto/keystore in dedicated `uffs-security` crate (not cache submodule) | `crates/uffs-security/` | ✅ DONE (cleaner than original plan) |

### Wave S2.2 — Platform Key Storage

**What**: Generate, store, and retrieve a 256-bit AES key using OS-native
secure storage. The user never sees or manages keys.

#### Tasks

| ID | Task | File | Status |
|----|------|------|--------|
| S2.2.1 | Public function `get_cache_key() -> io::Result<[u8; 32]>` with platform dispatch | `uffs-security/src/keystore.rs` | ✅ DONE |
| S2.2.2 | Windows impl: DPAPI (`CryptProtectData`/`CryptUnprotectData`) with entropy `uffs-cache-v1` | `uffs-security/src/keystore.rs` | ✅ DONE |
| S2.2.3 | macOS impl: Keychain via `security-framework` (service `com.uffs.cache`, account `encryption-key-v1`) | `uffs-security/src/keystore.rs` | ✅ DONE |
| S2.2.4 | Linux impl: file-based key fallback (Secret Service deferred) | `uffs-security/src/keystore.rs` | ✅ DONE (file fallback; dbus deferred) |
| S2.2.5 | Public function dispatches to platform impl, generates key on first call | `uffs-security/src/keystore.rs` | ✅ DONE |
| S2.2.6 | Unit test: key round-trip — generate, store, retrieve, compare | `uffs-security/src/keystore.rs` (test module) | ✅ DONE |
| S2.2.7 | Unit test: key is non-zero | `uffs-security/src/keystore.rs` (test module) | ✅ DONE |

**Acceptance criteria**:
- `get_cache_key()` returns same 32 bytes across calls
- Key never appears in any file (except DPAPI-encrypted blob on Windows)
- On macOS: key visible in Keychain Access under `com.uffs.cache`
- First call generates key + stores; subsequent calls retrieve

#### Key Lifecycle

```
First run:
  get_cache_key()
    → key not found in OS store
    → generate 256-bit random key (OsRng)
    → store in OS secure vault
    → return key

Subsequent runs:
  get_cache_key()
    → key found in OS store
    → return key (no generation)

Key loss (password reset, keychain corruption):
  get_cache_key()
    → key not found / decryption fails
    → generate NEW key
    → store new key
    → return new key
    → old cache files will fail GCM auth → trigger rebuild from MFT
    → user sees "Rebuilding index…" once, then normal operation
```

### Wave S2.3 — Encrypt / Decrypt Core

**What**: AES-256-GCM encrypt/decrypt with the `UFFSENC` file format.

#### Encrypted File Format

```
Offset  Size    Field
──────  ──────  ──────────────────────────────
0       8       Magic: b"UFFSENC\0"
8       2       Encryption format version (u16 LE) — currently 1
10      1       Algorithm ID: 0x01 = AES-256-GCM
11      1       KDF ID: 0x01=DPAPI, 0x02=Keychain, 0x03=SecretService, 0x04=HKDF
12      12      Nonce (96-bit, random per write)
24      4       Plaintext length (u32 LE)
28      N       Ciphertext
28+N    16      GCM Authentication Tag
────────────────────────────────────────────────
Total overhead: 44 bytes
AAD: bytes 0..28 (header, included in GCM auth)
```

#### Tasks

| ID | Task | File | Status |
|----|------|------|--------|
| S2.3.1 | Define constants: `ENCRYPTED_MAGIC`, `ENC_FORMAT_VERSION`, `ALGO_AES_256_GCM`, `KDF_*` IDs | `uffs-security/src/crypto.rs` | ✅ DONE |
| S2.3.2 | Implement `fn encrypt_cache(plaintext: &[u8], key: &[u8; 32]) -> io::Result<Vec<u8>>` | `uffs-security/src/crypto.rs` | ✅ DONE |
| S2.3.3 | Implement `fn decrypt_cache(data: &[u8], key: &[u8; 32]) -> io::Result<Vec<u8>>` | `uffs-security/src/crypto.rs` | ✅ DONE |
| S2.3.4 | Implement `fn detect_format(data: &[u8]) -> CacheFormat` | `uffs-security/src/crypto.rs` | ✅ DONE |
| S2.3.5 | Unit test: encrypt → decrypt round-trip (0B, 1B, 1MB) | `uffs-security/src/crypto.rs` (test module) | ✅ DONE (3 tests) |
| S2.3.6 | Unit test: tampered ciphertext → decrypt returns Err | `uffs-security/src/crypto.rs` (test module) | ✅ DONE |
| S2.3.7 | Unit test: tampered header (nonce, algo ID) → decrypt returns Err | `uffs-security/src/crypto.rs` (test module) | ✅ DONE (2 tests) |
| S2.3.8 | Unit test: truncated file → decrypt returns Err | `uffs-security/src/crypto.rs` (test module) | ✅ DONE |
| S2.3.9 | Unit test: legacy `UFFSIDX\0` magic → `detect_format` returns `LegacyPlaintext` | `uffs-security/src/crypto.rs` (test module) | ✅ DONE |

### Wave S2.4 — Integration into File I/O

**What**: Wire encrypt/decrypt into the existing save/load paths. All callers
get encryption automatically — no API changes.

#### Tasks

| ID | Task | File | Status |
|----|------|------|--------|
| S2.4.1 | Update `save_to_file()`: serialize → encrypt → atomic_write | `crates/uffs-mft/src/index/storage/file_io.rs` | ✅ DONE |
| S2.4.2 | Update `load_from_file()`: read → detect_format → decrypt/passthrough → deserialize | `crates/uffs-mft/src/index/storage/file_io.rs` | ✅ DONE |
| S2.4.3 | Legacy migration: `LegacyPlaintext` passes through, re-encrypts on next save | `crates/uffs-mft/src/index/storage/file_io.rs` | ✅ DONE |
| S2.4.4 | Update `read_and_cache_index()`: encrypt before atomic_write | `crates/uffs-mft/src/reader/index_cache.rs` | ✅ DONE |
| S2.4.5 | Handle key-unavailable: fall back to plaintext with `tracing::warn` | `file_io.rs` + `index_cache.rs` | ✅ DONE |
| S2.4.6 | Handle GCM auth failure: log warning, delete corrupted file, return Err | `crates/uffs-mft/src/index/storage/file_io.rs` | ✅ DONE |

**Acceptance criteria**:
- All existing callers (`save_to_cache`, `load_cached_index`, `check_cache_status`, `read_and_cache_index`, TUI compact, TUI full_record) work unchanged
- Cache files on disk start with `UFFSENC\0` (not `UFFSIDX\0`)
- `hexdump` of cache file shows encrypted content (no plaintext filenames)
- Old plaintext cache files auto-migrate on first load
- Key loss → cache rebuild from MFT (no error dialog, no crash)

**Call sites that auto-inherit** (go through `save_to_file`/`load_from_file`):
- `cache.rs: save_to_cache()` → calls `index.save_to_file()`
- `cache.rs: load_cached_index()` → calls `MftIndex::load_from_file()`
- `cache.rs: check_cache_status()` → calls `MftIndex::load_from_file()`
- `cache.rs: load_or_build_dataframe_cached_sync()` → calls `load_cached_index()`
- `commands/windows/incremental.rs:158` → calls `index.save_to_file()`
- `commands/windows/incremental.rs:192` → calls `MftIndex::load_from_file()`

**Call site that needs direct update** (bypasses `save_to_file`):
- `reader/index_cache.rs:256-266` → calls `index.serialize()` + `fs::write()` directly

### Wave S2.5 — Performance Validation

#### Tasks

| ID | Task | File | Status |
|----|------|------|--------|
| S2.5.1 | Add benchmark: `encrypt_cache` throughput (MB/s) for 100MB, 500MB payloads | `crates/uffs-mft/benches/` or inline timing | ⬜ TODO |
| S2.5.2 | Add benchmark: `decrypt_cache` throughput | Same | ⬜ TODO |
| S2.5.3 | Add benchmark: end-to-end `save_to_file` (serialize + encrypt + write) vs baseline | Same | ⬜ TODO |
| S2.5.4 | Add benchmark: end-to-end `load_from_file` (read + decrypt + deserialize) vs baseline | Same | ⬜ TODO |
| S2.5.5 | Verify: AES-NI / ARM CE is being used (check `aes-gcm` feature flags, run `RUSTFLAGS="-C target-cpu=native"`) | CI config | ⬜ TODO |
| S2.5.6 | Document results in this file (Performance Results section below) | This document | ⬜ TODO |

**Targets**:
- Encrypt/decrypt throughput: ≥4 GB/s (AES-NI) or ≥2 GB/s (ARM CE)
- `save_to_file` overhead: <200ms for 500MB
- `load_from_file` overhead: <100ms for 500MB
- TUI 7-drive startup: <6s (from ~5s baseline)

---

## Phase S3: Secure Lifecycle

> **Goal**: Secure wipe, file locking.  
> **Dependencies**: S1  
> **Effort**: 1 day  
> **New crates/deps**: None

### Wave S3.1 — Secure Wipe

#### Tasks

| ID | Task | File | Status |
|----|------|------|--------|
| S3.1.1 | Create `fn secure_remove(path: &Path) -> io::Result<()>` — zero-overwrite + `sync_all()` + `remove_file()` | `uffs-security/src/fs.rs` | ✅ DONE |
| S3.1.2 | Update `remove_cached_index()` to use `secure_remove()` | `crates/uffs-mft/src/cache.rs` | ✅ DONE |
| S3.1.3 | Update `remove_all_cached_indices()` to wipe each `.uffs` file before removing dir | `crates/uffs-mft/src/cache.rs` | ✅ DONE |
| S3.1.4 | `cleanup_expired_cache()` calls `remove_all_cached_indices()` which now uses `secure_remove()` | `crates/uffs-mft/src/cache.rs` | ✅ DONE (inherited) |
| S3.1.5 | Clean up stale `.uffs.tmp` files on startup via `cleanup_stale_temps()` | `crates/uffs-mft/src/cache.rs` | ✅ DONE (S1) |

**Acceptance criteria**:
- After `remove_cached_index('C')`, reading the raw disk sectors shows zeros
  (on HDD; best-effort on SSD)
- No `.uffs` or `.uffs.tmp` files remain after cleanup

### Wave S3.2 — File Locking

#### Tasks

| ID | Task | File | Status |
|----|------|------|--------|
| S3.2.1 | Create `FileLock` + `with_file_lock()` with timeout + retry | `uffs-security/src/fs.rs` | ✅ DONE |
| S3.2.2 | Use `flock` (Unix) / `LockFileEx` (Windows) for platform locking | `uffs-security/src/fs.rs` | ✅ DONE |
| S3.2.3 | Wrap `save_to_cache()` body in exclusive lock | `crates/uffs-mft/src/cache.rs` | ✅ DONE |
| S3.2.4 | Wrap `load_cached_index()` body in shared (read) lock | `crates/uffs-mft/src/cache.rs` | ✅ DONE |
| S3.2.5 | Wrap `read_and_cache_index()` raw write path in exclusive lock | `crates/uffs-mft/src/reader/index_cache.rs` | ✅ DONE |

**Acceptance criteria**:
- Two concurrent UFFS processes writing to same drive cache → no corruption
- Lock contention resolves within 5 seconds (timeout + retry)

---

## Phase S4: Daemon IPC Hardening

> **Goal**: Secure the daemon's IPC socket/pipe.  
> **Dependencies**: Daemon Phase 1 (daemon exists)  
> **Effort**: 2–3 days  
> **New crates/deps**: None beyond daemon deps

### Wave S4.1 — Socket / Pipe Permissions

| ID | Task | File | Status |
|----|------|------|--------|
| S4.1.1 | Unix: create socket with mode 0600 | `crates/uffs-daemon/src/ipc.rs` | ✅ DONE |
| S4.1.2 | Unix: create socket dir with mode 0700 | `crates/uffs-daemon/src/ipc.rs` | ✅ DONE |
| S4.1.3 | Windows: AF_UNIX socket with owner-only ACL (icacls) | `crates/uffs-daemon/src/ipc.rs` | ✅ DONE (N/A: AF_UNIX, not named pipes; socket dir ACL via S1.2.6) |
| S4.1.4 | Windows: SID isolation via socket dir ACL | `crates/uffs-daemon/src/ipc.rs` | ✅ DONE (icacls owner-only ACL on socket dir) |

### Wave S4.2 — Peer Credential Verification

| ID | Task | File | Status |
|----|------|------|--------|
| S4.2.1 | Linux: verify `getpeereid()` UID matches daemon UID on every accept | `crates/uffs-daemon/src/ipc.rs` | ✅ DONE |
| S4.2.2 | macOS: verify `getpeereid()` UID on every accept | `crates/uffs-daemon/src/ipc.rs` | ✅ DONE |
| S4.2.3 | Windows: socket dir ACL prevents other-user connections | `crates/uffs-daemon/src/ipc.rs` | ✅ DONE (OS-enforced via S1.2.6 icacls) |
| S4.2.4 | Reject connections from different UID with log warning | `crates/uffs-daemon/src/ipc.rs` | ✅ DONE |

### Wave S4.3 — Daemon Identity Verification (Client-Side)

| ID | Task | File | Status |
|----|------|------|--------|
| S4.3.1 | PID file format: `{pid}\n{timestamp}\n{exe_path_hash}\n{shutdown_nonce}\n` | `lifecycle.rs` | ✅ DONE |
| S4.3.2 | PID file permissions: 0600 | `lifecycle.rs` | ✅ DONE |
| S4.3.3 | `parse_pid_file()` + `expected_daemon_exe_hash()` for client verification | `lifecycle.rs` | ✅ DONE |
| S4.3.4 | Client: verify exe path after connect via PID file + exe hash | `connect.rs` + `verify.rs` | ✅ DONE |
| S4.3.5 | macOS: `proc_pidpath()` for exe path lookup | `verify.rs` | ✅ DONE |
| S4.3.6 | Linux: `/proc/{pid}/exe` readlink for exe path lookup | `verify.rs` | ✅ DONE |
| S4.3.7 | Windows: `QueryFullProcessImageNameW()` for exe path lookup | `verify.rs` | ✅ DONE |
| S4.3.8 | Code signature: `codesign` (macOS), `Get-AuthenticodeSignature` (Windows), N/A (Linux) | `verify.rs` | ✅ DONE |

### Wave S4.4 — Input Validation & Resource Limits

| ID | Task | File | Status |
|----|------|------|--------|
| S4.4.1 | Max message size: 16 MB (reject + disconnect) | `crates/uffs-daemon/src/ipc.rs` | ✅ DONE |
| S4.4.2 | JSON parse with size-bounded reader | `crates/uffs-daemon/src/ipc.rs` | ✅ DONE (line length check) |
| S4.4.3 | Max pattern length: 4096 chars | `crates/uffs-daemon/src/handler.rs` | ✅ DONE |
| S4.4.4 | Hard cap on `limit` param: 100,000 rows | `crates/uffs-daemon/src/handler.rs` | ✅ DONE |
| S4.4.5 | Max concurrent connections: 32 | `crates/uffs-daemon/src/ipc.rs` | ✅ DONE |
| S4.4.6 | Per-connection rate limit: 100 queries/sec (token bucket) | `ipc.rs` | ✅ DONE |
| S4.4.7 | Read timeout: 30 seconds per message | `ipc.rs` | ✅ DONE |
| S4.4.8 | Idle connection timeout: 5 minutes | `ipc.rs` | ✅ DONE |
| S4.4.9 | `shutdown` requires nonce from PID file | `handler.rs` + `lifecycle.rs` | ✅ DONE |

---

## Phase S5: Access Broker Hardening (Windows)

> **Goal**: Secure the privileged broker pipe.  
> **Dependencies**: Daemon Phase 5 (broker exists)  
> **Effort**: 1–2 days

| ID | Task | File | Status |
|----|------|------|--------|
| S5.1 | Broker pipe DACL: elevated process → Administrators-only default DACL | `broker.rs` | ✅ DONE |
| S5.2 | Client Authenticode verification: `Get-AuthenticodeSignature` via PowerShell | `broker.rs` | ✅ DONE |
| S5.3 | Audit logging: structured tracing (action, PID, exe, drive, detail) for every request | `broker.rs` | ✅ DONE |
| S5.4 | Rate limit: 1 handle per drive per 10s (`HashMap<char, Instant>`) | `broker.rs` | ✅ DONE |
| S5.5 | Read-only handles only: `FILE_GENERIC_READ` in `DuplicateHandle` | `broker.rs` | ✅ DONE |

---

## Phase S6: Network Transport Security

> **Goal**: TLS + auth for HTTP/SSE (Phase 6 of daemon roadmap).  
> **Dependencies**: Daemon Phase 6 (HTTP listener exists)  
> **Effort**: 2 days

| ID | Task | File | Status |
|----|------|------|--------|
| S6.1 | Add `rustls` (or `native-tls`) + `tokio-rustls` dependency | `crates/uffs-daemon/Cargo.toml` (future) | ⬜ TODO |
| S6.2 | Auto-generate self-signed cert on first start (stored in data dir) | `crates/uffs-daemon/src/tls.rs` (future) | ⬜ TODO |
| S6.3 | Wrap HTTP listener with TLS acceptor | `crates/uffs-daemon/src/http.rs` (future) | ⬜ TODO |
| S6.4 | Generate random bearer token on daemon start, store in `~/.local/share/uffs/token` (0600) | `crates/uffs-daemon/src/auth.rs` (future) | ⬜ TODO |
| S6.5 | Validate `Authorization: Bearer {token}` on every HTTP request | `crates/uffs-daemon/src/http.rs` | ⬜ TODO |
| S6.6 | Bind to `127.0.0.1` / `::1` by default; require explicit `--bind 0.0.0.0` for remote | `crates/uffs-daemon/src/http.rs` | ⬜ TODO |
| S6.7 | mTLS: optional client cert verification via `--mtls-ca` flag | `crates/uffs-daemon/src/tls.rs` | ⬜ TODO |
| S6.8 | CORS: deny all by default; `--cors-origin` flag for explicit allowlist | `crates/uffs-daemon/src/http.rs` | ⬜ TODO |

---

## Progress Tracking

### Overall Status

| Phase | Status | Started | Completed | Notes |
|-------|--------|---------|-----------|-------|
| **S1** Secure Foundation | 🟢 DONE | 2026-03-26 | 2026-03-26 | All complete |
| **S2** Encryption at Rest | 🟢 DONE | 2026-03-26 | 2026-03-26 | S2.5 benchmarks deferred; Linux dbus/Secret Service deferred (file-based is fine) |
| **S3** Secure Lifecycle | 🟢 DONE | 2026-03-26 | 2026-03-26 | |
| **S4** Daemon IPC | 🟢 DONE | 2026-03-26 | 2026-03-26 | All complete |
| **S5** Access Broker | 🟢 DONE | 2026-03-26 | 2026-03-26 | All 5 tasks complete |
| **S6** Network Transport | ⬜ NOT STARTED | — | — | Depends on HTTP |

### Wave-Level Status

| Wave | Tasks | Done | Remaining | Status |
|------|-------|------|-----------|--------|
| S1.1 Cache Dir Relocation | 6 | 6 | 0 | ✅ |
| S1.2 Permissions | 6 | 6 | 0 | ✅ |
| S1.3 Atomic Writes | 4 | 4 | 0 | ✅ |
| S2.1 Deps & Scaffold | 6 | 6 | 0 | ✅ |
| S2.2 Key Storage | 7 | 7 | 0 | ✅ |
| S2.3 Encrypt/Decrypt Core | 9 | 9 | 0 | ✅ |
| S2.4 File I/O Integration | 6 | 6 | 0 | ✅ |
| S2.5 Perf Validation | 6 | 0 | 6 | ⬜ (deferred) |
| S3.1 Secure Wipe | 5 | 5 | 0 | ✅ |
| S3.2 File Locking | 5 | 5 | 0 | ✅ |
| S4.1 Socket/Pipe Perms | 4 | 4 | 0 | ✅ |
| S4.2 Peer Credentials | 4 | 4 | 0 | ✅ |
| S4.3 Daemon Identity | 8 | 8 | 0 | ✅ |
| S4.4 Input Validation | 9 | 9 | 0 | ✅ |
| S5 Broker Hardening | 5 | 5 | 0 | ✅ |
| S6 Network Transport | 8 | 0 | 8 | ⬜ |
| **TOTAL** | **97** | **92** | **5** | |

### Completion Log

Record completed items here as work progresses:

```
Date        | ID      | Description                              | Commit
────────────┼─────────┼──────────────────────────────────────────┼─────────
2026-03-26  | S1.1.*  | Cache dir relocation (secure_cache_dir,   | 6f4477039
            |         | migrate_legacy_cache, cleanup_stale_temps)|
2026-03-26  | S1.2.*  | Dir/file permissions (create_secure_dir,  | 6f4477039
            |         | set_file_permissions_owner_only)          |
2026-03-26  | S1.3.*  | Atomic writes (atomic_write, updated      | 6f4477039
            |         | save_to_file + read_and_cache_index)     |
2026-03-26  | C1.*    | Created uffs-security crate, moved S1     | 6f4477039
            |         | primitives from cache.rs to fs.rs         |
2026-03-26  | S3.1.*  | Secure wipe (secure_remove, wired into    | 6f4477039
            |         | remove_cached_index, remove_all)          |
2026-03-26  | S3.2.*  | File locking (FileLock, with_file_lock,   | 6f4477039
            |         | wired into save/load/read_and_cache)      |
2026-03-26  | S2.1.*  | aes-gcm + rand + security-framework deps  | 6f4477039
2026-03-26  | S2.3.*  | AES-256-GCM encrypt/decrypt (11 tests)    | 6f4477039
2026-03-26  | S2.2.*  | macOS Keychain keystore (2 tests)         | 6f4477039
2026-03-26  | S2.4.*  | Encryption wired into file_io.rs +        | 6f4477039
            |         | index_cache.rs (auto-migrate legacy)      |
2026-03-26  | S4.1.1-2| Socket 0600, socket dir 0700 (ipc.rs)    | 2353b9b4d
2026-03-26  | S4.2.1-2| Peer credential: getpeereid (Unix)       | 723d0872f
2026-03-26  | S4.2.4  | Reject different UID with log warning     | 723d0872f
2026-03-26  | S4.4.1  | Max message size 16 MB                   | 2353b9b4d
2026-03-26  | S4.4.2  | JSON parse with line length check         | 2353b9b4d
2026-03-26  | S4.4.3  | Max pattern length 4096 chars             | 723d0872f
2026-03-26  | S4.4.4  | Hard cap on limit: 100,000 rows           | 723d0872f
2026-03-26  | S4.4.5  | Max concurrent connections: 32            | 2353b9b4d
2026-03-26  | S4.4.7  | Read timeout: 30 seconds per message      | 2353b9b4d
2026-03-26  | S4.3.1  | PID file: pid + timestamp + exe_hash +   | 2fe32c4be
            |         | shutdown_nonce (FNV-1a hash)              |
2026-03-26  | S4.3.2  | PID file permissions 0600                | 2fe32c4be
2026-03-26  | S4.3.3  | parse_pid_file() + expected_daemon_exe_  | 2fe32c4be
            |         | hash() for client identity verification  |
2026-03-26  | S4.4.6  | Rate limit: 100 queries/sec per conn     | 2fe32c4be
            |         | (token bucket in handle_connection)       |
2026-03-26  | S4.4.8  | Idle connection timeout: 5 min            | 2fe32c4be
2026-03-26  | S4.4.9  | Shutdown nonce: random hex in PID file,   | 2fe32c4be
            |         | handler verifies before accepting         |
```

---

## Performance Results

Record benchmark results here as they become available:

```
Benchmark                                | Baseline | With Security | Overhead | Target  | Pass?
─────────────────────────────────────────┼──────────┼───────────────┼──────────┼─────────┼──────
Cache Save (500 MB, NVMe)                |          |               |          | <200ms  |
Cache Load (500 MB, NVMe)                |          |               |          | <100ms  |
Cache Save (500 MB, HDD)                 |          |               |          | <200ms  |
Cache Load (500 MB, HDD)                 |          |               |          | <100ms  |
TUI 7-drive startup (all cached)         |          |               |          | <6s     |
Encrypt throughput (AES-NI / ARM CE)     |          |               |          | ≥4 GB/s |
Decrypt throughput (AES-NI / ARM CE)     |          |               |          | ≥4 GB/s |
IPC round-trip latency (daemon mode)     |          |               |          | <15ms   |
```

---

## Files Changed Summary

Quick reference of all files that will be modified or created:

### Modified Files

| File | Phase | Changes |
|------|-------|---------|
| `Cargo.toml` (workspace) | S2.1 | Add `aes-gcm`, `rand` workspace deps |
| `crates/uffs-mft/Cargo.toml` | S1.1, S2.1 | Add `dirs-next`, `aes-gcm`, `rand`, platform key deps |
| `crates/uffs-mft/src/cache.rs` | S1, S3 | Refactor to `cache/mod.rs`; new dir, permissions, atomic write, secure wipe, lock fns |
| `crates/uffs-mft/src/index/storage/file_io.rs` | S1.3, S2.4 | Atomic write, encrypt on save, decrypt on load |
| `crates/uffs-mft/src/reader/index_cache.rs` | S1.2, S1.3, S2.4, S3.2 | Secure dir creation, atomic write, encrypt, lock |
| `crates/uffs-mft/src/lib.rs` | S1.1 | Re-export any new public cache APIs |

### New Files

| File | Phase | Purpose |
|------|-------|---------|
| `crates/uffs-mft/src/cache/mod.rs` | S2.1 | Refactored cache module root (existing code moves here) |
| `crates/uffs-mft/src/cache/crypto.rs` | S2.1 | AES-256-GCM encrypt/decrypt, format detection |
| `crates/uffs-mft/src/cache/keystore.rs` | S2.1 | Platform key storage (DPAPI, Keychain, Secret Service) |

### Future Files (created with daemon)

| File | Phase | Purpose |
|------|-------|---------|
| `crates/uffs-daemon/src/ipc.rs` | S4 | Socket/pipe creation, permissions, peer creds |
| `crates/uffs-daemon/src/lifecycle.rs` | S4 | PID file management |
| `crates/uffs-client/src/connect.rs` | S4 | Daemon identity verification |
| `crates/uffs-daemon/src/handler.rs` | S4 | Input validation, resource limits |
| `crates/uffs-broker/src/auth.rs` | S5 | Authenticode verification |
| `crates/uffs-daemon/src/tls.rs` | S6 | TLS cert generation, acceptor |
| `crates/uffs-daemon/src/http.rs` | S6 | Bearer auth, CORS, bind control |

---

## Security Audit Script

A platform-aware Rust script checks all security measures in place and reports
gaps. Run it at any time to see the current security posture:

```bash
rust-script scripts/dev/security-audit.rs
```

**What it checks** (maps directly to implementation plan task IDs):

| Phase | Checks |
|-------|--------|
| **S1** | Cache dir location (legacy vs secure), dir permissions (0700), file permissions (0600), stale .tmp files, `temp_dir()` references in source |
| **S2** | Cache file format (UFFSENC vs plaintext UFFSIDX), platform key storage (DPAPI blob / Keychain entry / Secret Service), crypto module exists, deps in Cargo.toml, encrypt/decrypt wired into file_io.rs |
| **S3** | `secure_remove()` usage vs plain `fs::remove_file`, file locking patterns |
| **S4** | Daemon socket permissions (0600), PID file permissions + format, daemon dir permissions (0700) |

**Example output** (current state — before any security work):

```
╔══════════════════════════════════════════════════════════════╗
║           UFFS Security Posture Audit                       ║
╠══════════════════════════════════════════════════════════════╣
║  Platform: macOS                                            ║
║  Date:     2026-03-26 11:29 UTC                             ║
╚══════════════════════════════════════════════════════════════╝

── Phase S1: Secure Foundation ──

  ✅ [S1.1] Legacy cache dir should not contain .uffs files: ...
  ⚠️  [S1.1] Secure cache dir exists: ... does not exist (no cache yet)
  ❌ [S1.1] cache.rs no longer uses temp_dir(): still references temp_dir()
  🔲 [S1.3] Cache writes use atomic write: not yet implemented

── Phase S2: Encryption at Rest ──

  🔲 [S2.1] cache.rs refactored to cache/ module directory: Still single cache.rs
  🔲 [S2.1] Crypto module exists: not found
  🔲 [S2.1] Keystore module exists: not found
  🔲 [S2.1] aes-gcm dependency: not found
  🔲 [S2.2] Encryption key in Keychain: not yet implemented
  🔲 [S2.4] file_io.rs integrates encrypt/decrypt: plain read/write

── Phase S3: Secure Lifecycle ──

  🔲 [S3.1] Cache removal uses secure_remove(): not yet implemented
  🔲 [S3.2] Cache operations use file locking: not yet implemented

══════════════════════════════════════════════════════════════
  SUMMARY
══════════════════════════════════════════════════════════════
  Total checks:  16
  ✅ Pass:        1
  ❌ Fail:        1
  ⚠️  Warn:        1
  ⏭️  Skip:        3
  🔲 Not Impl:    10

  Security Score: 8% (grade: F) — 1/13 applicable checks pass

  🚨 ACTION REQUIRED: 1 failing checks need attention
  📋 10 checks are NOT YET IMPLEMENTED
```

As security work progresses, run the script again — passing checks go green,
score climbs toward 100%. Target: **grade A (≥90%)** before release.

---

## Decision Log

Record design decisions and trade-offs here as they come up:

```
Date        | Decision                                        | Rationale
────────────┼─────────────────────────────────────────────────┼──────────────────────────
2026-03-26  | AES-256-GCM over ChaCha20-Poly1305              | HW accel (AES-NI/ARM CE) → 4-8 GB/s vs 1-2 GB/s
2026-03-26  | No encryption on local IPC                      | Kernel-isolated, same-user equiv, industry standard
2026-03-26  | DPAPI / Keychain for key storage                | Zero user friction, OS-managed, hardware-backed
2026-03-26  | GCM auth failure → rebuild from MFT             | Fail-secure, no error dialogs
2026-03-26  | Legacy plaintext auto-migration                 | Zero user friction on upgrade
            |                                                 |
```

---

*Document Version: 1.0*  
*Last Updated: 2026-03-26*  
*Reference: `docs/architecture/CACHE_SECURITY_ANALYSIS.md`*
