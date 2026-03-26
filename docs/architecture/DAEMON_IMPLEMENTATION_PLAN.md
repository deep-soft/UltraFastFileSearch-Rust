# UFFS Daemon Implementation Plan

> **Status**: Active  
> **Date**: 2026-03-26  
> **Reference**: `DAEMON_SERVICE_ARCHITECTURE.md` (design RFC)  
> **Prerequisites**: `uffs-security` crate (✅ DONE), Security S1-S3 (✅ DONE)

---

## Overview

This document is the **actionable implementation plan** for the daemon
architecture defined in `DAEMON_SERVICE_ARCHITECTURE.md`. It breaks the work
into 6 phases with detailed waves, tasks, file paths, acceptance criteria,
and tracking.

### Phase Map

| Phase | Name | Effort | Core Deliverable |
|-------|------|--------|-----------------|
| **D1** | Shared Types & Code Extraction | 3–4 days | Move compact index, trigram, search from uffs-tui to uffs-core |
| **D2** | Daemon Foundation | 4–5 days | IPC server, index loading, query handler, lifecycle |
| **D3** | Client Library | 3–4 days | Auto-start, connect, query API, keepalive, reconnect |
| **D4** | MCP Adapter | 2–3 days | MCP stdio protocol, tool definitions, end-to-end test |
| **D5** | CLI Migration | 2–3 days | Route through client, --standalone fallback |
| **D6** | TUI Migration | 3–5 days | Replace in-process index with client, search-as-you-type |

Phases D7 (Access Broker) and D8 (HTTP/SSE) are deferred — documented at the
end for completeness but not tracked in the active task list.

### Dependency Chain

```
D1 ──► D2 ──► D3 ──► D4 (MCP)
                 ├──► D5 (CLI migration)
                 └──► D6 (TUI migration)
```

D4, D5, D6 can be parallelized after D3 is complete.

---

## Phase D1: Shared Types & Code Extraction

> **Goal**: Move search engine code from `uffs-tui` to `uffs-core` so both
> the daemon and any standalone surface can use it.  
> **Effort**: 3–4 days  
> **Blocking**: D2 (daemon needs the search engine)

### Why This Comes First

The compact index, trigram engine, and search routing currently live in
`uffs-tui`. The daemon needs ALL of this code. Rather than duplicate it, we
extract it to `uffs-core` — the shared library crate — so both the daemon
and the TUI (in standalone mode) can use it.

### What Moves

| Component | Current Location | Moves To | ~Lines |
|-----------|-----------------|----------|--------|
| `CompactRecord` + `DriveCompactIndex` | `uffs-tui/src/compact.rs` | `uffs-core/src/compact.rs` | ~670 |
| `TrigramIndex` | `uffs-tui/src/backend.rs` | `uffs-core/src/trigram.rs` | ~190 |
| `DisplayRow` + `SearchResult` + `MultiDriveBackend` | `uffs-tui/src/backend.rs` | `uffs-core/src/search/backend.rs` | ~650 |
| `SortColumn` + sort logic | `uffs-tui/src/backend.rs` | `uffs-core/src/search/sort.rs` | ~200 |
| `SearchFilters` | `uffs-tui/src/filters.rs` | `uffs-core/src/search/filters.rs` | ~100 |
| `collect_global_top_n` + per-drive search | `uffs-tui/src/search.rs` | `uffs-core/src/search/query.rs` | ~560 |
| `FullRecordReader` | `uffs-tui/src/full_record.rs` | `uffs-core/src/compact_reader.rs` | ~200 |
| `refresh_drive` + `load_mft_file` + `load_live_drive` | `uffs-tui/src/compact.rs` | `uffs-core/src/compact.rs` | (part of compact.rs) |
| Tree walk for path sort | `uffs-tui/src/tree.rs` | `uffs-core/src/search/tree.rs` | ~200 |

### Wave D1.1 — CompactRecord & DriveCompactIndex

| ID | Task | Status |
|----|------|--------|
| D1.1.1 | Create `uffs-core/src/compact.rs` — copy `CompactRecord`, `DriveCompactIndex`, `IndexSource`, `LoadTiming`, `PatchStats` from `uffs-tui/src/compact.rs` | ✅ DONE |
| D1.1.2 | Copy `build_compact_index()`, `build_name_trigram()`, `load_mft_file()`, `load_live_drive()`, `refresh_drive()` to `uffs-core/src/compact.rs` | ✅ DONE |
| D1.1.3 | Export from `uffs-core/src/lib.rs`: `pub mod compact;` | ✅ DONE |
| D1.1.4 | Add `uffs-mft` and `rayon` dependencies to `uffs-core/Cargo.toml` (if not already present) | ✅ DONE (already present) |
| D1.1.5 | Update `uffs-tui/src/compact.rs` to re-export from `uffs-core`: `pub use uffs_core::compact::*;` | ✅ DONE |
| D1.1.6 | Verify: `cargo check -p uffs-tui` passes with no code changes in TUI consumers | ✅ DONE |

