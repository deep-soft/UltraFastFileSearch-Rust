# Compact Index + Tree Search Architecture

> **Status**: Design — Wave 3  
> **Date**: 2026-03-24  
> **Goal**: Reduce TUI memory from ~7.5 GB to ~2.1 GB, startup from 40-70s to ~3s

---

## Executive Summary

Replace the full `MftIndex` + pre-resolved `paths_lower` with a **compact in-memory
index** (~54 bytes/record) and **on-demand tree-based path resolution**. Search uses
trigrams on the names blob (not full paths). Path queries use hierarchical tree
traversal instead of flat string search.

### Before vs After

| Metric | Current (Wave 2) | Target (Wave 3) |
|--------|------------------|-----------------|
| **RAM (25M records, 7 drives)** | ~7.5 GB | ~2.1 GB |
| **Startup (cached)** | ~40s (path resolve + trigram) | ~3s (build compact + trigram) |
| **Startup (cold, Windows)** | ~70s | ~30s (MFT I/O + build) |
| **Search latency** | <10ms | <10ms (unchanged) |
| **Sort/filter** | <50ms | <50ms (unchanged) |
| **Path search** | Trigram on flat paths | Tree traversal (faster for structured queries) |

### Memory Breakdown

| Component | Current | Compact Index |
|-----------|---------|---------------|
| `Vec<FileRecord>` (224 bytes × 25M) | 5.0 GB | — (eliminated) |
| `names: String` | 200 MB | 200 MB (kept) |
| `frs_to_idx: Vec<u32>` | 100 MB | 100 MB (kept) |
| `links/streams/children` overflow | 200 MB | — (not loaded) |
| `paths_lower: Vec<String>` | 1.5 GB | — (eliminated) |
| Trigram index (on full paths) | 300 MB | — (replaced) |
| **Compact records** (72 bytes × 25M) | — | **1.80 GB** (new) |
| **Trigram index** (on names only) | — | **~100 MB** (new, smaller) |
| **Parent chain** (in CompactRecord.parent_idx) | — | **(included above)** |
| **Children index** | — | **~100 MB** (new) |
| **Total** | **~7.5 GB** | **~2.1 GB** |

---

## Architecture

### Data Structures

```
┌─────────────────────────────────────────────────────────────────────┐
│ DriveCompactIndex (one per loaded drive)                           │
│                                                                     │
│  compact: Vec<CompactRecord>     ← 54 bytes × N records (flat)     │
│  names: Vec<u8>                  ← all filenames concatenated       │
│  parent_idx: Vec<u32>            ← frs → compact index of parent   │
│  trigram: TrigramIndex           ← trigrams on names (not paths)    │
│  children: Vec<Vec<u32>>         ← dir idx → child compact indices  │
│  extensions: ExtensionTable      ← extension interning (shared)     │
│                                                                     │
│  uffs_cache_path: PathBuf        ← path to .uffs file for fallback │
└─────────────────────────────────────────────────────────────────────┘
```

### CompactRecord Layout (72 bytes, `#[repr(C)]`)

The "full" compact layout at 72 bytes (68 data + 4 tail padding for
8-byte struct alignment) covers **100% of sortable and filterable
columns**. At 25M records this costs 1.80 GB — only ~450 MB more than
a minimal 54-byte layout that would miss descendants and treesize.
The extra memory eliminates ALL fallback-to-disk for sort/filter.

