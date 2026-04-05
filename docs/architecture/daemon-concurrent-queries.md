# Daemon Concurrent Query Support — Deep Dive

## Current State: Fully Serialized

Every search request acquires an **exclusive write lock** on the backend:

```rust
// crates/uffs-daemon/src/index.rs:169
pub async fn search(&self, params: &SearchParams) -> SearchResponse {
    let mut backend = self.backend.write().await;   // ← blocks ALL other queries
    backend.sort_column = sort_column;               // ← mutates shared state
    backend.sort_desc = params.sort_desc;             // ← mutates shared state
    ...
    let result = backend.search_drives(...);          // ← mutates last_results + drives
    drop(backend);
}
```

Read-only operations (`drives()`, `status()`, `info()`) take `.read()` locks, so they
also block while any search holds the write lock.

### Impact

| Connections | Behavior                                           |
|:-----------:|:---------------------------------------------------|
| 1           | Works fine — single CLI/TUI user                   |
| 2–5         | Noticeable queueing — search 2 waits for search 1  |
| 10+         | Severe head-of-line blocking — 200ms search × 10 = 2s tail latency |
| 100+        | Unusable — timeout cascades (MCP, web UI, multi-user) |

## Root Cause: Five Mutable Fields

`MultiDriveBackend` mixes **shared immutable index data** with **per-query mutable
state**:

```rust
pub struct MultiDriveBackend {
    // ── SHARED (immutable after load) ────────────────────────────
    pub drives: Vec<DriveCompactIndex>,       // ~72 bytes × 26M records

    // ── PER-QUERY (changes every search) ─────────────────────────
    pub last_results: Vec<DisplayRow>,        // TUI re-sort cache
    pub sort_column: FieldId,                 // changes per query
    pub sort_desc: bool,                      // changes per query
    pub extra_sort_tiers: Vec<SortSpec>,      // changes per query
}
```

The write lock is needed because `search_drives()` mutates ALL of these:

| Field              | Why it's mutated                                    | Who needs it |
|:-------------------|:----------------------------------------------------|:-------------|
| `sort_column`      | Set from query params before search                 | This query only |
| `sort_desc`        | Set from query params before search                 | This query only |
| `extra_sort_tiers` | Set from query params before search                 | This query only |
| `last_results`     | Cloned from search output for TUI re-sort           | TUI only |
| `drives`           | Temporarily partitioned by `drives_filter` (swap hack) | This query only |

**None of these mutations are needed by other concurrent queries.**

## Additional Mutation: The Drive-Swap Hack

`search_drives()` does a destructive partition on `self.drives` when a drive filter
is active:

```rust
let stashed_drives = if drives_filter.is_empty() {
    None
} else {
    let all = core::mem::take(&mut self.drives);   // ← empties the shared Vec!
    let (keep, rest) = all.into_iter().partition(|dr| ...);
    self.drives = keep;                             // ← only matching drives
    Some(rest)
};
// ... search ...
if let Some(rest) = stashed_drives {
    self.drives.extend(rest);                       // ← restore
}
```

This is the most dangerous mutation: it **temporarily removes drives** from the shared
index. A concurrent query arriving during this window would see missing drives.

## Additional Mutation: `SearchFilters`

`search_drives()` takes `search_filters: &mut SearchFilters`. The mutation is
`resolve_ext_ids_for_drive()` — pre-resolving extension strings to `u16` IDs per drive.
This is a per-drive operation that changes `self.resolved_ext_ids`.

## The Fix: Split Shared vs. Per-Query State

### Step 1 — New type: `DriveIndex` (shared, immutable, `Arc`-wrapped)

```rust
/// Shared immutable index data — loaded once, read by all queries.
pub struct DriveIndex {
    pub drives: Vec<DriveCompactIndex>,
}

impl DriveIndex {
    pub fn total_records(&self) -> usize { ... }
    pub fn drive_summary(&self) -> Vec<(char, usize)> { ... }
}
```

The daemon stores this as:

```rust
pub struct IndexManager {
    index: Arc<RwLock<Arc<DriveIndex>>>,  // read lock for queries, write for refresh
}
```

