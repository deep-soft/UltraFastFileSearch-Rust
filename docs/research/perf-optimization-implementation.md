# Performance Optimization — Implementation Tracker

Status: **In Progress** (OPT-1,2,3,4,6,7 complete) | Created: 2026-04-14

## Measured Results (7 drives, 25.95M records)

| Metric | Before all OPTs | After OPT-1,2,3,4 | After OPT-6,7 (current) |
|---|---|---|---|
| **Peak RSS** | ~23 GB | ~23 GB | **~10 GB** |
| **Settled RSS** | ~14 GB | ~12 GB | **~6 GB** |
| **Index heap** | ~4.8 GB | ~4.8 GB | **~3.5 GB** (est. with pruning) |

### Per-drive memory breakdown (from `uffs daemon status`)

```
Index heap:    4781 MB
  G: —     15,094 records (live) —    2 MB  [rec=1   names=0   tri=0   ch=0  ext=0]
  F: —  2,221,346 records (live) —  448 MB  [rec=169 names=57  tri=193 ch=16 ext=8]
  C: —  3,533,886 records (live) —  731 MB  [rec=269 names=94  tri=324 ch=26 ext=13]
  M: —  1,908,808 records (live) —  301 MB  [rec=145 names=29  tri=104 ch=14 ext=7]
  D: —  7,066,020 records (live) — 1459 MB  [rec=539 names=209 tri=629 ch=53 ext=26]
  E: —  2,929,522 records (live) —  474 MB  [rec=223 names=47  tri=168 ch=22 ext=11]
  S: —  8,278,105 records (live) — 1363 MB  [rec=631 names=140 tri=495 ch=63 ext=31]
```

### Memory composition (all drives, pre-pruning)

| Component | Total MB | % of heap | Notes |
|-----------|----------|-----------|-------|
| `records` | 1,977 | 41% | `Vec<CompactRecord>` — 26M × 80 bytes |
| `trigram` | 1,913 | 40% | Keys + offsets + posting lists |
| `names` | 576 | 12% | `Vec<u8>` — all filenames concatenated |
| `children` | 194 | 4% | Parent→children CSR index |
| `ext_index` | 96 | 2% | Extension→records CSR index |
| **Total** | **4,756** | **100%** | |

## Codebase Reality Check

Before planning optimizations, we audited the current code and found
several optimizations from the analysis doc were **already implemented**:

| Optimization | Status | Notes |
|---|---|---|
| Drop `names_lower` (fold at query time) | ✅ Already done | Uses `CaseFold` ($UpCase table), no `names_lower` field |
| CSR for `children` (eliminate per-record Vec) | ✅ Already done | `ChildrenIndex` with CSR layout |
| CSR for `TrigramIndex` | ✅ Already done | `keys: Vec<u64>`, `offsets: Vec<u32>`, `values: Vec<u32>` |
| `ExtensionIndex` (CSR) | ✅ Already done | O(K) `--ext` queries |
| `CompactRecord` is 80 bytes Pod | ✅ Already done | `bytemuck::Pod`, bulk memcpy serialization |

### Corrected memory estimate (D: drive, 7.07M records)

| Component | Formula | Size |
|-----------|---------|------|
| `records: Vec<CompactRecord>` | 80 B × 7.07M | 566 MB |
| `names: Vec<u8>` | ~23 B avg × 7.07M | 163 MB |
| `trigram: TrigramIndex` (CSR) | ~50K keys + ~70M values | ~280 MB |
| `children: ChildrenIndex` (CSR) | 7.07M offsets + ~6M values | ~52 MB |
| `ext_index: ExtensionIndex` (CSR) | offsets + ~7M values | ~28 MB |
| `fold: CaseFold` | static table | ~0 MB |
| **Steady state D:** | | **~1,089 MB ≈ 1.1 GB** |

For 7 drives (est. 25M total records): **~3.5–4 GB steady state**.
Observed before optimizations: **~16 GB** → gap of ~12 GB.

