//! JSON-RPC request handler: dispatches methods to [`IndexManager`].

use uffs_client::protocol::{
    ERR_INVALID_PARAMS, ERR_METHOD_NOT_FOUND, RefreshParams, RpcErrorResponse, RpcRequest,
    RpcResponse, SearchParams,
};

/// Maximum pattern length to prevent regex `DoS` (`S4.4.3`).
const MAX_PATTERN_LENGTH: usize = 4096;

use crate::index::IndexManager;
use crate::lifecycle::LifecycleHandle;

/// Request handler holding shared daemon state.
pub struct RequestHandler {
    /// Shared index manager.
    pub index: alloc::sync::Arc<IndexManager>,
    /// Lifecycle handle for idle timer, shutdown, connections.
    pub lifecycle: LifecycleHandle,
}

impl RequestHandler {
    /// Handle a single JSON-RPC request and return a JSON response string.
    pub async fn handle(&self, req: &RpcRequest) -> String {
        let id = req.id.unwrap_or(0_u64);
        let connections = self.lifecycle.active_connections();

        match req.method.as_str() {
            "search" => self.handle_search(id, req).await,
            "drives" => self.handle_drives(id).await,
            "status" => self.handle_status(id, connections).await,
            "stats" => self.handle_stats(id).await,
            "info" => self.handle_info(id, req).await,
            "refresh" => self.handle_refresh(id, req),
            "keepalive" => self.handle_keepalive(id, req),
            "shutdown" => self.handle_shutdown(id, req),
            _ => serde_json::to_string(&RpcErrorResponse::error(
                Some(id),
                ERR_METHOD_NOT_FOUND,
                &format!("Method not found: {}", req.method),
            ))
            .unwrap_or_default(),
        }
    }

    /// Handle `search` method.
    async fn handle_search(&self, id: u64, req: &RpcRequest) -> String {
        let search_params: SearchParams = match req
            .params
            .as_ref()
            .and_then(|val| serde_json::from_value(val.clone()).ok())
        {
            Some(parsed) => parsed,
            None => {
                return serde_json::to_string(&RpcErrorResponse::error(
                    Some(id),
                    ERR_INVALID_PARAMS,
                    "Missing or invalid search params",
                ))
                .unwrap_or_default();
            }
        };

        // S4.4.3: Reject overly long patterns (regex DoS prevention)
        if search_params.pattern.len() > MAX_PATTERN_LENGTH {
            return serde_json::to_string(&RpcErrorResponse::error(
                Some(id),
                ERR_INVALID_PARAMS,
                &format!(
                    "Pattern too long ({} chars, max {MAX_PATTERN_LENGTH})",
                    search_params.pattern.len()
                ),
            ))
            .unwrap_or_default();
        }

        let mut response = self.index.search(&search_params).await;
        let row_count = response.rows.len();

        // D5.1: adaptive routing — use shmem for large result sets.
        if row_count > uffs_client::shmem::SHMEM_THRESHOLD {
            let t_shmem = std::time::Instant::now();
            match uffs_client::shmem::write_search_results(
                &response.rows,
                response.duration_ms,
                response.records_scanned as u64,
                response.truncated,
            ) {
                Ok(path) => {
                    let shmem_ms = t_shmem.elapsed().as_millis();
                    let count = row_count as u64;
                    let path_str = path.to_string_lossy().into_owned();
                    tracing::info!(
                        rows = row_count,
                        shmem_write_ms = shmem_ms,
                        path = %path_str,
                        "🗂️ shmem: wrote bulk results"
                    );
                    response.shmem_path = Some(path_str);
                    response.shmem_count = Some(count);
                    // Clear inline rows — data is in shmem now.
                    response.rows = Vec::new();
                }
                Err(shmem_err) => {
                    let shmem_ms = t_shmem.elapsed().as_millis();
                    tracing::warn!(
                        error = %shmem_err,
                        rows = row_count,
                        shmem_write_ms = shmem_ms,
                        "shmem write failed; falling back to inline JSON"
                    );
                    // Fall through — send inline (may be slow for very
                    // large result sets, but at least it works).
                }
            }
        }

        let t_serialize = std::time::Instant::now();
        let result = serde_json::to_value(&response).unwrap_or_default();
        let json = serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default();
        let ser_ms = t_serialize.elapsed().as_millis();

        if row_count > 10_000 || ser_ms > 100 {
            tracing::info!(
                rows = row_count,
                serialize_ms = ser_ms,
                json_bytes = json.len(),
                shmem = response.shmem_path.is_some(),
                "🔌 search response serialized"
            );
        }

        json
    }

