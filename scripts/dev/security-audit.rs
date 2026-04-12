#!/usr/bin/env rust-script
//! ```cargo
//! [dependencies]
//! dirs-next = "2.0"
//! ```
// =============================================================================
// scripts/dev/security-audit.rs - UFFS Security Posture Audit
// =============================================================================
//
// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.
//
// UFFS - UltraFastFileSearch: High-Performance File Search Tool
//
//! Checks all security measures from SECURITY_IMPLEMENTATION_PLAN.md.
//! Run with: rust-script scripts/dev/security-audit.rs
//!
//! Platform-aware: detects Windows, macOS, Linux and checks platform-specific
//! security properties (permissions, encryption, key storage, etc.)

use std::fs;
use std::path::{Path, PathBuf};

// ─── Result tracking ─────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Status {
    Pass,
    Fail,
    Warn,
    Skip,
    NotImpl,
}

struct Check {
    phase: &'static str,
    id: &'static str,
    description: &'static str,
    status: Status,
    detail: String,
}

impl Check {
    fn new(
        phase: &'static str,
        id: &'static str,
        description: &'static str,
        status: Status,
        detail: String,
    ) -> Self {
        Self {
            phase,
            id,
            description,
            status,
            detail,
        }
    }
}

fn status_icon(s: Status) -> &'static str {
    match s {
        Status::Pass => "✅",
        Status::Fail => "❌",
        Status::Warn => "⚠️ ",
        Status::Skip => "⏭️ ",
        Status::NotImpl => "🔲",
    }
}

#[allow(dead_code)]
fn status_label(s: Status) -> &'static str {
    match s {
        Status::Pass => "PASS",
        Status::Fail => "FAIL",
        Status::Warn => "WARN",
        Status::Skip => "SKIP",
        Status::NotImpl => "NOT IMPL",
    }
}

// ─── Platform detection ──────────────────────────────────────────────────────

fn platform_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else {
        "Unknown"
    }
}

// ─── Cache directory helpers ─────────────────────────────────────────────────

/// Returns the OLD (insecure) cache directory in temp.
fn legacy_cache_dir() -> PathBuf {
    std::env::temp_dir().join("uffs_index_cache")
}

/// Returns the NEW (secure) platform-appropriate cache directory.
fn secure_cache_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        // %LOCALAPPDATA%\uffs\cache
        dirs_next::data_local_dir().map(|d| d.join("uffs").join("cache"))
    } else if cfg!(target_os = "macos") {
        // ~/Library/Caches/com.uffs
        dirs_next::cache_dir().map(|d| d.join("com.uffs"))
    } else {
        // $XDG_CACHE_HOME/uffs or ~/.cache/uffs
        dirs_next::cache_dir().map(|d| d.join("uffs"))
    }
}

/// Returns the daemon runtime directory.
fn daemon_dir() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        dirs_next::data_local_dir().map(|d| d.join("uffs"))
    } else {
        // ~/.local/share/uffs
        dirs_next::data_dir().map(|d| d.join("uffs"))
    }
}

// ─── Permission checks (Unix) ────────────────────────────────────────────────

