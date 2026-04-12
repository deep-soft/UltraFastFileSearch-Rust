// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

use super::*;

#[test]
fn test_navigation() {
    let mut app = App::new();
    app.results = vec![
        DisplayRow::new(0, 'C', "C:\\a".to_owned(), 0, false, 0, 0, 0, 0, 0, 0, 0, 0),
        DisplayRow::new(0, 'C', "C:\\b".to_owned(), 0, false, 0, 0, 0, 0, 0, 0, 0, 0),
        DisplayRow::new(0, 'C', "C:\\c".to_owned(), 0, true, 0, 0, 0, 0, 0, 0, 0, 0),
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
    app.results = vec![DisplayRow::new(
        0,
        'C',
        "C:\\x".to_owned(),
        0,
        false,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    )];
    // textarea starts empty → searches for "*" (all files)
    // With no drives loaded, this triggers the "no drives" error
    app.search();
    assert!(app.error.is_some());
}
