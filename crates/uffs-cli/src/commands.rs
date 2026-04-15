// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! CLI command implementations.
//!
//! This module provides the public command surface for the UFFS CLI and shared
//! helpers used by the split command modules.

// indicatif is only used on Windows; retain the dep for cross-compilation.
#[cfg(not(windows))]
use indicatif as _;
#[cfg(windows)]
use indicatif::MultiProgress;
#[cfg(windows)]
use indicatif::{ProgressBar, ProgressStyle};

/// Aggregate analytics subcommand.
pub mod aggregate;
/// Daemon management subcommands.
pub mod daemon_mgmt;
// Index and info subcommands were merged into other modules.
/// MCP server management subcommands.
pub mod mcp_mgmt;
/// Output helpers for search results.
pub mod output;
/// Search command implementation.
pub mod search;
/// Stats subcommand implementation.
pub mod stats;
/// Combined `uffs status` command.
pub mod system_status;

/// Check if progress bars are disabled via `UFFS_NO_PROGRESS=1` environment
/// variable.
#[cfg(windows)]
#[inline]
fn is_progress_disabled() -> bool {
    std::env::var("UFFS_NO_PROGRESS")
        .map(|val| val == "1" || val.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Create a multi-progress container for multiple drives.
/// Returns `None` if progress is disabled via `UFFS_NO_PROGRESS=1`.
#[cfg(windows)]
fn create_multi_progress() -> Option<MultiProgress> {
    if is_progress_disabled() {
        None
    } else {
        Some(MultiProgress::new())
    }
}

/// Create a progress bar for a specific drive.
#[cfg(windows)]
fn add_drive_progress(mp: &MultiProgress, drive: char) -> ProgressBar {
    let pb = mp.add(ProgressBar::new(0));
    pb.set_style(
        ProgressStyle::with_template(
            "[{elapsed_precise}] {bar:40.cyan/blue} {bytes:>7}/{total_bytes:7} {msg}",
        )
        .unwrap_or_else(|_| ProgressStyle::default_bar())
        .progress_chars("=>-"),
    );
    pb.set_message(format!("Reading {drive}:\\$MFT"));
    pb
}

/// Format a number with comma separators.
fn format_number(num: u64) -> String {
    let num_str = num.to_string();
    let mut result = String::with_capacity(num_str.len() + num_str.len() / 3);
    for (idx, ch) in num_str.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}

/// Format file size in human-readable format.
#[expect(
    clippy::float_arithmetic,
    reason = "division for human-readable size formatting"
)]
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    // Precision loss acceptable — display-only formatting where ±1 byte is fine.
    #[expect(
        clippy::cast_precision_loss,
        reason = "display-only human-readable formatting"
    )]
    let bytes_f64 = bytes as f64;
    if bytes >= TB {
        #[expect(
            clippy::cast_precision_loss,
            reason = "display-only human-readable formatting"
        )]
        let divisor = TB as f64;
        format!("{:.2} TB", bytes_f64 / divisor)
    } else if bytes >= GB {
        #[expect(
            clippy::cast_precision_loss,
            reason = "display-only human-readable formatting"
        )]
        let divisor = GB as f64;
        format!("{:.2} GB", bytes_f64 / divisor)
    } else if bytes >= MB {
        #[expect(
            clippy::cast_precision_loss,
            reason = "display-only human-readable formatting"
        )]
        let divisor = MB as f64;
        format!("{:.2} MB", bytes_f64 / divisor)
    } else if bytes >= KB {
        #[expect(
            clippy::cast_precision_loss,
            reason = "display-only human-readable formatting"
        )]
        let divisor = KB as f64;
        format!("{:.2} KB", bytes_f64 / divisor)
    } else {
        format!("{bytes} B")
    }
}
