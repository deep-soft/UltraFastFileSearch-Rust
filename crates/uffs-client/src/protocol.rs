//! JSON-RPC 2.0 protocol types shared between client and daemon.
//!
//! These types define the wire format for IPC communication. Both
//! `uffs-daemon` and `uffs-client` depend on this module.

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
    /// Projected CSV text using daemon defaults.
    Csv,
    /// Projected table text using daemon defaults.
    Table,
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
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
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

    // ── Misc ───────────────────────────────────────────────────────
    /// Hide system meta-files (names starting with `$`).
    #[serde(default)]
    pub hide_system: bool,

    // ── Profiling ──────────────────────────────────────────────────
    /// Request detailed timing breakdown from the daemon.
    #[serde(default)]
    pub profile: bool,
}

impl SearchParams {
    /// Resolve the effective filter mode, preferring the canonical field.
    #[must_use]
    pub fn resolved_filter_mode(&self) -> SearchFilterMode {
        self.filter_mode.unwrap_or(match self.filter.as_deref() {
            Some("files") => SearchFilterMode::Files,
            Some("dirs") => SearchFilterMode::Dirs,
            _ => SearchFilterMode::All,
        })
    }

    /// Resolve the effective sort clauses, preferring the canonical vector.
    #[must_use]
    pub fn resolved_sorts(&self) -> Vec<SearchSortSpec> {
        if self.sorts.is_empty() {
            self.sort.as_deref().map_or_else(Vec::new, |sort| {
                Self::canonicalize_legacy_sort(sort, self.sort_desc)
            })
        } else {
            self.sorts.clone()
        }
    }

    /// Resolve the effective canonical predicate list.
    #[must_use]
    pub fn resolved_predicates(&self) -> Vec<SearchPredicate> {
        if !self.predicates.is_empty() {
            return self.predicates.clone();
        }

        let mut predicates = Vec::new();
        self.push_bound_predicates(&mut predicates);
        self.push_legacy_time_predicates(&mut predicates);
        self.push_extension_and_exclude(&mut predicates);
        self.push_attr_predicates(&mut predicates);

        if self.hide_system {
            predicates.push(SearchPredicate {
                field: "system_name".to_owned(),
                op: SearchPredicateOp::Eq,
                value: SearchPredicateValue::Bool(false),
            });
        }

        predicates
    }

    /// Push size and descendant bound predicates from legacy fields.
    fn push_bound_predicates(&self, predicates: &mut Vec<SearchPredicate>) {
        if let Some(min_size) = self.min_size {
            predicates.push(SearchPredicate {
                field: "size".to_owned(),
                op: SearchPredicateOp::Gte,
                value: SearchPredicateValue::U64(min_size),
            });
        }
        if let Some(max_size) = self.max_size {
            predicates.push(SearchPredicate {
                field: "size".to_owned(),
                op: SearchPredicateOp::Lte,
                value: SearchPredicateValue::U64(max_size),
            });
        }
        if let Some(min_descendants) = self.min_descendants {
            predicates.push(SearchPredicate {
                field: "descendants".to_owned(),
                op: SearchPredicateOp::Gte,
                value: SearchPredicateValue::U64(u64::from(min_descendants)),
            });
        }
        if let Some(max_descendants) = self.max_descendants {
            predicates.push(SearchPredicate {
                field: "descendants".to_owned(),
                op: SearchPredicateOp::Lte,
                value: SearchPredicateValue::U64(u64::from(max_descendants)),
            });
        }
    }

    /// Push all six legacy time-bound predicates (newer/older ×
    /// modified/created/accessed).
    fn push_legacy_time_predicates(&self, predicates: &mut Vec<SearchPredicate>) {
        for (field, op, spec) in [
            ("modified", SearchPredicateOp::Gte, self.newer.as_deref()),
            ("modified", SearchPredicateOp::Lt, self.older.as_deref()),
            (
                "created",
                SearchPredicateOp::Gte,
                self.newer_created.as_deref(),
            ),
            (
                "created",
                SearchPredicateOp::Lt,
                self.older_created.as_deref(),
            ),
            (
                "accessed",
                SearchPredicateOp::Gte,
                self.newer_accessed.as_deref(),
            ),
            (
                "accessed",
                SearchPredicateOp::Lt,
                self.older_accessed.as_deref(),
            ),
        ] {
            if let Some(val) = spec {
                predicates.push(SearchPredicate {
                    field: field.to_owned(),
                    op,
                    value: SearchPredicateValue::String(val.to_owned()),
                });
            }
        }
    }

