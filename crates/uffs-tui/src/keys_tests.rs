// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;

#[expect(
    clippy::single_call_fn,
    reason = "test-only helper for roundtrip verification"
)]
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
        | KeyCode::Media(_) => "?",
    };

    if parts.is_empty() {
        key.to_owned()
    } else {
        format!("{}+{key}", parts.join("+"))
    }
}

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
    let cases = ["ctrl+q", "f5", "enter", "down", "pageup"];
    for case in cases {
        let bind = parse_key_string(case).unwrap();
        let formatted = format_key_bind(bind);
        let reparsed = parse_key_string(&formatted).unwrap();
        assert_eq!(bind, reparsed, "roundtrip failed for \"{case}\"");
    }
}

#[test]
fn test_windows_preset_parses() {
    let (keymap, preset) = parse_toml(PRESET_WINDOWS).expect("Windows preset should parse");
    assert_eq!(preset.as_deref(), Some("windows"));
    assert!(
        keymap.matches(
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
            Action::Quit
        ),
        "Ctrl+Q should match Quit"
    );
    assert!(
        keymap.matches(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
            Action::Refresh
        ),
        "Ctrl+R should match Refresh"
    );
    assert!(
        keymap.matches(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT),
            Action::ToggleCaseSensitive
        ),
        "Alt+C should match ToggleCaseSensitive"
    );
    assert!(
        keymap.matches(
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT),
            Action::ToggleWholeWord
        ),
        "Alt+W should match ToggleWholeWord"
    );
    assert!(
        keymap.matches(
            KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL),
            Action::Paste
        ),
        "Ctrl+V should match Paste"
    );
}

#[test]
fn test_emacs_preset_parses() {
    let (keymap, preset) = parse_toml(PRESET_EMACS).expect("Emacs preset should parse");
    assert_eq!(preset.as_deref(), Some("emacs"));
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