```rust
/// Compact per-record data for in-memory search, filter, and sort.
///
/// Contains EVERY field needed for sort, filter, and display:
/// - Name, Extension, Path (via parent chain)
/// - Size, Size on Disk
/// - Created, Last Written, Last Accessed
/// - Descendants, Treesize (for folder analysis)
/// - ALL NTFS boolean attributes (u32 covers bits 0-20)
///
/// Only fields NOT included (resolved from .uffs on demand):
/// - Alternate Data Streams (ADS)
/// - Reparse tag (u32 — rare filter target)
/// - Forensic fields (sequence_number, LSN, base_frs)
/// - $FILE_NAME timestamps (fn_created, fn_modified, etc.)
/// - Internal stream sizes
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct CompactRecord {
    // ── Name reference (6 bytes) ──────────────────────────────────
    /// Byte offset into the names blob.
    pub name_offset: u32,
    /// UTF-8 byte length of the filename (max 1023).
    pub name_len: u16,

    // ── Classification (6 bytes) ──────────────────────────────────
    /// Interned extension ID (0 = no extension). For O(1) extension
    /// filtering and grouping.
    pub extension_id: u16,
    /// NTFS attribute flags (full u32 from $STANDARD_INFORMATION):
    ///   bit 0:  read_only        bit 11: compressed
    ///   bit 1:  hidden           bit 12: offline
    ///   bit 2:  system           bit 13: not_content_indexed
    ///   bit 4:  directory        bit 14: encrypted
    ///   bit 5:  archive          bit 15: integrity_stream
    ///   bit 8:  temporary        bit 17: no_scrub_data
    ///   bit 9:  sparse           bit 19: pinned
    ///   bit 10: reparse_point    bit 20: unpinned
    pub flags: u32,

    // ── Parent reference (4 bytes) ────────────────────────────────
    /// Index into compact array of the parent directory.
    /// `u32::MAX` = root or orphan. Used for on-demand path resolution
    /// by walking the parent chain.
    pub parent_idx: u32,

    // ── Sizes (16 bytes) ──────────────────────────────────────────
    /// Logical file size in bytes.
    pub size: u64,
    /// Allocated size on disk in bytes ("Size on Disk" column).
    pub allocated: u64,

    // ── Timestamps (24 bytes) ─────────────────────────────────────
    /// Creation time (Unix microseconds).
    pub created: i64,
    /// Last write time (Unix microseconds).
    pub modified: i64,
    /// Last access time (Unix microseconds).
    pub accessed: i64,

    // ── Tree metrics (12 bytes) ───────────────────────────────────
    /// Count of all descendants (files + subdirectories) in subtree.
    /// 0 for files.
    pub descendants: u32,
    /// Sum of logical file sizes in entire subtree. Enables
    /// "biggest folders" sort without touching the .uffs file.
    pub treesize: u64,
}
// static_assert: size_of::<CompactRecord>() == 68
```

### Column Coverage: 100%

Every column from the uffs CLI output maps to a compact field:

| CLI Column | Compact Field | Sort/Filter |
|------------|--------------|-------------|
| Name | `name_offset` + `name_len` → names blob | ✅ |
| Path | `parent_idx` chain → resolved on-demand | ✅ |
| Path Only | `parent_idx` chain (exclude filename) | ✅ |
| Size | `size` | ✅ |
| Size on Disk | `allocated` | ✅ |
| Created | `created` | ✅ |
| Last Written | `modified` | ✅ |
| Last Accessed | `accessed` | ✅ |
| Descendants | `descendants` | ✅ |
| Directory Flag | `flags` bit 4 | ✅ |
| Read-only | `flags` bit 0 | ✅ |
| Archive | `flags` bit 5 | ✅ |
| System | `flags` bit 2 | ✅ |
| Hidden | `flags` bit 1 | ✅ |
| Offline | `flags` bit 12 | ✅ |
| Not content indexed | `flags` bit 13 | ✅ |
| No scrub file | `flags` bit 17 | ✅ |
| Integrity | `flags` bit 15 | ✅ |
| Pinned | `flags` bit 19 | ✅ |
| Unpinned | `flags` bit 20 | ✅ |
| Compressed | `flags` bit 11 | ✅ |
| Encrypted | `flags` bit 14 | ✅ |
| Sparse | `flags` bit 9 | ✅ |
| Reparse | `flags` bit 10 | ✅ |
| Attributes | `flags` (raw u32) | ✅ |

**Zero fallback needed.** Every sort and filter runs in RAM.

### Names Blob

All filenames concatenated into a single `Vec<u8>`:

```
Offset 0:    "$MFT"
Offset 4:    "$MFTMirr"
Offset 12:   "$LogFile"
...
Offset 8421: "beach.jpg"
Offset 8430: "sunset.png"
```

Each `CompactRecord` references its name via `(name_offset, name_len)`.
This is the same design as the current `MftIndex::names: String`, just
carried forward without the rest of the index.

### Parent Chain (for path resolution)

