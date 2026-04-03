# 18 — NTFS-Compatible High-Performance Case Folding: Implementation Guide

> **Date:** 2026-04-02
> **Revised:** 2026-04-03 — Phases 2F, 3–5 implemented
> **Last verified:** 2026-04-03 — implementation audit against live codebase
> **Status:** All phases implemented ✅ (3C Arc results cancelled — low ROI)
> **Prerequisite docs:** 15 (bottleneck inventory), 16 (lowercase strategy),
> 17 ($UpCase cost/benefit analysis)
>
> **Goal:** Combine NTFS-native `$UpCase` case folding with every feasible
> performance optimisation from the bottleneck analysis into a single,
> ordered implementation plan. The result: 100% NTFS-compatible
> case-insensitive search with zero unnecessary allocations, ~400 MB less
> transient memory, and <7 ms additional search latency.

---

## Implementation Status Summary (2026-04-03)

| Phase | Step | Description | Status | Notes |
|---|---|---|---|---|
| **0** | 0A | `uffs-text` crate scaffold | ✅ Done | `crates/uffs-text/` exists |
| | 0B | Wire into workspace | ✅ Done | workspace members + deps |
| | 0C | `CaseFold` struct + fold helpers | ✅ Done | `case_fold.rs`, full API incl. `fold_to_u16`, `eq_folded`, `starts_with_folded`, `ends_with_folded`, `contains_folded` |
| | 0D | Default `$UpCase` table binary | ✅ Done | `upcase_default.bin` (128 KB) |
| | 0E | Trigram pack/unpack helpers | ✅ Done | `trigram_key.rs` |
| | 0F | Windows `$UpCase` reader | ✅ Done | `platform/upcase.rs` reads live table; `compact.rs::resolve_case_fold()` tries live → fallback to default; diffs logged via tracing |
| | 0G | Wire `uffs-text` into downstream crates | ✅ Done | `DriveCompactIndex.fold` field, `uffs-text` dep in `uffs-core` |
| | 0H | `rustc-hash` dep for `FxHashMap` | ✅ Done | Used in `trigram.rs`, `tree.rs`, `fast.rs` |
| **1** | 1A | Change trigram key type to char-based | ✅ Done | Keys are `u64` (packed char trigrams) |
| | 1B | Replace flat LUT with `FxHashMap` | ✅ Done | `FxHashMap<u64, u32>` in build |
| | 1C | Rewrite build pass 1 with char-level fold | ✅ Done | Parallel chunks, `CaseFold` |
| | 1D | Rewrite scatter pass 2 with char-level fold | ✅ Done | `scatter_one_record` uses char trigrams |
| | 1E | Update `TrigramIndex::search` | ✅ Done | `search(&self, needle, fold)` |
| | 1F | Update 3 callers (no clone) | ✅ Done | compact, cache, loader all pass `fold` by value |
| **2** | 2A | Search match loop fold swap | ✅ Done | `fold.fold_into(name, buf)`, reusable buffer, pre-folded needle |
| | 2B | `sort_indices_by_name` zero-alloc | ✅ Done | `fold.fold_char(ch)` in sort key |
| | 2C | Tree search fold + buffer threading | ✅ Done | `fold_buf` threaded through `tree_search` |
| | 2D | Numeric sort fast path | ✅ Done | `sort_rows_with_fold` with Schwartzian transform |
| | 2E | Filter fold swap | ✅ Done | `matches_record(…, fold)` |
| | 2F | `index_search/pattern.rs` | ✅ Done | Pre-folded `Vec<u16>` patterns, `CaseFold`-based zero-alloc matching (6 new comparison methods) |
| **3** | 3A | `BinaryHeap` for global top-N | ✅ Done | O(N log K) capped heap in `collect_global_top_n_numeric`; fallback for unlimited |
| | 3B | `FxHashMap` for `DirCache` | ✅ Done | `tree.rs`: `FxHashMap<u32, String>` via `DirCacheExt` trait |
| | 3C | `Arc<Vec<DisplayRow>>` for results | ❌ Cancelled | Invasive change; conflicts with in-place `sort_rows()`; `clone_from` is already efficient. Low ROI. |
| | 3D | `FxHashMap` for `path_cache` | ✅ Done | `fast.rs`: `FxHashMap<u64, String>` — eliminates 168 MB sparse alloc |
| | 3E | `LazyLock` for `CACHE_PROFILE` | ✅ Done | `static CACHE_PROFILE: LazyLock<bool>` — one env read, not 3 per search |
| | 3F | Stack-allocated `volume_prefix` | ✅ Done | `stack_volume_prefix()` — zero heap alloc per search |
| **4** | 4A | Cache v6 header | ✅ Done | `COMPACT_VERSION = 6`; no separate `$UpCase` section (uses `CaseFold::default_table()`) |
| | 4B | Serialize char-trigram CSR | ✅ Done | v6 writes `tri_keys: u64[]`, `tri_offsets: u32[]`, `tri_values: u32[]` — zero-rebuild on load |
| | 4C | Backward-compatible v5 load | ✅ Done | v5 caches accepted: trigram rebuilt with `CaseFold`; 5 round-trip tests |
| **5** | 5A | Drop `MftIndex` early | ✅ Done | `drop(mft_index)` in `compact_loader.rs:124` after `build_compact_index()` |
| | 5B | Scope `MftIndex` in daemon startup | ✅ Done | Daemon uses `load_drive()` → `compact_loader::load_drive()` which has the early drop |

**Summary:** All phases are complete. NTFS-compatible `CaseFold` replaces
`to_ascii_lowercase()` across the entire search pipeline: trigram build, search,
sort, filter, tree traversal, and pattern matching. Performance optimisations
(BinaryHeap, FxHashMap caches, LazyLock, stack alloc) and cache v6 (persisted
char-trigram CSR) are all shipped. The only cancelled item is 3C
(Arc results — low ROI).

---

## Table of Contents

