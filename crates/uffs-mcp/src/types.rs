//! MCP protocol types.

use serde::{Deserialize, Serialize};

// ────────────────────────────────────────────────────────────────────────────
// MCP (JSON-RPC 2.0) types
// ────────────────────────────────────────────────────────────────────────────

/// MCP JSON-RPC 2.0 request.
#[derive(Debug, Deserialize)]
pub struct McpRequest {
    /// JSON-RPC version (must be `"2.0"`).
    #[allow(dead_code)]
    pub jsonrpc: String,
    /// Request ID for correlation.
    pub id: serde_json::Value,
    /// Method name.
    pub method: String,
    /// Method parameters.
    pub params: serde_json::Value,
}

/// MCP JSON-RPC 2.0 success response.
#[derive(Debug, Serialize)]
pub struct McpResponse {
    /// JSON-RPC version.
    pub jsonrpc: String,
    /// Matching request ID.
    pub id: serde_json::Value,
    /// Result payload.
    pub result: serde_json::Value,
}

/// MCP JSON-RPC 2.0 error response.
#[derive(Debug, Serialize)]
pub struct McpErrorResponse {
    /// JSON-RPC version.
    pub jsonrpc: String,
    /// Matching request ID.
    pub id: serde_json::Value,
    /// Error details.
    pub error: McpError,
}

/// MCP JSON-RPC 2.0 error object.
#[derive(Debug, Serialize)]
pub struct McpError {
    /// Error code.
    pub code: i32,
    /// Human-readable error message.
    pub message: String,
}
