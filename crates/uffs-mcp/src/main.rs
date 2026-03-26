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
    /// JSON-RPC version.
    jsonrpc: String,
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

/// MCP tool definition.
#[derive(Debug, Serialize)]
struct ToolDef {
    /// Tool name.
    name: String,
    /// Human-readable description.
    description: String,
    /// JSON Schema for input parameters.
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

/// MCP tool call result content.
#[derive(Debug, Serialize)]
struct ToolResult {
    /// Content items.
    content: Vec<ContentItem>,
}

/// MCP content item.
#[derive(Debug, Serialize)]
struct ContentItem {
    /// Content type.
    #[serde(rename = "type")]
    content_type: String,
    /// Text content.
    text: String,
}

// ────────────────────────────────────────────────────────────────────────────
// Tool Definitions
// ────────────────────────────────────────────────────────────────────────────

/// Build the list of tools we advertise to MCP clients.
fn tool_definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "uffs_search".to_owned(),
            description: "Search files across all indexed NTFS drives. Supports glob patterns (*.rs), regex (>pattern), path patterns (\\dir\\*.txt), and substring search. Returns file name, path, size, timestamps, and attributes.".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Search pattern. Examples: '*.rs' (glob), '>.*\\.log' (regex), '\\Users\\*.txt' (path), 'readme' (substring)"
                    },
                    "case_sensitive": {
                        "type": "boolean",
                        "description": "Case-sensitive matching (default: false)",
                        "default": false
                    },
                    "sort": {
                        "type": "string",
                        "description": "Sort column: name, size, modified, created, accessed, path, extension, type, descendants",
                        "default": "modified"
                    },
                    "sort_desc": {
                        "type": "boolean",
                        "description": "Sort descending (default: true for size/date, false for name/path)",
                        "default": true
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max results to return (default: 100, max: 10000)",
                        "default": 100
                    },
                    "filter": {
                        "type": "string",
                        "description": "Filter: 'all' (default), 'files', 'dirs'",
                        "default": "all"
                    }
                },
                "required": ["pattern"]
            }),
        },
        ToolDef {
            name: "uffs_drives".to_owned(),
            description: "List all loaded drives with record counts and source information.".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolDef {
            name: "uffs_status".to_owned(),
            description: "Get daemon status: loading progress, uptime, connections, PID.".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolDef {
            name: "uffs_info".to_owned(),
            description: "Get detailed information about a specific file or directory by its full path. Returns all available metadata including size, timestamps, attributes, descendants, and treesize.".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Full file path, e.g. 'C:\\Users\\Documents\\report.pdf'"
                    }
                },
                "required": ["path"]
            }),
        },
    ]
}

// ────────────────────────────────────────────────────────────────────────────
// Main Loop
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

    // Connect to daemon (auto-starts if needed)
    let mut client = match UffsClient::connect().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "Failed to connect to daemon");
            anyhow::bail!("Cannot connect to uffs-daemon: {e}");
        }
    };

    tracing::info!("Connected to uffs-daemon");

    // Read stdin line by line (MCP uses newline-delimited JSON-RPC)
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(error = %e, "stdin read error");
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Parse MCP request
        let req: McpRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                let err = McpErrorResponse {
                    jsonrpc: "2.0".to_owned(),
                    id: Value::Null,
                    error: McpError {
                        code: -32700,
                        message: format!("Parse error: {e}"),
                    },
                };
                write_response(&err)?;
                continue;
            }
        };

        let id = req.id.clone().unwrap_or(Value::Null);

        // Dispatch
        let result = handle_mcp_request(&req, &mut client).await;

        match result {
            Ok(value) => {
                let resp = McpResponse {
                    jsonrpc: "2.0".to_owned(),
                    id,
                    result: value,
                };
                write_response(&resp)?;
            }
            Err(e) => {
                let err = McpErrorResponse {
                    jsonrpc: "2.0".to_owned(),
                    id,
                    error: McpError {
                        code: -32603,
                        message: e.to_string(),
                    },
                };
                write_response(&err)?;
            }
        }
    }

    tracing::info!("uffs-mcp shutting down");
    Ok(())
}

/// Write a JSON response to stdout (MCP protocol channel).
fn write_response<T: Serialize>(value: &T) -> anyhow::Result<()> {
    let json = serde_json::to_string(value)?;
    println!("{json}");
    Ok(())
}

/// Handle a single MCP request.
async fn handle_mcp_request(
    req: &McpRequest,
    client: &mut UffsClient,
) -> anyhow::Result<Value> {
    match req.method.as_str() {
        "initialize" => handle_initialize(),
        "tools/list" => handle_tools_list(),
        "tools/call" => handle_tool_call(&req.params, client).await,
        "resources/list" => handle_resources_list(),
        "resources/read" => handle_resources_read(&req.params, client).await,
        "prompts/list" => handle_prompts_list(),
        "prompts/get" => handle_prompts_get(&req.params),
        // Notifications (no response needed, but we return ok)
        "notifications/initialized" => Ok(Value::Null),
        other => {
            tracing::debug!(method = other, "Unknown MCP method");
            anyhow::bail!("Method not found: {other}")
        }
    }
}

