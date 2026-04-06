# Aggregation Architecture Design

> **Status:** Draft — 2026-04-05
> **Scope:** CLI `--aggregate` flags, daemon `aggregate` RPC, MCP `uffs_aggregate` tool

---

## 1. Problem Statement

Today every UFFS query returns **rows** (individual file/dir records). AI agents
via MCP — and power users on the CLI — frequently need **summary statistics**
instead:

| Natural-language question | What's needed |
|--------------------------|---------------|
| "How many PDF files on this machine?" | `count where ext=pdf` |
| "Total size of all video files?" | `sum(size) where type=video` |
| "Disk usage breakdown by extension?" | `group_by(ext) → count, sum(size)` |
| "Show me the file age distribution" | `group_by(date_bucket) → count, sum(size)` |
| "Which file types waste the most space?" | `group_by(ext) → sum(allocated - size)` |
| "How many duplicate filenames?" | `group_by(name) having count>1` |
| "Storage used per drive?" | `group_by(drive) → count, sum(size), sum(allocated)` |

Returning 10 000 raw rows and asking the LLM to count them is **wasteful**,
**slow** (token budget), and **inaccurate** (truncated results). The daemon
must compute aggregates server-side and return compact JSON.

---

## 2. Design Principles

1. **Same pipeline, different output** — aggregation reuses the existing
   search pipeline (pattern + all 30+ filters). It diverges only at the
   final stage: instead of materializing `DisplayRow`s, we accumulate
   statistics.

2. **Operate on CompactRecord directly** — avoid path resolution and
   `DisplayRow` construction for pure aggregation. This enables O(N)
   single-pass over the compact index with zero allocations per record.

3. **Two-tier API** — simple "profile" presets for MCP agents (one string)
   and a composable `group_by` + `metrics` API for CLI power users.

4. **Extension-aware from day one** — `extension_id` and `ext_names` are
   already in `DriveCompactIndex`; `semantic_type_from_extension()` maps
   extension → category. Aggregation taps these directly.

5. **MCP-first** — the primary consumer is AI agents. Output is structured
   JSON, not a pretty table. CLI formats it for humans.

---

## 3. Grouping Dimensions

Each dimension defines how records are bucketed before aggregate functions
run. Multiple dimensions can be combined (cross-tabulation).

| Dimension | Key type | Source | Cost |
|-----------|----------|--------|------|
| `extension` | `String` | `rec.extension_id` → `drive.ext_names[id]` | O(1)/rec |
| `type` | `&'static str` | `semantic_type_from_extension(ext)` | O(1)/rec |
| `drive` | `char` | `drive.letter` (outer loop) | free |
| `size_bucket` | `&'static str` | `classify_size(rec.size)` | O(1)/rec |
| `date_bucket` | `&'static str` | `classify_date(rec.modified)` ¹ | O(1)/rec |
| `year` | `u16` | extract year from `rec.modified` ¹ | O(1)/rec |
| `month` | `u8` | extract month from `rec.modified` ¹ | O(1)/rec |
| `day_of_week` | `u8` | extract weekday from `rec.modified` ¹ | O(1)/rec |
| `hour_of_day` | `u8` | extract hour from `rec.modified` ¹ | O(1)/rec |
| `attribute` | `String` | decode `rec.flags` bits | O(1)/rec |
| `depth` | `u8` | `rec.path_len`-based or parent chain walk | O(d)/rec |
| `top_folder` | `String` | first path component after drive root | O(d)/rec ² |
| `name` | `String` | `rec.name(drive.names)` — for duplicate detection | O(1)/rec ³ |
| `is_directory` | `bool` | `rec.is_directory()` | O(1)/rec |

> ¹ Configurable: `--agg-date-field modified|created|accessed` (default: `modified`).
> ² Requires parent chain walk — can be expensive on cold caches; optional dimension.
> ³ Builds a HashMap — memory proportional to unique names. Guard with result limits.

### 3.1 Size Buckets

```
Empty     : 0 bytes
Tiny      : 1 B – 1 KB
Small     : 1 KB – 1 MB
Medium    : 1 MB – 100 MB
Large     : 100 MB – 1 GB
Huge      : 1 GB – 10 GB
Massive   : 10 GB+
```

### 3.2 Date Buckets

```
today       : within 24h
this_week   : within 7d
this_month  : within 30d
this_quarter: within 90d
this_year   : within 365d
last_year   : 365d – 730d
older       : > 730d
ancient     : > 5 years
```

