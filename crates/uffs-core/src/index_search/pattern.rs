// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! Pattern compilation and matching for direct `MftIndex` search.
//!
//! Uses [`uffs_text::case_fold::CaseFold`] for NTFS-compatible case-insensitive
//! matching. Pattern strings are pre-folded to `Vec<u16>` at compile time;
//! input strings are folded char-by-char at match time.  This is
//! zero-allocation for the common Exact/Prefix/Suffix/Contains variants.

use std::collections::HashSet;

use aho_corasick::AhoCorasick;
use regex::Regex;
use uffs_text::case_fold::CaseFold;

use crate::compiled_pattern::{GlobKind, classify_glob};
use crate::error::{CoreError, Result};
use crate::pattern::{ParsedPattern, PatternType};

/// Compiled pattern for direct matching on `MftIndex`.
///
/// This mirrors `CompiledPattern` but generates match functions instead of
/// Polars expressions.  Case-insensitive matching uses NTFS `$UpCase` folding
/// via [`CaseFold`] instead of ASCII-only `to_ascii_lowercase()`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum IndexPattern {
    /// Always matches (e.g., `*`).
    Any,

    /// Exact string match.
    Exact {
        /// The exact value to match (case-sensitive).
        value: String,
        /// Pre-folded codepoints for case-insensitive matching.
        folded: Vec<u16>,
    },

    /// Prefix match (e.g., `foo*`).
    Prefix {
        /// The prefix to match (case-sensitive).
        prefix: String,
        /// Pre-folded codepoints for case-insensitive matching.
        folded: Vec<u16>,
    },

    /// Suffix match (e.g., `*bar`, `*.txt`).
    Suffix {
        /// The suffix to match (case-sensitive).
        suffix: String,
        /// Pre-folded codepoints for case-insensitive matching.
        folded: Vec<u16>,
    },

    /// Literal substring match (e.g., `*needle*`).
    Contains {
        /// The substring to search for (case-sensitive).
        needle: String,
        /// Pre-folded codepoints for case-insensitive matching.
        folded: Vec<u16>,
    },

    /// Prefix AND suffix match (e.g., `foo*bar`).
    PrefixSuffix {
        /// The prefix to match (case-sensitive).
        prefix: String,
        /// The suffix to match (case-sensitive).
        suffix: String,
        /// Pre-folded prefix codepoints.
        prefix_folded: Vec<u16>,
        /// Pre-folded suffix codepoints.
        suffix_folded: Vec<u16>,
    },

    /// Multiple exact matches (hash set lookup).
    ExactSet {
        /// Set of exact values (case-sensitive).
        values: HashSet<String>,
        /// Pre-folded sets for case-insensitive matching.
        folded_set: HashSet<Vec<u16>>,
    },

    /// Multiple suffix matches (e.g., extensions).
    SuffixSet {
        /// List of suffixes (case-sensitive).
        suffixes: Vec<String>,
        /// Pre-folded suffix codepoints for case-insensitive matching.
        suffixes_folded: Vec<Vec<u16>>,
    },

    /// Multiple literal substrings (Aho-Corasick).
    ContainsAny {
        /// Aho-Corasick automaton for case-sensitive matching.
        automaton: AhoCorasick,
        /// Aho-Corasick automaton for case-insensitive matching (folded).
        automaton_folded: AhoCorasick,
        /// Original patterns for debugging.
        patterns: Vec<String>,
    },

    /// Fallback to regex.
    Regex {
        /// Compiled regex for case-sensitive matching.
        regex: Regex,
        /// Compiled regex for case-insensitive matching.
        regex_lower: Regex,
    },

    /// OR: match if ANY sub-pattern matches (e.g., `*.txt|*.log`).
    Or {
        /// Sub-patterns — record matches if any one matches.
        patterns: Vec<Self>,
    },
}

