# UFFS MCP Frontend Phase-by-Phase Implementation Specification

Date: 2026-04-08  
Format: Markdown  
Audience: UFFS core, daemon, MCP, CLI, TUI, GUI, release owners  
Status: Proposed implementation spec

---

## 1. Executive summary

This document turns the prior UFFS MCP architecture proposal into an execution-grade implementation spec.

The recommendation is:

1. **Keep the MCP frontend daemon-native.** `uffs-mcp` should talk to `uffs-client`, which talks to `uffs-daemon`. It should not shell out to `uffs` for normal tool execution.
2. **Use the official Rust MCP SDK as the protocol boundary, not as the semantic boundary.** The current official Rust SDK (`rmcp`) is listed as an official Tier 2 SDK and supports server/client functionality, tools/resources/prompts, schema generation, stdio transport, and Streamable HTTP transport. That makes it a good outer protocol layer, but UFFS should still keep its own internal abstractions so the daemon/query model does not depend on SDK churn.
3. **Keep one canonical UFFS query model.** Search, aggregate, facet values, info, and duplicate verification should remain daemon-owned semantics expressed through `uffs-client::protocol`, not redefined inside MCP.
4. **Ship MCP in layers.** First make `uffs.search`, `uffs.info`, `uffs.drives`, and `uffs.status` production-grade over stdio. Then add daemon-native aggregation/facets. Then add resources/prompts/roots awareness. Then add tasks for long-running work. Then add a Streamable HTTP gateway.
5. **Do not leak CLI or shmem internals into MCP.** MCP should expose bounded rows, structured aggregates, resource links, cursors, and tasks. It should not hand an LLM a raw shared-memory path or a human-oriented CLI dump.

This direction fits the current UFFS architecture well. Internal docs already describe UFFS as a unified daemon system with `uffs-daemon` as the execution backend, `uffs-client` as the thin transport library, and `uffs-mcp` as a stdio adapter over the daemon. Warm queries are already in the right range for a serious MCP service: the daemon architecture docs report about **9 ms median warm latency**, about **12.4 s cold start** from `.uffs` cache for 7 drives, and about **7.3 GiB steady-state memory** with **0 GiB** when the daemon retires. Internal docs also already position shared-memory handoff as the correct bulk-result path for non-MCP surfaces. Internal sources: `DAEMON_SERVICE_ARCHITECTURE.md`, `daemon.md`, `FILTER_SORT_FEATURE_MATRIX.md`.

This direction also fits the current MCP surface. The current MCP specification defines **stdio** and **Streamable HTTP** as the two standard transports, uses **tools**, **resources**, and **prompts** as the core server primitives, supports **roots** from clients, allows tool results to return **structuredContent**, **resource_link**, and optional **outputSchema**, and now includes experimental **tasks** for deferred result retrieval. MCP also defines tool annotations such as `readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`, and execution metadata such as `taskSupport`.

---

## 2. Source basis for this spec

### 2.1 Internal UFFS design anchors

This implementation spec is grounded in the following uploaded internal materials:

- `DAEMON_SERVICE_ARCHITECTURE.md`
- `daemon.md`
- `FILTER_SORT_FEATURE_MATRIX.md`
- `UFFS_AGGREGATION_ARCHITECTURE_CONSOLIDATED.md`
- `AGGREGATION_ARCHITECTURE.md`
- `cli-overview.md`
- `filters.md`
- `search-modes.md`
- `sorting.md`
- `api-validation.rs`
- `cli-flag-validation.rs`
- prior generated architecture proposal: `UFFS_MCP_frontend_architecture_proposal_2026-04-08.md`

### 2.2 External protocol anchors

These official MCP references shape the protocol-facing recommendations:

- MCP SDKs page: official SDK availability and tiering
- MCP specification overview
- MCP transports
- MCP tools
- MCP schema reference
- MCP roots
- MCP tasks
- MCP roadmap

Reference URLs are listed in Appendix B.

---

## 3. What is already true in UFFS

### 3.1 The backend shape is already correct for MCP

UFFS is already structured around a daemon-centric backend:

- `uffs-daemon` holds the compact index and executes queries.
- `uffs-client` abstracts daemon connection, auto-start, reconnect, keepalive, and shmem bulk transfer.
- `uffs-mcp` already exists as a thin adapter over `uffs-client`.
- CLI is daemon-only.
- TUI is effectively daemon-first with remaining UX polish.

That means UFFS does **not** need an MCP-specific search engine. It needs a better MCP presentation and control layer over the daemon it already has.

### 3.2 The current measured performance is already MCP-viable

The internal daemon docs report:

- warm query latency: about **9 ms median**
- cold start from cache: about **12.4 s** for 7 drives
- steady-state daemon memory: about **7.3 GiB**
- peak memory during load: about **10 GiB**
- zero idle memory after daemon retirement

That performance profile is good for a persistent local MCP server. It is **not** good for a shell-out-per-request design. This is the single strongest practical reason to keep MCP daemon-native.

### 3.3 The internal consolidation work already points to one canonical request model

The internal docs are very clear about direction:

- all surfaces should converge on `SearchParams -> daemon -> results`
- the old CLI/TUI standalone duplication should disappear
- `FieldId`, `FieldMeta`, and `AggregateMeta` should become the semantic source of truth
- aggregation should be a first-class response path inside the same daemon-owned search contract

This implementation spec follows that direction rather than inventing a parallel one.

---

## 4. Core architectural decisions

## 4.1 Decision: do not build the production MCP path as a CLI wrapper

### Rejected end state

```text
LLM host -> MCP wrapper -> spawn `uffs` CLI -> parse stdout/stderr -> respond
```

### Why rejected

- extra process spawn and CLI framing cost become visible to users
- hard to preserve warm-state advantages
- difficult to express typed `outputSchema`
- awkward for resources/prompts/roots/tasks
- hard to avoid human-oriented output conventions bleeding into model-facing responses
- cannot cleanly expose exactness/truncation/cursor/task metadata
- bulk shared-memory paths are not suitable for direct model consumption

### Allowed bootstrap use

A CLI-backed fallback can still exist behind a debug feature or emergency compatibility mode, but it should not be the production default.

## 4.2 Decision: use the official Rust MCP SDK as the outer protocol layer

### Recommendation

Use the official Rust MCP SDK (`rmcp`) for:

- stdio transport
- Streamable HTTP transport when added
- tool/resource/prompt registration
- schema plumbing
- tasks integration when adopted

But keep a thin UFFS-owned adapter boundary so that:

- the daemon contract remains independent of the SDK
- SDK changes do not force a semantic rewrite
- a custom MCP boundary remains possible if needed later

### Practical consequence

`uffs-mcp` should have two layers:

```text
rmcp-facing layer         -> protocol mechanics
UFFS adapter layer        -> UFFS semantics + policy + shaping
uffs-client              -> daemon transport
uffs-daemon              -> execution
```

### Why this is the right compromise

The official Rust SDK already exists and covers the protocol features UFFS wants. But since it is still Tier 2 rather than Tier 1, the safest design is to treat it as a replaceable shell, not as the place where UFFS semantics live.

## 4.3 Decision: keep one canonical UFFS request model

### Recommendation

The daemon should continue converging on a single typed request surface.

For MCP that means:

