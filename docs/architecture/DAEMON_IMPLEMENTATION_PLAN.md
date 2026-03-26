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
| D1.1.1 | Create `uffs-core/src/compact.rs` — copy `CompactRecord`, `DriveCompactIndex`, `IndexSource`, `LoadTiming`, `PatchStats` from `uffs-tui/src/compact.rs` | ⬜ TODO |
| D1.1.2 | Copy `build_compact_index()`, `build_name_trigram()`, `load_mft_file()`, `load_live_drive()`, `refresh_drive()` to `uffs-core/src/compact.rs` | ⬜ TODO |
| D1.1.3 | Export from `uffs-core/src/lib.rs`: `pub mod compact;` | ⬜ TODO |
| D1.1.4 | Add `uffs-mft` and `rayon` dependencies to `uffs-core/Cargo.toml` (if not already present) | ⬜ TODO |
| D1.1.5 | Update `uffs-tui/src/compact.rs` to re-export from `uffs-core`: `pub use uffs_core::compact::*;` | ⬜ TODO |
| D1.1.6 | Verify: `cargo check -p uffs-tui` passes with no code changes in TUI consumers | ⬜ TODO |

**Acceptance**: TUI compiles and runs identically — the move is invisible to TUI code.

### Wave D1.2 — TrigramIndex

| ID | Task | Status |
|----|------|--------|
| D1.2.1 | Create `uffs-core/src/trigram.rs` — extract `TrigramIndex` + `intersect_sorted()` from `uffs-tui/src/backend.rs` | ⬜ TODO |
| D1.2.2 | Export from `uffs-core/src/lib.rs`: `pub mod trigram;` | ⬜ TODO |
| D1.2.3 | Update `uffs-core/src/compact.rs`: use `crate::trigram::TrigramIndex` for `DriveCompactIndex.trigram` field | ⬜ TODO |
| D1.2.4 | Update `uffs-tui/src/backend.rs`: remove `TrigramIndex` definition, import from `uffs_core::trigram::TrigramIndex` | ⬜ TODO |
| D1.2.5 | Verify: `cargo check -p uffs-tui` passes | ⬜ TODO |

### Wave D1.3 — Search Backend (DisplayRow, MultiDriveBackend, Sort)

| ID | Task | Status |
|----|------|--------|
| D1.3.1 | Create `uffs-core/src/search/mod.rs` with submodules: `backend`, `sort`, `filters`, `query`, `tree` | ⬜ TODO |
| D1.3.2 | Move `DisplayRow`, `SearchResult`, `MultiDriveBackend` to `uffs-core/src/search/backend.rs` | ⬜ TODO |
| D1.3.3 | Move `SortColumn`, `SortSpec`, sort comparators to `uffs-core/src/search/sort.rs` | ⬜ TODO |
| D1.3.4 | Move `SearchFilters` to `uffs-core/src/search/filters.rs` | ⬜ TODO |
| D1.3.5 | Move `collect_global_top_n`, `search_compact_drive`, per-drive search functions to `uffs-core/src/search/query.rs` | ⬜ TODO |
| D1.3.6 | Move tree walk (`collect_path_sorted`, depth-first traversal) to `uffs-core/src/search/tree.rs` | ⬜ TODO |
| D1.3.7 | Export `pub mod search;` from `uffs-core/src/lib.rs` | ⬜ TODO |
| D1.3.8 | Update all `uffs-tui` imports to use `uffs_core::search::*` | ⬜ TODO |
| D1.3.9 | Verify: `cargo check -p uffs-tui` && `cargo test -p uffs-tui` pass | ⬜ TODO |

### Wave D1.4 — FullRecordReader

| ID | Task | Status |
|----|------|--------|
| D1.4.1 | Move `FullRecordReader` to `uffs-core/src/compact_reader.rs` | ⬜ TODO |
| D1.4.2 | Update `uffs-tui/src/full_record.rs` to re-export from `uffs-core` | ⬜ TODO |
| D1.4.3 | Verify: TUI info panel (F9) still works | ⬜ TODO |

### Wave D1.5 — Column Definitions

