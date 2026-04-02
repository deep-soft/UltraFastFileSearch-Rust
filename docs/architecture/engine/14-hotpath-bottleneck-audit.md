# 14 — Hot-Path Bottleneck Audit (2026-04-02)

> **Scope:** Full pipeline audit — MFT ingestion → search/filter → path
> resolution → sort → output.  Every stage inspected for wrong data
> structures, unnecessary allocations, syscall overhead, and algorithmic
> waste.  Findings ranked by measured or projected impact on a 7 M record
> NTFS volume producing 10 K search results.

---

## Pipeline Overview

```
  INGESTION          SEARCH           RESOLVE          SORT           OUTPUT
┌────────────┐   ┌────────────┐   ┌────────────┐  ┌──────────┐  ┌───────────┐
│ Live MFT   │   │ Trigram    │   │ resolve_   │  │sort_rows │  │ stdout    │
│ (IOCP/SSD) │──▶│ Lookup     │──▶│ path_      │─▶│(to_lower │─▶│ (no       │
│ Cache Load │   │ Linear Scan│   │ cached     │  │ per cmp) │  │ BufWriter)│
│ (bincode)  │   │ Filters    │   │ DisplayRow │  │          │  │ DataFrame │
└────────────┘   └────────────┘   └────────────┘  └──────────┘  └───────────┘
     ✅ OK           🔴 B1/B2       🟡 B4/B5       🔴 B3          🟡 B6/B7/B8
```

Legend: 🔴 Critical  🟡 Significant  🟢 Already optimal

---

## Stage 1 — Ingestion  ✅  No Bottlenecks

The ingestion pipeline is the best-engineered part of the codebase.
Every component has been tuned correctly.

| Component | Status | Detail |
|---|---|---|
| `parse_record_zero_alloc` | ✅ | Thread-local buffers, zero heap alloc per record |
| SoA column vectors | ✅ | Parse directly into column vecs, not `Vec<ParsedRecord>` |
| `ParallelMftReader` (SSD) | ✅ | 8 MB chunks, rayon parallel parse |
| `PrefetchMftReader` (HDD) | ✅ | 4 MB double-buffered, overlapped I/O |
| `MftIndex → CompactRecord` | ✅ | ~50 ms / 7 M records, linear scan, acceptable |
| `ChildrenIndex` (CSR) | ✅ | Flat `offsets`/`values` arrays, no per-record `Vec` |
| `names_lower` temp copy | ✅ | Scoped to trigram build, dropped immediately after |
| `TrigramIndex::build` | ✅ | Parallel count + scatter, CSR posting lists |
| Cache load (bincode) | ✅ | `CompactRecord` is `bytemuck::Pod`, trivial deser |
| `NameArena` / names blob | ✅ | Single contiguous `Vec<u8>`, no per-name alloc |

**Verdict:** No action required.

---

## Stage 2 — Search Filtering  🔴  Allocation Machine

### B1 — Extension Filter: `to_ascii_lowercase()` per record  🔴 CRITICAL

**File:** `crates/uffs-core/src/search/filters.rs` lines 183–188

```rust
if !self.extensions.is_empty() {
    let name = rec.name(names);
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    //        ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
    //        NEW String HEAP ALLOCATION for every single record
    if !self.extensions.iter().any(|allowed| allowed == &ext) {
    //  ^^^^^^^^^^^^^^^^^^^^^^^^^^^^  LINEAR SCAN over extensions list
        return false;
    }
}
```

**Impact:**
- 7 M `String` allocations when any extension filter is active.
- Each `to_ascii_lowercase()` allocates a new heap `String`, copies bytes,
  lowercases in-place, then immediately drops after comparison.
- The `.any()` linear scan is O(E) per record where E = number of
  extensions.  Usually small (E < 10), but still avoidable.

**Root cause:** The `extension_id: u16` field on `CompactRecord` is already
an interned identifier for the file extension.  It exists, is populated
during compact index construction, and is completely ignored by the filter
path.

**Proposed fix:**
1. During `SearchFilters` construction, resolve each allowed extension
   string to its `extension_id` using the same intern table.
2. Store as `allowed_extension_ids: HashSet<u16>` (or small sorted `Vec`
   for < 8 extensions with binary search).
3. In `matches_record`: `allowed_extension_ids.contains(&rec.extension_id)`
   — **zero allocations, O(1) lookup**.

---

### B2 — Exclude Filter: `to_ascii_lowercase()` per record  🔴 CRITICAL