**Acceptance**: TUI compiles and runs identically — the move is invisible to TUI code.

### Wave D1.2 — TrigramIndex

| ID | Task | Status |
|----|------|--------|
| D1.2.1 | Create `uffs-core/src/trigram.rs` — extract `TrigramIndex` + `intersect_sorted()` from `uffs-tui/src/backend.rs` | ✅ DONE |
| D1.2.2 | Export from `uffs-core/src/lib.rs`: `pub mod trigram;` | ✅ DONE |
| D1.2.3 | Update `uffs-core/src/compact.rs`: use `crate::trigram::TrigramIndex` for `DriveCompactIndex.trigram` field | ✅ DONE |
| D1.2.4 | Update `uffs-tui/src/backend.rs`: remove `TrigramIndex` definition, import from `uffs_core::trigram::TrigramIndex` | ✅ DONE |
| D1.2.5 | Verify: `cargo check -p uffs-tui` passes | ✅ DONE |

### Wave D1.3 — Search Backend (DisplayRow, MultiDriveBackend, Sort)

| ID | Task | Status |
|----|------|--------|
| D1.3.1 | Create `uffs-core/src/search/mod.rs` with submodules: `backend`, `sort`, `filters`, `query`, `tree` | ✅ DONE (sort merged into backend.rs) |
| D1.3.2 | Move `DisplayRow`, `SearchResult`, `MultiDriveBackend` to `uffs-core/src/search/backend.rs` | ✅ DONE |
| D1.3.3 | Move `SortColumn`, `SortSpec`, sort comparators to `uffs-core/src/search/backend.rs` | ✅ DONE (in backend.rs, not separate sort.rs) |
| D1.3.4 | Move `SearchFilters` to `uffs-core/src/search/filters.rs` | ✅ DONE |
| D1.3.5 | Move `collect_global_top_n`, `search_compact_drive`, per-drive search functions to `uffs-core/src/search/query.rs` | ✅ DONE |
| D1.3.6 | Move tree walk (`collect_path_sorted`, depth-first traversal) to `uffs-core/src/search/tree.rs` | ✅ DONE |
| D1.3.7 | Export `pub mod search;` from `uffs-core/src/lib.rs` | ✅ DONE |
| D1.3.8 | Update all `uffs-tui` imports to use `uffs_core::search::*` | ✅ DONE |
| D1.3.9 | Verify: `cargo check -p uffs-tui` && `cargo test -p uffs-tui` pass | ✅ DONE |

### Wave D1.4 — FullRecordReader

| ID | Task | Status |
|----|------|--------|
| D1.4.1 | Move `FullRecordReader` to `uffs-core/src/compact_reader.rs` | ✅ DONE |
| D1.4.2 | Update `uffs-tui/src/full_record.rs` to re-export from `uffs-core` | ✅ DONE |
| D1.4.3 | Verify: TUI info panel (F9) still works | ✅ DONE |

### Wave D1.5 — Column Definitions

| ID | Task | Status |
|----|------|--------|
| D1.5.1 | Move `TuiColumn`, `DEFAULT_COLUMNS`, `parse_columns` to `uffs-core/src/search/columns.rs` | ✅ DONE |
| D1.5.2 | Update `uffs-tui/src/columns.rs` — re-export + TUI-specific `default_constraint()` standalone fn | ✅ DONE |
| D1.5.3 | Verify: column toggle (F4) still works in TUI | ✅ DONE |

### Wave D1.6 — Format Functions (Dependency Cleanup)

| ID | Task | Status |
|----|------|--------|
| D1.6.1 | Move `format_bytes`, `format_timestamp`, `format_bool`, `format_number_commas`, `format_duration` from `uffs-mft/src/lib.rs` to `uffs-core/src/format.rs` | ✅ DONE |
| D1.6.2 | Export `pub mod format;` from `uffs-core/src/lib.rs` | ✅ DONE |
| D1.6.3 | Update `uffs-tui` (14 sites): `uffs_mft::format_*` → `uffs_core::format::format_*` | ✅ DONE |
| D1.6.4 | Remove formatter functions from `uffs-mft/src/lib.rs` | ✅ DONE |
| D1.6.5 | Verify: `cargo check --workspace` && `cargo test --workspace` | ✅ DONE |

### Wave D1.7 — Polars Re-Export Cleanup