---

## Remaining Optimizations

### OPT-1: `shrink_to_fit()` all Vecs after compact index build

**Priority:** High | **Effort:** Trivial | **Est. savings:** ~500 MB

**Problem:** Vec uses a doubling growth strategy. After building the
compact index, Vecs may have 25-100% extra capacity. For 7.07M records:
- `records` capacity might be 8M → wastes 0.93M × 80 B = 74 MB
- `trigram.values` capacity might be 100M → wastes 30M × 4 B = 120 MB
- Similar waste across all Vecs

**Fix:** Add `shrink_to_fit()` calls at the end of `build_compact_index()`
in `crates/uffs-core/src/compact.rs` and in `TrigramIndex::build()`.

**Files to change:**
- `crates/uffs-core/src/compact.rs` — after building records, names, children
- `crates/uffs-core/src/trigram.rs` — after building keys, offsets, values
- `crates/uffs-core/src/compact.rs` — `ExtensionIndex::build()`

**Status:** [x] Complete — `shrink_to_fit()` added in
`shrink_compact_vecs()` (called at end of `build_compact_index()`)
with tracing of reclaimed bytes.  Also added `heap_size_bytes()`
methods on `DriveCompactIndex`, `ChildrenIndex`, `ExtensionIndex`,
and `TrigramIndex` for per-component memory reporting.

---

### OPT-2: Memory release after drive load (allocator purge)

**Priority:** High | **Effort:** Low | **Est. savings:** ~2-3 GB

**Problem:** After each drive load, `MftIndex` (~3 GB for D:) is dropped.
The system allocator may not return freed pages to the OS immediately,
leading to high RSS.

**Fix:** After each drive load completes (in the daemon's drive loading
loop), call allocator-specific memory release.

**Note:** Originally used platform-specific calls (`HeapCompact` on
Windows, `malloc_trim` on Linux). These were replaced by
`mi_collect(true)` in OPT-6 when mimalloc became the global allocator.

**Files changed:**
- `crates/uffs-daemon/src/index/mod.rs` — `release_allocator_pages()`

**Status:** [x] Complete — `release_allocator_pages()` calls
`mi_collect(true)` at 5 sites covering every memory-freeing moment:

| Call site | What's freed |
|-----------|-------------|
| After each MFT file load | MftIndex temp (~1.6 GB) |
| After each live drive load | MftIndex temp (~1.6 GB) |
| After all drives loaded | Final cleanup |
| After each drive refresh | Old drive index + MftIndex temp |
| After hot-load | MftIndex temp |

Also added memory visibility to daemon status:
- `daemon status` now shows per-drive heap breakdown:
  `D: — 7,066,020 records (live) — 1459 MB [rec=539 names=209 tri=629 ch=53 ext=26]`
- Total index heap shown: `Index heap: 4781 MB`
- `DriveCompactIndex::log_heap_report()` logs breakdown after each drive load

---

### OPT-3: Investigate background cache save memory

**Priority:** Medium | **Effort:** Investigation

**Problem:** `save_compact_cache_background()` serializes the index on
the calling thread into a `Vec<u8>` buffer, then spawns a background
thread for zstd compression + encryption + disk write. For D: drive:
- Serialized buffer: ~1.1 GB (uncompressed)
- Compressed buffer: ~200 MB (zstd)
- Encrypted buffer: ~200 MB

During save, both the serialized and compressed buffers exist
simultaneously → ~1.3 GB temporary allocation.

**Previous code path:**
```
serialize_compact(&index)     → Vec<u8> ~1.1 GB      ← eliminated
zstd::encode_all(...)          → Vec<u8> ~200 MB
encrypt_cache(...)             → Vec<u8> ~200 MB
atomic_write(...)              → disk
Peak: ~1.3 GB (serialized + compressed coexist)
```

