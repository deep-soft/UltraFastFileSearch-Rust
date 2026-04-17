// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Thin client library for the UFFS daemon.
//!
//! All surfaces (CLI, TUI, GUI, MCP) use this crate to communicate with
//! the daemon. It handles auto-start, connection, keepalive, and reconnect.
//!
//! # Example
//!
//! ```rust,ignore
//! let client = UffsClient::connect().await?;
//! let results = client.search("*.rs").await?;
//! let drives = client.drives().await?;
//! ```

// Suppress unused crate warnings for deps used in sub-modules
use serde as _;
use uffs_security as _;

/// Async `UffsClient` over tokio — used by the MCP gateway and daemon.
///
/// Gated behind the `async` feature so the sync CLI can drop tokio (and
/// `ws2_32.dll`) from its binary.
#[cfg(feature = "async")]
pub mod connect;
pub mod connect_sync;
pub mod daemon_ctl;
pub mod error;
pub mod format;
pub mod mcp_pid;
pub mod protocol;
pub mod shmem;
pub mod types;
pub mod verify;

pub mod schema;
