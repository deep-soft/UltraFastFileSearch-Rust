//! UFFS (Ultra Fast File Search) GUI
//!
//! Native graphical user interface for file search.
//! This is a placeholder — the GUI will use `uffs-client` to communicate
//! with the daemon when implemented.

// Suppress unused crate warnings for deps reserved for future GUI implementation
use anyhow as _;
use clap as _;
use tokio as _;
use tracing as _;
use tracing_subscriber as _;
use uffs_client as _;

/// Entry point: prints a placeholder banner and exits.
#[expect(
    clippy::print_stderr,
    reason = "placeholder banner intentionally prints to stderr"
)]
fn main() -> std::process::ExitCode {
    eprintln!("╔══════════════════════════════════════════════════════════════╗");
    eprintln!("║       UFFS (Ultra Fast File Search) GUI - Coming Soon!       ║");
    eprintln!("╠══════════════════════════════════════════════════════════════╣");
    eprintln!("║                                                              ║");
    eprintln!("║  The GUI is not yet implemented.                             ║");
    eprintln!("║                                                              ║");
    eprintln!("║  In the meantime, please use:                                ║");
    eprintln!("║    • uffs      - Command-line interface                      ║");
    eprintln!("║    • uffs_tui  - Terminal user interface                     ║");
    eprintln!("║    • uffs-mcp  - MCP adapter for AI agents                  ║");
    eprintln!("║                                                              ║");
    eprintln!("╚══════════════════════════════════════════════════════════════╝");

    std::process::ExitCode::FAILURE
}
