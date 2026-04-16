# Thin Client Roadmap — `uffs.exe` → sub-200 KB

## Problem

`uffs.exe` is a "thin client" that sends JSON-RPC over a Unix domain socket
to the daemon and prints results.  Yet it started at **6.2 MB** and was
**2.5 MB** after extracting `uffs-mcp` into `uffsmcp`.  Everything's
`es.exe` does the same job in **155 KB**.

The bloat came from async infrastructure and frameworks the CLI doesn't need:

| Dependency          | Size (est.) | Why it was there                     | Needed? | Status   |
|---------------------|-------------|--------------------------------------|---------|----------|
| `tokio`             | ~800 KB     | `UffsClient` is async                | **No**  | Removed  |
| `clap` (derive)     | ~400 KB     | CLI arg parsing                      | **No**  | Removed  |
| `tracing-subscriber`| ~250 KB     | Structured logging to file           | **No**  | Removed  |
| `tracing-appender`  | ~100 KB     | Log file rotation                    | **No**  | Removed  |
| `serde_json`        | ~150 KB     | JSON-RPC serialization               | Yes     | Kept (Value only) |
| `serde` (derive)    | ~100 KB     | Struct (de)serialization             | **No**  | Removed (T7) |
| `indicatif`         | ~100 KB     | Progress bars                        | **No**  | Removed  |
| `mimalloc`          | ~100 KB     | Alternative allocator                | **No**  | Removed  |
| `which`             | ~30 KB      | Binary lookup for `uffsmcp`          | Can inline | Kept |
| `anyhow`            | ~30 KB      | Error handling                       | Can replace | Kept |
| Rust async glue     | ~200 KB     | Future combinators, waker, etc.      | **No**  | Removed  |

**Target: < 200 KB** — matching `es.exe` class.
**Current: 738 KB** — after T1–T7 completed.

## Architecture

```
Before:
  uffs.exe ──tokio──async──clap──► UffsClient (async) ──► Unix socket ──► uffsd

After (current):
  uffs.exe ──sync──► UffsClientSync (blocking) ──search_cli──► Unix socket ──► uffsd
                     │                                          │
                     │  CLI detects subcommand only              │  Daemon parses all
                     │  Forwards raw args via search_cli RPC     │  search flags + sugar
```

The IPC protocol is newline-delimited JSON-RPC over a Unix domain socket.
The blocking client: connect socket, write JSON + `\n`, read line, parse JSON.
No event loop, no futures, no waker.

For search, the CLI forwards raw `argv` to the daemon via the `search_cli`
RPC method.  The daemon parses flags into `SearchParams` (including all sugar
expansion: `--begins-with`, `--between`, `--exact-size`, etc.) and runs the
query.  The CLI never touches search-flag validation.

## Implementation Phases

### Phase T1: Sync IPC client in `uffs-client` — ✅ DONE

Added `UffsClientSync` alongside the existing async `UffsClient`.  The async
version stays for `uffsmcp` and `uffsd` which genuinely need async I/O.

Blocking `connect()` → `search()` → done.  Daemon auto-start uses
`std::process::Command` + `std::thread::sleep` retry loop.  Notifications
from the daemon are silently skipped when reading responses.

### Phase T2: Drop tokio from `uffs-cli` — ✅ DONE

Switched `main()` from `#[tokio::main] async fn main()` to plain
`fn main()`.  All async calls replaced with sync `UffsClientSync` methods.
MCP shim delegates to `uffsmcp` via sync `std::process::Command`.

### Phase T3: Drop tracing from `uffs-cli` — ✅ DONE

Removed `tracing`, `tracing-subscriber`, `tracing-appender`.  The CLI is a
short-lived process — `eprintln!` suffices for error/debug output.

### Phase T4: Drop mimalloc from `uffs-cli` — ✅ DONE

System allocator is fine for a process that allocates ~1 MB total.

### Phase T5: Drop clap — ✅ DONE

Dropped `clap` entirely.  Instead of parsing 60+ flags client-side into
typed fields (only to reassemble them into `SearchParams`), the CLI now:

1. Detects the subcommand from the first token (`stats`, `aggregate`,
   `daemon`, `mcp`, `status`, `help`, `version`)
2. For search (default): forwards **all raw args** to the daemon via the
   new `search_cli` JSON-RPC method
3. The daemon parses the raw args into `SearchParams` using
   `SearchParams::from_cli_args()` in `uffs-client/src/protocol/cli_args.rs`
4. All sugar expansion (`--begins-with`→pattern, `--between`→newer+older,
   `--exact-size`→min+max, `--count`/`--facet`→agg specs, etc.) happens
   daemon-side

The CLI's `main.rs` is ~150 lines.  `args.rs` is ~185 lines (just
`DaemonAction` enum, `parse_daemon_action`, help text, version).

