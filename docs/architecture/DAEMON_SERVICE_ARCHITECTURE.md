# UFFS Daemon & Service Architecture

> **Status**: Design — RFC  
> **Date**: 2026-03-26  
> **Scope**: Unified backend service for CLI, TUI, GUI, and MCP surfaces

---

## Executive Summary

UFFS today loads a full MFT index per surface invocation. Each CLI run pays
5–30s cold start. The TUI holds 7–8 GiB in-process for 25.9M records. There
is no sharing between surfaces — two concurrent sessions duplicate everything.

This document defines a **unified daemon architecture** where a single
background process holds the MFT index and compact search index in memory,
serving all surfaces (CLI, TUI, GUI, MCP) via IPC. The daemon auto-starts on
first client request and auto-retires after an idle timeout, freeing all memory.

### Key Properties

| Property | Value |
|----------|-------|
| **Steady-state memory** | ~7–8 GiB (25.9M records, 7 drives) |
| **Peak during load** | ~10–13 GiB (cached), ~30 GiB (cold) |
| **Query latency** | <10ms (trigram), <50ms (full scan + filter) |
| **Warm restart** | ~5s (from `.uffs` cache) |
| **Cold start** | ~30s (raw MFT parse on Windows) |
| **Idle memory** | 0 (daemon auto-retires) |

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
                         ║   Unix domain socket (Mac)   ║
                         ║   Named pipe (Windows)       ║
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

### 1. `uffs-daemon` — The Search Engine Process

A single user-space process that:

1. **Loads the MFT** from cache (`.uffs`) or raw source (live MFT / offline files)
2. **Builds the compact index** (72-byte CompactRecord × N records) + trigram + children
3. **Drops the full MftIndex** after compact build (~5.9 GiB freed)
4. **Serves queries** via IPC (JSON-RPC over Unix socket or named pipe)
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

### 2. `uffs-client` — Thin Client Library

A Rust library crate that all surfaces depend on. It abstracts the daemon
connection entirely — surfaces never deal with IPC, lifecycle, or elevation.

```rust
// All any surface does:
let client = UffsClient::connect()?;         // auto-starts daemon if needed
let results = client.search(query).await?;   // <10ms once warm
let drives = client.drives().await?;         // list loaded drives
let status = client.status().await?;         // loading progress, memory, uptime
client.refresh().await?;                      // trigger MFT re-read
// drop(client) → daemon starts idle timer
```

**Responsibilities:**
- **Auto-start**: If daemon isn't running, spawn it (detached process)
- **Connect**: Open IPC socket/pipe, wait for "ready" signal
- **Keepalive**: Long-lived sessions (TUI, GUI, MCP) send periodic heartbeats
- **Reconnect**: If daemon crashes or retires, transparently restart + reconnect
- **Serialization**: Query structs ↔ JSON-RPC messages

### 3. `uffs-mcp` — MCP Protocol Adapter

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

### 4. Access Broker (Windows only, optional)

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

| Platform | PID file | Socket |
|----------|----------|--------|
| **Mac** | `~/.local/share/uffs/daemon.pid` | `~/.local/share/uffs/daemon.sock` |
| **Linux** | `$XDG_RUNTIME_DIR/uffs/daemon.pid` | `$XDG_RUNTIME_DIR/uffs/daemon.sock` |
| **Windows** | `%LOCALAPPDATA%\uffs\daemon.pid` | `\\.\pipe\uffs-daemon` |

---

## IPC Protocol

JSON-RPC 2.0 over the Unix socket (Mac/Linux) or named pipe (Windows).
Same protocol foundation as MCP — the MCP adapter is a thin translation layer.

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
  uffs-core/       # EXISTING — MFT parsing, search, indexing, cache I/O
  uffs-mft/        # EXISTING — MFT reading engine, NTFS structures
  uffs-polars/     # EXISTING — DataFrame facade

  uffs-daemon/     # NEW — daemon process: index management, IPC server, lifecycle
  uffs-client/     # NEW — thin client library: connect, auto-start, query, keepalive
  uffs-mcp/        # NEW — MCP stdio adapter over uffs-client

  uffs-cli/        # EXISTING → refactor to use uffs-client (Phase 2)
  uffs-tui/        # EXISTING → refactor to use uffs-client (Phase 3)
  uffs-gui/        # EXISTING → will use uffs-client from the start
  uffs-diag/       # EXISTING — diagnostic tools (no change)
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

