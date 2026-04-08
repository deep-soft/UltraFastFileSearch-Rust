//! MCP server implementation and formatting helpers.

use serde::Serialize;
use serde_json::Value;
use uffs_client::connect::UffsClient;
use uffs_client::protocol::SearchParams;

use crate::types::{McpError, McpErrorResponse, McpRequest, McpResponse};

/// MCP server state wrapping a daemon client.
pub struct McpServer {
    /// Connected daemon client.
    pub client: UffsClient,
}

/// Write a JSON response to stdout (MCP protocol channel).
///
/// Called from both `process_request` and `dispatch_tool_call` (multiple
/// sites).
#[expect(clippy::print_stdout, reason = "MCP protocol uses stdout as transport")]
pub fn write_response<T: Serialize>(value: &T) -> anyhow::Result<()> {
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
    pub(crate) async fn process_request(&mut self, trimmed: &str) -> anyhow::Result<()> {
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

        let id = req.id.clone();

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
            { "name": "uffs_info", "description": "Get detailed information about a specific file or directory by its full path.", "inputSchema": { "type": "object", "properties": { "path": { "type": "string", "description": "Full file path" } }, "required": ["path"] } },
            { "name": "uffs_aggregate", "description": "Summarize filesystem results with server-side aggregations. Use this for counts, storage breakdowns, histograms, folder rollups, or duplicate summaries instead of raw file rows. Available presets: overview, by_type, by_extension, by_drive, by_size, by_age, storage, activity, top_folders, duplicates, media, cleanup.", "inputSchema": { "type": "object", "properties": { "pattern": { "type": "string", "default": "*", "description": "Search pattern to scope aggregation" }, "preset": { "type": "string", "description": "Named preset (overview, by_type, by_extension, by_drive, by_size, by_age, storage, activity, top_folders, duplicates, media, cleanup)" }, "aggregations": { "type": "array", "description": "Custom aggregate specs in power syntax (e.g. 'terms:extension,top=50')" }, "drives": { "type": "array", "items": { "type": "string" }, "description": "Limit to specific drive letters" } }, "required": [] } },
            { "name": "uffs_facet_values", "description": "Search within facet values for a specific field. Returns top values with counts. Use cursor/page_size to paginate through large value spaces.", "inputSchema": { "type": "object", "properties": { "field": { "type": "string", "description": "Field to facet on (e.g. extension, type, drive)" }, "pattern": { "type": "string", "default": "*", "description": "Search pattern to scope facet" }, "prefix": { "type": "string", "description": "Filter facet values by prefix" }, "top": { "type": "integer", "default": 20_i64, "description": "Number of facet values to return" }, "cursor": { "type": "string", "description": "Opaque cursor from a previous response's next_cursor to fetch the next page" }, "page_size": { "type": "integer", "description": "Max buckets per page (enables pagination; response includes next_cursor when more pages exist)" } }, "required": ["field"] } }
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
            "uffs_aggregate" => self.tool_aggregate(&arguments).await,
            "uffs_facet_values" => self.tool_facet_values(&arguments).await,
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
        let mut search_params = SearchParams {
            pattern,
            case_sensitive: arguments
                .get("case_sensitive")
                .and_then(Value::as_bool)
                .unwrap_or(false),
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
                .and_then(|val| u32::try_from(val).ok()),
            filter: arguments
                .get("filter")
                .and_then(Value::as_str)
                .map(String::from),
            ..Default::default()
        };
        search_params.populate_canonical_fields();
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

    /// Tool handler: `uffs_aggregate`.
    async fn tool_aggregate(&mut self, arguments: &Value) -> anyhow::Result<String> {
        use uffs_client::protocol::AggregateSpecWire;

        let pattern = arguments
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or("*");

        let mut agg_specs: Vec<AggregateSpecWire> = Vec::new();

        // Handle preset parameter.
        if let Some(preset) = arguments.get("preset").and_then(Value::as_str) {
            agg_specs.push(AggregateSpecWire {
                kind: "preset".to_owned(),
                label: None,
                field: None,
                top: None,
                interval: None,
                calendar: None,
                boundaries: vec![],
                metrics: vec![],
                preset: Some(preset.to_owned()),
                sample: None,
                sample_sort: None,
                sample_desc: None,
                verify: None,
                verify_bytes: None,
            });
        }

        // Handle custom aggregations array.
        if let Some(aggs) = arguments.get("aggregations").and_then(Value::as_array) {
            for agg_str in aggs {
                if let Some(spec_str) = agg_str.as_str() {
                    agg_specs.push(AggregateSpecWire {
                        kind: "raw".to_owned(),
                        label: Some(spec_str.to_owned()),
                        field: None,
                        top: None,
                        interval: None,
                        calendar: None,
                        boundaries: vec![],
                        metrics: vec![],
                        preset: None,
                        sample: None,
                        sample_sort: None,
                        sample_desc: None,
                        verify: None,
                        verify_bytes: None,
                    });
                }
            }
        }

        // Default to overview if no specs given.
        if agg_specs.is_empty() {
            agg_specs.push(AggregateSpecWire {
                kind: "preset".to_owned(),
                label: None,
                field: None,
                top: None,
                interval: None,
                calendar: None,
                boundaries: vec![],
                metrics: vec![],
                preset: Some("overview".to_owned()),
                sample: None,
                sample_sort: None,
                sample_desc: None,
                verify: None,
                verify_bytes: None,
            });
        }

        let drives: Vec<char> = arguments
            .get("drives")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .filter_map(|ch| ch.chars().next())
                    .collect()
            })
            .unwrap_or_default();

        let params = SearchParams {
            pattern: pattern.to_owned(),
            aggregations: agg_specs,
            include_rows: false,
            drives,
            ..Default::default()
        };

        let response = self
            .client
            .search(&params)
            .await
            .map_err(|err| anyhow::anyhow!("Aggregate failed: {err}"))?;

        let summary = format_aggregate_summary(&response.aggregations);
        let json = serde_json::to_string_pretty(&serde_json::json!({
            "records_scanned": response.records_scanned,
            "duration_ms": response.duration_ms,
            "aggregations": response.aggregations
        }))?;

        Ok(format!("{summary}\n\n```json\n{json}\n```"))
    }

    /// Tool handler: `uffs_facet_values`.
    async fn tool_facet_values(&mut self, arguments: &Value) -> anyhow::Result<String> {
        use uffs_client::protocol::AggregateSpecWire;

        let field = arguments
            .get("field")
            .and_then(Value::as_str)
            .unwrap_or("extension");
        let pattern = arguments
            .get("pattern")
            .and_then(Value::as_str)
            .unwrap_or("*");
        let top: u16 = arguments
            .get("top")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .try_into()
            .unwrap_or(u16::MAX);
        let cursor = arguments
            .get("cursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let page_size: Option<u16> = arguments
            .get("page_size")
            .and_then(Value::as_u64)
            .and_then(|val| val.try_into().ok());

        let agg_spec = AggregateSpecWire {
            kind: "terms".to_owned(),
            label: Some(format!("facet_{field}")),
            field: Some(field.to_owned()),
            top: Some(top),
            interval: None,
            calendar: None,
            boundaries: vec![],
            metrics: vec!["count".to_owned(), "total_bytes".to_owned()],
            preset: None,
            sample: None,
            sample_sort: None,
            sample_desc: None,
            verify: None,
            verify_bytes: None,
        };

        let params = SearchParams {
            pattern: pattern.to_owned(),
            aggregations: vec![agg_spec],
            include_rows: false,
            agg_cursor: cursor,
            agg_page_size: page_size,
            ..Default::default()
        };

        let response = self
            .client
            .search(&params)
            .await
            .map_err(|err| anyhow::anyhow!("Facet values failed: {err}"))?;

        let summary = format_aggregate_summary(&response.aggregations);
        let json = serde_json::to_string_pretty(&serde_json::json!({
            "field": field,
            "aggregations": response.aggregations
        }))?;

        Ok(format!("{summary}\n\n```json\n{json}\n```"))
    }
}

