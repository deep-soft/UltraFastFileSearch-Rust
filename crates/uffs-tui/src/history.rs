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
///
/// Mirrors every user-facing CLI flag so that history entries fully reproduce
/// the original search.  Fields that are `None` / `false` / default are omitted
/// from the serialised CLI string.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "mirrors CLI flags; each bool is an independent toggle"
)]
pub struct SearchState {
    // ── toggles ────────────────────────────────────────────────────────
    /// Case-sensitive matching (`--case`).
    pub case_sensitive: bool,
    /// Smart case: auto case-sensitive when pattern has uppercase
    /// (`--smart-case`).
    pub smart_case: bool,
    /// Whole-word matching (`--word`).
    pub whole_word: bool,
    /// Match filename only, not full path (`--name-only`).
    pub name_only: bool,
    /// Hide NTFS system files starting with `$` (`--hide-system`).
    pub hide_system: bool,

    // ── filter mode ────────────────────────────────────────────────────
    /// File / directory filter (`--files-only`, `--dirs-only`).
    pub filter: FilterMode,

    // ── sort ────────────────────────────────────────────────────────────
    /// Sort specification, e.g. `"name:asc,modified:desc"` (`--sort`).
    pub sort: Option<String>,

    // ── limit ───────────────────────────────────────────────────────────
    /// Maximum number of results (`--limit`).  `None` = backend default.
    pub limit: Option<u32>,

    // ── size range ──────────────────────────────────────────────────────
    /// Minimum file size in bytes (`--min-size`).
    pub min_size: Option<u64>,
    /// Maximum file size in bytes (`--max-size`).
    pub max_size: Option<u64>,

    // ── time range (modified) ──────────────────────────────────────────
    /// Only files modified within duration / after date (`--newer`).
    pub newer: Option<String>,
    /// Only files modified before duration / date (`--older`).
    pub older: Option<String>,

    // ── time range (created) ───────────────────────────────────────────
    /// Only files created within duration / after date (`--newer-created`).
    pub newer_created: Option<String>,
    /// Only files created before duration / date (`--older-created`).
    pub older_created: Option<String>,

    // ── time range (accessed) ──────────────────────────────────────────
    /// Only files accessed within duration / after date (`--newer-accessed`).
    pub newer_accessed: Option<String>,
    /// Only files accessed before duration / date (`--older-accessed`).
    pub older_accessed: Option<String>,

    // ── descendants range ──────────────────────────────────────────
    /// Minimum descendant count (`--min-descendants`).
    pub min_descendants: Option<u32>,
    /// Maximum descendant count (`--max-descendants`).
    pub max_descendants: Option<u32>,

    // ── pattern filters ────────────────────────────────────────────────
    /// Exclude files matching this pattern (`--exclude`).
    pub exclude: Option<String>,
    /// Filter by file extension(s), comma-separated (`--ext`).
    pub ext: Option<String>,
    /// NTFS attribute filter, e.g. `"hidden,!system"` (`--attr`).
    pub attr: Option<String>,

    // ── length filters ─────────────────────────────────────────────
    /// Minimum filename length in characters (`--min-name-length`).
    pub min_name_len: Option<u16>,
    /// Maximum filename length in characters (`--max-name-length`).
    pub max_name_len: Option<u16>,
    /// Minimum full-path length in characters (`--min-path-length`).
    pub min_path_len: Option<u16>,
    /// Maximum full-path length in characters (`--max-path-length`).
    pub max_path_len: Option<u16>,

    // ── size-on-disk filters ───────────────────────────────────────
    /// Minimum allocated (on-disk) size in bytes (`--min-size-on-disk`).
    pub min_allocated: Option<u64>,
    /// Maximum allocated (on-disk) size in bytes (`--max-size-on-disk`).
    pub max_allocated: Option<u64>,

    // ── month-of-year filter ───────────────────────────────────────
    /// Month/quarter filter spec, e.g. `"jan"`, `"Q1"` (`--month`).
    pub month: Option<String>,

