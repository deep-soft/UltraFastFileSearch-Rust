//! Trigram key helpers for character-level trigram indices.
//!
//! Provides pack/unpack between 3 folded `u16` codepoints and a `u64`
//! key suitable for hash-map lookup.  Used by the `TrigramIndex` in
//! `uffs-core`.

/// Pack 3 folded `u16` codepoints into a `u64`.
///
/// Layout: `[cp0:16][cp1:16][cp2:16]` in the high 48 bits, low 16 bits zero.
/// This gives lexicographic ordering when the `u64` is compared directly.
#[inline]
#[must_use]
pub const fn pack_char_trigram(cp0: u16, cp1: u16, cp2: u16) -> u64 {
    (cp0 as u64) << 32 | (cp1 as u64) << 16 | (cp2 as u64)
}

/// Unpack a `u64` back to 3 folded `u16` codepoints.
#[inline]
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    reason = "right-shift guarantees each fragment fits u16"
)]
pub const fn unpack_char_trigram(packed: u64) -> [u16; 3] {
    [(packed >> 32) as u16, (packed >> 16) as u16, packed as u16]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_ascii() {
        let ch_a = u16::from(b'A');
        let ch_b = u16::from(b'B');
        let ch_c = u16::from(b'C');
        let packed = pack_char_trigram(ch_a, ch_b, ch_c);
        let unpacked = unpack_char_trigram(packed);
        assert_eq!(unpacked, [ch_a, ch_b, ch_c]);
    }

    #[test]
    fn roundtrip_unicode() {
        let ch_u_uml = 0x00DC_u16; // Ü
        let ch_b = 0x0042_u16; // B
        let ch_e = 0x0045_u16; // E
        let packed = pack_char_trigram(ch_u_uml, ch_b, ch_e);
        let unpacked = unpack_char_trigram(packed);
        assert_eq!(unpacked, [ch_u_uml, ch_b, ch_e]);
    }

    #[test]
    fn lexicographic_order() {
        let abc = pack_char_trigram(u16::from(b'A'), u16::from(b'B'), u16::from(b'C'));
        let abd = pack_char_trigram(u16::from(b'A'), u16::from(b'B'), u16::from(b'D'));
        assert!(abc < abd);
    }
}