| ID | Task | Status |
|----|------|--------|
| D1.5.1 | Move `TuiColumn`, `DEFAULT_COLUMNS`, `parse_columns` to `uffs-core/src/search/columns.rs` | ⬜ TODO |
| D1.5.2 | Update `uffs-tui/src/columns.rs` and `backend.rs` re-export to point to `uffs-core` | ⬜ TODO |
| D1.5.3 | Verify: column toggle (F4) still works in TUI | ⬜ TODO |

### Wave D1.6 — Format Functions (Dependency Cleanup)

| ID | Task | Status |
|----|------|--------|
| D1.6.1 | Move `format_bytes`, `format_timestamp`, `format_bool`, `format_number_commas`, `format_duration` from `uffs-mft/src/lib.rs` to `uffs-core/src/format.rs` | ⬜ TODO |
| D1.6.2 | Export `pub mod format;` from `uffs-core/src/lib.rs` | ⬜ TODO |
| D1.6.3 | Update `uffs-tui` (14 sites): `uffs_mft::format_*` → `uffs_core::format::format_*` | ⬜ TODO |
| D1.6.4 | Remove formatter functions from `uffs-mft/src/lib.rs` (or add deprecation re-exports) | ⬜ TODO |
| D1.6.5 | Verify: `cargo check --workspace` && `cargo test --workspace` | ⬜ TODO |

### Wave D1.7 — Polars Re-Export Cleanup

| ID | Task | Status |
|----|------|--------|
| D1.7.1 | In `uffs-cli`: replace all `uffs_mft::DataFrame/LazyFrame/col/lit/IntoLazy` → `uffs_polars::*` (33 sites) | ⬜ TODO |
| D1.7.2 | Remove `pub use uffs_polars::*` from `uffs-mft/src/lib.rs:184` | ⬜ TODO |
| D1.7.3 | Verify: `cargo check --workspace` && `cargo test --workspace` | ⬜ TODO |

---

## Phase D2: Daemon Foundation (`uffs-daemon`)

> **Goal**: A working daemon that loads indices, serves queries, and auto-retires.  
> **Effort**: 4–5 days  
> **Blocking**: D3 (client needs a daemon to connect to)

### Wave D2.1 — Crate Scaffold & Binary

| ID | Task | Status |
|----|------|--------|
| D2.1.1 | Create `crates/uffs-daemon/Cargo.toml` (deps: uffs-core, uffs-mft, uffs-security, tokio, serde, serde_json, tracing, dirs-next, clap) | ⬜ TODO |
| D2.1.2 | Create `crates/uffs-daemon/src/main.rs` — CLI args (`--data-dir`, `--idle-timeout`, `--no-retire`, `--log-level`) + daemon bootstrap | ⬜ TODO |
| D2.1.3 | Add to workspace `Cargo.toml` members + deps | ⬜ TODO |
| D2.1.4 | Create module structure: `ipc.rs`, `handler.rs`, `lifecycle.rs`, `index.rs`, `protocol.rs` | ⬜ TODO |
| D2.1.5 | Verify: `cargo check -p uffs-daemon` | ⬜ TODO |

### Wave D2.2 — Shared Protocol Types

| ID | Task | Status |
|----|------|--------|
| D2.2.1 | Create `crates/uffs-daemon/src/protocol.rs` — JSON-RPC 2.0 request/response types (`RpcRequest`, `RpcResponse`, `RpcError`, `RpcNotification`) | ⬜ TODO |
| D2.2.2 | Define `SearchParams` struct (mirrors JSON in architecture doc) | ⬜ TODO |
| D2.2.3 | Define `SearchResponse`, `DrivesResponse`, `StatusResponse`, `InfoResponse` structs | ⬜ TODO |
| D2.2.4 | Define `DaemonStatus` enum: `Loading { drives_loaded, drives_total, pct }`, `Ready`, `Refreshing` | ⬜ TODO |
| D2.2.5 | Unit tests: serialize/deserialize round-trip for all message types | ⬜ TODO |

**Note**: These protocol types will later be extracted to a tiny shared crate
(or put in `uffs-core`) so `uffs-client` can use them too without depending
on `uffs-daemon`. For now, define in daemon, extract when client is built.

### Wave D2.3 — Index Loading

