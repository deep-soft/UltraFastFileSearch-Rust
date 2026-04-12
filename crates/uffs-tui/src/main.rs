// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! UFFS (Ultra Fast File Search) TUI
//!
//! Interactive terminal user interface for file search.
//!
//! ## Usage
//!
//! ```bash
//! # Load MFT files (cross-platform)
//! uffs_tui --mft-file C_mft.iocp --drive C
//! uffs_tui --mft-file C.iocp,D.iocp
//!
//! # Windows: auto-detect NTFS drives (future)
//! uffs_tui
//! ```
//!
//! ## Logging
//!
//! Use `-v` / `--verbose` for info-level terminal output.
//! - `RUST_LOG`: Terminal log level (default: `error`, or `info` with `-v`)
//! - `RUST_LOG_FILE`: File log level (default: `info`)
//! - `UFFS_LOG_DIR`: Log directory (default: `~/bin/uffs/logs`)

#![expect(
    unused_crate_dependencies,
    reason = "tokio is a transitive runtime dependency not directly referenced"
)]
#![expect(
    clippy::option_if_let_else,
    reason = "if-let chains clearer for loading with error handling"
)]

use std::io;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tracing_appender::non_blocking::NonBlocking;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::time::UtcTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::Registry;
use tracing_subscriber::{EnvFilter, Layer};

/// Application state and search logic.
mod app;
/// Small utility helpers for the TUI `App` (clipboard, textarea factory).
mod app_util;
/// Search backend: compact-index multi-drive search.
pub(crate) mod backend;
/// Daemon client backend for IPC-based search (D6).
mod client_backend;
/// Compact in-memory index (72 bytes/record, replaces full MftIndex).
mod columns;
mod compact;
mod filters;
/// On-demand full record lookup from `.uffs` cache files.
mod full_record;
/// Search history: entry type, file format, CLI command roundtrip.
mod history;
/// Centralized keybinding definitions.
mod keys;
/// Drive refresh and loading helpers.
mod refresh;
/// Search functions for compact-index drives.
mod search;
/// Tree-based path search, glob matching, and path resolution.
mod tree;
/// TUI rendering — layout, table, help bar, and text highlighting.
mod ui;

use app::{App, Focus};
use keys::Action;

/// UFFS (Ultra Fast File Search) Terminal UI
#[derive(Parser)]
#[command(name = "uffs_tui")]
#[command(
    author,
    version,
    about = "Terminal UI for UFFS (Ultra Fast File Search)"
)]
struct Cli {
    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    /// MFT file(s) to load — supports raw, IOCP capture, and compressed
    ///
    /// Cross-platform: works on macOS, Linux, and Windows.
    /// Auto-detects format. Drive letter inferred from filename.
    ///
    /// Examples:
    ///   `uffs_tui` `D_mft.iocp`
    ///   `uffs_tui` `C.iocp` `D.iocp`
    ///   `uffs_tui` `/path/to/C_mft.bin` `--drive` C
    #[arg(value_name = "FILE")]
    mft_file: Vec<PathBuf>,

    /// Data directory containing `drive_*` subdirectories with MFT files
    ///
    /// Auto-discovers all MFT files in `drive_c/`, `drive_d/`, etc.
    /// Example: `uffs_tui --data-dir ~/uffs_data`
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Drive letter(s) to override auto-detection from filenames.
    #[arg(long, value_delimiter = ',')]
    drive: Vec<char>,

    /// Bypass cache and read MFT fresh (default: use cache + USN updates)
    #[arg(long)]
    no_cache: bool,

    /// Auto-refresh interval in seconds (0 = disabled, default: 60)
    ///
    /// Reloads all drives in the background every N seconds to pick up
    /// file changes. Uses `.uffs` cache + USN journal on Windows.
    #[arg(long, default_value = "60")]
    refresh_interval: u64,

    /// Keybinding preset — overwrites the config file with this preset.
    ///
    /// Available presets: `windows` (default), `emacs`.
    /// The config file is at the platform config directory
    /// (e.g., `~/.config/uffs/keys.toml` on Linux).
    /// After switching, you can hand-edit the file for further customization.
    #[arg(long)]
    keys: Option<String>,

    /// Reset search history to the built-in defaults.
    ///
    /// Overwrites the history file with the pre-populated example searches
    /// that ship with UFFS. Any user-added history entries will be lost.
    #[arg(long)]
    reset_history: bool,
}

