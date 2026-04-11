# UltraFastFileSearch Aggregation Architecture
## Consolidated design for CLI, daemon, and MCP

**Status:** Proposed consolidated design  
**Date:** 2026-04-06  
**Primary audience:** core engine, daemon, CLI, MCP, reviewer, release owner  
**Format:** Markdown  
**Scope:** server-side aggregations, facets, rollups, distributions, duplicate analytics, and MCP-friendly structured summaries

---

## 1. Executive summary

UFFS should add **aggregations as a first-class response path inside the same daemon-owned search contract** that already centralizes pattern matching, predicates, sorting, projection, and response shaping.

This is the core decision:

- **Do not** build a second analytics engine beside search.
- **Do not** overload row grouping or table formatting and call it “aggregation”.
- **Do not** force MCP agents or CLI users to fetch thousands of rows just to count, bucket, or summarize them.
- **Do** extend the canonical request model so one query can return:
  - rows,
  - aggregations,
  - or both.

This design keeps UFFS aligned with the direction already documented elsewhere in the codebase:

- one pipeline,
- one field model,
- one daemon execution engine,
- one semantic source of truth.

The proposal below consolidates:
1. the current aggregation draft,
2. the unified daemon search architecture,
3. the filter/sort/field model work,
4. the search pipeline refactor,
5. the user-manual semantics already visible on the CLI,
6. the search-defaults research,
7. external prior art from modern search systems and file-search tools,
8. community demand patterns from forums and practitioner discussions.

The result is an aggregation system that is:
- **fast** on hot-path fields,
- **typed** instead of SQL-like,
- **useful** for cleanup, storage, and audit workflows,
- **LLM/MCP-friendly**,
- **incrementally shippable**.

---

## 2. Why this feature matters

### 2.1 Problem statement

Today UFFS fundamentally returns **rows**.  
But many real user questions are **summary questions**:

| Natural question | What the engine should return |
|---|---|
| “How many PDF files are here?” | `count(where ext=pdf)` |
| “How much space do videos use?” | `sum(size) where type=video` |
| “Break disk usage down by extension.” | `terms(ext) -> count, sum(size), sum(size_on_disk)` |
| “Show file age distribution.” | `date_histogram(modified)` |
| “Which folders consume the most space?” | `rollup(path, depth=n) -> sum(size), count` |
| “What categories waste the most allocation?” | `terms(type) -> sum(size_on_disk - size)` |
| “How many duplicate candidates are there?” | `duplicates(keys...)` |
| “What percentage of results are pictures?” | `terms(type) + share_of_total` |

Returning raw rows and asking a user or an LLM to count them is wasteful and error-prone:
- it burns tokens,
- it can truncate,
- it forces post-processing in the wrong layer,
- and it hides the real capabilities of the daemon.

### 2.2 Real demand pattern

The strongest recurring user jobs in the desktop file-search ecosystem are:

1. known-item lookup,
2. type/date/size narrowing,
3. cleanup and audit workflows,
4. content/regex workflows,
5. finding what default indexes miss,
6. scoped searches inside a folder or project tree.

That pattern already appears in the UFFS defaults research and in the current UFFS manual structure. Aggregations matter most for the **second and third** groups:
- storage triage,
- cleanup,
- duplicate analysis,
- old/stale content,
- hidden/system/attribute inspection,
- path-length and name-length outliers,
- drive and folder rollups,
- “what kinds of files are here?” questions.

This means the first release should not chase abstract math first. It should optimize for the jobs users repeatedly ask search tools to do.

### 2.3 Why MCP makes this even more important

MCP/LLM clients are disproportionately likely to ask for:
- counts,
- summaries,
- distributions,
- comparisons,
- “top N categories”,
- drill-down guidance.

That makes aggregation a **primary MCP surface**, not a side feature.

---

## 3. Inputs synthesized into this design

This document consolidates and refines the following internal inputs:

### 3.1 Internal UFFS docs synthesized

- `AGGREGATION_ARCHITECTURE.md`
- `FILTER_SORT_FEATURE_MATRIX.md`
- `FILTER_SORT_GAPS.md`
- `UNIFIED_DAEMON_SEARCH_GAP_IMPLEMENTATION_PLAN.md`
- `SEARCH_PIPELINE_REFACTOR.md`
- `mft_search_defaults_report.md`
- `cli-overview.md`
- `filters.md`
- `search-modes.md`
- `sorting.md`

### 3.2 External research synthesized

The design also incorporates patterns from:
- Elasticsearch aggregations, composite bucket pagination, and `top_hits`
- Solr JSON Facet API
- Azure AI Search facets, hierarchical facets, facet aggregations, and facet filters
- Meilisearch `facetDistribution`, `facetStats`, and facet-value search
- Typesense faceting, numeric facet stats, and grouped results
- Algolia facet counts, exhaustivity metadata, and facet-value search
- SearchMyFiles summary mode and duplicate search
- Everything forum guidance on duplicates/unique views scoped to current results
- MCP tools, `outputSchema`, `structuredContent`, `resource_link`, and tasks

A reference section appears at the end.

---

## 4. Architecture constraints and non-negotiables

## 4.1 One pipeline, not two

Aggregation must plug into the same conceptual flow already described by the pipeline refactor and the unified daemon search plan:

```text
pattern -> predicates -> match set -> aggregate and/or rows -> response shaping
```

Aggregation is **not** a different search engine.
It is a different **response operator** over the same matched domain.

## 4.2 Daemon-owned semantics

The daemon must own:
- field meaning,
- aggregate validity,
- aggregate execution,
- projection of per-bucket sample rows,
- exactness/truncation metadata,
- response shaping.

The CLI, MCP, TUI, and future GUI should only assemble user intent and render results.

## 4.3 Typed model, not SQL

UFFS should continue to avoid becoming a general query engine or SQL clone.

The model should stay:
- typed,
- field-driven,
- linear,
- hot-path aware,
- explicit about cost tiers.

The goal is not `SELECT ... GROUP BY ... HAVING ...`.
The goal is:

- search semantics that already fit UFFS,
- plus a typed aggregation model that works naturally over file-system data.

## 4.4 Aggregate queries must not route through `Vec<DisplayRow>` by default

For aggregate-only requests:
- do **not** eagerly resolve paths unless an aggregate requires it,
- do **not** sort rows unless sample rows require it,
- do **not** materialize output columns just because the CLI might show a table.

Operate directly on compact/hot data wherever possible.

## 4.5 Keep the CLI search-first mental model

UFFS is still a search tool.
Aggregation should feel like “search plus summary”, not like a separate BI subsystem.

## 4.6 Preserve compatibility with existing `uffs stats`

`uffs stats` should become a **thin compatibility layer** over the new aggregation engine.
It should not remain a separate implementation island.

---

## 5. Current-state field inventory (reconciled 2026-04-06)

### 5.1 Field inventory — resolved

Previous drafts of this section noted a perceived disagreement between docs
("35 variants" vs "52/55-field target"). A code-first audit against
`crates/uffs-core/src/search/field.rs` resolved the discrepancy:

**Code has 39 `FieldId` variants today.** The original 52-variant target
enum in `FILTER_SORT_FEATURE_MATRIX.md §4.3` was a *target design*, not a
description of current state. The gap is the **17 cold-path/forensic fields**
planned for Wave 5 of `SEARCH_PIPELINE_REFACTOR.md` but not yet added to the
enum. Four fields were added organically after the target was written.

