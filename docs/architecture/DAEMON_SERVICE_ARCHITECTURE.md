# UFFS Daemon & Service Architecture

> **Status**: **Implemented** — Phases 1–5 complete, Phase 6 (HTTP/SSE) deferred
> **Original design**: 2026-03-26
> **Last updated**: 2026-04-01 (reflects production validation at v0.4.51)
> **Scope**: Unified backend service for CLI, TUI, GUI, and MCP surfaces

---

## Executive Summary

UFFS uses a **unified daemon architecture** where a single background process
holds the compact search index in memory, serving all surfaces (CLI, TUI, GUI,
MCP) via IPC. The daemon auto-starts on first client request and auto-retires
after an idle timeout, freeing all memory.

Previously, each surface loaded its own MFT index per invocation — CLI paid
5–30s cold start per run, TUI held 7–8 GiB in-process, and concurrent sessions
duplicated everything. The daemon eliminates all of this.

### Key Properties (measured, v0.4.51, 25.8M records, 7 drives)

| Property | Design Target | Measured |
|----------|---------------|----------|
| **Steady-state memory** | ~7–8 GiB | ~7.3 GiB |
| **Peak during load** | ~10–13 GiB | ~10 GiB |
| **Query latency (warm)** | <10ms (trigram) | **9ms median** (34-test suite) |
| **Query latency (filtered)** | <50ms (full scan + filter) | 8–1,886ms (depends on filter type) |
| **Cold start** | ~30s | **12.4s** (7 drives, .uffs cache) |
| **Warm query after cold** | <1s | **9ms** (no difference from fully warm) |
| **Idle memory** | 0 | 0 (daemon auto-retires) |

---

## Architecture Overview

```
                    ┌───────────────────────────────────────────────┐
                    │              Surface Layer                     │
                    │                                               │
                    │  ┌─────┐  ┌─────┐  ┌─────┐  ┌───────────┐  │
                    │  │ CLI │  │ TUI │  │ GUI │  │ MCP stdio │  │
                    │  └──┬──┘  └──┬──┘  └──┬──┘  └─────┬─────┘  │
                    │     │        │        │            │         │
                    │     └────────┴────────┴────────────┘         │
                    │                   │                           │
                    │          ┌────────┴────────┐                 │
                    │          │  uffs-client     │                 │
                    │          │  (library crate) │                 │
                    │          └────────┬────────┘                 │
                    └──────────────────┼───────────────────────────┘
                                       │ IPC
                         ╔═════════════╧═══════════════╗
                         ║   AF_UNIX domain socket       ║
                         ║   (all platforms — see §IPC)  ║
                         ╚═════════════╤═══════════════╝
                                       │
┌──────────────────────────────────────┼──────────────────────────────────────┐
│                              uffs-daemon                                    │
│                         (user-space process)                                │
│                                                                             │
│  ┌───────────────────┐  ┌────────────────────┐  ┌───────────────────────┐  │
│  │  MFT Index Store   │  │  Compact Index      │  │  Query Engine         │  │
│  │  (MftIndex per drv)│  │  (72-byte records)  │  │  (search/sort/filter) │  │
│  │  ~5.9 GiB          │→│  ~2.1 GiB           │  │  trigram + tree       │  │
│  │  (dropped after    │  │  (retained)         │  │  <10ms per query      │  │
│  │   compact build)   │  │                     │  │                       │  │
│  └───────────────────┘  └────────────────────┘  └───────────────────────┘  │
│                                                                             │
│  ┌───────────────────┐  ┌────────────────────┐  ┌───────────────────────┐  │
│  │  IPC Server        │  │  Lifecycle Manager  │  │  FS Watcher           │  │
│  │  JSON-RPC over     │  │  idle timer         │  │  USN (Win) / manual   │  │
│  │  socket/pipe       │  │  auto-retire        │  │  refresh (Mac/Linux)  │  │
│  └───────────────────┘  └────────────────────┘  └───────────────────────┘  │
│                                                                             │
│  ┌──────────────────────────────────────────────────────────────────────┐   │
│  │  MFT Access Layer                                                    │   │
│  │                                                                      │   │
│  │  Windows:  Access Broker service (tiny, always elevated)             │   │
│  │            OR direct elevated access (UAC on daemon start)           │   │
│  │  Mac/Linux: Offline .uffs / .iocp / .bin files (no elevation)       │   │
│  └──────────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Components

### 1. `uffs-daemon` — The Search Engine Process ✅

A single user-space process that:

1. **Loads the MFT** from cache (`.uffs`) or raw source (live MFT / offline files)
2. **Builds the compact index** (72-byte CompactRecord × N records) + trigram + children
3. **Drops the full MftIndex** after compact build (~5.9 GiB freed)
4. **Serves queries** via IPC (JSON-RPC over AF_UNIX socket, all platforms)
5. **Maintains live updates** via USN journal (Windows) or manual refresh
6. **Auto-retires** after an idle timeout, freeing all memory

#### Memory Lifecycle (measured on Mac M4, 25.9M records, 7 drives)

```
Time     Memory    Phase
─────    ────────  ──────────────────────────────────
0s       0 GiB    Daemon starts
1-5s     ↗ rising  Loading .uffs caches (parallel, 7 drives)
5-15s    ~10 GiB  MftIndex + CompactIndex coexist (peak)
15-25s   ~10 GiB  Building trigrams, children index
25-31s   ↘ drop    Dropping MftIndex per drive as compact completes
31s+     ~7.3 GiB  Steady state — only compact index in memory
         ▼ 0 GiB   After idle timeout → daemon exits
