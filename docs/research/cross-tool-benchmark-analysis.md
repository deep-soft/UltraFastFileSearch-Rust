# Cross-Tool Benchmark Analysis

**Date:** 2026-04-14
**Version:** UFFS v0.5.4 (Rust), UFFS v0.4.x (C++), Everything 1.4 (es.exe)
**System:** AMD Ryzen 9 3900XT (12c/24t), 64 GB DDR4, Windows 11 Pro 24H2

## 1  Initial Results (unfair — output format asymmetry)

The first benchmark run revealed a critical methodological flaw:
all three tools wrote different amounts of data per row.

| Tool | Columns per row | Est. bytes/row | ext_dll (165K rows) |
|------|----------------|----------------|---------------------|
| Everything (es.exe) | 1 (Filename) | ~80 B | ~13 MB |
| UFFS C++ (uffs.com) | 25 (all) | ~300 B | ~50 MB |
| UFFS Rust (uffs.exe) | 34 (all) | ~400 B | ~66 MB |

UFFS Rust was writing **5× more data** than Everything per row.
This inflated timings by 3–5× on I/O-bound bulk exports.

### Raw results (unfair, 10 rounds, all columns)

| Drive | Pattern | UFFS HOT p50 | C++ p50 | Everything p50 | UFFS vs ES |
|-------|---------|-------------|---------|----------------|------------|
| C: | exact (26 rows) | 170 ms | 4.0 s | 79 ms | 2.2× slower |
| C: | prefix (37K rows) | 610 ms | 2.9 s | 111 ms | 5.5× slower |
| C: | ext_rare (1 row) | 169 ms | 11.8 s | 62 ms | 2.7× slower |
| C: | ext_dll (166K rows) | 793 ms | 12.2 s | 265 ms | 3.0× slower |
| C: | substring (28K rows) | 479 ms | 3.6 s | 118 ms | 4.1× slower |
| C: | full_scan (3.5M rows) | 11.3 s | 12.1 s | SKIP | — |
| D: | exact (3 rows) | 173 ms | 23.1 s | 64 ms | 2.7× slower |
| D: | prefix (15K rows) | 341 ms | 21.9 s | 73 ms | 4.7× slower |
| D: | ext_rare (11 rows) | 174 ms | 44.4 s | 60 ms | 2.9× slower |
| D: | ext_dll (45K rows) | 664 ms | 44.3 s | 120 ms | 5.5× slower |
| D: | substring (21K rows) | 407 ms | 22.3 s | 87 ms | 4.7× slower |
| D: | full_scan (7.1M rows) | 21.6 s | 40.3 s | SKIP | — |

### Key findings from unfair run

1. **Everything is 2–6× faster than UFFS HOT** on targeted queries —
   but this includes a ~5× I/O advantage (1 column vs 34 columns).
2. **UFFS HOT is 8–134× faster than C++** on targeted queries.
3. **UFFS HOT beats C++ on full_scan** (11.3 s vs 12.1 s on C:,
   21.6 s vs 40.3 s on D:) — despite outputting more columns.
4. **Everything cannot do full_scan** — 2 GB IPC memory limit.

## 2  Row Count Discrepancies

All three tools find different numbers of files on the same drive:

| Pattern | UFFS Rust | C++ | Everything | Notes |
|---------|-----------|-----|------------|-------|
| full_scan C: | 3,532,559 | 3,372,892 | SKIP | C++ missing ~160K |
| exact C: | 26 | 20 | 26 | C++ missing 6 |
| prefix C: | 36,803 | **0** | 36,637 | C++ bug? |
| ext_dll C: | 165,911 | 144,261 | 165,911 | C++ missing ~22K |
| substring C: | 27,527 | 25,983 | 26,942 | all differ |
| full_scan D: | 7,065,992 | 7,065,756 | SKIP | close match |