| Category | Variants | In code? | Notes |
|----------|----------|----------|-------|
| Core identity (Path, Name, PathOnly, Extension, Type, Drive) | 6 | ✅ | |
| Size/storage (Size, SizeOnDisk¹, TreeSize, TreeAllocated, Bulkiness) | 5 | ✅ | ¹ target called this `Allocated` |
| Hot timestamps (Created, Modified, Accessed) | 3 | ✅ | |
| Bool attrs (Hidden … RecallOnDataAccess) | 19 | ✅ | `DirectoryFlag` in code = `Directory` in target |
| Structure (Descendants) | 1 | ✅ | |
| Raw aggregate (Attributes, AttributeValue²) | 2 | ✅ | ² added after target was written |
| Length fields (NameLength, PathLength)² | 2 | ✅ | ² added after target was written |
| Legacy compat (ParityAttributes)² | 1 | ✅ | ² legacy artifact; may deprecate |
| **Subtotal — implemented** | **39** | **✅** | |
| $FILE_NAME timestamps (FnCreated … FnMftChanged) | 4 | ❌ | Wave 5 — cold path |
| MFT structure (Frs … StreamCount) | 7 | ❌ | Wave 5 — cold path |
| Journaling (Usn, Lsn) | 2 | ❌ | Wave 5 — cold path |
| Security (SecurityId, OwnerId) | 2 | ❌ | Wave 5 — cold path |
| Reparse (ReparseTag) | 1 | ❌ | Wave 5 — cold path |
| Forensic (ForensicFlags) | 1 | ❌ | Wave 5 — cold path |
| **Subtotal — planned (cold/forensic)** | **17** | **❌** | Not in `FieldId` yet |
| **Grand total** | **56** | | 39 implemented + 17 planned |

The earlier "55" figure in `FILTER_SORT_FEATURE_MATRIX.md §5.4` was a
counting artifact: the coverage analysis tables listed `extension_id`
(internal, not a `FieldId`) and counted `NameLength`/`PathLength` in
the hot-path analysis without adding them to the 52-variant target enum,
producing 13+6+19+17 = 55 in the summary while the enum had 52.

### 5.2 Access-tier status — resolved

All **39 implemented fields** are either hot (in `CompactRecord`) or derived
from hot data (path resolution, extension lookup, type classification,
bulkiness calculation). No implemented field requires cold-path I/O.

The 17 planned fields *will* require either `ExtraRecordFields` from the
`.uffs` cache or MFT re-reads. They are genuinely cold and remain deferred
to Wave 5.

### 5.3 Required practice

**Aggregation capability must be generated from code, not maintained by hand in docs.**

Concretely:
- `FieldId` + `FieldMeta` is the source of truth for which fields exist,
- aggregation support should be expressed in `AggregateMeta`,
- docs and CLI help should be generated from metadata,
- not copied manually into a second matrix.

Until that is done, any hand-written "aggregatable fields" table will drift.

---

## 6. Design principles

1. **Same pipeline, different output.**  
   Aggregation reuses search semantics; only the result operator changes.

2. **Operate on hot data first.**  
   Common aggregates should use `CompactRecord`/compact-index data directly.

3. **Aggregate over the matched set, not the paged row subset.**  
   By default, aggregations summarize the full filtered result domain.

4. **Rows are optional.**  
   Aggregate-only mode should be the default whenever aggregate flags are present.

5. **Presets for common jobs, a DSL for power users.**  
   MCP and most humans want ergonomic presets; advanced users need composable specs.

6. **MCP-first structured outputs.**  
   Aggregations should return machine-valid structured results first, with optional human formatting.

7. **Explicit exactness and truncation.**  
   Never silently approximate or truncate without saying so.

8. **Hierarchy matters.**  
   Path/folder rollups are a core file-system primitive, not an afterthought.

9. **Scope and drill-down matter.**  
   Aggregation should always respect the current query scope and produce drill-down handles.

10. **Duplicate detection is staged.**  
    Fast candidate grouping first, expensive verification second.

11. **`--group-by` stays reserved.**  
    Do not overload planned row-grouping semantics with aggregate grouping syntax.

---

## 7. What external research says UFFS should learn from

## 7.1 Modern search engines: the baseline feature set

Modern search systems converge on a shared aggregation vocabulary:

- **bucket aggregations**  
  group results into buckets,
- **metric aggregations**  
  compute counts, sums, min/max, averages, etc.,
- **sub-aggregations**  
  compute metrics inside buckets,
- **sample / top-hit exemplars**  
  show representative rows per bucket,
- **bucket pagination**  
  for very large cardinality spaces,
- **facet value search**  
  search within possible facet values,
- **exactness metadata**  
  distinguish complete vs approximate counts,
- **hierarchical facets**  
  especially for nested categories,
- **numeric facet stats**  
  min/max/sum/avg on numeric buckets.

UFFS should adopt these ideas, but with file-search semantics.

## 7.2 File-search tools: the jobs users actually care about

The strongest file-search precedents are:
- SearchMyFiles summary mode,
- SearchMyFiles duplicate search,
- SearchMyFiles empty-folder views,
- Everything duplicate/unique views over current results,
- scoped duplicate finding after narrowing the search,
- folder and drive summary reporting,
- hidden/system/compressed counts,
- path and folder scoping as first-class user intent.

This strongly suggests that UFFS must prioritize:

- type and extension breakdowns,
- size and age distributions,
- folder rollups,
- hidden/system/compressed/encrypted summaries,
- empty-folder and zero-byte cleanup,
- duplicate candidates and verified duplicates.

## 7.3 Community demand patterns that should shape the design

Community threads and practitioner discussions repeatedly emphasize:
- “search inside the current folder / subtree”
- “show only this scope, not global noise”
- “search what the default index misses”
- “find duplicates only after I narrow the scope”
- “distinguish filename search from deeper/content search”
- “show representative results, not only counters”

That reinforces four design requirements:

1. **all aggregations must respect scope**  
   especially drive, subtree, and folder filters

2. **duplicate workflows must be scoped and staged**  
   first narrow, then group, then verify

3. **sample rows per bucket are important**  
   for both humans and LLM agents

4. **aggregation should support guided refinement**  
   bucket outputs should provide drill-down predicates

---

## 8. Product model: what “aggregation” means in UFFS

UFFS aggregation should be defined in terms of these primitives.

## 8.1 Aggregate domain

The record set over which aggregation runs.

```text
Matched domain (default): all records matching pattern + predicates
Page domain (optional): only rows surviving row sort + page window
```

**Default must be `Matched`.**

## 8.2 Bucket

A group of matching records sharing one or more keys:
- extension bucket,
- type bucket,
- drive bucket,
- size range bucket,
- month bucket,
- folder rollup bucket,
- duplicate candidate group.

## 8.3 Metric

A scalar computed over a domain or a bucket:
- count,
- sum(size),
- min(modified),
- avg(size),
- waste bytes,
- share of total.

## 8.4 Facet

A bucket aggregation primarily used for navigation/refinement:
- typically low or medium cardinality,
- counts shown next to values,
- often paired with “drill down”.

## 8.5 Rollup

A file-system-specific hierarchical aggregation:
- group by folder ancestor,
- group by drive,
- group by path depth,
- group by top folder within a scope.