```

Source: `LOG/2026_03_26_tui_memory_footprint_analysis.md`

#### Steady-State Memory Budget (25.9M records)

| Component | Per Record | Total |
|-----------|-----------|-------|
| CompactRecord (68 B + 4 pad) | 72 B | 1.7 GiB |
| Names blob | ~15 B avg | 390 MB |
| Names_lower (lowercase copy) | ~15 B avg | 390 MB |
| Children index | ~6 B avg | 155 MB |
| Trigram postings | ~40–80 B avg | 1.0–2.0 GiB |
| Allocator overhead + page rounding | variable | ~2–3 GiB |
| **Observed steady state** | | **~7.3 GiB** |

### 2. `uffs-client` — Thin Client Library ✅

A Rust library crate that all surfaces depend on. Abstracts the daemon
connection entirely — surfaces never deal with IPC, lifecycle, or elevation.

```rust
// All any surface does:
let client = UffsClient::connect()?;         // auto-starts daemon if needed
let results = client.search(query).await?;   // 9ms median once warm
let drives = client.drives().await?;         // list loaded drives
let status = client.status().await?;         // loading progress, memory, uptime
client.refresh().await?;                      // trigger MFT re-read
// drop(client) → daemon starts idle timer
```

**Responsibilities (all implemented):**
- **Auto-start**: If daemon isn't running, spawn it (detached process, `CreateProcessW` on Windows)
- **Connect**: Open AF_UNIX socket, retry with exponential backoff (up to 20 attempts)
- **Keepalive**: Long-lived sessions (TUI, GUI, MCP) send periodic heartbeats
- **Reconnect**: If daemon crashes or retires, transparently restart + reconnect
- **Serialization**: Query structs ↔ JSON-RPC messages (line-delimited JSON)
- **Shmem bulk transfer**: Results >100K rows bypass IPC via shared-memory `.bin` files

### 3. `uffs-mcp` — MCP Protocol Adapter ✅

A thin binary that bridges MCP's stdio JSON-RPC protocol to `uffs-client`:

```
AI Agent (Claude / Cursor / Windsurf / etc.)
    ↓ MCP stdio (JSON-RPC over stdin/stdout)
uffs-mcp
    ↓ uffs-client (IPC to daemon)
