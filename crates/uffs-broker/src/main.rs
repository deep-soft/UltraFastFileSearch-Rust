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

// Suppress unused crate warnings for deps used on Windows only
use anyhow as _;
use tracing as _;
use uffs_security as _;

mod broker;

fn main() {
    #[cfg(windows)]
    {
        if let Err(e) = broker::run() {
            eprintln!("uffs-broker error: {e}");
            std::process::exit(1);
        }
    }

    #[cfg(not(windows))]
    {
        eprintln!("uffs-broker is a Windows-only component.");
        eprintln!("On macOS/Linux, the daemon reads MFT files directly (no elevation needed).");
        std::process::exit(1);
    }
}
