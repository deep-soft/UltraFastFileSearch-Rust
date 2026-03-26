//! Secure filesystem operations: atomic writes, secure delete, permissions,
//! file locking.
//!
//! These primitives are used by `uffs-mft` (cache), `uffs-daemon` (socket dir,
//! PID file), and any other crate that needs secure file handling.
//!
//! # Platform Support
//!
//! | Function | macOS | Linux | Windows |
//! |----------|-------|-------|---------|
//! | `create_secure_dir` | 0700 | 0700 | inherits parent ACL + read-only attr |
//! | `set_file_permissions_owner_only` | 0600 | 0600 | read-only attribute |
//! | `atomic_write` | rename | rename | rename (MoveFileExW) |
//! | `secure_remove` | zero+delete | zero+delete | zero+delete |
//! | `FileLock` | flock | flock | LockFileEx |

use std::io;
use std::path::Path;

// ────────────────────────────────────────────────────────────────────────────
// Directory & File Permissions (S1.2)
// ────────────────────────────────────────────────────────────────────────────

/// Creates a directory (and parents) with owner-only permissions.
///
/// - **Unix** (macOS + Linux): mode `0700` (`drwx------`)
/// - **Windows**: creates the directory; sets read-only attribute as a basic
///   protection layer (full DACL requires elevated context)
///
/// # Errors
///
/// Returns an error if directory creation or permission setting fails.
pub fn create_secure_dir(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)?;

    #[cfg(unix)]
    return {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    };

    #[cfg(windows)]
    return {
        // Try icacls first — works without elevation, sets proper DACL
        if !win_set_owner_only_acl(path) {
            // Fallback: at least mark hidden
            win_set_hidden(path)?;
        }
        Ok(())
    };
}

/// Sets a file's permissions to owner-only (read+write).
///
/// - **Unix** (macOS + Linux): mode `0600` (`-rw-------`)
/// - **Windows**: sets read-only attribute removed (writable by owner); marks
///   hidden to discourage casual access
///
/// # Errors
///
/// Returns an error if permission setting fails.
pub fn set_file_permissions_owner_only(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    };

    #[cfg(windows)]
    return {
        // Ensure writable
        let meta = std::fs::metadata(path)?;
        let mut perms = meta.permissions();
        if perms.readonly() {
            perms.set_readonly(false);
            std::fs::set_permissions(path, perms)?;
        }
        // Try proper DACL, fall back to hidden
        if !win_set_owner_only_acl(path) {
            win_set_hidden(path)?;
        }
        Ok(())
    };
}

/// Windows: set the `FILE_ATTRIBUTE_HIDDEN` flag on a path.
#[cfg(windows)]
fn win_set_hidden(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    // SAFETY: GetFileAttributesW / SetFileAttributesW are well-defined Win32 APIs.
    #[expect(unsafe_code, reason = "Win32 file attribute APIs require unsafe FFI")]
    unsafe {
        use windows::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_HIDDEN, FILE_FLAGS_AND_ATTRIBUTES, GetFileAttributesW,
            SetFileAttributesW,
        };
        use windows::core::PCWSTR;

        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let pcwstr = PCWSTR(wide.as_ptr());

        let current = GetFileAttributesW(pcwstr);
        if current != u32::MAX {
            let _ok = SetFileAttributesW(
                pcwstr,
                FILE_FLAGS_AND_ATTRIBUTES(current | FILE_ATTRIBUTE_HIDDEN.0),
            );
        }
    }
    Ok(())
}

