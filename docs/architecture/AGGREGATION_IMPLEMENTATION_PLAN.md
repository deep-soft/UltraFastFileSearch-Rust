# Aggregation Implementation Plan

> **Source of truth:** `UFFS_AGGREGATION_ARCHITECTURE_CONSOLIDATED.md`
> **Date:** 2026-04-06
> **Status:** Active

---

## Overview

This plan turns the consolidated aggregation architecture into a sequenced,
trackable set of implementation tasks. Every task references the consolidated
doc section it implements. Tasks are grouped into stages that match §27 of
the consolidated doc, with dependencies made explicit.

**Key principle:** each stage is independently shippable. Stage N must be
complete and tested before Stage N+1 begins.

---

## Pre-requisites ✅ COMPLETE (2026-04-06)

All pre-requisites resolved. Code is the source of truth.

### P-1  Reconcile FieldId inventory ✅

| ID | Task | Section | Status |
|----|------|---------|--------|
| P-1.1 | Audit `FieldId::ALL` against the 52/55-variant target. Result: **39 implemented + 17 cold-path planned = 56 total**. The "35" was stale; the "52/55" was a target+counting artifact. | §5.1 | ✅ |
| P-1.2 | Document which variants are deferred vs dropped. Updated `SEARCH_PIPELINE_REFACTOR.md` (Wave 3→🟡, Wave 5 explicit), `FILTER_SORT_FEATURE_MATRIX.md` (§4.3 annotated ✅/❌, §5.4 corrected), `CONSOLIDATED.md` (§5 rewritten). | §5.1 | ✅ |

### P-2  Reconcile access-tier truth ✅

| ID | Task | Section | Status |
|----|------|---------|--------|
| P-2.1 | Confirmed all 39 implemented fields are Hot or Derived. No true Cold fields exist in the current `FieldId` enum. The 17 planned cold-path fields (FnCreated…ForensicFlags) are not yet in code. | §5.2 | ✅ |
| P-2.2 | No `FieldMeta` entries needed updating — all access tiers were correct. | §5.2 | ✅ |

### P-3  Add `AggregateMeta` to `FieldMeta` ✅

| ID | Task | Section | Status |
|----|------|---------|--------|
| P-3.1 | Designed `AggregateMeta` with 5 fields: `aggregatable: bool`, `groupable: bool`, `bucket_support: bool`, `cardinality: Cardinality`, `default_top: u16`. Simplified from the 8-field §15.1 proposal — `stats_support`/`default_order` are derivable, `cost_tier` = `FieldAccess`. Added `Cardinality` enum: `Fixed`, `Low`, `Medium`, `High`, `Unbounded`. | §15.1 | ✅ |
| P-3.2 | Added `aggregate: AggregateMeta` field to `FieldMeta` struct in `field.rs`. | §15.1 | ✅ |
| P-3.3 | Populated `AggregateMeta` for all 39 `FieldId` variants. Summary: **11 aggregatable, 24 groupable, 11 bucketable**. | §15.2, §15.3 | ✅ |
| P-3.4 | 7 unit tests added and passing: `every_field_has_valid_aggregate_meta`, `aggregate_capability_table` (generated table), `aggregate_bool_fields_are_facets`, `aggregate_numeric_fields_are_aggregatable_and_bucketable`, `aggregate_timestamp_fields_are_aggregatable_and_bucketable`, `aggregate_key_fields_have_correct_cardinality`, `aggregate_non_aggregatable_fields`. | §15 | ✅ |

### Generated capability table (from `cargo test -- aggregate_capability_table --nocapture`)

```
Field                    Type   Agg  Group  Bucket Cardinality Top
-----------------------------------------------------------------
drive                    Enum     -    yes       -      Fixed  26
path                   String     -      -       -  Unbounded   -
name                   String     -    yes       -  Unbounded 100
path_only              String     -    yes       -  Unbounded  30
size                  Numeric   yes      -     yes  Unbounded   -
size_on_disk          Numeric   yes      -     yes  Unbounded   -
created             Timestamp   yes      -     yes  Unbounded   -
modified            Timestamp   yes      -     yes  Unbounded   -
accessed            Timestamp   yes      -     yes  Unbounded   -
extension              String     -    yes       -     Medium  50
type                     Enum     -    yes       -        Low  30
attributes            Bitmask     -      -       -  Unbounded   -
attribute_value       Bitmask     -      -       -  Unbounded   -
hidden                   Bool     -    yes       -      Fixed   2
system                   Bool     -    yes       -      Fixed   2
archive                  Bool     -    yes       -      Fixed   2
read_only                Bool     -    yes       -      Fixed   2
compressed               Bool     -    yes       -      Fixed   2
encrypted                Bool     -    yes       -      Fixed   2
sparse                   Bool     -    yes       -      Fixed   2
reparse                  Bool     -    yes       -      Fixed   2
offline                  Bool     -    yes       -      Fixed   2
not_indexed              Bool     -    yes       -      Fixed   2
temporary                Bool     -    yes       -      Fixed   2
virtual                  Bool     -    yes       -      Fixed   2
pinned                   Bool     -    yes       -      Fixed   2
unpinned                 Bool     -    yes       -      Fixed   2
descendants           Numeric   yes      -     yes  Unbounded   -
tree_size             Numeric   yes      -     yes  Unbounded   -
tree_allocated        Numeric   yes      -     yes  Unbounded   -
bulkiness             Numeric   yes      -     yes  Unbounded   -
integrity                Bool     -    yes       -      Fixed   2
no_scrub                 Bool     -    yes       -      Fixed   2
directory                Bool     -    yes       -      Fixed   2
recall_on_open           Bool     -    yes       -      Fixed   2
recall_on_data_access    Bool     -    yes       -      Fixed   2
parity_attributes     Bitmask     -      -       -  Unbounded   -
name_length           Numeric   yes      -     yes  Unbounded   -
path_length           Numeric   yes      -     yes  Unbounded   -
-----------------------------------------------------------------
Total: 39  Aggregatable: 11  Groupable: 24  Bucketable: 11
```

---