**New streaming code path:**
```
serialize_compact_to_writer → zstd::Encoder<Vec<u8>>  (no intermediate buffer)
encoder.finish()             → Vec<u8> ~200 MB (compressed)
encrypt_cache(...)           → Vec<u8> ~200 MB
atomic_write(...)            → disk
Peak: ~400 MB (compressed + encrypted coexist)
```

**Status:** [x] Complete — streaming serialization eliminates 1.1 GB buffer:
- `serialize_compact_to_writer()`: writes same byte layout directly to
  any `impl Write`, zero intermediate allocation
- `save_compact_cache_background()`: calling thread does
  serialize → zstd compress (~200 MB output), background thread does
  AES-256-GCM encrypt → atomic disk write
- `save_compact_cache()` (blocking): uses
  `compress_encrypt_write_streaming()` with closure
- `compress_encrypt_write_streaming()` added to `uffs-mft/cache.rs`
- `new_zstd_mt_encoder()` extracts encoder creation for reuse
- Peak memory: ~400 MB (down from ~1.3 GB)

---

### OPT-4: Daemon writes `--out` file directly

**Priority:** High | **Effort:** Medium | **Est. savings:** eliminates IPC for bulk export

**Problem:** When user specifies `--out file.csv`, the current flow is:
1. Daemon searches → builds `Vec<SearchRow>` (16 MB for 166K rows)
2. Serializes to JSON/shmem → transfers via IPC (~32 MB)
3. Client deserializes → converts → writes to file

The daemon already has the data — the client just writes it to a file.

**Fix:** Add `output_file` parameter to the search RPC request. When
present, the daemon:
1. Searches as usual
2. Writes results directly to the specified file (CSV/JSON format)
3. Returns only metadata: `{ "rows_written": 166000, "duration_ms": 45 }`

Total IPC: ~200 bytes instead of ~32 MB.

**Status:** [x] Complete — atomic file output with full `OutputConfig`:
- `SearchParams.output_file` + output config fields added to protocol
  (`output_separator`, `output_quote`, `output_header`, `output_pos`,
  `output_neg`, `output_columns`)
- Daemon reconstructs `OutputConfig` from protocol fields and calls
  `OutputConfig::write_display_rows()` — identical output to CLI
- Writes to `.uffs.tmp` temp file → `sync_all()` → atomic `rename()`
- Zero rows → no file created/touched (target untouched)
- Write error → temp file cleaned up, falls back to normal IPC
- CLI resolves `--out` to absolute path, passes full output config
- Supports all output options: `--sep`, `--quotes`, `--header`,
  `--pos`, `--neg`, `--columns` (all 30+ column types including
  individual NTFS attribute flags)

---

### OPT-5: Thin CLI client (reduce binary load time)

**Priority:** Medium | **Effort:** Medium | **Est. savings:** 152 ms → ~15 ms process load

**Problem:** `uffs.exe` is 52.7 MB → 152 ms to load on Windows.
The daemon search itself is 0 ms. The process creation overhead
is the single largest contributor to wall clock latency.

**Measured binary size vs load time (Windows, 10-run avg):**

| Binary | Size | Load time |
|--------|------|-----------|
| find.exe (system) | 40 KB | 11.1 ms |
| es.exe (C) | 151 KB | 35.6 ms |
| uffs.com (C++) | 1.2 MB | 40.9 ms |
| uffs_mft.exe (Rust) | 20.5 MB | 77.5 ms |
| uffs.exe (Rust+Polars) | 52.7 MB | 152.5 ms |

Formula: **~12 ms floor + ~2.7 ms per MB**.

**Options (from smallest to largest):**

| Client | Language | Size | Load | Total |
|--------|----------|------|------|-------|
| C (no CRT, raw Win32) | C | ~5 KB | ~11 ms | ~14 ms |
| Rust (std-only, no deps) | Rust | ~500 KB | ~15 ms | ~18 ms |
| PowerShell function | PS | 0 KB | 0 ms | ~5 ms |

All options talk to the same daemon via the existing Unix socket /
named pipe. The thin client is just a pipe adapter — it sends
the query, streams the response to stdout or file.