## 8.6 Exemplar / sample row

A small representative set of rows per bucket:
- top 1–3 rows by sort,
- useful for quick inspection,
- essential for MCP agent actionability.

## 8.7 Duplicate candidate group

A bucket keyed by duplicate heuristics:
- e.g. `(size, name)` or `(size, hash)`

Duplicate groups have stages:
1. candidate,
2. partially verified,
3. fully verified.

---

## 9. Recommended aggregate families

UFFS should ship these aggregate families in this order.

## 9.1 Scalar summaries (must-have v1)

These should be available globally and inside any bucket.

### Core counts
- `count`
- `file_count`
- `dir_count`

### Storage totals
- `sum(size)`
- `sum(size_on_disk)`
- `waste_bytes = sum(size_on_disk - size)`
- `waste_pct`

### Min / max / average
- `min(size)`
- `max(size)`
- `avg(size)`
- `min(size_on_disk)`
- `max(size_on_disk)`
- `avg(size_on_disk)`

### Time bounds
- `newest(modified)`
- `oldest(modified)`
- same for created/accessed if requested

### Distinct counts
- `distinct_count(ext)`
- `distinct_count(type)`
- `distinct_count(path_only)` (careful on cost)

### Missing / special-value counts
- no extension
- zero-byte files
- no semantic type match / `other`
- files vs dirs
- hidden/system/compressed/encrypted counts
- long-path count
- empty-directory count (when relevant)

## 9.2 Semantic facets (must-have v1)

These are the highest-value interactive buckets.

### Recommended default facet fields
- `type`
- `ext`
- `drive`
- `directory_flag` / file-vs-dir
- boolean attributes
- month-of-year
- year
- modified age buckets

### Why `type` must be first-class
Users ask:
- pictures,
- documents,
- videos,
- code,
- executables,
- archives.

They do not usually start from raw extension sets.
So `type` should be the human-facing default facet, while `ext` remains the power-user refinement.

## 9.3 Numeric distributions (must-have v1)

These make the feature feel modern.

### Histograms and ranges
- `histogram(size)`
- `histogram(size_on_disk)`
- `histogram(bulkiness)`
- `histogram(descendants)`
- `histogram(name_length)`
- `histogram(path_length)`
- `histogram(tree_size)` for directories
- `histogram(tree_allocated)` for directories

### Practical default size buckets
- `0`
- `1B–1KB`
- `1KB–1MB`
- `1MB–100MB`
- `100MB–1GB`
- `1GB–10GB`
- `10GB+`

### Practical age buckets
- today
- 7d
- 30d
- 90d
- 1y
- 2y
- 5y+
- custom range mode

### Practical path-risk buckets
- `<128`
- `128–199`
- `200–239`
- `240–259`
- `260+`

## 9.4 Date histograms (must-have v1)

Support:
- hour
- day
- week
- month
- quarter
- year

Fields:
- modified (default)
- created
- accessed

Use cases:
- “what changed recently?”
- “what years dominate this archive?”
- “how old is this subtree?”
- “when was this type of content created?”

## 9.5 Rollups and folder summaries (must-have v2)

This is where UFFS can beat general-purpose search engines.

### Recommended rollups
- by drive
- by immediate parent folder
- by ancestor depth `N`
- by path root / top folder
- nested `drive -> top_folder -> type`
- nested `top_folder -> ext`

### Important semantic rule

UFFS already has directory-level fields like `tree_size` and `tree_allocated`.
Those are **directory-record metrics**.

Rollups are different:
- `rollup(path)` summarizes the **matched set**, grouped by folder.
- `tree_size` summarizes the **entire subtree for a directory row**.

These are both useful and must not be conflated.

## 9.6 Distinct / unique / duplicate families (must-have v2, partially v1)

This family deserves first-class treatment, not a hacked filter.

### Distinct
- distinct values only
- optionally return one representative row per value

### Unique
- values occurring exactly once

### Duplicate candidates
- bucket where count > 1

### Verified duplicates
- bucket where verification confirms identity

### Useful duplicate keys
- `name`
- `size`
- `size + name`
- `size + ext`
- `size + modified + name`
- `content hash` (deep stage)

### Duplicate metrics
- candidate group count
- candidate file count
- verified group count
- reclaimable bytes
- largest group
- top duplicate keys
- sample rows per group

## 9.7 Later / advanced families

These should be deferred until field truth is resolved and initial aggregates are stable.

### Advanced numeric
- percentiles
- median
- percentile ranks
- cumulative histogram metrics

### Forensic/admin
- namespace
- reparse tag
- owner/security ID
- forensic flags
- USN / LSN ranges

### Pipeline-style derivatives
- percentage of total
- percentage of parent bucket
- running total
- bucket ranking
- delta between domains (future snapshot compare)

---

## 10. Concrete answer to “what should we aggregate?”

This section turns the feature into specific file-search outputs.

## 10.1 Global summary card

Every aggregate response should be able to include a compact top-level summary:

- total matches
- files
- directories
- total logical bytes
- total allocated bytes
- waste bytes
- waste %
- newest modified
- oldest modified
- unique extensions
- unique types
- hidden/system/compressed/encrypted counts
- top drive by bytes
- top type by bytes

This should be the basis of `--aggregate overview`.

## 10.2 Type breakdown

Buckets by `type` with:
- count
- file_count
- total_size
- total_size_on_disk
- waste_bytes
- avg_size
- pct_of_total_count
- pct_of_total_bytes
- sample rows

This answers:
- how many pictures/documents/videos/code files,
- what categories dominate storage,
- which categories are wasteful.

## 10.3 Extension breakdown

Buckets by `ext` with:
- count
- total_size
- total_size_on_disk
- avg_size
- newest modified
- pct_of_total_bytes
- sample rows

This answers:
- top extensions by bytes,
- top extensions by count,
- stale or risky extensions,
- which extensions are bloated.

## 10.4 Drive summary

Buckets by `drive` with:
- count
- total_size
- total_size_on_disk
- waste_bytes
- newest modified
- oldest modified
- top types nested
- top folders nested

This is especially useful for MCP and system audit flows.

## 10.5 Folder storage summary

Buckets by path rollup with:
- count
- total_size
- total_size_on_disk
- max modified
- type distribution
- ext distribution
- sample rows

This is the UFFS-native equivalent of folder summary reports.

## 10.6 Age distribution

Date histogram or age ranges with:
- count
- total_size
- pct_of_total
- representative types
- optionally top folders per bucket

Useful for:
- archive triage,
- cleanup,
- “what changed recently?”,
- “what is stale?”.

## 10.7 Waste / allocation analysis

Buckets by:
- type,
- extension,
- path rollup,
- bulkiness range

Metrics:
- total_size
- total_size_on_disk
- waste_bytes
- waste_pct

This turns `bulkiness` into a true analysis feature instead of only a row sort/filter.

## 10.8 Hidden/system/compressed/encrypted summary

Buckets by bool flags or bool flag combinations.

Examples:
- hidden vs not hidden
- system vs not system
- compressed vs not compressed
- encrypted vs not encrypted
- `hidden + system`
- `hidden + !system`

This is useful for cleanup, troubleshooting, admin, and forensics.

## 10.9 Empty / sparse structure summary

Counts and rollups for:
- empty directories
- zero-byte files
- long paths
- long names
- sparse files
- reparse points

This is an extremely valuable cleanup-oriented preset.