---

## 4. Aggregate Metrics

Per group (and in the global summary), the following metrics are computed:

| Metric | Type | Description |
|--------|------|-------------|
| `count` | `u64` | Number of records in group |
| `file_count` | `u64` | Files only (excludes dirs) |
| `dir_count` | `u64` | Directories only |
| `total_size` | `u64` | `sum(size)` — logical bytes |
| `total_allocated` | `u64` | `sum(allocated)` — on-disk bytes |
| `waste` | `u64` | `total_allocated - total_size` (slack space) |
| `waste_pct` | `f64` | `waste / total_allocated * 100` |
| `min_size` | `u64` | Smallest file in group |
| `max_size` | `u64` | Largest file in group |
| `avg_size` | `f64` | `total_size / file_count` |
| `median_size` | `u64` | Median file size (optional — requires sort) |
| `newest` | `i64` | `max(modified)` — Unix µs |
| `oldest` | `i64` | `min(modified)` — Unix µs |
| `total_descendants` | `u64` | `sum(descendants)` — dirs only |
| `total_treesize` | `u64` | `sum(treesize)` — dirs only |

**Default metrics** (when not explicitly specified): `count`, `total_size`,
`total_allocated`, `waste`.

**Expensive metrics** (opt-in): `median_size` (requires per-group sort).

---

## 5. Pre-built Profiles

Profiles are one-word shortcuts that expand to `(group_by, metrics, sort, limit)`.
MCP agents can say `"profile": "by_extension"` instead of composing raw params.

| Profile | Group By | Metrics | Sort | Limit | Description |
|---------|----------|---------|------|-------|-------------|
| `overview` | *(none)* | all | — | — | Global summary, no grouping |
| `by_extension` | `extension` | count, total_size, avg_size | size desc | 50 | Top extensions by space |
| `by_type` | `type` | count, total_size, waste | size desc | 30 | Semantic category breakdown |
| `by_drive` | `drive` | count, total_size, total_allocated, waste | size desc | 26 | Per-drive summary |
| `by_size` | `size_bucket` | count, total_size | count desc | 8 | Size distribution histogram |
| `by_age` | `date_bucket` | count, total_size | *(bucket order)* | 8 | Temporal distribution |
| `by_year` | `year` | count, total_size | year desc | 20 | Per-year breakdown |
| `by_attribute` | `attribute` | count, total_size | count desc | 20 | NTFS attribute flags |
| `storage_report` | `type` + `drive` | count, total_size, waste | size desc | — | Compound: type×drive |

---

## 6. Data Types & Rust Structures

### 6.1 Protocol Types (uffs-client/protocol.rs)

```rust
/// Parameters for the `aggregate` daemon method.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct AggregateParams {
    // ── Same search scope as SearchParams ─────────────────────
    /// Search pattern (glob, regex, substring). Empty/"*" = all records.
    pub pattern: String,
    /// Case-sensitive matching.
    #[serde(default)]
    pub case_sensitive: bool,
    /// Filter mode: "all", "files", "dirs".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// Drives to aggregate (empty = all loaded).
    #[serde(default)]
    pub drives: Vec<char>,

    // ── Aggregation-specific ──────────────────────────────────
    /// Pre-built profile name (overrides group_by/metrics/sort/limit).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Grouping dimensions: "extension", "type", "drive", etc.
    #[serde(default)]
    pub group_by: Vec<String>,
    /// Metrics to compute: "count", "total_size", "waste", etc.
    /// Empty = default set (count, total_size, total_allocated, waste).
    #[serde(default)]
    pub metrics: Vec<String>,
    /// Sort groups by this metric (default: "total_size").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<String>,
    /// Sort direction for groups (default: true = descending).
    #[serde(default = "default_true")]
    pub sort_desc: bool,
    /// Maximum groups to return (default: 50).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Minimum count threshold — exclude groups with fewer records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_count: Option<u64>,
    /// Date field for temporal grouping: "modified" (default), "created", "accessed".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_field: Option<String>,
}

/// Response for the `aggregate` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct AggregateResponse {
    /// Global summary (always present).
    pub summary: AggregateSummary,
    /// Grouped results (empty for "overview" profile).
    pub groups: Vec<AggregateGroup>,
    /// Total records scanned.
    pub records_scanned: u64,
    /// Computation duration in milliseconds.
    pub duration_ms: u64,
    /// Whether groups were truncated by limit.
    pub truncated: bool,
    /// Profile used (if any).
    pub profile: Option<String>,
    /// Grouping dimensions used.
    pub group_by: Vec<String>,
}

/// Global summary statistics (always computed).
#[derive(Debug, Serialize, Deserialize)]
pub struct AggregateSummary {
    pub total_count: u64,
    pub file_count: u64,
    pub dir_count: u64,
    pub total_size: u64,
    pub total_allocated: u64,
    pub waste: u64,
    pub waste_pct: f64,
    pub avg_file_size: f64,
    pub max_file_size: u64,
    pub min_file_size: u64,
    pub newest_modified: i64,
    pub oldest_modified: i64,
    /// Number of unique extensions seen.
    pub unique_extensions: u32,
    /// Number of unique semantic types seen.
    pub unique_types: u32,
}

/// A single aggregation group.
#[derive(Debug, Serialize, Deserialize)]
pub struct AggregateGroup {
    /// Group key(s) — one per grouping dimension.
    /// E.g., {"extension": "pdf"} or {"type": "video", "drive": "D"}.
    pub key: serde_json::Map<String, serde_json::Value>,
    /// Computed metrics for this group.
    pub metrics: AggregateMetrics,
}

/// Computed metrics within an aggregation group.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct AggregateMetrics {
    pub count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_allocated: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waste: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waste_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_size: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_descendants: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_treesize: Option<u64>,
}
```