    // ── column selection ───────────────────────────────────────────
    /// Visible columns and their order, e.g. `"name,size,modified,path"`
    /// (`--columns`).  `None` = default column set.
    pub columns: Option<String>,
}

// ═══════════════════════════════════════════════════════════════════════════
// CLI command serialization
// ═══════════════════════════════════════════════════════════════════════════

/// Serialize a search state into a `uffs` CLI command string.
///
/// Example output: `uffs.exe "*.rs" --case --files-only --sort name:asc
/// --limit 500`
#[must_use]
pub fn search_state_to_cli(pattern: &str, state: &SearchState) -> String {
    let mut parts = vec!["uffs.exe".to_owned()];

    // Quote the pattern if it contains spaces or special chars
    if pattern.contains(' ')
        || pattern.contains('*')
        || pattern.contains('?')
        || pattern.contains('>')
        || pattern.contains('|')
        || pattern.contains('$')
        || pattern.contains('\\')
    {
        parts.push(format!("\"{pattern}\""));
    } else {
        parts.push(pattern.to_owned());
    }

    // ─────────────────────────────────────────────────────────────
    // Emit switches in the canonical pipeline order documented in
    // DEFAULT_HISTORY:
    //   pattern → scope → match-flags → attr-filter →
    //   time-filter → size-filter → descendant-filter →
    //   exclude/ext → sort → columns → limit
    // `--limit` is always last: it truncates the final result set
    // after all filtering and sorting.
    // ─────────────────────────────────────────────────────────────

    // 1. scope ──────────────────────────────────────────────────────
    match state.filter {
        FilterMode::All => {}
        FilterMode::FilesOnly => parts.push("--files-only".to_owned()),
        FilterMode::DirsOnly => parts.push("--dirs-only".to_owned()),
    }
    if state.name_only {
        parts.push("--name-only".to_owned());
    }

    // 2. match flags ────────────────────────────────────────────────
    if state.case_sensitive {
        parts.push("--case".to_owned());
    }
    if state.smart_case {
        parts.push("--smart-case".to_owned());
    }
    if state.whole_word {
        parts.push("--word".to_owned());
    }

    // 3. attribute filters ──────────────────────────────────────────
    if state.hide_system {
        parts.push("--hide-system".to_owned());
    }
    push_opt(&mut parts, "--attr", state.attr.as_ref());

    // 4. time filters ───────────────────────────────────────────────
    push_opt(&mut parts, "--newer", state.newer.as_ref());
    push_opt(&mut parts, "--older", state.older.as_ref());
    push_opt(&mut parts, "--newer-created", state.newer_created.as_ref());
    push_opt(&mut parts, "--older-created", state.older_created.as_ref());
    push_opt(
        &mut parts,
        "--newer-accessed",
        state.newer_accessed.as_ref(),
    );
    push_opt(
        &mut parts,
        "--older-accessed",
        state.older_accessed.as_ref(),
    );

    // 5. size range ─────────────────────────────────────────────────
    if let Some(min) = state.min_size {
        parts.push(format!("--min-size {min}"));
    }
    if let Some(max) = state.max_size {
        parts.push(format!("--max-size {max}"));
    }

    // 6. descendants range ──────────────────────────────────────────
    if let Some(min) = state.min_descendants {
        parts.push(format!("--min-descendants {min}"));
    }
    if let Some(max) = state.max_descendants {
        parts.push(format!("--max-descendants {max}"));
    }

    // 6b. length filters ─────────────────────────────────────────────
    if let Some(len) = state.min_name_len {
        parts.push(format!("--min-name-length {len}"));
    }
    if let Some(len) = state.max_name_len {
        parts.push(format!("--max-name-length {len}"));
    }
    if let Some(len) = state.min_path_len {
        parts.push(format!("--min-path-length {len}"));
    }
    if let Some(len) = state.max_path_len {
        parts.push(format!("--max-path-length {len}"));
    }

    // 6c. size-on-disk filters ─────────────────────────────────────
    if let Some(alloc) = state.min_allocated {
        parts.push(format!("--min-size-on-disk {alloc}"));
    }
    if let Some(alloc) = state.max_allocated {
        parts.push(format!("--max-size-on-disk {alloc}"));
    }

    // 6d. month filter ─────────────────────────────────────────────
    push_opt(&mut parts, "--month", state.month.as_ref());

    // 7. pattern filters ────────────────────────────────────────────
    push_opt(&mut parts, "--exclude", state.exclude.as_ref());
    push_opt(&mut parts, "--ext", state.ext.as_ref());

    // 8. sort ───────────────────────────────────────────────────────
    if let Some(sort) = &state.sort {
        parts.push(format!("--sort {sort}"));
    }

    // 9. column selection ───────────────────────────────────────────
    push_opt(&mut parts, "--columns", state.columns.as_ref());

    // 10. limit (always last — truncates after everything else) ────
    if let Some(limit) = state.limit {
        parts.push(format!("--limit {limit}"));
    }

    parts.join(" ")
}