#[cfg(unix)]
fn get_unix_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).ok().map(|m| m.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn get_unix_mode(_path: &Path) -> Option<u32> {
    None
}

#[cfg(unix)]
fn check_mode(path: &Path, expected: u32) -> (Status, String) {
    match get_unix_mode(path) {
        Some(mode) if mode == expected => (
            Status::Pass,
            format!("mode {:04o} ✓", mode),
        ),
        Some(mode) => (
            Status::Fail,
            format!("mode {:04o} (expected {:04o})", mode, expected),
        ),
        None => (Status::Skip, "path does not exist".to_string()),
    }
}

#[cfg(not(unix))]
fn check_mode(_path: &Path, _expected: u32) -> (Status, String) {
    (Status::Skip, "Unix permission check N/A on this platform".to_string())
}

// ─── File format detection ───────────────────────────────────────────────────

const MAGIC_ENCRYPTED: &[u8; 8] = b"UFFSENC\0";
const MAGIC_PLAINTEXT: &[u8; 8] = b"UFFSIDX\0";

enum CacheFormat {
    Encrypted,
    LegacyPlaintext,
    Unknown,
    Empty,
}

fn detect_cache_format(path: &Path) -> CacheFormat {
    match fs::read(path) {
        Ok(data) if data.len() < 8 => CacheFormat::Empty,
        Ok(data) => {
            let magic: &[u8] = &data[..8];
            if magic == MAGIC_ENCRYPTED {
                CacheFormat::Encrypted
            } else if magic == MAGIC_PLAINTEXT {
                CacheFormat::LegacyPlaintext
            } else {
                CacheFormat::Unknown
            }
        }
        Err(_) => CacheFormat::Empty,
    }
}

// ─── Find all .uffs files in a directory ─────────────────────────────────────

fn find_uffs_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map_or(false, |e| e == "uffs") {
                files.push(path);
            }
        }
    }
    files
}

// ─── Phase S1 checks ─────────────────────────────────────────────────────────

fn check_s1(results: &mut Vec<Check>) {
    // S1.1: Cache directory relocation
    let legacy = legacy_cache_dir();
    let secure = secure_cache_dir();

    // Check: legacy dir should NOT exist (or be empty)
    let legacy_files = find_uffs_files(&legacy);
    if legacy.exists() && !legacy_files.is_empty() {
        results.push(Check::new(
            "S1",
            "S1.1",
            "Legacy cache dir should not contain .uffs files",
            Status::Fail,
            format!(
                "{} has {} .uffs files — not migrated to secure location",
                legacy.display(),
                legacy_files.len()
            ),
        ));
    } else if legacy.exists() {
        results.push(Check::new(
            "S1",
            "S1.1",
            "Legacy cache dir should not contain .uffs files",
            Status::Warn,
            format!("{} exists but is empty", legacy.display()),
        ));
    } else {
        results.push(Check::new(
            "S1",
            "S1.1",
            "Legacy cache dir should not contain .uffs files",
            Status::Pass,
            format!("{} does not exist ✓", legacy.display()),
        ));
    }

    // Check: secure dir should exist
    if let Some(ref secure_dir) = secure {
        if secure_dir.exists() {
            results.push(Check::new(
                "S1",
                "S1.1",
                "Secure cache dir exists at platform-appropriate location",
                Status::Pass,
                format!("{}", secure_dir.display()),
            ));
        } else {
            results.push(Check::new(
                "S1",
                "S1.1",
                "Secure cache dir exists at platform-appropriate location",
                Status::Warn,
                format!("{} does not exist (no cache yet, or not migrated)", secure_dir.display()),
            ));
        }
    } else {
        results.push(Check::new(
            "S1",
            "S1.1",
            "Secure cache dir exists at platform-appropriate location",
            Status::Fail,
            "Could not determine platform cache directory".to_string(),
        ));
    }

    // S1.2: Directory permissions
    if let Some(ref secure_dir) = secure {
        if secure_dir.exists() {
            let (status, detail) = check_mode(secure_dir, 0o700);
            results.push(Check::new(
                "S1",
                "S1.2",
                "Cache directory permissions: owner-only (0700)",
                status,
                detail,
            ));
        } else {
            results.push(Check::new(
                "S1",
                "S1.2",
                "Cache directory permissions: owner-only (0700)",
                Status::Skip,
                "Cache dir does not exist yet".to_string(),
            ));
        }
    }

    // S1.2: File permissions on each .uffs file
    let cache_dir_to_check = secure.as_deref().filter(|d| d.exists())
        .or_else(|| if legacy.exists() { Some(legacy.as_path()) } else { None });

    if let Some(dir) = cache_dir_to_check {
        let files = find_uffs_files(dir);
        if files.is_empty() {
            results.push(Check::new(
                "S1",
                "S1.2",
                "Cache file permissions: owner-only (0600)",
                Status::Skip,
                "No .uffs files found".to_string(),
            ));
        }
        for file in &files {
            let fname = file.file_name().unwrap_or_default().to_string_lossy();
            let (status, detail) = check_mode(file, 0o600);
            results.push(Check::new(
                "S1",
                "S1.2",
                "Cache file permissions: owner-only (0600)",
                status,
                format!("{}: {}", fname, detail),
            ));
        }
    }

    // S1.3: Check for stale .tmp files (evidence of non-atomic or crashed writes)
    let dirs_to_check: Vec<&Path> = [secure.as_deref(), Some(legacy.as_path())]
        .into_iter()
        .flatten()
        .filter(|d| d.exists())
        .collect();

    let mut found_tmp = false;
    for dir in &dirs_to_check {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |e| e == "tmp") {
                    found_tmp = true;
                    results.push(Check::new(
                        "S1",
                        "S1.3",
                        "No stale .tmp files (atomic write cleanup)",
                        Status::Warn,
                        format!("Stale temp file: {}", path.display()),
                    ));
                }
            }
        }
    }
    if !found_tmp {
        results.push(Check::new(
            "S1",
            "S1.3",
            "No stale .tmp files (atomic write cleanup)",
            Status::Pass,
            "No stale .tmp files found ✓".to_string(),
        ));
    }
}