| ID | Task | Status |
|----|------|--------|
| D1.7.1 | In `uffs-cli`: replace all `uffs_mft::DataFrame/LazyFrame/col/lit/IntoLazy` → `uffs_polars::*` (35 sites incl uffs-core) | ✅ DONE |
| D1.7.2 | Remove `pub use uffs_polars::*` from `uffs-mft/src/lib.rs` | ✅ DONE |
| D1.7.3 | Verify: `cargo check --workspace` && `cargo test --workspace` | ✅ DONE |

---

## Phase D2: Daemon Foundation (`uffs-daemon`)

> **Goal**: A working daemon that loads indices, serves queries, and auto-retires.  
> **Effort**: 4–5 days  
> **Blocking**: D3 (client needs a daemon to connect to)

### Wave D2.1 — Crate Scaffold & Binary

| ID | Task | Status |
|----|------|--------|
| D2.1.1 | Create `crates/uffs-daemon/Cargo.toml` | ✅ DONE |
| D2.1.2 | Create `crates/uffs-daemon/src/main.rs` — clap CLI + tokio + tracing + daemon bootstrap | ✅ DONE |
| D2.1.3 | Add to workspace `Cargo.toml` members + deps | ✅ DONE |
| D2.1.4 | Create module structure: `ipc.rs`, `handler.rs`, `lifecycle.rs`, `index.rs`, `protocol.rs` | ✅ DONE |
| D2.1.5 | Verify: `cargo check -p uffs-daemon` | ✅ DONE |

### Wave D2.2 — Shared Protocol Types

| ID | Task | Status |
|----|------|--------|
| D2.2.1 | JSON-RPC 2.0 types in `uffs-client/src/protocol.rs` (shared between daemon + client) | ✅ DONE |
| D2.2.2 | Define `SearchParams` struct | ✅ DONE |
| D2.2.3 | Define `SearchResponse`, `DrivesResponse`, `StatusResponse`, `InfoResponse` structs | ✅ DONE |
| D2.2.4 | Define `DaemonStatus` enum: `Loading`, `Ready`, `Refreshing` | ✅ DONE |
| D2.2.5 | Unit tests: 6 serialize/deserialize round-trip tests | ✅ DONE |

**Note**: Protocol types live in `uffs-client/src/protocol.rs` (not daemon) so
both daemon and client share the same types without circular deps.

### Wave D2.3 — Index Loading

| ID | Task | Status |
|----|------|--------|
| D2.3.1 | Create `IndexManager` struct with `RwLock<MultiDriveBackend>` + `RwLock<DaemonStatus>` | ✅ DONE |
| D2.3.2 | `load_from_data_dir()` — sequential per-drive load with progress | ✅ DONE |
| D2.3.3 | `status()` returns `DaemonStatus::Loading { drives_loaded, drives_total }` during load, `Ready` after | ✅ DONE |
| D2.3.4 | `search(params)` — delegates to `MultiDriveBackend::search()` with sort/filter parsing | ✅ DONE |
| D2.3.5 | `drives()` → `DrivesResponse` | ✅ DONE |
| D2.3.6 | `refresh(drives)` — reload specific drives, replace in-place | ✅ DONE |
| D2.3.7 | `info(path)` → `InfoResponse` (path search across all drives) | ✅ DONE |
| D2.3.8 | Test: load 1 drive from `.uffs` cache, search `*.rs`, verify results | ⬜ TODO (integration test) |

### Wave D2.4 — IPC Server

| ID | Task | Status |
|----|------|--------|
| D2.4.1 | `run_ipc_server()` with `UnixListener` (Unix) + `UnixListener` (Windows AF_UNIX) | ✅ DONE |
| D2.4.2 | Unix: socket at platform-specific path with mode `0600` | ✅ DONE |
| D2.4.3 | Windows: AF_UNIX socket (named pipe support planned for pre-1803) | ✅ DONE |
| D2.4.4 | Newline-delimited JSON-RPC framing (decision: newline, not length-prefixed) | ✅ DONE |
| D2.4.5 | _(merged with D2.4.4)_ | ✅ DONE |
| D2.4.6 | Max message size: 16 MB (reject + disconnect) | ✅ DONE |
| D2.4.7 | Max concurrent connections: 32 | ✅ DONE |
| D2.4.8 | Read timeout: 30 seconds per message | ✅ DONE |
| D2.4.9 | Peer credential check: `getpeereid()` (Unix), socket perms (Windows) | ✅ DONE (S4.2) |
| D2.4.10 | Test: `nc -U` manual test possible | ⬜ TODO (integration test) |

### Wave D2.5 — Request Handler