## 10.10 Duplicate analytics

Default duplicate outputs should include:
- candidate group count
- candidate file count
- total duplicate bytes
- reclaimable bytes estimate
- top duplicate groups
- top duplicated names
- top duplicated sizes
- sample rows
- verification stage metadata

---

## 11. Recommended preset library

Presets should be job-oriented, not math-oriented.

## 11.1 Core presets

| Preset | Purpose | Default expansion |
|---|---|---|
| `overview` | Global summary | count, files/dirs, totals, type facet, drive facet, modified histogram |
| `by_type` | Category breakdown | `terms:type` with size/waste metrics |
| `by_extension` | Extension breakdown | `terms:ext` with count/size metrics |
| `by_drive` | Per-drive summary | `terms:drive` with totals and nested top types |
| `by_size` | Size distribution | `hist:size` + totals |
| `by_age` | Age distribution | `datehist:modified` or age ranges |
| `storage` | Storage triage | type, extension, top folders, waste |
| `activity` | Change activity | modified/created histograms + hot folders |
| `media` | Pictures/audio/video summary | type facet + size totals + age |
| `cleanup` | Cleanup candidates | zero-byte, empty dirs, long paths, old archives, waste |
| `duplicates` | Duplicate analysis | candidate groups + reclaimable bytes + samples |
| `top_folders` | Largest folders in current scope | `rollup:path` with totals |

## 11.2 MCP-oriented presets

These should be especially concise and highly reusable.

| Preset | Designed for MCP question style |
|---|---|
| `quick_overview` | “What’s on this machine / drive / folder?” |
| `storage_hotspots` | “What takes the most space?” |
| `recent_activity` | “What changed recently?” |
| `file_type_mix` | “What kinds of files are here?” |
| `cleanup_candidates` | “What can I delete or investigate?” |
| `duplicate_candidates` | “Are there duplicates?” |

## 11.3 Rule for presets

Presets are macros over canonical aggregate specs.
They must not introduce a second semantic layer.

---

## 12. Canonical request model

## 12.1 Recommendation

The canonical wire model should remain **one search request type** extended with aggregation fields.

That means:
- no second execution engine,
- no semantic fork,
- no separate “search rows API” and “stats API” internally.

A dedicated daemon RPC named `aggregate` may still exist as a **convenience alias**, but it should compile to the same canonical request model.

## 12.2 Proposed request shape

```rust
pub struct SearchParams {
    pub pattern: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub match_path: bool,

    pub predicates: Vec<SearchPredicate>,
    pub sorts: Vec<SearchSortSpec>,
    pub projection: Option<Vec<FieldId>>,
    pub response_mode: ResponseMode,

    pub aggregations: Vec<AggregateSpec>,
    pub include_rows: bool,              // false => aggregate-only
    pub row_limit: Option<u32>,          // rows only
    pub drives: Vec<char>,

    pub profile: Option<String>,         // optional preset expansion
}
```

## 12.3 Aggregate spec

```rust
pub struct AggregateSpec {
    pub id: String,
    pub kind: AggregateKind,
    pub domain: AggregateDomain,         // Matched | Page
    pub exactness: ExactnessMode,        // Auto | Exact | Approx
}
```

## 12.4 Aggregate kinds

```rust
pub enum AggregateKind {
    Count,
    Distinct { field: FieldId },
    Missing { field: FieldId },

    Stats {
        field: FieldId,
        metrics: Vec<ScalarMetric>,
    },

    Terms {
        fields: Vec<FieldId>,            // 1..N bucket keys
        top: Option<u32>,
        order: BucketOrder,
        metrics: Vec<BucketMetric>,
        sample: Option<TopHitsSpec>,
        facet_mode: FacetMode,           // Filtered | Disjunctive
    },

    Histogram {
        field: FieldId,
        interval: HistogramInterval,
        metrics: Vec<BucketMetric>,
    },

    DateHistogram {
        field: FieldId,
        interval: CalendarInterval,
        metrics: Vec<BucketMetric>,
    },

    Range {
        field: FieldId,
        ranges: Vec<RangeSpec>,
        metrics: Vec<BucketMetric>,
    },

    Rollup {
        field: RollupField,              // Path | Drive | Ancestor
        depth: u16,
        top: Option<u32>,
        metrics: Vec<BucketMetric>,
        sample: Option<TopHitsSpec>,
    },

    Duplicates {
        keys: Vec<FieldId>,
        verify: VerifyMode,
        top: Option<u32>,
        sample: Option<TopHitsSpec>,
    },

    Preset {
        name: AggregatePreset,
    },
}
```

## 12.5 Metrics

```rust
pub enum ScalarMetric {
    Count,
    FileCount,
    DirCount,
    Sum,
    Min,
    Max,
    Avg,
    MissingCount,
    DistinctCount,
}

pub enum BucketMetric {
    Count,
    FileCount,
    DirCount,
    Sum(FieldId),
    Min(FieldId),
    Max(FieldId),
    Avg(FieldId),
    DistinctCount(FieldId),
    WasteBytes,
    WastePct,
    ShareOfTotalCount,
    ShareOfTotalBytes,
}
```

## 12.6 Aggregate response shape

```rust
pub struct SearchResponse {
    pub rows: Option<Vec<StructuredRow>>,
    pub aggregations: Option<Vec<AggregateResult>>,
    pub total_matches: Option<u64>,
    pub response_mode: ResponseMode,
    pub execution_ms: u64,
    pub warnings: Vec<String>,
}
```

## 12.7 Aggregate result shape

```rust
pub struct AggregateResult {
    pub id: String,
    pub kind: AggregateKindDiscriminant,
    pub exact: bool,
    pub truncated: bool,
    pub other_count: Option<u64>,
    pub next_bucket_cursor: Option<String>,
    pub domain_count: u64,
    pub summary: Option<AggregateSummary>,
    pub buckets: Vec<AggregateBucket>,
}
```

## 12.8 Bucket shape

```rust
pub struct AggregateBucket {
    pub key: AggregateKey,
    pub count: u64,
    pub metrics: BTreeMap<String, serde_json::Value>,
    pub sample_rows: Option<Vec<StructuredRow>>,
    pub drilldown: Option<Vec<SearchPredicate>>,
}
```

---

## 13. CLI surface

## 13.1 Public design goals

The CLI should be:
- ergonomic for common cases,
- composable for power users,
- aligned with existing flag style,
- careful not to steal `--group-by`.

## 13.2 Recommended public flags

```text
AGGREGATION OPTIONS
  --aggregate <PRESET>          Run a preset aggregate plan
  --agg <SPEC>                  Repeatable power form
  --count                       Aggregate-only total count
  --facet <FIELD[:TOP]>         Terms bucket shorthand
  --stats <FIELD[:METRICS]>     Scalar stats shorthand
  --histogram <FIELD:INTERVAL>  Histogram shorthand
  --rows                        Include normal rows in addition to aggregations
```

## 13.3 Important CLI rules

1. If any aggregate flag is present and `--rows` is absent:
   - default to aggregate-only output.

2. `--limit` applies to rows, not the aggregate domain.

3. Bucket limits are controlled by:
   - `top=` inside `--agg`,
   - or the `:TOP` part of `--facet`.

4. `--group-by` remains reserved for row grouping / display grouping.
   - Do **not** reuse it for aggregation.
   - Use `terms:` or `rollup:` in `--agg`.