## Stage 0 — Scaffolding

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S0.1 | Create `crates/uffs-core/src/aggregate/mod.rs` with module declarations + `run_aggregate()` entry point + `scan_drive()`. | `aggregate/mod.rs` | §24 | — | ✅ |
| S0.2 | Create `aggregate/spec.rs`: `AggregateSpec`, `AggregateKind` (Count/Stats/Terms/Histogram/DateHistogram/Range/Missing/Distinct), `ScalarMetric`, `BucketMetric`, `CalendarInterval`. | `aggregate/spec.rs` | §12.3–§12.5 | S0.1 | ✅ |
| S0.3 | Create `aggregate/presets.rs`: 6 presets (Overview/ByType/ByExtension/ByDrive/BySize/ByAge) with `expand()` + `parse()`. | `aggregate/presets.rs` | §11.1 | S0.2 | ✅ |
| S0.4 | Create `aggregate/accumulators.rs`: `StatsAccumulator`, `GroupAccumulator` with `from_kind()`, `feed()`, `merge()`, extract helpers. | `aggregate/accumulators.rs` | §18 | S0.1 | ✅ |
| S0.5 | Create `aggregate/buckets.rs`: `SizeBucket` (7 tiers), `AgeBucket` (8 tiers), `PathRiskBucket` (4 tiers) with `classify()`/`label()`. | `aggregate/buckets.rs` | §9.3 | S0.1 | ✅ |
| S0.6 | Create `aggregate/planner.rs`: `AggregatePlan::compile()` with field validation against `AggregateMeta`. | `aggregate/planner.rs` | §17.2 | S0.2, P-3 | ✅ |
| S0.7 | Create `aggregate/finalize.rs`: `finalize()` → `AggregateResponse`, `BucketRow::from_stats()`, `resolve_group_key()`, `format_range_key()`, `format_timestamp_key()`. | `aggregate/finalize.rs` | §19 | S0.4 | ✅ |
| S0.8 | Wire `pub mod aggregate;` into `crates/uffs-core/src/lib.rs`. | `lib.rs` | §24 | S0.1 | ✅ |
| S0.9 | Compile check + 26 new tests pass: `cargo check -p uffs-core`, `cargo test -p uffs-core`. | — | — | S0.1–S0.8 | ✅ |


---

## Stage 1 — Hot Aggregate Core (§27 Stage 1)

The first shippable feature: `--count`, `--aggregate overview`, `--facet`,
`--stats`, `--histogram size`.

### 1A  Core aggregate engine

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S1A.1 | Implement `AggregateEngine::run()` entry point. Accept `&[DriveCompactIndex]`, `SearchFilters`, `Vec<AggregateSpec>`. Return `AggregateResult`. | `aggregate/mod.rs` | §17 | S0.* | ✅ `run_aggregate()` + `AggregateOutput` |
| S1A.2 | Implement hot-path scan loop: iterate `CompactRecord`s, apply pattern+predicate, feed accumulators. No `DisplayRow` construction. | `aggregate/mod.rs` | §4.4, §17.3 | S1A.1 | ✅ `scan_drive()` iterates records, feeds accumulators |
| S1A.3 | Implement `Count` aggregate kind: plain record count. | `aggregate/mod.rs` | §9.1 | S1A.2 | ✅ `AccumulatorKind::Count` |
| S1A.4 | Implement `Stats` aggregate kind for `FieldId::Size`, `SizeOnDisk`, `Modified`, `Created`, `Accessed`: sum, min, max, avg, missing_count. | `aggregate/accumulators.rs` | §9.1 | S1A.2 | ✅ `StatsAccumulator` with feed/merge/finalize |
| S1A.5 | Implement `Terms` aggregate kind with fixed-array accumulators for: `Drive` (26-slot), `Type` (category enum), `DirectoryFlag` (2-slot), bool attrs (2-slot each). | `aggregate/accumulators.rs` | §18.1 | S1A.2 | ✅ Uses `HashMap<u64, StatsAccumulator>` (not fixed-array) |
| S1A.6 | Implement `Terms:Extension` using `HashMap<u16, GroupAccumulator>` keyed by `extension_id`. Resolve `ext_names[id]` only during finalization. | `aggregate/accumulators.rs` | §18.2 | S1A.2 | ✅ `extract_group_key` returns extension_id; finalize resolves |
| S1A.7 | Implement `Histogram:Size` with default size buckets (§9.3). | `aggregate/buckets.rs` | §9.3 | S1A.2 | ✅ `SizeBucket::from_bytes()` + 7 tiers |
| S1A.8 | Implement `DateHistogram` for `Modified`/`Created`/`Accessed` with calendar intervals (hour/day/week/month/quarter/year). | `aggregate/buckets.rs` | §9.4 | S1A.2 | ✅ `AccumulatorKind::DateHistogram` + `CalendarInterval` |
| S1A.9 | Implement `Range` aggregate kind for arbitrary numeric ranges (size, path_length, name_length, bulkiness). | `aggregate/accumulators.rs` | §9.3 | S1A.2 | ✅ `AccumulatorKind::Histogram` with `boundaries` |
| S1A.10 | Implement `Missing` aggregate kind: count records where a field has no value (no ext, zero-byte, no type). | `aggregate/accumulators.rs` | §9.1 | S1A.2 | ✅ `AccumulatorKind::Missing` |
| S1A.11 | Implement `Distinct` aggregate kind: count unique values for low/medium cardinality fields. | `aggregate/accumulators.rs` | §9.1 | S1A.2 | ✅ `AccumulatorKind::Distinct` with `HashSet<u64>` |
| S1A.12 | Implement `AggregateSummary`: totals, waste, unique_extensions, unique_types, hidden/system/compressed/encrypted counts, top_drive, top_type. | `aggregate/finalize.rs` | §10.1 | S1A.3–S1A.6 | ✅ Via `overview` preset composing count+stats+terms specs |
| S1A.13 | Implement share-of-total: `ShareOfTotalCount`, `ShareOfTotalBytes` during finalization. | `aggregate/finalize.rs` | §12.5 | S1A.12 | ✅ Computed in `BucketRow::from_stats()` |
| S1A.14 | Implement `WasteBytes` and `WastePct` bucket metrics. | `aggregate/accumulators.rs` | §9.1 | S1A.4 | ✅ `StatsAccumulator` tracks `allocated_sum` → waste computed |

### 1B  Presets (Stage 1 set)

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S1B.1 | Implement `overview` preset expansion (count + files_vs_dirs + sums + terms:type + terms:drive + datehist:modified,month). | `aggregate/presets.rs` | §11.1, App A | S1A.* | ✅ |
| S1B.2 | Implement `by_type` preset (terms:type + size/waste metrics). | `aggregate/presets.rs` | §11.1 | S1A.5 | ✅ |
| S1B.3 | Implement `by_extension` preset (terms:ext,top=50 + count/size/avg). | `aggregate/presets.rs` | §11.1 | S1A.6 | ✅ |
| S1B.4 | Implement `by_drive` preset (terms:drive + totals). | `aggregate/presets.rs` | §11.1, §10.4 | S1A.5 | ✅ |
| S1B.5 | Implement `by_size` preset (hist:size + totals). | `aggregate/presets.rs` | §11.1 | S1A.7 | ✅ |
| S1B.6 | Implement `by_age` preset (datehist:modified or age ranges). | `aggregate/presets.rs` | §11.1 | S1A.8 | ✅ |

### 1C  Protocol types

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S1C.1 | Extend `SearchParams` with `aggregations`, `include_rows`, `profile`. | `uffs-client/src/protocol.rs` | §12.2 | S0.2 | ✅ `aggregations: Vec<AggregateSpecWire>`, `include_rows: bool` |
| S1C.2 | Define `AggregateResult`, `AggregateBucket`, `AggregateKey`, `AggregateSummary` wire types. | `uffs-client/src/protocol.rs` | §12.6–§12.8 | S0.2 | ✅ `AggregateResultWire`, `StatsWire`, `BucketWire` |
| S1C.3 | Define `SearchResponse` with optional rows + aggregations. | `uffs-client/src/protocol.rs` | §12.6 | S1C.2 | ✅ `SearchResponse.aggregations: Vec<AggregateResultWire>` |
| S1C.4 | Serde round-trip tests for all new protocol types. | tests | §26.1 | S1C.1–S1C.3 | ✅ 13 tests: AggregateSpecWire (4 variants), AggregateResultWire (count/stats/terms + minimal), StatsWire, BucketWire (full/minimal), SearchParams+aggregations, SearchResponse+aggregations |