/// Initialize logging with terminal + file support.
///
/// If `verbose` is true and `RUST_LOG` is not set, uses `info` level for
/// terminal. Otherwise, terminal logging is controlled by `RUST_LOG` (default:
/// `error`). File logging is controlled by `RUST_LOG_FILE` (default: `info`).
#[expect(
    clippy::single_call_fn,
    reason = "separated from main for readability; logging setup is a distinct concern"
)]
fn init_logging(verbose: bool) -> tracing_appender::non_blocking::WorkerGuard {
    use std::fs;

    // Get log directory (default: ~/bin/uffs/logs)
    let log_dir = std::env::var("UFFS_LOG_DIR").map_or_else(
        |_| {
            dirs_next::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("bin")
                .join("uffs")
                .join("logs")
        },
        PathBuf::from,
    );

    // Create log directory if it doesn't exist
    drop(fs::create_dir_all(&log_dir));

    // Create rolling file appender (daily rotation).
    // Use the builder API which returns Result instead of panicking, and retry
    // briefly to handle transient Windows file-lock races (e.g. previous process
    // still releasing the log file handle).
    let max_attempts = 4_u32;
    let mut file_log_err: Option<String> = None;
    let mut file_log_attempt = 0_u32;
    let (non_blocking, guard): (NonBlocking, _) = {
        let mut last_err = None;
        let mut appender = None;
        for attempt in 0..max_attempts {
            if attempt > 0 {
                std::thread::sleep(core::time::Duration::from_millis(250));
            }
            match RollingFileAppender::builder()
                .rotation(Rotation::DAILY)
                .filename_prefix("uffs_tui_log_")
                .build(&log_dir)
            {
                Ok(file_appender) => {
                    file_log_attempt = attempt;
                    appender = Some(file_appender);
                    break;
                }
                Err(init_err) => last_err = Some(init_err),
            }
        }
        appender.map_or_else(
            || {
                file_log_err = Some(
                    last_err
                        .as_ref()
                        .map_or_else(|| "unknown error".to_owned(), ToString::to_string),
                );
                NonBlocking::new(io::sink())
            },
            NonBlocking::new,
        )
    };

    // Terminal filter: -v sets info if RUST_LOG not explicitly set
    // Note: TUI uses stderr for logging to avoid interfering with the UI
    let terminal_default = if verbose { "info" } else { "error" };
    let terminal_filter =
        EnvFilter::new(std::env::var("RUST_LOG").unwrap_or_else(|_| terminal_default.to_owned()));

    // File filter (default: info)
    let file_filter =
        EnvFilter::new(std::env::var("RUST_LOG_FILE").unwrap_or_else(|_| "info".to_owned()));

    // Timer format
    let timer = UtcTime::rfc_3339();

    // Terminal layer (to stderr to avoid TUI interference, with ANSI colors,
    // file/line info)
    let terminal_layer = tracing_subscriber::fmt::layer()
        .with_writer(io::stderr)
        .with_timer(timer.clone())
        .with_ansi(true)
        .with_file(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .with_target(true)
        .with_filter(terminal_filter);

    // File layer (no ANSI colors, but with full context)
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_timer(timer)
        .with_ansi(false)
        .with_file(true)
        .with_line_number(true)
        .with_thread_ids(true)
        .with_target(true)
        .with_filter(file_filter);

    // Combine layers
    let subscriber = Registry::default().with(terminal_layer).with(file_layer);

    #[expect(
        clippy::expect_used,
        reason = "global subscriber must be set once at startup; failure is unrecoverable"
    )]
    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set global tracing subscriber");

    // Post-init diagnostics: surface file-appender issues through tracing now
    // that the subscriber is active.
    if let Some(err_msg) = &file_log_err {
        tracing::error!(
            log_dir = %log_dir.display(),
            attempts = max_attempts,
            error = %err_msg,
            "File logging DISABLED — log file could not be opened after all retries. \
             All tracing output is terminal-only for this session."
        );
    } else if file_log_attempt > 0 {
        tracing::warn!(
            log_dir = %log_dir.display(),
            retries = file_log_attempt,
            "Log file opened after {file_log_attempt} retries — \
             previous process may have been slow to release the file handle"
        );
    }

    guard
}

