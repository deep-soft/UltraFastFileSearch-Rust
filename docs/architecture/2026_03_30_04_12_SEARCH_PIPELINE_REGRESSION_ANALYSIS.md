# Search Pipeline Refactor Regression — Root Cause Analysis

**Date:** 2026-03-30
**Last working version:** v0.4.28 (commit `4442b0b13`)
**Broken version:** v0.4.30 (commit `a98acce10`)
**Commits between:** 1 (the "search pipeline refactor")
**Lines deleted:** 3,973
**Lines added:** 220

## 1. Executive Summary

The v0.4.30 refactor replaced the proven **streaming output pipeline** with a
**compact search pipeline**. The refactor deleted 10 source files containing the
streaming path without validating that the compact path produces byte-identical
output. The result: 8 missing rows and 9 directory descendants mismatches in the
G-drive offline parity test.

## 2. What Worked at v0.4.28

### 2.1 Streaming Output Pipeline (the deleted path)

For `--mft-file` queries, v0.4.28 used this pipeline:

```
CLI --mft-file
  → dispatch.rs: run_single_file_dispatch()
    → single_file.rs: run_single_file_streaming()
      → raw_io.rs: load_index_from_mft_file()
        → IOCP detection: load_iocp_to_index()    ← uses process_record (unified.rs)
        → Raw MFT: load_raw_to_index_with_options() ← uses parse_record_full
      → streaming_io.rs: write_streaming_output()
        → output/row_writer.rs: write_index_streaming_with_filter()
          → Iterates ALL MftIndex.records
          → For each record: name_count × stream_count expansion
          → Inline path resolution via PathResolver
          → Direct row writing to BufWriter
```

**Key properties of this path:**
- IOCP files detected and loaded via `load_iocp_to_index()` (uses `process_record`/`unified.rs`)
- Records iterated in FRS order (deterministic)
- `name_count × stream_count` expansion inline (via `get_name_at()` linked list walk)
- Path resolution via `PathResolver.materialize_path_into()` + `materialize_path_for_name()`
- Tree metrics from `record.tree_metrics()` for stream_idx == 0
- Zero DataFrame creation — rows written directly to output

### 2.2 Parity Results at v0.4.28

LIVE Windows test against C++ baseline:
- ✅ D, E, G, M, S drives: PASS (sorted/superset match)
- ❌ C, F: MISMATCH (LIVE timing differences — expected)
- G drive: **SUPERSET MATCH** (15059 C++, 15071 Rust, +6 verified hardlinks)

## 3. What Changed in v0.4.30

### 3.1 Deleted Files (streaming pipeline)

| File | Lines | Purpose |
|------|-------|---------|
| `output/row_writer.rs` | 631 | Core streaming writer with name×stream expansion |
| `output/streaming.rs` | 305 | Multi-drive streaming writer (DisplayRow batches) |
| `output/filter.rs` | 286 | Record filter matching, sort comparison |
| `output/types.rs` | 290 | StreamingRecordFilter, attribute/age/sort types |
| `search/streaming_io.rs` | 290 | I/O helpers for streaming output |
| `search/streaming.rs` | 174 | Streaming search module |
| `search/single_file.rs` | 178 | Single-file MFT streaming search |
| `search/mft_file.rs` | 174 | Multi-file MFT streaming search |
| `search/util.rs` | 296 | Extension index helpers, drive inference |

### 3.2 New Dispatch Routing

```rust
// dispatch.rs lines 26-41 (NEW — routes ALL --mft-file through compact)
if !config.mft_file.is_empty() && config.query_mode != "dataframe" {
    let sources = config.mft_file.iter()
        .map(|path| MftSource::File(path.clone(), drive))
        .collect();
    let rows = search_from_sources(sources, &config.filters, config.no_cache).await?;
    return Ok(SearchDispatchResult::NativeRows(rows));
}
```

This replaced the old dispatch that checked `can_write_native_results()` and
routed to `run_single_file_dispatch()`.

### 3.3 New Pipeline