| ID | Task | Status |
|----|------|--------|
| D2.3.1 | Create `crates/uffs-daemon/src/index.rs` — `IndexManager` struct holding `Vec<DriveCompactIndex>` | ⬜ TODO |
| D2.3.2 | Implement `IndexManager::load_all(data_dir, drives)` — parallel per-drive load using `uffs_core::compact::load_mft_file` / `load_live_drive` | ⬜ TODO |
| D2.3.3 | Implement progress reporting: `IndexManager::status()` returns `DaemonStatus::Loading { pct }` during load, `Ready` after | ⬜ TODO |
| D2.3.4 | Implement `IndexManager::search(params) -> SearchResponse` — delegates to `uffs_core::search::MultiDriveBackend::search()` | ⬜ TODO |
| D2.3.5 | Implement `IndexManager::drives()` → `DrivesResponse` | ⬜ TODO |
| D2.3.6 | Implement `IndexManager::refresh(drives)` — reload specific drives, rebuild compact + trigram | ⬜ TODO |
| D2.3.7 | Implement `IndexManager::info(path)` → `InfoResponse` (lookup via `FullRecordReader`) | ⬜ TODO |
| D2.3.8 | Test: load 1 drive from `.uffs` cache, search `*.rs`, verify results | ⬜ TODO |

### Wave D2.4 — IPC Server

| ID | Task | Status |
|----|------|--------|
| D2.4.1 | Create `crates/uffs-daemon/src/ipc.rs` — `IpcServer` struct | ⬜ TODO |
| D2.4.2 | Unix: `tokio::net::UnixListener` on `~/.local/share/uffs/daemon.sock` with mode `0600` (via `uffs_security::fs`) | ⬜ TODO |
| D2.4.3 | Windows: `tokio::net::windows::named_pipe` on `\\.\pipe\uffs-daemon-{SID}` with owner-only DACL | ⬜ TODO |
| D2.4.4 | Connection handler: read newline-delimited JSON-RPC messages, dispatch to handler, write response | ⬜ TODO |
| D2.4.5 | Length-prefixed framing: `u32 LE length` + `JSON payload` (or newline-delimited — decide and document) | ⬜ TODO |
| D2.4.6 | Max message size: 16 MB (reject + disconnect) | ⬜ TODO |
| D2.4.7 | Max concurrent connections: 32 | ⬜ TODO |
| D2.4.8 | Read timeout: 30 seconds per message | ⬜ TODO |
| D2.4.9 | Peer credential check on accept (Unix: `getpeereid`, Windows: `GetNamedPipeClientProcessId`) | ⬜ TODO |
| D2.4.10 | Test: connect via `nc -U ~/.local/share/uffs/daemon.sock`, send `{"jsonrpc":"2.0","id":1,"method":"status"}`, verify response | ⬜ TODO |

### Wave D2.5 — Request Handler

| ID | Task | Status |
|----|------|--------|
| D2.5.1 | Create `crates/uffs-daemon/src/handler.rs` — `handle_request(req: RpcRequest, index: &IndexManager) -> RpcResponse` | ⬜ TODO |
| D2.5.2 | Route `"search"` → `IndexManager::search()` | ⬜ TODO |
| D2.5.3 | Route `"drives"` → `IndexManager::drives()` | ⬜ TODO |
| D2.5.4 | Route `"status"` → `IndexManager::status()` + daemon uptime, memory, connections | ⬜ TODO |
| D2.5.5 | Route `"info"` → `IndexManager::info()` | ⬜ TODO |
| D2.5.6 | Route `"refresh"` → spawn `IndexManager::refresh()`, return immediate ack, send notification on complete | ⬜ TODO |
| D2.5.7 | Route `"keepalive"` → reset idle timer, return `{"ok": true}` | ⬜ TODO |
| D2.5.8 | Route `"shutdown"` → initiate graceful shutdown | ⬜ TODO |
| D2.5.9 | Unknown method → return `RpcError { code: -32601, message: "Method not found" }` | ⬜ TODO |
| D2.5.10 | Regex compilation timeout: 100ms cap on search pattern compilation | ⬜ TODO |
| D2.5.11 | Response limit cap: max 100,000 rows per search response | ⬜ TODO |

### Wave D2.6 — Lifecycle Manager