### 6.2 Core Engine Types (uffs-core/aggregate/)

```rust
/// Which dimension to group by (parsed from string).
pub enum GroupDimension {
    Extension,
    Type,          // semantic_type_from_extension
    Drive,
    SizeBucket,
    DateBucket,    // configurable: modified/created/accessed
    Year,
    Month,
    DayOfWeek,
    HourOfDay,
    Attribute,     // NTFS flags
    TopFolder,     // first path segment after drive root
    Name,          // for duplicate detection
    IsDirectory,
    Depth,         // directory depth (from path_len or parent walk)
}

/// Which metrics to compute.
pub enum MetricId {
    Count,
    FileCount,
    DirCount,
    TotalSize,
    TotalAllocated,
    Waste,
    WastePct,
    MinSize,
    MaxSize,
    AvgSize,
    MedianSize,
    Newest,
    Oldest,
    TotalDescendants,
    TotalTreesize,
}

/// Accumulator for one group — updated per record.
struct GroupAccumulator {
    count: u64,
    file_count: u64,
    dir_count: u64,
    total_size: u64,
    total_allocated: u64,
    min_size: u64,     // init: u64::MAX
    max_size: u64,     // init: 0
    newest: i64,       // init: i64::MIN
    oldest: i64,       // init: i64::MAX
    descendants: u64,
    treesize: u64,
    // For median: optional Vec<u64> (only when median requested)
    sizes: Option<Vec<u64>>,
}
```

---

## 7. Data Flow

```
┌──────────────┐   ┌──────────────┐   ┌──────────────────────┐
│ CLI / MCP /  │──▶│ Daemon RPC   │──▶│ IndexManager         │
│ TUI          │   │ "aggregate"  │   │   .aggregate(params) │
└──────────────┘   └──────────────┘   └──────────┬───────────┘
                                                  │
                         ┌────────────────────────▼──────────────────────┐
                         │ AggregationEngine::run()                      │
                         │                                                │
                         │ 1. Parse profile / expand to (group_by, etc.) │
                         │ 2. For each drive (parallel):                  │
                         │    a. Apply pattern match (reuse search logic) │
                         │    b. Apply filters (reuse SearchFilters)      │
                         │    c. For each matching CompactRecord:         │
                         │       - Extract group key(s) from record       │
                         │       - Accumulate into GroupAccumulator        │
                         │ 3. Merge per-drive accumulators                │
                         │ 4. Compute derived metrics (avg, pct, median) │
                         │ 5. Sort groups, apply limit & min_count       │
                         │ 6. Build AggregateResponse                    │
                         └───────────────────────────────────────────────┘
```

### 7.1 Performance Path

For the **common case** (no pattern / pattern="*", group by extension/type):

