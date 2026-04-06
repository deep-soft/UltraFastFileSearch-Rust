# Filter / Sort / Attribute Feature Matrix

> **Purpose:** Single source of truth for every filter, sort, and attribute
> feature across all UFFS front-ends. Documents the current state and the
> target state after D5+D6 migration (all frontends through daemon) and
> Waves 3–5 (unified field system).
>
> **Date:** 2026-04-04 — post-daemon filter wiring session. All `SearchParams`
> fields are fully wired. Compact search is the sole pipeline. CLI sugar
> expansion (extension collections, attribute presets, month specs, `path:`
> prefix) is implemented client-side. Record indices standardised on `u32`.

---

## 1. Architecture Context

### 1.1 The Filter Data Flow (current — 4 structs)

```
CLI args (clap)                              ← user input
  ↓ main.rs: 40+ positional params
search(pattern, ..., newer, older, ...)      ← search/mod.rs
  ↓ build_search_config()
SearchConfig<'a> { ..., newer, attr, sort }  ← dispatch.rs (god struct, borrows CLI strings)
  ↓ extracts subset
QueryFilters<'a> { parsed, ext, sizes }      ← raw_io.rs   (10 fields; MISSING date/attr/sort/exclude)
  ↓ OwnedQueryFilters::from_borrowed()
OwnedQueryFilters { parsed, ext, sizes }     ← raw_io_windows.rs (owned for Send; same 10 fields)
  ↓ search_compact()
SearchFilters { newer_us, attr_require, … }  ← uffs-core/search/filters.rs (pre-parsed; ready for hot path)
```

**Problem:** `QueryFilters` and `OwnedQueryFilters` carry only 10 of the 24
filter/sort fields. The other 14 sit on `SearchConfig` — set from CLI args
but **never read by any dispatch path** since the streaming pipeline was deleted.

### 1.2 The Filter Data Flow (TUI — 1 struct, works correctly)

```
SearchState { hide_system, newer, attr, sort, … }   ← history.rs
  ↓ build_search_filters(state)
SearchFilters { newer_us, attr_require, … }          ← uffs-tui/filters.rs
  +
backend.sort_column / sort_desc / extra_sort_tiers   ← set from state.sort via parse_sort_spec
```

The TUI builds `SearchFilters` directly from `SearchState` — one hop, no
intermediaries. Sort is set directly on the `MultiDriveBackend`. **All 24
features work in the TUI.**

### 1.3 The Filter Data Flow (daemon — ✅ fully wired)

```
SearchParams { pattern, sort, sort_desc, filter,       ← uffs-client/protocol.rs (JSON-RPC)
  hide_system, min_size, max_size, ext, attr, exclude,
  newer, older, newer_created, older_created,
  newer_accessed, older_accessed, min_descendants,
  max_descendants, min_name_len, max_name_len,
  min_path_len, max_path_len, min_allocated, max_allocated,
  allowed_months, match_path, predicates, … }
  ↓ IndexManager::search()
SearchFilters::from_params(…)                          ← built from SearchParams fields ✅
  +
backend.sort_column / sort_desc / extra_sort_tiers     ← wired from params.sort/sorts
  +
compile_predicates_into_filters(…)                     ← canonical predicates → hot-path ✅
```

**Status (post-session):** The daemon now constructs `SearchFilters` from
all `SearchParams` fields via `from_params()`. Canonical predicates are
compiled into the hot-path filter pipeline by `compile_predicates_into_filters()`.
Size, date, attribute, extension, exclude, descendant, name/path length,
size-on-disk, and month-of-year filters are all wired and functional.

### 1.4 The Filter Data Flow (deleted streaming pipeline — was correct)

The streaming pipeline (deleted in Wave 1) read filter values directly from
`SearchConfig` and applied them via `StreamingRecordFilter::matches()`:

```
SearchConfig { newer, attr_filter, sort, exclude, … }
  ↓ build_record_filter()                              ← DELETED
StreamingRecordFilter { files_only, hide_system,       ← DELETED
  min_size, max_size, newer_modified, older_modified,
  newer_created, older_created, newer_accessed,
  older_accessed, attr_require, attr_exclude,
  exclude_lower }
```

This was a parallel filter implementation to `SearchFilters` — it duplicated
the same logic (size bounds, date bounds, attribute bitmasks, exclude glob).
Deleting it was correct (DRY), but the replacement wiring was never completed.

### 1.5 The Filter Data Flow (target — post D5/D6, daemon-only)

Once `DAEMON_IMPLEMENTATION_PLAN.md` phases D5 (CLI migration) and D6
(TUI migration) are complete, **every search goes through one funnel —
no standalone mode, no second code path:**

```
CLI args (clap)  ─────────┐
TUI toggles (ratatui)  ───┤
MCP tool params (JSON)  ──┤──→  SearchParams  ──→  daemon  ──→  results
GUI actions (future)  ────┤     (one struct)       (one search engine)
HTTP/REST (future)  ──────┘

Inside the daemon:
  SearchParams
    ↓ IndexManager::search()
  SearchFilters::from_params()   ← built ONCE from SearchParams fields
    +
  backend.sort_column / sort_desc / extra_sort_tiers
    ↓
  MultiDriveBackend::search()
    ↓
  Result delivery (sized by result count):
    ≤ 100K rows  →  JSON-RPC response (inline, ~instant)
    > 100K rows  →  shared memory handoff (zero-copy, near-native speed)
```

**Result delivery: JSON-RPC vs shared memory**

For filtered queries (99% of real usage), results fit in a normal
JSON-RPC response — no special handling needed. For bulk queries
(`uffs "*"` → 25M rows), the daemon writes results as flat binary to a
shared memory region and returns the shmem path in the JSON-RPC response.
The CLI mmaps it and reads directly — zero serialization, zero copy.

```
Bulk query flow:
  Daemon:
    1. Search → Vec<DisplayRow> in daemon memory
    2. shm_open() / CreateFileMapping() → shared region
    3. Write rows as flat binary layout (no JSON overhead)
    4. Return { "shmem": "/dev/shm/uffs-XXXX", "count": 25000000 }

  CLI:
    1. Receive JSON-RPC response with shmem path
    2. mmap(path) → &[DisplayRow] (zero-copy, zero-deserialize)
    3. Format each row → stdout (same output code as today)
    4. munmap + unlink
```

**Performance comparison (25M files, bulk query):**

| Scenario | MFT/cache load | Search+sort | Transfer | Stdout | Total |
|----------|---------------|-------------|----------|--------|-------|
| Today (cold, live MFT) | 5–30s | ~1.5s | 0 (in-process) | ~8s | 15–40s |
| Today (warm, .uffs cache) | 1–3s | ~1.5s | 0 (in-process) | ~8s | 11–13s |
| **Daemon + shmem** | **0s** (warm) | ~1.5s | **~0.2s** (mmap) | ~8s | **~10s** |

The daemon is **faster than today** for every case because it eliminates
the .uffs cache load (1–3s) and index build (0.5–1s). The shared memory
overhead (~200ms for 1.8 GB memcpy) is less than what's saved. The
stdout write (~8s for 2 GB of output) is the true bottleneck and is
identical in all approaches.

**Why this changes everything:**

1. **The DRY problem vanishes.** There is exactly ONE place where
   `SearchParams → SearchFilters → search()` happens: the daemon's
   `IndexManager`. No more `QueryFilters` vs `OwnedQueryFilters` vs
   `SearchFilters` vs `SearchState` vs `SearchParams` — just
   `SearchParams` on the wire and `SearchFilters` in the engine.

2. **All filter/sort/field logic lives in `uffs-core`.** The daemon
   calls `uffs-core` search functions. Each frontend just populates
   `SearchParams` from its own UI/args — no filter logic in frontends.

3. **Adding a new filter/sort/column field is one change.** Add it to
   `SearchParams` (wire type) + `SearchFilters` (engine type) + the
   `FieldId` enum. Every frontend gets it through `SearchParams` for free.

4. **No standalone mode. ONE pipeline.** All searches go through the
   daemon. Bulk results use shared memory for near-native speed. No
   second code path to maintain, no filter logic duplication.

**Implication for this document:** Waves 3–5 (FieldId, predicates,
cold-path) should be implemented **after D5+D6** so the unified field
system only needs to be built once, in one place.

---

## 2. Feature Matrix

### Legend


### 2.1 Pattern Matching

| Feature | CLI flag | CLI | TUI | Daemon | Core engine |
|---------|----------|-----|-----|--------|-------------|
| Glob pattern | `*.txt` | ✅ | ✅ | ✅ | `search_compact_drive` |
| Regex pattern | `>.*\.log$` | ✅ | ✅ | ✅ | `search_compact_drive_regex` |
| Literal substring | `report` | ✅ | ✅ | ✅ | `search_compact_drive` |
| Path-aware pattern | `c:/users/*.txt` | ✅ | ✅ | ✅ | `search_compact_drive_tree` |
| Match-all | `*` | ✅ | ✅ | ✅ | `collect_global_top_n` |
| Case-sensitive | `--case` | ✅ | ✅ | ✅ | `case_sensitive` param |
| Smart case | `--smart-case` | ✅ | ✅ | ⬜ | auto in `build_search_config` |
| Whole word | `--word` | ✅ | ✅ | ✅ | `whole_word` param |
| Name-only match | `--name-only` | 🟡 ¹ | ✅ | ⬜ | trigram on `names_lower` blob |
| Path match prefix | `path:report` | ✅ | ✅ | ✅ | `match_path` param + descendant expansion |
| Dir scope prefix | `dir:logs` | ✅ | ⬜ | ⬜ | CLI sugar → `dirs_only` + pattern |
| File scope prefix | `file:readme` | ✅ | ⬜ | ⬜ | CLI sugar → `files_only` + pattern |
| Begins-with sugar | `--begins-with foo` | ✅ | ⬜ | ⬜ | CLI sugar → `foo*` pattern |
| Ends-with sugar | `--ends-with .log` | ✅ | ⬜ | ⬜ | CLI sugar → `*.log` pattern |
| Contains sugar | `--contains report` | ✅ | ⬜ | ⬜ | CLI sugar → `*report*` pattern |
| Not-contains sugar | `--not-contains temp` | ✅ | ⬜ | ⬜ | CLI sugar → `--exclude *temp*` |

> ¹ `--name-only` is set on `SearchConfig` but the compact search path doesn't
> currently read it — the trigram search always matches against the lowered name
> blob (which is name-only by default). Full-path matching is only used for
> path-pattern detection. Effect: `--name-only` is a no-op on the compact path
> because name-only is already the default behavior.
>
> **Path match (`path:`) implementation:** When `match_path=true`, after
> trigram+name matching, matching directories are expanded via DFS walk of
> the CSR `ChildrenIndex` to include all descendants. Zero extra memory —
> uses the existing children index. Consistent with Everything's `path:` prefix.

### 2.2 Scope Filters