uffs-daemon
    ↓ query engine
results
```

Exposes 3–4 MCP tools:

| Tool | Description |
|------|-------------|
| `uffs_search` | Search files — pattern, flags, sort, columns, limit |
| `uffs_drives` | List available drives + record counts + load status |
| `uffs_status` | Daemon health — memory, uptime, last refresh |
| `uffs_info` | Detailed info for a specific file path (all 25 columns) |

The agent adds one line to its MCP config:
```json
{ "uffs": { "command": "uffs-mcp" } }
```

### 4. Access Broker (Windows only, optional) ✅

On Windows, reading the MFT requires elevation (`SeBackupPrivilege`).
Two strategies, in order of preference:

#### Strategy A: Access Broker Service (polished UX)

A minimal Windows Service (~5 MB memory) whose only job is to provide
privileged file handles to the daemon:

```
┌──────────────┐         ┌──────────────────────┐
│  uffs-daemon  │ ──IPC──→│  uffs-broker          │
│  (non-elevated)│         │  (Windows Service,    │
│               │ ←handle──│   always elevated)    │
└──────────────┘         └──────────────────────┘
```

- Installed once with admin rights: `uffs-broker --install`
- Runs as `LocalSystem` — always has MFT access
- Provides volume handles via named pipe to the daemon
- Daemon itself runs non-elevated — no UAC prompts ever
- Memory footprint: negligible (just a handle broker, no index)

#### Strategy B: Direct Elevation (simple, no install)

The daemon itself requests elevation on startup:

- Binary manifest includes `requireAdministrator` (or `highestAvailable`)
- Windows shows one UAC prompt when daemon starts
- Daemon stays elevated for its lifetime
- Next auto-retire + restart → another UAC prompt

**Strategy B is the starting point.** Strategy A is a polish feature for
users who want zero UAC prompts.

#### Mac / Linux: No Elevation Needed

Offline `.uffs` / `.iocp` / `.bin` files are regular files — no elevation
required. The daemon reads them directly.

---

## Daemon Lifecycle

### Startup Flow

```
1. Surface calls UffsClient::connect()
       │
2. Client checks for daemon:
   ├─ PID file exists + socket responds → CONNECT (fast path, <10ms)
   └─ PID file missing or socket dead:
       │
3. Spawn daemon as detached process
   ├─ Windows: uffs-daemon.exe (elevated via manifest or broker)
   └─ Mac/Linux: uffs-daemon (no elevation)
       │
4. Daemon starts:
   a. Write PID file
   b. Open IPC listener (socket/pipe)
   c. Accept connections immediately (can respond "loading")
   d. Begin loading index in background threads (parallel per drive)
       │
5. Client connects, receives status:
   ├─ {"status": "loading", "progress": "3/7 drives", "pct": 42}
   └─ {"status": "ready", "drives": 7, "records": 25900000}
       │
6. Client sends queries
```

### Auto-Retire Flow

```
Query received → reset idle timer
Keepalive received → reset idle timer
                     │
               ┌─────┴─────┐
               │ Idle Timer │
               │ (default   │
               │  10 min)   │
               └─────┬─────┘
                     │ timer expires
                     ▼
              Active connections? ──yes──→ do NOT retire (reset timer)
                     │ no
                     ▼
              Save updated .uffs caches (if MFT changed)
                     │
              Remove PID file
                     │
              Close IPC listener
                     │
              Exit (OS reclaims all memory)
