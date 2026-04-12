//! MCP [`ServerHandler`] implementation — bridges rmcp to the UFFS daemon.
//!
//! This is the core of the MCP server.  It implements the rmcp
//! [`ServerHandler`] trait, dispatching `tools/call`, `resources/read`,
//! and `prompts/get` to the appropriate handlers.

extern crate alloc;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use rmcp::model::{
    AnnotateAble, CallToolRequestParams, CallToolResult, GetPromptRequestParams, GetPromptResult,
    Implementation, ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult,
    ListToolsResult, PaginatedRequestParams, PromptMessage, RawResource, RawResourceTemplate,
    ReadResourceRequestParams, ReadResourceResult, ResourceContents, ServerCapabilities,
    ServerInfo, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde_json::Value;
use tokio::sync::Mutex;
use uffs_client::connect::UffsClient;

use crate::error::BridgeError;
use crate::roots::{self, SharedRootsState};
use crate::stats::McpStats;
use crate::tools;

// ── Agent instructions ────────────────────────────────────────────────
//
// This is the first thing every MCP agent sees.  It must be:
// • Concise enough to fit in a context window without crowding.
// • Comprehensive enough that an agent can answer filesystem questions
//   without reading any other docs.
// • Actionable — every sentence should help the agent decide which tool
//   to call and with which parameters.

/// Server instructions returned in the `initialize` response.
///
/// Designed to be read by LLM agents, not humans.  Teaches the agent
/// how to use UFFS tools effectively in the fewest possible queries.
const AGENT_INSTRUCTIONS: &str = "\
UFFS — Ultra Fast File Search.  Indexes NTFS drives via the Master File \
Table and serves sub-millisecond queries over millions of files.

TOOLS (all read-only):
• uffs_search   — Search files/dirs.  Supports glob (*.pdf), regex (>pattern), \
  substring (invoice), and match-all (*).  40+ filter parameters: size, date, \
  extension, type, path, NTFS attributes, bulkiness, treesize, descendants.
• uffs_aggregate — Server-side analytics.  Use presets for one-call answers: \
  overview, by_type, by_extension, by_drive, by_size, by_age, storage, \
  activity, top_folders, duplicates, media, cleanup.
• uffs_facet_values — Discover distinct values of a field (extension, type, \
  drive) with counts and byte totals.  Use BEFORE searching to understand \
  what exists.
• uffs_info     — Full metadata for a single file/directory by path.
• uffs_drives   — List indexed drives with record counts.
• uffs_status   — Daemon health, uptime, memory, loading progress.

QUERY STRATEGY (minimize round-trips):
1. Start with uffs_aggregate preset='overview' to get the lay of the land.
2. Use uffs_facet_values to discover what extensions/types/drives exist.
3. Use uffs_search with specific filters to drill down.
4. Combine multiple filters in ONE call — they are ANDed.  More filters = \
   fewer results = faster.

KEY PARAMETERS for uffs_search:
• pattern: '*' (match-all), '*.ext' (glob), 'word' (substring), '>regex'
• filter: 'files', 'dirs', or 'all'
• ext: 'pdf' or collection aliases: pictures, documents, videos, music, \
  archives, code
• type_filter: semantic category: picture, document, archive, code, video, \
  audio, executable, database, config, log, system
• min_size / max_size: bytes (1073741824 = 1 GB)
• newer / older: '7d', '24h', '2w', '2026-01-15', 'today', 'last_30d'
• newer_created / older_created / newer_accessed / older_accessed
• path_contains: scope to a subtree ('Users\\\\name' or 'Users/name')
• drives: ['C'] or ['C','D'] to scope to specific drives
• sort: 'modified', '-size', 'name', '-treesize', '-descendants', '-bulkiness'
• limit: max results (default 50, cap 500)
• projection: columns to return — name, ext, type, size, modified, path, drive, \
  created, accessed, allocated, treesize, descendants, tree_allocated
• whole_word: true for word-boundary matching
• attr: NTFS attributes — 'hidden', 'system', 'compressed', 'encrypted', etc.
• min_descendants / max_descendants: filter dirs by child count

KEY PARAMETERS for uffs_aggregate:
• preset: one-word shortcut — overview, by_type, by_extension, by_drive, \
  by_size, by_age, storage, activity, top_folders, duplicates, media, cleanup
• aggregations: array of custom power-syntax specs for full control. \
  10 kinds: count, stats:FIELD, terms:FIELD, hist:FIELD, datehist:FIELD, \
  range:FIELD, missing:FIELD, distinct:FIELD, rollup:path, duplicates:KEY+KEY. \
  Options: top=N, sample=N, interval=N, calendar=day|week|month|quarter|year, \
  depth=N, bins=A..B+C..D, sub=kind:field.
• pattern / drives: scope aggregation to a subset (same as search).
• page_size: enable paginated buckets. Response includes next_cursor.
• cursor: opaque token from previous response to fetch the next page.
POWER MOVE: stack multiple specs in ONE call — \
  aggregations=['count','stats:size','terms:type,top=10'] runs all in one pass.
AGGREGATABLE fields (stats/hist/range): size, allocated, modified, created, \
  accessed, descendants, treesize, tree_allocated, bulkiness, name_length, \
  path_length.
GROUPABLE fields (terms/rollup): extension, type, drive, name, directory, \
  hidden, system, compressed, encrypted, read_only, archive, sparse, reparse, \
  temporary, offline.

KEY PARAMETERS for uffs_facet_values:
• field: 'extension', 'type', or 'drive'
• top: number of values to return (default 20)
• pattern: scope to files matching a search pattern
• prefix: filter values by prefix (e.g. 'doc' → doc, docx, docm)
• page_size / cursor: pagination (same as aggregate)

RESOURCES (readable even if tools are unavailable):
• uffs://cookbook — ESSENTIAL: ~30 curated example queries with ready-to-use \
  arguments objects organized by workflow.  Read this FIRST to learn patterns.
• uffs://schema/search — JSON Schema for uffs_search parameters.
• uffs://schema/fields — Complete field catalog with types and capabilities.
• uffs://presets/aggregate — List of aggregate presets with descriptions.
• uffs://drives — Live drive listing.
• uffs://status — Daemon health.
• uffs://info/{path} — Dynamic resource template for file metadata \
  (percent-encode path: C:\\Windows → C%3A%5CWindows).

COMMON USER REQUESTS (natural language -> tool call):
• Find my file        -> uffs_search pattern='filename'
• Where is folder X?  -> uffs_search pattern='X' filter='dirs'
• What eats space?    -> uffs_aggregate preset='overview', then sort='-size'
• Clean up disk       -> uffs_aggregate preset='cleanup', then old+large search
• Recent files        -> uffs_search newer='7d' sort='-modified'
• Recently opened     -> uffs_search newer_accessed='7d' sort='-accessed'
• Find duplicates     -> uffs_aggregate preset='duplicates'
• Empty folders       -> uffs_search filter='dirs' max_descendants=0
• Long path problems  -> uffs_search min_path_length=260 sort='-path_length'
• Hidden files        -> uffs_search attr='hidden' sort='-size'
• Find config files   -> uffs_search ext='json,toml,yaml,yml,ini,env,cfg'
• Big photos/videos   -> uffs_search ext='pictures,videos' min_size=52428800 sort='-size'
• Recent executables  -> uffs_search type_filter='executable' newer='7d'
• Old large files     -> uffs_search min_size=104857600 older='365d' sort='-size'
• Inventory a drive   -> uffs_aggregate preset='overview' drives=['X']
NOTE: UFFS does NOT search inside file contents — it searches file names, \
paths, and metadata.  For content search, suggest ripgrep or similar.

PROMPTS (guided multi-step workflows):
find_large_files, find_by_extension, disk_usage_report, cleanup_report, \
recent_changes, duplicate_investigation, subtree_analysis.
";

/// Connection strategy for the daemon client.
///
/// Both `Active` and `None` support auto-reconnect: if the daemon dies
/// mid-session, the next tool call clears the stale client, reconnects
/// via `connect_with_args` (which auto-starts the daemon), and retries.
enum ClientSlot {
    /// Connected (or reconnectable) client.
    ///
    /// Used by both stdio (`mcp run`) and HTTP gateway (`mcp serve`).
    /// The `client` starts as `Some` for eager connections (stdio) or
    /// `None` for lazy connections (HTTP).  Either way, if the daemon
    /// dies, the client is cleared and reconnected on the next tool call.
    Active {
        /// Args forwarded to `uffs daemon run` on auto-start / reconnect.
        spawn_args: Vec<String>,
        /// The daemon connection — `None` before first use or after a
        /// reconnect-clearing.
        client: Mutex<Option<UffsClient>>,
    },
    /// No daemon — metadata-only / testing.
    None,
}

/// The UFFS MCP server — wraps a daemon client and dispatches MCP requests.
pub struct UffsMcpServer {
    /// Daemon connection (pre-connected, lazy, or none).
    slot: ClientSlot,
    /// Current roots state (updated via `on_roots_list_changed`).
    roots: SharedRootsState,
    /// Timestamp of the last MCP activity (tool call, resource read, etc.).
    /// Used by the idle-timeout logic in [`crate::run_mcp_server_with_config`].
    last_activity: Arc<AtomicU64>,
    /// Runtime statistics (shared across sessions in HTTP mode).
    stats: Arc<McpStats>,
}

impl UffsMcpServer {
    /// Create a new server wrapping a pre-connected daemon client.
    ///
    /// The `spawn_args` are stored for reconnection if the daemon dies.
    #[must_use]
    pub fn new(client: UffsClient, spawn_args: Vec<String>) -> Self {
        Self::with_stats(
            ClientSlot::Active {
                spawn_args,
                client: Mutex::new(Some(client)),
            },
            Arc::new(McpStats::default()),
        )
    }

    /// Create a server that lazily connects to the daemon on first tool call.
    ///
    /// This is used by the HTTP gateway, where the factory closure must be
    /// synchronous but daemon connection is async.  The `spawn_args` are
    /// forwarded to [`UffsClient::connect_with_args`] on first use and on
    /// reconnect after daemon failure.
    #[must_use]
    pub fn new_lazy(spawn_args: Vec<String>) -> Self {
        Self::new_lazy_with_stats(spawn_args, Arc::new(McpStats::default()))
    }

    /// Create a lazy server with shared stats (for HTTP gateway).
    #[must_use]
    pub fn new_lazy_with_stats(spawn_args: Vec<String>, stats: Arc<McpStats>) -> Self {
        stats.session_started();
        Self::with_stats(
            ClientSlot::Active {
                spawn_args,
                client: Mutex::new(None),
            },
            stats,
        )
    }

    /// Create a server without a daemon connection.
    ///
    /// Listing tools/resources/prompts works, but calling tools that need the
    /// daemon will return an error.  Useful for testing and metadata
    /// introspection.
    #[must_use]
    pub fn new_unconnected() -> Self {
        Self::with_stats(ClientSlot::None, Arc::new(McpStats::default()))
    }

    /// Internal constructor.
    fn with_stats(slot: ClientSlot, stats: Arc<McpStats>) -> Self {
        Self {
            slot,
            roots: SharedRootsState::default(),
            last_activity: Arc::new(AtomicU64::new(Self::now_secs())),
            stats,
        }
    }

    /// Get a shared handle to the stats for the HTTP `/status` endpoint.
    #[must_use]
    pub const fn stats(&self) -> &Arc<McpStats> {
        &self.stats
    }

    /// Get a shared handle to the last-activity timestamp for the idle
    /// timeout loop.
    #[must_use]
    pub fn last_activity_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.last_activity)
    }

    /// Record that the server just handled an MCP request.
    fn touch(&self) {
        self.last_activity
            .store(Self::now_secs(), Ordering::Relaxed);
    }

    /// Current time as seconds since the Unix epoch.
    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |dur| dur.as_secs())
    }

    /// Get a lock on the daemon client, or return a bridge error.
    ///
    /// For [`ClientSlot::Active`]: returns the cached client, connecting
    ///   on first call or after a reconnect-clearing.
    /// For [`ClientSlot::None`]: returns an error.
    async fn client(&self) -> Result<ClientGuard<'_>, BridgeError> {
        match &self.slot {
            ClientSlot::Active { spawn_args, client } => {
                let mut guard = client.lock().await;
                if guard.is_none() {
                    tracing::info!("Connecting to daemon (auto-starting if needed)...");
                    let connected = UffsClient::connect_with_args(spawn_args)
                        .await
                        .map_err(|err| BridgeError::Daemon(err.to_string()))?;
                    *guard = Some(connected);
                }
                Ok(ClientGuard(guard))
            }
            ClientSlot::None => Err(BridgeError::Daemon("Not connected to daemon".to_owned())),
        }
    }

    /// Clear the cached daemon connection so the next `client()` call
    /// reconnects via `connect_with_args` (auto-starting the daemon if
    /// needed).
    ///
    /// Called after a tool call fails with a daemon connection error.
    async fn clear_cached_client(&self) {
        if let ClientSlot::Active { client, .. } = &self.slot {
            let mut guard = client.lock().await;
            if guard.is_some() {
                tracing::info!("Clearing stale daemon connection for reconnect");
                *guard = None;
            }
        }
    }

    /// Read-only access to the shared roots state.
    #[expect(dead_code, reason = "accessor for future external use")]
    pub(crate) const fn roots(&self) -> &SharedRootsState {
        &self.roots
    }
}