/// Handle `initialize` — return server info + capabilities.
fn handle_initialize() -> anyhow::Result<Value> {
    Ok(serde_json::json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {},
            "resources": {},
            "prompts": {}
        },
        "serverInfo": {
            "name": "uffs",
            "version": env!("CARGO_PKG_VERSION")
        }
    }))
}

/// Handle `tools/list` — advertise available tools.
fn handle_tools_list() -> anyhow::Result<Value> {
    let tools = tool_definitions();
    Ok(serde_json::json!({ "tools": tools }))
}

/// Handle `tools/call` — dispatch to the appropriate tool.
async fn handle_tool_call(
    params: &Value,
    client: &mut UffsClient,
) -> anyhow::Result<Value> {
    let tool_name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    let text = match tool_name {
        "uffs_search" => tool_search(arguments, client).await?,
        "uffs_drives" => tool_drives(client).await?,
        "uffs_status" => tool_status(client).await?,
        "uffs_info" => tool_info(arguments, client).await?,
        other => anyhow::bail!("Unknown tool: {other}"),
    };

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": text
        }]
    }))
}

/// Tool: uffs_search — search files.
async fn tool_search(args: Value, client: &mut UffsClient) -> anyhow::Result<String> {
    let pattern = args
        .get("pattern")
        .and_then(Value::as_str)
        .unwrap_or("*")
        .to_owned();

    let params = SearchParams {
        pattern,
        case_sensitive: args.get("case_sensitive").and_then(Value::as_bool).unwrap_or(false),
        whole_word: false,
        sort: args.get("sort").and_then(Value::as_str).map(String::from),
        sort_desc: args.get("sort_desc").and_then(Value::as_bool).unwrap_or(true),
        limit: args.get("limit").and_then(Value::as_u64).map(|n| n.min(10_000) as u32),
        filter: args.get("filter").and_then(Value::as_str).map(String::from),
        drives: Vec::new(),
    };

    let response = client.search(&params).await
        .map_err(|e| anyhow::anyhow!("Search failed: {e}"))?;

    // Format results as a readable table
    let mut output = String::new();
    output.push_str(&format!(
        "Found {} results ({} records scanned in {}ms)\n\n",
        response.rows.len(),
        response.records_scanned,
        response.duration_ms
    ));

    if response.rows.is_empty() {
        output.push_str("No matches found.\n");
        return Ok(output);
    }

    // Header
    output.push_str("| Name | Size | Modified | Path |\n");
    output.push_str("|------|------|----------|------|\n");

    for row in &response.rows {
        let size = uffs_client::protocol::format_size(row.size);
        let modified = uffs_client::protocol::format_time(row.modified);
        output.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            row.name, size, modified, row.path
        ));
    }

    if response.truncated {
        output.push_str(&format!(
            "\n(Results truncated. Use 'limit' parameter to see more.)\n"
        ));
    }

    Ok(output)
}

/// Tool: uffs_drives — list loaded drives.
async fn tool_drives(client: &mut UffsClient) -> anyhow::Result<String> {
    let response = client.drives().await
        .map_err(|e| anyhow::anyhow!("Failed to list drives: {e}"))?;

    let mut output = String::new();
    output.push_str(&format!("Loaded {} drive(s):\n\n", response.drives.len()));

    for drive in &response.drives {
        output.push_str(&format!(
            "  {}:  {:>10} records  ({})\n",
            drive.letter, drive.records, drive.source
        ));
    }

    Ok(output)
}

/// Tool: uffs_status — daemon status.
async fn tool_status(client: &mut UffsClient) -> anyhow::Result<String> {
    let response = client.status().await
        .map_err(|e| anyhow::anyhow!("Failed to get status: {e}"))?;

    let status_str = serde_json::to_string_pretty(&response.status)?;
    Ok(format!(
        "Daemon Status: {}\nUptime: {}s\nConnections: {}\nPID: {}\n",
        status_str,
        response.uptime_secs,
        response.connections,
        response.pid
    ))
}

/// Tool: uffs_info — file/directory info by path.
async fn tool_info(args: Value, client: &mut UffsClient) -> anyhow::Result<String> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("");

    if path.is_empty() {
        return Ok("Error: 'path' parameter is required.".to_owned());
    }

    let response = client.info(path).await
        .map_err(|e| anyhow::anyhow!("Failed to get info: {e}"))?;

    if !response.found {
        return Ok(format!("File not found: {path}"));
    }

    match response.record {
        Some(record) => Ok(serde_json::to_string_pretty(&record)?),
        None => Ok(format!("File found but no details available: {path}")),
    }
}

// ────────────────────────────────────────────────────────────────────────────
// MCP Resources
// ────────────────────────────────────────────────────────────────────────────

