# Casting & Truncation — Full Repository Tracking

> **Generated**: 2026-04-11 | **Last updated**: 2026-04-11 | **Scope**: ALL 13 crates in workspace
> **Reference**: `CASTING_TRUNCATION_AUDIT.md` (2026-03-23, covered 4 crates)
>
> This document extends the original audit to cover the **entire repository**,
> including 7 crates added or grown since the original audit.

---

## Executive Summary

| Metric | Original Audit | After Discovery | Current State |
|--------|---------------|-----------------|---------------|
| Crates covered | 4 | 13 | 13 |
| Lint suppressions | 105 | 170 | **116** |
| Raw `as` casts (prod) | — | ~1,363 | **~1,047** |
| Suppressions removed | 42 | 42 | **96** |
| Completion | 40% | 25% | **47%** |

### Recent Progress — uffs-mft Phase 1 & 2 Complete (2026-04-11)

Removed **54 suppressions** from `uffs-mft` production code (56 → 6 remaining, of which
5 are in centralized helper functions in `index/types.rs` and 1 is a test-module blanket).
Replaced **~361 raw `as` casts** with type-safe helpers (998 → 637).

**Key changes:**
- Created centralized helpers: `nonneg_to_u64`, `u32_as_usize`, `u64_to_f64`, `usize_to_f64`,
  `bytes_to_mb_f64`, `u32_to_f64`, `len_to_u16`, `len_to_u32`, `frs_to_usize`
- Added `AttributeType::END_MARKER`, `DATA_TYPE`, `REPARSE_POINT_TYPE` constants
- Fixed all parser modules: `parse/`, `io/parser/`, `io/extent_map.rs`, `io/chunking.rs`
- Fixed display/stats: `cache.rs`, `display.rs`, `index/base.rs`, `index/storage/file_io.rs`
- Fixed binary-target modules: `commands/load.rs`, `commands/windows/save.rs`
- Zero clippy warnings, zero test failures across entire workspace

---

## Per-Crate Summary

| Crate | Suppressions | Raw `as` Casts (prod) | In Original Audit? | Status |
|-------|-------------|----------------------|-------------------|--------|
| `uffs-mft` | ~~56~~ → **6** | ~~998~~ → **637** | ✅ Yes | ✅ **Done** (5 in helpers, 1 test blanket) |
| `uffs-core` | 71 | 229 | ✅ Yes | ⏳ Next |
| `uffs-cli` | 10 | 21 | ✅ Yes | ⏳ Pending |
| `uffs-diag` | 12 | 44 | ✅ Yes | ⏳ Low priority |
| `uffs-client` | 8 | 27 | ❌ **NEW** | ⏳ Pending |
| `uffs-daemon` | 5 | 30 | ❌ **NEW** | ⏳ Pending |
| `uffs-text` | 7 | 14 | ❌ **NEW** | ⏳ Pending |
| `uffs-broker` | 0 | 8 | ❌ **NEW** | ⏳ Low priority |
| `uffs-security` | 0 | 12 | ❌ **NEW** | ⏳ Low priority |
| `uffs-mcp` | 1 | 5 | ❌ **NEW** | ⏳ Low priority |
| `uffs-tui` | 0 | 7 | ❌ **NEW** | ⏳ Low priority |
| `uffs-polars` | 0 | 0 | — | — |
| `uffs-gui` | 0 | 0 | — | — |
| **Total** | ~~170~~ → **116** | ~~~1,395~~ → **~1,047** | | |

---

## NEW CRATE #1 — `uffs-client` (8 suppressions, 27 `as` casts)

### `shmem.rs` — Shared memory IPC

| Line | Cast | Lint | Risk | Fix |
|------|------|------|------|-----|
| 172 | `row.drive as u8` | — | Low | Widening if drive is char; check type |
| 200 | `row_count as u64` | — | Low | `u64::from()` if source is u32 |
| 201 | `strings_offset as u64` | — | Low | Same |
| 215 | `total_size as u64` | — | Low | Same |
| 257 | blanket `cast_possible_truncation` | **Yes** | Medium | 7 casts: `header.row_count as usize`, `strings_offset as usize`, `rec.path_off as usize`, `rec.path_len as usize`, `rec.name_off as usize`, `rec.name_len as usize`, `header.records_scanned as usize` |
| 294–360 | `as usize` for header fields | covered | Medium | Use `usize::try_from()` |