### CLI

**Before (current):** Every `uffs *.rs` invocation loads the MFT from scratch
or cache, builds extension index, scans, outputs, exits. Cold: 5–30s per run.

**After:** CLI calls `UffsClient::connect()`, sends a `search` request, prints
results, disconnects. If daemon is warm: **<1s total** (connect + query + output).
If daemon is cold: first run pays ~5–30s warmup, subsequent runs are instant.

```
$ uffs "*.rs" --files-only --sort modified:desc --limit 100
# First time: "Starting uffs daemon..." (5s warmup)
# → 100 results in 5.2s
$ uffs "*.rs" --newer 7d
# → 47 results in 0.3s (daemon already warm)
```

**Backward compatibility:** `uffs --standalone` flag for direct mode (no daemon),
useful for scripting environments where daemon lifecycle is undesirable.

### TUI

**Before (current):** TUI loads all drives in-process (~7 GiB, 5–30s startup).
The TUI process itself holds the compact index + trigrams.

**After:** TUI connects to daemon via `uffs-client`. Startup is instant if
daemon is already warm (from a prior CLI run). TUI sends queries on each
keystroke, daemon responds in <10ms.

The TUI process drops from ~7 GiB to **<50 MB** (just the rendering state +
display rows for the current view).

**Key change:** Search-as-you-type latency goes from <10ms (in-process) to
<15ms (IPC round-trip + query). Imperceptible difference.

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

### Phase 1: Daemon + Client Foundation

Build the core daemon and client library. Validate with a simple CLI test.

| Task | Priority | Notes |
|------|----------|-------|
| `uffs-daemon` crate scaffold | High | Binary crate, depends on uffs-core/uffs-mft |
| IPC server (Unix socket + named pipe) | High | JSON-RPC 2.0, tokio-based async |
| `search` method handler | High | Reuses existing compact index + trigram |
| `drives` and `status` methods | High | Simple metadata queries |
| PID file + socket management | High | Create on start, remove on exit |
| Idle timer + auto-retire | High | Configurable timeout, keepalive resets |
| `uffs-client` crate scaffold | High | Library crate, auto-start + connect |
| Auto-start daemon from client | High | Spawn detached process, wait for ready |
| Extract compact index to shared location | High | Move from uffs-tui to uffs-core or uffs-daemon |
| Extract trigram + search engine to shared location | High | Move from uffs-tui to uffs-core or uffs-daemon |

### Phase 2: MCP Adapter

Wire up MCP protocol over `uffs-client`.

| Task | Priority | Notes |
|------|----------|-------|
| `uffs-mcp` crate scaffold | High | Binary crate, MCP stdio protocol |
| MCP `initialize` + `tools/list` | High | Advertise uffs_search, uffs_drives, etc. |
| MCP `tools/call` → client.search() | High | Translate MCP params to query struct |
| Tool descriptions with examples | Medium | Rich descriptions for agent context |
| Test with Claude Desktop / Cursor | Medium | End-to-end validation |

### Phase 3: CLI Migration

Refactor `uffs-cli` to use `uffs-client` with `--standalone` fallback.

| Task | Priority | Notes |
|------|----------|-------|
| Add `uffs-client` dependency to uffs-cli | Medium | |
| Route queries through client when daemon available | Medium | |
| `--standalone` flag for direct mode | Medium | Backward compat for scripts |
| Performance validation: overhead of IPC | Medium | Target: <50ms added |

### Phase 4: TUI Migration

Refactor `uffs-tui` from in-process index to daemon client.

