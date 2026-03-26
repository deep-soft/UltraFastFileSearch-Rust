//! UFFS Access Broker — Windows service for elevated MFT handle brokering.
//!
//! A tiny Windows service that runs elevated and provides read-only NTFS
//! volume handles to the daemon process (which runs as a normal user).
//!
//! # Usage
//!
//! ```bash
//! uffs-broker --install     # Install as Windows Service
//! uffs-broker --uninstall   # Remove Windows Service
//! uffs-broker --start       # Start the service
//! uffs-broker --stop        # Stop the service
//! uffs-broker --run         # Run in foreground (for debugging)
//! ```
//!
//! On non-Windows platforms, this binary prints an error and exits.

// Deps used by broker.rs on Windows only — suppress unused-crate warnings
use anyhow as _;
use uffs_security as _;

mod broker;

fn main() {
    #[cfg(windows)]
    {
        if let Err(run_err) = broker::run() {
            tracing::error!(%run_err, "uffs-broker fatal error");
            std::process::exit(1);
        }
    }

    #[cfg(not(windows))]
    {
        tracing::error!("uffs-broker is a Windows-only component");
        std::process::exit(1);
    }
}
