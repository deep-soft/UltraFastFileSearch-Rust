# Filter & Sort Implementation Gaps

> **Purpose:** Comprehensive gap analysis comparing the target feature matrix
> against the actual codebase.  Each gap has enough detail for a junior
> developer to implement it.  The tracking table at the end can be used as a
> work log.
>
> **Date:** 2026-04-04
>
> **Reference:** The target feature matrix is in
> `docs/architecture/FILTER_SORT_FEATURE_MATRIX.md`.

---

## Legend

| Symbol | Meaning |
|--------|---------|
| ✅ | Fully implemented end-to-end (CLI → daemon → engine) |
| ⚠️ | Partially implemented (engine yes, but daemon/CLI missing) |
| 🔴 | Not implemented at all |

---

## 1  String Column Filters (Path, Name, PathOnly, Extension)

The feature matrix specifies **LENGTH** and **Begins With / Ends With /
Contains / Does not contain** filter options for every string column.

### 1.1  Begins With / Ends With / Contains / Does not contain

**Status: ✅ Implemented — via canonical predicates + pattern syntax**

The canonical predicate system in the daemon (`SearchPredicateOp`) supports
`Match` / `NotMatch` (glob wildcard), `Eq`, `Ne`, `In`, `NotIn`,
`HasAll`, `HasAny`, `HasNone`, `Contains`, `StartsWith`, `EndsWith`.
A TUI or API caller can express these via JSON-RPC predicates.

| Gap ID | Item | Status |
|--------|------|--------|
| S1.1 | `--begins-with PREFIX` | ✅ Client-side sugar → `PREFIX*` |
| S1.2 | `--ends-with SUFFIX` | ✅ Client-side sugar → `*SUFFIX` |
| S1.3 | `--contains NEEDLE` | ✅ Client-side sugar → `*NEEDLE*` |
| S1.4 | `--not-contains NEEDLE` | ✅ Client-side sugar → `--exclude '*NEEDLE*'` |
| S1.5 | `HasAll` / `HasAny` / `HasNone` ops in `match_string` | ✅ Fixed |

### 1.2  LENGTH Filter

**Status: ✅ Implemented**

Filters by filename and full-path character length are available.

| Gap ID | Item | Detail |
|--------|------|--------|
| S2.1 | `--min-name-length <N>` | Filter files by minimum filename length |
| S2.2 | `--max-name-length <N>` | Filter files by maximum filename length |
| S2.3 | `--min-path-length <N>` | Filter by minimum full-path length |
| S2.4 | `--max-path-length <N>` | Filter by maximum full-path length (useful for MAX_PATH detection) |

**Implementation guidance:**

- Add fields to `SearchFilters`: `min_name_len: Option<u16>`,
  `max_name_len: Option<u16>`, `min_path_len: Option<u16>`,
  `max_path_len: Option<u16>`.
- In `matches_record()`: check `rec.name_len` against bounds.
- In `apply_search_filters()`: check `row.name().len()` and
  `row.path.len()`.
- Add CLI args: `--min-name-length`, `--max-name-length`,
  `--min-path-length`, `--max-path-length`.
- Add daemon `SearchParams` fields and wire through
  `SearchFilters::from_params()`.
- **Key use case:** Find files near Windows MAX_PATH (260) limit:
  `uffs '*' --min-path-length 250`.

**Files to change:**
1. `crates/uffs-core/src/search/filters.rs` — add fields + checks
2. `crates/uffs-cli/src/args.rs` — add CLI flags
3. `crates/uffs-client/src/protocol.rs` — add SearchParams fields
4. `crates/uffs-daemon/src/index.rs` — wire into `from_params()` and
   `compile_predicates_into_filters()`
5. `crates/uffs-tui/src/filters.rs` — wire into `build_search_filters()`
6. `crates/uffs-core/src/search/filters_tests.rs` — add tests

---

## 2  Size Filters

### 2.1  Size — Less Than / Equal / Bigger Than

**Status: ✅ Complete**

| Gap ID | Item | Status | Detail |
|--------|------|--------|--------|
| SZ1 | Less Than (`--max-size`) | ✅ | Implemented |
| SZ2 | Bigger Than (`--min-size`) | ✅ | Implemented |
| SZ3 | Equal (`--exact-size`) | ✅ | Client-side sugar — sets both `min_size` and `max_size` |
| SZ4 | Human-readable suffixes (KB/MB/GB) | ✅ | `parse_size()` handles KB/MB/GB/TB |

