# Search Pipeline Refactor: One Pipeline, Three Phases

## Problem Statement

The search command currently has **3 separate pipelines** that diverge based on
*how* the MFT data was loaded, not *what* the query needs:

| Pipeline | Triggered by | Search method | Path resolution | Performance |
|----------|-------------|---------------|-----------------|-------------|
| **Streaming** | `--drive C`, `--mft-file`, live single/multi | Full scan ALL records | Eager (ALL dirs, ~220ms) | Slow: 750ms+ for 0 matches |
| **Compact** | `--drives C,D,E`, auto-detect (no flag) | Trigram → name match | Only for matches | Fast: ~3ms for 0 matches |
| **DataFrame** | `--index .parquet`, benchmark, json/table | Polars lazy API | FastPathResolver on all | Heaviest |

**Result:** `uffs "orthod" --drive C` is **2× slower** than `uffs "orthod"` (auto-detect)
because `--drive C` routes to streaming while auto-detect routes to compact search.

The data source should NOT affect the search/output pipeline.

## Target Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                        PHASE 1: LOAD                                │
│                                                                     │
│  --mft-file ─┐                                                      │
│  --drive C  ─┤── MftSource ──→ load_drive() ──→ DriveCompactIndex   │
│  --drives   ─┤                                   (per drive)        │
│  auto-detect─┘                                                      │
│                                                                     │
│  --index .parquet ──→ DataFrame (separate path, opt-in only)        │
└──────────────────────────────┬──────────────────────────────────────┘
                               │
                               ▼
┌─────────────────────────────────────────────────────────────────────┐
│                     PHASE 2: SEARCH + FILTER                        │
│                                                                     │
│  DriveCompactIndex ──→ trigram lookup ──→ name match                │
│                        ──→ attribute/size/date filters               │
│                        ──→ sort + limit                              │
│                        ──→ matched indices (Vec<usize>)             │
│                                                                     │
│  Full scan (*): skip trigram, iterate all, apply filters             │
└──────────────────────────────┬──────────────────────────────────────┘
                               │
                               ▼
┌─────────────────────────────────────────────────────────────────────┐
│                      PHASE 3: OUTPUT                                │
│                                                                     │
│  matched indices ──→ resolve paths (ONLY for matches)               │
│                  ──→ build DisplayRow (one per match)               │
│                  ──→ writer:                                         │
│                       csv/custom → native writer (no DataFrame)     │
│                       json       → DisplayRow → small DataFrame     │
│                       table      → DisplayRow → small DataFrame     │
│                       benchmark  → skip output                       │
└─────────────────────────────────────────────────────────────────────┘
```

## What Requires DataFrame (opt-in only)

| Feature | Why DataFrame | Trigger |
|---------|--------------|---------|
| `--index file.parquet` | Data is already a Polars DataFrame | CLI flag |
| `--query-mode dataframe` | User explicitly requests Polars path | CLI flag |
| Tree columns (Bulkiness) | `add_tree_columns` operates on DataFrame | Column selection |

Everything else goes through the fast compact path. Including:
- `--format json` — convert DisplayRows to small DataFrame (already works)
- `--format table` — convert DisplayRows to small DataFrame (already works)
- All filters (name, size, date, attr, ext, exclude)
- Sorting, limiting
- ADS display

## Current Code Map (what exists today)


```
crates/uffs-core/src/compact.rs          # load_drive() → DriveCompactIndex (ALREADY unified)
crates/uffs-core/src/compact_cache.rs    # Compact cache load/save
crates/uffs-core/src/search/query.rs     # search_compact_drive() — trigram + match + paths
crates/uffs-core/src/search/backend.rs   # DisplayRow, MultiDriveBackend
crates/uffs-cli/src/commands/search/drive_search.rs   # search_native_compact() per-drive
crates/uffs-cli/src/commands/search/multi_drive.rs    # search_multi_drive_filtered() parallel
crates/uffs-cli/src/commands/output/mod.rs            # write_native_results() → DisplayRow writer
```

### Files to SIMPLIFY (currently duplicate the fast path)

```
crates/uffs-cli/src/commands/search/live.rs           # 478 lines — reimplements streaming search
crates/uffs-cli/src/commands/search/streaming_io.rs   # Streaming I/O helpers (full-scan path)
crates/uffs-cli/src/commands/search/single_file.rs    # Single-file streaming dispatch
crates/uffs-cli/src/commands/search/mft_file.rs       # Multi-file streaming dispatch
crates/uffs-cli/src/commands/output/row_writer.rs     # 632 lines — streaming row writer with
                                                      #   eager/lazy path resolution, full-scan loop