/// Wrapper that provides `&mut UffsClient` regardless of the slot type.
///
/// **Invariant**: the `Lazy` variant is **only** constructed after the
/// `Option<UffsClient>` has been populated by [`UffsMcpServer::client()`],
/// so the `expect` is structurally unreachable (the `Option` is always
/// populated by `client()` before the guard is returned).
pub(crate) struct ClientGuard<'a>(tokio::sync::MutexGuard<'a, Option<UffsClient>>);

#[expect(
    clippy::expect_used,
    reason = "client() always populates the Option before returning this guard"
)]
impl core::ops::Deref for ClientGuard<'_> {
    type Target = UffsClient;
    fn deref(&self) -> &UffsClient {
        self.0
            .as_ref()
            .expect("BUG: client not initialized before deref")
    }
}

#[expect(
    clippy::expect_used,
    reason = "client() always populates the Option before returning this guard"
)]
impl core::ops::DerefMut for ClientGuard<'_> {
    fn deref_mut(&mut self) -> &mut UffsClient {
        self.0
            .as_mut()
            .expect("BUG: client not initialized before deref_mut")
    }
}

impl UffsMcpServer {
    /// Gate on daemon readiness — returns `Err` if the daemon is still
    /// loading drives so the LLM receives a transient error and retries.
    ///
    /// Returns `Ok(())` when ready.
    async fn readiness_gate(client: &mut UffsClient) -> Result<(), BridgeError> {
        use uffs_client::protocol::DaemonStatus;
        let status = client
            .status()
            .await
            .map_err(|err| BridgeError::Daemon(format!("readiness check failed: {err}")))?;
        match status.status {
            DaemonStatus::Loading {
                drives_loaded,
                drives_total,
            } => Err(BridgeError::Daemon(format!(
                "⏳ Daemon is starting up — {drives_loaded}/{drives_total} drives loaded. \
                 Please retry in a few seconds."
            ))),
            DaemonStatus::Ready | DaemonStatus::Refreshing { .. } => Ok(()),
        }
    }

