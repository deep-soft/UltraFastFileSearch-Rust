//! Centralized keybinding definitions for the TUI.
//!
//! Keybindings are loaded from a TOML config file at startup. If no config
//! file exists, the app writes a default preset (Windows-style) to the
//! platform config directory. Presets are embedded in the binary — no
//! external files to distribute.
//!
//! ## Config file location
//!
//! - macOS: `~/Library/Application Support/uffs/keys.toml`
//! - Windows: `%APPDATA%\uffs\keys.toml`
//! - Linux: `~/.config/uffs/keys.toml`
//!
//! ## Switching presets
//!
//! `uffs_tui --keys emacs` — overwrites the config file with the Emacs
//! preset. The user can then hand-edit the file for further customization.
//!
//! ## Groups
//!
//! - **App** — global keys that work everywhere (quit, refresh, help).
//! - **Search box** — keys for the search input (text editing, history,
//!   search-mode toggles).
//! - **Results panel** — keys for navigating and sorting the results table.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;

/// A keybinding definition: (key code, required modifiers).
pub type KeyBind = (KeyCode, KeyModifiers);

// ═══════════════════════════════════════════════════════════════════════════
// Embedded presets — carried inside the binary
// ═══════════════════════════════════════════════════════════════════════════

/// Windows-style keybindings (default).
const PRESET_WINDOWS: &str = r#"# UFFS TUI Keybindings — Windows preset
# Edit this file to customize. Use `uffs_tui --keys emacs` to switch presets.
# Delete this file to reset to defaults on next launch.

[app]
quit = ["ctrl+q"]
refresh = ["f5", "ctrl+r"]
help_cycle = ["f1"]

[search_box]
clear_line = ["ctrl+u"]
undo = ["ctrl+z"]
redo = ["ctrl+y"]
select_all = ["ctrl+a"]
history_back = ["ctrl+p"]
history_forward = ["ctrl+n"]
toggle_name_only = ["f2"]
toggle_filter = ["f3"]
toggle_case_sensitive = ["f7"]
toggle_whole_word = ["f8"]

[results]
nav_down = ["down"]
nav_up = ["up"]
page_down = ["pagedown"]
page_up = ["pageup"]
show_path = ["enter"]
sort_cycle = ["tab"]
sort_direction = ["shift+tab"]
"#;

/// Emacs-style keybindings.
const PRESET_EMACS: &str = r#"# UFFS TUI Keybindings — Emacs preset
# Edit this file to customize. Use `uffs_tui --keys windows` to switch presets.
# Delete this file to reset to defaults on next launch.

[app]
quit = ["ctrl+q"]
refresh = ["f5", "ctrl+r"]
help_cycle = ["f1"]

[search_box]
clear_line = ["ctrl+k"]
undo = ["ctrl+/"]
redo = ["ctrl+shift+/"]
select_all = ["ctrl+a"]
history_back = ["ctrl+p"]
history_forward = ["ctrl+n"]
toggle_name_only = ["f2"]
toggle_filter = ["f3"]
toggle_case_sensitive = ["f7"]
toggle_whole_word = ["f8"]

[results]
nav_down = ["down", "ctrl+j"]
nav_up = ["up", "ctrl+k"]
page_down = ["pagedown", "ctrl+v"]
page_up = ["pageup", "alt+v"]
show_path = ["enter"]
sort_cycle = ["tab"]
sort_direction = ["shift+tab"]
"#;

// ═══════════════════════════════════════════════════════════════════════════
// Action enum
// ═══════════════════════════════════════════════════════════════════════════