| Feature | CLI flag | CLI | TUI | Daemon | Core engine |
|---------|----------|-----|-----|--------|-------------|
| Files only | `--files-only` | ✅ | ✅ | ✅ | `FilterMode::FilesOnly` |
| Dirs only | `--dirs-only` | ✅ | ✅ | ✅ | `FilterMode::DirsOnly` |
| Hide system (`$*`) | `--hide-system` | ✅ | ✅ | ✅ | `SearchFilters.hide_system` via `SearchParams.hide_system` |
| Extension filter | `--ext jpg,png` | ✅ | ✅ | ✅ | `SearchFilters.extensions` via `SearchParams.ext` |
| Extension collections | `--ext documents` | ✅ | ✅ | ✅ ² | `expand_ext_spec()` → client-side sugar expansion |
| Executables collection | `--ext executables` | ✅ | ✅ | ✅ ² | `collections::EXECUTABLES` (exe,msi,bat,cmd,ps1,com,scr,vbs,wsf,dll,sys) |

> ² **Extension collection expansion is client-side sugar.** The CLI/TUI calls
> `expand_ext_spec("documents")` → `"doc,docx,pdf,txt,rtf,odt,xls,xlsx,ppt,pptx,csv,md"`
> before sending to the daemon. The daemon only receives primitive extension
> names. Available collections: `documents`/`docs`, `pictures`/`images`,
> `videos`/`video`, `music`/`audio`, `archives`/`compressed`, `code`/`source`,
> `executables`/`exec`.

### 2.3 Size Filters

| Feature | CLI flag | CLI | TUI | Daemon | Core engine |
|---------|----------|-----|-----|--------|-------------|
| Minimum size | `--min-size 1024` | ✅ | ✅ | ✅ | `SearchFilters.min_size` via `SearchParams.min_size` |
| Maximum size | `--max-size 10G` | ✅ | ✅ | ✅ | `SearchFilters.max_size` via `SearchParams.max_size` |
| Min size-on-disk | `--min-size-on-disk 4K` | ✅ | ⬜ | ✅ | `SearchFilters.min_allocated` via `SearchParams.min_allocated` |
| Max size-on-disk | `--max-size-on-disk 1G` | ✅ | ⬜ | ✅ | `SearchFilters.max_allocated` via `SearchParams.max_allocated` |

### 2.3a Name / Path Length Filters

| Feature | CLI flag | CLI | TUI | Daemon | Core engine |
|---------|----------|-----|-----|--------|-------------|
| Min name length | `--min-name-len 5` | ✅ | ⬜ | ✅ | `SearchFilters.min_name_len` (hot-path) |
| Max name length | `--max-name-len 50` | ✅ | ⬜ | ✅ | `SearchFilters.max_name_len` (hot-path) |
| Min path length | `--min-path-len 100` | ✅ | ⬜ | ✅ | `SearchFilters.min_path_len` (post-filter) |
| Max path length | `--max-path-len 260` | ✅ | ⬜ | ✅ | `SearchFilters.max_path_len` (post-filter, MAX_PATH detection) |

> **Name length** is checked in the hot-path (`matches_record`) — it uses
> `rec.name(names).chars().count()` which is already in cache. **Path length**
> is checked in `apply_search_filters` (post-filter) since full paths are
> resolved after the initial match. The canonical predicate system also supports
> `NameLength`/`PathLength` via `compile_predicates_into_filters()`.

### 2.4 Date / Time Filters

| Feature | CLI flag | CLI | TUI | Daemon | Core engine |
|---------|----------|-----|-----|--------|-------------|
| Newer (modified) | `--newer 7d` | ✅ | ✅ | ✅ | `SearchFilters.newer_us` via `SearchParams.newer` |
| Older (modified) | `--older 30d` | ✅ | ✅ | ✅ | `SearchFilters.older_us` via `SearchParams.older` |
| Newer (created) | `--newer-created 1w` | ✅ | ✅ | ✅ | `SearchFilters.newer_created_us` via `SearchParams.newer_created` |
| Older (created) | `--older-created 2026-01-01` | ✅ | ✅ | ✅ | `SearchFilters.older_created_us` via `SearchParams.older_created` |
| Newer (accessed) | `--newer-accessed 24h` | ✅ | ✅ | ✅ | `SearchFilters.newer_accessed_us` via `SearchParams.newer_accessed` |
| Older (accessed) | `--older-accessed 90d` | ✅ | ✅ | ✅ | `SearchFilters.older_accessed_us` via `SearchParams.older_accessed` |
| Month-of-year | `--month january` | ✅ | ⬜ | ✅ | `SearchFilters.allowed_months` (hot-path) |
| Quarter | `--month Q1` | ✅ | ⬜ | ✅ | `parse_month_spec("Q1")` → `[1,2,3]` |
| Month combo | `--month jan,feb,Q4` | ✅ | ⬜ | ✅ | Mixed month names + quarters |

> **Time spec formats supported by core:** `7d` (days), `24h` (hours),
> `30m` (minutes), `90s` (seconds), `2w` (weeks), `2026-01-15` (ISO date).
>
> **Month-of-year filter:** Parsed client-side via `parse_month_spec()` →
> `Vec<u32>` of month numbers (1-12). Sent as `SearchParams.allowed_months`.
> Supports: full names (`january`), abbreviations (`jan`), quarters (`Q1`-`Q4`),
> and comma-separated combos (`jan,feb,Q4`). The daemon receives only primitive
> `u32` month numbers — no string parsing at query time.
>
> **Planned (Wave 4):** `today`, `yesterday`, `ytd`, `this_week`, `last_week`,
> `this_month`, `last_month`, `this_year`, `last_year`. All resolve to
> Unix µs at parse time via the existing `parse_time_bound()` function.

### 2.5 NTFS Attribute Filters

| Feature | CLI flag | CLI | TUI | Daemon | Core engine |
|---------|----------|-----|-----|--------|-------------|
| Require attributes | `--attr hidden,compressed` | ✅ | ✅ | ✅ | `SearchFilters.attr_require` via `SearchParams.attr` |
| Exclude attributes | `--attr !system,!hidden` | ✅ | ✅ | ✅ | `SearchFilters.attr_exclude` via `SearchParams.attr` |
| Mixed include/exclude | `--attr compressed,!system` | ✅ | ✅ | ✅ | Both bitmask fields |
| Attribute presets | `--attr system-files` | ✅ | ⬜ | ✅ ³ | `expand_attr_spec()` → client-side expansion |

> ³ **Attribute presets are client-side sugar.** The CLI calls
> `expand_attr_spec("system-files")` → `"hidden,system"` before sending to
> the daemon. Available presets: `system-files` → `hidden,system`,
> `user-files` → `!hidden,!system`, `compressed-encrypted` → `compressed,encrypted`,
> `temp-files` → `temporary`, `offline-files` → `offline`.
>
> **Supported attribute names** (with shortcuts): `readonly` (r), `hidden` (h),
> `system` (s), `directory` (d), `archive` (a), `device`, `normal`,
> `temporary` (t), `sparse`, `reparse`, `compressed` (c), `offline` (o),
> `notindexed` (n), `encrypted` (e), `integrity` (i), `virtual` (v),
> `noscrub` (x), `pinned` (p), `unpinned` (u).

### 2.6 Descendant Filters

| Feature | CLI flag | CLI | TUI | Daemon | Core engine |
|---------|----------|-----|-----|--------|-------------|
| Min descendants | `--min-descendants 10` | ✅ | ✅ | ✅ | `SearchFilters.min_descendants` via `SearchParams.min_descendants` |
| Max descendants | `--max-descendants 0` | ✅ | ✅ | ✅ | `SearchFilters.max_descendants` via `SearchParams.max_descendants` |

### 2.7 Exclude Pattern

| Feature | CLI flag | CLI | TUI | Daemon | Core engine |
|---------|----------|-----|-----|--------|-------------|
| Exclude glob | `--exclude backup*` | ✅ | ✅ | ✅ | `SearchFilters.exclude_lower` via `SearchParams.exclude` |

> **How it works:** Lowercased glob match on filename. Applied post-search
> via `apply_search_filters()` and pre-search via `matches_record()`.

### 2.8 Sort

| Feature | CLI flag | CLI | TUI | Daemon | Core engine |
|---------|----------|-----|-----|--------|-------------|
| Single-column sort | `--sort size` | ✅ | ✅ | ✅ | `MultiDriveBackend.sort_column` via `SearchParams.sort` |
| Multi-tier sort | `--sort modified,size,name` | ✅ | ✅ | ✅ | `SearchParams.sorts: Vec<SearchSortSpec>` |
| Sort direction | `--sort size:desc` | ✅ | ✅ | ✅ | `SearchSortSpec.direction` |
| Reverse sort | `--sort-desc` | ✅ | ✅ | ✅ | `sort_desc` flag (legacy compat) |
| Cycle sort (Tab key) | N/A | ⬜ | ✅ | ⬜ | `cycle_sort()` |
| Toggle direction | N/A | ⬜ | ✅ | ⬜ | `toggle_sort_direction()` |
| Re-sort last results | N/A | ⬜ | ✅ | ⬜ | `sort()` method |

> **Available sort columns** (11): `name`, `size`, `sizeondisk`/`allocated`,
> `created`, `modified`/`date`/`written`, `accessed`, `path`, `drive`,
> `ext`/`extension`, `type` (devicon), `descendants`.
>
> **Default directions:** Size/dates/descendants default to descending;
> name/path/drive/extension/type default to ascending.
>
> **Sort wiring:** CLI calls `canonicalize_legacy_sort()` to convert
> `--sort size:desc` into `Vec<SearchSortSpec>`, which is sent as
> `SearchParams.sorts`. The daemon resolves sort specs via `resolved_sorts()`.

### 2.9 Output Control

| Feature | CLI flag | CLI | TUI | Daemon | Core engine |
|---------|----------|-----|-----|--------|-------------|
| Result limit | `--limit 500` / `-n 500` | ✅ | ✅ | ✅ | `result_limit` param |
| Output format | `--format csv\|json\|table\|custom` | ✅ | ⬜ | ⬜ | output/mod.rs |
| Column selection | `--columns name,size,path` | ✅ | ✅ | 🔲 | `OutputConfig` / `TuiColumn` |
| Column separator | `--sep \|` | ✅ | ⬜ | ⬜ | `OutputConfig.sep` |
| Quote char | `--quotes '` | ✅ | ⬜ | ⬜ | `OutputConfig.quotes` |
| Header row | `--header false` | ✅ | ⬜ | ⬜ | `OutputConfig.header` |
| Bool true repr | `--pos Yes` | ✅ | ⬜ | ⬜ | `OutputConfig.pos` |
| Bool false repr | `--neg No` | ✅ | ⬜ | ⬜ | `OutputConfig.neg` |
| Output to file | `--out results.csv` | ✅ | ⬜ | ⬜ | `write_native_results` |
| Parity compat mode | `--parity-compat` | ✅ | ⬜ | ⬜ | 25-column C++ mask |
| Hide ADS streams | `--hide-ads` | ✅ | ✅ | ✅ | `SearchFilters.hide_ads` (daemon hot-path, `memchr` scan) |
| Timezone override | `--tz-offset -8` | ✅ | ⬜ | ⬜ | `tz_offset` param |

