//! Centralized keybinding definitions for the TUI.
//!
//! ALL keybindings are defined here as constants. The event loop in `main.rs`
//! uses these to match key events. To change a keybinding, update it here —
//! it propagates everywhere automatically.
//!
//! ## Convention
//!
//! Each binding is a `(KeyCode, KeyModifiers)` tuple with a descriptive name.
//! The help bar in `main.rs` should reference these same constants for labels.

use crossterm::event::{KeyCode, KeyModifiers};

/// A keybinding definition: (key code, required modifiers).
pub type KeyBind = (KeyCode, KeyModifiers);

// ─── Navigation ─────────────────────────────────────────────────────────

/// Move selection down in results.
pub const NAV_DOWN: KeyBind = (KeyCode::Down, KeyModifiers::NONE);
/// Move selection up in results.
pub const NAV_UP: KeyBind = (KeyCode::Up, KeyModifiers::NONE);
/// Page down in results.
pub const NAV_PAGE_DOWN: KeyBind = (KeyCode::PageDown, KeyModifiers::NONE);
/// Page up in results.
pub const NAV_PAGE_UP: KeyBind = (KeyCode::PageUp, KeyModifiers::NONE);
/// Show selected file path in status bar.
pub const SHOW_PATH: KeyBind = (KeyCode::Enter, KeyModifiers::NONE);

// ─── Sort ───────────────────────────────────────────────────────────────

/// Cycle sort column (Name → Size → Modified → Path → Drive → Extension → Type).
pub const SORT_CYCLE: KeyBind = (KeyCode::Tab, KeyModifiers::NONE);
/// Toggle sort direction (ascending ↔ descending).
pub const SORT_DIRECTION: KeyBind = (KeyCode::BackTab, KeyModifiers::SHIFT);

// ─── Search mode toggles ───────────────────────────────────────────────

/// Toggle name-only matching.
pub const TOGGLE_NAME_ONLY: KeyBind = (KeyCode::F(2), KeyModifiers::NONE);
/// Cycle file/directory filter (All → Files → Dirs).
pub const TOGGLE_FILTER: KeyBind = (KeyCode::F(3), KeyModifiers::NONE);
/// Toggle case-sensitive search.
pub const TOGGLE_CASE_SENSITIVE: KeyBind = (KeyCode::F(7), KeyModifiers::NONE);
/// Toggle whole-word search.
pub const TOGGLE_WHOLE_WORD: KeyBind = (KeyCode::F(8), KeyModifiers::NONE);

// ─── Search history ─────────────────────────────────────────────────────

/// Previous search in history.
pub const HISTORY_BACK: KeyBind = (KeyCode::Char('p'), KeyModifiers::CONTROL);
/// Next search in history.
pub const HISTORY_FORWARD: KeyBind = (KeyCode::Char('n'), KeyModifiers::CONTROL);

// ─── Actions ────────────────────────────────────────────────────────────

/// Refresh all drives (reload MFT / .uffs cache).
pub const REFRESH: KeyBind = (KeyCode::F(5), KeyModifiers::NONE);
/// Refresh all drives (alternative keybinding).
pub const REFRESH_ALT: KeyBind = (KeyCode::Char('r'), KeyModifiers::CONTROL);
/// Quit the TUI.
pub const QUIT: KeyBind = (KeyCode::Char('q'), KeyModifiers::CONTROL);

// ─── Text editing ───────────────────────────────────────────────────────

/// Clear the search input line.
pub const CLEAR_LINE: KeyBind = (KeyCode::Char('u'), KeyModifiers::CONTROL);
/// Undo last text edit.
pub const UNDO: KeyBind = (KeyCode::Char('z'), KeyModifiers::CONTROL);
/// Redo last undone edit.
pub const REDO: KeyBind = (KeyCode::Char('y'), KeyModifiers::CONTROL);
/// Select all text.
pub const SELECT_ALL: KeyBind = (KeyCode::Char('a'), KeyModifiers::CONTROL);

/// Check if a key event matches a keybinding.
#[inline]
#[must_use]
pub const fn matches(key: crossterm::event::KeyEvent, bind: KeyBind) -> bool {
    matches_code(key.code, bind.0) && key.modifiers.contains(bind.1)
}

/// Compare key codes (handles Char case for control keys).
#[inline]
#[must_use]
#[expect(
    clippy::single_call_fn,
    reason = "separated for readability; key code comparison is a distinct concern"
)]
const fn matches_code(actual: KeyCode, expected: KeyCode) -> bool {
    match (actual, expected) {
        (KeyCode::Char(ch_a), KeyCode::Char(ch_b)) => ch_a as u32 == ch_b as u32,
        (KeyCode::F(fn_a), KeyCode::F(fn_b)) => fn_a == fn_b,
        (KeyCode::Down, KeyCode::Down)
        | (KeyCode::Up, KeyCode::Up)
        | (KeyCode::PageDown, KeyCode::PageDown)
        | (KeyCode::PageUp, KeyCode::PageUp)
        | (KeyCode::Enter, KeyCode::Enter)
        | (KeyCode::Tab, KeyCode::Tab)
        | (KeyCode::BackTab, KeyCode::BackTab) => true,
        _ => false,
    }
}

/// Help text label for a keybinding (for the help bar).
#[must_use]
#[expect(dead_code, reason = "public API for help bar rendering; wired in future")]
pub const fn label(bind: KeyBind) -> &'static str {
    match bind {
        (KeyCode::F(2), _) => "F2",
        (KeyCode::F(3), _) => "F3",
        (KeyCode::F(5), _) => "F5",
        (KeyCode::F(7), _) => "F7",
        (KeyCode::F(8), _) => "F8",
        (KeyCode::Tab, _) => "Tab",
        (KeyCode::BackTab, _) => "S-Tab",
        (KeyCode::Up, _) => "↑",
        (KeyCode::Down, _) => "↓",
        (KeyCode::PageUp, _) => "PgUp",
        (KeyCode::PageDown, _) => "PgDn",
        (KeyCode::Enter, _) => "Enter",
        (KeyCode::Char('q'), _) => "Ctrl+Q",
        (KeyCode::Char('u'), _) => "Ctrl+U",
        (KeyCode::Char('z'), _) => "Ctrl+Z",
        (KeyCode::Char('y'), _) => "Ctrl+Y",
        (KeyCode::Char('a'), _) => "Ctrl+A",
        (KeyCode::Char('r'), _) => "Ctrl+R",
        (KeyCode::Char('p'), _) => "Ctrl+P",
        (KeyCode::Char('n'), _) => "Ctrl+N",
        _ => "?",
    }
}
