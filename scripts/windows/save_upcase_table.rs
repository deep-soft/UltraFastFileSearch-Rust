#!/usr/bin/env rust-script
//! Read the NTFS `$UpCase` table from a live Windows volume and save it.
//!
//! `$UpCase` is a 128 KB system file (65 536 × u16) containing the
//! uppercase mapping table Windows uses for case-insensitive filename
//! comparisons.  This script reads it directly as a file — no MFT
//! parsing required.
//!
//! # Usage (run as Administrator)
//!
//! ```powershell
//! # Save from boot drive (C:) to default location
//! rust-script scripts/windows/save_upcase_table.rs
//!
//! # Save from a specific drive to a specific path
//! rust-script scripts/windows/save_upcase_table.rs --drive D --output my_upcase.bin
//!
//! # Compare against the compiled-in default table
//! rust-script scripts/windows/save_upcase_table.rs --compare crates/uffs-text/src/upcase_default.bin
//! ```
//!
//! # Output
//!
//! Raw little-endian `[u16; 65_536]` — 131 072 bytes, identical format
//! to `crates/uffs-text/src/upcase_default.bin`.

use std::fs;
use std::path::PathBuf;

const EXPECTED_SIZE: usize = 65_536 * 2; // 131_072 bytes

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut drive = 'C';
    let mut output = PathBuf::from("upcase_table.bin");
    let mut compare_path: Option<PathBuf> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--drive" | "-d" => {
                i += 1;
                drive = args.get(i)
                    .and_then(|s| s.chars().next())
                    .expect("--drive requires a letter (e.g. C)");
            }
            "--output" | "-o" => {
                i += 1;
                output = PathBuf::from(args.get(i).expect("--output requires a path"));
            }
            "--compare" | "-c" => {
                i += 1;
                compare_path = Some(PathBuf::from(args.get(i).expect("--compare requires a path")));
            }
            "--help" | "-h" => {
                eprintln!("Usage: save_upcase_table.rs [--drive C] [--output upcase.bin] [--compare default.bin]");
                eprintln!();
                eprintln!("Reads the NTFS $UpCase table from a live volume (requires Admin).");
                eprintln!();
                eprintln!("Options:");
                eprintln!("  --drive, -d     Drive letter (default: C)");
                eprintln!("  --output, -o    Output file path (default: upcase_table.bin)");
                eprintln!("  --compare, -c   Compare against another .bin file and report diffs");
                std::process::exit(0);
            }
            other => {
                eprintln!("Unknown argument: {other}");
                std::process::exit(1);
            }
        }
        i += 1;
    }

    let drive = drive.to_ascii_uppercase();
    let upcase_path = format!("\\\\?\\{drive}:\\$UpCase");

    eprintln!("═══════════════════════════════════════════════════════════");
    eprintln!("  Reading $UpCase from {drive}:");
    eprintln!("  Path: {upcase_path}");
    eprintln!("═══════════════════════════════════════════════════════════");

    // Read the file directly — Windows allows this with Admin + backup semantics.
    let data = match fs::read(&upcase_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!();
            eprintln!("ERROR: Failed to read {upcase_path}: {e}");
            eprintln!();
            eprintln!("Make sure you are running as Administrator on an NTFS volume.");
            std::process::exit(1);
        }
    };

    if data.len() != EXPECTED_SIZE {
        eprintln!(
            "WARNING: $UpCase is {} bytes (expected {EXPECTED_SIZE})",
            data.len()
        );
    }

    // Save to output file.
    fs::write(&output, &data).unwrap_or_else(|e| {
        eprintln!("ERROR: Failed to write {}: {e}", output.display());
        std::process::exit(1);
    });

    eprintln!();
    eprintln!("  Saved: {} ({} bytes)", output.display(), data.len());

    // Quick sanity checks on the table.
    if data.len() >= EXPECTED_SIZE {
        let table: &[u16] = unsafe {
            std::slice::from_raw_parts(data.as_ptr().cast::<u16>(), 65_536)
        };
        eprintln!();
        eprintln!("  Sanity checks:");
        eprintln!("    'a' → '{}'  (expect 'A')", char::from(table[b'a' as usize] as u8));
        eprintln!("    'z' → '{}'  (expect 'Z')", char::from(table[b'z' as usize] as u8));
        eprintln!("    ü (0x00FC) → 0x{:04X}  (expect 0x00DC = Ü)", table[0x00FC]);
        eprintln!("    é (0x00E9) → 0x{:04X}  (expect 0x00C9 = É)", table[0x00E9]);
        eprintln!("    中 (0x4E2D) → 0x{:04X}  (expect 0x4E2D = identity)", table[0x4E2D]);
    }

    // Compare mode.
    if let Some(ref cmp_path) = compare_path {
        eprintln!();
        let cmp_data = fs::read(cmp_path).unwrap_or_else(|e| {
            eprintln!("ERROR: Failed to read {}: {e}", cmp_path.display());
            std::process::exit(1);
        });

        if data == cmp_data {
            eprintln!("  ✅ Tables are IDENTICAL ({} bytes)", data.len());
        } else {
            let min_len = data.len().min(cmp_data.len());
            let live: &[u16] = unsafe {
                std::slice::from_raw_parts(data.as_ptr().cast::<u16>(), min_len / 2)
            };
            let default: &[u16] = unsafe {
                std::slice::from_raw_parts(cmp_data.as_ptr().cast::<u16>(), min_len / 2)
            };
            let mut diffs = 0;
            for i in 0..live.len() {
                if live[i] != default[i] {
                    if diffs < 20 {
                        eprintln!(
                            "  DIFF at 0x{i:04X}: live=0x{:04X} default=0x{:04X}",
                            live[i], default[i]
                        );
                    }
                    diffs += 1;
                }
            }
            eprintln!("  ⚠️  {diffs} differences found out of {} entries", live.len());
        }
    }

    eprintln!();
    eprintln!("  Done.");
}