/// Every bindable action in the TUI.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum Action {
    // ═══ App ════════════════════════════════════════════════════════════
    /// Quit the TUI.
    Quit,
    /// Refresh all drives (reload MFT / .uffs cache).
    Refresh,
    /// Cycle help bar pages.
    HelpCycle,

    // ═══ Search box ════════════════════════════════════════════════════
    /// Clear the search input line.
    ClearLine,
    /// Undo last text edit.
    Undo,
    /// Redo last undone edit.
    Redo,
    /// Select all text.
    SelectAll,
    /// Previous search in history.
    HistoryBack,
    /// Next search in history.
    HistoryForward,
    /// Toggle name-only matching.
    ToggleNameOnly,
    /// Cycle file/directory filter (All → Files → Dirs).
    ToggleFilter,
    /// Toggle case-sensitive search.
    ToggleCaseSensitive,
    /// Toggle whole-word search.
    ToggleWholeWord,

    // ═══ Results panel ═════════════════════════════════════════════════
    /// Move selection down in results.
    NavDown,
    /// Move selection up in results.
    NavUp,
    /// Page down in results.
    PageDown,
    /// Page up in results.
    PageUp,
    /// Show selected file path in status bar.
    ShowPath,
    /// Cycle sort column.
    SortCycle,
    /// Toggle sort direction (ascending ↔ descending).
    SortDirection,
}

/// Map a TOML key name to an `Action`.
fn action_from_name(group: &str, name: &str) -> Option<Action> {
    match (group, name) {
        ("app", "quit") => Some(Action::Quit),
        ("app", "refresh") => Some(Action::Refresh),
        ("app", "help_cycle") => Some(Action::HelpCycle),
        ("search_box", "clear_line") => Some(Action::ClearLine),
        ("search_box", "undo") => Some(Action::Undo),
        ("search_box", "redo") => Some(Action::Redo),
        ("search_box", "select_all") => Some(Action::SelectAll),
        ("search_box", "history_back") => Some(Action::HistoryBack),
        ("search_box", "history_forward") => Some(Action::HistoryForward),
        ("search_box", "toggle_name_only") => Some(Action::ToggleNameOnly),
        ("search_box", "toggle_filter") => Some(Action::ToggleFilter),
        ("search_box", "toggle_case_sensitive") => Some(Action::ToggleCaseSensitive),
        ("search_box", "toggle_whole_word") => Some(Action::ToggleWholeWord),
        ("results", "nav_down") => Some(Action::NavDown),
        ("results", "nav_up") => Some(Action::NavUp),
        ("results", "page_down") => Some(Action::PageDown),
        ("results", "page_up") => Some(Action::PageUp),
        ("results", "show_path") => Some(Action::ShowPath),
        ("results", "sort_cycle") => Some(Action::SortCycle),
        ("results", "sort_direction") => Some(Action::SortDirection),
        _ => None,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Keymap
// ═══════════════════════════════════════════════════════════════════════════

/// Runtime keymap — maps actions to one or more key bindings.
pub struct Keymap {
    bindings: HashMap<Action, Vec<KeyBind>>,
}

impl Keymap {
    /// Check if a key event matches any binding for the given action.
    #[must_use]
    pub fn matches(&self, key: KeyEvent, action: Action) -> bool {
        self.bindings
            .get(&action)
            .is_some_and(|binds| binds.iter().any(|b| matches_bind(key, *b)))
    }

    /// Get the human-readable label for the primary binding of an action.
    #[must_use]
    #[expect(
        dead_code,
        reason = "public API for help bar rendering; wired in a future phase"
    )]
    pub fn label(&self, action: Action) -> String {
        self.bindings
            .get(&action)
            .and_then(|binds| binds.first())
            .map_or_else(|| "?".to_owned(), |bind| format_key_bind(*bind))
    }
}