// ─── Phase S2 checks ─────────────────────────────────────────────────────────

fn check_s2(results: &mut Vec<Check>) {
    let secure = secure_cache_dir();
    let legacy = legacy_cache_dir();

    let cache_dir_to_check = secure.as_deref().filter(|d| d.exists())
        .or_else(|| if legacy.exists() { Some(legacy.as_path()) } else { None });

    let Some(dir) = cache_dir_to_check else {
        results.push(Check::new(
            "S2",
            "S2.3",
            "Cache files are encrypted (UFFSENC format)",
            Status::Skip,
            "No cache directory found".to_string(),
        ));
        return;
    };

    let files = find_uffs_files(dir);
    if files.is_empty() {
        results.push(Check::new(
            "S2",
            "S2.3",
            "Cache files are encrypted (UFFSENC format)",
            Status::Skip,
            "No .uffs files found".to_string(),
        ));
        return;
    }

    for file in &files {
        let fname = file.file_name().unwrap_or_default().to_string_lossy();
        match detect_cache_format(file) {
            CacheFormat::Encrypted => {
                results.push(Check::new(
                    "S2",
                    "S2.3",
                    "Cache file is encrypted (UFFSENC format)",
                    Status::Pass,
                    format!("{}: UFFSENC ✓", fname),
                ));
            }
            CacheFormat::LegacyPlaintext => {
                results.push(Check::new(
                    "S2",
                    "S2.3",
                    "Cache file is encrypted (UFFSENC format)",
                    Status::Fail,
                    format!("{}: PLAINTEXT (UFFSIDX) — NOT ENCRYPTED", fname),
                ));
            }
            CacheFormat::Unknown => {
                results.push(Check::new(
                    "S2",
                    "S2.3",
                    "Cache file is encrypted (UFFSENC format)",
                    Status::Warn,
                    format!("{}: unknown format", fname),
                ));
            }
            CacheFormat::Empty => {
                results.push(Check::new(
                    "S2",
                    "S2.3",
                    "Cache file is encrypted (UFFSENC format)",
                    Status::Warn,
                    format!("{}: empty or unreadable", fname),
                ));
            }
        }
    }

    // S2.2: Key storage — check platform-specific key existence
    check_key_storage(results);
}

