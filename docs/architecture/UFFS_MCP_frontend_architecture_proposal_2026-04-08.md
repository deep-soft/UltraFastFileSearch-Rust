# UFFS MCP Frontend Architecture Proposal

Date: 2026-04-08  
Format: Markdown  
Audience: UFFS core, daemon, MCP, CLI, TUI, GUI, and release owners

## Executive summary

The right long-term MCP architecture for UFFS is **not** a CLI wrapper and **not** a second query engine. It is a **daemon-native MCP layer** that treats `uffs-daemon` as the single execution backend, keeps all semantics in the canonical typed query model, and exposes a small number of high-value MCP tools with structured outputs, drill-down handles, schema resources, and task support for expensive operations.

That recommendation is strongly supported by the current UFFS architecture and timing evidence:

- UFFS already has the right backend shape: `uffs-daemon` owns indexing and query execution, `uffs-client` is the thin transport library, and `uffs-mcp` is already positioned as an MCP adapter over the daemon rather than a separate engine. The broader architecture is explicitly unified around one daemon serving CLI, TUI, GUI, and MCP. Internal docs describe this as implemented through Phases 1-5, with `uffs-mcp` already present and the standalone CLI/TUI search paths removed. [Internal: `DAEMON_SERVICE_ARCHITECTURE.md`, `daemon.md`]
- Warm daemon search latency is already in the right regime for a serious local MCP service: the daemon architecture document reports **9 ms median warm query latency**, **12.4 s cold start** for 7 drives from `.uffs` cache, and **~7.3 GiB steady-state memory** with **~10 GiB peak during load** on a 25.8M-record dataset. [Internal: `DAEMON_SERVICE_ARCHITECTURE.md`, `daemon.md`]
- Aggregation is the obvious next MCP differentiator. Your consolidated aggregation design is already pointing in the correct direction: keep aggregation inside the same daemon-owned search contract, use typed field metadata, make MCP structured output first-class, and add path rollups, duplicate analysis, sample rows, exactness/truncation metadata, and drill-down handles. [Internal: `UFFS_AGGREGATION_ARCHITECTURE_CONSOLIDATED.md`]
- Official MCP has now matured to the point where a high-end server should expose more than tools. The current specification supports **stdio** and **Streamable HTTP** as the two official transports, encourages **output schemas** and **structuredContent** for tools, supports **resources**, **prompts**, **roots**, and **tool task support**, and defines annotation hints such as `readOnlyHint`, `idempotentHint`, and `taskSupport`. [1][2][3][4][5][6][7]

The bottom line: **UFFS is already a strong MCP backend candidate today**, but the cutting-edge version is not just "MCP over existing CLI". The best version is a daemon-native, schema-driven, agent-optimized service that gives LLMs four things they care about most:

1. very fast exact search,
2. compact structured summaries,
3. drill-down refinement without prompt bloat,
4. explicit cost/latency boundaries.

## What the current system already proves

### 1. The daemon architecture is already the correct foundation

The internal daemon architecture docs describe UFFS as a unified daemon system where a single background process holds the compact search index in memory and serves all surfaces through IPC. `uffs-client` handles connection, auto-start, reconnect, keepalive, and bulk-result shared-memory transfer; `uffs-mcp` is a stdio adapter over `uffs-client`; CLI is already daemon-only; TUI is nearly there. [Internal: `DAEMON_SERVICE_ARCHITECTURE.md`, `daemon.md`]

This matters because the most common anti-pattern in MCP file tools is to shell out to a CLI on every request. UFFS is already past that architecture. The daemon means:

- one warm in-memory index,
- one canonical query engine,
- one place for performance instrumentation,
- one place for policy and capability gating,
- one place to add search, aggregate, duplicate, and export semantics.

That is exactly what an MCP server wants.

### 2. Performance is already good enough for serious agent workflows

From your internal docs:

- warm daemon search latency: **9 ms median**,
- cold startup from `.uffs` cache: **12.4 s**,
- steady-state memory: **~7.3 GiB**,
- peak load memory: **~10 GiB**,
- filtered query latency range: **8 ms to 1,886 ms**, depending on filter type. [Internal: `DAEMON_SERVICE_ARCHITECTURE.md`]

From the uploaded `Output` log, the picture becomes even clearer.

### 2.1 What the uploaded timing log suggests

The `Output` file shows several distinct performance regimes:

#### A. Daemon internal query performance is excellent

In the Windows live-NTFS profiling runs, once the daemon is hot, representative queries over all loaded drives were roughly:

- hot total around **155 ms** end-to-end for all drives with `--profile --limit 100`,
- internal daemon search around **147 ms** in that run,
- single-drive hot totals around **23-54 ms** depending on drive size,
- much larger cold and warm-cache startup phases dominated by index loading rather than query execution.

This is exactly the pattern you want for MCP: once hot, the backend itself is fast enough that transport and agent orchestration dominate.

#### B. CLI process spawn is a real cost center

The readiness scenarios in the same log show a very different number for one-shot CLI invocations:

- hot startup + query around **865 ms**,
- warm startup + query around **7.7 s**,
- cold startup + query around **66.8 s**.