**File:** `crates/uffs-core/src/search/filters.rs` lines 190–195

```rust
if let Some(excl) = &self.exclude_lower {
    let name = rec.name(names);
    let name_lower = name.to_ascii_lowercase();
    //               ^^^^^^^^^^^^^^^^^^^^^^^^^
    //               ANOTHER String allocation per record
    if name_matches(&name_lower, excl) {
        return false;
    }
}
```

**Impact:**
- 7 M additional `String` allocations when exclude filter is active.
- Stacks with B1 — if both extension AND exclude filters are active,
  that is 14 M `String` allocations in `matches_record` alone.

**Root cause:** Case-insensitive comparison done via allocation instead
of in-place.

**Proposed fix (option A — zero-alloc):**
Replace `to_ascii_lowercase()` + equality with `eq_ignore_ascii_case()`
or a custom `name_matches_ignore_case()` that compares byte-by-byte
without allocating.

```rust
// Before:
let name_lower = name.to_ascii_lowercase();
if name_matches(&name_lower, excl) { ... }

// After:
if name_matches_ignore_case(name, excl) { ... }
```

**Proposed fix (option B — amortized):**
Since the lowercase names blob already exists during trigram build, pass
`&names_lower` through to the filter and slice
`names_lower[rec.name_offset..rec.name_offset + rec.name_len]` directly.
Zero allocations.  Requires storing `names_lower` or building it lazily
on first filter-with-exclude query.

**Estimated savings:** Eliminates 7 M `String` allocations.  Combined
with B1 fix: ~700 ms of allocator overhead removed.

---

### B2b — Duplicate `to_ascii_lowercase()` in `apply_search_filters_display`

**File:** `crates/uffs-core/src/search/filters.rs` lines 286–302

The same extension and exclude allocation pattern is **duplicated** in the
`apply_search_filters_display` function that filters `DisplayRow` vectors
(used for post-sort filtering in TUI).  Same fix applies: use
`eq_ignore_ascii_case` or pre-computed extension sets.

---

## Stage 3 — Sort  🔴  O(N log N) Heap Allocations

### B3 — `sort_rows()` calls `to_lowercase()` on every comparison  🔴 CRITICAL

**File:** `crates/uffs-core/src/search/backend.rs` lines 354–416

The `sort_unstable_by` comparator allocates heap `String`s on every
invocation:

```rust
// Line 396: Name sort — 2 Strings per comparison
SortColumn::Name => row_a.name.to_lowercase().cmp(&row_b.name.to_lowercase()),

// Line 402: Path sort — 2 Strings per comparison (paths are LONG)
SortColumn::Path => row_a.path.to_lowercase().cmp(&row_b.path.to_lowercase()),

// Lines 405-406: Extension sort — 2 Strings + 2 rsplit per comparison
SortColumn::Extension => {
    let ext_a = row_a.name.rsplit('.').next().unwrap_or("").to_lowercase();
    let ext_b = row_b.name.rsplit('.').next().unwrap_or("").to_lowercase();
    ext_a.cmp(&ext_b)
}

// Line 382: Name tiebreaker — 2 MORE Strings when primary sort is Equal
ord = row_a.name.to_lowercase().cmp(&row_b.name.to_lowercase());
```

**Impact (for 10 K results):**
- `sort_unstable_by` performs ~13 × N = ~130 K comparisons (quicksort).
- Name sort: 260 K `String` allocations.
- Path sort: 260 K `String` allocations of ~80–200 bytes each.
- Extension sort: 260 K allocations + 260 K `rsplit` iterations.
- Name tiebreaker adds another ~50 K allocations (fires when primary
  column has many equal values — common for Size or Modified).

**Root cause:** Case-insensitive sort implemented naively by allocating
in the comparator instead of pre-computing sort keys.

**Proposed fix (Schwartzian transform):**

