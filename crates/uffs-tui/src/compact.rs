//! Compact in-memory index — re-exported from `uffs-core`.
//!
//! All types and functions now live in `uffs_core::compact`. This module
//! re-exports everything so existing TUI code compiles unchanged.

// Re-export all public types and functions from uffs-core
pub(crate) use uffs_core::compact::{
    ChildrenIndex, DriveCompactIndex, IndexSource, LoadTiming, refresh_drive,
};