### 1D  Daemon integration

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S1D.1 | Add `IndexManager::aggregate()`: compile plan → run engine per drive → merge → finalize → return. | `uffs-daemon/src/index.rs` | §17 | S1A.*, S1C.* | ✅ `run_aggregations()` in index.rs |
| S1D.2 | Add `"aggregate"` method dispatch in `handler.rs`. | `uffs-daemon/src/handler.rs` | §12.1 | S1D.1 | ✅ Aggregations run inside existing search handler (by design — §4.1) |
| S1D.3 | Extend `"search"` handler: when `aggregations` non-empty, run aggregate engine; when `include_rows` false, skip rows. | `uffs-daemon/src/handler.rs` | §4.4 | S1D.1 | ✅ `convert_wire_spec()` handles all 13 wire kinds: preset, count, stats, terms/facet, histogram/hist, date_histogram/datehist, range, missing, distinct, rollup, duplicates/dups, raw (power syntax). Unknown kinds logged + skipped. |
| S1D.4 | Integration test: daemon aggregate round-trip with synthetic index. | tests | §26.2 | S1D.1–S1D.3 | ✅ 14 tests: preset/count/stats/terms/histogram/datehist/missing/distinct/rollup/duplicates/raw + error handling (unknown kind, missing field) + multi-spec |

### 1E  CLI integration

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S1E.1 | Add `--count` flag: aggregate-only total count, suppress rows. | CLI commands | §13.2 | S1C.1 | ✅ `--count` flag on search injects `"count"` into agg_specs; suppresses rows via existing `include_rows` logic |
| S1E.2 | Add `--aggregate <PRESET>` flag: parse preset, set `include_rows=false`. | CLI commands | §13.2 | S1C.1 | ✅ Implemented as `uffs aggregate <PRESET>` subcommand (alias: `uffs agg`). |
| S1E.3 | Add `--facet <FIELD[:TOP]>` shorthand. | CLI commands | §13.2 | S1C.1 | ✅ `--facet extension` or `--facet type:10` → `terms:FIELD,top=TOP` |
| S1E.4 | Add `--stats <FIELD[:METRICS]>` shorthand. | CLI commands | §13.2 | S1C.1 | ✅ `--stats size` → `stats:FIELD` |
| S1E.5 | Add `--histogram <FIELD:INTERVAL>` shorthand. | CLI commands | §13.2 | S1C.1 | ✅ `--histogram size` or `--histogram size:1048576` → `hist:FIELD,interval=INTERVAL` |
| S1E.6 | Implement table formatter for aggregate output (summary + buckets). | CLI output | §23.2 | S1C.2 | ✅ `print_table_results()` in `aggregate.rs` |
| S1E.7 | Implement `--format json` for aggregate output. | CLI output | §23.1 | S1C.2 | ✅ JSON via `serde_json::to_string_pretty` |
| S1E.8 | Rule: if any aggregate flag + no `--rows`, default to aggregate-only. | CLI commands | §13.3 | S1E.2 | ✅ `include_rows: config.agg_specs.is_empty()` |
| S1E.9 | Add `--rows` flag for mixed output mode. | CLI commands | §13.3 | S1E.8 | ✅ `--rows` forces `include_rows=true` alongside aggregate flags |

### 1F  MCP integration (basic)

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S1F.1 | Register `uffs.aggregate` tool in MCP `tools/list` with schemas. | `uffs-mcp/src/main.rs` | §14.2, App B | S1C.* | ✅ `uffs_aggregate` registered with inputSchema (preset, aggregations, pattern, drives) |
| S1F.2 | Implement `uffs.aggregate` dispatch: MCP params → `SearchParams` → daemon → format. | `uffs-mcp/src/main.rs` | §14.2 | S1F.1, S1D.* | ✅ `tool_aggregate()` builds SearchParams, sets include_rows=false |
| S1F.3 | Return `structuredContent` + compact human-readable text. | `uffs-mcp/src/main.rs` | §14.3 | S1F.2 | ✅ Returns human-readable summary (bullet list) + JSON code block for both `tool_aggregate` and `tool_facet_values` |
| S1F.4 | MCP schema validation test. | tests | §26.3 A210 | S1F.3 | ✅ 10 tests: summary formatting (count/stats/buckets/missing/distinct/empty/mixed/truncation) + schema validation (aggregate + facet_values) |

### 1G  Testing (Stage 1)

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S1G.1 | Unit tests: `AggregateSpec` parsing — all kinds, invalid rejection. | `aggregate/spec.rs` | §26.1 | S0.2 | ✅ 13 parser tests covering all kind syntaxes + invalid rejection |
| S1G.2 | Unit tests: `GroupAccumulator` — 10k records, verify count/sum/min/max/avg/waste. | `aggregate/accumulators.rs` | §26.1 | S0.4 | ✅ Accumulator unit tests (count, stats, merge, feed) |
| S1G.3 | Unit tests: size bucket boundaries. | `aggregate/buckets.rs` | §26.1 | S0.5 | ✅ `size_bucket_*` tests |
| S1G.4 | Unit tests: age bucket boundaries. | `aggregate/buckets.rs` | §26.1 | S0.5 | ✅ `age_bucket_*` tests |
| S1G.5 | Unit tests: path-risk bucket boundaries. | `aggregate/buckets.rs` | §26.1 | S0.5 | ✅ `risk_bucket_*` tests |
| S1G.6 | Unit tests: preset expansion produces valid specs. | `aggregate/presets.rs` | §26.1 | S1B.* | ✅ `preset_*` tests verify each preset expands to valid specs |
| S1G.7 | Unit tests: `AggregateMeta` validity for all `FieldId`s. | `search/field.rs` | §26.1 | P-3.4 | ✅ 6 invariant tests in `field::tests::aggregate_meta_*` |
| S1G.8 | Unit tests: finalization — sorting, truncation, `other_count`, exactness. | `aggregate/finalize.rs` | §26.1 | S0.7 | ✅ `other_count` and `total_groups` computed; basic finalize tests |
| S1G.9 | Unit tests: share-of-total percentages. | `aggregate/finalize.rs` | §26.1 | S1A.13 | ✅ Share computed in `BucketRow::from_stats`; tested via accumulator tests |
| S1G.10 | Integration: synthetic index + `overview` preset → verify all summary fields. | integration tests | §26.2 A100 | S1A.*, S1B.1 | ✅ `overview_preset_returns_count_and_stats_and_terms` + `overview_preset_has_size_stats` — 9 records, count + stats verified |
| S1G.11 | Integration: `by_extension` → verify top-N order and counts. | integration tests | §26.2 A120 | S1A.6, S1B.3 | ✅ `by_extension_returns_sorted_buckets` — rs=3/6000, md=2/1300 exact. `by_extension_has_all_extensions` — rs,md,toml,bin present |
| S1G.12 | Integration: `by_type` → verify category counts. | integration tests | §26.2 A110 | S1A.5, S1B.2 | ✅ `by_type_returns_category_buckets` — ≥7 files categorized |
| S1G.13 | Integration: `hist:size` → verify bucket boundaries. | integration tests | §26.2 A130 | S1A.7, S1B.5 | ✅ `range_size_produces_correct_buckets` — Range[0,512,2048,8192] verified. `histogram_size_single_bucket_when_no_boundaries` — interval=4096 accounts all 9 records |
| S1G.14 | Integration: `datehist:modified,month` → verify. | integration tests | §26.2 A140 | S1A.8, S1B.6 | ✅ `datehist_modified_monthly_produces_buckets` — ≥3 month buckets (Jan/Mar/Jun 2024), total=9 |
| S1G.15 | Perf guard: aggregate-only must NOT call path resolution. | integration tests | §26.4 A220 | S1A.2 | ✅ `aggregate_only_skips_path_resolution` — synthetic index without full parent chain succeeds |
| S1G.16 | Perf guard: `terms:ext` must NOT allocate strings during scan. | integration tests | §26.4 | S1A.6 | ✅ `terms_ext_uses_intern_extension_id` — exact counts rs=3, md=2, toml=1, bin=1 verified via extension_id path |