/// Format aggregate results as a compact human-readable summary.
pub fn format_aggregate_summary(results: &[uffs_client::protocol::AggregateResultWire]) -> String {
    use core::fmt::Write;
    let mut out = String::new();

    for result in results {
        let label = result.label.as_deref().unwrap_or(&result.kind);

        match result.kind.as_str() {
            "count" => {
                let val = result.value.unwrap_or(0);
                writeln!(out, "• {label}: {val}").ok();
            }
            "missing" => {
                let val = result.value.unwrap_or(0);
                writeln!(out, "• {label}: {val} records with missing value").ok();
            }
            "distinct" => {
                let val = result.value.unwrap_or(0);
                writeln!(out, "• {label}: {val} distinct values").ok();
            }
            "stats" => {
                if let Some(stats) = &result.stats {
                    writeln!(
                        out,
                        "• {label}: count={} sum={} min={} max={} avg={:.1}",
                        stats.count, stats.sum, stats.min, stats.max, stats.avg
                    )
                    .ok();
                    if stats.waste_bytes > 0 {
                        writeln!(
                            out,
                            "  waste: {} bytes ({:.1}%)",
                            stats.waste_bytes, stats.waste_pct
                        )
                        .ok();
                    }
                }
            }
            "buckets" | "terms" | "rollup" | "duplicates" => {
                format_bucket_summary(&mut out, label, result);
            }
            _ => {
                writeln!(
                    out,
                    "• {label}: (kind={}, {} buckets)",
                    result.kind,
                    result.buckets.len()
                )
                .ok();
            }
        }
    }

    if out.is_empty() {
        out.push_str("No aggregate results.");
    }

    out
}