### 2.10 Operational Flags

| Feature | CLI flag | CLI | TUI | Daemon | Core engine |
|---------|----------|-----|-----|--------|-------------|
| Profile timing | `--profile` | ✅ | ⬜ | ⬜ | timing instrumentation |
| Benchmark (no output) | `--benchmark` | ✅ | ⬜ | ⬜ | skip write path |
| Debug tree | `--debug-tree` | ✅ | ⬜ | ⬜ | hardlink diagnostics |
| Skip bitmap | `--no-bitmap` | ✅ | ⬜ | ⬜ | MFT read mode |
| Skip cache | `--no-cache` | ✅ | ⬜ | ⬜ | MFT read mode |
| Chaos seed | `--chaos-seed 42` | ✅ | ⬜ | ⬜ | chunk order randomization |
| Reserved allocated | `--reserved-allocated N` | ✅ | ⬜ | ⬜ | root dir size parity |
| Query mode | `--query-mode auto\|index\|dataframe` | ✅ | ⬜ | ⬜ | dispatch path selection |

---

## 3. Regression Summary

### 3.1 CLI — ✅ All previously broken features now wired

All 14 previously broken CLI filter/sort flags now work through the daemon
pipeline. The CLI builds `SearchParams` from CLI args (in `daemon.rs`) and
sends to the daemon. Client-side sugar expansion handles collection aliases
and attribute presets before the RPC call.

| Category | Status | How fixed |
|----------|--------|-----------|
| Date (6) | ✅ Fixed | `SearchParams` carries `newer`, `older`, `newer_created`, `older_created`, `newer_accessed`, `older_accessed` |
| Attribute (1) | ✅ Fixed | `SearchParams.attr` + client-side `expand_attr_spec()` |
| Exclude (1) | ✅ Fixed | `SearchParams.exclude` |
| Sort (2) | ✅ Fixed | `SearchParams.sorts: Vec<SearchSortSpec>` + `canonicalize_legacy_sort()` |
| Ext collections (1) | ✅ Fixed | Client-side `expand_ext_spec()` → primitive ext names in `SearchParams.ext` |
| Name-only (1) | 🟡 No-op | Name-only is the default. `path:` prefix provides explicit path matching. |
| Hide ADS (1) | ✅ Fixed | `--hide-ads` → `SearchFilters.hide_ads` → daemon hot-path `memchr(b':')` scan |
| Reserved alloc (1) | N/A | Volume-level constant for root dir `tree_allocated` parity; `--reserved-allocated` hidden flag wired in parity verification only. Not a per-record filter/sort concern. |

### 3.2 Derived fields

| Field | Status | Implementation |
|-------|--------|----------------|
| `Bulkiness` | ✅ computed | `allocated * 100 / size` — integer percentage. 100 = perfectly packed. >100 = cluster slack/waste. 0 for zero-byte files. |
| `TreeAllocated` | ✅ computed | `CompactRecord.tree_allocated` — sum of allocated sizes in subtree. Pre-computed during MFT indexing (O(n) leaf-peeling). |
| `TreeSize` | ✅ computed | `CompactRecord.treesize` — sum of logical sizes in subtree. Pre-computed during MFT indexing. |

### 3.3 Daemon — ✅ All filters now exposed in `SearchParams`

`SearchParams` (uffs-client/protocol.rs) now carries **all** filter fields:

**Core:** `pattern`, `case_sensitive`, `whole_word`, `match_path`
**Sort:** `sort`, `sorts: Vec<SearchSortSpec>`, `sort_desc`
**Limit:** `limit`
**Filter mode:** `filter`, `filter_mode`, `drives`
**Size:** `min_size`, `max_size`, `min_allocated`, `max_allocated`
**Descendants:** `min_descendants`, `max_descendants`
**Date:** `newer`, `older`, `newer_created`, `older_created`, `newer_accessed`, `older_accessed`
**Attribute:** `attr`, `hide_system`
**Extension:** `ext`
**Exclude:** `exclude`
**Length:** `min_name_len`, `max_name_len`, `min_path_len`, `max_path_len`
**Month:** `allowed_months: Vec<u32>`
**Canonical:** `predicates: Vec<SearchPredicate>`, `projection`, `response_mode`

The daemon calls `SearchFilters::from_params()` with all fields and
`compile_predicates_into_filters()` for canonical predicates. No fields
are ignored.

### 3.4 What was lost in the streaming deletion (historical)

The deleted `StreamingRecordFilter` implemented these filter checks inline.
All of these capabilities now exist in `SearchFilters` (uffs-core) and are
fully wired through the daemon pipeline. The gap that existed in the CLI
wiring layer has been closed.

---

## 4. Approach: Do We Need a Query Engine / DSL / SQL?

### 4.1 No.

UFFS is not a database. It does exactly one thing: **pattern → filter → sort →
limit → project columns**. This is a linear pipeline, not a relational query.
There are no joins, aggregations, subqueries, grouping, or CTEs.

What we actually need is far simpler:

| Query engine feature | UFFS needs it? | Why not? |
|---------------------|----------------|----------|
| SQL parser | ❌ | Our queries are always: match + filter + sort + limit |
| Relational algebra | ❌ | Single table (MFT records), no joins |
| Aggregation (GROUP BY) | ❌ | We show individual rows, not summaries |
| Window functions | ❌ | No ranking, no running totals |
| Subqueries | ❌ | No nested queries |
| Query optimizer | ❌ | Our pipeline order is fixed and optimal |
| Schema discovery | ❌ | Schema is static (NTFS + derived fields) |

What we DO need: **a unified field-addressing system** so that filter, sort,
output, and all frontends speak the same language when referring to any field.

### 4.2 What we have today: 3 separate enums, 1 bespoke filter struct

```
OutputColumn   (34 variants) — "what can I put in a column?"
SortColumn     (11 variants) — "what can I sort by?"
TuiColumn      (25 variants) — "what can the TUI display?"
SearchFilters  (15 fields)   — "what can I filter on?" (hardcoded struct, not enum-driven)
```

Each enum has its own `parse()`, its own name strings, its own mapping to
`DisplayRow` fields. Adding a new field means touching 3 enums + `SearchFilters`
\+ `from_params()` + `matches_record()` + `apply_search_filters()` +
`write_display_row_columns()` + all frontend parsers.

### 4.3 Target: One `FieldId` enum, type-driven dispatch

> **Reconciled 2026-04-06.** Code currently has **39 variants** (see field.rs).
> This target enum shows all 56 (39 implemented + 17 cold-path planned for Wave 5).
> Fields marked ✅ exist in code; fields marked ❌ are planned but not yet in the enum.
> Four fields (marked ²) were added organically after this target was first written.

```rust
/// Every addressable field in the UFFS schema.
/// Used by filter, sort, output, and all frontends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldId {
    // ── Core Identity ──────────────────────────────────────
    Path,           // ✅ String  — derived: parent chain walk
    Name,           // ✅ String  — from CompactRecord (names blob)
    PathOnly,       // ✅ String  — derived: Path minus filename
    Extension,      // ✅ String  — derived: after last dot in Name
    Type,           // ✅ String  — derived: semantic category from Extension
    Drive,          // ✅ char    — from DriveCompactIndex.letter

    // ── Size & Storage ─────────────────────────────────────
    Size,           // ✅ u64     — CompactRecord.size
    SizeOnDisk,     // ✅ u64     — CompactRecord.allocated  (target called this `Allocated`)
    TreeSize,       // ✅ u64     — CompactRecord.treesize
    TreeAllocated,  // ✅ u64     — CompactRecord.tree_allocated
    Bulkiness,      // ✅ f64     — derived: allocated / size

    // ── Time: $STANDARD_INFORMATION (hot path) ─────────────
    Created,        // ✅ i64 µs  — CompactRecord.created
    Modified,       // ✅ i64 µs  — CompactRecord.modified
    Accessed,       // ✅ i64 µs  — CompactRecord.accessed

    // ── Length fields (added after original target) ²────────
    NameLength,     // ✅² u16   — CompactRecord.name_len
    PathLength,     // ✅² u16   — derived: full path length

    // ── Raw aggregate ──────────────────────────────────────
    Attributes,     // ✅ u32     — CompactRecord.flags (formatted attribute string)
    AttributeValue, // ✅² u32   — CompactRecord.flags (raw numeric value)
    ParityAttributes, // ✅² u32 — legacy 25-column C++ mask (may deprecate)

    // ── Structure ──────────────────────────────────────────
    Descendants,    // ✅ u32     — CompactRecord.descendants
    DirectoryFlag,  // ✅ bool   — flags & 0x0010  (target called this `Directory`)

    // ── Attribute booleans (from CompactRecord.flags) ──────
    ReadOnly,       // ✅ bool    — flags & 0x0001
    Hidden,         // ✅ bool    — flags & 0x0002
    System,         // ✅ bool    — flags & 0x0004
    Archive,        // ✅ bool    — flags & 0x0020
    Temporary,      // ✅ bool    — flags & 0x0100
    Sparse,         // ✅ bool    — flags & 0x0200
    Reparse,        // ✅ bool    — flags & 0x0400
    Compressed,     // ✅ bool    — flags & 0x0800
    Offline,        // ✅ bool    — flags & 0x1000
    NotIndexed,     // ✅ bool    — flags & 0x2000
    Encrypted,      // ✅ bool    — flags & 0x4000
    Integrity,      // ✅ bool    — flags & 0x8000
    Virtual,        // ✅ bool    — flags & 0x10000
    NoScrub,        // ✅ bool    — flags & 0x20000
    RecallOnOpen,   // ✅ bool    — flags & 0x40000
    Pinned,         // ✅ bool    — flags & 0x80000
    Unpinned,       // ✅ bool    — flags & 0x100000
    RecallOnDataAccess, // ✅ bool — flags & 0x400000

    // ═══════════════════════════════════════════════════════
    // BELOW: 17 cold-path fields — planned for Wave 5.
    // NOT YET IN CODE. Require ExtraRecordFields / MFT re-read.
    // ═══════════════════════════════════════════════════════

    // ── Time: $FILE_NAME (cold path) ───────────────────────
    FnCreated,      // ❌ i64 µs  — ExtraRecordFields.fn_created
    FnModified,     // ❌ i64 µs  — ExtraRecordFields.fn_modified
    FnAccessed,     // ❌ i64 µs  — ExtraRecordFields.fn_accessed
    FnMftChanged,   // ❌ i64 µs  — ExtraRecordFields.fn_mft_changed

    // ── Structure (cold path) ──────────────────────────────
    Frs,            // ❌ u64     — from MftIndex (not in CompactRecord)
    ParentFrs,      // ❌ u64     — from MftIndex
    BaseFrs,        // ❌ u64     — ExtraRecordFields.base_frs
    SequenceNumber, // ❌ u16     — ExtraRecordFields.sequence_number
    Namespace,      // ❌ u8      — ExtraRecordFields.namespace (0=POSIX,1=Win32,2=DOS,3=Win32+DOS)
    NameCount,      // ❌ u16     — from MftIndex (not in CompactRecord)
    StreamCount,    // ❌ u16     — from MftIndex

    // ── Journaling (cold path) ─────────────────────────────
    Usn,            // ❌ u64     — ExtraRecordFields.stdinfo_usn
    Lsn,            // ❌ u64     — ExtraRecordFields.lsn

    // ── Security (cold path) ───────────────────────────────
    SecurityId,     // ❌ u32     — ExtraRecordFields.security_id
    OwnerId,        // ❌ u32     — ExtraRecordFields.owner_id

    // ── Reparse (cold path) ────────────────────────────────
    ReparseTag,     // ❌ u32     — ExtraRecordFields.reparse_tag

    // ── Forensic (cold path) ───────────────────────────────
    ForensicFlags,  // ❌ u8      — ExtraRecordFields.forensic_flags (bitmask → named strings)
                    //          "deleted","corrupt","extension","has_data","has_i30","unified"
}
```