That is not a contradiction. It means the daemon is fast, but repeatedly launching the CLI and re-doing client connection/startup work is expensive. That is the single strongest argument against a shell-out MCP wrapper.

#### C. Concurrency looks healthy

The stress output in your log indicates no failures across concurrency levels and throughput rising into the high hundreds or low thousands of queries per second depending on concurrency level. Representative points from the uploaded log:

- concurrency 1: p50 around **2.2 ms**, mean around **2.9 ms**,
- concurrency 16: p50 around **6.6 ms**, mean around **7.0 ms**, peak throughput around **1,896 qps**,
- concurrency 128: p50 around **27.4 ms**, mean around **29.6 ms**.

For a local agent host, those are strong numbers.

#### D. The direct API surface is already testable at scale

The uploaded API validation output shows a large automated JSON-RPC validation suite running directly against the daemon. That matters for MCP because it proves there is already a direct typed surface worth exposing, not just a CLI presentation layer.

### 2.2 Verdict on suitability as an MCP server

For a **persistent local agent** in an IDE, desktop assistant, or MCP-capable editor, UFFS looks **very good** as an MCP backend.

For an **ephemeral one-shot agent**, UFFS is only as good as its warming strategy. The cold start is too large to hide if every MCP request launches a fresh daemon. But that is an architecture issue, not an engine issue.

Therefore the design recommendation is:

- **Local default:** stdio MCP server process plus long-lived warm daemon.
- **No CLI shell-out in production path.**
- **Optional explicit `warmup` maintenance path** for hosts that want to pre-heat the daemon.
- **Task/cursor patterns** for expensive or very large requests.

## Architectural recommendation

## The recommended shape

```text
LLM host / IDE / assistant
    |
    | MCP transport
    |  - stdio (default local)
    |  - Streamable HTTP (future / remote)
    v
uffs-mcp
    |
    | typed Rust adapter layer
    |  - schemas
    |  - policy and scope
    |  - result shaping
    |  - tasks / cursors
    |  - prompts / resources / completions
    v
uffs-client
    |
    | local JSON-RPC over AF_UNIX
    v
uffs-daemon
    |
    | canonical execution backend
    |  - search
    |  - aggregate
    |  - facet values
    |  - info
    |  - duplicate verify
    |  - export / shmem / streaming
    v
uffs-core + compact index + MFT sources
```

### Key decision

**Do not let the MCP server invent semantics.**

The MCP layer should translate between MCP concepts and the daemon's canonical typed request model, but:

- field meaning stays in `uffs-core`,
- filtering stays in the daemon/query layer,
- aggregation stays in the daemon/query layer,
- duplicate logic stays in the daemon/query layer,
- MCP only adds transport, capability negotiation, policy, schema, and result presentation.

This matches your own internal direction that aggregation should remain a first-class response path inside the same daemon-owned search contract and that all frontends should converge on one daemon path. [Internal: `UFFS_AGGREGATION_ARCHITECTURE_CONSOLIDATED.md`, `FILTER_SORT_FEATURE_MATRIX.md`]

## Why a CLI-wrapper MCP server is the wrong end state

A shell-out MCP server is tempting because it ships fast. But for UFFS it is the wrong long-term design.

### CLI-wrapper advantages

- fastest bootstrap,
- immediate feature coverage if CLI has the option,
- minimal Rust refactor up front,
- easy to test manually.

### CLI-wrapper problems

- process spawn cost becomes visible to the user,
- poor warm-state reuse if not carefully managed,
- weak introspection for agents,
- harder to expose typed schemas,
- harder to expose exactness/truncation/task metadata,
- harder to expose resources/prompts/completions coherently,
- duplicated parsing between CLI layer and daemon layer,
- bulk output shape is optimized for humans, not LLMs.

Given your timing evidence, the strongest practical objection is simple: **the daemon is fast, the CLI wrapper path is not nearly as fast**. The MCP server should talk to `uffs-client` or directly to the daemon API, not spawn `uffs` for every tool call.

## Why a daemon-native MCP layer is the right end state

A daemon-native MCP layer gives you:

- hot-index reuse,
- typed requests and responses,
- easier output schema generation,
- clean support for aggregation and drill-down,
- direct exposure of readiness, health, and loaded-drive state,
- simpler policy enforcement,
- clearer distinction between read-only tools and maintenance tools,
- a clean path to tasks, cursors, and resources.

## Recommended transport strategy

### 1. Local first: stdio MCP server

Official MCP still treats **stdio** as a first-class transport and says clients should support stdio whenever possible. For a local filesystem search engine, stdio is the right default. [1]

Why:

- simplest deployment,
- easiest agent/IDE integration,
- avoids auth complexity for purely local use,
- matches the existing `uffs-mcp` direction,
- keeps the trust boundary local.

### 2. Optional future: Streamable HTTP gateway

Official MCP now also supports **Streamable HTTP** as the other official transport. This is the right future option for team or remote deployments, but not the place to start for local NTFS host inspection. [1][8]

Use it later when you want:

- remote agent access,
- multi-user access controls,
- headless workstation/service use,
- fleet tooling,
- centralized auth.