| ID | Task | Status |
|----|------|--------|
| D2.6.1 | Create `crates/uffs-daemon/src/lifecycle.rs` — `LifecycleManager` | ⬜ TODO |
| D2.6.2 | PID file: write on start (`~/.local/share/uffs/daemon.pid`), content = `{pid}\n{start_timestamp}\n{exe_path_hash}`, permissions `0600` | ⬜ TODO |
| D2.6.3 | PID file: remove on graceful shutdown | ⬜ TODO |
| D2.6.4 | PID file: stale PID check on startup (if PID file exists but process is dead → clean up and proceed) | ⬜ TODO |
| D2.6.5 | Idle timer: configurable timeout (default 10 min), reset on every query/keepalive | ⬜ TODO |
| D2.6.6 | Idle timer: differentiated timeouts — 5 min after CLI, 15 min after TUI/GUI/MCP | ⬜ TODO |
| D2.6.7 | Idle timer: do NOT retire if active connections exist (check before firing) | ⬜ TODO |
| D2.6.8 | Auto-retire: save updated `.uffs` caches (if MFT changed via USN), remove PID file, close socket, exit | ⬜ TODO |
| D2.6.9 | `--no-retire` flag: disable idle timer entirely (persistent daemon) | ⬜ TODO |
| D2.6.10 | Signal handling: `SIGTERM`/`SIGINT` → graceful shutdown (same as auto-retire) | ⬜ TODO |
| D2.6.11 | Test: start daemon, verify PID file exists, wait for idle timeout, verify PID file removed + process exited | ⬜ TODO |

### Wave D2.7 — Integration Test

| ID | Task | Status |
|----|------|--------|
| D2.7.1 | End-to-end test: start daemon, connect via socket, send `search` request, verify results, send `shutdown` | ⬜ TODO |
| D2.7.2 | Test: daemon loads from `.uffs` cache files (Mac offline mode) | ⬜ TODO |
| D2.7.3 | Test: daemon loads from live MFT (Windows) | ⬜ TODO |
| D2.7.4 | Test: concurrent clients — 3 connections, interleaved queries, verify no corruption | ⬜ TODO |
| D2.7.5 | Test: idle timeout fires after no activity | ⬜ TODO |
| D2.7.6 | Benchmark: query latency (target: <10ms for trigram, <50ms for full scan) | ⬜ TODO |

---

## Phase D3: Client Library (`uffs-client`)

> **Goal**: A library crate that any surface uses to talk to the daemon.  
> **Effort**: 3–4 days  
> **Blocking**: D4, D5, D6

### Wave D3.1 — Crate Scaffold & Types

| ID | Task | Status |
|----|------|--------|
| D3.1.1 | Create `crates/uffs-client/Cargo.toml` (deps: uffs-security, tokio, serde, serde_json, tracing, dirs-next, thiserror) | ⬜ TODO |
| D3.1.2 | Create `crates/uffs-client/src/lib.rs` with public API | ⬜ TODO |
| D3.1.3 | Add to workspace members + deps | ⬜ TODO |
| D3.1.4 | Extract protocol types from `uffs-daemon/src/protocol.rs` to shared location (either `uffs-core` or a new `uffs-protocol` module in uffs-client) | ⬜ TODO |

### Wave D3.2 — Connection & Auto-Start

| ID | Task | Status |
|----|------|--------|
| D3.2.1 | `UffsClient::connect() -> Result<Self>` — attempt socket connection | ⬜ TODO |
| D3.2.2 | Auto-start: if socket doesn't exist or connect fails → spawn `uffs-daemon` as detached process | ⬜ TODO |
| D3.2.3 | Wait for daemon ready: poll socket with backoff (10ms, 20ms, 40ms, ..., max 30s) | ⬜ TODO |
| D3.2.4 | Daemon identity verification: read PID file → verify exe path (from `CACHE_SECURITY_ANALYSIS.md` §8.3) | ⬜ TODO |
| D3.2.5 | Platform socket paths: `~/.local/share/uffs/daemon.sock` (Mac), `$XDG_RUNTIME_DIR/uffs/daemon.sock` (Linux), `\\.\pipe\uffs-daemon-{SID}` (Windows) | ⬜ TODO |
| D3.2.6 | Test: client auto-starts daemon, connects, queries, receives results | ⬜ TODO |

### Wave D3.3 — Query API