/// Windows: set owner-only ACL via `icacls` command.
///
/// S1.2.6: Equivalent to `icacls path /inheritance:r /grant:r
/// %USERNAME%:(OI)(CI)F` which removes inherited ACEs and grants only the
/// current user full control. Returns `true` on success.
#[cfg(windows)]
fn win_set_owner_only_acl(path: &Path) -> bool {
    let username = std::env::var("USERNAME").unwrap_or_default();
    if username.is_empty() {
        return false;
    }

    let path_str = path.to_string_lossy();

    // Remove inherited permissions
    let inherit_result = std::process::Command::new("icacls")
        .args([path_str.as_ref(), "/inheritance:r"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    if inherit_result.map_or(false, |s| s.success()) {
        // Grant only current user full control (with Object Inherit + Container
        // Inherit)
        let grant_arg = format!("{username}:(OI)(CI)F");
        let grant_result = std::process::Command::new("icacls")
            .args([path_str.as_ref(), "/grant:r", &grant_arg])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        return grant_result.map_or(false, |s| s.success());
    }

    false
}

// ────────────────────────────────────────────────────────────────────────────
// Atomic Writes (S1.3)
// ────────────────────────────────────────────────────────────────────────────

/// Writes data atomically: write to `.uffs.tmp`, `sync_all()`, rename over
/// the target.
///
/// If the process is killed mid-write, the original file remains intact.
/// Stale `.uffs.tmp` files are cleaned up on the next `cache_dir()` call.
///
/// Works on all platforms: POSIX `rename` is atomic on the same filesystem;
/// on Windows `std::fs::rename` uses `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`.
///
/// # Errors
///
/// Returns an error if writing, syncing, or renaming fails.
pub fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    use std::io::Write;

    let tmp_path = path.with_extension("uffs.tmp");

    let mut file = std::fs::File::create(&tmp_path)?;
    file.write_all(data)?;
    file.sync_all()?;
    drop(file);

    set_file_permissions_owner_only(&tmp_path)?;
    std::fs::rename(&tmp_path, path)?;

    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// Secure Wipe (S3.1) — fully cross-platform
// ────────────────────────────────────────────────────────────────────────────

/// Securely removes a file: zero-overwrite, `sync_all()`, then delete.
///
/// On HDD this overwrites the data sectors with zeros before unlinking.
/// On SSD the overwrite is best-effort (wear-leveling may retain old data),
/// but combined with encryption (S2) the plaintext is unrecoverable.
///
/// Does nothing if the file doesn't exist.
/// Works identically on macOS, Linux, and Windows.
///
/// # Errors
///
/// Returns an error if overwriting, syncing, or removal fails.
pub fn secure_remove(path: &Path) -> io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};

    /// Size of the zero-fill buffer for secure wipe.
    const ZERO_BUF_SIZE: usize = 64 * 1024;

    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };

    let file_len = meta.len();

    // On Windows, ensure the file isn't read-only before we try to write
    #[cfg(windows)]
    {
        let mut perms = meta.permissions();
        if perms.readonly() {
            perms.set_readonly(false);
            std::fs::set_permissions(path, perms)?;
        }
    }

    let mut file = std::fs::OpenOptions::new().write(true).open(path)?;

    let zeros = vec![0_u8; ZERO_BUF_SIZE];
    let mut remaining = file_len;

    file.seek(SeekFrom::Start(0))?;
    while remaining > 0 {
        let chunk = if remaining >= ZERO_BUF_SIZE as u64 {
            ZERO_BUF_SIZE
        } else {
            usize::try_from(remaining).unwrap_or(ZERO_BUF_SIZE)
        };
        let buf = zeros
            .get(..chunk)
            .ok_or_else(|| io::Error::other("zero buffer slice out of bounds"))?;
        file.write_all(buf)?;
        remaining -= chunk as u64;
    }

    file.sync_all()?;
    drop(file);

    std::fs::remove_file(path)
}

// ────────────────────────────────────────────────────────────────────────────
// File Locking (S3.2)
// ────────────────────────────────────────────────────────────────────────────

/// Advisory file lock type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockKind {
    /// Shared (read) lock — multiple readers allowed.
    Shared,
    /// Exclusive (write) lock — single writer, no readers.
    Exclusive,
}

/// An advisory file lock backed by a `.lock` file.
///
/// The lock is released when this struct is dropped (closing the fd/handle
/// releases the flock/`LockFileEx` lock automatically).
///
/// Works on all platforms: `flock` (macOS + Linux), `LockFileEx` (Windows).
pub struct FileLock {
    /// Kept open for the lifetime of the lock.
    _file: std::fs::File,
}