```

### Idle Timeout Strategy

| Condition | Timeout | Rationale |
|-----------|---------|-----------|
| No connections, last used by CLI | 5 min | CLI is fire-and-forget |
| No connections, last used by TUI/GUI/MCP | 15 min | User may return soon |
| Active connections with keepalive | Never | Session in progress |
| `--no-retire` flag | Never | User explicitly wants persistent daemon |

### PID File & Socket Locations

All platforms use AF_UNIX domain sockets (see [Design Decision: IPC Transport](#design-decision-ipc-transport-af_unix-everywhere)).

| Platform | PID file | Socket (AF_UNIX) |
|----------|----------|------------------|
| **Mac** | `~/.local/share/uffs/daemon.pid` | `~/.local/share/uffs/daemon.sock` |
| **Linux** | `$XDG_RUNTIME_DIR/uffs/daemon.pid` | `$XDG_RUNTIME_DIR/uffs/daemon.sock` |
| **Windows** | `%LOCALAPPDATA%\uffs\daemon.pid` | `%LOCALAPPDATA%\uffs\daemon.sock` |

---

## IPC Protocol ✅

JSON-RPC 2.0 over AF_UNIX domain socket (all platforms).
Same protocol foundation as MCP — the MCP adapter is a thin translation layer.
Messages are line-delimited JSON (one JSON object per line).

### Requests

#### `search` — Primary query

```json
{
  "jsonrpc": "2.0", "id": 1, "method": "search",
  "params": {
    "pattern": "*.rs",
    "files_only": true,
    "dirs_only": false,
    "name_only": false,
    "case_sensitive": false,
    "whole_word": false,
    "hide_system": false,
    "attr": ["hidden", "!system"],
    "min_size": null,
    "max_size": null,
    "newer": "7d",
    "older": null,
    "newer_accessed": null,
    "older_accessed": null,
    "min_descendants": null,
    "max_descendants": null,
    "sort": "modified:desc,name:asc",
    "columns": ["name", "size", "modified", "path"],
    "limit": 500,
    "drives": null
  }
}
```

#### `search` — Response

```json
{
  "jsonrpc": "2.0", "id": 1,
  "result": {
    "status": "ok",
    "count": 847,
    "total_scanned": 25900000,
    "elapsed_ms": 8,
    "rows": [
      {
        "name": "main.rs",
        "size": 4096,
        "modified": "2026-03-25T19:44:00Z",
        "path": "C:\\src\\uffs\\crates\\uffs-tui\\src\\main.rs",
        "drive": "C"
      }
    ]
  }
}
```

#### `drives` — List loaded drives

```json
→ {"jsonrpc":"2.0", "id":2, "method":"drives"}
← {"jsonrpc":"2.0", "id":2, "result": {
     "drives": [
       {"letter":"C", "records":3400000, "loaded":true},
       {"letter":"D", "records":7100000, "loaded":true},
       {"letter":"S", "records":8300000, "loading":true, "pct":65}
     ],
     "total_records": 25900000
   }}
```

#### `status` — Daemon health

```json
→ {"jsonrpc":"2.0", "id":3, "method":"status"}
← {"jsonrpc":"2.0", "id":3, "result": {
     "status": "ready",
     "uptime_seconds": 342,
     "memory_mb": 7500,
     "connections": 2,
     "idle_timeout_seconds": 600,
     "last_refresh": "2026-03-25T19:44:00Z"
   }}
```

#### `refresh` — Reload MFT data

```json
→ {"jsonrpc":"2.0", "id":4, "method":"refresh", "params": {"drives": ["C"]}}
← {"jsonrpc":"2.0", "id":4, "result": {"status":"refreshing"}}
  ... (async notification when complete) ...
← {"jsonrpc":"2.0", "method":"refresh_complete", "params": {
     "drive":"C", "records":3400012, "elapsed_ms":5200
   }}
```

#### `info` — Detailed record info (all 25 columns)

```json
→ {"jsonrpc":"2.0", "id":5, "method":"info",
   "params": {"path": "C:\\Users\\rob\\README.md"}}