## 13.4 Convenience examples

```bash
# Total match count only
uffs '*' --count

# Overview preset
uffs '*' --aggregate overview

# Top file types
uffs '*' --facet type:20

# Extension stats
uffs '*' --facet ext:50

# File-size stats
uffs '*' --stats size:sum+avg+min+max

# Size histogram
uffs '*' --histogram size:100MB

# Aggregate + rows
uffs '*.jpg' --newer 30d --aggregate media --rows --limit 20

# Folder rollup
uffs '*' --agg 'rollup:path,depth=2,top=25,metrics=count+sum(size)+sum(sizeondisk)'

# Duplicate candidates
uffs '*' --agg 'duplicates:size+name,verify=none,top=100'
```

## 13.5 Power syntax for `--agg`

Recommended syntax:

```text
count
distinct:ext
missing:ext
stats:size,metrics=sum+avg+min+max
terms:type,top=20,metrics=count+sum(size)+share_bytes,sample=1
terms:drive+type,top=50,metrics=count+sum(sizeondisk)
hist:size,interval=100MB,metrics=count+sum(size)
datehist:modified,calendar=month,metrics=count+sum(size)
range:bulkiness,bins=0..100+100..200+200..500+500..
rollup:path,depth=2,top=25,metrics=count+sum(size)
duplicates:size+name,verify=sha256,top=100,sample=2
preset:cleanup
```

---

## 14. MCP design

## 14.1 Why MCP needs a first-class surface

LLM clients usually want:
- a compact answer,
- a structured object,
- drill-down handles,
- not a thousand file rows.

That means UFFS should expose dedicated aggregate tools.

## 14.2 Recommended MCP tools

### Tool 1: `uffs_aggregate`

Primary summary tool.

#### Input
- pattern
- predicates
- aggregations
- includeRows
- rowLimit
- sampleProjection
- exactness
- cursor

#### Output
- query summary
- matched domain size
- execution metadata
- aggregate results
- optional sample rows
- warnings
- next cursor
- drill-down handles

### Tool 2: `uffs_facet_values`

Facet-value search / autocomplete.

#### Input
- field
- prefix
- predicates
- top
- exactness

#### Output
- field
- matching facet values
- counts
- exactness
- next cursor

This is especially useful when a field has many possible values and the agent wants to refine interactively.

### Optional Tool 3 later: `uffs_duplicate_verify`

Longer-running verification step for duplicate groups.

## 14.3 MCP output design recommendations

The tool result should use:
- `structuredContent` for machine-readable data,
- `outputSchema` so clients know the exact shape,
- human-readable text only as a compact supplement,
- `resource_link` or drill-down payloads for bucket follow-up.

## 14.4 Long-running tasks

Deep duplicate verification or very large high-cardinality rollups may be long-running.
The MCP tasks draft/spec now supports task-based “call now, fetch later” flows.

UFFS should therefore:
- keep most hot aggregates synchronous,
- but be prepared to expose duplicate verification and other heavy scans through tasks,
- especially for MCP hosts that support task augmentation.

## 14.5 Example MCP aggregate result shape

```json
{
  "query": {
    "pattern": "*",
    "predicates": [{"field":"type","op":"eq","value":"video"}]
  },
  "domain": {
    "matchedCount": 18214,
    "exact": true,
    "executionMs": 38
  },
  "aggregations": [
    {
      "id": "by_drive",
      "kind": "terms",
      "field": "drive",
      "exact": true,
      "truncated": false,
      "buckets": [
        {
          "key": {"drive":"D"},
          "count": 10332,
          "metrics": {
            "sum_size": 912340120123,
            "share_bytes": 0.72
          },
          "sampleRows": [
            {"name":"movie1.mkv","size":7340032000,"path":"D:\\Media\\movie1.mkv"}
          ],
          "drilldown": [
            {"field":"drive","op":"eq","value":"D"},
            {"field":"type","op":"eq","value":"video"}
          ]
        }
      ]
    }
  ]
}
```

---

## 15. Field capability model

## 15.1 `AggregateMeta` — implemented (2026-04-06)

> **Status:** Implemented in `crates/uffs-core/src/search/field.rs`.
> All 39 `FieldId` variants have `AggregateMeta` populated. 7 invariant
> tests enforce correctness. Run `cargo test -- aggregate_capability_table
> --nocapture` to see the generated table.

Every canonical field declares aggregation behavior via `AggregateMeta`,
embedded in the existing `FieldMeta` struct:

```rust
/// Cardinality hint for aggregation planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cardinality {
    Fixed,      // ≤26 values — use array-indexed accumulator (drive, bool, type)
    Low,        // ≤~100 — small HashMap (semantic type/FileCategory)
    Medium,     // ≤~10 000 — HashMap (file extensions)
    High,       // ≤~1 000 000 — guard with top-N + other_count (folder paths)
    Unbounded,  // millions — only aggregate on explicit request (filenames, paths)
}

/// Aggregation capability for a single FieldId.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AggregateMeta {
    pub aggregatable: bool,       // sum/min/max/avg (numeric + timestamp)
    pub groupable: bool,          // terms / group-by key (enum, string, bool)
    pub bucket_support: bool,     // histogram / range buckets (numeric + timestamp)
    pub cardinality: Cardinality, // expected distinct-value count
    pub default_top: u16,         // default top-N for terms (0 = not terms-suitable)
}
```

**Design simplification from original proposal:** The 8-field design in the
original draft (`stats_support`, `bucket_support` as enums, `default_order`,
`cost_tier`) was reduced to 5 fields:
- `stats_support` → subsumed by `aggregatable` (if aggregatable, all stats apply)
- `bucket_support` → simplified to `bool` (bucket configuration is on the spec, not the field)
- `default_order` → derivable (numeric→desc by metric, string→asc by key)
- `cost_tier` → already captured by `FieldMeta.access` (`FieldAccess::Hot`/`Derived`)

### Current field capability summary

Generated from code — **11 aggregatable, 24 groupable, 11 bucketable**:

| Category | Fields | Agg | Group | Bucket | Cardinality |
|----------|--------|-----|-------|--------|-------------|
| Size/storage | size, size_on_disk, tree_size, tree_allocated, bulkiness | ✅ | — | ✅ | Unbounded |
| Timestamps | created, modified, accessed | ✅ | — | ✅ | Unbounded |
| Length | name_length, path_length | ✅ | — | ✅ | Unbounded |
| Structure | descendants | ✅ | — | ✅ | Unbounded |
| Enum keys | drive (Fixed/26), type (Low/30) | — | ✅ | — | Fixed/Low |
| String keys | extension (Medium/50), name (Unbounded/100), path_only (Unbounded/30) | — | ✅ | — | Medium–Unbounded |
| Bool attrs | 21 fields (hidden…recall_on_data_access, directory) | — | ✅ | — | Fixed/2 |
| Inert | path, attributes, attribute_value, parity_attributes | — | — | — | — |

## 15.2 Why this matters

Aggregation validity and planning comes from metadata, not hardcoded match arms.

The aggregation engine queries `field.metadata().aggregate` to decide:
- Whether a field can be a group-by key (`groupable`)
- Whether sum/min/max/avg are valid (`aggregatable`)
- Whether histogram/range bucketing applies (`bucket_support`)
- What accumulator strategy to use (`cardinality`: Fixed→array, Low/Medium→HashMap, High/Unbounded→guarded HashMap)
- Default result limit (`default_top`)