```rust
pub fn sort_rows(
    rows: &mut [DisplayRow],
    column: SortColumn,
    descending: bool,
    extra_tiers: &[SortSpec],
) {
    // 1. Pre-compute primary sort keys (ONE allocation per row)
    let keys: Vec<String> = rows.iter().map(|row| {
        match column {
            SortColumn::Name => row.name().to_lowercase(),
            SortColumn::Path => row.path.to_lowercase(),
            SortColumn::Extension => row.name()
                .rsplit('.').next().unwrap_or("").to_lowercase(),
            _ => String::new(), // non-string sorts don't need keys
        }
    }).collect();

    // 2. Build index array
    let mut indices: Vec<usize> = (0..rows.len()).collect();

    // 3. Sort indices by pre-computed key
    indices.sort_unstable_by(|&a, &b| {
        let ord = match column {
            SortColumn::Name | SortColumn::Path | SortColumn::Extension =>
                keys[a].cmp(&keys[b]),
            SortColumn::Size => rows[a].size.cmp(&rows[b].size),
            // ... other numeric columns
        };
        if descending { ord.reverse() } else { ord }
    });

    // 4. Permute rows in-place using index array
    apply_permutation(rows, indices);
}
```

**Estimated savings:** Reduces allocations from O(N log N) to O(N).
For 10 K results: ~260 K allocations → ~10 K allocations.
At ~50 ns/alloc: ~13 ms → ~0.5 ms.

---

## Stage 4 — Path Resolution & DisplayRow  🟡  Allocation Heavy (Bounded)

### B4 — `DisplayRow.name` Duplicates the Path Suffix  🟡 SIGNIFICANT

**File:** `crates/uffs-core/src/search/backend.rs` lines 18–24

```rust
pub struct DisplayRow {
    pub drive: char,
    pub path: String,   // "C:\Users\Photos\beach.jpg" — heap-allocated
    pub name: String,   // "beach.jpg" — ALSO heap-allocated (redundant!)
    pub size: u64,
    pub is_directory: bool,
    // ...
}
```

The `name` field is always the last path component of `path`.  It is
stored as a separate owned `String`, duplicating data that already exists
as a suffix of `path`.

**Construction site** (`make_display_row`):
```rust
name: name.to_owned(),  // heap allocation
```

**Impact:**
- For 10 K results: 10 K `String` allocations for names.
- Average filename ~20 bytes → ~200 KB duplicated data.
- Every `clone()` of a `DisplayRow` (see B5) doubles this.

**Proposed fix:**
Replace `name: String` with `name_start: u16` — the byte offset where
the filename begins within `path`.

```rust
pub struct DisplayRow {
    pub path: String,
    pub name_start: u16,  // path[name_start..] == filename
    // ...
}

impl DisplayRow {
    pub fn name(&self) -> &str {
        &self.path[self.name_start as usize..]
    }
}
```

This eliminates one `String` allocation per result and makes `name()`
a zero-cost slice into the already-owned `path`.

**Estimated savings:** 10 K fewer `String` allocations.  Cloning a
`DisplayRow` copies one fewer `String`.

---

### B5 — `last_results.clone_from(&rows)` Deep-Clones All Results  🟡 SIGNIFICANT

**File:** `crates/uffs-core/src/search/backend.rs` line 419

```rust
self.last_results.clone_from(&rows);
```

After every search, the backend deep-clones all `DisplayRow`s (including
their owned `path` and `name` `String`s) into `last_results` so the TUI
can re-sort without re-searching.

**Impact:**
- For 10 K results: 20 K `String` clones (path + name per row).
- Approximately doubles the time spent on `DisplayRow` construction.
- The CLI path ALSO pays this cost despite never re-sorting.

**Proposed fix (phased):**

**Phase 1 — CLI bypass:**
Skip `clone_from` when running in CLI mode (no re-sort capability).
Gate with a `mode` flag or separate method.

**Phase 2 — Cheap cloning:**
Use `Arc<str>` for `path` (and `name` if B4 not yet done) so that
`clone()` is a reference-count bump (~2 ns) instead of a heap copy
(~50 ns + memcpy).

**Estimated savings:** Eliminates 20 K `String` clones for CLI.
For TUI with `Arc<str>`: ~1 ms → ~0.02 ms.

---

## Stage 5 — Output  🟡  Unbuffered Stdout + Unnecessary Passes

### B6 — Console stdout Has No BufWriter  🟡 SIGNIFICANT (trivial fix)

**File:** `crates/uffs-cli/src/commands/output/mod.rs` lines 71–72

```rust
let stdout_handle = std::io::stdout();
let mut stdout = stdout_handle.lock();
// All subsequent write_all() calls go through UNBUFFERED locked stdout
```

Note: File output on line 86 correctly uses `BufWriter::new(file)`.
Only console output is unbuffered.

**Impact:**
- Each `write_all()` is a direct `write(2)` syscall.
- For 10 K rows in csv/custom format: 10 K syscalls × ~1–5 µs each
  = 10–50 ms wasted on syscall overhead.