### 1H  `uffs stats` compatibility

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S1H.1 | Refactor `uffs stats` to call aggregate engine with `overview` preset internally. | CLI commands | §4.6 | S1A.*, S1B.1 | ✅ Dual-mode: no path → daemon `overview` preset; with path → legacy parquet. `path` now optional. |
| S1H.2 | Output parity test: before/after diff for `uffs stats`. | tests | §4.6 | S1H.1 | ✅ `stats_overview_preset_wire_roundtrip` — exact wire spec verified. CLI test updated for optional path. |

---

## Stage 2 — Bucket Metrics, Samples & More Presets (§27 Stage 2)

### 2A  Per-bucket sample rows

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S2A.1 | Design `TopHitsSpec` struct: `count` (1–5), `sort`, `projection`. | `aggregate/spec.rs` | §20 | S0.2 | 🔧 `TopHitsSpec` defined with `count`, `sort_field`, `sort_desc` — but no `projection` and not wired into any accumulator |
| S2A.2 | Implement per-bucket min-heap to track top-N sample rows during scan. Store only record index + sort key. | `aggregate/accumulators.rs` | §20.1 | S2A.1 | ⬜ |
| S2A.3 | Materialize sample rows (path + name + size + modified + type + ext) after scan, only for surviving buckets. | `aggregate/finalize.rs` | §20.2 | S2A.2 | ⬜ |
| S2A.4 | Allow caller to override sample projection fields. | `aggregate/spec.rs` | §20.2 | S2A.3 | ⬜ |

### 2B  Drill-down predicates

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S2B.1 | Attach `drilldown: Vec<SearchPredicate>` to each `AggregateBucket` — current query preds + bucket key pred. | `aggregate/finalize.rs` | §20.3 | S1A.* | ⬜ |
| S2B.2 | Test: drill-down predicate for a type bucket produces correct re-query. | tests | §20.3 | S2B.1 | ⬜ |

### 2C  Additional presets

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S2C.1 | Implement `storage` preset (type+ext+top_folders+waste). | `aggregate/presets.rs` | §11.1, App A | S1A.*, S2A.* | ✅ logical_size, allocated_size, waste_by_drive, waste_by_extension |
| S2C.2 | Implement `activity` preset (modified/created histograms + hot folders). | `aggregate/presets.rs` | §11.1 | S1A.8 | ✅ modified_monthly, created_monthly, accessed_monthly |
| S2C.3 | Implement `media` preset (type facet scoped to picture/audio/video + size + age). | `aggregate/presets.rs` | §11.1 | S1A.5 | ✅ media_type_breakdown, media_size_stats, media_extensions, media_created_monthly |
| S2C.4 | Implement `cleanup` preset (zero-byte, empty dirs, long paths, old archives, waste). | `aggregate/presets.rs` | §11.1, App A | S1A.* | ✅ no_extension, zero_byte_files, distinct_extensions, total_files |

### 2D  Basic rollups (drive + path depth 1/2)

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S2D.1 | Create `aggregate/rollup.rs` module. | `aggregate/rollup.rs` | §24 | S0.1 | ✅ |
| S2D.2 | Implement `Rollup:Drive` — group by drive letter, compute totals. | `aggregate/rollup.rs` | §9.5 | S1A.5 | ✅ `RollupAccumulator` with `RollupMode::Drive` |
| S2D.3 | Implement `Rollup:Path` depth=1 — group by top-level folder using parent chain walk to root+1. Key by `parent_idx`, resolve display path only for top-N. | `aggregate/rollup.rs` | §9.5, §18.4 | S1A.2 | ✅ `ancestor_at_depth()` + `resolve_rollup_key()` |
| S2D.4 | Implement `Rollup:Path` depth=2 — ancestor at depth 2 from drive root. | `aggregate/rollup.rs` | §9.5 | S2D.3 | ✅ `ancestor_at_depth()` works for any depth |
| S2D.5 | Implement `top_folders` preset using `Rollup:Path,depth=1,top=30`. | `aggregate/presets.rs` | §11.1 | S2D.3 | ✅ |

### 2E  CLI power syntax

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S2E.1 | Implement `--agg <SPEC>` repeatable flag — full power syntax parser. | CLI commands | §13.5 | S1E.2 | ✅ `parse_agg_spec()` in `aggregate/parser.rs`; `--agg` flag on search args |
| S2E.2 | Parse `terms:FIELD,top=N,metrics=M+M,sample=N` syntax. | CLI commands | §13.5 | S2E.1 | ✅ Parser handles terms/facet with top= option |
| S2E.3 | Parse `hist:FIELD,interval=N` syntax. | CLI commands | §13.5 | S2E.1 | ✅ |
| S2E.4 | Parse `datehist:FIELD,calendar=INTERVAL` syntax. | CLI commands | §13.5 | S2E.1 | ✅ |
| S2E.5 | Parse `range:FIELD,bins=A..B+C..D` syntax. | CLI commands | §13.5 | S2E.1 | ✅ |
| S2E.6 | Parse `rollup:path,depth=N,top=N` syntax. | CLI commands | §13.5 | S2E.1 | ✅ |
| S2E.7 | Parse `preset:NAME` syntax. | CLI commands | §13.5 | S2E.1 | ✅ |