Adding a new `FieldId` variant requires populating `AggregateMeta` — the
7 invariant tests enforce consistency (Bool→groupable+Fixed, Numeric→aggregatable+bucketable, etc.).

## 15.3 Cost tiers

Aggregation planning uses `FieldMeta.access` (the existing `FieldAccess` enum)
rather than a separate cost tier:

| Access | Aggregation cost | Notes |
|--------|-----------------|-------|
| `Hot` | O(1)/record | Direct `CompactRecord` field access |
| `Derived` | O(1)–O(d)/record | Computed from hot data; most are O(1) except path (O(depth)) |

All 39 implemented fields are Hot or Derived. The 17 planned cold-path fields
(Wave 5) will introduce a genuine `Cold` tier requiring `ExtraRecordFields`
or MFT re-reads. When those are added, `FieldAccess::Cold` will serve as the
aggregation cost signal — no separate `AggregateCostTier` enum needed.

**Deep operations** (path chain walk, duplicate hash verification) are not
field-level costs — they are *aggregation-kind* costs handled by the
`AggregateSpec` planner (§17), not by `AggregateMeta`.

---

## 16. Facet modes

Facets are not just counts. They have modes.

## 16.1 Filtered facet mode (default)

Compute buckets on the fully filtered matched set.

This is the simplest mode and should ship first.

## 16.2 Disjunctive facet mode (later)

For a facet field currently selected in filters:
- recompute that facet excluding its own constraint,
- so users/agents can still see alternative values.

This is important for rich UI/MCP refinement flows and is a known faceted-search pattern.
It should be deferred until the base system is stable.

---

## 17. Execution architecture

## 17.1 Overview

Aggregation execution should be planned in four phases.

```text
1. Compile
2. Scan / accumulate
3. Deep follow-up work
4. Finalize / shape response
```

## 17.2 Phase 1: compile

Tasks:
- parse presets and convenience flags into canonical `AggregateSpec`s
- validate against `FieldMeta` + `AggregateMeta`
- normalize bucket ordering and defaults
- split work into:
  - hot aggregate sinks,
  - derived aggregate sinks,
  - deep follow-up actions

### Output of this phase
An `AggregatePlan`.

```rust
pub struct AggregatePlan {
    pub hot: Vec<HotAggPlan>,
    pub derived: Vec<DerivedAggPlan>,
    pub deep: Vec<DeepAggPlan>,
    pub row_samples: Vec<RowSamplePlan>,
}
```

## 17.3 Phase 2: scan / accumulate

Tasks:
- run pattern + predicates exactly as search already does
- for each matching record:
  - feed hot accumulators,
  - record minimal data needed for derived/deep steps,
  - optionally feed tiny per-bucket sample heaps

Important:
- if rows are not requested, do not build `DisplayRow`s.
- if path is not needed, do not resolve full paths.
- if duplicate verification is not requested, do not hash content.

## 17.4 Phase 3: deep follow-up work

This phase is only for aggregates that need it.

Examples:
- path rollups,
- high-cardinality path buckets,
- sample row materialization,
- duplicate verification,
- optional future deep/cold fields.

## 17.5 Phase 4: finalize

Tasks:
- compute derived metrics like averages, share-of-total, waste%
- sort buckets
- trim to `top`
- compute `otherCount`
- attach `exact`, `truncated`, `nextBucketCursor`
- render structured response

---

## 18. Accumulator strategies

The implementation should choose accumulator shape by field/cardinality.

## 18.1 Fixed-array accumulators

Use fixed arrays for low-cardinality hot fields.

| Field | Strategy |
|---|---|
| `drive` | 26-slot array or compact observed-drive map |
| `type` | fixed category array |
| bool attrs | 2-slot counters or bit counters |
| month | 12-slot array |
| quarter | 4-slot array |
| file-vs-dir | 2-slot array |

## 18.2 ID-keyed maps

Use numeric IDs where available.

| Field | Key |
|---|---|
| `ext` | `extension_id` |
| `type` | category enum ID |
| drive | drive enum/index |

Resolve human strings only when finalizing output.

## 18.3 Numeric histograms

Use fixed bins for common numeric histograms.
Avoid generic dynamic buckets unless explicitly requested.

## 18.4 Path rollups

Do **not** aggregate on full path strings during the hot scan if ancestor IDs are available.

Preferred flow:
1. group by folder/ancestor record ID,
2. merge totals by ID,
3. resolve display path strings only for surviving top buckets.

## 18.5 Duplicate groups

Stage them:

1. candidate grouping on cheap keys,
2. discard singletons,
3. optional deeper verification,
4. finalize top groups and reclaimable-byte metrics.

---

## 19. Bucket ordering, truncation, and pagination

## 19.1 Bucket order

Recommended default ordering:
- count desc for facets,
- byte sum desc for storage-oriented presets,
- chronological asc for date histograms unless explicitly overridden,
- path/value asc when order is semantic rather than rank-oriented.

## 19.2 Truncation metadata

Every bucketed response should expose:
- `truncated`
- `returnedBucketCount`
- `otherCount`
- `nextBucketCursor` when more buckets exist

## 19.3 Large bucket spaces

For high-cardinality fields like:
- `path_only`
- `name`
- sometimes `ext`
- possible future owner/security IDs

support cursor-based bucket pagination rather than only top-N truncation.

This is the right analogue of composite aggregation pagination.

## 19.4 Exactness metadata

Every aggregate result should include:
- `exact: true|false`
- `valuesComplete: true|false`
- `approximation: Option<ApproximationMeta>`

Recommended interpretation:
- low-cardinality fields default to exact,
- exact distinct counts for small/medium cardinality,
- approximate distinct counts only when the caller allows it,
- never silently approximate.

---

## 20. Sample rows and drill-down behavior

## 20.1 Sample rows are important

Each bucket should optionally return 1–3 sample rows.
This is useful for:
- human inspection,
- MCP/LLM actionability,
- quick validation that the bucket “makes sense”.

## 20.2 Sample row projection

Sample rows should use a compact projection by default:
- name
- size
- modified
- path
- type
- ext

Allow callers to override.

## 20.3 Drill-down

Every bucket should optionally carry a **drill-down predicate patch**:
- the current query predicates,
- plus the bucket key predicate(s).

This makes follow-up row retrieval trivial.

---

## 21. Duplicate analytics architecture

Duplicate functionality deserves its own design section because it is so important in this domain.

## 21.1 Stages

### Stage A — candidate grouping
Cheap grouping by selected keys:
- `size + name` (good default)
- `name`
- `size`
- `size + ext`
- `size + modified + name`

### Stage B — reduce
Drop groups with count <= 1.

### Stage C — verify (optional)
Verification modes:
- `none`
- `first_bytes`
- `sha256`
- `bytewise`

### Stage D — finalize
Compute:
- candidate groups
- verified groups
- reclaimable bytes
- sample rows
- recommended deletion order hints later

## 21.2 Defaults

Recommended default duplicate preset:
- keys = `size + name`
- verify = `none`
- sample = 2
- top = 100 by reclaimable bytes

## 21.3 Heavy-work controls

Add:
- `max_groups`
- `max_records_to_verify`
- `verification_budget`
- `task_mode` for MCP

---

## 22. Relationship to existing UFFS concepts

