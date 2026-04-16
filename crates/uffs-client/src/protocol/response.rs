// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Response types, RPC convenience methods, and command parameter types.

use serde::{Deserialize, Serialize};

use super::{
    AggregateResultWire, BucketWire, RpcError, RpcErrorResponse, RpcRequest, RpcResponse,
    SearchResponseMode, SearchSortSpec,
};

/// Parameters for the `refresh` method.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct RefreshParams {
    /// Specific drives to refresh (empty = all loaded).
    #[serde(default)]
    pub drives: Vec<char>,
}

/// Parameters for the `load_drive` method.
///
/// Tells the daemon to hot-load one or more MFT files that it doesn't
/// already have.  Used when the CLI connects to an already-running daemon
/// that was started without a particular drive's data.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct LoadDriveParams {
    /// MFT file paths to load (absolute paths).
    #[serde(default)]
    pub mft_files: Vec<String>,
    /// Skip cache when loading.
    #[serde(default)]
    pub no_cache: bool,
}

/// Response for the `load_drive` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct LoadDriveResponse {
    /// Drives that were successfully loaded.
    pub loaded: Vec<char>,
    /// Drives that were already present (skipped).
    pub already_loaded: Vec<char>,
    /// Errors encountered (drive letter → message).
    pub errors: Vec<String>,
}

/// Parameters for the `facet_values` convenience method.
///
/// Retrieves the distinct values (with counts) for a given field.
/// Internally translates to a `search` with a `terms` aggregation.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct FacetValuesParams {
    /// Field to facet on (e.g. `"extension"`, `"type"`).
    #[serde(default = "default_facet_field")]
    pub field: String,

    /// Optional glob pattern to restrict which records are included.
    /// Defaults to `"*"` (all records).
    #[serde(default = "default_pattern")]
    pub pattern: String,

    /// Maximum number of values to return per page.
    /// Defaults to `50`.
    #[serde(default)]
    pub page_size: Option<u16>,

    /// Cursor token from a previous response for pagination.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Default facet field.
fn default_facet_field() -> String {
    "extension".to_owned()
}

/// Default pattern.
fn default_pattern() -> String {
    "*".to_owned()
}

/// Response from the `facet_values` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct FacetValuesResponse {
    /// The field that was faceted.
    pub field: String,

    /// Facet values with counts, sorted by count descending.
    pub values: Vec<BucketWire>,

    /// Total number of distinct values (before pagination).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_distinct: Option<usize>,

    /// Cursor for the next page.  `None` when all values fit in this
    /// page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Parameters for the `info` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct InfoParams {
    /// Full path to look up.
    pub path: String,
}

/// Parameters for the `keepalive` method.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct KeepaliveParams {
    /// Session type hint for idle timeout differentiation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_type: Option<String>,
}

// ────────────────────────────────────────────────────────────────────────────
// Method responses
// ────────────────────────────────────────────────────────────────────────────

/// Response for the `search` method.
///
/// Results are delivered either inline (`rows`) or via shared memory
/// (`shmem_path`).  When `shmem_path` is set, `rows` is empty and the
/// client should read the file with [`crate::shmem::read_search_results`].
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResponse {
    /// Matching result rows (inline delivery — empty when shmem is used).
    pub rows: Vec<SearchRow>,
    /// Total number of matching records (before `limit` truncation).
    ///
    /// When the search uses a `limit`, only a subset of rows is returned
    /// but `total_count` reflects the full match count.
    #[serde(default)]
    pub total_count: u64,
    /// Total records scanned.
    pub records_scanned: usize,
    /// Search duration in milliseconds.
    pub duration_ms: u64,
    /// Whether results were truncated by limit.
    pub truncated: bool,
    /// Path to a shared-memory file containing the results (D5.0).
    ///
    /// When set, `rows` is empty and the file should be read with
    /// [`crate::shmem::read_search_results`].  The client is responsible
    /// for deleting the file after reading.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shmem_path: Option<String>,
    /// Number of rows in the shmem file (present only when `shmem_path`
    /// is set).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shmem_count: Option<u64>,
    /// Detailed timing breakdown from the daemon (only when
    /// `SearchParams::profile` was `true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<SearchProfile>,
    /// Effective canonical sort clauses applied by the daemon.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_sorts: Vec<SearchSortSpec>,
    /// Effective projection fields applied by the daemon.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_projection: Vec<String>,
    /// Response shaping mode for the payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_mode: Option<SearchResponseMode>,
    /// Projected rows for direct daemon callers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projected_rows: Option<Vec<serde_json::Map<String, serde_json::Value>>>,
    /// Aggregation results (present when `SearchParams::aggregations` was
    /// non-empty).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aggregations: Vec<AggregateResultWire>,
}

