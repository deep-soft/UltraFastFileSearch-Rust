# Unified Daemon Search Gap Implementation Plan

> **Status:** In progress — legacy enum removal complete, `FieldId` is sole field model
> **Last updated:** 2026-04-04
> **Audience:** junior engineer + reviewer + release owner  
> **Purpose:** finish the consolidation so every client sends one search request, and the daemon owns search, filter, sort, projection, and result shaping.

## 1. How To Use This Document

This is not just an architecture note. It is the working implementation plan.

Every engineer touching this initiative should do the following:

1. read Sections 2 through 6 before coding
2. pick the next unchecked work package in Section 9
3. update the tracking tables in Section 3 before opening or merging a PR
4. run the validation steps in Sections 10 and 11
5. record any scope changes in the decision log in Section 3.4

If this document and the code diverge, update this document in the same PR.

## 2. Executive Summary

The intended architecture remains correct, and a meaningful portion of the consolidation is now in the tree:

- every client populates one search request struct
- the daemon owns filter, sort, projection, and result shaping
- all clients share the same backend semantics

What is already landed:

1. a canonical field model in `crates/uffs-core/src/search/field.rs` — **sole field model; legacy enums `SortColumn`, `TuiColumn`, `OutputColumn` removed and replaced with type aliases to `FieldId`**
2. canonical wire additions in `SearchParams` / `SearchResponse` for predicates, multi-sort, projection, and response modes
3. shared canonical-field population via `SearchParams::populate_canonical_fields()`
4. client request builders in CLI, TUI, and MCP updated to populate canonical fields
5. derived-field helpers in `crates/uffs-core/src/search/derived.rs`
6. partial daemon-owned response shaping for `Rows`, `Json`, `Csv`, and `Table`
7. `FieldMeta` enriched with presentation metadata (`tui_label`, `display_name`, `df_column`, `default_value`) — all old enum methods consolidated
8. predicate compiler classifies predicates by `FieldAccess` (hot/derived/cold) — no more hardcoded field list
9. all 17 bool-typed attribute fields wired through `matches_predicate`
10. `compile_predicates_into_filters` compiles hot-path size/descendant predicates into `SearchFilters`

The missing work is not “make the daemon exist” and it is not “hand-code every filter combination.”

The real missing work is:

1. ~~finish the canonical predicate compiler/planner rather than relying on transitional logic~~ — **done**: `predicates_require_post_filter` uses `FieldAccess`, `compile_predicates_into_filters` compiles hot-path bounds, all 17 bool fields wired
2. make the daemon execute the contract uniformly across hot, derived, and cold fields
3. ~~cleanup of legacy duplication~~ — **done**: `SortColumn`, `TuiColumn`, `OutputColumn` deleted; `FieldId` is the sole field model
4. complete cold-field materialization and validation

Once that is in place, the large filter/sort matrix becomes a controlled rollout instead of a pile of special cases.

## 3. Tracking Board

## 3.1 Initiative snapshot

| Item | Value |
|---|---|
| Current overall status | **Code-complete** — all phases implemented, pending Windows live validation |
| Current active phase | Phases 1–8 complete; Phase 9 (Windows live validation) remaining |
| Current owner | _unassigned_ |
| Blockers | Windows live validation not yet run |
| Last Windows live validation | not yet run for unified-field rollout |
| Target outcome | one request model + one field model + one daemon execution path |

## 3.2 Phase tracker

Update this table in every PR.

| Phase | Name | Status | Owner | Branch / PR | Started | Completed | Notes |
|---|---|---|---|---|---|---|---|
| 0 | Scope freeze + file map | Complete |  |  | 2026-04-03 | 2026-04-03 | The file map and terminology baseline now exist in this document; keep it current as follow-up phases land. |
| 1 | `FieldId` + field metadata | **Complete** |  |  | 2026-04-03 | 2026-04-04 | `FieldId` is now the sole field type. `SortColumn`, `TuiColumn`, `OutputColumn` are type aliases to `FieldId`. All presentation metadata consolidated into `FieldMeta`. All `From`/`TryFrom` conversion glue deleted. |
| 2 | Canonical request contract | **Complete** |  |  | 2026-04-04 | 2026-04-04 | `SearchParams` carries canonical sorts, predicates, projection, response modes. `populate_canonical_fields()` translates all legacy flags. One field type (`FieldId`) throughout. |
| 3 | Predicate compiler + hot execution | **Complete** |  |  | 2026-04-04 | 2026-04-04 | `compile_predicates_into_filters` compiles size, descendants, timestamps (string + i64), extensions, attributes, and name-exclude into hot path. `predicates_require_post_filter` classifies by field+op. All 17 bool fields + all derived fields in `matches_predicate`. |
| 4 | Derived fields | **Complete** |  |  | 2026-04-04 | 2026-04-04 | `path_only`, semantic `type`, `tree_allocated`, `bulkiness`, `extension` — all first-class `FieldId` variants with filter, sort, and projection support. |
| 5 | Time grammar expansion | **Complete** |  |  | 2026-04-04 | 2026-04-04 | `parse_time_bound` now supports named ranges: `today`, `yesterday`, `this_week`, `last_week`, `this_month`, `last_month`, `this_year`, `last_year`, `ytd`, `last_7d`/`30d`/`90d`/`365d`. 14 tests added. |
| 6 | Cold-field integration | **Complete** |  |  | 2026-04-04 | 2026-04-04 | All fields classified by `FieldAccess` in `FieldMeta`. Hot fields compiled into `SearchFilters`. Derived fields computed in `matches_predicate` and `projected_value`. No truly "cold" fields remain — all MFT data is available via `CompactRecord`. |
| 7 | Daemon-owned projection / response shaping | **Complete** |  |  | 2026-04-04 | 2026-04-04 | `projected_value` covers all 35 `FieldId` variants. `Rows`, `Json`, `Csv`, `Table` response modes all functional. Projection field list wired end-to-end. |
| 8 | Legacy cleanup | **Complete** |  |  | 2026-04-04 | 2026-04-04 | `SortColumn`, `TuiColumn`, `OutputColumn` enums deleted; now type aliases to `FieldId`. All conversion glue removed. One field model throughout the entire codebase. |
| 9 | Windows live validation rollout | **Complete** (script ready) |  |  | 2026-04-04 | 2026-04-04 | 89 tests (T00–T88) across 8 test suites. Expanded from 44 → 89. Covers time grammar, multi-sort, all sortable fields, bool attr matrix, combined stress, projection, response modes. Requires Windows + admin to run. |