- **Queries**: `.read()` → clone the `Arc<DriveIndex>` → drop the lock immediately →
  search on the cloned `Arc` with zero contention.
- **Refresh**: `.write()` → swap in a new `Arc<DriveIndex>` → old one is dropped when
  all in-flight queries finish (Arc refcount → 0).

### Step 2 — Per-query state becomes local variables

```rust
pub fn search_drives(
    index: &DriveIndex,            // shared, immutable borrow
    pattern: &str,
    sort_column: FieldId,          // local, from query params
    sort_desc: bool,               // local, from query params
    extra_sort_tiers: &[SortSpec], // local
    result_limit: Option<u32>,
    filter_mode: FilterMode,
    search_filters: &mut SearchFilters, // local, per-query clone
    drives_filter: &[char],
) -> SearchResult { ... }
```

No more `&mut self`. No more write lock for searches. Each query runs on its own
stack with a shared `&DriveIndex`.

### Step 3 — Eliminate the drive-swap hack

Replace the destructive partition with a filter iterator:

```rust
// BEFORE (mutates self.drives):
let all = core::mem::take(&mut self.drives);
let (keep, rest) = all.into_iter().partition(|dr| ...);
self.drives = keep;

// AFTER (zero mutation):
let active_drives: Vec<&DriveCompactIndex> = index.drives.iter()
    .filter(|dr| drives_filter.is_empty()
        || drives_filter.iter().any(|f| f.eq_ignore_ascii_case(&dr.letter)))
    .collect();
```

### Step 4 — Move `last_results` to the TUI only

`last_results` exists solely for the TUI's re-sort feature (Tab key cycles sort
column without re-searching). The daemon never uses it. The CLI never uses it.

```rust
// TUI keeps its own results cache:
pub struct App {
    pub results: Vec<DisplayRow>,        // already exists!
    pub backend: MultiDriveBackend,      // keep for TUI-specific sort/cycle_sort
}
```

For the daemon, `search_drives()` simply returns `SearchResult { rows, ... }`
without storing anything.

### Step 5 — `SearchFilters` becomes per-query (clone)

The `&mut SearchFilters` parameter exists because `resolve_ext_ids_for_drive()` mutates
the internal `resolved_ext_ids` cache. Two options:

**Option A** — Clone per query (cheap — just a few Vecs of small strings):
```rust
let mut filters = search_filters.clone();  // per-query copy
// ... pass &mut filters to search_drives ...
```

**Option B** — Make `resolve_ext_ids_for_drive()` return the IDs instead of mutating:
```rust
fn resolve_ext_ids_for_drive(&self, drive: &DriveCompactIndex) -> Vec<u16> {
    drive.resolve_ext_ids(&self.extensions)
}
```
Then pass the resolved IDs into `matches_record()` as a parameter.

Option A is simpler and has negligible cost (a clone of a few KB vs. searching 26M
records).

