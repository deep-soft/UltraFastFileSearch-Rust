// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Human-readable text formatting for MCP tool responses.
//!
//! These formatters produce compact summaries alongside structured JSON,
//! giving LLMs context without requiring them to parse raw data.

use core::fmt::Write;

/// Format aggregate results as a compact human-readable summary.
#[must_use]
pub fn format_aggregate_summary(results: &[uffs_client::protocol::AggregateResultWire]) -> String {
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

/// Format a search result row as a markdown table line.
///
/// Columns: `| Name | Ext | Type | Size | Modified | Path |`
#[must_use]
pub fn format_search_row(row: &uffs_client::protocol::response::SearchRow) -> String {
    let ext = match row.name.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
    };
    let kind = if row.is_directory { "dir" } else { "file" };
    format!(
        "| {} | {} | {} | {} | {} | {} |",
        row.name,
        ext,
        kind,
        uffs_client::protocol::response::format_size(row.size),
        uffs_client::protocol::response::format_time(row.modified),
        row.path,
    )
}