/// Push `--flag value` to the parts list if the value is `Some`.
fn push_opt(parts: &mut Vec<String>, flag: &str, value: Option<&String>) {
    if let Some(val) = value {
        parts.push(format!("{flag} {val}"));
    }
}

/// Parse a `uffs` CLI command string back into pattern + search state.
///
/// Returns `None` if the line doesn't look like a uffs command.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "CLI parser with many flags; splitting would scatter related parsing logic"
)]
#[allow(clippy::single_call_fn)] // called once in production, multiple times in tests
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

    // Parse tokens into pattern + flags.
    // Flags that take a value (--sort, --limit, etc.) consume the NEXT token.
    let mut idx = 0;
    while idx < tokens.len() {
        let token = tokens.get(idx).map_or("", String::as_str);
        match token {
            // ── boolean flags ──────────────────────────────────────
            "--case" => state.case_sensitive = true,
            "--smart-case" => state.smart_case = true,
            "--word" => state.whole_word = true,
            "--name-only" => state.name_only = true,
            "--hide-system" => state.hide_system = true,
            "--files-only" => state.filter = FilterMode::FilesOnly,
            "--dirs-only" => state.filter = FilterMode::DirsOnly,
            // ── value flags ────────────────────────────────────────
            "--sort" => {
                idx += 1;
                state.sort = tokens.get(idx).cloned();
            }
            "--limit" => {
                idx += 1;
                state.limit = tokens.get(idx).and_then(|tok| tok.parse().ok());
            }
            "--min-size" => {
                idx += 1;
                state.min_size = tokens.get(idx).and_then(|tok| tok.parse().ok());
            }
            "--max-size" => {
                idx += 1;
                state.max_size = tokens.get(idx).and_then(|tok| tok.parse().ok());
            }
            "--min-descendants" => {
                idx += 1;
                state.min_descendants = tokens.get(idx).and_then(|tok| tok.parse().ok());
            }
            "--max-descendants" => {
                idx += 1;
                state.max_descendants = tokens.get(idx).and_then(|tok| tok.parse().ok());
            }
            "--newer" => {
                idx += 1;
                state.newer = tokens.get(idx).cloned();
            }
            "--older" => {
                idx += 1;
                state.older = tokens.get(idx).cloned();
            }
            "--newer-created" => {
                idx += 1;
                state.newer_created = tokens.get(idx).cloned();
            }
            "--older-created" => {
                idx += 1;
                state.older_created = tokens.get(idx).cloned();
            }
            "--newer-accessed" => {
                idx += 1;
                state.newer_accessed = tokens.get(idx).cloned();
            }
            "--older-accessed" => {
                idx += 1;
                state.older_accessed = tokens.get(idx).cloned();
            }
            "--exclude" => {
                idx += 1;
                state.exclude = tokens.get(idx).cloned();
            }
            "--ext" => {
                idx += 1;
                state.ext = tokens.get(idx).cloned();
            }
            "--attr" => {
                idx += 1;
                state.attr = tokens.get(idx).cloned();
            }
            "--columns" => {
                idx += 1;
                state.columns = tokens.get(idx).cloned();
            }
            "--min-name-length" => {
                idx += 1;
                state.min_name_len = tokens.get(idx).and_then(|tok| tok.parse().ok());
            }
            "--max-name-length" => {
                idx += 1;
                state.max_name_len = tokens.get(idx).and_then(|tok| tok.parse().ok());
            }
            "--min-path-length" => {
                idx += 1;
                state.min_path_len = tokens.get(idx).and_then(|tok| tok.parse().ok());
            }
            "--max-path-length" => {
                idx += 1;
                state.max_path_len = tokens.get(idx).and_then(|tok| tok.parse().ok());
            }
            "--min-size-on-disk" => {
                idx += 1;
                state.min_allocated = tokens.get(idx).and_then(|tok| tok.parse().ok());
            }
            "--max-size-on-disk" => {
                idx += 1;
                state.max_allocated = tokens.get(idx).and_then(|tok| tok.parse().ok());
            }
            "--month" => {
                idx += 1;
                state.month = tokens.get(idx).cloned();
            }
            // ── positional (pattern) ───────────────────────────────
            _ if !token.starts_with("--") && pattern.is_empty() => {
                if let Some(tok) = tokens.get(idx) {
                    pattern.clone_from(tok);
                }
            }
            _ => {} // ignore unknown flags (forward compat)
        }
        idx += 1;
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
///
/// Presets are grouped into four buckets inspired by cross-tool research:
///   Quick Find — name / path lookups
///   Find by Type — extension-based filtering
///   Triage & Cleanup — size, date, system files
///   Developer & Power User — code, configs, regex, path scoping
///
/// Every preset uses only MFT-available metadata (name, size, timestamps,
/// flags). Content search, duplicate detection, and non-NTFS sources are
/// out of scope.
pub const DEFAULT_HISTORY: &str = "\
# ── Quick Find ──────────────────────────────────────────
# Report §6 items 1-5: name/path lookup is the #1 workflow
#
# Switch order: pattern → scope → match-flags → attr-filter →
#   time-filter → size-filter → descendant-filter →
#   sort → columns → limit  (limit is always last: it truncates
#   the final result set after all filtering and sorting)

# Find a file by partial name (sorted A-Z, newest first)
uffs.exe \"report\" --files-only --sort name:asc,modified:desc --limit 500

# Find a folder by name (path-first layout, show descendants)
uffs.exe \"Projects\" --dirs-only --name-only --sort name:asc --columns path,name,descendants,drive

# Case-sensitive search for an exact filename
uffs.exe \"README.md\" --name-only --case

# Find files modified today
uffs.exe \"*\" --files-only --newer 1d --older 0d --sort modified:desc,name:asc --columns name,modified,size,path --limit 500

# Find recently accessed files (last 7 days — note: NTFS may disable access timestamps)
uffs.exe \"*\" --files-only --newer-accessed 7d --older-accessed 0d --sort accessed:desc,name:asc --columns name,accessed,modified,size,path --limit 500

# ── Find by Type ────────────────────────────────────────
# Report §6 items 3, 13, 14: type/extension is the first refinement

# Find documents (Office, PDF, text) — hide system files, newest first
uffs.exe \"*.pdf|*.docx|*.xlsx|*.pptx|*.txt\" --files-only --hide-system --sort modified:desc,name:asc --limit 500

# Find images sorted by size (largest first), show extension column
uffs.exe \"*.jpg|*.png|*.gif|*.bmp|*.webp|*.svg\" --files-only --sort size:desc,name:asc --columns name,ext,size,sizeondisk,path --limit 500

# Find videos — typically large, sort by size then newest
uffs.exe \"*.mp4|*.mkv|*.avi|*.mov|*.wmv\" --files-only --sort size:desc,modified:desc --columns name,size,modified,path --limit 500

# Find audio files sorted by name, then newest first
uffs.exe \"*.mp3|*.flac|*.wav|*.aac|*.ogg\" --files-only --sort name:asc,modified:desc --limit 500

# Find executables and installers, excluding system files
uffs.exe \"*.exe|*.msi|*.bat|*.cmd|*.ps1\" --files-only --hide-system --sort name:asc,modified:desc --limit 500

# ── Triage & Cleanup ───────────────────────────────────
# Report §6 items 6, 10, 11, 18, 20: cleanup is underrated but high-value

# Find large files (> 100 MB), show size vs disk allocation
uffs.exe \"*\" --files-only --min-size 104857600 --sort size:desc,name:asc --columns name,size,sizeondisk,ext,path --limit 500

# Find files modified in the last 7 days — show all timestamps (modified, created, accessed)
uffs.exe \"*\" --files-only --newer 7d --older 0d --sort modified:desc,name:asc --columns name,modified,created,accessed,path --limit 500

# Find empty folders (zero descendants)
uffs.exe \"*\" --dirs-only --max-descendants 0 --sort name:asc --columns name,path --limit 500

# Find bloated directories (>1,000 descendants — cleanup candidates)
uffs.exe \"*\" --dirs-only --min-descendants 1000 --sort descendants:desc --columns name,descendants,treesize,path --limit 500

# Find hidden files — show all NTFS attributes
uffs.exe \"*\" --files-only --attr hidden --sort name:asc --columns name,attributes,hidden,system,readonly,size,path --limit 500

# Find system files (hidden + system attributes)
uffs.exe \"*\" --files-only --attr hidden,system --sort name:asc --columns name,attrs,size,path --limit 500

# Find NTFS system metadata files ($MFT, $Bitmap, etc.)
uffs.exe \"$*\" --name-only --columns name,size,attributes,path

# Find compressed files — compare size vs on-disk
uffs.exe \"*\" --files-only --attr compressed --sort size:desc,name:asc --columns name,size,sizeondisk,compressed,path --limit 500

# Find reparse points (junctions, symlinks, mount points)
uffs.exe \"*\" --attr reparse --sort name:asc --columns name,reparse,directoryflag,attributes,path --limit 500

# ── Developer & Power User ─────────────────────────────
# Report §6 items 8, 9, 14: developer and scoped workflows

# Find source code files, newest first — dev layout with extension
uffs.exe \"*.rs|*.py|*.js|*.ts|*.cpp|*.c|*.java|*.go\" --files-only --hide-system --sort modified:desc,name:asc --columns name,ext,modified,size,path --limit 500

# Find project configs and manifests, A-Z then newest
uffs.exe \"*.toml|*.json|*.yaml|*.yml|*.xml|*.ini\" --files-only --hide-system --sort name:asc,modified:desc --limit 500

# Find log files using regex, newest first then by name
uffs.exe \">.*\\.log$\" --files-only --sort modified:desc,name:asc --limit 500

# Search within a specific folder tree (scoped search)
uffs.exe \"\\users\\**\\*.rs\" --sort name:asc,modified:desc --limit 500

# Find directories by descendant count (biggest trees first)
uffs.exe \"*\" --dirs-only --sort descendants:desc --columns name,descendants,treesize,path --limit 500

# Find large directory trees (>100 descendants)
uffs.exe \"*\" --dirs-only --min-descendants 100 --sort descendants:desc --columns name,descendants,treesize,path --limit 500

# Attribute audit: show all flags for all files
uffs.exe \"*\" --sort name:asc --columns name,hidden,system,archive,readonly,compressed,encrypted,sparse,reparse,path --limit 500
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
#[path = "history_tests.rs"]
mod tests;
