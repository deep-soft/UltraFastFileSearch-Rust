# Daemon Concurrent Query Support — Implementation Guide

> **Audience:** Junior developer implementing this feature.
> **Goal:** Allow multiple search queries to execute in parallel against the
> daemon's in-memory index, eliminating the current head-of-line blocking.

---

## 1. Problem Statement

### What's broken

Every search acquires an **exclusive write lock** on `MultiDriveBackend`:

```rust
// crates/uffs-daemon/src/index.rs — IndexManager::search()
let mut backend = self.backend.write().await;   // ← blocks ALL other queries
backend.sort_column = sort_column;               // ← mutates shared state
backend.sort_desc = sort_desc;                   // ← mutates shared state
let result = backend.search(req);                // ← mutates last_results + drives
drop(backend);
```

While one search runs (~20–200 ms), **every other request** — including
read-only ones like `drives()`, `status()`, `info()` — is blocked.

### Impact

| Connections | Behavior                                                |
|:-----------:|:--------------------------------------------------------|
| 1           | Fine — single CLI/TUI user                              |
| 2–5         | Noticeable queueing — search 2 waits for search 1       |
| 10+         | Severe head-of-line blocking — 200 ms × 10 = 2 s tail  |
| 100+        | Unusable — timeout cascades (MCP, web UI, multi-user)   |

### Root cause: five mutable fields

`MultiDriveBackend` mixes **shared immutable index data** with **per-query
mutable state**:

```rust
// crates/uffs-core/src/search/backend.rs
pub struct MultiDriveBackend {
    // ── SHARED (immutable after load) ─────────────────────
    pub drives: Vec<DriveCompactIndex>,    // ~80 bytes × 26M records

    // ── PER-QUERY (changes every search) ──────────────────
    pub last_results: Vec<DisplayRow>,     // TUI re-sort cache
    pub sort_column: FieldId,              // set from query params
    pub sort_desc: bool,                   // set from query params
    pub extra_sort_tiers: Vec<SortSpec>,   // set from query params
}
```

| Field              | Why mutated                                 | Who needs it   |
|:-------------------|:--------------------------------------------|:---------------|
| `sort_column`      | Set from query params before search         | This query only |
| `sort_desc`        | Set from query params before search         | This query only |
| `extra_sort_tiers` | Set from query params before search         | This query only |
| `last_results`     | Cloned from search output for TUI re-sort   | TUI only        |
| `drives`           | Temporarily partitioned (drive-swap hack)   | This query only |

**None of these mutations are needed by other concurrent queries.**

### The drive-swap hack

When a `drives_filter` is active, `search()` **destructively partitions**
`self.drives`:

```rust
// crates/uffs-core/src/search/backend.rs — inside search()
let stashed_drives = if drives_filter.is_empty() {
    None
} else {
    let all = core::mem::take(&mut self.drives);  // ← empties the Vec!
    let (keep, rest) = all.into_iter().partition(|dr| ...);
    self.drives = keep;                            // ← only matching drives
    Some(rest)
};
// ... search ...
if let Some(rest) = stashed_drives {
    self.drives.extend(rest);                      // ← restore
}
```

A concurrent query arriving mid-swap would see **missing drives**.

### The SearchFilters mutation

`search()` takes `search_filters: &mut SearchFilters`. The mutation is
`resolve_ext_ids_for_drive()` — pre-resolving extension strings to `u16`
IDs. This is per-query state that must not be shared.

---

## 2. Solution Overview

Split `MultiDriveBackend` into:

1. **`DriveIndex`** — shared, immutable, `Arc`-wrapped. Contains only
   `Vec<Arc<DriveCompactIndex>>`.
2. **Per-query local variables** — sort column, sort direction, filters,
   result buffer. Created on the stack for each query, zero sharing.
3. **Free function `search_index()`** — takes `&DriveIndex` (shared borrow)
   + per-query params. No `&mut self`. No lock needed for search.

The daemon stores the index as:

```rust
index: Arc<RwLock<Arc<DriveIndex>>>
//     │    │        └─ snapshot pointer — cloned by each query (< 1 μs)
//     │    └─ protects the pointer swap during refresh / load
//     └─ shared across handler tasks
```

- **Queries:** `.read()` → clone the inner `Arc<DriveIndex>` → drop the
  lock immediately → search on the cloned `Arc` with zero contention.
- **Refresh:** `.write()` → build a new `Arc<DriveIndex>` → swap in → old
  one is dropped when all in-flight queries finish (refcount → 0).

---

## 3. Implementation Phases

**Do these in order. Each phase is independently shippable and testable.**


### Phase 1 — Create `DriveIndex` and the free function (uffs-core only)

**Files to change:**
- `crates/uffs-core/src/search/backend.rs`
- `crates/uffs-core/src/search/backend_tests.rs`
- `crates/uffs-core/src/search/mod.rs` (re-export)

