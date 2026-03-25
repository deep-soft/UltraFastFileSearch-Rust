//! Search history: entry type, file format, and CLI command roundtrip.
//!
//! History file format:
//! ```text
//! # Find all Rust source files
//! uffs "*.rs" --files-only
//!
//! # Large executables on C drive
//! uffs "c:*.exe" --case --sort size
//!
//! # User-generated (no comment)
//! uffs "readme"
//! ```
//!
//! Rules:
//! - `#` lines are comments (attached to the next command below).
//! - Blank lines separate entries.
//! - Non-comment, non-blank lines are CLI commands.

use crate::backend::FilterMode;

/// A single search history entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    /// Optional human-readable description (from `#` comment lines).
    pub comment: Option<String>,
    /// The search pattern (what goes in the search box).
    pub pattern: String,
    /// Search state flags captured at the time of the search.
    pub state: SearchState,
}

/// Captured search state (toggles active at the time of the search).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchState {
    /// Case-sensitive matching.
    pub case_sensitive: bool,
    /// Whole-word matching.
    pub whole_word: bool,
    /// Match filename only (not full path).
    pub name_only: bool,
    /// Filter mode (all, files-only, dirs-only).
    pub filter: FilterMode,
}

// ═══════════════════════════════════════════════════════════════════════════
// CLI command serialization
// ═══════════════════════════════════════════════════════════════════════════

/// Serialize a search state into a `uffs` CLI command string.
///
/// Example output: `uffs "*.rs" --case --files-only --name-only`
#[must_use]
pub fn search_state_to_cli(pattern: &str, state: &SearchState) -> String {
    let mut parts = vec!["uffs.exe".to_owned()];

    // Quote the pattern if it contains spaces or special chars
    if pattern.contains(' ')
        || pattern.contains('*')
        || pattern.contains('?')
        || pattern.contains('>')
        || pattern.contains('|')
    {
        parts.push(format!("\"{pattern}\""));
    } else {
        parts.push(pattern.to_owned());
    }

    if state.case_sensitive {
        parts.push("--case".to_owned());
    }
    if state.whole_word {
        parts.push("--word".to_owned());
    }
    if state.name_only {
        parts.push("--name-only".to_owned());
    }
    match state.filter {
        FilterMode::All => {}
        FilterMode::FilesOnly => parts.push("--files-only".to_owned()),
        FilterMode::DirsOnly => parts.push("--dirs-only".to_owned()),
    }

    parts.join(" ")
}