If you add it, do not bolt it onto the daemon directly first. Add a **gateway process** that fronts the daemon, because the gateway needs HTTP auth, origin validation, rate limiting, and multi-tenant policy. Official security guidance is very explicit about origin validation and token scoping. [1][9][10]

## Cutting-edge MCP surface design

A strong MCP server today is not just a list of tools. It should use:

- **tools** for actions and queries,
- **outputSchema** and **structuredContent** for machine-readable results,
- **resources** for durable schemas, catalogs, and saved results,
- **prompts** for high-value guided workflows,
- **roots** to respect editor/workspace scope,
- **task support** for long-running work,
- **annotations** for read-only/idempotent hints. [2][3][4][5][6]

Your server should use all of those, not just tools.

## Recommended MCP tool set

I would separate tools into three layers.

### Layer A: always-on, read-only core tools

These are the tools an agent should almost always use.

#### 1. `uffs.search`

Purpose:
- known-item lookup,
- exact or filtered row retrieval,
- top-k row inspection,
- answer "show me the matching files/directories".

Input shape:
- pattern,
- predicates,
- sorts,
- projection,
- scope/drives,
- rowLimit,
- cursor,
- includeSchemaHints (optional),
- responseMode (`inline`, `resource_link`, `shmem` internal, etc.).

Output shape:
- `structuredContent` with rows, total match count, truncation metadata, execution time, cursor,
- compact text summary for compatibility,
- optional `resource_link` when payload is large.

Annotations:
- `readOnlyHint = true`,
- `destructiveHint = false`,
- `idempotentHint = true`,
- `openWorldHint = false`.

#### 2. `uffs.aggregate`

Purpose:
- counts,
- storage summaries,
- type/extension breakdowns,
- age distributions,
- path rollups,
- duplicate candidate analytics,
- any question that is naturally answered with buckets/metrics instead of rows.

This should be the centerpiece MCP tool after search.

Input shape:
- pattern,
- predicates,
- `aggregations` array or `profile`,
- includeRows,
- rowLimit,
- sampleProjection,
- exactness,
- bucket cursor / page size.

Output shape:
- domain summary,
- aggregate results,
- exact/truncated flags,
- `other_count`,
- bucket cursor,
- optional sample rows,
- optional drill-down predicate patch.

Annotations:
- read-only,
- idempotent,
- task support optional.

This aligns directly with your own consolidated aggregation design. [Internal: `UFFS_AGGREGATION_ARCHITECTURE_CONSOLIDATED.md`]

#### 3. `uffs.facet_values`

Purpose:
- search inside high-cardinality group keys,
- narrow candidate values for extension/path/type-like refinements,
- support iterative agent refinement without huge scans.

This is especially useful because official MCP completion is currently focused on prompt/resource argument completion, not guaranteed arbitrary tool-argument completion. `uffs.facet_values` therefore gives you a portable, host-independent refinement primitive.

Input shape:
- field,
- prefix or query,
- predicates,
- top,
- exactness,
- cursor.

Output shape:
- matched values,
- counts,
- cursor,
- exactness metadata,
- optional drill-down patches.

#### 4. `uffs.info`

Purpose:
- rich metadata for one selected path or record,
- answer "tell me everything important about this file/dir",
- bridge from a sampled bucket row to precise object detail.

Input shape:
- path or stable record reference,
- requested fields,
- optionally `includeColdFields`.

Output shape:
- one structured record,
- field provenance/access-tier metadata,
- warnings if cold fields are unavailable.

### Layer B: operational and introspection tools

These help agents know whether the server is ready, healthy, and scoped correctly.

#### 5. `uffs.status`

Purpose:
- tell the host whether daemon is cold/warm/loading,
- show loaded drives,
- show memory and uptime,
- show whether live MFT access is available.

This is especially useful because cold-start cost is UFFS's main weakness in one-shot scenarios.

#### 6. `uffs.capabilities`

Purpose:
- tell the agent which fields, aggregates, presets, duplicate modes, and cold-path capabilities are currently supported.

This can also be a resource rather than a tool, but I like exposing it both ways:

- resource for persistent schema browsing,
- tool for explicit agent checks.

#### 7. `uffs.explain_query`

Purpose:
- agent debugging and self-correction,
- show how pattern sugar, prefixes, filters, and defaults were normalized,
- explain which fields are hot/derived/cold,
- estimate expected cost class.

This is not required by MCP, but it is extremely useful for agentic reliability.

A good `explain_query` tool helps an LLM repair its own tool calls instead of hallucinating why a result was empty.

### Layer C: expensive or admin-class tools

These should exist, but often hidden by default or guarded by policy.

#### 8. `uffs.duplicate_verify`

Purpose:
- expensive second-stage verification of duplicate candidate groups.

This should support tasks. It is a perfect use of MCP task support because the work may be materially slower than normal hot-path queries. [3][7]

#### 9. `uffs.export`

Purpose:
- produce durable result resources,
- export large search or aggregate outputs without bloating normal tool responses,
- optionally write temp artifacts or named resources.

I would avoid making normal `uffs.search` write host files directly. Instead, `uffs.export` should materialize a resource or task result that the client can read as a resource.