**Risk:** Low — new types alongside existing code. Nothing breaks.
**Effort:** ~4 hours.

#### Step 1.1 — Define `DriveIndex`

Add to `backend.rs`, **before** `MultiDriveBackend`:

```rust
use std::sync::Arc;

/// Shared, immutable index snapshot. Holds all loaded drives.
///
/// Wrapped in `Arc` so concurrent queries hold cheap references.
/// Mutations (load, refresh, remove) create a **new** `DriveIndex`
/// and swap the `Arc` pointer — in-flight queries keep the old
/// snapshot until they finish.
pub struct DriveIndex {
    /// Per-drive compact indices, each individually Arc-wrapped so
    /// adding/removing a single drive copies only ~7 Arc pointers
    /// (~56 bytes), not the 1.8 GB of record data.
    pub drives: Vec<Arc<DriveCompactIndex>>,
}

impl DriveIndex {
    /// Total record count across all loaded drives.
    #[must_use]
    pub fn total_records(&self) -> usize {
        self.drives.iter().map(|dr| dr.records.len()).sum()
    }

    /// List loaded drives with record counts.
    #[must_use]
    pub fn drive_summary(&self) -> Vec<(char, usize)> {
        self.drives
            .iter()
            .map(|dr| (dr.letter, dr.records.len()))
            .collect()
    }
}
```

**Why `Vec<Arc<DriveCompactIndex>>`?** Without per-drive `Arc`, swapping a
single drive during refresh would clone the entire `Vec<DriveCompactIndex>`
including all record data (~250 MB per drive). With per-drive `Arc`,
cloning the `Vec` copies only the `Arc` pointers (~8 bytes each).

#### Step 1.2 — Extract the free function `search_index()`

Create a **new public free function** in `backend.rs`. This is a
mechanical extraction of the current `MultiDriveBackend::search()` body
with `&mut self` replaced by explicit parameters:

```rust
/// Execute a search against a shared `DriveIndex` snapshot.
///
/// All per-query state (sort, filters, limit) is passed as parameters.
/// This function never mutates the index — safe to call from multiple
/// threads/tasks simultaneously.
pub fn search_index(
    index: &DriveIndex,
    req: SearchRequest<'_>,
    sort_column: FieldId,
    sort_desc: bool,
    extra_sort_tiers: &[SortSpec],
) -> SearchResult {
    // ... body extracted from MultiDriveBackend::search() ...
}
```

**Line-by-line changes inside the body** (relative to the current
`MultiDriveBackend::search()` at `backend.rs:265`):

| Current code | Replacement | Why |
|:-------------|:------------|:----|
| `self.sort_column` | `sort_column` (parameter) | Per-query, not shared |
| `self.sort_desc` | `sort_desc` (parameter) | Per-query, not shared |
| `self.extra_sort_tiers` | `extra_sort_tiers` (parameter) | Per-query, not shared |
| `self.drives` | `index.drives` | Shared snapshot |
| `self.last_results = rows.clone()` | *delete this line* | Daemon never re-sorts |
| `self.drives.first()` (for `CaseFold`) | `index.drives.first()` | Same |
| Drive-swap hack (see below) | Filter iterator (see below) | No mutation |

**Replace the drive-swap hack:**

```rust
// BEFORE (mutates self.drives — lines ~305-325 in backend.rs):
let stashed_drives = if drives_filter.is_empty() {
    None
} else {
    let all = core::mem::take(&mut self.drives);
    let (keep, rest) = all.into_iter().partition(|dr| ...);
    self.drives = keep;
    Some(rest)
};
// ... search ...
if let Some(rest) = stashed_drives {
    self.drives.extend(rest);
}

// AFTER (zero mutation):
let active_drives: Vec<&DriveCompactIndex> = index.drives.iter()
    .filter(|dr| {
        drives_filter.is_empty()
            || drives_filter.iter()
                .any(|&f| f.eq_ignore_ascii_case(&dr.letter))
    })
    .map(|arc| arc.as_ref())
    .collect();
```

Then pass `&active_drives` to the internal search functions instead of
`&self.drives`.

#### Step 1.3 — Adapt internal functions to accept `&[&DriveCompactIndex]`

The search dispatches to these internal functions in `query.rs`:

| Function | Current signature | Change needed |
|:---------|:-----------------|:--------------|
| `collect_global_top_n` | `drives: &[DriveCompactIndex]` | Change to `drives: &[&DriveCompactIndex]` |
| `search_compact_drive` | `drive: &DriveCompactIndex` | No change — called per-drive |
| `search_compact_drive_regex` | `drive: &DriveCompactIndex` | No change |
| `search_compact_drive_tree` | `drive: &DriveCompactIndex` | No change |

Only `collect_global_top_n` needs a signature change because it iterates
the whole drives array. The per-drive functions are called with
`drive.as_ref()` which auto-derefs `Arc<DriveCompactIndex>` to
`&DriveCompactIndex`.