- `uffs.search` maps to canonical `SearchParams`
- `uffs.aggregate` maps to canonical aggregation embedded in `SearchParams` or to a thin `aggregate` alias that compiles to that same shape
- `uffs.facet_values` maps to canonical facet value search over the same field catalog
- `uffs.info` maps to a daemon info/read path
- `uffs.duplicate_verify` maps to daemon job/task infrastructure

### Strong recommendation on internal aliasing

Use **one canonical internal model**, but allow **two public JSON-RPC method names** if that improves clarity:

- canonical internal model: `SearchParams { aggregations: ... }`
- optional convenience daemon alias: `aggregate(params)`

That gives you ergonomic public methods without splitting the engine.

## 4.4 Decision: expose more than tools, but do it in phases

### End-state UFFS MCP surface

- **Tools** for execution and retrieval
- **Resources** for stable schema and metadata documents
- **Prompts** for high-value analysis/report flows
- **Roots** awareness for workspace-bound searches when supported by the client
- **Tasks** for expensive operations

### But not all at once

The rollout should be:

1. tools first
2. structured schemas and result shaping
3. resources and roots-aware policy
4. prompts
5. tasks
6. Streamable HTTP gateway

## 4.5 Decision: MCP must never expose raw shmem paths

Shared-memory result transfer is the right solution for CLI and possibly GUI/TUI, but it is the wrong abstraction for LLM-facing tool results.

### MCP result sizing policy

- **small search results:** return inline rows
- **small/medium aggregates:** return inline structured content
- **large result sets:** return cursor + summary + optional resource link
- **very expensive operations:** return task handle

Never return:

- raw `/dev/shm/...` paths
- raw `CreateFileMapping` names
- OS-specific handle metadata
- giant row dumps by default

## 4.6 Decision: read-only MCP by default

The default advertised tool set should be read-only and analysis-oriented.

### Default public tools

- `uffs.search`
- `uffs.aggregate`
- `uffs.facet_values`
- `uffs.info`
- `uffs.drives`
- `uffs.status`

### Admin or maintenance tools

These should be hidden by default, gated by config, or exposed only in explicit admin mode:

- `uffs.refresh_index`
- `uffs.warmup`
- `uffs.shutdown_daemon`

Even though they do not modify the filesystem, they do modify process state and can degrade user experience if a model calls them casually.

---

## 5. End-state architecture

```text
LLM host / IDE / assistant
    |
    | MCP
    |  - stdio first
    |  - Streamable HTTP later
    v
uffs-mcp
    |
    | protocol shell (rmcp)
    | adapter layer (UFFS-owned)
    | - tool registry
    | - schema generation
    | - policy / roots scoping
    | - result shaping
    | - cursor + task mapping
    v
uffs-client
    |
    | local JSON-RPC over AF_UNIX
    v
uffs-daemon
    |
    | canonical execution backend
    | - search
    | - aggregate
    | - facet values
    | - info
    | - jobs / duplicate verify
    v
uffs-core
    |
    | compact index + search + aggregate + field catalog
    v
uffs-mft / .uffs / live NTFS
```

### Layer responsibilities

#### `uffs-core`

Owns:

- `FieldId`, `FieldMeta`, `AggregateMeta`
- compact-record search
- aggregation planner and accumulators
- hot/derived/deep field semantics
- duplicate candidate grouping and verification orchestration

#### `uffs-daemon`

Owns:

- JSON-RPC daemon protocol
- search execution
- aggregate execution
- facet-value execution
- info/detail lookup
- daemon job/task registry
- index refresh and lifecycle

#### `uffs-client`

Owns:

- daemon connection management
- auto-start and reconnect
- typed request/response transport
- daemon job/task client helpers
- shared-memory handling for non-MCP surfaces

#### `uffs-mcp`

Owns:

- MCP initialization and capability negotiation
- tool/resource/prompt advertisement
- roots-aware scoping policy
- mapping MCP requests to UFFS client calls
- `structuredContent`, `outputSchema`, `resource_link`, task response shaping
- bounded pagination/cursor behavior for model safety

---

## 6. MCP capability plan

| Capability | Ship | Notes |
|---|---:|---|
| stdio transport | P1 | default local integration |
| tools/list + tools/call | P1 | core search/info/status/drives |
| `structuredContent` | P1 | all structured tools |
| `outputSchema` | P1 | all structured tools |
| tool annotations | P1 | read-only hints and task support |
| resources/list + resources/read | P2 | schema docs and live metadata |
| roots awareness | P2 | optional, client-dependent |
| prompts/list + prompts/get | P4 | reusable analysis/report flows |
| cursorized bucket paging | P4 | especially for facets and rollups |
| tasks | P5 | duplicate verification and heavy scans |
| Streamable HTTP | P6 | separate gateway mode |
| subscriptions / live resource updates | P6+ | only if a concrete client need appears |
| completions | P6+ | useful but not critical for v1 |
| disjunctive facets | P7+ | defer until UX pressure exists |

### 6.1 Initial capability advertisement recommendation

### Server features to advertise in v1

- `tools`
- `resources` only after P2
- `prompts` only after P4

### Client features to consume when present

- `roots`
- `tasks` only after P5

### `listChanged` recommendation

- `tools.listChanged = false` initially because UFFS tool inventory should be stable
- `resources.listChanged = false` initially unless you expose dynamic resources that actually change membership
- `prompts.listChanged = false` initially for the same reason

That keeps the first server simple and predictable.

---

## 7. Public MCP surface

## 7.1 Tool naming policy

Use lowercase dotted names:

- `uffs.search`
- `uffs.aggregate`
- `uffs.facet_values`
- `uffs.info`
- `uffs.drives`
- `uffs.status`
- `uffs.duplicate_verify`

Why dotted names:

- align with MCP tool naming guidance
- namespace UFFS clearly
- leave room for later admin/private tools
- avoid collisions with host/global tools

## 7.2 Tool inventory and annotations

| Tool | Phase | readOnly | destructive | idempotent | openWorld | taskSupport |
|---|---:|---:|---:|---:|---:|---|
| `uffs.search` | P1 | true | false | true | false | forbidden |
| `uffs.info` | P1 | true | false | true | false | forbidden |
| `uffs.drives` | P1 | true | false | true | false | forbidden |
| `uffs.status` | P1 | true | false | true | false | forbidden |
| `uffs.aggregate` | P3 | true | false | true | false | optional |
| `uffs.facet_values` | P4 | true | false | true | false | forbidden |
| `uffs.duplicate_verify` | P5 | true | false | true | false | required |
| `uffs.refresh_index` | gated | false | false | false | false | optional |
| `uffs.warmup` | gated | false | false | true | false | forbidden |

### Why `openWorldHint = false`

UFFS is operating over the local indexed filesystem domain, optionally bounded by roots. It is not a web search or arbitrary external action tool.

### Why `duplicate_verify` is task-backed

Content verification can be expensive and should not force synchronous tool latency.

---

## 8. Agent usage model: how an LLM should answer NTFS questions with UFFS

This is the core operating model for a host or agent.

### 8.1 Known-item lookup

User question:

- “Find the largest `.pst` file on D:”
- “Show me recent PDFs in Downloads.”

Agent path:

1. call `uffs.search`
2. optionally call `uffs.info` for one selected row