#### 10. `uffs.refresh` and `uffs.warmup`

Purpose:
- maintenance,
- explicit daemon warming,
- explicit index refresh.

These are operationally useful but should be hidden or policy-gated in many hosts. They are not destructive to the filesystem, but they do mutate server state, consume resources, and may require elevation paths on Windows.

## Recommended resources

Resources are underused in many MCP servers. UFFS can make excellent use of them because it has stable schemas, presets, and performance/status facts.

I recommend at least the following URI families:

### 1. Schema resources

- `uffs://schema/fields`
- `uffs://schema/aggregates`
- `uffs://schema/presets`
- `uffs://schema/sorts`
- `uffs://schema/filters`
- `uffs://schema/tooling`

These should be generated from the same Rust metadata that powers the server. Your own docs already argue that aggregation capability must be generated from code rather than hand-maintained matrices. [Internal: `UFFS_AGGREGATION_ARCHITECTURE_CONSOLIDATED.md`]

### 2. Inventory/status resources

- `uffs://drives`
- `uffs://drives/{letter}`
- `uffs://status`
- `uffs://diagnostics/performance`

These are ideal for agent bootstrapping and quick introspection.

### 3. Result resources

- `uffs://results/{id}`
- `uffs://aggregates/{id}`
- `uffs://exports/{id}`
- `uffs://tasks/{id}`

These let you return lightweight tool responses that link to larger durable content.

### 4. Saved workflow resources

- `uffs://saved-query/{name}`
- `uffs://saved-aggregate/{name}`

That gives agents and users a stable way to reuse complex workflows.

## Recommended prompts

Prompts are especially valuable when the server wants to guide an LLM toward efficient use of the toolset.

Recommended prompts:

- `quick_overview`
- `storage_hotspots`
- `recent_activity`
- `cleanup_candidates`
- `duplicate_investigation`
- `workspace_changed_recently`
- `ntfs_attribute_audit`
- `largest_folders_in_scope`

Each prompt should explicitly tell the model which tool sequence to use. For example:

### `storage_hotspots`

Suggested tool sequence:
1. `uffs.aggregate` profile `overview`
2. `uffs.aggregate` profile `by_type`
3. `uffs.aggregate` rollup by path depth 2
4. optionally `uffs.search` on the hottest bucket

### `duplicate_investigation`

Suggested tool sequence:
1. narrow scope with `roots` or predicates,
2. `uffs.aggregate` duplicates candidate profile,
3. if needed `uffs.duplicate_verify` as a task,
4. `uffs.search` on selected duplicate group.

Official MCP prompts support structured argument definitions and prompt retrieval; they are a good fit here. [5]

## Roots and scoping design

This is one of the most important cutting-edge design decisions.

Many local MCP servers ignore `roots` and then accidentally become broader than the host workspace intended.

UFFS should do better.

### Recommended rule

When the client provides roots, the server should intersect them with:

- requested drive/path scope,
- local policy allowlist,
- any server-level sandboxing.

That gives a final effective search domain.

### Why this matters

It solves a common agent problem:

> "Search my current project" or "search under this folder".

Instead of making the model guess the path every time, the host can provide workspace roots and UFFS can respect them. Official MCP roots are specifically meant to communicate host-controlled filesystem boundaries and can notify servers when they change. [6]

### Practical recommendation

- If roots exist, default search and aggregate scope to those roots unless the model explicitly broadens scope.
- Expose effective scope in `uffs.status` and `uffs.explain_query`.
- Return scope in every aggregate/search response.

## Output design: how to make agents succeed

The most important result-shaping rules are:

### 1. Always return structured data first

Official MCP tools support `structuredContent` and optional `outputSchema`; clients should validate structured outputs, and the server should conform exactly when a schema is declared. The spec also recommends including a JSON text block for backwards compatibility when structured data is returned. [2][3]

For UFFS that means:

- tool result should contain `structuredContent`,
- schema version should be explicit,
- also include compact human text summary,
- do not rely on pretty text tables for MCP.

### 2. Never dump giant row sets inline by default

For inline MCP output, prefer:

- `rowLimit` defaults,
- cursors,
- resource links,
- aggregate-first responses,
- sample rows per bucket.

### 3. Always include exactness and truncation

Your own aggregate design is correct here: bucketed results should expose `exact`, `truncated`, `other_count`, and `next_bucket_cursor`. [Internal: `UFFS_AGGREGATION_ARCHITECTURE_CONSOLIDATED.md`]

### 4. Buckets should carry drill-down patches

Every aggregate bucket should optionally include the predicates needed to turn the bucket into a concrete follow-up search.

This is one of the biggest differences between a generic API and an agent-optimized one.

## How an agent can answer "any question" about NTFS on the host

The realistic answer is:

- **most host inventory, search, storage, path, date, type, and attribute questions are answerable now**, because the currently implemented fields are all hot or derived and already support rich filtering/sorting/projection;
- **some deeper forensic questions are not fully first-class yet**, because 17 cold-path fields are planned but not yet wired into the unified field model. Internal docs explicitly say those fields are parsed but not yet exposed for filter/sort/output. [Internal: `UFFS_AGGREGATION_ARCHITECTURE_CONSOLIDATED.md`, `FILTER_SORT_FEATURE_MATRIX.md`]