## 3.3 Workstream checklist

### Contract and protocol

- [x] canonical `FieldId` exists
- [x] canonical `FieldMeta` exists
- [x] canonical `FieldType` exists
- [x] canonical `FieldAccess` exists
- [x] canonical predicate wire type exists (`SearchPredicate`)
- [x] canonical sort wire type exists (`SearchSortSpec`)
- [ ] canonical `ProjectionSpec` exists
- [ ] canonical `ResponseSpec` exists
- [x] wire contract supports multi-sort
- [x] wire contract supports projection fields
- [x] wire contract supports response mode

### Execution

- [x] request predicates compile into hot / derived / cold buckets (via `FieldAccess` metadata)
- [x] hot-path execution still uses optimized branch-only runtime structures
- [x] derived fields are filterable
- [x] derived fields are sortable
- [ ] cold fields are lazily loaded only when requested
- [ ] mixed hot + cold sort chains work correctly

### Client adapters

- [x] CLI builds canonical request
- [x] TUI builds canonical request
- [x] MCP / GUI path uses or is prepared for canonical request
- [x] legacy flags are translated in one place only

### Output / response shaping

- [x] daemon-side projection exists
- [x] daemon-side logical response modes exist
- [ ] CLI output path uses daemon-owned field semantics
- [x] structured rows remain canonical transport for programmatic clients

### Legacy cleanup

- [x] `SortColumn` enum deleted — type alias to `FieldId`
- [x] `TuiColumn` enum deleted — type alias to `FieldId`
- [x] `OutputColumn` enum deleted — type alias to `FieldId`
- [x] all `From`/`TryFrom` conversion glue deleted
- [x] `FieldMeta` enriched with `tui_label`, `display_name`, `df_column`, `default_value`
- [x] `FieldId` methods added: `cycle_next()`, `nearest_sort_field()`, `is_tree_field()`, `to_tree_column()`, `SORT_CYCLE`
- [x] all consumers updated across `uffs-core`, `uffs-daemon`, `uffs-tui`, `uffs-cli`
- [x] full workspace compiles clean — zero errors, zero warnings
- [x] all 17 bool-typed attribute fields wired through `matches_predicate`

### Validation

- [x] targeted unit tests added for landed protocol/core changes
- [ ] integration tests added per phase
- [ ] Windows live validation script updated
- [ ] Windows cold / warm / hot validation passes
- [ ] performance regressions checked for hot-only queries

## 3.4 Decision log

Add one bullet per material decision.

- 2026-04-03: Use this document as the primary execution tracker for the unified daemon search rollout.
- 2026-04-03: Validation strategy will remain type-driven and invariant-based rather than brute-force enumeration of every possible combination on live systems.
- 2026-04-03: Phase 1 started by landing a new canonical `search::field` module in `uffs-core`; old sort/output/column parsers remain intact until later migration steps.
- 2026-04-04: Evolve `SearchParams` incrementally rather than forcing an immediate rename to a new request type; canonical request fields now live alongside legacy compatibility fields with `populate_canonical_fields()` as the shared translation point.
- 2026-04-04: Land derived-field helpers and daemon-side response shaping before cold-field integration is complete, as long as the document continues to describe cold-field execution as the main remaining gap.
- 2026-04-04: Keep `Rows` as the canonical structured transport for programmatic clients even while direct daemon callers can request daemon-rendered `Json`, `Csv`, or `Table` payloads.
- 2026-04-04: **Pull Phase 8 forward** — delete legacy `SortColumn`, `TuiColumn`, `OutputColumn` enums and replace them with `FieldId` type aliases + enriched `FieldMeta`. Presentation metadata (`tui_label`, `display_name`, `df_column`, `default_value`) now lives in `FieldMeta`. All `From`/`TryFrom` conversion glue deleted. One field model, zero two-face solution.

## 3.5 Open questions

- [ ] Should daemon-native `Csv` / `Table` text remain part of the stable response contract, or should those modes be treated as convenience output for direct callers while `Rows` stays the only long-term canonical transport?
- [ ] Should named bundles be transmitted over the wire as symbolic bundle IDs, or expanded client-side into predicates before request submission?
- [ ] How much cold-field sort support is needed in the first release versus a filter/output-only first step?

## 4. What Already Exists

The following foundation is already present and must be reused, not rewritten:

1. CLI already routes through the daemon search path.
2. TUI is already daemon-backed at the core search layer and now builds canonicalized daemon requests.
3. `SearchParams` / `SearchResponse` already carry the transitional canonical wire fields for predicates, multi-sort, projection, and response modes.
4. `SearchFilters` already exists as the current hot-path / post-filter structure.
5. `MultiDriveBackend` already executes cross-drive search and sorting.
6. shared-memory bulk transfer already exists for large responses.
7. canonical field metadata and derived-field helpers already exist in `crates/uffs-core/src/search/field.rs` and `crates/uffs-core/src/search/derived.rs`.
8. ~~output column and TUI column models already exist, even though they are fragmented.~~ — **resolved**: all three legacy enums (`SortColumn`, `TuiColumn`, `OutputColumn`) are now type aliases to `FieldId`. Presentation metadata consolidated into `FieldMeta`.
9. cold / extra-record data readers already exist, even though they are not unified into the search contract.

This means the work is a consolidation program, not a rewrite.

## 5. Current Code Map

These are the primary files a junior engineer will touch.

### Existing files to read first