/// Daemon-side timing breakdown returned when `SearchParams::profile` is set.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SearchProfile {
    /// Daemon uptime in milliseconds (time since daemon started).
    pub uptime_ms: u64,
    /// Total startup duration: first drive start → last drive ready (ms).
    pub startup_ms: u64,
    /// Time to acquire the `RwLock` + prepare filters (ms).
    pub lock_ms: u64,
    /// Pure search time across all drives (ms).
    pub search_ms: u64,
    /// Time to convert `DisplayRow` → `SearchRow` (ms).
    pub row_build_ms: u64,
    /// JSON serialization / shmem write time (ms).
    pub serialize_ms: u64,
    /// Per-drive breakdown.
    pub drives: Vec<DriveProfile>,
}

/// Per-drive timing within a search (search + load/startup metrics).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DriveProfile {
    /// Drive letter.
    pub drive: char,
    /// Records in this drive's index.
    pub records: usize,
    /// Matching rows found in this search.
    pub matches: usize,
    // ── Startup/load timing (captured once at daemon start) ─────
    /// Compact-cache deserialization time (ms). 0 if cache miss.
    #[serde(default)]
    pub cache_ms: u64,
    /// MFT read time (ms). 0 if cache hit.
    #[serde(default)]
    pub mft_ms: u64,
    /// Compact index build time (ms). 0 if cache hit.
    #[serde(default)]
    pub compact_ms: u64,
    /// Trigram index build time (ms). 0 if cache hit.
    #[serde(default)]
    pub trigram_ms: u64,
}

/// A single search result row.
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchRow {
    /// Drive letter.
    pub drive: char,
    /// Full resolved path.
    pub path: String,
    /// Filename.
    pub name: String,
    /// File size in bytes.
    pub size: u64,
    /// Whether this is a directory.
    pub is_directory: bool,
    /// Last modified time (Unix microseconds).
    pub modified: i64,
    /// Creation time (Unix microseconds).
    pub created: i64,
    /// Last access time (Unix microseconds).
    pub accessed: i64,
    /// Raw NTFS attribute flags.
    pub flags: u32,
    /// Allocated size on disk.
    pub allocated: u64,
    /// Descendant count.
    pub descendants: u32,
    /// Subtree size.
    pub treesize: u64,
    /// Sum of allocated sizes in entire subtree (directories only).
    #[serde(default)]
    pub tree_allocated: u64,
}

/// Response for the `drives` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct DrivesResponse {
    /// Loaded drives with record counts.
    pub drives: Vec<DriveInfo>,
}

/// Information about a loaded drive.
#[derive(Debug, Serialize, Deserialize)]
pub struct DriveInfo {
    /// Drive letter.
    pub letter: char,
    /// Number of records in the compact index.
    pub records: usize,
    /// Source (e.g. `"cache"`, `"live"`, `"mft_file"`).
    pub source: String,
}

/// Response for the `status` method.
#[derive(Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    /// Current daemon status.
    pub status: DaemonStatus,
    /// Daemon uptime in seconds.
    pub uptime_secs: u64,
    /// Number of active connections.
    pub connections: usize,
    /// Daemon process ID.
    pub pid: u32,
    /// Process RSS (resident set size) in bytes, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_bytes: Option<u64>,
    /// Calculated heap footprint of all loaded indices (bytes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_heap_bytes: Option<u64>,
    /// Per-drive memory breakdown (drive letter → heap bytes).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drive_memory: Vec<DriveMemoryInfo>,
}

