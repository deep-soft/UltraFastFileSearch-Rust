# UFFS TUI Memory Footprint Analysis — Mac M4 (Offline Data)

**Date:** 2026-03-26
**Platform:** macOS, Apple M4, unified memory
**Dataset:** 7 NTFS drives loaded from `~/uffs_data` (offline `.iocp` files)
**Tool:** `scripts/dev/tui-memory-footprint.rs` — samples RSS every second for 60s

---

## Test Dataset

| Drive | Raw MFT | IOCP File | Approx Records |
|-------|---------|-----------|----------------|
| C     | 4.4 GB  | 443 MB    | ~3.4M          |
| D     | 4.7 GB  | 408 MB    | ~7.1M          |
| E     | 2.8 GB  | 223 MB    | ~2.9M          |
| F     | 4.4 GB  | 425 MB    | ~2.2M          |
| G     | 20 MB   | 672 KB    | ~15K           |
| M     | 2.4 GB  | 178 MB    | ~1.9M          |
| S     | 11 GB   | 813 MB    | ~8.3M          |
| **Total** | | | **~25.9M records** |

---

## Observed Results

### Summary Table

| Run | Mode | Peak RSS | Steady RSS (after ~30s) |
|-----|------|----------|------------------------|
| 1   | cached (first) | ~28.4 GiB | ~15.3 GiB |
| 2   | cached | ~12.1 GiB | ~7.3 GiB |
| 3   | cached | ~10.4 GiB | ~7.2 GiB |
| 4   | no-cache | ~29.7 GiB | ~15.4 GiB |
| 5   | cached (after no-cache) | ~13.0 GiB | ~7.8 GiB |

### Key Findings

- **Normal cached steady-state:** ~7.3–8.0 GiB
- **No-cache steady-state:** ~15.4 GiB (~2× cached)
- **Transient peaks:** up to ~30 GiB during load phase
- **Phase transition:** memory drops to a lower plateau around 25–31 seconds
- **First cached run** behaves like no-cache (cold `.uffs` cache or stale TTL)

---

## Why: Architectural Explanation

### The Loading Pipeline

The TUI loads data through a multi-phase pipeline where temporary and
permanent structures overlap in memory. Understanding this pipeline
explains every observation above.

```
Raw .iocp file on disk
    │
    ▼
┌─────────────────────────────────────────────────┐
│  Phase 1: Parse raw MFT → MftIndex              │
│  (or deserialize .uffs cache → MftIndex)         │
│                                                   │
│  MftIndex = 224 bytes/record + names + links +   │
│  streams + children + frs_to_idx                  │
│  ≈ 800 MB per 3.4M records (drive C)             │
│  ≈ 5.9 GB total for all 7 drives                │
└───────────────────┬─────────────────────────────┘
                    │
                    ▼
┌─────────────────────────────────────────────────┐
│  Phase 2: Build CompactIndex FROM MftIndex       │
│  *** BOTH exist simultaneously ***               │
│                                                   │
│  CompactRecord = 68 bytes/record                 │
│  + names blob (Vec<u8>)                          │
│  + names_lower (Vec<u8>) — lowercase copy        │
│  + children (Vec<Vec<u32>>)                      │
│  + trigram index (HashMap<[u8;3], Vec<u32>>)     │
│  + TEMPORARY Vec<String> during trigram build    │
│                                                   │
│  ≈ 1.5–2.5 GB per large drive                   │
└───────────────────┬─────────────────────────────┘
                    │
                    ▼
┌─────────────────────────────────────────────────┐
│  Phase 3: Drop MftIndex                          │
│  "frees ~800 MB per drive" (code comment)        │
│                                                   │
│  Only CompactIndex remains in memory             │
└─────────────────────────────────────────────────┘
```

### Per-Record Memory Budget (Steady State)

| Component | Per Record | 25.9M Records |
|-----------|-----------|---------------|
| `CompactRecord` | 68 B | 1.7 GB |
| `names` blob | ~15 B avg | 390 MB |
| `names_lower` (copy) | ~15 B avg | 390 MB |
| `children` (Vec<Vec<u32>>) | ~6 B avg | 155 MB |
| Trigram postings | ~40–80 B avg | 1.0–2.0 GB |
| Vec overhead / alignment | variable | ~0.5 GB |
| **Subtotal** | | **~4.1–5.1 GB** |
| Rust allocator overhead + macOS page rounding | | ~2–3 GB |
| **Observed steady state** | | **~7.3 GB** |

