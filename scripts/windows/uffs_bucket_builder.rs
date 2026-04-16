use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const RAW_ROOT: &str = r"C:\Users\rnio\uffs_data";
const BUCKET_ROOT_PREFIX: &str = r"C:\Users\rnio\uffs_scale_";

/// Drives marked Automatic are not copied into bucket dirs.
const AUTOMATIC_DRIVES: &[char] = &['C', 'D', 'E', 'F', 'G', 'M', 'S'];

/// Fallback template when a raw drive folder does not exist.
const CLONE_TEMPLATE_DRIVE: char = 'S';

fn main() -> io::Result<()> {
    let buckets = bucket_map();

    println!("RAW_ROOT = {}", RAW_ROOT);
    println!("BUCKET_ROOT_PREFIX = {}", BUCKET_ROOT_PREFIX);
    println!("Automatic drives are excluded from bucket dirs: {:?}", AUTOMATIC_DRIVES);
    println!("Missing raw drives will be cloned from drive_{}", CLONE_TEMPLATE_DRIVE.to_ascii_lowercase());
    println!();

    for (bucket_name, drives) in buckets {
        let bucket_dir = PathBuf::from(format!("{}{}", BUCKET_ROOT_PREFIX, bucket_name));
        ensure_bucket(&bucket_dir, &drives)?;
    }

    println!("\nDone.");
    Ok(())
}

fn bucket_map() -> BTreeMap<&'static str, Vec<char>> {
    // Only NON-AUTOMATIC drives belong in bucket dirs.
    //
    // Based on your table:
    // 25M  -> no manual drives
    // 50M  -> H I J
    // 75M  -> H I J K L N
    // 100M -> H I J K L N O P Q
    // 125M -> H I J K L N O P Q R T U
    // 150M -> H I J K L N O P Q R T U V W X
    // 180M -> A B H I J K L N O P Q R T U V W X Y Z
    let mut map = BTreeMap::new();

    map.insert("25M", vec![]);
    map.insert("50M", vec!['H', 'I', 'J']);
    map.insert("75M", vec!['H', 'I', 'J', 'K', 'L', 'N']);
    map.insert("100M", vec!['H', 'I', 'J', 'K', 'L', 'N', 'O', 'P', 'Q']);
    map.insert("125M", vec!['H', 'I', 'J', 'K', 'L', 'N', 'O', 'P', 'Q', 'R', 'T', 'U']);
    map.insert("150M", vec!['H', 'I', 'J', 'K', 'L', 'N', 'O', 'P', 'Q', 'R', 'T', 'U', 'V', 'W', 'X']);
    map.insert("180M", vec!['A', 'B', 'H', 'I', 'J', 'K', 'L', 'N', 'O', 'P', 'Q', 'R', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z']);

    map
}

fn ensure_bucket(bucket_dir: &Path, requested_drives: &[char]) -> io::Result<()> {
    fs::create_dir_all(bucket_dir)?;

    let automatic: BTreeSet<char> = AUTOMATIC_DRIVES.iter().copied().collect();
    let required: Vec<char> = requested_drives
        .iter()
        .copied()
        .filter(|d| !automatic.contains(d))
        .collect();

    println!("=== Bucket: {} ===", bucket_dir.display());
    println!("Required non-automatic drives: {:?}", required);

    // Check what already exists in the bucket.
    let existing = list_bucket_drives(bucket_dir)?;
    if !existing.is_empty() {
        println!("Existing bucket drives: {:?}", existing);
    }

    for drive in required {
        let drive_dir_name = drive_dir_name(drive);
        let target = bucket_dir.join(&drive_dir_name);

        if target.exists() {
            println!("  OK   {} already exists", target.display());
            continue;
        }

        let source = raw_drive_path(drive);
        if source.exists() {
            println!("  COPY {} -> {}", source.display(), target.display());
            copy_dir_recursive(&source, &target)?;
        } else {
            let template = raw_drive_path(CLONE_TEMPLATE_DRIVE);
            if !template.exists() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "Raw drive {} is missing, and template {} is also missing",
                        source.display(),
                        template.display()
                    ),
                ));
            }

            println!(
                "  CLONE missing {} from template {} -> {}",
                source.display(),
                template.display(),
                target.display()
            );
            copy_dir_recursive(&template, &target)?;
        }
    }

    println!();
    Ok(())
}

fn drive_dir_name(drive: char) -> String {
    format!("drive_{}", drive.to_ascii_lowercase())
}

fn raw_drive_path(drive: char) -> PathBuf {
    Path::new(RAW_ROOT).join(drive_dir_name(drive))
}

fn list_bucket_drives(bucket_dir: &Path) -> io::Result<Vec<String>> {
    let mut names = Vec::new();

    for entry in fs::read_dir(bucket_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                names.push(name.to_string());
            }
        }
    }

    names.sort();
    Ok(names)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    if !src.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Source does not exist: {}", src.display()),
        ));
    }

    fs::create_dir_all(dst)?;

    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            if let Some(parent) = dst_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&src_path, &dst_path)?;
        } else if file_type.is_symlink() {
            // On Windows, simplest safe behavior is to skip symlinks unless you
            // explicitly want to resolve/copy them.
            println!("  WARN skipping symlink {}", src_path.display());
        }
    }

    Ok(())
}