Total: **56 fields** — 39 implemented (✅) + 17 cold-path planned (❌).
Previously reported as "52 fields"; 4 fields were added after the original
target was written (marked ² above).

### 4.4 Field metadata table (derived at compile time)

Each `FieldId` carries static metadata:

```rust
pub struct FieldMeta {
    /// Canonical name for CLI/TUI/daemon/MCP parsing.
    pub name: &'static str,
    /// Aliases (e.g., "written" → Modified, "date" → Modified).
    pub aliases: &'static [&'static str],
    /// Rust type category (determines filter ops + sort behavior).
    pub field_type: FieldType,
    /// Where the data lives at query time.
    pub access: FieldAccess,
    /// Default sort direction (true = descending).
    pub default_desc: bool,
    /// Display header name (matches C++ output exactly).
    pub display_name: &'static str,
}

pub enum FieldType {
    String,     // → filter: contains, glob, regex, eq, starts_with, ends_with, length
    U64,        // → filter: =, !=, <, >, <=, >=, between
    I64,        // → filter: =, !=, <, >, <=, >=, between, newer/older
    U32,        // → filter: =, !=, <, >, <=, >=, bitmask
    U16,        // → filter: =, !=, <, >, <=, >=
    U8,         // → filter: =, !=, bitmask
    F64,        // → filter: =, !=, <, >, <=, >=, between
    Bool,       // → filter: is_set, not_set
    Char,       // → filter: eq, in
    Timestamp,  // → filter: newer, older, between, before, after (parses "7d", "2026-01-15")
    /// Named enum — stored as integer, filtered/sorted/output as string.
    /// Exactly one value selected. Lookup table maps name ↔ integer.
    /// Examples:
    ///   Namespace: "posix"=0, "win32"=1, "dos"=2, "win32dos"=3
    ///   Type:      "document", "picture", "video", ... (FileCategory enum)
    NamedEnum(&'static [(&'static str, u8)]),
    /// Named bitmask — stored as integer, each bit has a string name.
    /// Multiple bits can be set simultaneously. Output as comma-separated.
    /// Filter: has/not operators on individual flag names.
    /// Examples:
    ///   ForensicFlags: "deleted"=0x01, "corrupt"=0x02, "extension"=0x04, ...
    ///   (NTFS Attributes already handled as 19 individual Bool FieldIds,
    ///    but ForensicFlags uses this pattern since the bits are forensic-only
    ///    and don't warrant 6 separate FieldId variants)
    NamedBitmask(&'static [(&'static str, u8)]),
}

pub enum FieldAccess {
    /// Available directly from CompactRecord (72 bytes, in L1 cache).
    /// Can be filtered/sorted DURING search (pre-path-resolution).
    Hot,
    /// Requires loading ExtraRecordFields from .uffs cache (disk I/O).
    /// Can only be filtered/sorted AFTER initial search + match.
    Cold,
    /// Computed from Hot fields (no extra I/O).
    Derived,
}
```

### 4.5 Field access tiers — performance implications

This is the key architectural insight. Not all fields are equal:

```
┌──────────────────────────────────────────────────────────────┐
│ HOT PATH (CompactRecord — 72 bytes, in L1 cache)             │
│ Filterable/sortable DURING trigram+match loop                │
│                                                              │
│ size, allocated, treesize, created, modified, accessed,      │
│ flags (→ all 19 bool attrs), descendants, extension_id,     │
│ name (via names blob), parent_idx                            │
│                                                              │
│ ⚡ Cost: 0 — already loaded, branch-only checks              │
└──────────────────────────┬───────────────────────────────────┘
                           │ matched indices
                           ▼
┌──────────────────────────────────────────────────────────────┐
│ DERIVED (computed from hot fields — no I/O)                  │
│ Computed AFTER path resolution, before output                │
│                                                              │
│ path, path_only, extension, type, drive, bulkiness,         │
│ tree_allocated, directory (from flags)                       │
│                                                              │
│ ⚡ Cost: microseconds — string ops on match set only          │
└──────────────────────────┬───────────────────────────────────┘
                           │ display rows
                           ▼
┌──────────────────────────────────────────────────────────────┐
│ COLD PATH (ExtraRecordFields — disk seek per record)         │
│ Loaded ON-DEMAND from .uffs cache, only for matched rows     │
│                                                              │
│ frs, parent_frs, base_frs, sequence_number, namespace,      │
│ name_count, stream_count, usn, lsn, security_id, owner_id,  │
│ reparse_tag, fn_created, fn_modified, fn_accessed,           │
│ fn_mft_changed, forensic_flags                               │
│                                                              │
│ 🐢 Cost: ~0.1ms per record (disk seek + read 195 bytes)      │
│    Cached: 512-entry LRU in FullRecordReader                 │
│    For 500 results: ~50ms (amortized by cache)               │
└──────────────────────────────────────────────────────────────┘
```

**Key design rule:** Filters on cold-path fields are applied AFTER the
initial search returns matched indices. The pipeline becomes:

```
trigram → match → HOT filter → sort (hot) → limit → resolve paths
→ DERIVED filter → COLD filter (on-demand load) → COLD sort (re-sort)
→ output (project selected columns)
```

### 4.6 Unified filter specification

All frontends (CLI, TUI, Daemon, GUI, MCP) parse user input into the same
filter representation:

```rust
/// A single field predicate.
pub struct FieldPredicate {
    pub field: FieldId,
    pub op: FilterOp,
}

/// Filter operations, determined by FieldType.
///
/// Coverage map vs LOG/Output target "Suggested Query Capabilities":
///
///   String:  Equals ✅ Contains ✅ Not Contains ✅ Begins With ✅
///            Ends With ✅ Length ✅ Regex ✅
///   Numeric: = ✅ ≠ ✅ < ✅ > ✅ ≤ ✅ ≥ ✅ Between ✅
///            Top/Bottom N → sort + limit (not a filter op)
///   Time:    Before ✅ After ✅ Between ✅
///            Today/Yesterday/YTD/Relative → resolved to Before/After at parse time
///            Group by → output grouping, not a filter (see §4.7)
///   Bool:    Is Set ✅ Not Set ✅
///            Multi-flag → multiple FieldPredicates combined
pub enum FilterOp {
    // ── Numeric (u16/u32/u64/i64/f64) ────────────
    Eq(i64),
    NotEq(i64),
    Gt(i64),
    Gte(i64),
    Lt(i64),
    Lte(i64),
    Between(i64, i64),

    // ── Timestamp sugar ──────────────────────────
    // Resolved to i64 µs at parse time. Accepts:
    //   Duration: "7d", "24h", "30m", "90s", "2w"
    //   ISO date: "2026-01-15"
    //   Named:    "today", "yesterday", "ytd",
    //             "this_week", "last_week", "this_month",
    //             "last_month", "this_year", "last_year"
    NewerThan(String),
    OlderThan(String),
    TimeBetween(String, String),  // two time specs → (lower_us, upper_us)

    // ── String ───────────────────────────────────
    StringEq(String),
    Contains(String),
    NotContains(String),
    StartsWith(String),
    EndsWith(String),
    LengthGte(u32),          // name length ≥ N
    LengthLte(u32),          // name length ≤ N
    Glob(String),
    Regex(String),

    // ── Boolean (from flags bitmask) ─────────────
    IsSet,
    NotSet,

    // ── Set membership ───────────────────────────
    In(Vec<String>),         // ext filter: --ext jpg,png,gif

    // ── Collection alias (expands to In) ─────────
    // Resolved at parse time via ExtensionFilter::parse():
    //   "documents"  → doc,docx,pdf,txt,rtf,odt,xls,xlsx,ppt,pptx,csv,md
    //   "pictures"   → jpg,jpeg,png,gif,bmp,tiff,tif,webp,svg,ico,raw,heic
    //   "videos"     → mp4,avi,mkv,mov,wmv,flv,webm,mpeg,mpg,m4v,3gp
    //   "music"      → mp3,wav,flac,aac,ogg,wma,m4a,opus,aiff
    //   "archives"   → zip,rar,7z,tar,gz,bz2,xz,iso
    //   "code"       → rs,py,js,ts,java,c,cpp,h,hpp,go,rb,php,swift,kt
    // Aliases: "images"="pictures", "docs"="documents", "video"="videos",
    //          "audio"="music", "compressed"="archives", "source"="code"
    // Mix: --ext "documents,mp4,heic" → collection + individual
    //
    // NOTE: ExtensionFilter already exists in uffs-core/extensions/mod.rs
    // with full parse + match support. Currently only used by --ext flag
    // in SearchFilters. The FieldPredicate system should delegate to it.
    InCollection(String),
}
```

**Coverage vs target query capabilities:**