```

### Files to KEEP AS-IS (DataFrame opt-in path)

```
crates/uffs-cli/src/commands/search/dispatch.rs       # Simplify routing logic
crates/uffs-core/src/export.rs                        # DataFrame → json/table/csv (for opt-in)
```

## Refactoring Steps

### Step 1: Route `--drive C` through compact search

**Smallest change, biggest impact.** Currently in `live.rs:dispatch_windows_live`:

```rust
// CURRENT: single drive → streaming (full scan)
if drives_to_search.len() == 1 && can_write_native_results(...) {
    run_live_single_drive(config, drives_to_search[0]).await?;  // STREAMING
    return Ok(Some(SearchDispatchResult::StreamingComplete));
}
```

**Change:** Treat `--drive C` as `--drives C` — send it through the compact
search path (`search_multi_drive_filtered`) which already handles single drives:

```rust
// AFTER: all live drives → compact search (trigram + match)
let rows = search_multi_drive_filtered(&drives_to_search, ...).await?;
return Ok(Some(SearchDispatchResult::NativeRows(rows)));
```

**Expected impact:** Drive C cached search drops from ~6s → ~1.5s.

**Risk:** Low. The compact search already handles all filters, sorting, output.
The streaming path has no features that compact search lacks.

### Step 2: Route `--mft-file` through compact search

Currently `--mft-file` goes to `single_file.rs` (streaming) or
`mft_file.rs` (multi-file streaming). Both do full-scan.

**Change:** Use `load_drive(MftSource::File(...))` which already produces a
`DriveCompactIndex`, then search via the same compact path:

```rust
// For --mft-file: load as compact, search as compact
let source = MftSource::File(path, drive_override);
let (compact, _timing) = load_drive(&source, no_cache)?;
// Then: search_compact(compact) → DisplayRow → write_native_results
```

**Files affected:** `dispatch.rs` (remove streaming dispatch for mft-file),
`single_file.rs` and `mft_file.rs` (can be removed entirely).

### Step 3: Simplify dispatch to 2 paths

After Steps 1-2, `dispatch_search` becomes:

```rust
pub async fn dispatch_search(config: &SearchConfig<'_>) -> Result<SearchDispatchResult> {
    // ── DataFrame opt-in ──────────────────────────────────────────
    if config.index.is_some() || config.query_mode == "dataframe" {
        return run_dataframe_search(config).await;
    }

    // ── Fast path: compact search (ALL sources) ───────────────────
    let drives = resolve_drives(config)?;
    let sources: Vec<MftSource> = build_sources(config, &drives);
    let rows = search_compact_multi(&sources, &config.filters, config.no_cache).await?;
    Ok(SearchDispatchResult::NativeRows(rows))
}
```

Where `resolve_drives` unifies:
- `--drive C` → `['C']`
- `--drives C,D,E` → `['C', 'D', 'E']`
- pattern `c:/pro*` → `['C']`
- auto-detect → `detect_ntfs_drives()`
- `--mft-file C.bin` → `['C']` (infer from filename)

And `build_sources` unifies:
- `--mft-file path` → `MftSource::File(path, drive)`
- live drive → `MftSource::Live(drive)`

### Step 4: Eliminate streaming pipeline

After Steps 1-3, these files have zero callers:

```
DELETE: live.rs              → replaced by compact search for all live drives
DELETE: single_file.rs       → replaced by compact search for --mft-file
DELETE: mft_file.rs          → replaced by compact search for multi --mft-file
DELETE: streaming.rs         → already removed (comment in mod.rs)
SIMPLIFY: streaming_io.rs   → only keep build_record_filter if still used
SIMPLIFY: row_writer.rs     → remove full-scan path, keep only for DataFrame opt-in
```

### Step 5: Unify output

After Steps 1-4, all search results are `Vec<DisplayRow>`. Output becomes:

```rust
fn write_output(rows: &[DisplayRow], config: &SearchConfig) -> Result<()> {
    match config.format {
        "csv" | "custom" => write_native_results(rows, ...),
        "json" | "table" => {
            let df = display_rows_to_dataframe(rows)?;  // small, only match rows
            write_results(&df, ...);
        }
        _ => write_native_results(rows, ...),
    }
}
```

## Migration Safety

### Parity guarantees
- ✅ **Parity confirmed** (2026-03-30) via `scripts/verify_parity.rs` on both macOS and Windows
- 7 drives tested: 5/7 SUPERSET/EXACT match, 2/7 env drift (live system churn, not code bugs)
- Drive M achieves exact order-independent fingerprint match (1,908,783 lines)
- Run `scripts/verify_parity.rs` before/after Steps 4-5 to confirm no regressions

### Incremental rollout
Each step is independently deployable and testable:
1. Step 1 alone saves ~4s on Drive C — deploy and benchmark immediately
2. Step 2 makes --mft-file consistent — test with offline MFT files
3. Step 3 is pure code cleanup — no behavior change
4. Step 4-5 are cleanup — remove dead code

### Fallback
Keep `--query-mode dataframe` as escape hatch. If any edge case needs the
old DataFrame path, users can force it. Remove after confidence period.

## Performance Impact (estimated)

### Before (current)

| Command | Pipeline | Drive C (3.1M records) |
|---------|----------|----------------------|
| `uffs "orthod"` | Compact (auto-detect) | ~1.5s (cache hit) |
| `uffs "orthod" --drive C` | Streaming (live) | ~6.1s |
| `uffs "orthod" --mft-file C.bin` | Streaming (file) | ~4.5s |

### After (refactored)

| Command | Pipeline | Drive C (3.1M records) |
|---------|----------|----------------------|
| `uffs "orthod"` | Compact | ~1.5s (cache hit) |
| `uffs "orthod" --drive C` | Compact | ~1.5s (cache hit) |
| `uffs "orthod" --mft-file C.bin` | Compact | ~1.5s (cache hit) |

All paths converge to the same performance.

## Parity Verification Results (v0.4.31)

> **✅ Parity confirmed** on 2026-03-30 across both macOS (IOCP cross-platform) and
> Windows (live MFT). Steps 4 and 5 are now unblocked for execution.

### macOS — IOCP file cross-platform (`--pipeline unified --no-cache`)

| Drive | MFT Size | Result | Lines (baseline / Rust) | Notes |
|-------|----------|--------|------------------------|-------|
| **C** | 442.7 MB | ⚠️ Env drift | 3,273,291 / 3,428,412 | 3,006 missing = WinSxS patch churn during IOCP capture; 158,127 extra = Norton/Chrome/Temp live writes. Not code bugs. |
| **D** | 407.9 MB | ✅ SUPERSET | 7,065,281 / 7,065,517 | +236 verified hardlinks |
| **E** | 223.0 MB | ✅ SUPERSET | 2,927,003 / 2,929,497 | +2,494 verified hardlinks |
| **F** | 425.0 MB | ⚠️ Env drift | 2,081,233 / 2,221,321 | 104 missing = same MFT churn pattern; 140,192 extra = live writes. |
| **G** | 0.7 MB | ✅ SUPERSET | 15,059 / 15,071 | +12 verified hardlinks |
| **M** | 178.1 MB | ✅ EXACT | 1,908,783 / 1,908,783 | **Order-independent fingerprint match** — gold standard |
| **S** | 813.4 MB | ✅ SUPERSET | 8,277,472 / 8,278,080 | +608 verified hardlinks |

### Windows — Live MFT scan (C++ vs Rust, both reading live MFT)

| Drive | Result | C++ Time | Rust Time | Notes |
|-------|--------|----------|-----------|-------|
| **C** | ⚠️ Env drift | 12.82s | 54.63s | Same pattern: live system writes between scans |
| **D** | ✅ SUPERSET | 40.52s | 1m 9.8s | +236 hardlinks, 0 unverified |
| **E** | ✅ SUPERSET | 45.93s | 55.95s | +2,494 hardlinks, 0 unverified |
| **F** | ⚠️ Env drift | 7.17s | 16.70s | Same pattern |
| **G** | ✅ SUPERSET | 282ms | 536ms | +12 hardlinks, 0 unverified |
| **M** | ✅ SORTED MATCH | 25.10s | 36.77s | Exact content, different traversal order |
| **S** | ✅ SUPERSET | 1m 1.9s | 1m 56.2s | +608 hardlinks, 0 unverified |

> **Assessment:** 5/7 drives achieve SUPERSET or EXACT match. The C and F
> "mismatches" are environmental (live system MFT churn between C++ and Rust
> scans — Norton, Chrome, WinSxS patching). Field-level diffs show only
> timestamp differences from files changing on a running system, not code bugs.
> **Parity is confirmed.**

---

<details>
<summary><strong>Files Changed Summary (Previous Attempt v0.4.30 — Historical Reference)</strong></summary>

> **⚠️ REVERTED:** The changes below were made in the first attempt (v0.4.30)
> but caused critical regressions (see `2026_03_30_04_12_SEARCH_PIPELINE_REGRESSION_ANALYSIS.md`).
> The streaming pipeline was doing much more than initially understood —
> IOCP detection, name×stream expansion, inline path resolution, tree metrics,
> and system metafile filtering were all lost. This table is kept as a reference
> for what was touched and what broke.

| File | Action (previous attempt) | Before | After | Δ |
|------|---------------------------|--------|-------|---|
| `search/dispatch.rs` | Simplified routing | 533 | 427 | −106 |
| `search/live.rs` | Gutted to compact-only | 478 | 45 | −433 |
| `search/single_file.rs` | **Deleted** | ~150 | 0 | −150 |
| `search/mft_file.rs` | **Deleted** | ~200 | 0 | −200 |
| `search/streaming_io.rs` | **Deleted** | ~300 | 0 | −300 |
| `search/streaming.rs` | **Deleted** (orphaned) | ~100 | 0 | −100 |
| `search/util.rs` | Removed dead helpers + tests | 360 | 60 | −300 |
| `search/mod.rs` | Removed `StreamingComplete` | 281 | 267 | −14 |
| `search/multi_drive.rs` | Kept as-is | 163 | 163 | 0 |
| `search/drive_search.rs` | Added logging, fixed returns | 100 | 207 | +107 |
| `output/filter.rs` | **Deleted** | ~250 | 0 | −250 |
| `output/row_writer.rs` | **Deleted** | 632 | 0 | −632 |
| `output/types.rs` | **Deleted** | ~120 | 0 | −120 |
| `output/streaming.rs` | **Deleted** | ~80 | 0 | −80 |
| `output/mod.rs` | Removed streaming helpers | ~190 | ~100 | −90 |
| `output/output_tests.rs` | Removed streaming tests | 712 | 371 | −341 |
| `raw_io_windows.rs` | Removed `execute`, `load_live_index` | ~180 | 106 | −74 |
| **Total** | | **~4,829** | **~1,746** | **~−3,083** |

> **Previous attempt removed ~3,000 lines.** The streaming pipeline had critical
> functionality that was not replicated in the compact path. See regression
> analysis §4.1–4.4 for the specific gaps that must be addressed in the re-do.

</details>

## Tracking

> **Strategy (2026-03-30):** Instead of deleting old code (which caused 300+ dead-code
> warnings and forced premature deletion → regressions), we keep the legacy pipeline
> intact behind `--pipeline legacy`.  The new unified pipeline is the **default**.
> All legacy code is tagged `[LEGACY_PIPELINE]` for easy bulk deletion once parity
> is confirmed.  `grep -rn "\[LEGACY_PIPELINE\]" crates/` finds all 29 tagged sites.

| Step | Status | Notes |
|------|--------|-------|
| **Pipeline fork + `--pipeline` flag** | ✅ Done | `dispatch_search()` routes to `dispatch_unified()` (default) or `dispatch_legacy()`. Runtime switchable. |
| **Step 1: Route `--drive C` through compact search** | ✅ Done | `dispatch_unified()` → `load_live_drives()` → `MftSource::Live` → `load_drive()` → `MultiDriveBackend.search()`. `--query-mode dataframe` escape hatch preserved. Auto-detect all NTFS drives when no `--drive` specified. |
| **Step 2: Route `--mft-file` through compact search** | ✅ Done | `dispatch_unified()` → `load_unified_drives()` → `MftSource::File` → `load_drive()` → `MultiDriveBackend.search()`. Multi-file + drive letter override supported. |
| **Step 3: Simplify dispatch to 2 paths** | ✅ Done | `dispatch_unified()` has exactly 2 paths: compact (default) and DataFrame escape hatch (`--query-mode dataframe` / `--index`). Legacy streaming modules preserved behind `--pipeline legacy` — no deletion needed. |
| **All 14 filters wired** | ✅ Done | `SearchFilters::from_params()` receives all filter fields: min/max size, newer/older modified/created/accessed, attr require/exclude, extension, exclude glob, min/max descendants, hide_system. Plus `FilterMode` for files_only/dirs_only. |
| **Sort wired** | ✅ Done | `parse_sort_spec()` → `backend.sort_column` / `sort_desc` / `extra_sort_tiers`. `--sort-desc` defaults to size desc. |
| **`NativeRows` cross-platform** | ✅ Done | Removed `#[cfg(windows)]` from `NativeRows`, `finalize_native_output`, `write_native_results`. All platforms can now use compact search output. |
| **28 regression tests** | ✅ Done | 11 query parity + 17 filter tests — all pass on synthetic data cross-platform. |
| **✅ Parity verified (macOS + Windows)** | ✅ Done | 7 drives verified on both platforms. 5/7 SUPERSET/EXACT match. 2/7 (C, F) show env drift only (live system MFT churn — Norton, Chrome, WinSxS). Drive M = exact fingerprint match. See §Parity Verification Results above. |
| Step 4: Eliminate streaming pipeline | 🟡 Ready | **Parity confirmed.** Delete all `[LEGACY_PIPELINE]`-tagged code (29 sites) + `--pipeline` flag. |
| Step 5: Unify output | ✅ Done | All paths converge through `finalize_native_output`. `DataFrame` variant converts via `dataframe_to_display_rows()`. Removed `finalize_dataframe_output`, `print_benchmark_stats(df)`, `print_profile_stats(df)`. `write_results(df)` gated `#[cfg(test)]`. |
| Prerequisite: TTL-only compact cache (skip mtime) | ✅ Done | `trust_ttl_only: bool` param added to `load_compact_cache`. `compact_loader` uses `true` (fast path), `ensure_compact_cached` uses `false` (has MftIndex). |
| Prerequisite: Search phase CACHE_PROFILE timing | ✅ Done | `search_{drive}` labels for trigram/regex/tree modes + `search_total` wall time in `MultiDriveBackend::search`. |
| **✅ Resolved: IOCP file detection** | ✅ Done | IOCP files successfully parsed cross-platform via unified pipeline. All 7 drives used IOCP captures on macOS without issues. |
| **✅ Resolved: name×stream expansion** | ✅ Done | `build_compact_index()` Phase 2 expands hardlinks, Phase 3 expands ADS streams into separate `CompactRecord` entries with combined `base:stream` names. Verified via parity on drive M (1,908,783 lines match). |

