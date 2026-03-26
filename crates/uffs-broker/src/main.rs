//! UFFS Access Broker — Windows service for elevated MFT handle brokering.
//!
//! This is a tiny Windows service that runs elevated (via UAC or as a
//! Windows Service) and provides read-only volume handles to the daemon
//! process running as a normal user.
//!
//! On non-Windows platforms, this binary prints an error and exits.

// Suppress unused crate warnings for deps used on Windows only
use anyhow as _;
use tracing as _;
use uffs_security as _;

fn main() {
    #[cfg(windows)]
    {
        eprintln!("uffs-broker: not yet implemented (D7/S5 planned)");
        std::process::exit(1);
    }

    #[cfg(not(windows))]
    {
        eprintln!("uffs-broker is a Windows-only component");
        std::process::exit(1);
    }
}