    /// Dispatch a single tool call to the appropriate handler.
    ///
    /// Separated from `call_tool` so the retry-on-reconnect logic can
    /// call it a second time with the same arguments.
    async fn dispatch_tool(
        &self,
        tool_name: &str,
        args: serde_json::Map<String, Value>,
    ) -> Result<CallToolResult, BridgeError> {
        let mut client = self.client().await?;

        // Gate: don't run queries against a partially-loaded daemon.
        // The `uffs_status` tool is exempt so the agent can check
        // readiness explicitly.
        if tool_name != "uffs_status" {
            Self::readiness_gate(&mut client).await?;
        }

        let roots_state = self.roots.read().await;

        match tool_name {
            "uffs_search" => {
                let parsed = serde_json::from_value(Value::Object(args)).map_err(|err| {
                    BridgeError::InvalidParam {
                        name: "arguments",
                        reason: err.to_string(),
                    }
                })?;
                tools::search::run(&mut client, parsed, &roots_state).await
            }
            "uffs_drives" => tools::drives::run(&mut client).await,
            "uffs_status" => tools::status::run(&mut client).await,
            "uffs_info" => {
                let parsed = serde_json::from_value(Value::Object(args)).map_err(|err| {
                    BridgeError::InvalidParam {
                        name: "arguments",
                        reason: err.to_string(),
                    }
                })?;
                tools::info::run(&mut client, parsed).await
            }
            "uffs_aggregate" => {
                let parsed = serde_json::from_value(Value::Object(args)).map_err(|err| {
                    BridgeError::InvalidParam {
                        name: "arguments",
                        reason: err.to_string(),
                    }
                })?;
                tools::aggregate::run(&mut client, parsed, &roots_state).await
            }
            "uffs_facet_values" => {
                let parsed = serde_json::from_value(Value::Object(args)).map_err(|err| {
                    BridgeError::InvalidParam {
                        name: "arguments",
                        reason: err.to_string(),
                    }
                })?;
                tools::facet_values::run(&mut client, parsed, &roots_state).await
            }
            other => Err(BridgeError::Daemon(format!("Unknown tool: {other}"))),
        }
    }
}