/// Build arguments forwarded to `uffs-daemon` when auto-starting.
///
/// On Windows this returns an empty list — the daemon auto-discovers
/// live NTFS drives.  On Mac/Linux this includes `--mft-file` paths.
#[expect(
    clippy::single_call_fn,
    reason = "extracted from main to reduce cognitive complexity"
)]
fn build_daemon_spawn_args(
    mft_files: &[PathBuf],
    data_dir: Option<&std::path::Path>,
    no_cache: bool,
) -> Vec<String> {
    if cfg!(windows) {
        return Vec::new();
    }
    let mut args = Vec::new();
    // Forward --data-dir raw — daemon resolves it internally.
    if let Some(dir) = data_dir {
        args.push("--data-dir".to_owned());
        args.push(dir.to_string_lossy().into_owned());
    }
    for mft_path in mft_files {
        args.push("--mft-file".to_owned());
        args.push(mft_path.to_string_lossy().into_owned());
    }
    if no_cache {
        args.push("--no-cache".to_owned());
    }
    args
}

/// Connect to the UFFS daemon and attach the backend to the app.
///
/// On success, sets `app.daemon_backend` and triggers an initial search.
/// On failure, sets `app.error` with a diagnostic message.
#[expect(
    clippy::single_call_fn,
    reason = "extracted from main to reduce cognitive complexity"
)]
fn init_daemon_backend(app: &mut App, spawn_args: Vec<String>, no_local_data: bool) {
    // Non-Windows without data sources: fail fast — no point starting a daemon.
    if !cfg!(windows) && no_local_data {
        app.error = Some(
            "No MFT data source specified.\n\
             Use --data-dir <path> or --mft-file <path> to provide MFT files."
                .to_owned(),
        );
        "⚠ No data — provide --data-dir or --mft-file".clone_into(&mut app.status);
        return;
    }

    let mut daemon_backend = client_backend::DaemonBackend::new(spawn_args);
    match daemon_backend.connect() {
        Ok(()) => {
            daemon_backend.set_session_tui();
            app.daemon_backend = Some(daemon_backend);
            "🔌 Connected to daemon — type to search".clone_into(&mut app.status);
            app.search();
        }
        Err(err) => {
            app.error = Some(format!("Failed to connect to daemon: {err}"));
            "⚠ Daemon unavailable".clone_into(&mut app.status);
        }
    }
}

fn main() -> Result<()> {
    // Check for -v/--verbose flag early
    let verbose = std::env::args().any(|arg| arg == "-v" || arg == "--verbose");

    // Initialize logging with terminal + file support
    let _guard = init_logging(verbose);

    let cli = Cli::parse();

    // Handle --reset-history: reset the file, then continue launching the TUI
    if cli.reset_history {
        history::reset_history();
        tracing::info!("Search history reset to built-in defaults");
        if let Some(path) = history::history_file_path() {
            tracing::info!(path = %path.display(), "History file location");
        }
    }

    let mft_files = cli.mft_file;

    // Setup terminal immediately so the TUI is visible during loading
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let ratatui_backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(ratatui_backend)?;

    // Load keybindings from config file (or create default)
    let (keymap, keys_msg) = keys::load_or_create_keymap(cli.keys.as_deref());
    tracing::info!("{keys_msg}");

    // Create app and load search history from disk
    let mut app = App::with_keymap(keymap);
    app.load_history();

    // Record how MFT data was sourced (for CLI command generation on Enter)
    app.data_source = if let Some(data_dir) = &cli.data_dir {
        app::DataSource::DataDir(data_dir.clone())
    } else if !mft_files.is_empty() {
        app::DataSource::MftFiles(mft_files.clone())
    } else {
        app::DataSource::None
    };

    // ── Connect to daemon ──────────────────────────────────────────────
    let cli_no_cache = cli.no_cache;
    let spawn_args = build_daemon_spawn_args(&mft_files, cli.data_dir.as_deref(), cli_no_cache);
    "🔌 Connecting to UFFS daemon...".clone_into(&mut app.status);
    terminal.draw(|frame| ui::ui(frame, &mut app))?;
    let no_local_data = mft_files.is_empty() && cli.data_dir.is_none();
    init_daemon_backend(&mut app, spawn_args, no_local_data);

    // Spawn auto-refresh timer thread (if interval > 0)
    let refresh_interval = cli.refresh_interval;
    if refresh_interval > 0 && app.has_data() {
        let (timer_tx, timer_rx) = std::sync::mpsc::channel();
        app.auto_refresh_rx = Some(timer_rx);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(core::time::Duration::from_secs(refresh_interval));
                if timer_tx.send(()).is_err() {
                    break; // Receiver dropped (app exited)
                }
            }
        });
    }

    let res = run_app(&mut terminal, &mut app);

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        #[expect(
            clippy::print_stderr,
            reason = "terminal is restored at this point; stderr is appropriate for error reporting"
        )]
        #[expect(
            clippy::use_debug,
            reason = "Debug format provides full error chain for diagnostics"
        )]
        {
            eprintln!("Error: {err:?}");
        }
    }

    Ok(())
}