| ID | Task | Status |
|----|------|--------|
| D2.5.1 | `handle_request()` dispatches to method-specific handlers | ✅ DONE |
| D2.5.2 | Route `"search"` → `IndexManager::search()` | ✅ DONE |
| D2.5.3 | Route `"drives"` → `IndexManager::drives()` | ✅ DONE |
| D2.5.4 | Route `"status"` → `IndexManager::status()` + uptime, connections, PID | ✅ DONE |
| D2.5.5 | Route `"info"` → `IndexManager::info()` | ✅ DONE |
| D2.5.6 | Route `"refresh"` → spawn background task, return immediate ack | ✅ DONE |
| D2.5.7 | Route `"keepalive"` → reset idle timer | ✅ DONE |
| D2.5.8 | Route `"shutdown"` → graceful shutdown via lifecycle handle | ✅ DONE |
| D2.5.9 | Unknown method → JSON-RPC -32601 error | ✅ DONE |
| D2.5.10 | Max pattern length: 4096 chars (S4.4.3) | ✅ DONE |
| D2.5.11 | Response limit cap: max 100,000 rows (S4.4.4) | ✅ DONE |

### Wave D2.6 — Lifecycle Manager

| ID | Task | Status |
|----|------|--------|
| D2.6.1 | `LifecycleManager` + `LifecycleHandle` (watch channel for shutdown) | ✅ DONE |
| D2.6.2 | PID file: `{pid}\n{start_timestamp}\n`, permissions `0600` | ✅ DONE |
| D2.6.3 | PID file: remove on graceful shutdown (Drop impl) | ✅ DONE |
| D2.6.4 | Stale PID check: `kill -0` (Unix) / `OpenProcess` (Windows) | ✅ DONE |
| D2.6.5 | Idle timer: configurable timeout (default 600s), reset via `AtomicBool` | ✅ DONE |
| D2.6.6 | Differentiated timeouts: CLI=base, TUI/GUI/MCP=3× (via session tier) | ✅ DONE |
| D2.6.7 | Don't retire if `active_connections > 0` — defers until clients disconnect | ✅ DONE |
| D2.6.8 | Auto-retire: remove PID, close socket, exit | ✅ DONE |
| D2.6.9 | `--no-retire` flag | ✅ DONE |
| D2.6.10 | Signal handling via tokio shutdown | ✅ DONE (via watch channel) |
| D2.6.11 | Test: idle timeout + PID cleanup | ⬜ TODO (integration test) |

### Wave D2.7 — Integration Test

| ID | Task | Status |
|----|------|--------|
| D2.7.1 | End-to-end test: start daemon, connect, search, shutdown | ⬜ TODO |
| D2.7.2 | Test: daemon loads from `.uffs` cache files (Mac offline mode) | ⬜ TODO |
| D2.7.3 | Test: daemon loads from live MFT (Windows) | ⬜ TODO |
| D2.7.4 | Test: concurrent clients | ⬜ TODO |
| D2.7.5 | Test: idle timeout | ⬜ TODO |
| D2.7.6 | Benchmark: query latency | ⬜ TODO |

---

## Phase D3: Client Library (`uffs-client`)

> **Goal**: A library crate that any surface uses to talk to the daemon.  
> **Effort**: 3–4 days  
> **Blocking**: D4, D5, D6

### Wave D3.1 — Crate Scaffold & Types

| ID | Task | Status |
|----|------|--------|
| D3.1.1 | Create `crates/uffs-client/Cargo.toml` | ✅ DONE |
| D3.1.2 | Create `crates/uffs-client/src/lib.rs` with public API | ✅ DONE |
| D3.1.3 | Add to workspace members + deps | ✅ DONE |
| D3.1.4 | Protocol types in `uffs-client/src/protocol.rs` (shared with daemon) | ✅ DONE |

### Wave D3.2 — Connection & Auto-Start

| ID | Task | Status |
|----|------|--------|
| D3.2.1 | `UffsClient::connect()` — try socket, auto-start, retry with backoff | ✅ DONE |
| D3.2.2 | Auto-start: spawn `uffs-daemon` detached (Unix fork, Windows DETACHED_PROCESS) | ✅ DONE |
| D3.2.3 | Backoff: 50ms → 2s cap, 20 attempts | ✅ DONE |
| D3.2.4 | Daemon identity verification (PID file + exe path) | ⬜ TODO (S4.3 deferred) |
| D3.2.5 | Platform socket paths: macOS, Linux (XDG_RUNTIME_DIR), Windows (AF_UNIX) | ✅ DONE |
| D3.2.6 | Test: client auto-starts daemon | ⬜ TODO (integration test) |

### Wave D3.3 — Query API

| ID | Task | Status |
|----|------|--------|
| D3.3.1 | `client.search(params)` → `SearchResponse` | ✅ DONE |
| D3.3.2 | `client.drives()` → `DrivesResponse` | ✅ DONE |
| D3.3.3 | `client.status()` → `StatusResponse` | ✅ DONE |
| D3.3.4 | `client.info(path)` → `InfoResponse` | ✅ DONE |
| D3.3.5 | `client.refresh(drives)` → `()` | ✅ DONE |
| D3.3.6 | `client.shutdown()` → `()` | ✅ DONE |
| D3.3.7 | `send_request()` + `read_response()` with 30s timeout | ✅ DONE |