    /// Handle `stats` method — performance metrics.
    async fn handle_stats(&self, id: u64) -> String {
        let response = self.index.stats().await;
        let result = serde_json::to_value(&response).unwrap_or_default();
        serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
    }

    /// Handle `drives` method.
    async fn handle_drives(&self, id: u64) -> String {
        let response = self.index.drives().await;
        let result = serde_json::to_value(&response).unwrap_or_default();
        serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
    }

    /// Handle `status` method.
    async fn handle_status(&self, id: u64, connections: usize) -> String {
        let response = self.index.status(connections).await;
        let result = serde_json::to_value(&response).unwrap_or_default();
        serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
    }

    /// Handle `info` method — look up a file by path.
    async fn handle_info(&self, id: u64, req: &RpcRequest) -> String {
        let file_path = req
            .params
            .as_ref()
            .and_then(|val| val.get("path"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");

        if file_path.is_empty() {
            return serde_json::to_string(&RpcErrorResponse::error(
                Some(id),
                ERR_INVALID_PARAMS,
                "Missing 'path' parameter",
            ))
            .unwrap_or_default();
        }

        let response = self.index.info(file_path).await;
        let result = serde_json::to_value(&response).unwrap_or_default();
        serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
    }

    /// Handle `refresh` method — spawns refresh in background, returns
    /// immediate ack.
    fn handle_refresh(&self, id: u64, req: &RpcRequest) -> String {
        let refresh_params: RefreshParams = req
            .params
            .as_ref()
            .and_then(|val| serde_json::from_value(val.clone()).ok())
            .unwrap_or_default();

        let idx_clone = alloc::sync::Arc::clone(&self.index);
        tokio::spawn(async move {
            idx_clone.refresh(&refresh_params.drives).await;
        });

        let result = serde_json::json!({"ok": true, "message": "refresh started"});
        serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
    }

    /// Handle `keepalive` method.
    ///
    /// D3.4.3: Also processes optional `session_type` parameter to set
    /// differentiated idle timeout tier.
    fn handle_keepalive(&self, id: u64, req: &RpcRequest) -> String {
        self.lifecycle.reset_idle_timer();

        // D3.4.3: If session_type is provided, update the timeout tier
        if let Some(session_type) = req
            .params
            .as_ref()
            .and_then(|val| val.get("session_type"))
            .and_then(serde_json::Value::as_str)
        {
            self.lifecycle.set_session_type(session_type);
        }

        let result = serde_json::json!({"ok": true});
        serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
    }

    /// Handle `shutdown` method.
    ///
    /// `S4.4.9`: Requires a `nonce` parameter matching the one in the PID file.
    /// This prevents unauthorized shutdown via the socket.
    fn handle_shutdown(&self, id: u64, req: &RpcRequest) -> String {
        let provided_nonce = req
            .params
            .as_ref()
            .and_then(|val| val.get("nonce"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");

        if !self.lifecycle.verify_shutdown_nonce(provided_nonce) {
            return serde_json::to_string(&RpcErrorResponse::error(
                Some(id),
                ERR_INVALID_PARAMS,
                "Invalid or missing shutdown nonce (read from daemon.pid file)",
            ))
            .unwrap_or_default();
        }

        self.lifecycle.request_shutdown();
        let result = serde_json::json!({"ok": true, "message": "shutting down"});
        serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
    }
}
