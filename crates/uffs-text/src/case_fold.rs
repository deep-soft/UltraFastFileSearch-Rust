//! NTFS-compatible case folding via the `$UpCase` table.
//!
//! The `$UpCase` table is a 128 KB flat array mapping every BMP Unicode
//! codepoint (0x0000–0xFFFF) to its uppercase equivalent.  NTFS uses this
//! table for ALL case-insensitive filename operations.
//!
//! For case-insensitive comparison, we fold both sides to uppercase
//! (matching NTFS semantics) and compare the folded values.

/// Alignment wrapper for the embedded `$UpCase` binary (128 KB).
///
/// `include_bytes!` returns `&[u8]` with alignment 1, but
/// `bytemuck::cast_slice` to `&[u16]` requires alignment 2.  This wrapper
/// guarantees correct alignment at the linker level.
#[repr(C, align(2))]
struct Aligned128K {
    /// Raw little-endian bytes of the `$UpCase` table (65 536 × `u16`).
    data: [u8; 131_072],
}

/// Default `$UpCase` table compiled into the binary (128 KB).
/// Generated from Unicode standard uppercase mappings matching NTFS behavior.
/// Covers all BMP codepoints (U+0000–U+FFFF).
static DEFAULT_UPCASE_ALIGNED: Aligned128K = Aligned128K {
    data: *include_bytes!("upcase_default.bin"),
};

/// NTFS-compatible case-folding engine.
///
/// Wraps a reference to a `$UpCase` table (128 KB, 65 536 × `u16`).
/// `Copy` and cheap to pass by value — it is just a pointer.
///
/// # Construction
///
/// - [`CaseFold::default_table()`] — compiled-in default (always available).
/// - [`CaseFold::from_ntfs()`] — live table read from an NTFS volume.
#[derive(Clone, Copy)]
pub struct CaseFold {
    /// 65 536-entry `u16` table. Each entry maps a BMP codepoint to its
    /// uppercase equivalent.  Non-BMP codepoints (> U+FFFF) are identity.
    table: &'static [u16],
}

impl CaseFold {
    /// Create from the compiled-in default `$UpCase` table.
    #[must_use]
    pub fn default_table() -> Self {
        let table: &[u16] = bytemuck::cast_slice(&DEFAULT_UPCASE_ALIGNED.data);
        Self { table }
    }

    /// Create from a live `$UpCase` table read from an NTFS volume.
    ///
    /// The caller must ensure the slice is at least 65 536 entries and has
    /// `'static` lifetime (e.g. via `Box::leak`).
    #[must_use]
    pub fn from_ntfs(table: &'static [u16]) -> Self {
        debug_assert!(table.len() >= 65_536, "$UpCase table too short");
        Self { table }
    }