/// Parse a `uffs` CLI command string back into pattern + search state.
///
/// Returns `None` if the line doesn't look like a uffs command.
#[must_use]
#[allow(clippy::single_call_fn)] // public API, called from parse_history_file
pub fn cli_to_search_state(cli: &str) -> Option<(String, SearchState)> {
    let trimmed = cli.trim();

    // Must start with "uffs" or "uffs.exe" (skip it)
    let rest = trimmed
        .strip_prefix("uffs.exe")
        .or_else(|| trimmed.strip_prefix("uffs"))?
        .trim_start();

    let mut state = SearchState::default();
    let mut pattern = String::new();
    let mut in_quotes = false;
    let mut tokens: Vec<String> = Vec::new();

    // Simple tokenizer: respects double quotes
    let mut current = String::new();
    for ch in rest.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ' ' if !in_quotes => {
                if !current.is_empty() {
                    tokens.push(core::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }

    for token in &tokens {
        match token.as_str() {
            "--case" => state.case_sensitive = true,
            "--word" => state.whole_word = true,
            "--name-only" => state.name_only = true,
            "--files-only" => state.filter = FilterMode::FilesOnly,
            "--dirs-only" => state.filter = FilterMode::DirsOnly,
            _ if !token.starts_with("--") && pattern.is_empty() => {
                pattern.clone_from(token);
            }
            _ => {} // ignore unknown flags (forward compat)
        }
    }

    if pattern.is_empty() {
        return None;
    }

    Some((pattern, state))
}

// ═══════════════════════════════════════════════════════════════════════════
// History file parsing and serialization
// ═══════════════════════════════════════════════════════════════════════════

/// Result of parsing a history file: valid entries + whether healing occurred.
pub struct ParseResult {
    /// Successfully parsed history entries.
    pub entries: Vec<HistoryEntry>,
    /// `true` if any lines were invalid and commented out (file needs rewrite).
    pub healed: bool,
    /// The healed file content (only meaningful when `healed` is `true`).
    pub healed_content: String,
}

/// Parse a history file into a list of entries, validating each line.
///
/// Format: `#` comment lines attach to the next command. Blank lines separate
/// entries. Non-comment, non-blank lines must be valid `uffs.exe` / `uffs`
/// CLI commands.
///
/// Invalid lines are prefixed with `# INVALID: ` in the healed output so the
/// user can inspect and fix them.
#[must_use]
pub fn parse_history_file(content: &str) -> ParseResult {
    let mut entries = Vec::new();
    let mut comment_lines: Vec<String> = Vec::new();
    let mut healed_lines: Vec<String> = Vec::new();
    let mut healed = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            // Blank line — reset pending comment if no command followed
            comment_lines.clear();
            healed_lines.push(String::new());
        } else if let Some(comment_text) = trimmed.strip_prefix('#') {
            comment_lines.push(comment_text.trim().to_owned());
            healed_lines.push(line.to_owned());
        } else if let Some((pattern, state)) = cli_to_search_state(trimmed) {
            let comment = if comment_lines.is_empty() {
                None
            } else {
                Some(comment_lines.join(" "))
            };
            entries.push(HistoryEntry {
                comment,
                pattern,
                state,
            });
            comment_lines.clear();
            healed_lines.push(line.to_owned());
        } else {
            // Invalid line — comment it out for the user to review
            tracing::warn!(line = trimmed, "Invalid history entry — commenting out");
            healed_lines.push(format!("# INVALID: {trimmed}"));
            // Discard any pending comment lines (they belonged to this entry)
            comment_lines.clear();
            healed = true;
        }
    }

    let healed_content = healed_lines.join("\n");
    ParseResult {
        entries,
        healed,
        healed_content,
    }
}

/// Serialize a list of history entries to the file format.
#[must_use]
#[allow(dead_code, clippy::single_call_fn)] // used by future history editor overlay
pub fn serialize_history_file(entries: &[HistoryEntry]) -> String {
    let mut output = String::new();
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            output.push('\n');
        }
        if let Some(comment) = &entry.comment {
            output.push_str("# ");
            output.push_str(comment);
            output.push('\n');
        }
        output.push_str(&search_state_to_cli(&entry.pattern, &entry.state));
        output.push('\n');
    }
    output
}

/// Default history file content with example searches.
///
/// Shipped on first launch. Never overwritten once the user adds entries.
pub const DEFAULT_HISTORY: &str = "\
# Find all Rust source files
uffs.exe \"*.rs\" --files-only

# Find all executable files
uffs.exe \"*.exe\" --files-only

# Find directories named 'node_modules'
uffs.exe node_modules --dirs-only --name-only

# Find large files (search all, sort by size in TUI)
uffs.exe \"*\" --files-only

# Find config files in project trees
uffs.exe \"\\projects\\**\\*.toml\"

# Case-sensitive search for README files
uffs.exe README --case --name-only

# Find log files using regex
uffs.exe \">.*\\.log$\" --files-only

# Find files modified recently (type a date filter in TUI)
uffs.exe \"*\"

# Find hidden system files
uffs.exe \"$*\" --name-only
";