```
CLI --mft-file
  → dispatch.rs: search_from_sources(MftSource::File)
    → drive_search.rs: search_native_compact()
      → compact.rs: load_drive(source)
        → load_mft_index_from_file()           ← NO IOCP detection!
          → load_raw_to_index_with_options()    ← treats IOCP as raw MFT
        → build_compact_index()                 ← builds CompactRecords
          → expand_hardlinks()                  ← walks linked list for extras
      → raw_io_windows.rs: search_compact()
        → MultiDriveBackend::search("*")
          → collect_global_top_n()              ← has sort + limit behavior
            → DisplayRow output
```



## 4. Confirmed Issues

### 4.1 IOCP File Detection Lost (CRITICAL)

**Old path:** `raw_io.rs` line 67-68 explicitly detected IOCP files and called
`load_iocp_to_index()` which uses `process_record` (unified.rs parser).

**New path:** `compact.rs` → `load_mft_index_from_file()` always calls
`load_raw_to_index_with_options()` which uses `parse_record_full()` +
`MftRecordMerger`. **No IOCP format detection.**

**Impact:** IOCP capture files (`.iocp`) are parsed as raw MFT binary data.
The IOCP format has a different header structure (magic bytes, chunk headers,
FRS offsets). Parsing IOCP as raw MFT produces a corrupted `MftIndex` with
missing/wrong records.

**This alone explains the 8 missing rows and 9 descendants mismatches.**

### 4.2 Sort/Limit Behavior in Compact Search (MODERATE)

The old streaming path iterated records in **FRS order** with no limit.
The new compact path uses `collect_global_top_n()` which:
- Applies a sort column (default: `Modified` desc)
- Uses a binary heap with a limit (set to `u32::MAX` in parity mode)

While `u32::MAX` should not truncate results, the sort behavior changes output
order. The parity comparison tool does sorted comparison, so order shouldn't
matter — but if the sort introduces off-by-one or truncation bugs, rows drop.

### 4.3 expand_hardlinks Not Validated Against get_name_at (LOW-MODERATE)

The old streaming path used `index.get_name_at(record, name_idx)` which walked
the `first_name.next_entry` linked list. The compact path's `expand_hardlinks`
also walks the same linked list. The logic APPEARS equivalent but **has never
been validated with a unit test.**

Edge cases to check:
- Records with `name_count > 1` but broken linked list (`next_entry = NO_ENTRY`)
- Extension record names merged after base record link-list construction
- `name_count` set by one parser, linked list built by a different parser

### 4.4 DisplayRow Field Mapping (LOW)

The old row_writer.rs had explicit field formatting for `descendants`,
`treesize`, `tree_allocated`, `streams`, and path. The new compact path formats
from CompactRecord fields set during `build_compact_index`. If any field is not
transferred correctly from `FileRecord` → `CompactRecord`, output will differ.

## 5. Suspected Issues (Not Yet Confirmed)

### 5.1 tree_metrics Timing

The old streaming path called `record.tree_metrics()` during row writing.
The compact path calls `tree_metrics()` during `build_compact_index`. If the
compact path skips tree_metrics or calls it before all records are loaded (e.g.,
before extension merging), descendants values could differ.

### 5.2 PathResolver vs Compact Path Storage

The old streaming path resolved paths lazily via `PathResolver`. The compact
path stores pre-resolved path indices. If path resolution happens before all
records are loaded, some paths may be incomplete or wrong.

### 5.3 stream_count Expansion

The old streaming path emitted `stream_count` rows per name. Does
`expand_hardlinks` also handle `stream_count > 1` expansion, or only
`name_count` expansion? Need to verify.

### 5.4 System Metafile Filtering Differences

The old streaming path checked `resolver.is_valid_idx(record_idx)` marking
FRS 0-15 (except root FRS 5) as invalid. The compact path's
`MultiDriveBackend::search()` may apply different filters.

## 6. What We Attempted (and Should NOT Repeat)

### 6.1 DOS Namespace Changes (WRONG DIRECTION — REVERTED)

Modified 11 parser files to include DOS-only FILE_NAME attributes (namespace 2)
in ChildInfo entries. Theory: C++ counted DOS entries in descendants.