← {"jsonrpc":"2.0", "id":5, "result": {
     "name": "README.md",
     "path": "C:\\Users\\rob\\README.md",
     "size": 1234,
     "size_on_disk": 4096,
     "created": "2026-01-15T10:30:00Z",
     "modified": "2026-03-25T19:44:00Z",
     "accessed": "2026-03-25T20:00:00Z",
     "readonly": false,
     "hidden": false,
     "system": false,
     "archive": true,
     "compressed": false,
     "encrypted": false,
     "sparse": false,
     "reparse": false,
     "directory": false,
     "descendants": 0,
     "treesize": 0,
     "attributes": "0x00000020"
   }}
```

#### `keepalive` — Reset idle timer

```json
→ {"jsonrpc":"2.0", "id":6, "method":"keepalive"}
← {"jsonrpc":"2.0", "id":6, "result": {"ok": true}}
```

#### `shutdown` — Graceful exit

```json
→ {"jsonrpc":"2.0", "id":7, "method":"shutdown"}
← {"jsonrpc":"2.0", "id":7, "result": {"ok": true}}
```

---

## Crate Structure

```
crates/
  uffs-polars/     # Polars facade — all crates depend on this, NOT polars directly
  uffs-mft/        # MFT reading engine, NTFS structures, Windows I/O
  uffs-core/       # Query engine: compact index, trigram, search, sort, filter, path resolver

  uffs-daemon/     # ✅ Daemon process: index management, IPC server, lifecycle
  uffs-client/     # ✅ Thin client library: connect, auto-start, query, keepalive, shmem
  uffs-mcp/        # ✅ MCP stdio adapter over uffs-client

  uffs-cli/        # ✅ Refactored — daemon-only (standalone pipeline removed v0.4.51)
  uffs-tui/        # 🟡 Refactored — daemon-only (standalone removed, UX polish pending)
  uffs-gui/        # ⬜ Future — will use uffs-client from the start
  uffs-diag/       # Diagnostic tools (no change)
```

### Dependency Graph (target state)

```
uffs-cli ──────► uffs-client ──► (IPC to daemon)
uffs-tui ──────► uffs-client
uffs-gui ──────► uffs-client
uffs-mcp ──────► uffs-client

uffs-daemon ───► uffs-core ──► uffs-mft ──► uffs-polars
               └► uffs-tui::compact (or extract to uffs-core)
```

### What Moves Where

| Component | Currently In | Moves To | Reason |
|-----------|-------------|----------|--------|
| CompactRecord, build_compact_index | uffs-tui/compact.rs | uffs-core or uffs-daemon | Shared by daemon, all surfaces |
| TrigramIndex | uffs-tui/backend.rs | uffs-core or uffs-daemon | Query engine lives in daemon |
| Search routing (name vs tree) | uffs-tui/backend.rs | uffs-daemon | Daemon owns query execution |
| MftIndex loading | uffs-tui/compact.rs + uffs-cli | uffs-daemon | Single owner of MFT data |
| USN refresh | uffs-tui/compact.rs | uffs-daemon | Daemon manages live updates |
| FullRecordReader | uffs-tui/full_record.rs | uffs-daemon | Daemon has .uffs file access |

---

## How Each Surface Uses the Daemon

### CLI ✅

**Before:** Every `uffs *.rs` invocation loaded the MFT from scratch or cache,
built extension index, scanned, output, exited. Cold: 5–30s per run.

**After (measured, v0.4.51):** CLI calls `UffsClient::connect()`, sends a
`search` request via IPC, prints results, disconnects. 34/34 CLI flag tests pass.

```
$ uffs "*.rs" --files-only --sort modified:desc --limit 100
# Cold (daemon not running): 12.4s (spawn + load 7 drives)
# → subsequent queries: 9ms median
$ uffs "*.rs" --newer 7d
# → 8ms (daemon already warm)
```

> **Note:** The standalone in-process pipeline (`UFFS_STANDALONE=1`) was
> removed in v0.4.51. All CLI searches now route through the daemon. The
> standalone code (streaming pipeline, `QueryFilters`, `dispatch_search`,
> `raw_io.rs`) has been deleted.

### TUI 🟡 (core wiring done, UX polish pending)

**Before:** TUI loaded all drives in-process (~7 GiB, 5–30s startup).

**After:** TUI connects to daemon via `uffs-client`. Startup is instant if
daemon is already warm (from a prior CLI run). TUI sends queries on each
keystroke, daemon responds in <10ms. The standalone in-process pipeline
(`search_standalone`, `UFFS_STANDALONE=1`) was removed in v0.4.51.

**Remaining TUI work (D6.2–D6.5):** Search-as-you-type debounce, loading
progress bar, auto-keepalive, UX parity validation. These are UI polish —
the daemon backend is the same engine proven with 34/34 CLI tests.

### GUI

Built from the start on `uffs-client`. Same model as TUI but with a native
window. Thin rendering layer, all search logic in the daemon.

### MCP

`uffs-mcp` is a stdio adapter. The MCP process itself is ~5 MB — just protocol
translation. All heavy lifting is in the daemon.

```
Agent spawns uffs-mcp
  → uffs-mcp calls UffsClient::connect()
    → daemon auto-starts if needed (5-30s first time)
  → Agent sends tools/call → uffs_search
    → uffs-mcp translates to daemon search request
    → daemon responds in <10ms
    → uffs-mcp formats as MCP result
  → Agent gets results in <100ms total