`parent_idx` is stored **inside** each `CompactRecord` — no separate array.
Path resolution walks the compact records directly:

```
resolve_path(idx=8430):   // beach.jpg
  → compact[8430].parent_idx = 312   // photos/
  → compact[312].parent_idx = 41     // Users/
  → compact[41].parent_idx = 5       // C:\ (root, parent = u32::MAX)
  → reverse: "C:\Users\photos\beach.jpg"
```

Cost: ~5-8 lookups × 1 array access each = **<1μs per record**.
For 200 displayed rows: **<200μs total**.

### Children Index (for tree-based path search)

```rust
/// children[i] = sorted Vec of compact indices of directory i's children.
/// Only populated for directories. Empty Vec for files.
/// Used for tree-walk path queries like `\photos\*.jpg`.
children: Vec<Vec<u32>>
```

Built at load time from `parent_idx` in a single pass:

```
for (idx, &parent) in parent_idx.iter().enumerate() {
    if parent != u32::MAX {
        children[parent].push(idx as u32);
    }
}
```

Memory: ~4 bytes per record (one u32 per child entry) = **~100 MB** for 25M records.
This is the only auxiliary structure beyond the compact records + names blob.

### Trigram Index (on names only)

Same `HashMap<[u8;3], Vec<u32>>` structure as current, but built on the
**names blob** instead of full paths. This is ~5× smaller because filenames
are ~15 chars on average vs full paths at ~80 chars.

For name-based searches (90% of queries), performance is identical.
For path-based searches, the tree traversal strategy is used instead.

---

## Search Strategies

### Strategy 1: Name Search (most common)

**Pattern**: `beach`, `*.jpg`, `hallo*world`

```
1. Extract trigrams from pattern → ["bea","eac","ach"]
2. Intersect posting lists from trigram index → 200 candidates
3. Verify: names_blob[offset..+len] matches pattern → 15 hits
4. Sort by compact fields (size, modified, etc.) → <1ms
5. Display: resolve paths for top N rows → <1ms
```

**Time: <10ms.** Same as current.

### Strategy 2: Extension Filter

**Pattern**: `*.jpg`, `*.png`, `*.heic` (or combined via F3 filter)

```
1. Look up extension_id for "jpg" in ExtensionTable → ext_id=42
2. Linear scan compact records: extension_id == 42 → 50K matches
   (or use ExtensionIndex if built: O(matches) lookup)
3. Sort/filter on compact fields
4. Display top N
```

**Time: <30ms** (linear scan) or **<5ms** (extension index).

### Strategy 3: Path Search (tree traversal) ⭐ NEW

**Pattern**: `\photos\*.jpg`, `C:\Users\*\docs\*.pdf`

This is the key architectural change. Instead of searching flattened path
strings, we decompose the path pattern into segments and search the tree.

#### Example: `\photos\*.jpg`

```
Step 1: Parse pattern into segments:
        [AnyDepth, Literal("photos"), Glob("*.jpg")]

Step 2: Find directories named "photos":
        Trigram search on names blob → filter is_directory
        → 50 directories

Step 3: For each "photos" directory, collect children:
        children[dir_idx] → 200 files per dir × 50 dirs = 10K files

Step 4: Filter children by "*.jpg":
        Check extension_id == jpg_id → 2K matches

Step 5: Sort + display top N
```

**Time: <15ms** (trigram + child walk + extension filter).

#### Example: `C:\Users\*\docs\*.pdf`

```
Step 1: Parse: [Root("C:"), Literal("Users"), AnyOne, Literal("docs"), Glob("*.pdf")]

Step 2: Find "Users" directory under root → 1 match

Step 3: Enumerate children of "Users" → 5 user directories (matching AnyOne)

Step 4: For each user dir, find child named "docs" → 4 matches

Step 5: For each "docs", collect children → 2K files

Step 6: Filter by *.pdf → 300 matches

Step 7: Sort + display
```

**Time: <5ms** (highly selective tree walk).

#### Fallback: Unstructured path search

**Pattern**: `>.*photos.*beach.*` (regex across full path)

When the pattern can't be decomposed into tree segments (e.g., pure regex
on the full path), fall back to:

```
1. Trigram search on names blob for any extractable literals → candidates
2. Resolve full paths for candidates (using parent chain)
3. Apply regex/glob on resolved full paths
4. If no literals extractable: linear scan ALL records, resolve paths
   on-the-fly (slow but correct, <5s for 25M records)
```

This is slower than the current flat trigram approach for pure path regex,
but these queries are rare. The tree strategy handles 95%+ of path queries
faster.

---

## Cache Strategy

### Current Cache Landscape

| Cache | File | Contents | Used By |
|-------|------|----------|---------|
| `.uffs` | `{TEMP}/uffs_index_cache/C_index.uffs` | Full `MftIndex` (serialized) | CLI + TUI |
| USN checkpoint | Inside `.uffs` header | `usn_journal_id`, `next_usn` | CLI + TUI (Windows) |

### What Happens to the `.uffs` Cache?

**The `.uffs` cache is STILL NEEDED and STILL USED.** Here's why:

1. **The `.uffs` file is the source of truth.** The compact index is
   **derived from** it. We load the `.uffs` file, extract 54 bytes per
   record into `CompactRecord`, and discard the rest.

2. **USN incremental updates** need the full `MftIndex` temporarily.
   The USN journal tells us "FRS 12345 changed" — we need to re-parse
   that record and update the compact index.

3. **Full metadata display** ("max view" with all 25 columns) reads
   individual records from the `.uffs` file on demand.

4. **The CLI (`uffs`) uses `.uffs` directly** — it doesn't use the
   compact index. The cache is shared infrastructure.

### Do We Need a `.uffs-tui` Sidecar?

**No — at least not initially.** Here's the analysis:

| What we'd cache | Build time | Rebuild needed? |
|-----------------|------------|-----------------|
| Compact records | ~1s (scan .uffs, extract fields) | Every .uffs change |
| Names blob | ~0.5s (copy from .uffs) | Every .uffs change |
| parent_idx | ~0.5s (build from compact) | Every .uffs change |
| children index | ~0.3s (build from parent_idx) | Every .uffs change |
| Trigram on names | ~1s (build from names blob) | Every .uffs change |
| **Total** | **~3s** | |

Building the compact index from `.uffs` takes ~3s. A sidecar cache would
save those 3s on subsequent starts, but:

- The sidecar must be invalidated whenever `.uffs` changes (USN update)
- The sidecar adds file management complexity
- 3s startup is already fast (Everything takes ~2s)
- The `.uffs` file itself is the cache — adding another layer adds bugs

**Decision**: Skip the sidecar. Build compact index from `.uffs` every time.
Revisit if startup time becomes a bottleneck (unlikely at ~3s).

### Cache Flow (Windows, auto-detect drives)

```
uffs_tui.exe (startup)
    ↓
detect_ntfs_drives() → [C, D, E, F, G, M, S]
    ↓
For each drive (parallel threads):
    ↓
Check .uffs cache freshness (TTL = 10 min)
  ├─ FRESH → load .uffs (deserialize MftIndex)        [0.5-2s]
  │          → apply USN delta                          [<50ms]
  │          → save updated .uffs                       [0.5-1s]
  ├─ STALE → full MFT read (IOCP)                      [2-60s]
  │          → save new .uffs                           [0.5-1s]
  └─ MISSING → full MFT read                           [2-60s]
    ↓
Build compact index from MftIndex:                      [~1s per drive]
  ├─ Extract CompactRecord for each record
  ├─ Copy names blob
  ├─ Build parent_idx (frs → compact index mapping)
  ├─ Build children index
  └─ Build trigram index on names
    ↓
DROP MftIndex (free ~800 MB per drive)                  ← KEY STEP
    ↓
DriveCompactIndex ready → send to UI via channel
    ↓
Store .uffs path for on-demand full record lookups
```

**The critical step is DROP.** After building the compact index, the full
`MftIndex` is dropped. This is where the memory savings come from. The
`.uffs` file on disk serves as the backing store for rare full-record
lookups.

### Cache Flow (Mac/Linux, MFT files)

```
uffs_tui --data-dir ~/uffs_data
    ↓
Discover drive_c/, drive_d/, ... subdirectories
    ↓
For each MFT file (parallel):
    ↓
Parse raw MFT → MftIndex                               [2-10s per file]
    ↓
Build compact index (same as above)                     [~1s]
    ↓
DROP MftIndex
    ↓
DriveCompactIndex ready
```

