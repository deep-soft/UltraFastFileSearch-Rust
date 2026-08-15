// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Output schemas for MCP tool `outputSchema` and `structuredContent`.
//!
//! Each struct derives [`serde::Serialize`] and [`schemars::JsonSchema`] so
//! it can be used with [`rmcp::model::Tool::with_output_schema`] and
//! serialized into [`rmcp::model::CallToolResult::structured_content`].

use schemars::JsonSchema;
use serde::Serialize;

// ── uffs_search ─────────────────────────────────────────────────────

/// Structured output for `uffs_search`.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct SearchOutput {
    /// Number of matching rows returned in this page.
    pub returned: usize,
    /// Total matching records (before limit/pagination).
    pub total_count: u64,
    /// Total records scanned across all drives.
    pub records_scanned: usize,
    /// Scan time in milliseconds — excludes index warm-up, which is
    /// reported separately as `promotion_ms`.
    pub duration_ms: u64,
    /// Milliseconds spent paging parked/cold drives back in before the
    /// scan could run; `0` on a warm index.  Without this a query that
    /// warmed for 21 s and scanned for 1 ms reports `duration_ms: 1`,
    /// hiding the expensive case entirely.
    pub promotion_ms: u64,
    /// Whether more results exist beyond this page.
    pub truncated: bool,
    /// Opaque cursor for fetching the next page (null when no more pages).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(default)]
    pub next_cursor: Option<String>,
    /// Warnings about adjusted parameters (e.g. limit was capped).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[schemars(default)]
    pub warnings: Vec<String>,
    /// Matching file/directory rows.
    pub rows: Vec<SearchRowOutput>,
}

/// A single search result row (structured).
///
/// Mirrors every field from [`uffs_client::protocol::response::SearchRow`] so
/// `structuredContent` exposes 100% of the data the CLI/API returns.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct SearchRowOutput {
    /// Drive letter (single ASCII A..=Z over the wire — see
    /// `DriveLetter` serde impl).  JSON schema is `char` to preserve
    /// the prior MCP contract since `DriveLetter` lives in `uffs-mft`
    /// (no `schemars` dep).
    #[schemars(with = "char")]
    pub drive: uffs_mft::platform::DriveLetter,
    /// Filename.
    pub name: String,
    /// File extension (lowercase, without leading dot). Empty for directories
    /// and files without an extension.
    pub ext: String,
    /// Entry type: `"file"` or `"dir"`.
    pub r#type: String,
    /// File size in bytes.
    pub size: u64,
    /// Allocated size on disk in bytes.
    pub allocated: u64,
    /// Last modified time (Unix microseconds).
    pub modified: i64,
    /// Creation time (Unix microseconds).
    pub created: i64,
    /// Last access time (Unix microseconds).
    pub accessed: i64,
    /// Raw NTFS `FILE_ATTRIBUTE_*` flags.
    pub flags: u32,
    /// Whether this is a directory.
    pub is_directory: bool,
    /// Descendant count (directories only, 0 for files).
    pub descendants: u32,
    /// Sum of logical file sizes in entire subtree (directories only).
    pub treesize: u64,
    /// Sum of allocated sizes in entire subtree (directories only).
    pub tree_allocated: u64,
    /// Full resolved path.
    pub path: String,
}

// ── uffs_info ───────────────────────────────────────────────────────

/// Structured output for `uffs_info`.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct InfoOutput {
    /// Whether the path was found in the index.
    pub found: bool,
    /// Detailed file record (all NTFS columns).
    /// Null when `found` is false.
    #[schemars(default)]
    pub record: Option<serde_json::Value>,
}

// ── uffs_drives ─────────────────────────────────────────────────────

/// Structured output for `uffs_drives`.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct DrivesOutput {
    /// Number of loaded drives.
    pub count: usize,
    /// Per-drive details.
    pub drives: Vec<DriveOutput>,
}

/// A single drive entry (structured).
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct DriveOutput {
    /// Drive letter (e.g. `DriveLetter::C`).  Serialized as a single
    /// ASCII char (e.g. `"C"`) to keep wire compatibility with prior
    /// `char`-typed schema.  JSON schema is `char` (see [`SearchRowOutput`]).
    #[schemars(with = "char")]
    pub letter: uffs_mft::platform::DriveLetter,
    /// Number of records in the compact index.  **`0` for a `parked` or
    /// `cold` drive** — the body is released, not empty; the count
    /// returns when the drive re-warms.  Read `tier` before concluding
    /// a drive holds nothing.
    pub records: usize,
    /// Data source (`"cache"`, `"live"`, `"mft_file"`).
    pub source: String,
    /// Memory tier: `"hot"` / `"warm"` (searchable now) or
    /// `"parked"` / `"cold"` (body released — a query re-warms it,
    /// taking 30–120 s).  `null` from a pre-tiering daemon.
    pub tier: Option<String>,
}