### 2.2  Size on Disk Filters

**Status: ✅ Complete**

| Gap ID | Item | Status |
|--------|------|--------|
| SZ5 | `--min-size-on-disk <SIZE>` | ✅ Implemented |
| SZ6 | `--max-size-on-disk <SIZE>` | ✅ Implemented |
| SZ7 | `--exact-size-on-disk <SIZE>` | ✅ Implemented (client-side sugar) |

---

## 3  Date/Time Filters

### 3.1  Duration Suffixes

**Status: ✅ Implemented**

`s` (seconds), `m` (minutes), `h` (hours), `d` (days), `w` (weeks) all
work in `parse_time_bound()`.

### 3.2  ISO Date

**Status: ✅ Implemented**

`YYYY-MM-DD` format works.

### 3.3  Named Ranges

**Status: ✅ Implemented**

All named time specs are implemented in `parse_named_time_range()` within
`parse_time_bound()`.

| Gap ID | Named Range | Resolves To | Detail |
|--------|-------------|-------------|--------|
| T1 | `today` | Midnight of current day → now | `--newer today` |
| T2 | `yesterday` | Yesterday 00:00 → today 00:00 | Sets both newer + older |
| T3 | `this_week` | Monday 00:00 of current week → now | |
| T4 | `last_week` | Previous Monday 00:00 → this Monday 00:00 | |
| T5 | `this_month` | 1st of current month 00:00 → now | |
| T6 | `last_month` | 1st of previous month → 1st of current month | |
| T7 | `this_year` | Jan 1 of current year → now | |
| T8 | `last_year` | Jan 1 of previous year → Jan 1 of current year | |
| T9 | `ytd` | Alias for `this_year` | Year to date |
| T10 | `last_7d` | Alias for `7d` | Convenience |
| T11 | `last_30d` | Alias for `30d` | Convenience |
| T12 | `last_90d` | Alias for `90d` | Convenience |
| T13 | `last_365d` | Alias for `365d` | Convenience |

**Implementation guidance:**

- All changes go in **one function**: `parse_time_bound()` in
  `crates/uffs-core/src/search/filters.rs`.
- Add a `match` block **before** the duration-suffix parsing that checks
  for known named strings (lowercased).
- Ranges like `yesterday`, `last_week`, `last_month`, `last_year` need
  **two** bounds (start and end).  The current `parse_time_bound()`
  returns a single `Option<i64>`.  Options:
  - **Option A (simple):** Return the *start* of the range when called
    with `is_newer=true`, and the *end* when called with `is_newer=false`.
    Users would write `--newer yesterday --older yesterday` and each call
    sees a different side.  This is elegant but non-obvious.
  - **Option B (better):** Change the return type to
    `Option<(i64, Option<i64>)>` where the second value is an optional
    upper bound.  The caller checks if a second bound was returned and
    sets the corresponding older/newer field automatically.
  - **Recommendation:** Option B — it makes `--newer yesterday` do the
    right thing without requiring the user to also pass `--older`.
- For date arithmetic, use the existing `month_days()` helper. You'll
  need to extract year/month/day from `now_us`.  Consider adding a
  `unix_micros_to_ymd(us: i64) -> (i64, u32, u32)` helper (reverse of
  the existing ISO-date → µs conversion).

**Files to change:**
1. `crates/uffs-core/src/search/filters.rs` — extend `parse_time_bound()`
2. `crates/uffs-core/src/search/filters_tests.rs` — tests for every range
3. No CLI/daemon changes needed — they already pass time specs through.

### 3.4  Month-of-Year Filter ("Every January")

**Status: ✅ Implemented**

The feature matrix says: *"Every Month (e.g. January)"* — meaning "show
files created/modified in **any** January, across **all** years."  This is
fundamentally different from a time range — it's a **month-of-year
extraction** filter.

| Gap ID | Item | Detail |
|--------|------|--------|
| T14 | `january` / `jan` | Files with timestamp in any January |
| T15 | `february` / `feb` | (same pattern for all 12 months) |
| T16 | `march` / `mar` | |
| T17 | `april` / `apr` | |
| T18 | `may` | |
| T19 | `june` / `jun` | |
| T20 | `july` / `jul` | |
| T21 | `august` / `aug` | |
| T22 | `september` / `sep` | |
| T23 | `october` / `oct` | |
| T24 | `november` / `nov` | |
| T25 | `december` / `dec` | |

