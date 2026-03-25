//! TUI Application state — compact-index search.

use std::sync::mpsc;

use ratatui::widgets::TableState;
use ratatui_textarea::TextArea;

use crate::backend::{DisplayRow, FilterMode, MultiDriveBackend, SortColumn};
use crate::compact::{DriveCompactIndex, LoadTiming};
use crate::keys::Keymap;

/// Result type for a single drive refresh (label + result).
type RefreshResult = (String, anyhow::Result<(DriveCompactIndex, LoadTiming)>);

/// Application state.
#[expect(
    clippy::struct_excessive_bools,
    reason = "App state struct — independent toggle flags are clearest as bools"
)]
pub struct App {
    /// Runtime keymap (action → key bindings).
    pub keymap: Keymap,
    /// Search input text area (full editing: cursor, selection, clipboard).
    pub textarea: TextArea<'static>,
    /// Search results (from last search).
    pub results: Vec<DisplayRow>,
    /// Table selection state.
    pub table_state: TableState,
    /// Search backend (multi-drive `MftIndex`).
    pub backend: MultiDriveBackend,
    /// Status message.
    pub status: String,
    /// Error message (if any).
    pub error: Option<String>,
    /// Last search duration in milliseconds.
    pub last_search_ms: u128,
    /// Whether name-only matching is active.
    pub name_only: bool,
    /// Whether case-sensitive search is active (Alt+C toggle).
    pub case_sensitive: bool,
    /// Whether whole-word search is active (Alt+W toggle).
    pub whole_word: bool,
    /// Filter mode: `All`, `FilesOnly`, or `DirsOnly`.
    pub filter_mode: FilterMode,
    /// Whether a refresh is in progress (background thread).
    pub refreshing: bool,
    /// Channel receiver for completed drive refreshes.
    pub refresh_rx: Option<mpsc::Receiver<RefreshResult>>,
    /// Number of drives being refreshed (for progress tracking).
    pub refresh_total: usize,
    /// Number of drives completed so far.
    pub refresh_done: usize,
    /// Channel receiver for auto-refresh timer signals.
    pub auto_refresh_rx: Option<mpsc::Receiver<()>>,
    /// Search history (most recent last).
    pub search_history: Vec<String>,
    /// Current position in search history (`None` = not browsing history).
    pub history_idx: Option<usize>,
    /// Saved current input before browsing history.
    pub history_saved_input: String,
    /// Longest search pattern in the current typing session.
    /// Pushed to history when the search box is cleared.
    pub peak_search: String,
    /// Current help bar page (F1 cycles through pages).
    pub help_page: u8,
    /// Visible page size for PageUp/Down (set by `ui()` on each render).
    pub page_size: usize,
}

impl App {
    /// Get the current search text from the textarea.
    pub fn input_text(&self) -> String {
        self.textarea
            .lines()
            .first()
            .map_or(String::new(), ToOwned::to_owned)
    }

