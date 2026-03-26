//! JSON-RPC request handler: dispatches methods to [`IndexManager`].

use std::sync::Arc;

use uffs_client::protocol::{
    ERR_BAD_PATTERN, ERR_INTERNAL, ERR_INVALID_PARAMS, ERR_METHOD_NOT_FOUND, ERR_NOT_READY,
    InfoParams, InfoResponse, KeepaliveParams, RefreshParams, RpcErrorResponse, RpcRequest,
    RpcResponse, SearchParams,
};

use crate::index::IndexManager;
use crate::lifecycle::LifecycleHandle;

/// Handle a single JSON-RPC request and return a JSON response string.
pub async fn handle_request(
    req: &RpcRequest,
    index: &Arc<IndexManager>,
    lifecycle: &LifecycleHandle,
    connections: usize,
) -> String {
    let id = req.id.unwrap_or(0);

    let result = match req.method.as_str() {
        "search" => handle_search(id, req, index).await,
        "drives" => handle_drives(id, index).await,
        "status" => handle_status(id, index, connections).await,
        "refresh" => handle_refresh(id, req, index).await,
        "keepalive" => handle_keepalive(id, lifecycle).await,
        "shutdown" => handle_shutdown(id, lifecycle).await,
        _ => {
            let err = RpcErrorResponse::error(
                Some(id),
                ERR_METHOD_NOT_FOUND,
                &format!("Method not found: {}", req.method),
            );
            serde_json::to_string(&err).unwrap_or_default()
        }
    };

    result
}

/// Handle `search` method.
async fn handle_search(id: u64, req: &RpcRequest, index: &Arc<IndexManager>) -> String {
    let params: SearchParams = match req
        .params
        .as_ref()
        .and_then(|p| serde_json::from_value(p.clone()).ok())
    {
        Some(p) => p,
        None => {
            return serde_json::to_string(&RpcErrorResponse::error(
                Some(id),
                ERR_INVALID_PARAMS,
                "Missing or invalid search params",
            ))
            .unwrap_or_default();
        }
    };

    let response = index.search(&params).await;
    let result = serde_json::to_value(&response).unwrap_or_default();
    serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
}

/// Handle `drives` method.
async fn handle_drives(id: u64, index: &Arc<IndexManager>) -> String {
    let response = index.drives().await;
    let result = serde_json::to_value(&response).unwrap_or_default();
    serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
}

/// Handle `status` method.
async fn handle_status(id: u64, index: &Arc<IndexManager>, connections: usize) -> String {
    let response = index.status(connections).await;
    let result = serde_json::to_value(&response).unwrap_or_default();
    serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
}

/// Handle `refresh` method — spawns refresh in background, returns immediate ack.
async fn handle_refresh(id: u64, req: &RpcRequest, index: &Arc<IndexManager>) -> String {
    let params: RefreshParams = req
        .params
        .as_ref()
        .and_then(|p| serde_json::from_value(p.clone()).ok())
        .unwrap_or_default();

    let index = Arc::clone(index);
    tokio::spawn(async move {
        index.refresh(&params.drives).await;
    });

    let result = serde_json::json!({"ok": true, "message": "refresh started"});
    serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
}

/// Handle `keepalive` method.
async fn handle_keepalive(id: u64, lifecycle: &LifecycleHandle) -> String {
    lifecycle.reset_idle_timer();
    let result = serde_json::json!({"ok": true});
    serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
}

/// Handle `shutdown` method.
async fn handle_shutdown(id: u64, lifecycle: &LifecycleHandle) -> String {
    lifecycle.request_shutdown();
    let result = serde_json::json!({"ok": true, "message": "shutting down"});
    serde_json::to_string(&RpcResponse::success(id, result)).unwrap_or_default()
}