/// Run the TUI event loop, handling key input and rendering.
#[expect(
    clippy::single_call_fn,
    reason = "separated from main for readability; event loop is a distinct concern"
)]
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "only specific keys are handled; wildcard is idiomatic for key dispatch"
)]
fn run_app<B: ratatui::backend::Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()>
where
    <B as ratatui::backend::Backend>::Error: Send + Sync + 'static,
{
    let mut needs_search = false;

    loop {
        // 0. Poll for background refresh completions
        if app.refreshing {
            refresh::poll_refresh(&mut *app);
        }

        // 0b. Check auto-refresh timer
        if let Some(timer_rx) = &app.auto_refresh_rx
            && timer_rx.try_recv().is_ok()
            && !app.refreshing
        {
            refresh::start_refresh(app);
        }

        // 1. Always render first — input box is always up-to-date
        terminal.draw(|frame| ui::ui(frame, &mut *app))?;

        // 2. If search is pending, drain ALL buffered keystrokes first so the input box
        //    stays responsive even if search is slow.
        if needs_search {
            // Drain any queued keystrokes (non-blocking)
            while event::poll(core::time::Duration::ZERO)? {
                if let Event::Key(key) = event::read()?
                    && key.kind == KeyEventKind::Press
                {
                    if is_exit_key(&app.keymap, key) {
                        return Ok(());
                    }
                    if app.keymap.matches(key, Action::NavDown) {
                        app.next();
                    } else if app.keymap.matches(key, Action::NavUp) {
                        app.previous();
                    } else if app.keymap.matches(key, Action::SortCycle) {
                        app.cycle_sort();
                    } else if app.keymap.matches(key, Action::SortDirection) {
                        app.toggle_sort_direction();
                    } else {
                        app.textarea.input(key);
                    }
                }
            }

            // Re-render with ALL accumulated input BEFORE searching
            terminal.draw(|frame| ui::ui(frame, &mut *app))?;

            // Now search (blocks, but user already sees their typed text)
            app.search();
            needs_search = false;
            continue;
        }

        // 3. Wait for next event (with debounce timeout)
        if event::poll(core::time::Duration::from_millis(200))? {
            let ev = event::read()?;
            match &ev {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if is_exit_key(&app.keymap, *key) {
                        return Ok(());
                    }

                    // ESC toggles focus between SearchBox and Results.
                    let key_ev = *key;
                    if key_ev.code == KeyCode::Esc {
                        app.toggle_focus();
                        continue;
                    }

                    // Global keys — work regardless of focus.
                    if app.keymap.matches(key_ev, Action::HelpCycle) {
                        const HELP_PAGES: u8 = 4;
                        app.help_page = (app.help_page + 1) % HELP_PAGES;
                        continue;
                    } else if app.keymap.matches(key_ev, Action::Refresh) {
                        refresh::start_refresh(app);
                        continue;
                    }

                    // Focus-specific dispatch.
                    match app.focus {
                        Focus::SearchBox => {
                            if handle_search_box_key(app, key_ev, &mut needs_search) {
                                continue;
                            }
                        }
                        Focus::Results => {
                            if handle_results_key(app, key_ev, &mut needs_search) {
                                continue;
                            }
                        }
                    }
                }
                _ => {}
            }

            // Forward unhandled events to textarea only when SearchBox
            // is focused (typing, cursor movement, mouse, etc.)
            if app.focus == Focus::SearchBox {
                let before = app.input_text();
                app.textarea.input(ev);
                let after = app.input_text();
                if before != after {
                    app.history_idx = None;
                    needs_search = true;
                }
            }
        } else if needs_search {
            // Debounce expired — no more typing, run search
            app.search();
            needs_search = false;
        }
    }
}