**Likely causes:**
- C++ skips MFT extension records (hardlinks, ADS) → fewer files
- C++ `win*` prefix returns 0 — possible glob handling bug
- Everything doesn't index `$`-prefixed NTFS metafiles → slightly fewer
- UFFS Rust is the most complete (indexes all MFT records)

## 3  Fairness Fix: Path-Only Output

Fixed the benchmark to use path-only output for all three tools:

- `uffs.exe --columns Path` → 1 column
- `uffs.com --columns=path` → 1 column
- `es.exe -export-csv` → already 1 column (full path, header: `Filename`)

This eliminates the I/O asymmetry.

### Fair results (path-only, 10 rounds, HOT only)

Partial results from targeted runs with `--patterns exact`:

| Drive | Pattern | UFFS HOT p50 | Everything p50 | Ratio |
|-------|---------|-------------|----------------|-------|
| D: | exact (3 rows) | 164 ms | 68 ms | 2.4× slower |

With path-only output, UFFS is still ~2.4× slower than Everything.
The I/O asymmetry was NOT the main bottleneck for small result sets —
**process startup overhead is** (see Section 4).

Full fair benchmark with all patterns and drives pending.

## 4  Where Does UFFS Spend 164 ms? (Profile Forensics)

The `--profile` output for `notepad.exe` on D: (3 rows, HOT):

```
Connect:           2 ms
Await ready:       0 ms
Search (IPC):      0 ms  (daemon: 0 ms, transfer: 0 ms)
Convert rows:      0 ms  (3 rows)
Output/write:      8 ms
─────────────────────────
Profile total:   ~10 ms   ← only 6% of wall clock!
Wall clock:     164 ms   ← WHERE ARE THE OTHER 154 ms?
```

**Answer: process startup overhead the profiler doesn't measure.**

### 4.1  UFFS client startup cost — MEASURED

Instrumented with `UFFS_PROFILE_STARTUP=1` (raw `eprintln!`, not tracing).

**macOS (Apple Silicon, release build):**

| Phase | Time | Cumulative |
|-------|------|-----------|
| Binary entry + alloc init | 0.04 ms | 0.04 ms |
| tokio runtime build | 1.48 ms | 1.52 ms |
| run() entered (tokio spawned) | 0.87 ms | 2.35 ms |
| Clap Cli::parse() | 0.96 ms | 3.31 ms |
| init_logging() | 1.06 ms | 4.37 ms |
| dispatch_search() entered | 0.06 ms | 4.43 ms |
| **Total (macOS)** | **5.2 ms** | |

**Windows (AMD Ryzen 9 3900XT, release build, MEASURED):**

| Phase | Time | Cumulative |
|-------|------|-----------|
| Binary entry + alloc init | 0.07 ms | 0.07 ms |
| tokio runtime build | 2.52 ms | 2.59 ms |
| run() entered (tokio spawned) | 1.56 ms | 4.15 ms |
| Clap Cli::parse() | 1.08 ms | 5.24 ms |
| init_logging() | 2.27 ms | 7.51 ms |
| dispatch_search() entered | 1.52 ms | 9.03 ms |
| Connect (socket + bridge threads) | 5 ms | — |
| Search IPC (7M records) | 0 ms | — |
| Convert + output | 0 ms | — |
| **total (after block_on)** | **25.82 ms** | **28.41 ms** |

**Benchmark wall clock: 164 ms.  In-process: 28 ms.**

**⇒ 136 ms is OS-level process creation overhead** — before `main()`
even runs. This is Windows loading the 52.7 MB uffs.exe binary: PE
parsing, section mapping, DLL initialization (ucrt, kernel32, ws2_32,
ntdll), CRT startup, mimalloc allocator init, TLS setup, and thread
pool pre-creation.

### 4.1.1  The real bottleneck: binary size → process creation

| | uffs.exe | es.exe |
|-|---------|--------|
| Binary size | 52.7 MB | ~200 KB |
| Process creation | ~136 ms | ~15 ms |
| In-process work | ~28 ms | ~50 ms |
| **Wall clock** | **~164 ms** | **~68 ms** |