/// Path to the persistent search history file.
///
/// Uses the platform-appropriate config directory:
/// - macOS: `~/Library/Application Support/uffs/search_history.txt`
/// - Windows: `%APPDATA%\uffs\search_history.txt`
/// - Linux: `~/.config/uffs/search_history.txt`
#[must_use]
pub fn history_file_path() -> Option<std::path::PathBuf> {
    dirs_next::config_dir().map(|config| config.join("uffs").join("search_history.txt"))
}

/// Load history from disk, creating the default file if it doesn't exist.
///
/// If any entries are invalid (e.g. user edits broke the format), they are
/// commented out with `# INVALID: ` and the file is rewritten so the user
/// can review and fix them.
#[must_use]
#[allow(clippy::single_call_fn)] // public API, called from App::load_history
pub fn load_history() -> Vec<HistoryEntry> {
    let Some(path) = history_file_path() else {
        return Vec::new();
    };

    if path.exists() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            let result = parse_history_file(&content);
            if result.healed {
                tracing::warn!("History file contained invalid entries — commented out for review");
                drop(std::fs::write(&path, &result.healed_content));
            }
            return result.entries;
        }
    } else {
        // First launch — create default history file
        if let Some(parent) = path.parent() {
            drop(std::fs::create_dir_all(parent));
        }
        drop(std::fs::write(&path, DEFAULT_HISTORY));
        return parse_history_file(DEFAULT_HISTORY).entries;
    }

    Vec::new()
}

/// Save the full history to disk (overwrites the file).
#[allow(dead_code)] // used by future history editor overlay
pub fn save_history(entries: &[HistoryEntry]) {
    if let Some(path) = history_file_path() {
        if let Some(parent) = path.parent() {
            drop(std::fs::create_dir_all(parent));
        }
        let content = serialize_history_file(entries);
        drop(std::fs::write(&path, content));
    }
}

/// Append a single entry to the history file on disk.
#[allow(clippy::single_call_fn)] // public API, called from App::save_history_entry
pub fn append_history_entry(entry: &HistoryEntry) {
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
            // Blank line separator before new entry
            drop(writeln!(file));
            if let Some(comment) = &entry.comment {
                drop(writeln!(file, "# {comment}"));
            }
            drop(writeln!(
                file,
                "{}",
                search_state_to_cli(&entry.pattern, &entry.state)
            ));
        }
    }
}

/// Reset the history file to the default content.
#[allow(clippy::single_call_fn)] // public API, called from main --reset-history
pub fn reset_history() {
    if let Some(path) = history_file_path() {
        if let Some(parent) = path.parent() {
            drop(std::fs::create_dir_all(parent));
        }
        drop(std::fs::write(&path, DEFAULT_HISTORY));
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)] // test assertions verify length before indexing
mod tests {
    use super::*;

    // ═══════════════════════════════════════════════════════════════════
    // search_state_to_cli
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn cli_simple_pattern_no_flags() {
        let state = SearchState::default();
        let cli = search_state_to_cli("readme", &state);
        assert_eq!(cli, "uffs.exe readme");
    }

    #[test]
    fn cli_pattern_with_wildcards_is_quoted() {
        let state = SearchState::default();
        let cli = search_state_to_cli("*.rs", &state);
        assert_eq!(cli, "uffs.exe \"*.rs\"");
    }

    #[test]
    fn cli_pattern_with_spaces_is_quoted() {
        let state = SearchState::default();
        let cli = search_state_to_cli("my file", &state);
        assert_eq!(cli, "uffs.exe \"my file\"");
    }

    #[test]
    fn cli_pattern_with_question_mark_is_quoted() {
        let state = SearchState::default();
        let cli = search_state_to_cli("file?.txt", &state);
        assert_eq!(cli, "uffs.exe \"file?.txt\"");
    }

    #[test]
    fn cli_pattern_with_regex_prefix_is_quoted() {
        let state = SearchState::default();
        let cli = search_state_to_cli(">.*\\.log$", &state);
        assert_eq!(cli, "uffs.exe \">.*\\.log$\"");
    }

