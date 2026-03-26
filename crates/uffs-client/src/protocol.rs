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

// Application error codes (daemon-specific)
/// Daemon is still loading indices.
pub const ERR_NOT_READY: i32 = -1;
/// Search pattern compilation failed (bad regex).
pub const ERR_BAD_PATTERN: i32 = -2;

// ────────────────────────────────────────────────────────────────────────────
// Method parameters
// ────────────────────────────────────────────────────────────────────────────

/// Parameters for the `search` method.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct SearchParams {
    /// Search pattern (glob, regex with `>` prefix, or substring).
    pub pattern: String,
    /// Case-sensitive matching.
    #[serde(default)]
    pub case_sensitive: bool,
    /// Whole-word matching.
    #[serde(default)]
    pub whole_word: bool,
    /// Sort column name (e.g. `"modified"`, `"size"`, `"name"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
    /// Sort direction: `true` = descending.
    #[serde(default)]
    pub sort_desc: bool,
    /// Maximum results to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Filter mode: `"all"` (default), `"files"`, `"dirs"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// Specific drives to search (empty = all loaded).
    #[serde(default)]
    pub drives: Vec<char>,
}

/// Parameters for the `refresh` method.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct RefreshParams {
    /// Specific drives to refresh (empty = all loaded).
    #[serde(default)]
    pub drives: Vec<char>,
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
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResponse {
    /// Matching result rows.
    pub rows: Vec<SearchRow>,
    /// Total records scanned.
    pub records_scanned: usize,
    /// Search duration in milliseconds.
    pub duration_ms: u64,
    /// Whether results were truncated by limit.
    pub truncated: bool,
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
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
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
        let resp = RpcResponse::success(42, serde_json::json!({"rows": [], "records_scanned": 0}));
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

    /// D2.2.5: SearchParams serialize/deserialize.
    #[test]
    fn search_params_round_trip() {
        let params = SearchParams {
            pattern: "*.rs".to_owned(),
            case_sensitive: true,
            limit: Some(100),
            ..Default::default()
        };
        let json = serde_json::to_value(&params).expect("serialize");
        let parsed: SearchParams = serde_json::from_value(json).expect("deserialize");
        assert_eq!(parsed.pattern, "*.rs");
        assert!(parsed.case_sensitive);
        assert_eq!(parsed.limit, Some(100));
    }

    /// D2.2.5: DaemonStatus serialize/deserialize.
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
        let json = serde_json::to_string(&ready).expect("serialize");
        let parsed: DaemonStatus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, ready);
    }

    /// D2.2.5: SearchResponse with rows.
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
            }],
            records_scanned: 1_000_000,
            duration_ms: 8,
            truncated: false,
        };
        let json = serde_json::to_string(&resp).expect("serialize");
        let parsed: SearchResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.rows.len(), 1);
        assert_eq!(parsed.rows[0].name, "test.rs");
        assert_eq!(parsed.duration_ms, 8);
    }
}