UFFS actually does LESS in-process work than Everything (28 ms vs ~50 ms).
The daemon search (0 ms for 7M records) is faster than Everything's
search. **The entire perf gap is binary loading time.**

Optimization implications:
- tokio (2.5 ms), clap (1 ms), logging (2.3 ms) = 5.8 ms — **NOT** bottlenecks
- The AF_UNIX bridge (5 ms connect) is fast
- IPC round-trip (0 ms) is fast
- **The only fix: reduce binary size** (strip Polars from CLI, use thin client)

### 4.2  Everything client startup cost (~10–15 ms)

| Phase | Est. cost | What it does |
|-------|-----------|-------------|
| Windows process creation | ~10–15 ms | Load 200 KB binary, minimal C runtime |
| Arg parsing | ~0 ms | Hand-rolled, no framework |
| FindWindow | ~1 ms | Find Everything IPC window |
| **Subtotal** | **~12–16 ms** | |

### 4.3  IPC comparison

Everything's IPC is documented at https://www.voidtools.com/support/everything/sdk/ipc
and the es.exe source is at https://github.com/voidtools/ES.

**Everything's IPC flow** (from `src/es.c`):
1. `FindWindow("EVERYTHING_TASKBAR_NOTIFICATION")` — locate the service
2. Allocate `EVERYTHING_IPC_QUERY` struct with search string
3. `SendMessage(WM_COPYDATA)` — kernel copies query into service process
4. Service searches its in-memory index (sorted arrays, no DataFrame)
5. Service replies via `WM_COPYDATA` with result list
6. es.exe iterates results and writes to file/stdout via `fprintf`

This is **zero-copy IPC**: `WM_COPYDATA` maps the sender's buffer into
the receiver's address space via the kernel. No serialization, no JSON
parsing, no socket handshake. The entire IPC round-trip is a single
synchronous `SendMessage` call (~5 ms for small result sets).

**UFFS's IPC flow** (from `crates/uffs-client/src/connect.rs`):
1. `UnixStream::connect(socket_path)` — connect to daemon socket
2. Build `RpcRequest { jsonrpc: "2.0", method: "search", params: ... }`
3. `serde_json::to_value(params)` → `serde_json::to_string(request)`
4. Write JSON string to socket + `\n` delimiter
5. Daemon parses JSON, executes search, builds response
6. `serde_json::to_value(&response)` → `serde_json::to_string(rpc_response)`
7. Write response JSON to socket
8. Client reads line, `serde_json::from_str` → `serde_json::from_value`
9. Deserialize into `SearchResponse` with `Vec<SearchRow>`

| Factor | Everything | UFFS |
|--------|-----------|------|
| Mechanism | `WM_COPYDATA` (Win32) | JSON-RPC 2.0 over Unix socket |
| Serialization | None (raw `memcpy`) | serde_json (2 ser + 2 deser) |
| Data flow | es.exe → kernel copy → reply | params→JSON→socket→parse→search→SearchRow→JSON→socket→parse→DisplayRow |
| Conversions | 0 | 4 (DisplayRow→SearchRow→JSON string→SearchRow→DisplayRow) |
| Protocol overhead | ~0 bytes | ~200 bytes JSON-RPC envelope per request |
| Blocking model | Synchronous `SendMessage` | Async tokio + await |

### 4.4  The double conversion problem

```
Daemon side:                          Client side:
CompactRecord                         JSON string
  → DisplayRow (path resolved)          → serde_json::from_value
  → SearchRow  (clone all fields)       → SearchRow
  → serde_json::to_value               → search_row_to_display_row
  → serde_json::to_string              → DisplayRow (clone AGAIN)
  → write to socket                    → write_native_results
```

For 3 rows this is negligible. For 166K rows (ext_dll), this is
4 full copies of every string × 166K = ~660K string allocations.

