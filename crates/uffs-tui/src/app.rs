//! TUI Application state — daemon-backed search via IPC.

use std::path::PathBuf;
use std::sync::mpsc;

use ratatui::widgets::TableState;
use ratatui_textarea::TextArea;

use crate::backend::{DisplayRow, FieldId, FilterMode, MultiDriveBackend};
use crate::client_backend::DaemonBackend;
use crate::compact::{DriveCompactIndex, LoadTiming};
use crate::history::{HistoryEntry, SearchState};
use crate::keys::Keymap;

/// How the TUI was told to find MFT data (for CLI command generation).
#[derive(Debug, Clone, Default)]
pub(crate) enum DataSource {
    /// No explicit source — Windows live MFT or nothing loaded.
    #[default]
    None,
    /// `--data-dir <path>` — auto-discovered drive subdirectories.
    DataDir(PathBuf),
    /// `--mft-file <paths>` — explicit MFT file list.
    MftFiles(Vec<PathBuf>),
}

/// Result type for a single drive refresh (label + result).
type RefreshResult = (String, anyhow::Result<(DriveCompactIndex, LoadTiming)>);

/// Which pane currently has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    /// Search box — typing edits the query, ↑/↓ browse history.
    SearchBox,
    /// Results panel — ↑/↓ navigate rows, Enter shows path.
    Results,
}

/// Application state.
#[expect(
    clippy::struct_excessive_bools,
    reason = "App state struct — independent toggle flags are clearest as bools"
)]
pub(crate) struct App {
    /// Which pane currently has keyboard focus.
    pub focus: Focus,
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
    pub search_history: Vec<HistoryEntry>,
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
    /// Override result limit from history entry (`None` = backend default).
    pub result_limit: Option<u32>,
    /// Extended post-search filters from history entry.
    pub search_filters: crate::backend::SearchFilters,
    /// Visible columns and their display order.
    ///
    /// Defaults to [`crate::backend::DEFAULT_COLUMNS`].  Overridden by
    /// `--columns` in a history entry.
    pub visible_columns: Vec<FieldId>,
    /// The full [`SearchState`] from the currently active history entry.
    ///
    /// Used as the base when saving a modified search so that extended
    /// filters (`--attr`, `--min-size`, `--newer`, `--columns`, etc.)
    /// survive even though the TUI has no interactive controls for them yet.
    pub active_search_state: SearchState,
    /// How the TUI was told to find MFT data (for CLI command generation).
    pub data_source: DataSource,
    /// Daemon client backend (IPC).
    pub daemon_backend: Option<DaemonBackend>,
}