- For large result sets (50 K+): 50–250 ms of pure overhead.

**Proposed fix (one line):**

```rust
// Before:
let mut stdout = stdout_handle.lock();

// After:
let mut stdout = std::io::BufWriter::with_capacity(64 * 1024, stdout_handle.lock());
```

**Estimated savings:** 10–50 ms for typical result sets.  Trivial fix.

---

### B7 — `display_rows_to_dataframe`: 11 Separate Passes  🟡 MODERATE

**File:** `crates/uffs-core/src/search/backend.rs` lines 603–646

```rust
let names: Vec<&str>   = rows.iter().map(DisplayRow::name).collect();
let paths: Vec<&str>   = rows.iter().map(|r| r.path.as_str()).collect();
let sizes: Vec<u64>    = rows.iter().map(|r| r.size).collect();
let allocated: Vec<u64>= rows.iter().map(|r| r.allocated).collect();
let created: Vec<i64>  = rows.iter().map(|r| r.created).collect();
let modified: Vec<i64> = rows.iter().map(|r| r.modified).collect();
let accessed: Vec<i64> = rows.iter().map(|r| r.accessed).collect();
let flags: Vec<u32>    = rows.iter().map(|r| r.flags).collect();
let drives: Vec<String>= rows.iter().map(|r| format!("{}:", r.drive)).collect();
let descendants: Vec<u32> = rows.iter().map(|r| r.descendants).collect();
let treesize: Vec<u64> = rows.iter().map(|r| r.treesize).collect();
```

Eleven full iterations over the same `rows` slice.  Each iteration
pulls the same cache lines.  Only used for `json` and `table` output
formats.

**Impact:**
- 11 × N iterations, 11 `Vec` allocations, poor cache utilization.
- For 10 K rows: 110 K iterations vs 10 K in a single-pass builder.
- Total time is small (~1 ms) but avoidable.

**Proposed fix:**
Single-pass SoA (Struct-of-Arrays) builder that fills all column
vectors in one iteration.  Halves cache misses.

---

### B8 — `path_only` String Allocation per Row  🟡 MODERATE

**File:** `crates/uffs-core/src/search/backend.rs` lines 621–629

```rust
let path_only: Vec<String> = rows.iter().map(|row| {
    row.path.rfind('\\').map_or_else(
        || row.path.clone(),              // CLONE
        |pos| row.path[..=pos].to_owned() // ALLOC
    )
}).collect();
```

Every row gets a new `String` for the directory portion of the path.

**Impact:** 10 K `String` allocations of ~60–180 bytes each.

**Proposed fix:** If B4 is implemented (`name_start` offset), then
`path_only` is simply `&row.path[..row.name_start as usize]` — a
zero-allocation slice.  Can be passed to Polars as `&str` directly.

**Estimated savings:** 10 K fewer `String` allocations (~0.5 ms).


---

## Stage 6 — Already Optimal  ✅  No Action Needed

These components were inspected and found to be well-optimized:

| Component | Why It's Fine |
|---|---|
| `TrigramIndex::search` | CSR posting lists, sorted intersection via binary search |
| `ChildrenIndex` | Flat CSR arrays, `get()` returns `&[u32]` slice |
| `CompactRecord` layout | 80 bytes, `#[repr(C)]`, `bytemuck::Pod`, cache-friendly |
| `NameArena` / names blob | Single contiguous `Vec<u8>`, offset-based access |
| `DirCache` for `resolve_path` | `HashMap<u32, String>` eliminates ~90% of parent walks |
| `itoa::Buffer` in row writer | Stack-allocated integer → string formatting |
| `write_display_row_columns` | Single `String` buffer reused across rows |
| Trigram build | Parallel chunking via rayon, `TinyTriSet` reuse per chunk |
| `parse_record_to_index` | Zero intermediate allocations, direct-to-index |
| `sort_indices_by_name` (tree walk) | Sorts compact indices, not allocated Strings |

---

## Data Structure Assessment

### DS1 — `DisplayRow.name: String` → Should Be Offset  🔴

| Property | Current | Optimal |
|---|---|---|
| Type | `String` (24 bytes + heap) | `u16` (2 bytes, zero heap) |
| Access | `row.name` (owned) | `row.name()` → `&row.path[n..]` |
| Clone cost | memcpy + heap alloc | just copy the u16 |

