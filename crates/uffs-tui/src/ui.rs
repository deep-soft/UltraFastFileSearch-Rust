//! TUI rendering — layout, table, help bar, and text highlighting.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::app::{App, Focus};
use crate::backend;
use crate::keys::Action;

/// Render the TUI layout: search bar, status, results list, and help bar.
#[expect(
    clippy::indexing_slicing,
    reason = "layout split guarantees exactly 4 chunks matching the 4 constraints"
)]
#[expect(
    clippy::missing_asserts_for_indexing,
    reason = "layout split guarantees exactly 4 chunks matching the 4 constraints"
)]
#[expect(
    clippy::too_many_lines,
    reason = "UI rendering is a single cohesive function; splitting would fragment layout logic"
)]
pub fn ui(frame: &mut Frame, app: &mut App) {
    // Focus-aware border styles: bright cyan for focused, dim gray for unfocused.
    let focused_border = Style::default().fg(Color::Cyan);
    let unfocused_border = Style::default().fg(Color::DarkGray);
    let search_border = if app.focus == Focus::SearchBox {
        focused_border
    } else {
        unfocused_border
    };
    let results_border = if app.focus == Focus::Results {
        focused_border
    } else {
        unfocused_border
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3), // Search input
            Constraint::Length(3), // Status/Error bar
            Constraint::Min(10),   // Results
            Constraint::Length(3), // Help bar
        ])
        .split(frame.area());

    // Build drive color map (dynamic palette based on number of drives)
    let drive_colors = backend::build_drive_colors(&app.backend.drives);
    let get_drive_color =
        |letter: char| -> Color { drive_colors.get(&letter).copied().unwrap_or(Color::White) };

    // Search input with drive indicators (sorted, comma-formatted count)
    let mut drive_letters: Vec<char> = app
        .backend
        .drive_summary()
        .iter()
        .map(|(letter, _count)| *letter)
        .collect();
    drive_letters.sort_unstable();
    let filter_indicator = app.filter_label();
    if app.has_data() {
        // Build colored drive letters for the title
        let mut title_spans: Vec<Span> = vec![Span::raw(" Search NTFS Drives [")];
        for (idx, &letter) in drive_letters.iter().enumerate() {
            if idx > 0 {
                title_spans.push(Span::raw(" "));
            }
            title_spans.push(Span::styled(
                letter.to_string(),
                Style::default()
                    .fg(get_drive_color(letter))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        title_spans.push(Span::raw(format!(
            "] {} Files",
            uffs_core::format::format_number_commas(app.backend.total_records() as u64),
        )));
        // Search mode indicators: [Cc] [W] [NAME] [FILES] etc.
        let badge = |label: &str, hint: &str, active: bool| -> Span<'static> {
            if active {
                Span::styled(
                    format!(" [{label}]"),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(
                    format!(" [{label}:{hint}]"),
                    Style::default().fg(Color::DarkGray),
                )
            }
        };
        title_spans.push(badge("Cc", "Tab", app.case_sensitive));
        title_spans.push(badge("W", "^W", app.whole_word));
        if app.name_only {
            title_spans.push(badge("NAME", "^F", true));
        }
        if !filter_indicator.is_empty() {
            title_spans.push(Span::styled(
                filter_indicator.to_owned(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        title_spans.push(Span::raw(" "));
        app.textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(search_border)
                .title(Line::from(title_spans)),
        );
    } else {
        app.textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(search_border)
                .title(" Search (use --mft-file to load data) "),
        );
    }
    frame.render_widget(&app.textarea, chunks[0]);

    // Status/Error bar — shows history comment when browsing history with a
    // commented entry, otherwise shows normal status or error.
    let (status_title, status_content) = if let Some(err) = &app.error {
        (
            " Status ",
            Line::from(vec![
                Span::styled("Error: ", Style::default().fg(Color::Red)),
                Span::styled(err.as_str(), Style::default().fg(Color::Red)),
            ]),
        )
    } else if let Some(comment) = app.current_history_comment() {
        let idx = app.history_idx.unwrap_or(0) + 1;
        let total = app.search_history.len();
        (
            " History Note ",
            Line::from(vec![
                Span::styled(format!("📝 {comment}"), Style::default().fg(Color::Cyan)),
                Span::styled(
                    format!("  ({idx}/{total})"),
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
        )
    } else {
        (
            " Status ",
            Line::from(vec![Span::styled(
                app.status.as_str(),
                Style::default().fg(Color::Green),
            )]),
        )
    };
    let status_bar = Paragraph::new(status_content)
        .block(Block::default().borders(Borders::ALL).title(status_title));
    frame.render_widget(status_bar, chunks[1]);

    // Update page size from actual results area height (minus 3 for borders +
    // header)
    app.page_size = chunks[2].height.saturating_sub(3) as usize;

    // Sort indicator helper — appends ▲/▼ to the active column header
    let sort_arrow = if app.sort_desc() { " ▼" } else { " ▲" };
    let current_sort = app.sort_column();
    let col_header = |col: backend::FieldId, label: &str| -> Line<'static> {
        if col == current_sort {
            Line::from(vec![
                Span::styled(
                    label.to_owned(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(sort_arrow.to_owned(), Style::default().fg(Color::Yellow)),
            ])
        } else {
            Line::from(Span::styled(
                label.to_owned(),
                Style::default().fg(Color::White),
            ))
        }
    };

    // Build table header row from visible columns
    let vis = &app.visible_columns;
    let header_cells: Vec<Cell> = vis
        .iter()
        .map(|col| Cell::from(col_header(col.nearest_sort_field(), col.tui_label())))
        .collect();
    let header = Row::new(header_cells)
        .style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(0);

    // Extract literal words from the pattern for highlighting.
    // *\documents\*.txt → ["documents", "txt"]
    // *sex*ge* → ["sex", "ge"]
    // >regex → [] (regex too complex to highlight)
    let raw_input = app.input_text().to_lowercase();
    let highlight_terms: Vec<&str> = if raw_input.starts_with('>') {
        // Regex: extract literal segments between metacharacters.
        // `>.*\.log$` → [".log"]
        raw_input
            .strip_prefix('>')
            .unwrap_or("")
            .split(['*', '+', '?', '^', '$', '(', ')', '[', ']', '{', '}', '\\'])
            .filter(|seg| seg.len() > 1)
            .collect()
    } else {
        raw_input
            .split(['*', '?', '\\', '/', '|'])
            .filter(|seg| !seg.is_empty())
            .collect()
    };

    // Build table rows from results, respecting visible column selection
    let num_cols = vis.len();
    let rows: Vec<Row> = app
        .results
        .iter()
        .map(|row| {
            // Loading progress messages (path empty = loading msg)
            if row.path.is_empty() {
                let mut cells: Vec<Cell> = vec![Cell::from(""); num_cols];
                // Put the message in the second column (or first if only one)
                let msg_idx = usize::from(num_cols > 1);
                cells[msg_idx] = Cell::from(Line::from(Span::styled(
                    row.name().to_owned(),
                    Style::default()
                        .fg(get_drive_color(row.drive))
                        .add_modifier(Modifier::BOLD),
                )));
                return Row::new(cells);
            }

            let cells: Vec<Cell> = vis
                .iter()
                .map(|col| build_cell(*col, row, &highlight_terms, &drive_colors))
                .collect();
            Row::new(cells)
        })
        .collect();

    let widths: Vec<Constraint> = vis
        .iter()
        .map(|col| crate::columns::default_constraint(*col))
        .collect();
    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(results_border)
                .title({
                    let sort_label = app.sort_column().tui_label();
                    let dir_label = if app.sort_desc() { "▼" } else { "▲" };
                    let filter_label = app.filter_label();
                    let mode_label = if app.input_text().is_empty() {
                        " │ ALL"
                    } else {
                        ""
                    };
                    format!(
                        " Results ({}) │ Sort: {sort_label} {dir_label}{filter_label}{mode_label} ",
                        app.results.len()
                    )
                }),
        )
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(table, chunks[2], &mut app.table_state.clone());

    // Dynamic help bar — key labels read from the active keymap,
    // platform-aware (Alt keys hidden on macOS).
    let km = &app.keymap;
    let key_style = Style::default().fg(Color::Green);
    let more_style = Style::default().fg(Color::DarkGray);
    let help_key = km.label(Action::HelpCycle);

    let help_spans = build_help_spans(
        km,
        app.help_page,
        app.focus,
        &help_key,
        key_style,
        more_style,
    );
    let focus_label = match app.focus {
        Focus::SearchBox => "Search",
        Focus::Results => "Results",
    };
    let page_labels = ["Nav", "Toggles", "Edit", "Patterns"];
    let page_label = page_labels.get(app.help_page as usize).unwrap_or(&"Help");
    let help =
        Paragraph::new(Line::from(help_spans)).block(Block::default().borders(Borders::ALL).title(
            format!(" Help ({page_label} · {focus_label}) — {help_key} to cycle · Esc to switch "),
        ));
    frame.render_widget(help, chunks[3]);
}

/// Push a key→description pair into a help bar span list.
fn help_kv(spans: &mut Vec<Span<'static>>, key: &str, desc: &str, style: Style) {
    spans.push(Span::styled(key.to_owned(), style));
    spans.push(Span::raw(format!(" {desc}  ")));
}

/// Build the help bar spans for the given page, reading labels from the keymap.
#[expect(
    clippy::single_call_fn,
    reason = "extracted from draw_ui to keep rendering function readable"
)]
fn build_help_spans(
    km: &crate::keys::Keymap,
    page: u8,
    focus: Focus,
    help_key: &str,
    key_style: Style,
    more_style: Style,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    match page {
        0 => match focus {
            Focus::SearchBox => {
                help_kv(&mut spans, "↑↓", "History", key_style);
                help_kv(&mut spans, "Esc", "→Results", key_style);
                help_kv(&mut spans, &km.label(Action::Quit), "Quit", key_style);
            }
            Focus::Results => {
                help_kv(&mut spans, "↑↓", "Nav", key_style);
                help_kv(&mut spans, "PgUp/Dn", "Page", key_style);
                help_kv(&mut spans, &km.label(Action::ShowPath), "Path", key_style);
                help_kv(&mut spans, &km.label(Action::SortCycle), "Sort", key_style);
                help_kv(
                    &mut spans,
                    &km.label(Action::SortDirection),
                    "Dir",
                    key_style,
                );
                help_kv(&mut spans, "Esc", "→Search", key_style);
                help_kv(&mut spans, &km.label(Action::Quit), "Quit", key_style);
            }
        },
        1 => {
            help_kv(
                &mut spans,
                &km.label(Action::ToggleNameOnly),
                "Name-only",
                key_style,
            );
            help_kv(
                &mut spans,
                &km.label(Action::ToggleFilter),
                "Filter",
                key_style,
            );
            help_kv(
                &mut spans,
                &km.label(Action::ToggleCaseSensitive),
                "Case",
                key_style,
            );
            help_kv(
                &mut spans,
                &km.label(Action::ToggleWholeWord),
                "Word",
                key_style,
            );
            help_kv(&mut spans, &km.label(Action::Refresh), "Refresh", key_style);
        }
        2 => {
            help_kv(&mut spans, &km.label(Action::ClearLine), "Clear", key_style);
            help_kv(&mut spans, &km.label(Action::Undo), "Undo", key_style);
            help_kv(&mut spans, &km.label(Action::Redo), "Redo", key_style);
            help_kv(
                &mut spans,
                &km.label(Action::SelectAll),
                "Select",
                key_style,
            );
            help_kv(&mut spans, &km.label(Action::Copy), "Copy", key_style);
            help_kv(&mut spans, &km.label(Action::Paste), "Paste", key_style);
        }
        _ => {
            help_kv(&mut spans, "text", "substring", key_style);
            help_kv(&mut spans, "*glob*", "wildcard", key_style);
            help_kv(&mut spans, "?", "single char", key_style);
            help_kv(&mut spans, "\\path\\*", "tree", key_style);
            help_kv(&mut spans, "**", "recursive", key_style);
            help_kv(&mut spans, ">regex", "", key_style);
        }
    }
    help_kv(&mut spans, help_key, "More…", more_style);
    spans
}

/// Highlight multiple terms in text. Each term is highlighted independently.
///
/// For `*\documents\*.txt` → highlights "documents" and "txt" separately.
fn highlight_multi(
    text: &str,
    terms: &[&str],
    normal_style: Style,
    highlight_style: Style,
) -> Vec<Span<'static>> {
    if terms.is_empty() {
        return vec![Span::styled(text.to_owned(), normal_style)];
    }
    // Apply first term, then apply subsequent terms to the non-highlighted spans
    let Some(&first_term) = terms.first() else {
        return vec![Span::styled(text.to_owned(), normal_style)];
    };
    let mut spans = highlight_matches(text, first_term, normal_style, highlight_style);
    for &term in terms.get(1..).unwrap_or(&[]) {
        if term.is_empty() {
            continue;
        }
        let mut new_spans = Vec::new();
        for span in spans {
            if span.style == highlight_style {
                // Already highlighted — keep as-is
                new_spans.push(span);
            } else {
                // Not highlighted — apply next term
                new_spans.extend(highlight_matches(
                    span.content.as_ref(),
                    term,
                    normal_style,
                    highlight_style,
                ));
            }
        }
        spans = new_spans;
    }
    spans
}

/// Split text into spans, highlighting case-insensitive matches of `needle`.
///
/// Non-matching parts use `normal_style`, matching parts use `highlight_style`.
fn highlight_matches(
    text: &str,
    needle: &str,
    normal_style: Style,
    highlight_style: Style,
) -> Vec<Span<'static>> {
    if needle.is_empty() {
        return vec![Span::styled(text.to_owned(), normal_style)];
    }

    let lower = text.to_lowercase();
    let mut spans = Vec::new();
    let mut last_end = 0;

    for (start, matched) in lower.match_indices(needle) {
        if start > last_end {
            if let Some(before) = text.get(last_end..start) {
                spans.push(Span::styled(before.to_owned(), normal_style));
            }
        }
        let end = start + matched.len();
        if let Some(hit) = text.get(start..end) {
            spans.push(Span::styled(hit.to_owned(), highlight_style));
        }
        last_end = end;
    }

    if last_end < text.len() {
        if let Some(tail) = text.get(last_end..) {
            spans.push(Span::styled(tail.to_owned(), normal_style));
        }
    }

    if spans.is_empty() {
        spans.push(Span::styled(text.to_owned(), normal_style));
    }

    spans
}

/// Convert a devicons hex color string (e.g., `"#e37933"`) to a ratatui
/// `Color`.
///
/// Hex strings from devicons are always 7-byte ASCII (`#RRGGBB`), so
/// byte-level `.get()` slicing is safe.
fn devicon_color(hex: &str) -> Color {
    if hex.len() == 7 && hex.starts_with('#') {
        if let (Some(rr), Some(gg), Some(bb)) = (hex.get(1..3), hex.get(3..5), hex.get(5..7)) {
            if let (Ok(red), Ok(green), Ok(blue)) = (
                u8::from_str_radix(rr, 16),
                u8::from_str_radix(gg, 16),
                u8::from_str_radix(bb, 16),
            ) {
                return Color::Rgb(red, green, blue);
            }
        }
    }
    Color::White
}

/// Format milliseconds compactly: `23 ms`, `535 ms`, `1.1  s`, `19.6  s`.
pub fn format_ms_compact(ms: u128) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        // Integer arithmetic: tenths of a second to avoid float_arithmetic lint
        let tenths = (ms + 50) / 100; // round to nearest tenth
        let whole = tenths / 10;
        let frac = tenths % 10;
        format!("{whole}.{frac}  s")
    }
}

/// Truncate a path string for display, keeping the end visible.
fn truncate_path(path: &str, max_len: usize) -> String {
    if path.chars().count() <= max_len {
        return path.to_owned();
    }
    let skip = path.chars().count() - max_len + 1;
    let truncated: String = path.chars().skip(skip).collect();
    format!("…{truncated}")
}

/// Build a single table cell for the given column and row.
#[expect(
    clippy::single_call_fn,
    clippy::too_many_lines,
    reason = "large cell-rendering logic; separation keeps table-drawing code readable"
)]
fn build_cell<'a>(
    col: backend::FieldId,
    row: &backend::DisplayRow,
    highlight_terms: &[&str],
    drive_colors: &std::collections::HashMap<char, Color>,
) -> Cell<'a> {
    use backend::FieldId;
    match col {
        FieldId::Drive => Cell::from(Line::from(Span::styled(
            row.drive.to_string(),
            Style::default()
                .fg(drive_colors
                    .get(&row.drive)
                    .copied()
                    .unwrap_or(Color::White))
                .add_modifier(Modifier::BOLD),
        ))),
        FieldId::Name => {
            let fi = devicons::icon_for_file(row.name(), &None);
            let icon_str = fi.icon.to_string();
            let icon_color = devicon_color(fi.color);
            let mut spans = vec![
                Span::styled(icon_str, Style::default().fg(icon_color)),
                Span::raw(" "),
            ];
            spans.extend(highlight_multi(
                row.name(),
                highlight_terms,
                Style::default().fg(Color::Cyan),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
            Cell::from(Line::from(spans))
        }
        FieldId::Size => Cell::from(Line::from(Span::styled(
            uffs_core::format::format_bytes(row.size),
            Style::default().fg(Color::Yellow),
        ))),
        FieldId::Modified => Cell::from(Line::from(Span::styled(
            uffs_core::format::format_timestamp(row.modified),
            Style::default().fg(Color::DarkGray),
        ))),
        FieldId::Created => Cell::from(Line::from(Span::styled(
            uffs_core::format::format_timestamp(row.created),
            Style::default().fg(Color::DarkGray),
        ))),
        FieldId::Accessed => Cell::from(Line::from(Span::styled(
            uffs_core::format::format_timestamp(row.accessed),
            Style::default().fg(Color::DarkGray),
        ))),
        FieldId::Path => {
            let path_display = truncate_path(&row.path, 60);
            let path_spans = highlight_multi(
                &path_display,
                highlight_terms,
                Style::default().fg(Color::DarkGray),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            );
            Cell::from(Line::from(path_spans))
        }
        FieldId::PathOnly => {
            let dir = row
                .path
                .rfind('\\')
                .and_then(|idx| row.path.get(..idx))
                .unwrap_or("");
            let dir_display = truncate_path(dir, 50);
            Cell::from(Line::from(Span::styled(
                dir_display,
                Style::default().fg(Color::DarkGray),
            )))
        }
        FieldId::SizeOnDisk => Cell::from(Line::from(Span::styled(
            uffs_core::format::format_bytes(row.allocated),
            Style::default().fg(Color::Yellow),
        ))),
        FieldId::Extension => {
            let ext = row.name().rsplit('.').next().unwrap_or("");
            Cell::from(Line::from(Span::styled(
                ext.to_owned(),
                Style::default().fg(Color::Cyan),
            )))
        }
        FieldId::Type => {
            let fi = devicons::icon_for_file(row.name(), &None);
            Cell::from(Line::from(Span::styled(
                fi.icon.to_string(),
                Style::default().fg(devicon_color(fi.color)),
            )))
        }
        // ── formatted attribute string ─────────────────────────────
        FieldId::Attributes => Cell::from(Line::from(Span::styled(
            format_ntfs_attrs(row.flags),
            Style::default().fg(Color::Magenta),
        ))),
        FieldId::AttributeValue => Cell::from(Line::from(Span::styled(
            format!("{:06X}", row.flags),
            Style::default().fg(Color::DarkGray),
        ))),
        // ── individual attribute booleans ───────────────────────────
        FieldId::Hidden => attr_cell(row.flags, 0x0002),
        FieldId::System => attr_cell(row.flags, 0x0004),
        FieldId::Archive => attr_cell(row.flags, 0x0020),
        FieldId::ReadOnly => attr_cell(row.flags, 0x0001),
        FieldId::Compressed => attr_cell(row.flags, 0x0800),
        FieldId::Encrypted => attr_cell(row.flags, 0x4000),
        FieldId::Sparse => attr_cell(row.flags, 0x0200),
        FieldId::Reparse => attr_cell(row.flags, 0x0400),
        FieldId::Offline => attr_cell(row.flags, 0x1000),
        FieldId::NotIndexed => attr_cell(row.flags, 0x2000),
        FieldId::Temporary => attr_cell(row.flags, 0x0100),
        FieldId::Virtual => attr_cell(row.flags, 0x0001_0000),
        FieldId::Pinned => attr_cell(row.flags, 0x0008_0000),
        FieldId::Unpinned => attr_cell(row.flags, 0x0010_0000),
        FieldId::Integrity => attr_cell(row.flags, 0x8000),
        FieldId::NoScrub => attr_cell(row.flags, 0x0002_0000),
        FieldId::DirectoryFlag => attr_cell(row.flags, 0x0010),
        // ── tree metrics ───────────────────────────────────────────
        FieldId::Descendants => Cell::from(Line::from(Span::styled(
            if row.descendants > 0 {
                format!("{}", row.descendants)
            } else {
                String::new()
            },
            Style::default().fg(Color::Cyan),
        ))),
        FieldId::TreeSize => Cell::from(Line::from(Span::styled(
            if row.treesize > 0 {
                uffs_core::format::format_bytes(row.treesize)
            } else {
                String::new()
            },
            Style::default().fg(Color::Yellow),
        ))),
        // Remaining FieldId variants: show canonical name as fallback.
        FieldId::TreeAllocated
        | FieldId::Bulkiness
        | FieldId::RecallOnOpen
        | FieldId::RecallOnDataAccess
        | FieldId::ParityAttributes => Cell::from(Line::from(Span::styled(
            col.canonical_name().to_owned(),
            Style::default().fg(Color::DarkGray),
        ))),
    }
}

/// Render a boolean attribute cell: `✓` (green) or blank.
fn attr_cell<'a>(flags: u32, bit: u32) -> Cell<'a> {
    if flags & bit != 0 {
        Cell::from(Line::from(Span::styled(
            "✓",
            Style::default().fg(Color::Green),
        )))
    } else {
        Cell::from("")
    }
}

/// Format NTFS attribute flags into a compact string.
///
/// Uses the same single-letter codes as Everything / the CLI:
/// `R`ead-only, `H`idden, `S`ystem, `D`irectory, `A`rchive, `T`emporary,
/// `s`parse, `r`eparse, `C`ompressed, `O`ffline, `N`ot-indexed,
/// `E`ncrypted, `I`ntegrity, `V`irtual, `P`inned, `U`npinned, `X`(no-scrub).
#[expect(
    clippy::single_call_fn,
    reason = "standalone formatter; keeps attribute-flag rendering isolated"
)]
fn format_ntfs_attrs(flags: u32) -> String {
    /// Bit-flag / letter pairs for NTFS attributes.
    const ATTR_MAP: &[(u32, char)] = &[
        (0x0001, 'R'),
        (0x0002, 'H'),
        (0x0004, 'S'),
        (0x0010, 'D'),
        (0x0020, 'A'),
        (0x0100, 'T'),
        (0x0200, 's'),
        (0x0400, 'r'),
        (0x0800, 'C'),
        (0x1000, 'O'),
        (0x2000, 'N'),
        (0x4000, 'E'),
        (0x8000, 'I'),
        (0x0001_0000, 'V'),
        (0x0002_0000, 'X'),
        (0x0008_0000, 'P'),
        (0x0010_0000, 'U'),
    ];
    let mut out = String::with_capacity(8);
    for &(bit, ch) in ATTR_MAP {
        if flags & bit != 0 {
            out.push(ch);
        }
    }
    out
}