## 22.1 `type` collections and semantic categories

Aggregation must reuse the same semantic category system already visible in CLI filters and docs.

## 22.2 `bulkiness`

Do not treat `bulkiness` as row-only.
It should become a first-class histogram/range/summary dimension.

## 22.3 Tree metrics

Both row-level tree metrics and matched-set rollups should be supported, but clearly distinguished.

## 22.4 Filters and sorts

All existing filters remain valid.
Aggregations always run on the post-filter domain.

Sorts affect:
- row output,
- per-bucket sample rows,
- optionally bucket order if explicitly configured.

They do **not** redefine the aggregate domain unless `domain=page` is requested.

---

## 23. Output modes

## 23.1 JSON / MCP structured output

Primary mode for programmatic clients.

## 23.2 Table output

Human-friendly for CLI.
Should show:
- summary,
- then bucket rows,
- then optional samples.

## 23.3 CSV output

Useful mainly for flat bucket tables.
For nested aggregates, prefer JSON.

## 23.4 Row + aggregate mixed output

Allowed, but:
- rows and aggregations should be clearly separated,
- not interleaved.

---

## 24. Suggested module layout

No new crate is required.

```text
crates/
├── uffs-core/src/
│   ├── aggregate/
│   │   ├── mod.rs
│   │   ├── spec.rs          # AggregateSpec, enums, parser helpers
│   │   ├── planner.rs       # AggregatePlan
│   │   ├── accumulators.rs  # hot/derived accumulator types
│   │   ├── buckets.rs       # histogram/range/date bucket helpers
│   │   ├── rollup.rs        # path/ancestor rollup helpers
│   │   ├── duplicates.rs    # duplicate grouping + verification orchestration
│   │   ├── presets.rs       # preset expansions
│   │   ├── finalize.rs      # ordering, truncation, exactness, cursors
│   │   └── render.rs        # logical shaping if needed
│   ├── search/
│   │   ├── field.rs         # FieldId, FieldMeta, AggregateMeta
│   │   ├── derived.rs
│   │   └── ...
│
├── uffs-client/src/
│   └── protocol.rs          # SearchParams/SearchResponse extensions
│
├── uffs-daemon/src/
│   ├── index.rs             # aggregate execution entry points
│   └── handler.rs           # RPC dispatch
│
├── uffs-cli/src/
│   └── commands/
│       └── search/          # aggregate flags -> SearchParams
│
└── uffs-mcp/src/
    └── main.rs              # uffs_aggregate, uffs_facet_values, tasks
```

---

## 25. Performance goals

These are design targets, not measured promises.

## 25.1 Goals

1. Aggregate-only hot-field queries should avoid row materialization entirely.
2. `terms:type`, `terms:drive`, and bool-flag summaries should be near the cost of a normal filtered scan.
3. `terms:ext` should avoid string allocation during accumulation.
4. Rollups should resolve full path strings only for final buckets.
5. Duplicate verification should only touch reduced candidate groups.
6. Hot-only queries must not pay any deep-path penalty.

## 25.2 Optional later cache

A daemon-local aggregate result cache is a good future optimization for:
- hot aggregate-only requests,
- repeated overview presets,
- stable index epochs.

Cache key:
- normalized request,
- index epoch / cache generation,
- response mode-independent core result.

This is optional and should not block v1.

---

## 26. Testing and validation

## 26.1 Unit tests

Add unit coverage for:
- aggregate spec parsing
- preset expansion
- aggregate metadata validity
- histogram boundary behavior
- date bucket behavior
- rollup semantics
- waste metrics
- share-of-total calculations
- duplicate stage transitions
- exactness/truncation flags

## 26.2 Synthetic integration tests

Build synthetic compact indexes with known distributions and verify:
- counts
- bucket membership
- nested metrics
- rollup totals
- duplicate grouping
- sample row projection
- cursor pagination

## 26.3 Live Windows validation

Add aggregate suites to the existing Windows validation harness.

### Suggested suites
- `A100` summary counts
- `A110` by_type facet correctness
- `A120` by_extension facet correctness
- `A130` size histogram correctness
- `A140` age histogram correctness
- `A150` path rollup correctness
- `A160` hidden/system/compressed counts
- `A170` long-path and long-name distributions
- `A180` duplicate candidate correctness
- `A190` verified duplicate correctness (on controlled fixture)
- `A200` aggregate + rows mixed-mode parity
- `A210` MCP schema validation
- `A220` no-row-materialization regression guard for aggregate-only queries

## 26.4 Performance regression rules

Hot-only aggregate queries must not regress because aggregation exists.
Specifically:
- no path resolution when not required,
- no row building when not requested,
- no deep loads unless required,
- no string inflation for extension/type buckets during scan.

---

## 27. Rollout plan

## Stage 0 — reconcile field/access truth
Before feature coding:
- generate aggregate capability table from code,
- ~~settle the 35 vs 52/55 field drift~~ — **resolved** (§5.1): 39 implemented + 17 planned = 56,
- ~~settle the hot/derived/deep truth~~ — **resolved** (§5.2): all 39 implemented fields are hot/derived.

## Stage 1 — hot aggregate core
Ship:
- `--count`
- `--aggregate overview`
- `--facet type|ext|drive|bool`
- `--stats size|sizeondisk|modified`
- `--histogram size`
- MCP `uffs_aggregate` basic mode

## Stage 2 — bucket metrics + samples + presets
Ship:
- per-bucket metrics
- sample rows
- `storage`, `activity`, `media`, `cleanup`
- basic rollups by drive and path depth 1/2

## Stage 3 — rollups + pagination + facet values
Ship:
- cursor-based bucket pagination
- `uffs_facet_values`
- hierarchical/path rollups
- exactness/truncation metadata finalized

## Stage 4 — duplicate analytics
Ship:
- candidate duplicate groups
- reclaimable bytes
- sample rows
- optional hash verification
- MCP task-mode for long runs

## Stage 5 — advanced and forensic
Ship only after field model is stable:
- percentiles
- namespace/reparse/security/forensic buckets
- disjunctive facets
- advanced pipeline metrics

---

## 28. Specific decisions adopted from this consolidation

### 28.1 Adopt
- one daemon-owned aggregate engine
- aggregate support inside canonical search request
- optional daemon `aggregate` alias
- aggregate domain defaults to matched set
- `--group-by` stays reserved
- presets + `--agg` power form
- MCP-first structured responses
- explicit exactness/truncation
- path rollups as first-class
- staged duplicate model
- sample rows per bucket

### 28.2 Reject
- separate hand-maintained aggregate field matrix
- aggregate-only implementation on top of `DisplayRow`
- row-pagination-defined aggregates by default
- SQL-style query layer
- silent approximation
- overloading existing row-grouping terminology

---

## 29. Open questions that remain worth deciding explicitly

1. Should daemon-native `aggregate` be a convenience alias over `SearchParams`, or should only `search` exist on the wire with `aggregations` populated?
2. Should `uffs stats` remain user-visible long term, or become hidden/aliased to `--aggregate`?
3. Should the first release include any approximate distinct-count mode, or stay exact-only until pressure appears?
4. How much rollup nesting should be allowed in v1 before response size becomes unwieldy?
5. Should `facet_values` support fuzzy/prefix search immediately, or exact prefix first?
6. Should disjunctive facets wait for GUI/TUI demand, or should MCP get them earlier?

