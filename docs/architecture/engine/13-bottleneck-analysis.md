# Performance Bottleneck Analysis — Full Pipeline Deep Dive

> **Date**: 2026-04-02  
> **Scope**: Every stage from raw MFT bytes → search results → formatted output  
> **Method**: Static analysis of all hot-path code, data structure audit, allocation
>   tracking, cache-friendliness assessment  

---

## Executive Summary

UFFS has an excellent high-level architecture.  The LIVE path (`parse_record_to_index`),
the CSR `ChildrenIndex`, the CSR `TrigramIndex`, and the compact 80-byte
`CompactRecord` are all well-engineered.  However, several **secondary paths** and
**output-stage code** contain bottlenecks that can dominate wall-clock time for
common workloads.  The issues below are ranked by estimated real-world impact.

### Severity Legend

| Icon | Meaning |
|------|---------|
| 🔴 | Critical — measurable wall-clock regression, easy to verify |
| 🟡 | Medium — noticeable under load or large result sets |
| 🟢 | Low / already optimised — documented for completeness |

---

## Pipeline Stage Map

```
RAW BYTES ──► PARSE ──► MftIndex ──► CompactRecord ──► SEARCH ──► DisplayRow ──► OUTPUT
  (disk)      (CPU)    (240B/rec)    (80B/rec+CSR)   (trigram)   (owned Strings)  (I/O)
```

Each section below corresponds to one stage.

---

## STAGE 1 — MFT Ingestion

### 🟢 1A. LIVE Path: `parse_record_to_index` (direct-to-index)

**File**: `crates/uffs-mft/src/io/parser/index.rs`  
**Status**: Already optimal.

The LIVE path parses raw MFT records and writes directly into `MftIndex`
fields — no intermediate `ParsedRecord`, no per-record heap allocations for
names/streams.  Names are appended to the contiguous `MftIndex::names` buffer.
This is the gold-standard path.

### ✅ 1B. OFFLINE Search Path — RESOLVED (2026-04-02)

**Status**: Fixed.  All paths now converge on the unified `process_record()`
parser via `load_raw_to_index_direct()`.

**What was wrong**: `compact_loader.rs` and `reader/index_read.rs`
(`read_index_from_file`) called the legacy `load_raw_to_index_with_options`,
which used `parse_record_full()` → `ParsedRecord` → `MftRecordMerger` with
≥ 28M heap allocations for a 7M-record MFT.  The `UFFS_LEGACY_PARSE=1` env
var escape hatch also existed in the LIVE path and `commands/load.rs`.

**What was fixed**:
- `compact_loader.rs` → switched to `load_raw_to_index_direct`
- `reader/index_read.rs` (`read_index_from_file`) → switched to `load_raw_to_index_direct`
- `commands/load.rs` → removed `UFFS_LEGACY_PARSE` if/else branches (×2)
- `reader/index_read.rs` (`read_mft_index_internal`) → removed legacy escape hatch
- Doc comments in `merger.rs`, `full.rs`, `builder.rs` updated

All paths now use the same parser:

| Path | Caller | Function | Parser |
|------|--------|----------|--------|
| LIVE | `read_mft_index_internal` | `process_record()` | ✅ Unified |
| `uffs_mft load` CLI | `cmd_load` | `load_raw_to_index_direct()` | ✅ Unified |
| `uffs` search CLI | `compact_loader.rs` | `load_raw_to_index_direct()` | ✅ Unified |
| File-based reader | `read_index_from_file` | `load_raw_to_index_direct()` | ✅ Unified |

**Savings**: 1–3 seconds on a 7M-record drive; ~200 MB less peak heap.

### 🟢 1C. Cache Load: `.uffs` Deserialization (v10+)

**File**: `crates/uffs-mft/src/index/storage/deserialize.rs`, lines 202–209

v10+ (current `INDEX_VERSION = 12`) already uses `bytemuck` bulk copy for
`FileRecord` (240 bytes/record).  The `frs_to_idx` table is also bulk-copied
via `aligned_vec_from_bytes`.  **No action needed** for the current version.

Legacy v3–v9 caches fall back to field-by-field reads (30+ macro calls per
record).  This is acceptable because old caches are rare and eventually expire.

### 🟢 1D. Compact Cache: `.uffs_compact` Deserialization

**File**: `crates/uffs-core/src/compact_cache.rs`

Records via `bytemuck::cast_slice`, CSR children/trigram via bulk copy.
Already optimal.

---

