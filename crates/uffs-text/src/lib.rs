//! # uffs-text: Unicode Text Processing for UFFS
//!
//! Layer 0 foundation crate providing NTFS-compatible case folding
//! and text processing primitives. No internal crate dependencies.
//!
//! ## Current Capabilities
//!
//! - **[`CaseFold`]**: NTFS `$UpCase` case folding engine (128 KB table,
//!   `Copy`, zero-alloc comparisons, buffer-reuse folding)
//! - **Trigram key helpers**: [`pack_char_trigram`] / [`unpack_char_trigram`]
//!   for packing 3 folded `u16` codepoints into a `u64`.
//!
//! ## Future (i18n)
//!
//! - Unicode normalisation (NFC/NFD)
//! - Script detection
//! - Locale-aware collation
//! - Search tokenisation

mod case_fold;
mod trigram_key;

pub use case_fold::CaseFold;
pub use trigram_key::{pack_char_trigram, unpack_char_trigram};