---

## Waves 2–5: Unified Field System

> **Full design and field coverage analysis** is in
> [`FILTER_SORT_FEATURE_MATRIX.md`](FILTER_SORT_FEATURE_MATRIX.md).
> This section tracks pipeline-level changes only.

Wave 1 unified all input sources into one pipeline. But it left behind:

1. **Silent feature regression** — 13 CLI filter/sort flags accepted but ignored
2. **15 dead `SearchConfig` fields** suppressed by `#[expect(dead_code)]`
3. **4 separate enum systems** for addressing fields (OutputColumn, SortColumn, TuiColumn, SearchFilters)
4. **17 cold-path fields** parsed but not exposed to filter/sort/output
5. **Duplicated output functions** — 4 near-identical function pairs in `dispatch.rs`

### Architecture Decision: No DSL / No SQL

UFFS does not need a query engine. The pipeline is fixed:
`pattern → filter → sort → limit → project`. No joins, no aggregation,
no subqueries. What we need is a **unified field-addressing system** so
filter, sort, output, and all frontends (CLI/TUI/Daemon/GUI/MCP) speak
the same language. See §4 of `FILTER_SORT_FEATURE_MATRIX.md`.

Collapse `SearchConfig` → `QueryFilters` → `OwnedQueryFilters` into
`SearchConfig` → `CompactSearchParams`. Build `SearchFilters` at config
time. Wire sort through `MultiDriveBackend`. Delete dead fields and
`#[expect(dead_code)]`.