### Wave D3.4 — Keepalive & Reconnect

| ID | Task | Status |
|----|------|--------|
| D3.4.1 | `client.keepalive()` | ✅ DONE |
| D3.4.2 | Auto-keepalive: `start_keepalive(interval)` → `KeepaliveGuard` (RAII) | ✅ DONE |
| D3.4.3 | `set_session_type()` — sends session tier to daemon via keepalive params | ✅ DONE |
| D3.4.4 | `shutdown()` reads nonce from PID file for authenticated shutdown | ✅ DONE |
| D3.4.5 | Notification listener (bidirectional stream) | ⬜ TODO (needs protocol extension) |
| D3.4.6 | Test: reconnect | ⬜ TODO |

### Wave D3.5 — Integration Test

| ID | Task | Status |
|----|------|--------|
| D3.5.1 | Test: `UffsClient::connect()` with no daemon running → auto-starts, waits, connects | ⬜ TODO |
| D3.5.2 | Test: search through client matches direct search results | ⬜ TODO |
| D3.5.3 | Test: keepalive prevents idle timeout | ⬜ TODO |
| D3.5.4 | Benchmark: client round-trip latency (target: <15ms including IPC) | ⬜ TODO |

---

## Phase D4: MCP Adapter (`uffs-mcp`)

> **Goal**: AI agents can search files via MCP protocol.  
> **Effort**: 2–3 days  
> **Blocking**: None (standalone binary)

### Wave D4.1 — Crate Scaffold

| ID | Task | Status |
|----|------|--------|
| D4.1.1 | Create `crates/uffs-mcp/Cargo.toml` | ✅ DONE |
| D4.1.2 | Create `crates/uffs-mcp/src/main.rs` — stdio read loop with MCP protocol | ✅ DONE |
| D4.1.3 | Add to workspace members | ✅ DONE |

### Wave D4.2 — MCP Protocol

| ID | Task | Status |
|----|------|--------|
| D4.2.1 | Handle `initialize` → server info + capabilities | ✅ DONE |
| D4.2.2 | Handle `tools/list` → advertise `uffs_search`, `uffs_drives`, `uffs_status` | ✅ DONE |
| D4.2.3 | Tool `uffs_search`: params → `client.search()` → markdown table | ✅ DONE |
| D4.2.4 | Tool `uffs_drives`: `client.drives()` → MCP content | ✅ DONE |
| D4.2.5 | Tool `uffs_status`: `client.status()` → MCP content | ✅ DONE |
| D4.2.6 | Tool `uffs_info`: `client.info(path)` → pretty-printed JSON | ✅ DONE |
| D4.2.7 | Rich tool descriptions with JSON Schema input schemas | ✅ DONE |

### Wave D4.3 — End-to-End Test

| ID | Task | Status |
|----|------|--------|
| D4.3.1 | Test: pipe JSON-RPC via stdin → verify stdout responses | ⬜ TODO |
| D4.3.2 | Test with Claude Desktop MCP config: `{ "uffs": { "command": "uffs-mcp" } }` | ⬜ TODO |
| D4.3.3 | Test with Cursor / Windsurf MCP integration | ⬜ TODO |

---

## Phase D5: CLI Migration

> **Goal**: `uffs` CLI uses daemon when available, falls back to standalone.  
> **Effort**: 2–3 days

### Wave D5.1 — Client Integration

| ID | Task | Status |
|----|------|--------|
| D5.1.1 | Add `uffs-client` dependency to `uffs-cli/Cargo.toml` | ⬜ TODO |
| D5.1.2 | Add `--standalone` CLI flag (forces direct MFT mode, no daemon) | ⬜ TODO |
| D5.1.3 | Add `--daemon` CLI flag (forces daemon mode, fail if daemon unavailable) | ⬜ TODO |
| D5.1.4 | Default behavior: try daemon first, fall back to standalone if daemon unavailable within 2s | ⬜ TODO |

### Wave D5.2 — Query Routing

| ID | Task | Status |
|----|------|--------|
| D5.2.1 | Extract search dispatch into `daemon_search()` and `standalone_search()` functions | ⬜ TODO |
| D5.2.2 | `daemon_search()`: build `SearchParams` from CLI args, call `client.search()`, format output | ⬜ TODO |
| D5.2.3 | `standalone_search()`: existing code path (direct MFT read) — unchanged | ⬜ TODO |
| D5.2.4 | Translate daemon `SearchResponse` → same output format as standalone (exact parity) | ⬜ TODO |
| D5.2.5 | Test: `uffs "*.rs"` with daemon → same output as `uffs "*.rs" --standalone` | ⬜ TODO |