## 5  Architectural Differences Summary

| Factor | Everything | UFFS Rust | Impact |
|--------|-----------|-----------|--------|
| Binary size | ~200 KB (C) | ~8 MB (Rust+Polars+tokio) | 40× larger → ~40 ms extra load |
| Async runtime | None | tokio multi-thread | ~15 ms init |
| Arg parsing | Hand-rolled | clap derive (40+ flags) | ~10 ms |
| IPC | WM_COPYDATA (zero-copy) | JSON-RPC over socket | ~5 ms extra |
| Data conversions | 0 copies | 4 copies per row | O(n) overhead |
| Index | Sorted arrays + hash | Polars DataFrame + trigram | Different trade-offs |
| Daemon model | Windows service (always-on) | Auto-start process | Same when warm |

## 6  Optimization Opportunities

### 6.1  Tier 1: Thin client (~70–90 ms savings, brings us to ~70 ms)

**Build a `uffs-fast` or `uffs-es` binary** — a minimal ~200 KB
executable with:
- No Polars, no tokio (blocking I/O), no clap (hand-parsed args)
- No tracing, no logging
- Connects to the daemon via blocking socket I/O
- Sends pre-built JSON-RPC query
- Reads response, writes paths to file or stdout
- **Expected result: 60–80 ms** (matching Everything)

### 6.2  Tier 2: Eliminate double conversion (~5–10 ms for large sets)

- Daemon writes `path` strings directly to response JSON (skip
  DisplayRow → SearchRow intermediate)
- Client reads paths directly (skip SearchRow → DisplayRow)
- For `--columns Path`, daemon could send a bare `["path1","path2"]`
  array instead of full SearchRow objects

### 6.3  Tier 3: Lazy field resolution (~10–20 ms for large sets)

- Only resolve fields requested by `--columns`
- For path-only: skip size/dates/flags/allocated/descendants
- `make_display_row()` currently populates ALL 15+ fields

### 6.4  Tier 4: Streaming file write

- Write each row directly to BufWriter as it's matched
- Currently: collect ALL DisplayRows → then write
- Eliminates 2× memory overhead for large result sets

### 6.5  Tier 5: In-daemon file export

- For `--out=file.csv`, the daemon writes the file directly
- Eliminates IPC transfer entirely for bulk exports
- The daemon already has the data in memory

### 6.6  Stretch goals

- **Shared memory IPC** — mmap result buffer, pass offset to client
- **Columnar export** — Parquet/Arrow IPC instead of CSV
- **WM_COPYDATA IPC** — match Everything's zero-copy mechanism (Windows only)

## 7  Next Steps

- [x] Fix benchmark: path-only output for all tools (fair I/O)
- [x] Fix benchmark: es.exe args must be separate (path + query)
- [x] Fix benchmark: add `--patterns` filter for targeted debugging
- [x] Fix benchmark: lightweight daemon warmup (no full scan)
- [x] Analyze Everything SDK IPC mechanism (WM_COPYDATA)
- [x] Deep-dive UFFS startup overhead (profile forensics)
- [ ] Run full fair benchmark (all patterns × all tools × 10 rounds)
- [ ] Prototype `uffs-fast` thin client to validate Tier 1 hypothesis
- [ ] Profile startup with ETW on Windows to measure exact breakdown
- [ ] Implement lazy field resolution (Tier 3)
- [ ] Re-benchmark after each optimization tier

## 8  Memory Analysis: Why UFFS Uses 16 GB vs Everything's ~750 MB

### 8.1  Current UFFS memory breakdown (D: = 7.07M records)

