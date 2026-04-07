// CLI aggregate formatter: tabular output with terse loop vars, println for
// user-facing output, and controlled precision casts for display.
#![allow(
    clippy::min_ident_chars,
    clippy::print_stdout,
    clippy::redundant_pub_crate,
    clippy::default_numeric_fallback,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::collapsible_if,
    reason = "CLI display code: terse loop vars, stdout output, display casts"
)]

//! Aggregate command implementation.
//!
//! Runs aggregate analytics via the daemon and prints results.

use std::io::Write;

use anyhow::Result;
use uffs_client::protocol::AggregateResultWire;

use super::{format_number, format_size};

/// Print aggregate results in a human-readable table format.
pub(crate) fn print_table_results(results: &[AggregateResultWire]) -> Result<()> {
    let mut stdout = std::io::stdout().lock();

    for result in results {
        let label = result.label.as_deref().unwrap_or(&result.kind);

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
                        writeln!(
                            stdout,
                            "  Waste:  {} ({:.1}%)",
                            format_size(stats.waste_bytes),
                            stats.waste_pct
                        )?;
                    }
                }
            }
            "buckets" | "rollup" | "duplicates" => {
                if result.buckets.is_empty() {
                    writeln!(stdout, "  (no data)")?;
                } else {
                    // Table header.
                    writeln!(
                        stdout,
                        "  {:<30} {:>12} {:>14} {:>8} {:>8}",
                        "Key", "Count", "Total Size", "Count%", "Size%"
                    )?;
                    writeln!(
                        stdout,
                        "  {:-<30} {:-<12} {:-<14} {:-<8} {:-<8}",
                        "", "", "", "", ""
                    )?;
                    for row in &result.buckets {
                        let share_c = row.share_count.unwrap_or(0.0);
                        let share_b = row.share_bytes.unwrap_or(0.0);
                        writeln!(
                            stdout,
                            "  {:<30} {:>12} {:>14} {:>7.1}% {:>7.1}%",
                            row.key,
                            format_number(row.count),
                            format_size(row.total_bytes),
                            share_c,
                            share_b
                        )?;
                        // Print sample rows (top-hits) indented under the bucket.
                        for sr in &row.sample_rows {
                            let name = sr.fields.get("name").map_or("?", |s| s.as_str());
                            let size = sr
                                .fields
                                .get("size")
                                .and_then(|s| s.parse::<u64>().ok())
                                .map_or_else(String::new, |n| format!(" ({})", format_size(n)));
                            let modified = sr
                                .fields
                                .get("modified")
                                .map_or(String::new(), |s| format!(" mod:{s}"));
                            writeln!(stdout, "    → {name}{size}{modified}")?;
                        }
                    }
                    if let Some(other) = result.other_count {
                        if other > 0 {
                            writeln!(
                                stdout,
                                "  ... and {} more groups ({} records)",
                                result
                                    .total_groups
                                    .unwrap_or(0)
                                    .saturating_sub(result.buckets.len()),
                                format_number(other)
                            )?;
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

/// Print aggregate results in CSV/TSV format.
pub(crate) fn print_csv_results(results: &[AggregateResultWire], tsv: bool) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    let sep = if tsv { '\t' } else { ',' };

    for result in results {
        let label = result.label.as_deref().unwrap_or(&result.kind);

        match result.kind.as_str() {
            "count" => {
                writeln!(stdout, "# {label}")?;
                writeln!(stdout, "count")?;
                if let Some(v) = result.value {
                    writeln!(stdout, "{v}")?;
                }
            }
            "stats" => {
                if let Some(stats) = &result.stats {
                    writeln!(stdout, "# {label}")?;
                    writeln!(
                        stdout,
                        "count{sep}sum{sep}min{sep}max{sep}avg{sep}waste_bytes{sep}waste_pct"
                    )?;
                    writeln!(
                        stdout,
                        "{}{sep}{}{sep}{}{sep}{}{sep}{:.2}{sep}{}{sep}{:.2}",
                        stats.count,
                        stats.sum,
                        stats.min,
                        stats.max,
                        stats.avg,
                        stats.waste_bytes,
                        stats.waste_pct
                    )?;
                }
            }
            "buckets" | "rollup" | "duplicates" => {
                // Check if any bucket has sample rows or drilldown.
                let has_samples = result.buckets.iter().any(|r| !r.sample_rows.is_empty());
                let has_drill = result.buckets.iter().any(|r| !r.drilldown.is_empty());

                writeln!(stdout, "# {label}")?;
                write!(
                    stdout,
                    "key{sep}count{sep}total_bytes{sep}total_allocated{sep}avg_size{sep}share_count{sep}share_bytes"
                )?;
                if has_samples {
                    write!(stdout, "{sep}samples")?;
                }
                if has_drill {
                    write!(stdout, "{sep}drilldown")?;
                }
                writeln!(stdout)?;

                for row in &result.buckets {
                    write!(
                        stdout,
                        "{}{sep}{}{sep}{}{sep}{}{sep}{:.2}{sep}{:.2}{sep}{:.2}",
                        row.key,
                        row.count,
                        row.total_bytes,
                        row.total_allocated.unwrap_or(0),
                        row.avg_size.unwrap_or(0.0),
                        row.share_count.unwrap_or(0.0),
                        row.share_bytes.unwrap_or(0.0),
                    )?;
                    if has_samples {
                        let json = serde_json::to_string(&row.sample_rows)
                            .unwrap_or_else(|_| "[]".to_owned());
                        write!(stdout, "{sep}{json}")?;
                    }
                    if has_drill {
                        let json = serde_json::to_string(&row.drilldown)
                            .unwrap_or_else(|_| "[]".to_owned());
                        write!(stdout, "{sep}{json}")?;
                    }
                    writeln!(stdout)?;
                }
            }
            "missing" | "distinct" => {
                writeln!(stdout, "# {label}")?;
                writeln!(stdout, "value")?;
                if let Some(v) = result.value {
                    writeln!(stdout, "{v}")?;
                }
            }
            _ => {}
        }
        writeln!(stdout)?;
    }

    Ok(())
}
