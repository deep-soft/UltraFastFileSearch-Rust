#!/usr/bin/env rust-script
//! Generate a default NTFS $UpCase table (128 KB binary).
//!
//! This produces a table that matches NTFS behavior: each u16 entry at
//! index `i` contains the uppercase mapping for codepoint `i`.
//!
//! For codepoints without an uppercase mapping, the identity is used.

use std::io::Write;

fn main() {
    let mut table = vec![0u16; 65536];

    // Initialize with identity mapping
    for i in 0u32..65536 {
        table[i as usize] = i as u16;
    }

    // Apply Unicode uppercase mappings for all BMP codepoints.
    // char::from_u32 + to_uppercase gives us the standard Unicode mapping.
    for cp in 0u32..65536 {
        if let Some(ch) = char::from_u32(cp) {
            let mut upper_chars = ch.to_uppercase();
            if let Some(upper) = upper_chars.next() {
                // Only use simple (1:1) case mappings — NTFS $UpCase is 1:1.
                // If the uppercase is a single char in BMP, use it.
                if upper_chars.next().is_none() {
                    let upper_cp = upper as u32;
                    if upper_cp < 65536 {
                        table[cp as usize] = upper_cp as u16;
                    }
                }
            }
        }
    }

    // Write as little-endian u16 array (128 KB)
    let output_path = std::env::args().nth(1).unwrap_or_else(|| {
        "crates/uffs-text/src/upcase_default.bin".to_string()
    });
    let mut file = std::fs::File::create(&output_path).expect("create file");
    for &val in &table {
        file.write_all(&val.to_le_bytes()).expect("write");
    }
    eprintln!("Wrote {} bytes to {}", 65536 * 2, output_path);

    // Verify key mappings
    assert_eq!(table[b'a' as usize], b'A' as u16);
    assert_eq!(table[b'z' as usize], b'Z' as u16);
    assert_eq!(table[b'A' as usize], b'A' as u16);
    assert_eq!(table[b'0' as usize], b'0' as u16);
    assert_eq!(table[0x00FC], 0x00DC); // ü → Ü
    assert_eq!(table[0x00E9], 0x00C9); // é → É
    assert_eq!(table[0x00F6], 0x00D6); // ö → Ö
    assert_eq!(table[0x4E2D], 0x4E2D); // 中 → 中 (identity)
    eprintln!("All assertions passed.");
}