    #[test]
    fn cli_pattern_with_pipe_is_quoted() {
        let state = SearchState::default();
        let cli = search_state_to_cli("foo|bar", &state);
        assert_eq!(cli, "uffs.exe \"foo|bar\"");
    }

    #[test]
    fn cli_case_sensitive_flag() {
        let state = SearchState {
            case_sensitive: true,
            ..Default::default()
        };
        let cli = search_state_to_cli("readme", &state);
        assert_eq!(cli, "uffs.exe readme --case");
    }

    #[test]
    fn cli_whole_word_flag() {
        let state = SearchState {
            whole_word: true,
            ..Default::default()
        };
        let cli = search_state_to_cli("readme", &state);
        assert_eq!(cli, "uffs.exe readme --word");
    }

    #[test]
    fn cli_name_only_flag() {
        let state = SearchState {
            name_only: true,
            ..Default::default()
        };
        let cli = search_state_to_cli("readme", &state);
        assert_eq!(cli, "uffs.exe readme --name-only");
    }

    #[test]
    fn cli_files_only_flag() {
        let state = SearchState {
            filter: FilterMode::FilesOnly,
            ..Default::default()
        };
        let cli = search_state_to_cli("readme", &state);
        assert_eq!(cli, "uffs.exe readme --files-only");
    }

    #[test]
    fn cli_dirs_only_flag() {
        let state = SearchState {
            filter: FilterMode::DirsOnly,
            ..Default::default()
        };
        let cli = search_state_to_cli("readme", &state);
        assert_eq!(cli, "uffs.exe readme --dirs-only");
    }

    #[test]
    fn cli_all_flags_combined() {
        let state = SearchState {
            case_sensitive: true,
            whole_word: true,
            name_only: true,
            filter: FilterMode::FilesOnly,
        };
        let cli = search_state_to_cli("*.rs", &state);
        assert_eq!(
            cli,
            "uffs.exe \"*.rs\" --case --word --name-only --files-only"
        );
    }

    #[test]
    fn cli_filter_all_produces_no_flag() {
        let state = SearchState {
            filter: FilterMode::All,
            ..Default::default()
        };
        let cli = search_state_to_cli("test", &state);
        assert_eq!(cli, "uffs.exe test");
    }

    // ═══════════════════════════════════════════════════════════════════
    // cli_to_search_state
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn parse_cli_simple() {
        let (pat, state) = cli_to_search_state("uffs.exe readme").unwrap();
        assert_eq!(pat, "readme");
        assert_eq!(state, SearchState::default());
    }

    #[test]
    fn parse_cli_without_exe_suffix() {
        let (pat, state) = cli_to_search_state("uffs readme").unwrap();
        assert_eq!(pat, "readme");
        assert_eq!(state, SearchState::default());
    }

    #[test]
    fn parse_cli_quoted_pattern() {
        let (pat, _) = cli_to_search_state("uffs.exe \"*.rs\"").unwrap();
        assert_eq!(pat, "*.rs");
    }

    #[test]
    fn parse_cli_quoted_pattern_with_spaces() {
        let (pat, _) = cli_to_search_state("uffs.exe \"my file.txt\"").unwrap();
        assert_eq!(pat, "my file.txt");
    }

    #[test]
    fn parse_cli_case_flag() {
        let (_, state) = cli_to_search_state("uffs.exe readme --case").unwrap();
        assert!(state.case_sensitive);
        assert!(!state.whole_word);
    }

    #[test]
    fn parse_cli_word_flag() {
        let (_, state) = cli_to_search_state("uffs.exe readme --word").unwrap();
        assert!(state.whole_word);
    }

    #[test]
    fn parse_cli_name_only_flag() {
        let (_, state) = cli_to_search_state("uffs.exe readme --name-only").unwrap();
        assert!(state.name_only);
    }