**Implementation guidance:**

- This **cannot** be implemented as a simple time range.  It requires
  extracting the month component from the Unix µs timestamp and comparing.
- Add a new field to `SearchFilters`:
  `month_of_year: Option<u32>` (1-12).
- In the hot-path `matches_record()` and `apply_search_filters()`:
  extract month from `rec.modified` (or `row.modified`) using a fast
  integer-only decomposition (similar to `format.rs`'s civil time code).
- A helper `fn month_from_unix_micros(us: i64) -> u32` should be added
  to `filters.rs`.
- Wire the keyword parsing into `parse_time_bound()` — when the spec
  is `"january"`, return a sentinel or a new variant instead of µs.
  Alternatively, add a separate `--month` flag.
- **Performance note:** This runs per-record in the hot path.  The month
  extraction is ~10 integer ops — negligible compared to string matching.

**Files to change:**
1. `crates/uffs-core/src/search/filters.rs` — add field + month extraction
2. `crates/uffs-cli/src/args.rs` — add `--month` flag (or parse in time spec)
3. `crates/uffs-client/src/protocol.rs` — add SearchParams field
4. `crates/uffs-daemon/src/index.rs` — wire through
5. `crates/uffs-core/src/search/filters_tests.rs` — tests

### 3.5  Quarter Filter (Q1, Q2, Q3, Q4)

**Status: ✅ Implemented**

| Gap ID | Item | Detail |
|--------|------|--------|
| T26 | `Q1` | January–March (months 1-3) of any year |
| T27 | `Q2` | April–June (months 4-6) |
| T28 | `Q3` | July–September (months 7-9) |
| T29 | `Q4` | October–December (months 10-12) |

**Implementation guidance:**
- Same approach as §3.4 but with a range of months.
- Add `quarter: Option<u8>` (1-4) to `SearchFilters`, or reuse
  `month_of_year` with a set/range.
- Or: `time_month_filter: Option<Vec<u32>>` — a set of allowed months.
  Q1 = `[1,2,3]`, January = `[1]`.  This unifies §3.4 and §3.5.

### 3.6  This/Last/Next Periods

**Status: ✅ Implemented**

The matrix specifies: *(next / This / Last) → Day / Week / Month / Year*

| Gap ID | Item | Detail |
|--------|------|--------|
| T30 | `next_day` / `next_week` / `next_month` / `next_year` | Rarely useful for file search (files in the future?). **Consider skipping.** |
| T31 | `this_day` | Alias for `today` (T1) |
| T32 | `this_week` | Same as T3 |
| T33 | `this_month` | Same as T5 |
| T34 | `this_year` | Same as T7 |
| T35 | `last_day` | Alias for `yesterday` (T2) |
| T36 | `last_week` | Same as T4 |
| T37 | `last_month` | Same as T6 |
| T38 | `last_year` | Same as T8 |

Most of these are aliases for the named ranges in §3.3.
`next_*` would find files with future timestamps (clock skew, timezone
issues).  **Recommendation:** implement `next_*` for completeness but
document them as edge-case tools.

### 3.7  "Between" Syntax

**Status: ✅ Implemented**

Users can combine `--newer` and `--older` to create a between range:
`uffs '*' --newer 2026-01-01 --older 2026-03-31`.  This works today.

| Gap ID | Item | Detail |
|--------|------|--------|
| T39 | Single `--between` flag | `--between 2026-01-01,2026-03-31` |

**Implementation guidance:**
- Add `--between <START>,<END>` to CLI that splits on comma and sets
  both `newer` and `older`.
- Low priority — the two-flag approach already works.

---

## 4  Descendant Filters

**Status: ✅ Implemented**

`--min-descendants` and `--max-descendants` work end-to-end.

| Gap ID | Item | Status |
|--------|------|--------|
| D1 | Less Than | ✅ `--max-descendants` |
| D2 | Bigger Than | ✅ `--min-descendants` |
| D3 | Equal | ✅ `--exact-descendants` sets min=max |

**Implementation guidance for D3:**
- Same as SZ3 (exact size): set both min and max to the same value.
- Or add `--exact-descendants <N>` that does this automatically.

---

## 5  NTFS Attribute Filters

**Status: ✅ Implemented**

All 18+ attribute flags are implemented in both the CLI (`--attr`) and
the daemon canonical predicate system.  Set/not-set filtering works via
`!` prefix in the attr spec and `Eq(true)`/`Eq(false)` in predicates.

| Item | Status |
|------|--------|
| Read-only, Archive, System, Hidden | ✅ |
| Offline, NotIndexed, NoScrub, Integrity | ✅ |
| Pinned, Unpinned, DirectoryFlag | ✅ |
| Compressed, Encrypted, Sparse, Reparse | ✅ |
| Temporary, Virtual | ✅ |
| RecallOnOpen, RecallOnDataAccess | ✅ |
| Common attribute combos (preset) | ✅ `system-files`, `user-files` — see §5.1 |

### 5.1  Attribute Presets

**Status: ✅ Partially implemented (system-files, user-files)**

The matrix mentions *"Common combinations of attributes e.g. system files"*
as a preset concept in the Attributes row.

| Gap ID | Item | Detail |
|--------|------|--------|
| A1 | `--attr system-files` | Preset: `hidden,system` |
| A2 | `--attr user-files` | Preset: `!hidden,!system` + `--hide-system` |
| A3 | `--attr compressed-encrypted` | Preset: `compressed` OR `encrypted` |

**Implementation guidance:**
- Add preset expansion in `parse_attr_require()` / `parse_attr_exclude()`
  before the per-token loop.
- Low priority — users can already compose `--attr hidden,system`.

---

## 6  Extension Presets

**Status: ✅ Implemented**

Collection aliases (`pictures`, `documents`, `videos`, `music`,
`archives`, `code`) are implemented and expand to extension lists.

### 6.1  Executables Collection

| Gap ID | Item | Status |
|--------|------|--------|
| E1 | `executables` / `exec` | ✅ Implemented (`exe,msi,bat,cmd,ps1,com,scr,vbs,wsf,dll,sys`) |

Also added centralized `expand_collection()` used by `SearchFilters::from_params`,
`ExtensionFilter::parse()`, and TUI's `build_search_filters`.

---

## 7  Sort Gaps

### 7.1  Implemented Sort Columns

All sort columns from the feature matrix are implemented:

| Column | Status |
|--------|--------|
| Name, Path, PathOnly | ✅ |
| Size, SizeOnDisk | ✅ |
| Created, Modified, Accessed | ✅ |
| Extension, Type | ✅ |
| Descendants | ✅ |
| Drive | ✅ |
| TreeAllocated, Bulkiness | ✅ (bonus) |

### 7.2  Sort by Length

**Status: ✅ Implemented**

| Gap ID | Item | Status |
|--------|------|--------|
| SO1 | Sort by name length | ✅ `--sort name_length` |
| SO2 | Sort by path length | ✅ `--sort path_length` |

Added `FieldId::NameLength` / `FieldId::PathLength` with full sorting,
TUI column rendering, daemon JSON output, and CLI output support.

---

## 8  Daemon Wire Protocol Gaps

The daemon `SearchParams` has legacy individual fields AND the canonical
`predicates: Vec<SearchPredicate>` system.  The canonical system is more
powerful, but some gaps remain.

| Gap ID | Item | Status |
|--------|------|--------|
| W1 | `HasAll`/`HasAny`/`HasNone` in `match_string` | ✅ Fixed |
| W2 | `Contains` / `StartsWith` / `EndsWith` predicate ops | ✅ Implemented |
| W3 | `Length` predicate op | ✅ Implemented — `NameLength`/`PathLength` compiled into hot-path filters |

---

## 9  Implementation Priority

### Wave 1 — High Impact, Low Effort — ✅ ALL DONE

| Gap ID | Item | Status |
|--------|------|--------|
| W1 | Fix `HasAll`/`HasAny`/`HasNone` in `match_string` | ✅ |
| T1-T13 | Named time ranges + duration aliases | ✅ (already existed) |
| E1 | `executables` extension collection | ✅ |

### Wave 2 — Medium Impact, Medium Effort — ✅ ALL DONE

| Gap ID | Item | Status |
|--------|------|--------|
| S2.1-S2.4 | Name/path length filters | ✅ |
| SZ5-SZ6 | Size-on-disk filters | ✅ |
| T14-T29 | Month-of-year + quarter filter | ✅ |
| D3 | Exact descendant count | ✅ |

### Wave 3 — Low Impact, Low–Medium Effort — ✅ ALL DONE

| Gap ID | Item | Status |
|--------|------|--------|
| SZ3 | Exact size match | ✅ |
| T30 | `next_*` periods | ✅ |
| T39 | `--between` single flag | ✅ |
| A1-A2 | Attribute presets (system-files, user-files) | ✅ |
| SO1-SO2 | Sort by length | ✅ |
| W2 | Contains/StartsWith/EndsWith predicate ops | ✅ |

### Wave 4 — ✅ DONE

| Gap ID | Item | Status |
|--------|------|--------|
| S1.1-S1.4 | `--begins-with` / `--ends-with` / `--contains` / `--not-contains` CLI flags | ✅ Client-side sugar — translates to glob patterns |

---

## 10  Tracking

| Gap ID | Description | Status | Notes |
|--------|-------------|--------|-------|
| W1 | Fix HasAll/HasAny/HasNone in match_string | ✅ DONE | Case-insensitive substring containment in `index.rs` |
| T1 | Named range: `today` | ✅ DONE | Already in `parse_named_time_range()` |
| T2 | Named range: `yesterday` | ✅ DONE | Already in `parse_named_time_range()` |
| T3 | Named range: `this_week` | ✅ DONE | Already in `parse_named_time_range()` |
| T4 | Named range: `last_week` | ✅ DONE | Already in `parse_named_time_range()` |
| T5 | Named range: `this_month` | ✅ DONE | Already in `parse_named_time_range()` |
| T6 | Named range: `last_month` | ✅ DONE | Already in `parse_named_time_range()` |
| T7 | Named range: `this_year` | ✅ DONE | Already in `parse_named_time_range()` |
| T8 | Named range: `last_year` | ✅ DONE | Already in `parse_named_time_range()` |
| T9 | Named range: `ytd` | ✅ DONE | Already in `parse_named_time_range()` |
| T10-T13 | Duration aliases (last_7d, etc.) | ✅ DONE | Already in `parse_named_time_range()` |
| T14-T25 | Month-of-year filter | ✅ DONE | `--month jan`, `parse_month_spec()`, `month_from_unix_micros()` |
| T26-T29 | Quarter filter (Q1-Q4) | ✅ DONE | `--month Q1` expands to months 1-3 via `parse_month_spec()` |
| T30 | `next_*` periods | ✅ DONE | `next_day`/`tomorrow`, `next_week`, `next_month`, `next_year` |
| T39 | `--between` single flag | ✅ DONE | Splits on comma → sets `--newer` + `--older` |
| S2.1-S2.4 | Name/path length filters | ✅ DONE | `--min-name-length`, `--max-name-length`, `--min-path-length`, `--max-path-length` |
| SZ3 | Exact size match | ✅ DONE | `--exact-size` sets both min and max |
| SZ5-SZ6 | Size-on-disk filters | ✅ DONE | `--min-size-on-disk`, `--max-size-on-disk` |
| SZ7 | Exact size-on-disk | ✅ DONE | `--exact-size-on-disk` sets min=max on client side |
| D3 | Exact descendant count | ✅ DONE | `--exact-descendants` sets both min and max |
| E1 | `executables` extension collection | ✅ DONE | `exe,msi,bat,cmd,ps1,com,scr,vbs,wsf,dll,sys` + centralized `expand_collection()` |
| A1-A2 | Attribute presets (system-files, user-files) | ✅ DONE | `expand_attr_preset()` in `parse_attr_require`/`parse_attr_exclude` |
| A3 | Attribute preset (compressed-encrypted) | ✅ DONE | `--attr compressed-encrypted` → requires both bits |
| SO1-SO2 | Sort by name/path length | ✅ DONE | `FieldId::NameLength`, `FieldId::PathLength` + sorting + TUI |
| W2 | Contains/StartsWith/EndsWith predicate ops | ✅ DONE | New `SearchPredicateOp` variants + daemon matching |
| W3 | Length predicate op | ✅ DONE | `NameLength`/`PathLength` predicates compiled into hot-path min/max filters |