impl Drop for UffsMcpServer {
    fn drop(&mut self) {
        // Decrement active session count for HTTP gateway sessions.
        if matches!(self.slot, ClientSlot::Active { .. }) {
            self.stats.session_ended();
        }
    }
}

impl ServerHandler for UffsMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
        .with_server_info(Implementation::new("uffs", env!("CARGO_PKG_VERSION")))
        .with_instructions(AGENT_INSTRUCTIONS.to_owned())
    }

    async fn on_roots_list_changed(&self, context: rmcp::service::NotificationContext<RoleServer>) {
        // Ask the client for the current list of roots.
        match context.peer.list_roots().await {
            Ok(result) => {
                let mut state = self.roots.write().await;
                roots::update_roots_state(&mut state, &result.roots);
                let mapped = state
                    .roots
                    .iter()
                    .filter(|root| root.ntfs_prefix.is_some())
                    .count();
                let unmapped = state.roots.len() - mapped;
                tracing::info!(total = state.roots.len(), mapped, unmapped, "roots updated");
                for warning in &state.warnings {
                    tracing::warn!("{warning}");
                }
            }
            Err(err) => {
                tracing::warn!("failed to list roots from client: {err}");
            }
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.touch();
        Ok(ListToolsResult {
            tools: tool_definitions(),
            next_cursor: None,
            meta: None,
        })
    }

    #[expect(
        clippy::cognitive_complexity,
        reason = "tool dispatch with timing, error handling, stats, and reconnect"
    )]
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.touch();
        let tool_name = request.name.to_string();
        let args = request.arguments.unwrap_or_default();

        let args_json = serde_json::to_string(&args).unwrap_or_default();
        tracing::info!(
            tool = %tool_name,
            args = %args_json,
            "→ tool call received"
        );
        let t0 = std::time::Instant::now();

        // Reject unknown tools early (before touching the daemon).
        if !is_known_tool(&tool_name) {
            return Err(McpError::invalid_params(
                format!("Unknown tool: {tool_name}"),
                None,
            ));
        }

        // First attempt — retry once on daemon connection errors.
        let first_result = self.dispatch_tool(&tool_name, args.clone()).await;
        let final_result = match first_result {
            Err(err) if err.is_daemon_connection_error() => {
                tracing::warn!(
                    tool = %tool_name,
                    error = %err,
                    "Daemon connection lost — reconnecting and retrying..."
                );
                self.clear_cached_client().await;
                self.dispatch_tool(&tool_name, args).await
            }
            other => other,
        };

        let elapsed = t0.elapsed();
        let latency_us = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        match &final_result {
            Ok(_) => {
                tracing::info!(
                    tool = %tool_name,
                    elapsed_ms,
                    "← tool call OK"
                );
                self.stats.record_tool_call(&tool_name, latency_us);
            }
            Err(err) => {
                tracing::warn!(
                    tool = %tool_name,
                    elapsed_ms,
                    error = %err,
                    "← tool call FAILED"
                );
                self.stats.record_tool_error(&tool_name, latency_us);
            }
        }

        final_result.map_err(McpError::from)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        self.touch();
        Ok(ListResourcesResult {
            resources: vec![
                // ── Live metadata (require daemon) ──────────────────
                RawResource::new("uffs://drives", "Indexed Drives")
                    .with_description("List of all NTFS drives indexed by the UFFS daemon")
                    .with_mime_type("application/json")
                    .no_annotation(),
                RawResource::new("uffs://status", "Daemon Status")
                    .with_description(
                        "Current daemon status: loading progress, uptime, connections",
                    )
                    .with_mime_type("application/json")
                    .no_annotation(),
                // ── Static schema resources (no daemon needed) ──────
                RawResource::new("uffs://schema/fields", "Field Catalog")
                    .with_description(
                        "Canonical field catalog: every searchable/sortable/filterable \
                         field with type, aliases, and aggregation capability",
                    )
                    .with_mime_type("application/json")
                    .no_annotation(),
                RawResource::new("uffs://schema/search", "Search Request Schema")
                    .with_description("JSON Schema for the uffs_search tool input parameters")
                    .with_mime_type("application/json")
                    .no_annotation(),
                RawResource::new("uffs://schema/aggregate", "Aggregate Request Schema")
                    .with_description("JSON Schema for the uffs_aggregate tool input parameters")
                    .with_mime_type("application/json")
                    .no_annotation(),
                RawResource::new("uffs://presets/aggregate", "Aggregate Presets")
                    .with_description(
                        "Built-in aggregate presets (overview, by_type, by_extension, \
                         storage, etc.) with descriptions",
                    )
                    .with_mime_type("application/json")
                    .no_annotation(),
                // ── Agent cookbook (query examples) ──────────────────
                RawResource::new("uffs://cookbook", "Query Cookbook")
                    .with_description(
                        "Curated example MCP tool calls organized by workflow — \
                         ready-to-use arguments objects, tips, and multi-step patterns. \
                         Read this first to learn how to compose effective UFFS queries.",
                    )
                    .with_mime_type("application/json")
                    .no_annotation(),
            ],
            next_cursor: None,
            meta: None,
        })
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        self.touch();
        Ok(ListResourceTemplatesResult {
            resource_templates: vec![
                RawResourceTemplate::new("uffs://info/{path}", "File/Directory Info")
                    .with_description(
                        "Full metadata for a file or directory by path. \
                     The {path} parameter is a percent-encoded Windows path \
                     with forward slashes (e.g. C:/Users/me/file.txt).",
                    )
                    .with_mime_type("application/json")
                    .no_annotation(),
            ],
            next_cursor: None,
            meta: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        self.touch();
        self.stats.record_resource_read();
        let uri_str = request.uri.as_str().to_owned();

        // Static schema resources — no daemon connection needed.
        let json = match uri_str.as_str() {
            "uffs://schema/fields" => crate::resources::field_catalog_json(),
            "uffs://schema/search" => crate::resources::search_schema_json(),
            "uffs://schema/aggregate" => crate::resources::aggregate_schema_json(),
            "uffs://presets/aggregate" => crate::resources::aggregate_presets_json(),
            "uffs://cookbook" => crate::resources::cookbook_json(),

            // Live metadata resources — need daemon.
            "uffs://drives" => {
                let mut client = self.client().await?;
                let resp = client
                    .drives()
                    .await
                    .map_err(|err| McpError::internal_error(format!("drives: {err}"), None))?;
                drop(client);
                serde_json::to_string_pretty(&resp)
                    .map_err(|err| McpError::internal_error(err.to_string(), None))?
            }
            "uffs://status" => {
                let mut client = self.client().await?;
                let resp = client
                    .status()
                    .await
                    .map_err(|err| McpError::internal_error(format!("status: {err}"), None))?;
                drop(client);
                serde_json::to_string_pretty(&resp)
                    .map_err(|err| McpError::internal_error(err.to_string(), None))?
            }
            // Dynamic info resource: uffs://info/{percent-encoded-path}
            _ if uri_str.starts_with("uffs://info/") => {
                let info_prefix_len = "uffs://info/".len();
                let encoded_path = uri_str.get(info_prefix_len..).unwrap_or_default();
                let decoded_path = percent_decode_path(encoded_path);
                // Normalize URI-style forward slashes back to Windows backslashes.
                let win_path = decoded_path.replace('/', "\\");

                let mut client = self.client().await?;
                let resp = client
                    .info(&win_path)
                    .await
                    .map_err(|err| McpError::internal_error(format!("info: {err}"), None))?;
                drop(client);
                serde_json::to_string_pretty(&resp)
                    .map_err(|err| McpError::internal_error(err.to_string(), None))?
            }

            _ => {
                return Err(McpError::resource_not_found(
                    format!("Unknown resource: {uri_str}"),
                    None,
                ));
            }
        };

        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            json,
            request.uri,
        )]))
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        self.touch();
        Ok(ListPromptsResult {
            prompts: prompt_definitions(),
            next_cursor: None,
            meta: None,
        })
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        self.stats.record_prompt_get();
        let prompt_args = request.arguments.unwrap_or_default();

        let messages = build_prompt_messages(request.name.as_ref(), &prompt_args)?;

        Ok(GetPromptResult::new(messages)
            .with_description(format!("UFFS prompt: {}", request.name)))
    }
}

