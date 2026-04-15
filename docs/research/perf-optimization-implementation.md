# Performance Optimization — Implementation Tracker

Status: **In Progress** (OPT-1,2,3,4 complete) | Created: 2026-04-14

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
Observed: **~16 GB** → gap of ~12 GB.

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
loop), call platform-specific memory release:
- Linux/macOS: `libc::malloc_trim(0)` or `jemalloc::purge()`
- Windows: `HeapCompact(GetProcessHeap(), 0)` or `SetProcessWorkingSetSizeEx`
- Cross-platform: ensure no custom allocator is holding pages

Note: daemon does NOT use mimalloc (only the CLI does). The daemon
uses the system allocator.

**Files to change:**
- `crates/uffs-daemon/src/index/mod.rs` — after each drive result in
  the `join_set.join_next()` loop (around line 252)

**Status:** [x] Complete — `release_allocator_pages()` added in
`index/mod.rs` after each drive load and after all drives are loaded.
Platform-specific:
- Windows: `HeapCompact(GetProcessHeap(), HEAP_FLAGS(0))`
- Linux: `malloc_trim(0)`
- macOS: no-op (returns pages eagerly)

Also added memory visibility to daemon status:
- `daemon status` now shows per-drive heap breakdown:
  `D: — 7,066,020 records (live) — 1089 MB [rec=566 names=163 tri=280 ch=52 ext=28]`
- Total index heap shown: `Index heap: 1089 MB`
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

**Status:** [ ] Not started

---

## Priority Matrix

| # | Optimization | Savings | Effort | Status |
|---|-------------|---------|--------|--------|
| OPT-1 | `shrink_to_fit()` + heap reporting | ~500 MB RAM | Trivial | ✅ Done |
| OPT-2 | Allocator purge + memory visibility | ~2-3 GB RAM | Low | ✅ Done |
| OPT-4 | Daemon writes `--out` (full OutputConfig) | eliminates IPC | Medium | ✅ Done |
| OPT-3 | Cache save streaming | ~1 GB peak RAM | Medium | ✅ Done |
| OPT-5 | Thin CLI client | 152→15 ms load | Medium | 🟡 Not started |

## Implementation Order

1. ~~**OPT-1 + OPT-2** — trivial memory wins, no behavior change~~ ✅ Done
2. **Measure** — confirm memory reduction on Windows with 7 drives
   - D: alone: 8 GB RSS (was ~14 GB), index heap ~1.1 GB
   - Gap still large → allocator not returning pages (next: investigate)
3. ~~**OPT-4** — daemon-writes-file for `--out`~~ ✅ Done
4. ~~**OPT-3** — streaming cache save (eliminate 1.1 GB buffer)~~ ✅ Done
5. **OPT-5** — thin client (after architecture is validated)

## Non-goals (preserve current functionality)

- Do NOT remove tree metrics (treesize, descendants, tree_allocated)
- Do NOT remove trigram index (enables fast substring search)
- Do NOT remove shmem transport (handles > 100K row stdout well)
- Do NOT change the daemon's search API (additive changes only)
- Do NOT change the `.uffs` cache format (backward compatible)