So the right framing in the design is:

### Already answerable well

- Where is file X?
- Which files match pattern Y?
- What changed recently?
- What kinds of files are here?
- Which types/extensions take the most space?
- Which folders are largest?
- Which paths are dangerously long?
- Which files are hidden/system/compressed/encrypted?
- Which directories are empty?
- Which files look wasteful by allocation ratio?
- Which duplicate candidates exist under this scope?

### Not yet fully first-class without Wave 5 / cold path

- precise FRS / parent FRS / base FRS questions,
- namespace questions,
- USN / LSN questions,
- security ID / owner ID questions,
- detailed reparse-tag questions,
- some forensic flag workflows.

That is not a flaw in the MCP design. It just means the MCP server should surface capability truth clearly instead of pretending all NTFS forensic depth is already present.

## Recommended agent playbook

An effective agent using UFFS should follow these patterns.

### Pattern 1: Start aggregate-first for summary questions

Question:
- "What takes up space on this machine?"

Plan:
1. `uffs.aggregate` with `overview`
2. `uffs.aggregate` with `by_type`
3. `uffs.aggregate` rollup by path depth 2
4. `uffs.search` only for the dominant bucket

Why:
- much lower token cost,
- much faster convergence,
- less hallucination risk.

### Pattern 2: Use `facet_values` before high-cardinality bucketing

Question:
- "What extensions under this directory look suspicious?"

Plan:
1. `uffs.facet_values` on `extension`
2. pick values or prefixes worth exploring,
3. `uffs.aggregate` or `uffs.search` on narrowed set.

### Pattern 3: Use roots for workspace-scoped search

Question:
- "What changed in this project this week?"

Plan:
1. read current roots,
2. default UFFS scope to those roots,
3. `uffs.search` with `newer=7d` and relevant sort,
4. optionally `uffs.aggregate` by type or folder.

### Pattern 4: Use duplicate verification only after narrowing

Question:
- "Do I have duplicate installers older than a year on D:?"

Plan:
1. filter to drive D, type executable or relevant extensions, older than 1y,
2. run duplicate candidate aggregation,
3. verify only the top groups if needed,
4. search/export concrete members.

That mirrors the design lessons in your aggregation docs and common file-search workflows.

## Canonical query model recommendation

The MCP layer should not have its own free-form query language.

Instead, everything should compile to the same canonical backend model:

```rust
SearchParams {
    pattern,
    predicates,
    sorts,
    projection,
    response_mode,
    aggregations,
    include_rows,
    row_limit,
    drives,
    profile,
}
```

That mirrors the direction in your own consolidated aggregation proposal. [Internal: `UFFS_AGGREGATION_ARCHITECTURE_CONSOLIDATED.md`]

### Why this matters

It prevents the MCP server from turning into:

- a natural-language parser,
- a second DSL,
- a SQL translation layer,
- a second source of truth.

The LLM can reason in natural language. The server should remain typed and strict.

## Field model recommendation

Your own docs point to the right move: make `FieldId`, `FieldMeta`, and `AggregateMeta` the single source of truth for:

- filterability,
- sortability,
- aggregatability,
- access tier,
- display names,
- aliases,
- schema generation.

The MCP server should build its schemas and resources from that metadata.

### Why this is especially important for MCP

Without code-generated field metadata, the MCP server will drift from:

- CLI help,
- daemon capability,
- docs,
- agent prompts,
- output schemas.

Your own docs already call this out explicitly. [Internal: `UFFS_AGGREGATION_ARCHITECTURE_CONSOLIDATED.md`]

### Practical recommendation

Generate from Rust metadata:

- JSON Schema for tool input,
- JSON Schema for structured output,
- resource content for `uffs://schema/*`,
- prompt argument choices,
- facet/groupability catalogs,
- capability flags for cold-path fields.

## Rust implementation proposal

## Recommendation: use the official Rust MCP SDK for protocol framing, not for business logic

There is now an official Rust SDK in the MCP ecosystem, and it is the right default for transport/capability negotiation in a Rust implementation. [11]

But keep your business logic in your own adapter layer.

### Good split of responsibilities

#### Let the SDK do

- server bootstrap,
- stdio transport handling,
- Streamable HTTP later if needed,
- capability advertisement,
- tool/resource/prompt registration,
- structured output framing,
- task plumbing where supported.

#### Keep UFFS-owned

- translation from MCP inputs to `SearchParams` / aggregate specs,
- policy and scope intersection,
- task orchestration for duplicate verify/export,
- schema generation from `FieldMeta` / `AggregateMeta`,
- result shaping and truncation policy,
- daemon client integration,
- performance instrumentation.

That gives you standards alignment without making core behavior hostage to SDK churn.

## Suggested crate/module layout