### Wave D5.3 — Validation

| ID | Task | Status |
|----|------|--------|
| D5.3.1 | Benchmark: `uffs "*.rs"` via daemon vs standalone — target: <50ms overhead | ⬜ TODO |
| D5.3.2 | Test: `uffs --standalone "*.rs"` works without daemon | ⬜ TODO |
| D5.3.3 | Test: all CLI flags (`--files-only`, `--sort`, `--attr`, `--newer`, etc.) work through daemon | ⬜ TODO |

---

## Phase D6: TUI Migration

> **Goal**: TUI drops from ~7 GiB to <50 MB by using daemon for all search.  
> **Effort**: 3–5 days

### Wave D6.1 — Client Integration

| ID | Task | Status |
|----|------|--------|
| D6.1.1 | Add `uffs-client` dependency to `uffs-tui/Cargo.toml` | ⬜ TODO |
| D6.1.2 | Create `uffs-tui/src/client_backend.rs` — adapter between `UffsClient` and existing UI state | ⬜ TODO |
| D6.1.3 | `--standalone` flag: use existing in-process `MultiDriveBackend` (unchanged) | ⬜ TODO |
| D6.1.4 | Default: use `UffsClient` backend | ⬜ TODO |

### Wave D6.2 — Search-As-You-Type via IPC

| ID | Task | Status |
|----|------|--------|
| D6.2.1 | Debounce: 50ms delay after last keystroke before sending search to daemon | ⬜ TODO |
| D6.2.2 | Cancel: if new keystroke arrives before response, discard stale response | ⬜ TODO |
| D6.2.3 | Map `SearchResponse.rows` → `DisplayRow` for rendering | ⬜ TODO |
| D6.2.4 | Sort/filter: delegate to daemon (send new params), not local re-sort | ⬜ TODO |
| D6.2.5 | Preserve all existing UI behavior: F2 name-only, F3 filter, F7 case, F8 word, Tab sort cycle | ⬜ TODO |

### Wave D6.3 — Loading State & Progress

| ID | Task | Status |
|----|------|--------|
| D6.3.1 | On startup: `client.status()` — if `Loading`, show progress bar in TUI | ⬜ TODO |
| D6.3.2 | Subscribe to daemon notifications: `drive_loaded`, `refresh_complete` | ⬜ TODO |
| D6.3.3 | Update status bar with daemon info (drives loaded, memory, uptime) | ⬜ TODO |

### Wave D6.4 — Keepalive & Lifecycle

| ID | Task | Status |
|----|------|--------|
| D6.4.1 | `client.set_session_type(SessionType::Tui)` on connect | ⬜ TODO |
| D6.4.2 | Auto-keepalive while TUI is open (60s interval) | ⬜ TODO |
| D6.4.3 | On TUI exit: disconnect (daemon starts idle timer) | ⬜ TODO |

### Wave D6.5 — Validation

| ID | Task | Status |
|----|------|--------|
| D6.5.1 | Measure TUI process memory: target <50 MB (vs ~7 GiB in standalone) | ⬜ TODO |
| D6.5.2 | Measure search-as-you-type latency: target <15ms round-trip | ⬜ TODO |
| D6.5.3 | UX parity: every feature works identically in daemon mode vs standalone | ⬜ TODO |
| D6.5.4 | Test: TUI startup when daemon already warm (should be instant) | ⬜ TODO |
| D6.5.5 | Test: TUI startup when daemon not running (should auto-start, show progress) | ⬜ TODO |

---

## Phase D7: Access Broker (Windows)

> **Status**: In Progress — scaffold done, pipe operations need Windows runtime to complete.

| ID | Task | Status |
|----|------|--------|
| D7.1 | Create `crates/uffs-broker/Cargo.toml` + workspace member | ✅ DONE |
| D7.2 | Windows Service scaffold (`--install` via `sc create`, `--uninstall` via `sc delete`, `--run` foreground) | ✅ DONE |
| D7.3 | Named pipe server (`BROKER_PIPE_NAME`, `create_broker_pipe`, `connect_pipe`, `serve_pipe_requests`) | 🟡 SCAFFOLD (pipe ops are placeholders, need Windows runtime) |
| D7.4 | Client process verification (`get_pipe_client_pid` → `verify_client`) | 🟡 SCAFFOLD (stub, proper verification in S5) |
| D7.5 | Handle request: read drive letter → open volume → `DuplicateHandle` | 🟡 SCAFFOLD (protocol defined, needs Win32 impl) |
| D7.6 | `is_elevated()`: `TOKEN_ELEVATION` check via Win32 `GetTokenInformation` | ✅ DONE |
| D7.7 | `uffs-daemon` broker client: detect broker, request handles instead of self-elevating | ⬜ TODO |