### `protocol/response.rs` — Response formatting

| Line | Cast | Lint | Risk | Fix |
|------|------|------|------|-----|
| 381 | blanket `cast_precision_loss` | **Yes** | None | Display-only `bytes as f64` — keep |
| 392–396 | `bytes as f64` | covered | None | Display formatting |
| 403–404 | `cast_possible_truncation`, `cast_sign_loss` | **Yes** | Low | DateTime math (`secs % 86400 as u32`, `doe as u32`) — mathematically bounded |
| 408 | `cast_lossless` | **Yes** | None | `yoe as i64` — widening, use `i64::from()` |

### `verify.rs` — Binary verification

| Line | Cast | Lint | Risk | Fix |
|------|------|------|------|-----|
| 145–147 | `cast_possible_wrap`, `cast_possible_truncation`, `cast_sign_loss` | **Yes** | Medium | Win32 API casts (`buf.len() as u32`, `len as usize`, `size as usize`) |
| 160–213 | `buf.len() as u32`, `len as usize`, `size as usize` | covered | Medium | Use `u32::try_from()` for buffer sizes |

### `daemon_ctl.rs` — Daemon control

| Line | Cast | Lint | Risk | Fix |
|------|------|------|------|-----|
| 234 | `size_of::<STARTUPINFOW>() as u32` | — | None | Constant, always fits |
| 311 | `size_of::<TOKEN_ELEVATION>() as u32` | — | None | Constant, always fits |
| 361 | `hinst.0 as isize` | — | Low | Win32 HINSTANCE handle |

---

## NEW CRATE #2 — `uffs-daemon` (5 suppressions, 30 `as` casts)

### `index/aggregation.rs` — Aggregation engine

| Line | Cast | Lint | Risk | Fix |
|------|------|------|------|-----|
| 5–6 | blanket `cast_precision_loss`, `cast_possible_truncation` | **Yes** | Medium | Covers ~15 casts: display floats + index lookups |
| 46, 133 | `drive_ordinal as usize` | covered | Low | u8→usize, lossless |
| 70 | `count as usize` | covered | Medium | Could overflow on 32-bit |
| 110–116 | `bytes as f64` | covered | None | Display-only |
| 278 | `ps as usize` | covered | Low | Page size u16→usize |
| 475, 500 | `as f64` | covered | None | Display-only ratios |
| 795, 799 | `as u32` | covered | Low | Bounded by input |

### `index/mod.rs` — Index core

| Line | Cast | Lint | Risk | Fix |
|------|------|------|------|-----|
| 401 | `cast_precision_loss` | **Yes** | None | Display-only: `total_us as f64 / total_queries as f64` |
| 414, 419 | `as f64` | covered | None | Query stats display |
| 594 | `cast_possible_truncation` | **Yes** | Medium | `idx as u32` — FRS-to-index pattern |
| 598–647 | `idx as u32`, `root_idx as usize`, `child_idx as usize` | — | Medium | Same FRS pattern as uffs-mft |

### `index/search.rs`

| Line | Cast | Lint | Risk | Fix |
|------|------|------|------|-----|
| 266 | `filtered_rows.len() as u64` | — | Low | usize→u64 on 64-bit |
| 268 | `limit as usize` | — | Low | u32→usize |
| 286 | `cap as usize` | — | Low | u32→usize |

### `index/predicates.rs`

| Line | Cast | Lint | Risk | Fix |
|------|------|------|------|-----|
| 463–464 | `chars().count() as u64` | — | Low | usize→u64 for comparison |

### `handler.rs`, `broker_client.rs`

| Line | Cast | Lint | Risk | Fix |
|------|------|------|------|-----|
| handler:115 | `response.records_scanned as u64` | — | Low | Widening |
| handler:120 | `row_count as u64` | — | Low | Widening |
| broker_client:54 | `drive_letter.to_ascii_uppercase() as u8` | — | None | char→u8, ASCII only |

---

## NEW CRATE #3 — `uffs-text` (7 suppressions, 14 `as` casts)

### `case_fold.rs` — Unicode case folding

