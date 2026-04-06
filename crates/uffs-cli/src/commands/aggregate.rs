//! Aggregate command implementation.
//!
//! Runs aggregate analytics via the daemon and prints results.

use std::io::Write;

use anyhow::{Context, Result, bail};
use uffs_client::protocol::{
    AggregateResultWire, AggregateSpecWire, SearchParams,
};

use super::{format_number, format_size};

/// Run an aggregate preset or count against the daemon.
///
/// # Errors
///
/// Returns an error if the daemon cannot be reached or the preset
/// is not recognized.
#[expect(
    clippy::single_call_fn,
    reason = "public CLI command handler called from main dispatch"
)]
pub async fn aggregate(preset: &str, format: &str) -> Result<()> {
    // Build the aggregate-only SearchParams.
    let wire_spec = if preset == "count" {
        AggregateSpecWire {
            kind: "count".to_owned(),
            label: Some("total_count".to_owned()),
            field: None,
            top: None,
            interval: None,
            calendar: None,
            boundaries: vec![],
            metrics: vec![],
            preset: None,
        }
    } else {
        // Validate preset name.
        if uffs_core::aggregate::presets::AggregatePreset::parse(preset).is_none() {
            bail!(
                "Unknown preset: `{preset}`. Available presets: {}",
                uffs_core::aggregate::presets::AggregatePreset::ALL_NAMES.join(", ")
            );
        }
        AggregateSpecWire {
            kind: "preset".to_owned(),
            label: None,
            field: None,
            top: None,
            interval: None,
            calendar: None,
            boundaries: vec![],
            metrics: vec![],
            preset: Some(preset.to_owned()),
        }
    };

    let params = SearchParams {
        pattern: "*".to_owned(),
        aggregations: vec![wire_spec],
        include_rows: false,
        ..Default::default()
    };

    // Connect to daemon and run the aggregation.
    let mut client = uffs_client::connect::UffsClient::connect()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to daemon: {e}"))?;

    let response = client
        .search(&params)
        .await
        .map_err(|e| anyhow::anyhow!("Aggregate query failed: {e}"))?;

    // Output results.
    if format == "json" {
        let json = serde_json::to_string_pretty(&response.aggregations)?;
        println!("{json}");
    } else {
        print_table_results(&response.aggregations)?;
    }

    Ok(())
}

/// Print aggregate results in a human-readable table format.
fn print_table_results(results: &[AggregateResultWire]) -> Result<()> {
    let mut stdout = std::io::stdout().lock();

    for result in results {
        let label = result
            .label
            .as_deref()
            .unwrap_or(&result.kind);

        writeln!(stdout, "\n=== {label} ===")?;

        match result.kind.as_str() {
            "count" => {
                if let Some(value) = result.value {
                    writeln!(stdout, "  Total: {}", format_number(value))?;
                }
            }
            "stats" => {
                if let Some(stats) = &result.stats {
                    writeln!(stdout, "  Count:  {}", format_number(stats.count))?;
                    writeln!(stdout, "  Sum:    {}", format_size(stats.sum))?;
                    writeln!(stdout, "  Min:    {}", format_size(stats.min))?;
                    writeln!(stdout, "  Max:    {}", format_size(stats.max))?;
                    writeln!(stdout, "  Avg:    {}", format_size(stats.avg as u64))?;
                    if stats.waste_bytes > 0 {
                        writeln!(stdout, "  Waste:  {} ({:.1}%)", format_size(stats.waste_bytes), stats.waste_pct)?;
                    }
                }
            }
            "buckets" => {
                if result.buckets.is_empty() {
                    writeln!(stdout, "  (no data)")?;
                } else {
                    // Table header.
                    writeln!(stdout, "  {:<30} {:>12} {:>14} {:>8} {:>8}",
                        "Key", "Count", "Total Size", "Count%", "Size%")?;
                    writeln!(stdout, "  {:-<30} {:-<12} {:-<14} {:-<8} {:-<8}",
                        "", "", "", "", "")?;
                    for row in &result.buckets {
                        let share_c = row.share_count.unwrap_or(0.0);
                        let share_b = row.share_bytes.unwrap_or(0.0);
                        writeln!(stdout, "  {:<30} {:>12} {:>14} {:>7.1}% {:>7.1}%",
                            row.key,
                            format_number(row.count),
                            format_size(row.total_bytes),
                            share_c,
                            share_b)?;
                    }
                    if let Some(other) = result.other_count {
                        if other > 0 {
                            writeln!(stdout, "  ... and {} more groups ({} records)",
                                result.total_groups.unwrap_or(0).saturating_sub(result.buckets.len()),
                                format_number(other))?;
                        }
                    }
                }
            }
            "missing" | "distinct" => {
                if let Some(value) = result.value {
                    writeln!(stdout, "  {}: {}", result.kind, format_number(value))?;
                }
            }
            _ => {
                writeln!(stdout, "  (unknown result kind: {})", result.kind)?;
            }
        }
    }

    writeln!(stdout)?;
    Ok(())
}