## STAGE 2 — Index Build (MftIndex → DriveCompactIndex)

### ✅ 2A. `names_lower` Construction — ALREADY OPTIMISED

**File**: `crates/uffs-core/src/compact.rs`, lines 429–432

Already uses the in-place pattern — no intermediate `String` allocation:

```rust
// Clone-then-lowercase avoids the intermediate `String` allocation that
// `to_ascii_lowercase().into_bytes()` would create (~150MB saved).
let mut names_lower = names.clone();
names_lower.make_ascii_lowercase();
```

**No action needed.**

### ✅ 2B. Trigram Build — Scatter Phase Parallelised (2026-04-02)

**File**: `crates/uffs-core/src/trigram.rs`, function
`scatter_postings_parallel()`

**Status**: Fixed.  Pass 2 (scatter) is now parallelised with rayon.

**How**: Per-chunk trigram counts from Pass 1 are retained (previously
merged and discarded).  These are used to compute non-overlapping write
regions in the CSR `values` array for each chunk.  Each chunk then scatters
independently via `par_chunks`.  Records within each chunk remain in order,
and chunks are ordered, so posting lists stay sorted by record index.

Uses `AtomicU32` with `Relaxed` ordering for the shared values array.
On x86-64 this compiles to plain `mov` — zero overhead vs non-atomic
writes.  This avoids `unsafe` in a crate that `#![forbid(unsafe_code)]`.

**Estimated savings**: 2–4× faster trigram build for 7M records.

### 🟢 2C. `ChildrenIndex::build` — Already Optimal

**File**: `crates/uffs-core/src/compact.rs`, lines 118–163

Two-pass counting-sort into CSR arrays.  O(n) time, O(n) space, excellent
cache locality.  No HashMap, no per-record Vec allocations.  **No action
needed.**

### 🟢 2D. `CompactRecord` Build Loop — Acceptable

**File**: `crates/uffs-core/src/compact.rs`, `build_compact_index()`

Single-threaded iteration over `MftIndex::records`, extracting fields into
80-byte `CompactRecord`s.  The FRS→idx lookup for `parent_idx` can cause
random cache misses in `frs_to_idx`, but the overall time is small (~50 ms
for 7M records) because the per-record work is trivial.

---

## STAGE 3 — Search & Filter

### 🔴 3A. `resolve_path` — No Caching of Directory Prefixes

**File**: `crates/uffs-core/src/search/tree.rs`, lines 14–59

```rust
pub fn resolve_path(
    drive: &DriveCompactIndex,
    record_idx: usize,
    volume_prefix: &str,
) -> String {
    let mut components = Vec::with_capacity(8);
    let mut current_idx = record_idx;
    loop {
        let record = drive.records.get(current_idx)?;
        let name = record.name(&drive.names);
        components.push(name);
        current_idx = record.parent_idx as usize;
        // ... walks to root every time
    }
    // reverse + join
}
```