| Line | Cast | Lint | Risk | Fix |
|------|------|------|------|-----|
| 88 | `ch as u32` | — | None | char→u32, lossless by definition |
| 91, 103 | `cast_possible_truncation` | **Yes** | Medium | `cp as u16` — only valid for BMP codepoints; guarded by `cp <= 0xFFFF` check |
| 94–95 | `cp as u16`, `cp as usize` | covered | Medium | Table lookup with BMP guard |
| 107 | `cp as u16` | — | Medium | Needs BMP guard verification |
| 248, 270, 276, 283 | `cast_possible_truncation` | **Yes** | Medium | `idx as u16`, `cp as u16`, `folded as u8` — all guarded by range checks |
| 252 | `idx as u16` | covered | Low | Index within 64K table |

### `trigram_key.rs` — Trigram key packing

| Line | Cast | Lint | Risk | Fix |
|------|------|------|------|-----|
| 14 | `cp0 as u64`, `cp1 as u64`, `cp2 as u64` | — | None | u16→u64 widening |
| 21 | `cast_possible_truncation` | **Yes** | None | `(packed >> N) as u16` — extracting u16 from known positions, mathematically correct |

---

## NEW CRATE #4 — `uffs-broker` (0 suppressions, 8 `as` casts)

### `broker.rs` — Named-pipe broker for privilege elevation

| Line | Cast | Lint | Risk | Fix |
|------|------|------|------|-----|
| 236 | `buf.len() as u32` | — | Medium | Win32 buffer size, should use `u32::try_from()` |
| 247 | `size as usize` | — | Low | u32→usize, lossless on 64-bit |
| 362 | `win_err.code().0 as u32` | — | Low | HRESULT comparison |
| 423 | `buf.len() as u32` | — | Medium | Same as 236 |
| 435 | `size as usize` | — | Low | Same as 247 |
| 525 | `client_handle.0 as u64` | — | Low | Handle value for logging |
| 555 | `bytes_read as usize` | — | Low | u32→usize comparison |
| 599 | `size_of::<TOKEN_ELEVATION>() as u32` | — | None | Constant, always fits |

**Assessment**: No suppressions needed — casts are all Win32 API interop.
Fix `buf.len() as u32` with `u32::try_from()` for defense-in-depth (2 instances).

---

## NEW CRATE #5 — `uffs-security` (0 suppressions, 12 `as` casts)

### `keystore.rs` — DPAPI key storage (Windows)

| Line | Cast | Lint | Risk | Fix |
|------|------|------|------|-----|
| 212, 216 | `data.len() as u32`, `DPAPI_ENTROPY.len() as u32` | — | Low | Key data always small |
| 253, 312 | `output_blob.cbData as usize` | — | Medium | u32→usize from Win32 API output |
| 272, 276 | `blob.len() as u32`, `DPAPI_ENTROPY.len() as u32` | — | Low | Same pattern |

### `fs.rs` — Secure file I/O

| Line | Cast | Lint | Risk | Fix |
|------|------|------|------|-----|
| 222 | `ZERO_BUF_SIZE as u64` | — | None | Constant widening |
| 231 | `chunk as u64` | — | Low | usize→u64 |

### `crypto.rs` — Encryption

| Line | Cast | Lint | Risk | Fix |
|------|------|------|------|-----|
| 113 | `plaintext.len() as u64` | — | Low | usize→u64, lossless on 64-bit |
| 223 | `u32::from_le_bytes(len_buf) as usize` | — | Medium | u32→usize, fine on 64-bit |

**Assessment**: All Windows FFI interop casts. Low risk. Add `u32::try_from()` for
`len()` calls for consistency with the rest of the codebase.

---

## NEW CRATE #6 — `uffs-mcp` (1 suppression, 5 `as` casts)

### `main.rs`

| Line | Cast | Lint | Risk | Fix |
|------|------|------|------|-----|
| 49 | `cast_sign_loss` | **Yes** | Low | Test fixture data: `(15 - i) as u64` |
| 253–254 | `(15 - i) as u64` | covered | Low | Bounded loop counter |

### `tools/search.rs`

| Line | Cast | Lint | Risk | Fix |
|------|------|------|------|-----|
| 364 | `offset as usize` | — | Low | Pagination offset |
| 365 | `effective_limit as usize` | — | Low | Pagination limit |