    /// Push extension filter and exclude predicates from legacy fields.
    fn push_extension_and_exclude(&self, predicates: &mut Vec<SearchPredicate>) {
        if let Some(ext) = self.ext.as_deref() {
            let values = ext
                .split(',')
                .map(|segment| segment.trim().trim_start_matches('.').to_owned())
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>();
            if !values.is_empty() {
                predicates.push(SearchPredicate {
                    field: "extension".to_owned(),
                    op: SearchPredicateOp::In,
                    value: SearchPredicateValue::StringList(values),
                });
            }
        }

        if let Some(exclude) = self.exclude.as_ref() {
            predicates.push(SearchPredicate {
                field: "name".to_owned(),
                op: SearchPredicateOp::NotMatch,
                value: SearchPredicateValue::String(exclude.clone()),
            });
        }
    }

    /// Push attribute require/exclude predicates from legacy `--attr` flag.
    fn push_attr_predicates(&self, predicates: &mut Vec<SearchPredicate>) {
        let mut required = Vec::new();
        let mut excluded = Vec::new();
        if let Some(attr) = self.attr.as_deref() {
            for part in attr.split(',') {
                let trimmed = part.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Some(name) = trimmed.strip_prefix('!') {
                    excluded.push(name.to_ascii_lowercase());
                } else {
                    required.push(trimmed.to_ascii_lowercase());
                }
            }
        }
        if !required.is_empty() {
            predicates.push(SearchPredicate {
                field: "attributes".to_owned(),
                op: SearchPredicateOp::HasAll,
                value: SearchPredicateValue::StringList(required),
            });
        }
        if !excluded.is_empty() {
            predicates.push(SearchPredicate {
                field: "attributes".to_owned(),
                op: SearchPredicateOp::HasNone,
                value: SearchPredicateValue::StringList(excluded),
            });
        }
    }

    /// Resolve the requested response mode.
    #[must_use]
    pub fn resolved_response_mode(&self) -> SearchResponseMode {
        self.response_mode.unwrap_or(SearchResponseMode::Rows)
    }

    /// Fill additive canonical fields from legacy fields in one shared place.
    pub fn populate_canonical_fields(&mut self) {
        if self.filter_mode.is_none() {
            self.filter_mode = Some(self.resolved_filter_mode());
        }
        if self.sorts.is_empty() {
            self.sorts = self.resolved_sorts();
        }
        if self.predicates.is_empty() {
            self.predicates = self.resolved_predicates();
        }
        if self.response_mode.is_none() {
            self.response_mode = Some(self.resolved_response_mode());
        }
    }