// ── uffs_status ─────────────────────────────────────────────────────

/// Structured output for `uffs_status`.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct StatusOutput {
    /// Daemon **process** state: `"running"`, `"loading (3/7 drives)"`,
    /// or `"refreshing (C, D)"`.
    ///
    /// Deliberately never says "ready" — that word belongs to
    /// `index_ready` alone.  A running daemon can have every drive
    /// parked, and when both fields said "ready"/"false" the payload
    /// answered the same apparent question two ways; the wrong one was
    /// found first.
    pub daemon_process: String,
    /// `true` when every loaded drive is `hot`/`warm`, i.e. a query
    /// answers immediately.  `false` means at least one drive is
    /// parked/cold and a query against it triggers a 30–120 s re-warm.
    /// This is the field to poll while waiting out a warm.
    pub index_ready: bool,
    /// Per-drive tier, `"C"` → `"warm"`.  The authoritative answer to
    /// "is the index actually ready", which the lifecycle `status`
    /// field above does not give.
    pub drives: alloc::collections::BTreeMap<String, String>,
    /// Records resident across all loaded drives.  **`0` while every
    /// drive is parked** — bodies released, not an empty index.  Reads
    /// as the corroborating signal for `index_ready`.
    pub total_records: usize,
    /// Index bytes resident in the heap, in MB.  `0` while parked;
    /// several GB when warm.  `null` from a daemon that does not
    /// report it.
    pub index_heap_mb: Option<u64>,
    /// Drives currently being paged in, e.g. `["E"]`.
    ///
    /// A re-warm is stepwise per drive, so `total_records` plateaus for
    /// tens of seconds while one large drive loads.  Without knowing a
    /// load is in flight, three identical polls read as "hung" and the
    /// natural response is to give up — the one remaining way a caller
    /// bails early on a warm that is working fine.  Empty means nothing
    /// is loading right now.
    pub currently_loading: Vec<String>,
    /// Records expected once every drive is warm — the denominator for
    /// `total_records`.  `null` when no drive has been warm yet, so
    /// there is genuinely nothing to measure against.
    pub records_when_warm: Option<u64>,
    /// Re-warm progress, 0–100, as `total_records / records_when_warm`.
    /// `null` when the denominator is unknown.
    ///
    /// Measured in **records, not drives**: drive-count progress
    /// misleads badly, since drives differ in size by three orders of
    /// magnitude (four of seven warm was only 24 % of records).
    pub warming_progress_pct: Option<u8>,
    /// Daemon uptime in seconds.
    pub uptime_secs: u64,
    /// Number of active connections.
    pub connections: usize,
    /// Daemon process ID.
    pub pid: u32,
    /// Version of the running `uffsmcp` server binary (e.g. `"0.6.10"`). Lets
    /// an agent see which UFFS build is serving it. UFFS can self-update,
    /// so an occasional `uffs --update` (run by the user) keeps
    /// capabilities current — see the server instructions.
    pub server_version: String,
}

// ── uffs_aggregate ──────────────────────────────────────────────────

/// Structured output for `uffs_aggregate`.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct AggregateOutput {
    /// Total records scanned.
    pub records_scanned: usize,
    /// Query execution time in milliseconds.
    pub duration_ms: u64,
    /// Aggregation result buckets (raw daemon wire format).
    pub aggregations: serde_json::Value,
    /// Opaque cursor for fetching the next page of buckets (null when no
    /// more pages).  Only present when `page_size` was set in the request.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(default)]
    pub next_cursor: Option<String>,
}

// ── uffs_facet_values ───────────────────────────────────────────────

/// Structured output for `uffs_facet_values`.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct FacetValuesOutput {
    /// The field that was faceted.
    pub field: String,
    /// Total records scanned.
    pub records_scanned: usize,
    /// Query execution time in milliseconds.
    pub duration_ms: u64,
    /// Aggregation result buckets.
    pub aggregations: serde_json::Value,
    /// Opaque cursor for fetching the next page of facet values (null when no
    /// more pages).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(default)]
    pub next_cursor: Option<String>,
}