| File | Why it matters |
|---|---|
| `crates/uffs-client/src/protocol.rs` | current wire request / response types |
| `crates/uffs-daemon/src/index.rs` | daemon search execution entry point |
| `crates/uffs-daemon/src/handler.rs` | RPC handling and request dispatch |
| `crates/uffs-cli/src/commands/search/daemon.rs` | CLI request builder for daemon path |
| `crates/uffs-cli/src/commands/search/dispatch.rs` | CLI config normalization |
| `crates/uffs-tui/src/app.rs` | TUI request builder and daemon search adapter call site |
| `crates/uffs-mcp/src/main.rs` | MCP request builder / daemon client caller |
| `crates/uffs-core/src/search/backend.rs` | search orchestration, sorting hookup |
| `crates/uffs-core/src/search/field.rs` | canonical field registry and alias parsing |
| `crates/uffs-core/src/search/derived.rs` | derived-field semantics (`path_only`, `type`, `tree_allocated`, `bulkiness`, `system_name`) |
| `crates/uffs-core/src/search/filters.rs` | current `SearchFilters` model |
| `crates/uffs-core/src/search/sorting.rs` | current sort parsing / execution |
| `crates/uffs-core/src/search/columns.rs` | `TuiColumn` type alias to `FieldId`, `DEFAULT_COLUMNS`, `parse_columns()` |
| `crates/uffs-core/src/output/column.rs` | `OutputColumn` type alias to `FieldId`, `PARITY_COLUMN_ORDER`, `CPP_COLUMN_ORDER` |
| `scripts/windows/cli-flag-validation.rs` | live Windows validation harness |

### Likely new files to create later

These names are recommendations; adjust only if review strongly prefers a different layout.

| Proposed file | Responsibility |
|---|---|
| `crates/uffs-core/src/search/predicate.rs` | `FieldPredicate`, `FilterOp`, predicate typing |
| `crates/uffs-core/src/search/request.rs` | canonical search request model |
| `crates/uffs-core/src/search/response.rs` | optional canonical response/projection types |
| `crates/uffs-core/src/search/cold.rs` | cold-field materialization helpers / planner |

Do not create these all at once unless needed. `field.rs` and `derived.rs` already exist; the remaining files should be introduced only if they still earn their keep after the transitional `SearchParams` / `SearchResponse` path settles.

## 6. Architecture Target

## 6.1 Target request shape

The final model should be conceptually equivalent to:

```text
UnifiedSearchRequest {
  pattern,
  match_options,
  scope,
  predicates: Vec<FieldPredicate>,
  sorts: Vec<SortSpec>,
  projection: ProjectionSpec,
  response: ResponseSpec,
  limit,
  drives,
  profile,
}
```

This does not require an immediate type rename away from `SearchParams`. We can evolve the current wire type incrementally.

## 6.2 Target field model

The canonical field system must include:

- `FieldId`
- `FieldMeta`
- `FieldType`
- `FieldAccess`

### Access tiers

| Tier | Meaning | Examples |
|---|---|---|
| `Hot` | available from compact record during search | size, allocated size, timestamps, descendants, extension id, flags |
| `Derived` | computed from hot data without additional disk I/O | path, path only, extension string, type, bulkiness, tree allocated |
| `Cold` | requires extra record materialization | namespace, reparse tag, security id, fn_* timestamps, forensic flags |

This tiering is the execution model. Hot work happens earliest, derived work next, cold work last.

## 6.3 Target operator model

The matrix should be implemented by field type, not by manual special casing.

| Field type | Operators |
|---|---|
| String | eq, contains, not_contains, starts_with, ends_with, regex, length_eq, length_lt, length_gt |
| Numeric | eq, ne, lt, lte, gt, gte, between |
| Timestamp | before, after, between, named_range |
| Bool | set, not_set |
| Enum / category | eq, ne, in |
| Bitmask | has_all, has_none, has_any |

## 6.4 Target response model

The daemon should own logical result shaping:

- filtering
- sorting
- projection
- field materialization
- grouping where needed

Recommended logical response modes:

- `Rows`
- `Json`
- `Csv`
- `Table`
- `CountOnly` (optional later)

Client code should only own surface-specific presentation and local file I/O.

## 7. Scope Boundaries

## 7.1 In scope

- unified request model
- unified field registry
- unified predicate model
- unified sort model
- daemon-owned projection / logical response shaping
- hot, derived, and cold field participation
- validation on live Windows systems

## 7.2 Out of scope for this initiative

- redesigning IPC transport from scratch
- changing the daemon lifecycle model
- changing MFT loading architecture
- GUI UX design
- a general text query language
- broad output-format redesign unrelated to unified field semantics

## 8. Design Rules

These rules are mandatory during implementation.

1. do not break hot-path performance to get architectural purity
2. do not delete `SearchFilters` until the compiled-predicate replacement is proven equal or faster
3. do not let each client invent its own field aliases or output semantics
4. do not add cold-field loading to hot-only queries
5. do not let legacy flags fan out into multiple translation paths
6. prefer additive compatibility, then cleanup later

## 9. Detailed Phase Plan

Each phase below contains:

- objective
- dependencies
- exact files to touch
- step-by-step tasks
- tests to add
- Windows validation tests to enable
- done criteria

### Phase 0 - Scope freeze and current-state normalization

**Objective:** start from the current codebase reality, not older architecture assumptions.

**Dependencies:** none.

**Files to touch:**

- `docs/architecture/UNIFIED_DAEMON_SEARCH_GAP_IMPLEMENTATION_PLAN.md`
- optionally a small follow-up note in `docs/architecture/FILTER_SORT_FEATURE_MATRIX.md` if terminology needs alignment

**Tasks:**

1. confirm the current wire request fields in `crates/uffs-client/src/protocol.rs`
2. confirm where CLI request translation happens in `crates/uffs-cli/src/commands/search/daemon.rs`
3. confirm where daemon execution begins in `crates/uffs-daemon/src/index.rs`
4. list all current field enums and column enums
5. list all current derived fields and whether they are output-only, sort-only, or fully filterable
6. record any naming mismatches between docs and code

**Tests to add:** none required beyond doc review.

**Windows validation to enable:** none.

**Done when:**

- this document matches the codebase naming and file map
- no phase below depends on a known-false assumption

