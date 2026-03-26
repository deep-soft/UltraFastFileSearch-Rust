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
///
/// Priority: common Windows convention first, Everything (file search) backup.
const PRESET_WINDOWS: &str = r#"# UFFS TUI Keybindings — Windows preset
# Edit this file to customize. Use `uffs_tui --keys emacs` to switch presets.
# Delete this file to reset to defaults on next launch.
# New keys added in future versions are auto-filled from this preset.
#
# Priority: common Windows convention → Everything (file search tool) backup.

[meta]
preset = "windows"

[app]
quit = ["ctrl+q"]
refresh = ["ctrl+r"]
help_cycle = ["alt+h", "ctrl+g", "f1"]

[search_box]
clear_line = ["ctrl+u"]
undo = ["ctrl+z"]
redo = ["ctrl+y"]
select_all = ["ctrl+a"]
copy = ["ctrl+c"]
paste = ["ctrl+v"]
toggle_name_only = ["alt+n", "ctrl+f"]
toggle_filter = ["alt+t", "ctrl+t"]
toggle_case_sensitive = ["alt+c", "tab"]
toggle_whole_word = ["alt+w", "ctrl+w"]
copy_cli_command = ["enter"]

[results]
nav_down = ["down"]
nav_up = ["up"]
page_down = ["pagedown"]
page_up = ["pageup"]
show_path = ["enter"]
sort_cycle = ["ctrl+shift+s"]
sort_direction = ["ctrl+shift+d"]
"#;

/// Emacs-style keybindings.
///
/// Priority: Emacs convention first, common Alt-key alternatives as backup.
const PRESET_EMACS: &str = r#"# UFFS TUI Keybindings — Emacs preset
# Edit this file to customize. Use `uffs_tui --keys windows` to switch presets.
# Delete this file to reset to defaults on next launch.
# New keys added in future versions are auto-filled from this preset.
#
# Priority: Emacs convention → common Alt-key alternatives as backup.

[meta]
preset = "emacs"

[app]
quit = ["ctrl+q"]
refresh = ["ctrl+r"]
help_cycle = ["alt+h", "f1"]

[search_box]
clear_line = ["ctrl+k"]
undo = ["ctrl+/"]
redo = ["ctrl+shift+/"]
select_all = ["ctrl+a"]
copy = ["ctrl+c"]
paste = ["ctrl+v"]
toggle_name_only = ["alt+n", "ctrl+f"]
toggle_filter = ["alt+t", "ctrl+t"]
toggle_case_sensitive = ["alt+c", "tab"]
toggle_whole_word = ["alt+w", "ctrl+w"]
copy_cli_command = ["enter"]

[results]
nav_down = ["down", "ctrl+j"]
nav_up = ["up", "ctrl+k"]
page_down = ["pagedown"]
page_up = ["pageup", "alt+v"]
show_path = ["enter"]
sort_cycle = ["ctrl+shift+s"]
sort_direction = ["ctrl+shift+d"]
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
    /// Copy selected text to internal clipboard.
    Copy,
    /// Paste from internal clipboard.
    Paste,
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
    /// Copy the equivalent CLI command to the system clipboard.
    CopyCliCommand,

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
#[expect(
    clippy::single_call_fn,
    reason = "lookup table kept separate for clarity and testability"
)]
fn action_from_name(group: &str, name: &str) -> Option<Action> {
    match (group, name) {
        ("app", "quit") => Some(Action::Quit),
        ("app", "refresh") => Some(Action::Refresh),
        ("app", "help_cycle") => Some(Action::HelpCycle),
        ("search_box", "clear_line") => Some(Action::ClearLine),
        ("search_box", "undo") => Some(Action::Undo),
        ("search_box", "redo") => Some(Action::Redo),
        ("search_box", "select_all") => Some(Action::SelectAll),
        ("search_box", "copy") => Some(Action::Copy),
        ("search_box", "paste") => Some(Action::Paste),
        ("search_box", "history_back") => Some(Action::HistoryBack),
        ("search_box", "history_forward") => Some(Action::HistoryForward),
        ("search_box", "toggle_name_only") => Some(Action::ToggleNameOnly),
        ("search_box", "toggle_filter") => Some(Action::ToggleFilter),
        ("search_box", "toggle_case_sensitive") => Some(Action::ToggleCaseSensitive),
        ("search_box", "toggle_whole_word") => Some(Action::ToggleWholeWord),
        ("search_box", "copy_cli_command") => Some(Action::CopyCliCommand),
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
    /// Map from action to its configured key bindings.
    bindings: HashMap<Action, Vec<KeyBind>>,
}

impl Keymap {
    /// Check if a key event matches any binding for the given action.
    #[must_use]
    pub fn matches(&self, key: KeyEvent, action: Action) -> bool {
        self.bindings
            .get(&action)
            .is_some_and(|binds| binds.iter().any(|bind| matches_bind(key, *bind)))
    }

    /// Get the compact, human-readable label for the best binding of an
    /// action.
    ///
    /// On macOS, Alt-modified bindings are skipped (the Option key sends
    /// special characters in most terminals) and the next available binding
    /// is returned instead.
    #[must_use]
    pub fn label(&self, action: Action) -> String {
        let Some(binds) = self.bindings.get(&action) else {
            return "?".to_owned();
        };
        let best = pick_platform_bind(binds);
        best.map_or_else(|| "?".to_owned(), |bind| format_key_display(*bind))
    }
}

impl Default for Keymap {
    /// Build the default keymap by parsing the embedded Windows preset.
    fn default() -> Self {
        parse_toml(PRESET_WINDOWS).map_or_else(
            |_| Self {
                bindings: HashMap::new(),
            },
            |(km, _)| km,
        )
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
#[expect(
    clippy::single_call_fn,
    reason = "public API entry point; called from main"
)]
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
            let keymap = parse_toml(toml_content)
                .map(|(km, _)| km)
                .unwrap_or_default();
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
                    Ok((mut keymap, preset_name)) => {
                        // Backfill any missing actions from the matching
                        // default preset so new keys added in future
                        // versions appear automatically.
                        let origin = preset_name.as_deref().unwrap_or("windows");
                        backfill_from_preset(&mut keymap.bindings, origin);
                        let msg = format!(
                            "Keybindings loaded from {} (preset: {origin})",
                            path.display()
                        );
                        return (keymap, msg);
                    }
                    Err(err) => {
                        let msg = format!("⚠ keys.toml parse error: {err} — using defaults");
                        return (Keymap::default(), msg);
                    }
                },
                Err(err) => {
                    let msg = format!("⚠ Failed to read keys.toml: {err} — using defaults");
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

    (
        Keymap::default(),
        "Keybindings: windows (default)".to_owned(),
    )
}