**How to update `collect_global_top_n`:**

Find the function in `query.rs`. Its `drives` parameter currently has type
`&[DriveCompactIndex]`. Change it to `&[&DriveCompactIndex]`. The body
iterates with `drives.iter()` → `for drive in drives` — since `drive` is
now `&&DriveCompactIndex`, you may need to add a single `*` dereference in
the few places that pass `drive` to sub-functions.

Search the body for `.par_iter()` calls on `drives` — these also work
with `&[&DriveCompactIndex]` since rayon's `par_iter` on a slice yields
`&&DriveCompactIndex`, and `DriveCompactIndex` is accessed through
auto-deref.

**Alternatively**, use a generic parameter:

```rust
fn collect_global_top_n<D: AsRef<DriveCompactIndex> + Sync>(
    drives: &[D],
    ...
) -> Vec<DisplayRow> {
    for drive in drives {
        let drive: &DriveCompactIndex = drive.as_ref();
        // ... rest unchanged ...
    }
}
```

This works for both `DriveCompactIndex`, `&DriveCompactIndex`, and
`Arc<DriveCompactIndex>`.

#### Step 1.4 — Keep `MultiDriveBackend::search()` working

**Do not delete** `MultiDriveBackend::search()` in this phase. It still
works for the TUI. You can optionally refactor it to delegate to
`search_index()` internally:

```rust
impl MultiDriveBackend {
    pub fn search(&mut self, req: SearchRequest<'_>) -> SearchResult {
        // Wrap self.drives in a temporary DriveIndex.
        // This is fine for the TUI — single-threaded, no contention.
        let temp_index = DriveIndex {
            drives: self.drives.iter()
                .map(|d| Arc::new(d.clone()))  // only for TUI path
                .collect(),
        };
        // ... or keep the existing body. Either is fine for Phase 1.
    }
}
```

**Recommended:** Keep the existing body unchanged for Phase 1. Refactoring
it to use `search_index()` is a nice cleanup but not required.

#### Step 1.5 — Re-export from `search/mod.rs`

```rust
// crates/uffs-core/src/search/mod.rs
pub use backend::{DriveIndex, search_index, SearchRequest, SearchResult};
```

Check what's already re-exported and add the new types.

#### Step 1.6 — Write tests

Add to `backend_tests.rs`:

```rust
use std::sync::Arc;
use super::*;

#[test]
fn search_index_returns_results() {
    let (drive_c, drive_d) = build_two_test_drives();
    let index = DriveIndex {
        drives: vec![Arc::new(drive_c), Arc::new(drive_d)],
    };
    let mut filters = SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest::new("*", &mut filters),
        FieldId::Name,
        false,
        &[],
    );
    assert!(!result.rows.is_empty(), "match-all must return rows");
}

#[test]
fn search_index_drives_filter_excludes_non_matching() {
    let (drive_c, drive_d) = build_two_test_drives();
    let index = DriveIndex {
        drives: vec![Arc::new(drive_c), Arc::new(drive_d)],
    };
    let mut filters = SearchFilters::default();
    let result = search_index(
        &index,
        SearchRequest {
            drives_filter: &['C'],
            ..SearchRequest::new("*", &mut filters)
        },
        FieldId::Name,
        false,
        &[],
    );
    assert!(
        result.rows.iter().all(|r| r.drive == 'C'),
        "drive filter must exclude D: results"
    );
}

#[test]
fn search_index_is_safe_to_call_concurrently() {
    let (drive_c, drive_d) = build_two_test_drives();
    let index = Arc::new(DriveIndex {
        drives: vec![Arc::new(drive_c), Arc::new(drive_d)],
    });
    // Spawn two rayon tasks searching with different sort orders.
    let idx1 = Arc::clone(&index);
    let idx2 = Arc::clone(&index);
    let (r1, r2) = rayon::join(
        || {
            let mut f = SearchFilters::default();
            search_index(
                &idx1,
                SearchRequest::new("*", &mut f),
                FieldId::Size, true, &[],
            )
        },
        || {
            let mut f = SearchFilters::default();
            search_index(
                &idx2,
                SearchRequest::new("*", &mut f),
                FieldId::Name, false, &[],
            )
        },
    );
    assert!(!r1.rows.is_empty());
    assert!(!r2.rows.is_empty());
    // Different sort orders → different row ordering.
    if !r1.rows.is_empty() && !r2.rows.is_empty() {
        // Just verify both completed — ordering checked by other tests.
    }
}
```

#### Step 1.7 — Verification

```bash
cargo nextest run -p uffs-core        # all existing + new tests pass
cargo clippy -p uffs-core -- -D warnings  # clean
```

All existing tests must pass unchanged — we only added new code.


### Phase 2 — Wire the daemon to use `DriveIndex` + `search_index()`

**Files to change:**
- `crates/uffs-daemon/src/index.rs`