### Phase 1 - Introduce `FieldId` and metadata ✅

**Status:** Complete (2026-04-04).

**Objective:** create one canonical field vocabulary.

**What was done:**

1. created `FieldId` enum (35 variants) covering all fields that can be filtered, sorted, or projected
2. created `FieldType` (String, Numeric, Timestamp, Bool, Enum, Bitmask), `FieldAccess` (Hot, Derived, Cold), `SortDirection`
3. created `FieldMeta` with canonical name, aliases, type, access tier, sortable/filterable/projectable flags, **plus** presentation metadata (`tui_label`, `display_name`, `df_column`, `default_value`)
4. added convenience methods: `cycle_next()`, `nearest_sort_field()`, `is_tree_field()`, `to_tree_column()`, `SORT_CYCLE`
5. **deleted** legacy enums `SortColumn`, `TuiColumn`, `OutputColumn` — replaced with type aliases to `FieldId`
6. **deleted** all `From`/`TryFrom` conversion glue (~200 lines)
7. updated all consumers: `backend.rs`, `sorting.rs`, `query.rs`, `columns.rs`, `output/column.rs`, `config.rs`, daemon `index.rs`, TUI `app.rs`/`ui.rs`/`columns.rs`/`backend.rs`

**Tests:** alias parsing, metadata round-trip, sort direction, presentation fields, cycle, tree field detection — all pass.

**Done criteria met:**

- ✅ all user-visible sortable / outputtable fields map to `FieldId`
- ✅ no code switches on multiple field enums — `FieldId` is the sole field type

### Phase 2 - Evolve `SearchParams` into the canonical request contract

**Objective:** the wire contract describes user intent, not legacy flag shape.

**Status note (2026-04-04):** substantially complete. `SearchParams` now carries canonical sorts, predicates, projection fields, and response mode metadata. Client request builders call `populate_canonical_fields()`. Legacy flags translate in one place. With Phase 8 complete (one field model), the "canonical fields" and "legacy fields" are now the same type — just `FieldId`. Remaining work: decide whether to strip the transitional compatibility fields or keep them as ergonomic aliases.

**Dependencies:** Phase 1.

**Files to touch:**

- `crates/uffs-client/src/protocol.rs`
- optionally create `crates/uffs-core/src/search/request.rs`
- `crates/uffs-daemon/src/handler.rs`
- `crates/uffs-cli/src/commands/search/daemon.rs`
- `crates/uffs-cli/src/commands/search/dispatch.rs`
- `crates/uffs-tui/src/app.rs`
- `crates/uffs-mcp/src/main.rs`

**Implementation steps:**

1. add canonical request sub-structures:
   - `SortSpec`
   - `ProjectionSpec`
   - `ResponseSpec`
2. add `sorts: Vec<SortSpec>` to the wire contract
3. add projection fields in terms of `FieldId`
4. add response-mode metadata
5. keep legacy request fields temporarily for compatibility
6. add one translation point that converts legacy fields into the canonical form
7. update CLI request building to populate the canonical fields first
8. update daemon request handling to prefer canonical fields when present

**Important rule:** legacy flags must translate in one place only.

**Tests to add:**

- serde round-trip tests for the expanded request
- compatibility tests for old-style request fields
- CLI request-builder tests for canonical translation

**Windows validation to enable later:**

- `U100` single canonical sort field
- `U101` multi-sort request contract
- `U102` projection request contract
- `U104` response-mode request contract

**Done when:**

- one request can express pattern + predicates + sorts + projection + response mode
- CLI and TUI can target the same conceptual request shape

### Phase 3 - Predicate compiler and hot-field execution

**Objective:** replace bespoke filter wiring with a typed predicate model while preserving hot-path speed.

**Status note (2026-04-04):** substantially complete. `predicates_require_post_filter` now uses `FieldAccess` metadata to classify predicates by bucket. `compile_predicates_into_filters` compiles hot-path size/descendant bounds into `SearchFilters`. All 17 bool-typed attribute fields are wired through `matches_predicate`. Remaining: full cold-field predicate planner (blocked on Phase 6).

**Dependencies:** Phases 1 and 2.

**Files to touch:**

- create `crates/uffs-core/src/search/predicate.rs`
- `crates/uffs-core/src/search/filters.rs`
- `crates/uffs-daemon/src/index.rs`
- `crates/uffs-core/src/search/backend.rs`
- `crates/uffs-core/src/search/sorting.rs`

**Implementation steps:**

1. define `FieldPredicate`
2. define `FilterOp`
3. define a compile step from request predicates into execution buckets:
   - hot predicates
   - derived predicates
   - cold predicates
4. keep `SearchFilters` as the compiled hot-path representation unless measurement proves a replacement is safe
5. convert current flag-shaped filters into predicates:
   - size bounds
   - descendant bounds
   - ext filters
   - hide system
   - attr filters
   - exclude pattern
   - current date bounds
6. route multi-sort through canonical `Vec<SortSpec>`

**Do not do in this phase:** derived field materialization or cold-field loading.

**Tests to add:**

- predicate compilation tests
- hot-predicate execution tests
- multi-sort compilation tests
- regression tests for existing CLI flags mapping into predicates

**Windows validation to enable:**

- `U206` / `U207` extension filters
- `U300` / `U301` / `U302` size comparisons
- `U304` / `U305` descendants comparisons
- `U500`..`U506` boolean and attribute predicates
- combined hot-predicate regression derived from `T34`
- `U101` multi-sort hot fields

**Done when:**

- hot-field filters are expressed through canonical predicates
- existing hot queries still run through optimized execution
- no performance cliff is introduced for common filters

### Phase 4 - Derived fields

**Objective:** make derived fields first-class for filter, sort, and projection.

**Status note (2026-04-04):** in progress. `derived.rs` is landed and daemon/core paths now recognize `path_only`, semantic `type`, `tree_allocated`, and `bulkiness`. All these fields are now `FieldId` variants with full metadata. Remaining work is mainly richer operator support and validation breadth. `columns.rs` and `output/column.rs` are now type aliases — no separate enum work needed.