---

## 30. Bottom line

The right design is:

- **not** a second “stats” island,
- **not** a formatter bolted onto rows,
- **not** SQL.

It is a **first-class aggregate response path inside the same daemon-owned search contract**.

If implemented this way, UFFS gets:
- CLI summaries that feel natural,
- MCP outputs that are compact and agent-friendly,
- storage and cleanup workflows that are genuinely powerful,
- and an architecture that stays consistent with the rest of the daemon/search consolidation work.

This is the design that best fits the current UFFS direction **and** the expectations set by modern search systems and real file-search users.

---

## Appendix A — Example aggregate presets expanded

### `overview`
```text
count
files_vs_dirs
sum(size)
sum(sizeondisk)
terms:type,top=12,metrics=count+sum(size)
terms:drive,top=10,metrics=count+sum(size)
datehist:modified,calendar=month,metrics=count
```

### `storage`
```text
sum(size)
sum(sizeondisk)
stats:size,metrics=sum+avg+max
stats:sizeondisk,metrics=sum+avg+max
terms:type,top=20,metrics=count+sum(size)+sum(sizeondisk)+waste_bytes
terms:ext,top=50,metrics=count+sum(size)
rollup:path,depth=2,top=25,metrics=count+sum(size)+sum(sizeondisk)
hist:size,interval=100MB,metrics=count+sum(size)
```

### `cleanup`
```text
count
missing:ext
range:size,bins=0..0+1..1024+1024..1048576+1048576..
range:pathlength,bins=0..127+128..199+200..239+240..259+260..
range:bulkiness,bins=0..100+100..200+200..500+500..
terms:hidden,metrics=count
terms:compressed,metrics=count+sum(sizeondisk)
terms:encrypted,metrics=count
datehist:modified,calendar=year,metrics=count+sum(size)
```

### `duplicates`
```text
duplicates:size+name,verify=none,top=100,sample=2
```

---

## Appendix B — Example MCP tools

### `uffs_aggregate` tool descriptor sketch

```json
{
  "name": "uffs_aggregate",
  "title": "Summarize filesystem results",
  "description": "Run server-side aggregations over UFFS search results. Use this when you need counts, storage breakdowns, histograms, folder rollups, or duplicate summaries instead of raw file rows.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "pattern": {"type": "string", "default": "*"},
      "predicates": {"type": "array"},
      "aggregations": {"type": "array"},
      "profile": {"type": "string"},
      "includeRows": {"type": "boolean", "default": false},
      "rowLimit": {"type": "integer", "default": 0},
      "exactness": {"type": "string", "enum": ["auto", "exact", "approx"], "default": "auto"},
      "cursor": {"type": "string"}
    }
  },
  "outputSchema": {
    "type": "object",
    "properties": {
      "domain": {"type": "object"},
      "aggregations": {"type": "array"},
      "warnings": {"type": "array"},
      "nextBucketCursor": {"type": ["string", "null"]}
    }
  }
}
```

### `uffs_facet_values` tool descriptor sketch

```json
{
  "name": "uffs_facet_values",
  "title": "Search within facet values",
  "description": "Search for candidate facet values and counts inside the current UFFS query scope. Useful for large extension, path, or owner-like value spaces.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "field": {"type": "string"},
      "prefix": {"type": "string"},
      "predicates": {"type": "array"},
      "top": {"type": "integer", "default": 20},
      "cursor": {"type": "string"}
    },
    "required": ["field"]
  }
}
```

---

## Appendix C — Internal docs that should be updated after implementation

When the feature lands, update at least:

- `docs/user-manual/cli-overview.md`
- `docs/user-manual/filters.md`
- `docs/user-manual/sorting.md`
- `docs/user-manual/search-modes.md`
- `docs/architecture/FILTER_SORT_FEATURE_MATRIX.md`
- `docs/architecture/UNIFIED_DAEMON_SEARCH_GAP_IMPLEMENTATION_PLAN.md`

Also add:
- `docs/user-manual/aggregations.md`
- examples for CLI, daemon, and MCP

---


## Appendix D — Internal source docs synthesized

- `AGGREGATION_ARCHITECTURE.md`
- `FILTER_SORT_FEATURE_MATRIX.md`
- `FILTER_SORT_GAPS.md`
- `UNIFIED_DAEMON_SEARCH_GAP_IMPLEMENTATION_PLAN.md`
- `SEARCH_PIPELINE_REFACTOR.md`
- `mft_search_defaults_report.md`
- `cli-overview.md`
- `filters.md`
- `search-modes.md`
- `sorting.md`

## Appendix E — External references

### Search / aggregation systems
- Elasticsearch aggregations: https://www.elastic.co/guide/en/elasticsearch/reference/current/search-aggregations.html
- Elasticsearch composite aggregation: https://www.elastic.co/docs/reference/aggregations/search-aggregations-bucket-composite-aggregation
- Elasticsearch top hits aggregation: https://www.elastic.co/docs/reference/aggregations/search-aggregations-metrics-top-hits-aggregation
- Apache Solr JSON Facet API: https://solr.apache.org/guide/solr/latest/query-guide/json-facet-api.html
- Azure AI Search faceted navigation examples: https://learn.microsoft.com/en-us/azure/search/search-faceted-navigation-examples
- Azure AI Search facets overview: https://learn.microsoft.com/en-us/azure/search/search-faceted-navigation
- Meilisearch facets guide: https://www.meilisearch.com/docs/capabilities/filtering_sorting_faceting/how_to/filter_with_facets
- Meilisearch facet search: https://www.meilisearch.com/docs/reference/api/facet-search/search-in-facets
- Typesense search API: https://typesense.org/docs/latest/api/search.html
- Algolia faceting guide: https://www.algolia.com/doc/guides/managing-results/refine-results/faceting
- Algolia `facets` parameter: https://www.algolia.com/doc/api-reference/api-parameters/facets
- Algolia search for facet values: https://www.algolia.com/doc/rest-api/search/search-for-facet-values

### File-search tools and community evidence
- SearchMyFiles utility: https://www.nirsoft.net/utils/search_my_files.html
- SearchMyFiles duplicate-file article: https://www.nirsoft.net/articles/find_duplicate_files.html
- SearchMyFiles summary-mode folder report article: https://www.nirsoft.net/articles/folder-size-summary-report.html
- Everything forum: find duplicates: https://www.voidtools.com/forum/viewtopic.php?t=12733
- Everything forum: duplicate view scoped to current results: https://voidtools.com/forum/viewtopic.php?t=11014
- Listary discussion: current-folder search demand: https://discussion.listary.com/t/search-within-the-current-folder/8268
- MacRumors discussion: search outside default index / hidden remnants: https://forums.macrumors.com/threads/easyfind-versus-find-any-file.2446693/
- MacRumors discussion: NAS / hidden-file search gaps: https://forums.macrumors.com/threads/i-cant-seem-to-be-able-to-search-for-files-on-my-mounted-nas-drives.2356566/

### MCP
- MCP tools spec: https://modelcontextprotocol.io/specification/draft/server/tools
- MCP schema reference: https://modelcontextprotocol.io/specification/2025-11-25/schema
- MCP tasks draft/spec: https://modelcontextprotocol.io/specification/draft/basic/utilities/tasks
- MCP SEP-1686 tasks: https://modelcontextprotocol.io/seps/1686-tasks