fn check_key_storage(results: &mut Vec<Check>) {
    if cfg!(target_os = "windows") {
        // Check for DPAPI blob at %LOCALAPPDATA%\uffs\key.dpapi
        if let Some(data_dir) = dirs_next::data_local_dir() {
            let key_path = data_dir.join("uffs").join("key.dpapi");
            if key_path.exists() {
                results.push(Check::new(
                    "S2",
                    "S2.2",
                    "Encryption key stored in platform secure storage (DPAPI)",
                    Status::Pass,
                    format!("DPAPI blob: {} ✓", key_path.display()),
                ));
            } else {
                results.push(Check::new(
                    "S2",
                    "S2.2",
                    "Encryption key stored in platform secure storage (DPAPI)",
                    Status::NotImpl,
                    "No DPAPI key blob found — encryption not yet implemented".to_string(),
                ));
            }
        }
    } else if cfg!(target_os = "macos") {
        // Check Keychain via `security find-generic-password`
        let output = std::process::Command::new("security")
            .args(["find-generic-password", "-s", "com.uffs.cache", "-a", "encryption-key-v1"])
            .output();

        match output {
            Ok(o) if o.status.success() => {
                results.push(Check::new(
                    "S2",
                    "S2.2",
                    "Encryption key stored in platform secure storage (Keychain)",
                    Status::Pass,
                    "Keychain entry for com.uffs.cache found ✓".to_string(),
                ));
            }
            _ => {
                results.push(Check::new(
                    "S2",
                    "S2.2",
                    "Encryption key stored in platform secure storage (Keychain)",
                    Status::NotImpl,
                    "No Keychain entry for com.uffs.cache — encryption not yet implemented".to_string(),
                ));
            }
        }
    } else {
        // Linux: check Secret Service via `secret-tool lookup`
        let output = std::process::Command::new("secret-tool")
            .args(["lookup", "service", "com.uffs.cache", "account", "encryption-key-v1"])
            .output();

        match output {
            Ok(o) if o.status.success() && !o.stdout.is_empty() => {
                results.push(Check::new(
                    "S2",
                    "S2.2",
                    "Encryption key stored in platform secure storage (Secret Service)",
                    Status::Pass,
                    "Secret Service entry found ✓".to_string(),
                ));
            }
            _ => {
                results.push(Check::new(
                    "S2",
                    "S2.2",
                    "Encryption key stored in platform secure storage (Secret Service)",
                    Status::NotImpl,
                    "No Secret Service entry — encryption not yet implemented (or headless HKDF fallback)".to_string(),
                ));
            }
        }
    }
}

// ─── Phase S4 checks ─────────────────────────────────────────────────────────

fn check_s4(results: &mut Vec<Check>) {
    let daemon_dir = daemon_dir();

    // Check: daemon socket permissions
    if let Some(ref dir) = daemon_dir {
        let sock_path = dir.join("daemon.sock");
        let pid_path = dir.join("daemon.pid");

        if sock_path.exists() {
            let (status, detail) = check_mode(&sock_path, 0o600);
            results.push(Check::new(
                "S4",
                "S4.1",
                "Daemon socket permissions: owner-only (0600)",
                status,
                detail,
            ));
        } else {
            results.push(Check::new(
                "S4",
                "S4.1",
                "Daemon socket permissions: owner-only (0600)",
                Status::Skip,
                "Daemon socket does not exist (daemon not running)".to_string(),
            ));
        }

        if pid_path.exists() {
            let (status, detail) = check_mode(&pid_path, 0o600);
            results.push(Check::new(
                "S4",
                "S4.3",
                "PID file permissions: owner-only (0600)",
                status,
                detail,
            ));

            // Check PID file format: should have at least PID + timestamp
            match fs::read_to_string(&pid_path) {
                Ok(content) => {
                    let lines: Vec<&str> = content.lines().collect();
                    if lines.len() >= 3 {
                        results.push(Check::new(
                            "S4",
                            "S4.3",
                            "PID file format: pid + timestamp + exe_hash",
                            Status::Pass,
                            format!("PID file has {} lines (expected ≥3) ✓", lines.len()),
                        ));
                    } else if lines.len() == 1 {
                        results.push(Check::new(
                            "S4",
                            "S4.3",
                            "PID file format: pid + timestamp + exe_hash",
                            Status::Warn,
                            "PID file has only PID (no timestamp or exe hash — legacy format)".to_string(),
                        ));
                    } else {
                        results.push(Check::new(
                            "S4",
                            "S4.3",
                            "PID file format: pid + timestamp + exe_hash",
                            Status::Warn,
                            format!("PID file has {} lines", lines.len()),
                        ));
                    }
                }
                Err(e) => {
                    results.push(Check::new(
                        "S4",
                        "S4.3",
                        "PID file format: pid + timestamp + exe_hash",
                        Status::Fail,
                        format!("Cannot read PID file: {}", e),
                    ));
                }
            }
        } else {
            results.push(Check::new(
                "S4",
                "S4.3",
                "PID file permissions and format",
                Status::Skip,
                "PID file does not exist (daemon not running)".to_string(),
            ));
        }

        // Check daemon directory permissions
        if dir.exists() {
            let (status, detail) = check_mode(dir, 0o700);
            results.push(Check::new(
                "S4",
                "S4.1",
                "Daemon directory permissions: owner-only (0700)",
                status,
                detail,
            ));
        }
    } else {
        results.push(Check::new(
            "S4",
            "S4.1",
            "Daemon directory",
            Status::Skip,
            "Could not determine daemon directory for this platform".to_string(),
        ));
    }
}

