//! Search backend — re-exported from `uffs-core` with TUI-specific additions
//! (drive color palettes).

// ── Re-exports from uffs-core (all search types, sort, filters) ─────────
pub(crate) use uffs_core::search::backend::{
    DisplayRow, FilterMode, MultiDriveBackend, format_sort_spec, parse_sort_spec,
};
pub(crate) use uffs_core::search::field::FieldId;
pub(crate) use uffs_core::search::filters::{SearchFilters, apply_filter, apply_search_filters};
pub(crate) use uffs_core::trigram::TrigramIndex;

// Re-exported from `crate::columns`.
pub(crate) use crate::columns::{DEFAULT_COLUMNS, parse_columns};
use crate::compact::DriveCompactIndex;

// ── TUI-specific: drive color palettes ──────────────────────────────────

/// Maximally distinct color palettes for 1–10 drives.
///
/// Each sub-palette is hand-tuned so that N drives get N maximally
/// distinguishable colors on a dark terminal background.
const PALETTES: &[&[(u8, u8, u8)]] = &[
    // 1 drive
    &[(255, 255, 255)],
    // 2 drives
    &[(100, 180, 255), (255, 150, 50)],
    // 3 drives
    &[(100, 180, 255), (80, 220, 80), (255, 150, 50)],
    // 4 drives
    &[
        (100, 180, 255),
        (80, 220, 80),
        (255, 150, 50),
        (200, 100, 255),
    ],
    // 5 drives
    &[
        (100, 180, 255),
        (80, 220, 80),
        (255, 150, 50),
        (200, 100, 255),
        (255, 255, 80),
    ],
    // 6 drives
    &[
        (100, 180, 255),
        (80, 220, 80),
        (255, 150, 50),
        (200, 100, 255),
        (255, 255, 80),
        (255, 100, 100),
    ],
    // 7 drives
    &[
        (100, 180, 255),
        (80, 220, 80),
        (255, 150, 50),
        (200, 100, 255),
        (255, 255, 80),
        (255, 100, 100),
        (100, 255, 220),
    ],
    // 8 drives
    &[
        (100, 180, 255),
        (80, 220, 80),
        (255, 150, 50),
        (200, 100, 255),
        (255, 255, 80),
        (255, 100, 100),
        (100, 255, 220),
        (255, 130, 200),
    ],
    // 9 drives
    &[
        (100, 180, 255),
        (80, 220, 80),
        (255, 150, 50),
        (200, 100, 255),
        (255, 255, 80),
        (255, 100, 100),
        (100, 255, 220),
        (255, 130, 200),
        (255, 255, 255),
    ],
    // 10 drives
    &[
        (100, 180, 255),
        (80, 220, 80),
        (255, 150, 50),
        (200, 100, 255),
        (255, 255, 80),
        (255, 100, 100),
        (100, 255, 220),
        (255, 130, 200),
        (255, 255, 255),
        (180, 140, 100),
    ],
];

/// Build a drive-letter → color mapping for the currently loaded drives.
///
/// Assigns colors from the optimal palette for the given number of drives.
/// Drives are sorted alphabetically so the mapping is deterministic.
#[must_use]
#[expect(
    clippy::single_call_fn,
    reason = "public API: intentionally a standalone function for reuse and clarity"
)]
pub(crate) fn build_drive_colors(
    drives: &[DriveCompactIndex],
) -> std::collections::HashMap<char, ratatui::style::Color> {
    use ratatui::style::Color;

    let mut letters: Vec<char> = drives.iter().map(|dr| dr.letter).collect();
    letters.sort_unstable();
    letters.dedup();

    let count = letters.len();
    let palette_idx = count
        .saturating_sub(1)
        .min(PALETTES.len().saturating_sub(1));
    let default_palette: &[(u8, u8, u8)] = &[(255, 255, 255)];
    let palette = PALETTES.get(palette_idx).unwrap_or(&default_palette);

    letters
        .into_iter()
        .enumerate()
        .map(|(idx, letter)| {
            let &(red, green, blue) = palette
                .get(idx % palette.len().max(1))
                .unwrap_or(&(255, 255, 255));
            (letter, Color::Rgb(red, green, blue))
        })
        .collect()
}