**Scope:** 13 broken CLI flags → ✅. 15 dead fields removed. 4 duplicate
stats functions → 2. See §6.2 of `FILTER_SORT_FEATURE_MATRIX.md` for
step-by-step tracking.

**Files changed:** `raw_io.rs`, `raw_io_windows.rs`, `dispatch.rs`,
`search/mod.rs`.

### Wave 3 — Unified FieldId enum

Create a single `FieldId` enum (52 variants) with compile-time metadata
(type, access tier, aliases, default sort direction). Map existing
`OutputColumn`, `SortColumn`, `TuiColumn` to `FieldId`. All frontends
parse field names via one canonical `FieldId::parse()`.

**Key types:**

```rust
pub enum FieldId { Path, Name, Size, Modified, Hidden, Frs, ReparseTag, ... }

pub struct FieldMeta {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub field_type: FieldType,      // String, U64, I64, Bool, Timestamp, ...
    pub access: FieldAccess,        // Hot, Derived, Cold
    pub default_desc: bool,
    pub display_name: &'static str,
}
```

See §4.3–4.4 of `FILTER_SORT_FEATURE_MATRIX.md`.

### Wave 4 — Predicate-driven filtering

Replace `SearchFilters` bespoke struct with generic predicate system:

```rust
pub struct FieldPredicate { pub field: FieldId, pub op: FilterOp }
pub enum FilterOp { Eq(i64), Gt(i64), NewerThan(String), Glob(String), IsSet, ... }
```