**Assessment**: Minimal. The `cast_sign_loss` is in test/example code.

---

## NEW CRATE #7 — `uffs-tui` (0 suppressions, 7 `as` casts)

**Assessment**: All casts appear to be in display/TUI formatting code. No suppressions.
Low priority — review for `as f64` display patterns only.

---

## EXPANDED: `uffs-core` New Modules (not in original audit)

The original audit covered `path_resolver/` and `format.rs`. These modules are **new**:

### `compact.rs` — Compact index builder (9 suppressions)

| Line | Lint | Context | Fix |
|------|------|---------|-----|
| 174, 283, 432, 437 | `cast_possible_truncation` | `idx as u32` — record index fits u32 by design | Use `len_to_u32()` helper |
| 562, 565 | `cast_possible_truncation` | `idx as u32` — same pattern, with comment | Same |
| 585, 607 | `cast_possible_truncation` | Clamped to u16::MAX before cast | Correct pattern, improve reason |
| 625 | `cast_possible_truncation` | Filename len → u32 | Use `len_to_u32()` |

### `compact_reader.rs` — Compact index reader (2 suppressions)

| Line | Lint | Context | Fix |
|------|------|---------|-----|
| 71 | `cast_possible_truncation` | `HEADER_SIZE as usize` — constant | Use `usize::from()` |
| 142 | `cast_possible_truncation` | `record_byte_size as usize` — u32→usize | Lossless on 64-bit |

### `compact_loader.rs` — Compact index loader (3 suppressions)

| Line | Lint | Context | Fix |
|------|------|---------|-----|
| 384 | `cast_possible_truncation` | `name_start as u32` — name buffer offset | Use `len_to_u32()` |
| 405 | `cast_possible_truncation` | Same pattern | Same |
| 443 | `cast_possible_truncation` | Same pattern | Same |

### `compact_cache.rs` — Cache serialization (1 suppression)

| Line | Lint | Context | Fix |
|------|------|---------|-----|
| 628 | `cast_possible_truncation` | `value as u32` — usize→u32 for serialization | Guard with assert or `len_to_u32()` |

### `slot_pool.rs` — Memory pool (6 suppressions)

| Line | Lint | Context | Fix |
|------|------|---------|-----|
| 187–190 | `cast_possible_truncation`, `cast_sign_loss`, `cast_precision_loss` | Memory calculation: `(decompressed as f64 * MULTIPLIER) as u64` | Arithmetic — keep with reason |
| 228–230 | Same 3 lints | `(mem.available_bytes as f64 * FRACTION) as u64`, `(budget / max_cost) as usize` | Same — memory budget math |

### `trigram.rs` — Trigram index (2 suppressions)

| Line | Lint | Context | Fix |
|------|------|---------|-----|
| 156 | `cast_possible_truncation` | `key_idx as u32` — trigram key fits u32 | Use `len_to_u32()` |
| 380 | `cast_possible_truncation` | `rec_idx as u32` — record index | Same |

### `aggregate/mod.rs` — Aggregation engine (5 suppressions)

| Line | Lint | Context | Fix |
|------|------|---------|-----|
| 24–26 | blanket: `cast_precision_loss`, `cast_possible_truncation`, `cast_sign_loss` | Module-level blanket covering ~30 casts | Split into targeted expects |
| 579–580 | `cast_possible_truncation`, `cast_precision_loss` | Test helper function | Targeted expect is fine |

### `search/` subtree (20+ suppressions)

| File | Suppressions | Context |
|------|-------------|---------|
| `search/filters/mod.rs` | 1 | `chars().count() as u16` — name length |
| `search/filters/apply.rs` | 2 | `chars().count() as u16` — name/path length |
| `search/filters/time_parsing.rs` | 3 | DateTime math — `cast_possible_truncation`, `cast_sign_loss` |
| `search/sorting.rs` | 3 | `cast_possible_truncation`, `cast_sign_loss` — DataFrame row indices |
| `search/backend.rs` | 1 | `rfind() as u32` — path < 4GB |
| `search/tree.rs` | 4 | `idx as u32`, `parent as usize` — FRS/index pattern |
| `search/query/mod.rs` | 3 | `idx as u32` — FRS/index pattern |
| `search/query/numeric_top_n.rs` | 12 | `cast_possible_wrap`, `cast_possible_truncation` — sort keys |

