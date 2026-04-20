// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Detect the kind of destination `stdout` is connected to.
//!
//! Used by the thin CLI to choose the cheapest output strategy for the
//! current invocation:
//!
//! - [`StdoutKind::Null`] — `> NUL` on Windows or `> /dev/null` on Unix. The
//!   CLI asks the daemon to skip row materialisation + `paths_blob`
//!   construction + IPC row transfer entirely (saves ~20-30 ms on medium result
//!   sets by avoiding a 3.5 MB pipe transfer whose bytes would be discarded
//!   anyway).
//! - [`StdoutKind::Terminal`] — interactive console / TTY.  Prefer a single
//!   large write over per-line `writeln!` to minimise console syscall count
//!   (Phase 3.2 in `docs/research/perf-phase3-output-optimization.md`).
//! - [`StdoutKind::Pipe`] / [`StdoutKind::File`] — redirected output. Either a
//!   `BufWriter` or a single-buffer render works; used as a fallback label when
//!   neither `Null` nor `Terminal` applies.
//! - [`StdoutKind::Unknown`] — detection failed for any reason.  Treat as
//!   `Pipe` (the safe default: no NUL short-circuit, no TTY-specific batching).
//!
//! The module is intentionally zero-cost: only the entry point
//! [`StdoutKind::detect`] reaches out to the OS, and it is called once
//! per CLI invocation.

/// Classification of the process's standard output destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdoutKind {
    /// Interactive terminal / console (TTY).
    Terminal,
    /// Redirected to a regular file (e.g. `> out.csv`).
    File,
    /// Redirected to a pipe (e.g. `| head`, or a Windows named pipe).
    Pipe,
    /// Redirected to the null device (`> NUL` on Windows, `> /dev/null`
    /// on Unix) — output would be discarded by the kernel.
    Null,
    /// Detection failed or the handle is an OS-specific shape not
    /// covered by the other variants.  Callers should treat this the
    /// same as [`Self::Pipe`] (safe default: no NUL fast path, no
    /// TTY-only batching).
    Unknown,
}

impl StdoutKind {
    /// Detect the kind of destination `stdout` is currently connected to.
    ///
    /// Performs one or two syscalls on first call; the result is not
    /// cached because the process's stdout handle can theoretically be
    /// replaced at runtime (e.g. via `dup2`).  In practice the CLI
    /// calls this exactly once at entry.
    #[must_use]
    pub fn detect() -> Self {
        #[cfg(unix)]
        {
            platform_unix::detect()
        }
        #[cfg(windows)]
        {
            platform_windows::detect()
        }
        #[cfg(not(any(unix, windows)))]
        {
            Self::Unknown
        }
    }

    /// Returns `true` when stdout is the null device (output discarded).
    #[must_use]
    pub const fn is_null(self) -> bool {
        matches!(self, Self::Null)
    }

    /// Returns `true` when stdout is an interactive terminal.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Terminal)
    }
}

// ── Unix implementation ────────────────────────────────────────────────
//
// Uses `fstat` on fd 1 to read the mode bits.  When stdout is a
// character device (e.g. a TTY or `/dev/null`), cross-checks
// `st_rdev` against `stat("/dev/null")` to distinguish NUL from a
// real terminal — `isatty(3)` is already folded into this via the
// `S_IFCHR` + `st_rdev` comparison chain.

/// Unix-specific stdout classifier: `isatty(1)` + `fstat(1)` + device-id
/// match against `/dev/null`.
#[cfg(unix)]
mod platform_unix {
    use std::io::IsTerminal as _;
    use std::os::fd::AsRawFd as _;

    use super::StdoutKind;

    /// Platform entry point invoked by [`super::StdoutKind::detect`].
    pub(super) fn detect() -> StdoutKind {
        // Fast TTY check via the stdlib — covers xterm, Windows Terminal
        // over WSL, Apple Terminal, etc.  This is just `isatty(1)` on
        // Unix but avoids re-implementing it per libc crate.
        if std::io::stdout().is_terminal() {
            return StdoutKind::Terminal;
        }

        let fd = std::io::stdout().as_raw_fd();
        detect_for_fd(fd)
    }