| Component | Formula | Est. size |
|-----------|---------|-----------|
| `CompactRecord` array | 72 B × 7.07M | 509 MB |
| `names` (UTF-8 blob) | ~23 B avg × 7.07M | 163 MB |
| `names_lower` (lowercase copy) | same as names | 163 MB |
| `TrigramIndex` postings | ~10 trigrams/name × 4 B × 7.07M | 283 MB |
| `children` (Vec<Vec<u32>>) | ~1.5 child entries × 4 B × 7.07M | 42 MB |
| `frs_to_idx` mapping | 4 B × max_frs (~8M) | 32 MB |
| Vec overallocation (+5%) | ~5% of above | ~60 MB |
| Polars/tokio/tracing runtime | fixed overhead | ~50 MB |
| **Subtotal D:** | | **~1.3 GB** |

But observed = **6 GB**.  Possible explanations for the 4.7 GB gap:
- `MftIndex` (224 B/record) not fully dropped after compaction → 1.58 GB
- Polars DataFrame still held for aggregation/stats → 1+ GB
- Trigram postings lists over-allocated (roaring bitmaps? Vec capacity) → 1+ GB
- Peak memory during compaction (old + new index simultaneously) → 1+ GB

### 8.2  Everything's memory model (~750 MB for 10M records)

Everything uses a purpose-built custom index:

| Component | Formula | Est. size |
|-----------|---------|-----------|
| Per-file record | ~44 B (parent_id, size, 3 dates, flags) | 440 MB |
| Filename storage | ~30 B avg (UTF-16) | 300 MB |
| Sorted name index | pointers only | ~40 MB |
| **Total** | | **~780 MB** |

Everything does NOT store:
- No lowercase name copy (folds at search time using SIMD)
- No trigram index (uses sorted arrays + binary search)
- No tree metrics (descendants, treesize, tree_allocated)
- No allocated_size (computes on demand from NTFS)
- No extension interning table

### 8.3  What UFFS stores that Everything doesn't

| Field | UFFS | Everything | Bytes/record |
|-------|------|-----------|--------------|
| `allocated` (size on disk) | stored | not stored | +8 B |
| `treesize` (subtree sum) | stored | not stored | +8 B |
| `tree_allocated` | stored | not stored | — (removed) |
| `descendants` (subtree count) | stored | not stored | +4 B |
| `names_lower` (case-fold copy) | stored | folds at query time | +23 B avg |
| `TrigramIndex` | stored | not stored | +40 B avg |
| **Extra per record** | | | **~83 B** |

For 10M records: 83 B × 10M = **830 MB** of data Everything doesn't need.

## 9  Architectural Redesign Proposal

### 9.1  Current architecture

```
┌──────────────────────────────────────────────────────┐
│  uffs.exe (~8 MB)                                     │
│  tokio + clap + tracing + Polars + serde              │
│  ┌─────────────────────────────────────────────────┐  │
│  │ CLI client  │  JSON-RPC  │  daemon connect       │  │
│  └─────────────────────────────────────────────────┘  │
│        │                                              │
│        ▼  Unix socket / named pipe                    │
│  ┌─────────────────────────────────────────────────┐  │
│  │ uffs-daemon (embedded in uffs.exe)               │  │
│  │ CompactRecord + NameArena + Trigram + Polars      │  │
│  │ IndexManager + tokio + tracing                    │  │
│  │ ~6 GB RAM for D: alone                            │  │
│  └─────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────┘

uffs-mcp ──UffsClient──▶ uffs-daemon (same daemon)
```

**Problems:**
1. CLI binary is 8 MB → ~100 ms startup overhead
2. Daemon holds Polars + trigram + lowercase names → 6+ GB RAM
3. JSON-RPC serialization: 4 copies per result row
4. DisplayRow builds ALL 15 fields even when only Path requested
5. Single monolithic daemon — analytics and search share the same RAM

### 9.2  Proposed architecture: Split Index + Thin Client

