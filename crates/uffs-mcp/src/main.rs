//! UFFS MCP Adapter — bridges AI agents to the UFFS daemon via the
//! Model Context Protocol (MCP).
//!
//! Reads JSON-RPC from stdin, translates to uffs-client API calls,
//! writes responses to stdout.
//!
//! # MCP Configuration
//!
//! Add to your AI agent's MCP config:
//! ```json
//! { "uffs": { "command": "uffs-mcp" } }
//! ```

use std::io::BufRead;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uffs_client::connect::UffsClient;
use uffs_client::protocol::SearchParams;

// ────────────────────────────────────────────────────────────────────────────
// MCP Protocol Types
// ────────────────────────────────────────────────────────────────────────────

/// MCP JSON-RPC request (subset we handle).
#[derive(Debug, Deserialize)]
struct McpRequest {
    /// JSON-RPC version (deserialized for protocol compliance, not read).
    #[serde(rename = "jsonrpc")]
    _jsonrpc: String,
    /// Request ID.
    id: Option<Value>,
    /// Method name.
    method: String,
    /// Parameters.
    #[serde(default)]
    params: Value,
}

/// MCP JSON-RPC response.
#[derive(Debug, Serialize)]
struct McpResponse {
    /// JSON-RPC version.
    jsonrpc: String,
    /// Matching request ID.
    id: Value,
    /// Result payload.
    result: Value,
}

/// MCP JSON-RPC error response.
#[derive(Debug, Serialize)]
struct McpErrorResponse {
    /// JSON-RPC version.
    jsonrpc: String,
    /// Matching request ID.
    id: Value,
    /// Error details.
    error: McpError,
}

/// MCP error object.
#[derive(Debug, Serialize)]
struct McpError {
    /// Error code.
    code: i32,
    /// Error message.
    message: String,
}

// ────────────────────────────────────────────────────────────────────────────
// MCP Server
// ────────────────────────────────────────────────────────────────────────────

/// MCP server wrapping a daemon client connection.
struct McpServer {
    /// Connected daemon client.
    client: UffsClient,
}

/// Write a JSON response to stdout (MCP protocol channel).
///
/// Called from both `process_request` and `dispatch_tool_call` (multiple
/// sites).
#[expect(clippy::print_stdout, reason = "MCP protocol uses stdout as transport")]
fn write_response<T: Serialize>(value: &T) -> anyhow::Result<()> {
    let json = serde_json::to_string(value)?;
    println!("{json}");
    Ok(())
}

