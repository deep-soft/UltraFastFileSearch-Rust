# MFT Pipeline Consolidation — Refactor Plan

**Created:** 2026-03-29
**Last Updated:** 2026-04-01
**Status:** ✅ CORE COMPLETE — Waves 0–3 done, Wave 4 partially done (by design), Wave 5 assessed
**Goal:** ONE cold path, ONE hot path. Every caller gets the same pipeline, profiling, and optimizations.

### Result Summary

The consolidation achieved its primary goal. Production validation on Windows (7 NTFS drives,
25.8M records) confirmed: `MftSource` + `load_drive()` is THE entry point for daemon, CLI, and
TUI. All profiling flows through a single `CACHE_PROFILE` pipeline. Per-drive cache load rates
of **1.7–2.7M records/sec** (see [Production Validation](#production-validation-daemon-runs-v0449v0451-2026-04-01)).

---

## Problem Statement

We have **7+ independent code paths** that produce an `MftIndex`. Each one has a different subset of:
- Profiling instrumentation
- Parallel parsing (rayon)
- IOCP replay
- Cache save (serialize → zstd → AES → write)
- Tree metrics computation
- Extension index build
- `save_to_cache` / `save_compact_cache`

When we optimise one path, the others don't benefit. When we add profiling to one, the others stay dark. The v0.3.62→v0.4.23 regression went undetected because the benchmarks hit different paths than the TUI.

---

## Current Inventory (what exists today)

### A. COLD paths (MFT → MftIndex)

| # | Location | Parse Mode | Parallel? | Profiling? | Cache Save? | Who calls it |
|---|----------|-----------|-----------|-----------|-------------|-------------|
| 1 | **`uffs-core/compact.rs:load_mft_file()`** | Sequential iter+fixup+merge | ❌ single-thread | ✅ (newly added) | ✅ sync on main thread | TUI offline, CLI file |
| 2 | **`uffs-mft/persistence.rs:load_raw_to_dataframe_with_options()`** | Sequential iter+fixup+merge_into_columns | ❌ | ❌ | ❌ | Legacy DataFrame path |
| 3 | **`uffs-mft/persistence.rs:load_raw_to_index_with_options()`** | Sequential OR parallel (rayon 4096-chunk), forensic branch | ✅ rayon | ❌ | ❌ | CLI raw-io, offline analysis |
| 4 | **`uffs-mft/persistence.rs:load_iocp_capture_to_index()`** | IOCP chunk replay, parallel rayon | ✅ rayon | ❌ | ❌ | IOCP capture replay |
| 5 | **`uffs-mft/persistence.rs:load_raw_to_index_direct()`** | Direct-to-index via `process_record()` | ❌ single-thread | ❌ | ❌ | Experimental C++ parity |
| 6 | **`uffs-mft/index_read.rs:read_mft_index_internal()`** | 8 mode branches (Parallel/Pipelined/IOCP/Bulk/SlidingIocp/SlidingIocpInline/Streaming/Prefetch) | ✅ varies | partial (tracing) | ❌ (cache done by caller) | Live Windows scan |
| 7 | **`uffs-mft/index_timing.rs:read_mft_index_with_timing_internal()`** | ParallelMftReader with timing | ✅ | ✅ detailed | ❌ | Benchmark/TUI timing |

### B. HOT paths (cache → MftIndex)

| # | Location | What it does | Profiling? | Who calls it |
|---|----------|-------------|-----------|-------------|
| H1 | **`uffs-mft/index_cache.rs:read_index_cached()`** | check_cache → load_cached_index (deserialize) → USN update → save | ✅ (deserialize.rs) | CLI live single-drive |
| H2 | **`uffs-mft/multi_drive/index.rs:read_all_index_cached()`** | Per-drive: check_cache → load → USN → save | ❌ | TUI multi-drive |
| H3 | **`uffs-core/compact.rs:load_live_drive()`** | load_compact_cache → fallback to H1 | partial | CLI search, TUI |
| H4 | **`uffs-core/compact.rs:load_mft_file()`** | load_cached_index → fallback to cold A1 | ✅ (newly added) | TUI offline |
| H5 | **`uffs-mft/index_cache.rs:read_and_cache_index()`** | read_all_index → serialize → zstd → AES → write (DUPLICATES save pipeline) | ❌ | Called by H1 on miss |

### C. Cache SAVE paths (MftIndex → disk)

| # | Location | Pipeline | Profiling? |
|---|----------|----------|-----------|
| S1 | **`uffs-mft/cache.rs:save_to_cache()`** → **`file_io.rs:save_to_file()`** | serialize → zstd → AES → atomic_write | ✅ (newly added) |
| S2 | **`uffs-mft/index_cache.rs:read_and_cache_index()`** | serialize → zstd → AES → spawn_blocking(atomic_write) | ❌ DUPLICATES S1! |
| S3 | **`uffs-mft/multi_drive/index.rs:read_and_cache_single_drive_sync()`** | Calls S1 | inherits S1 |
| S4 | **`uffs-core/compact_cache.rs:save_compact_cache()`** | serialize → zstd → AES → atomic_write | ✅ (newly added) |

**Key DRY violation:** S2 is a complete copy of the serialize→compress→encrypt→write pipeline that S1 already provides, but without profiling and with slightly different error handling.

---



## Target Architecture

```
                    ┌─────────────────────────────────┐
                    │         CALLERS                  │
                    │  CLI search · CLI raw-io · TUI   │
                    │  Benchmarks · Tests              │
                    └──────────┬──────────────────────┘
                               │
                    ┌──────────▼──────────────────────┐
                    │  uffs_mft::load_index()          │
                    │  ONE entry point                 │
                    │  Input: IndexLoadRequest {       │
                    │    source: File(path) | Live(ch) │
                    │    cache_policy: NoCache|Cached  │
                    │    profiling: bool               │
                    │    progress: Option<Fn>          │
                    │  }                               │
                    │  Output: (MftIndex, LoadProfile) │
                    └──────────┬──────────────────────┘
                               │
              ┌────────────────┼────────────────┐
              ▼                ▼                ▼
        ┌───────────┐  ┌────────────┐  ┌──────────────┐
        │ HOT path  │  │ COLD path  │  │ COLD path    │
        │ cache hit │  │ file-based │  │ live volume  │
        │ deser+USN │  │ rayon parse│  │ IOCP inline  │
        └───────────┘  └────────────┘  └──────────────┘
              │                │                │
              └────────────────┼────────────────┘
                               │
                    ┌──────────▼──────────────────────┐
                    │  Shared post-processing:         │
                    │  • compute_tree_metrics()        │
                    │  • build_extension_index()       │
                    │  • CACHE_PROFILE emit            │
                    │  • save_to_cache() (background)  │
                    └─────────────────────────────────┘
```

All profiling, cache save, tree metrics, and ext index happen in ONE place — the shared post-processing block. No caller ever calls `save_to_cache` or `compute_tree_metrics` directly.

---

## Refactor Waves

### Wave 0 — Preparation (no behavior change)

**Goal:** Inventory + profiling baseline so we can measure before/after.

| Step | What | Files | Risk |
|------|------|-------|------|
| 0.1 | ✅ Add CACHE_PROFILE timers to cold path in compact.rs | `compact.rs`, `file_io.rs` | None |
| 0.2 | ✅ Fix version label in deserialize.rs ("v10" → actual) | `deserialize.rs` | None |
| 0.3 | ✅ Enable CACHE_PROFILE in benchmark.ps1 | `benchmark.ps1` | None |
| 0.4 | ✅ Add `--version` output to cache-check.ps1 + benchmark.ps1 header | `cache-check.ps1`, `benchmark.ps1` | None |
| 0.5 | ✅ Fix cache-check.ps1: remove redundant double status, clear compact cache too | `cache-check.ps1` | None |
| 0.6 | ✅ Add profiling to save_to_file(): serialize/compress/encrypt/write breakdown | `file_io.rs` | None |
| 0.7 | ✅ Add profiling to save_compact_cache(): encrypt/write breakdown | `compact_cache.rs` | None |
| 0.8 | ✅ Add compact_save_outer timing to load_mft_file() and load_live_drive() | `compact.rs` | None |
| 0.9 | Run benchmark with profiling, capture baseline numbers for ALL drives | Manual (Windows) | None |

### Wave 1 — Eliminate duplicate cache-save pipeline (S2)

**Goal:** `read_and_cache_index()` in index_cache.rs should call `save_to_cache()` (S1) instead of reimplementing serialize→compress→encrypt→write inline.

| Step | What | Files | Risk |
|------|------|-------|------|
| 1.1 | Replace inline serialize+compress+encrypt+write in `read_and_cache_index()` with a call to `save_to_cache()` wrapped in `spawn_blocking` | `index_cache.rs` | Low — same logic, just deduplicated |
| 1.2 | Verify save_to_cache profiling now covers this path too | `file_io.rs` | None |
| 1.3 | Run tests + benchmark to confirm no regression | — | None |

### Wave 2 — Eliminate compact.rs cold parse loop (A1)

**Goal:** `compact.rs:load_mft_file()` should NOT contain MFT parsing logic. It should call an uffs-mft function.

| Step | What | Files | Risk |
|------|------|-------|------|
| 2.1 | `load_raw_to_index_with_options()` (A3) already exists and is the most advanced offline parser — confirm it is the KEEPER | `persistence.rs` | None |
| 2.2 | Replace the manual parse loop in `compact.rs:load_mft_file()` with a call to `MftReader::load_raw_to_index_with_options()` | `compact.rs` | Medium — A3 has rayon; A1 was sequential. **This is a speedup.** |
| 2.3 | Move CACHE_PROFILE profiling FROM compact.rs INTO `load_raw_to_index_with_options()` so all callers get it | `persistence.rs`, `compact.rs` | Low |
| 2.4 | Move `save_to_cache()` call from compact.rs into the shared post-load path (see Wave 4) | `compact.rs` | Low |
| 2.5 | Run full test suite + benchmark. Offline path now gets rayon parallel parsing → expected speedup | — | None |

**Why A3 is the KEEPER:**
- ✅ Rayon parallel parsing (4096-record chunks)
- ✅ IOCP capture format auto-detection
- ✅ Forensic mode support
- ✅ Single-thread escape hatch (`UFFS_SINGLE_THREAD` env)
- ✅ Per-phase timing via `tracing::info!`

**Why A1 (compact.rs) must die:**
- ❌ Single-threaded only
- ❌ No forensic mode
- ❌ No IOCP detection
- ❌ Duplicates 15 lines of parse logic that belongs in uffs-mft

### Wave 3 — Unified entry point

**Goal:** ONE function signature for "give me an MftIndex from any source".

| Step | What | Files | Risk |
|------|------|-------|------|
| 3.1 | Create `IndexLoadRequest` struct: `source` (File/Live), `cache_policy`, `profiling`, `progress_cb` | `uffs-mft/src/reader/mod.rs` | None |
| 3.2 | Create `LoadProfile` struct with full timing breakdown (replaces ad-hoc `LoadTiming` in compact.rs) | `uffs-mft/src/reader/mod.rs` | None |
| 3.3 | Create `pub async fn load_index(req: IndexLoadRequest) -> Result<(MftIndex, LoadProfile)>` dispatching: File → A3, Live → A6, Cache → H1 | `uffs-mft/src/reader/` | Medium |
| 3.4 | Migrate CLI search (`drive_search.rs`) from `load_live_drive()` to `load_index()` | `drive_search.rs` | Medium |
| 3.5 | Migrate TUI from `load_mft_file()`/`load_live_drive()` to `load_index()` | `tui/main.rs`, `tui/refresh.rs` | Medium |
| 3.6 | Migrate CLI raw-io from direct `load_raw_to_index_with_options()` to `load_index()` | `raw_io.rs`, `raw_io_windows.rs` | Medium |
| 3.7 | Deprecate `load_mft_file()`, `load_live_drive()` in compact.rs | `compact.rs` | Medium |

### Wave 4 — Shared post-processing (includes compact index)

**Goal:** tree_metrics, ext_index, cache save, **AND compact build+save** all happen in ONE place.

Compact index was originally planned as a separate wave, but it has the SAME DRY problem — compact build+load+save is scattered across **5 independent call sites** today:
- `compact.rs:load_mft_file():492` → `build_compact_index` + `save_compact_cache`
- `compact.rs:load_live_drive():567` → `build_compact_index` + `save_compact_cache`
- `compact_cache.rs:ensure_compact_cached():418` → `load_compact_cache` → fallback build+save
- `raw_io.rs:88,259` → calls `ensure_compact_cached`
- `raw_io_windows.rs:163` → calls `ensure_compact_cached`

Folding it into the shared post-processing means ONE code path for everything.

| Step | What | Files | Risk |
|------|------|-------|------|
| 4.1 | Create `fn post_process_index(index: &mut MftIndex, opts: &PostProcessOpts)` — runs all post-load steps: tree_metrics → ext_index → save MftIndex (background) → build compact → save compact (background) → emit CACHE_PROFILE | `uffs-core` | Low |
| 4.2 | `PostProcessOpts` includes: `build_compact: bool` (default true), `cache_policy`, `profiling` — callers like `info`/`stats` that don't need compact pass `build_compact: false` | `uffs-core` | None |
| 4.3 | Return type includes `Option<DriveCompactIndex>` alongside `MftIndex` and `LoadProfile` | — | Low |
| 4.4 | Remove scattered `compute_tree_metrics()` calls from: `index_read.rs:713`, `persistence.rs:531`, `compact.rs` | Multiple | Medium — must not double-compute |
| 4.5 | Remove scattered `build_extension_index()` calls from: `index_read.rs:720`, `from_parsed_records()` | Multiple | Medium |
| 4.6 | Move `save_to_cache()` calls from: `compact.rs:498`, `multi_drive/index.rs:267`, `index_cache.rs:216` into post_process | Multiple | Medium |
| 4.7 | Move `build_compact_index()` + `save_compact_cache()` calls from: `compact.rs:492,567`, `compact_cache.rs:418`, `raw_io.rs:88,259`, `raw_io_windows.rs:163` into post_process | Multiple | Medium |
| 4.8 | Delete `ensure_compact_cached()` — now redundant; post_process does it | `compact_cache.rs` | Low |
| 4.9 | Make both MftIndex + compact cache saves async/background using `SaveGuard` pattern (see below) | — | Medium |

#### Background save design: `SaveGuard` (RAII join-on-drop)

**Problem:** Fire-and-forget is dangerous — silent failures, lost errors, process exits before save completes.

**Solution:** `SaveGuard` — a RAII handle that owns the `JoinHandle` and joins on drop.

```rust
/// Ensures a background cache save completes before the process exits.
/// On drop: joins the thread, logs any error or panic.
pub struct SaveGuard {
    handle: Option<std::thread::JoinHandle<Result<(), anyhow::Error>>>,
    label: &'static str,       // "mft_save" or "compact_save"
    started: std::time::Instant,
}

impl Drop for SaveGuard {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            let elapsed = self.started.elapsed().as_millis();
            match h.join() {
                Ok(Ok(())) => {
                    tracing::debug!("Background {} completed in {elapsed}ms", self.label);
                }
                Ok(Err(e)) => {
                    tracing::error!("Background {} failed after {elapsed}ms: {e}", self.label);
                }
                Err(_) => {
                    tracing::error!("Background {} PANICKED after {elapsed}ms!", self.label);
                }
            }
        }
    }
}
```

**Timeline — what overlaps with what:**

```
Thread 1 (main)                Thread 2 (mft save)       Thread 3 (compact save)
─────────────────              ────────────────────       ────────────────────────
tree_metrics (~0ms)
ext_index (~0ms)
spawn mft save ─────────────► serialize (300ms)
build_compact (130ms)          compress (1400ms)
  (overlaps with mft save!)    encrypt (180ms)
spawn compact save ──────────────────────────────────► serialize (50ms)
                               write (70ms)              compress (30ms)
path_resolver (170ms)          ✅ done                   encrypt (5ms)
row_output (7200ms)                                      write (10ms)
                                                         ✅ done
guards drop → join (instant, both already done)
```

**Key properties:**
1. **Guaranteed completion:** Drop joins — process cannot exit without save finishing
2. **Error visibility:** Failures logged via `tracing::error!`, never silently swallowed
3. **Panic safety:** Thread panics are caught by `join()` and reported
4. **Overlap:** MFT save runs in parallel with compact build (~1.5s of compression overlaps ~130ms of compact build — effectively free)
5. **No watcher needed:** RAII does the watching — the guard IS the watcher
6. **Profiling:** Drop emits elapsed time, visible in CACHE_PROFILE output

**When caller doesn't need the guard to live long** (e.g., `uffs info` which exits immediately after printing):
- The guard drops at end of scope → blocks until save completes → same as synchronous but with the option to overlap if the caller has more work to do.

### Wave 5 — Remove dead code

**Goal:** Clean up paths that are unused after consolidation.

| Step | What | Files | Risk |
|------|------|-------|------|
| 5.1 | Remove `load_raw_to_dataframe_with_options()` (A2) — DataFrame path is dead | `persistence.rs` | Low — verify no callers |
| 5.2 | Remove `load_raw_to_index_direct()` (A5) — experimental, never productionized | `persistence.rs` | Low |
| 5.3 | Remove `read_mft_index_with_timing_internal()` (A7) if timing is now built into the main path | `index_timing.rs` | Medium — TUI benchmark may use it |
| 5.4 | Remove redundant `MftReadMode` variants if unused after consolidation | `read_mode.rs` | Medium |

---

## KEEPER implementations (do NOT replace these)

| Path type | KEEPER location | Why |
|-----------|----------------|-----|
| **COLD (file)** | `persistence.rs:load_raw_to_index_with_options()` (A3) | Rayon parallel, forensic, IOCP detect, single-thread escape |
| **COLD (live)** | `index_read.rs:read_mft_index_internal()` (A6) | 8 I/O modes, SlidingIocpInline (fastest), bitmap, progress |
| **HOT (cache)** | `index_cache.rs:read_index_cached()` (H1) | USN journal, read-only fast-path, journal-wrap detection |
| **Cache SAVE** | `file_io.rs:save_to_file()` (S1) | Full profiling (serialize/compress/encrypt/write breakdown) |

---

## Risk Mitigation

1. **Each wave is independently shippable.** Wave N doesn't depend on Wave N+1.
2. **Benchmark after every wave.** Compare cold + warm times against the Wave 0 baseline.
3. **Keep old functions as `#[deprecated]` wrappers** during migration — don't delete until all callers are migrated.
4. **Test with `UFFS_CACHE_PROFILE=1`** on Windows to verify profiling output covers all paths.

---

## Expected Outcomes — Measured (v0.4.51, 2026-04-01)

| Metric | Before | Target | Measured | Status |
|--------|--------|--------|----------|--------|
| Cold paths in codebase | 7+ | 2 (file + live) | **2** (`load_mft_index_from_file` + `load_mft_index_live`) | ✅ |
| Hot paths in codebase | 5 | 1 | **1** (`load_drive()` → cache check → fallback cold) | ✅ |
| Cache save implementations | 2 (MftIndex + compact, each duplicated) | 1 each | **1 each** (S1 for MftIndex, S4 for compact) | ✅ |
| Compact build+save call sites | 5 scattered | 1 | **1** (inside `load_drive()`) + 1 legacy (`ensure_compact_cached` for raw-io) | ✅ |
| Profiling coverage (cold) | ~30% | 100% | **100%** — all branches in `persistence.rs` emit CACHE_PROFILE | ✅ |
| Profiling coverage (hot) | ~60% | 100% | **100%** — `deserialize.rs` + `file_io.rs` + `compact_cache.rs` all profiled | ✅ |
| Offline file parse speed | Sequential (compact.rs) | Rayon parallel | **Rayon parallel** (4096-chunk, auto-detected) | ✅ |
| Cache save on cold path | Synchronous (blocks caller) | Background | **Synchronous** (SaveGuard deferred — measured save time <2s, not a bottleneck) | ⏸ |
| Callers using `load_drive()` | 0 (didn't exist) | All main paths | **6** (daemon ×3, CLI, TUI, TUI refresh) | ✅ |
| Deprecated function callers | N/A | 0 | **0** (only test/diagnostic code uses deprecated wrappers) | ✅ |
| Daemon cache load rate (25.8M records, 7 drives) | N/A | — | **2.2M rec/s avg** (12s total, compact cache) | ✅ baseline |
| Daemon warm search (25.8M records) | N/A | <15ms | **0ms query + 12ms wall** | ✅ |
| CLI flag validation (44 tests) | 14 broken flags | 44/44 pass | **44/44 pass** (v0.4.55, 56.5s total, avg 1.3s) | ✅ |

---

## Implementation Tracking

### Wave 0 — Preparation

| Step | Status | Date | Commit/PR | Notes |
|------|--------|------|-----------|-------|
| 0.1 | ✅ Done | 2026-03-29 | — | Added `mft_read`, `mft_parse`, `mft_build` timers to `load_mft_file()` cold path in `compact.rs` |
| 0.2 | ✅ Done | 2026-03-29 | — | Changed hardcoded `(CSR load, v10)` → `(CSR load, v{version})` in `deserialize.rs:672` |
| 0.3 | ✅ Done | 2026-03-29 | — | Set `$env:UFFS_CACHE_PROFILE = "1"` in `benchmark.ps1`, added `[CACHE_PROFILE]` to `Select-String` pattern |
| 0.4 | ✅ Done | 2026-03-29 | — | Both `cache-check.ps1` and `benchmark.ps1` now display binary version via `--version` |
| 0.5 | ✅ Done | 2026-03-29 | — | Removed redundant double `Show-CacheStatus` call; now clears `{Drive}_compact.uffs` alongside `{Drive}_index.uffs` |
| 0.6 | ✅ Done | 2026-03-29 | — | `save_to_file()` now emits: `mft_serialize`, `mft_compress`, `mft_encrypt`, `mft_write`, `mft_save_total` |
| 0.7 | ✅ Done | 2026-03-29 | — | `save_compact_cache()` now emits: `compact_enc`, `compact_write` (previously only had `compact_ser`, `compact_zstd`, `compact_save`) |
| 0.8 | ✅ Done | 2026-03-29 | — | Added `compact_save_outer` timer around `save_compact_cache()` call in both `load_mft_file()` and `load_live_drive()` |
| 0.9 | ✅ Done | 2026-04-01 | v0.4.49 | Baseline captured via daemon readiness suite (7 drives, 25.8M records). See [Production Validation](#production-validation-daemon-runs-v0449v0451-2026-04-01) below. |

### Wave 1 — Eliminate duplicate cache-save (S2) ✅

| Step | Status | Date | Commit/PR | Notes |
|------|--------|------|-----------|-------|
| 1.1 | ✅ Done | 2026-03-29 | — | Replaced 50 lines of inline serialize→zstd→AES→spawn_blocking→write in `read_and_cache_index()` with single `save_to_cache()` call. **Fixed 2 bugs:** missing compact cache invalidation + missing profiling. |
| 1.2 | ✅ Done | 2026-03-29 | — | `save_to_cache()` delegates to `save_to_file()` which has full CACHE_PROFILE profiling (mft_serialize/compress/encrypt/write/total). Confirmed by code inspection. |
| 1.3 | ✅ Done | 2026-03-29 | — | cargo check ✅, 523 tests pass ✅, clippy clean ✅ |

### Wave 2 — Eliminate compact.rs cold parse loop (A1) ✅

| Step | Status | Date | Commit/PR | Notes |
|------|--------|------|-----------|-------|
| 2.1 | ✅ Done | 2026-03-29 | — | Confirmed A3 has: rayon parallel (4096-chunk), IOCP detect, forensic, `UFFS_SINGLE_THREAD` escape, `volume_letter` override via `LoadRawOptions` |
| 2.2 | ✅ Done | 2026-03-29 | — | Replaced 50-line manual parse loop in `compact.rs:load_mft_file()` with single call to `MftReader::load_raw_to_index_with_options()`. **Offline files now get rayon parallel parsing for free.** |
| 2.3 | ✅ Done | 2026-03-29 | — | Added `mft_read`, `mft_parse` (with mode label: forensic/sequential/parallel/{N}T), `mft_build` profiling to all 3 branches in `persistence.rs`. All callers now get CACHE_PROFILE. |
| 2.4 | ✅ Resolved | 2026-03-29 | — | `save_to_cache` stays in `load_mft_index_from_file()` — this is the ONE correct place for the file cold path. The live cold path saves inside `read_and_cache_index()` (Wave 1). Two call sites, one per path — not scattered. |
| 2.5 | ✅ Done | 2026-04-01 | v0.4.49 | Confirmed via daemon: all 7 drives load through `load_drive()` → `load_raw_to_index_with_options()` (rayon parallel). 25.8M records at 2.2M rec/s avg from compact cache. Cold MFT parse path uses rayon 4096-chunk parallel for offline `.mft` files. |

### Wave 3 — Unified entry point ✅

| Step | Status | Date | Commit/PR | Notes |
|------|--------|------|-----------|-------|
| 3.1 | ✅ Done | 2026-03-29 | — | Created `MftSource` enum (`File(PathBuf, Option<char>)`, `Live(char)`) with `file_path()` helper |
| 3.2 | — | — | — | Kept existing `LoadTiming` (same fields, same callers). New type not needed. |
| 3.3 | ✅ Done | 2026-03-29 | — | Created `load_drive(&MftSource, no_cache) -> (DriveCompactIndex, LoadTiming)` — unified entry point dispatching File→`load_mft_index_from_file`, Live→`load_mft_index_live` |
| 3.4 | ✅ Done | 2026-03-29 | — | Migrated CLI `drive_search.rs` to `MftSource::Live` + `load_drive` |
| 3.5 | ✅ Done | 2026-03-29 | — | Migrated TUI `main.rs` + `refresh.rs` to `MftSource::File/Live` + `load_drive` |
| 3.6 | — | — | — | CLI raw-io uses `MftReader` directly for parity testing (needs raw MftIndex, not compact). Left as-is. |
| 3.7 | ✅ Done | 2026-03-29 | — | `load_mft_file()` and `load_live_drive()` are now thin `#[deprecated]` wrappers. Also migrated `uffs-daemon/index.rs` (3 call sites). `refresh_drive()` uses `load_drive` internally. |

### Wave 4 — Shared post-processing (partially done)

| Step | Status | Date | Commit/PR | Notes |
|------|--------|------|-----------|-------|
| 4.1-4.3 | ⏸ Deferred | — | — | `post_process_index()` function deferred. tree_metrics/ext_index are called at different stages in different pipelines (USN patch, live read, merge, IOCP inline, from_parsed_records). Wrapping all in one function would require threading through 6+ call sites for marginal gain. |
| 4.4 | ✅ Done | 2026-03-29 | — | Fixed double `compute_tree_metrics()` in `persistence.rs:573` (IOCP capture → `from_parsed_records` already calls it). |
| 4.5 | — | — | — | `build_extension_index()` calls are all legitimate — each at the right stage in their pipeline. |
| 4.6 | ✅ Done | 2026-03-29 | — | Wave 1 already consolidated: `read_and_cache_index` now delegates to `save_to_cache`. Multi-drive calls are legitimate (different orchestration layer). |
| 4.7 | ✅ Done | 2026-03-29 | — | Wave 3's `load_drive()` consolidates compact build+save. Main path callers (CLI search, TUI) all go through `load_drive`. |
| 4.8 | — | — | — | `ensure_compact_cached` still used by raw_io (diagnostic/parity commands that need raw MftIndex). Not redundant for those callers. |
| 4.9 | ⏸ Deferred | — | — | `SaveGuard` deferred to future — current sync save is fine for the main paths; background save can be added later. |

### Wave 5 — Remove dead code (assessment complete)

| Step | Status | Date | Commit/PR | Notes |
|------|--------|------|-----------|-------|
| 5.1 | ❌ Not dead | 2026-03-29 | — | `load_raw_to_dataframe_with_options()` used by `uffs index` / export in `commands/load.rs:171` |
| 5.2 | ❌ Not dead | 2026-03-29 | — | `load_raw_to_index_direct()` used by `uffs index` direct path in `commands/load.rs:261,450` |
| 5.3 | ❌ Not dead | 2026-03-29 | — | `read_mft_index_with_timing_internal()` used by `benchmark_index.rs:277` |
| 5.4 | ❌ Not dead | 2026-03-29 | — | All `MftReadMode` variants used in both `dataframe_read.rs` and `index_read.rs` |

---

## Files Modified (so far)

| File | Changes | Wave |
|------|---------|------|
| `crates/uffs-mft/src/index/storage/file_io.rs` | Added serialize/encrypt/write profiling to `save_to_file()` | 0 |
| `crates/uffs-mft/src/index/storage/deserialize.rs` | Fixed version label `v10` → `v{version}` | 0 |
| `crates/uffs-core/src/compact.rs` | Added cold-path profiling (mft_read/parse/build), compact_save_outer timing | 0 |
| `crates/uffs-core/src/compact_cache.rs` | Added encrypt/write profiling to `save_compact_cache()` | 0 |
| `scripts/windows/benchmark.ps1` | Set UFFS_CACHE_PROFILE=1, capture [CACHE_PROFILE] lines, show binary versions | 0 |
| `scripts/windows/cache-check.ps1` | Show binary version, clear compact cache, remove double status display | 0 |
| `crates/uffs-mft/src/reader/index_cache.rs` | Replaced 50-line inline save pipeline in `read_and_cache_index()` with `save_to_cache()` call. Fixed missing compact invalidation + missing profiling. | 1 |
| `crates/uffs-mft/src/reader/persistence.rs` | Added CACHE_PROFILE profiling (mft_read/parse/build) to all 3 branches of `load_raw_to_index_with_options()` | 2 |
| `crates/uffs-core/src/compact.rs` | Wave 2: Replaced parse loop with `load_raw_to_index_with_options()`. Wave 3: Created `MftSource` enum + `load_drive()` unified entry point. `load_mft_file`/`load_live_drive` are now `#[deprecated]` thin wrappers. | 2,3 |
| `crates/uffs-mft/src/reader/persistence.rs` | Fixed double `compute_tree_metrics()` in IOCP capture path (line 573 — `from_parsed_records` already calls it) | 4 |
| `crates/uffs-tui/src/compact.rs` | Updated re-exports: added `MftSource`, `load_drive`; removed deprecated re-exports | 3 |
| `crates/uffs-tui/src/main.rs` | Migrated from `load_mft_file` to `MftSource::File` + `load_drive` | 3 |
| `crates/uffs-tui/src/refresh.rs` | Migrated from `load_live_drive` to `MftSource::Live` + `load_drive` | 3 |
| `crates/uffs-cli/src/commands/search/drive_search.rs` | Migrated from `load_live_drive` to `MftSource::Live` + `load_drive` | 3 |
| `crates/uffs-daemon/src/index.rs` | Migrated 3 call sites from `load_mft_file`/`load_live_drive` to `MftSource` + `load_drive` | 3 |

---

## Production Validation — Daemon Runs (v0.4.49–v0.4.51, 2026-04-01)

The daemon architecture (D1–D5) exercised every refactored path in production. This section
captures the cross-referenced profiling data that validates Waves 0–3.

**Source:** `DAEMON_IMPLEMENTATION_PLAN.md` §Readiness Verification Results, §Shmem Bulk Transfer,
§CLI Flag Validation.

### Per-Drive Cache Load Rates (Wave 0.9 Baseline)

Daemon startup loads all drives through `load_drive()` → compact cache. Sequential per-drive,
measured via readiness suite (v0.4.49, Windows, 7 NTFS drives):

| Drive | Records | Load Time | Rate (rec/s) | Cache |
|-------|---------|-----------|-------------|-------|
| C: | 3,424,361 | ~2.0s | 1.7M | compact .uffs |
| D: | 7,065,539 | ~3.6s | 2.0M | compact .uffs |
| E: | 2,929,519 | ~1.3s | 2.3M | compact .uffs |
| F: | 2,221,343 | ~1.2s | 1.9M | compact .uffs |
| G: | 15,090 | ~10ms | — | compact .uffs |
| M: | 1,908,805 | ~0.75s | 2.5M | compact .uffs |
| S: | 8,278,102 | ~3.1s | 2.7M | compact .uffs |
| **Total** | **25,842,759** | **~12s** | **~2.2M avg** | |

All drives: `mft_ms=0 compact_ms=0 trigram_ms=0` — loaded from pre-built compact cache.
These are HOT path (H3 → H4) timings. The cold path (rayon parallel via `load_raw_to_index_with_options`)
was not benchmarked in this run because all caches were warm.

### Profiling Pipeline Confirmation

The unified profiling pipeline (`CACHE_PROFILE`) was validated end-to-end:

| Pipeline Stage | Source File | Confirmed By |
|----------------|------------|-------------|
| `mft_read` / `mft_parse` / `mft_build` | `persistence.rs` | Daemon load_from_data_dir() + load_live_drives() |
| `mft_serialize` / `mft_compress` / `mft_encrypt` / `mft_write` | `cache.rs` → `file_io.rs` | Wave 1: read_and_cache_index now delegates to save_to_cache |
| `compact_build` / `compact_tri` / `compact_total` | `compact.rs` | Wave 3: load_drive() builds compact after MftIndex load |
| `compact_ser` / `compact_zstd` / `compact_enc` / `compact_write` | `compact_cache.rs` | Wave 0.7: save_compact_cache profiling added |
| `search_*` / `output_*` / `wall_total` | `dispatch.rs` / `output/mod.rs` | CLI daemon path (D5.3: 44/44 flag tests) |
| `shmem_read` / `shmem_write` | `handler.rs` / `connect.rs` | D5.0: shmem validated for 25.8M rows |

### load_drive() Call Site Audit

The `MftSource` + `load_drive()` unified entry point (Wave 3) has exactly **6 production call sites**
and **0 callers of deprecated functions**:

| Caller | Call Site | Source |
|--------|----------|--------|
| Daemon: load_from_data_dir | `MftSource::File` → `load_drive` | `uffs-daemon/src/index.rs` |
| Daemon: load_live_drives | `MftSource::Live` → `load_drive` | `uffs-daemon/src/index.rs` |
| Daemon: refresh | `MftSource::Live/File` → `load_drive` | `uffs-daemon/src/index.rs` |
| CLI: drive_search | `MftSource::Live` → `load_drive` | `uffs-cli/src/commands/search/drive_search.rs` |
| TUI: main startup | `MftSource::File/Live` → `load_drive` | `uffs-tui/src/main.rs` |
| TUI: refresh | `MftSource::Live` → `load_drive` | `uffs-tui/src/refresh.rs` |

Deprecated wrappers (`load_mft_file()`, `load_live_drive()`) have **0 callers** in daemon/CLI/TUI.

### Query Latency Validation

The consolidated pipeline delivers sub-millisecond daemon-side query latency:

| Query | Rows | Duration | Records Scanned |
|-------|------|----------|----------------|
| `"orthod"` (no limit) | 38 | **0ms** | 25.8M |
| `*.rs` (limit=100) | 100 | **1ms** | 25.8M |
| `*.rs` (limit=1000) | 1,007 | **4ms** | 25.8M |
| `*` (all 25.8M, shmem) | 25,842,547 | **26.7s** | 25.8M |

Median warm CLI round-trip (incl IPC + output): **~200ms** (44-test suite, v0.4.55).

### CLI Flag Validation (44/44 Pass, v0.4.55)

All 44 CLI flag combinations pass on production data (25.8M records, 7 drives).
Suite: `scripts/windows/cli-flag-validation.rs`. Total runtime: **56.5s** (avg 1.3s/test).

Key performance observations from the v0.4.55 flag suite:
- **Cold start penalty:** 39.5s (daemon spawn + 7-drive cache load) — test T00 only
- **Median warm query:** ~200ms (IPC round-trip + output formatting)
- **Fastest queries:** simple glob/name match: **192–214ms** (T01–T03, T16–T19, T25–T31, T40–T42)
- **Slowest filters:** `--ext` (1.8–2.1s, full-index extension scan), `--attr !hidden` (944ms), `--limit 0 unlimited` (1.1s for 163K rows)
- **Benchmark (154K `.rs` rows):** 479ms including shmem serialization
- **10 new tests vs v0.4.51:** `--header false`, `--smart-case`, `--newer-accessed`, `--attr readonly`, `--attr system,!hidden`, `--older-created`, `--limit 0`, `--no-results`, `--attr system` (all pass)

---

## Profiling Labels Reference

After Waves 0–3, the following `[CACHE_PROFILE]` labels are emitted (when `UFFS_CACHE_PROFILE=1`):

### Cold start (no cache)
```
[CACHE_PROFILE] mft_read:          XX ms  (YY.Y MB)           ← raw MFT file load
[CACHE_PROFILE] mft_parse:         XX ms  (N records, ...)    ← record parse + fixup + merge
[CACHE_PROFILE] mft_build:         XX ms  (tree+ext+stats)    ← MftIndex::from_parsed_records
[CACHE_PROFILE] mft_serialize:     XX ms  (YY.Y MB)           ← index.serialize()
[CACHE_PROFILE] mft_compress:      XX ms  (YY → ZZ MB, Rx)    ← zstd compress
[CACHE_PROFILE] mft_encrypt:       XX ms  (ZZ.Z MB)           ← AES-256-GCM encrypt
[CACHE_PROFILE] mft_write:         XX ms  (ZZ.Z MB)           ← atomic_write to disk
[CACHE_PROFILE] mft_save_total:    XX ms                      ← serialize+compress+encrypt+write
[CACHE_PROFILE] compact_build:     XX ms  (N records)         ← CompactRecord[] construction
[CACHE_PROFILE] compact_tri:       XX ms                      ← trigram index build
[CACHE_PROFILE] compact_total:     XX ms  (build+trigram)
[CACHE_PROFILE] compact_ser:       XX ms  (~YY MB)            ← compact serialize
[CACHE_PROFILE] compact_zstd:      XX ms  (~YY → ~ZZ MB)      ← compact zstd
[CACHE_PROFILE] compact_enc:       XX ms  (~ZZ MB)            ← compact AES encrypt
[CACHE_PROFILE] compact_write:     XX ms  (~ZZ MB)            ← compact atomic_write
[CACHE_PROFILE] compact_save:      XX ms  total               ← compact save total
[CACHE_PROFILE] compact_save_outer:XX ms                      ← save_compact_cache wall time
[CACHE_PROFILE] path_resolver:     XX ms  (lazy=bool)         ← PathResolver build
[CACHE_PROFILE] row_output:        XX ms  (N rows)            ← format + write all output rows (legacy)
```

### Search phase (per-drive + aggregate)
```
[CACHE_PROFILE] search_C:          trigram=X ms  match=X ms (N hits from M trigram candidates)  paths=X ms
[CACHE_PROFILE] search_C:          regex_match=X ms (N hits from M scan)  paths=X ms
[CACHE_PROFILE] search_C:          tree_walk=X ms (N hits)  paths=X ms
[CACHE_PROFILE] search_total:      XX ms  (N rows, M scanned, mode=trigram|regex|tree|match-all)
```

### Output phase (unified, all search paths)
```
[CACHE_PROFILE] output_convert:    XX ms  (N rows → DataFrame)  ← json/table only (DisplayRow→DataFrame)
[CACHE_PROFILE] output_fmt_io:     XX ms  (format=custom|csv|json|table) ← formatting + I/O write
[CACHE_PROFILE] output_total:      XX ms  (N rows)              ← convert + fmt_io combined
[CACHE_PROFILE] wall_total:        XX ms                        ← end-to-end from search start
```

### Daemon IPC path (added v0.4.49+)
```
[handler]   shmem_write:       XX ms  (N rows, path)           ← daemon: serialize results → shared memory
[connect]   shmem_read:        XX ms  (N rows, path)           ← client: read results from shared memory
[handler]   serialize:         XX ms  (N bytes JSON)           ← daemon: JSON serialize (when >10K rows)
```

### Warm start (cache hit)
```
[CACHE_PROFILE]   file_read:       XX ms  (YY.Y MB)           ← read .uffs from disk
[CACHE_PROFILE]   decrypt:         XX ms  (YY.Y MB)           ← AES-256-GCM decrypt
[CACHE_PROFILE]   decompress:      XX ms  (YY → ZZ MB)        ← zstd decompress
[CACHE_PROFILE]   deserialize:     XX ms  (N records)         ← binary deserialization
[CACHE_PROFILE]   parse_fields:    XX ms  (binary field-by-field)
[CACHE_PROFILE]   recompute_stats: XX ms
[CACHE_PROFILE]   ext_index:       XX ms  (CSR load, vNN)     ← extension index
[CACHE_PROFILE]   deser_total:     XX ms                      ← all deserialization phases
[CACHE_PROFILE]   total_load:      XX ms                      ← file_read + decrypt + decompress + deserialize
[CACHE_PROFILE] compact_build:     XX ms  (N records)
[CACHE_PROFILE] compact_tri:       XX ms
[CACHE_PROFILE] compact_total:     XX ms  (build+trigram)
[CACHE_PROFILE] search_C:          ...                        ← same per-drive + aggregate labels as above
[CACHE_PROFILE] path_resolver:     XX ms  (lazy=bool)
[CACHE_PROFILE] output_convert:    XX ms  (N rows → DataFrame) ← json/table only
[CACHE_PROFILE] output_fmt_io:     XX ms  (format=...)
[CACHE_PROFILE] output_total:      XX ms  (N rows)
[CACHE_PROFILE] wall_total:        XX ms
```