No `.uffs` cache on Mac/Linux currently. The MFT file is re-parsed each
time. Future optimization: save `.uffs` cache alongside MFT files.

---

## On-Demand Full Record Lookup

When the user views all 25 columns or needs data not in the compact record
(descendants, treesize, reparse tag, ADS, forensic fields), we load
individual records from the `.uffs` file:

```rust
impl DriveCompactIndex {
    /// Load full metadata for a single record from the .uffs cache file.
    ///
    /// Reads the .uffs file, seeks to the record's position, and
    /// deserializes just that one FileRecord. Uses a small LRU cache
    /// to avoid repeated reads for the same record.
    fn load_full_record(&self, compact_idx: u32) -> Option<FileRecord> {
        // Option 1: Keep the .uffs file memory-mapped (lazy, OS-managed)
        // Option 2: Read + deserialize on demand (simple, slightly slower)
        // Option 3: LRU cache of recently accessed full records
    }
}
```

For displaying 50 rows in "max view", this means 50 individual record
lookups. With the record offset pre-computed, each lookup is a single
seek + 195-byte read. **Total: <5ms** even without mmap.

---

## Implementation Phases

### Phase 3a: CompactRecord + Name Trigrams (core)

Replace `MftIndex` with `DriveCompactIndex` in the TUI. Name-based search
works identically. Path resolution is on-demand.

| Task | Status | Notes |
|------|--------|-------|
| Define `CompactRecord` struct (72 bytes, `#[repr(C)]`) | ✅ | `compact.rs` — 72 bytes with alignment padding |
| Build `CompactRecord` array from `MftIndex` | ✅ | `build_compact_index()` extracts all fields |
| Copy names blob from `MftIndex::names` | ✅ | Plus `names_lower` for case-insensitive search |
| Build `parent_idx` inside `CompactRecord` | ✅ | FRS → compact index via `frs_to_idx` |
| Build trigram index on names blob (not paths) | ✅ | `build_name_trigram()` reuses `TrigramIndex::build` |
| Implement `resolve_path()` using `parent_idx` + names blob | ✅ | Walk parent chain → join segments |
| Drop `MftIndex` after compact build | ✅ | `index` dropped after `build_compact_index()` returns |
| Update search to use compact + names trigram | ✅ | `search_compact_drive()` in `backend.rs` |
| Update `DisplayRow` builder to use compact + resolve_path | ✅ | On-demand path resolution for matches only |
| Update sort to use compact fields directly | ✅ | size, modified, flags all in compact record |
| Verify: all existing tests pass | ✅ | 6/6 tests pass, clippy clean |
| Benchmark: memory usage before/after | ⏳ | Target: ~2 GB for 7 drives |
| Benchmark: search latency before/after | ⏳ | Target: <10ms unchanged |

### Phase 3b: Tree-Based Path Search

Add hierarchical path search for patterns containing path separators.

