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

This eliminates the I/O asymmetry. **Re-run needed** to get fair numbers.

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

### 4.1  UFFS client startup cost (~100–120 ms)

| Phase | Est. cost | What it does |
|-------|-----------|-------------|
| Windows process creation | ~30–50 ms | Load 8 MB binary, relocations, DLL init |
| `#[tokio::main]` | ~15 ms | Tokio multi-thread runtime init |
| `init_logging()` | ~5–10 ms | Tracing subscriber + file appender |
| `Cli::parse()` (clap derive) | ~10 ms | 40+ flags, subcommands, validation |
| Static initializers | ~5–10 ms | Regex, OnceLock, lazy_static |
| **Subtotal** | **~70–100 ms** | **Before any search work begins** |

### 4.2  Everything client startup cost (~10–15 ms)

| Phase | Est. cost | What it does |
|-------|-----------|-------------|
| Windows process creation | ~10–15 ms | Load 200 KB binary, minimal C runtime |
| Arg parsing | ~0 ms | Hand-rolled, no framework |
| FindWindow | ~1 ms | Find Everything IPC window |
| **Subtotal** | **~12–16 ms** | |

### 4.3  IPC comparison

| Factor | Everything | UFFS |
|--------|-----------|------|
| Mechanism | `WM_COPYDATA` | JSON-RPC 2.0 over Unix socket |
| Serialization | None (raw memory) | serde_json (2 ser + 2 deser) |
| Data flow | es.exe → kernel copy → reply | params→JSON→socket→parse→search→SearchRow→JSON→socket→parse→DisplayRow |
| Conversions | 0 | 4 (DisplayRow→SearchRow→JSON string→SearchRow→DisplayRow) |
| Protocol overhead | 0 bytes | ~200 bytes envelope per request |

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

- [ ] Re-run benchmark with path-only output (fair comparison)
- [ ] Prototype `uffs-fast` thin client to validate Tier 1 hypothesis
- [ ] Profile startup with `cargo instruments` (or ETW on Windows)
- [ ] Implement lazy field resolution (Tier 3)
- [ ] Re-benchmark after each optimization tier