/// Helper to extract a string argument from the prompt args map.
#[must_use]
pub fn str_arg<'a>(args: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|val| val.as_str())
}

/// Helper to extract a numeric argument from the prompt args map.
#[must_use]
pub fn u64_arg(args: &serde_json::Map<String, Value>, key: &str, default: u64) -> u64 {
    str_arg(args, key)
        .and_then(|raw| raw.parse::<u64>().ok())
        .unwrap_or(default)
}

/// Build a single user-role prompt message.
fn user_msg(text: String) -> Vec<PromptMessage> {
    vec![PromptMessage::new_text(
        rmcp::model::PromptMessageRole::User,
        text,
    )]
}

/// Build the messages for a given prompt name and arguments.
///
/// # Errors
///
/// Returns `McpError::invalid_params` if the prompt name is unknown.
pub fn build_prompt_messages(
    name: &str,
    args: &serde_json::Map<String, Value>,
) -> Result<Vec<PromptMessage>, McpError> {
    match name {
        "find_large_files" => {
            let limit = u64_arg(args, "limit", 50);
            Ok(user_msg(format!(
                "Use the uffs_search tool to find the {limit} largest files. \
                 Use pattern '*', sort by 'size' descending, limit {limit}, \
                 filter 'files'. Show results as a table with name, size, and path."
            )))
        }
        "recent_changes" => {
            let days = u64_arg(args, "days", 1);
            Ok(user_msg(format!(
                "Use the uffs_search tool to find files modified in the last \
                 {days} day(s). Use pattern '*', sort by 'modified' descending, \
                 limit 100. Show results as a table."
            )))
        }
        "find_by_extension" => {
            let ext = str_arg(args, "extension").unwrap_or("txt");
            let limit = u64_arg(args, "limit", 100);
            Ok(user_msg(format!(
                "Use the uffs_search tool to find all *.{ext} files. Use pattern \
                 '*.{ext}', sort by 'modified' descending, limit {limit}. \
                 Show results as a table."
            )))
        }
        "find_duplicates_by_name" => {
            let filename = str_arg(args, "filename").unwrap_or("*");
            Ok(user_msg(format!(
                "Use the uffs_search tool to find all files named '{filename}' \
                 across all drives. This helps identify duplicate files. \
                 Show the full path for each result."
            )))
        }
        "disk_usage_report" => {
            let drive = str_arg(args, "drive").unwrap_or("");
            let scope = if drive.is_empty() {
                String::new()
            } else {
                format!(" Scope to drive {drive}: only.")
            };
            Ok(user_msg(format!(
                "Generate a disk usage report.{scope}\n\n\
                 Step 1: Call uffs_aggregate with preset 'overview' to get total \
                 file count and size.\n\
                 Step 2: Call uffs_aggregate with preset 'by_type' to see breakdown \
                 by file type (documents, images, video, audio, archives, etc.).\n\
                 Step 3: Call uffs_aggregate with preset 'by_extension' (top 20) to \
                 see the most common extensions.\n\
                 Step 4: Call uffs_aggregate with preset 'storage' to see size \
                 distribution buckets.\n\n\
                 Present the results as a clear, structured report with totals, \
                 percentages, and top contributors. Highlight anything unusual."
            )))
        }
        "cleanup_report" => {
            let min_size = u64_arg(args, "min_size_mb", 100);
            Ok(user_msg(format!(
                "Generate a cleanup candidates report.\n\n\
                 Step 1: Call uffs_aggregate with preset 'cleanup' to identify \
                 temporary files, caches, and other cleanup candidates.\n\
                 Step 2: Call uffs_search to find the top 50 largest files \
                 over {min_size}MB, sorted by size descending.\n\
                 Step 3: Call uffs_aggregate with preset 'duplicates' to find \
                 potential duplicate files.\n\n\
                 Present the results as an actionable cleanup report. Show total \
                 reclaimable space, list the biggest space hogs, and highlight \
                 duplicate groups. Be clear about which files are safe to review."
            )))
        }
        "duplicate_investigation" => {
            let ext = str_arg(args, "extension").unwrap_or("");
            let scope = if ext.is_empty() {
                String::new()
            } else {
                format!(" Focus on *.{ext} files.")
            };
            Ok(user_msg(format!(
                "Investigate duplicate files.{scope}\n\n\
                 Step 1: Call uffs_aggregate with preset 'duplicates' to find \
                 groups of files with identical names and sizes.\n\
                 Step 2: For the top 5 largest duplicate groups, show all file \
                 paths using uffs_search.\n\
                 Step 3: Summarise total wasted space and recommend which copies \
                 might be safe to remove (e.g. copies in temp/cache directories).\n\n\
                 Present findings as a structured report with duplicate groups, \
                 file paths, sizes, and total reclaimable space."
            )))
        }
        other => Err(McpError::invalid_params(
            format!("Unknown prompt: {other}"),
            None,
        )),
    }
}