1. **No path resolution needed** — extension_id and flags are in CompactRecord.
2. **No DisplayRow construction** — operate directly on `&CompactRecord`.
3. **Single pass** — iterate records once, O(N) with O(G) memory where G = groups.
4. **Parallel per drive** — each drive accumulates independently, merge at the end.

Expected performance on 25M records:
- `overview` (no grouping): **~50ms** (single counter pass)
- `by_extension` (1000 groups): **~80ms** (hash map per drive, merge)
- `by_type` (24 groups): **~60ms** (fixed array, no hash)
- `duplicates` (group by name): **~200ms** (hash map with string keys)

### 7.2 Pattern + Filter Integration

Aggregation reuses the existing search infrastructure:

```
AggregateParams.pattern  ──▶  same glob/regex/substring matching
AggregateParams.filter   ──▶  same FilterMode (all/files/dirs)
+ future filter fields   ──▶  same SearchFilters / FieldPredicate
```

The key difference: after filtering, instead of collecting `DisplayRow`s up
to a limit, we **stream** matching records through the accumulator. No limit
on input records — only on output groups.

---

## 8. CLI Interface

### 8.1 New Flags

```
AGGREGATION OPTIONS:
  --aggregate <PROFILE>      Run an aggregation profile instead of returning rows.
                              Profiles: overview, by_extension, by_type, by_drive,
                              by_size, by_age, by_year, by_attribute, storage_report,
                              duplicates, waste_analysis, temporal, top_folders
  --group-by <DIM[,DIM]>     Custom grouping (overrides profile): extension, type,
                              drive, size_bucket, date_bucket, year, month,
                              day_of_week, hour_of_day, attribute, top_folder,
                              name, is_directory, depth
  --agg-metrics <M[,M]>      Metrics to compute: count, total_size, total_allocated,
                              waste, waste_pct, min_size, max_size, avg_size,
                              median_size, newest, oldest (default: count,total_size,
                              total_allocated,waste)
  --agg-sort <METRIC>         Sort groups by metric (default: total_size)
  --agg-limit <N>             Max groups to return (default: 50)
  --agg-min-count <N>         Exclude groups with count < N
  --agg-date-field <FIELD>    Date field for temporal grouping: modified (default),
                              created, accessed
```

### 8.2 Example Commands

```bash
# Quick overview of entire filesystem
uffs search "*" --aggregate overview

# Top 20 extensions by disk space
uffs search "*" --aggregate by_extension --agg-limit 20

# Size breakdown of all Rust files
uffs search "*.rs" --aggregate by_size

# Video files per drive
uffs search "*" --type video --aggregate by_drive

# Files modified this year grouped by type
uffs search "*" --modified-after 2026-01-01 --aggregate by_type

# Custom: extension × drive cross-tab, sorted by waste
uffs search "*" --group-by extension,drive --agg-sort waste --agg-limit 100

# Find duplicate filenames (potential duplicates)
uffs search "*" --aggregate duplicates --agg-min-count 3

# Storage waste analysis
uffs search "*" --aggregate waste_analysis
```

### 8.3 Output Formats

```bash
# Default: human-readable table
uffs search "*" --aggregate by_extension

# JSON (for piping / MCP)
uffs search "*" --aggregate by_extension --format json

# CSV (for spreadsheet import)
uffs search "*" --aggregate by_extension --format csv
```

---

## 9. Daemon Protocol

### 9.1 JSON-RPC Method

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "aggregate",
  "params": {
    "pattern": "*",
    "profile": "by_extension",
    "filter": "files",
    "limit": 20
  }
}
```

### 9.2 Response

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "summary": {
      "total_count": 2450000,
      "file_count": 2100000,
      "dir_count": 350000,
      "total_size": 1894327156736,
      "total_allocated": 1921457823744,
      "waste": 27130667008,
      "waste_pct": 1.41,
      "avg_file_size": 902060,
      "max_file_size": 68719476736,
      "min_file_size": 0,
      "newest_modified": 1743868800000000,
      "oldest_modified": 946684800000000,
      "unique_extensions": 1847,
      "unique_types": 24
    },
    "groups": [
      {
        "key": {"extension": "dll"},
        "metrics": {
          "count": 185000,
          "total_size": 412000000000,
          "total_allocated": 415000000000,
          "waste": 3000000000
        }
      },
      {
        "key": {"extension": "exe"},
        "metrics": {
          "count": 42000,
          "total_size": 298000000000,
          "total_allocated": 301000000000,
          "waste": 3000000000
        }
      }
    ],
    "records_scanned": 2450000,
    "duration_ms": 78,
    "truncated": true,
    "profile": "by_extension",
    "group_by": ["extension"]
  }
}
```