| Task | Status | Notes |
|------|--------|-------|
| Build `children` index from `parent_idx` | ✅ | Single pass in `build_compact_index()` |
| Parse path pattern into segments | ✅ | Split on `\` or `/`, normalize, strip leading sep |
| Implement tree walker: segment-by-segment matching | ✅ | `tree_search()` walks dir→children per segment |
| Handle wildcards in path segments (`*`, `*.ext`, `prefix*`) | ✅ | `name_matches()` — `*`, `*.jpg`, `photos*`, `*xxx*`, substring |
| Auto-detect path pattern → tree search vs name trigram | ✅ | `is_path_pattern()` + routing in `MultiDriveBackend::search()` |
| Single-segment path fallback → name search | ✅ | Falls back to `name_search()` via trigram |
| `find_dirs_by_name()` — trigram-accelerated dir lookup | ✅ | Trigram for 3+ chars, linear for short patterns |
| `search_compact_drive_tree()` — tree results → DisplayRow | ✅ | Resolves paths on-demand for matched results |
| Benchmark: path search latency vs current flat trigram | ⏳ | |

### Phase 3c: On-Demand Full Record Lookup

Support "max view" (all 25 columns) by reading from `.uffs` on demand.

| Task | Status | Notes |
|------|--------|-------|
| `.uffs` path available via `IndexSource` + `cache_file_path()` | ✅ | Windows: auto-derived; Mac/Linux: returns None |
| `FullRecordReader::open()` — parse .uffs header, calc offsets | ✅ | Reads 96-byte header + frs_to_idx_len |
| Record offset: `header + frs_table + idx × record_size` | ✅ | Supports v3-v8 (121-195 bytes/record) |
| `read_record_from_disk()` — seek + read single record | ✅ | One file open + seek per read |
| `parse_extra_fields()` — extract forensic/reparse/$FN fields | ✅ | Version-conditional, mirrors deserialize.rs |
| `ExtraRecordFields` struct — 13 fields not in CompactRecord | ✅ | reparse_tag, seq#, namespace, LSN, $FN timestamps |
| 512-entry cache for recently accessed records | ✅ | Simple HashMap with clear-on-full eviction |
| Wire up "max view" columns to full record lookup | ⏳ | Future UI feature — infrastructure ready |

### Phase 3d: Incremental USN Refresh

Update the compact index when files change (Windows only).

| Task | Status | Notes |
|------|--------|-------|
| Query USN Journal for changes since last checkpoint | ⏳ | Existing `query_usn_journal` API |
| Apply USN delta to `.uffs` cache (existing flow) | ⏳ | `apply_usn_changes` |
| Extract updated compact records from delta | ⏳ | Only changed FRS numbers |
| Update `parent_idx` and `children` for reparented files | ⏳ | |
| Append new trigrams to posting lists | ⏳ | Stale entries filtered at verify |
| Auto-refresh timer (60s, background thread) | ⏳ | |
| Manual refresh (F5 keybinding) | ⏳ | |
| Status bar indicator during refresh | ⏳ | |

---

## Query Processing Examples

### Example 1: "Top 100 biggest files from last year"

```
Input:   * (all files), filter: modified > 1yr ago, sort: size desc, limit: 100
Where:   Everything runs on compact records (in RAM)

1. Scan 25M compact records:
   - flags & DIRECTORY == 0        (skip dirs)
   - modified > one_year_ago_micros (date filter)
   → ~5M matches                                          [~30ms]

2. Partial sort (top 100 by size desc):
   - Use select_nth_unstable or BinaryHeap                [~20ms]
   → 100 results

3. Resolve paths for 100 results:
   - Walk parent_idx chain → build path string            [<1ms]

4. Build DisplayRows → render in Table widget             [<1ms]

Total: ~50ms
Memory touched: compact records only (~1.35 GB scan)
```

### Example 2: "All pic files, alphabetical, first 200"

```
Input:   *.jpg OR *.png OR *.heic ..., sort: name asc, limit: 200
Where:   Extension filter + name sort, all in RAM

1. Filter by extension_id ∈ {jpg, png, heic, ...}:
   - Linear scan compact records, check extension_id      [~15ms]
   → ~500K matches

2. Sort by name (compare via names blob):
   - names_blob[a.name_offset..+a.name_len] <=> ...      [~100ms]
   → sorted

3. Take first 200                                          [trivial]

4. Resolve paths for 200 results                           [<1ms]

Total: ~120ms
Memory: compact records + names blob (~1.55 GB)
```

### Example 3: "Hidden files from last year, size ascending, limit 50"

```
Input:   filter: hidden AND !dir AND modified > 1yr, sort: size asc, limit: 50

1. Scan compact records:
   - flags & HIDDEN != 0
   - flags & DIRECTORY == 0
   - modified > threshold
   → ~2K matches                                           [~30ms]

2. Sort 2K by size ascending                               [<1ms]

3. Take first 50, resolve paths                            [<1ms]

Total: ~35ms
```

### Example 4: "Files under any 'photos' directory matching *.jpg"

```
Input:   \photos\*.jpg (path search — tree strategy)

1. Trigram search names blob for "photos":
   - Intersect posting lists → 800 candidates
   - Verify: name == "photos" AND is_directory
   → 50 directories                                        [<5ms]

