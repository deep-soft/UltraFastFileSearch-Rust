# 15 — Performance Deep-Dive: Full Pipeline Bottleneck Analysis & Implementation Plan

> **Date:** 2026-04-02
> **Scope:** Independent, code-driven audit of the entire UFFS pipeline — from raw
> MFT ingestion through compact index build, cache I/O, search/filter, path
> resolution, sort, and output.  Every hot path inspected for algorithmic waste,
> wrong data structures, unnecessary allocations, and cache pressure.
>
> **Reference volume:** 7 M MFT records, 10 K result set, typical NTFS C: drive.
>
> **Relationship to doc 14:** Doc 14 identified 8 bottlenecks (B1–B8), several now
> fixed.  This audit is a fresh, zero-assumptions re-examination that validates
> prior fixes, discovers new issues, and provides detailed implementation
> blueprints with code-level specificity.

---

## Table of Contents

1. [Pipeline Architecture Map](#1-pipeline-architecture-map)
2. [Bottleneck Inventory](#2-bottleneck-inventory)
3. [Stage-by-Stage Analysis](#3-stage-by-stage-analysis)
   - [3.1 MFT Ingestion & Parsing](#31-mft-ingestion--parsing)
   - [3.2 Compact Index Build](#32-compact-index-build)
   - [3.3 Cache I/O (Serialize / Deserialize)](#33-cache-io)
   - [3.4 Search & Filter Pipeline](#34-search--filter-pipeline)
   - [3.5 Path Resolution](#35-path-resolution)
   - [3.6 Sort Pipeline](#36-sort-pipeline)
   - [3.7 Output Pipeline](#37-output-pipeline)
4. [Data Structure Audit](#4-data-structure-audit)
5. [Memory Profile & Peak Analysis](#5-memory-profile--peak-analysis)
6. [Prioritised Implementation Plan](#6-prioritised-implementation-plan)
7. [Validation Strategy](#7-validation-strategy)

---

## 1. Pipeline Architecture Map

```
  ┌─────────────────────────────────────────────────────────────────────────────┐
  │                        UFFS PROCESSING PIPELINE                            │
  │                                                                            │
  │  ┌──────────┐   ┌──────────┐   ┌──────────┐   ┌────────┐   ┌──────────┐  │
  │  │ INGEST   │──▶│ COMPACT  │──▶│ SEARCH   │──▶│ SORT   │──▶│ OUTPUT   │  │
  │  │          │   │ BUILD    │   │ & FILTER │   │        │   │          │  │
  │  └──────────┘   └──────────┘   └──────────┘   └────────┘   └──────────┘  │
  │   ✅ Optimal     🟡 2 issues    🔴 4 issues   🟡 2 issues  🟡 2 issues   │
  │                                                                            │
  │  ┌──────────────────────────────────────────────────────────────────────┐  │
  │  │ CACHE I/O  (parallel path — load from .uffs compact cache)         │  │
  │  │ 🟡 1 issue: 140 MB names_lower clone on every cache load           │  │
  │  └──────────────────────────────────────────────────────────────────────┘  │
  └─────────────────────────────────────────────────────────────────────────────┘
```

### Data Flow Detail

```
Raw NTFS MFT ──▶ parse_record_to_index() ──▶ MftIndex (240 B/rec, 1.6 GB)
                                                 │
                                    build_compact_index()
                                                 │
                                                 ▼
                                    DriveCompactIndex (80 B/rec, 560 MB)
                                    ├── records: Vec<CompactRecord>
                                    ├── names:   Vec<u8> (140 MB blob)
                                    ├── trigram:  TrigramIndex (CSR)
                                    └── children: ChildrenIndex (CSR)
                                                 │
                                    ┌────────────┴────────────┐
                                    ▼                         ▼
                            search_compact_drive()    collect_global_top_n()
                                    │                         │
                                    ▼                         ▼
                            indices_to_rows()         sort + truncate
                            (resolve_path_cached)     (path resolution)
                                    │                         │
                                    └──────────┬──────────────┘
                                               ▼
                                         sort_rows()
                                               │
                                               ▼
                                    SearchResult { rows: Vec<DisplayRow> }
                                               │
                                               ▼
                                    display_rows_to_dataframe() ──▶ CLI output
                                    TUI direct rendering
```




---

## 2. Bottleneck Inventory

### Severity Legend

| Icon | Severity | Criteria |
|------|----------|----------|
| 🔴 | **Critical** | >2× improvement on interactive search latency, or >100 MB unnecessary memory |
| 🟡 | **Significant** | 10–50% improvement in its pipeline stage |
| 🟢 | **Moderate** | Polish-level; <10% improvement but easy to fix |

### Summary Table

| ID | Stage | Severity | Description | Est. Impact |
|----|-------|----------|-------------|-------------|
| P1 | Search | 🔴 | Global top-N uses O(N log N) sort instead of O(N log K) heap | 40% faster `*` queries |
| P2 | Search | 🔴 | `to_ascii_lowercase()` heap alloc per record in match loop | Millions of allocs eliminated |
| P3 | Resolve | 🔴 | `DirCache` uses `std::HashMap` (SipHash) for `u32` keys | 3–5× faster path resolution |
| P4 | Sort | 🟡 | Schwartzian transform builds 3 string keys even for numeric sorts | 30K allocs eliminated |
| P5 | Output | 🟡 | `last_results.clone()` deep-copies entire result set | 2 MB alloc eliminated |
| P6 | Build | 🟡 | `names.clone()` creates 140 MB temp for trigram lowercase | 140 MB peak reduction |
| P7 | Sort | 🟡 | `sort_indices_by_name` allocates 2 Strings per comparison | O(N log N) allocs eliminated |
| P8 | Resolve | 🟡 | `FastPathResolver::path_cache` is 168 MB of mostly-empty `Vec<Option<String>>` | 168 MB saved |
| P9 | Build | 🟢 | Trigram LUT allocates 64 MB for 0.3% utilisation | 63.7 MB saved (trade-off) |
| P10 | Search | 🟢 | `std::env::var_os` checked on every search call | Syscall eliminated |
| P11 | Search | 🟢 | `format!("{}:\\", drive.letter)` on every search | Micro-alloc eliminated |
| P12 | Build | 🟢 | `TinyTriSet::insert` linear scan for dedup | OK for ≤253 trigrams |
| P13 | Memory | 🟢 | `CompactRecord` carries 16 B tree fields for files (90% of records) | 112 MB theoretical |
| P14 | Search | 🟡 | Tree search `to_ascii_lowercase()` on every child in traversal | Thousands of allocs |
| P15 | Arch | 🟡 | MftIndex + CompactIndex coexist during build: 2.3 GB peak | Sequencing fix |

---

## 3. Stage-by-Stage Analysis

### 3.1 MFT Ingestion & Parsing

**Verdict: ✅ No bottlenecks found.**

The ingestion pipeline is the best-engineered part of the codebase. Every component
has been correctly optimised:

| Component | Location | Status | Notes |
|-----------|----------|--------|-------|
| `parse_record_zero_alloc` | `uffs-mft/src/parse/zero_alloc.rs` | ✅ | Thread-local buffers, zero heap alloc per record |
| SoA column vectors | `uffs-mft/src/parse/columns.rs` | ✅ | Parse directly into column vecs |
| `ParallelMftReader` (SSD) | `uffs-mft/src/reader/` | ✅ | 8 MB chunks, rayon parallel parse |
| `PrefetchMftReader` (HDD) | `uffs-mft/src/reader/` | ✅ | 4 MB double-buffered, overlapped I/O |
| `MftIndex` pre-allocation | `uffs-mft/src/index/base.rs` L83 | ✅ | `with_capacity_optimized` matches C++ ratios |
| `frs_to_idx` sparse array | `uffs-mft/src/index/model.rs` | ✅ | O(1) lookup, 4 B/slot |
| `IndexNameRef` bit-packing | `uffs-mft/src/index/types.rs` L64 | ✅ | 8 bytes: offset(4) + meta(4) |
| `FileRecord` Pod layout | `uffs-mft/src/index/types.rs` L312 | ✅ | 240 B, bytemuck bulk serialisable |
| `NameArena` contiguous blob | `uffs-core/src/path_resolver/arena.rs` | ✅ | Single `Vec<u8>`, no per-name alloc |

**Observation (non-blocking):** `FileRecord` at 240 bytes includes forensic
timestamps (`fn_created/modified/accessed/mft_changed` = 32 B) and internal
stream fields (16 B) only used in forensic mode. A hot/cold split could save
~48 B/record (20%), but MftIndex is transient (dropped after compact build)
so this is low priority.

---

### 3.2 Compact Index Build

**Verdict: 🟡 Two significant issues.**

The compact build pipeline (`build_compact_index` in `uffs-core/src/compact.rs`)
is well-structured — parallel `par_iter` for primary records, sequential expansion
for hardlinks/ADS (<1% of records), CSR children index via two-pass count+scatter.
Two issues remain:

#### P6 — 140 MB `names_lower` temporary clone 🟡

**File:** `crates/uffs-core/src/compact.rs` lines 432–437

```rust
let trigram = {
    let mut names_lower = names.clone();   // ← 140 MB memcpy
    names_lower.make_ascii_lowercase();    // ← 140 MB in-place mutation
    TrigramIndex::build(&records, &names_lower)
    // names_lower dropped here
};
```

**Also in:** `crates/uffs-core/src/compact_cache.rs` line 159 (cache load path)

```rust
let mut names_lower = names.clone();       // ← same 140 MB clone on EVERY cache load
names_lower.make_ascii_lowercase();
TrigramIndex::build(&records, &names_lower)
```

**Impact:** 140 MB temporary allocation on every fresh build AND every cache load.
For a TUI startup loading 2 drives: 280 MB of transient allocations. The clone
also generates cache pressure that evicts the main `names` blob from L3.

**Root cause:** `TrigramIndex::build()` takes `names_lower: &[u8]` as a pre-lowered
blob. The caller is responsible for creating this lowered copy.

**Proposed fix — inline lowering in trigram build:**

```rust
// In TrigramIndex::build(), change the signature:
pub fn build(records: &[CompactRecord], names: &[u8], pre_lowered: bool) -> Self
// OR better: always lowercase on the fly:

// In the window iteration loop (pass 1 and pass 2):
for window in bytes.windows(3) {
    let tri: [u8; 3] = [
        window[0].to_ascii_lowercase(),
        window[1].to_ascii_lowercase(),
        window[2].to_ascii_lowercase(),
    ];
    let packed = pack_trigram(tri);
    // ... rest unchanged
}
```

This lowercases 3 bytes per iteration — 21 bytes per average filename — instead of
copying and lowering 140 MB. The per-byte cost of `to_ascii_lowercase()` is a
single conditional subtraction, pipelined by the CPU. Net cost: ~0.

**Complexity:** Low. Two call sites change (compact.rs, compact_cache.rs).
`TrigramIndex::build` signature changes to accept original-case `names`.

---

#### P9 — 64 MB Trigram Flat LUT 🟢

**File:** `crates/uffs-core/src/trigram.rs` line 165

```rust
let mut tri_lut = vec![u32::MAX; TRIGRAM_LUT_SIZE];  // 16M × 4 B = 64 MB
```

**Analysis:** Only ~50K of 16M entries are populated (0.3% utilisation). The LUT
provides O(1) lookup during the parallel scatter phase, which is critical for
performance — a HashMap would add ~30 ns/lookup × 7M records × ~10 trigrams/name
= ~2.1 seconds of hash overhead. The LUT costs ~10 ms to allocate.

**Verdict:** The LUT is a **net win** for the build path. However, it creates 64 MB
of cache pressure. For memory-constrained systems (<8 GB), a two-level table
(first byte → sorted Vec of (u16, u32) for remaining 2 bytes) would use ~300 KB.

**Recommendation:** Keep as-is for now. Add an optional low-memory path if needed.

---

#### P15 — Peak Memory: MftIndex + CompactIndex Coexistence 🟡

**File:** `crates/uffs-core/src/compact.rs` `build_compact_index()`

During `build_compact_index`, the caller holds `&MftIndex` (1.6 GB) while building
the `DriveCompactIndex` (560 MB) + 140 MB `names_lower` temp = **2.3 GB peak**.

**Proposed fix:** In the CLI/daemon load path, drop `MftIndex` as soon as compact
build completes. The compact index is the sole search data structure — MftIndex is
only needed for the build. This requires the caller to restructure:

```rust
// Current:
let mft_index = load_mft_index(drive);          // 1.6 GB
let compact = build_compact_index('C', &mft_index); // +560 MB = 2.1 GB
// mft_index still alive here

// Proposed:
let compact = {
    let mft_index = load_mft_index(drive);       // 1.6 GB
    let compact = build_compact_index('C', &mft_index); // +560 MB = 2.1 GB
    drop(mft_index);                              // ← back to 560 MB
    compact
};
```

---

### 3.3 Cache I/O

**Verdict: ✅ Serialisation is excellent. Deserialisation has the P6 clone issue.**

| Component | Status | Notes |
|-----------|--------|-------|
| `serialize_compact` | ✅ | `bytemuck::cast_slice` bulk copy, zero per-record work |
| zstd compression | ✅ | Level 3 (fast), background thread via `save_compact_cache_background` |
| AES-256-GCM encryption | ✅ | Single-pass authenticated encryption |
| `deserialize_compact` | ✅ | `aligned_vec_from_bytes` — alignment-safe bulk copy |
| `ChildrenIndex::from_csr` | ✅ | Zero-rebuild constructor from bulk arrays |
| `TrigramIndex` on load | 🟡 | Rebuilds from names (140 MB clone — see P6) |
| Staleness check | ✅ | Epoch comparison + mtime fallback, header-only fast path |
| Atomic write | ✅ | Prevents partial/corrupt cache files |

The cache format (v5) is well-designed. The only issue is the trigram rebuild
requiring a 140 MB `names_lower` clone (P6 above). Once P6 is fixed (inline
lowering), cache load becomes zero-copy except for the trigram CSR build itself.

**Cache load latency breakdown (7 M records, typical SSD):**
- File read: ~50 ms
- AES decrypt: ~30 ms
- zstd decompress: ~100 ms
- Deserialise (bytemuck bulk): ~10 ms
- Trigram rebuild: ~200 ms (dominated by the 140 MB clone + lowercase)
- **Total: ~390 ms** → with P6 fix: ~290 ms (25% faster)

---

### 3.4 Search & Filter Pipeline

**Verdict: 🔴 Four issues — this is the hottest path (runs on every keystroke in TUI).**

#### P1 — Global Top-N: O(N log N) Sort Instead of O(N log K) Heap 🔴

**File:** `crates/uffs-core/src/search/query.rs` lines 142–213

```rust
fn collect_global_top_n_numeric(...) -> Vec<DisplayRow> {
    let mut candidates: Vec<(u16, u32, i64)> = Vec::new();
    // Iterates ALL 7M records on ALL drives:
    for (drive_idx, drive) in drives.iter().enumerate() {
        for (rec_idx, rec) in drive.records.iter().enumerate() {
            // ... filter checks ...
            candidates.push((drive_idx as u16, rec_idx as u32, sort_key));
        }
    }
    // Sorts ALL 7M candidates:
    candidates.sort_unstable_by_key(|entry| core::cmp::Reverse(entry.2));
    candidates.truncate(limit);  // keeps only 10K
    // ... resolve paths for the 10K winners ...
}
```

**Impact analysis:**
- **Memory:** 7M × 12 bytes = **84 MB** allocated for the candidates Vec
- **CPU:** `sort_unstable_by_key` on 7M items ≈ 7M × log₂(7M) ≈ 7M × 23 ≈ 161M
  comparisons
- **With BinaryHeap(10K):** 7M × log₂(10K) ≈ 7M × 14 ≈ 98M comparisons, but
  only when a candidate beats the current min. In practice, after the heap fills,
  ~99.8% of candidates are rejected with a single comparison against the heap
  minimum. **Effective comparisons: ~7.02M** (the scan) + ~14K (heap ops).
- **Memory with heap:** 10K × 12 bytes = **120 KB** instead of 84 MB.

**This is the single biggest win for match-all (`*`) queries.** The TUI starts with
an implicit `*` query showing top-N by modified date. Every user sees this latency.

**Proposed implementation:**

```rust
use std::collections::BinaryHeap;
use core::cmp::Reverse;

fn collect_global_top_n_numeric(...) -> Vec<DisplayRow> {
    // Min-heap: smallest sort_key at top, so we can quickly reject
    // candidates worse than the current Kth-best.
    let mut heap: BinaryHeap<Reverse<(i64, u16, u32)>> = BinaryHeap::with_capacity(limit + 1);

    let mut lower_buf: Vec<u8> = Vec::with_capacity(256);

    for (drive_idx, drive) in drives.iter().enumerate() {
        for (rec_idx, rec) in drive.records.iter().enumerate() {
            if rec.name_len == 0 { continue; }
            // ... filter checks (same as current) ...

            let sort_key = /* same extraction as current */;

            // For descending sort: negate the key so BinaryHeap min = best
            let effective_key = if sort_desc { -sort_key } else { sort_key };

            if heap.len() < limit {
                heap.push(Reverse((effective_key, drive_idx as u16, rec_idx as u32)));
            } else if let Some(&Reverse((worst_key, _, _))) = heap.peek() {
                if effective_key < worst_key {
                    // This candidate is better than the worst in the heap
                    heap.pop();
                    heap.push(Reverse((effective_key, drive_idx as u16, rec_idx as u32)));
                }
            }
        }
    }

    // Drain heap into sorted order
    let mut candidates: Vec<_> = heap.into_sorted_vec()
        .into_iter()
        .map(|Reverse((_, di, ri))| (di, ri))
        .collect();

    // Resolve paths only for the K winners
    // ...
}
```

**Complexity:** Medium. The function signature stays the same; only internal logic
changes. Must handle ascending vs descending correctly (negate sort key for
descending).

---

#### P2 — `to_ascii_lowercase()` Heap Alloc Per Record in Match Loop 🔴

**File:** `crates/uffs-core/src/search/query.rs` lines 410–440

The `search_compact_drive` function has **two paths** for name matching:

1. **Fast path (memchr):** Uses a reusable `lower_buf` — ✅ zero alloc per record.
2. **Slow path (glob/whole-word):** Calls `name.to_ascii_lowercase()` — 🔴 heap
   alloc per record.

```rust
// Fast path (already correct — lines 429-434):
} else if let Some(fnd) = &finder {
    buf.clear();
    buf.extend_from_slice(name.as_bytes());
    buf.make_ascii_lowercase();        // ← in-place on reusable buffer ✅
    fnd.find(buf.as_slice()).is_some()

// Slow path (lines 435-439):
} else if case_sensitive {
    tree::name_matches(name, needle)
} else {
    let lower = name.to_ascii_lowercase();  // ← NEW String HEAP ALLOC 🔴
    tree::name_matches(&lower, needle)
}
```

**Also in whole-word matching (lines 421-424):**

```rust
let lower = name.to_ascii_lowercase();  // ← heap alloc per record
```

**Impact:** When trigram narrows to 50K candidates and the query uses glob (`*.rs`)
or whole-word matching, this creates **50K heap allocations**. Without trigram
(queries < 3 chars), it's **7M allocations**.

**Proposed fix — extend the `lower_buf` pattern:**

```rust
// Change the matches closure to always use the reusable buffer:
let matches = |name: &str, buf: &mut Vec<u8>| -> bool {
    if name.is_empty() || name == "." { return false; }

    // Prepare lowered name in reusable buffer (zero alloc)
    buf.clear();
    buf.extend_from_slice(name.as_bytes());
    buf.make_ascii_lowercase();
    let lower = core::str::from_utf8(buf.as_slice()).unwrap_or("");

    if whole_word {
        if case_sensitive {
            if is_glob || is_or { tree::name_matches(name, needle) }
            else { name == needle }
        } else {
            if is_glob || is_or { tree::name_matches(lower, needle) }
            else { lower == needle }
        }
    } else if let Some(fnd) = &finder {
        fnd.find(buf.as_slice()).is_some()
    } else if case_sensitive {
        tree::name_matches(name, needle)
    } else {
        tree::name_matches(lower, needle)
    }
};
```

**Complexity:** Low. The `lower_buf` already exists in the function; just extend
its use to all paths.

---

#### P14 — Tree Search `to_ascii_lowercase()` on Every Child 🟡

**File:** `crates/uffs-core/src/search/tree.rs` lines 236, 264, 315

```rust
// In tree_search() leaf matching (line 264):
let child_name = child_rec.name(&drive.names).to_ascii_lowercase();
if name_matches(&child_name, leaf_pattern) { ... }

// In collect_all_descendants (line 315):
let name = child_rec.name(&drive.names).to_ascii_lowercase();
```

**Impact:** Tree search iterates children of candidate directories. For a deep
path pattern like `\users\**\*.rs`, this can touch 100K+ records, each allocating
a new lowercase String.

**Proposed fix:** Thread a reusable `Vec<u8>` buffer through the tree search
functions, same pattern as P2.

---

#### P10 — `std::env::var_os` Checked on Every Search 🟢

**File:** `crates/uffs-core/src/search/query.rs` lines 247, 443, 486

```rust
let profile = std::env::var_os("UFFS_CACHE_PROFILE").is_some();
```

**Fix:**

```rust
use std::sync::LazyLock;
static CACHE_PROFILE: LazyLock<bool> = LazyLock::new(||
    std::env::var_os("UFFS_CACHE_PROFILE").is_some()
);
// Then use: let profile = *CACHE_PROFILE;
```

#### P11 — `volume_prefix` Format on Every Search 🟢

**File:** `crates/uffs-core/src/search/query.rs` multiple locations

```rust
let volume_prefix = format!("{}:\\", drive.letter);  // heap alloc
```

**Fix:** Pre-compute in `DriveCompactIndex` or use a stack-allocated buffer:

```rust
let mut prefix_buf = [0u8; 4]; // "C:\" + null
prefix_buf[0] = drive.letter as u8;
prefix_buf[1] = b':';
prefix_buf[2] = b'\\';
let volume_prefix = core::str::from_utf8(&prefix_buf[..3]).unwrap_or("?:\\");
```

---

### 3.5 Path Resolution

**Verdict: 🔴 One critical issue, one significant issue.**

#### P3 — `DirCache` Uses `std::HashMap` (SipHash) for `u32` Keys 🔴

**File:** `crates/uffs-core/src/search/tree.rs` line 18

```rust
pub type DirCache = HashMap<u32, String>;
```

**Analysis:** Path resolution via `resolve_path_cached` is called for **every
result row** returned by a search. Each call does:
1. Check cache for current record's parent idx — `cache.get(&(current_idx as u32))`
2. Walk parent chain, checking cache at each level
3. Insert intermediate directory paths into cache

For 10K results with average depth 5: **~50K HashMap lookups + ~5K inserts**.

`std::HashMap` uses SipHash — a cryptographic-strength hash designed for DoS
resistance. For `u32` integer keys in an internal data structure with no
adversarial input, this is massive overkill:

| Hasher | Cost per hash | Notes |
|--------|---------------|-------|
| SipHash (std) | ~15 ns | DoS-resistant, designed for untrusted input |
| FxHash | ~2 ns | Identity hash for integers, used by rustc internals |
| `ahash` | ~5 ns | Fast, quality hash; good for mixed key types |

**Proposed fix:**

```rust
// In tree.rs:
use rustc_hash::FxHashMap;
pub type DirCache = FxHashMap<u32, String>;
```

Drop-in replacement. No API changes needed. The `rustc-hash` crate is tiny
(~100 lines), zero dependencies, battle-tested by the Rust compiler itself.

**Impact:** 3–5× faster per lookup × 50K lookups = measurable improvement on
every search. For match-all queries resolving 10K paths: **~0.5 ms → ~0.15 ms**.

---

#### P8 — `FastPathResolver::path_cache` — 168 MB Mostly Empty 🟡

**File:** `crates/uffs-core/src/path_resolver/fast.rs` lines 55, 111

```rust
pub struct FastPathResolver {
    entries: Vec<Option<FastEntry>>,    // 7M × 16 B = 112 MB
    path_cache: Vec<Option<String>>,   // 7M × 24 B = 168 MB ← mostly None!
    // ...
}

// Line 111:
let path_cache = vec![None; entries.len()];  // 168 MB of None values
```

**Impact:** Allocates 168 MB at construction time. Most entries remain `None`
because only paths that are actually resolved get cached. In a typical search
returning 10K results with 5K unique parent directories, only ~5K of 7M entries
are populated (0.07%).

**Proposed fix:**

```rust
path_cache: FxHashMap<u64, String>,  // Only populated entries use memory

// Construction:
let path_cache = FxHashMap::default();

// Lookup (line 143):
if let Some(cached) = self.path_cache.get(&frs) {
    return cached.clone();
}

// Insert (line 151):
self.path_cache.insert(frs, path.clone());
```

**Note:** `FastPathResolver` is used by the **legacy DataFrame path** (not the
compact index search path). The compact path uses `DirCache` instead. This fix
primarily benefits CLI commands that still use the DataFrame path (`info`, `stats`,
`--output` with custom columns).

---

### 3.6 Sort Pipeline

**Verdict: 🟡 Two issues.**

#### P4 — Schwartzian Transform Builds 3 String Keys Even for Numeric Sorts 🟡

**File:** `crates/uffs-core/src/search/backend.rs` lines 508–557

```rust
pub fn sort_rows(rows: &mut [DisplayRow], column: SortColumn, ...) {
    // ALWAYS builds string keys, even when sorting by Size or Modified:
    let mut decorated: Vec<(DisplayRow, RowSortKey)> = rows.iter_mut()
        .map(|row| {
            let key = RowSortKey {
                name: row.name().to_ascii_lowercase(),   // alloc even for Size sort
                path: row.path.to_ascii_lowercase(),     // alloc even for Size sort
                ext: row.name().rsplit('.').next()
                    .unwrap_or("").to_ascii_lowercase(),  // alloc even for Size sort
            };
            (core::mem::take(row), key)
        })
        .collect();
    // ...
}
```

**Impact:** For 10K results sorted by Size (the most common sort after Modified):
- 30K `String` allocations (name + path + ext lowercase) that are never used
- 10K `mem::take` + move-back operations (each moving ~120 B DisplayRow)

**Proposed fix — branch on sort type:**

```rust
pub fn sort_rows(rows: &mut [DisplayRow], column: SortColumn, ...) {
    if rows.len() <= 1 { return; }

    let needs_string_keys = matches!(
        column, SortColumn::Name | SortColumn::Path | SortColumn::Extension
    ) || extra_tiers.iter().any(|t| matches!(
        t.column, SortColumn::Name | SortColumn::Path | SortColumn::Extension
    ));

    if needs_string_keys {
        // Current Schwartzian transform (for string sorts)
        sort_rows_with_string_keys(rows, column, descending, extra_tiers);
    } else {
        // Fast path: sort directly by numeric field (zero alloc)
        rows.sort_unstable_by(|a, b| {
            let mut ord = compare_numeric(a, b, column);
            if descending { ord = ord.reverse(); }
            for tier in extra_tiers {
                if ord != Ordering::Equal { break; }
                ord = compare_numeric(a, b, tier.column);
                if tier.descending { ord = ord.reverse(); }
            }
            // Name tiebreaker uses byte-level comparison (no alloc)
            if ord == Ordering::Equal {
                ord = a.name().bytes().map(u8::to_ascii_lowercase)
                    .cmp(b.name().bytes().map(u8::to_ascii_lowercase));
            }
            ord
        });
    }
}
```

**Complexity:** Low. The existing `compare_by_column` already handles numeric
columns without touching the `RowSortKey`; we just need to avoid creating the
keys at all when they won't be used.

---

#### P7 — `sort_indices_by_name` Allocates 2 Strings Per Comparison 🟡

**File:** `crates/uffs-core/src/search/query.rs` lines 120–135

```rust
fn sort_indices_by_name(indices: &mut [u32], drive: &DriveCompactIndex, desc: bool) {
    indices.sort_unstable_by(|&idx_a, &idx_b| {
        let name_a = drive.records.get(idx_a as usize)
            .map_or("", |rec| rec.name(&drive.names));
        let name_b = drive.records.get(idx_b as usize)
            .map_or("", |rec| rec.name(&drive.names));
        let ord = name_a.to_ascii_lowercase()   // alloc!
            .cmp(&name_b.to_ascii_lowercase());  // alloc!
        if desc { ord.reverse() } else { ord }
    });
}
```

**Impact:** For N children: 2 × N × log₂(N) heap allocations. A directory
with 10K children: ~280K allocations.

**Proposed fix — Schwartzian (decorate-sort-undecorate):**

```rust
fn sort_indices_by_name(indices: &mut [u32], drive: &DriveCompactIndex, desc: bool) {
    // Decorate: pre-compute lowercase names once
    let mut decorated: Vec<(u32, String)> = indices.iter()
        .map(|&idx| {
            let name = drive.records.get(idx as usize)
                .map_or("", |rec| rec.name(&drive.names));
            (idx, name.to_ascii_lowercase())
        })
        .collect();

    decorated.sort_unstable_by(|(_, name_a), (_, name_b)| {
        let ord = name_a.cmp(name_b);
        if desc { ord.reverse() } else { ord }
    });

    // Undecorate
    for (dest, (idx, _)) in indices.iter_mut().zip(decorated) {
        *dest = idx;
    }
}
```

**Reduction:** N allocations instead of 2 × N × log₂(N).

**Alternative (zero-alloc):** Use byte-level comparison with `make_ascii_lowercase`
per byte:

```rust
indices.sort_unstable_by(|&idx_a, &idx_b| {
    let name_a = drive.records.get(idx_a as usize)
        .map_or(&[] as &[u8], |rec| rec.name(&drive.names).as_bytes());
    let name_b = drive.records.get(idx_b as usize)
        .map_or(&[] as &[u8], |rec| rec.name(&drive.names).as_bytes());
    let ord = name_a.iter().map(u8::to_ascii_lowercase)
        .cmp(name_b.iter().map(u8::to_ascii_lowercase));
    if desc { ord.reverse() } else { ord }
});
```

This has zero allocations. Each comparison does a lazy byte-by-byte lowercase
comparison that short-circuits on first difference.

---

### 3.7 Output Pipeline

**Verdict: 🟡 Two issues.**

#### P5 — `last_results.clone()` Deep-Copies Entire Result Set 🟡

**File:** `crates/uffs-core/src/search/backend.rs` lines 425–430

```rust
self.last_results = rows;
SearchResult {
    rows: self.last_results.clone(),  // deep clone — all Strings, all paths
    duration: start.elapsed(),
    records_scanned: scanned,
}
```

**Impact:** For 10K results: 10K × `DisplayRow` deep clone including:
- `path: String` (~80 bytes avg) — 10K heap allocations
- `file_name_range: Range<usize>` — trivial
- `size: u64`, timestamps — trivial
- Total: **~2 MB of heap allocations per search**

The code already has a comment: `"Future optimisation: make SearchResult borrow
from last_results"`.

**Proposed fix — shared ownership via `Arc`:**

```rust
pub struct SearchResult {
    pub rows: Arc<Vec<DisplayRow>>,  // shared, not cloned
    pub duration: Duration,
    pub records_scanned: usize,
}

// In search_drives():
let rows_arc = Arc::new(rows);
self.last_results = Arc::clone(&rows_arc);
SearchResult {
    rows: rows_arc,
    duration: start.elapsed(),
    records_scanned: scanned,
}
```

**Downstream impact:** All consumers of `SearchResult::rows` change from
`Vec<DisplayRow>` to `Arc<Vec<DisplayRow>>`. Since rows are only read (never
mutated) after construction, this is safe. Callers that need mutable access
(e.g., sort) would need `Arc::make_mut` or take ownership before sorting.

**Complexity:** Medium. Requires updating `SearchResult`, `SearchBackend`, and
all callers (TUI, CLI).

---

#### DataFrame Construction for CLI Output

**File:** `crates/uffs-core/src/search/backend.rs` lines 643–687

```rust
pub fn display_rows_to_dataframe(rows: &[DisplayRow]) -> Result<DataFrame> {
    let names: Vec<&str> = rows.iter().map(|r| r.name()).collect();
    let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
    // ... etc for each column
    DataFrame::new(vec![
        Column::new("Name".into(), &names),
        Column::new("Path".into(), &paths),
        // ...
    ])
}
```

**Analysis:** This creates a DataFrame from `DisplayRow` slices — effectively a
row-to-columnar transpose. For CLI output (CSV/TSV), a streaming row writer
would avoid the DataFrame entirely:

```rust
for row in rows {
    writer.write_record(&[row.name(), &row.path, &row.size.to_string()])?;
}
```

However, the DataFrame path provides automatic column formatting, alignment, and
the ability to apply further Polars transformations. For 10K rows the overhead is
negligible (~1 ms). This is a **design trade-off, not a bottleneck**.

**Verdict:** Keep as-is. The DataFrame construction overhead for CLI-sized output
(≤100K rows) is not measurable against the search + sort cost.

---

## 4. Data Structure Audit

### Structures That Are Correct ✅

| Structure | Location | Why It's Right |
|-----------|----------|----------------|
| `MftIndex::frs_to_idx` | `uffs-mft/src/index/model.rs` | `Vec<u32>` indexed by FRS. O(1) lookup, 4 B/slot. Perfect for dense FRS space. |
| `CompactRecord` (80 B Pod) | `uffs-core/src/compact.rs` | Fixed-size, `bytemuck::Pod`, cache-line friendly. Supports zero-copy serde. |
| `ChildrenIndex` (CSR) | `uffs-core/src/compact.rs` | Two arrays: `offsets[N+1]` + flat `children[]`. O(1) child access, contiguous memory. |
| `TrigramIndex` (CSR) | `uffs-core/src/trigram.rs` | Sorted keys + offsets + flat posting list. Binary search O(log K), intersect O(P₁ + P₂). |
| `NameArena` | `uffs-core/src/path_resolver/arena.rs` | Single `Vec<u8>` blob with `(offset, len)` indexing. Zero per-name allocation. |
| `IndexNameRef` (bit-packed) | `uffs-mft/src/index/types.rs` | 8 bytes: offset(4) + length/flags/ext_id(4). Compact metadata for 7M names. |
| `SmallVec<[u8; 64]>` in parser | `uffs-mft/src/parse/zero_alloc.rs` | Stack-allocated for typical NTFS filenames (≤32 chars × 2 bytes = 64 B). |

### Structures That Need Change 🔴

| Structure | Location | Current | Recommended | Rationale |
|-----------|----------|---------|-------------|-----------|
| `DirCache` | `tree.rs:18` | `HashMap<u32, String>` | `FxHashMap<u32, String>` | SipHash overkill for `u32` keys; 3–5× slower |
| `path_cache` | `fast.rs:55` | `Vec<Option<String>>` | `FxHashMap<u64, String>` | 168 MB for 0.07% utilisation |

### Structures With Trade-offs 🟡

| Structure | Location | Current | Alternative | Trade-off |
|-----------|----------|---------|-------------|-----------|
| `FastPathResolver::entries` | `fast.rs:54` | `Vec<Option<FastEntry>>` | `FxHashMap<u64, FastEntry>` | Vec is O(1) but wastes ~15% for sparse FRS. HashMap adds hash cost. Vec wins for <20% sparsity. |
| `CompactRecord::treesize` | `compact.rs` | 16 B in every record | Separate `Vec<(u32, u64, u64)>` for dirs | Saves 112 MB but breaks Pod layout. Low priority. |
| Trigram LUT | `trigram.rs:165` | `Vec<u32>` 64 MB flat | Two-level table, 300 KB | O(1) vs O(log 256) lookup. Flat LUT wins for build speed. |
| `SearchResult::rows` | `backend.rs` | `Vec<DisplayRow>` (cloned) | `Arc<Vec<DisplayRow>>` | Eliminates clone but adds Arc overhead for ownership. |

---

## 5. Memory Profile & Peak Analysis

### Steady-State Memory (After Cache Load, 7M Records Per Drive, 2 Drives)

| Component | Per Drive | 2 Drives | Notes |
|-----------|-----------|----------|-------|
| `CompactRecord` array | 560 MB | 1,120 MB | 7M × 80 B |
| `names` blob | 140 MB | 280 MB | Average 20 chars/name |
| `ChildrenIndex` offsets | 28 MB | 56 MB | 7M × 4 B |
| `ChildrenIndex` children | 28 MB | 56 MB | 7M × 4 B |
| `TrigramIndex` offsets | 200 KB | 400 KB | ~50K trigrams × 4 B |
| `TrigramIndex` postings | 140 MB | 280 MB | 7M × avg 2 trigrams × 4 B |
| **Total steady-state** | **~896 MB** | **~1,792 MB** | |

### Transient Allocations During Operations

| Operation | Transient Memory | With Fixes |
|-----------|-----------------|------------|
| Cache load: names_lower clone (P6) | 140 MB/drive | 0 MB |
| Cache load: trigram LUT (P9) | 64 MB | 64 MB (keep) |
| Search `*`: candidates Vec (P1) | 84 MB | 120 KB |
| Search `*`: path_cache (P8, legacy) | 168 MB | ~1 MB |
| Search: results clone (P5) | 2 MB | 0 MB |
| Search: sort key allocs (P4) | ~0.5 MB | 0 MB |
| **Total transient peak** | **~459 MB** | **~65 MB** |

### Build-Phase Peak Memory (Fresh MFT Read)

```
MftIndex:                    1,600 MB  (7M × 240 B — FileRecord)
  + CompactIndex building:   + 560 MB  (CompactRecord)
  + names blob:              + 140 MB
  + names_lower temp (P6):   + 140 MB
  + trigram LUT (P9):        +  64 MB
  ─────────────────────────────────────
  Peak:                      2,504 MB

With P6 fix:                 2,364 MB
With P15 fix (drop MftIndex early): 904 MB peak after compact is built
With both P6 + P15:          764 MB post-build
```

---

## 6. Prioritised Implementation Plan

### Wave 1: Quick Wins (Low risk, high impact, ≤1 hour each)

These changes are surgical, API-preserving, and can be done independently.

| # | Fix | Files Changed | Risk | Test Strategy |
|---|-----|---------------|------|---------------|
| 1 | **P3: FxHashMap for DirCache** | `tree.rs` + Cargo.toml | Trivial | Existing tests pass; benchmark path resolution |
| 2 | **P2: Extend lower_buf to all match paths** | `query.rs` (~20 lines) | Low | Existing search tests; glob + whole-word cases |
| 3 | **P7: Zero-alloc sort_indices_by_name** | `query.rs` (~15 lines) | Low | Existing sort tests |
| 4 | **P10: LazyLock for CACHE_PROFILE** | `query.rs` (~5 lines) | Trivial | Existing tests |
| 5 | **P11: Stack-allocated volume_prefix** | `query.rs` (~10 lines) | Trivial | Existing tests |

**Expected total impact:** 3–5× faster path resolution, millions fewer allocs in
search hot path, eliminates syscalls per search.

### Wave 2: Medium Changes (Moderate impact, some API changes)

| # | Fix | Files Changed | Risk | Test Strategy |
|---|-----|---------------|------|---------------|
| 6 | **P1: BinaryHeap for global top-N** | `query.rs` (~60 lines) | Medium | Test with known data: verify top-N ordering matches current; edge cases: limit > N, empty drives |
| 7 | **P4: Skip string keys for numeric sorts** | `backend.rs` (~40 lines) | Low | Sort stability tests with mixed tiebreakers |
| 8 | **P6: Inline lowercase in trigram build** | `trigram.rs` (~20 lines), `compact.rs` (~5 lines), `compact_cache.rs` (~5 lines) | Medium | Trigram search accuracy tests; verify same posting lists produced |
| 9 | **P14: Reusable buffer in tree search** | `tree.rs` (~15 lines) | Low | Tree search tests with case-insensitive patterns |

**Expected total impact:** 40% faster `*` queries, 140 MB less transient memory,
zero-alloc numeric sorts.

### Wave 3: Architectural Changes (Higher impact, broader changes)

| # | Fix | Files Changed | Risk | Test Strategy |
|---|-----|---------------|------|---------------|
| 10 | **P5: Arc<Vec<DisplayRow>> for results** | `backend.rs`, TUI app, CLI handler | Medium | All search consumers; verify no mutation after construction |
| 11 | **P8: FxHashMap for path_cache** | `fast.rs` (~10 lines) | Low | Path resolution accuracy tests |
| 12 | **P15: Drop MftIndex early** | CLI load path, daemon startup | Low | Build + search integration tests |

**Expected total impact:** 2 MB clone eliminated per search, 168 MB less memory
for legacy path, 1.6 GB freed sooner during build.

### Implementation Order Rationale

```
Wave 1 (P3, P2, P7, P10, P11) ──▶ Immediate, safe, measurable
    │
    ▼
Wave 2 (P1, P4, P6, P14) ──▶ Higher impact, needs targeted testing
    │
    ▼
Wave 3 (P5, P8, P15) ──▶ API changes, broader testing needed
```

Dependencies:
- P2 and P14 are the same pattern (lower_buf); do P2 first, then apply to P14.
- P3 requires adding `rustc-hash` crate dependency.
- P5 changes `SearchResult` shape — wait until after P1/P4 to avoid merge conflicts.
- P6 and P9 are independent of each other (different phases of trigram lifecycle).
- P15 is a caller-level change, independent of all others.

---

## 7. Validation Strategy

### Correctness Validation

Every fix must pass these checks:

1. **Existing test suite:** `cargo nextest run -p uffs-core -p uffs-mft`
2. **Search accuracy:** For a known index, verify that search results (names, paths,
   sizes, timestamps) are identical before/after the fix. Use golden-output tests.
3. **Sort stability:** For P1, P4, P7: verify that sort order matches current
   behaviour exactly (including tiebreakers).
4. **Trigram accuracy (P6):** Verify that `TrigramIndex::build` with inline
   lowering produces identical posting lists to the current clone+lowercase
   approach. Bit-for-bit comparison of the CSR arrays.

### Performance Validation

Each wave should be benchmarked on the Windows target with a representative
7M-record MFT index:

```
Benchmark suite:
  1. Cache load time (cold start)
  2. Match-all `*` query with top-10K by Modified (exercises P1)
  3. Substring search "readme" (exercises P2, P3)
  4. Glob search "*.rs" (exercises P2, P14)
  5. Whole-word search "main" (exercises P2)
  6. Tree search "\\users\\**\\*.log" (exercises P14, P3)
  7. Sort by Size descending (exercises P4)
  8. Sort by Name ascending (exercises P7)
  9. Peak RSS measurement during fresh build (exercises P6, P15)
  10. Peak RSS measurement during cache load (exercises P6)
```

### Metrics to Track

| Metric | Tool | Target |
|--------|------|--------|
| Search latency (p50, p99) | `std::time::Instant` in existing profiling | <50 ms for 7M records |
| Heap allocations per search | `dhat` or `tikv-jemallocator` with stats | <1000 for substring search |
| Peak RSS | `/proc/self/status` or Windows `GetProcessMemoryInfo` | <1.2 GB for single drive |
| Cache load time | Existing `UFFS_CACHE_PROFILE` timing | <300 ms for 7M records |

---

## Appendix: What's Already Well-Optimised

These components were audited and found to be **correct and efficient**. No changes
recommended:

| Component | Why It's Good |
|-----------|---------------|
| `parse_record_zero_alloc` | Thread-local buffers, SmallVec for filenames, zero heap alloc per record |
| `ParallelMftReader` | 8 MB aligned chunks, rayon parallel parse, SoA output |
| `PrefetchMftReader` | Double-buffered overlapped I/O for HDD sequential reads |
| `bytemuck` serialisation | Pod bulk copy for cache write — zero per-record serialisation |
| `TrigramIndex::search` | Binary search on sorted keys, smallest-first intersection, CSR postings |
| `ChildrenIndex::build` | Two-pass count+scatter into contiguous array — textbook CSR construction |
| `CompactRecord` Pod layout | 80 B fixed, cache-line friendly, zero-copy deserialise |
| `save_compact_cache_background` | Background thread for cache save — doesn't block search startup |
| `NameArena` contiguous blob | Single allocation for all names, indexed by (offset, len) |
| `memchr::memmem::Finder` fast path | SIMD-accelerated substring search for the common case |