**Architecture analysis:**

The current `uffs` CLI binary contains both thin (daemon-client) and
fat (direct MFT) code paths:

| Subcommand | Needs daemon? | Needs uffs-core/polars? |
|---|---|---|
| `uffs *.rs` (search) | ✅ | ❌ just sends protocol |
| `uffs aggregate` | ✅ | ❌ just sends protocol |
| `uffs daemon start/stop/status` | ❌ | ❌ |
| `uffs daemon run` | — | ✅ IS the daemon |
| `uffs mcp start/run` | ✅ | ❌ just proxies |
| `uffs stats` | ✅ | ❌ sends aggregate |
| `uffs status` | ✅ | ❌ |
| `uffs index` | ❌ | ✅ reads MFT, writes parquet |
| `uffs info` | ❌ | ✅ reads parquet |

The 152 ms startup comes from linking `uffs-core` → `uffs-polars` →
polars. But 90% of commands only need `uffs-client` (already thin).

**Key finding:** `uffs index` and `uffs info` are **duplicates** of
existing `uffs_mft` subcommands:
- `uffs index --drive C out.parquet` = `uffs_mft read -d C -o out.parquet`
- `uffs info out.parquet` = `uffs_mft load out.parquet --info-only`

The `uffs_mft` versions are more capable (support `--mode`, `--full`,
`--unique`, `--forensic`, `--build-index`, `--debug-tree`, etc.).

**Recommended approach (Option A — two binaries):**
```
uffs      → thin (~15 ms startup) — search, aggregate, daemon mgmt, mcp, stats
uffs-srv  → fat (current daemon) — runs as background service
```
- `uffs` links only: `clap`, `uffs-client`, `uffs-security`, `serde`, `tokio`
- `uffs-srv` keeps everything: `uffs-core`, `uffs-mft`, `uffs-polars`
- `uffs daemon start` spawns `uffs-srv` instead of `uffs daemon run`
- `uffs index` / `uffs info` removed (use `uffs_mft read` / `uffs_mft load`)
- User experience: **identical** — `uffs *.rs` still works, 10× faster

**Status:** ✅ Done — CLI is now a thin client, zero polars/uffs-core/uffs-mft in dep tree

---

### OPT-6: mimalloc as global allocator for daemon

**Priority:** High | **Effort:** Low | **Est. savings:** RSS→heap gap closed

**Problem:** After OPT-1 through OPT-4, index heap was 4.8 GB but
RSS settled at 12 GB — a 7.2 GB gap. The Windows CRT allocator was
holding freed pages from MftIndex temporaries (~1.6 GB per drive × 7)
and old serialization buffers.

`HeapCompact(GetProcessHeap())` had no effect because mimalloc was
already the global allocator via the CLI binary — the CRT heap was
unused.

**Fix:**
- Added `mimalloc` + `libmimalloc-sys` (with `extended` feature) to
  `uffs-daemon`
- Set `#[global_allocator] static GLOBAL: MiMalloc` in daemon's `main.rs`
- Replaced `HeapCompact` / `malloc_trim` with `mi_collect(true)` which
  aggressively decommits freed segments
- Added `mi_collect(true)` to all 5 memory-freeing sites (see OPT-2)

**Result:** RSS settled at ~6 GB (was 12 GB) — **6 GB recovered**.
Peak RSS dropped from ~23 GB to ~10 GB.

**Status:** [x] Complete

---

### OPT-7: Trigram frequency-cap pruning

**Priority:** Medium | **Effort:** Low | **Est. savings:** ~20-30% trigram RAM

**Problem:** The trigram index consumes 1,913 MB (40% of total heap).
The `values: Vec<u32>` posting lists are ~99% of that. Each record
contributes ~18 unique trigrams on average:

```
25.95M records × ~18 trigrams/record × 4 bytes = ~1,868 MB ≈ 1,913 MB
```