impl McpServer {
    /// Process a single MCP request line: parse, dispatch, write response.
    ///
    /// # Errors
    ///
    /// Returns an error only for fatal I/O failures (stdout broken).
    /// Protocol errors are sent back as JSON-RPC error responses.
    async fn process_request(&mut self, trimmed: &str) -> anyhow::Result<()> {
        let req: McpRequest = match serde_json::from_str(trimmed) {
            Ok(parsed) => parsed,
            Err(parse_err) => {
                let err_resp = McpErrorResponse {
                    jsonrpc: "2.0".to_owned(),
                    id: Value::Null,
                    error: McpError {
                        code: -32700,
                        message: format!("Parse error: {parse_err}"),
                    },
                };
                return write_response(&err_resp);
            }
        };

        let id = req.id.clone().unwrap_or(Value::Null);

        let result: anyhow::Result<Value> = match req.method.as_str() {
            "initialize" => Ok(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {}, "resources": {}, "prompts": {} },
                "serverInfo": { "name": "uffs", "version": env!("CARGO_PKG_VERSION") }
            })),
            "tools/list" => Ok(serde_json::json!({ "tools": [
            { "name": "uffs_search", "description": "Search files across all indexed NTFS drives. Supports glob patterns (*.rs), regex (>pattern), path patterns (\\dir\\*.txt), and substring search.", "inputSchema": { "type": "object", "properties": { "pattern": { "type": "string", "description": "Search pattern" }, "case_sensitive": { "type": "boolean", "default": false }, "sort": { "type": "string", "default": "modified" }, "sort_desc": { "type": "boolean", "default": true }, "limit": { "type": "integer", "default": 100_i64 }, "filter": { "type": "string", "default": "all" } }, "required": ["pattern"] } },
            { "name": "uffs_drives", "description": "List all loaded drives with record counts and source information.", "inputSchema": { "type": "object", "properties": {} } },
            { "name": "uffs_status", "description": "Get daemon status: loading progress, uptime, connections, PID.", "inputSchema": { "type": "object", "properties": {} } },
            { "name": "uffs_info", "description": "Get detailed information about a specific file or directory by its full path.", "inputSchema": { "type": "object", "properties": { "path": { "type": "string", "description": "Full file path" } }, "required": ["path"] } }
        ] })),
            "tools/call" => self.dispatch_tool_call(&req.params).await,
            "resources/list" => Ok(serde_json::json!({
                "resources": [
                    { "uri": "uffs://drives", "name": "Indexed Drives", "description": "List of all NTFS drives indexed by the UFFS daemon with record counts and source info", "mimeType": "application/json" },
                    { "uri": "uffs://status", "name": "Daemon Status", "description": "Current daemon status: loading progress, uptime, memory, connections", "mimeType": "application/json" }
                ]
            })),
            "resources/read" => self.handle_resources_read(&req.params).await,
            "prompts/list" => Ok(serde_json::json!({ "prompts": [
            { "name": "find_large_files", "description": "Find the largest files across all drives, sorted by size descending", "arguments": [{ "name": "limit", "description": "Number of results (default: 50)", "required": false }] },
            { "name": "recent_changes", "description": "Find files modified in the last N days", "arguments": [{ "name": "days", "description": "Number of days to look back (default: 1)", "required": false }] },
            { "name": "find_by_extension", "description": "Find all files with a specific extension", "arguments": [{ "name": "extension", "description": "File extension without dot (e.g., 'rs', 'pdf', 'jpg')", "required": true }, { "name": "limit", "description": "Number of results (default: 100)", "required": false }] },
            { "name": "find_duplicates_by_name", "description": "Search for files with the same name across all drives", "arguments": [{ "name": "filename", "description": "Exact filename to search for", "required": true }] }
        ] })),
            "prompts/get" => Self::handle_prompts_get(&req.params),
            "notifications/initialized" => Ok(Value::Null),
            other => {
                tracing::debug!(method = other, "Unknown MCP method");
                anyhow::bail!("Method not found: {other}")
            }
        };

        match result {
            Ok(value) => write_response(&McpResponse {
                jsonrpc: "2.0".to_owned(),
                id,
                result: value,
            }),
            Err(dispatch_err) => write_response(&McpErrorResponse {
                jsonrpc: "2.0".to_owned(),
                id,
                error: McpError {
                    code: -32603,
                    message: dispatch_err.to_string(),
                },
            }),
        }
    }

    /// Dispatch a `tools/call` request to the matching tool handler.
    async fn dispatch_tool_call(&mut self, params: &Value) -> anyhow::Result<Value> {
        let tool_name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));

        let text: anyhow::Result<String> = match tool_name {
            "uffs_search" => self.tool_search(&arguments).await,
            "uffs_drives" => self.tool_drives().await,
            "uffs_status" => self.tool_status().await,
            "uffs_info" => self.tool_info(&arguments).await,
            other => anyhow::bail!("Unknown tool: {other}"),
        };

        text.map(|txt| serde_json::json!({"content": [{"type": "text", "text": txt}]}))
    }

    // ── Extracted helpers ────────────────────────────────────────────

    /// Handle `resources/read` requests.
    async fn handle_resources_read(&mut self, params: &Value) -> anyhow::Result<Value> {
        let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
        let (content, mime) =
            match uri {
                "uffs://drives" => {
                    let drives_resp = self.client.drives().await.map_err(|drives_err| {
                        anyhow::anyhow!("Failed to read drives: {drives_err}")
                    })?;
                    (
                        serde_json::to_string_pretty(&drives_resp)?,
                        "application/json",
                    )
                }
                "uffs://status" => {
                    let status_resp = self.client.status().await.map_err(|status_err| {
                        anyhow::anyhow!("Failed to read status: {status_err}")
                    })?;
                    (
                        serde_json::to_string_pretty(&status_resp)?,
                        "application/json",
                    )
                }
                _ => anyhow::bail!("Unknown resource: {uri}"),
            };
        Ok(serde_json::json!({"contents": [{"uri": uri, "mimeType": mime, "text": content}]}))
    }

    /// Handle `prompts/get` requests.
    #[expect(
        clippy::single_call_fn,
        reason = "extracted from process_request for line-count compliance"
    )]
    fn handle_prompts_get(params: &Value) -> anyhow::Result<Value> {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let prompt_args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        let messages = match name {
            "find_large_files" => {
                let limit = prompt_args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(50_u64);
                vec![
                    serde_json::json!({"role": "user", "content": {"type": "text", "text": format!("Use the uffs_search tool to find the {limit} largest files. Use pattern '*', sort by 'size' descending, limit {limit}, filter 'files'. Show results as a table with name, size, and path.")}}),
                ]
            }
            "recent_changes" => {
                let days = prompt_args
                    .get("days")
                    .and_then(Value::as_u64)
                    .unwrap_or(1_u64);
                vec![
                    serde_json::json!({"role": "user", "content": {"type": "text", "text": format!("Use the uffs_search tool to find files modified in the last {days} day(s). Use pattern '*', sort by 'modified' descending, limit 100. Show results as a table.")}}),
                ]
            }
            "find_by_extension" => {
                let ext = prompt_args
                    .get("extension")
                    .and_then(Value::as_str)
                    .unwrap_or("txt");
                let limit = prompt_args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(100_u64);
                vec![
                    serde_json::json!({"role": "user", "content": {"type": "text", "text": format!("Use the uffs_search tool to find all *.{ext} files. Use pattern '*.{ext}', sort by 'modified' descending, limit {limit}. Show results as a table.")}}),
                ]
            }
            "find_duplicates_by_name" => {
                let filename = prompt_args
                    .get("filename")
                    .and_then(Value::as_str)
                    .unwrap_or("*");
                vec![
                    serde_json::json!({"role": "user", "content": {"type": "text", "text": format!("Use the uffs_search tool to find all files named '{filename}' across all drives. This helps identify duplicate files. Show the full path for each result.")}}),
                ]
            }
            _ => anyhow::bail!("Unknown prompt: {name}"),
        };
        Ok(
            serde_json::json!({"description": format!("UFFS search prompt: {name}"), "messages": messages}),
        )
    }

    /// Tool handler: `uffs_search`.
    async fn tool_search(&mut self, arguments: &Value) -> anyhow::Result<String> {
        use core::fmt::Write;

        let pattern = arguments
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or("*")
            .to_owned();
        let search_params = SearchParams {
            pattern,
            case_sensitive: arguments
                .get("case_sensitive")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            whole_word: false,
            sort: arguments
                .get("sort")
                .and_then(Value::as_str)
                .map(String::from),
            sort_desc: arguments
                .get("sort_desc")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            limit: arguments
                .get("limit")
                .and_then(Value::as_u64)
                .and_then(|val| u32::try_from(val.min(10_000)).ok()),
            filter: arguments
                .get("filter")
                .and_then(Value::as_str)
                .map(String::from),
            drives: Vec::new(),
        };
        let response = self
            .client
            .search(&search_params)
            .await
            .map_err(|search_err| anyhow::anyhow!("Search failed: {search_err}"))?;
        let mut output = String::new();
        write!(
            output,
            "Found {} results ({} records scanned in {}ms)\n\n",
            response.rows.len(),
            response.records_scanned,
            response.duration_ms
        )
        .ok();
        if response.rows.is_empty() {
            output.push_str("No matches found.\n");
        } else {
            output
                .push_str("| Name | Size | Modified | Path |\n|------|------|----------|------|\n");
            for row in &response.rows {
                writeln!(
                    output,
                    "| {} | {} | {} | {} |",
                    row.name,
                    uffs_client::protocol::format_size(row.size),
                    uffs_client::protocol::format_time(row.modified),
                    row.path
                )
                .ok();
            }
            if response.truncated {
                output.push_str("\n(Results truncated. Use 'limit' parameter to see more.)\n");
            }
        }
        Ok(output)
    }

    /// Tool handler: `uffs_drives`.
    async fn tool_drives(&mut self) -> anyhow::Result<String> {
        use core::fmt::Write;

        let response = self
            .client
            .drives()
            .await
            .map_err(|drives_err| anyhow::anyhow!("Failed to list drives: {drives_err}"))?;
        let mut output = String::new();
        write!(output, "Loaded {} drive(s):\n\n", response.drives.len()).ok();
        for drive in &response.drives {
            writeln!(
                output,
                "  {}:  {:>10} records  ({})",
                drive.letter, drive.records, drive.source
            )
            .ok();
        }
        Ok(output)
    }

    /// Tool handler: `uffs_status`.
    async fn tool_status(&mut self) -> anyhow::Result<String> {
        let response = self
            .client
            .status()
            .await
            .map_err(|status_err| anyhow::anyhow!("Failed to get status: {status_err}"))?;
        let status_str = serde_json::to_string_pretty(&response.status)?;
        Ok(format!(
            "Daemon Status: {status_str}\nUptime: {}s\nConnections: {}\nPID: {}\n",
            response.uptime_secs, response.connections, response.pid
        ))
    }

    /// Tool handler: `uffs_info`.
    async fn tool_info(&mut self, arguments: &Value) -> anyhow::Result<String> {
        let path = arguments.get("path").and_then(Value::as_str).unwrap_or("");
        if path.is_empty() {
            return Ok("Error: 'path' parameter is required.".to_owned());
        }
        let response = self
            .client
            .info(path)
            .await
            .map_err(|info_err| anyhow::anyhow!("Failed to get info: {info_err}"))?;
        if response.found {
            match response.record {
                Some(record) => Ok(serde_json::to_string_pretty(&record)?),
                None => Ok(format!("File found but no details available: {path}")),
            }
        } else {
            Ok(format!("File not found: {path}"))
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Entry Point
// ────────────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // MCP uses stderr for logging (stdout is the protocol channel)
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .init();

    tracing::info!("uffs-mcp starting");

    let client = match UffsClient::connect().await {
        Ok(connected) => connected,
        Err(conn_err) => {
            tracing::error!(error = %conn_err, "Failed to connect to daemon");
            anyhow::bail!("Cannot connect to uffs-daemon: {conn_err}");
        }
    };

    tracing::info!("Connected to uffs-daemon");

    let mut server = McpServer { client };

    let stdin = std::io::stdin();
    for raw_line in stdin.lock().lines() {
        let text = match raw_line {
            Ok(ok_line) => ok_line,
            Err(read_err) => {
                tracing::error!(error = %read_err, "stdin read error");
                break;
            }
        };

        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Err(io_err) = server.process_request(trimmed).await {
            tracing::error!(error = %io_err, "Fatal I/O error");
            break;
        }
    }

    tracing::info!("uffs-mcp shutting down");
    Ok(())
}