/// Handle a key event when the **search box** has focus.
///
/// Returns `true` if the key was consumed (caller should `continue`).
#[expect(
    clippy::single_call_fn,
    reason = "focus-specific handler extracted for readability"
)]
fn handle_search_box_key(app: &mut App, key: KeyEvent, needs_search: &mut bool) -> bool {
    // Search toggles
    if app.keymap.matches(key, Action::ToggleNameOnly) {
        app.toggle_name_only();
        *needs_search = true;
    } else if app.keymap.matches(key, Action::ToggleFilter) {
        app.cycle_filter();
        app.search();
    } else if app.keymap.matches(key, Action::ToggleCaseSensitive) {
        app.toggle_case_sensitive();
        app.search();
    } else if app.keymap.matches(key, Action::ToggleWholeWord) {
        app.toggle_whole_word();
        app.search();
    // History — Up/Down browse history when search box is focused
    } else if app.keymap.matches(key, Action::HistoryBack) || key.code == KeyCode::Up {
        app.history_back();
        *needs_search = true;
    } else if app.keymap.matches(key, Action::HistoryForward) || key.code == KeyCode::Down {
        app.history_forward();
        *needs_search = true;
    // Text editing
    } else if app.keymap.matches(key, Action::ClearLine) {
        app.textarea.select_all();
        app.textarea.cut();
        app.search();
    } else if app.keymap.matches(key, Action::Undo) {
        app.textarea.undo();
        *needs_search = true;
    } else if app.keymap.matches(key, Action::Redo) {
        app.textarea.redo();
        *needs_search = true;
    } else if app.keymap.matches(key, Action::SelectAll) {
        app.textarea.select_all();
    } else if app.keymap.matches(key, Action::Copy) {
        app.textarea.copy();
    } else if app.keymap.matches(key, Action::Paste) {
        app.textarea.paste();
        *needs_search = true;
    } else if app.keymap.matches(key, Action::CopyCliCommand) {
        app.copy_cli_to_clipboard();
    // PageUp/PageDown: auto-switch focus to results panel
    } else if app.keymap.matches(key, Action::PageDown) {
        app.focus = Focus::Results;
        app.page_down();
    } else if app.keymap.matches(key, Action::PageUp) {
        app.focus = Focus::Results;
        app.page_up();
    } else {
        return false; // not consumed — let textarea handle it
    }
    true
}

/// Handle a key event when the **results panel** has focus.
///
/// Returns `true` if the key was consumed (caller should `continue`).
#[expect(
    clippy::single_call_fn,
    reason = "focus-specific handler extracted for readability"
)]
fn handle_results_key(app: &mut App, key: KeyEvent, needs_search: &mut bool) -> bool {
    if app.keymap.matches(key, Action::NavDown) || key.code == KeyCode::Down {
        app.next();
    } else if app.keymap.matches(key, Action::NavUp) || key.code == KeyCode::Up {
        app.previous();
    } else if app.keymap.matches(key, Action::PageDown) {
        app.page_down();
    } else if app.keymap.matches(key, Action::PageUp) {
        app.page_up();
    } else if app.keymap.matches(key, Action::ShowPath) {
        if let Some(path) = app.selected_path() {
            app.status = format!("📋 {path}");
        }
    } else if app.keymap.matches(key, Action::SortCycle) {
        app.cycle_sort();
    } else if app.keymap.matches(key, Action::SortDirection) {
        app.toggle_sort_direction();
    // Search toggles also work from results panel
    } else if app.keymap.matches(key, Action::ToggleNameOnly) {
        app.toggle_name_only();
        *needs_search = true;
    } else if app.keymap.matches(key, Action::ToggleFilter) {
        app.cycle_filter();
        app.search();
    } else if app.keymap.matches(key, Action::ToggleCaseSensitive) {
        app.toggle_case_sensitive();
        app.search();
    } else if app.keymap.matches(key, Action::ToggleWholeWord) {
        app.toggle_whole_word();
        app.search();
    } else {
        return false;
    }
    true
}

/// Returns whether the given key event should terminate the TUI.
#[must_use]
fn is_exit_key(keymap: &keys::Keymap, key: KeyEvent) -> bool {
    keymap.matches(key, Action::Quit)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::is_exit_key;
    use crate::keys::Keymap;

    #[test]
    fn test_is_exit_key_accepts_ctrl_q() {
        let keymap = Keymap::default();
        assert!(is_exit_key(
            &keymap,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL,)
        ));
    }

    #[test]
    fn test_is_exit_key_rejects_regular_input() {
        let keymap = Keymap::default();
        // Plain 'q' types the letter, doesn't exit
        assert!(!is_exit_key(
            &keymap,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE,)
        ));
        // Esc goes to textarea, doesn't exit
        assert!(!is_exit_key(
            &keymap,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
        ));
        // Ctrl+C goes to textarea, doesn't exit
        assert!(!is_exit_key(
            &keymap,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL,)
        ));
        assert!(!is_exit_key(
            &keymap,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE,)
        ));
    }
}