**Problem**: Every search result independently walks the entire parent chain
from leaf to root.  For 10,000 results in `C:\Users\John\Documents\`, each
result re-walks the same 4-level chain.  That's 40,000 redundant name lookups
and string operations.

**Why it's critical**: Path resolution is called in `indices_to_rows()` for
every matched record.  For large result sets (10K–100K rows) or
path-heavy patterns, this dominates query latency.

**Fix**: Add a `HashMap<u32, String>` cache (directory_idx → resolved prefix).
When walking the parent chain, check the cache at each level.  On cache hit,
prepend the cached prefix and store the newly resolved intermediate paths.

```rust
pub fn resolve_paths_cached(
    drive: &DriveCompactIndex,
    record_indices: &[usize],
    volume_prefix: &str,
) -> Vec<String> {
    let mut dir_cache: HashMap<u32, String> = HashMap::with_capacity(256);
    record_indices.iter().map(|&idx| {
        let mut chain: SmallVec<[usize; 8]> = SmallVec::new();
        let mut current = idx;
        // Walk up until cache hit or root
        loop {
            if let Some(cached) = dir_cache.get(&(current as u32)) {
                // Build path from cache + remaining chain
                return build_from_cache(cached, &chain, drive);
            }
            chain.push(current);
            // ... continue walking ...
        }
        // ... populate cache with intermediate directories ...
    }).collect()
}
```

Note: `MftIndex::PathResolver` already has a `dir_cache` with this exact
pattern (see `crates/uffs-mft/src/index/path_resolver.rs` lines 130–148).
The compact index path should adopt the same approach.

**Estimated savings**: 50–80% reduction in path resolution time for typical
search queries with many results.

### 🟢 3B. Trigram Search — Well Optimised

**File**: `crates/uffs-core/src/trigram.rs`, `TrigramIndex::search()`

Binary search on sorted `keys` → CSR slice lookup → merge-intersect of
posting lists.  The lists are already sorted by record index (guaranteed by
the scatter-in-order build), so intersection is a single linear merge pass.

### 🟡 3C. `intersect_sorted` — Clones First Posting List

**File**: `crates/uffs-core/src/trigram.rs`, lines 256–268

```rust
let mut result = first_list.to_vec();  // ← clones entire posting list
for list in lists.iter().skip(1) {
    result = intersect_sorted(&result, list);
}
```

The first (smallest) posting list is cloned into a `Vec<u32>`.  For
moderately selective trigrams, this can be 100K–1M entries (400 KB – 4 MB).
Each subsequent intersection allocates a new `Vec` for the result.

**Fix**: In-place intersection that shrinks `result` without re-allocating:

```rust
fn intersect_in_place(result: &mut Vec<u32>, other: &[u32]) {
    let mut write = 0;
    let mut j = 0;
    for i in 0..result.len() {
        while j < other.len() && other[j] < result[i] { j += 1; }
        if j < other.len() && other[j] == result[i] {
            result[write] = result[i];
            write += 1;
            j += 1;
        }
    }
    result.truncate(write);
}
```

This eliminates N-1 allocations (one per intersection step).

**Estimated savings**: Negligible for highly selective queries; up to 50%
less allocation for broad queries with many trigram intersections.

### 🟡 3D. Linear Fallback for Short Queries (< 3 chars)

**File**: `crates/uffs-core/src/search/query.rs`, lines 316–328

For 1–2 character queries, trigram search returns `None` and the code falls
back to a full linear scan of all records:

```rust
drive.records.iter().enumerate()
    .filter(|(_, rec)| {
        let name = rec.name(names_blob);
        matches(name)
    })
    .take(limit)
```

For 7M records, this is a ~25 ms sequential scan.  Acceptable for rare
queries, but could be improved with:

- **1-gram / 2-gram index**: A supplementary flat array mapping each
  byte/pair to a bitset of matching records.  Space: 256 × (7M/8) = 224 KB
  for unigrams.
- **SIMD `memchr`**: Use the `memchr` crate for vectorised byte searching
  within the names blob — processes 32 bytes per cycle on AVX2.

**Priority**: Low.  The `take(limit)` short-circuits early for typical
interactive searches.

---

## STAGE 4 — Result Construction

### 🟡 4A. `DisplayRow` Owns Strings — Per-Result Heap Allocations

**File**: `crates/uffs-core/src/search/backend.rs`

```rust
pub struct DisplayRow {
    pub path: String,   // heap-allocated owned String
    pub name: String,   // heap-allocated owned String
    // ... 11 more fields (all Copy)
}
```

**File**: `crates/uffs-core/src/search/query.rs`, `make_display_row()`

```rust
fn make_display_row(...) -> DisplayRow {
    DisplayRow {
        name: name.to_owned(),      // ← HEAP ALLOC
        path: /* resolve_path() */,  // ← HEAP ALLOC (builds a new String)
        // ...
    }
}
```

For 10,000 results, this creates **20,000 heap allocations** (one `String`
for `name`, one for `path`).  The `name` is already available as a
zero-copy `&str` slice into the names blob; the `path` could be written
directly to the output buffer without intermediate ownership.

**Fix** (large refactor): Introduce a streaming output mode where
`CompactRecord` + names blob → formatted row is done in a single pass
without constructing `DisplayRow`.  For sorted output (which requires
all rows in memory), keep `DisplayRow` but use `Cow<'a, str>` for `name`
to avoid cloning.

**Priority**: Medium.  The allocation cost is ~0.5 ms for 10K results —
dwarfed by path resolution.  Becomes relevant for 100K+ result sets.

### 🟡 4B. `display_rows_to_dataframe` Roundtrip (JSON/Table Only)

**File**: `crates/uffs-cli/src/commands/output/mod.rs`, lines 56–65

For `json` and `table` output formats, the search results are converted
from `Vec<DisplayRow>` → `DataFrame` → serialised output.  This creates a
Polars DataFrame from owned Strings, which involves:

1. Collecting all `path` strings into a `StringChunked` column
2. Collecting all `name` strings into a `StringChunked` column
3. Building typed columns for each numeric field