// ─── Source code checks ──────────────────────────────────────────────────────

fn check_source(results: &mut Vec<Check>) {
    // Check if encryption module exists in the source tree
    let crypto_mod = Path::new("crates/uffs-mft/src/cache/crypto.rs");
    let keystore_mod = Path::new("crates/uffs-mft/src/cache/keystore.rs");
    let cache_mod_dir = Path::new("crates/uffs-mft/src/cache");
    let cache_single = Path::new("crates/uffs-mft/src/cache.rs");

    // Check if cache has been refactored into module dir
    if cache_mod_dir.is_dir() {
        results.push(Check::new(
            "S2",
            "S2.1",
            "cache.rs refactored to cache/ module directory",
            Status::Pass,
            "crates/uffs-mft/src/cache/ exists ✓".to_string(),
        ));
    } else if cache_single.exists() {
        results.push(Check::new(
            "S2",
            "S2.1",
            "cache.rs refactored to cache/ module directory",
            Status::NotImpl,
            "Still single cache.rs — not yet refactored".to_string(),
        ));
    }

    if crypto_mod.exists() {
        results.push(Check::new(
            "S2",
            "S2.1",
            "Crypto module exists (cache/crypto.rs)",
            Status::Pass,
            "cache/crypto.rs found ✓".to_string(),
        ));
    } else {
        results.push(Check::new(
            "S2",
            "S2.1",
            "Crypto module exists (cache/crypto.rs)",
            Status::NotImpl,
            "cache/crypto.rs not found — encryption not yet implemented".to_string(),
        ));
    }

    if keystore_mod.exists() {
        results.push(Check::new(
            "S2",
            "S2.1",
            "Keystore module exists (cache/keystore.rs)",
            Status::Pass,
            "cache/keystore.rs found ✓".to_string(),
        ));
    } else {
        results.push(Check::new(
            "S2",
            "S2.1",
            "Keystore module exists (cache/keystore.rs)",
            Status::NotImpl,
            "cache/keystore.rs not found — key storage not yet implemented".to_string(),
        ));
    }

    // Check Cargo.toml for security dependencies
    let cargo_toml = Path::new("Cargo.toml");
    if let Ok(content) = fs::read_to_string(cargo_toml) {
        let has_aes_gcm = content.contains("aes-gcm");
        let has_rand = content.contains("rand");

        if has_aes_gcm {
            results.push(Check::new(
                "S2",
                "S2.1",
                "aes-gcm dependency in workspace Cargo.toml",
                Status::Pass,
                "aes-gcm found ✓".to_string(),
            ));
        } else {
            results.push(Check::new(
                "S2",
                "S2.1",
                "aes-gcm dependency in workspace Cargo.toml",
                Status::NotImpl,
                "aes-gcm not found — not yet added".to_string(),
            ));
        }

        if has_rand {
            results.push(Check::new(
                "S2",
                "S2.1",
                "rand dependency in workspace Cargo.toml",
                Status::Pass,
                "rand found ✓".to_string(),
            ));
        } else {
            results.push(Check::new(
                "S2",
                "S2.1",
                "rand dependency in workspace Cargo.toml",
                Status::NotImpl,
                "rand not found — not yet added".to_string(),
            ));
        }
    }

    // Check if cache.rs still uses temp_dir (legacy pattern)
    if let Ok(content) = fs::read_to_string(cache_single) {
        if content.contains("temp_dir()") {
            results.push(Check::new(
                "S1",
                "S1.1",
                "cache.rs no longer uses temp_dir() for cache location",
                Status::Fail,
                "cache.rs still references temp_dir() — not yet migrated".to_string(),
            ));
        } else {
            results.push(Check::new(
                "S1",
                "S1.1",
                "cache.rs no longer uses temp_dir() for cache location",
                Status::Pass,
                "No temp_dir() references ✓".to_string(),
            ));
        }

        // Check for plain fs::remove_file (should be secure_remove)
        let plain_removes = content.matches("fs::remove_file").count();
        let secure_removes = content.matches("secure_remove").count();
        if plain_removes > 0 && secure_removes == 0 {
            results.push(Check::new(
                "S3",
                "S3.1",
                "Cache removal uses secure_remove() (zero-overwrite)",
                Status::NotImpl,
                format!(
                    "{} calls to fs::remove_file, 0 calls to secure_remove — not yet implemented",
                    plain_removes
                ),
            ));
        } else if secure_removes > 0 {
            results.push(Check::new(
                "S3",
                "S3.1",
                "Cache removal uses secure_remove() (zero-overwrite)",
                Status::Pass,
                format!("{} secure_remove() calls ✓", secure_removes),
            ));
        }

        // Check for atomic writes
        if content.contains("atomic_write") || content.contains("with_extension(\"uffs.tmp\")") {
            results.push(Check::new(
                "S1",
                "S1.3",
                "Cache writes use atomic write (temp + rename)",
                Status::Pass,
                "atomic_write pattern found ✓".to_string(),
            ));
        } else {
            results.push(Check::new(
                "S1",
                "S1.3",
                "Cache writes use atomic write (temp + rename)",
                Status::NotImpl,
                "No atomic_write pattern found — not yet implemented".to_string(),
            ));
        }

        // Check for file locking
        if content.contains("with_cache_lock") || content.contains("flock") || content.contains("LockFileEx") {
            results.push(Check::new(
                "S3",
                "S3.2",
                "Cache operations use file locking",
                Status::Pass,
                "File locking pattern found ✓".to_string(),
            ));
        } else {
            results.push(Check::new(
                "S3",
                "S3.2",
                "Cache operations use file locking",
                Status::NotImpl,
                "No file locking found — not yet implemented".to_string(),
            ));
        }
    }

    // Check file_io.rs for encryption integration
    let file_io = Path::new("crates/uffs-mft/src/index/storage/file_io.rs");
    if let Ok(content) = fs::read_to_string(file_io) {
        if content.contains("encrypt") || content.contains("decrypt") {
            results.push(Check::new(
                "S2",
                "S2.4",
                "file_io.rs integrates encrypt/decrypt on save/load",
                Status::Pass,
                "encrypt/decrypt calls found in file_io.rs ✓".to_string(),
            ));
        } else {
            results.push(Check::new(
                "S2",
                "S2.4",
                "file_io.rs integrates encrypt/decrypt on save/load",
                Status::NotImpl,
                "file_io.rs has plain read/write — encryption not yet wired in".to_string(),
            ));
        }
    }
}