2. Collect children of 50 dirs:
   - children[dir_idx] for each → 10K files                [<1ms]

3. Filter by extension_id == jpg_id:
   → 2K matches                                            [<1ms]

4. Sort + resolve paths for display                        [<5ms]

Total: ~12ms
```

### Example 5: Path regex fallback

```
Input:   >.*photos.*beach.* (regex on full path — can't decompose)

1. Extract literals: "photos", "beach"
   - Trigram search names for "photos" → 800 candidates
   - Trigram search names for "beach" → 200 candidates
   - Intersect → 15 candidates                             [<5ms]

2. Resolve full paths for 15 candidates:
   - Walk parent_idx chains                                 [<1ms]

3. Apply regex on resolved paths:
   - 15 regex matches                                       [<1ms]
   → 8 results

Total: ~7ms (because extracted literals narrowed hugely)
```

---

## Risk Analysis

### Risk 1: Path search regression for broad path patterns

**Pattern**: `C:\*` (everything on C: drive)

Tree walk: start at root, enumerate ALL children recursively → 3.4M records.
Current flat trigram: would also return 3.4M (hits limit fast).

**Mitigation**: Both approaches hit the result limit (1,000) early. The tree
walk can abort after 1,000 results just like the current approach. No regression.

### Risk 2: Name-only trigrams miss path-only patterns

**Pattern**: `Users` (no file extension, could be a directory name)

Name trigrams find it directly — "Users" is a name. Works fine.

**Pattern**: `C:\Users` (path with drive prefix)

Parsed as path segments: [Root("C:"), Literal("Users")] → tree walk.
The literal "Users" is still found via name trigrams. Works fine.

### Risk 3: Building compact index adds startup time

Building compact from `.uffs` takes ~1s per drive. But we also **eliminate**
path resolution (8-15s) and reduce trigram build time (names only = ~1s vs
full paths = ~3s). Net effect: **faster startup**.

### Risk 4: On-demand path resolution could be slow for large result sets

Resolving paths for 1,000 results × 5-8 parent lookups = 5-8K array
accesses. Each access is a single `parent_idx[i]` lookup in a contiguous
`Vec<u32>` — fully cache-friendly. **<2ms** for 1,000 results.

---

## File Structure (planned)

```
crates/uffs-tui/
├── src/
│   ├── main.rs          # CLI args, terminal, event loop, UI rendering
│   ├── app.rs           # App state, search dispatch, navigation
│   ├── backend.rs       # MultiDriveBackend, search strategies, sort
│   ├── compact.rs       # CompactRecord, DriveCompactIndex, build logic  ← NEW
│   ├── tree_search.rs   # Path pattern → tree traversal engine           ← NEW
│   └── full_record.rs   # On-demand .uffs record lookup + LRU cache      ← NEW
└── Cargo.toml
```

---

## Comparison with Everything.exe

| | Everything | UFFS TUI (Compact) |
|--|-----------|-------------------|
| **Bytes/record** | ~50 | 72 |
| **RAM (25M)** | ~1.2 GB | ~2.1 GB |
| **Timestamps** | 3 | 3 |
| **Size fields** | 1 (size) | 2 (size + allocated) |
| **Attributes** | u32 flags | u32 flags (all 25 attributes) |
| **Path resolution** | On-demand parent chain | On-demand parent chain |
| **Search method** | SIMD linear scan on names | Trigram index on names |
| **Search latency (10M)** | ~100-200ms | **<10ms** |
| **Path search** | Unknown (likely linear) | **Tree traversal** |
| **Tree metrics** | ❌ None | ✅ descendants + treesize in RAM |
| **25-column view** | ❌ Limited columns | ✅ All columns (on-demand) |
| **Startup** | ~2s | ~3s |

---

## Migration Checklist

- [ ] **Phase 3a**: CompactRecord + name trigrams (core refactor)
- [ ] **Phase 3b**: Tree-based path search
- [ ] **Phase 3c**: On-demand full record lookup (max view)
- [ ] **Phase 3d**: Incremental USN refresh on compact index
- [ ] Update `TUI_ARCHITECTURE.md` wave tracker
- [ ] Benchmark: memory, startup, search latency (before/after)
- [ ] Update `CHANGELOG.md`