```text
crates/
  uffs-mcp/
    src/
      main.rs
      server.rs
      transport.rs
      backend.rs           # trait over uffs-client
      policy.rs
      schema.rs
      errors.rs
      telemetry.rs
      tasking.rs
      completions.rs
      prompts.rs
      resources.rs
      explain.rs
      tools/
        search.rs
        aggregate.rs
        facet_values.rs
        info.rs
        status.rs
        export.rs
        duplicate_verify.rs
        maintenance.rs
```

### Suggested internal traits

```rust
trait UffsBackend {
    async fn search(&self, req: SearchParams) -> Result<SearchResponse>;
    async fn aggregate(&self, req: SearchParams) -> Result<SearchResponse>;
    async fn info(&self, req: InfoRequest) -> Result<InfoResponse>;
    async fn status(&self) -> Result<StatusResponse>;
    async fn refresh(&self, req: RefreshRequest) -> Result<RefreshResponse>;
}

trait ScopePolicy {
    fn restrict_query(&self, query: SearchParams, roots: &[Url]) -> Result<SearchParams>;
}

trait ResultShaper {
    fn inline_or_link(&self, resp: SearchResponse) -> MappedMcpResult;
}
```

## Query/result shaping policy

This is the policy I would actually implement.

### Inline result defaults

- `uffs.search`: default inline row limit 50
- hard inline max maybe 200 or 500 rows depending on host
- `uffs.aggregate`: default inline top 20 buckets per agg
- sample rows default 1-3 only when requested or profile implies it

### When to switch to resource/task

- too many rows,
- too many buckets,
- duplicate verification requested,
- export requested,
- nested rollup response too large,
- host asks for durable artifact.

### Search vs aggregate default

- if user intent is summary, use aggregate
- if user intent is object retrieval, use search
- if any aggregate flag/profile is present and rows not requested, keep aggregate-only default

This is consistent with the aggregate design in your internal docs. [Internal: `UFFS_AGGREGATION_ARCHITECTURE_CONSOLIDATED.md`]

## Recommended task strategy

Official MCP task support is now available but still young; use it selectively. [3][7]

### Good task candidates

- duplicate verification with hashing,
- large export jobs,
- very large high-cardinality rollups,
- maybe full-drive warmup/refresh in some environments.

### Not good task candidates

- normal hot search,
- normal aggregate overview/by_type/by_extension,
- single-record info,
- simple facet lookup.

### Task output pattern

- immediate accepted response with task id,
- optional progress,
- final result as resource link,
- small terminal summary inline.

## Security and safety model

A local MCP file-search server has real security implications even if it is read-only.

### 1. Treat read-only as powerful

`uffs.search` may be read-only, but it is still highly sensitive because it can enumerate a host's filesystem.

### 2. Scope by default

Use roots, drive allowlists, path allowlists, and host policy. Do not assume the model should see the whole machine by default.

### 3. Separate user tools from maintenance tools

- public read-only tools: search, aggregate, info, status, schema
- opt-in/admin tools: refresh, warmup, low-level diagnostics

### 4. For remote HTTP mode, enforce real auth

If you later add Streamable HTTP:

- validate origin,
- bind localhost for local-only mode,
- validate token audience correctly,
- never pass through client tokens to upstream services,
- keep stdio auth separate from HTTP auth.

Those are all explicitly reinforced by current MCP transport and authorization guidance. [1][9][10]

## Annotation recommendations

For tools that are always read-only:

- `readOnlyHint = true`
- `destructiveHint = false`
- `idempotentHint = true`
- `openWorldHint = false`

For long-running read-only tools:

- same as above,
- plus `execution.taskSupport = optional`.

For maintenance tools like `refresh` or `warmup`:

- `readOnlyHint = false`
- `destructiveHint = false`
- `idempotentHint = false` or omitted
- hide behind policy where appropriate.

Remember that MCP annotations are hints, not authorization. The spec is explicit that clients should not make security decisions purely from annotations supplied by untrusted servers. [4]

## What to do with prompts, completions, and resources in practice

### Prompts

Use prompts to teach efficient tool sequencing.

### Resources

Use resources for durable schemas, capabilities, saved queries, exported results, and diagnostics.

### Completions

Use MCP completion where supported for:

- preset names,
- field names,
- drive identifiers,
- resource URIs,
- prompt arguments.

But do not rely on host support for arbitrary tool-argument completion. For high-cardinality runtime value spaces, `uffs.facet_values` is the portable answer.

## Aggregation strategy: the real differentiator

This is where UFFS can move from "fast file search exposed via MCP" to "best-in-class agent filesystem intelligence backend".

The internal aggregation design is already strong, and I would adopt it almost completely.

### What should ship first

Your own rollout plan is correct:

### Stage 1

- `--count`
- `overview`
- `by_type`
- `by_extension`
- `by_drive`
- `by_size`
- basic MCP aggregate tool

### Stage 2

- per-bucket metrics
- sample rows
- `storage`, `activity`, `media`, `cleanup`
- rollups by drive and path depth

### Stage 3

- bucket pagination
- `facet_values`
- hierarchical rollups
- exactness/truncation polish

### Stage 4

- duplicate candidates
- reclaimable bytes
- duplicate verify task path

### Stage 5

- advanced cold/forensic fields
- percentiles
- disjunctive facets