| ID | Task | Status |
|----|------|--------|
| D3.3.1 | `client.search(params: SearchParams) -> Result<SearchResponse>` | ⬜ TODO |
| D3.3.2 | `client.drives() -> Result<DrivesResponse>` | ⬜ TODO |
| D3.3.3 | `client.status() -> Result<StatusResponse>` | ⬜ TODO |
| D3.3.4 | `client.info(path: &str) -> Result<InfoResponse>` | ⬜ TODO |
| D3.3.5 | `client.refresh(drives: Option<Vec<char>>) -> Result<()>` (returns immediately, daemon sends notification when done) | ⬜ TODO |
| D3.3.6 | `client.shutdown() -> Result<()>` | ⬜ TODO |
| D3.3.7 | Internal: `send_request()` + `read_response()` with timeout (5s default) | ⬜ TODO |

### Wave D3.4 — Keepalive & Reconnect

| ID | Task | Status |
|----|------|--------|
| D3.4.1 | `client.keepalive()` — send keepalive, reset daemon idle timer | ⬜ TODO |
| D3.4.2 | Auto-keepalive: background task sends keepalive every 60s for long-lived sessions | ⬜ TODO |
| D3.4.3 | `client.set_session_type(SessionType::Cli | Tui | Gui | Mcp)` — tells daemon which idle timeout to use | ⬜ TODO |
| D3.4.4 | Reconnect: on `ConnectionReset` / `BrokenPipe`, auto-restart daemon + reconnect (max 3 attempts) | ⬜ TODO |
| D3.4.5 | Notification listener: async stream of daemon notifications (drive loaded, refresh complete) | ⬜ TODO |
| D3.4.6 | Test: kill daemon while client connected → client reconnects transparently | ⬜ TODO |

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
| D4.1.1 | Create `crates/uffs-mcp/Cargo.toml` (deps: uffs-client, tokio, serde, serde_json, anyhow, tracing) | ⬜ TODO |
| D4.1.2 | Create `crates/uffs-mcp/src/main.rs` — stdio read loop | ⬜ TODO |
| D4.1.3 | Add to workspace members | ⬜ TODO |

### Wave D4.2 — MCP Protocol

| ID | Task | Status |
|----|------|--------|
| D4.2.1 | Handle `initialize` → respond with server info + capabilities | ⬜ TODO |
| D4.2.2 | Handle `tools/list` → advertise `uffs_search`, `uffs_drives`, `uffs_status`, `uffs_info` | ⬜ TODO |
| D4.2.3 | Tool `uffs_search`: translate MCP params → `client.search()`, format results as MCP content | ⬜ TODO |
| D4.2.4 | Tool `uffs_drives`: `client.drives()` → MCP content | ⬜ TODO |
| D4.2.5 | Tool `uffs_status`: `client.status()` → MCP content | ⬜ TODO |
| D4.2.6 | Tool `uffs_info`: `client.info(path)` → MCP content (all 25 columns) | ⬜ TODO |
| D4.2.7 | Rich tool descriptions with parameter schemas and examples (for agent context) | ⬜ TODO |

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

## Phase D7: Access Broker (Windows, Deferred)

> **Deferred** — implement when Windows UX polish is needed.

| ID | Task | Status |
|----|------|--------|
| D7.1 | Create `crates/uffs-broker/Cargo.toml` | ⬜ DEFERRED |
| D7.2 | Windows Service scaffold (`--install`, `--uninstall`, `--start`, `--stop`) | ⬜ DEFERRED |
| D7.3 | Named pipe server (Administrators + daemon SID DACL) | ⬜ DEFERRED |
| D7.4 | Client process verification (PID → exe path → Authenticode) | ⬜ DEFERRED |
| D7.5 | Handle request: receive drive letter → return read-only volume handle | ⬜ DEFERRED |
| D7.6 | Audit logging to Windows Event Log | ⬜ DEFERRED |
| D7.7 | `uffs-daemon` broker client: detect broker, request handles instead of self-elevating | ⬜ DEFERRED |

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
| **D1** Shared Types & Code Extraction | ⬜ NOT STARTED | — | — | |
| **D2** Daemon Foundation | ⬜ NOT STARTED | — | — | Depends on D1 |
| **D3** Client Library | ⬜ NOT STARTED | — | — | Depends on D2 |
| **D4** MCP Adapter | ⬜ NOT STARTED | — | — | Depends on D3 |
| **D5** CLI Migration | ⬜ NOT STARTED | — | — | Depends on D3 |
| **D6** TUI Migration | ⬜ NOT STARTED | — | — | Depends on D3 |
| **D7** Access Broker | ⬜ DEFERRED | — | — | |
| **D8** HTTP/SSE | ⬜ DEFERRED | — | — | |