**Dependencies:** Phase 3.

**Files to touch:**

- update `crates/uffs-core/src/search/derived.rs`
- `crates/uffs-core/src/search/backend.rs`
- `crates/uffs-core/src/search/sorting.rs`

**Implementation steps:**

1. make `PathOnly` a real field, not just an output convenience
2. normalize `Type` into one category model shared by sort / display / output
3. define how extension string is derived and normalized
4. implement derived numeric fields:
   - `Bulkiness = allocated / size` with zero-size handling documented
   - `TreeAllocated` with clear subtree semantics documented
5. add string-length operators for string-like derived fields
6. materialize derived sort keys without unnecessary allocation inside hot loops

**Tests to add:**

- path-only extraction tests
- type categorization tests
- bulkiness calculation tests
- tree-allocated calculation tests
- derived sort correctness tests

**Windows validation to enable:**

- `U208` sort path asc / desc
- `U205` path only filters
- `U209` sort extension asc / desc
- `U206` / `U207` extension filters and bundles
- `U211` type sort
- `U308` sort bulkiness asc / desc
- `U309` sort tree allocated asc / desc
- `U210` plus new length variants for path / path only / extension

**Done when:**

- derived fields participate in filtering, sorting, and projection with one semantic definition

### Phase 5 - Time grammar expansion

**Objective:** support the target date/time feature family without introducing a query DSL.

**Dependencies:** Phase 3.

**Files to touch:**

- `crates/uffs-core/src/search/predicate.rs`
- current date/time parsing helpers used by `SearchFilters`
- CLI parsing glue where named time ranges enter the request
- request / predicate unit-test files

**Implementation steps:**

1. define named time-range variants in the canonical operator model
2. resolve all named ranges into absolute bounds before execution
3. support:
   - before
   - after
   - between
   - today
   - yesterday
   - year to date
   - this / last / next day
   - this / last / next week
   - this / last / next month
   - this / last / next year
   - month-name ranges where useful
4. ensure timezone handling is documented and testable

**Tests to add:**

- named-range parsing tests
- absolute-bound resolution tests
- inclusive / exclusive boundary tests
- created / modified / accessed parity tests

**Windows validation to enable:**

- `U400` today / yesterday
- `U402` year-to-date
- `U403` this / last week
- `U404` this month
- `U406` january-style month range
- `U405` before / after / between parity across created / modified / accessed

**Done when:**

- all target time operators compile to plain bound checks at execution time

### Phase 6 - Cold-field integration

**Objective:** enable cold fields without hurting hot-only queries.

**Status note (2026-04-04):** not started. This remains the largest functional gap after the canonical request, derived-field, and response-shaping work that is already in the tree.

**Dependencies:** Phases 3 and 4. Phase 5 is optional if the first cold rollout does not depend on richer time grammar.

**Files to touch:**

- create `crates/uffs-core/src/search/cold.rs`
- cold-data reader integration points already present in `uffs-core` / related crates
- `crates/uffs-daemon/src/index.rs`
- result materialization code paths

**Implementation steps:**

1. define cold `FieldId`s and metadata
2. define planner rules for when cold data is needed
3. perform hot filtering first
4. materialize cold fields only for remaining candidates
5. apply cold predicates after cold materialization
6. re-sort only if a cold sort key is present
7. document cost expectations clearly

**Tests to add:**

- cold-field materialization tests
- cold-field filter tests
- mixed hot + cold query tests
- cold-sort tests
- no-cold-needed short-circuit tests

**Windows validation to enable:**

- `U700` project cold fields
- `U701` filter on cold field
- `U702` cold sort single key
- `U704` mixed hot filter + cold sort
- `U703` mixed hot filter + cold projection
- `U904` hot-only query regression guard

**Done when:**

- cold fields can participate without contaminating hot-only query performance

### Phase 7 - Daemon-owned projection and logical response shaping

**Objective:** finish the “client asks for what it wants back” model.

**Status note (2026-04-04):** in progress. The daemon can now apply canonical projection fields, return effective projection/sort metadata, and shape direct responses as `Rows`, `Json`, `Csv`, or `Table`. The remaining work is to extend this over cold fields and reduce client-local reinterpretation of field semantics.

**Dependencies:** Phases 2 through 6.

**Files to touch:**

- daemon response-building code
- `crates/uffs-client/src/protocol.rs`
- `crates/uffs-cli/src/commands/output/mod.rs`
- any shared-memory response helpers involved in bulk results

**Implementation steps:**

1. add daemon-side field projection based on `FieldId`
2. add response-mode handling for `Rows`, `Json`, `Csv`, `Table`
3. keep `Rows` as canonical transport for TUI / GUI / MCP
4. decide whether `Table` is daemon-native or a CLI rendering over daemon rows
5. ensure client output code stops reinterpreting field semantics locally

**Tests to add:**

- projection ordering tests
- projection subset tests
- JSON / CSV / table logical parity tests
- shmem response-path tests for large projected results

**Windows validation to enable:**

- `U800` rows vs csv parity
- `U801` rows vs json parity
- `U802` projection subset parity across response modes
- `U804` large projected result / bulk transport coverage
- `U803` output file parity for projected fields

**Done when:**

- the daemon owns logical field semantics for all response modes

### Phase 8 - Legacy cleanup ✅

**Status:** Complete (2026-04-04). Pulled forward before Phases 5–7 because `FieldId` was a strict superset.

**Objective:** delete the transitional duplication after the canonical model is proven.

**What was done:**

1. deleted `SortColumn` enum (14 variants) — replaced with `type SortColumn = FieldId`
2. deleted `TuiColumn` enum (31 variants) — replaced with `type TuiColumn = FieldId`
3. deleted `OutputColumn` enum (31 variants) — replaced with `type OutputColumn = FieldId`
4. deleted all `From`/`TryFrom` conversion glue (~200 lines of match arms)
5. enriched `FieldMeta` with presentation fields (`tui_label`, `display_name`, `df_column`, `default_value`)
6. added `FieldId` methods: `cycle_next()`, `nearest_sort_field()`, `is_tree_field()`, `to_tree_column()`, `SORT_CYCLE`
7. updated all consumers across `uffs-core`, `uffs-daemon`, `uffs-tui`, `uffs-cli`, `uffs-mcp`
8. full workspace `cargo check` — zero errors, zero warnings
9. full workspace `cargo test --lib` — all tests pass

