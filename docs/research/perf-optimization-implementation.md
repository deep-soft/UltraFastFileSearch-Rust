# Performance Optimization — Implementation Tracker

Status: **In Progress** | Created: 2026-04-14

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

**Status:** [ ] Not started

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

**Status:** [ ] Not started

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

**Current code path:**
```
compact_cache.rs:228  serialize_compact(&index)     → Vec<u8> ~1.1 GB
compact_cache.rs:236  zstd::encode_all(...)          → Vec<u8> ~200 MB
                      (serialized dropped here)
compact_cache.rs:242  encrypt_cache(...)             → Vec<u8> ~200 MB
                      (compressed dropped here)
compact_cache.rs:245  atomic_write(...)              → disk
```

**Possible fixes:**
- Stream serialization → zstd → encrypt → disk (no full buffer)
- Serialize directly to a zstd encoder (pipe, no intermediate Vec)
- Delay cache save until after all drives are loaded

**Files to investigate:**
- `crates/uffs-core/src/compact_cache.rs` — `serialize_compact()` + `save_compact_cache()`
- `crates/uffs-core/src/compact_loader.rs:129` — where bg save is triggered

**Status:** [ ] Not started

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

| # | Optimization | Savings | Effort | Priority |
|---|-------------|---------|--------|----------|
| OPT-1 | `shrink_to_fit()` | ~500 MB RAM | Trivial | 🔴 Do first |
| OPT-2 | Allocator purge | ~2-3 GB RAM | Low | 🔴 Do first |
| OPT-4 | Daemon writes `--out` | eliminates IPC | Medium | 🟠 Do second |
| OPT-3 | Cache save streaming | ~1 GB peak RAM | Investigation | 🟡 Investigate |
| OPT-5 | Thin CLI client | 152→15 ms load | Medium | 🟡 After validation |

## Implementation Order

1. **OPT-1 + OPT-2** — trivial memory wins, no behavior change
2. **Measure** — confirm memory reduction on Windows with 7 drives
3. **OPT-4** — daemon-writes-file for `--out`
4. **OPT-3** — investigate cache save memory, fix if worthwhile
5. **OPT-5** — thin client (after architecture is validated)

## Non-goals (preserve current functionality)

- Do NOT remove tree metrics (treesize, descendants, tree_allocated)
- Do NOT remove trigram index (enables fast substring search)
- Do NOT remove shmem transport (handles > 100K row stdout well)
- Do NOT change the daemon's search API (additive changes only)
- Do NOT change the `.uffs` cache format (backward compatible)
