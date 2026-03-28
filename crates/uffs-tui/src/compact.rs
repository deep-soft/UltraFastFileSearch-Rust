//! Compact in-memory index — re-exported from `uffs-core`.
//!
//! All types and functions now live in `uffs_core::compact`. This module
//! re-exports everything so existing TUI code compiles unchanged.

// Re-export all public types and functions from uffs-core
#[cfg(windows)]
pub use uffs_core::compact::load_live_drive;
pub use uffs_core::compact::{
    ChildrenIndex, DriveCompactIndex, IndexSource, LoadTiming, load_mft_file, refresh_drive,
};