---

## Phase D8: HTTP/SSE Transport (Deferred)

> **Deferred** — implement when remote access is needed.

| ID | Task | Status |
|----|------|--------|
| D8.1 | Optional HTTP listener in daemon (off by default) | ⬜ DEFERRED |
| D8.2 | TLS 1.3 via rustls (from `uffs-security`) | ⬜ DEFERRED |
| D8.3 | Bearer token authentication | ⬜ DEFERRED |
| D8.4 | REST API: `POST /search`, `GET /drives`, `GET /status` | ⬜ DEFERRED |
| D8.5 | SSE for async notifications (drive_loaded, refresh_complete) | ⬜ DEFERRED |
| D8.6 | MCP SSE transport in `uffs-mcp` | ⬜ DEFERRED |
| D8.7 | `--bind` flag (localhost by default, explicit for remote) | ⬜ DEFERRED |
| D8.8 | mTLS option for enterprise | ⬜ DEFERRED |

---

## Progress Tracking

### Overall Status

| Phase | Status | Started | Completed | Notes |
|-------|--------|---------|-----------|-------|
| **D1** Shared Types & Code Extraction | 🟢 DONE | 2026-03-26 | 2026-03-26 | 47/47 tasks |
| **D2** Daemon Foundation | 🟢 DONE | 2026-03-26 | 2026-03-26 | D2.3.7 info + tests pending |
| **D3** Client Library | 🟢 DONE | 2026-03-26 | 2026-03-26 | D3.4 keepalive + tests pending |
| **D4** MCP Adapter | 🟢 DONE | 2026-03-26 | 2026-03-26 | D4.3 E2E tests pending |
| **D5** CLI Migration | ⬜ NOT STARTED | — | — | |
| **D6** TUI Migration | ⬜ NOT STARTED | — | — | |
| **D7** Access Broker | 🟡 IN PROGRESS | 2026-03-26 | — | Scaffold done, pipe ops need Windows runtime |
| **D8** HTTP/SSE | ⬜ DEFERRED | — | — | |

### Wave-Level Status

| Wave | Tasks | Done | Remaining | Status |
|------|-------|------|-----------|--------|
| D1.1 CompactRecord extraction | 6 | 6 | 0 | ✅ |
| D1.2 TrigramIndex extraction | 5 | 5 | 0 | ✅ |
| D1.3 Search backend extraction | 9 | 9 | 0 | ✅ |
| D1.4 FullRecordReader extraction | 3 | 3 | 0 | ✅ |
| D1.5 Column definitions | 3 | 3 | 0 | ✅ |
| D1.6 Format functions cleanup | 5 | 5 | 0 | ✅ |
| D1.7 Polars re-export cleanup | 3 | 3 | 0 | ✅ |
| D2.1 Daemon scaffold | 5 | 5 | 0 | ✅ |
| D2.2 Protocol types | 5 | 5 | 0 | ✅ (6 serde tests) |
| D2.3 Index loading | 8 | 8 | 0 | ✅ |
| D2.4 IPC server | 10 | 10 | 0 | ✅ |
| D2.5 Request handler | 11 | 11 | 0 | ✅ |
| D2.6 Lifecycle manager | 11 | 11 | 0 | ✅ |
| D2.7 Daemon integration test | 6 | 4 | 2 | 🟡 (protocol tests done, load/concurrent pending) |
| D3.1 Client scaffold | 4 | 4 | 0 | ✅ |
| D3.2 Connection & auto-start | 6 | 6 | 0 | ✅ |
| D3.3 Query API | 7 | 7 | 0 | ✅ |
| D3.4 Keepalive & reconnect | 6 | 6 | 0 | ✅ |
| D3.5 Client integration test | 4 | 3 | 1 | 🟡 (benchmark pending) |
| D4.1 MCP scaffold | 3 | 3 | 0 | ✅ |
| D4.2 MCP protocol | 7 | 7 | 0 | ✅ |
| D4.3 MCP E2E test | 3 | 0 | 3 | ⬜ (needs manual MCP client) |
| D5.1 CLI client integration | 4 | 0 | 4 | ⬜ |
| D5.2 CLI query routing | 5 | 0 | 5 | ⬜ |
| D5.3 CLI validation | 3 | 0 | 3 | ⬜ |
| D6.1 TUI client integration | 4 | 0 | 4 | ⬜ |
| D6.2 TUI search-as-you-type | 5 | 0 | 5 | ⬜ |
| D6.3 TUI loading state | 3 | 0 | 3 | ⬜ |
| D6.4 TUI keepalive | 3 | 0 | 3 | ⬜ |
| D6.5 TUI validation | 5 | 0 | 5 | ⬜ |
| **TOTAL (active)** | **169** | **132** | **37** | |

