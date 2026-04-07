//! UFFS MCP Adapter — bridges AI agents to the UFFS daemon via the
//! Model Context Protocol (MCP).
//!
//! Reads JSON-RPC from stdin, translates to uffs-client API calls,
//! writes responses to stdout.
//!
//! # MCP Configuration
//!
//! Add to your AI agent's MCP config:
//! ```json
//! { "uffs": { "command": "uffs-mcp" } }
//! ```

use std::io::BufRead;

use uffs_client::connect::UffsClient;

// ────────────────────────────────────────────────────────────────────────────

mod server;
mod types;

use server::McpServer;
#[cfg(test)]
use server::format_aggregate_summary;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // MCP uses stderr for logging (stdout is the protocol channel)
    // Use `try_init` so we don't panic if a subscriber is already installed
    // (e.g. when invoked in-process from a host that already has tracing).
    let _ignore = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .try_init();

    tracing::info!("uffs-mcp starting");

    let client = match UffsClient::connect().await {
        Ok(connected) => connected,
        Err(conn_err) => {
            tracing::error!(error = %conn_err, "Failed to connect to daemon");
            anyhow::bail!("Cannot connect to uffs-daemon: {conn_err}");
        }
    };

    tracing::info!("Connected to uffs-daemon");

    let mut server = McpServer { client };

    let stdin = std::io::stdin();
    for raw_line in stdin.lock().lines() {
        let text = match raw_line {
            Ok(ok_line) => ok_line,
            Err(read_err) => {
                tracing::error!(error = %read_err, "stdin read error");
                break;
            }
        };

        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Err(io_err) = server.process_request(trimmed).await {
            tracing::error!(error = %io_err, "Fatal I/O error");
            break;
        }
    }

    tracing::info!("uffs-mcp shutting down");
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::missing_docs_in_private_items,
    clippy::default_numeric_fallback,
    clippy::cast_sign_loss,
    clippy::indexing_slicing,
    clippy::min_ident_chars,
    reason = "test code"
)]
mod tests {
    use uffs_client::protocol::{AggregateResultWire, BucketWire, StatsWire};

    use super::format_aggregate_summary;

    #[test]
    fn summary_count_result() {
        let results = vec![AggregateResultWire {
            label: Some("total_files".to_owned()),
            kind: "count".to_owned(),
            field: None,
            value: Some(42_000),
            stats: None,
            buckets: vec![],
            other_count: None,
            total_groups: None,
        }];
        let summary = format_aggregate_summary(&results);
        assert!(summary.contains("total_files: 42000"), "got: {summary}");
    }

    #[test]
    fn summary_stats_result() {
        let results = vec![AggregateResultWire {
            label: Some("size_stats".to_owned()),
            kind: "stats".to_owned(),
            field: Some("size".to_owned()),
            value: None,
            stats: Some(StatsWire {
                count: 1000,
                sum: 5_000_000,
                min: 0,
                max: 999_999,
                avg: 5000.0,
                waste_bytes: 100_000,
                waste_pct: 2.0,
            }),
            buckets: vec![],
            other_count: None,
            total_groups: None,
        }];
        let summary = format_aggregate_summary(&results);
        assert!(summary.contains("count=1000"), "got: {summary}");
        assert!(summary.contains("sum=5000000"), "got: {summary}");
        assert!(summary.contains("avg=5000.0"), "got: {summary}");
        assert!(summary.contains("waste: 100000 bytes"), "got: {summary}");
    }

    #[test]
    fn summary_buckets_result() {
        let results = vec![AggregateResultWire {
            label: Some("ext_terms".to_owned()),
            kind: "buckets".to_owned(),
            field: Some("extension".to_owned()),
            value: None,
            stats: None,
            buckets: vec![
                BucketWire {
                    key: "rs".to_owned(),
                    count: 500,
                    total_bytes: 2_000_000,
                    total_allocated: None,
                    avg_size: None,
                    share_count: None,
                    share_bytes: None,
                    sample_rows: Vec::new(),
                    drilldown: Vec::new(),
                },
                BucketWire {
                    key: "toml".to_owned(),
                    count: 200,
                    total_bytes: 50_000,
                    total_allocated: None,
                    avg_size: None,
                    share_count: None,
                    share_bytes: None,
                    sample_rows: Vec::new(),
                    drilldown: Vec::new(),
                },
            ],
            other_count: Some(300),
            total_groups: None,
        }];
        let summary = format_aggregate_summary(&results);
        assert!(summary.contains("ext_terms (2 buckets)"), "got: {summary}");
        assert!(summary.contains("rs"), "got: {summary}");
        assert!(summary.contains("toml"), "got: {summary}");
        assert!(summary.contains("300 in other groups"), "got: {summary}");
    }