**Files NOT changed:**
- `crates/uffs-daemon/src/handler.rs` — calls `IndexManager` methods
  whose signatures stay the same.

**Risk:** Low — daemon-only change. TUI keeps its own `MultiDriveBackend`.
**Effort:** ~6 hours.

#### Step 2.1 — Change `IndexManager` fields

```rust
// BEFORE:
pub struct IndexManager {
    backend: RwLock<MultiDriveBackend>,
    status: RwLock<DaemonStatus>,
    start_time: Instant,
    data_dir: Option<PathBuf>,
    // ... other fields ...
}

// AFTER:
pub struct IndexManager {
    /// Shared index snapshot. Read lock to clone Arc (< 1 μs),
    /// write lock only during load/refresh/remove (< 1 μs swap).
    index: RwLock<Arc<DriveIndex>>,
    status: RwLock<DaemonStatus>,
    start_time: Instant,
    data_dir: Option<PathBuf>,
    // ... other fields unchanged ...
}
```

Update `IndexManager::new()`:

```rust
pub fn new(data_dir: Option<PathBuf>) -> Self {
    Self {
        index: RwLock::new(Arc::new(DriveIndex { drives: Vec::new() })),
        // ... rest unchanged ...
    }
}
```

#### Step 2.2 — Add helper methods for atomic drive mutations

```rust
impl IndexManager {
    /// Add a newly-loaded drive to the shared index (atomic swap).
    async fn add_drive(&self, new_drive: DriveCompactIndex) {
        let mut guard = self.index.write().await;
        // Clone the Vec of Arc pointers (~56 bytes), not the data.
        let mut drives = guard.drives.clone();
        drives.push(Arc::new(new_drive));
        *guard = Arc::new(DriveIndex { drives });
        // Write lock released. Old snapshot freed when last in-flight
        // query using it finishes (Arc refcount → 0).
    }

    /// Replace a drive (refresh) or remove it.
    async fn replace_drive(&self, letter: char, new_drive: DriveCompactIndex) {
        let mut guard = self.index.write().await;
        let mut drives: Vec<Arc<DriveCompactIndex>> = guard.drives
            .iter()
            .filter(|d| !d.letter.eq_ignore_ascii_case(&letter))
            .cloned()            // clones Arc pointers, not data
            .collect();
        drives.push(Arc::new(new_drive));
        *guard = Arc::new(DriveIndex { drives });
    }

    /// Remove a drive from the index.
    async fn remove_drive(&self, letter: char) {
        let mut guard = self.index.write().await;
        let drives: Vec<Arc<DriveCompactIndex>> = guard.drives
            .iter()
            .filter(|d| !d.letter.eq_ignore_ascii_case(&letter))
            .cloned()
            .collect();
        *guard = Arc::new(DriveIndex { drives });
    }
}
```

#### Step 2.3 — Change `IndexManager::search()`

This is the critical change. Replace the write lock with a read lock:

```rust
// BEFORE:
pub async fn search(&self, params: &SearchParams) -> SearchResponse {
    // ... param parsing ...
    let mut backend = self.backend.write().await;   // EXCLUSIVE
    backend.sort_column = sort_column;
    backend.sort_desc = sort_desc;
    backend.extra_sort_tiers = extra_sort_tiers;
    let result = backend.search(req);
    drop(backend);                                   // held 20-200ms
    // ... build response ...
}

// AFTER:
pub async fn search(&self, params: &SearchParams) -> SearchResponse {
    // ... param parsing (unchanged) ...

    // ── Snapshot the index (< 1 μs) ───────────────────────
    let snapshot: Arc<DriveIndex> = {
        let guard = self.index.read().await;
        Arc::clone(&guard)
    };
    // Lock dropped — other queries proceed immediately.

    // ── Build per-query state (all local) ─────────────────
    let mut filters = SearchFilters::from_params(/* ... same as before ... */);
    Self::compile_predicates_into_filters(&mut filters, &effective_params.predicates);
    let (sort_column, sort_desc, extra_sort_tiers) = /* same parsing */;

    // ── Execute search (no lock held) ─────────────────────
    let result = search_index(
        &snapshot,
        SearchRequest {
            pattern: &effective_params.pattern,
            case_sensitive: effective_params.case_sensitive,
            whole_word: effective_params.whole_word,
            match_path: effective_params.match_path,
            result_limit: search_limit,
            filter_mode,
            search_filters: &mut filters,
            drives_filter: &effective_params.drives,
        },
        sort_column,
        sort_desc,
        &extra_sort_tiers,
    );
    // snapshot dropped — old data freed if refresh happened.

    // ... build response (unchanged) ...
}
```

#### Step 2.4 — Update `load_from_data_dir()`

Find every `backend.drives.push(drive_index)` and replace with
`self.add_drive(drive_index).await`.

Specifically, look for this pattern:

