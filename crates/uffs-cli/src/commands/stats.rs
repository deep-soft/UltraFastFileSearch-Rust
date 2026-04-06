//! Stats command implementation.
//!
//! Supports two modes:
//! - **Daemon mode** (no path): runs the `overview` aggregate preset via the
//!   daemon, producing a rich summary with counts, size stats, and breakdowns.
//! - **Parquet mode** (path given): legacy path that loads a parquet index file
//!   and computes basic stats with Polars queries.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

use super::format_size;

/// Show statistics about files.
///
/// When `path` is `None`, connects to the daemon and runs the `overview`
/// aggregate preset. When `path` is `Some`, loads a parquet index file for
/// legacy stats.
///
/// # Errors
///
/// Returns an error if the daemon or index cannot be reached, or I/O fails.
pub async fn stats(path: Option<&Path>, top: u32) -> Result<()> {
    match path {
        None => stats_via_daemon().await,
        Some(dir) => stats_from_parquet(dir, top),
    }
}

/// Run `overview` preset aggregate via the daemon and print the results.
async fn stats_via_daemon() -> Result<()> {
    use uffs_client::protocol::{AggregateSpecWire, SearchParams};

    let params = SearchParams {
        pattern: "*".to_owned(),
        aggregations: vec![AggregateSpecWire {
            kind: "preset".to_owned(),
            label: None,
            field: None,
            top: None,
            interval: None,
            calendar: None,
            boundaries: vec![],
            metrics: vec![],
            preset: Some("overview".to_owned()),
        }],
        include_rows: false,
        ..Default::default()
    };

    let mut client = uffs_client::connect::UffsClient::connect()
        .await
        .map_err(|err| anyhow::anyhow!("Failed to connect to daemon: {err}"))?;

    let response = client
        .search(&params)
        .await
        .map_err(|err| anyhow::anyhow!("Stats query failed: {err}"))?;

    super::aggregate::print_table_results(&response.aggregations)
}

/// Legacy parquet-based stats.
fn stats_from_parquet(path: &Path, top: u32) -> Result<()> {
    use uffs_core::MftQuery;
    use uffs_mft::MftReader;

    let df = MftReader::load_parquet(path)
        .with_context(|| format!("Failed to load index: {}", path.display()))?;

    let total_records = df.height();
    let files = MftQuery::new(df.clone()).files_only().collect()?;
    let dirs = MftQuery::new(df.clone()).directories_only().collect()?;

    let file_count = files.height();
    let dir_count = dirs.height();
    let file_size_col = files.column("size")?.u64()?;
    let total_size: u64 = file_size_col.into_iter().flatten().sum();

    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "=== Index Statistics ===")?;
    writeln!(stdout)?;
    writeln!(stdout, "Total records: {total_records}")?;
    writeln!(stdout, "Files:         {file_count}")?;
    writeln!(stdout, "Directories:   {dir_count}")?;
    writeln!(stdout, "Total size:    {}", format_size(total_size))?;
    writeln!(stdout)?;

    writeln!(stdout, "=== Top {top} Largest Files ===")?;
    writeln!(stdout)?;

    let largest = MftQuery::new(df)
        .files_only()
        .sort_by_size(true)
        .limit(top)
        .collect()?;

    let name_col = largest.column("name")?.str()?;
    let largest_size_col = largest.column("size")?.u64()?;

    for idx in 0..largest.height() {
        let name = name_col.get(idx).unwrap_or("<unknown>");
        let size = largest_size_col.get(idx).unwrap_or(0);
        writeln!(stdout, "  {:>12}  {}", format_size(size), name)?;
    }

    Ok(())
}