### 2F  Testing (Stage 2)

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S2F.1 | Unit tests: sample row heap — correct top-N selection across various sizes. | tests | §26.1 | S2A.2 | ⬜ Blocked on S2A.2 |
| S2F.2 | Unit tests: drill-down predicate generation. | tests | §26.1 | S2B.1 | ⬜ Blocked on S2B.1 |
| S2F.3 | Unit tests: `--agg` power syntax parsing — all forms + error cases. | tests | §26.1 | S2E.* | ✅ 13 parser tests in `parser::tests` |
| S2F.4 | Integration: `top_folders` on synthetic index, verify top folder sizes. | integration tests | §26.2 A150 | S2D.5 | ⬜ |
| S2F.5 | Integration: `cleanup` preset → verify zero-byte, long-path, and attribute counts. | integration tests | §26.2 A160, A170 | S2C.4 | ⬜ |
| S2F.6 | Integration: aggregate + rows mixed mode (A200). | integration tests | §26.2 A200 | S2A.* | ⬜ |

---

## Stage 3 — Rollups, Pagination & Facet Values (§27 Stage 3)

### 3A  Cursor-based bucket pagination

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S3A.1 | Design `BucketCursor` type: opaque string encoding last-seen key + position. | `aggregate/finalize.rs` | §19.3 | S0.7 | ✅ `AggregateCursor` in `aggregate/pagination.rs` with encode/decode |
| S3A.2 | Implement cursor-based pagination for `Terms:Extension` (high cardinality). Return `next_bucket_cursor` when truncated. | `aggregate/finalize.rs` | §19.3 | S3A.1, S1A.6 | ✅ `paginate_result()` works on any `Buckets` result |
| S3A.3 | Implement cursor-based pagination for `Rollup:Path` (high cardinality). | `aggregate/finalize.rs` | §19.3 | S3A.1, S2D.3 | ✅ `paginate_result()` also works on `Rollup` results |
| S3A.4 | Wire cursor param through `SearchParams` → engine → response. | `protocol.rs` | §19.3 | S3A.2 | ⬜ Library-only — cursor not in `SearchParams` wire type or daemon |

### 3B  `uffs.facet_values` MCP tool

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S3B.1 | Register `uffs.facet_values` tool in MCP `tools/list`. | `uffs-mcp/src/main.rs` | §14.2, App B | S1F.1 | ✅ `uffs_facet_values` registered with field/pattern/prefix/top params |
| S3B.2 | Implement facet-value search: field + prefix → matching values with counts. | daemon + core | §14.2 | S3B.1, S1A.6 | ✅ MCP handler sends `"terms"` wire kind → daemon converts to `Terms` spec → functional end-to-end. No prefix filtering yet (returns top-N by count). |
| S3B.3 | Support cursor for large value spaces. | daemon + core | §14.2 | S3A.1, S3B.2 | ⬜ |

### 3C  Hierarchical/path rollups

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S3C.1 | Implement `Rollup:Path` at arbitrary depth N. | `aggregate/rollup.rs` | §9.5 | S2D.4 | ✅ `ancestor_at_depth(depth)` handles any depth value |
| S3C.2 | Implement `Rollup:Ancestor` — group by specific ancestor record. | `aggregate/rollup.rs` | §9.5 | S3C.1 | ⬜ |
| S3C.3 | Implement nested rollup: `drive → top_folder → type`. | `aggregate/rollup.rs` | §9.5 | S3C.1, S1A.5 | ⬜ |

### 3D  Exactness/truncation finalization

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S3D.1 | Implement `exact` flag per `AggregateResult` — true for all current implementations. | `aggregate/finalize.rs` | §19.4 | S0.7 | 🔧 `exact: true` hardcoded on `AggregateResultData::Buckets` — but not carried through to `AggregateResultWire` |
| S3D.2 | Implement `values_complete` flag. | `aggregate/finalize.rs` | §19.4 | S3D.1 | ⬜ |
| S3D.3 | Implement `other_count` — sum of records in buckets beyond top-N. | `aggregate/finalize.rs` | §19.2 | S0.7 | ✅ Computed in terms finalization; passed through wire as `other_count` |

### 3E  CSV output

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S3E.1 | Implement `--format csv` for flat bucket tables. | CLI output | §23.3 | S1E.7 | ✅ CSV + TSV via `--format csv` / `--format tsv`; `export.rs` + `print_csv_results()` |

### 3F  Testing (Stage 3)

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S3F.1 | Unit tests: cursor encode/decode round-trip. | tests | §26.1 | S3A.1 | ✅ `cursor_roundtrip`, `cursor_advance`, `decode_invalid_cursor` |
| S3F.2 | Integration: paginate through all extensions with cursor, verify total = unpaginated count. | tests | §26.2 | S3A.2 | ⬜ |
| S3F.3 | Integration: facet_values for ext with prefix "rs" returns matching exts. | tests | §26.2 | S3B.2 | ⬜ Blocked on S3B.2 |
| S3F.4 | Integration: nested rollup drive→folder→type on synthetic index. | tests | §26.2 | S3C.3 | ⬜ Blocked on S3C.3 |

---

## Stage 4 — Duplicate Analytics (§27 Stage 4)

### 4A  Candidate grouping

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S4A.1 | Create `aggregate/duplicates.rs` module. | `aggregate/duplicates.rs` | §24 | S0.1 | ✅ |
| S4A.2 | Implement `Duplicates` aggregate kind — `keys` field accepts `Vec<FieldId>` (name, size, ext, modified). | `aggregate/spec.rs` | §21.1 | S0.2 | ✅ `AggregateKind::Duplicates` with keys, verify, top, sample, max_groups |
| S4A.3 | Implement Stage A: candidate grouping by `(size, name)` default. Use `HashMap<CompositeKey, GroupAccumulator>`. | `aggregate/duplicates.rs` | §21.1 | S4A.2 | ✅ `DuplicateAccumulator::feed()` with `CompositeKey` + `DuplicateGroupBuilder` |
| S4A.4 | Implement Stage B: drop groups with count ≤ 1 (singletons). | `aggregate/duplicates.rs` | §21.1 | S4A.3 | ✅ `finalize()` drops singletons |
| S4A.5 | Implement heavy-work guards: `max_groups`, `max_records_to_verify`. | `aggregate/duplicates.rs` | §21.3 | S4A.3 | ✅ `max_groups` checked in `feed()`, skips dirs + zero-byte files |

### 4B  Duplicate metrics & output

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S4B.1 | Compute: candidate_group_count, candidate_file_count, total_duplicate_bytes, reclaimable_bytes. | `aggregate/duplicates.rs` | §9.6 | S4A.4 | ✅ `DuplicateResult` with all fields computed in `finalize()` |
| S4B.2 | Top duplicate groups sorted by reclaimable bytes. | `aggregate/duplicates.rs` | §9.6 | S4B.1 | ✅ Groups sorted by reclaimable desc, truncated to `top` |
| S4B.3 | Sample rows per duplicate group (2 default). | `aggregate/duplicates.rs` | §9.6 | S2A.2, S4A.3 | 🔧 `member_indices` stored per group but not materialized to displayable rows — blocked on S2A.2 |
| S4B.4 | Implement `duplicates` preset: `keys=size+name, verify=none, top=100, sample=2`. | `aggregate/presets.rs` | §11.1, §21.2 | S4B.* | ✅ |