```rust
// BEFORE (appears ~2 times in load_from_data_dir):
let mut backend = self.backend.write().await;
backend.drives.push(drive_index);
drop(backend);

// AFTER:
self.add_drive(drive_index).await;
```

#### Step 2.5 — Update `refresh()`

```rust
// BEFORE:
let mut backend = self.backend.write().await;
backend.drives.retain(|d| d.letter != letter);
// ... load new drive ...
backend.drives.push(new_drive);

// AFTER:
// ... load new drive (no lock held!) ...
self.replace_drive(letter, new_drive).await;
```

**Key improvement:** The MFT read / cache load (which can take seconds)
now happens **outside** any lock. Only the final pointer swap holds the
write lock for < 1 μs.

#### Step 2.6 — Update `load_live_drives()`

Same pattern as `load_from_data_dir`:

```rust
// BEFORE:
let mut backend = self.backend.write().await;
backend.drives.push(drive_index);

// AFTER:
self.add_drive(drive_index).await;
```

#### Step 2.7 — Update read-only methods

These methods currently use `self.backend.read().await`:

| Method | What to change |
|:-------|:---------------|
| `drives()` | `self.index.read().await` → clone Arc → iterate snapshot |
| `info()` | `self.index.read().await` → clone Arc → search snapshot |
| `has_drives()` | `self.index.read().await` → check `!guard.drives.is_empty()` |
| `total_records()` | `self.index.read().await` → `guard.total_records()` |
| `status()` | Uses `self.status` — no change needed |
| `events` | Independent field — no change needed |
| `drive_timings` | Independent `RwLock` — no change needed |

Example for `drives()`:

```rust
pub async fn drives(&self) -> DrivesResponse {
    let snapshot = {
        let guard = self.index.read().await;
        Arc::clone(&guard)
    };
    DrivesResponse {
        drives: snapshot.drives.iter().map(|dr| DriveInfo {
            letter: dr.letter,
            records: dr.records.len(),
            // ... same fields as before ...
        }).collect(),
    }
}
```

#### Step 2.8 — Remove old `MultiDriveBackend` import