1. [Crate Architecture Decision](#1-crate-architecture-decision)
2. [Architecture Overview](#2-architecture-overview)
3. [Dependency Map](#3-dependency-map)
4. [Phase 0 — Foundation: `uffs-text` Crate + CaseFold](#phase-0)
5. [Phase 1 — Trigram Migration: Byte → Character](#phase-1)
6. [Phase 2 — Search/Filter/Sort Fold Swap](#phase-2)
7. [Phase 3 — Performance Optimisations (P1–P15)](#phase-3)
8. [Phase 4 — Cache v6 Format](#phase-4)
9. [Phase 5 — Caller-Level Memory Optimisations](#phase-5)
10. [Testing Strategy](#testing-strategy)
11. [Rollback Plan](#rollback-plan)
12. [Success Criteria](#success-criteria)

---

## 1. Crate Architecture Decision

### Why a New Crate?

The original plan placed `CaseFold` in `uffs-core` and the `$UpCase` reader
in `uffs-mft`. This creates a **dependency split problem**:

```
Current dependency flow (one-way):
  uffs-polars ← uffs-mft ← uffs-core ← surfaces (cli/tui/daemon)
```

If `CaseFold` lives in `uffs-core`, then `uffs-mft` **cannot import it**
(circular dependency). Yet `uffs-mft` will need text processing for:
- Unicode-aware name normalisation during MFT parse (future)
- Locale-aware collation keys stored in the index (future i18n)
- Any filename canonicalisation at the I/O layer

### Solution: `uffs-text` — Layer 0 Foundation Crate

Follows the established pattern of `uffs-security` (Layer 0, no internal
crate dependencies):

```
Layer 0 (Foundation):
  uffs-polars     Polars facade (compilation isolation)
  uffs-security   Crypto, key storage, secure FS ops
  uffs-text       Unicode case folding, text processing, i18n foundation  ← NEW

Layer 1 (Engine):
  uffs-mft        MFT reading → depends on uffs-polars, uffs-security, uffs-text
  uffs-core       Query engine → depends on uffs-mft (transitive: uffs-text)

Layer 2 (Surfaces):
  uffs-cli, uffs-tui, uffs-daemon, uffs-mcp → depend on uffs-core
```

**Dependency graph with `uffs-text`:**

```
uffs-polars ─────────────────┐
uffs-security ───────────────┤
uffs-text ───────────────────┼──→ uffs-mft ──→ uffs-core ──→ surfaces
                             │         │              ↑
                             │         └──────────────┘
                             │
                    (all Layer 0, no internal deps)
```

### What Lives Where

**`uffs-text` (new — lightweight, ~500 lines today):**
- `CaseFold` struct + all fold helpers
- Default `$UpCase` table binary (`upcase_default.bin`, 128 KB)
- Trigram pack/unpack helpers (`pack_char_trigram`, `unpack_char_trigram`)
- Dependencies: `bytemuck`, `smallvec` only — no Polars, no async

**`uffs-mft` (gains `uffs-text` dependency):**
- Windows `$UpCase` reader (`read_upcase_from_volume`) — stays here
  because it is platform-specific NTFS I/O
- Constructs `CaseFold` from live volume data or falls back to default

**`uffs-core` (gains `uffs-text` via `uffs-mft`, or direct dep):**
- All search/sort/filter/trigram refactoring (Phases 1–5)
- Imports `CaseFold` from `uffs-text`

### Future i18n Roadmap (what `uffs-text` will grow into)

| Capability | Crate | When |
|---|---|---|
| `$UpCase` case folding (NTFS-native) | `uffs-text` | **Now (this doc)** |
| Unicode normalisation (NFC/NFD/NFKC/NFKD) | `uffs-text` | i18n Phase 1 |
| Script detection (Latin, CJK, Cyrillic, Arabic…) | `uffs-text` | i18n Phase 1 |
| Locale-aware collation / sort keys | `uffs-text` | i18n Phase 2 |
| Multi-language search tokenisation | `uffs-text` | i18n Phase 2 |
| ICU bindings (optional, feature-gated) | `uffs-text` | i18n Phase 3 |
| Transliteration tables | `uffs-text` | i18n Phase 3 |

---

## 2. Architecture Overview

### Before (Pre-Implementation — Historical)

> **Note:** This section describes the state *before* Phases 0–2 were implemented.
> It is retained for context. The codebase now matches the "After" section.

```
names blob (140 MB, original case, UTF-8)
     │
     ├── clone() 140 MB → make_ascii_lowercase → TrigramIndex::build
     ├── per-record String::to_ascii_lowercase() in search match loop
     ├── per-record String::to_ascii_lowercase() in tree traversal
     ├── per-comparison to_ascii_lowercase() × 2 in sort
     └── per-record to_ascii_lowercase() in filter
```

**Problems:** 420 MB transient clones, millions of heap allocs/search,
ASCII-only folding misses Ü↔ü / É↔é / Д↔д.

### After (Current State — Phases 0–2 Implemented ✅)

```
$UpCase table (128 KB, read once, &'static [u16; 65536])
     │
     ▼
uffs_text::CaseFold { table: &'static [u16; 65536] }   ← Copy, threaded everywhere
     │
     ├── TrigramIndex::build(records, names, fold)            ✅ implemented
     │     └── char trigrams: u64 packed, FxHashMap, 0 clone
     ├── search: fold.fold_into(name, &mut buf) → reused buf  ✅ implemented
     ├── tree:   fold.fold_into(name, &mut buf) → reused buf  ✅ implemented
     ├── sort:   Schwartzian with fold.fold_into pre-computed  ✅ implemented
     └── filter: fold.fold_into(name, &mut buf) → reused buf  ✅ implemented
```

**Result:** 0 transient clones, 0 per-record heap allocs, full Unicode
case folding via NTFS's own table, 128 KB total overhead.

> **All phases complete.** Performance optimisations (BinaryHeap top-N,
> FxHashMap caches, LazyLock, stack alloc), cache v6 (persisted char-trigram
> CSR — zero-rebuild on load), and early MftIndex drop are all shipped.

---

## 3. Dependency Map

```
Phase 0: Foundation — NEW uffs-text crate
  ├── 0A: ✅ Create uffs-text crate scaffold + Cargo.toml
  ├── 0B: ✅ Add bytemuck, smallvec workspace deps; wire into workspace members
  ├── 0C: ✅ Create CaseFold struct + fold helpers (crates/uffs-text/src/case_fold.rs)
  ├── 0D: ✅ Compile-in default $UpCase table (crates/uffs-text/src/upcase_default.bin)
  ├── 0E: ✅ Add trigram pack/unpack helpers (crates/uffs-text/src/trigram_key.rs)
  ├── 0F: ✅ Live $UpCase reader + resolve_case_fold() with diff logging
  ├── 0G: ✅ Wire uffs-text into uffs-mft and uffs-core dependencies
  └── 0H: ✅ Add rustc-hash dep to uffs-core (for FxHashMap in Phases 1, 3)
          │
Phase 1: Trigram Migration (depends on 0C, 0D, 0E, 0H) — ✅ COMPLETE
  ├── 1A: ✅ Change TrigramIndex key to u64 packed char trigrams
  ├── 1B: ✅ Replace flat tri_lut (64 MB) with FxHashMap (~2 MB)
  ├── 1C: ✅ Rewrite build() pass 1 with char-level fold
  ├── 1D: ✅ Rewrite scatter pass 2 with char-level fold
  ├── 1E: ✅ Update TrigramIndex::search to emit char trigrams
  └── 1F: ✅ Update 3 callers (compact.rs, compact_cache.rs, compact_loader.rs)
          │            ← eliminates 140 MB × 3 clones (P6)
          │            ← fixes 64 MB LUT waste (P9)
          │
Phase 2: Fold Swap (depends on 0C) — ✅ COMPLETE
  ├── 2A: ✅ search/query.rs — match loop fold swap (P2)
  ├── 2B: ✅ search/query.rs — sort_indices_by_name zero-alloc (P7)
  ├── 2C: ✅ search/tree.rs — thread fold + buffer through tree search (P14)
  ├── 2D: ✅ search/backend.rs — numeric sort fast path (P4)
  ├── 2E: ✅ search/filters.rs — filter fold swap (P15→C15)
  └── 2F: ✅ index_search/pattern.rs — CaseFold pre-folded Vec<u16> matching
          │            ← eliminates millions of per-record allocs
          │
Phase 3: Performance Optimisations (independent of Phases 1-2) — ✅ COMPLETE (3C cancelled)
  ├── 3A: ✅ BinaryHeap for global top-N (P1) — query.rs
  ├── 3B: ✅ FxHashMap for DirCache (P3) — tree.rs
  ├── 3C: ❌ Arc<Vec<DisplayRow>> for results (P5) — CANCELLED (conflicts with in-place sort)
  ├── 3D: ✅ FxHashMap for path_cache (P8) — fast.rs
  ├── 3E: ✅ LazyLock for CACHE_PROFILE (P10) — query.rs
  └── 3F: ✅ Stack-allocated volume_prefix (P11) — query.rs
          │
Phase 4: Cache v6 Format (depends on 1A, 0D) — ✅ COMPLETE
  ├── 4A: ✅ Bump to v6 (COMPACT_VERSION = 6)
  ├── 4B: ✅ Serialize char-trigram CSR (keys u64[], offsets u32[], values u32[])
  └── 4C: ✅ Backward-compatible v5 load (trigrams rebuilt with CaseFold)
          │
Phase 5: Caller-Level Memory (independent) — ✅ COMPLETE
  ├── 5A: ✅ Drop MftIndex early after compact build (compact_loader.rs:124)
  └── 5B: ✅ Daemon uses same load_drive() path — MftIndex freed via 5A
```

### Critical Path

```
0A → 0B → 0C → 0D → 0E → 0G → 1A → 1B → 1C → 1D → 1E → 1F → 4A → 4B
                                                                    ↑
0C → 2A → 2B → 2C → 2D → 2E ─────────────────────────────────────┘
```

Phase 0 (crate creation) is the new gating step.
Phases 2 and 3 can proceed in parallel after Phase 0.
Phase 4 must wait for Phase 1 (new trigram key format).
Phase 5 is fully independent.


---

<a id="phase-0"></a>

## Phase 0 — Foundation: `uffs-text` Crate + CaseFold ✅ COMPLETE

### 0A: ✅ Create `uffs-text` Crate Scaffold

```bash
mkdir -p crates/uffs-text/src
```

**File:** `crates/uffs-text/Cargo.toml`

```toml
# ============================================================================
# uffs-text: Unicode Text Processing for UFFS
# ============================================================================
# Layer 0 Foundation crate. No internal crate dependencies.
#
# Provides NTFS-compatible case folding via the $UpCase table, trigram key
# helpers, and (future) Unicode normalisation, collation, and i18n support.
# ============================================================================

[package]
name = "uffs-text"
description = "Unicode text processing for UFFS: NTFS case folding, trigram keys, i18n foundation"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
authors.workspace = true
readme.workspace = true
keywords.workspace = true
categories.workspace = true

[dependencies]
bytemuck = { version = "1.25.0", features = ["derive"] }
smallvec = "1.15.1"

[lints]
workspace = true
```

**File:** `crates/uffs-text/src/lib.rs`

```rust
//! # uffs-text: Unicode Text Processing for UFFS
//!
//! Layer 0 foundation crate providing NTFS-compatible case folding
//! and text processing primitives. No internal crate dependencies.
//!
//! ## Current Capabilities
//!
//! - **`CaseFold`**: NTFS `$UpCase` case folding engine (128 KB table,
//!   `Copy`, zero-alloc comparisons, buffer-reuse folding)
//! - **Trigram key helpers**: Pack/unpack `[u16; 3]` char trigrams
//!
//! ## Future (i18n)
//!
//! - Unicode normalisation (NFC/NFD)
//! - Script detection
//! - Locale-aware collation
//! - Search tokenisation

#![forbid(unsafe_code)]
#![warn(clippy::all, clippy::pedantic)]

mod case_fold;
mod trigram_key;

pub use case_fold::CaseFold;
pub use trigram_key::{pack_char_trigram, unpack_char_trigram};
```

### 0B: ✅ Wire Into Workspace

**File:** root `Cargo.toml` — add to `[workspace]` members and
`[workspace.dependencies]`:

```toml
# In [workspace] members, after uffs-security:
"crates/uffs-text",      # 📝 Unicode text processing, i18n foundation

# In [workspace.dependencies]:
uffs-text = { path = "crates/uffs-text" }
```

`bytemuck` and `smallvec` are already workspace dependencies —
`uffs-text` uses them directly (version-pinned, not workspace refs,
to keep the crate self-contained as a foundation).

### 0C: ✅ Create `CaseFold` Struct

**New file:** `crates/uffs-text/src/case_fold.rs`

This is the central abstraction that replaces every `to_ascii_lowercase`
call in the hot path. All search/sort/trigram functions receive a
`CaseFold` by value (it's `Copy` — just a pointer).

```rust
//! NTFS-compatible case folding via the $UpCase table.
//!
//! The `$UpCase` table is a 128 KB flat array mapping every BMP Unicode
//! codepoint (0x0000–0xFFFF) to its uppercase equivalent. NTFS uses this
//! table for ALL case-insensitive operations.
//!
//! For case-insensitive comparison, we fold both sides to uppercase
//! (matching NTFS semantics) and compare the folded values.

/// Default $UpCase table compiled into the binary.
/// Generated from a Windows 11 23H2 NTFS-formatted volume.
/// Covers Unicode 14.0+ case mappings.
static DEFAULT_UPCASE: &[u8; 131072] = include_bytes!("upcase_default.bin");

/// NTFS-compatible case-folding engine.
///
/// Wraps a reference to a `$UpCase` table (128 KB, 65536 × u16).
/// `Copy` and cheap to pass by value — it's just a pointer.
#[derive(Clone, Copy)]
pub struct CaseFold {
    /// Pointer to the 65536-entry u16 table.
    /// Each entry maps a BMP codepoint to its uppercase equivalent.
    table: &'static [u16],
}

impl CaseFold {
    /// Create from the compiled-in default $UpCase table.
    #[must_use]
    pub fn default_table() -> Self {
        // Safety: DEFAULT_UPCASE is 131072 bytes = 65536 × u16 (LE).
        // The binary is generated from a known-good NTFS volume.
        let table: &[u16] = bytemuck::cast_slice(DEFAULT_UPCASE);
        Self { table }
    }

    /// Create from a live $UpCase table read from a volume.
    ///
    /// The caller must ensure the table is exactly 65536 entries and
    /// has `'static` lifetime (e.g., via `Box::leak` or a static allocation).
    #[must_use]
    pub fn from_ntfs(table: &'static [u16]) -> Self {
        debug_assert!(table.len() >= 65536, "$UpCase table too short");
        Self { table }
    }

    // ── Per-codepoint fold ────────────────────────────────────────

    /// Fold a single Unicode codepoint to its NTFS uppercase equivalent.
    ///
    /// For BMP codepoints (< 0x10000): O(1) table lookup.
    /// For non-BMP (emoji, rare CJK): identity (no case).
    #[inline]
    pub fn fold_char(&self, ch: char) -> u16 {
        let cp = ch as u32;
        if cp < 0x10000 {
            self.table[cp as usize]
        } else {
            cp as u16
        }
    }

    /// Fold a single ASCII byte. Fast path — no BMP check needed.
    #[inline]
    pub fn fold_ascii(&self, b: u8) -> u8 {
        debug_assert!(b < 0x80, "fold_ascii called with non-ASCII byte");
        self.table[b as usize] as u8
    }

    // ── String comparison helpers ─────────────────────────────────

    /// Case-insensitive comparison of two UTF-8 strings.
    /// Zero allocations — folds lazily per codepoint.
    #[inline]
    pub fn cmp_str(&self, a: &str, b: &str) -> core::cmp::Ordering {
        let mut a_chars = a.chars();
        let mut b_chars = b.chars();
        loop {
            match (a_chars.next(), b_chars.next()) {
                (None, None) => return core::cmp::Ordering::Equal,
                (None, Some(_)) => return core::cmp::Ordering::Less,
                (Some(_), None) => return core::cmp::Ordering::Greater,
                (Some(ca), Some(cb)) => {
                    let fa = self.fold_char(ca);
                    let fb = self.fold_char(cb);
                    match fa.cmp(&fb) {
                        core::cmp::Ordering::Equal => continue,
                        other => return other,
                    }
                }
            }
        }
    }

    /// Case-insensitive equality of two UTF-8 strings.
    #[inline]
    pub fn eq_str(&self, a: &str, b: &str) -> bool {
        self.cmp_str(a, b) == core::cmp::Ordering::Equal
    }

    /// Case-insensitive substring check: does `haystack` contain `needle`?
    /// Both are compared with $UpCase folding.
    pub fn contains_folded(&self, haystack: &str, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }
        let h_len = haystack.chars().count();
        let n_len = needle.chars().count();
        if n_len > h_len {
            return false;
        }
        let n_folded: smallvec::SmallVec<[u16; 64]> = needle
            .chars()
            .map(|ch| self.fold_char(ch))
            .collect();
        let h_folded: smallvec::SmallVec<[u16; 256]> = haystack
            .chars()
            .map(|ch| self.fold_char(ch))
            .collect();
        h_folded.windows(n_folded.len()).any(|w| w == n_folded.as_slice())
    }

    // ── Buffer-reuse fold (Tier 2) ────────────────────────────────

    /// Fold a UTF-8 name into a reusable buffer as uppercase UTF-8.
    ///
    /// The buffer is cleared and reused — zero heap allocation after
    /// the first call (buffer capacity persists across calls).
    ///
    /// Returns the folded bytes as a `&str` slice into the buffer.
    pub fn fold_into<'a>(&self, name: &str, buf: &'a mut Vec<u8>) -> &'a str {
        buf.clear();
        let mut encode_buf = [0u8; 4];
        for ch in name.chars() {
            let cp = ch as u32;
            if cp < 0x80 {
                buf.push(self.table[cp as usize] as u8);
            } else if cp < 0x10000 {
                let folded_cp = self.table[cp as usize] as u32;
                if let Some(folded_ch) = char::from_u32(folded_cp) {
                    buf.extend_from_slice(
                        folded_ch.encode_utf8(&mut encode_buf).as_bytes()
                    );
                }
            } else {
                buf.extend_from_slice(
                    ch.encode_utf8(&mut encode_buf).as_bytes()
                );
            }
        }
        // Safety: we encoded valid UTF-8 chars above
        core::str::from_utf8(buf.as_slice()).unwrap_or("")
    }
}
```



### 0D: ✅ Compile-In Default `$UpCase` Table

**File:** `crates/uffs-text/src/upcase_default.bin` (128 KB binary)

Generate this file by reading `$UpCase` from a Windows 11 NTFS volume:

```powershell
# Run on Windows with Admin privileges:
$stream = [System.IO.File]::OpenRead("C:\`$UpCase")
$buf = New-Object byte[] 131072
$stream.Read($buf, 0, 131072) | Out-Null
$stream.Close()
[System.IO.File]::WriteAllBytes("upcase_default.bin", $buf)
```

**Verification:** The file must be exactly 131,072 bytes. Validate:

```rust
#[test]
fn upcase_table_is_valid() {
    let table: &[u16] = bytemuck::cast_slice(DEFAULT_UPCASE);
    assert_eq!(table.len(), 65536);
    // ASCII invariants:
    assert_eq!(table[b'a' as usize], b'A' as u16);  // a → A
    assert_eq!(table[b'z' as usize], b'Z' as u16);  // z → Z
    assert_eq!(table[b'A' as usize], b'A' as u16);  // A → A (identity)
    assert_eq!(table[b'0' as usize], b'0' as u16);  // 0 → 0 (identity)
    // European invariants:
    assert_eq!(table[0x00FC], 0x00DC);  // ü → Ü
    assert_eq!(table[0x00E9], 0x00C9);  // é → É
    assert_eq!(table[0x00F6], 0x00D6);  // ö → Ö
    // CJK identity:
    assert_eq!(table[0x4E2D], 0x4E2D);  // 中 → 中 (no case)
}
```

### 0E: ✅ Trigram Pack/Unpack Helpers

**New file:** `crates/uffs-text/src/trigram_key.rs`

These helpers are used by `uffs-core`'s `TrigramIndex` but defined in
`uffs-text` because they operate on folded `u16` codepoints — a text
concern, not a search-engine concern.

```rust
//! Trigram key helpers for character-level trigram indices.
//!
//! Provides pack/unpack between `[u16; 3]` (3 folded codepoints) and
//! `u64` (hash-map key). Used by `uffs-core::trigram::TrigramIndex`.

/// Pack 3 folded u16 codepoints into a u64.
#[inline]
pub const fn pack_char_trigram(a: u16, b: u16, c: u16) -> u64 {
    (a as u64) << 32 | (b as u64) << 16 | (c as u64)
}

/// Unpack a u64 to 3 folded codepoints.
#[inline]
pub const fn unpack_char_trigram(packed: u64) -> [u16; 3] {
    [(packed >> 32) as u16, (packed >> 16) as u16, packed as u16]
}
```

### 0F: ✅ Windows API `$UpCase` Reader

> **Implementation note (2026-04-02):** This step has NOT been implemented.
> No `upcase.rs` file exists in `crates/uffs-mft/src/platform/`. All code
> currently uses `CaseFold::default_table()` (compiled-in 128 KB binary).
> The live-volume reader is only needed for volumes with non-standard
> `$UpCase` tables, which is extremely rare in practice.

**File:** `crates/uffs-mft/src/platform/upcase.rs`

This stays in `uffs-mft` because it is Windows-specific NTFS I/O. It
returns a `Box<[u16; 65536]>` that callers leak into `&'static [u16]`
for `CaseFold::from_ntfs()`.

```rust
//! Read the live $UpCase table from an NTFS volume.

#[cfg(windows)]
use windows::Win32::{
    Foundation::CloseHandle,
    Storage::FileSystem::{
        CreateFileW, ReadFile, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_READ_DATA, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FILE_SHARE_DELETE, OPEN_EXISTING,
    },
};

/// Read the $UpCase table from a live NTFS volume.
///
/// Returns a boxed 65536-entry u16 array (128 KB).
/// Falls back to `None` if the file can't be opened (non-NTFS,
/// insufficient privileges, etc.).
#[cfg(windows)]
pub fn read_upcase_from_volume(drive_letter: char) -> Option<Box<[u16; 65536]>> {
    let path = format!("{}:\\$UpCase\0", drive_letter.to_ascii_uppercase());
    let wide: Vec<u16> = path.encode_utf16().collect();

    unsafe {
        let handle = CreateFileW(
            windows::core::PCWSTR(wide.as_ptr()),
            FILE_READ_DATA.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        ).ok()?;

        let mut buf = vec![0u8; 131072];
        let mut bytes_read = 0u32;
        let ok = ReadFile(handle, Some(&mut buf), Some(&mut bytes_read), None);
        let _ = CloseHandle(handle);

        if ok.is_err() || bytes_read as usize != 131072 {
            return None;
        }

        let mut table = Box::new([0u16; 65536]);
        for (i, chunk) in buf.chunks_exact(2).enumerate() {
            table[i] = u16::from_le_bytes([chunk[0], chunk[1]]);
        }
        Some(table)
    }
}

/// Non-Windows stub — always returns None.
#[cfg(not(windows))]
pub fn read_upcase_from_volume(_drive_letter: char) -> Option<Box<[u16; 65536]>> {
    None
}
```

### 0G: ✅ Wire `uffs-text` Into Downstream Crates

**File:** `crates/uffs-mft/Cargo.toml` — add dependency:

```toml
uffs-text.workspace = true
```

**File:** `crates/uffs-mft/src/platform.rs` — add module:

```rust
pub mod upcase;
```

**File:** `crates/uffs-core/Cargo.toml` — add dependency:

```toml
uffs-text.workspace = true
```

**File:** `crates/uffs-core/src/compact.rs` — add `CaseFold` to
`DriveCompactIndex`:

```rust
use uffs_text::CaseFold;

pub struct DriveCompactIndex {
    pub letter: char,
    pub records: Vec<CompactRecord>,
    pub names: Vec<u8>,
    pub children: ChildrenIndex,
    pub trigram: TrigramIndex,
    pub fold: CaseFold,  // ← NEW: case folding engine for this volume
}
```

All search/sort functions that receive `&DriveCompactIndex` now have
access to `drive.fold` — no parameter threading needed for most callers.

### 0H: ✅ Add `rustc-hash` Dependency to `uffs-core`

**Crate:** `uffs-core`

```bash
cargo add rustc-hash -p uffs-core
```

Used for `FxHashMap` (P3, P8) and the trigram LUT replacement (P9).
The crate is ~100 lines, zero dependencies, maintained by the Rust
compiler team. Note: `uffs-mft` already has this dependency.

---

<a id="phase-1"></a>

## Phase 1 — Trigram Migration: Byte → Character ✅ COMPLETE

**This is the most complex phase.** It changes the fundamental unit of
the trigram index from 3 raw bytes to 3 folded Unicode codepoints.

### Why Character Trigrams?

Byte trigrams cannot achieve correct case folding for multi-byte UTF-8
characters:

```
"über" (UTF-8): C3 BC 62 65 72   →  byte trigrams: [C3,BC,62] ...
"ÜBER" (UTF-8): C3 9C 42 45 52   →  byte trigrams: [C3,9C,42] ...

With byte-level fold: C3 and 9C are non-ASCII — to_ascii_lowercase
doesn't change them. The trigrams DON'T MATCH.

With char trigrams + $UpCase fold:
"über" → fold → [Ü,B,E,R]  →  char trigrams: (Ü,B,E), (B,E,R)
"ÜBER" → fold → [Ü,B,E,R]  →  char trigrams: (Ü,B,E), (B,E,R)  ← MATCH ✅
```

### 1A: ✅ Change Key Type

**File:** `crates/uffs-core/src/trigram.rs`

```rust
// BEFORE:
pub struct TrigramIndex {
    keys: Vec<[u8; 3]>,      // 3-byte trigrams
    offsets: Vec<u32>,
    values: Vec<u32>,
}

// AFTER:
pub struct TrigramIndex {
    keys: Vec<[u16; 3]>,     // 3-codepoint trigrams (folded via $UpCase)
    offsets: Vec<u32>,
    values: Vec<u32>,
}
```

**Impact on key space:**

| | Byte Trigrams | Char Trigrams |
|---|---|---|
| Key type | `[u8; 3]` → packed `u32` | `[u16; 3]` → packed `u64` |
| Key space | 2²⁴ = 16M | 2⁴⁸ = 281T (but sparse) |
| Unique keys (7M records) | ~50K | ~50K (same — most filenames are ASCII) |
| Key storage | 50K × 3 B = 150 KB | 50K × 6 B = 300 KB |

### 1B: ✅ Replace Flat LUT with `FxHashMap`

The current 64 MB flat LUT (`vec![u32::MAX; 16_777_216]`) is indexed by
packed `u32` byte trigram. With `u64` char trigrams, a flat LUT would be
impossibly large. Use `FxHashMap` instead:

```rust
// BEFORE (trigram.rs:165):
let mut tri_lut = vec![u32::MAX; TRIGRAM_LUT_SIZE]; // 64 MB!

// AFTER:
use rustc_hash::FxHashMap;
let mut tri_lut: FxHashMap<u64, u32> = FxHashMap::default();
tri_lut.reserve(global_counts.len()); // ~50K entries × 16 B ≈ 1.6 MB
```

**Memory improvement:** 64 MB → 1.6 MB. This ALSO fixes P9 from doc 15.

### 1C–1D: ✅ Rewrite Build Passes With Char-Level Fold

**File:** `crates/uffs-core/src/trigram.rs`

New signature — note `CaseFold` is imported from `uffs-text`:

```rust
use uffs_text::CaseFold;

impl TrigramIndex {
    /// Build a trigram index from compact records using $UpCase folding.
    ///
    /// Character-level trigrams: 3 Unicode codepoints, each folded via
    /// the CaseFold table, packed into a u64.
    #[must_use]
    pub fn build(
        records: &[CompactRecord],
        names: &[u8],        // ← original case (NOT pre-lowered)
        fold: CaseFold,       // ← $UpCase fold engine from uffs-text
    ) -> Self {
```

**Pass 1 — count (char-level):**

```rust
use uffs_text::pack_char_trigram;

// In the per-chunk parallel map:
for rec in chunk {
    let start = rec.name_offset as usize;
    let end = start + rec.name_len as usize;
    let name_bytes = match names.get(start..end) {
        Some(slice) => slice,
        None => continue,
    };
    let name_str = core::str::from_utf8(name_bytes).unwrap_or("");
    let folded: SmallVec<[u16; 64]> = name_str
        .chars()
        .map(|ch| fold.fold_char(ch))
        .collect();

    if folded.len() < 3 {
        continue;
    }
    seen.clear();
    for window in folded.windows(3) {
        let packed = pack_char_trigram(window[0], window[1], window[2]);
        if seen.insert(packed) {
            *local.entry(packed).or_insert(0) += 1;
        }
    }
}
```

**Pass 2 — scatter (char-level):**

```rust
use uffs_text::{CaseFold, pack_char_trigram};

fn scatter_one_record(
    name_bytes: &[u8],
    rec_idx: u32,
    tri_lut: &FxHashMap<u64, u32>,    // ← was flat &[u32]
    write_pos: &mut FxHashMap<u32, u32>,
    values: &[AtomicU32],
    seen: &mut TinyTriSet,
    fold: CaseFold,                    // ← from uffs-text
) {
    let name_str = core::str::from_utf8(name_bytes).unwrap_or("");
    let folded: SmallVec<[u16; 64]> = name_str
        .chars()
        .map(|ch| fold.fold_char(ch))
        .collect();
    if folded.len() < 3 { return; }

    for window in folded.windows(3) {
        let packed = pack_char_trigram(window[0], window[1], window[2]);
        if !seen.insert(packed) { continue; }
        let key_idx = match tri_lut.get(&packed) {
            Some(&ki) => ki,
            None => continue,
        };
        // ... rest same as current (write rec_idx into values)
    }
}
```

**TinyTriSet change:** Key type from `u32` to `u64`:

```rust
struct TinyTriSet {
    seen: Vec<u64>,  // was Vec<u32>
}
```


### 1E: ✅ Update `TrigramIndex::search`

**File:** `crates/uffs-core/src/trigram.rs`

The search function must generate char trigrams from the query using
the same fold:

```rust
use uffs_text::{CaseFold, pack_char_trigram, unpack_char_trigram};

impl TrigramIndex {
    /// Search for records whose names contain the query (case-insensitive).
    ///
    /// Generates char trigrams from the query using $UpCase fold,
    /// intersects the posting lists (smallest-first), and returns
    /// matching record indices.
    pub fn search(
        &self,
        query: &str,
        fold: CaseFold,   // ← from uffs-text
    ) -> Option<Vec<u32>> {
        let folded: SmallVec<[u16; 64]> = query
            .chars()
            .map(|ch| fold.fold_char(ch))
            .collect();
        if folded.len() < 3 {
            return None;
        }

        let mut trigrams: SmallVec<[u64; 32]> = SmallVec::new();
        let mut seen = FxHashSet::default();
        for window in folded.windows(3) {
            let packed = pack_char_trigram(window[0], window[1], window[2]);
            if seen.insert(packed) {
                trigrams.push(packed);
            }
        }
        if trigrams.is_empty() {
            return None;
        }

        let mut postings: SmallVec<[&[u32]; 16]> = SmallVec::new();
        for &tri in &trigrams {
            let key_arr = unpack_char_trigram(tri);
            match self.keys.binary_search(&key_arr) {
                Ok(ki) => {
                    let start = self.offsets[ki] as usize;
                    let end = self.offsets[ki + 1] as usize;
                    postings.push(&self.values[start..end]);
                }
                Err(_) => return Some(Vec::new()),
            }
        }
        postings.sort_unstable_by_key(|p| p.len());

        let mut result = postings[0].to_vec();
        for posting in &postings[1..] {
            intersect_sorted_inplace(&mut result, posting);
            if result.is_empty() { break; }
        }
        Some(result)
    }
}
```

### 1F: ✅ Update 3 Callers — Eliminate 140 MB Clones

All callers now pass `fold` (from `uffs_text::CaseFold`) instead of
cloning + lowering the names blob.

**File:** `crates/uffs-core/src/compact.rs` (line ~433)

```rust
// BEFORE:
let trigram = {
    let mut names_lower = names.clone();       // 140 MB CLONE
    names_lower.make_ascii_lowercase();         // in-place mutate
    TrigramIndex::build(&records, &names_lower) // consume lowered copy
};  // names_lower dropped here — 140 MB freed

// AFTER:
let trigram = TrigramIndex::build(&records, &names, fold);
// No clone. No lowering. Fold happens inline per codepoint.
```

**File:** `crates/uffs-core/src/compact_cache.rs` (line ~159)

```rust
// BEFORE:
let mut names_lower = names.clone();
names_lower.make_ascii_lowercase();
let trigram = TrigramIndex::build(&records, &names_lower);

// AFTER:
let trigram = TrigramIndex::build(&records, &names, fold);
```

**File:** `crates/uffs-core/src/compact_loader.rs` (line ~444)

```rust
// BEFORE:
let mut names_lower = self.names.clone();
names_lower.make_ascii_lowercase();
self.trigram = TrigramIndex::build(&self.records, &names_lower);

// AFTER:
self.trigram = TrigramIndex::build(&self.records, &self.names, self.fold);
```

**Impact:** Eliminates 140 MB × 3 call sites = **420 MB** of transient
allocations. This single change addresses P6 from doc 15.

### Phase 1 Performance Budget

| Operation | Before | After | Delta |
|-----------|--------|-------|-------|
| Trigram build (7M records) | 200 ms | 221 ms | +21 ms |
| Trigram build memory | +204 MB (140 MB clone + 64 MB LUT) | +1.6 MB (FxHashMap) | **-202 MB** |
| Trigram search | ~1 ms | ~1.2 ms | +0.2 ms |
| Trigram key storage | 150 KB | 300 KB | +150 KB |

---

<a id="phase-2"></a>

## Phase 2 — Search/Filter/Sort Fold Swap ✅ COMPLETE

All changes in this phase follow the same pattern: replace
`name.to_ascii_lowercase()` (heap alloc) with either:
- `drive.fold.cmp_str(a, b)` — for comparisons (Tier 1, zero alloc)
- `drive.fold.fold_into(name, &mut buf)` — for substring search (Tier 2, reused buffer)

`CaseFold` is accessed via `drive.fold` (added in 0G) — all types are
from `uffs_text`, no local definitions needed in `uffs-core`.

### 2A: ✅ Search Match Loop — `query.rs` (Fixes P2)

**File:** `crates/uffs-core/src/search/query.rs`, lines ~395–450

```rust
// AFTER — all paths use fold + buffer:
let fold = drive.fold;  // uffs_text::CaseFold, Copy
let mut fold_buf: Vec<u8> = Vec::with_capacity(256);

let matches = |name: &str, buf: &mut Vec<u8>| -> bool {
    if name.is_empty() || name == "." { return false; }

    if case_sensitive {
        if whole_word {
            if is_glob || is_or { tree::name_matches(name, needle) }
            else { name == needle }
        } else if let Some(fnd) = &finder {
            fnd.find(name.as_bytes()).is_some()
        } else {
            tree::name_matches(name, needle)
        }
    } else {
        let folded = fold.fold_into(name, buf);
        if whole_word {
            if is_glob || is_or { tree::name_matches(folded, needle) }
            else { fold.eq_str(folded, needle) }
        } else if let Some(fnd) = &finder {
            fnd.find(folded.as_bytes()).is_some()
        } else {
            tree::name_matches(folded, needle)
        }
    }
};
```

**Note:** `needle` must ALSO be pre-folded at query compilation time:

```rust
let needle = if case_sensitive {
    query.to_string()
} else {
    let mut buf = Vec::with_capacity(query.len());
    fold.fold_into(query, &mut buf).to_string()
};
```

**Impact:** Eliminates C1, C2 (50K–7M allocs per search → 0).

### 2B: ✅ `sort_indices_by_name` — `query.rs` (Fixes P7)

**File:** `crates/uffs-core/src/search/query.rs`, lines ~120–135

```rust
// AFTER — zero-alloc lazy codepoint fold:
fn sort_indices_by_name(indices: &mut [u32], drive: &DriveCompactIndex, desc: bool) {
    let fold = drive.fold;  // uffs_text::CaseFold
    indices.sort_unstable_by(|&idx_a, &idx_b| {
        let name_a = drive.records.get(idx_a as usize)
            .map_or("", |rec| rec.name(&drive.names));
        let name_b = drive.records.get(idx_b as usize)
            .map_or("", |rec| rec.name(&drive.names));
        let ord = fold.cmp_str(name_a, name_b);  // 0 allocs, lazy fold
        if desc { ord.reverse() } else { ord }
    });
}
```

**Impact:** Eliminates C3, C4. From 2 × N × log₂N allocs → 0.

### 2C: ✅ Tree Search — `tree.rs` (Fixes P14)

**File:** `crates/uffs-core/src/search/tree.rs`, lines ~185–330

Thread `fold: CaseFold` and `buf: &mut Vec<u8>` through `tree_search`:

```rust
// BEFORE:
let lower = rec.name(&drive.names).to_ascii_lowercase();  // HEAP ALLOC

// AFTER:
let folded = drive.fold.fold_into(rec.name(&drive.names), buf);
```

**Signature change:**

```rust
// AFTER:
pub fn tree_search(
    segments: &[&str],
    drive: &DriveCompactIndex,
    limit: usize,
    buf: &mut Vec<u8>,  // ← reusable fold buffer
    ...
) -> Vec<u32>
```

The caller (`search_compact_drive` in `query.rs`) passes its existing
`fold_buf` through.

**Impact:** Eliminates C7–C11 (thousands of allocs per tree search → 0).

### 2D: ✅ Numeric Sort Fast Path — `backend.rs` (Fixes P4)

**File:** `crates/uffs-core/src/search/backend.rs`, lines ~508–557

```rust
// AFTER — branch on sort type:
pub fn sort_rows(rows: &mut [DisplayRow], column: SortColumn, ..., fold: CaseFold) {
    if rows.len() <= 1 { return; }

    let needs_string_keys = matches!(
        column, SortColumn::Name | SortColumn::Path | SortColumn::Extension
    ) || extra_tiers.iter().any(|t| matches!(
        t.column, SortColumn::Name | SortColumn::Path | SortColumn::Extension
    ));

    if needs_string_keys {
        sort_rows_with_string_keys(rows, column, descending, extra_tiers, fold);
    } else {
        rows.sort_unstable_by(|a, b| {
            let mut ord = compare_numeric(a, b, column);
            if descending { ord = ord.reverse(); }
            for tier in extra_tiers {
                if ord != Ordering::Equal { break; }
                ord = compare_numeric(a, b, tier.column);
                if tier.descending { ord = ord.reverse(); }
            }
            if ord == Ordering::Equal {
                ord = fold.cmp_str(a.name(), b.name());
            }
            ord
        });
    }
}
```

Where `fold` is obtained from `SearchBackend` which stores it from the
loaded `DriveCompactIndex`.

**Impact:** For numeric sorts (90% case): 30K allocs → 0.

### 2E: ✅ Filter Fold Swap — `filters.rs` (Fixes C15)

**File:** `crates/uffs-core/src/search/filters.rs`, line ~323

```rust
// BEFORE:
let lower = row.name.to_ascii_lowercase();  // HEAP ALLOC

// AFTER:
let folded = fold.fold_into(&row.name, buf);
```

Thread `fold: CaseFold` and `buf: &mut Vec<u8>` into the filter
matching function.

**Impact:** Eliminates C15 (10K allocs per filter pass → 0).

### Phase 2 Performance Budget

| Operation | Before (allocs) | After (allocs) | Delta |
|-----------|-----------------|----------------|-------|
| Search match (50K candidates) | 50K–7M | 0 | **-50K to -7M** |
| Sort by name (10K rows) | 2 × N × log₂N (~280K) | 0 | **-280K** |
| Sort by size (10K rows, numeric) | 30K | 0 | **-30K** |
| Tree search (5K children) | 5K | 0 | **-5K** |
| Filter pass (10K rows) | 10K | 0 | **-10K** |
| **Per-search latency increase** | | | **+1.4 ms** (table lookup) |

---

<a id="phase-3"></a>

## Phase 3 — Performance Optimisations (P1, P3, P5, P8, P10, P11) ✅ COMPLETE (3C cancelled)

These are independent of Phases 1–2.

### 3A: ✅ BinaryHeap for Global Top-N (Fixes P1)

**File:** `crates/uffs-core/src/search/query.rs`, lines ~155–210

Currently, `search_compact_drive` collects ALL matching indices into a
Vec, sorts the entire Vec, then truncates to `limit`. For match-all
queries on 7M records, this sorts 7M entries to return 10K.

```rust
// AFTER — BinaryHeap capped at K:
use std::collections::BinaryHeap;
use core::cmp::Reverse;

struct ScoredIndex {
    sort_key: u64,
    idx: u32,
}
impl Ord for ScoredIndex { /* compare by sort_key */ }

let mut heap: BinaryHeap<Reverse<ScoredIndex>> = BinaryHeap::with_capacity(limit + 1);

for idx in 0..records.len() as u32 {
    let key = extract_sort_key(&records[idx as usize], sort_column);
    if heap.len() < limit {
        heap.push(Reverse(ScoredIndex { sort_key: key, idx }));
    } else if let Some(min) = heap.peek() {
        if key > min.0.sort_key {
            heap.pop();
            heap.push(Reverse(ScoredIndex { sort_key: key, idx }));
        }
    }
}

let mut result: Vec<u32> = heap.into_iter().map(|r| r.0.idx).collect();
result.sort_unstable_by(/* final sort for display */);
```

**Impact:**
- Memory: 28 MB (Vec of 7M u32) → 120 KB (heap of 10K entries)
- Time: O(N log N) → O(N log K) where K = limit (typically 10K)
- For N=7M, K=10K: log₂(7M) ≈ 23, log₂(10K) ≈ 13 → **~44% faster**

### 3B: ✅ FxHashMap for DirCache (Fixes P3)

**File:** `crates/uffs-core/src/search/tree.rs`, line ~18

```rust
// BEFORE:
use std::collections::HashMap;
struct DirCache {
    cache: HashMap<u32, String>,
}

// AFTER:
use rustc_hash::FxHashMap;
struct DirCache {
    cache: FxHashMap<u32, String>,
}
```

**Impact:** 3–5× faster path resolution for tree search.

### 3C: ❌ `Arc<Vec<DisplayRow>>` for Results — CANCELLED

> **Cancellation note (2026-04-03):** This change was cancelled because
> `sort_rows()` mutates `last_results` in-place, which conflicts with Arc
> sharing (`Arc::make_mut()` would clone if refcount > 1). The existing
> `clone_from` is already efficient. Low ROI for the invasiveness.

**File:** `crates/uffs-core/src/search/backend.rs`, line ~425

```rust
// AFTER:
pub struct SearchResult {
    pub rows: Arc<Vec<DisplayRow>>,
    pub duration: Duration,
    pub records_scanned: usize,
}

let rows_arc = Arc::new(rows);
self.last_results = Arc::clone(&rows_arc);
SearchResult {
    rows: rows_arc,
    ..
}
```

**Downstream impact:** All consumers of `SearchResult::rows` change
from `Vec<DisplayRow>` to `Arc<Vec<DisplayRow>>`. Update:
- `uffs-tui/src/app.rs` — table rendering reads `&result.rows[..]`
- `uffs-cli/commands/search.rs` — output formatting reads `&result.rows`
- `uffs-core/src/search/backend.rs` — `last_results` type changes

**Impact:** Eliminates ~2 MB deep clone per search.

### 3D: ✅ FxHashMap for `path_cache` (Fixes P8)

**File:** `crates/uffs-core/src/path_resolver/fast.rs`, line ~55

```rust
// BEFORE:
path_cache: Vec<Option<String>>,  // 168 MB for 7M sparse FRS slots

// AFTER:
path_cache: FxHashMap<u64, String>,  // ~1 MB for ~5K cached paths
```

**Impact:** 168 MB → ~1 MB for the legacy path cache.

### 3E: ✅ LazyLock for `CACHE_PROFILE` (Fixes P10)

**File:** `crates/uffs-core/src/search/query.rs`, line ~63

```rust
// AFTER:
use std::sync::LazyLock;
static CACHE_PROFILE: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("UFFS_CACHE_PROFILE").is_ok()
});
let cache_profile = *CACHE_PROFILE;
```

**Impact:** Eliminates 1 syscall per search. Trivial change.

### 3F: ✅ Stack-Allocated `volume_prefix` (Fixes P11)

**File:** `crates/uffs-core/src/search/query.rs`, line ~85

```rust
// AFTER:
let mut prefix_buf = [0u8; 4];
prefix_buf[0] = drive.letter.to_ascii_uppercase() as u8;
prefix_buf[1] = b':';
prefix_buf[2] = b'\\';
let volume_prefix = core::str::from_utf8(&prefix_buf[..3]).unwrap_or("?:\\");
```

**Impact:** Eliminates 1 heap alloc per search. Trivial change.

---

<a id="phase-4"></a>

## Phase 4 — Cache v6 Format ✅ COMPLETE

### 4A: ✅ Bump Cache to v6

**File:** `crates/uffs-core/src/compact_cache.rs`

Bump cache format version from 5 to 6. Add an upcase section:

```rust
const CACHE_FORMAT_VERSION: u32 = 6;

pub struct CacheHeader {
    // ... existing fields ...
    pub upcase_table_present: bool,  // NEW: indicates $UpCase section
}
```

### 4B: ✅ Serialize Character-Trigram CSR

The trigram CSR format changes because keys are now `[u16; 3]` instead
of `[u8; 3]`. The values (`Vec<u32>`) and offsets (`Vec<u32>`) are
unchanged.

```rust
// Serialize keys:
let key_bytes: &[u8] = bytemuck::cast_slice(&trigram.keys);
writer.write_all(key_bytes)?;  // 50K × 6 bytes = 300 KB

// Serialize $UpCase table:
let upcase_bytes: &[u8] = bytemuck::cast_slice(fold.table);
writer.write_all(upcase_bytes)?;  // 128 KB
```

### 4C: ✅ Backward-Compatible v5 Load

When loading a v5 cache:
1. No `$UpCase` section → use `CaseFold::default_table()` (from `uffs-text`)
2. Byte-trigram CSR → **discard and rebuild** with char trigrams
   using the default $UpCase table. This is a one-time cost (~220 ms)
   on the first load after upgrade.

```rust
use uffs_text::CaseFold;

fn load_cache(path: &Path) -> Result<DriveCompactIndex> {
    let header = read_header(&data)?;
    let fold = if header.version >= 6 && header.upcase_table_present {
        let upcase_slice = read_upcase_section(&data, &header)?;
        CaseFold::from_ntfs(Box::leak(upcase_slice.into_boxed_slice()))
    } else {
        CaseFold::default_table()
    };

    let trigram = if header.version >= 6 {
        deserialize_char_trigram_csr(&data, &header)?
    } else {
        TrigramIndex::build(&records, &names, fold)
    };

    Ok(DriveCompactIndex { records, names, children, trigram, fold, .. })
}
```

---

<a id="phase-5"></a>

## Phase 5 — Caller-Level Memory Optimisations ✅ COMPLETE

### 5A: ✅ Drop `MftIndex` Early (Fixes P15)

**File:** CLI/daemon load paths

After building `DriveCompactIndex` from `MftIndex`, the `MftIndex` is
no longer needed. Drop it explicitly to free ~1.6 GB (7M × 240 B):

```rust
let mft_index = reader.read_mft()?;          // 1.6 GB
let compact = build_compact_index(&mft_index, fold);  // +896 MB
drop(mft_index);                              // FREE 1.6 GB ← HERE
```

### 5B: ✅ Scope `MftIndex` in Daemon Startup

```rust
let compact = {
    let mft_index = reader.read_mft()?;
    let compact = build_compact_index(&mft_index, fold);
    compact  // mft_index dropped here automatically
};
```

---

<a id="testing-strategy"></a>

## Testing Strategy

### Per-Phase Test Plan

| Phase | Test Type | What to Verify | Crate |
|-------|-----------|----------------|-------|
| 0 | Unit | `CaseFold` ASCII invariants, European fold (ü↔Ü), CJK identity, non-BMP passthrough | `uffs-text` |
| 0 | Unit | `fold_into` produces valid UTF-8, buffer reuse works across calls | `uffs-text` |
| 0 | Unit | `cmp_str` matches `eq_str`, ordering is consistent and transitive | `uffs-text` |
| 0 | Unit | `pack_char_trigram` ↔ `unpack_char_trigram` roundtrip | `uffs-text` |
| 1 | Integration | Build char-trigram index from test names, search with known queries | `uffs-core` |
| 1 | Regression | Build byte-trigram (old) and char-trigram (new) from same ASCII data → identical results | `uffs-core` |
| 1 | Regression | Build char-trigram with European names → verify "über" matches "ÜBER" | `uffs-core` |
| 2 | Unit | Search match: all query types return correct results | `uffs-core` |
| 2 | Unit | Sort: by name, size, modified, path — verify ordering against golden output | `uffs-core` |
| 2 | Regression | Full search pipeline on fixture data: compare output before/after | `uffs-core` |
| 3 | Unit | BinaryHeap top-N returns same top-K as full sort+truncate | `uffs-core` |
| 3 | Unit | FxHashMap DirCache: functional equivalence to HashMap | `uffs-core` |
| 4 | Integration | Save v6 cache, load it back, verify all fields roundtrip | `uffs-core` |
| 4 | Integration | Load v5 cache with v6 reader, verify trigram rebuild | `uffs-core` |
| 5 | Integration | Memory measurement: peak RSS with/without early MftIndex drop | `uffs-cli` |

**Key change from original:** Phase 0 tests live in `uffs-text`, not
`uffs-core`. This ensures the foundation crate is independently testable
with `cargo test -p uffs-text`.

### Fixture Data Requirements

Create test fixtures that cover:

```
test_names = [
    "readme.txt",          // pure ASCII
    "README.TXT",          // ASCII uppercase (must match "readme.txt")
    "über.txt",            // German lowercase
    "ÜBER.TXT",            // German uppercase (must match "über.txt")
    "résumé.pdf",          // French accents
    "RÉSUMÉ.PDF",          // French uppercase
    "中文文件.doc",         // CJK (no case)
    "файл.txt",            // Cyrillic lowercase
    "ФАЙЛ.TXT",            // Cyrillic uppercase (must match)
    "🎵music.mp3",         // Non-BMP (emoji, no case fold)
    "a",                   // Too short for trigram
    "ab",                  // Too short for trigram
    "abc",                 // Minimum trigram length
    "",                    // Empty name
    ".",                   // Current dir
]
```

### Performance Regression Gate

```
PASS criteria:
  - Search latency (p50): ≤ baseline + 10 ms
  - Search latency (p99): ≤ baseline + 25 ms
  - Trigram build time: ≤ baseline + 50 ms
  - Peak RSS (search): ≤ baseline (should decrease)
  - Allocations per search: ≤ 100 (down from millions)
```

---

<a id="rollback-plan"></a>

## Rollback Plan

### Per-Phase Rollback

Each phase is a self-contained commit or commit series that can be
reverted independently:

| Phase | Rollback | Risk of Partial State |
|-------|----------|-----------------------|
| 0 | Revert `uffs-text` crate, remove from workspace + downstream deps | None — foundation only |
| 1 | Revert trigram changes, restore clone+lowercase callers | Low — self-contained |
| 2 | Revert fold_into calls, restore to_ascii_lowercase | Low — mechanical |
| 3 | Revert individual optimisations | None — fully independent |
| 4 | Revert cache version bump | Forces cache rebuild on downgrade |
| 5 | Revert drop statements | None — no functional impact |

### Cache Rollback

If v6 cache is deployed and needs rollback:
- v5 reader rejects v6 cache (version check) → triggers full rebuild
- No data loss — cache is always rebuildable from MFT

### Feature Flag (Optional)

For gradual rollout, the `CaseFold` struct can be configured at startup:

```rust
use uffs_text::CaseFold;

let fold = if use_ntfs_upcase {
    match uffs_mft::platform::upcase::read_upcase_from_volume(drive_letter) {
        Some(table) => CaseFold::from_ntfs(Box::leak(table)),
        None => CaseFold::default_table(),
    }
} else {
    CaseFold::ascii_only()  // backward-compatible ASCII-only mode
};
```

This allows A/B testing and gradual migration.

---

<a id="success-criteria"></a>

## Success Criteria

### Functional

| # | Criterion | How to Verify |
|---|-----------|---------------|
| F1 | ASCII search works identically | Golden-output comparison |
| F2 | "über" matches "ÜBER.txt" | New test case (`uffs-text` + `uffs-core`) |
| F3 | "résumé" matches "RÉSUMÉ.PDF" | New test case (`uffs-text` + `uffs-core`) |
| F4 | Cyrillic "файл" matches "ФАЙЛ" | New test case (`uffs-text` + `uffs-core`) |
| F5 | CJK "中文" matches "中文" | New test case (`uffs-text`) |
| F6 | Sort by name: case-insensitive Unicode | Golden-output comparison |
| F7 | v5 cache loads correctly with v6 reader | Integration test |
| F8 | v6 cache roundtrips correctly | Integration test |
| F9 | `uffs-text` builds and tests independently | `cargo test -p uffs-text` |
| F10 | No circular dependencies introduced | `cargo tree` verification |

### Performance

| # | Criterion | Target | Before |
|---|-----------|--------|--------|
| P1 | Search latency (7M records, substring) | <50 ms | ~12 ms + alloc overhead |
| P2 | Heap allocs per search | <100 | 50K–7M |
| P3 | Trigram build time | <250 ms | 200 ms + 140 MB clone |
| P4 | Trigram build memory | <2 MB transient | +204 MB transient |
| P5 | Cache load time | <600 ms | 390 ms + 140 MB clone |
| P6 | Peak RSS (search steady-state) | <1.0 GB | ~1.8 GB |
| P7 | Peak RSS (fresh build) | <1.0 GB | ~2.5 GB |
| P8 | Sort (numeric, 10K rows) | <3 ms, 0 allocs | 2 ms, 30K allocs |

### Summary: Before vs After (All Phases Combined)

```
BEFORE (pre-implementation):
  Transient memory for case folding:   420 MB
  Per-search heap allocs:              50,000 – 7,000,000
  Trigram LUT memory:                  64 MB
  Path cache memory (legacy):          168 MB
  MftIndex after compact build:        1.6 GB (wasted)
  Top-N sort algorithm:                O(N log N), 28 MB
  Case sensitivity:                    ASCII only (A-Z)
  NTFS compatibility:                  Partial
  CaseFold location:                   N/A (inline to_ascii_lowercase)

AFTER (all phases implemented):
  Transient memory for case folding:   256 bytes (one reusable buffer)         ✅
  Per-search heap allocs:              ~0 (for fold operations)                ✅
  Trigram LUT memory:                  1.6 MB (FxHashMap)                      ✅
  Path cache memory (legacy):          ~1 MB (FxHashMap<u64, String>)          ✅ Phase 3D
  MftIndex after compact build:        0 (early drop in compact_loader.rs)     ✅ Phase 5A
  Top-N sort algorithm:                O(N log K), capped BinaryHeap           ✅ Phase 3A
  Cache format:                        v6 with persisted char-trigram CSR      ✅ Phase 4
  Case sensitivity:                    Full Unicode via NTFS $UpCase            ✅
  NTFS compatibility:                  100% (live $UpCase when available)       ✅ Phase 0F
  CaseFold location:                   uffs-text (Layer 0 foundation crate)    ✅
  Live $UpCase reader:                 resolve_case_fold() with diff logging   ✅ Phase 0F
  CACHE_PROFILE env read:              Once (LazyLock<bool>)                   ✅ Phase 3E
  Volume prefix allocation:            Stack (zero heap)                       ✅ Phase 3F
  DirCache hash map:                   FxHashMap<u32, String>                  ✅ Phase 3B
  Pattern matching (index_search):     CaseFold pre-folded Vec<u16>            ✅ Phase 2F

  Total memory saved (all phases):     ~1.85 GB
  Total allocs eliminated:             Millions per search
  New permanent overhead:              128 KB ($UpCase table)
  New per-search latency:              +1.4 ms (table lookup)
  New crate:                           uffs-text (~500 LOC, 2 deps)
```