| Target capability | FilterOp | Notes |
|-------------------|----------|-------|
| **String: Equals** | `StringEq` | Case-insensitive by default |
| **String: Contains** | `Contains` | Substring match |
| **String: Not Contains** | `NotContains` | Inverse substring match |
| **String: Begins With** | `StartsWith` | Prefix match |
| **String: Ends With** | `EndsWith` | Suffix match |
| **String: Length** | `LengthGte` / `LengthLte` | Filter by name/path length |
| **String: Regex** | `Regex` | Full regex on String fields |
| **Numeric: =, ≠, <, >, ≤, ≥** | `Eq`..`Lte` | All 6 comparison ops |
| **Numeric: Between** | `Between(lo, hi)` | Inclusive range |
| **Numeric: Top/Bottom N** | `--sort field --limit N` | Composition of sort + limit, not a filter op |
| **Time: Before / After** | `OlderThan` / `NewerThan` | Parsed from duration or date string |
| **Time: Between** | `TimeBetween(from, to)` | Two time specs → µs range |
| **Time: Today** | `NewerThan("today")` | Resolved at parse time to midnight µs |
| **Time: Yesterday** | `TimeBetween("yesterday", "today")` | Yesterday 00:00 → today 00:00 |
| **Time: YTD** | `NewerThan("ytd")` | Jan 1 of current year |
| **Time: Relative** | `NewerThan("this_week")` etc. | Named periods resolved at parse time |
| **Time: Group by** | Output grouping (see §4.7) | Not a filter — display-layer concern |
| **Bool: Is Set** | `IsSet` | Attribute flag is 1 |
| **Bool: Not Set** | `NotSet` | Attribute flag is 0 |
| **Bool: Multi-flag** | Multiple `FieldPredicate`s | `--filter "hidden:set,compressed:set,system:notset"` |
| **Extension: Collection** | `InCollection("documents")` | `--ext documents` expands to 12 extensions |
| **Extension: Mixed** | `InCollection` + `In` | `--ext "documents,mp4,heic"` = collection + individual |

**Existing collection support (uffs-core/extensions/mod.rs):**

| Collection name | Aliases | Extensions |
|-----------------|---------|------------|
| `pictures` | `images` | jpg, jpeg, png, gif, bmp, tiff, tif, webp, svg, ico, raw, heic |
| `documents` | `docs` | doc, docx, pdf, txt, rtf, odt, xls, xlsx, ppt, pptx, csv, md |
| `videos` | `video` | mp4, avi, mkv, mov, wmv, flv, webm, mpeg, mpg, m4v, 3gp |
| `music` | `audio` | mp3, wav, flac, aac, ogg, wma, m4a, opus, aiff |
| `archives` | `compressed` | zip, rar, 7z, tar, gz, bz2, xz, iso |
| `code` | `source` | rs, py, js, ts, java, c, cpp, h, hpp, go, rb, php, swift, kt |

> `ExtensionFilter::parse()` already supports mixing: `"documents,mp4,heic"`
> parses to the document collection + individual `mp4` + `heic`. This is fully
> implemented and tested. The gap is that `SearchFilters::from_params()` splits
> extensions as raw strings instead of routing through `ExtensionFilter::parse()`.

**CLI syntax (proposal — comma-separated, position = rank for sort):**

```bash
# Filter: field:op:value (colon-separated)
uffs "*.txt" --filter "size:gt:1M,modified:newer:7d,hidden:set"

# Sort: field:direction (position = tier rank)
uffs "*" --sort "size:desc,modified:desc,name:asc"

# Output: field list (position = column order)
uffs "*" --columns "name,size,modified,path"

# Backward-compatible shorthand (existing flags still work):
uffs "*.txt" --newer 7d --min-size 1M --sort size --attr hidden
# Equivalent to:
uffs "*.txt" --filter "modified:newer:7d,size:gte:1048576,hidden:set" --sort "size:desc"
```

**Key point:** The existing CLI flags (`--newer`, `--min-size`, `--attr`,
`--sort`, `--ext`, `--exclude`) remain as ergonomic shortcuts. They are
parsed into `Vec<FieldPredicate>` and `Vec<SortSpec>` internally. The
`--filter` flag is the general form for power users and programmatic access.

### 4.7 Time grouping is output formatting, not aggregation

The target doc lists "Group by: Day / Week / Month / Year" under Time
Fields. This sounds like SQL `GROUP BY` but it's not — we still show
individual rows, just visually grouped with section headers:

```
── 2026-03-29 (Today) ──────────────────────────
  report.docx     1.2 MB   2026-03-29 14:30
  budget.xlsx     340 KB   2026-03-29 09:15
── 2026-03-28 (Yesterday) ──────────────────────
  notes.txt       12 KB    2026-03-28 18:00
── 2026-03-25 ──────────────────────────────────
  backup.zip      4.5 GB   2026-03-25 02:00
```

This is a **display-layer concern**, not a query concern. Implementation:

```rust
pub enum GroupBy {
    None,
    Day,      // group header per calendar day
    Week,     // group header per ISO week
    Month,    // group header per year-month
    Year,     // group header per year
}
```

Applied AFTER sort + limit, in the output formatting layer. The output
writer inserts group headers when the time bucket changes between
consecutive rows. Requires the rows to be sorted by the grouped time
field (enforced automatically: `--group-by day` implies `--sort modified:desc`
unless an explicit sort is specified).

**No aggregation functions needed.** If a future use case requires
"count of files per day" or "total size per month", that's a different
feature (analytics/reporting), not search.

### 4.8 How this replaces the current struct proliferation

**Today (pre-D5/D6):** 4 CLI structs + TUI struct + daemon struct = 6 structs
doing the same job in 6 different ways:

```
CLI:    SearchConfig → QueryFilters → OwnedQueryFilters → SearchFilters
TUI:    SearchState → SearchFilters (direct — works correctly)
Daemon: SearchParams → SearchFilters::default() (BROKEN — filters ignored)
```

**After D5+D6 (all-through-daemon):** 2 structs total:

```
ALL FRONTENDS:
  CLI args (clap)  ──────┐
  TUI toggles  ──────────┤
  MCP params  ────────────┤──→  SearchParams  ──→  daemon
  GUI actions  ───────────┘     (wire type)        IndexManager::search()
                                                     ↓
                                              SearchFilters::from_params()
                                              (engine type — built once)
```

`SearchParams` is the **wire type** (JSON-serializable, `Send`, frontend-
agnostic). `SearchFilters` is the **engine type** (pre-parsed µs timestamps,
bitmasks, compiled regexes). The daemon builds the latter from the former
exactly once per query. No standalone mode — all search goes through the
daemon, bulk results delivered via shared memory (see §1.5).

**Where `FieldId` / `FieldPredicate` fit (post-D5/D6):**

```
SearchParams {
  pattern: String,
  case_sensitive: bool,
  whole_word: bool,
  predicates: Vec<FieldPredicate>,   // replaces 15 individual filter fields
  sort_specs: Vec<SortSpec>,         // uses FieldId for column reference
  filter_mode: FilterMode,
  limit: u32,
  columns: Option<Vec<FieldId>>,     // output column selection
}
```

Each frontend parses its UI into `Vec<FieldPredicate>` and `Vec<SortSpec>`.
The daemon's `IndexManager::search()` splits predicates by access tier
(hot vs cold), builds `SearchFilters` from hot predicates, runs the search,
then applies cold predicates on the matched rows.

### 4.9 Rollout strategy: D5 first, then unified field system

> **Wave 2 is absorbed into D5.** The 14 broken CLI filter flags are
> broken because of the `SearchConfig → QueryFilters → OwnedQueryFilters →
> SearchFilters` pipeline. D5 **deletes this entire pipeline** — the CLI
> sends `SearchParams` to the daemon, which builds `SearchFilters`
> correctly. Fixing the broken wiring pre-D5 would be throwaway work.

**Phase A (D5+D6): All frontends through daemon — NEXT.**
Complete CLI migration (D5) and TUI migration (D6). After this, every
search goes through `SearchParams → daemon → results`. No standalone
mode. Bulk results use shared memory (see §1.5). The DRY problem
vanishes — one search path, one filter implementation. The 14 broken
CLI filter flags are fixed automatically (broken wiring deleted).

**Phase B (Wave 3): Introduce `FieldId` enum — AFTER D5+D6.**
Create `FieldId` with metadata in `uffs-core`. Unify `OutputColumn`,
`SortColumn`, `TuiColumn` into `FieldId`. Since all frontends now go
through the daemon, this is ONE implementation in ONE place.

**Phase C (Wave 4): Introduce `FieldPredicate` + `FilterOp`.**
Replace `SearchFilters` bespoke struct with predicate-driven filtering.
Add `--filter` CLI flag. The daemon builds predicates and splits by
access tier. Each frontend just populates `SearchParams.predicates`.

**Phase D (Wave 5): Cold-path output + filter.**
Add cold-path `FieldId` variants. The daemon loads `ExtraRecordFields`
on demand for matched rows. No frontend changes needed — just add fields
to `SearchParams.columns`.

---

## 5. Complete Field Coverage Analysis

### 5.1 Hot path fields (in CompactRecord — always available)

| Field | FieldId | Type | Filter today | Sort today | Output today |
|-------|---------|------|-------------|-----------|-------------|
| Name | `Name` | String | ✅ pattern match | ✅ `SortColumn::Name` | ✅ `OutputColumn::Name` |
| Size | `Size` | u64 | ✅ `min_size`/`max_size` | ✅ `SortColumn::Size` | ✅ `OutputColumn::Size` |
| Allocated | `Allocated` | u64 | ✅ `min/max_allocated` | ✅ `SortColumn::SizeOnDisk` | ✅ `OutputColumn::SizeOnDisk` |
| TreeSize | `TreeSize` | u64 | ✅ `min/max_treesize` | ✅ `FieldId::TreeSize` | ✅ `OutputColumn::TreeSize` |
| TreeAllocated | `TreeAllocated` | u64 | ✅ `min/max_tree_allocated` | ✅ `FieldId::TreeAllocated` | ✅ `OutputColumn::TreeAllocated` |
| Created | `Created` | i64 | ✅ `newer/older_created` | ✅ `SortColumn::Created` | ✅ `OutputColumn::Created` |
| Modified | `Modified` | i64 | ✅ `newer/older` | ✅ `SortColumn::Modified` | ✅ `OutputColumn::Modified` |
| Accessed | `Accessed` | i64 | ✅ `newer/older_accessed` | ✅ `SortColumn::Accessed` | ✅ `OutputColumn::Accessed` |
| Flags | `Attributes` | u32 | ✅ `attr_require/exclude` | ❌ | ✅ `OutputColumn::Attributes` |
| Descendants | `Descendants` | u32 | ✅ `min/max_descendants` | ✅ `SortColumn::Descendants` | ✅ `OutputColumn::Descendants` |
| extension_id | (internal) | u16 | ✅ `extensions` | ✅ `SortColumn::Extension` | (via Extension) |
| NameLength | `NameLength` | u16 | ✅ `min/max_name_len` (hot) | ✅ `FieldId::NameLength` | ✅ daemon projection |
| PathLength | `PathLength` | u16 | ✅ `min/max_path_len` (post) | ✅ `FieldId::PathLength` | ✅ daemon projection |

### 5.2 Derived fields (computed from hot path — no I/O)