Remove the `use uffs_core::search::backend::MultiDriveBackend` import
from `index.rs` (it's no longer used by the daemon). Add imports for
`DriveIndex`, `search_index`.

#### Step 2.9 — Verification

```bash
cargo check -p uffs-daemon
cargo nextest run -p uffs-daemon
cargo clippy -p uffs-daemon -- -D warnings

# Manual smoke test:
# Terminal 1: start daemon
# Terminal 2: uffs search "*.exe" --limit 10
# Terminal 3: uffs search "*.dll" --limit 10   (simultaneously)
# Both should return results without delay.
```


### Phase 3 — Raise connection limits and add safety valves

**Files to change:**
- `crates/uffs-daemon/src/ipc.rs`
- `crates/uffs-daemon/src/index.rs` (add semaphore)

**Risk:** Medium — memory pressure from concurrent large result sets.
**Effort:** ~2 hours.

#### Step 3.1 — Raise `MAX_CONNECTIONS`

```rust
// crates/uffs-daemon/src/ipc.rs
// BEFORE:
const MAX_CONNECTIONS: usize = 32;

// AFTER:
const MAX_CONNECTIONS: usize = 256;
```

#### Step 3.2 — Add a concurrent-search semaphore

Each search uses CPU (rayon `par_iter`) and allocates result memory.
Cap simultaneous searches to prevent CPU/memory exhaustion:

```rust
// crates/uffs-daemon/src/index.rs
use tokio::sync::Semaphore;

pub struct IndexManager {
    index: RwLock<Arc<DriveIndex>>,
    /// Limits simultaneous search operations.
    search_semaphore: Semaphore,
    // ... rest unchanged ...
}

impl IndexManager {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        Self {
            index: RwLock::new(Arc::new(DriveIndex { drives: Vec::new() })),
            search_semaphore: Semaphore::new(num_cpus::get()),
            // ...
        }
    }

    pub async fn search(&self, params: &SearchParams) -> SearchResponse {
        // Acquire permit — blocks if too many concurrent searches.
        let _permit = self.search_semaphore.acquire().await
            .expect("semaphore closed");

        let snapshot = { /* clone Arc */ };
        let result = search_index(&snapshot, ...);
        // _permit dropped → next waiting search proceeds.
        // ... build response ...
    }
}
```

**Why `num_cpus`?** Each search uses rayon's thread pool. More
simultaneous searches than CPU cores just adds context-switch overhead.

#### Step 3.3 — Add per-query timeout

```rust
pub async fn search(&self, params: &SearchParams) -> SearchResponse {
    let _permit = self.search_semaphore.acquire().await.expect("closed");

    let snapshot = { /* clone Arc */ };

    // Run CPU-bound search on a blocking thread with timeout.
    let search_result = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::task::spawn_blocking(move || {
            search_index(&snapshot, req, sort_column, sort_desc, &extra_tiers)
        }),
    ).await;

    match search_result {
        Ok(Ok(result)) => { /* build response */ }
        Ok(Err(panic)) => { /* task panicked — return error response */ }
        Err(_timeout) => { /* return timeout error response */ }
    }
}
```

**Note:** `spawn_blocking` is needed because `search_index` uses rayon
`par_iter` which blocks the current thread. Without it, the tokio runtime
thread would be blocked. The current daemon already uses `spawn_blocking`
for MFT loads but NOT for searches — adding it here is a correctness fix.

#### Step 3.4 — Verification

```bash
cargo nextest run -p uffs-daemon
# Stress test (PowerShell, on Windows):
# 1..20 | ForEach-Object -Parallel { uffs search "*.exe" --limit 5 }
```

---

## 4. What NOT to change

### The TUI keeps `MultiDriveBackend`

The TUI uses the daemon client (`search_via_daemon`), so
`MultiDriveBackend` is only used for TUI-local operations like `sort()`,
`cycle_sort()`, `toggle_sort_direction()` which re-sort `last_results`
without re-searching.

**Do not delete `MultiDriveBackend`.** It still provides:
- `sort()` / `cycle_sort()` / `toggle_sort_direction()` for TUI re-sort
- `drive_summary()` / `total_records()` helpers
- `SearchRequest` struct and related types (shared by the free function)

### Existing test files

`backend_tests.rs` and `query_tests.rs` test `MultiDriveBackend::search()`.
These continue to work unchanged. Add **new** tests for `search_index()`.

---

## 5. Memory & Performance Characteristics

### Lock hold times (after implementation)

| Operation         | Lock type | Duration             | Blocks queries? |
|:------------------|:----------|:---------------------|:----------------|
| Search            | None      | 0                    | No              |
| Clone Arc (query) | `read()`  | < 1 μs               | No              |
| Add drive         | `write()` | < 1 μs (pointer swap)| < 1 μs          |
| Refresh drive     | `write()` | < 1 μs (pointer swap)| < 1 μs          |
| Remove drive      | `write()` | < 1 μs (pointer swap)| < 1 μs          |

### Memory overhead

- **Per-query results:** 10–1000 rows × ~200 bytes = 2–200 KB each.
  100 concurrent queries = ~20 MB. Negligible vs 1.8 GB index.
- **Dual snapshots during refresh:** Briefly two copies of one drive
  (~250 MB for 3.5M records). Freed when last in-flight query on the
  old snapshot finishes.
- **Worst case:** All 7 drives refreshing simultaneously while queries
  hold old snapshots = 2× memory (~3.6 GB). Bounded and short-lived.
- **Arc<DriveCompactIndex> overhead:** 16 bytes per drive (strong + weak
  counters). 7 drives = 112 bytes. Negligible.

### Rayon thread pool

`search_index()` uses `par_iter()` (rayon) to search drives in parallel.
Multiple concurrent queries share the global rayon thread pool. Rayon's
work-stealing scheduler handles this — drives are read-only during search,
so there's no data-race risk. The `search_semaphore` (Phase 3) prevents
more than `num_cpus` queries from competing for the pool simultaneously.


---

## 6. Mutation Scenarios (reference diagrams)

### Adding a drive (loading D: while queries run against C:)

```
Timeline:
───────────────────────────────────────────────────────────────
Query A  ──read()──clone Arc₁──drop lock──search(C:)──→ done
                                                       (still using Arc₁)
Load D:  ──────[build DriveCompactIndex on thread pool]───┐
                                                           │
Query B  ──read()──clone Arc₁──drop lock──search(C:)──→   │
                                                           │
         ┌── write() ──┐                                   │
Swap:    │ Arc₂ = Arc₁.drives + D: (< 1 μs)               │
         │ *index = Arc₂  │◄──────────────────────────────┘
         └────────────────┘

Query C  ──read()──clone Arc₂──drop lock──search(C: + D:)──→ done

Arc₁ refcount: Query A(1) + Query B(1) = 2. Freed when both finish.
Arc₂ refcount: index(1) + Query C(1) = 2.
```

### Refreshing a drive (C: reloaded while queries use old C:)

```
Timeline:
───────────────────────────────────────────────────────────────
Query A  ──read()──clone Arc₁──drop lock──search(old C:)──→ done
                                                            (correct: uses
                                                             consistent snapshot)
Refresh  ──[read MFT / apply USN journal — takes 2-5 seconds]──┐
                                                                 │
Query B  ──read()──clone Arc₁──drop lock──search(old C:)──→     │
                                                                 │
         ┌── write() ──┐                                         │
Swap:    │ Arc₂ = [new C:, D:, E:, ...] (< 1 μs)                │
         │ *index = Arc₂  │◄────────────────────────────────────┘
         └────────────────┘

Query C  ──read()──clone Arc₂──drop lock──search(new C:)──→ done

Old C: data: freed when Query A and Query B finish.
```

### Removing a drive

Same pattern — build new `Vec<Arc<DriveCompactIndex>>` without the
removed drive, swap. In-flight queries on the removed drive complete
normally with consistent old data.

---

## 7. Complete Diff Checklist

Use this as a PR checklist. Check off each item as you implement it.

### Phase 1 (uffs-core only)

- [ ] Add `DriveIndex` struct to `backend.rs`
- [ ] Add `search_index()` free function to `backend.rs`
- [ ] Update `collect_global_top_n` to accept `&[&DriveCompactIndex]`
      or use `<D: AsRef<DriveCompactIndex> + Sync>`
- [ ] Re-export `DriveIndex` and `search_index` from `search/mod.rs`
- [ ] Add 3 unit tests for `search_index()`
- [ ] `cargo nextest run -p uffs-core` — all pass
- [ ] `cargo clippy -p uffs-core -- -D warnings` — clean

### Phase 2 (uffs-daemon)

- [ ] Change `IndexManager.backend` → `IndexManager.index: RwLock<Arc<DriveIndex>>`
- [ ] Add `add_drive()` helper method
- [ ] Add `replace_drive()` helper method
- [ ] Add `remove_drive()` helper method
- [ ] Update `IndexManager::new()` constructor
- [ ] Update `IndexManager::search()` — clone Arc, call `search_index()`
- [ ] Update `load_from_data_dir()` — use `add_drive()`
- [ ] Update `load_live_drives()` — use `add_drive()`
- [ ] Update `refresh()` — use `replace_drive()`
- [ ] Update `drives()` — clone Arc, iterate snapshot
- [ ] Update `info()` — clone Arc, iterate snapshot
- [ ] Update `has_drives()` — read lock, check empty
- [ ] Update `total_records()` — read lock, delegate
- [ ] Remove `MultiDriveBackend` import from `index.rs`
- [ ] Add `DriveIndex`, `search_index` imports
- [ ] `cargo nextest run -p uffs-daemon` — all pass
- [ ] `cargo clippy -p uffs-daemon -- -D warnings` — clean
- [ ] Manual test: concurrent searches via two terminals

### Phase 3 (safety valves)

- [ ] Raise `MAX_CONNECTIONS` to 256 in `ipc.rs`
- [ ] Add `search_semaphore: Semaphore` to `IndexManager`
- [ ] Acquire permit in `search()` before cloning Arc
- [ ] Wrap `search_index()` in `spawn_blocking` + `tokio::time::timeout`
- [ ] Add timeout error response handling
- [ ] `cargo nextest run -p uffs-daemon` — all pass
- [ ] Stress test: 20 concurrent searches on Windows

---

## 8. FAQ

**Q: Does the TUI need changes?**
A: No. The TUI uses the daemon client (`search_via_daemon`), which calls
`IndexManager::search()`. The TUI never touches `DriveIndex` directly.
`MultiDriveBackend` remains for TUI-local sort operations.

**Q: What if a query is very slow (30+ seconds)?**
A: Phase 3 adds `tokio::time::timeout`. The query is cancelled and an
error response returned. The `Arc` snapshot is dropped, freeing resources.

**Q: Can two refreshes run concurrently?**
A: Yes, but the last one to acquire the write lock wins. Each refresh
builds its new data outside the lock, then swaps atomically. If two
refreshes for different drives overlap, the second swap reads the current
`Arc` (which already includes the first swap) and builds on top of it.

**Q: What about `SearchFilters` — is the `&mut` safe?**
A: Yes. `filters` is a local variable in each `search()` call. Each query
creates its own `SearchFilters`, mutates it during the search (for
extension ID resolution), and drops it when done. Zero sharing.

**Q: Memory: what if 100 queries each hold an Arc to a 250 MB drive?**
A: They all share the **same** `Arc` — one copy of the data, not 100.
Memory is only duplicated if a refresh happens while queries are in-flight,
and even then it's just one extra copy freed when the last old-snapshot
query finishes.

**Q: What if `add_drive()` is called while a query is snapshotting?**
A: The `RwLock` prevents both from running simultaneously. But since the
read lock is held for < 1 μs (just cloning an `Arc`), the `add_drive()`
write lock waits at most 1 μs. After the swap, new queries see the new
drive; in-flight queries continue with the old snapshot.

**Q: Does this introduce `unsafe` code?**
A: No. `Arc`, `RwLock`, `Semaphore` are all safe abstractions. The
`DriveCompactIndex` data is read through shared references only. No raw
pointers, no `unsafe` blocks needed.

**Q: What about the `#[cfg(windows)]` MFT code?**
A: Unchanged. MFT reading is a separate concern — it produces a
`DriveCompactIndex` which is then passed to `add_drive()` or
`replace_drive()`. The concurrency changes are entirely in how the index
is stored and queried, not in how it's built.

---

## 9. Implementation Tracking

> Update this section as work progresses. Date format: `YYYY-MM-DD`.

### Status: ✅ COMPLETE (2026-04-05)

| Phase | Status   | Started    | Completed  |
|:------|:---------|:-----------|:-----------|
| 1     | ✅ Done  | 2026-04-05 | 2026-04-05 |
| 2     | ✅ Done  | 2026-04-05 | 2026-04-05 |
| 3     | ✅ Done  | 2026-04-05 | 2026-04-05 |

### Prerequisites

- [x] `SearchRequest` struct created (replaces 8-arg `search()` method)
- [x] `CompactRecord::path_len` precomputed (eliminates display-row filter for path length)
- [x] Bulkiness promoted to scan-level filter (eliminates display-row filter for bulkiness)
- [x] `needs_display_row_filter()` only returns true for `path_contains` and `type_filter`

### Phase 1 — `DriveIndex` + `search_index()` (uffs-core)

| Step | Item                                                          | Done |
|:-----|:--------------------------------------------------------------|:-----|
| 1.1  | Define `DriveIndex` struct in `backend.rs`                    | [x]  |
| 1.2  | Extract `search_index()` free function                        | [x]  |
| 1.3  | Adapt `collect_global_top_n` via generic `D: AsRef<DriveCompactIndex>` | [x] |
| 1.4  | Keep `MultiDriveBackend::search()` working (unchanged)        | [x]  |
| 1.5  | Types accessible via `search::backend::DriveIndex` (no extra re-export needed) | [x] |
| 1.6  | Write 3 unit tests (returns results, drive filter, concurrent) | [x]  |
| 1.7  | `cargo nextest run -p uffs-core` — 413 pass                   | [x]  |

### Phase 2 — Wire daemon (uffs-daemon)

| Step | Item                                                          | Done |
|:-----|:--------------------------------------------------------------|:-----|
| 2.1  | Change `IndexManager.backend` → `index: RwLock<Arc<DriveIndex>>` | [x] |
| 2.2  | Add `add_drive()`, `replace_drive()`, `snapshot()`            | [x]  |
| 2.3  | Rewrite `IndexManager::search()` — clone Arc, call `search_index()` | [x] |
| 2.4  | Update `load_from_data_dir()` → `add_drive()`                 | [x]  |
| 2.5  | Update `refresh()` → `replace_drive()`                        | [x]  |
| 2.6  | Update `load_live_drives()` → `add_drive()`                   | [x]  |
| 2.7  | Update `drives()`, `info()`, `has_drives()`, `total_records()`, `loaded_drive_letters()`, `load_single_mft_file()` | [x] |
| 2.8  | Remove `MultiDriveBackend` import, add `DriveIndex` + `search_index` imports | [x] |
| 2.9  | `cargo nextest run` — 727 pass, 0 warnings                    | [x]  |

### Phase 3 — Safety valves

| Step | Item                                                          | Done |
|:-----|:--------------------------------------------------------------|:-----|
| 3.1  | Raise `MAX_CONNECTIONS` to 256                                | [x]  |
| 3.2  | Add `search_semaphore: Semaphore` (`available_parallelism`) to `IndexManager` | [x] |
| 3.3  | Wrap search in `spawn_blocking` + `tokio::time::timeout(30s)` | [x]  |
| 3.4  | `cargo nextest run` — 727 pass, 0 warnings                    | [x]  |

### Decision Log

| Date       | Decision                                              | Rationale                           |
|:-----------|:------------------------------------------------------|:------------------------------------|
| 2026-04-05 | Use `Arc<RwLock<Arc<DriveIndex>>>` pattern             | < 1 μs lock hold, zero-copy search |
| 2026-04-05 | Keep `MultiDriveBackend` for TUI re-sort               | TUI uses daemon client for search; MDB only for local sort |
| 2026-04-05 | Use `std::thread::available_parallelism()` (not `num_cpus`) | Already in std since 1.59, avoids new dependency |
| 2026-04-05 | Use generic `D: AsRef<DriveCompactIndex>` for `collect_global_top_n` | Works for both `DriveCompactIndex`, `&DriveCompactIndex`, and `Arc<DriveCompactIndex>` |
| 2026-04-05 | Added `impl AsRef<DriveCompactIndex> for DriveCompactIndex` in `compact.rs` | Required for the generic to work with `MultiDriveBackend`'s `Vec<DriveCompactIndex>` |

### Resolved Questions

| # | Question                                                     | Resolution |
|:--|:-------------------------------------------------------------|:-----------|
| 1 | Generic vs concrete for `collect_global_top_n`?               | Generic `D: AsRef<DriveCompactIndex>` — works for all callers |
| 2 | Does `num_cpus` need to be added?                             | No — used `std::thread::available_parallelism()` instead |
| 3 | `--max-concurrent-searches` config flag?                      | Deferred — current `available_parallelism()` default is sensible |