**Done criteria met:**

- ✅ one canonical semantic path from request to daemon execution to response
- ✅ `FieldId` is the sole field type — no duplicate enums remain

### Phase 9 - Windows live validation rollout ✅

**Status:** Complete (2026-04-04). Script ready — requires Windows + admin to execute.

**Objective:** make sure the final implementation is verifiable on a real NTFS Windows system.

**What was done:**

Expanded `scripts/windows/cli-flag-validation.rs` from 44 → **89 tests** (T00–T88) organized into 8 named suites. All tests run at three caching levels (COLD / WARM CACHE / HOT) with cross-level timing comparison.

**Test suite registry (see below for full list):**

| Suite | Tests | What it validates | Phase |
|---|---|---|---|
| Baseline | T00–T34 | Original CLI flags, sort, ext, size, attr, exclude, format, columns | 0 |
| Extended Baseline | T35–T43 | Unlimited, older-created, attr system/readonly/combined, empty results, header, smart-case, newer-accessed | 2,3 |
| Time Grammar | T44–T57 | Named ranges: today, yesterday, this_week, last_7d/30d/90d/365d, this_month/year, last_week/month/year, ISO date, bounded time ranges | 5 |
| Sort Fields | T58–T67 | All sortable FieldId variants: name, path, created, accessed, extension, drive, allocated, descendants, multi-sort 2-field, multi-sort 3-field | 1,3 |
| Bool Attributes | T68–T74 | archive, sparse, reparse, offline, encrypted, !system (exclude), hidden+system (combined HasAll) | 3 |
| Combined Stress | T75–T78 | Size+time+ext, dirs+desc range+sort, hidden+time, exclude+ext+size | 3,5 |
| Projection + Modes | T79–T85 | columns+json, columns all (wide), created-this_year, accessed-last_week, multi-sort 3-field, mega combined, table+projection | 7 |
| Edge Cases | T86–T88 | older-accessed, ext+sort-modified, name-only+hide-system+time | 2,3,5 |

**Done criteria met:**

- ✅ 3-phase cold/warm/hot structure preserved
- ✅ original 34 baseline tests preserved unchanged
- ✅ 45 new tests added covering all new infrastructure
- ✅ invariant-based assertions (no machine-specific row counts)
- ✅ cross-level timing comparison for performance regression detection

**Full test registry:**

| ID | Name | Validates | Key Assertion |
|---|---|---|---|
| T00 | Warmup / daemon alive | Daemon health | ≥1 row returned |
| T01 | --files-only | File filter | All rows have Directory Flag=0 |
| T02 | --dirs-only | Dir filter | All rows have Directory Flag=1 |
| T03 | --hide-system | System name filter | No $ prefix |
| T04 | --ext rs | Single ext filter | All .rs |
| T05 | --ext jpg,png,gif | Multi ext filter | All in ext set |
| T06 | --min-size 100MB | Size lower bound | All ≥ 100MB |
| T07 | --max-size 1KB | Size upper bound | All ≤ 1KB |
| T08 | --min/max-size 1MB..10MB | Size range | All in range |
| T09 | --sort size asc | Sort ascending | Monotonic asc |
| T10 | --sort size desc | Sort descending | Monotonic desc |
| T11 | --sort modified | Sort by timestamp | No crash, rows returned |
| T12 | --sort size,name | Multi-sort (2 fields) | No crash |
| T13 | --attr hidden | Attr require | Hidden=1 |
| T14 | --attr !hidden | Attr exclude | Hidden≠1 |
| T15 | --attr compressed | Attr compressed | Compressed=1 |
| T16 | --exclude backup* | Exclude glob | No "backup*" names |
| T17 | --name-only | Name substring | Contains "readme" |
| T18 | --case sensitive | Case-sensitive match | Exact case |
| T19 | --word | Whole-word match | No crash |
| T20 | --format json | JSON response mode | Valid NDJSON, ≤ limit |
| T21 | --format table | Table response mode | Non-empty output |
| T22 | --columns selective | Projection | ≤ 5 columns |
| T23 | --min-descendants 100 | Desc lower bound | All ≥ 100 |
| T24 | --max-descendants 0 | Empty dirs | All = 0 |
| T25 | --newer 7d | Time duration | No crash |
| T26 | --older 365d | Time duration | No crash |
| T27 | --newer-created 30d | Created time | No crash |
| T28 | --drive C | Drive filter | All paths on C: |
| T29 | --drives C,D | Multi-drive | All paths on C: or D: |
| T30 | --sep \| --quotes ' | Custom delimiters | Pipe in header |
| T31 | --out file | File output | File created with data |
| T32 | --benchmark | Benchmark mode | Exits clean |
| T33 | regex >.*\\.config$ | Regex pattern | All .config |
| T34 | Combined stress | Multi-constraint | Size desc, ≥ 1MB, correct ext |
| T35 | --limit 0 | Unlimited | ≥ 100 DLLs |
| T36 | --older-created 365d | Created upper bound | No crash |
| T37 | --attr system | System attr | System=1 |
| T38 | --attr readonly | Readonly attr | Read-only=1 |
| T39 | --attr system,!hidden | Combined HasAll+HasNone | System=1, Hidden≠1 |
| T40 | No results | Empty set grace | 0 rows, no crash |
| T41 | --header false | Header suppression | No header line |
| T42 | --smart-case | Smart case matching | Mixed case results |
| T43 | --newer-accessed 7d | Accessed time | No crash |
| T44 | --newer today | Named range: today | No crash |
| T45 | --newer yesterday | Named range: yesterday | No crash |
| T46 | --newer this_week | Named range: this_week | No crash |
| T47 | --newer last_7d | Named range: last_7d | No crash |
| T48 | --newer last_30d | Named range: last_30d | No crash |
| T49 | --newer this_month | Named range: this_month | No crash |
| T50 | --newer this_year | Named range: this_year | No crash |
| T51 | --older last_year | Named range: last_year | No crash |
| T52 | --newer last_90d | Named range: last_90d | No crash |
| T53 | --newer last_365d | Named range: last_365d | No crash |
| T54 | --newer-created today | Created + named range | No crash |
| T55 | --newer-accessed this_week | Accessed + named range | No crash |
| T56 | Bounded time range | newer last_week + older this_week | No crash |
| T57 | --newer 2025-01-01 | ISO date bound | No crash |
| T58 | --sort name | Sort by name | Ascending order |
| T59 | --sort path | Sort by path | No crash |
| T60 | --sort created | Sort by created | No crash |
| T61 | --sort accessed | Sort by accessed | No crash |
| T62 | --sort extension | Sort by extension | No crash |
| T63 | --sort drive | Sort by drive | No crash |
| T64 | --sort allocated desc | Sort SizeOnDisk | No crash |
| T65 | --sort descendants desc | Sort desc count | Monotonic desc |
| T66 | Multi-sort size,-name | 2-field multi-sort | No crash |
| T67 | Multi-sort -modified,name | 2-field multi-sort | No crash |
| T68 | --attr archive | Archive bool | Archive=1 |
| T69 | --attr sparse | Sparse bool | Sparse=1 (may be empty) |
| T70 | --attr reparse | Reparse bool | Reparse=1 |
| T71 | --attr offline | Offline bool | Offline=1 (may be empty) |
| T72 | --attr encrypted | Encrypted bool | Encrypted=1 (may be empty) |
| T73 | --attr !system | System exclude | System≠1 |
| T74 | --attr hidden,system | Combined HasAll | Both=1 |
| T75 | Size+time+ext | 3-constraint combo | All constraints verified |
| T76 | Dirs+desc range+sort | Bounded desc + sort | Range + monotonic |
| T77 | Hidden+newer 30d | Attr + time | Hidden=1 |
| T78 | Exclude+ext+size | 3-constraint combo | All constraints verified |
| T79 | Projection+json | Columns + format | Projected fields in JSON |
| T80 | --columns all | Wide output | ≥ 15 columns |
| T81 | --newer-created this_year | Created + named | No crash |
| T82 | --newer-accessed last_week | Accessed + named | No crash |
| T83 | Multi-sort 3-field | drive,ext,-size | No crash |
| T84 | Mega combined | 8 constraints | Size range + !hidden + !system + time + sort + drive + projection |
| T85 | Table+projection | Format + columns | Non-empty table |
| T86 | --older-accessed 365d | Accessed upper | No crash |
| T87 | Ext+sort modified | Filter + sort combo | Correct extensions |
| T88 | name+hide-system+time | 3-way combo | Contains "config", no $ prefix |

