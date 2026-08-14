// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `uffs_status` tool — daemon health and loading progress.

use rmcp::model::{CallToolResult, ContentBlock};
use uffs_client::connect::UffsClient;
use uffs_client::protocol::response::ShardTier;

use crate::error::BridgeError;
use crate::schemas::StatusOutput;

/// Thousands-separate a count so `0` versus `24,988,343` is legible at
/// a glance — the difference between a parked and a warm index.
fn commas(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (idx, ch) in digits.chars().enumerate() {
        if idx > 0 && (digits.len() - idx).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

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

    // Residency corroboration: records + heap.  Both read 0 while every
    // drive is parked, which is the same fact the tier map states — but
    // an agent that distrusts one number has the other to check it
    // against, and "0 records / 0 MB" is unmistakable.
    let total_records = client.stats().await.map_or(0, |stats| stats.total_records);
    let index_heap_mb = response.index_heap_bytes.map(|bytes| bytes / (1024 * 1024));

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

    let heap_str = index_heap_mb.map_or_else(|| "n/a".to_owned(), |mb| format!("{mb} MB"));
    let text = format!(
        "Daemon Status: {status_str}\n{readiness_line}\n{tier_line}\n\
         Resident: {} records, index heap {heap_str}\n\
         Uptime: {}s (process uptime — NOT how long the index has been warm)\n\
         Connections: {}\nPID: {}\nUFFS server version: {server_version}\n",
        commas(total_records),
        response.uptime_secs,
        response.connections,
        response.pid
    );

    let structured = StatusOutput {
        status: serde_json::to_value(&response.status)?,
        index_ready,
        drives: tiers,
        total_records,
        index_heap_mb,
        uptime_secs: response.uptime_secs,
        connections: response.connections,
        pid: response.pid,
        server_version: server_version.to_owned(),
    };

    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    result.structured_content = Some(serde_json::to_value(structured)?);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use uffs_client::protocol::response::ShardTier;

    use super::{commas, tier_name};

    /// The exact conflation that caused a field misdiagnosis: a daemon
    /// whose lifecycle reads `Ready` while every drive is parked. The
    /// tier names must say `parked` so `index_ready` can be false.
    #[test]
    fn parked_tiers_are_named_parked_not_ready() {
        assert_eq!(tier_name(Some(ShardTier::Parked)), "parked");
        assert_eq!(tier_name(Some(ShardTier::Cold)), "cold");
        assert_eq!(tier_name(Some(ShardTier::Warm)), "warm");
        assert_eq!(tier_name(Some(ShardTier::Hot)), "hot");
    }

    /// A pre-tiering daemon never demotes, so absent tier reads warm —
    /// otherwise every query against an old daemon would gate forever.
    #[test]
    fn absent_tier_reads_warm() {
        assert_eq!(tier_name(None), "warm");
    }

    /// `0` vs `24,988,343` is the parked/warm tell; it has to be
    /// readable at a glance in the text block.
    #[test]
    fn record_counts_are_thousands_separated() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1_000), "1,000");
        assert_eq!(commas(24_988_343), "24,988,343");
    }
}