impl IndexPattern {
    /// Check if a string matches this pattern.
    ///
    /// `fold` provides NTFS-compatible case folding for case-insensitive
    /// matching.  Most variants use zero-allocation char-by-char fold
    /// comparison.  `ExactSet` and `ContainsAny` may allocate (rare).
    #[inline]
    #[must_use]
    pub fn matches(&self, input: &str, case_sensitive: bool, fold: CaseFold) -> bool {
        match self {
            Self::Any => true,
            Self::Exact { value, folded } => {
                if case_sensitive {
                    input == value
                } else {
                    fold.eq_folded(input, folded)
                }
            }
            Self::Prefix { prefix, folded } => {
                if case_sensitive {
                    input.starts_with(prefix.as_str())
                } else {
                    fold.starts_with_folded(input, folded)
                }
            }
            Self::Suffix { suffix, folded } => {
                if case_sensitive {
                    input.ends_with(suffix.as_str())
                } else {
                    fold.ends_with_folded(input, folded)
                }
            }
            Self::Contains { needle, folded } => {
                if case_sensitive {
                    input.contains(needle.as_str())
                } else {
                    fold.contains_folded(input, folded)
                }
            }
            Self::PrefixSuffix {
                prefix,
                suffix,
                prefix_folded,
                suffix_folded,
            } => {
                if case_sensitive {
                    input.starts_with(prefix.as_str()) && input.ends_with(suffix.as_str())
                } else {
                    fold.starts_with_folded(input, prefix_folded)
                        && fold.ends_with_folded(input, suffix_folded)
                }
            }
            Self::ExactSet { values, folded_set } => {
                if case_sensitive {
                    values.contains(input)
                } else {
                    // Fold input to Vec<u16> for HashSet lookup — alloc per call,
                    // but ExactSet is rare.
                    folded_set.contains(&fold.fold_to_u16(input))
                }
            }
            Self::SuffixSet {
                suffixes,
                suffixes_folded,
            } => {
                if case_sensitive {
                    suffixes.iter().any(|suf| input.ends_with(suf.as_str()))
                } else {
                    suffixes_folded
                        .iter()
                        .any(|suf| fold.ends_with_folded(input, suf))
                }
            }
            Self::ContainsAny {
                automaton,
                automaton_folded,
                ..
            } => {
                if case_sensitive {
                    automaton.is_match(input)
                } else {
                    // Aho-Corasick needs a folded string — use fold_into with
                    // a thread-local buffer (ContainsAny is rare).
                    thread_local! {
                        static BUF: core::cell::RefCell<Vec<u8>> =
                            core::cell::RefCell::new(Vec::with_capacity(256));
                    }
                    BUF.with(|cell| {
                        let mut buf = cell.borrow_mut();
                        let folded_str = fold.fold_into(input, &mut buf);
                        automaton_folded.is_match(folded_str)
                    })
                }
            }
            Self::Regex { regex, regex_lower } => {
                if case_sensitive {
                    regex.is_match(input)
                } else {
                    regex_lower.is_match(input)
                }
            }
            Self::Or { patterns } => patterns
                .iter()
                .any(|pat| pat.matches(input, case_sensitive, fold)),
        }
    }
}

/// Compile a glob pattern into an `IndexPattern`.
///
/// Uses the default `$UpCase` table for pre-folding pattern strings.
///
/// # Errors
///
/// Returns an error if the pattern is invalid (e.g., malformed glob or regex).
pub fn compile_index_pattern(pattern: &str) -> Result<IndexPattern> {
    compile_index_pattern_with_fold(pattern, CaseFold::default_table())
}

/// Compile a glob pattern into an `IndexPattern` with a specific `CaseFold`.
///
/// # Errors
///
/// Returns an error if the pattern is invalid (e.g., malformed glob or regex).
pub fn compile_index_pattern_with_fold(pattern: &str, fold: CaseFold) -> Result<IndexPattern> {
    let kind = classify_glob(pattern);
    match kind {
        GlobKind::Any => Ok(IndexPattern::Any),
        GlobKind::Exact(value) => {
            let folded = fold.fold_to_u16(&value);
            Ok(IndexPattern::Exact { value, folded })
        }
        GlobKind::Prefix(prefix) => {
            let folded = fold.fold_to_u16(&prefix);
            Ok(IndexPattern::Prefix { prefix, folded })
        }
        GlobKind::Suffix(suffix) => {
            let folded = fold.fold_to_u16(&suffix);
            Ok(IndexPattern::Suffix { suffix, folded })
        }
        GlobKind::Extension(ext) => {
            let suffix = format!(".{ext}");
            let folded = fold.fold_to_u16(&suffix);
            Ok(IndexPattern::Suffix { suffix, folded })
        }
        GlobKind::Contains(needle) => {
            let folded = fold.fold_to_u16(&needle);
            Ok(IndexPattern::Contains { needle, folded })
        }
        GlobKind::PrefixSuffix { prefix, suffix } => {
            let prefix_folded = fold.fold_to_u16(&prefix);
            let suffix_folded = fold.fold_to_u16(&suffix);
            Ok(IndexPattern::PrefixSuffix {
                prefix,
                suffix,
                prefix_folded,
                suffix_folded,
            })
        }
        GlobKind::Complex(glob_pattern) => {
            // globset treats `\` as an escape character, but our patterns use
            // `\` as a Windows path separator.  Convert to `/` so globset
            // interprets them as directory separators.
            let mut globset_pattern = glob_pattern.replace('\\', "/");
            // A leading `/` means "match at any depth" (like Everything),
            // not "anchored at root".  Prepend `**/` so globset allows any
            // prefix before the first segment.
            if globset_pattern.starts_with('/') {
                globset_pattern = format!("**{globset_pattern}");
            }
            let glob =
                globset::Glob::new(&globset_pattern).map_err(|err| CoreError::InvalidGlob {
                    pattern: glob_pattern.clone(),
                    reason: err.to_string(),
                })?;
            let raw_regex = glob.regex();
            // globset emits `(?-u)` which disables Unicode mode — that makes
            // `[/\\]` potentially match invalid UTF-8, which `regex::Regex`
            // rejects.  Our paths are always valid UTF-8, so strip the flag.
            let regex_str = raw_regex.strip_prefix("(?-u)").unwrap_or(raw_regex);
            // globset emits `/` separators in the regex; our paths use `\`.
            // Replace the separator class so the regex matches both.
            let regex_str_win = regex_str.replace('/', r"[/\\]");
            let regex = Regex::new(&regex_str_win).map_err(|err| CoreError::InvalidRegex {
                pattern: regex_str_win.clone(),
                reason: err.to_string(),
            })?;
            let regex_lower = Regex::new(&format!("(?i){regex_str_win}")).map_err(|err| {
                CoreError::InvalidRegex {
                    pattern: regex_str_win,
                    reason: err.to_string(),
                }
            })?;
            Ok(IndexPattern::Regex { regex, regex_lower })
        }
    }
}