    /// Borrow the underlying table for serialization or inspection.
    #[must_use]
    pub const fn table(&self) -> &'static [u16] {
        self.table
    }

    // ── Per-codepoint fold ────────────────────────────────────────

    /// Fold a single Unicode codepoint to its NTFS uppercase equivalent.
    ///
    /// BMP codepoints (< U+10000): O(1) table lookup.
    /// Non-BMP (emoji, rare CJK): returned as-is (no case).
    #[inline]
    #[must_use]
    pub fn fold_char(&self, ch: char) -> u16 {
        let cp = ch as u32;
        if cp < 0x10000 {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "cp < 0x10000 guarantees u16 fits"
            )]
            let fallback = cp as u16;
            self.table.get(cp as usize).copied().unwrap_or(fallback)
        } else {
            // Non-BMP — no uppercase mapping; truncate to u16 is intentional:
            // callers comparing two non-BMP chars will get the same low 16 bits
            // for identical chars and almost certainly different bits for
            // different chars. Full correctness for supplementary planes is
            // deferred to i18n Phase 2.
            #[expect(
                clippy::cast_possible_truncation,
                reason = "non-BMP identity: only low 16 bits stored; acceptable for filename trigrams"
            )]
            {
                cp as u16
            }
        }
    }

    // ── String comparison (Tier 1 — zero alloc) ───────────────────

    /// Case-insensitive ordering of two UTF-8 strings.
    /// Zero allocations — folds lazily per codepoint.
    #[inline]
    #[must_use]
    pub fn cmp_str(&self, lhs: &str, rhs: &str) -> core::cmp::Ordering {
        let mut lhs_chars = lhs.chars();
        let mut rhs_chars = rhs.chars();
        loop {
            match (lhs_chars.next(), rhs_chars.next()) {
                (None, None) => return core::cmp::Ordering::Equal,
                (None, Some(_)) => return core::cmp::Ordering::Less,
                (Some(_), None) => return core::cmp::Ordering::Greater,
                (Some(ca), Some(cb)) => {
                    let fa = self.fold_char(ca);
                    let fb = self.fold_char(cb);
                    match fa.cmp(&fb) {
                        core::cmp::Ordering::Equal => {}
                        core::cmp::Ordering::Less | core::cmp::Ordering::Greater => {
                            return fa.cmp(&fb);
                        }
                    }
                }
            }
        }
    }

    /// Case-insensitive equality of two UTF-8 strings.
    #[inline]
    #[must_use]
    pub fn eq_str(&self, lhs: &str, rhs: &str) -> bool {
        self.cmp_str(lhs, rhs) == core::cmp::Ordering::Equal
    }

    // ── Buffer-reuse fold (Tier 2 — one reusable buffer) ──────────

    /// Fold a UTF-8 name into a reusable `u8` buffer as uppercase UTF-8.
    ///
    /// The buffer is cleared and reused — zero heap allocation after the
    /// first call (buffer capacity persists across calls).
    ///
    /// Returns the folded bytes as a `&str` slice into the buffer.
    pub fn fold_into<'buf>(&self, name: &str, buf: &'buf mut Vec<u8>) -> &'buf str {
        buf.clear();
        let mut encode_buf = [0_u8; 4];
        for ch in name.chars() {
            let cp = ch as u32;
            if cp < 0x80 {
                // ASCII fast path — folded value guaranteed ≤ 0x7F for
                // ASCII inputs, so the u8 truncation is safe.
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "cp < 0x80 → folded value fits in u8 for ASCII inputs"
                )]
                let fallback = cp as u16;
                let folded = self.table.get(cp as usize).copied().unwrap_or(fallback);
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "ASCII uppercase ≤ 0x7F fits in u8"
                )]
                let byte = folded as u8;
                buf.push(byte);
            } else if cp < 0x10000 {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "cp < 0x10000 → fits in u16"
                )]
                let fallback = cp as u16;
                let folded_cp = u32::from(self.table.get(cp as usize).copied().unwrap_or(fallback));
                if let Some(folded_ch) = char::from_u32(folded_cp) {
                    buf.extend_from_slice(folded_ch.encode_utf8(&mut encode_buf).as_bytes());
                }
            } else {
                // Non-BMP — pass through unchanged.
                buf.extend_from_slice(ch.encode_utf8(&mut encode_buf).as_bytes());
            }
        }
        // We wrote valid UTF-8 chars above; fall back to empty on error.
        core::str::from_utf8(buf.as_slice()).unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The compiled-in default `$UpCase` table must be exactly 65 536 entries.
    #[test]
    fn upcase_table_size() {
        let fold = CaseFold::default_table();
        assert_eq!(fold.table.len(), 65_536);
    }

    /// ASCII lowercase letters must map to their uppercase equivalents.
    #[test]
    fn ascii_lowercase_to_uppercase() {
        let fold = CaseFold::default_table();
        for lower in b'a'..=b'z' {
            let upper = lower - b' '; // a=0x61, A=0x41; diff=0x20
            assert_eq!(
                fold.fold_char(char::from(lower)),
                u16::from(upper),
                "0x{lower:02X} ({}) should fold to 0x{upper:02X} ({})",
                char::from(lower),
                char::from(upper),
            );
        }
    }

    /// ASCII uppercase, digits, and printable symbols are identity-mapped.
    #[test]
    fn ascii_identity_codepoints() {
        let fold = CaseFold::default_table();
        // Uppercase letters
        for ch in b'A'..=b'Z' {
            assert_eq!(fold.fold_char(char::from(ch)), u16::from(ch));
        }
        // Digits
        for ch in b'0'..=b'9' {
            assert_eq!(fold.fold_char(char::from(ch)), u16::from(ch));
        }
        // NUL
        assert_eq!(fold.fold_char('\0'), 0);
    }

    /// European accented characters must fold correctly (NTFS `$UpCase`).
    #[test]
    fn european_accented_characters() {
        let fold = CaseFold::default_table();
        // ü (U+00FC) → Ü (U+00DC)
        assert_eq!(fold.fold_char('\u{00FC}'), 0x00DC, "ü → Ü");
        // é (U+00E9) → É (U+00C9)
        assert_eq!(fold.fold_char('\u{00E9}'), 0x00C9, "é → É");
        // ö (U+00F6) → Ö (U+00D6)
        assert_eq!(fold.fold_char('\u{00F6}'), 0x00D6, "ö → Ö");
        // ñ (U+00F1) → Ñ (U+00D1)
        assert_eq!(fold.fold_char('\u{00F1}'), 0x00D1, "ñ → Ñ");
        // å (U+00E5) → Å (U+00C5)
        assert_eq!(fold.fold_char('\u{00E5}'), 0x00C5, "å → Å");
    }

    /// CJK ideographs have no case — they must be identity-mapped.
    #[test]
    fn cjk_identity() {
        let fold = CaseFold::default_table();
        // 中 (U+4E2D)
        assert_eq!(fold.fold_char('\u{4E2D}'), 0x4E2D, "中 identity");
        // 文 (U+6587)
        assert_eq!(fold.fold_char('\u{6587}'), 0x6587, "文 identity");
    }

    /// Cyrillic lowercase must fold to uppercase.
    #[test]
    fn cyrillic_folding() {
        let fold = CaseFold::default_table();
        // д (U+0434) → Д (U+0414)
        assert_eq!(fold.fold_char('\u{0434}'), 0x0414, "д → Д");
        // я (U+044F) → Я (U+042F)
        assert_eq!(fold.fold_char('\u{044F}'), 0x042F, "я → Я");
    }

    /// `fold_into` must produce correct case-folded strings.
    #[test]
    fn fold_into_mixed_string() {
        let fold = CaseFold::default_table();
        let mut buf = Vec::new();
        let result = fold.fold_into("Hello.TXT", &mut buf);
        assert_eq!(result, "HELLO.TXT");
    }

    /// `fold_into` with accented characters.
    #[test]
    fn fold_into_accented() {
        let fold = CaseFold::default_table();
        let mut buf = Vec::new();
        let result = fold.fold_into("über", &mut buf);
        assert_eq!(result, "\u{00DC}BER");
    }

    /// `cmp_str` must be case-insensitive.
    #[test]
    fn cmp_str_case_insensitive() {
        let fold = CaseFold::default_table();
        assert_eq!(fold.cmp_str("hello", "HELLO"), core::cmp::Ordering::Equal);
        assert_eq!(fold.cmp_str("abc", "ABD"), core::cmp::Ordering::Less);
    }
}