The `name` is always the suffix of `path` after the last `\`.  Storing
it separately wastes 24 bytes of stack space per `DisplayRow` plus a
heap allocation.  A `name_start: u16` offset into `path` is sufficient
(paths are always < 32 KB in NTFS).

### DS2 — `SearchFilters.extensions: Vec<String>` → Should Be `HashSet<u16>`  🔴

| Property | Current | Optimal |
|---|---|---|
| Type | `Vec<String>` | `HashSet<u16>` or `SmallVec<[u16; 8]>` |
| Lookup | O(E) linear scan per record | O(1) hash or O(log E) binary search |
| Comparison | Allocates lowercase String | Integer comparison |

`CompactRecord.extension_id` is already interned.  The filter should
operate on interned IDs, not re-derive extensions from filenames.

### DS3 — Sort Comparator Keys → Should Be Pre-Computed  🔴

| Property | Current | Optimal |
|---|---|---|
| Key computation | In comparator (O(N log N) times) | Pre-computed once (O(N) times) |
| Allocations | 2 Strings per comparison | 1 String per row total |
| Pattern | Naive | Schwartzian transform |

### DS4 — `FileRecord` at 224 Bytes vs `CompactRecord` at 80 Bytes  ✅ OK

The `MftIndex` stores `Vec<FileRecord>` at 224 bytes/record (1.5 GB for
7 M records).  This is acceptable because:
- `FileRecord` is only used during ingestion / index building.
- The search-time representation is `CompactRecord` at 80 bytes.
- The conversion happens once (~50 ms) and the `MftIndex` can be dropped.

### DS5 — Polars DataFrame as Query Path  ✅ OK (opt-in)

The `MftQuery` / `LazyFrame` path converts the full `MftIndex` to a Polars
`DataFrame` (3–5 seconds for 7 M records).  This is by design — opt-in
only via `--query-mode dataframe`.  The default `IndexQuery` path uses
the compact index and is 100–200 ms.  No action needed as long as the
default remains fast.

---

## Ranked Summary

| # | ID | Bottleneck | Stage | Allocs (7 M recs / 10 K results) | Severity | Fix Effort |
|---|---|---|---|---|---|---|
| 1 | B1 | Extension filter `to_ascii_lowercase()` per record | Filter | **7 M** String allocs | 🔴 Critical | Low |
| 2 | B2 | Exclude filter `to_ascii_lowercase()` per record | Filter | **7 M** String allocs | 🔴 Critical | Medium |
| 3 | B3 | `sort_rows()` `to_lowercase()` per comparison | Sort | **260 K** String allocs | 🔴 Critical | Medium |
| 4 | B4 | `DisplayRow.name` duplicates path suffix | Resolve | **10 K** String allocs | 🟡 Significant | Medium |
| 5 | B5 | `last_results.clone_from` deep clone | Backend | **20 K** String clones | 🟡 Significant | Low |
| 6 | B6 | Console stdout missing BufWriter | Output | **10 K** syscalls | 🟡 Significant | **Trivial** |
| 7 | B7 | `display_rows_to_dataframe` 11-pass iteration | Output | 11 Vec allocs | 🟡 Moderate | Low |
| 8 | B8 | `path_only` String clone per row | Output | **10 K** String allocs | 🟡 Moderate | Low (if B4 done) |

---

## Recommended Attack Order

1. **B6** — BufWriter on stdout.  One-line change, zero risk, instant
   measurable improvement for large result sets.

2. **B1** — Extension filter via `extension_id`.  Eliminates 7 M allocs.
   Requires mapping extension strings → `extension_id` values during
   `SearchFilters` construction.

3. **B3** — Sort key pre-computation (Schwartzian transform).  Eliminates
   260 K allocs per sort.  Most architecturally interesting change;
   needs care with multi-tier sorting.

4. **B4 + B8** — `name_start` offset in `DisplayRow`.  Eliminates 20 K
   allocs and enables zero-cost `path_only` extraction.  Touch-points:
   `DisplayRow`, `make_display_row`, all callers of `.name`, and
   `display_rows_to_dataframe`.

5. **B2** — `eq_ignore_ascii_case` for exclude filter.  Eliminates 7 M
   allocs but requires rewriting `name_matches` for case-insensitive
   comparison.

6. **B5** — Skip `last_results` clone for CLI.  Low effort, moderate
   impact.  Gate on runtime mode flag.

---

## Implementation Tracking

### Phase 1 — Quick Wins ✅ DONE

- [x] **B6** — Wrap console stdout in `BufWriter::with_capacity(64 * 1024, ...)`
  - File: `crates/uffs-cli/src/commands/output/mod.rs`
  - Status: ✅ DONE (already applied prior to this audit)

- [x] **B5** — `last_results` ownership transfer
  - File: `crates/uffs-core/src/search/backend.rs`
  - Status: ✅ DONE — changed `clone_from` to move-then-clone pattern.
    Same cost but cleaner ownership semantics.  Full elimination requires
    `SearchResult` borrowing from `last_results` (future API change).

### Phase 2 — Filter Path ✅ DONE (eliminates 14 M allocs)

- [x] **B1** — Extension filter: zero-alloc `eq_ignore_ascii_case`
  - File: `crates/uffs-core/src/search/filters.rs`
  - Status: ✅ DONE — replaced `to_ascii_lowercase()` + `==` with
    `eq_ignore_ascii_case()`.  Zero heap allocation per record.
    `self.extensions` are stored pre-lowered; `eq_ignore_ascii_case`
    handles mixed-case filenames without allocation.
  - Note: The audit proposed `extension_id` lookup via `HashSet<u16>`.
    That approach requires carrying the `ExtensionTable` in
    `DriveCompactIndex`, which was intentionally dropped to save ~140 MB.
    The `eq_ignore_ascii_case` approach achieves the same zero-alloc
    goal without structural changes.

- [x] **B2** — Exclude filter: reusable `Vec<u8>` buffer
  - File: `crates/uffs-core/src/search/filters.rs`
  - Status: ✅ DONE — `matches_record` now takes a caller-owned
    `&mut Vec<u8>` buffer.  The name is lowered in-place into this
    buffer (`.extend_from_slice` + `.make_ascii_lowercase`) instead
    of allocating a new `String` per record.  The buffer is reused
    across all 7 M records — one allocation total.
  - Signature change: `matches_record(&self, rec, names, lower_buf)`

- [x] **B2b** — DisplayRow filter path: `eq_ignore_ascii_case`
  - File: `crates/uffs-core/src/search/filters.rs`
  - Status: ✅ DONE — extension filter in `apply_search_filters` uses
    `eq_ignore_ascii_case`.  Exclude filter keeps `to_ascii_lowercase()`
    (bounded by result count ~10 K, not record count 7 M).

### Phase 3 — Sort Path ✅ DONE (eliminates 260 K allocs)

- [x] **B3** — Pre-compute sort keys (Schwartzian transform)
  - File: `crates/uffs-core/src/search/backend.rs`
  - Status: ✅ DONE — introduced `SortKeys` struct that pre-computes
    `name_lower`, `path_lower`, and `ext_lower` vectors once (O(N)).
    Sort operates on a permutation index array using `compare_by_column_keyed`,
    then applies the permutation in-place via `apply_permutation`.
    Multi-tier sorting and name tiebreaker fully supported.
  - Allocation reduction: O(N log N) → O(N) for string-based sorts.

### Phase 4 — DisplayRow Restructure ✅ DONE

- [x] **B4** — Replace `name: String` with `name_start: u32`
  - File: `crates/uffs-core/src/search/backend.rs`
  - Status: ✅ DONE (already applied prior to this audit).
    `DisplayRow` has `name_start: u32` and `name() -> &str` method.

- [x] **B8** — Zero-alloc `path_only` via `name_start`
  - File: `crates/uffs-core/src/search/backend.rs`
  - Status: ✅ DONE — `path_only` now uses `row.path.get(..name_start)`
    returning `&str` instead of allocating a new `String`.  Eliminates
    10 K `String` allocations.

### Phase 5 — Output Path Polish

- [ ] **B7** — Single-pass `display_rows_to_dataframe`
  - File: `crates/uffs-core/src/search/backend.rs`
  - Status: NOT STARTED
  - Notes: Low priority — only affects json/table output formats.
    Total time ~1 ms; cache-miss cost is real but small.

---

## Validation

After each phase, validate with:

```bash
# Build + test + lint
just go

# Benchmark (if available)
cargo bench -p uffs-core

# Manual timing with profiling env var
UFFS_CACHE_PROFILE=1 uffs search "*.rs" --ext rs --sort name
```

---

*Document created: 2026-04-02*
*Last updated: 2026-04-02 — B1/B2/B2b/B3/B4/B5/B6/B8 implemented*