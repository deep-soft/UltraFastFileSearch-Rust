<!--
SPDX-License-Identifier: MPL-2.0
Copyright (c) 2025-2026 SKY, LLC.

UFFS - Ultra Fast File Search
-->

# Changelog

All notable changes to UFFS will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Phase 3 output-path optimization** (`docs/research/perf-phase3-output-optimization.md`)
  - **3.1 NUL fast path** — CLI detects `> NUL` / `> /dev/null` via the new
    `uffs_client::stdout_kind` module (Unix `fstat` + `/dev/null` device-id
    match; Windows `GetFileType` + `GetConsoleMode`) and auto-injects
    `--no-output`.  The daemon gates `SearchRow` materialisation on
    `include_rows`, so `paths_blob` packing, shmem offload, and IPC row
    transfer all no-op on suppressed queries.  Expected saving: 20–30 ms
    on medium result sets piped to NUL.
  - **3.2 Single-buffer multi-column console render** — the console branch
    of `write_native_results` now renders CSV / JSON / table / parity output
    into a `Vec<u8>` and issues one `stdout.lock().write_all`, replacing the
    previous `BufWriter<StdoutLock>` + per-row `writeln!` pattern.  Guarded
    by a 50 MiB cap via the pure `choose_console_strategy(row_count, cap,
    est)` helper — falls back to streaming on pathological result sets.
  - **3.3 Windows `WriteConsoleW` direct path** — when stdout is a real
    console on Windows, `uffs_client::stdout_kind::write_stdout_buffer`
    transcodes the rendered buffer to UTF-16 once and issues chunked
    `WriteConsoleW` calls, bypassing the narrow-CRT codepage translation
    that otherwise mangles non-ASCII output on legacy conhost.
- **Async `UffsClient` wire-protocol test coverage**
  (`crates/uffs-client/src/connect_tests.rs`) — six behavioural regression
  pins mirroring the sync suite (`status`-method contract,
  `ConnectionFailed` remediation text, `cached_status` short-circuit in
  both directions).  Drives the client through in-memory tokio
  `AsyncRead`/`AsyncWrite` doubles — no real socket, no daemon.

### Changed
- **Bulkiness sort-key eliminates per-candidate `DisplayRow` allocation**
  (`crates/uffs-core/src/search/query/numeric_top_n.rs`).  Added
  `bulkiness_for_record(&CompactRecord)` as a sibling of `bulkiness_for_row`;
  both forward to a shared private `bulkiness_from_sizes` so they cannot
  drift.  On the numeric top-N hot path this shaves an 18-line
  `DisplayRow::new(..., String::new(), ...)` dance — ~μs per candidate —
  measured impact ≈ 45 ms on a 45K-row `--sort bulkiness *.dll` query.

### Fixed
- **`shmem::tests` race** — `concurrent_writes_get_unique_paths` and
  `gc_cleans_orphaned_bins_and_preserves_non_bins` shared the global
  `shmem_dir()`; the GC test's `cleanup_stale_shmem_files()` sweep could
  wipe in-flight files written by the concurrent-writes test when cargo's
  threadpool scheduled both in parallel.  Serialised via a file-local
  `Mutex<()>`.  Production never hit this — GC only runs at daemon
  startup, and the PID file prevents overlap in real usage.
