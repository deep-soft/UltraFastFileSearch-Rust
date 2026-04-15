// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025-2026 SKY, LLC.

//! # uffs-text: Unicode Text Processing for UFFS
//!
//! Layer 0 foundation crate providing NTFS-compatible case folding
//! and text processing primitives. No internal crate dependencies.
//!
//! ## Current Capabilities
//!
//! - **[`case_fold::CaseFold`]**: NTFS `$UpCase` case folding engine (128 KB
//!   table, `Copy`, zero-alloc comparisons, buffer-reuse folding)
//! - **Trigram key helpers**: [`trigram_key::pack_char_trigram`] /
//!   [`trigram_key::unpack_char_trigram`] for packing 3 folded `u16` codepoints
//!   into a `u64`.
//!
//! ## Future (i18n)
//!
//! - Unicode normalisation (NFC/NFD)
//! - Script detection
//! - Locale-aware collation
//! - Search tokenisation

pub mod case_fold;
pub mod trigram_key;