/// Handle `resources/list` — expose drives as browsable resources.
fn handle_resources_list() -> anyhow::Result<Value> {
    Ok(serde_json::json!({
        "resources": [
            {
                "uri": "uffs://drives",
                "name": "Indexed Drives",
                "description": "List of all NTFS drives indexed by the UFFS daemon with record counts and source info",
                "mimeType": "application/json"
            },
            {
                "uri": "uffs://status",
                "name": "Daemon Status",
                "description": "Current daemon status: loading progress, uptime, memory, connections",
                "mimeType": "application/json"
            }
        ]
    }))
}

/// Handle `resources/read` — read a specific resource.
async fn handle_resources_read(params: &Value, client: &mut UffsClient) -> anyhow::Result<Value> {
    let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");

    let (content, mime) = match uri {
        "uffs://drives" => {
            let resp = client.drives().await
                .map_err(|e| anyhow::anyhow!("Failed to read drives: {e}"))?;
            (serde_json::to_string_pretty(&resp)?, "application/json")
        }
        "uffs://status" => {
            let resp = client.status().await
                .map_err(|e| anyhow::anyhow!("Failed to read status: {e}"))?;
            (serde_json::to_string_pretty(&resp)?, "application/json")
        }
        _ => anyhow::bail!("Unknown resource: {uri}"),
    };

    Ok(serde_json::json!({
        "contents": [{
            "uri": uri,
            "mimeType": mime,
            "text": content
        }]
    }))
}

// ────────────────────────────────────────────────────────────────────────────
// MCP Prompts
// ────────────────────────────────────────────────────────────────────────────

/// Handle `prompts/list` — pre-built search templates.
fn handle_prompts_list() -> anyhow::Result<Value> {
    Ok(serde_json::json!({
        "prompts": [
            {
                "name": "find_large_files",
                "description": "Find the largest files across all drives, sorted by size descending",
                "arguments": [
                    {
                        "name": "limit",
                        "description": "Number of results (default: 50)",
                        "required": false
                    }
                ]
            },
            {
                "name": "recent_changes",
                "description": "Find files modified in the last N days",
                "arguments": [
                    {
                        "name": "days",
                        "description": "Number of days to look back (default: 1)",
                        "required": false
                    }
                ]
            },
            {
                "name": "find_by_extension",
                "description": "Find all files with a specific extension",
                "arguments": [
                    {
                        "name": "extension",
                        "description": "File extension without dot (e.g., 'rs', 'pdf', 'jpg')",
                        "required": true
                    },
                    {
                        "name": "limit",
                        "description": "Number of results (default: 100)",
                        "required": false
                    }
                ]
            },
            {
                "name": "find_duplicates_by_name",
                "description": "Search for files with the same name across all drives",
                "arguments": [
                    {
                        "name": "filename",
                        "description": "Exact filename to search for",
                        "required": true
                    }
                ]
            }
        ]
    }))
}

/// Handle `prompts/get` — return a specific prompt with arguments filled in.
fn handle_prompts_get(params: &Value) -> anyhow::Result<Value> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(Value::Object(serde_json::Map::new()));

    let messages = match name {
        "find_large_files" => {
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50);
            vec![serde_json::json!({
                "role": "user",
                "content": {
                    "type": "text",
                    "text": format!("Use the uffs_search tool to find the {limit} largest files. Use pattern '*', sort by 'size' descending, limit {limit}, filter 'files'. Show results as a table with name, size, and path.")
                }
            })]
        }
        "recent_changes" => {
            let days = args.get("days").and_then(Value::as_u64).unwrap_or(1);
            vec![serde_json::json!({
                "role": "user",
                "content": {
                    "type": "text",
                    "text": format!("Use the uffs_search tool to find files modified in the last {days} day(s). Use pattern '*', sort by 'modified' descending, limit 100. Show results as a table.")
                }
            })]
        }
        "find_by_extension" => {
            let ext = args.get("extension").and_then(Value::as_str).unwrap_or("txt");
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(100);
            vec![serde_json::json!({
                "role": "user",
                "content": {
                    "type": "text",
                    "text": format!("Use the uffs_search tool to find all *.{ext} files. Use pattern '*.{ext}', sort by 'modified' descending, limit {limit}. Show results as a table.")
                }
            })]
        }
        "find_duplicates_by_name" => {
            let filename = args.get("filename").and_then(Value::as_str).unwrap_or("*");
            vec![serde_json::json!({
                "role": "user",
                "content": {
                    "type": "text",
                    "text": format!("Use the uffs_search tool to find all files named '{filename}' across all drives. This helps identify duplicate files. Show the full path for each result.")
                }
            })]
        }
        _ => anyhow::bail!("Unknown prompt: {name}"),
    };

    Ok(serde_json::json!({
        "description": format!("UFFS search prompt: {name}"),
        "messages": messages
    }))
}
