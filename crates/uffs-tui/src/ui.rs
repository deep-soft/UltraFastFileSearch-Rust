//! TUI rendering — layout, table, help bar, and text highlighting.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

use crate::app::App;
use crate::backend;

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
            uffs_mft::format_number_commas(app.backend.total_records() as u64),
        )));
        // Search mode indicators: [Cc] [W] [NAME] [FILES] etc.
        let badge = |label: &str, active: bool| -> Span<'static> {
            if active {
                Span::styled(
                    format!(" [{label}]"),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(format!(" [{label}]"), Style::default().fg(Color::DarkGray))
            }
        };
        title_spans.push(badge("Cc", app.case_sensitive));
        title_spans.push(badge("W", app.whole_word));
        if app.name_only {
            title_spans.push(badge("NAME", true));
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
                .title(Line::from(title_spans)),
        );
    } else {
        app.textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Search (use --mft-file to load data) "),
        );
    }
    frame.render_widget(&app.textarea, chunks[0]);

    // Status/Error bar
    let status_content = if let Some(err) = &app.error {
        Line::from(vec![
            Span::styled("Error: ", Style::default().fg(Color::Red)),
            Span::styled(err.as_str(), Style::default().fg(Color::Red)),
        ])
    } else {
        Line::from(vec![Span::styled(
            app.status.as_str(),
            Style::default().fg(Color::Green),
        )])
    };
    let status_bar = Paragraph::new(status_content)
        .block(Block::default().borders(Borders::ALL).title(" Status "));
    frame.render_widget(status_bar, chunks[1]);

    // Update page size from actual results area height (minus 3 for borders +
    // header)
    app.page_size = chunks[2].height.saturating_sub(3) as usize;

    // Sort indicator helper — appends ▲/▼ to the active column header
    let sort_arrow = if app.sort_desc() { " ▼" } else { " ▲" };
    let current_sort = app.sort_column();
    let col_header = |col: backend::SortColumn, label: &str| -> Line<'static> {
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

    // Build table header row
    let header = Row::new(vec![
        Cell::from(col_header(backend::SortColumn::Drive, "Drv")),
        Cell::from(col_header(backend::SortColumn::Name, "Name")),
        Cell::from(col_header(backend::SortColumn::Size, "Size")),
        Cell::from(col_header(backend::SortColumn::Modified, "Modified")),
        Cell::from(col_header(backend::SortColumn::Path, "Path")),
    ])
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
        Vec::new() // regex — don't highlight
    } else {
        raw_input
            .split(['*', '?', '\\', '/', '.'])
            .filter(|seg| !seg.is_empty())
            .collect()
    };

    // Build table rows from results
    let rows: Vec<Row> = app
        .results
        .iter()
        .map(|row| {
            // Loading progress messages (path empty = loading msg)
            if row.path.is_empty() {
                return Row::new(vec![
                    Cell::from(""),
                    Cell::from(Line::from(Span::styled(
                        row.name.clone(),
                        Style::default()
                            .fg(get_drive_color(row.drive))
                            .add_modifier(Modifier::BOLD),
                    ))),
                    Cell::from(""),
                    Cell::from(""),
                    Cell::from(""),
                ]);
            }

            // Get file-type icon from devicons (Nerd Font glyphs)
            let fi = devicons::icon_for_file(&row.name, &None);
            let icon_str = fi.icon.to_string();
            let icon_color = devicon_color(fi.color);

            // Drive column (colored letter)
            let drive_cell = Cell::from(Line::from(Span::styled(
                row.drive.to_string(),
                Style::default()
                    .fg(get_drive_color(row.drive))
                    .add_modifier(Modifier::BOLD),
            )));

            // Name column: icon + highlighted name
            let mut name_spans = vec![
                Span::styled(icon_str, Style::default().fg(icon_color)),
                Span::raw(" "),
            ];
            name_spans.extend(highlight_multi(
                &row.name,
                &highlight_terms,
                Style::default().fg(Color::Cyan),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
            let name_cell = Cell::from(Line::from(name_spans));

            // Size column
            let size_cell = Cell::from(Line::from(Span::styled(
                uffs_mft::format_bytes(row.size),
                Style::default().fg(Color::Yellow),
            )));

            // Modified column
            let modified_cell = Cell::from(Line::from(Span::styled(
                uffs_mft::format_timestamp(row.modified),
                Style::default().fg(Color::DarkGray),
            )));

            // Path column (highlighted, truncated)
            let path_display = truncate_path(&row.path, 60);
            let path_spans = highlight_multi(
                &path_display,
                &highlight_terms,
                Style::default().fg(Color::DarkGray),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            );
            let path_cell = Cell::from(Line::from(path_spans));

            Row::new(vec![
                drive_cell,
                name_cell,
                size_cell,
                modified_cell,
                path_cell,
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(3),  // Drive
            Constraint::Min(20),    // Name (flexible, takes remaining space)
            Constraint::Length(12), // Size
            Constraint::Length(19), // Modified
            Constraint::Length(62), // Path
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title({
        let sort_label = app.sort_column().label();
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
    }))
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("▶ ");

    frame.render_stateful_widget(table, chunks[2], &mut app.table_state.clone());

    // Dynamic help bar — F1 cycles pages
    let key_style = Style::default().fg(Color::Green);
    let help_spans: Vec<Span> = match app.help_page {
        0 => vec![
            Span::styled("↑↓", key_style),
            Span::raw(" Nav  "),
            Span::styled("PgUp/Dn", key_style),
            Span::raw(" Page  "),
            Span::styled("Enter", key_style),
            Span::raw(" Path  "),
            Span::styled("Tab", key_style),
            Span::raw(" Sort  "),
            Span::styled("S-Tab", key_style),
            Span::raw(" Reverse  "),
            Span::styled("Ctrl+Q", key_style),
            Span::raw(" Quit  "),
            Span::styled("F1", Style::default().fg(Color::DarkGray)),
            Span::raw(" More…"),
        ],
        1 => vec![
            Span::styled("F2", key_style),
            Span::raw(" Name-only  "),
            Span::styled("F3", key_style),
            Span::raw(" Filter  "),
            Span::styled("F5", key_style),
            Span::raw(" Refresh  "),
            Span::styled("F7", key_style),
            Span::raw(" Case  "),
            Span::styled("F8", key_style),
            Span::raw(" Word  "),
            Span::styled("F1", Style::default().fg(Color::DarkGray)),
            Span::raw(" More…"),
        ],
        2 => vec![
            Span::styled("Ctrl+U", key_style),
            Span::raw(" Clear  "),
            Span::styled("Ctrl+Z", key_style),
            Span::raw(" Undo  "),
            Span::styled("Ctrl+Y", key_style),
            Span::raw(" Redo  "),
            Span::styled("Ctrl+A", key_style),
            Span::raw(" Select  "),
            Span::styled("Ctrl+P", key_style),
            Span::raw(" Prev  "),
            Span::styled("Ctrl+N", key_style),
            Span::raw(" Next  "),
            Span::styled("Ctrl+R", key_style),
            Span::raw(" Refresh  "),
            Span::styled("F1", Style::default().fg(Color::DarkGray)),
            Span::raw(" More…"),
        ],
        _ => vec![
            Span::styled("text", key_style),
            Span::raw(" substring  "),
            Span::styled("*glob*", key_style),
            Span::raw(" wildcard  "),
            Span::styled("?", key_style),
            Span::raw(" single char  "),
            Span::styled("\\path\\*", key_style),
            Span::raw(" tree  "),
            Span::styled("**", key_style),
            Span::raw(" recursive  "),
            Span::styled(">regex", key_style),
            Span::raw("  "),
            Span::styled("F1", Style::default().fg(Color::DarkGray)),
            Span::raw(" More…"),
        ],
    };
    let page_labels = ["Nav", "Toggles", "Ctrl", "Patterns"];
    let page_label = page_labels.get(app.help_page as usize).unwrap_or(&"Help");
    let help = Paragraph::new(Line::from(help_spans)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" Help ({page_label}) — F1 to cycle ")),
    );
    frame.render_widget(help, chunks[3]);
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
#[expect(
    clippy::single_call_fn,
    reason = "standalone color-parsing helper; keeps rendering code readable"
)]
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
#[expect(
    clippy::single_call_fn,
    reason = "called from ui rendering; separation keeps display formatting isolated"
)]
fn truncate_path(path: &str, max_len: usize) -> String {
    if path.chars().count() <= max_len {
        return path.to_owned();
    }
    let skip = path.chars().count() - max_len + 1;
    let truncated: String = path.chars().skip(skip).collect();
    format!("…{truncated}")
}