/// Format bucket-style results (terms, rollup, duplicates) into `out`.
fn format_bucket_summary(
    out: &mut String,
    label: &str,
    result: &uffs_client::protocol::AggregateResultWire,
) {
    use core::fmt::Write;

    writeln!(out, "• {label} ({} buckets):", result.buckets.len()).ok();
    for bucket in result.buckets.iter().take(10) {
        writeln!(
            out,
            "    {:<30} count={:<8} bytes={}",
            bucket.key, bucket.count, bucket.total_bytes
        )
        .ok();
        // Sample rows (top-hits), max 3 per bucket.
        let max_samples = 3;
        for sr in bucket.sample_rows.iter().take(max_samples) {
            let name = sr.fields.get("name").map_or("?", |val| val.as_str());
            let size = sr
                .fields
                .get("size")
                .and_then(|val| val.parse::<u64>().ok())
                .map_or(String::new(), |n| format!(" ({n} B)"));
            writeln!(out, "      → {name}{size}").ok();
        }
        let remaining = bucket.sample_rows.len().saturating_sub(max_samples);
        if remaining > 0 {
            writeln!(out, "      ... and {remaining} more").ok();
        }
        // Nested sub-aggregation buckets.
        for sub in bucket.sub_buckets.iter().take(5) {
            writeln!(
                out,
                "      ├─ {:<26} count={:<8} bytes={}",
                sub.key, sub.count, sub.total_bytes
            )
            .ok();
        }
        let sub_rest = bucket.sub_buckets.len().saturating_sub(5);
        if sub_rest > 0 {
            writeln!(out, "      ... and {sub_rest} more sub-buckets").ok();
        }
    }
    if result.buckets.len() > 10 {
        writeln!(out, "    ... and {} more", result.buckets.len() - 10).ok();
    }
    if let Some(other) = result.other_count
        && other > 0
    {
        writeln!(out, "    (+ {other} in other groups)").ok();
    }
    if result.values_complete == Some(false) {
        writeln!(out, "    [truncated — not all values shown]").ok();
    }
    if result.exact == Some(false) {
        writeln!(out, "    [approximate — not all records scanned]").ok();
    }
    if let Some(cursor) = &result.next_cursor {
        writeln!(out, "    [next_cursor: {cursor}]").ok();
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Entry Point
// ────────────────────────────────────────────────────────────────────────────
