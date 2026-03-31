//! Pure utility helpers for the search command.

/// Compute the list of output targets (drive letters) for results.
pub(super) fn compute_output_targets(
    single_drive: Option<char>,
    multi_drives: Option<&Vec<char>>,
    pattern_drive: Option<char>,
) -> Vec<char> {
    single_drive
        .map(|drive| vec![drive])
        .or_else(|| multi_drives.cloned())
        .or_else(|| pattern_drive.map(|drive| vec![drive]))
        .unwrap_or_default()
}