    #[test]
    fn parse_cli_files_only_flag() {
        let (_, state) = cli_to_search_state("uffs.exe readme --files-only").unwrap();
        assert_eq!(state.filter, FilterMode::FilesOnly);
    }

    #[test]
    fn parse_cli_dirs_only_flag() {
        let (_, state) = cli_to_search_state("uffs.exe readme --dirs-only").unwrap();
        assert_eq!(state.filter, FilterMode::DirsOnly);
    }

    #[test]
    fn parse_cli_all_flags() {
        let (pat, state) =
            cli_to_search_state("uffs.exe \"*.rs\" --case --word --name-only --files-only")
                .unwrap();
        assert_eq!(pat, "*.rs");
        assert!(state.case_sensitive);
        assert!(state.whole_word);
        assert!(state.name_only);
        assert_eq!(state.filter, FilterMode::FilesOnly);
    }

    #[test]
    fn parse_cli_unknown_flags_ignored() {
        let (pat, state) = cli_to_search_state("uffs.exe readme --case --future-flag").unwrap();
        assert_eq!(pat, "readme");
        assert!(state.case_sensitive);
    }

    #[test]
    fn parse_cli_not_uffs_returns_none() {
        assert!(cli_to_search_state("grep readme").is_none());
    }

    #[test]
    fn parse_cli_empty_returns_none() {
        assert!(cli_to_search_state("").is_none());
    }

    #[test]
    fn parse_cli_uffs_no_pattern_returns_none() {
        assert!(cli_to_search_state("uffs.exe").is_none());
    }

    #[test]
    fn parse_cli_uffs_only_flags_returns_none() {
        assert!(cli_to_search_state("uffs.exe --case --word").is_none());
    }

    #[test]
    fn parse_cli_leading_trailing_whitespace() {
        let (pat, _) = cli_to_search_state("  uffs.exe readme  ").unwrap();
        assert_eq!(pat, "readme");
    }

    // ═══════════════════════════════════════════════════════════════════
    // Roundtrip: serialize → parse
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn roundtrip_simple_pattern() {
        let state = SearchState::default();
        let cli = search_state_to_cli("readme", &state);
        let (pat, parsed) = cli_to_search_state(&cli).unwrap();
        assert_eq!(pat, "readme");
        assert_eq!(parsed, state);
    }

    #[test]
    fn roundtrip_all_flags() {
        let state = SearchState {
            case_sensitive: true,
            whole_word: true,
            name_only: true,
            filter: FilterMode::DirsOnly,
        };
        let cli = search_state_to_cli("*.toml", &state);
        let (pat, parsed) = cli_to_search_state(&cli).unwrap();
        assert_eq!(pat, "*.toml");
        assert_eq!(parsed, state);
    }

    #[test]
    fn roundtrip_quoted_pattern_with_spaces() {
        let state = SearchState {
            case_sensitive: true,
            ..Default::default()
        };
        let cli = search_state_to_cli("my documents", &state);
        let (pat, parsed) = cli_to_search_state(&cli).unwrap();
        assert_eq!(pat, "my documents");
        assert_eq!(parsed, state);
    }

    #[test]
    fn roundtrip_regex_pattern() {
        let state = SearchState {
            filter: FilterMode::FilesOnly,
            ..Default::default()
        };
        let cli = search_state_to_cli(">.*\\.log$", &state);
        let (pat, parsed) = cli_to_search_state(&cli).unwrap();
        assert_eq!(pat, ">.*\\.log$");
        assert_eq!(parsed, state);
    }