impl App {
    /// Toggle focus between `SearchBox` and `Results`.
    pub(crate) const fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::SearchBox => Focus::Results,
            Focus::Results => Focus::SearchBox,
        };
    }

    /// Get the current search text from the textarea.
    pub(crate) fn input_text(&self) -> String {
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
    pub(crate) fn with_backend(backend: MultiDriveBackend) -> Self {
        let drive_info = backend
            .drive_summary()
            .iter()
            .map(|(letter, count)| format!("{letter}:{count}"))
            .collect::<Vec<_>>()
            .join(" ");
        let total = backend.total_records();
        let status = format!("Loaded {total} records [{drive_info}]");

        Self {
            focus: Focus::SearchBox,
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
            result_limit: None,
            search_filters: crate::backend::SearchFilters::default(),
            visible_columns: crate::backend::DEFAULT_COLUMNS.to_vec(),
            active_search_state: SearchState::default(),
            data_source: DataSource::default(),
            daemon_backend: None,
        }
    }

    /// Create an empty application with a pre-loaded keymap.
    pub(crate) fn with_keymap(keymap: Keymap) -> Self {
        Self {
            focus: Focus::SearchBox,
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
            result_limit: None,
            search_filters: crate::backend::SearchFilters::default(),
            visible_columns: crate::backend::DEFAULT_COLUMNS.to_vec(),
            active_search_state: SearchState::default(),
            data_source: DataSource::default(),
            daemon_backend: None,
        }
    }

    /// Create an empty application (no drives loaded, default keymap).
    // allow: single-call in bin target, multi-call in test target (called from unit
    // tests)
    pub(crate) fn new() -> Self {
        Self {
            focus: Focus::SearchBox,
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
            result_limit: None,
            search_filters: crate::backend::SearchFilters::default(),
            visible_columns: crate::backend::DEFAULT_COLUMNS.to_vec(),
            active_search_state: SearchState::default(),
            data_source: DataSource::default(),
            daemon_backend: None,
        }
    }

    /// Check if daemon is connected.
    #[must_use]
    pub(crate) const fn has_data(&self) -> bool {
        self.daemon_backend.is_some()
    }

    /// Move selection to next item.
    pub(crate) const fn next(&mut self) {
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
    pub(crate) const fn previous(&mut self) {
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
    pub(crate) fn page_down(&mut self) {
        let len = self.results.len();
        if len == 0 {
            return;
        }
        let current = self.table_state.selected().unwrap_or(0);
        let new_idx = (current + self.page_size).min(len - 1);
        self.table_state.select(Some(new_idx));
    }

    /// Move selection up by one visible page.
    pub(crate) fn page_up(&mut self) {
        if self.results.is_empty() {
            return;
        }
        let current = self.table_state.selected().unwrap_or(0);
        let new_idx = current.saturating_sub(self.page_size);
        self.table_state.select(Some(new_idx));
    }

    /// Get the full path of the currently selected result.
    #[must_use]
    pub(crate) fn selected_path(&self) -> Option<&str> {
        let idx = self.table_state.selected()?;
        self.results.get(idx).map(|row| row.path.as_str())
    }

    /// Execute search with current input.
    ///
    /// When the search box is empty, searches for `*` (all files) to show
    /// the first 1,000 entries — the TUI always has something visible.
    pub(crate) fn search(&mut self) {
        self.error = None;
        let input = self.input_text();

        // Empty search box → show all files (first 1,000)
        // History: track the longest pattern during typing. Save to history
        // only when the box is cleared (user finished their search).
        let pattern = if input.is_empty() {
            // Box just cleared → commit peak_search to history
            if !self.peak_search.is_empty() {
                let peak = core::mem::take(&mut self.peak_search);
                self.save_history_entry(&peak);
            }
            self.history_idx = None;
            self.reset_search_overrides();
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
        self.status = format!("⏳ Searching for \"{pattern}\"...");

        self.search_via_daemon(&pattern);

        if self.results.is_empty() {
            self.table_state.select(None);
        } else {
            self.table_state.select(Some(0));
        }
    }

    /// Build daemon search parameters from the current TUI state.
    fn build_daemon_params(&self, pattern: &str) -> uffs_client::protocol::SearchParams {
        use uffs_client::protocol::{SearchFilterMode, SearchParams, SearchResponseMode};

        let effective_limit = self.effective_limit(pattern);

        let filter = match self.filter_mode {
            FilterMode::FilesOnly => Some("files".to_owned()),
            FilterMode::DirsOnly => Some("dirs".to_owned()),
            FilterMode::All => None,
        };

        let sort_label = self.sort_column().canonical_name().to_ascii_lowercase();
        let sort = Some(format!(
            "{sort_label}:{}",
            if self.sort_desc() { "desc" } else { "asc" }
        ));

        let projection = self
            .visible_columns
            .iter()
            .map(|column| column.canonical_name().to_owned())
            .collect::<Vec<_>>();
        let mut params = SearchParams {
            pattern: pattern.to_owned(),
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
            match_path: false, // TUI uses name-only by default
            sort,
            sorts: Vec::new(),
            sort_desc: self.sort_desc(),
            limit: effective_limit,
            filter,
            filter_mode: Some(if self.filter_mode == FilterMode::FilesOnly {
                SearchFilterMode::Files
            } else if self.filter_mode == FilterMode::DirsOnly {
                SearchFilterMode::Dirs
            } else {
                SearchFilterMode::All
            }),
            drives: Vec::new(),
            projection,
            response_mode: Some(SearchResponseMode::Rows),
            min_size: self.search_filters.min_size,
            max_size: self.search_filters.max_size,
            min_descendants: self.search_filters.min_descendants,
            max_descendants: self.search_filters.max_descendants,
            newer: None,
            older: None,
            newer_created: None,
            older_created: None,
            newer_accessed: None,
            older_accessed: None,
            attr: None,
            ext: None,
            exclude: None,
            path_contains: None,
            type_filter: None,
            min_bulkiness: None,
            max_bulkiness: None,
            min_name_len: self.search_filters.min_name_len,
            max_name_len: self.search_filters.max_name_len,
            min_path_len: self.search_filters.min_path_len,
            max_path_len: self.search_filters.max_path_len,
            min_allocated: self.search_filters.min_allocated,
            max_allocated: self.search_filters.max_allocated,
            min_treesize: self.search_filters.min_treesize,
            max_treesize: self.search_filters.max_treesize,
            min_tree_allocated: self.search_filters.min_tree_allocated,
            max_tree_allocated: self.search_filters.max_tree_allocated,
            allowed_months: self.search_filters.allowed_months.clone(),
            hide_system: self.search_filters.hide_system,
            hide_ads: self.search_filters.hide_ads,
            profile: false,
            predicates: Vec::new(),
            aggregations: Vec::new(),
            include_rows: true,
            agg_cursor: None,
            agg_page_size: None,
        };
        params.populate_canonical_fields();
        params
    }

    /// Search via daemon IPC.
    fn search_via_daemon(&mut self, pattern: &str) {
        let params = self.build_daemon_params(pattern);

        // `daemon_backend` is `Some` — checked by caller.
        let db = self.daemon_backend.as_mut();

        let fc = |n: usize| uffs_core::format::format_number_commas(n as u64); // usize→u64 lossless on 64-bit

        match db {
            Some(backend) => match backend.search(&params) {
                Ok(result) => {
                    self.last_search_ms = u128::from(result.duration_ms);
                    self.results = result.rows;

                    let ms = result.duration_ms;
                    let time_str = if ms < 1000 {
                        format!("{ms}ms")
                    } else {
                        let tenths = (ms + 50) / 100;
                        let whole = tenths / 10;
                        let frac = tenths % 10;
                        format!("{whole}.{frac}s")
                    };

                    self.status = format!(
                        "🔌 {} matches  │  {}  │  {} records scanned{}",
                        fc(self.results.len()),
                        time_str,
                        fc(result.records_scanned),
                        if result.truncated {
                            "  │  (truncated)"
                        } else {
                            ""
                        },
                    );
                }
                Err(err) => {
                    self.error = Some(format!("Daemon error: {err}"));
                }
            },
            None => {
                self.error = Some("Daemon backend not connected".to_owned());
            }
        }
    }

    /// Compute the effective result limit for a search pattern.
    ///
    /// TUI interactive limit: cap results to keep the UI responsive.
    /// History entries can override via `self.result_limit`.
    fn effective_limit(&self, pattern: &str) -> Option<u32> {
        self.result_limit.or_else(|| {
            if pattern == "*" || pattern.len() > 2 {
                Some(1_000)
            } else {
                Some(200)
            }
        })
    }

    /// Cycle sort column and re-sort results.
    ///
    /// When the search box is empty (match-all), this re-runs the full
    /// global scan so the top-N is correct for the new sort column
    /// (e.g., Tab from Modified to Size → show 1000 biggest, not 1000
    /// newest re-sorted by size).
    pub(crate) fn cycle_sort(&mut self) {
        self.backend.cycle_sort();
        if self.input_text().is_empty() {
            self.search(); // re-scan all 25M for new sort column (status set inside)
        } else {
            self.results = self.backend.last_results.clone();
            crate::backend::apply_filter(&mut self.results, self.filter_mode);
            crate::backend::apply_search_filters(&mut self.results, &self.search_filters);
        }
    }

    /// Toggle sort direction and re-sort results.
    ///
    /// When the search box is empty (match-all), this re-runs the full
    /// global scan for the reversed direction.
    pub(crate) fn toggle_sort_direction(&mut self) {
        self.backend.toggle_sort_direction();
        if self.input_text().is_empty() {
            self.search(); // re-scan all 25M for reversed direction (status set inside)
        } else {
            self.results = self.backend.last_results.clone();
            crate::backend::apply_filter(&mut self.results, self.filter_mode);
            crate::backend::apply_search_filters(&mut self.results, &self.search_filters);
        }
    }

    /// Get the current sort column.
    #[must_use]
    pub(crate) const fn sort_column(&self) -> FieldId {
        self.backend.sort_column
    }

    /// Get whether sort is descending.
    #[must_use]
    pub(crate) const fn sort_desc(&self) -> bool {
        self.backend.sort_desc
    }

    /// Navigate to the previous search in history (Up arrow).
    ///
    /// First call saves the current input, then walks backward through history.
    pub(crate) fn history_back(&mut self) {
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
        self.apply_history_entry(new_idx);
    }

    /// Navigate to the next search in history (Down arrow).
    ///
    /// When not browsing, starts from the oldest entry (ring behaviour).
    /// At the end of history, restores the saved input from before browsing.
    pub(crate) fn history_forward(&mut self) {
        if self.search_history.is_empty() {
            return;
        }
        let new_idx = match self.history_idx {
            None => {
                // First press: save current input and jump to oldest entry
                self.history_saved_input = self.input_text();
                0
            }
            Some(idx) if idx + 1 < self.search_history.len() => idx + 1,
            Some(_) => {
                // Past the end → restore saved input + clear overrides
                self.history_idx = None;
                self.reset_search_overrides();
                let saved = self.history_saved_input.clone();
                self.set_input(&saved);
                return;
            }
        };
        self.history_idx = Some(new_idx);
        self.apply_history_entry(new_idx);
    }

    /// Get the comment for the currently browsed history entry, if any.
    #[must_use]
    pub(crate) fn current_history_comment(&self) -> Option<&str> {
        let idx = self.history_idx?;
        self.search_history.get(idx)?.comment.as_deref()
    }

    /// Apply a history entry at the given index: set pattern + restore all
    /// toggles, sort, limit, and extended filters.
    fn apply_history_entry(&mut self, idx: usize) {
        if let Some(entry) = self.search_history.get(idx).cloned() {
            self.set_input(&entry.pattern);

            // ── toggles ────────────────────────────────────────────
            self.case_sensitive = entry.state.case_sensitive;
            self.whole_word = entry.state.whole_word;
            self.name_only = entry.state.name_only;
            self.filter_mode = entry.state.filter;

            // Smart-case: if enabled and pattern has uppercase, force
            // case-sensitive (mirrors CLI --smart-case behaviour).
            if entry.state.smart_case && entry.pattern.chars().any(char::is_uppercase) {
                self.case_sensitive = true;
            }

            // ── sort (multi-tier) ───────────────────────────────────
            if let Some(sort_str) = &entry.state.sort {
                let specs = crate::backend::parse_sort_spec(sort_str);
                if let Some(primary) = specs.first() {
                    self.backend.sort_column = primary.column;
                    self.backend.sort_desc = primary.descending;
                    self.backend.extra_sort_tiers = specs.get(1..).unwrap_or_default().to_vec();
                }
            } else {
                self.backend.extra_sort_tiers.clear();
            }

            // ── limit ──────────────────────────────────────────────
            self.result_limit = entry.state.limit;

            // ── extended filters ───────────────────────────────────
            self.search_filters = crate::filters::build_search_filters(&entry.state);

            // ── column selection ───────────────────────────────────
            self.visible_columns = entry
                .state
                .columns
                .as_deref()
                .and_then(crate::backend::parse_columns)
                .unwrap_or_else(|| crate::backend::DEFAULT_COLUMNS.to_vec());

            // ── preserve full state for save_history_entry ──────────
            self.active_search_state = entry.state;
        }
    }

    /// Reset extended filters, limit, and column selection to defaults
    /// (called when user clears history browsing or types a new pattern).
    pub(crate) fn reset_search_overrides(&mut self) {
        self.result_limit = None;
        self.search_filters = crate::backend::SearchFilters::default();
        self.backend.extra_sort_tiers.clear();
        self.visible_columns = crate::backend::DEFAULT_COLUMNS.to_vec();
        self.active_search_state = SearchState::default();
    }

    /// Replace the textarea content with the given string.
    fn set_input(&mut self, text: &str) {
        self.textarea.select_all();
        self.textarea.cut();
        self.textarea.insert_str(text);
    }

    /// Load search history from disk (new CLI-command format).
    pub(crate) fn load_history(&mut self) {
        self.search_history = crate::history::load_history();
    }

    /// Capture the current search state and persist a history entry.
    ///
    /// Uses `active_search_state` (set by `apply_history_entry`) as the
    /// base so that extended filters the TUI cannot yet edit interactively
    /// (`--attr`, `--min-size`, `--newer`, `--columns`, etc.) survive
    /// when the user only changes the pattern or toggles.
    fn save_history_entry(&mut self, pattern: &str) {
        // Start from the full state of the active history entry (if any).
        // Override only the fields the TUI can currently change.
        let mut state = self.active_search_state.clone();
        state.case_sensitive = self.case_sensitive;
        state.smart_case = false; // TUI doesn't have a smart-case toggle
        state.whole_word = self.whole_word;
        state.name_only = self.name_only;
        state.hide_system = self.search_filters.hide_system;
        state.filter = self.filter_mode;
        state.limit = self.result_limit;

        // Capture the current sort from the backend (user may have
        // cycled sort with Tab / toggled direction).
        let sort_str = crate::backend::format_sort_spec(
            self.backend.sort_column,
            self.backend.sort_desc,
            &self.backend.extra_sort_tiers,
        );
        state.sort = if sort_str.is_empty() {
            None
        } else {
            Some(sort_str)
        };

        let entry = HistoryEntry {
            comment: None, // user-generated entries have no comment
            pattern: pattern.to_owned(),
            state,
        };
        // Avoid duplicates (same pattern + same state)
        if self.search_history.last().is_none_or(|last| *last != entry) {
            crate::history::append_history_entry(&entry);
            self.search_history.push(entry);
        }
    }

    /// Toggle name-only matching mode.
    pub(crate) const fn toggle_name_only(&mut self) {
        self.name_only = !self.name_only;
    }

    /// Toggle case-sensitive search mode.
    pub(crate) const fn toggle_case_sensitive(&mut self) {
        self.case_sensitive = !self.case_sensitive;
    }

    /// Toggle whole-word search mode.
    pub(crate) const fn toggle_whole_word(&mut self) {
        self.whole_word = !self.whole_word;
    }

    /// Cycle filter mode: `All` → `FilesOnly` → `DirsOnly` → `All`.
    pub(crate) const fn cycle_filter(&mut self) {
        self.filter_mode = match self.filter_mode {
            FilterMode::All => FilterMode::FilesOnly,
            FilterMode::FilesOnly => FilterMode::DirsOnly,
            FilterMode::DirsOnly => FilterMode::All,
        };
    }

    /// Get a display label for the current filter mode.
    #[must_use]
    pub(crate) const fn filter_label(&self) -> &str {
        match self.filter_mode {
            FilterMode::All => "",
            FilterMode::FilesOnly => " [FILES]",
            FilterMode::DirsOnly => " [DIRS]",
        }
    }

    // `build_cli_command` and `copy_cli_to_clipboard` are in `app_util.rs`.
}

// Clipboard and textarea helpers extracted to `app_util.rs`.
use crate::app_util::make_search_textarea;

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