```

---

## Elevation & Access Flow

### Windows

```
Surface starts → UffsClient::connect()
    │
    ├─ Daemon running? → connect, done
    │
    └─ Daemon not running:
        │
        ├─ Access Broker service installed?
        │   YES → spawn daemon (non-elevated)
        │          daemon connects to broker for volume handles
        │          no UAC prompt
        │
        └─ NO → spawn daemon with elevation
                 Windows shows UAC prompt (once per daemon lifecycle)
                 daemon reads MFT directly
```

Once the daemon has MFT access (via either path), it does not need to
re-request elevation until it retires and restarts. A typical workflow:

1. User opens TUI → daemon starts (UAC prompt once) → loads 7 drives in 30s
2. User searches for 20 minutes → daemon stays warm
3. User closes TUI → daemon idle timer starts (15 min)
4. User runs `uffs *.log` from CLI within 15 min → instant results (no UAC)
5. 15 min idle → daemon retires → 0 memory
6. User runs `uffs *.txt` later → daemon restarts (UAC prompt again)

### Mac / Linux

No elevation. Daemon reads offline files directly:

```
Surface starts → UffsClient::connect()
    │
    └─ Daemon not running:
        → spawn uffs-daemon --data-dir ~/uffs_data
        → daemon loads .uffs / .iocp / .bin files (5–30s)
        → ready