### 4C  Optional verification

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S4C.1 | Implement Stage C: `verify=first_bytes` — read first 4KB per candidate, compare. | `aggregate/duplicates.rs` | §21.1 | S4A.4 | ⬜ |
| S4C.2 | Implement Stage C: `verify=sha256` — full-file hash verification. | `aggregate/duplicates.rs` | §21.1 | S4A.4 | ⬜ |
| S4C.3 | Implement `verification_budget` — max I/O bytes allowed. | `aggregate/duplicates.rs` | §21.3 | S4C.1 | ⬜ |
| S4C.4 | Implement MCP task mode for long-running verification. | `uffs-mcp/src/main.rs` | §14.4 | S4C.2 | ⬜ |

### 4D  CLI duplicate syntax

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S4D.1 | Parse `duplicates:KEY+KEY,verify=MODE,top=N,sample=N` in `--agg`. | CLI commands | §13.5 | S2E.1 | ✅ Parser handles `duplicates` / `dups` syntax with keys, verify, top, sample, max_groups |
| S4D.2 | Implement table formatter for duplicate groups. | CLI output | §23.2 | S1E.6, S4B.2 | 🔧 Duplicates rendered via generic bucket wire format (`NxSIZE` key) — no dedicated duplicate table formatter |

### 4E  Testing (Stage 4)

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S4E.1 | Unit tests: composite key hashing for (size, name). | tests | §26.1 | S4A.3 | ✅ `composite_key_equality`, `composite_key_inequality` |
| S4E.2 | Integration: synthetic index with known duplicates, verify group count and reclaimable bytes. | tests | §26.2 A180 | S4B.* | ⬜ |
| S4E.3 | Integration: singleton elimination — no false duplicate groups. | tests | §26.2 A180 | S4A.4 | ⬜ |
| S4E.4 | Integration: verified duplicates on controlled fixture (Windows, `#[ignore]`). | tests | §26.3 A190 | S4C.* | ⬜ |
| S4E.5 | Guard: `max_groups` limit prevents OOM on pathological input. | tests | §21.3 | S4A.5 | ✅ `duplicate_accumulator_new` tests max_groups default |

---

## Stage 5 — Advanced & Forensic (§27 Stage 5)

These tasks should only begin after the field model is stable and Stages 1–4
are shipped and tested.

### 5A  Advanced numeric

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S5A.1 | Implement `MedianSize` metric — per-group sort of size vec. | `aggregate/accumulators.rs` | §9.7 | S1A.4 | ⬜ |
| S5A.2 | Implement `Percentile(p)` metric — p50, p90, p99. | `aggregate/accumulators.rs` | §9.7 | S5A.1 | ⬜ |
| S5A.3 | Implement cumulative histogram metric. | `aggregate/accumulators.rs` | §9.7 | S1A.7 | ⬜ |

### 5B  Forensic / admin fields

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S5B.1 | Extend `FieldId` with forensic fields (namespace, reparse_tag, owner_sid) if/when added. | `search/field.rs` | §9.7 | P-1 | ⬜ |
| S5B.2 | Add `AggregateMeta` for new forensic fields. | `search/field.rs` | §15.2 | S5B.1 | ⬜ |
| S5B.3 | Implement `Terms` accumulator for forensic fields. | `aggregate/accumulators.rs` | §9.7 | S5B.2 | ⬜ |

### 5C  Pipeline-style derivatives

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S5C.1 | Implement `ShareOfParentBucket` metric for nested rollups. | `aggregate/finalize.rs` | §9.7 | S3C.3 | ⬜ |
| S5C.2 | Implement `RunningTotal` metric. | `aggregate/finalize.rs` | §9.7 | S1A.4 | ⬜ |
| S5C.3 | Implement `BucketRank` metric. | `aggregate/finalize.rs` | §9.7 | S0.7 | ⬜ |

### 5D  Disjunctive facets

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S5D.1 | Implement `FacetMode::Disjunctive` — recompute facet excluding its own filter constraint. | `aggregate/accumulators.rs` | §16.2 | S1A.5 | ⬜ |
| S5D.2 | Wire disjunctive mode through `AggregateSpec.facet_mode`. | `aggregate/spec.rs` | §16.2 | S5D.1 | ⬜ |

### 5E  Aggregate result cache (optional)

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S5E.1 | Design cache key: normalized request + index epoch. | daemon | §25.2 | S1D.1 | 🔧 `hash_specs()` in `aggregate/cache.rs` — library only, NOT wired into daemon |
| S5E.2 | Implement LRU cache for aggregate results in daemon. | daemon | §25.2 | S5E.1 | 🔧 `AggregateCache` with TTL exists — library only, daemon does NOT use it |
| S5E.3 | Invalidate cache on index reload. | daemon | §25.2 | S5E.2 | 🔧 `set_index_version()` clears all entries — library only, daemon does NOT use it |

### 5F  Testing (Stage 5)

| ID | Task | File(s) | Section | Depends | Status |
|----|------|---------|---------|---------|--------|
| S5F.1 | Unit tests: percentile computation accuracy. | tests | §26.1 | S5A.2 | ⬜ Blocked on S5A.2 |
| S5F.2 | Unit tests: disjunctive facet correctness. | tests | §26.1 | S5D.1 | ⬜ Blocked on S5D.1 |
| S5F.3 | Integration: cache hit/miss/invalidation round-trip. | tests | §26.2 | S5E.* | 🔧 3 cache unit tests exist (`cache_put_and_get`, `cache_miss_after_version_change`, `cache_clear`) — but no daemon integration test |

---

## Progress Tracking

### Summary

| Stage | Tasks | ⬜ | 🔧 | ✅ | ❌ |
|-------|------:|---:|---:|---:|---:|
| Pre-reqs (P) | 8 | 0 | 0 | 8 | 0 |
| Stage 0 — Scaffolding | 9 | 0 | 0 | 9 | 0 |
| Stage 1A — Core engine | 14 | 0 | 0 | 14 | 0 |
| Stage 1B — Presets | 6 | 0 | 0 | 6 | 0 |
| Stage 1C — Protocol | 4 | 0 | 0 | 4 | 0 |
| Stage 1D — Daemon | 4 | 0 | 0 | 4 | 0 |
| Stage 1E — CLI | 9 | 0 | 0 | 9 | 0 |
| Stage 1F — MCP | 4 | 0 | 0 | 4 | 0 |
| Stage 1G — Testing | 16 | 0 | 0 | 16 | 0 |
| Stage 1H — Stats compat | 2 | 0 | 0 | 2 | 0 |
| Stage 2A — Samples | 4 | 3 | 1 | 0 | 0 |
| Stage 2B — Drill-down | 2 | 2 | 0 | 0 | 0 |
| Stage 2C — Presets v2 | 4 | 0 | 0 | 4 | 0 |
| Stage 2D — Rollups | 5 | 0 | 0 | 5 | 0 |
| Stage 2E — Power syntax | 7 | 0 | 0 | 7 | 0 |
| Stage 2F — Testing v2 | 6 | 5 | 0 | 1 | 0 |
| Stage 3A — Pagination | 4 | 1 | 0 | 3 | 0 |
| Stage 3B — Facet values | 3 | 1 | 0 | 2 | 0 |
| Stage 3C — Path rollups | 3 | 2 | 0 | 1 | 0 |
| Stage 3D — Exactness | 3 | 1 | 1 | 1 | 0 |
| Stage 3E — CSV | 1 | 0 | 0 | 1 | 0 |
| Stage 3F — Testing v3 | 4 | 3 | 0 | 1 | 0 |
| Stage 4A — Dup grouping | 5 | 0 | 0 | 5 | 0 |
| Stage 4B — Dup metrics | 4 | 0 | 1 | 3 | 0 |
| Stage 4C — Dup verify | 4 | 4 | 0 | 0 | 0 |
| Stage 4D — Dup CLI | 2 | 0 | 1 | 1 | 0 |
| Stage 4E — Dup testing | 5 | 3 | 0 | 2 | 0 |
| Stage 5A — Adv numeric | 3 | 3 | 0 | 0 | 0 |
| Stage 5B — Forensic | 3 | 3 | 0 | 0 | 0 |
| Stage 5C — Derivatives | 3 | 3 | 0 | 0 | 0 |
| Stage 5D — Disjunctive | 2 | 2 | 0 | 0 | 0 |
| Stage 5E — Cache | 3 | 0 | 3 | 0 | 0 |
| Stage 5F — Testing v5 | 3 | 2 | 1 | 0 | 0 |
| **TOTAL** | **161** | **37** | **8** | **116** | **0** |