### 8.2 Summary questions

User question:

- “How much space do videos take?”
- “What kinds of files dominate this drive?”
- “How old is this subtree?”

Agent path:

1. call `uffs.aggregate`
2. use returned drill-down predicates for any follow-up detail query

### 8.3 Refinement questions

User question:

- “What extensions are common under this result set?”
- “What types are still available if I remove `type=video`?”

Agent path:

1. call `uffs.facet_values`
2. or call `uffs.aggregate` with `terms` buckets
3. refine with returned drill-down patch

### 8.4 Duplicate investigations

User question:

- “Do I have duplicate photos?”
- “Find duplicate installers over 100MB.”

Agent path:

1. call `uffs.aggregate` with `duplicates` or preset `duplicate_candidates`
2. if user wants stronger proof, call `uffs.duplicate_verify`
3. inspect sample rows and reclaimable bytes

### 8.5 Workspace-bounded questions

User question:

- “What is in this repo that is wasting space?”
- “Find big archives inside the current project root only.”

Agent path:

1. read client roots if available
2. server intersects those roots with UFFS scope
3. call `uffs.aggregate` or `uffs.search`

### 8.6 What agents should not do

- do not fetch thousands of rows to count them in-model
- do not request all columns unless needed
- do not page through huge result sets when an aggregate answers the question
- do not call admin tools automatically

---

## 9. Result shaping rules

## 9.1 Search result rules

### Default inline row rules

- default row count: **50**
- recommended hard cap for inline MCP rows: **500**
- default projection: `name,size,modified,path,type,ext`
- explicit projection override allowed

### Search response contract

Return:

- query summary
- exact or known total match count when available
- returned row count
- cursor when more rows exist
- warnings when projection or limits were adjusted

### Why cap inline rows

Even if the daemon can return more, the MCP surface should be optimized for model consumption, not raw transport throughput.

## 9.2 Aggregate result rules

### Default aggregate domain

`Matched`, not `Page`.

### Default aggregate output

- top-level domain summary
- aggregate results
- sample rows only when requested or preset-defined
- drill-down predicate patch for each bucket
- cursor for additional bucket pages when cardinality is high

## 9.3 Large result rules

When a search or aggregate response is too large for sensible inline MCP use:

1. keep the inline response compact
2. return cursor and truncation metadata
3. optionally return `resource_link` to a server-readable result resource
4. if computation itself is long-running, use tasks

### Recommended rule

Use **resources for already-produced artifacts** and **tasks for still-running work**.

---

## 10. Phase-by-phase implementation plan

## Phase 0 — freeze the contract boundary

### Goal

Decide exactly where the MCP shell ends and where UFFS semantics begin.

### Required decisions

1. Adopt `rmcp` as the outer protocol shell.
2. Keep an internal `McpFacade` / adapter layer owned by UFFS.
3. Decide canonical internal aggregate model:
   - recommended: `SearchParams { aggregations: Vec<AggregateSpec> }`
   - optional external alias: daemon `aggregate` JSON-RPC method
4. Freeze public tool names.
5. Freeze cursor and truncation conventions.
6. Freeze error taxonomy and read-only/admin tool split.

### Deliverables

- `docs/architecture/MCP_IMPLEMENTATION_DECISIONS.md`
- golden JSON examples for all P1 tools
- a test matrix mapping each public MCP tool to daemon methods and response types

### Acceptance gate

No further MCP feature work starts until the above decisions are written down.

---

## Phase 1 — production-grade stdio MCP over existing daemon methods

### Goal

Turn the current rudimentary `uffs-mcp` into a robust, typed, read-only MCP server over stdio.

### Scope

Ship these tools only:

- `uffs.search`
- `uffs.info`
- `uffs.drives`
- `uffs.status`

### Crate changes

#### `crates/uffs-mcp`

Create this module layout:

```text
crates/uffs-mcp/src/
├── main.rs
├── app.rs
├── server.rs
├── state.rs
├── error.rs
├── policy.rs
├── tool_registry.rs
├── text.rs
├── schemas/
│   ├── mod.rs
│   ├── search.rs
│   ├── info.rs
│   ├── drives.rs
│   └── status.rs
└── tools/
    ├── mod.rs
    ├── search.rs
    ├── info.rs
    ├── drives.rs
    └── status.rs
```

#### `crates/uffs-client`

No new daemon methods yet. Reuse existing:

- `search`
- `info`
- `drives`
- `status`
- `keepalive`

### Behavior requirements

- stdio only
- no stdout pollution outside MCP messages
- logging to stderr only
- use `structuredContent` for all four tools
- provide `outputSchema` for all four tools
- attach tool annotations (`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`)
- no aggregate support yet
- no roots yet

### Acceptance gate

- MCP Inspector and at least one host can initialize and call all four tools
- `uffs.search` returns bounded rows with stable field names
- all tool outputs validate against declared schemas
- server survives daemon-not-ready and daemon-restart scenarios gracefully

---

## Phase 2 — resources, roots-aware policy, and safer search shaping

### Goal

Make the server self-describing and workspace-aware.

### Scope

Add:

- resources
- optional roots integration
- better search projection/limit policy

### Crate changes

#### `crates/uffs-mcp`

Extend layout:

```text
crates/uffs-mcp/src/
├── roots.rs
├── resources/
│   ├── mod.rs
│   ├── schemas.rs
│   ├── fields.rs
│   ├── presets.rs
│   └── daemon_status.rs
└── schemas/
    └── common.rs
```

### Recommended resources

| Resource URI | Purpose |
|---|---|
| `uffs://schema/fields` | canonical field catalog |
| `uffs://schema/search` | search request model |
| `uffs://schema/aggregate` | aggregate request model |
| `uffs://presets/aggregate` | built-in aggregate presets |
| `uffs://daemon/status` | daemon health snapshot |
| `uffs://drives` | loaded drive snapshot |

### Roots policy

If the client advertises roots support:

1. read current roots
2. normalize them
3. map them to UFFS path predicates when possible
4. intersect user-supplied query scope with roots scope

### Important nuance

Roots are straightforward on Windows live volumes (`file:///C:/...`). They are less straightforward for macOS/Linux offline-capture workflows where UFFS may index NTFS captures that do not map cleanly onto the local host path model. In those cases:

- apply roots only when a confident mapping exists
- otherwise ignore roots and return a warning in the tool result

### Search shaping policy

- default projection becomes compact
- server may reduce requested projection if it exceeds configured row size limits
- server may reduce inline row count and return cursor
- no shmem exposure to MCP

### Acceptance gate

- resources/list and resources/read work
- roots-aware scoping works on Windows live-drive mode
- roots mismatch cases produce explicit warnings, not silent wrong scoping

---

## Phase 3 — daemon-native aggregation core and `uffs.aggregate`

### Goal

Add first-class summaries, not row-counting by prompt.

### Scope

Add:

- daemon-side aggregate engine
- client protocol additions
- `uffs.aggregate`

### Cross-crate changes

#### `crates/uffs-core`

Create the aggregation module:

```text
crates/uffs-core/src/aggregate/
├── mod.rs
├── spec.rs
├── planner.rs
├── accumulators.rs
├── buckets.rs
├── finalize.rs
├── rollup.rs
├── duplicates.rs
└── presets.rs
```