// ═══════════════════════════════════════════════════════════════════════════
// TOML parsing
// ═══════════════════════════════════════════════════════════════════════════

/// Metadata section in the TOML config file.
#[derive(Deserialize, Default)]
struct MetaConfig {
    /// Which preset this config was originally created from.
    #[serde(default)]
    preset: Option<String>,
}

/// TOML structure for keybinding config file.
#[derive(Deserialize)]
struct KeyConfig {
    /// Metadata — tracks which preset this config originated from.
    #[serde(default)]
    meta: MetaConfig,
    /// App-level keybindings (quit, refresh, help).
    #[serde(default)]
    app: HashMap<String, Vec<String>>,
    /// Search-box keybindings (text editing, history, toggles).
    #[serde(default)]
    search_box: HashMap<String, Vec<String>>,
    /// Results-panel keybindings (navigation, sorting).
    #[serde(default)]
    results: HashMap<String, Vec<String>>,
}

/// Parse a TOML string into a `Keymap`.
///
/// Returns the keymap and the preset name (if present in `[meta]`).
fn parse_toml(toml_str: &str) -> Result<(Keymap, Option<String>), String> {
    let config: KeyConfig =
        toml::from_str(toml_str).map_err(|err| format!("TOML parse error: {err}"))?;

    let preset_name = config.meta.preset.clone();
    let bindings = bindings_from_config(&config);

    Ok((Keymap { bindings }, preset_name))
}

/// Extract action→keybind map from a parsed `KeyConfig`.
fn bindings_from_config(config: &KeyConfig) -> HashMap<Action, Vec<KeyBind>> {
    let mut bindings = HashMap::new();

    let groups: [(&str, &HashMap<String, Vec<String>>); 3] = [
        ("app", &config.app),
        ("search_box", &config.search_box),
        ("results", &config.results),
    ];
    for (group_name, group_map) in groups {
        let mut sorted_actions: Vec<_> = group_map.iter().collect();
        sorted_actions.sort_by_key(|(name, _)| name.as_str());
        for (action_name, key_strings) in sorted_actions {
            let Some(action) = action_from_name(group_name, action_name) else {
                continue;
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

    bindings
}

/// Backfill missing actions from the default preset that matches the user's
/// config origin.
///
/// If the user's `keys.toml` was created from the "windows" preset and a new
/// version adds a `copy` action, this fills it in from the embedded Windows
/// preset — without overwriting anything the user has customized.
#[expect(
    clippy::single_call_fn,
    reason = "backfill logic kept separate for clarity; called from load_or_create_keymap"
)]
fn backfill_from_preset(bindings: &mut HashMap<Action, Vec<KeyBind>>, preset_name: &str) {
    let Some(default_toml) = preset_toml(preset_name) else {
        return;
    };
    let Ok(default_config): Result<KeyConfig, _> = toml::from_str(default_toml) else {
        return;
    };
    let defaults = bindings_from_config(&default_config);
    let mut filled = Vec::new();
    // Collect keys to insert first, then apply — avoids iterating a HashMap
    // directly (clippy::iter_over_hash_type).
    let missing: Vec<_> = defaults
        .iter()
        .filter(|(action, _)| !bindings.contains_key(action))
        .map(|(action, keybinds)| (*action, keybinds.clone()))
        .collect();
    for (action, keybinds) in missing {
        filled.push(format!("{action:?}"));
        bindings.insert(action, keybinds);
    }
    if !filled.is_empty() {
        filled.sort();
        tracing::info!(
            "Backfilled {} missing key(s) from \"{preset_name}\" preset: {}",
            filled.len(),
            filled.join(", ")
        );
    }
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
// allow: single-call in bin target, multi-call in test target (called from unit tests)
#[allow(clippy::single_call_fn)]
fn parse_key_string(input: &str) -> Option<KeyBind> {
    let normalized = input.trim().to_lowercase();
    let parts: Vec<&str> = normalized.split('+').collect();

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
        ch if ch.len() == 1 => KeyCode::Char(ch.chars().next()?),
        _ => return None,
    };

    Some((code, modifiers))
}

/// Pick the best keybinding for the current platform.
///
/// On macOS, Alt-modified bindings are skipped because the Option key sends
/// special characters in most terminal emulators. Returns the first
/// non-Alt binding, or falls back to the first binding if all use Alt.
#[expect(
    clippy::single_call_fn,
    reason = "platform-aware bind selection kept separate for clarity"
)]
fn pick_platform_bind(binds: &[KeyBind]) -> Option<&KeyBind> {
    use crossterm::event::KeyModifiers;
    if cfg!(target_os = "macos") {
        // Prefer non-Alt bindings on macOS
        let non_alt = binds
            .iter()
            .find(|bind| !bind.1.contains(KeyModifiers::ALT));
        non_alt.or_else(|| binds.first())
    } else {
        binds.first()
    }
}

