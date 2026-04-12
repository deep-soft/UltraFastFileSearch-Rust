// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

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
        ..Default::default()
    };
    let cli = search_state_to_cli("*.rs", &state);
    assert_eq!(
        cli,
        "uffs.exe \"*.rs\" --files-only --name-only --case --word"
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
        cli_to_search_state("uffs.exe \"*.rs\" --case --word --name-only --files-only").unwrap();
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
        ..Default::default()
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
            ..Default::default()
        },
    }];
    let output = serialize_history_file(&entries);
    assert_eq!(
        output,
        "uffs.exe \"*.rs\" --files-only --name-only --case --word\n"
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
    // We ship 26 example searches across four buckets
    assert_eq!(
        result.entries.len(),
        26,
        "Expected 26 default history entries, got {}",
        result.entries.len()
    );
}
