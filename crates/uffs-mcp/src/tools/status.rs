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
fn commas(value: u64) -> String {
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

/// Describe the daemon **process** state without ever using the word
/// "ready".
///
/// The daemon's lifecycle enum has a `Ready` variant meaning "the
/// process is up and accepting connections".  Serialised verbatim it
/// put `"state": "ready"` in the payload beside `index_ready: false` —
/// two fields answering what looks like the same question with
/// different answers, and a model skimming for a readiness signal finds
/// the wrong one first.  That is not hypothetical: it is the exact
/// misread that produced a confident "the daemon is not cold" against
/// seven parked shards.
///
/// So the process state is rendered as `running` / `loading` /
/// `refreshing`, which is what it actually means, and the only "ready"
/// left anywhere in the payload is `index_ready` — the field that
/// really answers it.
fn daemon_state_label(status: &uffs_client::protocol::response::DaemonStatus) -> String {
    use uffs_client::protocol::response::DaemonStatus;
    match status {
        DaemonStatus::Ready => "running".to_owned(),
        DaemonStatus::Loading {
            drives_loaded,
            drives_total,
        } => format!("loading ({drives_loaded}/{drives_total} drives)"),
        DaemonStatus::Refreshing { drives } => {
            let list: Vec<String> = drives.iter().map(ToString::to_string).collect();
            format!("refreshing ({})", list.join(", "))
        }
    }
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

/// `Drives: C=warm D=cold …`, or a note that tiers are unavailable.
fn render_tier_line(tiers: &alloc::collections::BTreeMap<String, String>) -> String {
    if tiers.is_empty() {
        return "Drives: (tier state unavailable)".to_owned();
    }
    let rendered: Vec<String> = tiers
        .iter()
        .map(|(letter, tier)| format!("{letter}={tier}"))
        .collect();
    format!("Drives: {}", rendered.join(" "))
}

/// The one line that says whether a query will answer or warm.
const fn readiness_line(index_ready: bool) -> &'static str {
    if index_ready {
        "Index: READY — every drive is warm; queries answer immediately."
    } else {
        "Index: WARMING NEEDED — one or more drives are parked/cold. A query \
         against them triggers a 30-120 s re-warm and returns a retry hint. \
         Poll this tool until index_ready is true."
    }
}

/// Name the drive being paged in, so a plateau reads as "S is loading"
/// rather than "hung" — the difference between waiting and giving up.
fn render_loading_line(loading_now: &[String]) -> String {
    if loading_now.is_empty() {
        return String::new();
    }
    format!(
        "\nLoading now: {} (counts plateau until it lands)",
        loading_now.join(", ")
    )
}

/// Progress against the expected total — shown only mid-warm, where at
/// 100 % it is noise and without a denominator it would be a guess.
fn render_progress(pct: Option<u8>, expected: Option<u64>) -> String {
    match (pct, expected) {
        (Some(percent), Some(total)) if percent < 100 => {
            format!(" of {} expected ({percent}% warmed)", commas(total))
        }
        _ => String::new(),
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

    let daemon_process = daemon_state_label(&response.status);

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
    let loading_now: Vec<String> = drives
        .as_ref()
        .map(|list| {
            list.iter()
                .filter(|drv| drv.loading == Some(true))
                .map(|drv| drv.letter.to_string())
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

    // Re-warm progress against a real denominator: the records each
    // drive held when last resident, summed.  Deliberately record-based
    // — drive-count progress reads 57 % when only 24 % of the records
    // are in, because the drives still cold are the big ones.
    let records_when_warm: Option<u64> = drives.as_ref().and_then(|list| {
        let expected: u64 = list
            .iter()
            .map(|drv| drv.records_when_warm.unwrap_or(0))
            .sum();
        (expected > 0).then_some(expected)
    });
    let warming_progress_pct = records_when_warm.map(|expected| {
        let loaded = u64::try_from(total_records).unwrap_or(u64::MAX);
        let pct = loaded
            .saturating_mul(100)
            .checked_div(expected)
            .unwrap_or(0);
        u8::try_from(pct.min(100)).unwrap_or(100)
    });

    let tier_line = render_tier_line(&tiers);
    let readiness_line = readiness_line(index_ready);
    let loading_line = render_loading_line(&loading_now);
    let heap_str = index_heap_mb.map_or_else(|| "n/a".to_owned(), |mb| format!("{mb} MB"));
    let progress_str = render_progress(warming_progress_pct, records_when_warm);

    let text = format!(
        "Daemon process: {daemon_process}\n{readiness_line}\n{tier_line}{loading_line}\n\
         Resident: {} records{progress_str}, index heap {heap_str}\n\
         Uptime: {}s (process uptime — NOT how long the index has been warm)\n\
         Connections: {}\nPID: {}\nUFFS server version: {server_version}\n",
        commas(u64::try_from(total_records).unwrap_or(u64::MAX)),
        response.uptime_secs,
        response.connections,
        response.pid
    );

    let structured = StatusOutput {
        daemon_process: daemon_process.clone(),
        index_ready,
        drives: tiers,
        total_records,
        index_heap_mb,
        currently_loading: loading_now.clone(),
        records_when_warm,
        warming_progress_pct,
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
        assert_eq!(commas(0_u64), "0");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1_000), "1,000");
        assert_eq!(commas(24_988_343), "24,988,343");
    }
}