/// Format a `KeyBind` as a compact human-readable string for UI display.
#[expect(
    clippy::single_call_fn,
    reason = "compact key display formatter kept separate for clarity"
)]
fn format_key_display(bind: KeyBind) -> String {
    use crossterm::event::KeyModifiers;

    let ctrl = bind.1.contains(KeyModifiers::CONTROL);
    let alt = bind.1.contains(KeyModifiers::ALT);
    let shift = bind.1.contains(KeyModifiers::SHIFT) && !matches!(bind.0, KeyCode::BackTab);

    let prefix = match (ctrl, alt, shift) {
        (true, false, true) => "Ctrl+Shift+",
        (true, false, false) => "Ctrl+",
        (false, true, true) => "Alt+Shift+",
        (false, true, false) => "Alt+",
        (true, true, false) => "Ctrl+Alt+",
        (true, true, true) => "Ctrl+Alt+Shift+",
        (false, false, true) => "Shift+",
        (false, false, false) => "",
    };

    let key = match bind.0 {
        KeyCode::Char(ch) => ch.to_ascii_uppercase().to_string(),
        KeyCode::F(n) => return format!("F{n}"),
        KeyCode::Up => "↑".to_owned(),
        KeyCode::Down => "↓".to_owned(),
        KeyCode::Left => "←".to_owned(),
        KeyCode::Right => "→".to_owned(),
        KeyCode::PageUp => "PgUp".to_owned(),
        KeyCode::PageDown => "PgDn".to_owned(),
        KeyCode::Home => "Home".to_owned(),
        KeyCode::End => "End".to_owned(),
        KeyCode::Enter => "Enter".to_owned(),
        KeyCode::Tab => "Tab".to_owned(),
        KeyCode::BackTab => return "Shift+Tab".to_owned(),
        KeyCode::Backspace => "Bksp".to_owned(),
        KeyCode::Delete => "Del".to_owned(),
        KeyCode::Esc => "Esc".to_owned(),
        KeyCode::Insert
        | KeyCode::Null
        | KeyCode::CapsLock
        | KeyCode::ScrollLock
        | KeyCode::NumLock
        | KeyCode::PrintScreen
        | KeyCode::Pause
        | KeyCode::Menu
        | KeyCode::KeypadBegin
        | KeyCode::Modifier(_)
        | KeyCode::Media(_) => "?".to_owned(),
    };

    format!("{prefix}{key}")
}

// ═══════════════════════════════════════════════════════════════════════════
// Key matching (runtime)
// ═══════════════════════════════════════════════════════════════════════════

/// Check if a key event matches a single keybinding.
#[inline]
#[expect(
    clippy::single_call_fn,
    reason = "matching logic kept separate for clarity"
)]
const fn matches_bind(key: KeyEvent, bind: KeyBind) -> bool {
    matches_code(key.code, bind.0) && key.modifiers.contains(bind.1)
}

/// Compare key codes (handles Char case for control keys).
#[inline]
#[expect(
    clippy::single_call_fn,
    reason = "code comparison kept separate for clarity"
)]
const fn matches_code(actual: KeyCode, expected: KeyCode) -> bool {
    match (actual, expected) {
        (KeyCode::Char(actual_ch), KeyCode::Char(expected_ch)) => {
            actual_ch as u32 == expected_ch as u32
        }
        (KeyCode::F(actual_n), KeyCode::F(expected_n)) => actual_n == expected_n,
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

#[cfg(test)]
#[path = "keys_tests.rs"]
mod tests;
