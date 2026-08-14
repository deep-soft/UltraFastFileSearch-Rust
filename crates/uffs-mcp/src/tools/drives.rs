// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! `uffs_drives` tool — list all indexed NTFS drives.

use core::fmt::Write as _;

use rmcp::model::{CallToolResult, ContentBlock};
use uffs_client::connect::UffsClient;

use crate::error::BridgeError;
use crate::schemas::{DriveOutput, DrivesOutput};

/// Execute the drives tool (no arguments).
///
/// # Errors
///
/// Returns [`BridgeError`] if the daemon call fails.
pub(crate) async fn run(client: &mut UffsClient) -> Result<CallToolResult, BridgeError> {
    let response = client
        .drives()
        .await
        .map_err(|err| BridgeError::Daemon(format!("Failed to list drives: {err}")))?;

    let structured = DrivesOutput {
        count: response.drives.len(),
        drives: response
            .drives
            .iter()
            .map(|drv| DriveOutput {
                letter: drv.letter,
                records: drv.records,
                source: drv.source.clone(),
                tier: Some(super::status::tier_name(drv.tier).to_owned()),
            })
            .collect(),
    };

    let mut output = String::new();
    _ = write!(output, "Loaded {} drive(s):\n\n", response.drives.len());
    for drive in &response.drives {
        // A parked/cold drive reports 0 records because its body is
        // released — say so inline, or the listing reads as an empty
        // index and an agent concludes there is nothing to search.
        let tier = super::status::tier_name(drive.tier);
        let note = match tier {
            "parked" | "cold" => "  ← body released; a query re-warms it (30-120 s)",
            _ => "",
        };
        _ = writeln!(
            output,
            "  {}:  {:>10} records  ({}, {}){}",
            drive.letter, drive.records, drive.source, tier, note
        );
    }

    let mut result = CallToolResult::success(vec![ContentBlock::text(output)]);
    result.structured_content = Some(serde_json::to_value(structured)?);
    Ok(result)
}