/// Standard JSON Schema format values recognised by AJV and most validators.
/// Non-standard formats (e.g. `uint64`, `uint32`, `uint` emitted by
/// `schemars` for Rust integer types) cause noisy warnings in MCP clients
/// that use AJV (Augment, `OpenCode`, Gemini CLI).
const STANDARD_FORMATS: &[&str] = &[
    // JSON Schema built-in string formats
    "date-time",
    "date",
    "time",
    "duration",
    "email",
    "idn-email",
    "hostname",
    "idn-hostname",
    "ipv4",
    "ipv6",
    "uri",
    "uri-reference",
    "iri",
    "iri-reference",
    "uuid",
    "uri-template",
    "json-pointer",
    "relative-json-pointer",
    "regex",
];

/// Recursively strip non-standard `"format"` values from a JSON Schema tree.
///
/// `schemars` emits `"format": "uint64"` for `u64`, `"format": "uint32"` for
/// `u32`, `"format": "uint"` for `usize`, etc.  These are valid Rust-specific
/// annotations but not standard JSON Schema, and AJV-based clients warn on
/// every one.  This walk removes them while preserving standard formats like
/// `"date-time"` and `"uuid"`.
fn strip_nonstandard_formats(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(fmt)) = map.get("format")
                && !STANDARD_FORMATS.contains(&fmt.as_str())
            {
                map.remove("format");
            }
            for val in map.values_mut() {
                strip_nonstandard_formats(val);
            }
        }
        Value::Array(arr) => {
            for val in arr.iter_mut() {
                strip_nonstandard_formats(val);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// Convert a schemars `Schema` to the rmcp `InputSchema` type
/// (`Arc<Map<String, Value>>`), stripping non-standard format annotations.
fn schema_to_input(schema: schemars::Schema) -> Arc<serde_json::Map<String, Value>> {
    let mut value = serde_json::to_value(schema).unwrap_or_default();
    strip_nonstandard_formats(&mut value);
    if let Value::Object(map) = value {
        Arc::new(map)
    } else {
        Arc::new(serde_json::Map::new())
    }
}

/// Like [`Tool::with_output_schema`] but strips non-standard format values
/// from the generated schema before attaching it.
fn with_clean_output_schema<T: schemars::JsonSchema + 'static>(tool: Tool) -> Tool {
    let schema = schemars::schema_for!(T);
    let mut value = serde_json::to_value(schema).unwrap_or_default();
    strip_nonstandard_formats(&mut value);
    let mut result = tool;
    if let Value::Object(map) = value {
        result.output_schema = Some(Arc::new(map));
    }
    result
}

/// Known tool names — used for early rejection before daemon dispatch.
const KNOWN_TOOLS: &[&str] = &[
    "uffs_search",
    "uffs_drives",
    "uffs_status",
    "uffs_info",
    "uffs_aggregate",
    "uffs_facet_values",
];

/// Check if a tool name is known.
fn is_known_tool(name: &str) -> bool {
    KNOWN_TOOLS.contains(&name)
}

/// Build the static list of tool definitions for `tools/list`.
#[must_use]
pub fn tool_definitions() -> Vec<Tool> {
    use crate::schemas::{DrivesOutput, InfoOutput, StatusOutput};

    let read_only = ToolAnnotations::from_raw(
        None,        // title
        Some(true),  // read_only_hint
        Some(false), // destructive_hint
        Some(true),  // idempotent_hint
        Some(false), // open_world_hint
    );

    // Empty object schema for tools that take no arguments.
    // Uses rmcp's own helper — produces `{"type": "object", "properties": {}}`.
    let empty_schema = rmcp::handler::server::common::schema_for_empty_input();

    vec![
        Tool::new(
            "uffs_search",
            "Search files across all indexed NTFS drives. Supports glob patterns \
             (*.rs), regex (prefix with >), and substring matching. Returns \
             file name, size, modification time, and full path.",
            schema_to_input(schemars::schema_for!(tools::search::SearchArgs)),
        )
        // NOTE: outputSchema removed — structuredContent is disabled for
        // search (see search.rs).  Augment enforces the contract: if you
        // declare an outputSchema you MUST return structuredContent.
        .with_annotations(read_only.clone()),
        with_clean_output_schema::<InfoOutput>(Tool::new(
            "uffs_info",
            "Look up detailed information about a specific file or directory by its \
             full path. Returns size, timestamps, MFT attributes, and parent info.",
            schema_to_input(schemars::schema_for!(tools::info::InfoArgs)),
        ))
        .with_annotations(read_only.clone()),
        with_clean_output_schema::<DrivesOutput>(Tool::new(
            "uffs_drives",
            "List all NTFS drives currently indexed by the UFFS daemon. Returns \
             drive letter, record count, and index source (MFT or cached).",
            Arc::clone(&empty_schema),
        ))
        .with_annotations(read_only.clone()),
        with_clean_output_schema::<StatusOutput>(Tool::new(
            "uffs_status",
            "Get the current health and loading progress of the UFFS daemon. Returns \
             daemon state, uptime, memory usage, connection count, and PID.",
            empty_schema,
        ))
        .with_annotations(read_only.clone()),
        Tool::new(
            "uffs_aggregate",
            "Run server-side aggregations over the file index. Supports presets \
             (overview, by_type, by_extension, storage, cleanup, duplicates) and \
             custom specs. Returns counts, stats, terms, rollups, and histograms.",
            schema_to_input(schemars::schema_for!(tools::aggregate::AggregateArgs)),
        )
        // NOTE: outputSchema removed — structuredContent disabled.
        .with_annotations(read_only.clone()),
        Tool::new(
            "uffs_facet_values",
            "Explore distinct values of a field (extension, type, drive, etc.). \
             Returns top-N values with counts and byte totals. Useful for \
             understanding the composition of files before searching.",
            schema_to_input(schemars::schema_for!(tools::facet_values::FacetValuesArgs)),
        )
        // NOTE: outputSchema removed — structuredContent disabled.
        .with_annotations(read_only),
    ]
}

/// Build the static list of prompt definitions for `prompts/list`.
#[must_use]
pub fn prompt_definitions() -> Vec<rmcp::model::Prompt> {
    use rmcp::model::{Prompt, PromptArgument};

    vec![
        Prompt::new(
            "find_large_files",
            Some("Find the largest files across all drives, sorted by size descending"),
            Some(vec![
                PromptArgument::new("limit")
                    .with_description("Number of results (default: 50)")
                    .with_required(false),
            ]),
        ),
        Prompt::new(
            "recent_changes",
            Some("Find files modified in the last N days"),
            Some(vec![
                PromptArgument::new("days")
                    .with_description("Number of days to look back (default: 1)")
                    .with_required(false),
            ]),
        ),
        Prompt::new(
            "find_by_extension",
            Some("Find all files with a specific extension"),
            Some(vec![
                PromptArgument::new("extension")
                    .with_description("File extension without dot (e.g., 'rs', 'pdf', 'jpg')")
                    .with_required(true),
                PromptArgument::new("limit")
                    .with_description("Number of results (default: 100)")
                    .with_required(false),
            ]),
        ),
        Prompt::new(
            "find_duplicates_by_name",
            Some("Search for files with the same name across all drives"),
            Some(vec![
                PromptArgument::new("filename")
                    .with_description("Exact filename to search for")
                    .with_required(true),
            ]),
        ),
        Prompt::new(
            "disk_usage_report",
            Some("Generate a comprehensive disk usage report with type/extension/size breakdown"),
            Some(vec![
                PromptArgument::new("drive")
                    .with_description("Optional drive letter to scope report (e.g., 'C')")
                    .with_required(false),
            ]),
        ),
        Prompt::new(
            "cleanup_report",
            Some("Identify cleanup candidates: large files, temp files, caches, and duplicates"),
            Some(vec![
                PromptArgument::new("min_size_mb")
                    .with_description("Minimum file size in MB to flag (default: 100)")
                    .with_required(false),
            ]),
        ),
        Prompt::new(
            "duplicate_investigation",
            Some("Investigate duplicate files across all drives with size and path details"),
            Some(vec![
                PromptArgument::new("extension")
                    .with_description("Optional extension filter (e.g., 'pdf', 'jpg')")
                    .with_required(false),
            ]),
        ),
    ]
}

/// Percent-decode a URI path component back to a plain string.
///
/// Handles `%XX` sequences (e.g. `%20` → space, `%5C` → backslash).
pub(crate) fn percent_decode_path(encoded: &str) -> String {
    let mut decoded = Vec::with_capacity(encoded.len());
    let bytes = encoded.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        // Collapse the outer bounds-check and the inner `from_str_radix` into
        // a single `if let` chain to satisfy `collapsible_if`.  We use `.get()`
        // instead of direct indexing to avoid `indexing_slicing` on byte arrays
        // and `string_slice` on the `&str`.
        if bytes.get(idx).copied() == Some(b'%')
            && idx + 2 < bytes.len()
            && let Some(hex_pair) = encoded.get(idx + 1..idx + 3)
            && let Ok(byte) = u8::from_str_radix(hex_pair, 16)
        {
            decoded.push(byte);
            idx += 3;
            continue;
        }
        if let Some(&raw) = bytes.get(idx) {
            decoded.push(raw);
        }
        idx += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    /// Verify that optional/skippable fields are NOT in the `required` array.
    ///
    /// MCP hosts reject `structuredContent` that omits a `required` field.
    /// Fields with `#[serde(skip_serializing_if)]` must use
    /// `#[schemars(default)]` so schemars excludes them from `required`.
    #[test]
    fn output_schema_required_fields_match_serde() {
        use crate::schemas::SearchOutput;
        let settings = schemars::generate::SchemaSettings::draft2020_12();
        let generator = settings.into_generator();
        let schema = generator.into_root_schema_for::<SearchOutput>();
        let json = serde_json::to_string_pretty(&schema).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        let required = val
            .get("required")
            .and_then(|req| req.as_array())
            .unwrap()
            .iter()
            .map(|elem| elem.as_str().unwrap())
            .collect::<Vec<_>>();
        // These are skip_serializing_if — must NOT be required.
        assert!(
            !required.contains(&"warnings"),
            "warnings must not be required"
        );
        assert!(
            !required.contains(&"next_cursor"),
            "next_cursor must not be required"
        );
        // These ARE always present — must be required.
        assert!(required.contains(&"returned"));
        assert!(required.contains(&"rows"));
    }
}