That sequencing is very sensible because it maximizes agent value quickly without forcing deep cold-path exposure before the unified field model is stable. [Internal: `UFFS_AGGREGATION_ARCHITECTURE_CONSOLIDATED.md`]

### Why this is especially important for agents

Agents disproportionately ask summary questions:

- how many,
- how much,
- what types,
- what changed,
- where are the hotspots,
- what looks suspicious,
- what should I inspect next.

Aggregations answer those better than row dumps.

### Likely aggregate performance envelope

Your internal aggregate draft estimates on 25M records roughly:

- overview around **50 ms**,
- by_type around **60 ms**,
- by_extension around **80 ms**,
- duplicates-by-name around **200 ms**. [Internal: `AGGREGATION_ARCHITECTURE.md`]

If those numbers hold in the production daemon path, UFFS becomes a genuinely elite local-agent summarization backend.

## Design alternatives and trade-offs

## Alternative A: one monolithic `uffs.query` tool

### Pros

- smallest visible tool list,
- one canonical schema,
- easy to evolve.

### Cons

- larger and more complex single schema,
- harder for weaker models to discover best mode,
- harder for hosts to present intent cleanly,
- fewer semantic guardrails.

### Verdict

Good internally, weaker externally.

## Alternative B: many small specialized tools

### Pros

- clearer intent,
- easier tool selection by agent,
- simpler schemas.

### Cons

- tool list sprawl,
- more server registration boilerplate,
- more versioning surface.

### Verdict

Best for MCP host usability if kept disciplined.

## Alternative C: curated hybrid (recommended)

Expose a small, semantically clean set:

- `uffs.search`
- `uffs.aggregate`
- `uffs.facet_values`
- `uffs.info`
- `uffs.status`
- `uffs.capabilities`
- optional: `uffs.export`, `uffs.duplicate_verify`, `uffs.refresh`

This is what I recommend.

## Alternative D: natural-language query tool inside the server

Example:
- `uffs.ask("find the biggest hidden videos from last month")`

### Pros

- friendly for non-LLM humans,
- superficially simple.

### Cons

- duplicates host-model reasoning,
- increases ambiguity,
- harder to test,
- harder to make deterministic,
- invites semantic drift.

### Verdict

Do not make this the primary design. Let the host LLM reason; keep the server typed.

## Alternative E: remote-only HTTP MCP server

### Pros

- central deployment,
- works for remote agents.

### Cons

- unnecessary auth and security burden for local use,
- more operational complexity,
- weaker fit for host-local NTFS inspection.

### Verdict

Add later as a gateway, not as the first-class local shape.

## How I would actually implement it, phase by phase

## Phase 0: tighten the existing MCP adapter

Goal: move from rudimentary adapter to standards-aligned local server.

Do now:

- use official Rust MCP SDK for transport/capability surface,
- keep `uffs-client` backend integration,
- add `outputSchema` for all stable tools,
- return `structuredContent` plus concise text,
- add `uffs.status` and `uffs.capabilities`,
- add resources for `schema/fields`, `schema/presets`, `drives`, `status`,
- add prompts for overview/hotspots/cleanup,
- add roots-aware scoping policy,
- add read-only annotations,
- add `explain_query`.

## Phase 1: make aggregation first-class in MCP

Goal: make the server truly agent-native.

Do next:

- `uffs.aggregate`
- aggregate profiles
- sample rows
- drill-down patches
- exact/truncated/cursor metadata
- resource-backed large aggregate results.

## Phase 2: high-cardinality navigation

Goal: let agents refine without prompting blindly.

Do next:

- `uffs.facet_values`
- bucket pagination
- capability resource for groupable/aggregatable fields
- more saved aggregate presets.

## Phase 3: tasks and exports

Goal: support expensive work and durable outputs.

Do next:

- duplicate verify task flow
- export task flow
- durable resource links
- progress reporting when host supports tasks.

## Phase 4: cold-path and forensic expansion

Goal: broaden the server from storage/search intelligence to deeper NTFS forensic support.

Do later:

- expose cold-path fields once unified field work lands,
- add availability/cost flags for those fields,
- add forensic prompts/resources,
- keep default tools focused on hot/derived fields unless explicitly requested.

## Specific design decisions I recommend

1. **Do not shell out to the CLI in the steady-state MCP path.**
2. **Keep `uffs-daemon` as the only execution backend.**
3. **Adopt the official Rust MCP SDK for protocol mechanics, not semantics.**
4. **Ship stdio first; add Streamable HTTP later via a gateway.**
5. **Expose aggregation as a first-class MCP tool immediately after core search/status.**
6. **Use resources and prompts aggressively; do not stop at tools.**
7. **Intersect MCP roots with UFFS scope and policy.**
8. **Generate schemas from `FieldId` / `FieldMeta` / `AggregateMeta`.**
9. **Treat large results as resources or tasks, not giant inline blobs.**
10. **Surface capability truth explicitly, especially for not-yet-wired cold NTFS fields.**

## Final judgment

From a high-level architecture point of view, UFFS is already unusually well positioned to become a best-in-class MCP server for filesystem intelligence.

Why:

- the daemon architecture is already the right shape,
- the warm query numbers are strong,
- the backend is already typed and testable,
- the CLI/TUI are already converging on one daemon path,
- your internal aggregate design is exactly the right next move,
- the current MCP spec now has the primitives needed to expose UFFS properly.

So my recommendation is not "build an MCP wrapper around UFFS".

It is:

> **Turn UFFS into a daemon-native, schema-driven MCP filesystem intelligence service whose primary agent surfaces are search, aggregation, refinement, and drill-down.**

That is the design that best fits the current UFFS architecture, your timing evidence, and the latest MCP protocol direction.

## Appendix A: concise assessment of current speed

### Search quality as MCP backend

- Hot daemon path: strong.
- Warm local editor/IDE workflow: strong.
- Aggregate-first agent workflows: potentially excellent.
- High concurrency: healthy.
- One-shot cold start: weak unless daemon is already warm.

### Practical interpretation

- Great for local persistent assistants.
- Great for IDEs/editors with long-lived sessions.
- Good for workstation agents if you prewarm or tolerate first-hit cold cost.
- Poor as a naive fork-per-call CLI wrapper.

## Appendix B: recommended public MCP-facing tools and defaults

| Tool | Default mode | Notes |
|---|---|---|
| `uffs.search` | rows, limit 50 | Known-item lookup and concrete enumeration |
| `uffs.aggregate` | aggregate-only | Summary/distribution/rollup/default for analysis questions |
| `uffs.facet_values` | top 20 | High-cardinality refinement |
| `uffs.info` | one record | Rich detail for selected path/record |
| `uffs.status` | singleton | Warm/cold/loading/drive/memory info |
| `uffs.capabilities` | singleton | Field/preset/tool/cold-path truth |
| `uffs.export` | task/resource | Large durable outputs |
| `uffs.duplicate_verify` | task | Expensive second-stage verification |
| `uffs.refresh` | hidden/policy-gated | Maintenance |
| `uffs.warmup` | hidden/policy-gated | Explicit preheating |

## Appendix C: examples of agent questions and best tool sequences

### "What kinds of files dominate this drive?"

1. `uffs.aggregate(profile="overview", predicates=[drive=D])`
2. `uffs.aggregate(profile="by_type", predicates=[drive=D])`
3. optional `uffs.search` on chosen type bucket

### "Show me the biggest folders under this workspace"

1. get roots
2. `uffs.aggregate(agg=rollup:path, depth=2, roots=current workspace)`
3. `uffs.search` on selected bucket

### "Did anything suspicious change recently?"

1. `uffs.aggregate(profile="recent_activity", newer=7d)`
2. `uffs.search(sort=modified:desc, newer=7d, filters for hidden/system/compressed/reparse as needed)`

### "Are there duplicate installers older than a year?"

1. `uffs.aggregate(duplicates:size+name, predicates=[older=365d, type=executable])`
2. `uffs.duplicate_verify` task for top groups if needed
3. `uffs.search` to enumerate members

### "Why is this subtree bloated?"

1. `uffs.aggregate(profile="storage", scope=subtree)`
2. `uffs.aggregate(histogram=bulkiness, scope=subtree)`
3. `uffs.aggregate(by_extension, scope=subtree)`
4. `uffs.search(sort=sizeondisk:desc, scope=subtree)`

## Appendix D: source notes used for this proposal

### Internal UFFS docs provided in this review

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
- uploaded `Output` timing log

### Key internal points relied on

- unified daemon architecture already implemented,
- `uffs-client` and `uffs-mcp` already exist,
- measured warm search around 9 ms median,
- steady-state daemon memory around 7.3 GiB,
- aggregation design already points to typed daemon-owned MCP-first structured results,
- current implemented field universe is 39 hot/derived fields with 17 cold-path fields still planned.

## Appendix E: external MCP references

[1] Model Context Protocol: Transports  
https://modelcontextprotocol.io/specification/draft/basic/transports

[2] Model Context Protocol: Tools  
https://modelcontextprotocol.io/specification/draft/server/tools

[3] Model Context Protocol: Schema reference (2025-11-25)  
https://modelcontextprotocol.io/specification/2025-11-25/schema

[4] Model Context Protocol: Tool annotations / task support in schema  
https://modelcontextprotocol.io/specification/2025-11-25/schema

[5] Model Context Protocol: Prompts  
https://modelcontextprotocol.io/specification/draft/server/prompts

[6] Model Context Protocol: Roots  
https://modelcontextprotocol.io/specification/draft/client/roots

[7] Model Context Protocol: Tasks draft  
https://modelcontextprotocol.io/specification/draft/basic/utilities/tasks

[8] OpenAI / MCP transport roadmap note (Streamable HTTP remains official with stdio)  
https://www.anthropic.com/news/mcp-roadmap

[9] Model Context Protocol: Security best practices  
https://modelcontextprotocol.io/specification/draft/basic/security_best_practices

[10] Model Context Protocol: Authorization  
https://modelcontextprotocol.io/specification/draft/basic/authorization

[11] Official Rust SDK for MCP  
https://modelcontextprotocol.io/docs/sdk/rust