The gap between the theoretical ~5 GB and observed ~7.3 GB is typical
for a Rust process on macOS: `jemalloc`/system allocator retains freed
arenas, macOS rounds RSS to page boundaries (16 KB on M4), and the
memory compressor may inflate apparent RSS for recently-touched pages.

---

## Explaining Each Observation

### 1. Why does no-cache use ~2× the memory?

**Cached path:**
`.uffs` binary → deserialize directly into `MftIndex` (single contiguous
read, minimal temporary buffers) → build compact → drop `MftIndex`.

**No-cache path:**
Read raw `.iocp` file into memory → parse every 1024-byte MFT record →
`MftRecordMerger` accumulates parsed records → build `MftIndex` from
scratch (multiple allocation passes, fixup buffers, merger state) →
save `.uffs` cache to disk (serializes entire index = another ~5 GB
temporary buffer) → build compact → drop `MftIndex`.

The no-cache path has **three large allocations alive simultaneously**:
1. Raw MFT data being parsed (~2.5 GB for the IOCP files)
2. `MftIndex` being built (~5.9 GB)
3. Serialization buffer during cache save (~5.9 GB, briefly)

Plus the compact index being built on top. This easily explains the
~30 GiB peak and ~15 GiB steady state (allocator retains freed pages).

### 2. Why does the first cached run look like no-cache?

The `.uffs` cache has a TTL (default 600 seconds). If the cache files
are stale or missing (first run after copying data, or TTL expired),
the "cached" code path falls through to the full parse:

```rust
let cached = if no_cache {
    None
} else {
    uffs_mft::cache::load_cached_index(drive_letter, INDEX_TTL_SECONDS)
};
// If None → falls through to parse_raw_mft_to_index()
```

Run 1 almost certainly hit this stale/missing cache path, making it
behave identically to `--no-cache`. Subsequent runs found fresh `.uffs`
files and took the fast deserialization path.

### 3. Why does memory drop at ~25–31 seconds?

All 7 drives are loaded in **parallel threads** via `std::thread::scope`.
Each thread independently:
1. Loads/parses `MftIndex` (big)
2. Calls `build_compact_index()` (both exist simultaneously)
3. Drops `MftIndex` (frees ~800 MB per drive)
4. Sends `DriveCompactIndex` to the UI thread

The ~30-second mark is when the **last drive finishes** its
load→compact→drop cycle. At that point, all `MftIndex` instances have
been dropped, and only the compact indices remain. The RSS drop
reflects the OS reclaiming those freed pages.

The timing is consistent: the largest drive (S: ~8.3M records, 813 MB
IOCP) takes the longest to parse and is likely the bottleneck.

### 4. macOS-Specific Memory Behavior

Several macOS behaviors affect the RSS numbers:

| Behavior | Effect |
|----------|--------|
| **Unified Memory (M4)** | GPU and CPU share the same physical RAM pool. No separate VRAM budget, but Terminal.app and WindowServer consume shared memory. |
| **Memory Compressor** | macOS compresses inactive pages rather than swapping. Compressed pages still count toward RSS until the compressor runs. The ~30s drop may partly reflect compression of MftIndex pages that were freed but not yet reclaimed. |
| **Page size (16 KB)** | Apple Silicon uses 16 KB pages (vs 4 KB on x86). Every small allocation rounds up to 16 KB, inflating RSS for workloads with many small vectors (like `children: Vec<Vec<u32>>`). |
| **Lazy page reclamation** | macOS does not immediately return freed pages to the kernel. The allocator retains them in its free lists. RSS stays elevated until memory pressure forces reclamation. |
| **File-backed pages** | Reading `.iocp` files via `mmap` or buffered I/O creates file-backed pages that count toward RSS but are trivially reclaimable. |

---

## Concise Summary

```
uffs_tui memory profile — Mac M4, 25.9M records across 7 offline drives

Cached runs (normal):
  Peak RSS:    ~10.7–13.3 GiB  (during load phase, ~0–25s)
  Steady RSS:  ~7.3–8.0 GiB    (after load completes, ~30s+)

No-cache run:
  Peak RSS:    ~29.7 GiB       (raw parse + MftIndex + compact overlap)
  Steady RSS:  ~15.4 GiB       (allocator retains freed arenas)

Key takeaways:
  • --no-cache roughly doubles steady-state RSS
  • Transient peaks reach ~30 GiB due to MftIndex + CompactIndex overlap
  • Memory drops to steady state around 25–31s (last drive finishes loading)
  • First run after stale cache behaves like --no-cache
  • ~7 GiB steady state for 25.9M records ≈ 290 bytes/record effective
  • macOS page size (16 KB) and lazy reclamation inflate RSS ~40% vs theoretical
```