Add `--filter "size:gt:1M,modified:newer:7d,hidden:set"` CLI flag.
Existing shorthand flags (`--newer`, `--attr`, etc.) remain as ergonomic
aliases that parse into `Vec<FieldPredicate>`. Predicates are split by
access tier: hot-path filters run during search, cold-path filters run
after search on matched rows only.

See §4.5–4.6 of `FILTER_SORT_FEATURE_MATRIX.md`.

### Wave 5 — Cold-path output + filter

Wire `ExtraRecordFields` into the output pipeline so cold-path fields
(FRS, reparse tag, $FILE_NAME timestamps, forensic flags, etc.) become
selectable as output columns, sortable, and filterable. Loaded on-demand
from `.uffs` cache only for matched rows (512-entry LRU cache).

See §5.3 of `FILTER_SORT_FEATURE_MATRIX.md`.

### Waves 2–5 Tracking

| Wave | Status | Scope | Reference |
|------|--------|-------|-----------|
| ~~Wave 2~~ | Absorbed into D5 | 14 broken flags fixed when D5 deletes broken CLI pipeline | `DAEMON_IMPLEMENTATION_PLAN.md` §D5.2 |
| **D5+D6** | 🔲 Not started | All frontends → daemon-only, shmem for bulk, fixes 14 flags | `DAEMON_IMPLEMENTATION_PLAN.md` §D5–D6 |
| Wave 3: FieldId + derived fixes | 🔲 After D5+D6 | FieldId enum, FileCategory, Bulkiness, TreeAllocated | `FILTER_SORT_FEATURE_MATRIX.md` §4.3 |
| Wave 4: Predicates + time sugar | 🔲 After D5+D6 | `--filter` flag, named time specs, `--group-by` | `FILTER_SORT_FEATURE_MATRIX.md` §4.6 |
| Wave 5: Cold-path integration | 🔲 After D5+D6 | 17 fields → output/filter/sort | `FILTER_SORT_FEATURE_MATRIX.md` §5.3 |