impl Default for Keymap {
    /// Build the default keymap by parsing the embedded Windows preset.
    fn default() -> Self {
        parse_toml(PRESET_WINDOWS).unwrap_or_else(|_| Self {
            bindings: HashMap::new(),
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Config file management
// ═══════════════════════════════════════════════════════════════════════════

/// Available preset names.
pub const PRESET_NAMES: &[&str] = &["windows", "emacs"];

/// Path to the keybinding config file.
fn config_file_path() -> Option<std::path::PathBuf> {
    dirs_next::config_dir().map(|config| config.join("uffs").join("keys.toml"))
}

/// Get the embedded TOML content for a preset name.
fn preset_toml(name: &str) -> Option<&'static str> {
    match name {
        "windows" => Some(PRESET_WINDOWS),
        "emacs" => Some(PRESET_EMACS),
        _ => None,
    }
}

/// Load keybindings from the config file, or create one with the default
/// preset if it doesn't exist.
///
/// If `preset_override` is `Some("emacs")`, the config file is overwritten
/// with that preset before loading.
pub fn load_or_create_keymap(preset_override: Option<&str>) -> (Keymap, String) {
    // If --keys <preset> was given, write that preset to disk
    if let Some(name) = preset_override {
        if let Some(toml_content) = preset_toml(name) {
            if let Some(path) = config_file_path() {
                if let Some(parent) = path.parent() {
                    drop(std::fs::create_dir_all(parent));
                }
                drop(std::fs::write(&path, toml_content));
            }
            let keymap = parse_toml(toml_content).unwrap_or_default();
            let msg = format!("Keybindings: {name} preset (written to config)");
            return (keymap, msg);
        }
        // Unknown preset name — fall through to default
        let keymap = Keymap::default();
        let msg = format!(
            "Unknown preset \"{name}\" — using windows. Available: {}",
            PRESET_NAMES.join(", ")
        );
        return (keymap, msg);
    }

    // Try to load existing config file
    if let Some(path) = config_file_path() {
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => match parse_toml(&content) {
                    Ok(keymap) => {
                        let msg = format!(
                            "Keybindings loaded from {}",
                            path.display()
                        );
                        return (keymap, msg);
                    }
                    Err(err) => {
                        let msg = format!(
                            "⚠ keys.toml parse error: {err} — using defaults"
                        );
                        return (Keymap::default(), msg);
                    }
                },
                Err(err) => {
                    let msg = format!(
                        "⚠ Failed to read keys.toml: {err} — using defaults"
                    );
                    return (Keymap::default(), msg);
                }
            }
        }

        // Config file doesn't exist — create it with the default preset
        if let Some(parent) = path.parent() {
            drop(std::fs::create_dir_all(parent));
        }
        drop(std::fs::write(&path, PRESET_WINDOWS));
    }

    (Keymap::default(), "Keybindings: windows (default)".to_owned())
}

// ═══════════════════════════════════════════════════════════════════════════
// TOML parsing
// ═══════════════════════════════════════════════════════════════════════════

/// TOML structure for keybinding config file.
#[derive(Deserialize)]
struct KeyConfig {
    #[serde(default)]
    app: HashMap<String, Vec<String>>,
    #[serde(default)]
    search_box: HashMap<String, Vec<String>>,
    #[serde(default)]
    results: HashMap<String, Vec<String>>,
}

/// Parse a TOML string into a `Keymap`.
fn parse_toml(toml_str: &str) -> Result<Keymap, String> {
    let config: KeyConfig =
        toml::from_str(toml_str).map_err(|err| format!("TOML parse error: {err}"))?;

    let mut bindings = HashMap::new();

    for (group_name, group_map) in [
        ("app", &config.app),
        ("search_box", &config.search_box),
        ("results", &config.results),
    ] {
        for (action_name, key_strings) in group_map {
            let Some(action) = action_from_name(group_name, action_name) else {
                continue; // skip unknown action names gracefully
            };
            let mut keybinds = Vec::new();
            for key_str in key_strings {
                match parse_key_string(key_str) {
                    Some(bind) => keybinds.push(bind),
                    None => {
                        tracing::warn!(
                            "Ignoring unknown key string \"{key_str}\" for {group_name}.{action_name}"
                        );
                    }
                }
            }
            if !keybinds.is_empty() {
                bindings.insert(action, keybinds);
            }
        }
    }

    Ok(Keymap { bindings })
}

// ═══════════════════════════════════════════════════════════════════════════
// Key string parser: "ctrl+q" → (KeyCode::Char('q'), KeyModifiers::CONTROL)
// ═══════════════════════════════════════════════════════════════════════════

/// Parse a human-readable key string into a `KeyBind`.
///
/// Supported formats:
/// - `"ctrl+q"`, `"ctrl+shift+a"`, `"alt+v"`
/// - `"f1"` .. `"f12"`
/// - `"enter"`, `"tab"`, `"shift+tab"`, `"up"`, `"down"`, `"pageup"`, etc.
/// - `"ctrl+/"` (single-char keys)
#[expect(
    clippy::too_many_lines,
    reason = "flat match table for all supported key names; splitting would fragment the parser"
)]
fn parse_key_string(s: &str) -> Option<KeyBind> {
    let s = s.trim().to_lowercase();
    let parts: Vec<&str> = s.split('+').collect();

    let mut modifiers = KeyModifiers::NONE;
    let mut key_part = None;

    for &part in &parts {
        match part {
            "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            "alt" | "meta" => modifiers |= KeyModifiers::ALT,
            other => key_part = Some(other),
        }
    }

    let key_str = key_part?;
    let code = match key_str {
        // Function keys
        "f1" => KeyCode::F(1),
        "f2" => KeyCode::F(2),
        "f3" => KeyCode::F(3),
        "f4" => KeyCode::F(4),
        "f5" => KeyCode::F(5),
        "f6" => KeyCode::F(6),
        "f7" => KeyCode::F(7),
        "f8" => KeyCode::F(8),
        "f9" => KeyCode::F(9),
        "f10" => KeyCode::F(10),
        "f11" => KeyCode::F(11),
        "f12" => KeyCode::F(12),
        // Navigation
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        // Special keys
        "enter" | "return" => KeyCode::Enter,
        "tab" => {
            if modifiers.contains(KeyModifiers::SHIFT) {
                modifiers.remove(KeyModifiers::SHIFT);
                // BackTab already implies Shift; keep SHIFT in modifiers
                // for matches_code to match
                modifiers |= KeyModifiers::SHIFT;
                KeyCode::BackTab
            } else {
                KeyCode::Tab
            }
        }
        "backspace" | "bs" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "escape" | "esc" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        // Single character
        c if c.len() == 1 => KeyCode::Char(c.chars().next()?),
        _ => return None,
    };

    Some((code, modifiers))
}