/// Compile a `ParsedPattern` into an `IndexPattern`.
///
/// Uses the default `$UpCase` table for pre-folding.
///
/// # Errors
///
/// Returns an error if the pattern is invalid (e.g., malformed glob or regex).
pub fn compile_parsed_pattern(parsed: &ParsedPattern) -> Result<IndexPattern> {
    compile_parsed_pattern_with_fold(parsed, CaseFold::default_table())
}

/// Compile a `ParsedPattern` into an `IndexPattern` with a specific `CaseFold`.
///
/// # Errors
///
/// Returns an error if the pattern is invalid (e.g., malformed glob or regex).
pub fn compile_parsed_pattern_with_fold(
    parsed: &ParsedPattern,
    fold: CaseFold,
) -> Result<IndexPattern> {
    // OR operator: split on | and compile each part.
    // "*.txt|*.log" → Or([Suffix(".txt"), Suffix(".log")])
    let pat = parsed.pattern();
    if parsed.pattern_type() != PatternType::Regex && pat.contains('|') {
        let parts: Vec<&str> = pat.split('|').collect();
        if parts.len() > 1 {
            let sub_patterns: Result<Vec<IndexPattern>> = parts
                .iter()
                .map(|part| compile_index_pattern_with_fold(part.trim(), fold))
                .collect();
            return Ok(IndexPattern::Or {
                patterns: sub_patterns?,
            });
        }
    }

    match parsed.pattern_type() {
        PatternType::Glob => compile_index_pattern_with_fold(parsed.pattern(), fold),
        PatternType::Regex => {
            let pattern_str = parsed.pattern();
            // Auto-anchor with $ if the pattern isn't already end-anchored.
            // Rust's regex::is_match() does substring matching by default,
            // so >.*\.(jpg|png) would match "icon.png.vir" (finding .png
            // mid-string). Users expect end-of-string matching: the file
            // must END with the extension. Adding $ fixes this to match
            // expected behavior and user intent.
            let anchored = if pattern_str.ends_with('$') {
                pattern_str.to_owned()
            } else {
                format!("{pattern_str}$")
            };
            let regex = Regex::new(&anchored).map_err(|err| CoreError::InvalidRegex {
                pattern: pattern_str.to_owned(),
                reason: err.to_string(),
            })?;
            let regex_lower =
                Regex::new(&format!("(?i){anchored}")).map_err(|err| CoreError::InvalidRegex {
                    pattern: pattern_str.to_owned(),
                    reason: err.to_string(),
                })?;
            Ok(IndexPattern::Regex { regex, regex_lower })
        }
        PatternType::Literal => {
            let needle = parsed.pattern().to_owned();
            let folded = fold.fold_to_u16(&needle);
            Ok(IndexPattern::Contains { needle, folded })
        }
    }
}

/// Compile multiple extension patterns into a `SuffixSet`.
///
/// Uses the default `$UpCase` table for pre-folding.
#[must_use]
pub fn compile_extensions(extensions: &[&str]) -> IndexPattern {
    compile_extensions_with_fold(extensions, CaseFold::default_table())
}

/// Compile multiple extension patterns into a `SuffixSet` with a specific
/// `CaseFold`.
#[must_use]
pub fn compile_extensions_with_fold(extensions: &[&str], fold: CaseFold) -> IndexPattern {
    let suffixes: Vec<String> = extensions
        .iter()
        .map(|ext| {
            if ext.starts_with('.') {
                ext.to_string()
            } else {
                format!(".{ext}")
            }
        })
        .collect();
    let suffixes_folded: Vec<Vec<u16>> = suffixes.iter().map(|suf| fold.fold_to_u16(suf)).collect();
    IndexPattern::SuffixSet {
        suffixes,
        suffixes_folded,
    }
}