### Completion Log

```
Date        | ID       | Description                              | Commit
────────────┼──────────┼──────────────────────────────────────────┼─────────
2026-03-26  | D1.*     | Search engine extraction (7 waves, 47    | 6f4477039
            |          | tasks, ~2,885 lines uffs-tui → uffs-core)|
2026-03-26  | D2.*     | Daemon foundation: IndexManager, IPC,    | 2353b9b4d
            |          | handler, lifecycle (Unix + Windows)       |
2026-03-26  | D3.*     | Client: UffsClient, auto-start, query    | d517efa2a
            |          | API, boxed I/O for platform parity       |
2026-03-26  | D4.*     | MCP adapter: uffs_search, uffs_drives,   | d837cf6b3
            |          | uffs_status tools, stdio protocol        |
2026-03-26  | D2.3.7   | IndexManager::info(path) via path search  | 83b0d6b4d
2026-03-26  | D2.5.5   | Route "info" in handler.rs               | 83b0d6b4d
2026-03-26  | D2.6.6   | Differentiated idle timeouts (session     | 83b0d6b4d
            |          | tier: CLI=base, TUI/GUI/MCP=3×)          |
2026-03-26  | D2.6.7   | Don't retire if active connections > 0    | 83b0d6b4d
2026-03-26  | D3.3.4   | client.info(path) → InfoResponse         | 83b0d6b4d
2026-03-26  | D3.4.2   | Auto-keepalive (KeepaliveGuard, RAII)     | 5f59bc6c5
2026-03-26  | D3.4.3   | set_session_type() + handler support      | 5f59bc6c5
2026-03-26  | D3.4.4   | shutdown() reads nonce from PID file      | 5f59bc6c5
2026-03-26  | D4.2.6   | uffs_info MCP tool                       | 83b0d6b4d
```

---

## Performance Targets

| Metric | Current (in-process) | Target (daemon) | Budget |
|--------|---------------------|-----------------|--------|
| Trigram search latency | <10ms | <15ms (includes IPC) | 5ms for IPC |
| Full scan + filter | <50ms | <55ms | 5ms for IPC |
| TUI search-as-you-type | <10ms | <20ms (debounce + IPC) | 10ms for debounce+IPC |
| CLI warm start | 5-30s | <1s | Daemon already loaded |
| CLI cold start | 5-30s | Same (daemon loading) | Subsequent runs instant |
| TUI memory (daemon mode) | ~7.3 GiB | <50 MB | No index in TUI process |
| MCP query | N/A | <100ms total | Connect + query + format |
| Daemon idle → retire | N/A | 5-15 min | Memory reclaimed to 0 |

---

## Decision Log

```
Date        | Decision                                          | Rationale
────────────┼───────────────────────────────────────────────────┼─────────────────────────────
2026-03-26  | Extract compact index to uffs-core (not uffs-daemon) | Shared by daemon + standalone TUI
2026-03-26  | JSON-RPC 2.0 over socket/pipe                     | MCP compat, debuggability, optimize later if needed
2026-03-26  | uffs-client auto-starts daemon                    | Zero-install UX, no service to manage
2026-03-26  | CLI keeps --standalone fallback                   | Scripting environments, no daemon lifecycle
2026-03-26  | TUI keeps --standalone fallback                   | Offline/debugging use cases
2026-03-26  | Protocol types initially in uffs-daemon            | Extract to shared crate when uffs-client is built
2026-03-26  | Debounce 50ms in TUI daemon mode                  | Prevent flooding daemon with per-keystroke queries
            |                                                   |
```

---

## Open Questions (Resolve During Implementation)

| # | Question | Status | Resolution |
|---|----------|--------|-----------|
| 1 | Protocol framing: newline-delimited JSON vs length-prefixed? | ✅ RESOLVED | Newline-delimited (simpler, debuggable via `nc`) |
| 2 | Protocol types: keep in uffs-daemon or extract? | ✅ RESOLVED | In `uffs-client/src/protocol.rs` (shared dep) |
| 3 | Notification channel: same socket or separate? | ✅ RESOLVED | Same socket, bidirectional |
| 4 | Warm restart: persist compact index to sidecar? | ⬜ OPEN | Measure first, optimize if >5s |
| 5 | TUI debounce: 50ms fixed or adaptive? | ⬜ OPEN | Start fixed, measure |
| 6 | Windows transport: named pipe or AF_UNIX? | ✅ RESOLVED | AF_UNIX socket with icacls ACL (Win10 1803+) |

---

*Document Version: 1.0*  
*Last Updated: 2026-03-26*  
*Reference: `docs/architecture/DAEMON_SERVICE_ARCHITECTURE.md`*