- **Two miswritten `#[expect(clippy::cognitive_complexity)]` reason strings**
  in `crates/uffs-daemon/src/index/mod.rs` had been copy-pasted from
  unrelated functions (`load_single_mft_file` tagged as "multi-drive
  search"; `ensure_drives_loaded` as "tree metrics computation").
  Replaced with accurate per-function justifications.

## [0.5.58] - 2026-04-19

### Added
- **Phase 2 performance measurement series** (closed): 11 instrumented
  runs comparing UFFS to Everything / UltraSearch / ES across cold-warm-hot
  phases.  Shipped `docs/research/perf-phase2-measurement-plan.md` as
  the permanent record.
- **`paths_blob` single-buffer fast path (v0.5.35)** — daemon packs
  path-only projections into a newline-terminated UTF-8 buffer; CLI
  writes with one `write_all`, skipping per-row JSON deserialisation.
  Inline for ≤ `SHMEM_THRESHOLD` rows; large results fall back to the
  shmem transport.
- **UAC refactor (v0.5.36)** — `ElevationPolicy::RequireExistingElevation`
  default, `--elevate` opt-in, `UFFS_ELEVATE=1` session override, plus an
  actionable error surface listing all three recovery paths (elevated
  shell, explicit UAC, broker install).
- **Deep health check (Run 10 Part B)** — `UffsClientSync` /
  `UffsClient` consolidate the connect-time liveness probe and
  pre-search readiness poll into a single `status` RPC, with a
  `cached_status` short-circuit in `await_ready`.  ~5–10 ms saved per
  CLI invocation on Windows named pipes.
- **Shared-memory transport for bulk results** (`uffs-client::shmem`) —
  results beyond `SHMEM_THRESHOLD` bypass JSON and memory-map a temp
  file.  Includes format v2 binary header, best-effort GC of stale
  `.bin` files on daemon startup.
- **Cross-tool benchmark harness**
  (`scripts/windows/cross-tool-benchmark.rs`) — drives UFFS, Everything,
  UltraSearch, and the legacy `uffs.com` C++ build through an
  apples-to-apples workload with cold/warm/hot phases and per-drive
  isolation.

### Changed
- **`cli_args.rs` refactored** — 11 stateless parsers extracted to
  `cli_args_helpers.rs`; `search.rs` tests moved to `search_tests.rs`
  via `#[path]` module re-attach.  Keeps both files under the 800-LOC
  file-size policy with no suppression.
- **Startup profiling** — `UFFS_PROFILE_STARTUP=1` prints per-phase
  wall-clock from `main()` through first `write_all`, driving the
  Phase 2 + Phase 3 measurement work.

### Fixed
- **`*.<ext>` and `<letter>:*` CLI sugar** — parse-time promotion to
  `pattern="*" + ext=<ext>` (and drive-prefix extraction) was briefly
  regressed during the fat→thin CLI split; restored plus a dispatch-time
  safety net in `uffs_core::search::backend::search_index` for direct
  JSON-RPC callers.
- **PathOnly sort** — now matches Windows Folder-column semantics
  (directories compare before files at equal path prefix; case-folded
  via the drive's upcase table).
- **Lifecycle / PID file** — stale-PID detection, `--no-retire` flag
  for long-running CI sessions, and session-tier upgrades (TUI / GUI /
  MCP at tier 1 get 3× the idle timeout of CLI at tier 0).

## [0.5.0] - 2026-03-15

Major architectural milestone — daemon-first CLI, MCP adapter, and
aggregate engine all ship together.

### Added
- **Aggregate engine** (`uffs_core::aggregate`) — Stages 0-5 complete:
  scaffolding + `AggregateMeta`; protocol + daemon + CLI integration;
  rollup, duplicates, parser, presets; pagination + CSV/TSV export;
  cache; `--agg` flag surface; MCP aggregation tools; 10-test
  validation suite (T119–T128).
- **MCP (Model Context Protocol) gateway** (`uffs-mcp`) — stdio adapter
  that bridges Claude, Cursor, Windsurf, and other AI agents to the
  daemon via JSON-RPC.  D3.4.5 notifications, D4.3 E2E tests, MCP
  resources + prompts.
- **Security hardening** — S1 cache DACL / file permissions, S2.2.2
  Windows DPAPI keystore, S4 daemon IPC hardening (peer credentials,
  input validation, limit caps), S4.3 client-side daemon identity
  verification (macOS codesign / Windows Authenticode), S4.4 rate
  limiting + idle timeout + shutdown nonce, S5 Access Broker hardening.
- **`uffs-broker`** — optional Windows service providing elevated MFT
  handles so the daemon itself can run `asInvoker` with no UAC prompt.
- **Scenario M** — incremental MFT hot-load validation scenario; exercises
  the daemon's `load_drive` + `info` + `refresh` paths against live
  drives.

### Changed
- **`--parity-compat` mode** — `CPP_COLUMN_ORDER` for exact C++-binary
  output shape, `parity_attributes()` mask for the 15 baseline NTFS
  flag bits.  Lets the Rust daemon drop into legacy automation with
  zero ini changes.

## [0.4.0] - 2026-02-12

Daemon-first architecture lands — CLI / TUI / GUI / MCP are now all
thin clients over a unified `uffsd` process.

### Added
- **Daemon foundation (D2)** — `IndexManager` holds the compact index +
  trigrams; `IpcServer` over Unix domain socket (macOS/Linux) or named
  pipe (Windows); RPC handler; lifecycle manager with idle auto-retire.
- **Client library (D3)** — `UffsClient` (async, tokio) and
  `UffsClientSync` (blocking, tokio-free) with auto-start, keepalive,
  reconnect, structured error types.
- **MCP adapter scaffolding (D4)** — stdio bridge, initial tool
  definitions, handler dispatch.
- **Windows Access Broker scaffold (D7)** — `uffs-broker` service,
  client, shared handle passing via Win32 named pipes; unblocks the
  "no UAC prompt for search" target posture.
- **Thin-client CLI / TUI / GUI** — `uffs`, `uffs_tui`, `uffs_gui` now
  delegate all heavy lifting to the daemon.  TUI drops from ~7 GiB
  peak RSS to < 50 MB.

## [0.3.0] - 2026-02-01

### Added
- **Compact index** — 72 bytes/record `CompactRecord` (`repr(C)`,
  `bytemuck::Pod/Zeroable`) replaces the full `MftIndex` after cache
  build.  ~72% memory reduction (7.5 GB → 2.1 GB for 25.9M records
  across 7 drives).
- **TUI** (`uffs_tui`) with ratatui — search box, paginated table,
  multi-tier sort (seven columns), file/dir/all filter, drive colour
  palette.  Wave 1 (trigram index, textarea, devicons) and Wave 2
  (table, sort, filter) complete.
- **Tree-based path search** — children index + segment decomposition
  for `C:\foo\bar`-style queries; glob matching with `*`, `?`, `**`.
- **On-demand full record lookup** — 25-column max view via seek+read
  from the `.uffs` cache, no need to keep full records in memory.
- **`.uffs` cache on macOS** — mirrors the Windows cache flow so MFT
  files captured on Windows can be searched on macOS.
- **Persistent search history** (`Ctrl+P` / `Ctrl+N`) — platform
  config dir, deduplicated, survives restarts.
- **Keymap system** — `~/.config/uffs/keys.toml`, embedded
  `PRESET_WINDOWS` and `PRESET_EMACS`, `--keys emacs` CLI override.

### Fixed
- **NTFS flags refactor** — `StandardInfo.flags` now stores raw
  `FILE_ATTRIBUTE_*` bits matching Windows semantics (`IS_READONLY=0x0001`,
  `IS_HIDDEN=0x0002`, etc.) instead of an internal remapping.  Cache
  format v9 (v8 auto-converts via `v8_flags_to_raw_ntfs()`).  Unblocks
  downstream parity work.

## [0.2.208] - 2026-01-27

### Added
- Baseline CI validation for modernization effort
- Windows cross-compilation for all binaries (uffs, uffs_mft, uffs_tui, uffs_gui)
- Modernization tracker and wave guides

### Changed
- Updated Polars to commit 8b99db82

## [0.2.114] - 2026-01-26

### Added
- Initial UFFS Rust implementation
- MFT reading and parsing with Polars DataFrames
- Path resolution during MFT digestion
- Hard link expansion (default on)
- Multi-drive parallel indexing support
- Cache architecture with zstd compression

### Fixed
- Various MFT parsing edge cases

[Unreleased]: https://github.com/githubrobbi/UltraFastFileSearch/compare/v0.5.58...HEAD
[0.5.58]: https://github.com/githubrobbi/UltraFastFileSearch/compare/v0.5.0...v0.5.58
[0.5.0]: https://github.com/githubrobbi/UltraFastFileSearch/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/githubrobbi/UltraFastFileSearch/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/githubrobbi/UltraFastFileSearch/compare/v0.2.208...v0.3.0
[0.2.208]: https://github.com/githubrobbi/UltraFastFileSearch/compare/v0.2.114...v0.2.208
[0.2.114]: https://github.com/githubrobbi/UltraFastFileSearch/releases/tag/v0.2.114