// ─── Main ────────────────────────────────────────────────────────────────────

fn main() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║           UFFS Security Posture Audit                       ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  Platform: {:<49}║", platform_name());
    println!("║  Date:     {:<49}║", chrono_now());
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();

    let mut results = Vec::new();

    // Run all checks
    check_s1(&mut results);
    check_s2(&mut results);
    check_s4(&mut results);
    check_source(&mut results);

    // Print results grouped by phase
    let phases = ["S1", "S2", "S3", "S4"];
    let phase_names = [
        "Secure Foundation",
        "Encryption at Rest",
        "Secure Lifecycle",
        "Daemon IPC Hardening",
    ];

    for (phase, name) in phases.iter().zip(phase_names.iter()) {
        let phase_results: Vec<&Check> = results.iter().filter(|c| c.phase == *phase).collect();
        if phase_results.is_empty() {
            continue;
        }

        println!("── Phase {}: {} ──", phase, name);
        println!();

        for check in &phase_results {
            println!(
                "  {} [{}] {}: {}",
                status_icon(check.status),
                check.id,
                check.description,
                check.detail
            );
        }
        println!();
    }

    // Summary
    let total = results.len();
    let pass = results.iter().filter(|c| c.status == Status::Pass).count();
    let fail = results.iter().filter(|c| c.status == Status::Fail).count();
    let warn = results.iter().filter(|c| c.status == Status::Warn).count();
    let skip = results.iter().filter(|c| c.status == Status::Skip).count();
    let not_impl = results.iter().filter(|c| c.status == Status::NotImpl).count();

    println!("══════════════════════════════════════════════════════════════");
    println!("  SUMMARY");
    println!("══════════════════════════════════════════════════════════════");
    println!("  Total checks:  {}", total);
    println!("  ✅ Pass:        {}", pass);
    println!("  ❌ Fail:        {}", fail);
    println!("  ⚠️  Warn:        {}", warn);
    println!("  ⏭️  Skip:        {}", skip);
    println!("  🔲 Not Impl:    {}", not_impl);
    println!();

    // Security score
    let applicable = total - skip;
    let score = if applicable > 0 {
        (pass as f64 / applicable as f64 * 100.0) as u32
    } else {
        0
    };

    let grade = match score {
        90..=100 => "A",
        75..=89 => "B",
        60..=74 => "C",
        40..=59 => "D",
        _ => "F",
    };

    println!(
        "  Security Score: {}% (grade: {}) — {}/{} applicable checks pass",
        score, grade, pass, applicable
    );
    println!();

    if fail > 0 {
        println!("  🚨 ACTION REQUIRED: {} failing checks need attention", fail);
    }
    if not_impl > 0 {
        println!(
            "  📋 {} checks are NOT YET IMPLEMENTED (see SECURITY_IMPLEMENTATION_PLAN.md)",
            not_impl
        );
    }
    if fail == 0 && not_impl == 0 {
        println!("  🎉 All security measures are in place!");
    }
    println!();
}

/// Simple timestamp without pulling in chrono.
fn chrono_now() -> String {
    use std::time::SystemTime;
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => {
            let secs = d.as_secs();
            // Very rough UTC datetime (good enough for a report header)
            let days = secs / 86400;
            let time_secs = secs % 86400;
            let hours = time_secs / 3600;
            let mins = (time_secs % 3600) / 60;

            // Days since epoch to Y-M-D (simplified)
            let mut y = 1970;
            let mut remaining_days = days;
            loop {
                let days_in_year = if is_leap(y) { 366 } else { 365 };
                if remaining_days < days_in_year {
                    break;
                }
                remaining_days -= days_in_year;
                y += 1;
            }
            let month_days: [u64; 12] = if is_leap(y) {
                [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
            } else {
                [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
            };
            let mut m = 0;
            for (i, &md) in month_days.iter().enumerate() {
                if remaining_days < md {
                    m = i + 1;
                    break;
                }
                remaining_days -= md;
            }
            let d = remaining_days + 1;
            format!("{:04}-{:02}-{:02} {:02}:{:02} UTC", y, m, d, hours, mins)
        }
        Err(_) => "unknown".to_string(),
    }
}

const fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}