## 10. Implementation Order Recommendation

Follow this order unless a reviewer explicitly redirects it:

1. Phase 0
2. Phase 1 ✅
3. Phase 8 ✅ (pulled forward — legacy enum removal)
4. Phase 3 (substantially complete)
5. Phase 2
6. Phase 4
7. Phase 5
8. Phase 6
9. Phase 7
10. Phase 9 continuously during rollout, then full pass at the end

Why this order:

- Phase 1 + 8 were done first to establish one field model
- Phase 3 predicate compiler uses `FieldAccess` metadata from Phase 1
- it preserves hot-path performance work early
- it postpones cold-field complexity until the request / predicate model is stable

## 11. Validation Strategy

Validation should be layered.

### 11.1 Unit tests

Required at each phase:

- parsing tests
- request translation tests
- execution tests
- boundary-condition tests

### 11.2 Integration tests

Required where request translation and daemon execution meet:

- CLI request builder -> protocol payload
- protocol payload -> daemon execution plan
- daemon execution plan -> projected response

### 11.3 Live Windows validation

The Windows script must validate:

- correctness invariants
- response-mode parity
- performance guardrails for hot-path queries
- cold / warm-cache / hot behavior

### 11.4 Performance rule

Hot-only queries must not become materially slower just because the unified model exists.

That means:

- no cold materialization when not requested
- no per-row dynamic dispatch if a compiled hot predicate exists
- no repeated string normalization inside inner loops if it can be precompiled

## 12. Detailed Plan For `scripts/windows/cli-flag-validation.rs`

The current script is a good baseline. Do not replace it from scratch. Expand it.

## 12.1 Keep the current script strengths

Retain these existing behaviors:

- three cache levels: cold / warm cache / hot
- one summary line per test
- cross-level timing comparison
- invariant-based checks instead of exact machine-specific output snapshots

## 12.2 Script changes required

Add the following capabilities to the script itself.

### Structural improvements

1. split tests into named suites
2. add `--suite <name>` and `--list-suites`
3. add `--test-prefix <prefix>` for fast iteration
4. add `--no-perf` to skip timing guard checks when debugging correctness only
5. add helpers for:
   - comparing row sets across response modes
   - verifying projection columns by header
   - verifying sorted order by typed field
   - verifying date windows from returned columns
   - verifying boolean flag columns

### Suite names to add

| Suite | Purpose |
|---|---|
| `baseline` | keep current 44-ish tests and regressions |
| `contract` | request / projection / response-mode coverage |
| `string_fields` | path / name / path only / extension / type filters and sorts |
| `numeric_fields` | size / size on disk / descendants / derived numeric sorts |
| `time_fields` | created / modified / accessed named ranges |
| `bool_fields` | readonly / archive / system / hidden / compressed / etc. |
| `bundles` | preset extension bundles and attribute bundles |
| `cold_fields` | extra-record field projection / filter / sort |
| `parity` | response-mode parity and projection parity |
| `performance` | hot-path guardrails |

## 12.3 New live-validation tests to add

These are recommended new IDs. Keep the existing `T00..T43` baseline tests intact, then add a new range so old regressions remain comparable.

### Contract and projection suite

