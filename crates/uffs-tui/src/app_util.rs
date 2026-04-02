//! Small utility helpers for the TUI `App`.
//!
//! Extracted from `app.rs` for file-size policy compliance.

use ratatui_textarea::TextArea;

/// Copy text to the system clipboard using platform-native tools.
///
/// - **macOS**: `pbcopy`
/// - **Windows**: `clip.exe`
/// - **Linux**: `xclip -selection clipboard` (falls back to `xsel --clipboard`)
#[expect(
    clippy::single_call_fn,
    reason = "30-line clipboard helper kept as named function for readability"
)]
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let (program, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("pbcopy", &[])
    } else if cfg!(target_os = "windows") {
        ("clip", &[])
    } else {
        ("xclip", &["-selection", "clipboard"])
    };

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("{program}: {err}"))?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|err| format!("write to {program}: {err}"))?;
    }

    child
        .wait()
        .map_err(|err| format!("{program} wait: {err}"))?;

    Ok(())
}

/// Create a configured single-line `TextArea` for the search box.
pub fn make_search_textarea<'a>() -> TextArea<'a> {
    use ratatui::style::{Color, Style};

    let mut textarea = TextArea::default();
    textarea.set_cursor_line_style(Style::default());
    textarea.set_style(Style::default().fg(Color::Yellow));
    textarea.set_placeholder_text("Type to search...");
    textarea.set_block(ratatui::widgets::Block::default());
    textarea
}

// ── App methods extracted for file-size policy ────────────────────────────

use crate::app::{App, DataSource};

impl App {
    /// Build a platform-aware CLI command string from the current search state.
    ///
    /// On Windows: `uffs.exe "pattern" --flags...`
    /// On macOS/Linux: `cargo run --release --bin uffs -- "pattern" --data-dir
    /// ... --flags...`
    #[must_use]
    pub fn build_cli_command(&self) -> String {
        let input = self.input_text();
        let pattern = if input.is_empty() { "*" } else { &input };

        // Build the search state from current toggles
        let mut state = self.active_search_state.clone();
        state.case_sensitive = self.case_sensitive;
        state.whole_word = self.whole_word;
        state.name_only = self.name_only;
        state.hide_system = self.search_filters.hide_system;
        state.filter = self.filter_mode;
        state.limit = self.result_limit;
        if let Some(sort_str) = {
            let sort = crate::backend::format_sort_spec(
                self.backend.sort_column,
                self.backend.sort_desc,
                &self.backend.extra_sort_tiers,
            );
            if sort.is_empty() { None } else { Some(sort) }
        } {
            state.sort = Some(sort_str);
        }

        // Get the base CLI command (always uses "uffs.exe" prefix)
        let base_cli = crate::history::search_state_to_cli(pattern, &state);

        // Strip the "uffs.exe" prefix — we'll replace it with the platform command
        let flags = base_cli.strip_prefix("uffs.exe ").unwrap_or(&base_cli);

        // Build data source arguments
        let data_args = match &self.data_source {
            DataSource::None => String::new(),
            DataSource::DataDir(dir) => format!(" --data-dir {}", dir.display()),
            DataSource::MftFiles(files) => {
                let paths: Vec<String> =
                    files.iter().map(|fp| format!("{}", fp.display())).collect();
                format!(" --mft-file {}", paths.join(","))
            }
        };

        if cfg!(windows) {
            format!("uffs.exe {flags}{data_args}")
        } else {
            format!("cargo run --release --bin uffs -- {flags}{data_args}")
        }
    }

    /// Copy the CLI command to the system clipboard and update the status bar.
    pub fn copy_cli_to_clipboard(&mut self) {
        let command = self.build_cli_command();
        match copy_to_clipboard(&command) {
            Ok(()) => {
                self.status = format!("📋 Copied: {command}");
            }
            Err(err) => {
                self.status = format!("❌ Clipboard error: {err}");
            }
        }
    }
}
