//! Drive refresh and loading helpers.
//!
//! Background refresh spawns threads that reload each drive from its
//! original source (`.uffs` cache with USN delta on Windows, raw MFT
//! re-parse on Mac/Linux). Results are received via a channel during
//! the event loop.

use anyhow::Result;

use crate::app::App;
use crate::{backend, compact, ui};

/// Start a background refresh of all loaded drives.
///
/// Spawns threads that reload each drive from its original source.
/// Results are received via `app.refresh_rx` channel during the event loop.
pub fn start_refresh(app: &mut App) {
    if app.refreshing || app.backend.drives.is_empty() {
        return;
    }

    let drive_count = app.backend.drives.len();
    let (sender, receiver) = std::sync::mpsc::channel();

    // Collect drive info for refresh threads (letter + source path)
    let drive_info: Vec<(char, compact::IndexSource)> = app
        .backend
        .drives
        .iter()
        .map(|dr| {
            let source = match &dr.source {
                compact::IndexSource::MftFile(path) => compact::IndexSource::MftFile(path.clone()),
            };
            (dr.letter, source)
        })
        .collect();

    // Spawn refresh threads
    std::thread::spawn(move || {
        std::thread::scope(|scope| {
            for (letter, source) in &drive_info {
                let thread_sender = sender.clone();
                let thread_letter = *letter;
                let thread_source = match source {
                    compact::IndexSource::MftFile(path) => path.clone(),
                };
                scope.spawn(move || {
                    let label = format!("{thread_letter}:");
                    // Build a temporary DriveCompactIndex just for refresh_drive
                    let temp = compact::DriveCompactIndex {
                        letter: thread_letter,
                        records: Vec::new(),
                        names: Vec::new(),
                        names_lower: Vec::new(),
                        trigram: backend::TrigramIndex::empty(),
                        children: Vec::new(),
                        source: compact::IndexSource::MftFile(thread_source),
                    };
                    let result = compact::refresh_drive(&temp);
                    drop(thread_sender.send((label, result)));
                });
            }
        });
    });

    app.refreshing = true;
    app.refresh_rx = Some(receiver);
    app.refresh_total = drive_count;
    app.refresh_done = 0;
    app.status = format!("🔄 Refreshing {drive_count} drive(s)...");
}

/// Poll for completed drive refreshes and swap them into the backend.
///
/// Called from the event loop on each iteration while `app.refreshing` is true.
#[expect(
    clippy::single_call_fn,
    reason = "separated from event loop for readability; refresh polling is a distinct concern"
)]
pub fn poll_refresh(app: &mut App) {
    let Some(receiver) = &app.refresh_rx else {
        return;
    };

    while let Ok((label, result)) = receiver.try_recv() {
        app.refresh_done += 1;
        match result {
            Ok((new_drive, timing)) => {
                // Find and replace the matching drive in the backend
                if let Some(existing) = app
                    .backend
                    .drives
                    .iter_mut()
                    .find(|dr| dr.letter == new_drive.letter)
                {
                    *existing = new_drive;
                } else {
                    app.backend.drives.push(new_drive);
                }
                app.status = format!(
                    "🔄 Refreshed {label} ({}/{}) — mft:{} compact:{} tri:{}",
                    app.refresh_done,
                    app.refresh_total,
                    ui::format_ms_compact(timing.mft),
                    ui::format_ms_compact(timing.compact),
                    ui::format_ms_compact(timing.trigram),
                );
            }
            Err(err) => {
                app.status = format!("❌ Refresh {label} failed: {err}");
            }
        }
    }

    // Check if all drives are done
    if app.refresh_done >= app.refresh_total {
        app.refreshing = false;
        app.refresh_rx = None;
        let fc = |n: usize| uffs_mft::format_number_commas(n as u64);
        app.status = format!(
            "✅ Refreshed {} drive(s), {} records — type to search",
            app.backend.drives.len(),
            fc(app.backend.total_records()),
        );
        // Re-run search if user has a pattern
        if !app.input_text().is_empty() {
            app.search();
        }
    }
}

/// Load a live NTFS drive — platform dispatch.
#[cfg(windows)]
pub fn load_live_drive_impl(
    drive_letter: char,
    no_cache: bool,
) -> anyhow::Result<(compact::DriveCompactIndex, compact::LoadTiming)> {
    compact::load_live_drive(drive_letter, no_cache)
}

/// Load a live NTFS drive — not available on non-Windows.
#[cfg(not(windows))]
#[expect(
    clippy::single_call_fn,
    reason = "platform-specific stub; Windows version in compact::load_live_drive"
)]
pub fn load_live_drive_impl(
    drive_letter: char,
    _no_cache: bool,
) -> Result<(compact::DriveCompactIndex, compact::LoadTiming)> {
    anyhow::bail!("Live drive loading requires Windows (drive {drive_letter}:)")
}