#### `crates/uffs-client`

Extend `protocol.rs` with:

- `AggregateSpec`
- `AggregateKind`
- `AggregateResult`
- `AggregateBucket`
- `FacetValuesParams`
- `FacetValuesResult`

#### `crates/uffs-daemon`

Add:

- aggregate execution entry point
- `aggregate` daemon method or `search` aggregation branch
- validation and shaping for cursors/exactness/truncation

#### `crates/uffs-mcp`

Add:

- `tools/aggregate.rs`
- `schemas/aggregate.rs`

### First aggregate features to ship

- `count`
- `overview`
- `terms:type`
- `terms:ext`
- `terms:drive`
- `hist:size`
- `datehist:modified`

### Important execution rule

Aggregate-only calls should not materialize `DisplayRow`s unless sample rows were requested.

### Acceptance gate

- `uffs.aggregate` answers storage and category questions in one call
- aggregate-only execution avoids row materialization for hot-field cases
- exactness/truncation metadata is explicit
- bucket drill-down predicate patches are present

---

## Phase 4 — `uffs.facet_values`, prompts, and richer drill-down

### Goal

Make MCP navigation and refinement excellent, not merely functional.

### Scope

Add:

- `uffs.facet_values`
- prompts
- bucket pagination and richer drill-down behavior

### Crate changes

#### `crates/uffs-mcp`

Extend:

```text
crates/uffs-mcp/src/
├── tools/
│   ├── facet_values.rs
│   └── aggregate.rs
└── prompts/
    ├── mod.rs
    ├── disk_usage_report.rs
    ├── cleanup_report.rs
    └── duplicate_investigation.rs
```

### Recommended prompts

| Prompt | Purpose |
|---|---|
| `uffs.disk_usage_report` | summarize type/drive/age/storage hotspots |
| `uffs.cleanup_report` | summarize cleanup candidates |
| `uffs.duplicate_investigation` | walk from candidates to verified duplicates |

### `uffs.facet_values`

This tool should support:

- field selection
- current predicate scope
- prefix search
- top N
- cursor

### Recommended v1 rule

Support **prefix** facet search first. Defer fuzzy matching.

### Acceptance gate

- agents can refine large value spaces without dumping raw rows
- prompts return useful reusable workflows without embedding fragile business logic in the host

---

## Phase 5 — task-backed duplicate verification and long-running work

### Goal

Support durable, expensive operations cleanly.

### Scope

Add:

- daemon job registry
- MCP task integration
- `uffs.duplicate_verify`

### Cross-crate changes

#### `crates/uffs-daemon`

Add a jobs subsystem:

```text
crates/uffs-daemon/src/
├── jobs.rs
├── jobs/
│   ├── mod.rs
│   ├── types.rs
│   ├── registry.rs
│   ├── duplicate_verify.rs
│   └── aggregate_export.rs
```

#### `crates/uffs-client`

Add:

- `jobs_submit`
- `jobs_get`
- `jobs_result`
- `jobs_cancel`

#### `crates/uffs-mcp`

Add:

- task-backed tool execution mapping
- `tools/duplicate_verify.rs`

### Verification modes

Start with the staged duplicate model already recommended internally:

- `none`
- `first_bytes`
- `sha256`
- `bytewise`

### Recommended default

- candidate grouping: `size + name`
- verify: optional
- task-backed when verify is requested or group count exceeds threshold

### Acceptance gate

- long duplicate verification runs do not block normal MCP traffic
- task status and result retrieval work end-to-end
- cancellation behaves correctly

---

## Phase 6 — Streamable HTTP gateway

### Goal

Support remote and multi-process MCP deployments without forcing HTTP semantics into the daemon.

### Recommendation

Implement Streamable HTTP in a **gateway mode**, not by turning `uffs-daemon` itself into an HTTP server.

### Why

The gateway needs:

- auth
- session handling
- rate limits
- origin validation
- proxy/gateway semantics
- possibly server cards later

The daemon should stay focused on local execution and daemon IPC.

### Crate changes

Preferred shape:

```text
crates/
├── uffs-mcp/        # stdio default, shared logic
└── uffs-mcp-http/   # optional gateway binary reusing shared service layer
```

Or keep one crate with feature-gated binaries if you prefer fewer crates.

### Acceptance gate

- Streamable HTTP passes the same golden tool/resource/prompt tests as stdio
- auth and origin checks exist before remote use is documented

---

## Phase 7 — validation, conformance, and hardening

### Goal

Make the MCP layer testable with the same seriousness as CLI and daemon RPC.

### Test classes

#### Unit

- schema generation tests
- tool annotation tests
- roots mapping tests
- cursor encoding/decoding tests
- error mapping tests

#### Integration

- stdio initialize / tools/list / tools/call
- resources/list / resources/read
- prompts/list / prompts/get
- tasks end-to-end
- daemon restart and reconnect scenarios

#### Performance

- no extra daemon warm-query regression from MCP layer
- bounded memory when returning large aggregate results
- aggregate-only no-row-materialization regression guard

#### Host/interoperability

- MCP Inspector
- at least one IDE host
- one Streamable HTTP test client when P6 lands

### Recommended new harness

Add a dedicated validation harness parallel to your existing suites:

```text
scripts/
├── windows/
│   ├── cli-flag-validation.rs
│   ├── api-validation.rs
│   └── mcp-validation.rs
└── tests/
    └── test-definitions.toml
```

### Suggested MCP suites

- `M100` initialize + capability negotiation
- `M110` tools/list shape
- `M120` `uffs.search` schema and row bounds
- `M130` `uffs.info`
- `M140` `uffs.drives`
- `M150` `uffs.status`
- `M200` resources/list/read
- `M300` aggregate schema correctness
- `M310` bucket cursor pagination
- `M320` drill-down patch correctness
- `M400` roots boundary enforcement
- `M500` duplicate verify task flow
- `M600` no-shmem-path leak
- `M700` stderr-only logging under stdio

---

## 11. Concrete Rust module skeletons

The following are compile-oriented skeletons, not guaranteed drop-in code. They are meant to make the implementation shape concrete.

## 11.1 `crates/uffs-mcp/Cargo.toml`

```toml
[package]
name = "uffs-mcp"
version = "0.1.0"
edition = "2024"

[dependencies]
anyhow = "1"
thiserror = "2"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
schemars = "1"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "io-std"] }
tracing = "0.1"
tracing-subscriber = "0.3"

rmcp = { version = "1.3", features = [
  "server",
  "macros",
  "schemars",
  "transport-io"
] }

uffs-client = { path = "../uffs-client" }

[features]
default = []
streamable-http = ["rmcp/transport-streamable-http-server"]
```

## 11.2 `crates/uffs-mcp/src/main.rs`

```rust
mod app;
mod error;
mod policy;
mod server;
mod state;
mod text;
mod tool_registry;
mod tools;
mod schemas;
mod resources;
mod prompts;
mod roots;

use anyhow::Result;
use app::McpApp;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let app = McpApp::bootstrap().await?;
    server::serve_stdio(app).await
}
```

## 11.3 `crates/uffs-mcp/src/state.rs`

