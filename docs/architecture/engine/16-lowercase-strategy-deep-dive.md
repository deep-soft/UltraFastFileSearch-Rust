# 16 — The Lowercase Problem: A Systemic Analysis & Optimal Resolution Strategy

> **Date:** 2026-04-02
> **Scope:** Deep investigation of the recurring case-folding overhead identified
> across docs 13, 14, and 15. Maps every lowercase operation in the codebase,
> classifies by lifecycle stage and impact, evaluates six architectural
> alternatives, and proposes a single coherent "never-allocate" strategy.
>
> **Core finding:** ASCII case-folding is a **per-byte** operation, not a
> **per-string** operation. You never need to materialise the entire lowered
> string. Every consumer can operate on original-case bytes and fold at the
> point of comparison. This eliminates 140 MB temporary clones, millions of
> per-record heap allocations, and unnecessary sort key Strings — with zero
> additional permanent memory.

---

## Table of Contents

1. [The Problem Pattern](#1-the-problem-pattern)
2. [Complete Inventory: 89 Lowercase Sites](#2-complete-inventory)
3. [Data Flow: The Names Blob Lifecycle](#3-data-flow)
4. [Six Architectural Alternatives](#4-six-architectural-alternatives)
5. [The Optimal Strategy: Never-Allocate Case Folding](#5-the-optimal-strategy)
6. [Implementation Blueprint](#6-implementation-blueprint)
7. [Migration Path](#7-migration-path)
8. [Validation](#8-validation)

---

## 1. The Problem Pattern

Across three independent performance analyses (docs 13–15), the same issue
surfaces repeatedly under different names:

| Doc | Finding | Impact |
|-----|---------|--------|
| 13 | "Bulk `names_lower` clone in trigram build" | 140 MB temp allocation |
| 14 | B5: "per-record `to_ascii_lowercase` in match loop" | Millions of heap allocs |
| 14 | B8: "140 MB names clone on every cache load" | Cache load +25% slower |
| 15 | P2: "heap alloc per record in glob/whole-word path" | 50K–7M allocs/search |
| 15 | P6: "names.clone() for trigram build AND cache load" | 140 MB × 3 call sites |
| 15 | P7: "2 Strings per comparison in sort_indices_by_name" | O(N log N) allocs |
| 15 | P4: "3 String keys even for numeric sorts" | 30K allocs/sort |
| 15 | P14: "tree search lowercases every child" | Thousands of allocs |

**This is not eight separate issues. It is one systemic issue manifesting in
eight places.** The root cause: the codebase treats lowercase as a string-level
transformation (allocate a new String, fill it with lowered bytes) when it is
fundamentally a byte-level operation (compare with folding at each byte).

### Why This Matters

On a 7 M record NTFS volume with a typical search returning 10 K results:

```
Current state:
  Build/cache load:  140 MB clone × 3 sites = 420 MB transient allocs
  Per-search:        50K–7M heap allocs (depending on query type)
  Per-sort:          30K–280K heap allocs (depending on column + result size)

Optimal state:
  Build/cache load:  0 bytes transient allocs
  Per-search:        0 heap allocs
  Per-sort:          0 heap allocs
```



---

## 2. Complete Inventory: 89 Lowercase Sites

A `rg` search across all Rust sources found **89** sites calling
`to_ascii_lowercase`, `make_ascii_lowercase`, or `to_lowercase`. These fall
into four categories by lifecycle stage and performance impact:

### Category A: One-Time / Setup (~40 sites) — ✅ No action needed

These execute once per query, once per config parse, or once per UI render.

| Location | Purpose | Freq |
|----------|---------|------|
| `compiled_pattern/mod.rs:223,227` | Pattern compilation | 1×/query |
| `index_search/pattern.rs:308-342` | `compile_index_pattern` variants | 1×/query |
| `index_search/routing.rs:35` | Route classification | 1×/query |
| `index_search/query/planning.rs:78` | Extension extraction | 1×/query |
| `extensions/mod.rs:98,136,201,307,315,345` | Extension registry lookup | 1×/load |
| `output/column.rs:162`, `output/config.rs:74` | Output config parse | 1×/config |
| `search/columns.rs:99`, `tree/column.rs:32` | Column name parse | 1×/config |
| `search/backend.rs:309,597,827,833` | Pattern/column label | 1×/query |
| `search/filters.rs:104,112,431,444` | Filter construction | 1×/query |
| `query/matching.rs:118,121,172` | Extension matching setup | 1×/query |
| `uffs-cli/commands/output/mod.rs:44` | CLI output mode parse | 1×/startup |
| `uffs-tui/src/app.rs:387`, `filters.rs:63,71` | TUI label/filter | 1×/render |
| `uffs-tui/src/keys.rs:490`, `ui.rs:210,466` | TUI input parsing | 1×/keystroke |
| `uffs-daemon/src/index.rs:510,522,672,775` | Daemon path comparison | 1×/event |
| `uffs-mft/src/commands/load.rs:685` | Load command parse | 1×/invocation |
| `uffs-mft/src/reader/read_mode.rs:83` | Read mode parse | 1×/invocation |
| `uffs-mft/src/index/extensions.rs:45` | Extension normalize | 1×/build |
| `uffs-diag/src/bin/*.rs` | Diagnostic tools | Not production |

### Category B: Bulk Blob Lowering (3 sites) — 🔴 CRITICAL

These clone the **entire 140 MB names blob** and lowercase it in bulk, solely
to feed `TrigramIndex::build()`. Happens on every fresh build, cache load,
and USN journal refresh.

| # | Location | Trigger | Frequency |
|---|----------|---------|-----------|
| B1 | `compact.rs:433-434` | Fresh compact build | 1× per drive init |
| B2 | `compact_cache.rs:159-160` | Cache load (v4+/v5) | 1× per drive startup |
| B3 | `compact_loader.rs:444-445` | USN journal refresh | Per drive refresh |

**Total:** 140 MB × 3 sites × 2 drives = up to **840 MB** of transient allocs.

### Category C: Per-Record Heap Alloc (~15 sites) — 🔴 HOT PATH

These call `to_ascii_lowercase()` on individual filenames inside tight loops.

| # | Location | Context | Records Touched |
|---|----------|---------|-----------------|
| C1 | `query.rs:422` | Whole-word match (CI) | 50K–7M |
| C2 | `query.rs:437` | Glob/general match (CI) | 50K–7M |
| C3 | `query.rs:131` | `sort_indices_by_name` LHS | N × log₂N |
| C4 | `query.rs:132` | `sort_indices_by_name` RHS | N × log₂N |
| C5 | `query.rs:189` | Top-N name sort key (byte loop) | 7M (per-byte ✅) |
| C6 | `query.rs:198` | Top-N drive sort key (byte loop) | 7M (per-byte ✅) |
| C7 | `tree.rs:191` | Single-segment tree search | Trigram candidates |
| C8 | `tree.rs:218` | First segment dir match | Trigram candidates |
| C9 | `tree.rs:236` | Intermediate dir children | Children of candidates |
| C10 | `tree.rs:264` | Leaf children match | Children of candidate dirs |
| C11 | `tree.rs:315` | `collect_all_descendants` | All descendants |
| C12 | `backend.rs:512` | `sort_rows` name key | 10K |
| C13 | `backend.rs:513` | `sort_rows` path key | 10K |
| C14 | `backend.rs:519` | `sort_rows` extension key | 10K |
| C15 | `filters.rs:323` | Filter `matches_row` | 10K |

**Note:** C5/C6 are byte-level ops in a `[0u8; 8]` — already zero-alloc.

### Category D: Zero-Alloc Byte-Level (~15 sites) — ✅ Already optimal

These use the correct pattern: byte-level `u8::to_ascii_lowercase()` or
reusable buffer with `make_ascii_lowercase()`.

| Location | Pattern |
|----------|---------|
| `index_search/pattern.rs:237,254,283,288` | `hay.to_ascii_lowercase() == *ndl` |
| `index_search/pattern.rs:229-296` | `starts/ends/contains_ignore_ascii_case` |
| `query.rs:430-433` | `buf.make_ascii_lowercase()` (reusable) |
| `filters.rs:210-212` | `lower_buf.make_ascii_lowercase()` (reusable) |
| `child_order.rs:231-232`, `types.rs:621-622` | `.map(\|b\| b.to_ascii_lowercase())` |

**These are the correct patterns.** The goal: apply them to all Category C
sites and eliminate all Category B sites.

---

## 3. Data Flow: The Names Blob Lifecycle

Understanding where names are created, stored, and consumed is essential
to designing the right strategy.

```
MFT Parser                        Compact Build               Search
─────────                         ─────────────               ──────

parse_record_to_index()           build_compact_index()        search_compact_drive()
  │                                 │                            │
  ▼                                 ▼                            ▼
MftIndex::names                   names = index.names           rec.name(&drive.names)
  NameArena (contiguous            .as_bytes().to_vec()            │
   UTF-8 blob, original case)        │                            ▼ (CURRENTLY)
  140 MB for 7M records              ├─▶ CompactRecord::name_offset  name.to_ascii_lowercase()
                                     │     (u32 into names blob)     ← HEAP ALLOC per record
                                     │
                                     ├─▶ names.clone()              TrigramIndex::search()
                                     │   make_ascii_lowercase()       │
                                     │   TrigramIndex::build()        ▼
                                     │   drop(names_lower)          Query trigrams are
                                     │   ← 140 MB CLONE             pre-lowered by caller
                                     │                              (1× per query, cheap)
                                     ▼
                                  DriveCompactIndex {
                                    names: Vec<u8>,       // 140 MB, ORIGINAL case
                                    trigram: TrigramIndex, // built from lowered copy
                                    ...
                                  }
```

### Key Insight: The Names Blob Is Read-Only After Construction

Once `DriveCompactIndex` is built, `names` is **never mutated**. It is
accessed only via `CompactRecord::name(&drive.names)` which returns a `&str`
slice. Every consumer that needs lowercase is independently re-deriving it
from this read-only source.

This is the fundamental waste: **the same transformation is being applied
to the same data, independently, thousands to millions of times.**

### Why Not Just Store `names_lower` Permanently?

The v5 cache format was specifically designed to NOT store `names_lower` on
disk (doc comment in `compact_cache.rs`): it saves ~140 MB uncompressed.
And keeping `names_lower` in memory doubles the permanent names footprint
from 140 MB to 280 MB per drive.

**But what if we don't need either approach?**

---

## 4. Six Architectural Alternatives

### Option 1: Dual Names Blob (Store Both Original + Lowered)

```rust
pub struct DriveCompactIndex {
    names: Vec<u8>,        // 140 MB — original case (for display)
    names_lower: Vec<u8>,  // 140 MB — lowered (for search/trigram)
}
```

| Metric | Value |
|--------|-------|
| Permanent memory | +140 MB/drive (280 MB total for 2 drives) |
| Build transient | 0 (build names_lower once, keep it) |
| Search allocs | 0 (index into names_lower) |
| Disk cache | +140 MB uncompressed (reverses v5 optimisation) |

**Verdict:** Simple but wasteful. Doubles permanent memory. This is what
the codebase deliberately moved AWAY from in v5.

### Option 2: Inline Lowering at Every Callsite (Fix Each Site)

Keep single `names` blob. Fix each of the 15+ Category C sites individually:
use reusable buffers, byte-level comparison, or `eq_ignore_ascii_case()`.

| Metric | Value |
|--------|-------|
| Permanent memory | 0 extra |
| Build transient | 0 (fix trigram to lowercase inline) |
| Search allocs | 0 (buffer reuse + byte-level compare) |
| Complexity | 15+ individual fixes, no architectural change |

**Verdict:** Zero memory overhead, but scattered implementation. Risk of
regression if new callsites are added without following the pattern.

### Option 3: Pre-Lowered Names as Primary Storage

Store `names_lower` as the primary blob. Store original-case names
separately (only needed for display).

| Metric | Value |
|--------|-------|
| Permanent memory | Same as Option 1 (need both for display) |
| API confusion | High — `rec.name()` returns lowercase |

**Verdict:** Same memory as dual blob, worse API. Rejected.

### Option 4: Case-Bit Compression

Store `names_lower` (140 MB) + 1 bit per byte indicating uppercase (17.5 MB).
Reconstruct original case on demand.

| Metric | Value |
|--------|-------|
| Permanent memory | 157.5 MB vs 280 MB (44% less than dual blob) |
| Complexity | High — bitfield management, non-ASCII fragility |

**Verdict:** Clever but fragile. Not worth the complexity for 17.5 MB
savings over dual blob when Option 2 uses 0 extra bytes.

### Option 5: `names_lower` as Shared `Arc<[u8]>`

Build `names_lower` once during compact construction, wrap in `Arc<[u8]>`,
share with trigram builder and store permanently.

| Metric | Value |
|--------|-------|
| Permanent memory | +140 MB/drive (same as dual blob) |
| Build transient | 0 (no clone — share via Arc) |

**Verdict:** Same memory cost as dual blob. Arc indirection adds nothing
vs just storing the Vec.

### Option 6: Never-Allocate Case Folding (Recommended) ⭐

**The fundamental insight:** ASCII case-folding is a per-byte operation:
`b'A'.to_ascii_lowercase() == b'a'`. It is a single conditional
subtraction. You never need to materialise the entire lowered string.

Every consumer can fold at the point of comparison:

```
Comparison:  eq_ignore_ascii_case(a, b)         — 0 allocs, 0 buffers
Ordering:    cmp_ignore_ascii_case(a, b)         — 0 allocs, 0 buffers
Substring:   memchr on buf.make_ascii_lowercase  — 0 allocs, reused buf
Trigram:     window[i].to_ascii_lowercase()       — 0 allocs, inline
```

| Metric | Value |
|--------|-------|
| Permanent memory | **0 extra** |
| Build transient | **0** (trigram lowercases 3 bytes per window inline) |
| Search allocs | **0** (byte-level compare or reusable buffer) |
| Sort allocs | **0** (lazy byte-level `cmp`) |
| Complexity | Medium — ~15 callsite fixes + trigram build change |

**This is strictly dominant:** same permanent memory as current, zero
transient overhead, zero per-record allocations.

### Comparison Matrix

```
                    Permanent    Build         Search       Sort
                    Memory       Transient     Allocs/query Allocs/sort
                    (per drive)  (per build)
─────────────────── ──────────── ──────────── ──────────── ────────────
Current             140 MB       +140 MB      50K–7M       30K–280K
Option 1: Dual      280 MB       0            0            0
Option 2: Fix sites 140 MB       0            0            0
Option 3: Flip      280 MB       0            0            0
Option 4: Bits      157.5 MB     0            0            0
Option 5: Arc       280 MB       0            0            0
Option 6: Never ⭐  140 MB       0            0            0
                    ─────────    ────         ─            ─
```

**Option 6 (Never-Allocate) is the only approach that achieves zero overhead
across ALL dimensions while using zero additional memory.**

---

## 5. The Optimal Strategy: Never-Allocate Case Folding

### The Principle

> **Every consumer of a filename operates on the original-case bytes and folds
> case at the point of comparison — never materialising a separate lowered
> copy.**

This is not a novel technique. It is how `strcmp_ci` works in C, how ICU's
`u_strCaseCompare` works, and how the Rust stdlib's
`str::eq_ignore_ascii_case` works. The UFFS codebase already uses this
pattern correctly in `index_search/pattern.rs` (Category D). The strategy
simply extends it to all remaining sites.

### Three Implementation Tiers

Different consumers need different levels of folding support:

#### Tier 1: Comparison-Point Folding (Zero Alloc, Zero Buffer)

For equality checks, ordering, prefix/suffix matching — fold each byte
lazily during the comparison. Short-circuits on first difference.

```rust
// Already exists in index_search/pattern.rs — extract to shared location:

/// Case-insensitive byte-level comparison. O(min(a,b)) with lazy folding.
#[inline]
fn cmp_ignore_ascii_case(a: &[u8], b: &[u8]) -> core::cmp::Ordering {
    a.iter()
        .map(u8::to_ascii_lowercase)
        .cmp(b.iter().map(u8::to_ascii_lowercase))
}

/// Case-insensitive equality.
#[inline]
fn eq_ignore_ascii_case(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)  // stdlib already has this!
}
```

**Applicable to:** C3, C4 (sort comparators), C12 (sort name tiebreaker).

**CPU cost:** One `to_ascii_lowercase()` per byte = one conditional
subtraction per byte. On modern x86, this is a single `cmp + cmov` pair,
pipelined at ~0.5 cycles/byte. For a 20-byte filename, that's ~10 cycles
— less than the cost of a SINGLE heap allocation (~100 cycles).

#### Tier 2: Buffer-Reuse Folding (Zero Alloc, Reused Buffer)

For substring search via `memchr::memmem::Finder` (needs contiguous
lowered bytes to feed the SIMD searcher):

```rust
// Already exists in query.rs:430-433 — extend to ALL match paths:

fn lowercase_into<'a>(name: &str, buf: &'a mut Vec<u8>) -> &'a [u8] {
    buf.clear();
    buf.extend_from_slice(name.as_bytes());
    buf.make_ascii_lowercase();
    buf.as_slice()
}
```

The `Vec<u8>` buffer is allocated ONCE at the start of the search function
and reused for every record. `clear()` resets length without freeing —
the underlying allocation persists and grows to max-name-length (~255 bytes),
then stays there.

**Applicable to:** C1, C2 (glob/whole-word match), C7–C11 (tree search),
C15 (filter matching).

**Memory cost:** One `Vec<u8>` with capacity ~256 bytes. Never grows
beyond NTFS max filename length (255 bytes).

#### Tier 3: Window-Level Folding (Zero Alloc, Inline)

For trigram index building — fold 3 bytes per sliding window:

```rust
// In TrigramIndex::build(), change inner loop:
for window in bytes.windows(3) {
    let tri: [u8; 3] = [
        window[0].to_ascii_lowercase(),  // one cmp+cmov
        window[1].to_ascii_lowercase(),  // one cmp+cmov
        window[2].to_ascii_lowercase(),  // one cmp+cmov
    ];
    let packed = pack_trigram(tri);
    // ... rest unchanged
}
```

**Applicable to:** B1, B2, B3 (the 140 MB clone sites).

**CPU cost:** 3 extra `to_ascii_lowercase()` calls per window = ~1.5 cycles.
The trigram build for 7M records with avg 20-byte names processes ~126M
windows. Extra cost: 126M × 1.5 = ~189M cycles ≈ **60 ms** at 3 GHz.
The 140 MB `memcpy` (clone) alone costs ~30 ms. Plus the in-place lowercase
pass costs another ~30 ms. So inline folding costs 60 ms vs clone+lower
costing 60 ms — **identical throughput, zero memory overhead**.

### Why This Works for NTFS / ASCII Filenames

NTFS stores filenames as UTF-16. The UFFS parser transcodes to UTF-8. For
case-insensitive matching, only ASCII letters A-Z need folding (bytes
0x41-0x5A → 0x61-0x7A). `u8::to_ascii_lowercase()` handles exactly this.

Non-ASCII Unicode characters (e.g., Ü → ü) are NOT folded by ASCII
lowering. This is acceptable because:
1. NTFS itself uses a locale-specific case mapping table for non-ASCII
2. Windows file search (Everything, etc.) also uses ASCII-only case folding
3. The trigram index already only indexes ASCII-folded trigrams
4. Unicode case folding would require ICU tables (~100 KB) and is 10×
   slower per character

The codebase consistently uses `to_ascii_lowercase` (not `to_lowercase`)
everywhere. This strategy preserves that consistency.

---

## 6. Implementation Blueprint

### Step 1: Fix Trigram Build — Eliminate 140 MB Clone (Tier 3)

**Files:** `trigram.rs`, `compact.rs`, `compact_cache.rs`, `compact_loader.rs`

**Change in `TrigramIndex::build`:** Accept original-case `names: &[u8]`
(rename parameter from `names_lower`). Apply `to_ascii_lowercase()` inline
in both passes (count and scatter):

```rust
// Pass 1 — count (line 124 of trigram.rs):
for window in bytes.windows(3) {
    let tri: [u8; 3] = match window.try_into() {
        Ok(arr) => arr,
        Err(_) => continue,
    };
    // Fold case inline — 3 conditional subtractions per window
    let packed = pack_trigram([
        tri[0].to_ascii_lowercase(),
        tri[1].to_ascii_lowercase(),
        tri[2].to_ascii_lowercase(),
    ]);
    if seen.insert(packed) {
        *local.entry(packed).or_insert(0) += 1;
    }
}

// Pass 2 — scatter (scatter_one_record, line 443):
for window in bytes.windows(3) {
    let tri: [u8; 3] = match window.try_into() {
        Ok(arr) => arr,
        Err(_) => continue,
    };
    let packed = pack_trigram([
        tri[0].to_ascii_lowercase(),
        tri[1].to_ascii_lowercase(),
        tri[2].to_ascii_lowercase(),
    ]);
    // ... rest unchanged
}
```

**Change in callers** — remove the clone:

```rust
// compact.rs:432-437 — BEFORE:
let trigram = {
    let mut names_lower = names.clone();
    names_lower.make_ascii_lowercase();
    TrigramIndex::build(&records, &names_lower)
};

// compact.rs — AFTER:
let trigram = TrigramIndex::build(&records, &names);
```

Same pattern for `compact_cache.rs:159-161` and `compact_loader.rs:444-446`.

**Impact:** Eliminates 140 MB × 3 call sites = **420 MB transient allocs**.

---

### Step 2: Fix Search Matching — Extend Buffer Pattern (Tier 2)

**File:** `query.rs` lines 410–440

The `matches` closure currently has three paths. The memchr path already
uses `lower_buf`. Extend it to the glob and whole-word paths:

```rust
// AFTER: All paths use the reusable buffer
let matches = |name: &str, buf: &mut Vec<u8>| -> bool {
    if name.is_empty() || name == "." { return false; }

    // Prepare lowered name in reusable buffer (zero alloc)
    buf.clear();
    buf.extend_from_slice(name.as_bytes());
    buf.make_ascii_lowercase();
    // Safety: ASCII lowering preserves UTF-8 validity
    let lower = core::str::from_utf8(buf.as_slice()).unwrap_or("");

    if whole_word {
        if case_sensitive {
            if is_glob || is_or { tree::name_matches(name, needle) }
            else { name == needle }
        } else if is_glob || is_or {
            tree::name_matches(lower, needle)
        } else {
            lower == needle
        }
    } else if let Some(fnd) = &finder {
        fnd.find(buf.as_slice()).is_some()
    } else if case_sensitive {
        tree::name_matches(name, needle)
    } else {
        tree::name_matches(lower, needle)
    }
};
```

**Impact:** Eliminates C1, C2 (50K–7M allocs per search).

---

### Step 3: Fix Tree Search — Thread Buffer Through Functions (Tier 2)

**File:** `tree.rs` — 5 call sites (C7–C11)

Add `lower_buf: &mut Vec<u8>` parameter to `tree_search` and its helpers.
Use the buffer for all name lowering inside tree traversal:

```rust
// Single-segment (C7, line 191):
let lower = lowercase_into(rec.name(&drive.names), buf);
let lower_str = core::str::from_utf8(lower).unwrap_or("");
!lower_str.is_empty() && lower_str != "." && lower_str.contains(first_segment)

// First segment dir match (C8, line 218):
let lower = lowercase_into(&rec.name(&drive.names), buf);
let lower_str = core::str::from_utf8(lower).unwrap_or("");
segment_matches(lower_str, first_segment)

// Intermediate children (C9, line 236):
// Same pattern — use buf for lowercase_into

// Leaf match (C10, line 264):
// Same pattern

// collect_all_descendants (C11, line 315):
// Same pattern — pass buf through recursive calls
```

**Impact:** Eliminates C7–C11 (thousands of allocs per tree search).

---

### Step 4: Fix Sort — Zero-Alloc Byte Comparison (Tier 1)

**File:** `query.rs` — `sort_indices_by_name` (C3, C4)

Replace the two `to_ascii_lowercase()` allocs per comparison with lazy
byte-level comparison:

```rust
fn sort_indices_by_name(indices: &mut [u32], drive: &DriveCompactIndex, desc: bool) {
    indices.sort_unstable_by(|&idx_a, &idx_b| {
        let a = drive.records.get(idx_a as usize)
            .map_or(&[] as &[u8], |rec| rec.name(&drive.names).as_bytes());
        let b = drive.records.get(idx_b as usize)
            .map_or(&[] as &[u8], |rec| rec.name(&drive.names).as_bytes());
        // Lazy byte-level case-insensitive comparison:
        // Short-circuits on first difference — average ~5 bytes compared
        let ord = a.iter()
            .map(u8::to_ascii_lowercase)
            .cmp(b.iter().map(u8::to_ascii_lowercase));
        if desc { ord.reverse() } else { ord }
    });
}
```

**Impact:** Eliminates C3, C4. Reduction: 2 × N × log₂(N) allocs → 0.

---

**File:** `backend.rs` — `sort_rows` (C12, C13, C14)

For numeric sorts (Size, Modified — the 90% case), skip string keys entirely:

```rust
pub fn sort_rows(rows: &mut [DisplayRow], column: SortColumn, ...) {
    if rows.len() <= 1 { return; }

    let needs_string_keys = matches!(
        column,
        SortColumn::Name | SortColumn::Path | SortColumn::Extension
    );

    if needs_string_keys {
        // Existing Schwartzian transform (but with buffer reuse
        // for the string keys — see below)
        sort_rows_string(rows, column, descending, extra_tiers);
    } else {
        // Fast path: direct numeric comparison
        rows.sort_unstable_by(|a, b| {
            let mut ord = compare_numeric(a, b, column);
            if descending { ord = ord.reverse(); }
            // Name tiebreaker: lazy byte-level compare (zero alloc)
            if ord == Ordering::Equal {
                ord = a.name().as_bytes().iter()
                    .map(u8::to_ascii_lowercase)
                    .cmp(b.name().as_bytes().iter()
                        .map(u8::to_ascii_lowercase));
            }
            ord
        });
    }
}
```

For the string-key path (name/path/extension sorts), the Schwartzian
transform still makes sense (N allocs instead of N log N), but the keys
should be built with buffer reuse where possible.

**Impact:** For numeric sorts: C12, C13, C14 eliminated (30K allocs → 0).
For string sorts: reduced from 3N allocs to N allocs via Schwartzian
(acceptable — these are result-sized, not corpus-sized).

---

### Step 5: Fix Filter Matching (Tier 2)

**File:** `filters.rs` — `matches_row` (C15)

The `matches_row` function already has a `lower_buf` for one path.
Extend it to the name matching path:

```rust
// BEFORE (line 323):
let lower = row.name.to_ascii_lowercase();

// AFTER:
lower_buf.clear();
lower_buf.extend_from_slice(row.name.as_bytes());
lower_buf.make_ascii_lowercase();
let lower = core::str::from_utf8(&lower_buf).unwrap_or("");
```

**Impact:** Eliminates C15 (10K allocs per filter pass).

---

## 7. Migration Path

### Execution Order

The steps are ordered to minimise risk and maximise early payoff:

```
Step 1: Trigram inline lowering  ─── Highest memory impact (420 MB saved)
  │                                  Self-contained in trigram.rs + 3 callers
  ▼
Step 2: Search match buffer      ─── Highest CPU impact (hot path)
  │                                  Self-contained in query.rs
  ▼
Step 3: Tree search buffer       ─── Moderate impact
  │                                  Requires threading buf through 3 functions
  ▼
Step 4: Sort zero-alloc          ─── Moderate impact
  │                                  Two independent changes (query.rs + backend.rs)
  ▼
Step 5: Filter buffer            ─── Low impact
                                     Self-contained in filters.rs
```

### Dependencies

- Steps 1–5 are **fully independent** and can be done in any order.
- No step changes the public API of `DriveCompactIndex` or `SearchResult`.
- The `TrigramIndex::build` signature changes in Step 1 (parameter rename,
  not type change), which affects its 3 callers.
- Steps 2–5 change internal function signatures (add `&mut Vec<u8>` param).

### Risk Assessment

| Step | Risk | Mitigation |
|------|------|------------|
| 1 | Trigram posting lists differ? | Bit-for-bit comparison test: build old way + new way, compare CSR arrays |
| 2 | Closure lifetime with buffer borrow? | Rust borrow checker enforces correctness; existing buf pattern proven |
| 3 | Recursive tree search with mutable buf? | Single buf is fine — recursion depth ≤ path depth (~20), no concurrency |
| 4 | Sort order differs? | Golden-output test: sort 10K results old way + new way, compare order |
| 5 | Filter correctness? | Existing filter tests cover all patterns |

### What NOT To Change

These are correctly optimised and must not be touched:

1. **Category A sites** (40 setup/config sites) — their allocations are
   amortised and occur once per query/config, not per-record.
2. **Category D sites** (15 already-optimal byte-level sites) — these are
   the reference implementation for the target pattern.
3. **The `names` blob itself** — it must remain original-case for display.
4. **Trigram search-time lowering** — query trigrams are lowered once per
   query (in the caller), which is correct and cheap.

---

## 8. Validation

### Correctness Tests

Each step must pass:

1. **Existing test suite:** `cargo nextest run -p uffs-core -p uffs-mft`
2. **Trigram accuracy (Step 1):** Build trigram index both old way (clone +
   lowercase) and new way (inline folding). Assert that `keys`, `offsets`,
   and `postings` arrays are bit-for-bit identical.
3. **Search accuracy (Steps 2, 3):** For a known compact index, run a set
   of test queries (substring, glob, whole-word, tree path) and assert that
   result sets (names, paths, sizes) are identical before and after.
4. **Sort accuracy (Step 4):** Sort 10K diverse DisplayRows by each column
   (Size, Modified, Name, Path, Extension) and assert order is identical.
5. **Filter accuracy (Step 5):** Run filter matching on known rows and
   assert same accept/reject decisions.

### Performance Benchmarks

| Benchmark | Metric | Target |
|-----------|--------|--------|
| Trigram build (Step 1) | Build time (ms) + peak RSS | Time: ≤ current ±5%; RSS: -140 MB |
| Substring search (Step 2) | Search latency (ms) | <50 ms for 7M records |
| Glob search (Step 2) | Heap allocs per search | 0 per-record allocs |
| Tree search (Step 3) | Heap allocs per tree traversal | 0 per-child allocs |
| Numeric sort (Step 4) | Sort time (ms) + alloc count | 0 allocs; time ≤ current |
| Name sort (Step 4) | Sort time (ms) + alloc count | 0 allocs; time ≤ current |
| Cache load (Step 1) | Load time (ms) + peak RSS | Time: -25%; RSS: -140 MB |

---

## Summary: Before vs. After

```
BEFORE (current state)
══════════════════════
  Category B: 3 sites × 140 MB clone         = 420 MB transient
  Category C: 15 sites × per-record String    = millions of heap allocs/search
  Total lowercase heap allocs per search:       50,000 – 7,000,000
  Peak transient memory for lowercasing:        420 MB

AFTER (never-allocate strategy)
═══════════════════════════════
  Category B: 0 bytes — inline trigram folding
  Category C: 0 allocs — buffer reuse + byte-level compare
  Total lowercase heap allocs per search:       0
  Peak transient memory for lowercasing:        256 bytes (one reusable buffer)

  Improvement factor:
    Memory:  420 MB → 256 bytes  (1,640,625× reduction)
    Allocs:  7M → 0             (∞ reduction)
    CPU:     Comparable          (per-byte folding ≈ bulk memcpy + lower)
```

The strategy is simple, proven (already used in Category D sites), and
requires no new dependencies, no new data structures, and no API changes.
It eliminates the systemic lowercase problem identified across docs 13–15
with a single coherent principle: **fold at the point of comparison, never
materialise.**

---

## 9. International & Multi-Byte Character Analysis

### How NTFS Actually Stores Filenames

A common misconception is that NTFS stores filenames as ASCII with a "bit
setting" to reconstruct proper multi-byte characters. **This is not the
case.** NTFS stores **every filename as UTF-16LE — always**, even pure
ASCII names:

```
"readme.txt" on disk (UTF-16LE):
  72 00  65 00  61 00  64 00  6D 00  65 00  2E 00  74 00  78 00  74 00
  r      e      a      d      m      e      .      t      x      t
```

Every character occupies at least 2 bytes. Characters outside the Basic
Multilingual Plane (emoji, rare CJK) use 4-byte surrogate pairs.

NTFS does store **two name variants** per file when the long name isn't
8.3-compatible, but this is for DOS backward compatibility — not encoding:

| Namespace | Purpose | Example |
|-----------|---------|---------|
| 0x01 (Win32) | Full long name, UTF-16LE | `Über die Quantenmechanik.pdf` |
| 0x02 (DOS) | Legacy 8.3 short name | `BERDI~1.PDF` |
| 0x03 (Both) | When long name IS 8.3 | `README.TXT` |

### How UFFS Converts to UTF-8

The parser transcodes UTF-16LE → UTF-8 at MFT parse time. The most
optimised path (`unified.rs`) handles the full UTF-16 spec including
surrogate pairs:

```rust
fn decode_utf16le_into(bytes: &[u8], out: &mut String) {
    out.clear();
    while i + 1 < bytes.len() {
        let code = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        match code {
            0xD800..=0xDBFF => { /* high surrogate → read low → compose */ }
            0xDC00..=0xDFFF => { out.push(char::REPLACEMENT_CHARACTER); }
            _ => { out.push(char::from_u32(u32::from(code)).unwrap_or('�')); }
        }
    }
}
```

The `names` blob in `DriveCompactIndex` is therefore **valid UTF-8**, and
multi-byte characters are already stored correctly:

| Character | Script | UTF-16LE (MFT) | UTF-8 (names blob) | Bytes |
|-----------|--------|-----------------|---------------------|-------|
| `r` | ASCII | `72 00` | `72` | 1 |
| `ü` | German | `FC 00` | `C3 BC` | 2 |
| `é` | French | `E9 00` | `C3 A9` | 2 |
| `ñ` | Spanish | `F1 00` | `C3 B1` | 2 |
| `Σ` | Greek | `A3 03` | `CE A3` | 2 |
| `Д` | Cyrillic | `14 04` | `D0 94` | 2 |
| `中` | Chinese | `2D 4E` | `E4 B8 AD` | 3 |
| `日` | Japanese | `E5 65` | `E6 97 A5` | 3 |
| `한` | Korean | `5C D5` | `ED 95 9C` | 3 |
| `🎵` | Emoji | `3C D8 B5 DF` | `F0 9F 8E B5` | 4 |

### The Case Folding Gap

`to_ascii_lowercase()` only folds bytes 0x41–0x5A (A–Z → a–z). It does
**not** fold non-ASCII Unicode characters:

```
"ÜBER.txt"   → to_ascii_lowercase → "üBER.txt"   ← Ü (0xC3 0x9C) NOT folded
"Résumé.pdf" → to_ascii_lowercase → "résumé.pdf"  ← É (0xC3 0x89) NOT folded
"NAÏVE.doc"  → to_ascii_lowercase → "naÏve.doc"   ← Ï (0xC3 0x8F) NOT folded
"中文.txt"   → to_ascii_lowercase → "中文.txt"     ← OK (CJK has no case)
"МОСКВА.txt" → to_ascii_lowercase → "мОСКВА.txt"  ← Cyrillic NOT folded
```

This means **searching for "über" will not find "ÜBER.txt"** — a real bug
for European users. This is a pre-existing limitation across the entire
codebase, not something the never-allocate strategy introduces.

### How NTFS Handles Case: The `$UpCase` Table

NTFS has its own case-folding mechanism. MFT Record #10 (`$UpCase`)
contains a **128 KB lookup table** — 65,536 entries × 2 bytes — mapping
every UTF-16 code point to its uppercase equivalent. NTFS uses this for
ALL case-insensitive operations (filename uniqueness, directory lookups).

The UFFS codebase already knows about this file (it filters it as a
system file) but never reads or uses the table for search.

### Three Strategies for International Case Folding

#### Strategy A: Unicode Simple Case Fold Table (Recommended for Phase 2)

Unicode defines a **Simple Case Folding** mapping — a static,
locale-independent, one-to-one codepoint transformation published in
`CaseFolding.txt`. It covers ~1,400 codepoint mappings across Latin,
Greek, Cyrillic, Armenian, Georgian, and other scripts.

**Key property:** Simple Case Folding is **length-preserving** for all
practical scripts. A single codepoint always folds to a single codepoint
that occupies the same number of UTF-8 bytes. This makes it fully
compatible with the never-allocate strategy.

```rust
// Compile-time generated two-level lookup table (~5.6 KB):
// Level 1: 256-entry block index (covers high byte of codepoint)
// Level 2: variable-size blocks for each populated high byte

/// Fold a single Unicode codepoint to its case-folded equivalent.
/// Returns the character unchanged if no fold is defined.
#[inline]
fn case_fold_char(c: char) -> char {
    let cp = c as u32;
    if cp < 0x80 {
        // ASCII fast path — zero overhead vs current behaviour
        return (cp as u8).to_ascii_lowercase() as char;
    }
    // Two-level table lookup for non-ASCII (~11 comparisons worst case)
    lookup_case_fold_table(cp).map_or(c, |folded| {
        char::from_u32(folded).unwrap_or(c)
    })
}
```

| Metric | Value |
|--------|-------|
| Memory | ~5.6 KB static (compiled into binary, zero runtime alloc) |
| ASCII fast path | Identical to current — zero overhead for English files |
| Non-ASCII lookup | ~11 comparisons (binary search over ~1,400 entries) |
| Locale-aware? | No — locale-independent (same as NTFS `$UpCase`) |
| Coverage | Latin, Greek, Cyrillic, Armenian, Georgian, Cherokee, etc. |
| Length-preserving? | Yes for Simple Folding (no ß→ss, no İ→iı) |

**What it fixes:**

```
"ÜBER.txt"   → case_fold → "über.txt"     ✅ Ü → ü
"Résumé.pdf" → case_fold → "résumé.pdf"   ✅ É → é
"NAÏVE.doc"  → case_fold → "naïve.doc"    ✅ Ï → ï
"МОСКВА.txt" → case_fold → "москва.txt"   ✅ М → м, О → о, etc.
```

#### Strategy B: Read NTFS `$UpCase` from MFT (Perfect NTFS Compatibility)

Read the `$UpCase` table (MFT record #10, `$DATA` attribute) during MFT
ingestion. This gives **bit-for-bit identical** case folding to what
NTFS itself uses.

```rust
// 128 KB table: upcase[codepoint] = uppercase equivalent
// Read once during MFT ingestion, stored alongside DriveCompactIndex
struct NtfsUpCase {
    table: Box<[u16; 65536]>,  // 128 KB
}

impl NtfsUpCase {
    /// Fold a UTF-16 code point using the NTFS volume's own case table.
    #[inline]
    fn fold(&self, cp: u16) -> u16 {
        self.table[cp as usize]
    }
}
```

| Metric | Value |
|--------|-------|
| Memory | 128 KB per volume (could be shared across same-version volumes) |
| Precision | Bit-exact match with NTFS's own case folding |
| Complexity | Medium — requires reading `$UpCase` data attribute |
| Caveat | Table maps UTF-16 code units; names blob is UTF-8 |

The caveat is significant: using this table requires decoding each UTF-8
byte sequence to its codepoint (to get the UTF-16 code unit for lookup),
then folding, then comparing. This is still O(1) per codepoint but adds
a UTF-8 decode step per character. For BMP characters (all European
and CJK), UTF-16 code unit = Unicode codepoint, so the lookup is direct.

#### Strategy C: Rust `str::to_lowercase()` — Rejected

Rust's full Unicode `to_lowercase()` can **change string length**:

```rust
"ß".to_uppercase() == "SS"        // 1 char → 2 chars
"İ".to_lowercase() == "i\u{307}"  // 1 char → 2 chars (Turkish dotted I)
"ﬁ".to_uppercase() == "FI"       // 1 ligature → 2 chars
```

Length-changing folds **break** the never-allocate strategy because:
1. Byte-level comparison assumes `fold(a).len()` is predictable
2. Trigram byte windows assume each character's folded form fits in the
   same byte span
3. Buffer-reuse folding assumes the output buffer is ≤ input length

Full Unicode `to_lowercase()` also requires locale context for Turkish
and Azeri (İ/ı distinction). **Rejected for search use.**

### Impact on the Never-Allocate Strategy

The never-allocate strategy from §5 is **fully compatible** with Unicode
Simple Case Folding. Each tier adapts naturally:

#### Tier 1: Comparison-Point Folding (still zero alloc)

```rust
// CURRENT (ASCII-only):
a.iter().map(u8::to_ascii_lowercase).cmp(b.iter().map(u8::to_ascii_lowercase))

// UNICODE (per-codepoint):
a.chars().map(case_fold_char).cmp(b.chars().map(case_fold_char))
```

`str::chars()` is a zero-alloc iterator that decodes UTF-8 on the fly.
Combined with `case_fold_char`, this folds and compares with zero heap
allocation, same as the ASCII version. For pure ASCII filenames (the
vast majority), the `cp < 0x80` fast path makes this identical cost.

#### Tier 2: Buffer-Reuse Folding (still zero alloc)

```rust
// CURRENT (ASCII-only):
buf.clear();
buf.extend_from_slice(name.as_bytes());
buf.make_ascii_lowercase();

// UNICODE:
buf.clear();
let mut encode_buf = [0u8; 4];
for ch in name.chars() {
    let folded = case_fold_char(ch);
    buf.extend_from_slice(folded.encode_utf8(&mut encode_buf).as_bytes());
}
```

The buffer is still reused across records. For pure ASCII, this could
be optimised to detect all-ASCII names (common case) and fall back to
the fast `make_ascii_lowercase()` path.

#### Tier 3: Trigram Window Folding

This is the one tier that requires careful handling with multi-byte
characters. Currently trigrams operate on raw **byte** windows:

```
"über" in UTF-8:  C3 BC 62 65 72
  byte trigrams:  [C3,BC,62] [BC,62,65] [62,65,72]

"ÜBER" in UTF-8:  C3 9C 42 45 52
  byte trigrams:  [C3,9C,42] [9C,42,45] [42,45,52]
```

With ASCII-only folding, "über" and "ÜBER" produce **different byte
trigrams** because Ü (C3 9C) ≠ ü (C3 BC). The trigram index cannot
match them — this is the root cause of the European search bug.

**Solution: Fold before trigram extraction.**

Since Unicode Simple Case Folding is length-preserving for all BMP
characters (Latin, Greek, Cyrillic, CJK), folding a 2-byte UTF-8
sequence always produces a 2-byte UTF-8 sequence. This means:

```rust
// In TrigramIndex::build(), fold each byte within its codepoint context:
for window in bytes.windows(3) {
    // For ASCII bytes (0x00-0x7F): fold individually (fast path)
    // For continuation bytes (0x80-0xBF): leave as-is (they follow
    //   a start byte that was already folded)
    // For start bytes (0xC0-0xFF): decode codepoint, fold, re-encode
    let tri = fold_trigram_bytes(window, names);
    let packed = pack_trigram(tri);
    // ...
}
```

For the 95%+ ASCII case, the fast path `window[i].to_ascii_lowercase()`
is unchanged. The multi-byte path only activates when a trigram window
spans a non-ASCII character boundary.

An alternative (simpler) approach: compute **character-level trigrams**
instead of byte-level trigrams for the index build and query:

```rust
// Character trigrams: 3 Unicode codepoints → packed u64
// "über" → trigrams: ('ü','b','e'), ('b','e','r')
// "ÜBER" → fold → ('ü','b','e'), ('b','e','r')  ← MATCHES ✅
fn char_trigrams(name: &str) -> impl Iterator<Item = u64> {
    let chars: Vec<char> = name.chars().map(case_fold_char).collect();
    chars.windows(3).map(|w| pack_char_trigram(w[0], w[1], w[2]))
}
```

This requires changing the trigram packing from `u32` (3 bytes) to `u64`
(3 codepoints, up to 21 bits each = 63 bits). The LUT size stays the
same if we hash the u64 keys instead of using a flat array.

### CJK and Script-Specific Considerations

#### Chinese / Japanese / Korean (CJK)

CJK characters do not have uppercase/lowercase distinctions. Case folding
is a no-op for the CJK Unified Ideographs block (U+4E00–U+9FFF) and
its extensions. This means:

- **Search works correctly today** — "文件" matches "文件" without folding
- **Trigrams work well** — each CJK character is 3 UTF-8 bytes, producing
  a naturally selective trigram. A 2-character search like "文件" generates
  3 overlapping byte trigrams, all of which are rare.
- **No action needed** for case folding

The main CJK consideration is **minimum query length**: the trigram index
requires at least 3 bytes (= 1 CJK character) for a lookup. A single
CJK character search query would produce 1 trigram — which is fine for
filtering (CJK trigrams have small posting lists due to high cardinality).

#### Japanese Hiragana / Katakana

Japanese has a special case: hiragana (あ) and katakana (ア) represent the
same sounds. Some search tools treat them as equivalent. Unicode Simple
Case Folding does **not** conflate hiragana with katakana — they are
distinct scripts. Full-text search equivalence (katakana → hiragana)
would require a separate normalisation layer, not case folding.

**Recommendation:** Not in scope for case folding. If needed, add as a
separate search option ("kana-insensitive") in the future.

#### Unicode Normalisation (NFC vs NFD)

A subtlety that affects all non-ASCII filenames:

```
"é" can be stored as:
  NFC:  U+00E9        (1 codepoint, 2 UTF-8 bytes: C3 A9)
  NFD:  U+0065 U+0301 (2 codepoints, 3 UTF-8 bytes: 65 CC 81)
```

NTFS normalises filenames to **NFC** (precomposed form). However, files
created by some cross-platform tools (e.g., macOS HFS+ interop) may
introduce NFD names. If two files have the "same" name in NFC vs NFD,
NTFS treats them as different files.

For search: the trigram index would not match NFC "é" against NFD "é"
because their byte representations differ. This is technically correct
(they ARE different filenames in NTFS), but users may find it surprising.

**Recommendation:** Not a Phase 2 concern. If needed, add optional NFC
normalisation during trigram build and query, using the `unicode-normalization`
crate (~20 KB tables).

### Recommended Phasing

```
Phase 1 (Now): ASCII-only folding + never-allocate strategy
├── Implement §6 Steps 1–5 (inline folding, buffer reuse, byte compare)
├── Correct for English and the 95%+ ASCII filenames worldwide
├── Zero regression risk — same behaviour as current, less overhead
└── Delivers: 420 MB saved, millions of allocs eliminated

Phase 2 (International): Unicode Simple Case Fold
├── Add ~5.6 KB static case fold table (compile-time generated)
├── Swap fold function from to_ascii_lowercase → case_fold_char
├── Upgrade trigram build to fold at codepoint boundaries
├── ASCII fast path preserved — zero overhead for English
├── Delivers: Ü/ü, É/é, Cyrillic, Greek all searchable case-insensitively
└── Never-allocate strategy unchanged — just different fold function

Phase 3 (Optional): NTFS $UpCase exact compatibility
├── Read $UpCase from MFT record #10 during ingestion
├── 128 KB in-memory table per volume
├── Bit-exact NTFS case behaviour for edge cases
└── Only needed if Unicode Simple Fold has observable gaps vs NTFS
```

### Why the Never-Allocate Strategy Is Future-Proof

The critical architectural insight: **the never-allocate principle is
orthogonal to which fold function is used.** The three tiers (comparison-
point, buffer-reuse, window-level) work identically whether the fold
function is:

- `u8::to_ascii_lowercase()` (Phase 1)
- `case_fold_char()` via Unicode table (Phase 2)
- `ntfs_upcase.fold()` via $UpCase table (Phase 3)

No data structures change. No APIs change. No memory layout changes.
The fold function is a leaf-level implementation detail that plugs into
the same zero-allocation framework.

```
Never-Allocate Framework (fixed)
  ├── Tier 1: .chars().map(FOLD_FN).cmp(...)     ← swap FOLD_FN
  ├── Tier 2: for ch in .chars() { buf ← FOLD_FN(ch) }  ← swap FOLD_FN
  └── Tier 3: trigram windows with FOLD_FN per codepoint  ← swap FOLD_FN
```

The framework is stable. The fold function is a parameter.