---

## 10. MCP Integration

### 10.1 New MCP Tool: `uffs_aggregate`

```json
{
  "name": "uffs_aggregate",
  "description": "Aggregate file system statistics. Returns counts, sizes, and distributions instead of individual files. Use this when you need summary statistics, breakdowns, or distributions — NOT individual file listings. Supports pre-built profiles (by_extension, by_type, by_drive, etc.) or custom group_by dimensions.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "profile": {
        "type": "string",
        "description": "Pre-built aggregation profile. One of: overview (global stats), by_extension (top extensions by size), by_type (semantic category breakdown: code/picture/video/etc.), by_drive (per-drive summary), by_size (size distribution), by_age (temporal distribution), by_year (per-year), by_attribute (NTFS flags), storage_report (type×drive compound), duplicates (duplicate filenames), waste_analysis (slack space by extension), temporal (year-month), top_folders (largest root dirs)."
      },
      "pattern": {
        "type": "string",
        "default": "*",
        "description": "Search pattern to scope the aggregation (glob, regex with > prefix, or substring). Default '*' = all files."
      },
      "filter": {
        "type": "string",
        "default": "all",
        "description": "Record type filter: 'all', 'files', 'dirs'."
      },
      "group_by": {
        "type": "array",
        "items": {"type": "string"},
        "description": "Custom grouping dimensions (overrides profile): extension, type, drive, size_bucket, date_bucket, year, month, day_of_week, hour_of_day, attribute, top_folder, name, is_directory, depth."
      },
      "sort_by": {
        "type": "string",
        "default": "total_size",
        "description": "Sort groups by this metric: count, total_size, total_allocated, waste, avg_size."
      },
      "sort_desc": {"type": "boolean", "default": true},
      "limit": {"type": "integer", "default": 50, "description": "Max groups to return."},
      "min_count": {"type": "integer", "description": "Exclude groups with fewer than this many records."}
    }
  }
}
```

### 10.2 MCP Usage Scenarios

These are the kinds of questions AI agents will ask — the `uffs_aggregate` tool
enables answering them in a **single tool call** with compact JSON output:

| Agent Question | Profile/Group | Why Aggregation |
|----------------|---------------|-----------------|
| "How much disk space is used?" | `overview` | 1 response vs 25M rows |
| "What types of files take the most space?" | `by_type` | 24 groups vs millions of rows |
| "Are there any duplicate files?" | `duplicates` | Count groups vs scanning all |
| "Show me a breakdown of files by age" | `by_age` | 8 time buckets, instant |
| "How much space do videos take?" | `overview` + filter `--type video` | Scoped summary |
| "Which drive has the most free space waste?" | `by_drive` | Per-drive waste metrics |
| "What percentage of files are images?" | `by_type` | Percentage from count |
| "Show me the 10 largest file extensions" | `by_extension` limit=10 | Top-N |
| "When were most files last modified?" | `by_year` or `temporal` | Temporal distribution |
| "How fragmented is my disk?" | `waste_analysis` | Slack space analysis |
| "What's in my Documents folder?" | `by_type` + path pattern | Scoped aggregation |
| "Compare storage usage across drives" | `by_drive` | Side-by-side comparison |

### 10.3 MCP Prompts (New)

```json
{
  "name": "disk_usage_report",
  "description": "Generate a comprehensive disk usage report with storage breakdown by type, drive, and file age.",
  "arguments": []
}
```

Prompt template:
> "Use the uffs_aggregate tool three times:
> 1. profile='overview' to get the global summary
> 2. profile='by_type' to get category breakdown
> 3. profile='by_drive' to get per-drive usage
>
> Present the results as a formatted report with sections for each."

---

## 11. Architecture: Where Code Lives