| Task | Priority | Notes |
|------|----------|-------|
| Replace MultiDriveBackend with UffsClient | Medium | |
| Adapt search-as-you-type to async IPC | Medium | <15ms round-trip target |
| Loading state from daemon progress events | Medium | |
| Keepalive while TUI is open | Medium | |
| Validate: search latency, UX parity | Medium | |

### Phase 5: Access Broker (Windows, optional)

| Task | Low | Notes |
|------|-----|-------|
| `uffs-broker` Windows Service | Low | Tiny privilege broker |
| `--install` / `--uninstall` service management | Low | One-time setup |
| Named pipe IPC between daemon and broker | Low | Handle passing |

### Phase 6: HTTP/SSE Transport (remote access)

| Task | Low | Notes |
|------|-----|-------|
| Optional HTTP listener in daemon | Low | REST + SSE for remote clients |
| Authentication (API key or mTLS) | Low | Required for network exposure |
| MCP SSE transport | Low | Remote agent access |

---

## Comparison with Everything

| | Everything | UFFS (target) |
|--|-----------|---------------|
| **Architecture** | Client + optional service | Daemon + client library |
| **Service purpose** | Elevated MFT/USN access | Full index + query engine |
| **Service memory** | ~2.5 GiB (25M, always resident) | ~7.3 GiB (while active) |
| **Idle memory** | ~2.5 GiB (always running) | **0 GiB** (auto-retire) |
| **Client memory** | ~2.5 GiB (index in client) | **<50 MB** (thin client) |
| **Query latency** | ~100–200ms (SIMD linear scan) | **<10ms** (trigram index) |
| **Install step** | Service installer | None (auto-start) |
| **UAC prompts** | Once (service install) | Once per daemon lifecycle |
| **Multi-client** | GUI only | CLI + TUI + GUI + MCP |
| **Remote access** | Everything SDK (TCP) | Phase 6: HTTP/SSE |
| **Extra features** | — | Treesize, descendants, 25 columns, MCP |

---

## Open Questions

1. **Compact index location**: Extract to `uffs-core` (shared crate) or keep
   in `uffs-daemon` (daemon-specific)? The TUI currently owns this code.
   Recommendation: `uffs-core` — it's a general-purpose search structure.

2. **IPC serialization format**: JSON-RPC (human-readable, easy debugging) vs
   MessagePack (2–3× smaller, faster parse) vs FlatBuffers (zero-copy)?
   Recommendation: Start with JSON-RPC (MCP compatibility, debuggability).
   Optimize to binary protocol only if profiling shows IPC is a bottleneck.

3. **Notification channel**: Should the daemon push async notifications
   (refresh complete, drive loaded) to connected clients? Yes — via JSON-RPC
   notifications (no `id` field). Clients that don't care can ignore them.

4. **Multiple daemon instances**: Should the daemon support multiple
   data directories simultaneously (e.g., work + personal)? Start with one
   global instance per user. Namespaced instances are a future feature.

5. **Warm restart optimization**: Currently the daemon rebuilds the compact
   index from `.uffs` on every start (~5s). A binary sidecar cache (`.uffs-compact`)
   could eliminate this, making warm restart <1s. Deferred — 5s is acceptable.

---

## Summary

```
BEFORE                              AFTER
──────                              ─────
CLI: 5-30s per run                  CLI: <1s (daemon warm)
TUI: 7 GiB in-process              TUI: <50 MB (thin client)
GUI: TBD                            GUI: <50 MB (thin client)
MCP: N/A                            MCP: <5 MB (stdio adapter)
                                    Daemon: ~7 GiB while active, 0 when idle
                                    Shared: all surfaces benefit from one warm index

Total peak (2 surfaces):            Total peak (4 surfaces):
  ~14 GiB (TUI + CLI)                ~7.3 GiB (daemon) + ~100 MB (clients)
```

The daemon is the search engine. The surfaces are just presentation layers.
One index, many views, zero wasted memory.

---

*Document Version: 1.0*  
*Last Updated: 2026-03-26*  
*Based on: Memory analysis from `LOG/2026_03_26_tui_memory_footprint_analysis.md`*