    /// Classify a file descriptor.  Extracted so unit tests can point it
    /// at an arbitrary fd (tempfile, pipe, `/dev/null`, etc.) without
    /// disturbing the process's real stdout.
    #[expect(unsafe_code, reason = "FFI to libc::fstat / libc::stat")]
    pub(crate) fn detect_for_fd(fd: std::os::fd::RawFd) -> StdoutKind {
        // SAFETY: zero-initialising POD (`libc::stat`) — all fields are
        // plain integers; zero is a valid bit pattern for each of them.
        let mut st: libc::stat = unsafe { core::mem::zeroed() };
        // SAFETY: `fstat` writes to our valid stack-allocated `stat`
        // struct; any fd error is surfaced through the return value.
        let fstat_rc = unsafe { libc::fstat(fd, &raw mut st) };
        if fstat_rc != 0_i32 {
            return StdoutKind::Unknown;
        }

        let ifmt = st.st_mode & libc::S_IFMT;
        if ifmt == libc::S_IFREG {
            return StdoutKind::File;
        }
        if ifmt == libc::S_IFIFO || ifmt == libc::S_IFSOCK {
            return StdoutKind::Pipe;
        }
        if ifmt != libc::S_IFCHR {
            // Block device, directory, or something exotic — treat as
            // unknown so the CLI falls back to the safe default.
            return StdoutKind::Unknown;
        }

        // Character device: could be a TTY (already excluded above in
        // `detect`, but `detect_for_fd` is called without that prefix)
        // or `/dev/null`.  Compare the device id to `/dev/null`'s.
        //
        // SAFETY: zero-initialising POD (`libc::stat`).
        let mut null_st: libc::stat = unsafe { core::mem::zeroed() };
        // SAFETY: `c"/dev/null"` is a well-formed NUL-terminated C
        // string; `stat` writes into our valid stack allocation.
        let stat_rc = unsafe { libc::stat(c"/dev/null".as_ptr(), &raw mut null_st) };
        if stat_rc == 0_i32 && st.st_rdev == null_st.st_rdev {
            return StdoutKind::Null;
        }

        // Some other character device (e.g. `/dev/tty` when
        // `is_terminal()` returned `false` for odd reasons).  Return
        // `Terminal` conservatively — the caller will treat it as
        // interactive, which is the safer choice than accidentally
        // suppressing output.
        StdoutKind::Terminal
    }

    #[cfg(test)]
    mod tests {
        use std::io::Write as _;
        use std::os::fd::AsRawFd as _;

        use super::{StdoutKind, detect_for_fd};

        #[test]
        fn regular_file_is_classified_as_file() {
            let tmp = tempfile::NamedTempFile::new().expect("create tempfile");
            let fd = tmp.as_file().as_raw_fd();
            assert_eq!(detect_for_fd(fd), StdoutKind::File);
        }

        #[test]
        fn dev_null_is_classified_as_null() {
            let null = std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/null")
                .expect("open /dev/null");
            assert_eq!(detect_for_fd(null.as_raw_fd()), StdoutKind::Null);
        }

        #[test]
        #[expect(
            unsafe_code,
            reason = "FFI to libc::pipe / libc::close for a test fixture"
        )]
        fn pipe_is_classified_as_pipe() {
            // `libc::pipe` returns a pair of fds; the write end is a
            // FIFO, which is the shape `|` produces between shell
            // stages.
            let mut fds = [0_i32; 2];
            // SAFETY: pipe writes two valid fds into `fds`.
            let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
            assert_eq!(rc, 0_i32, "libc::pipe must succeed");
            let kind = detect_for_fd(fds[1]);
            // SAFETY: closing the read-end fd we own.  Binding the
            // result (even though we ignore errors) keeps the `unsafe`
            // block a pure expression, which satisfies both
            // `semicolon-{inside,outside}-block` clippy rules at once.
            // Each close gets its own block to satisfy
            // `multiple-unsafe-ops-per-block`.
            let _close_read_rc: libc::c_int = unsafe { libc::close(fds[0]) };
            // SAFETY: closing the write-end fd we own.
            let _close_write_rc: libc::c_int = unsafe { libc::close(fds[1]) };
            assert_eq!(kind, StdoutKind::Pipe);
        }

        #[test]
        fn invalid_fd_is_classified_as_unknown() {
            // fd -1 is always invalid.  `fstat` returns -1 / EBADF.
            assert_eq!(detect_for_fd(-1), StdoutKind::Unknown);
        }

        #[test]
        fn write_to_detected_file_still_works() {
            // Sanity: detection does not perturb the fd it inspects.
            let mut tmp = tempfile::NamedTempFile::new().expect("create tempfile");
            let fd = tmp.as_file().as_raw_fd();
            assert_eq!(detect_for_fd(fd), StdoutKind::File);
            tmp.write_all(b"hello\n").expect("write after detect");
        }
    }
}

// ── Windows implementation ─────────────────────────────────────────────
//
// `GetFileType` classifies the handle into DISK / PIPE / CHAR / UNKNOWN.
// Only `CHAR` is ambiguous (console vs NUL vs other character device) —
// we resolve it by calling `GetConsoleMode`, which succeeds for a real
// console handle and fails for NUL.