```rust
use std::sync::Arc;
use tokio::sync::RwLock;
use uffs_client::UffsClient;

#[derive(Clone)]
pub struct AppState {
    pub client: UffsClient,
    pub roots: Arc<RwLock<RootsState>>,
    pub limits: Limits,
    pub features: FeatureFlags,
}

#[derive(Debug, Default)]
pub struct RootsState {
    pub advertised: bool,
    pub roots: Vec<RootScope>,
}

#[derive(Debug, Clone)]
pub struct RootScope {
    pub uri: String,
    pub display_name: Option<String>,
    pub normalized_prefix: String,
}

#[derive(Debug, Clone)]
pub struct Limits {
    pub default_row_limit: u32,
    pub max_inline_rows: u32,
    pub default_bucket_limit: u32,
    pub max_inline_buckets: u32,
}

#[derive(Debug, Clone)]
pub struct FeatureFlags {
    pub enable_resources: bool,
    pub enable_prompts: bool,
    pub enable_tasks: bool,
    pub enable_admin_tools: bool,
}
```

## 11.4 `crates/uffs-mcp/src/error.rs`

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpBridgeError {
    #[error("daemon unavailable")]
    DaemonUnavailable,

    #[error("invalid search request: {0}")]
    InvalidSearch(String),

    #[error("invalid aggregate request: {0}")]
    InvalidAggregate(String),

    #[error("requested path is outside allowed roots")]
    OutsideRoots,

    #[error("feature not enabled: {0}")]
    FeatureDisabled(&'static str),

    #[error("internal error: {0}")]
    Internal(String),
}
```

## 11.5 `crates/uffs-mcp/src/app.rs`

```rust
use anyhow::Result;
use crate::state::{AppState, FeatureFlags, Limits, RootsState};
use std::sync::Arc;
use tokio::sync::RwLock;
use uffs_client::UffsClient;

#[derive(Clone)]
pub struct McpApp {
    pub state: AppState,
}

impl McpApp {
    pub async fn bootstrap() -> Result<Self> {
        let client = UffsClient::connect().await?;

        Ok(Self {
            state: AppState {
                client,
                roots: Arc::new(RwLock::new(RootsState::default())),
                limits: Limits {
                    default_row_limit: 50,
                    max_inline_rows: 500,
                    default_bucket_limit: 25,
                    max_inline_buckets: 200,
                },
                features: FeatureFlags {
                    enable_resources: true,
                    enable_prompts: false,
                    enable_tasks: false,
                    enable_admin_tools: false,
                },
            },
        })
    }
}
```

## 11.6 `crates/uffs-mcp/src/policy.rs`

```rust
use crate::error::McpBridgeError;
use crate::state::RootsState;
use uffs_client::protocol::{SearchParams, SearchPredicate};

pub fn apply_roots_scope(
    mut params: SearchParams,
    roots: &RootsState,
) -> Result<SearchParams, McpBridgeError> {
    if !roots.advertised || roots.roots.is_empty() {
        return Ok(params);
    }

    // Representative policy only. Exact mapping depends on path model.
    let allowed_prefixes: Vec<String> = roots
        .roots
        .iter()
        .map(|r| r.normalized_prefix.clone())
        .collect();

    params.predicates.push(SearchPredicate::PathWithinAny {
        prefixes: allowed_prefixes,
    });

    Ok(params)
}
```

## 11.7 `crates/uffs-mcp/src/tool_registry.rs`

```rust
use crate::schemas;

pub struct ToolDescriptor {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub input_schema: serde_json::Value,
    pub output_schema: Option<serde_json::Value>,
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    pub open_world: bool,
    pub task_support: &'static str,
}

pub fn builtin_tools() -> Vec<ToolDescriptor> {
    vec![
        ToolDescriptor {
            name: "uffs.search",
            title: "Search indexed NTFS files",
            description: "Find files and directories using UFFS search semantics.",
            input_schema: schemas::search::input_schema(),
            output_schema: Some(schemas::search::output_schema()),
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
            task_support: "forbidden",
        },
        ToolDescriptor {
            name: "uffs.aggregate",
            title: "Summarize indexed NTFS data",
            description: "Run server-side aggregations and return compact structured summaries.",
            input_schema: schemas::aggregate::input_schema(),
            output_schema: Some(schemas::aggregate::output_schema()),
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: false,
            task_support: "optional",
        },
        // info / drives / status / facet_values / duplicate_verify
    ]
}
```

## 11.8 `crates/uffs-mcp/src/tools/search.rs`

```rust
use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uffs_client::protocol::{SearchParams, SearchResponse};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SearchToolArgs {
    #[schemars(default = "default_pattern")]
    pub pattern: String,
    #[schemars(default = "default_scope")]
    pub scope: String,
    #[serde(default)]
    pub predicates: Vec<PredicateArg>,
    #[serde(default)]
    pub sorts: Vec<SortArg>,
    #[serde(default)]
    pub columns: Option<Vec<String>>,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct PredicateArg {
    pub field: String,
    pub op: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct SortArg {
    pub field: String,
    #[serde(default)]
    pub desc: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SearchToolResult {
    pub query: SearchQuerySummary,
    pub domain: SearchDomainSummary,
    pub rows: Vec<SearchRow>,
    pub next_cursor: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SearchQuerySummary {
    pub pattern: String,
    pub scope: String,
    pub returned: u32,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SearchDomainSummary {
    pub total_matches: Option<u64>,
    pub exact: bool,
    pub execution_ms: u64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SearchRow {
    pub name: String,
    pub size: Option<u64>,
    pub modified: Option<String>,
    pub path: String,
    pub ext: Option<String>,
    pub file_type: Option<String>,
}

pub async fn run_search(app: &crate::app::McpApp, args: SearchToolArgs) -> Result<SearchToolResult> {
    let mut params = SearchParams::default();
    params.pattern = args.pattern.clone();
    params.limit = Some(args.limit);
    params.include_rows = true;
    params.predicates = crate::tools::common::compile_predicates(args.predicates)?;
    params.sorts = crate::tools::common::compile_sorts(args.sorts)?;
    params.projection = crate::tools::common::compile_projection(args.columns)?;

    let roots = app.state.roots.read().await;
    let params = crate::policy::apply_roots_scope(params, &roots)?;

    let response: SearchResponse = app.state.client.search(params).await?;
    crate::tools::common::shape_search_response(args, response)
}

fn default_pattern() -> String { "*".to_string() }
fn default_scope() -> String { "all".to_string() }
fn default_limit() -> u32 { 50 }
```

## 11.9 `crates/uffs-mcp/src/tools/aggregate.rs`

```rust
use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uffs_client::protocol::{AggregateSpec, SearchParams, SearchResponse};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AggregateToolArgs {
    #[schemars(default = "default_pattern")]
    pub pattern: String,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub predicates: Vec<PredicateArg>,
    #[serde(default)]
    pub aggregations: Vec<AggregateArg>,
    #[serde(default)]
    pub include_rows: bool,
    #[serde(default)]
    pub row_limit: Option<u32>,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AggregateArg {
    pub kind: String,
    #[serde(default)]
    pub field: Option<String>,
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub top: Option<u32>,
    #[serde(default)]
    pub metrics: Vec<String>,
    #[serde(default)]
    pub interval: Option<String>,
    #[serde(default)]
    pub verify: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AggregateToolResult {
    pub query: AggregateQuerySummary,
    pub domain: AggregateDomainSummary,
    pub aggregations: Vec<AggregateView>,
    pub next_bucket_cursor: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AggregateQuerySummary {
    pub pattern: String,
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AggregateDomainSummary {
    pub matched_count: u64,
    pub exact: bool,
    pub execution_ms: u64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AggregateView {
    pub id: String,
    pub kind: String,
    pub exact: bool,
    pub truncated: bool,
    pub domain_count: u64,
    pub buckets: Vec<AggregateBucketView>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AggregateBucketView {
    pub key: serde_json::Value,
    pub count: u64,
    pub metrics: serde_json::Value,
    pub sample_rows: Option<Vec<SearchRow>>,
    pub drilldown: Option<Vec<PredicatePatch>>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PredicatePatch {
    pub field: String,
    pub op: String,
    pub value: serde_json::Value,
}

pub async fn run_aggregate(app: &crate::app::McpApp, args: AggregateToolArgs) -> Result<AggregateToolResult> {
    let mut params = SearchParams::default();
    params.pattern = args.pattern.clone();
    params.predicates = crate::tools::common::compile_predicates(args.predicates)?;
    params.aggregations = compile_aggregations(args.profile.clone(), args.aggregations)?;
    params.include_rows = args.include_rows;
    params.limit = args.row_limit;

    let roots = app.state.roots.read().await;
    let params = crate::policy::apply_roots_scope(params, &roots)?;

    let response: SearchResponse = app.state.client.search(params).await?;
    crate::tools::common::shape_aggregate_response(args, response)
}

fn compile_aggregations(
    profile: Option<String>,
    items: Vec<AggregateArg>,
) -> Result<Vec<AggregateSpec>> {
    // Placeholder: compile from MCP args into canonical daemon aggregation spec.
    todo!()
}

fn default_pattern() -> String { "*".to_string() }
```

## 11.10 `crates/uffs-mcp/src/tools/facet_values.rs`

```rust
use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uffs_client::protocol::{FacetValuesParams, FacetValuesResponse};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct FacetValuesArgs {
    pub field: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub predicates: Vec<PredicateArg>,
    #[serde(default = "default_top")]
    pub top: u32,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FacetValuesResult {
    pub field: String,
    pub values: Vec<FacetValue>,
    pub exact: bool,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FacetValue {
    pub value: String,
    pub count: u64,
}

pub async fn run_facet_values(app: &crate::app::McpApp, args: FacetValuesArgs) -> Result<FacetValuesResult> {
    let params = FacetValuesParams {
        field: args.field,
        prefix: args.prefix,
        predicates: crate::tools::common::compile_predicates(args.predicates)?,
        top: args.top,
        cursor: args.cursor,
    };

    let response: FacetValuesResponse = app.state.client.facet_values(params).await?;
    Ok(FacetValuesResult {
        field: response.field,
        values: response.values,
        exact: response.exact,
        next_cursor: response.next_cursor,
    })
}

fn default_top() -> u32 { 20 }
```

## 11.11 `crates/uffs-mcp/src/tools/duplicate_verify.rs`

```rust
use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uffs_client::protocol::{DuplicateVerifyParams, JobHandle};

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DuplicateVerifyArgs {
    pub keys: Vec<String>,
    #[serde(default = "default_verify")]
    pub verify: String,
    #[serde(default)]
    pub predicates: Vec<PredicateArg>,
    #[serde(default)]
    pub top: Option<u32>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DuplicateVerifyAccepted {
    pub task_id: String,
    pub status: String,
    pub poll_hint: String,
}

pub async fn run_duplicate_verify(
    app: &crate::app::McpApp,
    args: DuplicateVerifyArgs,
) -> Result<DuplicateVerifyAccepted> {
    let handle: JobHandle = app.state.client.submit_duplicate_verify(DuplicateVerifyParams {
        keys: args.keys,
        verify: args.verify,
        predicates: crate::tools::common::compile_predicates(args.predicates)?,
        top: args.top,
    }).await?;

    Ok(DuplicateVerifyAccepted {
        task_id: handle.id,
        status: "accepted".to_string(),
        poll_hint: "Use MCP tasks/get and tasks/result to monitor progress and fetch the verified result.".to_string(),
    })
}

fn default_verify() -> String { "first_bytes".to_string() }
```

## 11.12 `crates/uffs-mcp/src/resources/schemas.rs`

```rust
use serde_json::json;

pub fn field_catalog() -> serde_json::Value {
    json!({
        "type": "field_catalog",
        "fields": [
            {"name": "name", "kind": "string", "groupable": true},
            {"name": "size", "kind": "u64", "aggregatable": true, "bucketable": true},
            {"name": "type", "kind": "enum", "groupable": true},
            {"name": "ext", "kind": "string", "groupable": true},
            {"name": "modified", "kind": "timestamp", "aggregatable": true, "bucketable": true}
        ]
    })
}
```

## 11.13 `crates/uffs-client/src/protocol.rs` additions

```rust
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SearchParams {
    pub pattern: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub match_path: bool,
    pub predicates: Vec<SearchPredicate>,
    pub sorts: Vec<SearchSortSpec>,
    pub projection: Option<Vec<FieldId>>,
    pub aggregations: Vec<AggregateSpec>,
    pub include_rows: bool,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
    pub drives: Vec<char>,
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum AggregateSpec {
    Count,
    Distinct { field: FieldId },
    Missing { field: FieldId },
    Terms {
        fields: Vec<FieldId>,
        top: Option<u32>,
        metrics: Vec<BucketMetric>,
        sample: Option<TopHitsSpec>,
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
    Rollup {
        field: RollupField,
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
    Preset { name: AggregatePreset },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchResponse {
    pub rows: Option<Vec<StructuredRow>>,
    pub aggregations: Option<Vec<AggregateResult>>,
    pub total_matches: Option<u64>,
    pub exact: bool,
    pub truncated: bool,
    pub next_cursor: Option<String>,
    pub execution_ms: u64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FacetValuesParams {
    pub field: String,
    pub prefix: Option<String>,
    pub predicates: Vec<SearchPredicate>,
    pub top: u32,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FacetValuesResponse {
    pub field: String,
    pub values: Vec<FacetValue>,
    pub exact: bool,
    pub next_cursor: Option<String>,
}
```

## 11.14 `crates/uffs-daemon/src/handler.rs` additions

```rust
pub async fn dispatch_request(state: &DaemonState, req: RpcRequest) -> RpcResponse {
    match req.method.as_str() {
        "search" => handle_search(state, req).await,
        "aggregate" => handle_aggregate_alias(state, req).await,
        "facet_values" => handle_facet_values(state, req).await,
        "info" => handle_info(state, req).await,
        "drives" => handle_drives(state, req).await,
        "status" => handle_status(state, req).await,
        "jobs/submit" => handle_job_submit(state, req).await,
        "jobs/get" => handle_job_get(state, req).await,
        "jobs/result" => handle_job_result(state, req).await,
        "jobs/cancel" => handle_job_cancel(state, req).await,
        _ => RpcResponse::method_not_found(req.id),
    }
}
```

## 11.15 `crates/uffs-core/src/aggregate/mod.rs`

```rust
mod spec;
mod planner;
mod accumulators;
mod buckets;
mod finalize;
mod rollup;
mod duplicates;
mod presets;

pub use spec::*;
pub use planner::*;

use crate::search::{FieldCatalog, MultiDriveBackend, SearchPlan};

pub struct AggregateEngine<'a> {
    pub fields: &'a FieldCatalog,
    pub backend: &'a MultiDriveBackend,
}

impl<'a> AggregateEngine<'a> {
    pub fn execute(
        &self,
        search_plan: &SearchPlan,
        aggregate_specs: &[AggregateSpec],
    ) -> anyhow::Result<Vec<AggregateResult>> {
        let plan = planner::compile(self.fields, aggregate_specs)?;
        let partials = accumulators::scan(self.backend, search_plan, &plan)?;
        finalize::finalize(plan, partials)
    }
}
```

## 11.16 `crates/uffs-daemon/src/jobs.rs`

```rust
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone, Default)]
pub struct JobRegistry {
    inner: Arc<RwLock<HashMap<String, JobState>>>,
}

#[derive(Debug, Clone)]
pub enum JobState {
    Queued,
    Running { progress: Option<f32> },
    Completed { result: serde_json::Value },
    Failed { error: String },
    Cancelled,
}

impl JobRegistry {
    pub async fn submit_duplicate_verify(&self, spec: DuplicateVerifySpec) -> String {
        // enqueue and spawn
        todo!()
    }

    pub async fn get(&self, id: &str) -> Option<JobState> {
        self.inner.read().await.get(id).cloned()
    }
}
```

---

## 12. MCP tool schemas

The schemas below are intentionally concrete and close to the intended wire shape.

## 12.1 `uffs.search` input schema

```json
{
  "type": "object",
  "properties": {
    "pattern": {
      "type": "string",
      "default": "*",
      "description": "Glob, literal, or regex pattern using UFFS search semantics."
    },
    "scope": {
      "type": "string",
      "enum": ["all", "files", "dirs"],
      "default": "all"
    },
    "predicates": {
      "type": "array",
      "default": [],
      "items": {
        "type": "object",
        "properties": {
          "field": { "type": "string" },
          "op": { "type": "string" },
          "value": {}
        },
        "required": ["field", "op"]
      }
    },
    "sorts": {
      "type": "array",
      "default": [],
      "items": {
        "type": "object",
        "properties": {
          "field": { "type": "string" },
          "desc": { "type": "boolean", "default": false }
        },
        "required": ["field"]
      }
    },
    "columns": {
      "type": ["array", "null"],
      "items": { "type": "string" },
      "default": null
    },
    "limit": {
      "type": "integer",
      "minimum": 1,
      "maximum": 500,
      "default": 50
    },
    "cursor": {
      "type": ["string", "null"],
      "default": null
    }
  },
  "additionalProperties": false
}
```

## 12.2 `uffs.search` output schema

```json
{
  "type": "object",
  "properties": {
    "query": {
      "type": "object",
      "properties": {
        "pattern": { "type": "string" },
        "scope": { "type": "string" },
        "returned": { "type": "integer" }
      },
      "required": ["pattern", "scope", "returned"]
    },
    "domain": {
      "type": "object",
      "properties": {
        "total_matches": { "type": ["integer", "null"] },
        "exact": { "type": "boolean" },
        "execution_ms": { "type": "integer" }
      },
      "required": ["exact", "execution_ms"]
    },
    "rows": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "name": { "type": "string" },
          "size": { "type": ["integer", "null"] },
          "modified": { "type": ["string", "null"] },
          "path": { "type": "string" },
          "ext": { "type": ["string", "null"] },
          "file_type": { "type": ["string", "null"] }
        },
        "required": ["name", "path"]
      }
    },
    "next_cursor": { "type": ["string", "null"] },
    "warnings": { "type": "array", "items": { "type": "string" } }
  },
  "required": ["query", "domain", "rows", "warnings"],
  "additionalProperties": false
}
```

## 12.3 `uffs.aggregate` input schema

```json
{
  "type": "object",
  "properties": {
    "pattern": {
      "type": "string",
      "default": "*"
    },
    "profile": {
      "type": ["string", "null"],
      "default": null,
      "description": "Optional preset such as overview, by_type, by_extension, by_drive, by_age, storage, cleanup, duplicates."
    },
    "predicates": {
      "type": "array",
      "default": [],
      "items": {
        "type": "object",
        "properties": {
          "field": { "type": "string" },
          "op": { "type": "string" },
          "value": {}
        },
        "required": ["field", "op"]
      }
    },
    "aggregations": {
      "type": "array",
      "default": [],
      "items": {
        "type": "object",
        "properties": {
          "kind": { "type": "string" },
          "field": { "type": ["string", "null"] },
          "fields": {
            "type": "array",
            "items": { "type": "string" },
            "default": []
          },
          "top": { "type": ["integer", "null"] },
          "metrics": {
            "type": "array",
            "items": { "type": "string" },
            "default": []
          },
          "interval": { "type": ["string", "null"] },
          "verify": { "type": ["string", "null"] }
        },
        "required": ["kind"]
      }
    },
    "include_rows": { "type": "boolean", "default": false },
    "row_limit": { "type": ["integer", "null"], "default": null },
    "cursor": { "type": ["string", "null"], "default": null }
  },
  "additionalProperties": false
}
```

## 12.4 `uffs.aggregate` output schema

```json
{
  "type": "object",
  "properties": {
    "query": {
      "type": "object",
      "properties": {
        "pattern": { "type": "string" },
        "profile": { "type": ["string", "null"] }
      },
      "required": ["pattern"]
    },
    "domain": {
      "type": "object",
      "properties": {
        "matched_count": { "type": "integer" },
        "exact": { "type": "boolean" },
        "execution_ms": { "type": "integer" }
      },
      "required": ["matched_count", "exact", "execution_ms"]
    },
    "aggregations": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "id": { "type": "string" },
          "kind": { "type": "string" },
          "exact": { "type": "boolean" },
          "truncated": { "type": "boolean" },
          "domain_count": { "type": "integer" },
          "buckets": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "key": {},
                "count": { "type": "integer" },
                "metrics": {},
                "sample_rows": {
                  "type": ["array", "null"],
                  "items": { "type": "object" }
                },
                "drilldown": {
                  "type": ["array", "null"],
                  "items": {
                    "type": "object",
                    "properties": {
                      "field": { "type": "string" },
                      "op": { "type": "string" },
                      "value": {}
                    },
                    "required": ["field", "op"]
                  }
                }
              },
              "required": ["key", "count", "metrics"]
            }
          }
        },
        "required": ["id", "kind", "exact", "truncated", "domain_count", "buckets"]
      }
    },
    "next_bucket_cursor": { "type": ["string", "null"] },
    "warnings": { "type": "array", "items": { "type": "string" } }
  },
  "required": ["query", "domain", "aggregations", "warnings"],
  "additionalProperties": false
}
```

## 12.5 `uffs.info` input schema

```json
{
  "type": "object",
  "properties": {
    "path": {
      "type": "string",
      "description": "Fully-qualified NTFS path for the selected file or directory."
    }
  },
  "required": ["path"],
  "additionalProperties": false
}
```

## 12.6 `uffs.facet_values` input schema

```json
{
  "type": "object",
  "properties": {
    "field": { "type": "string" },
    "prefix": { "type": ["string", "null"], "default": null },
    "predicates": {
      "type": "array",
      "default": [],
      "items": {
        "type": "object",
        "properties": {
          "field": { "type": "string" },
          "op": { "type": "string" },
          "value": {}
        },
        "required": ["field", "op"]
      }
    },
    "top": { "type": "integer", "minimum": 1, "maximum": 200, "default": 20 },
    "cursor": { "type": ["string", "null"], "default": null }
  },
  "required": ["field"],
  "additionalProperties": false
}
```

## 12.7 `uffs.duplicate_verify` input schema

```json
{
  "type": "object",
  "properties": {
    "keys": {
      "type": "array",
      "items": { "type": "string" },
      "minItems": 1
    },
    "verify": {
      "type": "string",
      "enum": ["none", "first_bytes", "sha256", "bytewise"],
      "default": "first_bytes"
    },
    "predicates": {
      "type": "array",
      "default": [],
      "items": {
        "type": "object",
        "properties": {
          "field": { "type": "string" },
          "op": { "type": "string" },
          "value": {}
        },
        "required": ["field", "op"]
      }
    },
    "top": { "type": ["integer", "null"], "default": null }
  },
  "required": ["keys"],
  "additionalProperties": false
}
```

---

## 13. Recommended prompts and resources

## 13.1 Prompts

### `uffs.disk_usage_report`

Purpose:

- answer “what is taking space?”
- call `uffs.aggregate` with `overview`, `by_type`, and `by_drive`
- optionally drill down into `top_folders`

### `uffs.cleanup_report`

Purpose:

- answer “what can I delete or investigate?”
- use `cleanup` preset plus a small search sample for each major bucket

### `uffs.duplicate_investigation`

Purpose:

- answer “are these really duplicates?”
- use candidate aggregate first, then optionally task-backed verify

## 13.2 Resources

### Stable schema resources

- `uffs://schema/fields`
- `uffs://schema/search`
- `uffs://schema/aggregate`
- `uffs://schema/presets`

### Live metadata resources

- `uffs://daemon/status`
- `uffs://drives`

### Why resources matter for UFFS

They let a host or power user inspect the server’s capabilities without prompting the model to discover them by trial and error.

---

## 14. Security, policy, and UX rules

## 14.1 Human-in-the-loop assumption

The MCP tools spec recommends that applications keep a human in the loop for tool invocation approval and visibility. UFFS should assume hosts may expose confirmation flows and should make tool semantics obvious and low-risk.

## 14.2 Default trust stance

- read-only tools only by default
- explicit roots scoping when available
- explicit warnings for root/path mismatch cases
- no daemon-control tools in default advertisement

## 14.3 Stdio logging rule

Under stdio transport:

- MCP messages on stdout only
- all logs to stderr only

## 14.4 Windows privilege nuance

On Windows, UFFS may run elevated or use the access broker depending on deployment mode. MCP must not paper over this reality. If the daemon is not ready because NTFS access is unavailable, tool errors should say so clearly.

---

## 15. Recommended answers to the open architecture questions

### Q1. Should daemon-native `aggregate` be a convenience alias over `SearchParams`, or should only `search` exist on the wire with `aggregations` populated?

**Recommendation:** Keep one canonical internal `SearchParams` model with `aggregations`, but allow a convenience daemon `aggregate` alias if it simplifies external callers and debugging.

### Q2. Should `uffs stats` remain user-visible long term?

**Recommendation:** Keep it for compatibility in the short term, but rebase it onto the aggregate engine and eventually document `--aggregate` as the primary path.

### Q3. Should v1 include approximate distinct counts?

**Recommendation:** No. Stay exact-only in v1. Add approximation later only behind explicit opt-in.

### Q4. How much rollup nesting should v1 allow?

**Recommendation:** Limit inline nested rollups to depth 2 and at most two dimensions before requiring cursor/drill-down.

### Q5. Should `facet_values` support fuzzy search immediately?

**Recommendation:** No. Support prefix first.

### Q6. Should disjunctive facets ship early?

**Recommendation:** No. Defer until there is real GUI/TUI/host pressure.

---

## 16. Bottom line

The best UFFS MCP server is:

- **daemon-native**, not CLI-wrapped,
- **SDK-backed at the protocol shell**, not SDK-bound at the semantic core,
- **schema-first**, not stringly-typed,
- **aggregate-capable**, not row-counted by prompt,
- **roots-aware and read-only by default**, not globally unconstrained,
- **task-capable for expensive work**, not synchronously blocking everything,
- **stdio-first locally**, with **Streamable HTTP via a gateway** later.

That design fits both the UFFS architecture you already have and the modern MCP surface that exists now.

---

## Appendix A — implementation checklist

### P1 checklist

- [ ] adopt `rmcp` shell
- [ ] stdio initialize works
- [ ] `uffs.search` / `uffs.info` / `uffs.drives` / `uffs.status`
- [ ] output schemas emitted
- [ ] stderr-only logging
- [ ] row bounding + compact projection

### P2 checklist

- [ ] schema resources
- [ ] daemon status / drives resources
- [ ] roots mapping policy
- [ ] warnings for unmappable roots

### P3 checklist

- [ ] `uffs-core::aggregate`
- [ ] daemon aggregation method
- [ ] `uffs.aggregate`
- [ ] sample rows + drilldown
- [ ] exact/truncated metadata

### P4 checklist

- [ ] `uffs.facet_values`
- [ ] prompts/list + prompts/get
- [ ] bucket cursor pagination

### P5 checklist

- [ ] daemon jobs registry
- [ ] `uffs.duplicate_verify`
- [ ] MCP tasks mapping

### P6 checklist

- [ ] Streamable HTTP gateway
- [ ] auth + origin checks
- [ ] stdio and HTTP parity tests

---

## Appendix B — external protocol references

- MCP SDKs: <https://modelcontextprotocol.io/docs/sdk>
- MCP specification overview: <https://modelcontextprotocol.io/specification/2025-11-25>
- MCP transports: <https://modelcontextprotocol.io/specification/2025-11-25/basic/transports>
- MCP tools: <https://modelcontextprotocol.io/specification/2025-11-25/server/tools>
- MCP schema reference: <https://modelcontextprotocol.io/specification/2025-11-25/schema>
- MCP roots: <https://modelcontextprotocol.io/specification/2025-06-18/client/roots>
- MCP tasks: <https://modelcontextprotocol.io/specification/draft/basic/utilities/tasks>
- MCP roadmap: <https://modelcontextprotocol.io/development/roadmap>
- official Rust SDK docs: <https://docs.rs/rmcp/latest/rmcp/>

---

## Appendix C — internal source files used

- `DAEMON_SERVICE_ARCHITECTURE.md`
- `daemon.md`
- `FILTER_SORT_FEATURE_MATRIX.md`
- `AGGREGATION_ARCHITECTURE.md`
- `UFFS_AGGREGATION_ARCHITECTURE_CONSOLIDATED.md`
- `cli-overview.md`
- `filters.md`
- `search-modes.md`
- `sorting.md`
- `api-validation.rs`
- `cli-flag-validation.rs`
- `UFFS_MCP_frontend_architecture_proposal_2026-04-08.md`
