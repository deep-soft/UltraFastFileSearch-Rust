//! JSON-RPC 2.0 protocol types shared between client and daemon.
//!
//! These types define the wire format for IPC communication. Both
//! `uffs-daemon` and `uffs-client` depend on this module.

mod response;
mod search_params;
#[cfg(test)]
mod tests;

pub use response::*;
use serde::{Deserialize, Serialize};

// ────────────────────────────────────────────────────────────────────────────
// JSON-RPC 2.0 envelope
// ────────────────────────────────────────────────────────────────────────────

/// JSON-RPC 2.0 request.
#[derive(Debug, Serialize, Deserialize)]
pub struct RpcRequest {
    /// Must be `"2.0"`.
    pub jsonrpc: String,
    /// Request ID (correlates request → response). `None` for notifications.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    /// Method name (e.g. `"search"`, `"drives"`, `"status"`).
    pub method: String,
    /// Method parameters (JSON object or array).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 success response.
#[derive(Debug, Serialize, Deserialize)]
pub struct RpcResponse {
    /// Must be `"2.0"`.
    pub jsonrpc: String,
    /// Matching request ID.
    pub id: u64,
    /// Result payload (method-specific).
    pub result: serde_json::Value,
}

/// JSON-RPC 2.0 error response.
#[derive(Debug, Serialize, Deserialize)]
pub struct RpcErrorResponse {
    /// Must be `"2.0"`.
    pub jsonrpc: String,
    /// Matching request ID.
    pub id: Option<u64>,
    /// Error details.
    pub error: RpcError,
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Serialize, Deserialize)]
pub struct RpcError {
    /// Error code (standard JSON-RPC or application-specific).
    pub code: i32,
    /// Human-readable error message.
    pub message: String,
    /// Optional structured error data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// JSON-RPC 2.0 notification (no `id`, no response expected).
#[derive(Debug, Serialize, Deserialize)]
pub struct RpcNotification {
    /// Must be `"2.0"`.
    pub jsonrpc: String,
    /// Notification method (e.g. `"drive_loaded"`, `"refresh_complete"`).
    pub method: String,
    /// Notification parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

// Standard JSON-RPC error codes
/// Parse error (invalid JSON).
pub const ERR_PARSE: i32 = -32700;
/// Invalid request (missing fields).
pub const ERR_INVALID_REQUEST: i32 = -32600;
/// Method not found.
pub const ERR_METHOD_NOT_FOUND: i32 = -32601;
/// Invalid parameters.
pub const ERR_INVALID_PARAMS: i32 = -32602;
/// Internal error.
pub const ERR_INTERNAL: i32 = -32603;

/// Canonical result filter mode for file-vs-directory selection.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchFilterMode {
    /// Return both files and directories.
    All,
    /// Return files only.
    Files,
    /// Return directories only.
    Dirs,
}

/// Canonical sort direction in the daemon wire contract.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchSortDirection {
    /// Ascending order.
    Asc,
    /// Descending order.
    Desc,
}

/// Canonical sort clause in the daemon wire contract.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct SearchSortSpec {
    /// Canonical field name or accepted alias.
    pub field: String,
    /// Explicit direction. When omitted, the daemon applies the field default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<SearchSortDirection>,
}

/// Canonical predicate operator in the daemon wire contract.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchPredicateOp {
    /// Equality comparison.
    Eq,
    /// Inequality comparison.
    Ne,
    /// Strictly less-than comparison.
    Lt,
    /// Less-than-or-equal comparison.
    Lte,
    /// Strictly greater-than comparison.
    Gt,
    /// Greater-than-or-equal comparison.
    Gte,
    /// Membership in a set of values.
    In,
    /// Exclusion from a set of values.
    NotIn,
    /// Field contains all listed values.
    HasAll,
    /// Field contains any listed value.
    HasAny,
    /// Field contains none of the listed values.
    HasNone,
    /// Pattern/glob match.
    Match,
    /// Negated pattern/glob match.
    NotMatch,
    /// Case-insensitive substring containment.
    Contains,
    /// Case-insensitive prefix match.
    StartsWith,
    /// Case-insensitive suffix match.
    EndsWith,
}

/// Canonical predicate value in the daemon wire contract.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SearchPredicateValue {
    /// String scalar.
    String(String),
    /// String list.
    StringList(Vec<String>),
    /// Unsigned integer scalar.
    U64(u64),
    /// Signed integer scalar.
    I64(i64),
    /// Boolean scalar.
    Bool(bool),
}

/// Canonical predicate clause in the daemon wire contract.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct SearchPredicate {
    /// Canonical field name or accepted alias.
    pub field: String,
    /// Comparison operator.
    pub op: SearchPredicateOp,
    /// Predicate operand.
    pub value: SearchPredicateValue,
}

/// Canonical response shaping mode for direct daemon callers.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchResponseMode {
    /// Traditional full-row response.
    Rows,
    /// Projected JSON objects keyed by projected field name.
    Json,
}

