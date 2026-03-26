//! UFFS Daemon — background service holding MFT indices, serving queries
//! via IPC (Unix domain socket / Windows named pipe).
//!
//! # Usage
//!
//! ```bash
//! uffs-daemon                          # default settings
//! uffs-daemon --idle-timeout 300       # retire after 5 min idle
//! uffs-daemon --no-retire              # stay running indefinitely
//! uffs-daemon --log-level debug        # verbose logging
//! ```

// Suppress unused crate warnings for deps that will be used as modules are implemented
use anyhow as _;
use clap as _;
use dirs_next as _;
use serde as _;
use serde_json as _;
use thiserror as _;
use tracing as _;
use tracing_appender as _;
use tracing_subscriber as _;
use uffs_core as _;
use uffs_mft as _;
use uffs_security as _;

mod handler;
mod index;
mod ipc;
mod lifecycle;
mod protocol;

fn main() {
    eprintln!("uffs-daemon: not yet implemented (D2 in progress)");
    std::process::exit(1);
}