### `output/config.rs` (4 suppressions)

| Line | Lint | Context | Fix |
|------|------|---------|-----|
| 688, 696 | `cast_possible_truncation` | `chars().count() as u16` — name/path length | Bounded by filesystem limits |
| 719, 723 | `cast_sign_loss`, `cast_possible_truncation` | DateTime decomposition — `rem_euclid` pattern | Mathematically bounded |

---

## ✅ COMPLETED: `uffs-mft` (2026-04-11)

**Before**: 56 suppressions, 998 raw `as` casts
**After**: 6 suppressions (5 in centralized helpers, 1 test blanket), 637 raw `as` casts
**Removed**: 50 suppressions, ~361 raw `as` casts replaced with type-safe helpers

### Centralized helpers created in `index/types.rs`:

| Helper | Replaces | Lint Absorbed |
|--------|----------|---------------|
| `nonneg_to_u64(i64) -> u64` | `.max(0) as u64` (35 sites) | `cast_sign_loss` |
| `u32_as_usize(u32) -> usize` | `field as usize` for NTFS fields | `cast_possible_truncation` |
| `len_to_u16(usize) -> u16` | `name_len as u16` (saturating) | `cast_possible_truncation` |
| `len_to_u32(usize) -> u32` | `vec.len() as u32` (saturating) | `cast_possible_truncation` |
| `frs_to_usize(u32) -> usize` | `frs as usize` | `cast_possible_truncation` |
| `u64_to_f64(u64) -> f64` | `bytes as f64` for display | `cast_precision_loss` |
| `usize_to_f64(usize) -> f64` | `count as f64` for display | `cast_precision_loss` |
| `bytes_to_mb_f64(u64) -> f64` | `bytes as f64 / MB` for display | `cast_precision_loss` |
| `u32_to_f64(u32) -> f64` | `val as f64` (uses `f64::from`) | None (truly lossless) |

### Constants added to `ntfs/records.rs`:
- `AttributeType::END_MARKER` (u32) — replaces `AttributeType::End as u32`
- `AttributeType::DATA_TYPE` (u32) — replaces `AttributeType::Data as u32`
- `AttributeType::REPARSE_POINT_TYPE` (u32) — replaces `AttributeType::ReparsePoint as u32`

### Files modified (production code):
`parse/direct_index.rs`, `parse/direct_index_extension.rs`, `parse/index_helpers.rs`,
`parse/attribute_helpers.rs`, `parse/forensic/base.rs`, `parse/forensic/extension.rs`,
`parse/full.rs`, `io/parser/index.rs`, `io/parser/index_extension.rs`,
`io/parser/unified.rs`, `io/parser/fragment.rs`, `io/parser/fragment_extension.rs`,
`io/extent_map.rs`, `io/chunking.rs`, `io/readers/parallel/mod.rs`,
`cache.rs`, `display.rs`, `index/base.rs`, `index/storage/file_io.rs`,
`commands/load.rs`, `commands/windows/save.rs`,
`platform/system.rs`, `platform/upcase.rs`, `ntfs/data_runs.rs`, `ntfs/boot_sector.rs`

### Remaining 6 suppressions (all intentional):

| File | Lint | Why Kept |
|------|------|----------|
| `index/types.rs:81` | `cast_sign_loss` | Inside `nonneg_to_u64` — the centralized helper |
| `index/types.rs:106` | `cast_precision_loss` | Inside `u64_to_f64` — centralized helper |
| `index/types.rs:119` | `cast_precision_loss` | Inside `usize_to_f64` — centralized helper |
| `index/types.rs:132` | `cast_precision_loss` | Inside `bytes_to_mb_f64` — centralized helper |
| `index.rs:60` | `cast_possible_truncation` | Test module blanket |
| `index.rs:64` | `cast_sign_loss` | Test module blanket |

### Remaining `as` casts (637) — breakdown:
- `#[cfg(windows)]` code (persistence_capture, usn, platform) — ~40 casts, not linted on macOS
- Win32 API interop (`size_of as u32`, handle casts) — safe, constant or bounded
- Already use helpers but counted by grep (false positives from helper call sites)

---

## Action Plan — Priority Order

