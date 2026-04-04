//! Derived search-field helpers shared by daemon projection/filter logic.

use super::backend::DisplayRow;
use crate::extensions::collections;

/// Bulkiness fixed-point scale (`1.0 == 1_000_000`).
pub const BULKINESS_SCALE: u64 = 1_000_000;

/// Executable file extensions.
const EXECUTABLES: &[&str] = &["exe", "msi", "bat", "cmd", "ps1", "com", "scr"];
/// Font file extensions.
const FONTS: &[&str] = &["ttf", "otf", "woff", "woff2", "eot"];
/// Database file extensions.
const DATABASES: &[&str] = &["db", "sqlite", "mdb", "accdb", "sql", "ldf", "mdf", "ndf"];
/// Configuration file extensions.
const CONFIGS: &[&str] = &["ini", "cfg", "conf", "yaml", "yml", "toml", "json", "xml"];
/// Log file extensions.
const LOGS: &[&str] = &["log", "out", "err", "trace"];
/// Backup/temporary file extensions.
const BACKUPS: &[&str] = &["bak", "old", "orig", "swp", "tmp", "temp"];
/// Disk image / virtual disk extensions.
const DISK_IMAGES: &[&str] = &["vmdk", "vhd", "vhdx", "vdi", "qcow2", "img", "wim"];

/// Return the lowercase extension without a leading dot.
#[must_use]
pub fn extension_from_name(name: &str) -> Option<&str> {
    let dot = name.rfind('.')?;
    if dot == 0 || dot + 1 >= name.len() {
        return None;
    }
    name.get(dot + 1..)
}

/// Return whether a name is an NTFS metadata/system entry.
#[must_use]
pub fn is_system_name(name: &str) -> bool {
    name.starts_with('$')
}

/// Semantic type/category name for a row.
#[must_use]
pub fn semantic_type_for_row(row: &DisplayRow) -> &'static str {
    if row.is_directory {
        return "directory";
    }

    let Some(ext) = extension_from_name(row.name()) else {
        return "file";
    };
    let ext_lower = ext.to_ascii_lowercase();
    semantic_type_from_extension(&ext_lower)
}

/// Semantic type/category name for an extension.
#[must_use]
pub fn semantic_type_from_extension(ext: &str) -> &'static str {
    if collections::DOCUMENTS.contains(&ext) {
        "document"
    } else if collections::PICTURES.contains(&ext) {
        "picture"
    } else if collections::VIDEOS.contains(&ext) {
        "video"
    } else if collections::MUSIC.contains(&ext) {
        "audio"
    } else if collections::ARCHIVES.contains(&ext) {
        "archive"
    } else if collections::CODE.contains(&ext) {
        "code"
    } else if EXECUTABLES.contains(&ext) {
        "executable"
    } else if FONTS.contains(&ext) {
        "font"
    } else if DATABASES.contains(&ext) {
        "database"
    } else if CONFIGS.contains(&ext) {
        "config"
    } else if LOGS.contains(&ext) {
        "log"
    } else if BACKUPS.contains(&ext) {
        "backup"
    } else if DISK_IMAGES.contains(&ext) {
        "disk"
    } else {
        "other"
    }
}

/// Tree-allocated metric for projection/sort/filter.
#[must_use]
pub const fn tree_allocated_for_row(row: &DisplayRow) -> u64 {
    if row.is_directory {
        row.tree_allocated
    } else {
        row.allocated
    }
}

/// Bulkiness metric as fixed-point ratio scaled by [`BULKINESS_SCALE`].
#[must_use]
pub const fn bulkiness_for_row(row: &DisplayRow) -> u64 {
    let (logical, allocated) = if row.is_directory {
        (row.treesize, row.tree_allocated)
    } else {
        (row.size, row.allocated)
    };
    if logical == 0 {
        return 0;
    }
    allocated.saturating_mul(BULKINESS_SCALE) / logical
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::search::backend::DisplayRow;

    #[test]
    fn semantic_type_categorizes_known_extensions() {
        let row = DisplayRow::new(
            0,
            'C',
            "C:\\docs\\report.pdf".to_owned(),
            12,
            false,
            0,
            0,
            0,
            0x20,
            16,
            0,
            0,
            16,
        );
        assert_eq!(semantic_type_for_row(&row), "document");
        assert_eq!(semantic_type_from_extension("rs"), "code");
        assert_eq!(semantic_type_from_extension("zip"), "archive");
    }

    #[test]
    fn bulkiness_uses_tree_metrics_for_directories() {
        let row = DisplayRow::new(
            0,
            'C',
            "C:\\dir".to_owned(),
            12,
            true,
            0,
            0,
            0,
            0x10,
            16,
            3,
            200,
            300,
        );
        assert_eq!(tree_allocated_for_row(&row), 300);
        assert_eq!(bulkiness_for_row(&row), 1_500_000);
    }
}