/// Format a `KeyBind` as a human-readable string (for TOML output / help bar).
fn format_key_bind(bind: KeyBind) -> String {
    let mut parts = Vec::new();
    if bind.1.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl");
    }
    if bind.1.contains(KeyModifiers::ALT) {
        parts.push("alt");
    }
    if bind.1.contains(KeyModifiers::SHIFT) && !matches!(bind.0, KeyCode::BackTab) {
        parts.push("shift");
    }

    let key = match bind.0 {
        KeyCode::Char(ch) => return format!("{}+{ch}", parts.join("+")),
        KeyCode::F(n) => return format!("f{n}"),
        KeyCode::Up => "up",
        KeyCode::Down => "down",
        KeyCode::Left => "left",
        KeyCode::Right => "right",
        KeyCode::PageUp => "pageup",
        KeyCode::PageDown => "pagedown",
        KeyCode::Home => "home",
        KeyCode::End => "end",
        KeyCode::Enter => "enter",
        KeyCode::Tab => "tab",
        KeyCode::BackTab => {
            parts.push("shift");
            "tab"
        }
        KeyCode::Backspace => "backspace",
        KeyCode::Delete => "delete",
        KeyCode::Esc => "escape",
        _ => "?",
    };

    if parts.is_empty() {
        key.to_owned()
    } else {
        format!("{}+{key}", parts.join("+"))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Key matching (runtime)
// ═══════════════════════════════════════════════════════════════════════════

/// Check if a key event matches a single keybinding.
#[inline]
fn matches_bind(key: KeyEvent, bind: KeyBind) -> bool {
    matches_code(key.code, bind.0) && key.modifiers.contains(bind.1)
}

/// Compare key codes (handles Char case for control keys).
#[inline]
const fn matches_code(actual: KeyCode, expected: KeyCode) -> bool {
    match (actual, expected) {
        (KeyCode::Char(a), KeyCode::Char(b)) => a as u32 == b as u32,
        (KeyCode::F(a), KeyCode::F(b)) => a == b,
        (KeyCode::Down, KeyCode::Down)
        | (KeyCode::Up, KeyCode::Up)
        | (KeyCode::Left, KeyCode::Left)
        | (KeyCode::Right, KeyCode::Right)
        | (KeyCode::PageDown, KeyCode::PageDown)
        | (KeyCode::PageUp, KeyCode::PageUp)
        | (KeyCode::Home, KeyCode::Home)
        | (KeyCode::End, KeyCode::End)
        | (KeyCode::Enter, KeyCode::Enter)
        | (KeyCode::Tab, KeyCode::Tab)
        | (KeyCode::BackTab, KeyCode::BackTab)
        | (KeyCode::Backspace, KeyCode::Backspace)
        | (KeyCode::Delete, KeyCode::Delete)
        | (KeyCode::Esc, KeyCode::Esc) => true,
        _ => false,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;

    #[test]
    fn test_parse_key_string_basic() {
        assert_eq!(
            parse_key_string("ctrl+q"),
            Some((KeyCode::Char('q'), KeyModifiers::CONTROL))
        );
        assert_eq!(
            parse_key_string("f5"),
            Some((KeyCode::F(5), KeyModifiers::NONE))
        );
        assert_eq!(
            parse_key_string("enter"),
            Some((KeyCode::Enter, KeyModifiers::NONE))
        );
        assert_eq!(
            parse_key_string("shift+tab"),
            Some((KeyCode::BackTab, KeyModifiers::SHIFT))
        );
        assert_eq!(
            parse_key_string("down"),
            Some((KeyCode::Down, KeyModifiers::NONE))
        );
        assert_eq!(
            parse_key_string("ctrl+shift+/"),
            Some((
                KeyCode::Char('/'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            ))
        );
    }

    #[test]
    fn test_parse_key_string_case_insensitive() {
        assert_eq!(
            parse_key_string("Ctrl+Q"),
            Some((KeyCode::Char('q'), KeyModifiers::CONTROL))
        );
        assert_eq!(
            parse_key_string("F5"),
            Some((KeyCode::F(5), KeyModifiers::NONE))
        );
    }

    #[test]
    fn test_parse_key_string_unknown() {
        assert_eq!(parse_key_string("nonsense"), None);
    }

    #[test]
    fn test_format_key_bind_roundtrip() {
        let cases = [
            "ctrl+q",
            "f5",
            "enter",
            "down",
            "pageup",
        ];
        for case in cases {
            let bind = parse_key_string(case).unwrap();
            let formatted = format_key_bind(bind);
            let reparsed = parse_key_string(&formatted).unwrap();
            assert_eq!(bind, reparsed, "roundtrip failed for \"{case}\"");
        }
    }

    #[test]
    fn test_windows_preset_parses() {
        let keymap = parse_toml(PRESET_WINDOWS).expect("Windows preset should parse");
        assert!(
            keymap.matches(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
                Action::Quit
            ),
            "Ctrl+Q should match Quit"
        );
        assert!(
            keymap.matches(
                KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE),
                Action::Refresh
            ),
            "F5 should match Refresh"
        );
    }

    #[test]
    fn test_emacs_preset_parses() {
        let keymap = parse_toml(PRESET_EMACS).expect("Emacs preset should parse");
        assert!(
            keymap.matches(
                KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL),
                Action::ClearLine
            ),
            "Ctrl+K should match ClearLine in emacs"
        );
        assert!(
            keymap.matches(
                KeyEvent::new(KeyCode::Char('/'), KeyModifiers::CONTROL),
                Action::Undo
            ),
            "Ctrl+/ should match Undo in emacs"
        );
    }

    #[test]
    fn test_default_keymap_matches_windows() {
        let keymap = Keymap::default();
        assert!(keymap.matches(
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
            Action::Undo
        ));
        assert!(keymap.matches(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
            Action::Refresh
        ));
    }
}