```

---

## Implementation Phases

### Phase 1: Daemon + Client Foundation ✅

| Task | Status | Notes |
|------|--------|-------|
| `uffs-daemon` crate scaffold | ✅ | Binary crate, depends on uffs-core/uffs-mft |
| IPC server (AF_UNIX socket, all platforms) | ✅ | JSON-RPC 2.0, tokio-based async |
| `search` method handler | ✅ | Compact index + trigram, 9ms median |
| `drives` and `status` methods | ✅ | Metadata queries |
| `refresh`, `info`, `keepalive`, `shutdown` | ✅ | All 7 RPC methods implemented |
| PID file + socket management | ✅ | Create on start, remove on exit |
| Idle timer + auto-retire | ✅ | Configurable timeout, keepalive resets |
| `uffs-client` crate scaffold | ✅ | Library crate, auto-start + connect |
| Auto-start daemon from client | ✅ | Spawn detached, exponential backoff retry |
| Shmem bulk transfer | ✅ | >100K rows via shared `.bin` files |
| Extract compact index to `uffs-core` | ✅ | Moved from uffs-tui |
| Extract trigram + search engine to `uffs-core` | ✅ | Moved from uffs-tui |

### Phase 2: MCP Adapter ✅

| Task | Status | Notes |
|------|--------|-------|
| `uffs-mcp` crate scaffold | ✅ | Binary crate, MCP stdio protocol |
| MCP `initialize` + `tools/list` | ✅ | Advertises uffs_search, uffs_drives, uffs_status, uffs_info |
| MCP `tools/call` → client.search() | ✅ | Translates MCP params to daemon query |
| Tool descriptions with examples | ✅ | Rich descriptions for agent context |
| E2E validation | ✅ | Tested with MCP-compatible agents |

### Phase 3: CLI Migration ✅

| Task | Status | Notes |
|------|--------|-------|
| Add `uffs-client` dependency to uffs-cli | ✅ | |
| Route all queries through daemon | ✅ | Daemon-only — no fallback path |
| 34/34 CLI flag validation suite | ✅ | Cold: 12.4s, Warm: 9ms median |
| Standalone pipeline removed | ✅ | v0.4.51: streaming, QueryFilters, dispatch, raw_io deleted |

### Phase 4: TUI Migration 🟡

| Task | Status | Notes |
|------|--------|-------|
| Connect TUI to daemon via `uffs-client` | ✅ | Core wiring complete |
| Remove standalone pipeline | ✅ | v0.4.51: `search_standalone`, `UFFS_STANDALONE` deleted |
| Search-as-you-type debounce | ⬜ | D6.2 |
| Loading state from daemon events | ⬜ | D6.3 |
| Keepalive while TUI is open | 🟡 | D6.4: session type done, auto-keepalive pending |
| Validate: search latency, UX parity | ⬜ | D6.5 |

### Phase 5: Access Broker (Windows) ✅

| Task | Status | Notes |
|------|--------|-------|
| `uffs-broker` Windows Service | ✅ | Tiny privilege broker |
| `--install` / `--uninstall` service management | ✅ | One-time setup |
| Handle passing via named pipe | ✅ | Daemon ↔ broker IPC |

### Phase 6: HTTP/SSE Transport (remote access) ⬜ DEFERRED

| Task | Status | Notes |
|------|--------|-------|
| Optional HTTP listener in daemon | ⬜ | REST + SSE for remote clients |
| Authentication (API key or mTLS) | ⬜ | Required for network exposure |
| MCP SSE transport | ⬜ | Remote agent access |

---

## Comparison with Everything

| | Everything | UFFS (measured) |
|--|-----------|-----------------|
| **Architecture** | Client + optional service | Daemon + client library |
| **Service purpose** | Elevated MFT/USN access | Full index + query engine |
| **Service memory** | ~2.5 GiB (25M, always resident) | ~7.3 GiB (while active) |
| **Idle memory** | ~2.5 GiB (always running) | **0 GiB** (auto-retire) |
| **Client memory** | ~2.5 GiB (index in client) | **<50 MB** (thin client) |
| **Query latency** | ~100–200ms (SIMD linear scan) | **9ms median** (trigram index) |
| **Install step** | Service installer | None (auto-start on first query) |
| **UAC prompts** | Once (service install) | Once per daemon lifecycle |
| **Multi-client** | GUI only | CLI ✅ + TUI 🟡 + GUI ⬜ + MCP ✅ |
| **Remote access** | Everything SDK (TCP) | Phase 6: HTTP/SSE (deferred) |
| **Extra features** | — | Treesize, descendants, 25 columns, MCP |

---

## Open Questions (resolved)

1. **Compact index location** → ✅ **`uffs-core`**. Extracted from uffs-tui to
   `uffs-core::compact` and `uffs-core::search`. Shared by daemon, CLI, TUI.

2. **IPC serialization format** → ✅ **JSON-RPC** (line-delimited JSON over AF_UNIX).
   MCP compatibility and debuggability confirmed. Shmem bulk transfer handles the
   high-volume case (>100K rows) — IPC serialization is not a bottleneck.

3. **Notification channel** → 🟡 **Pending** (D6.3). Not yet implemented — daemon
   currently responds synchronously. Async notifications for drive-load progress
   and refresh-complete events are planned for TUI loading state.

4. **Multiple daemon instances** → ✅ **Single global instance per user**. Confirmed
   as the right approach. No demand for namespaced instances.

5. **Warm restart optimization** → ✅ **Deferred** (12.4s cold start is acceptable).
   The daemon loads from `.uffs` cache in ~12s for 7 drives. A binary sidecar
   (`.uffs-compact`) could reduce this to <1s but is not needed at this point.

---

## Design Decision: IPC Transport — AF_UNIX Everywhere

The original RFC specified named pipes (`\\.\pipe\uffs-daemon`) on Windows and
Unix domain sockets on Mac/Linux. The implementation uses **AF_UNIX domain
sockets on all platforms**, including Windows.

### Rationale

| Factor | AF_UNIX | Named Pipes |
|--------|---------|-------------|
| **Cross-platform code** | ✅ **One codepath** — zero `#[cfg(windows)]` in IPC layer | ❌ Requires separate Windows async I/O |
| **Tokio support** | ✅ Native `tokio::net::UnixListener` | ⚠️ Needs `tokio-named-pipes` or raw win32 |
| **Windows support** | Win 10 1803+ (April 2018) | Since NT 3.1 |
| **Performance** | Sub-microsecond | Sub-microsecond |
| **Throughput** | Identical for <1MB payloads | Identical for <1MB payloads |
| **Security** | File-system permissions | Built-in ACLs + impersonation |
| **Industry adoption** | Docker Desktop, VS Code Remote, WSL2, GitHub CLI | Everything SDK, SQL Server |