### ~~Phase 0: uffs-mft~~ ✅ COMPLETE
- [x] Created centralized helpers in `index/types.rs`
- [x] Removed 50 suppressions from parser, I/O, display, cache, and command modules
- [x] Replaced ~361 raw `as` casts with type-safe helpers
- [x] Zero clippy warnings, all 131 uffs-mft tests pass

### Phase 3: `uffs-core` (71 suppressions) — NEXT
- [ ] **compact*.rs**: Use `len_to_u32()`/`len_to_u16()` helpers (14 suppressions)
- [ ] **slot_pool.rs**: Review memory budget math (6 suppressions)
- [ ] **trigram.rs**: Use `len_to_u32()` (2 suppressions)
- [ ] **aggregate/mod.rs**: Split blanket into targeted expects (3 blankets → ~30 targeted)
- [ ] **search/tree.rs**: Apply `frs_to_usize()` pattern (4 suppressions)
- [ ] **search/query/mod.rs**: Same (3 suppressions)
- [ ] **search/query/numeric_top_n.rs**: Review 12 suppressions
- [ ] **search/filters/**: DateTime math, name length (6 suppressions)
- [ ] **search/sorting.rs**: DataFrame row indices (3 suppressions)
- [ ] **output/config.rs**: DateTime decomposition, name length (4 suppressions)
- [ ] **path_resolver/, format.rs**: Original audit items

### Phase 4: `uffs-cli` (10 suppressions)
- [ ] **commands.rs, system_status.rs, mcp_mgmt.rs, aggregate.rs, daemon_mgmt.rs, info.rs**

### Phase 5: `uffs-client` (8 suppressions)
- [ ] **shmem.rs**: Replace blanket with targeted `usize::try_from()` (1 blanket, 7 casts)
- [ ] **protocol/response.rs**: Replace `cast_lossless` with `i64::from()` (1 suppression)
- [ ] **verify.rs**: Win32 API casts (3 suppressions)
- [ ] **daemon_ctl.rs**: Win32 handle casts

### Phase 6: `uffs-daemon` (5 suppressions)
- [ ] **index/aggregation.rs**: Split blanket (2 blankets → ~15 targeted)
- [ ] **index/mod.rs**: FRS pattern (2 suppressions)
- [ ] **index/search.rs**: Pagination casts

### Phase 7: `uffs-text` (7 suppressions)
- [ ] **case_fold.rs**: Review BMP guard patterns (6 suppressions)
- [ ] **trigram_key.rs**: Bit extraction (1 suppression)

### Phase 8: Smaller crates (1 suppression)
- [ ] **uffs-mcp**: Test fixture cast_sign_loss (1 suppression)
- [ ] **uffs-broker**: Add `u32::try_from()` for `buf.len()` (0 suppressions, 2 casts)
- [ ] **uffs-security**: Add `u32::try_from()` for Win32 buffer sizes (0 suppressions, 6 casts)
- [ ] **uffs-tui**: Review display casts (0 suppressions)

### Phase 9: `uffs-diag` (12 suppressions)
- [ ] Diagnostic tool blankets — low priority

### Phase 10: Test code
- [ ] Fix test code casts across all crates

---

## Updated Scorecard

| Phase | Suppressions | Status |
|-------|-------------|--------|
| Phase 0: uffs-mft (helpers + all prod code) | 50 removed | ✅ **Done** |
| Phase 3: uffs-core | ~71 to address | ⏳ Next |
| Phase 4: uffs-cli | ~10 to address | ⏳ Pending |
| Phase 5: uffs-client | ~8 to address | ⏳ Pending |
| Phase 6: uffs-daemon | ~5 to address | ⏳ Pending |
| Phase 7: uffs-text | ~7 to address | ⏳ Pending |
| Phase 8: Smaller crates | ~1 to address | ⏳ Low priority |
| Phase 9: uffs-diag | ~12 to address | ⏳ Low priority |
| Phase 10: Test code | TBD | ⏳ Low priority |
| Legitimate keeps (centralized helpers) | 5 in uffs-mft | 📌 Keep |
| **Original total** | **170** | |
| **Removed so far** | **54 (32%)** | |
| **Remaining suppressions** | **116** | |
| **Of which addressable** | **~110** | |
| **Of which legitimate keeps** | **~6** | |