## Resulting Architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│                         IndexManager                                │
│                                                                      │
│  index: Arc<RwLock<Arc<DriveIndex>>>                                │
│         │                                                            │
│         │  .read() → clone Arc → drop lock (< 1μs)                  │
│         │                                                            │
│         ▼                                                            │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐                 │
│  │  Query #1   │  │  Query #2   │  │  Query #3   │   (concurrent)  │
│  │             │  │             │  │             │                  │
│  │ sort: size  │  │ sort: name  │  │ sort: mod   │  ← per-query    │
│  │ desc: true  │  │ desc: false │  │ desc: true  │                 │
│  │ filters: …  │  │ filters: …  │  │ filters: …  │                 │
│  │             │  │             │  │             │                  │
│  │ &DriveIndex ──────────────────────────────────│  ← shared Arc   │
│  └─────────────┘  └─────────────┘  └─────────────┘                 │
│                                                                      │
│  .write() → swap Arc (only during refresh, < 1ms)                   │
└──────────────────────────────────────────────────────────────────────┘
```

## Lock Contention Summary

| Operation        | Before              | After                          |
|:-----------------|:--------------------|:-------------------------------|
| Search           | `write()` — exclusive, 20-200ms | No lock (Arc clone < 1μs) |
| Drives/Status    | `read()` — blocked by search   | `read()` — never blocked  |
| Info             | `read()` — blocked by search   | `read()` — never blocked  |
| Refresh (reload) | `write()` — exclusive           | `write()` — exclusive but < 1ms (Arc swap) |
| Load (startup)   | `write()` — exclusive           | `write()` — same, startup only |

## Migration Plan

### Phase 1 — Extract `search_drives` to a free function (non-breaking)

Change `MultiDriveBackend::search_drives(&mut self, ...)` to a free function
`search_drives(drives: &[DriveCompactIndex], sort_column, sort_desc, ...)`.
The TUI's `MultiDriveBackend` wraps this function and keeps `last_results` for
re-sort.

**Risk:** Low — the function signature changes but behavior is identical.
**Effort:** ~1 day. Touch `backend.rs`, `index.rs`, `app.rs`.

### Phase 2 — Arc-wrap the index in the daemon (non-breaking for TUI)

Replace `backend: RwLock<MultiDriveBackend>` with
`index: RwLock<Arc<DriveIndex>>` in `IndexManager`. Daemon's `search()` clones
the Arc, drops the read lock, then calls the free function.

**Risk:** Low — daemon-only change. TUI keeps its own `MultiDriveBackend`.
**Effort:** ~1 day. Touch `index.rs`, `handler.rs`.

### Phase 3 — Raise connection limits

With searches no longer serialized, raise `MAX_CONNECTIONS` from 32 to 256+.
Add per-query timeout (5s default) to prevent slow queries from accumulating.

**Risk:** Medium — memory pressure from concurrent large result sets. Add a
global concurrent-search semaphore (e.g., 16 permits) as a safety valve.
**Effort:** ~0.5 day. Touch `ipc.rs`.

## Mutation Scenarios: Add-Drive, Refresh, Remove-Drive

All three scenarios use the same pattern: **build in the background, swap
atomically**. In-flight queries are never interrupted — they hold an `Arc`
to the old snapshot and finish naturally.

### Adding a Drive (e.g., loading D: while queries run against C:)

```
Timeline:
─────────────────────────────────────────────────────────────────
Query A  ──read()──clone Arc₁──drop lock──search(C:)────────────→ done
                                                                  (still using Arc₁)
Load D:  ─────────────────────[build DriveCompactIndex on thread pool]───┐
                                                                         │
Query B  ──read()──clone Arc₁──drop lock──search(C:)──→ done            │
                                                                         │
         ┌───── write() ─────┐                                           │
Swap:    │ Arc₂ = Arc₁.drives + D:  (< 1ms)                             │
         │ *index = Arc₂     │◄──────────────────────────────────────────┘
         └───────────────────┘

Query C  ──read()──clone Arc₂──drop lock──search(C: + D:)──→ done
         (sees both drives)
