// Enable unstable Windows Unix domain socket support (available since
// Windows 10 1803+).  This is a nightly-only feature gate.
#![cfg_attr(windows, feature(windows_unix_domain_sockets))]

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

pub mod connect;
pub mod daemon_ctl;
pub mod error;
pub mod protocol;
pub mod shmem;
pub mod types;
pub mod verify;