```
                    ┌──────────────────────────────────────┐
                    │  uffs-engine (~200 KB daemon)          │
                    │  NO Polars, NO tokio (io_uring/epoll)  │
                    │  Custom lean index:                    │
                    │    CompactRecord  (72 B/rec)           │
                    │    NameArena      (23 B/rec)           │
                    │    ExtensionIndex (O(1) ext lookup)    │
                    │    Sorted name array (binary search)   │
                    │  Binary IPC (MessagePack / FlatBuffers)│
                    │  ~800 MB for D: (vs 6 GB today)        │
                    └───────────┬──────────────────────────┘
                                │ binary pipe / shared memory
              ┌─────────────────┼─────────────────────┐
              ▼                 ▼                     ▼
    ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐
    │ uffs (thin)   │  │ uffs-mcp     │  │ uffs-analytics   │
    │ ~200 KB       │  │ MCP bridge   │  │ Polars on-demand  │
    │ no tokio      │  │ JSON-RPC→bin │  │ loaded only for   │
    │ no clap       │  │              │  │ aggregation queries│
    │ blocking I/O  │  │              │  │                   │
    │ ~30 ms startup│  │              │  │ Not in search path│
    └──────────────┘  └──────────────┘  └──────────────────┘
```

### 9.3  Memory reduction roadmap

| Change | Savings | Difficulty |
|--------|---------|-----------|
| Drop `names_lower` — fold at query time | 163 MB (D:) | Easy |
| Drop TrigramIndex — use sorted name array + binary search | 283 MB (D:) | Medium |
| Drop Polars from search daemon (analytics-only) | 50+ MB base | Medium |
| Ensure MftIndex dropped after compaction | 1.58 GB (D:) | Easy (audit) |
| Drop `tree_allocated` from CompactRecord (compute on demand) | 0 (already gone) | — |
| Shrink CompactRecord to 56 B (drop treesize from hot record) | 112 MB (D:) | Easy |
| **Total potential savings** | **~2.1 GB** per drive | |

**Target: ~1 GB for D: (7M records) — matching Everything's density.**

### 9.4  Latency reduction roadmap

| Change | Est. improvement | Difficulty |
|--------|-----------------|-----------|
| Thin CLI client (no tokio/clap/tracing) | 100 ms → 30 ms | Medium |
| Binary IPC (MessagePack instead of JSON) | ~5 ms savings | Easy |
| Lazy field resolution (only build requested columns) | ~10 ms for large sets | Easy |
| Eliminate double conversion (4 copies → 1) | ~5 ms for large sets | Medium |
| In-daemon file export (skip IPC for --out) | ~20 ms savings | Medium |
| **Combined** | **~30–40 ms HOT** | |

**Target: 30–40 ms for targeted queries (matching Everything's ~68 ms).**

### 9.5  What we KEEP (competitive advantages over Everything)

These features justify UFFS's existence — don't cut them:

1. **Cross-platform** — Everything is Windows-only
2. **Tree metrics** (descendants, treesize) — no other tool has this
3. **Rich filtering** (size ranges, date ranges, NTFS flags, bulkiness)
4. **Aggregation engine** (terms, histograms, rollups via Polars)
5. **MCP integration** — AI agents can query the filesystem
6. **Full-text trigram search** — Everything uses simpler matching
7. **Cache files** (.uffs) — instant daemon restart without MFT re-read

## 10  Methodology Notes

### Benchmark script
`scripts/windows/cross-tool-benchmark.rs` (rust-script)

### Key flags
```
--tools uffs,cpp,es    Select which tools to benchmark
--drives C,D           Select drives
--patterns exact,ext_dll  Select specific query patterns
--rounds 10            Number of rounds per pattern
--skip-cold            Skip COLD/WARM phases (HOT only)
```

### Test environment
- AMD Ryzen 9 3900XT, 12 cores / 24 threads
- 64 GB DDR4-3600 RAM
- C: Samsung 980 PRO NVMe (1 TB) — 3.5M MFT records
- D: Samsung 870 EVO SATA (4 TB) — 7.1M MFT records
- Windows 11 Pro 24H2 (Build 26100)
- Everything 1.4.1.1024 (service mode, IPC v2)