**Impact**: For 10K results, this is ~2–5 ms — acceptable.  For 100K+
results, the DataFrame construction becomes noticeable (~20–50 ms).

**Fix**: For `json` output, serialise directly from `DisplayRow` using
`serde_json` without the DataFrame intermediary.  For `table` output,
format directly using the `comfy-table` or similar crate.

**Priority**: Low.  JSON/table output is rarely used for large result sets.

---

## STAGE 5 — Output I/O

### 🔴 5A. Console Output — No `BufWriter` Wrapping

**File**: `crates/uffs-cli/src/commands/output/mod.rs`, lines 68–78

```rust
let stdout_handle = std::io::stdout();
let mut stdout = stdout_handle.lock();
// ↑ Raw locked stdout — every write_all() is a syscall!

match format {
    "csv" => output_config.write_display_rows(rows, &mut stdout)?,
    // ...
}
```

Compare with the file output path (same file, lines 85–90):

```rust
let file = File::create(path)?;
let mut writer = BufWriter::new(file);  // ← Correctly buffered!
output_config.write_display_rows(rows, &mut writer)?;
```

**Problem**: `write_display_rows` calls `writer.write_all(buf.as_bytes())`
once per row.  For console output, each call is an **unbuffered syscall**
(`write(1, ...)` on POSIX, `WriteFile` on Windows).

For 10,000 rows × ~200 bytes each:
- **Without BufWriter**: 10,000 syscalls, ~50 µs each → **~500 ms**
- **With BufWriter(64K)**: ~31 syscalls → **< 1 ms**

**Fix** (1 line):

```rust
let mut stdout = BufWriter::with_capacity(64 * 1024, stdout_handle.lock());
```

Flush is automatic on `Drop`, or add explicit `writer.flush()?;` after the
loop.

**Estimated savings**: 5–20× faster console output for result sets > 100
rows.  This may be the single highest-impact fix in the entire codebase for
interactive use.

### 🟢 5B. File Output — Properly Buffered

Already uses `BufWriter::new(file)`.  **No action needed.**

### 🟢 5C. Row Formatting — Reusable Buffer

`write_display_rows` reuses a `String` buffer (`buf.clear()` per row)
and uses `itoa::Buffer` for integer formatting.  **No action needed.**

---

## Data Structure Audit

### DS1. `frs_to_idx: Vec<u32>` — ✅ Correct

Sparse array indexed by FRS.  For 7M files, max FRS ≈ 8M → 32 MB.
O(1) lookup, cache-friendly sequential access during build.  Better than
a `HashMap<u64, u32>` which would add 40+ bytes overhead per entry and
have random memory access patterns.

### DS2. `ChildrenIndex` (CSR) — ✅ Correct

Two flat arrays (`offsets: Vec<u32>`, `values: Vec<u32>`).  O(1) children
lookup via `&values[offsets[idx]..offsets[idx+1]]`.  Cache-friendly,
zero per-directory allocation.  Replaced the old `HashMap<u64, Vec<u64>>`
which had ~40 bytes per entry overhead and N heap allocations.

### DS3. `TrigramIndex` (CSR + flat LUT) — ✅ Correct

64 MB flat lookup table for O(1) trigram→key_index.  CSR posting lists.
No HashMap in the query path.  Build uses counting sort.  Optimal.

### DS4. `CompactRecord` (80 bytes, `#[repr(C)]`, `Pod`) — ✅ Correct

Flat, fixed-size, `bytemuck::Pod` for zero-copy serialisation.  Fields are
ordered to avoid padding.  80 bytes is tight for the data stored (FRS,
parent, name offset/len, timestamps, flags, sizes, tree metrics).

### DS5. `MftIndex::children: Vec<ChildInfo>` (linked list) — ⚠️ Acceptable

Children stored as an intrusive linked list within a flat `Vec`.  Each
`ChildInfo` has a `next_entry: u32` pointing to the next sibling.

**Concern**: Walking a directory's children requires following `next_entry`
pointers, which jumps around within the Vec.  For a directory with 1000
files, this is 1000 random-indexed accesses (though all within the same
contiguous allocation, so TLB misses are unlikely).

**Verdict**: Acceptable.  The `ChildInfo` Vec is only used during
`MftIndex` construction and tree-metric computation, then discarded when
`DriveCompactIndex` is built (which uses CSR `ChildrenIndex`).  Not on
the search hot path.