| Field | FieldId | Type | Filter today | Sort today | Output today |
|-------|---------|------|-------------|-----------|-------------|
| Path | `Path` | String | ✅ path-aware + `path:` prefix | ✅ `SortColumn::Path` | ✅ `OutputColumn::Path` |
| PathOnly | `PathOnly` | String | ✅ `--in-path` (glob on dir portion) | ✅ `FieldId::PathOnly` | ✅ `OutputColumn::PathOnly` |
| Extension | `Extension` | String | ✅ `--ext` | ✅ `SortColumn::Extension` | ✅ `OutputColumn::Type` |
| Type | `Type` | String | ✅ `--type CATEGORY` | ✅ `FieldId::Type` sort | ✅ `semantic_type_for_row()` |
| Drive | `Drive` | char | ✅ `--drive` | ✅ `SortColumn::Drive` | (in path prefix) |
| Bulkiness | `Bulkiness` | u64 | ✅ `--min/max-bulkiness` | ✅ `FieldId::Bulkiness` | ✅ `allocated*100/size` (integer %) |
| 19 bool attrs | `Hidden`, ... | bool | ✅ `--attr` | ✅ `field_to_attr_bit` flag sort | ✅ individual columns |

> ⁴⁵⁶ **Type field — fully implemented (2026-04-05).**
>
> - **Filter:** `--type code` → `SearchFilters.type_filter` post-filter via
>   `semantic_type_for_row()`. 24 categories: archive, audio, backup, cad,
>   cert, code, config, data, database, directory, disk, document, ebook,
>   executable, file, font, log, other, picture, script, shortcut, system,
>   video, web.
> - **Sort:** `FieldId::Type` sorts by `semantic_type_for_row()` category
>   name (alphabetical, case-folded). No longer sorts by devicon glyph.
> - **Output:** `OutputColumn::Type` outputs the category name (e.g.
>   `"code"`, `"document"`, `"picture"`). TUI retains devicon glyph as a
>   visual decoration in the Type column.
>
> **Bool attr sort — fully implemented (2026-04-05).** All 19 boolean
> attribute fields now sort by flag bit value (true/false grouping) with
> name tiebreaker, via `field_to_attr_bit()`. `--sort hidden:desc` puts
> hidden files first.

### 5.3 Cold path fields (require ExtraRecordFields from .uffs cache)

| Field | FieldId | Type | Filter today | Sort today | Output today |
|-------|---------|------|-------------|-----------|-------------|
| FRS | `Frs` | u64 | ❌ | ❌ | ❌ |
| Parent FRS | `ParentFrs` | u64 | ❌ | ❌ | ❌ |
| Base FRS | `BaseFrs` | u64 | ❌ | ❌ | ❌ |
| Sequence Number | `SequenceNumber` | u16 | ❌ | ❌ | ❌ |
| Namespace | `Namespace` | u8 → String ⁷ | ❌ | ❌ | ❌ |
| Name Count | `NameCount` | u16 | ❌ | ❌ | ❌ |
| Stream Count | `StreamCount` | u16 | ❌ | ❌ | ❌ |
| USN | `Usn` | u64 | ❌ | ❌ | ❌ |
| LSN | `Lsn` | u64 | ❌ | ❌ | ❌ |
| Security ID | `SecurityId` | u32 | ❌ | ❌ | ❌ |
| Owner ID | `OwnerId` | u32 | ❌ | ❌ | ❌ |
| Reparse Tag | `ReparseTag` | u32 | ❌ | ❌ | ❌ |
| $FN Created | `FnCreated` | i64 | ❌ | ❌ | ❌ |
| $FN Modified | `FnModified` | i64 | ❌ | ❌ | ❌ |
| $FN Accessed | `FnAccessed` | i64 | ❌ | ❌ | ❌ |
| $FN MFT Changed | `FnMftChanged` | i64 | ❌ | ❌ | ❌ |
| Forensic Flags | `ForensicFlags` | u8 → String ⁸ | ❌ | ❌ | ❌ |

> ⁷ **Namespace uses named strings, not raw u8.** NTFS namespace values:
>
> | u8 | String | Meaning |
> |---|---|---|
> | 0 | `posix` | Case-sensitive, allows almost any character |
> | 1 | `win32` | Standard Windows long filename |
> | 2 | `dos` | 8.3 short filename (auto-generated) |
> | 3 | `win32dos` | Long name that also satisfies 8.3 rules |
>
> Filter, sort, and output all use the string form:
> - `--filter "namespace:eq:dos"` → find 8.3 short names
> - `--filter "namespace:eq:posix"` → find case-sensitive POSIX names
> - `--columns name,namespace` → outputs `"win32"`, `"dos"`, etc.
> - `--sort namespace` → alphabetical by name (`dos`, `posix`, `win32`, `win32dos`)

> ⁸ **Forensic Flags uses named strings, not raw u8.** Unlike Namespace
> (a named enum — one value), Forensic Flags is a **bitmask** of independent
> booleans — same pattern as NTFS attribute flags (`--attr hidden,system`).
>
> | Bit | String | Meaning |
> |-----|--------|---------|
> | 0 | `deleted` | MFT record not in use (FRS freed/recycled) |
> | 1 | `corrupt` | USA fixup failed (torn write / disk error) |
> | 2 | `extension` | Extension record (not a standalone base record) |
> | 3 | `has_data` | Unnamed `$DATA` attribute was found |
> | 4 | `has_i30` | `$I30` directory index stream was counted |
> | 5 | `unified` | Record created by unified parser |
>
> Filter, sort, and output all use the string form:
> - `--filter "forensic:has:deleted"` → find deleted/recycled records
> - `--filter "forensic:has:deleted,corrupt"` → find deleted OR corrupt
> - `--filter "forensic:not:extension"` → exclude extension records
> - `--columns name,forensic` → outputs `"deleted"`, `"deleted,corrupt"`, etc.
> - `--sort forensic` → alphabetical by flag combination string
>
> Note: bits 3–5 (`has_data`, `has_i30`, `unified`) are internal parser state,
> not user-facing forensic flags. They may be excluded from output/filter or
> placed under a `--forensic-internal` flag.

**All 17 cold-path fields are fully parsed and available** via
`FullRecordReader.get_extra_fields()` — but none are wired to
filter, sort, or output yet.

### 5.4 Coverage summary

> **Reconciled 2026-04-06.** Code has **39 `FieldId` variants** (see §4.3).
> The prior "55" total was a counting artifact (see explanation below table).

| Category | `FieldId` variants | Filterable | Sortable | Outputtable |
|----------|-------------------|-----------|----------|-------------|
| Hot path (size, timestamps, flags, descendants, name/path length) | 13 | 13/13 ✅ | 12/13 | 13/13 ✅ |
| Derived (Path, PathOnly, Extension, Type, Drive, Bulkiness) | 6 | 6/6 ✅ | 6/6 ✅ | 6/6 ✅ |
| Bool attrs (Hidden … RecallOnDataAccess) | 19 | 19/19 ✅ | 19/19 ✅ | 19/19 ✅ |
| Code-only additions² (AttributeValue, ParityAttributes, NameLength, PathLength) | — | — | — | — |
| Cold path (not yet in `FieldId` — Wave 5) | 17 | 0/17 ❌ | 0/17 ❌ | 0/17 ❌ |
| **Total (implemented)** | **39** | **38/39** | **37/39** | **38/39** |
| **Total (including planned)** | **56** | **38/56** | **37/56** | **38/56** |

> ² Four fields (NameLength, PathLength, AttributeValue, ParityAttributes)
> were added to code after the original 52-variant target was written.
> NameLength and PathLength are already counted in "Hot path" above (rows
> in §5.1). AttributeValue and ParityAttributes are raw-value / legacy
> variants of the `Attributes` field.
>
> The prior "55" count arose because the §5.1 analysis table included
> `extension_id` (an internal ID, not a `FieldId`) and counted
> NameLength/PathLength without adding them to the 52-variant target enum:
> 13+6+19+17 = 55 in the summary while the enum had 52.

> Updated 2026-04-05: Hot-path and derived fields **fully wired** across
> filter/sort/output. `TreeAllocated`/`TreeSize` filterable via
> `--min/max-treesize` and `--min/max-tree-allocated`. `Bulkiness`
> filterable via `--min/max-bulkiness`. `Type` filterable via `--type`.
> `PathOnly` filterable via `--in-path`. Bool attrs sortable via
> `field_to_attr_bit()`. `SearchFilterParams` struct replaces 27-arg
> positional function signature.

### 5.5 Type field: Unified category registry

**Problem:** The `Type` field has three inconsistent implementations:

- CLI output: raw extension string (`rs`, `docx`)
- TUI column: devicon glyph character (``, ``)
- Sort: compares devicon glyphs (Unicode codepoint order, meaningless)

None of these returns a human-readable **category name** like `document`,
`picture`, `video`. Users cannot filter by `--filter "type:eq:document"`
or sort alphabetically by category.

**Target:** A category registry that maps extensions → named categories,
reusing the existing `collections` module as the source of truth:

```rust
/// File type categories — superset of extension collections.
/// Extensions not in any collection → "other".
pub enum FileCategory {
    Document,   // doc, docx, pdf, txt, rtf, odt, xls, xlsx, ppt, pptx, csv, md
    Picture,    // jpg, jpeg, png, gif, bmp, tiff, tif, webp, svg, ico, raw, heic
    Video,      // mp4, avi, mkv, mov, wmv, flv, webm, mpeg, mpg, m4v, 3gp
    Audio,      // mp3, wav, flac, aac, ogg, wma, m4a, opus, aiff
    Archive,    // zip, rar, 7z, tar, gz, bz2, xz, iso
    Code,       // rs, py, js, ts, java, c, cpp, h, hpp, go, rb, php, swift, kt
    Executable, // exe, msi, bat, cmd, ps1, sh, com, scr
    Font,       // ttf, otf, woff, woff2, eot
    Database,   // db, sqlite, mdb, accdb, sql, ldf, mdf, ndf
    Config,     // ini, cfg, conf, yaml, yml, toml, json, xml, properties
    Log,        // log, out, err, trace
    Backup,     // bak, old, orig, swp, tmp, temp
    Disk,       // vmdk, vhd, vhdx, vdi, qcow2, img, wim
    Other,      // everything else
}
```

**How it works:**

1. At index time: `extension_id` is already stored in `CompactRecord`.
2. At query time: `ExtensionRegistry.get_extension(id)` → extension string.
3. Category lookup: `FileCategory::from_extension("docx")` → `Document`.
4. Build the lookup as a `HashMap<&str, FileCategory>` from the existing
   `collections::DOCUMENTS`, `collections::PICTURES`, etc. arrays +
   new arrays for `Executable`, `Font`, `Database`, `Config`, `Log`,
   `Backup`, `Disk`.

**Then:**

- `--filter "type:eq:document"` → filter by category name
- `--sort type` → sort alphabetically by category name
- `--columns type` → output `"document"`, `"picture"`, `"video"`, etc.
- `--ext documents` → existing collection alias (unchanged)
- TUI Type column → category name (devicon glyph stays as Name decoration)

**Where this lives:** Extend `uffs-core/extensions/mod.rs` with `FileCategory`
enum + `from_extension()` lookup. The existing `collections` arrays become
the single source of truth for both extension filtering AND type classification.

---

## 6. Implementation Roadmap