// Application error codes (daemon-specific)
/// Daemon is still loading indices.
pub const ERR_NOT_READY: i32 = -1;
/// Search pattern compilation failed (bad regex).
pub const ERR_BAD_PATTERN: i32 = -2;

// ────────────────────────────────────────────────────────────────────────────
// Method parameters
// ────────────────────────────────────────────────────────────────────────────

/// Parameters for the `search` method.
///
/// All filter fields mirror the CLI surface; see
/// [`uffs_core::search::filters::SearchFilters`] for semantics.
/// Every field is optional — omitted fields impose no constraint.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "JSON wire type — bools are the natural encoding"
)]
pub struct SearchParams {
    // ── Core ────────────────────────────────────────────────────────
    /// Search pattern (glob, regex with `>` prefix, or substring).
    pub pattern: String,
    /// Case-sensitive matching.
    #[serde(default)]
    pub case_sensitive: bool,
    /// Whole-word matching.
    #[serde(default)]
    pub whole_word: bool,
    /// Match pattern against the full path (not just the filename).
    ///
    /// When true, directory records whose name matches the pattern will also
    /// contribute all their descendants to the result set.  Default (`false`)
    /// matches filename-only, consistent with Everything's default behaviour.
    #[serde(default)]
    pub match_path: bool,

    // ── Sort ────────────────────────────────────────────────────────
    /// Sort column name (e.g. `"modified"`, `"size"`, `"name"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    /// Canonical ordered sort clauses. Preferred over legacy `sort`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sorts: Vec<SearchSortSpec>,
    /// Sort direction: `true` = descending.
    #[serde(default)]
    pub sort_desc: bool,

    // ── Limit ───────────────────────────────────────────────────────
    /// Maximum results to return (`None` = unlimited).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,

    // ── Filter mode ────────────────────────────────────────────────
    /// Filter mode: `"all"` (default), `"files"`, `"dirs"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// Canonical filter mode. Preferred over legacy `filter`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter_mode: Option<SearchFilterMode>,
    /// Canonical predicates. Preferred over legacy filter fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub predicates: Vec<SearchPredicate>,
    /// Specific drives to search (empty = all loaded).
    #[serde(default)]
    pub drives: Vec<char>,
    /// Requested projection fields in canonical order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projection: Vec<String>,
    /// Requested response shaping mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_mode: Option<SearchResponseMode>,

    // ── Size filters ───────────────────────────────────────────────
    /// Minimum file size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_size: Option<u64>,
    /// Maximum file size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_size: Option<u64>,

    // ── Descendant filters ─────────────────────────────────────────
    /// Minimum descendant count (inclusive).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_descendants: Option<u32>,
    /// Maximum descendant count (inclusive).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_descendants: Option<u32>,

    // ── Time filters ───────────────────────────────────────────────
    /// Modified-time lower bound (e.g. `"7d"`, `"24h"`, `"2026-01-15"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newer: Option<String>,
    /// Modified-time upper bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub older: Option<String>,
    /// Created-time lower bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newer_created: Option<String>,
    /// Created-time upper bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub older_created: Option<String>,
    /// Accessed-time lower bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newer_accessed: Option<String>,
    /// Accessed-time upper bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub older_accessed: Option<String>,

    // ── Attribute filter ───────────────────────────────────────────
    /// Attribute filter spec (e.g. `"hidden,compressed,!system"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attr: Option<String>,

    // ── Extension filter ───────────────────────────────────────────
    /// Comma-separated extension filter (e.g. `"rs,toml,md"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<String>,

    // ── Exclude ────────────────────────────────────────────────────
    /// Exclude glob pattern (e.g. `"backup*"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude: Option<String>,
    /// Directory-path pattern (glob). Only matches against the directory
    /// portion of the path, not the filename.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_contains: Option<String>,
    /// File type/category filter (e.g. `"code"`, `"document"`, `"picture"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_filter: Option<String>,
    /// Minimum bulkiness percentage (100 = perfectly packed, >100 = wasteful).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_bulkiness: Option<u64>,
    /// Maximum bulkiness percentage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bulkiness: Option<u64>,

    // ── Length filters ─────────────────────────────────────────────
    /// Minimum filename length in characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_name_len: Option<u16>,
    /// Maximum filename length in characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_name_len: Option<u16>,
    /// Minimum full-path length in characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_path_len: Option<u16>,
    /// Maximum full-path length in characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_path_len: Option<u16>,

    // ── Size-on-disk filters ──────────────────────────────────────
    /// Minimum allocated (on-disk) size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_allocated: Option<u64>,
    /// Maximum allocated (on-disk) size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_allocated: Option<u64>,

    // ── Tree metric filters ────────────────────────────────────────
    /// Minimum subtree logical size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_treesize: Option<u64>,
    /// Maximum subtree logical size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_treesize: Option<u64>,
    /// Minimum subtree allocated (on-disk) size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_tree_allocated: Option<u64>,
    /// Maximum subtree allocated (on-disk) size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tree_allocated: Option<u64>,