/// Windows-specific stdout classifier via `GetFileType` + `GetConsoleMode`.
#[cfg(windows)]
mod platform_windows {
    use std::os::windows::io::AsRawHandle as _;

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_TYPE_CHAR, FILE_TYPE_DISK, FILE_TYPE_PIPE, FILE_TYPE_UNKNOWN, GetFileType,
    };
    use windows::Win32::System::Console::{CONSOLE_MODE, GetConsoleMode};

    use super::StdoutKind;

    /// Platform entry point invoked by [`super::StdoutKind::detect`].
    pub(super) fn detect() -> StdoutKind {
        let handle = HANDLE(std::io::stdout().as_raw_handle().cast());
        detect_for_handle(handle)
    }

    /// Classify a Win32 handle.  Extracted so unit tests can point it
    /// at a tempfile / pipe handle without disturbing the real stdout.
    #[expect(unsafe_code, reason = "FFI to GetFileType / GetConsoleMode")]
    pub(crate) fn detect_for_handle(handle: HANDLE) -> StdoutKind {
        if handle.is_invalid() {
            return StdoutKind::Unknown;
        }

        // SAFETY: `GetFileType` is read-only and accepts any HANDLE,
        // returning `FILE_TYPE_UNKNOWN` for ones it can't classify.
        let ftype = unsafe { GetFileType(handle) };

        if ftype == FILE_TYPE_DISK {
            return StdoutKind::File;
        }
        if ftype == FILE_TYPE_PIPE {
            return StdoutKind::Pipe;
        }
        if ftype == FILE_TYPE_UNKNOWN {
            return StdoutKind::Unknown;
        }
        if ftype != FILE_TYPE_CHAR {
            return StdoutKind::Unknown;
        }

        // FILE_TYPE_CHAR: console, NUL, or another character device.
        // `GetConsoleMode` succeeds only for a real console handle.
        let mut mode = CONSOLE_MODE::default();
        // SAFETY: handle is valid (checked above), mode is a valid
        // out-param pointer.
        let is_console = unsafe { GetConsoleMode(handle, &raw mut mode) }.is_ok();
        if is_console {
            StdoutKind::Terminal
        } else {
            // Character device that is not a console — on Windows this
            // is overwhelmingly `\\.\NUL`.  Treat as Null to enable the
            // output-skip fast path.  If someone ever redirects stdout
            // to `\\.\COM1` or similar, the worst outcome is suppressed
            // output when they wanted to see it, which is the same as
            // `> NUL` would be for them anyway.
            StdoutKind::Null
        }
    }

    #[cfg(test)]
    mod tests {
        use std::fs::OpenOptions;
        use std::os::windows::io::AsRawHandle as _;

        use windows::Win32::Foundation::HANDLE;

        use super::{StdoutKind, detect_for_handle};

        #[test]
        fn regular_file_is_classified_as_file() {
            let tmp = tempfile::NamedTempFile::new().expect("create tempfile");
            let handle = HANDLE(tmp.as_file().as_raw_handle().cast());
            assert_eq!(detect_for_handle(handle), StdoutKind::File);
        }

        #[test]
        fn nul_device_is_classified_as_null() {
            let nul = OpenOptions::new()
                .write(true)
                .open("NUL")
                .expect("open NUL");
            let handle = HANDLE(nul.as_raw_handle().cast());
            assert_eq!(detect_for_handle(handle), StdoutKind::Null);
        }

        #[test]
        fn invalid_handle_is_classified_as_unknown() {
            assert_eq!(detect_for_handle(HANDLE::default()), StdoutKind::Unknown);
        }
    }
}

#[cfg(test)]
mod shared_tests {
    use super::StdoutKind;

    #[test]
    fn is_null_matches_enum() {
        assert!(StdoutKind::Null.is_null());
        assert!(!StdoutKind::Terminal.is_null());
        assert!(!StdoutKind::File.is_null());
        assert!(!StdoutKind::Pipe.is_null());
        assert!(!StdoutKind::Unknown.is_null());
    }

    #[test]
    fn is_terminal_matches_enum() {
        assert!(StdoutKind::Terminal.is_terminal());
        assert!(!StdoutKind::Null.is_terminal());
        assert!(!StdoutKind::File.is_terminal());
        assert!(!StdoutKind::Pipe.is_terminal());
        assert!(!StdoutKind::Unknown.is_terminal());
    }

    /// `detect()` must never panic regardless of what stdout is pointing
    /// at in the test harness (typically a pipe under `cargo test`).
    #[test]
    fn detect_does_not_panic() {
        let _kind = StdoutKind::detect();
    }
}
