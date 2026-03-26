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

// Suppress unused crate warnings for deps that will be used as modules are implemented
use dirs_next as _;
use serde as _;
use serde_json as _;
use thiserror as _;
use tokio as _;
use tracing as _;
use uffs_security as _;

pub mod connect;
pub mod error;
pub mod protocol;
pub mod types;