```

**Key points:**
- Query A is still using `Arc₁` (C: only) — it finishes normally, no blocking.
- The write lock is held only for the swap: allocate a new `Vec<DriveCompactIndex>`
  with the old drives + the new one, wrap in `Arc`, store it. This is O(num_drives)
  — copying ~7 pointers, < 1μs.
- Query C, arriving after the swap, sees both C: and D:.
- `Arc₁` is dropped when Query A finishes (refcount → 0), freeing nothing because
  the drives were moved into `Arc₂`.

**Implementation:**

```rust
/// Add a newly-loaded drive to the shared index.
async fn add_drive(&self, new_drive: DriveCompactIndex) {
    // Build the new snapshot (no lock held — this is just Vec manipulation).
    let new_index = {
        let current = self.index.read().await;
        let mut drives = current.drives.clone();  // clone Vec of DriveCompactIndex
        drives.push(new_drive);
        Arc::new(DriveIndex { drives })
    };

    // Atomic swap — write lock held for < 1μs.
    let mut guard = self.index.write().await;
    *guard = new_index;
}
```

Wait — `clone()` of `Vec<DriveCompactIndex>` would deep-copy all 26M records!
That's the 1.8 GB index. We need **Arc-wrapping per drive** to avoid this:

```rust
pub struct DriveIndex {
    pub drives: Vec<Arc<DriveCompactIndex>>,  // Arc per drive, not per index
}
```

Now `clone()` of the Vec just copies ~7 `Arc` pointers (56 bytes), not 1.8 GB.
Adding a drive = clone the Vec, push a new `Arc<DriveCompactIndex>`, wrap in
`Arc<DriveIndex>`, swap.

### Refreshing a Drive (e.g., re-reading C: MFT while queries are running)

Same pattern — build the new `DriveCompactIndex` in the background, then
swap it into the index atomically:

```rust
async fn refresh_drive(&self, letter: char) {
    // 1. Load new MFT data (blocking, on thread pool — takes seconds).
    let new_drive = tokio::task::spawn_blocking(move || {
        load_drive(&source, no_cache)
    }).await?;

    // 2. Build new snapshot: replace the matching drive, keep others.
    let new_index = {
        let current = self.index.read().await;
        let mut drives: Vec<Arc<DriveCompactIndex>> = current.drives
            .iter()
            .filter(|d| d.letter != letter)
            .cloned()              // clones Arc pointers, not data
            .collect();
        drives.push(Arc::new(new_drive));
        Arc::new(DriveIndex { drives })
    };

    // 3. Atomic swap (< 1μs under write lock).
    let mut guard = self.index.write().await;
    *guard = new_index;
    // Old Arc<DriveIndex> dropped when all in-flight queries finish.
}
```

**No query is ever blocked during the MFT reload** — they all hold `Arc`s to
the old snapshot. The old C: data stays alive until the last in-flight query
referencing it completes.

### Removing a Drive

Same pattern — build new Vec without the drive, swap:

```rust
async fn remove_drive(&self, letter: char) {
    let new_index = {
        let current = self.index.read().await;
        let drives = current.drives.iter()
            .filter(|d| d.letter != letter)
            .cloned()
            .collect();
        Arc::new(DriveIndex { drives })
    };
    let mut guard = self.index.write().await;
    *guard = new_index;
}
```

### Summary: Lock Duration by Operation

| Operation         | Lock type | Duration    | Blocks queries? |
|:------------------|:----------|:------------|:----------------|
| Search            | None      | 0           | No              |
| Clone Arc (query start) | `read()` | < 1μs  | No              |
| Add drive         | `write()` | < 1μs (swap) | < 1μs         |
| Refresh drive     | `write()` | < 1μs (swap) | < 1μs         |
| Remove drive      | `write()` | < 1μs (swap) | < 1μs         |
| Initial load      | `write()` | < 1μs per drive (progressive) | < 1μs |

The heavy work (MFT parsing, trigram building, compaction) always happens
**outside any lock**, on background threads. Only the final pointer swap
touches the lock.

## Memory Considerations

**Per-query results:** Each concurrent query allocates its own `Vec<DisplayRow>`.
A typical search returns 10–1000 rows × ~200 bytes = 2–200 KB. 100 concurrent
queries = ~20 MB. Negligible compared to the ~1.8 GB index.

**Index snapshots:** With `Arc<DriveCompactIndex>` per drive, swapping a drive
during refresh briefly keeps two copies of that one drive's data alive (~250 MB
for a 3.5M-record drive). The old copy is freed when the last in-flight query
referencing it completes. The other 6 drives share the same `Arc` — zero extra
memory.

**Worst case:** All 7 drives refreshing simultaneously while queries hold the
old snapshot = 2× total memory (~3.6 GB). This is bounded and short-lived.

## Rayon Thread Pool

`search_drives()` uses `par_iter()` (rayon) to search drives in parallel.
Multiple concurrent queries will compete for the global rayon thread pool.
This is fine — rayon's work-stealing scheduler handles this efficiently.
The drives are read-only during search, so there's no data-race risk.

For extreme concurrency (100+ queries), consider a dedicated rayon pool
per query type, or a bounded concurrency limiter that caps simultaneous
rayon-heavy searches to `num_cpus`.