### Why this doesn't matter for performance

The IPC transport adds **sub-microsecond** overhead regardless of choice. Our
measured 9ms median query time is dominated by search + path resolution + JSON
serialization — not socket transport. For bulk results (>100K rows), we bypass
IPC entirely via **shared-memory `.bin` files** (shmem).

### Why AF_UNIX wins for UFFS

1. **Single codepath** — All IPC code is platform-agnostic. No `#[cfg(windows)]`
   branches, no platform-specific async I/O abstractions.
2. **Tokio native** — `tokio::net::UnixListener`/`UnixStream` work identically
   on Mac, Linux, and Windows. Named pipes would need a separate async runtime.
3. **Microsoft's direction** — Microsoft actively invests in AF_UNIX (abstract
   namespace support in newer builds). Docker Desktop, VS Code, and GitHub CLI
   all use AF_UNIX on Windows.
4. **Production proven** — 34/34 CLI flag tests validated on Windows via AF_UNIX.
   12.4s cold start, 9ms median warm query. No transport-related issues observed.

Named pipes would only be better if UFFS needed Windows-native ACL security
(service-to-service auth) or pre-2018 Windows support. Neither applies.

---

## Summary

```
BEFORE                              AFTER (measured, v0.4.51)
──────                              ─────
CLI: 5-30s per run                  CLI: 9ms median (daemon warm), 12.4s cold
TUI: 7 GiB in-process              TUI: thin client via daemon IPC
GUI: TBD                            GUI: will use uffs-client (future)
MCP: N/A                            MCP: <5 MB stdio adapter ✅
                                    Daemon: ~7.3 GiB while active, 0 when idle
                                    Shared: all surfaces benefit from one warm index

Total peak (2 surfaces):            Total peak (4 surfaces):
  ~14 GiB (TUI + CLI)                ~7.3 GiB (daemon) + ~100 MB (clients)
```

The daemon is the search engine. The surfaces are just presentation layers.
One index, many views, zero wasted memory.

---

*Document Version: 2.0*
*Original: 2026-03-26 (RFC)*
*Updated: 2026-04-01 (reflects production validation at v0.4.51)*
*Based on: Production test runs in `LOG/Output`, memory analysis from `LOG/2026_03_26_tui_memory_footprint_analysis.md`*