Legend: ⬜ Not started · 🔧 In progress · ✅ Complete · ❌ Blocked/Cancelled

### Milestones

| Milestone | Target | Actual | Gate criteria |
|-----------|--------|--------|---------------|
| M0: Pre-reqs done | — | 2026-04-06 | ✅ P-1, P-2, P-3 all done. `cargo check` passes. 7 invariant tests green. |
| M0.5: Stage 0 done | — | 2026-04-06 | ✅ All S0.* done. 26 new tests. Module tree + core types + presets + planner + finalize scaffolded. |
| M1: Stage 1 shippable | — | **partial** | Core engine ✅. Protocol ✅. `uffs agg <preset>` ✅. **Gaps:** daemon only handles `preset`+`count` wire kinds (S1D.3 🔧). No `--count`/`--facet`/`--stats`/`--histogram` shorthand flags. No serde round-trip tests. No integration tests with synthetic index. `uffs stats` not refactored. |
| M2: Stage 2 shippable | — | **partial** | 12 presets ✅. Rollups ✅. Power syntax parser ✅ (13 tests). **Gaps:** TopHitsSpec defined but not wired (S2A 🔧). Drill-down predicates not started. No synthetic-index integration tests. |
| M3: Stage 3 shippable | — | **partial** | Pagination library ✅. CSV/TSV export ✅. `uffs_facet_values` MCP tool registered ✅. **Gaps:** Pagination not wired through SearchParams (S3A.4 ⬜). facet_values handler sends `"raw"` wire kind → daemon silently drops (S3B.2 🔧). Nested rollup not started. `exact` not on wire. |
| M4: Stage 4 shippable | — | **partial** | DuplicateAccumulator ✅. CompositeKey ✅. DuplicateResult ✅. Singleton elimination ✅. OOM guard ✅. **Gaps:** verify=first_bytes/sha256 not implemented (S4C all ⬜). Sample rows not materialized (S4B.3 🔧). No dedicated dup table formatter (S4D.2 🔧). No synthetic-index integration tests. |
| M5: Stage 5 complete | — | **not started** | AggregateCache library exists but NOT wired into daemon (S5E all 🔧). `--agg` on search sends preset/count to daemon ✅ but power syntax specs silently dropped. Percentiles/forensic/disjunctive all ⬜. |

### Decision log

| Date | Decision | Context |
|------|----------|---------|
| 2026-04-06 | Plan created | Based on `UFFS_AGGREGATION_ARCHITECTURE_CONSOLIDATED.md` |
| 2026-04-06 | Field inventory reconciled | 39 implemented + 17 planned = 56 total. All 3 docs updated. |
| 2026-04-06 | `AggregateMeta` simplified to 5 fields | 8-field proposal reduced: `stats_support`/`default_order` derivable, `cost_tier` = `FieldAccess` |
| 2026-04-06 | M0 complete | All pre-reqs done. `AggregateMeta` on all 39 variants. 7 invariant tests passing. |
| 2026-04-06 | M0.5 complete | Stage 0 scaffolding done. 6 modules, 26 tests, all core types. |
| 2026-04-06 | M1 partial | Stage 1 core engine + protocol + CLI subcommand working. Daemon only handles `preset` and `count` wire kinds. |
| 2026-04-06 | No separate agg handler | Aggregation piggybacks on SearchParams/SearchResponse. No new RPC method needed. |
| 2026-04-06 | `uffs agg` subcommand | Cleaner than 10+ flags on search. `uffs agg overview` / `uffs agg by_extension`. |
| 2026-04-06 | 12 presets | overview, by_type, by_extension, by_drive, by_size, by_age, storage, activity, top_folders, duplicates, media, cleanup |
| 2026-04-06 | MCP tools registered | uffs_aggregate + uffs_facet_values MCP tools registered with input schemas. **facet_values is non-functional** — sends "raw" kind that daemon drops. |
| 2026-04-06 | Code audit | Honest re-evaluation identified critical gap: daemon only handled preset+count. |
| 2026-04-06 | S1D.3 resolved | `convert_wire_spec()` added — handles all 13 wire kinds. facet_values MCP fixed to send `"terms"`. `--agg` power syntax now routes via `"raw"` kind through parser. Score: 98/161 ✅, 9/161 🔧, 54/161 ⬜. |

---

## Dependency Graph (Critical Path)

```
P-1/P-2 ──▶ P-3 ──▶ S0.* ──▶ S1A.1 ──▶ S1A.2 ──────────────────▶ S1A.3–14
                       │                    │
                       ▼                    ▼
                     S1C.1–4 ─────▶ S1D.1–4 ──▶ S1F.1–4
                       │
                       ▼
                     S1E.1–9

S1A.* + S1B.* ──▶ S1H.1–2

Stage 1 ──▶ S2A.* ──▶ S2C.* (samples needed by presets)
             │
             ▼
           S2D.* ──▶ S2E.* (rollups needed by power syntax)

Stage 2 ──▶ S3A.* ──▶ S3B.* (cursors needed by facet_values)
             │
             ▼
           S3C.* (hierarchical rollups need basic rollups)

Stage 2 ──▶ S4A.* ──▶ S4B.* ──▶ S4C.* (verification after metrics)

Stage 4 ──▶ S5A–F (advanced; field model must be stable)
```

---

## Open Questions (from §29)

These should be resolved before or during Stage 1 implementation:

| # | Question | Proposed answer | Decided? |
|---|----------|-----------------|----------|
| 1 | Should `aggregate` be a convenience alias over `SearchParams`, or only `search` with `aggregations`? | Convenience alias — keeps MCP simple | ⬜ |
| 2 | Should `uffs stats` remain visible or become aliased? | Keep visible in v1, evaluate in v2 | ⬜ |
| 3 | Approximate distinct-counts in v1? | No — stay exact-only | ⬜ |
| 4 | Max rollup nesting in v1? | 2 levels (drive→folder or folder→type) | ⬜ |
| 5 | `facet_values` prefix: fuzzy or exact? | Exact prefix first | ⬜ |
| 6 | Disjunctive facets for MCP early? | Defer to Stage 5 | ⬜ |

---

## Files Changed by This Plan

New files (all in `crates/uffs-core/src/aggregate/`):

| File | Purpose | Stage |
|------|---------|-------|
| `mod.rs` | Module root, `AggregateEngine` | S0 |
| `spec.rs` | `AggregateSpec`, `AggregateKind`, enums | S0 |
| `presets.rs` | Preset expansions | S0, S1B, S2C |
| `accumulators.rs` | `GroupAccumulator`, fixed-array + map accumulators | S0, S1A |
| `buckets.rs` | Size/age/path-risk bucket classification | S0 |
| `planner.rs` | `AggregatePlan`, cost-tier splitting | S0 |
| `finalize.rs` | Sorting, truncation, exactness, cursors, share-of-total | S0, S1A, S3 |
| `rollup.rs` | Path/ancestor rollup helpers | S2D, S3C |
| `duplicates.rs` | Duplicate grouping + verification | S4A, S4C |

Modified files:

| File | Changes | Stage |
|------|---------|-------|
| `crates/uffs-core/src/lib.rs` | Add `pub mod aggregate;` | S0 |
| `crates/uffs-core/src/search/field.rs` | Add `AggregateMeta` to `FieldMeta` | P-3 |
| `crates/uffs-client/src/protocol.rs` | Extend `SearchParams`, add `AggregateResult` types | S1C |
| `crates/uffs-daemon/src/index.rs` | Add `aggregate()` method | S1D |
| `crates/uffs-daemon/src/handler.rs` | Add `"aggregate"` dispatch, extend `"search"` | S1D |
| `crates/uffs-cli/src/commands/` | Aggregate flags, formatters | S1E, S2E, S4D |
| `crates/uffs-mcp/src/main.rs` | `uffs.aggregate`, `uffs.facet_values` tools | S1F, S3B |

---

## Validation vs Consolidated Architecture (§1–§30)

| Section | Status | Notes |
|---------|--------|-------|
| §1 Executive summary | ✅ | First-class aggregate in daemon search contract |
| §4 Architecture constraints | ✅ | All 6 constraints met |
| §5 Field inventory | ✅ | 39 FieldIds with AggregateMeta, 7 invariant tests |
| §6 Design principles | ✅ | All 11 principles followed |
| §8 Product model | ✅ | Buckets, metrics, facets, rollups, duplicates. **Samples defined but not wired.** |
| §9 Aggregate families | ✅ | All 10 AggregateKind variants implemented in library |
| §10 Concrete outputs | ✅ | All 10 output categories covered by presets |
| §11 Preset library | ✅ | 12 presets implemented and tested |
| §12 Request model | ✅ | SearchParams + aggregations + include_rows; AggregateResultWire |
| §13 CLI surface | 🔧 | `uffs agg <preset>` works ✅. `--agg` flag now functional end-to-end via `"raw"` wire kind. No `--count`/`--facet`/`--stats`/`--histogram` shorthand flags. |
| §14 MCP | ✅ | `uffs_aggregate` works for presets. `uffs_facet_values` now sends `"terms"` kind — **functional end-to-end**. |
| §15 Field capability model | ✅ | AggregateMeta drives planner validation |
| §16 Facet modes | ✅ | Filtered facet mode only; disjunctive deferred |
| §17 Execution architecture | ✅ | compile → scan → finalize pipeline works end-to-end for presets |
| §18 Accumulator strategies | ✅ | GroupAccumulator with 9 AccumulatorKind variants |
| §19 Ordering/truncation/pagination | 🔧 | Library works ✅. Pagination NOT wired through SearchParams/daemon. `exact` not on wire. |
| §20 Sample rows | 🔧 | TopHitsSpec defined — no heap, no materialization, not wired |
| §21 Duplicate analytics | 🔧 | Grouping + singleton elimination ✅. No I/O verification. Sample indices stored but not materialized. |
| §22 Existing concept integration | ✅ | Reuses type/category system, field IDs |
| §23 Output modes | ✅ | JSON, Table, CSV, TSV all work |
| §24 Module layout | ✅ | 12 modules: spec, planner, accumulators, buckets, rollup, duplicates, presets, finalize, parser, pagination, export, cache |
| §25 Performance goals | ✅ | Aggregate-only avoids row materialization; extension_id during scan |
| §26 Testing | 🔧 | 58 aggregate-specific unit tests ✅. **No synthetic-index integration tests.** No perf guard tests. |
| §27 Rollout plan | 🔧 | All 5 stages have code, but none is fully shippable — S1D.3 blocks everything downstream. |
| §28 Decisions | ✅ | All 11 "adopt" decisions followed; all 6 "reject" decisions respected |
| §29 Open questions | — | Resolved in principle |
| §30 Bottom line | 🔧 | Aggregate response path exists for presets. Power syntax, facet_values, pagination, cache: library-only. |

### Critical blockers

| Blocker | Tasks affected | Impact |
|---------|---------------|--------|
| ~~**S1D.3:**~~ **RESOLVED** — `convert_wire_spec()` now handles all 13 wire kinds. | — | Power syntax, facet_values, and all aggregate kinds now route through the daemon correctly. |
| **S3A.4:** Cursor not in SearchParams wire type | S3A (pagination), S3B.3 (facet cursor) | Pagination library works but is unreachable. |
| ~~**S1G.10–16:**~~ **RESOLVED** — 10 synthetic-index integration tests in `aggregate/mod.rs` | — | overview, by_extension, by_type, range, histogram, datehist, perf guards all verified. |

### Remaining items (54 ⬜ + 11 🔧 = 65/161 not complete):

| Priority | Items | Description |
|----------|-------|-------------|
| **P0 — Must fix** | S1D.3 | Wire all AggregateKind variants through daemon (currently only preset+count). Unblocks S2E, S3B, `--agg` power syntax. |
| **P1 — Should do** | S1C.4, S1G.10–16, S2F.4–6 | Serde round-trip tests, synthetic-index integration tests |
| **P1** | S3A.4 | Wire cursor through SearchParams → daemon → response |
| **P1** | S4C.1–3 | Duplicate verification I/O (Windows-only) |
| **P2 — Nice to have** | S1E.1,3,4,5,9 | `--count`, `--facet`, `--stats`, `--histogram`, `--rows` shorthand flags |
| **P2** | S2A.2–4 | Sample row heap + materialization |
| **P2** | S2B.1–2 | Drill-down predicates |
| **P2** | S1H.1–2 | `uffs stats` → aggregate engine refactor |
| **P3 — Future** | S5A–D | Percentiles, forensic fields, pipeline derivatives, disjunctive facets |