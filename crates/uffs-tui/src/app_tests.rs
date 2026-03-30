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
            created: 0,
            accessed: 0,
            flags: 0,
            allocated: 0,
            descendants: 0,
            treesize: 0,
            tree_allocated: 0,
        },
        DisplayRow {
            drive: 'C',
            path: "C:\\b".to_owned(),
            name: "b".to_owned(),
            size: 0,
            is_directory: false,
            modified: 0,
            created: 0,
            accessed: 0,
            flags: 0,
            allocated: 0,
            descendants: 0,
            treesize: 0,
            tree_allocated: 0,
        },
        DisplayRow {
            drive: 'C',
            path: "C:\\c".to_owned(),
            name: "c".to_owned(),
            size: 0,
            is_directory: true,
            modified: 0,
            created: 0,
            accessed: 0,
            flags: 0,
            allocated: 0,
            descendants: 0,
            treesize: 0,
            tree_allocated: 0,
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
        created: 0,
        accessed: 0,
        flags: 0,
        allocated: 0,
        descendants: 0,
        treesize: 0,
        tree_allocated: 0,
    }];
    // textarea starts empty → searches for "*" (all files)
    // With no drives loaded, this triggers the "no drives" error
    app.search();
    assert!(app.error.is_some());
}