```
crates/
├── uffs-core/src/
│   ├── aggregate/
│   │   ├── mod.rs              # AggregationEngine, public API
│   │   ├── dimensions.rs       # GroupDimension enum + key extraction
│   │   ├── metrics.rs          # MetricId, GroupAccumulator, computation
│   │   ├── profiles.rs         # Profile presets → (group_by, metrics, ...)
│   │   ├── buckets.rs          # Size/date bucket classification
│   │   └── accumulator.rs      # Per-drive accumulator, merge logic
│   └── search/
│       └── backend.rs          # (unchanged — aggregation uses CompactRecord directly)
│
├── uffs-client/src/
│   └── protocol.rs             # + AggregateParams, AggregateResponse, etc.
│
├── uffs-daemon/src/
│   └── index.rs                # + IndexManager::aggregate() method
│
├── uffs-mcp/src/
│   └── main.rs                 # + uffs_aggregate tool + disk_usage_report prompt
│
└── uffs-cli/src/
    └── commands/                # aggregate command or --aggregate flag handling
```

### 11.1 Dependency Direction

```
uffs-core/aggregate   ← uses CompactRecord, DriveCompactIndex, ext_names,
                        semantic_type_from_extension, SearchFilters
uffs-daemon           ← calls AggregationEngine from index.rs
uffs-client/protocol  ← defines wire types (AggregateParams/Response)
uffs-mcp              ← translates MCP tool call to AggregateParams
uffs-cli              ← formats AggregateResponse as table/json/csv
```

No new crate needed. The `aggregate/` module lives in `uffs-core` next to
`search/`, sharing the same `DriveCompactIndex` data structures.

---

## 12. Implementation Plan

### Wave 1 — Core Engine (uffs-core)

| Task | Priority | Notes |
|------|----------|-------|
| `GroupDimension` enum + `parse()` | High | extension, type, drive, size_bucket, date_bucket, year |
| `MetricId` enum + `parse()` | High | count, total_size, total_allocated, waste, etc. |
| `GroupAccumulator` + `merge()` | High | Per-group statistics accumulation |
| `AggregationEngine::run()` | High | Main entry point: pattern + filter + accumulate |
| Size/date bucket classification | High | `classify_size()`, `classify_date()` |
| Profile presets | High | Expand "by_extension" → params |
| Cross-tabulation (multi-dim) | Medium | HashMap with composite keys |
| Median computation | Low | Optional, requires per-group Vec |
| `top_folder` dimension | Medium | Parent chain walk to depth 1 |
| `name` dimension (duplicates) | Medium | HashMap<String, Accumulator> |

### Wave 2 — Protocol & Daemon

| Task | Priority | Notes |
|------|----------|-------|
| `AggregateParams` / `AggregateResponse` | High | Wire types in protocol.rs |
| `IndexManager::aggregate()` | High | Daemon handler |
| `UffsClient::aggregate()` | High | Client method |
| JSON-RPC `"aggregate"` method dispatch | High | In daemon request handler |
| Round-trip serialization tests | High | Parity with search tests |

### Wave 3 — MCP

| Task | Priority | Notes |
|------|----------|-------|
| `uffs_aggregate` tool in tools/list | High | Schema with profile + custom |
| `dispatch_tool_call` → aggregate | High | Translate MCP params → AggregateParams |
| Format aggregate output for LLM | High | Structured text, not raw JSON |
| `disk_usage_report` prompt | Medium | Multi-call prompt template |
| MCP protocol tests | High | End-to-end with mock daemon |

### Wave 4 — CLI

| Task | Priority | Notes |
|------|----------|-------|
| `--aggregate` clap flag | High | Enum of profiles or custom |
| `--group-by`, `--agg-sort`, etc. | High | CLI argument parsing |
| Table formatter for groups | High | Aligned columns, human-readable |
| `--format json` for aggregation | Medium | Pipe-friendly output |
| `--format csv` for aggregation | Low | Spreadsheet export |

### Wave 5 — Advanced

| Task | Priority | Notes |
|------|----------|-------|
| Attribute-level grouping (19 bool flags) | Medium | Per-flag counting |
| `depth` dimension | Medium | From path_len heuristic |
| `hour_of_day` / `day_of_week` | Low | Temporal pattern analysis |
| Multi-dimension cross-tabs | Medium | extension×drive, type×age |
| Streaming/incremental mode | Low | For very large result sets |
| Histogram text rendering | Low | ASCII bar charts in CLI |

---

## 13. Performance Considerations

### 13.1 Fast Path (No String Keys)

Dimensions with fixed/small key spaces use **array-indexed** accumulators
instead of `HashMap`:

| Dimension | Accumulator | Max slots |
|-----------|------------|-----------|
| `drive` | `[GroupAccumulator; 26]` | 26 |
| `type` | `[GroupAccumulator; 24]` | 24 (FileCategory variants) |
| `size_bucket` | `[GroupAccumulator; 7]` | 7 |
| `date_bucket` | `[GroupAccumulator; 8]` | 8 |
| `is_directory` | `[GroupAccumulator; 2]` | 2 |
| `day_of_week` | `[GroupAccumulator; 7]` | 7 |
| `hour_of_day` | `[GroupAccumulator; 24]` | 24 |
| `month` | `[GroupAccumulator; 12]` | 12 |

Only `extension`, `year`, `top_folder`, `name` require `HashMap`.

### 13.2 Extension Aggregation — Zero String Allocation

`extension_id` is already in `CompactRecord`. For "by_extension" grouping:

1. Use `HashMap<u16, GroupAccumulator>` — keyed by extension_id, not string.
2. Resolve `ext_names[id]` to string only for the **final top-N groups**.
3. This means 25M records → 0 string allocations during accumulation.

### 13.3 Parallel Accumulation

```
Drive C: records[0..12M] → Accumulator_C (thread 1)
Drive D: records[0..8M]  → Accumulator_D (thread 2)
Drive E: records[0..5M]  → Accumulator_E (thread 3)
                              ↓ merge ↓
                     Final Accumulator → Response
```

Merge is O(G) where G = number of groups — trivial compared to scan time.

### 13.4 Memory Budget

| Scenario | Groups | Memory |
|----------|--------|--------|
| overview (no groups) | 1 | ~128 bytes |
| by_type | 24 | ~3 KB |
| by_extension | ~2000 | ~256 KB |
| duplicates (by name) | ~1M unique | ~100 MB (watch out!) |

**Guard:** `duplicates` profile caps at 100K unique names by default.
Records beyond that are counted but not tracked individually.

---

## 14. Future Extensions

### 14.1 Compound Aggregations

Profile `storage_report` runs **multiple group_by passes** and returns a
nested structure. This is a natural extension — the daemon handler calls
`AggregationEngine::run()` multiple times and merges responses.

### 14.2 Time-Series Queries

"Show me disk growth over time" requires historical snapshots (not in scope
for V1). However, `by_year` + `by_month` on `created` dates gives a proxy
for file creation velocity.

### 14.3 TUI Integration

The TUI can display aggregate results in:
- A bar chart widget (ratatui bar chart)
- A pie chart (custom widget)
- A treemap for storage visualization

This is a separate feature (TUI aggregate view) that consumes the same
`AggregateResponse` from the daemon.

### 14.4 Percentile/Histogram

Beyond median, we could support `p50`, `p90`, `p99` size percentiles. This
requires sorting the per-group size vectors — only for explicitly requested
metrics.

### 14.5 Saved Aggregation Queries

Allow users to define named aggregate queries in a config file:
```toml
[aggregations.my_code_stats]
pattern = "*.rs"
profile = "by_size"
filter = "files"
```

---

## 15. Comparison with Existing Tools

| Feature | UFFS | Everything | WizTree | TreeSize |
|---------|------|-----------|---------|----------|
| Extension breakdown | ✅ `by_extension` | ❌ manual | ✅ built-in | ✅ built-in |
| Type categorization | ✅ 24 categories | ❌ | ❌ | ❌ |
| Temporal distribution | ✅ `by_age/year/month` | ❌ | ❌ | ❌ |
| Waste/slack analysis | ✅ `waste_analysis` | ❌ | ❌ | ❌ |
| Duplicate detection | ✅ `duplicates` (by name) | ❌ | ❌ | ❌ |
| Per-drive comparison | ✅ `by_drive` | ❌ | ❌ | ❌ |
| Size histogram | ✅ `by_size` | ❌ | ❌ | ❌ |
| NTFS attribute stats | ✅ `by_attribute` | ❌ | ❌ | ❌ |
| Cross-tabulation | ✅ multi-dim group_by | ❌ | ❌ | ❌ |
| API/MCP queryable | ✅ JSON-RPC + MCP | HTTP API (limited) | ❌ | ❌ |
| Speed (25M records) | **~80ms** | N/A | seconds | seconds |
| Sub-pattern scoping | ✅ any filter combo | ❌ | ❌ | partial |

**Key differentiator:** No other MFT-based tool offers server-side
aggregation with AI-agent-friendly output. UFFS is the only tool where
an LLM can ask "what percentage of my disk is videos?" and get a single
compact JSON response in <100ms.