### 6.0 Dependency Graph

```
D5 (CLI → daemon) ──→ D6 (TUI → daemon) ──→ Wave 3 ──→ Wave 4 ──→ Wave 5
│                                               │
├── Deletes broken CLI pipeline                 └── ONE pipeline, ONE place
├── 14 broken filter flags fixed automatically      (FieldId, predicates, cold-path)
└── Bulk results via shared memory (mmap)
```

> **Wave 2 is absorbed into D5.** The 14 broken CLI filter flags exist
> because of the 4-struct wiring pipeline (`SearchConfig → QueryFilters →
> OwnedQueryFilters → SearchFilters`). D5 deletes this entire pipeline.
> The CLI sends `SearchParams` to the daemon, which builds `SearchFilters`
> correctly. No throwaway pre-D5 fix needed.

**Why this order:**

1. **D5+D6 (NEXT):** All frontends route through daemon — no standalone
   mode. CLI, TUI, MCP, GUI all send `SearchParams` to daemon. Bulk
   results (>100K rows) delivered via shared memory for near-native
   speed (see §1.5). The 4-struct CLI pipeline is deleted entirely.
   The 14 broken CLI filter flags are fixed automatically.

2. **Waves 3–5 (AFTER D5+D6):** Build `FieldId`, `FieldPredicate`,
   `FilterOp`, cold-path integration. Since all search goes through
   ONE funnel (daemon), these are implemented ONCE in `uffs-core`,
   consumed by `IndexManager::search()`. No per-frontend wiring.

### 6.1 Execution Order

```
──────────────────────────────────────────────────────────────────────
PHASE A — D5 + D6: All frontends through daemon
  See DAEMON_IMPLEMENTATION_PLAN.md phases D5, D6
  D5 deletes the broken CLI pipeline (QueryFilters, OwnedQueryFilters,
  SearchConfig dead fields) — 14 broken filter flags fixed automatically.
  D5 adds shared memory for bulk results (mmap). No standalone mode.
  D6 replaces TUI in-process index with daemon client.
──────────────────────────────────────────────────────────────────────

  (former Wave 2 tasks absorbed into D5.2 — see DAEMON_IMPLEMENTATION_PLAN.md)
    D5.2.3: Build SearchParams from CLI args (replaces Step 1-2)
    D5.2.6: Delete QueryFilters, OwnedQueryFilters, dead fields (replaces Step 3,6)
    D5.3.4: Test all CLI flags work through daemon (replaces Step 8)

──────────────────────────────────────────────────────────────────────
PHASE B — Unified field system (all search goes through daemon)
  All changes in uffs-core. No per-frontend wiring needed.
──────────────────────────────────────────────────────────────────────

Wave 3 — Unified FieldId enum + derived field fixes
  Step 1: Create FieldId enum + FieldMeta table in uffs-core
  Step 2: Replace OutputColumn/SortColumn/TuiColumn with FieldId
  Step 3: Unify parse_sort_spec() to use FieldId
  Step 4: Unify output column parsing to use FieldId
  Step 5: Extend SearchParams with FieldId-based sort/columns
  Step 6: Compute Bulkiness (allocated / size) — replace hardcoded "0"
  Step 7: Compute TreeAllocated (sum allocated in subtree)
  Step 8: FileCategory enum — ext→category registry (see §5.5)

Wave 4 — Predicate-driven filtering + time sugar
  Step 9:  Create FieldPredicate + FilterOp types in uffs-core
  Step 10: Replace SearchFilters.from_params() with from_predicates()
  Step 11: Split predicates into hot/cold at search time
  Step 12: Add --filter CLI flag (general form) → SearchParams.predicates
  Step 13: Add named time specs (today/yesterday/ytd/this_week/etc.)
  Step 14: Add --group-by flag (output grouping by time bucket)

Wave 5 — Cold-path output + filter
  Step 15: Add cold-path FieldId variants to output column selection
  Step 16: Integrate FullRecordReader into daemon search pipeline
  Step 17: Add cold-path filtering (post-search, on matched rows)
  Step 18: Add cold-path sorting (re-sort after cold filter)
```

### 6.2 Tracking

**Phase A — D5 + D6 (all frontends through daemon)**

| Phase | Status | Ref | Notes |
|-------|--------|-----|-------|
| D5: CLI → daemon-only (shmem for bulk) | 🔲 | `DAEMON_IMPLEMENTATION_PLAN.md` §D5 | All filter wiring done ✅; shmem + standalone deletion remaining |
| D6: TUI → daemon-only | 🔲 | `DAEMON_IMPLEMENTATION_PLAN.md` §D6 | TUI drops from ~7 GiB to <50 MB |
| **Pre-D5 filter wiring** | ✅ Done | This session (2026-04-04) | All `SearchParams` fields wired; `SearchFilters::from_params()` called; canonical predicates compiled |

> Former Wave 2 tasks (Steps 0–8) have been **completed ahead of D5** via
> direct `SearchParams` expansion + daemon-side `from_params()` wiring.
> The broken `QueryFilters`/`OwnedQueryFilters` pipeline still exists but
> the daemon path bypasses it entirely. D5 will delete the dead code.

**Phase B — AFTER D5+D6 (all search through daemon)**

All changes below are in `uffs-core` only. No per-frontend wiring.

| Step | Wave | Status | Notes |
|------|------|--------|-------|
| 1: FieldId enum | 3 | 🟡 | 39 of 56 variants implemented; 17 cold-path fields deferred to Wave 5 |
| 2: Replace 3 column enums | 3 | 🔲 | OutputColumn/SortColumn/TuiColumn → FieldId |
| 3–5: Unify parsing | 3 | 🔲 | `FieldId::parse()` + extend `SearchParams` |
| 6: Compute Bulkiness | 3 | 🔲 | `allocated / size` ratio |
| 7: Compute TreeAllocated | 3 | 🔲 | Sum allocated in subtree |
| 8: FileCategory enum | 3 | 🔲 | `ext → category` registry (see §5.5) |
| 9–12: Predicates + `--filter` | 4 | 🔲 | FieldPredicate, FilterOp, `from_predicates()` |
| 13: Named time specs | 4 | 🔲 | `today`, `yesterday`, `ytd`, `this_week`, etc. |
| 14: Group-by output | 4 | 🔲 | `--group-by day\|week\|month\|year` |
| 15–18: Cold path | 5 | 🔲 | ExtraRecordFields in output/filter/sort |

### 6.3 Acceptance Criteria

**Filter wiring (✅ DONE):** All CLI filter/sort flags work through the
daemon pipeline. `SearchParams` carries all fields. `SearchFilters::from_params()`
builds the hot-path filter struct. Canonical predicates compile into hot-path
via `compile_predicates_into_filters()`.

```bash
# All formerly broken CLI flags now work through daemon: ✅
uffs "*.txt" --newer 7d
uffs "*" --sort size --files-only --limit 10
uffs "*" --attr hidden --files-only
uffs "*.log" --exclude "backup*"
uffs "*" --sort ext,size:desc --files-only --limit 20
uffs "*" --attr hidden --newer 30d --min-size 1048576 --sort size --files-only

# Extension collections expand correctly (client-side sugar): ✅
uffs --ext documents --files-only --limit 10    # → doc,docx,pdf,txt,...
uffs --ext "pictures,mp4" --limit 10            # → jpg,png,...,mp4
uffs --ext executables --limit 10               # → exe,msi,bat,cmd,...

# New features implemented this session: ✅
uffs "path:projects" --limit 100                # → path: prefix + descendant expansion
uffs "*" --month january --files-only           # → files modified in any January
uffs "*" --month Q1 --sort size --limit 10      # → Q1 (Jan-Mar) files by size
uffs "*" --begins-with report --limit 10        # → report* pattern sugar
uffs "*" --min-name-len 50 --limit 10           # → long filenames
uffs "*" --max-path-len 260 --limit 10          # → near MAX_PATH limit
uffs "*" --attr system-files --limit 10         # → attribute preset expansion
uffs "*" --min-size-on-disk 1G --limit 10       # → size-on-disk filter
```

**D5+D6 (remaining):** Delete standalone mode. All frontends route through
daemon. Bulk queries (>100K results) via shared memory. Delete dead
`QueryFilters`/`OwnedQueryFilters`/`SearchConfig` pipeline.

**Wave 3:** (after D5+D6) `OutputColumn`, `SortColumn`, `TuiColumn` are
replaced by `FieldId`. Adding a field to `FieldId` makes it available
to all frontends automatically via `SearchParams`. `Bulkiness` computes
real `allocated/size` ratio. `TreeAllocated` sums subtree allocated sizes.
`Type` outputs category names:

```bash
# Type field outputs category names:
uffs "*" --columns name,type --limit 10   # → "report.docx  document"
uffs "*" --sort type --limit 20           # → grouped by archive/audio/code/document/...
uffs "*" --filter "type:eq:document"      # → only document extensions
uffs "*" --filter "type:eq:picture"       # → same as --ext pictures
```

**Wave 4:** `--filter "size:gt:1M,modified:newer:7d,hidden:set"` works.
Named time specs: `--filter "modified:newer:today"`. Group-by output:
`--group-by month`. All implemented once in `uffs-core`, consumed by daemon.

**Wave 5:** `--columns name,size,reparse_tag,fn_created` works.
Cold-path fields loaded on-demand by daemon, only for matched rows.

---

## 7. CLI Syntax Design: Research & Recommendation

### 7.1 Prior art survey

How comparable tools handle sort, filter, attribute, and column syntax:

#### Sort

| Tool | Language | Sort syntax | Direction | Multi-tier |
|------|----------|------------|-----------|------------|
| **fd** | Rust | No sort option | N/A | N/A |
| **eza** | Rust | `--sort=size` or `-s size` | `--reverse` flag (global) | No |
| **Everything es.exe** | C (Win) | `-sort size-descending` | `-ascending`/`-descending` suffix | No |
| **Everything GUI** | C++ (Win) | `-sort "Date Modified"` | `-sort-ascending`/`-sort-descending` | No |
| **PowerShell** | .NET | `Sort-Object Size -Descending` | `-Descending` param | `Sort-Object A, B` |
| **Windows DIR** | CMD | `/o-s` (size desc) | `-` prefix = desc | Combine: `/ons` |
| **nushell** | Rust | `sort-by size --reverse` | `--reverse` flag | Positional: `sort-by a b` |
| **kubectl** | Go | `--sort-by=.status.phase` | N/A (always asc) | No |
| **REST APIs** | Various | `?sort=field:desc,field2:asc` | `:asc`/`:desc` suffix | Comma-separated |

**Observations:**
- Most file tools support only single-field sort
- PowerShell and nushell support multi-tier via positional args
- REST APIs universally use `field:direction` with comma separation
- No tool uses `=` or `>` as direction separator (shell-escaping problems)
- Windows tools use `-descending` suffix; POSIX tools use `--reverse` flag

#### Filter