### DS6. `names: Vec<u8>` + `names_lower: Vec<u8>` — ⚠️ Double Memory

Two copies of the names blob (~140 MB each for 7M records).  Total ~280 MB
for names alone.

**Alternatives considered**:
1. **Store only lowercase, reconstruct original from MFT on demand** —
   Breaks display of original-case names, which is a UX requirement.
2. **Interleave original and lowercase in a single buffer** — Saves one
   allocation but same total memory.
3. **Compute lowercase on the fly during search** — Adds `make_ascii_lowercase`
   per comparison.  For trigram search (which only looks up pre-built indices),
   this is irrelevant.  For linear-scan fallback, it would add ~5 ns per
   record (35 ms for 7M records) — acceptable.
4. **Store lowercase only + a "case bits" bitfield** — For ASCII names,
   one bit per character encodes whether it was uppercase.  Halves names
   memory.  Complex to implement, marginal benefit.

**Verdict**: Current approach is the right trade-off.  The `names_lower`
blob is essential for case-insensitive trigram search and the memory cost
(~140 MB) is acceptable for a tool that processes 7M+ file records.

### DS7. Legacy `TreeIndex` (`HashMap<u64, Vec<u64>>`) — 🗑️ Dead Weight

**File**: `crates/uffs-core/src/tree/index.rs`

Only used in benchmarks (`benches/`).  Not in any production CLI or TUI
code path.  The `HashMap`-based design has ~40 bytes overhead per entry ×
7M records = ~280 MB of hash overhead alone.

**Recommendation**: Gate behind `#[cfg(test)]` or remove entirely.  The
production code path uses `ChildrenIndex` (CSR), which is correct.

---

## Priority Implementation Matrix

| # | Fix | Impact | Effort | Stage |
|---|-----|--------|--------|-------|
| 1 | **BufWriter for console stdout** | 🔴 5–20× output speed | 1 line | Output |
| 2 | **resolve_path directory cache** | 🔴 50–80% less search latency | ~30 lines | Search |
| ~~3~~ | ~~**OFFLINE: switch to `load_raw_to_index_direct`**~~ | ✅ **DONE** | — | Ingest |
| ~~4~~ | ~~**names_lower in-place lowering**~~ | ✅ **ALREADY DONE** | — | Build |
| 5 | **intersect_sorted in-place** | 🟡 less alloc during search | ~15 lines | Search |
| ~~6~~ | ~~**Parallelise scatter_postings**~~ | ✅ **DONE** | — | Build |
| 7 | **Remove/gate legacy TreeIndex** | 🟡 less dead code | ~5 lines | Cleanup |
| 8 | **Streaming DisplayRow (Cow/borrow)** | 🟡 less alloc for 100K+ results | ~100 lines | Results |
| 9 | **Direct JSON serialisation** | 🟢 skip DataFrame roundtrip | ~40 lines | Output |
| 10 | **1-gram/2-gram index for short queries** | 🟢 faster 1–2 char search | ~80 lines | Search |

---

## Already Well-Optimised (No Action Required)

| Component | Why It's Right |
|-----------|---------------|
| LIVE `parse_record_to_index` | Direct-to-index, zero intermediate allocations |
| `ChildrenIndex` (CSR) | Flat arrays, O(1) lookup, counting-sort build |
| `TrigramIndex` (CSR + 64MB LUT) | O(1) key lookup, counting-sort build, merge-intersect |
| `CompactRecord` (80B, Pod) | `bytemuck::cast_slice` for zero-copy serialisation |
| `frs_to_idx` (sparse Vec) | O(1) FRS→idx, correct for dense FRS space |
| Cache deserialisation (v10+) | Bulk `bytemuck` copy, no field-by-field reads |
| File output buffering | `BufWriter::new(file)` with proper flush |
| Row format buffer reuse | `buf.clear()` + `itoa::Buffer` per row |
| `TinyTriSet` linear scan | Beats hashing for ≤253 elements (typical filenames) |

---

## Methodology Notes

- Analysis performed via static code review of all `crates/uffs-{mft,core,cli}/src/` hot paths
- Allocation counts estimated assuming 7M MFT records (typical 1TB Windows drive)
- Timing estimates based on documented profiling data from `docs/architecture/engine/11-performance-deep-dive.md`
  and standard per-operation costs (syscall ~50µs, malloc/free ~30–80ns, cache miss ~100ns)
- No live profiling performed — estimates should be validated with `perf`/`flamegraph` before implementation