/// Per-drive memory breakdown for status reporting.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DriveMemoryInfo {
    /// Drive letter.
    pub drive: char,
    /// Number of records in this drive's index.
    pub records: usize,
    /// Calculated heap footprint in bytes.
    pub heap_bytes: u64,
    /// Breakdown: records Vec.
    pub records_bytes: u64,
    /// Breakdown: names Vec.
    pub names_bytes: u64,
    /// Breakdown: trigram index.
    pub trigram_bytes: u64,
    /// Breakdown: children index.
    pub children_bytes: u64,
    /// Breakdown: extension index.
    pub ext_index_bytes: u64,
}

/// Response for the `stats` method — daemon performance metrics.
#[derive(Debug, Serialize, Deserialize)]
pub struct StatsResponse {
    /// Total search queries served since startup.
    pub total_queries: u64,
    /// Cumulative search time in microseconds.
    pub total_query_time_us: u64,
    /// Average query time in microseconds.
    pub avg_query_time_us: f64,
    /// Time from daemon start to `Ready` in milliseconds.
    pub startup_duration_ms: u64,
    /// Daemon uptime in seconds.
    pub uptime_secs: u64,
    /// Total records across all loaded drives.
    pub total_records: usize,
    /// Queries per second (over daemon lifetime).
    pub queries_per_second: f64,
}

/// Daemon operational status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state")]
pub enum DaemonStatus {
    /// Daemon is loading indices.
    #[serde(rename = "loading")]
    Loading {
        /// Drives loaded so far.
        drives_loaded: usize,
        /// Total drives to load.
        drives_total: usize,
    },
    /// Daemon is ready to serve queries.
    #[serde(rename = "ready")]
    Ready,
    /// Daemon is refreshing one or more drives.
    #[serde(rename = "refreshing")]
    Refreshing {
        /// Drives being refreshed.
        drives: Vec<char>,
    },
}

/// Response for the `info` method (all 25 columns for a path).
#[derive(Debug, Serialize, Deserialize)]
pub struct InfoResponse {
    /// Whether the path was found.
    pub found: bool,
    /// File details (if found).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record: Option<serde_json::Value>,
}

// ────────────────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────────────────

impl RpcRequest {
    /// Create a new JSON-RPC 2.0 request.
    #[must_use]
    pub fn new(id: u64, method: &str, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id: Some(id),
            method: method.to_owned(),
            params,
        }
    }
}

impl RpcResponse {
    /// Create a success response.
    #[must_use]
    pub fn success(id: u64, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            result,
        }
    }
}

impl RpcErrorResponse {
    /// Create an error response.
    #[must_use]
    pub fn error(id: Option<u64>, code: i32, message: &str) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            error: RpcError {
                code,
                message: message.to_owned(),
                data: None,
            },
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Display Helpers
// ────────────────────────────────────────────────────────────────────────────

/// Format a byte count as human-readable size (e.g. "1.23 MB").
#[must_use]
#[expect(
    clippy::float_arithmetic,
    reason = "floating-point division is intentional for human-readable size formatting"
)]
pub fn format_size(bytes: u64) -> String {
    #[expect(clippy::cast_precision_loss, reason = "u64→f64 acceptable for display")]
    let bytes_f64 = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes_f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes_f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes_f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// Format a raw FILETIME timestamp as `YYYY-MM-DD HH:MM`.
///
/// Decomposes the FILETIME directly — no intermediate Unix conversion.
#[must_use]
#[expect(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "Hinnant algorithm: intermediate values are bounded for valid dates"
)]
pub fn format_time(filetime: i64) -> String {
    const TICKS_PER_SECOND: i64 = 10_000_000; // 100-ns intervals per second
    if filetime == 0 {
        return "—".to_owned();
    }
    let total_secs = filetime / TICKS_PER_SECOND;
    let days_since_1601 = total_secs.div_euclid(86400);
    let day_secs = total_secs.rem_euclid(86400);
    let hour = (day_secs / 3600) as u32;
    let minute = ((day_secs % 3600) / 60) as u32;

    // Hinnant civil_from_days:
    // 719468 (0000-03-01→1970-01-01) − 134774 (1601-01-01→1970-01-01) = 584694
    let z = days_since_1601 + 584_694;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = i64::from(yoe) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────