| Tool | Size filter | Date filter | Type/extension |
|------|------------|-------------|----------------|
| **fd** | `-S +1M` / `-S -100k` | `--changed-within 2w` | `-e ext` (repeated), `-t f,d,l` |
| **Everything** | search: `size:>1mb` | search: `dm:today`, `dc:last2weeks` | search: `ext:jpg;png` |
| **PowerShell** | `Where { $_.Length -gt 1MB }` | `Where { $_.LastWriteTime -gt ...}` | `-Include *.jpg` |
| **find** | `-size +1M` | `-mtime -7` (days) | `-name "*.jpg"` |
| **nushell** | `where size > 1mb` | `where modified > 7day ago` | `where name =~ "\.rs$"` |

**Observations:**
- fd uses `+`/`-` prefix for greater/less (compact but unfamiliar)
- Everything uses `function:value` search syntax inline
- nushell uses natural language style (`where field > value`)
- All tools use separate flags for common filters, not a unified `--filter` string

#### Attribute filtering

| Tool | Syntax | Style |
|------|--------|-------|
| **Windows DIR** | `/aRHS`, `/a-S`, `/a:HS` | Single-letter codes, `-` to exclude |
| **Everything es.exe** | `/a[RHSDAVNTPLCOIE]`, `-` to exclude | Same as DIR |
| **fd** | `--type f,d,l,x,e` | Comma-separated words (file/dir/symlink/exec/empty) |
| **PowerShell** | `-Attributes Hidden,System` | Comma-separated full names |
| **UFFS (current)** | `--attr hidden` | Single full name |

**Observations:**
- Windows ecosystem uses single-letter codes (compact, expert-friendly)
- Modern Rust/cross-platform tools use full word names (readable, discoverable)
- Most support comma-separated lists for AND logic
- `!` or `-` prefix for exclusion (NOT)

#### Column selection

| Tool | Syntax |
|------|--------|
| **eza** | Individual flags: `-l` (long), `--no-time`, `--no-user` |
| **Everything es.exe** | Individual flags: `-name`, `-size`, `-dm`, `-ext` |
| **PowerShell** | `Select-Object Name, Length, LastWriteTime` |
| **cut** | `-f 1,3,5` (numeric positions) |
| **kubectl** | `--output=custom-columns=NAME:.metadata.name,STATUS:.status.phase` |

**Observations:**
- File tools use individual flags per column (verbose but discoverable)
- kubectl uses `NAME:jsonpath` (powerful but complex)
- PowerShell uses comma-separated property names (cleanest)

### 7.2 Shell-safety analysis

Delimiters must work unquoted in `cmd.exe`, PowerShell, bash, zsh, and fish:

| Character | bash | zsh | fish | cmd.exe | PowerShell | Safe? |
|-----------|------|-----|------|---------|------------|-------|
| `:` | ✅ | ✅ | ✅ | ✅ | ✅ | **✅ Safe everywhere** |
| `,` | ✅ | ✅ | ✅ | ✅ | ✅ | **✅ Safe everywhere** |
| `=` | ✅ | ✅ | ✅ | ✅ | ⚠️ | 🟡 PS splits on `=` in some contexts |
| `>` `<` | ❌ redirect | ❌ | ❌ | ❌ redirect | ❌ redirect | **❌ Requires quoting** |
| `!` | ⚠️ history | ✅ | ✅ | ✅ | ✅ | 🟡 bash history expansion |
| `+` `-` | ✅ | ✅ | ✅ | ✅ | ✅ | **✅ Safe everywhere** |
| `@` | ✅ | ✅ | ✅ | ✅ | ⚠️ | 🟡 PS splatting |

**Conclusion:** `:` (colon) and `,` (comma) are the only universal delimiters
that work unquoted in all shells. This is why REST APIs converged on
`field:direction` and `field:op:value` — they're URL-safe AND shell-safe.

### 7.3 Recommended UFFS syntax

#### Sort: `--sort field:dir,field:dir,...`

```bash
# Single field (default direction per field type)
uffs "*" --sort size                    # → size:desc (numeric default)
uffs "*" --sort name                    # → name:asc  (string default)

# Explicit direction
uffs "*" --sort size:asc                # ascending by size
uffs "*" --sort modified:desc           # newest first

# Multi-tier (position = rank)
uffs "*" --sort ext:asc,size:desc       # by extension, then by size within each ext
uffs "*" --sort type:asc,modified:desc  # by category, then newest first

# Backward compat
uffs "*" --sort size --sort-desc        # existing flag still works
```

**Direction defaults by field type:**

| FieldType | Default | Rationale |
|-----------|---------|-----------|
| `String`, `NamedEnum` | `asc` | Alphabetical: a→z |
| `U64`, `U32`, `U16`, `U8`, `F64` | `desc` | Largest first (size, descendants) |
| `Timestamp`, `I64` | `desc` | Most recent first |
| `Bool` | `desc` | "set" before "not set" |
| `NamedBitmask` | `desc` | Most flags set first |

#### Filter: shorthand flags + `--filter field:op:value,...`

```bash
# ── Ergonomic shorthand (existing, backward-compatible) ──────────
uffs "*.txt" --newer 7d                          # modified in last 7 days
uffs "*" --min-size 1M --max-size 1G             # size range
uffs "*" --ext documents                         # collection alias
uffs "*" --attr hidden,system                    # require both flags
uffs "*" --attr hidden,!system                   # hidden AND NOT system
uffs "*.log" --exclude "backup*"                 # exclude pattern

# ── General form (power users, programmatic access) ──────────────
uffs "*" --filter "size:gt:1M,modified:newer:7d,hidden:set"
uffs "*" --filter "type:eq:document,size:between:1K:10M"
uffs "*" --filter "namespace:eq:dos"
uffs "*" --filter "forensic:has:deleted"
uffs "*" --filter "name:len-gte:50"

# ── Shorthand and general can be mixed ───────────────────────────
uffs "*.txt" --newer 7d --filter "size:gt:1M"
```

**Filter operator syntax — `field:op:value`:**

| FieldType | Operators | Example |
|-----------|-----------|---------|
| String | `eq`, `contains`, `not-contains`, `starts-with`, `ends-with`, `glob`, `regex`, `len-gte`, `len-lte` | `name:contains:backup` |
| Numeric | `eq`, `neq`, `gt`, `gte`, `lt`, `lte`, `between:lo:hi` | `size:gt:1M`, `size:between:1K:10M` |
| Timestamp | `newer`, `older`, `between:from:to` | `modified:newer:7d`, `created:between:2026-01-01:2026-03-01` |
| Bool | `set`, `not-set` | `hidden:set`, `readonly:not-set` |
| NamedEnum | `eq`, `neq` | `namespace:eq:dos`, `type:eq:document` |
| NamedBitmask | `has`, `not` | `forensic:has:deleted`, `forensic:not:extension` |

**Size value suffixes:** `K`/`KB`, `M`/`MB`, `G`/`GB`, `T`/`TB` (case-insensitive).

**Time value formats:** `7d`, `24h`, `30m`, `2w`, `today`, `yesterday`,
`ytd`, `this_week`, `last_month`, `2026-01-15`.

#### Attribute: `--attr name,name,...`

```bash
# Full names (readable, cross-platform, recommended)
uffs "*" --attr hidden                    # require hidden
uffs "*" --attr hidden,system             # require both (AND)
uffs "*" --attr hidden,!system            # hidden AND NOT system
uffs "*" --attr compressed,!offline       # compressed but not offline

# Windows DIR-style single letters (expert shorthand, optional)
uffs "*" --attr HS                        # Hidden + System (same as hidden,system)
uffs "*" --attr H-S                       # Hidden, NOT System

# Both styles parse to the same (attr_require, attr_exclude) bitmask
```

**Attribute name mapping:**

| Full name | Letter | Flag bit |
|-----------|--------|----------|
| `readonly` | `R` | `0x0001` |
| `hidden` | `H` | `0x0002` |
| `system` | `S` | `0x0004` |
| `directory` | `D` | `0x0010` |
| `archive` | `A` | `0x0020` |
| `temporary` | `T` | `0x0100` |
| `sparse` | `P` | `0x0200` |
| `reparse` | `L` | `0x0400` |
| `compressed` | `C` | `0x0800` |
| `offline` | `O` | `0x1000` |
| `not-indexed` | `I` | `0x2000` |
| `encrypted` | `E` | `0x4000` |

The `!` prefix (full names) and `-` prefix (letters) both mean "exclude".

#### Columns: `--columns name,name,...`

```bash
uffs "*" --columns name,size,modified          # 3 columns
uffs "*" --columns name,size,type,path         # 4 columns, custom order
uffs "*" --columns name,namespace,forensic     # include cold-path fields
```

Position = column display order (left to right).
Uses `FieldId` canonical names (case-insensitive).

### 7.4 Design principles summary

| Principle | Choice | Rationale |
|-----------|--------|-----------|
| **Field:value delimiter** | `:` (colon) | Safe in all shells, no escaping needed. Matches REST API conventions. |
| **List delimiter** | `,` (comma) | Universal, safe in all shells. |
| **Sort direction** | `:asc` / `:desc` suffix | Explicit, readable. Default direction per field type when omitted. |
| **Multi-tier sort** | Comma-separated, position = rank | `--sort ext:asc,size:desc` — first is primary, second is tiebreaker. |
| **Filter** | Shorthand flags + `--filter` general form | Ergonomic for common cases, powerful for complex queries. Both coexist. |
| **Attribute negation** | `!` prefix (full names) / `-` prefix (letters) | `!` avoids conflict with CLI `--` prefixes. `!system` = exclude system flag. |
| **Case** | Case-insensitive field names | `Size` = `size` = `SIZE` — reduces user friction. |
| **Backward compat** | All existing flags remain | `--newer 7d`, `--min-size 1M`, `--sort size`, `--attr hidden` unchanged. |
| **Windows compat** | DIR-style `/aRHS`, `/o-s` NOT supported | We use `--` long opts (POSIX convention). Windows users use `--attr hidden`. |

### 7.5 Why this is "Rust CLI best practice"

The Rust CLI ecosystem has converged on these conventions:

1. **Long flags with `--` prefix** (clap default, fd, ripgrep, eza, bat, delta)
2. **Short flags with `-` prefix** for frequent options (clap default)
3. **Comma-separated values** for multi-value args (clap `value_delimiter = ','`)
4. **`=` or space** between flag and value (`--sort=size` or `--sort size`, both work in clap)
5. **Enum values parsed case-insensitively** (clap `rename_all = "kebab-case"`)
6. **No POSIX-incompatible syntax** (no `/` prefix, no single-dash long opts)
7. **Smart defaults** that require zero flags for common use cases

UFFS follows all of these. The `:` delimiter for field:op:value is the
one convention we add on top, borrowed from REST API and kubectl patterns
since Rust CLI tools haven't needed structured filter syntax before
(most are simpler tools with fewer filterable fields).