**Why it was wrong:** v0.4.28 LIVE test PASSED without counting DOS names. Both
C++ and old Rust agreed on descendants WITHOUT DOS names. Adding DOS names would
have INCREASED Rust descendants above C++ → breaking SUPERSET match.

All changes reverted to these files:
`parse/full.rs`, `parse/direct_index.rs`, `parse/direct_index_extension.rs`,
`parse/forensic/base.rs`, `parse/forensic/extension.rs`,
`io/parser/index.rs`, `io/parser/index_extension.rs`, `io/parser/unified.rs`,
`io/parser/fragment.rs`, `io/parser/fragment_extension.rs`, `index/builder.rs`

### 6.2 Chasing Parser Bugs (WRONG DIRECTION)

Significant time spent analyzing parser differences (direct_index vs unified vs
forensic vs fragment). Parsers have NOT changed between v0.4.28 and v0.4.30.
The issue is **dispatch routing**, not the parsers.

## 7. Recommended Recovery Plan

### Option A: Revert to v0.4.28, Surgical Re-apply (RECOMMENDED)

1. `git reset --hard 4442b0b13` — restore v0.4.28
2. Verify parity passes on Windows LIVE
3. Identify which refactor changes are actually needed:
   - Compact cache for TUI performance? → Add as SEPARATE path, keep streaming
   - Code cleanup? → Only remove dead code that has a validated replacement
4. Re-apply each change with parity validation after every step
5. Only delete streaming path AFTER compact path proves byte-identical output

### Option B: Fix Forward from v0.4.30

1. Fix IOCP detection in `compact.rs` → `load_mft_index_from_file()`
2. Add parity tests for `expand_hardlinks` vs `get_name_at` equivalence
3. Validate DisplayRow field mapping against old row_writer.rs
4. Run full Windows LIVE parity test
5. Higher risk — unknown unknowns in the 3,973 deleted lines

## 8. Prevention Rules for Future Refactors

1. **Never delete a working output path before the replacement passes parity.**
   Keep the old streaming path as `--legacy-output` until the new path matches
   byte-for-byte.

2. **IOCP detection is load-bearing.** Any new file-loading path MUST detect
   IOCP format (check magic bytes) and route to `load_iocp_to_index()`.

3. **Parity tests must run after EVERY refactor commit.** Not just at the end.
   One 3,973-line commit is impossible to bisect.

4. **name_count × stream_count expansion must have unit tests.** Create a
   fixture MFT with known hardlinks and verify exact row count + field values.

5. **Document dispatch routing.** The mapping from CLI flags to internal
   pipeline is critical. Any change to dispatch.rs must be called out explicitly
   in commit messages.

6. **Small atomic commits.** The refactor should have been 5-10 commits:
   - Add compact path alongside streaming
   - Add equivalence test (run both, diff output)
   - Route one query type to compact, verify parity
   - Route remaining query types, verify parity each time
   - Delete streaming path only after all parity passes

## 9. Key File References

| File | Role | Status |
|------|------|--------|
| `output/row_writer.rs` | Streaming writer with name×stream expansion | **DELETED** |
| `search/single_file.rs` | Single-file MFT search entry point | **DELETED** |
| `search/streaming_io.rs` | Streaming I/O helpers | **DELETED** |
| `search/dispatch.rs` | CLI → pipeline routing | **CHANGED** |
| `compact.rs` | Compact index builder | Missing IOCP detection |
| `drive_search.rs` | Compact search driver | New, not validated |
| `raw_iocp.rs` | IOCP format loader | Exists but bypassed |
| `io/parser/unified.rs` | IOCP record parser | Unchanged, bypassed |

## 10. Diff Summary (v0.4.28 → v0.4.30)

```
 output/filter.rs      | 286 ------    output/streaming.rs   | 305 ------
 output/row_writer.rs  | 631 ------    output/types.rs       | 290 ------
 output/mod.rs         |  75 +-        search/dispatch.rs    | 130 +--
 search/mft_file.rs    | 174 ------    search/single_file.rs | 178 ------
 search/streaming.rs   | 174 ------    search/streaming_io.rs| 290 ------
 search/util.rs        | 296 ------    raw_io.rs             | 247 +--
```

**Total: ~3,973 deletions, ~220 additions in ONE commit.**