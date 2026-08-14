// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `uffs_status` tool — daemon health and loading progress.

use rmcp::model::{CallToolResult, ContentBlock};
use uffs_client::connect::UffsClient;
use uffs_client::protocol::response::ShardTier;

use crate::error::BridgeError;
use crate::schemas::StatusOutput;

/// Render a shard tier as the lowercase name agents match on.
///
/// `None` (pre-tiering daemon) reads as `warm`: those daemons never
/// demote, so their shards are searchable by construction.
pub(crate) const fn tier_name(tier: Option<ShardTier>) -> &'static str {
    match tier {
        Some(ShardTier::Hot) => "hot",
        None | Some(ShardTier::Warm) => "warm",
        Some(ShardTier::Parked) => "parked",
        Some(ShardTier::Cold) => "cold",
        Some(ShardTier::Evicting) => "evicting",
        Some(ShardTier::Unknown) => "unknown",
    }
}

/// Execute the status tool (no arguments).
///
/// # Errors
///
/// Returns [`BridgeError`] if the daemon call fails.
pub(crate) async fn run(client: &mut UffsClient) -> Result<CallToolResult, BridgeError> {
    let response = client
        .status()
        .await
        .map_err(|err| BridgeError::Daemon(format!("Failed to get status: {err}")))?;

    let status_str = serde_json::to_string_pretty(&response.status)?;

    // Per-drive tiers.  Without these the tool answers "Ready" for a
    // daemon whose every drive is parked — which is how an agent
    // concluded "the daemon is not cold" while staring at seven parked
    // shards, then blamed the query shape.  The lifecycle status and
    // the index's searchability are different questions; this tool has
    // to answer both or it misleads.  Best-effort: a drives-RPC failure
    // degrades to the lifecycle-only view rather than failing the call.
    let drives = client.drives().await.ok().map(|resp| resp.drives);
    let tiers: alloc::collections::BTreeMap<String, String> = drives
        .as_ref()
        .map(|list| {
            list.iter()
                .map(|drv| (drv.letter.to_string(), tier_name(drv.tier).to_owned()))
                .collect()
        })
        .unwrap_or_default();
    let index_ready = drives.as_ref().is_some_and(|list| {
        list.iter()
            .all(|drv| matches!(drv.tier, None | Some(ShardTier::Warm | ShardTier::Hot)))
    });

    // The running `uffsmcp` build version — a read-only freshness signal the
    // agent can surface (UFFS self-updates via `uffs --update`).
    let server_version = env!("CARGO_PKG_VERSION");

    let tier_line = if tiers.is_empty() {
        "Drives: (tier state unavailable)".to_owned()
    } else {
        let rendered: Vec<String> = tiers
            .iter()
            .map(|(letter, tier)| format!("{letter}={tier}"))
            .collect();
        format!("Drives: {}", rendered.join(" "))
    };
    let readiness_line = if index_ready {
        "Index: READY — every drive is warm; queries answer immediately."
    } else {
        "Index: WARMING NEEDED — one or more drives are parked/cold. A query \
         against them triggers a 30-120 s re-warm and returns a retry hint. \
         Poll this tool until index_ready is true."
    };

    let text = format!(
        "Daemon Status: {status_str}\n{readiness_line}\n{tier_line}\nUptime: {}s\nConnections: {}\nPID: {}\nUFFS server version: {server_version}\n",
        response.uptime_secs, response.connections, response.pid
    );

    let structured = StatusOutput {
        status: serde_json::to_value(&response.status)?,
        index_ready,
        drives: tiers,
        uptime_secs: response.uptime_secs,
        connections: response.connections,
        pid: response.pid,
        server_version: server_version.to_owned(),
    };

    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    result.structured_content = Some(serde_json::to_value(structured)?);
    Ok(result)
}