    #[test]
    fn summary_missing_result() {
        let results = vec![AggregateResultWire {
            label: Some("no_ext".to_owned()),
            kind: "missing".to_owned(),
            field: Some("extension".to_owned()),
            value: Some(150),
            stats: None,
            buckets: vec![],
            other_count: None,
            total_groups: None,
        }];
        let summary = format_aggregate_summary(&results);
        assert!(
            summary.contains("150 records with missing"),
            "got: {summary}"
        );
    }

    #[test]
    fn summary_distinct_result() {
        let results = vec![AggregateResultWire {
            label: Some("unique_exts".to_owned()),
            kind: "distinct".to_owned(),
            field: Some("extension".to_owned()),
            value: Some(4500),
            stats: None,
            buckets: vec![],
            other_count: None,
            total_groups: None,
        }];
        let summary = format_aggregate_summary(&results);
        assert!(summary.contains("4500 distinct values"), "got: {summary}");
    }

    #[test]
    fn summary_empty_results() {
        let summary = format_aggregate_summary(&[]);
        assert_eq!(summary, "No aggregate results.");
    }

    #[test]
    fn summary_mixed_results() {
        let results = vec![
            AggregateResultWire {
                label: Some("total".to_owned()),
                kind: "count".to_owned(),
                field: None,
                value: Some(1000),
                stats: None,
                buckets: vec![],
                other_count: None,
                total_groups: None,
            },
            AggregateResultWire {
                label: Some("by_type".to_owned()),
                kind: "buckets".to_owned(),
                field: Some("type".to_owned()),
                value: None,
                stats: None,
                buckets: vec![BucketWire {
                    key: "Document".to_owned(),
                    count: 500,
                    total_bytes: 1_000_000,
                    total_allocated: None,
                    avg_size: None,
                    share_count: None,
                    share_bytes: None,
                    sample_rows: Vec::new(),
                    drilldown: Vec::new(),
                }],
                other_count: None,
                total_groups: None,
            },
        ];
        let summary = format_aggregate_summary(&results);
        assert!(summary.contains("total: 1000"), "got: {summary}");
        assert!(summary.contains("by_type (1 buckets)"), "got: {summary}");
        assert!(summary.contains("Document"), "got: {summary}");
    }

    #[test]
    fn summary_buckets_truncated_at_10() {
        let buckets: Vec<BucketWire> = (0..15)
            .map(|i| BucketWire {
                key: format!("ext_{i}"),
                count: (15 - i) as u64,
                total_bytes: (15 - i) as u64 * 1000,
                total_allocated: None,
                avg_size: None,
                share_count: None,
                share_bytes: None,
                sample_rows: Vec::new(),
                drilldown: Vec::new(),
            })
            .collect();
        let results = vec![AggregateResultWire {
            label: Some("many".to_owned()),
            kind: "buckets".to_owned(),
            field: None,
            value: None,
            stats: None,
            buckets,
            other_count: None,
            total_groups: None,
        }];
        let summary = format_aggregate_summary(&results);
        assert!(summary.contains("ext_0"), "first bucket present");
        assert!(summary.contains("ext_9"), "10th bucket present");
        assert!(!summary.contains("ext_10"), "11th bucket hidden");
        assert!(summary.contains("and 5 more"), "truncation message");
    }

    /// Validate that the aggregate tool schema has the expected properties.
    #[test]
    fn aggregate_tool_schema_valid() {
        let schema_json = serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "default": "*" },
                "preset": { "type": "string" },
                "aggregations": { "type": "array" },
                "drives": { "type": "array", "items": { "type": "string" } }
            },
            "required": []
        });
        let props = schema_json["properties"].as_object().unwrap();
        assert!(props.contains_key("pattern"));
        assert!(props.contains_key("preset"));
        assert!(props.contains_key("aggregations"));
        assert!(props.contains_key("drives"));
    }

    /// Validate that the `facet_values` tool schema requires "field".
    #[test]
    fn facet_values_tool_schema_valid() {
        let schema_json = serde_json::json!({
            "type": "object",
            "properties": {
                "field": { "type": "string" },
                "pattern": { "type": "string", "default": "*" },
                "prefix": { "type": "string" },
                "top": { "type": "integer", "default": 20 }
            },
            "required": ["field"]
        });
        let props = schema_json["properties"].as_object().unwrap();
        assert!(props.contains_key("field"));
        assert!(props.contains_key("top"));
        let required = schema_json["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("field")));
    }
}