    // ═══════════════════════════════════════════════════════════════════
    // parse_history_file
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn parse_file_single_entry_no_comment() {
        let result = parse_history_file("uffs.exe readme\n");
        assert!(!result.healed);
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].pattern, "readme");
        assert!(result.entries[0].comment.is_none());
    }

    #[test]
    fn parse_file_single_entry_with_comment() {
        let content = "# Find readme files\nuffs.exe readme\n";
        let result = parse_history_file(content);
        assert!(!result.healed);
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].pattern, "readme");
        assert_eq!(
            result.entries[0].comment.as_deref(),
            Some("Find readme files")
        );
    }

    #[test]
    fn parse_file_multiline_comment() {
        let content = "# Line one\n# Line two\nuffs.exe readme\n";
        let result = parse_history_file(content);
        assert_eq!(
            result.entries[0].comment.as_deref(),
            Some("Line one Line two")
        );
    }

    #[test]
    fn parse_file_multiple_entries() {
        let content = "\
# First search
uffs.exe \"*.rs\" --files-only

# Second search
uffs.exe readme --case
";
        let result = parse_history_file(content);
        assert!(!result.healed);
        assert_eq!(result.entries.len(), 2);
        assert_eq!(result.entries[0].pattern, "*.rs");
        assert_eq!(result.entries[0].state.filter, FilterMode::FilesOnly);
        assert_eq!(result.entries[1].pattern, "readme");
        assert!(result.entries[1].state.case_sensitive);
    }

    #[test]
    fn parse_file_blank_lines_reset_comments() {
        // Comment followed by blank line — comment is discarded
        let content = "# Orphan comment\n\nuffs.exe readme\n";
        let result = parse_history_file(content);
        assert_eq!(result.entries.len(), 1);
        assert!(result.entries[0].comment.is_none());
    }

    #[test]
    fn parse_file_empty_content() {
        let result = parse_history_file("");
        assert!(result.entries.is_empty());
        assert!(!result.healed);
    }

    #[test]
    fn parse_file_only_comments() {
        let result = parse_history_file("# Just a comment\n# Another\n");
        assert!(result.entries.is_empty());
        assert!(!result.healed);
    }

    #[test]
    fn parse_file_only_blank_lines() {
        let result = parse_history_file("\n\n\n");
        assert!(result.entries.is_empty());
        assert!(!result.healed);
    }

    // ═══════════════════════════════════════════════════════════════════
    // Validation / healing
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn healing_invalid_line_gets_commented_out() {
        let content = "uffs.exe readme\nthis is garbage\nuffs.exe \"*.rs\"\n";
        let result = parse_history_file(content);
        assert!(result.healed);
        assert_eq!(result.entries.len(), 2);
        assert_eq!(result.entries[0].pattern, "readme");
        assert_eq!(result.entries[1].pattern, "*.rs");
        assert!(result.healed_content.contains("# INVALID: this is garbage"));
    }

    #[test]
    fn healing_preserves_valid_entries() {
        let content = "# Good search\nuffs.exe readme --case\nbroken line\n";
        let result = parse_history_file(content);
        assert!(result.healed);
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].pattern, "readme");
        assert_eq!(result.entries[0].comment.as_deref(), Some("Good search"));
        assert!(result.healed_content.contains("# INVALID: broken line"));
    }

    #[test]
    fn healing_multiple_invalid_lines() {
        let content = "bad1\nbad2\nuffs.exe ok\nbad3\n";
        let result = parse_history_file(content);
        assert!(result.healed);
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].pattern, "ok");
        assert!(result.healed_content.contains("# INVALID: bad1"));
        assert!(result.healed_content.contains("# INVALID: bad2"));
        assert!(result.healed_content.contains("# INVALID: bad3"));
    }

    #[test]
    fn healing_comment_before_invalid_is_discarded() {
        // Comment attached to an invalid line — comment stays, line gets INVALID
        let content = "# My search\nnot_a_uffs_command\n";
        let result = parse_history_file(content);
        assert!(result.healed);
        assert!(result.entries.is_empty());
        assert!(
            result
                .healed_content
                .contains("# INVALID: not_a_uffs_command")
        );
    }

    #[test]
    fn no_healing_when_all_valid() {
        let content = "# Search\nuffs.exe readme\n\nuffs.exe \"*.rs\" --case\n";
        let result = parse_history_file(content);
        assert!(!result.healed);
        assert_eq!(result.entries.len(), 2);
    }

    // ═══════════════════════════════════════════════════════════════════
    // serialize_history_file
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn serialize_empty_list() {
        let output = serialize_history_file(&[]);
        assert!(output.is_empty());
    }

    #[test]
    fn serialize_single_entry_no_comment() {
        let entries = vec![HistoryEntry {
            comment: None,
            pattern: "readme".to_owned(),
            state: SearchState::default(),
        }];
        let output = serialize_history_file(&entries);
        assert_eq!(output, "uffs.exe readme\n");
    }

    #[test]
    fn serialize_single_entry_with_comment() {
        let entries = vec![HistoryEntry {
            comment: Some("Find readme".to_owned()),
            pattern: "readme".to_owned(),
            state: SearchState::default(),
        }];
        let output = serialize_history_file(&entries);
        assert_eq!(output, "# Find readme\nuffs.exe readme\n");
    }

    #[test]
    fn serialize_multiple_entries_separated_by_blank_line() {
        let entries = vec![
            HistoryEntry {
                comment: Some("First".to_owned()),
                pattern: "a".to_owned(),
                state: SearchState::default(),
            },
            HistoryEntry {
                comment: None,
                pattern: "b".to_owned(),
                state: SearchState {
                    case_sensitive: true,
                    ..Default::default()
                },
            },
        ];
        let output = serialize_history_file(&entries);
        assert_eq!(output, "# First\nuffs.exe a\n\nuffs.exe b --case\n");
    }

    #[test]
    fn serialize_preserves_all_flags() {
        let entries = vec![HistoryEntry {
            comment: None,
            pattern: "*.rs".to_owned(),
            state: SearchState {
                case_sensitive: true,
                whole_word: true,
                name_only: true,
                filter: FilterMode::FilesOnly,
            },
        }];
        let output = serialize_history_file(&entries);
        assert_eq!(
            output,
            "uffs.exe \"*.rs\" --case --word --name-only --files-only\n"
        );
    }

    // ═══════════════════════════════════════════════════════════════════
    // Full roundtrip: serialize → parse_history_file → entries match
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn file_roundtrip() {
        let original = vec![
            HistoryEntry {
                comment: Some("Rust files".to_owned()),
                pattern: "*.rs".to_owned(),
                state: SearchState {
                    filter: FilterMode::FilesOnly,
                    ..Default::default()
                },
            },
            HistoryEntry {
                comment: None,
                pattern: "readme".to_owned(),
                state: SearchState {
                    case_sensitive: true,
                    name_only: true,
                    ..Default::default()
                },
            },
        ];
        let serialized = serialize_history_file(&original);
        let result = parse_history_file(&serialized);
        assert!(!result.healed);
        assert_eq!(result.entries, original);
    }

    // ═══════════════════════════════════════════════════════════════════
    // DEFAULT_HISTORY validation
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn default_history_is_valid() {
        let result = parse_history_file(DEFAULT_HISTORY);
        assert!(
            !result.healed,
            "DEFAULT_HISTORY contains invalid entries: {}",
            result.healed_content
        );
        assert!(
            !result.entries.is_empty(),
            "DEFAULT_HISTORY should contain at least one entry"
        );
    }

    #[test]
    fn default_history_all_entries_have_comments() {
        let result = parse_history_file(DEFAULT_HISTORY);
        for (i, entry) in result.entries.iter().enumerate() {
            assert!(
                entry.comment.is_some(),
                "Default history entry {i} ({}) should have a comment",
                entry.pattern
            );
        }
    }

    #[test]
    fn default_history_entry_count() {
        let result = parse_history_file(DEFAULT_HISTORY);
        // We ship 9 example searches
        assert_eq!(
            result.entries.len(),
            9,
            "Expected 9 default history entries, got {}",
            result.entries.len()
        );
    }
}