    /// Create a new application with a pre-loaded backend.
    #[expect(
        dead_code,
        reason = "public API for synchronous loading; async loading builds incrementally"
    )]
    pub fn with_backend(backend: MultiDriveBackend) -> Self {
        let drive_info = backend
            .drive_summary()
            .iter()
            .map(|(letter, count)| format!("{letter}:{count}"))
            .collect::<Vec<_>>()
            .join(" ");
        let total = backend.total_records();
        let status = format!("Loaded {total} records [{drive_info}]");

        Self {
            keymap: Keymap::default(),
            textarea: make_search_textarea(),
            results: Vec::new(),
            table_state: TableState::default(),
            backend,
            status,
            error: None,
            last_search_ms: 0,
            name_only: false,
            case_sensitive: false,
            whole_word: false,
            filter_mode: FilterMode::All,
            refreshing: false,
            refresh_rx: None,
            refresh_total: 0,
            refresh_done: 0,
            auto_refresh_rx: None,
            search_history: Vec::new(),
            history_idx: None,
            history_saved_input: String::new(),
            peak_search: String::new(),
            help_page: 0,
            page_size: 20,
        }
    }

    /// Create an empty application with a pre-loaded keymap.
    pub fn with_keymap(keymap: Keymap) -> Self {
        Self {
            keymap,
            textarea: make_search_textarea(),
            results: Vec::new(),
            table_state: TableState::default(),
            backend: MultiDriveBackend::new(),
            status: "No drives loaded. Use --mft-file or --drive to load data.".to_owned(),
            error: None,
            last_search_ms: 0,
            name_only: false,
            case_sensitive: false,
            whole_word: false,
            filter_mode: FilterMode::All,
            refreshing: false,
            refresh_rx: None,
            refresh_total: 0,
            refresh_done: 0,
            auto_refresh_rx: None,
            search_history: Vec::new(),
            history_idx: None,
            history_saved_input: String::new(),
            peak_search: String::new(),
            help_page: 0,
            page_size: 20,
        }
    }

    /// Create an empty application (no drives loaded, default keymap).
    pub fn new() -> Self {
        Self {
            keymap: Keymap::default(),
            textarea: make_search_textarea(),
            results: Vec::new(),
            table_state: TableState::default(),
            backend: MultiDriveBackend::new(),
            status: "No drives loaded. Use --mft-file or --drive to load data.".to_owned(),
            error: None,
            last_search_ms: 0,
            name_only: false,
            case_sensitive: false,
            whole_word: false,
            filter_mode: FilterMode::All,
            refreshing: false,
            refresh_rx: None,
            refresh_total: 0,
            refresh_done: 0,
            auto_refresh_rx: None,
            search_history: Vec::new(),
            history_idx: None,
            history_saved_input: String::new(),
            peak_search: String::new(),
            help_page: 0,
            page_size: 20,
        }
    }

    /// Check if any drives are loaded.
    #[must_use]
    pub fn has_data(&self) -> bool {
        !self.backend.drives.is_empty()
    }

    /// Move selection to next item.
    pub fn next(&mut self) {
        let len = self.results.len();
        if len == 0 {
            return;
        }
        let idx = match self.table_state.selected() {
            Some(current) => {
                if current >= len - 1 {
                    0
                } else {
                    current + 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(idx));
    }

    /// Move selection to previous item.
    pub fn previous(&mut self) {
        let len = self.results.len();
        if len == 0 {
            return;
        }
        let idx = match self.table_state.selected() {
            Some(current) => {
                if current == 0 {
                    len - 1
                } else {
                    current - 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(idx));
    }

    /// Move selection down by one visible page.
    pub fn page_down(&mut self) {
        let len = self.results.len();
        if len == 0 {
            return;
        }
        let current = self.table_state.selected().unwrap_or(0);
        let new_idx = (current + self.page_size).min(len - 1);
        self.table_state.select(Some(new_idx));
    }

    /// Move selection up by one visible page.
    pub fn page_up(&mut self) {
        if self.results.is_empty() {
            return;
        }
        let current = self.table_state.selected().unwrap_or(0);
        let new_idx = current.saturating_sub(self.page_size);
        self.table_state.select(Some(new_idx));
    }

    /// Get the full path of the currently selected result.
    #[must_use]
    pub fn selected_path(&self) -> Option<&str> {
        let idx = self.table_state.selected()?;
        self.results.get(idx).map(|row| row.path.as_str())
    }

    /// Execute search with current input.
    ///
    /// When the search box is empty, searches for `*` (all files) to show
    /// the first 1,000 entries — the TUI always has something visible.
    pub fn search(&mut self) {
        self.error = None;
        let input = self.input_text();

        // Empty search box → show all files (first 1,000)
        // History: track the longest pattern during typing. Save to history
        // only when the box is cleared (user finished their search).
        let pattern = if input.is_empty() {
            // Box just cleared → commit peak_search to history
            if !self.peak_search.is_empty() {
                let peak = core::mem::take(&mut self.peak_search);
                if self.search_history.last().is_none_or(|last| *last != peak) {
                    self.search_history.push(peak.clone());
                    Self::save_history_entry(&peak);
                }
            }
            self.history_idx = None;
            "*".to_owned()
        } else {
            // Track the longest pattern in this typing session
            if input.len() > self.peak_search.len() {
                self.peak_search.clone_from(&input);
            }
            // Don't reset history_idx here — it's managed by
            // history_back/history_forward and reset_history_browsing()
            input
        };

        if !self.has_data() {
            self.error = Some("No drives loaded. Use --mft-file or --drive.".to_owned());
            return;
        }

        // Show working indicator (visible if UI renders before search completes)
        let fc = |n: usize| uffs_mft::format_number_commas(n as u64);
        let sort_label = self.sort_column().label();
        if pattern == "*" {
            self.status = format!(
                "⏳ Scanning {} records — Sort: {sort_label}...",
                fc(self.backend.total_records())
            );
        } else {
            self.status = format!("⏳ Searching for \"{pattern}\"...");
        }

        let result = self
            .backend
            .search(&pattern, self.case_sensitive, self.whole_word);
        self.last_search_ms = result.duration.as_millis();
        self.results = result.rows;
        crate::backend::apply_filter(&mut self.results, self.filter_mode);

        let total_trigrams: usize = self
            .backend
            .drives
            .iter()
            .map(|dr| dr.trigram.posting_count())
            .sum();
        self.status = format!(
            "{} matches  │  {}  │  {} records across {} drives  │  {} trigrams",
            fc(self.results.len()),
            {
                let ms = result.duration.as_millis();
                if ms < 1000 {
                    format!("{ms}ms")
                } else {
                    let tenths = (ms + 50) / 100;
                    let whole = tenths / 10;
                    let frac = tenths % 10;
                    format!("{whole}.{frac}s")
                }
            },
            fc(result.records_scanned),
            self.backend.drives.len(),
            fc(total_trigrams),
        );

        if self.results.is_empty() {
            self.table_state.select(None);
        } else {
            self.table_state.select(Some(0));
        }
    }

    /// Cycle sort column and re-sort results.
    ///
    /// When the search box is empty (match-all), this re-runs the full
    /// global scan so the top-N is correct for the new sort column
    /// (e.g., Tab from Modified to Size → show 1000 biggest, not 1000
    /// newest re-sorted by size).
    pub fn cycle_sort(&mut self) {
        self.backend.cycle_sort();
        if self.input_text().is_empty() {
            self.search(); // re-scan all 25M for new sort column (status set inside)
        } else {
            self.results = self.backend.last_results.clone();
            crate::backend::apply_filter(&mut self.results, self.filter_mode);
        }
    }

    /// Toggle sort direction and re-sort results.
    ///
    /// When the search box is empty (match-all), this re-runs the full
    /// global scan for the reversed direction.
    pub fn toggle_sort_direction(&mut self) {
        self.backend.toggle_sort_direction();
        if self.input_text().is_empty() {
            self.search(); // re-scan all 25M for reversed direction (status set inside)
        } else {
            self.results = self.backend.last_results.clone();
            crate::backend::apply_filter(&mut self.results, self.filter_mode);
        }
    }

    /// Get the current sort column.
    #[must_use]
    pub const fn sort_column(&self) -> SortColumn {
        self.backend.sort_column
    }

    /// Get whether sort is descending.
    #[must_use]
    pub const fn sort_desc(&self) -> bool {
        self.backend.sort_desc
    }

    /// Navigate to the previous search in history (Up arrow).
    ///
    /// First call saves the current input, then walks backward through history.
    pub fn history_back(&mut self) {
        if self.search_history.is_empty() {
            return;
        }
        let new_idx = match self.history_idx {
            None => {
                // First press: save current input and jump to most recent
                self.history_saved_input = self.input_text();
                self.search_history.len() - 1
            }
            Some(idx) => {
                if idx > 0 {
                    idx - 1
                } else {
                    return; // already at oldest
                }
            }
        };
        self.history_idx = Some(new_idx);
        if let Some(entry) = self.search_history.get(new_idx).cloned() {
            self.set_input(&entry);
        }
    }

    /// Navigate to the next search in history (Down arrow).
    ///
    /// At the end of history, restores the saved input from before browsing.
    pub fn history_forward(&mut self) {
        let Some(idx) = self.history_idx else {
            return; // not browsing history
        };
        if idx + 1 < self.search_history.len() {
            let new_idx = idx + 1;
            self.history_idx = Some(new_idx);
            if let Some(entry) = self.search_history.get(new_idx).cloned() {
                self.set_input(&entry);
            }
        } else {
            // Past the end → restore saved input
            self.history_idx = None;
            let saved = self.history_saved_input.clone();
            self.set_input(&saved);
        }
    }

    /// Replace the textarea content with the given string.
    fn set_input(&mut self, text: &str) {
        self.textarea.select_all();
        self.textarea.cut();
        self.textarea.insert_str(text);
    }

    /// Load search history from disk.
    pub fn load_history(&mut self) {
        if let Some(path) = history_file_path() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                self.search_history = content
                    .lines()
                    .filter(|line| !line.is_empty())
                    .map(ToOwned::to_owned)
                    .collect();
            }
        }
    }

    /// Append a single entry to the history file on disk.
    #[expect(
        clippy::single_call_fn,
        reason = "separated for readability; persistence is a distinct concern"
    )]
    fn save_history_entry(entry: &str) {
        use std::io::Write;
        if let Some(path) = history_file_path() {
            if let Some(parent) = path.parent() {
                drop(std::fs::create_dir_all(parent));
            }
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                drop(writeln!(file, "{entry}"));
            }
        }
    }

    /// Toggle name-only matching mode.
    pub const fn toggle_name_only(&mut self) {
        self.name_only = !self.name_only;
    }

    /// Toggle case-sensitive search mode.
    pub const fn toggle_case_sensitive(&mut self) {
        self.case_sensitive = !self.case_sensitive;
    }

    /// Toggle whole-word search mode.
    pub const fn toggle_whole_word(&mut self) {
        self.whole_word = !self.whole_word;
    }

    /// Cycle filter mode: `All` → `FilesOnly` → `DirsOnly` → `All`.
    pub const fn cycle_filter(&mut self) {
        self.filter_mode = match self.filter_mode {
            FilterMode::All => FilterMode::FilesOnly,
            FilterMode::FilesOnly => FilterMode::DirsOnly,
            FilterMode::DirsOnly => FilterMode::All,
        };
    }

    /// Get a display label for the current filter mode.
    #[must_use]
    pub const fn filter_label(&self) -> &str {
        match self.filter_mode {
            FilterMode::All => "",
            FilterMode::FilesOnly => " [FILES]",
            FilterMode::DirsOnly => " [DIRS]",
        }
    }
}