impl FileLock {
    /// Acquires an advisory lock on `lock_path`.
    ///
    /// Creates the lock file if it doesn't exist. Blocks up to `timeout`
    /// with a spin-sleep retry loop.
    ///
    /// # Errors
    ///
    /// Returns `io::ErrorKind::TimedOut` if the lock cannot be acquired
    /// within the timeout.
    #[cfg(unix)]
    pub fn acquire(
        lock_path: &Path,
        kind: LockKind,
        timeout: core::time::Duration,
    ) -> io::Result<Self> {
        use std::os::unix::io::AsRawFd;

        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(lock_path)?;

        let deadline = std::time::Instant::now() + timeout;
        let sleep_step = core::time::Duration::from_millis(50);

        let operation = match kind {
            LockKind::Shared => libc::LOCK_SH | libc::LOCK_NB,
            LockKind::Exclusive => libc::LOCK_EX | libc::LOCK_NB,
        };

        loop {
            // SAFETY: flock is a well-defined POSIX syscall operating on a valid fd.
            #[expect(unsafe_code, reason = "flock requires unsafe FFI call")]
            let result = unsafe { libc::flock(file.as_raw_fd(), operation) };

            if result == 0 {
                return Ok(Self { _file: file });
            }

            let lock_err = io::Error::last_os_error();
            let is_contention = lock_err.kind() == io::ErrorKind::WouldBlock
                || lock_err.raw_os_error() == Some(libc::EWOULDBLOCK)
                || lock_err.raw_os_error() == Some(libc::EAGAIN);

            if is_contention {
                if std::time::Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "could not acquire {kind:?} lock on {} within {}s",
                            lock_path.display(),
                            timeout.as_secs()
                        ),
                    ));
                }
                std::thread::sleep(sleep_step);
            } else {
                return Err(lock_err);
            }
        }
    }

    /// Acquire a file lock on Windows using `LockFileEx`.
    #[cfg(windows)]
    pub fn acquire(
        lock_path: &Path,
        kind: LockKind,
        timeout: core::time::Duration,
    ) -> io::Result<Self> {
        use std::os::windows::io::AsRawHandle;

        /// Windows error code for lock contention.
        const ERROR_LOCK_VIOLATION: i32 = 33;

        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .read(true)
            .open(lock_path)?;

        let deadline = std::time::Instant::now() + timeout;
        let sleep_step = core::time::Duration::from_millis(50);

        loop {
            // SAFETY: LockFileEx is a well-defined Win32 API operating on a valid handle.
            #[expect(unsafe_code, reason = "LockFileEx requires unsafe FFI call")]
            let lock_result = unsafe {
                use windows::Win32::Foundation::HANDLE;
                use windows::Win32::Storage::FileSystem::{
                    LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
                };

                let mut overlapped: windows::Win32::System::IO::OVERLAPPED = std::mem::zeroed();
                let mut flags = LOCKFILE_FAIL_IMMEDIATELY;
                if kind == LockKind::Exclusive {
                    flags |= LOCKFILE_EXCLUSIVE_LOCK;
                }

                let handle = HANDLE(file.as_raw_handle() as _);
                LockFileEx(handle, flags, Some(0), u32::MAX, u32::MAX, &mut overlapped)
            };

            if lock_result.is_ok() {
                return Ok(Self { _file: file });
            }

            let lock_err = io::Error::last_os_error();
            let is_contention = lock_err.kind() == io::ErrorKind::WouldBlock
                || lock_err.raw_os_error() == Some(ERROR_LOCK_VIOLATION);

            if is_contention {
                if std::time::Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "could not acquire {kind:?} lock on {} within {}s",
                            lock_path.display(),
                            timeout.as_secs()
                        ),
                    ));
                }
                std::thread::sleep(sleep_step);
            } else {
                return Err(lock_err);
            }
        }
    }
}

/// Runs a closure while holding an advisory file lock.
///
/// Creates a `.lock` file at `lock_path`, acquires the lock, runs `func`,
/// then releases the lock when the guard drops.
///
/// # Errors
///
/// Returns an error if the lock cannot be acquired within `timeout`, or if
/// `func` returns an error.
pub fn with_file_lock<F, T>(
    lock_path: &Path,
    kind: LockKind,
    timeout: core::time::Duration,
    func: F,
) -> io::Result<T>
where
    F: FnOnce() -> io::Result<T>,
{
    let _guard = FileLock::acquire(lock_path, kind, timeout)?;
    func()
}