Many trigrams appear in millions of records — common patterns like
`exe`, `dll`, `win`, `the`, `ing`. These huge posting lists:
1. Waste memory (a trigram hitting 50% of records = 3.5M × 4 = 14 MB)
2. Are useless for filtering (low selectivity, expensive to intersect)

**Fix:** During the trigram build, prune any trigram whose document
frequency exceeds 25% of total records (`records.len() / 4`):

```rust
let freq_cap = (record_count / 4).max(1024);
global_counts.retain(|_tri, cnt| (*cnt as usize) <= freq_cap);
```

- Minimum cap of 1024 prevents over-pruning small indices (tests pass)
- Search side unaffected: `filter_map` in `search()` silently skips
  missing trigrams — fewer, more selective trigrams are used
- Searches actually get **faster** (shorter intersection chains)

**Status:** [x] Complete — implemented in `TrigramIndex::build()` in
`crates/uffs-core/src/trigram.rs`. Pruning count logged via tracing.

---

## Priority Matrix

| # | Optimization | Savings | Effort | Status |
|---|-------------|---------|--------|--------|
| OPT-1 | `shrink_to_fit()` + heap reporting | ~500 MB RAM | Trivial | ✅ Done |
| OPT-2 | Allocator purge + memory visibility | ~2-3 GB RAM | Low | ✅ Done |
| OPT-4 | Daemon writes `--out` (full OutputConfig) | eliminates IPC | Medium | ✅ Done |
| OPT-3 | Cache save streaming | ~1 GB peak RAM | Medium | ✅ Done |
| OPT-6 | mimalloc global allocator | RSS→heap gap | Low | ✅ Done |
| OPT-7 | Trigram frequency-cap pruning | ~20-30% tri RAM | Low | ✅ Done |
| OPT-5 | Thin CLI client | 152→15 ms load | Medium | ✅ Done |

## Implementation Order

1. ~~**OPT-1 + OPT-2** — trivial memory wins, no behavior change~~ ✅ Done
2. **Measure round 1** — after OPT-1,2,3,4
   - All 7 drives: index heap 4,781 MB, RSS settled at ~12 GB, peak ~23 GB
   - Gap: ~7.2 GB from allocator not returning freed pages
3. ~~**OPT-4** — daemon-writes-file for `--out`~~ ✅ Done
4. ~~**OPT-3** — streaming cache save (eliminate 1.1 GB buffer)~~ ✅ Done
5. ~~**OPT-6** — mimalloc as `#[global_allocator]` for daemon~~ ✅ Done
6. ~~**OPT-7** — trigram frequency-cap pruning~~ ✅ Done
7. **Measure round 2** — after OPT-6,7
   - **Peak RSS: ~10 GB** (was ~23 GB — **-57%**)
   - **Settled RSS: ~6 GB** (was ~14 GB — **-57%**)
   - mimalloc + `mi_collect(true)` closed the RSS→heap gap
   - Trigram pruning reduced posting list memory
8. ~~**OPT-5** — thin client~~ ✅ Done
   - `uffs index` / `uffs info` removed from CLI (duplicates of `uffs_mft`)
   - `uffs-core`, `uffs-mft`, `uffs-polars` removed from `uffs-cli` deps
   - `uffs-core` also removed from `uffs-mcp` deps
   - `FieldId` types moved to `uffs-client::schema` (shared without polars)
   - Daemon binary renamed to `uffsd` (`[[bin]]` in `uffs-daemon`)
   - Format helpers moved to `uffs-client::format`
   - Expected: 52.7 MB → ~2 MB binary, 152 ms → ~15 ms startup

## Non-goals (preserve current functionality)

- Do NOT remove tree metrics (treesize, descendants, tree_allocated)
- Do NOT remove trigram index (enables fast substring search)
- Do NOT remove shmem transport (handles > 100K row stdout well)
- Do NOT change the daemon's search API (additive changes only)
- Do NOT change the `.uffs` cache format (backward compatible)