    /// Canonicalize a legacy comma-separated sort string plus `sort_desc` flag.
    ///
    /// Supports three direction syntaxes:
    /// - Prefix: `-size` means descending, bare `size` means ascending
    /// - Suffix: `size:desc` or `size:asc` (explicit)
    /// - Flag:   `--sort-desc` flips the first field to descending
    ///
    /// **First field:** ascending by default; descending if prefixed with `-`
    /// or if `sort_desc` is true.
    ///
    /// **Secondary fields:** use field-type defaults (numeric/time → desc,
    /// string → asc) unless overridden with prefix or suffix.
    #[must_use]
    pub fn canonicalize_legacy_sort(sort: &str, sort_desc: bool) -> Vec<SearchSortSpec> {
        sort.split(',')
            .enumerate()
            .filter_map(|(index, raw_part)| {
                let trimmed = raw_part.trim();
                if trimmed.is_empty() {
                    return None;
                }

                // Check for `-` prefix (e.g. "-modified" → descending).
                let (has_dash_prefix, after_dash) = trimmed
                    .strip_prefix('-')
                    .map_or((false, trimmed), |rest| (true, rest));

                let (field, explicit_direction) = after_dash
                    .split_once(':')
                    .map_or((after_dash, None), |(lhs, rhs)| {
                        (lhs.trim(), Some(rhs.trim()))
                    });

                // Parse explicit suffix direction token (e.g. "size:desc").
                let parsed_dir = explicit_direction.and_then(|dir| {
                    match dir.trim().to_ascii_lowercase().as_str() {
                        "asc" | "ascending" => Some(SearchSortDirection::Asc),
                        "desc" | "descending" => Some(SearchSortDirection::Desc),
                        _ => None,
                    }
                });

                // Resolve direction: suffix > prefix > flag (first field) > default.
                let direction = parsed_dir.or_else(|| {
                    if has_dash_prefix {
                        return Some(SearchSortDirection::Desc);
                    }
                    if index == 0 {
                        // First field: ascending unless --sort-desc is set.
                        return Some(if sort_desc {
                            SearchSortDirection::Desc
                        } else {
                            SearchSortDirection::Asc
                        });
                    }
                    // Secondary fields: field-type default.
                    Some(match field.trim().to_ascii_lowercase().as_str() {
                        "size" | "sizeondisk" | "size_on_disk" | "allocated" | "created"
                        | "modified" | "written" | "date" | "accessed" | "descendants"
                        | "treesize" | "tree_size" | "treeallocated" | "tree_allocated" => {
                            SearchSortDirection::Desc
                        }
                        _ => SearchSortDirection::Asc,
                    })
                });

                Some(SearchSortSpec {
                    field: field.to_owned(),
                    direction,
                })
            })
            .collect()
    }
}

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
    /// Rendered text for CSV/table direct daemon callers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projected_text: Option<String>,
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
    clippy::cast_precision_loss,
    reason = "precision loss acceptable for display"
)]
#[expect(
    clippy::float_arithmetic,
    reason = "floating-point division is intentional for human-readable size formatting"
)]
pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// Format a Unix-microsecond timestamp as `YYYY-MM-DD HH:MM`.
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "intermediate values are bounded by calendar math — truncation is safe"
)]
#[expect(
    clippy::cast_lossless,
    reason = "explicit casts are clearer than From for calendar arithmetic"
)]
pub fn format_time(unix_micros: i64) -> String {
    if unix_micros == 0 {
        return "—".to_owned();
    }
    let secs = unix_micros / 1_000_000;
    // Simple UTC conversion (good enough for display)
    let days_since_epoch = secs / 86400;
    let day_secs = (secs % 86400) as u32;
    let hour = day_secs / 3600;
    let minute = (day_secs % 3600) / 60;

    // Approximate date (Howard Hinnant algorithm, simplified)
    let civil_z = days_since_epoch + 719_468;
    let era = if civil_z >= 0 {
        civil_z
    } else {
        civil_z - 146_096
    } / 146_097;
    let doe = (civil_z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let base_year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let month_proxy = (5 * doy + 2) / 153;
    let day = doy - (153 * month_proxy + 2) / 5 + 1;
    let month = if month_proxy < 10 {
        month_proxy + 3
    } else {
        month_proxy - 9
    };
    let year = if month <= 2 { base_year + 1 } else { base_year };

    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    /// D2.2.5: serialize/deserialize round-trip for request.
    #[test]
    fn request_round_trip() {
        let req = RpcRequest::new(1, "search", Some(serde_json::json!({"pattern": "*.rs"})));
        let json = serde_json::to_string(&req).expect("serialize");
        let parsed: RpcRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.method, "search");
        assert_eq!(parsed.id, Some(1));
    }

    /// D2.2.5: serialize/deserialize round-trip for response.
    #[test]
    fn response_round_trip() {
        let resp = RpcResponse::success(
            42,
            serde_json::json!({"rows": [], "records_scanned": 0_u64}),
        );
        let json = serde_json::to_string(&resp).expect("serialize");
        let parsed: RpcResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.id, 42);
    }

    /// D2.2.5: serialize/deserialize round-trip for error.
    #[test]
    fn error_round_trip() {
        let err = RpcErrorResponse::error(Some(1), ERR_METHOD_NOT_FOUND, "Method not found");
        let json = serde_json::to_string(&err).expect("serialize");
        let parsed: RpcErrorResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.error.code, ERR_METHOD_NOT_FOUND);
    }

    /// D2.2.5: `SearchParams` serialize/deserialize.
    #[test]
    fn search_params_round_trip() {
        let params = SearchParams {
            pattern: "*.rs".to_owned(),
            case_sensitive: true,
            limit: Some(100),
            sorts: vec![SearchSortSpec {
                field: "size".to_owned(),
                direction: Some(SearchSortDirection::Desc),
            }],
            filter_mode: Some(SearchFilterMode::Files),
            projection: vec!["path".to_owned(), "size".to_owned()],
            response_mode: Some(SearchResponseMode::Json),
            ..Default::default()
        };
        let json = serde_json::to_value(&params).expect("serialize");
        let parsed: SearchParams = serde_json::from_value(json).expect("deserialize");
        assert_eq!(parsed.pattern, "*.rs");
        assert!(parsed.case_sensitive);
        assert_eq!(parsed.limit, Some(100));
        assert_eq!(parsed.sorts.len(), 1);
        assert_eq!(parsed.filter_mode, Some(SearchFilterMode::Files));
        assert_eq!(parsed.response_mode, Some(SearchResponseMode::Json));
    }

    /// Canonical helpers preserve legacy single-flag sort semantics.
    ///
    /// First field: ascending by default (no `--sort-desc`).
    /// Secondary fields: field-type defaults (numeric → desc, string → asc).
    /// `--sort-desc` flag flips the first field to descending.
    /// `-` prefix forces descending on any individual field.
    #[test]
    fn canonicalize_legacy_sort_preserves_primary_sort_desc_override() {
        // --sort size,name (no --sort-desc) → first=asc, second=field default
        let specs = SearchParams::canonicalize_legacy_sort("size,name", false);
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].field, "size");
        assert_eq!(
            specs[0].direction,
            Some(SearchSortDirection::Asc),
            "first field defaults to asc without --sort-desc"
        );
        assert_eq!(specs[1].field, "name");
        assert_eq!(
            specs[1].direction,
            Some(SearchSortDirection::Asc),
            "name (string) defaults to asc"
        );

        // --sort name --sort-desc → first field flipped to desc
        let desc_specs = SearchParams::canonicalize_legacy_sort("name", true);
        assert_eq!(desc_specs[0].direction, Some(SearchSortDirection::Desc));
    }

    /// `-` prefix forces descending on individual sort fields.
    #[test]
    fn canonicalize_legacy_sort_dash_prefix_descending() {
        // -modified,name → modified=desc, name=asc(default)
        let specs = SearchParams::canonicalize_legacy_sort("-modified,name", false);
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].field, "modified");
        assert_eq!(
            specs[0].direction,
            Some(SearchSortDirection::Desc),
            "dash prefix forces descending"
        );
        assert_eq!(specs[1].field, "name");
        assert_eq!(
            specs[1].direction,
            Some(SearchSortDirection::Asc),
            "name defaults to asc"
        );

        // -size alone
        let single = SearchParams::canonicalize_legacy_sort("-size", false);
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].field, "size");
        assert_eq!(single[0].direction, Some(SearchSortDirection::Desc));
    }

    /// Secondary numeric fields use field-type defaults (desc for
    /// size/time/descendants).
    #[test]
    fn canonicalize_legacy_sort_secondary_field_defaults() {
        let specs = SearchParams::canonicalize_legacy_sort("name,size,modified", false);
        assert_eq!(specs.len(), 3);
        assert_eq!(
            specs[0].direction,
            Some(SearchSortDirection::Asc),
            "first field = asc"
        );
        assert_eq!(
            specs[1].direction,
            Some(SearchSortDirection::Desc),
            "secondary size defaults to desc"
        );
        assert_eq!(
            specs[2].direction,
            Some(SearchSortDirection::Desc),
            "secondary modified defaults to desc"
        );
    }

    /// Canonical helpers prefer the new filter field over the legacy one.
    #[test]
    fn resolved_filter_mode_prefers_canonical_field() {
        let params = SearchParams {
            filter: Some("dirs".to_owned()),
            filter_mode: Some(SearchFilterMode::Files),
            ..Default::default()
        };

        assert_eq!(params.resolved_filter_mode(), SearchFilterMode::Files);
    }

    /// D2.2.5: `DaemonStatus` serialize/deserialize.
    #[test]
    fn daemon_status_round_trip() {
        let loading = DaemonStatus::Loading {
            drives_loaded: 3,
            drives_total: 7,
        };
        let json = serde_json::to_string(&loading).expect("serialize");
        let parsed: DaemonStatus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, loading);

        let ready = DaemonStatus::Ready;
        let ready_json = serde_json::to_string(&ready).expect("serialize");
        let ready_parsed: DaemonStatus = serde_json::from_str(&ready_json).expect("deserialize");
        assert_eq!(ready_parsed, ready);
    }

    /// D2.2.5: `SearchResponse` with rows.
    #[test]
    fn search_response_round_trip() {
        let resp = SearchResponse {
            rows: vec![SearchRow {
                drive: 'C',
                path: "C:\\test.rs".to_owned(),
                name: "test.rs".to_owned(),
                size: 1024,
                is_directory: false,
                modified: 1_700_000_000_000_000,
                created: 1_700_000_000_000_000,
                accessed: 1_700_000_000_000_000,
                flags: 0x20,
                allocated: 4096,
                descendants: 0,
                treesize: 0,
                tree_allocated: 0,
            }],
            records_scanned: 1_000_000,
            duration_ms: 8,
            truncated: false,
            shmem_path: None,
            shmem_count: None,
            profile: None,
            applied_sorts: vec![SearchSortSpec {
                field: "modified".to_owned(),
                direction: Some(SearchSortDirection::Desc),
            }],
            applied_projection: vec!["path".to_owned(), "size".to_owned()],
            response_mode: Some(SearchResponseMode::Rows),
            projected_rows: Some(vec![serde_json::Map::from_iter([
                (
                    "path".to_owned(),
                    serde_json::Value::String("C:\\test.rs".to_owned()),
                ),
                ("size".to_owned(), serde_json::Value::from(1024_u64)),
            ])]),
            projected_text: None,
        };
        let json = serde_json::to_string(&resp).expect("serialize");
        let parsed: SearchResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.rows.len(), 1);
        let first_row = parsed.rows.first().expect("at least one row");
        assert_eq!(first_row.name, "test.rs");
        assert_eq!(parsed.duration_ms, 8);
        assert_eq!(parsed.applied_sorts.len(), 1);
        assert_eq!(parsed.applied_projection.len(), 2);
        assert!(parsed.projected_rows.is_some());
    }
}