Shell completions: static file shipped alongside the binary (clap's runtime
generation is gone).

### Phase T6: Drop indicatif — ✅ DONE

Progress bars removed (included in T2 batch).

### Phase T7: Drop serde derives from CLI hot path — ✅ DONE

Eliminated typed protocol structs (`SearchRow`, `SearchConfig`, etc.) from
the CLI binary's runtime path.  The CLI now works entirely with
`serde_json::Value` for search responses:

- Added `search_cli_raw()` and `status_raw()` to `UffsClientSync` —
  return raw `serde_json::Value` instead of typed structs
- Rewrote `output/mod.rs` to extract fields from `Value` via small
  helpers (`vs()`, `vu()`, `vi()`, `vb()`)
- Deleted the entire old typed search pipeline:
  - `daemon.rs` (450+ lines — `search_via_daemon`, `build_search_params`)
  - `SearchConfig` struct (130+ fields)
  - `build_search_config` (150+ lines)
  - `finalize_output` (60+ lines)
  - `search()` (50-param function, 70+ lines)
  - `util.rs` (`compute_output_targets`)
- Converted stats/aggregate subcommands to also use `search_cli` passthrough
  (synthesise equivalent raw args, forward to daemon)
- Aggregate output keeps typed `AggregateResultWire` via on-the-fly
  deserialization (`print_table_results_raw`/`print_csv_results_raw`) —
  only 1-10 items, negligible derive cost

**Result**: 840 KB → 738 KB (102 KB / 12% reduction).

`serde_json` itself (~80 KB) remains — it's the IPC transport. Dropping it
would require a custom JSON parser (diminishing returns).  The `serde`
derive codegen for `SearchRow` (15 fields × thousands of rows) was the
main cost, and that's now eliminated via `Value`.

## Tracking

| Phase | Description                    | Status      | Size impact          |
|-------|--------------------------------|-------------|----------------------|
| T0    | Extract uffs-mcp → uffsmcp     | ✅ DONE     | -3.7 MB (6.2→2.5)   |
| T1    | Sync IPC client                | ✅ DONE     | (foundation)         |
| T2    | Drop tokio from uffs-cli       | ✅ DONE     | -1.12 MB (2.5→1.38) |
| T3    | Drop tracing from uffs-cli     | ✅ DONE     | (included in T2)     |
| T4    | Drop mimalloc                  | ✅ DONE     | (included in T2)     |
| T5    | Drop clap + search_cli RPC     | ✅ DONE     | -540 KB (1.38→0.84)  |
| T6    | Drop indicatif                 | ✅ DONE     | (included in T2)     |
| T7    | Drop serde derives from CLI   | ✅ DONE     | -102 KB (0.84→0.74)  |
| —     | **Current size**               |             | **738 KB**           |

## Completed

| Phase | Description                    | Date       | Size impact          |
|-------|--------------------------------|------------|----------------------|
| T0    | Extract uffs-mcp → uffsmcp     | 2026-04-16 | -3.7 MB (6.2→2.5)   |
| T1    | Sync IPC client                | 2026-04-16 | (foundation for T2)  |
| T2    | Drop tokio from uffs-cli       | 2026-04-16 | -1.12 MB (2.5→1.38) |
| T3    | Drop tracing from uffs-cli     | 2026-04-16 | (included in T2)     |
| T4    | Drop mimalloc                  | 2026-04-16 | (included in T2)     |
| T5    | Drop clap + search_cli RPC     | 2026-04-16 | -540 KB (1.38→0.84) |
| T6    | Drop indicatif                 | 2026-04-16 | (included in T2)     |
| T7    | Drop serde derives from CLI   | 2026-04-16 | -102 KB (0.84→0.74)  |

## Size History

```
6.2 MB  — Starting point (uffs-cli + uffs-mcp combined)
2.5 MB  — After T0: extract uffs-mcp → uffsmcp
1.38 MB — After T2: drop tokio, tracing, mimalloc, indicatif
840 KB  — After T5: drop clap, add search_cli RPC passthrough
738 KB  — After T7: drop SearchRow/SearchConfig derives, delete old pipeline
```

## Notes

- **All phases complete.**  Total reduction: **6.2 MB → 738 KB (88% smaller)**.
- T5 used Option A (protocol change): the daemon now accepts raw CLI args
  via `search_cli` RPC and parses them server-side.  The CLI is a pure
  passthrough for search — it only detects subcommands locally.
- T7 kept `serde_json` (~80 KB) for IPC transport but eliminated all typed
  protocol struct derives from the CLI binary by using `serde_json::Value`.
- The async `UffsClient` stays in `uffs-client` for `uffsmcp`, `uffsd`, and
  any future async surface.  The sync client is additive, not a replacement.
- Windows named pipe support (future) would use `CreateFile` + `ReadFile` /
  `WriteFile` — naturally synchronous.