### Wave-Level Status

| Wave | Tasks | Done | Remaining | Status |
|------|-------|------|-----------|--------|
| D1.1 CompactRecord extraction | 6 | 0 | 6 | ⬜ |
| D1.2 TrigramIndex extraction | 5 | 0 | 5 | ⬜ |
| D1.3 Search backend extraction | 9 | 0 | 9 | ⬜ |
| D1.4 FullRecordReader extraction | 3 | 0 | 3 | ⬜ |
| D1.5 Column definitions | 3 | 0 | 3 | ⬜ |
| D1.6 Format functions cleanup | 5 | 0 | 5 | ⬜ |
| D1.7 Polars re-export cleanup | 3 | 0 | 3 | ⬜ |
| D2.1 Daemon scaffold | 5 | 0 | 5 | ⬜ |
| D2.2 Protocol types | 5 | 0 | 5 | ⬜ |
| D2.3 Index loading | 8 | 0 | 8 | ⬜ |
| D2.4 IPC server | 10 | 0 | 10 | ⬜ |
| D2.5 Request handler | 11 | 0 | 11 | ⬜ |
| D2.6 Lifecycle manager | 11 | 0 | 11 | ⬜ |
| D2.7 Daemon integration test | 6 | 0 | 6 | ⬜ |
| D3.1 Client scaffold | 4 | 0 | 4 | ⬜ |
| D3.2 Connection & auto-start | 6 | 0 | 6 | ⬜ |
| D3.3 Query API | 7 | 0 | 7 | ⬜ |
| D3.4 Keepalive & reconnect | 6 | 0 | 6 | ⬜ |
| D3.5 Client integration test | 4 | 0 | 4 | ⬜ |
| D4.1 MCP scaffold | 3 | 0 | 3 | ⬜ |
| D4.2 MCP protocol | 7 | 0 | 7 | ⬜ |
| D4.3 MCP E2E test | 3 | 0 | 3 | ⬜ |
| D5.1 CLI client integration | 4 | 0 | 4 | ⬜ |
| D5.2 CLI query routing | 5 | 0 | 5 | ⬜ |
| D5.3 CLI validation | 3 | 0 | 3 | ⬜ |
| D6.1 TUI client integration | 4 | 0 | 4 | ⬜ |
| D6.2 TUI search-as-you-type | 5 | 0 | 5 | ⬜ |
| D6.3 TUI loading state | 3 | 0 | 3 | ⬜ |
| D6.4 TUI keepalive | 3 | 0 | 3 | ⬜ |
| D6.5 TUI validation | 5 | 0 | 5 | ⬜ |
| **TOTAL (active)** | **169** | **0** | **169** | |

### Completion Log

```
Date        | ID       | Description                              | Commit
────────────┼──────────┼──────────────────────────────────────────┼─────────
            |          |                                          |
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
| 1 | Protocol framing: newline-delimited JSON vs length-prefixed? | ⬜ OPEN | Decide in D2.4.5 |
| 2 | Protocol types: keep in uffs-daemon or extract to uffs-core? | ⬜ OPEN | Decide in D3.1.4 |
| 3 | Notification channel: same socket bidirectional or separate? | ⬜ OPEN | Decide in D2.4 |
| 4 | Warm restart: persist compact index to `.uffs-compact` sidecar? | ⬜ OPEN | Measure first, optimize if >5s |
| 5 | TUI debounce: 50ms fixed or adaptive based on daemon latency? | ⬜ OPEN | Start fixed, measure |
| 6 | Windows pipe name: SID-based or fixed `uffs-daemon`? | ⬜ OPEN | SID-based per security doc |

---

*Document Version: 1.0*  
*Last Updated: 2026-03-26*  
*Reference: `docs/architecture/DAEMON_SERVICE_ARCHITECTURE.md`*