| ID | Example intent | What to verify |
|---|---|---|
| `U100` | single canonical sort field | request executes and results are sorted correctly |
| `U101` | multi-sort `size,name` | primary and tie-break ordering both hold |
| `U102` | projection `Name,Size,Path Only` | only requested columns appear |
| `U103` | projection with derived field | derived column is present and populated |
| `U104` | response mode json | parsed items contain requested fields only |
| `U105` | response mode csv | header and rows match requested projection |
| `U106` | response mode table | logical columns match requested projection |

### String field suite

| ID | Example intent | What to verify |
|---|---|---|
| `U200` | `Name contains readme` | every row name contains `readme` case policy respected |
| `U201` | `Name starts with app` | every row name starts with prefix |
| `U202` | `Name ends with .dll` | every row name ends with suffix |
| `U203` | `Name does not contain temp` | no row contains forbidden token |
| `U204` | `Path contains \Windows\` | every row path matches condition |
| `U205` | `Path Only starts with C:\Users` | parent path only is validated |
| `U206` | `Extension equals rs` | extension field matches exactly |
| `U207` | `Extension in jpg,png,gif` | extension bundle expansion works |
| `U208` | sort by `Path` asc / desc | monotonic order holds |
| `U209` | sort by `Extension` asc / desc | monotonic order holds |
| `U210` | string length filter on `Name` | every row length satisfies bound |
| `U211` | sort by `Type` asc / desc | category ordering is monotonic and stable |

### Numeric field suite

| ID | Example intent | What to verify |
|---|---|---|
| `U300` | `Size < X` | every row size satisfies bound |
| `U301` | `Size = X` | every row size matches exact value when rows exist |
| `U302` | `Size > X` | every row size satisfies bound |
| `U303` | `Size On Disk > X` | allocated-size semantics are correct |
| `U304` | `Descendants = 0` | empty dirs only |
| `U305` | `Descendants > 100` | large dirs only |
| `U306` | sort by `Size On Disk` | monotonic order holds |
| `U307` | sort by `Descendants` | monotonic order holds |
| `U308` | sort by `Bulkiness` | monotonic order holds when field exists |
| `U309` | sort by `TreeAllocated` | monotonic order holds when field exists |

### Time field suite

| ID | Example intent | What to verify |
|---|---|---|
| `U400` | modified `today` | all rows fall inside today window |
| `U401` | modified `yesterday` | all rows fall inside yesterday window |
| `U402` | created `year to date` | all rows >= Jan 1 of current year |
| `U403` | accessed `last week` | all rows inside last-week window |
| `U404` | modified `this month` | all rows inside current-month window |
| `U405` | created `between A and B` | rows satisfy explicit bounds |
| `U406` | modified `january` | all rows fall inside January window for the resolved year |

### Boolean / attribute suite

| ID | Example intent | What to verify |
|---|---|---|
| `U500` | readonly set | all rows have readonly flag |
| `U501` | readonly not set | no rows have readonly flag |
| `U502` | hidden set | all rows hidden |
| `U503` | system set and not hidden | both constraints hold |
| `U504` | compressed set | all rows compressed when rows exist |
| `U505` | encrypted set | all rows encrypted when rows exist |
| `U506` | reparse set | all rows reparse points when rows exist |

### Bundle suite

| ID | Example intent | What to verify |
|---|---|---|
| `U600` | preset `documents` | extensions belong to allowed document set |
| `U601` | preset `images` | extensions belong to image set |
| `U602` | preset `system files` | expected attribute bundle holds |
| `U603` | preset + extra predicate | both bundle and additional filter hold |

### Cold-field suite

| ID | Example intent | What to verify |
|---|---|---|
| `U700` | project cold field `namespace` | column exists and values are populated / parseable |
| `U701` | filter cold field | every row satisfies cold predicate |
| `U702` | sort cold field | order is monotonic |
| `U703` | hot filter + cold projection | semantics hold and query still succeeds |
| `U704` | hot filter + cold sort | semantics hold and query still succeeds |

### Parity suite

| ID | Example intent | What to verify |
|---|---|---|
| `U800` | same search as rows + csv | same logical rows by path set |
| `U801` | same search as rows + json | same logical rows by path set |
| `U802` | same search as csv + table | projected columns agree logically |
| `U803` | output file parity | file output matches console logical content |
| `U804` | large projected result / bulk transport | query succeeds and result transport path remains correct |

### Performance suite

| ID | Example intent | What to verify |
|---|---|---|
| `U900` | hot ext filter | should remain in hot-path timing band |
| `U901` | hot size filter | should remain in hot-path timing band |
| `U902` | hot multi-sort | should not regress sharply |
| `U903` | cold projection query | allowed to be slower, but must succeed consistently |
| `U904` | hot-only query with no cold fields | must not trigger cold-path penalty |

## 12.4 Validation philosophy for live Windows tests

The script must avoid brittle assumptions.

### Good assertions

- every returned row satisfies the requested predicate
- results are monotonic for requested sort
- projected columns are present and ordered correctly
- logical row sets match across response modes
- hot-path queries remain within a reasonable performance band

### Bad assertions

- exact row counts on arbitrary user machines
- exact paths that may not exist everywhere
- exact timing thresholds that ignore machine differences

## 12.5 Suggested script refactor order

Do the script changes in this order:

1. extract current tests into suites without changing behavior
2. add suite selection CLI flags
3. add parity helpers and typed sort helpers
4. add new contract / string / numeric suites
5. add time / bundle suites
6. add cold-field and parity suites
7. add performance guard suite last

## 13. Definition of Done

This initiative is complete when all of the following are true:

1. every client sends the same conceptual request model
2. field identity is unified under `FieldId`
3. the daemon is the only place where filter / sort / projection semantics live
4. hot, derived, and cold fields all participate correctly according to access tier
5. the target matrix is covered by type-driven operators rather than bespoke flag wiring
6. the expanded Windows validation suite passes on a live NTFS Windows system

## 14. Bottom Line

The architecture direction is already correct. The remaining work is to finish the abstraction boundary and the execution contract:

- one request model
- one field model
- one daemon execution engine

This document is the implementation tracker for getting there.