/// Path to the persistent search history file.
///
/// Uses the platform-appropriate config directory:
/// - macOS: `~/Library/Application Support/uffs/search_history.txt`
/// - Windows: `%APPDATA%\uffs\search_history.txt`
/// - Linux: `~/.config/uffs/search_history.txt`
fn history_file_path() -> Option<std::path::PathBuf> {
    dirs_next::config_dir().map(|config| config.join("uffs").join("search_history.txt"))
}

/// Create a configured single-line `TextArea` for the search box.
fn make_search_textarea<'a>() -> TextArea<'a> {
    use ratatui::style::{Color, Style};

    let mut textarea = TextArea::default();
    textarea.set_cursor_line_style(Style::default());
    textarea.set_style(Style::default().fg(Color::Yellow));
    textarea.set_placeholder_text("Type to search...");
    textarea.set_block(ratatui::widgets::Block::default());
    textarea
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_navigation() {
        let mut app = App::new();
        app.results = vec![
            DisplayRow {
                drive: 'C',
                path: "C:\\a".to_owned(),
                name: "a".to_owned(),
                size: 0,
                is_directory: false,
                modified: 0,
            },
            DisplayRow {
                drive: 'C',
                path: "C:\\b".to_owned(),
                name: "b".to_owned(),
                size: 0,
                is_directory: false,
                modified: 0,
            },
            DisplayRow {
                drive: 'C',
                path: "C:\\c".to_owned(),
                name: "c".to_owned(),
                size: 0,
                is_directory: true,
                modified: 0,
            },
        ];

        app.next();
        assert_eq!(app.table_state.selected(), Some(0));

        app.next();
        assert_eq!(app.table_state.selected(), Some(1));

        app.previous();
        assert_eq!(app.table_state.selected(), Some(0));
    }

    #[test]
    fn test_search_without_data() {
        let mut app = App::new();
        app.textarea.insert_str("test");
        app.search();
        assert!(app.error.is_some());
        assert!(app.results.is_empty());
    }

    #[test]
    fn test_has_data() {
        let app = App::new();
        assert!(!app.has_data());
    }

    #[test]
    fn test_empty_search_shows_all() {
        let mut app = App::new();
        app.results = vec![DisplayRow {
            drive: 'C',
            path: "C:\\x".to_owned(),
            name: "x".to_owned(),
            size: 0,
            is_directory: false,
            modified: 0,
        }];
        // textarea starts empty → searches for "*" (all files)
        // With no drives loaded, this triggers the "no drives" error
        app.search();
        assert!(app.error.is_some());
    }
}