    // ── Month-of-year filter ──────────────────────────────────────
    /// Allowed month numbers (1-12).  Empty = no month filter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_months: Vec<u32>,

    // ── Misc ───────────────────────────────────────────────────────
    /// Hide system meta-files (names starting with `$`).
    #[serde(default)]
    pub hide_system: bool,
    /// Hide NTFS Alternate Data Streams from results.
    #[serde(default)]
    pub hide_ads: bool,

    // ── Profiling ──────────────────────────────────────────────────
    /// Request detailed timing breakdown from the daemon.
    #[serde(default)]
    pub profile: bool,

    // ── Aggregation ────────────────────────────────────────────────
    /// Aggregation specs to compute alongside or instead of rows.
    ///
    /// When non-empty, the daemon runs the aggregation engine in addition
    /// to (or instead of) returning rows, depending on `include_rows`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aggregations: Vec<AggregateSpecWire>,
    /// Whether to include result rows in the response.
    ///
    /// Defaults to `true`. Set to `false` for aggregate-only queries
    /// (equivalent to `--count` or `--aggregate` without `--rows`).
    #[serde(default = "default_true")]
    pub include_rows: bool,
}

/// Default-true helper for serde.
const fn default_true() -> bool {
    true
}

impl Default for SearchParams {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            case_sensitive: false,
            whole_word: false,
            match_path: false,
            sort: None,
            sorts: vec![],
            sort_desc: false,
            limit: None,
            filter: None,
            filter_mode: None,
            predicates: vec![],
            drives: vec![],
            projection: vec![],
            response_mode: None,
            min_size: None,
            max_size: None,
            min_descendants: None,
            max_descendants: None,
            newer: None,
            older: None,
            newer_created: None,
            older_created: None,
            newer_accessed: None,
            older_accessed: None,
            attr: None,
            ext: None,
            exclude: None,
            path_contains: None,
            type_filter: None,
            min_bulkiness: None,
            max_bulkiness: None,
            min_name_len: None,
            max_name_len: None,
            min_path_len: None,
            max_path_len: None,
            min_allocated: None,
            max_allocated: None,
            min_treesize: None,
            max_treesize: None,
            min_tree_allocated: None,
            max_tree_allocated: None,
            allowed_months: vec![],
            hide_system: false,
            hide_ads: false,
            profile: false,
            aggregations: vec![],
            include_rows: true,
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Aggregation wire types
// ────────────────────────────────────────────────────────────────────────────

/// Wire format for a single aggregation specification.
///
/// This is the JSON-serializable form of
/// `uffs_core::aggregate::spec::AggregateSpec`. It uses tagged-enum style for
/// `kind` to make JSON schemas self-describing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateSpecWire {
    /// The aggregation kind (e.g. `"count"`, `"terms"`, `"stats"`).
    pub kind: String,
    /// Optional label for this aggregation in the output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Field to aggregate on (required for most kinds except `"count"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Maximum groups for terms aggregation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top: Option<u16>,
    /// Bucket interval for histogram aggregation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<u64>,
    /// Calendar interval for date histogram (e.g. `"month"`, `"day"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calendar: Option<String>,
    /// Range boundaries for range aggregation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boundaries: Vec<u64>,
    /// Metrics to compute per bucket/group.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<String>,
    /// Preset name (when `kind` is `"preset"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
}

/// Wire format for an aggregate result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateResultWire {
    /// Label for this result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The result kind (mirrors the spec kind).
    pub kind: String,
    /// Field name (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Scalar value (for count/missing/distinct).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<u64>,
    /// Scalar statistics (for stats kind).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<StatsWire>,
    /// Bucket rows (for `terms`/`histogram`/`date_histogram`/`range`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buckets: Vec<BucketWire>,
    /// Count of records beyond top-N (for terms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other_count: Option<u64>,
    /// Total groups before truncation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_groups: Option<usize>,
}

/// Wire format for scalar statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsWire {
    /// Record count.
    pub count: u64,
    /// Sum of values.
    pub sum: u64,
    /// Minimum value.
    pub min: u64,
    /// Maximum value.
    pub max: u64,
    /// Average value.
    pub avg: f64,
    /// Waste bytes.
    pub waste_bytes: u64,
    /// Waste percentage.
    pub waste_pct: f64,
}

/// Wire format for a single bucket row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BucketWire {
    /// Bucket key (display string).
    pub key: String,
    /// Record count in this bucket.
    pub count: u64,
    /// Total bytes in this bucket.
    pub total_bytes: u64,
    /// Total allocated bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_allocated: Option<u64>,
    /// Average file size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_size: Option<f64>,
    /// Share of total count (percentage).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_count: Option<f64>,
    /// Share of total bytes (percentage).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_bytes: Option<f64>,
}
