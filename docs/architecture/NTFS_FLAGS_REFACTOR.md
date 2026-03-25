# NTFS Flags Refactor: Use Raw Bit Layout Everywhere

> **Status**: Planning  
> **Date**: 2026-03-24  
> **Priority**: High — correctness issue causing bugs in TUI directory detection,
> attribute filtering, and any downstream consumer of `StandardInfo.flags`

---

## Problem

`StandardInfo.flags` uses a **custom remapped bit layout** that differs from
the standard NTFS `FILE_ATTRIBUTE_*` constants:

| Attribute | NTFS Raw | StandardInfo Current | Delta |
|-----------|----------|---------------------|-------|
| READ_ONLY | 0x0001 (bit 0) | bit 0 | ✅ same |
| HIDDEN | 0x0002 (bit 1) | bit 3 | ❌ moved |
| SYSTEM | 0x0004 (bit 2) | bit 2 | ✅ same |
| DIRECTORY | 0x0010 (bit 4) | bit 10 | ❌ moved |
| ARCHIVE | 0x0020 (bit 5) | bit 1 | ❌ moved |
| SPARSE | 0x0200 (bit 9) | bit 13 | ❌ moved |
| REPARSE | 0x0400 (bit 10) | bit 14 | ❌ moved |
| COMPRESSED | 0x0800 (bit 11) | bit 11 | ✅ same |
| OFFLINE | 0x1000 (bit 12) | bit 4 | ❌ moved |
| NOT_INDEXED | 0x2000 (bit 13) | bit 5 | ❌ moved |
| ENCRYPTED | 0x4000 (bit 14) | bit 12 | ❌ moved |

**11 of 17 flags are in different positions than NTFS standard.**

### Consequences

1. **TUI `is_directory()` was broken** — checked bit 4 (NTFS convention)
   but `StandardInfo` stores directory at bit 10. Required `to_attributes()`
   workaround.

2. **Any new consumer** of `StandardInfo.flags` must know about the remapping
   or use `to_attributes()`. This is a trap for future developers.

3. **Conversion overhead** — `from_attributes()` and `to_attributes()` do
   bit-shuffling on every record. With 25M records, this adds up.

4. **Debugging confusion** — raw MFT attribute values (visible in hex editors,
   NTFS docs, Windows API) don't match what's stored in `StandardInfo.flags`.

5. **The `.uffs` cache format** serializes `StandardInfo.flags` directly,
   meaning cached files use the non-standard layout. External tools can't
   read them without knowing the remapping.

---

## Goal

Make `StandardInfo.flags` store **raw NTFS `FILE_ATTRIBUTE_*` bits** directly.
No remapping, no conversion. The bit at position N in `flags` means exactly
what NTFS says it means.

## Mandatory Rule: Named Constants Everywhere — Zero Magic Numbers

**The root cause of this bug was using raw hex values (`0x0001`, `0x0010`)
instead of named constants.** If `from_raw_ntfs_flags()` had used
`Self::IS_HIDDEN` instead of `0x0002`, the remapping would have been
caught immediately.

**Rule**: After this refactor, **every single place** in the codebase that
checks or sets NTFS attribute bits must use the named `StandardInfo::IS_*`
constants. No raw hex values. No `1 << N`. No `0x0010`. Always
`StandardInfo::IS_DIRECTORY`.

This applies to:
- `from_raw_ntfs_flags()` — input raw NTFS bits, but use constants on the output side
- `to_attributes()` — becomes trivial (identity), but if it ever needs logic, use constants
- `from_extended()` — already uses constants ✅
- `from_attributes()` — becomes trivial (identity)
- All parsers, consumers, CLI, TUI, tests
- The `v8_flags_to_ntfs()` compat function uses hex for the OLD v8 layout (acceptable
  since those are the frozen v8 format values), but maps TO named constants

If a developer ever needs to reference an NTFS attribute bit, they write
`StandardInfo::IS_HIDDEN`, never `0x0002`. This is enforced by code review
and should be checked in CI (grep for bare hex attribute values).

### Target Layout

```rust
impl StandardInfo {
    // Raw NTFS FILE_ATTRIBUTE_* constants — no remapping
    pub const IS_READONLY:      u32 = 0x0001;
    pub const IS_HIDDEN:        u32 = 0x0002;
    pub const IS_SYSTEM:        u32 = 0x0004;
    pub const IS_DIRECTORY:     u32 = 0x0010;
    pub const IS_ARCHIVE:       u32 = 0x0020;
    pub const IS_DEVICE:        u32 = 0x0040;
    pub const IS_NORMAL:        u32 = 0x0080;
    pub const IS_TEMPORARY:     u32 = 0x0100;
    pub const IS_SPARSE:        u32 = 0x0200;
    pub const IS_REPARSE:       u32 = 0x0400;
    pub const IS_COMPRESSED:    u32 = 0x0800;
    pub const IS_OFFLINE:       u32 = 0x1000;
    pub const IS_NOT_INDEXED:   u32 = 0x2000;
    pub const IS_ENCRYPTED:     u32 = 0x4000;
    pub const IS_INTEGRITY:     u32 = 0x8000;
    pub const IS_VIRTUAL:       u32 = 0x10000;
    pub const IS_NO_SCRUB_DATA: u32 = 0x20000;
    pub const IS_PINNED:        u32 = 0x80000;
    pub const IS_UNPINNED:      u32 = 0x100000;
}
```

After this refactor:
- `from_extended()` just copies the raw attribute bits (no shuffling)
- `to_attributes()` becomes a no-op (returns `self.flags` directly)
- `from_attributes()` is trivial (just store the u32)
- All downstream code uses standard NTFS constants
- `.uffs` cache stores raw NTFS bits (debuggable, portable)

---

## Impact Audit

### Files by match count (727 total references across 42 files)

| File | Matches | Impact |
|------|---------|--------|
| `reader/dataframe_build.rs` | 230 | DataFrame column extraction — reads flags |
| `index/standard_info.rs` | 128 | **Core: constant definitions + accessors** |
| `parse/columns.rs` | 108 | Parsed column extraction — sets flags |
| `ntfs/metadata.rs` | 47 | `ExtendedStandardInfo` — raw NTFS parsing |
| `index/dataframe.rs` | 24 | DataFrame conversion — reads flags |
| `commands/windows/info.rs` | 18 | CLI info display |
| `parse/forensic/base.rs` | 14 | Forensic parser — sets flags |
| `parse/full.rs` | 14 | Full record parser — sets flags |
| `flags.rs` | 13 | `FileFlags` bitflags — separate from `StandardInfo` |
| `commands/load.rs` | 10 | Load command — reads flags |
| `index/base.rs` | 9 | MftIndex operations — reads flags |
| Other (31 files) | ~100 | Various reads/writes |

### External crates affected

| Crate | Files | Impact |
|-------|-------|--------|
| `uffs-mft` | 42 files, 727 refs | **Primary target** — all flag constants + usage |
| `uffs-cli` | 6 files, 57 refs | CLI output, streaming filter, attribute display |
| `uffs-core` | 0 refs | No direct flag usage (pattern matching only) |
| `uffs-tui` | 1 file, 1 ref | Uses `to_attributes()` workaround (will simplify) |

### Serialization / Cache Impact

- **`.uffs` cache format version must bump** from v8 to v9
- Serialization writes `stdinfo.flags` directly — bit layout changes
- Old v8 caches become unreadable (automatic rebuild on version mismatch)
- The `deserialize()` function already handles version-conditional reads

---

## Implementation Plan

### Phase 1: Change the constants (core fix)

**File**: `crates/uffs-mft/src/index/standard_info.rs`

```rust
// BEFORE (remapped):
pub const IS_READONLY: u32 = 1 << 0;
pub const IS_ARCHIVE: u32 = 1 << 1;
pub const IS_SYSTEM: u32 = 1 << 2;
pub const IS_HIDDEN: u32 = 1 << 3;
// ...

// AFTER (raw NTFS):
pub const IS_READONLY: u32 = 0x0001;  // bit 0  — same
pub const IS_HIDDEN: u32 = 0x0002;    // bit 1  — was bit 3
pub const IS_SYSTEM: u32 = 0x0004;    // bit 2  — same
pub const IS_DIRECTORY: u32 = 0x0010; // bit 4  — was bit 10
pub const IS_ARCHIVE: u32 = 0x0020;   // bit 5  — was bit 1
// ...
```

| Task | Status | Notes |
|------|--------|-------|
| Update all 17 `IS_*` constants to raw NTFS values | ⏳ | Core change |
| Simplify `from_extended()` — direct bit copy, no shuffling | ⏳ | |
| Simplify `to_attributes()` — return `self.flags` directly | ⏳ | |
| Simplify `from_attributes()` — store raw, no conversion | ⏳ | |
| Remove deprecated `from_attributes()` warning | ⏳ | No longer needed |
| Update accessor methods (no change needed — they use constants) | ⏳ | Auto-fixed |

### Phase 2: Update parsers

All parsers that set flags via `StandardInfo` constants will automatically
use the new values. But verify each one:

| Task | Status | Notes |
|------|--------|-------|
| `parse/full.rs` — full record parser | ⏳ | Uses `set_directory()` etc. |
| `parse/columns.rs` — column extraction | ⏳ | Sets individual flags |
| `parse/forensic/base.rs` — forensic parser | ⏳ | Sets individual flags |
| `parse/forensic/extension.rs` — extension parser | ⏳ | |
| `parse/attribute_helpers.rs` — attribute helpers | ⏳ | |
| `parse/merger.rs` — record merger | ⏳ | |
| `io/parser/index.rs` — IOCP parser | ⏳ | |
| `io/parser/fragment.rs` — fragment parser | ⏳ | |
| `io/parser/unified.rs` — unified parser | ⏳ | |
| `ntfs/metadata.rs` — `ExtendedStandardInfo` | ⏳ | Raw NTFS parsing origin |

### Phase 3: Update serialization

| Task | Status | Notes |
|------|--------|-------|
| Bump `.uffs` format version to v9 | ⏳ | `INDEX_VERSION = 9` |
| `serialize.rs` — no code change needed (writes `flags` directly) | ⏳ | Verify |
| `deserialize.rs` — add v9 handling, v8 compat (convert old flags) | ⏳ | |
| Old v8 caches auto-rebuild on version mismatch | ⏳ | Already handled |

### Phase 4: Update consumers

| Task | Status | Notes |
|------|--------|-------|
| `reader/dataframe_build.rs` — DataFrame columns (230 refs) | ⏳ | Reads flags |
| `index/dataframe.rs` — DataFrame conversion | ⏳ | |
| `index/base.rs` — MftIndex operations | ⏳ | |
| `index/tree.rs` — tree metrics | ⏳ | |
| `tree_metrics.rs` — tree computation | ⏳ | |
| `commands/load.rs` — CLI load command | ⏳ | |
| `commands/windows/info.rs` — CLI info display | ⏳ | |
| `commands/windows/benchmark_index.rs` | ⏳ | |
| `commands/windows/incremental.rs` | ⏳ | |
| `flags.rs` — `FileFlags` bitflags (already NTFS layout) | ⏳ | Verify alignment |

### Phase 5: Update CLI (uffs-cli)

| Task | Status | Notes |
|------|--------|-------|
| `output/mod.rs` — streaming output (19 refs) | ⏳ | |
| `output/row_writer.rs` — row writer (14 refs) | ⏳ | |
| `output/filter.rs` — attribute filter (8 refs) | ⏳ | |
| `output/output_tests.rs` — tests (6 refs) | ⏳ | |
| `commands/info.rs` — info command (5 refs) | ⏳ | |
| `output/types.rs` — type definitions (5 refs) | ⏳ | |

### Phase 6: Update TUI

| Task | Status | Notes |
|------|--------|-------|
| `compact.rs` — remove `to_attributes()` call, use `flags` directly | ⏳ | Simplification |
| `DIRECTORY_BIT = 0x0010` already correct after refactor | ⏳ | |
| Update `flags` doc comment | ⏳ | |

### Phase 7: Verify

| Task | Status | Notes |
|------|--------|-------|
| All existing tests pass | ⏳ | `cargo test --workspace` |
| Clippy clean | ⏳ | `cargo clippy --workspace` |
| Parity check: CLI output matches before/after | ⏳ | Compare CSV output |
| TUI: `is_directory()`, F3 filter, tree search all work | ⏳ | Manual test |
| Old `.uffs` cache auto-rebuilds (v8 → v9) | ⏳ | |

---

## Risk Analysis

### Risk 1: Serialization backward compatibility

Old `.uffs` v8 files have remapped flags. New v9 code expects raw NTFS.

**Mitigation**: `deserialize()` already handles version-conditional reads.
For v8 files, apply the inverse remapping during deserialization (convert
old remapped bits to raw NTFS bits). Or simply invalidate old caches
(version mismatch triggers full rebuild — already implemented).

### Risk 2: 727 reference changes

Most references are `StandardInfo::IS_*` constants used in boolean
checks like `if stdinfo.is_hidden()`. Since the accessor methods
(`is_hidden()`, `is_directory()`, etc.) use the constants internally,
**changing the constants automatically fixes all 727 references**.

The only manual work is in functions that directly manipulate `flags`
bits without going through accessors.

### Risk 3: `FileFlags` in `flags.rs` (separate type)

`FileFlags` is a separate `bitflags!` type that **already uses raw NTFS
layout** (0x0001=READONLY, 0x0002=HIDDEN, etc.). After this refactor,
`StandardInfo` and `FileFlags` will finally be aligned.

### Risk 4: `ExtendedStandardInfo` in `ntfs/metadata.rs`

This struct parses raw NTFS `$STANDARD_INFORMATION` attributes and stores
individual `bool` fields. `StandardInfo::from_extended()` converts these
bools to the packed flags. After the refactor, `from_extended()` simply
maps each bool to its raw NTFS bit position — much simpler.

---

## Exact Code Changes

### 1. Constants (`standard_info.rs` lines 36-69)

```rust
// ─── BEFORE (remapped, non-standard) ───────────────────
pub const IS_READONLY:        u32 = 1 << 0;   // 0x0001
pub const IS_ARCHIVE:         u32 = 1 << 1;   // 0x0002 ← WRONG (NTFS: 0x0020)
pub const IS_SYSTEM:          u32 = 1 << 2;   // 0x0004
pub const IS_HIDDEN:          u32 = 1 << 3;   // 0x0008 ← WRONG (NTFS: 0x0002)
pub const IS_OFFLINE:         u32 = 1 << 4;   // 0x0010 ← WRONG (NTFS: 0x1000)
pub const IS_NOT_INDEXED:     u32 = 1 << 5;   // 0x0020 ← WRONG (NTFS: 0x2000)
pub const IS_NO_SCRUB_DATA:   u32 = 1 << 6;   // 0x0040 ← WRONG (NTFS: 0x20000)
pub const IS_INTEGRITY_STREAM:u32 = 1 << 7;   // 0x0080 ← WRONG (NTFS: 0x8000)
pub const IS_PINNED:          u32 = 1 << 8;   // 0x0100 ← WRONG (NTFS: 0x80000)
pub const IS_UNPINNED:        u32 = 1 << 9;   // 0x0200 ← WRONG (NTFS: 0x100000)
pub const IS_DIRECTORY:       u32 = 1 << 10;  // 0x0400 ← WRONG (NTFS: 0x0010)
pub const IS_COMPRESSED:      u32 = 1 << 11;  // 0x0800
pub const IS_ENCRYPTED:       u32 = 1 << 12;  // 0x1000 ← WRONG (NTFS: 0x4000)
pub const IS_SPARSE:          u32 = 1 << 13;  // 0x2000 ← WRONG (NTFS: 0x0200)
pub const IS_REPARSE:         u32 = 1 << 14;  // 0x4000 ← WRONG (NTFS: 0x0400)
pub const IS_TEMPORARY:       u32 = 1 << 15;  // 0x8000 ← WRONG (NTFS: 0x0100)
pub const IS_VIRTUAL:         u32 = 1 << 16;  // 0x10000

// ─── AFTER (raw NTFS FILE_ATTRIBUTE_*) ─────────────────
pub const IS_READONLY:        u32 = 0x0001;
pub const IS_HIDDEN:          u32 = 0x0002;
pub const IS_SYSTEM:          u32 = 0x0004;
pub const IS_DIRECTORY:       u32 = 0x0010;
pub const IS_ARCHIVE:         u32 = 0x0020;
pub const IS_DEVICE:          u32 = 0x0040;
pub const IS_NORMAL:          u32 = 0x0080;
pub const IS_TEMPORARY:       u32 = 0x0100;
pub const IS_SPARSE:          u32 = 0x0200;
pub const IS_REPARSE:         u32 = 0x0400;
pub const IS_COMPRESSED:      u32 = 0x0800;
pub const IS_OFFLINE:         u32 = 0x1000;
pub const IS_NOT_INDEXED:     u32 = 0x2000;
pub const IS_ENCRYPTED:       u32 = 0x4000;
pub const IS_INTEGRITY_STREAM:u32 = 0x8000;
pub const IS_VIRTUAL:         u32 = 0x10000;
pub const IS_NO_SCRUB_DATA:   u32 = 0x20000;
pub const IS_PINNED:          u32 = 0x80000;
pub const IS_UNPINNED:        u32 = 0x100000;
```

### 2. `from_raw_ntfs_flags()` — becomes trivial

Since constants now ARE raw NTFS values, there's no remapping. Just store.

```rust
// ─── BEFORE (50 lines with MAGIC HEX NUMBERS) ─────────
pub const fn from_raw_ntfs_flags(attrs: u32) -> Self {
    let mut flags = 0_u32;
    if attrs & 0x0001 != 0 { flags |= Self::IS_READONLY; }  // 0x0001 = magic number!
    if attrs & 0x0002 != 0 { flags |= Self::IS_HIDDEN; }    // 0x0002 = magic number!
    if attrs & 0x0004 != 0 { flags |= Self::IS_SYSTEM; }    // etc.
    if attrs & 0x0020 != 0 { flags |= Self::IS_ARCHIVE; }
    if attrs & 0x0100 != 0 { flags |= Self::IS_TEMPORARY; }
    if attrs & 0x0200 != 0 { flags |= Self::IS_SPARSE; }
    if attrs & 0x0400 != 0 { flags |= Self::IS_REPARSE; }
    if attrs & 0x0800 != 0 { flags |= Self::IS_COMPRESSED; }
    if attrs & 0x1000 != 0 { flags |= Self::IS_OFFLINE; }
    if attrs & 0x2000 != 0 { flags |= Self::IS_NOT_INDEXED; }
    if attrs & 0x4000 != 0 { flags |= Self::IS_ENCRYPTED; }
    if attrs & 0x8000 != 0 { flags |= Self::IS_INTEGRITY_STREAM; }
    if attrs & 0x0001_0000 != 0 { flags |= Self::IS_VIRTUAL; }
    if attrs & 0x0002_0000 != 0 { flags |= Self::IS_NO_SCRUB_DATA; }
    if attrs & 0x0008_0000 != 0 { flags |= Self::IS_PINNED; }
    if attrs & 0x0010_0000 != 0 { flags |= Self::IS_UNPINNED; }
    Self { flags, ..Self::DEFAULT }
}

// ─── AFTER (1 line — constants ARE raw NTFS, no remap) ──
pub const fn from_raw_ntfs_flags(attrs: u32) -> Self {
    Self { flags: attrs, ..Self::DEFAULT }
}
```

### 3. `from_extended()` — direct bit mapping

```rust
// ─── BEFORE (30 lines of if-statements) ────────────────
pub const fn from_extended(ext: &ExtendedStandardInfo) -> Self {
    let mut flags = 0_u32;
    if ext.is_readonly { flags |= Self::IS_READONLY; }
    if ext.is_archive { flags |= Self::IS_ARCHIVE; }
    // ... 14 more if-statements ...
    Self { flags, created: ext.created, ... }
}

// ─── AFTER (same structure, but constants now match NTFS) ──
// No code change needed — the if-statements use IS_* constants
// which now have the correct NTFS values. The logic is identical,
// the output bits are different (correct).
```

### 4. `to_attributes()` — becomes identity

```rust
// ─── BEFORE (30 lines of reverse bit shuffling) ────────
pub const fn to_attributes(&self) -> u32 {
    let mut attrs = 0_u32;
    if self.is_readonly() { attrs |= 0x0001; }
    if self.is_hidden() { attrs |= 0x0002; }
    // ... 14 more if-statements ...
    attrs
}

// ─── AFTER (1 line — flags ARE raw NTFS already) ───────
pub const fn to_attributes(&self) -> u32 {
    self.flags
}
```

### 5. `from_attributes()` (deprecated) — becomes identity

```rust
// ─── BEFORE ────────────────────────────────────────────
#[deprecated]
pub fn from_attributes(attrs: u32) -> Self {
    let mut flags = 0_u32;
    if attrs & 0x0001 != 0 { flags |= Self::IS_READONLY; }
    // ... same shuffling ...
    Self { flags, ..Default::default() }
}

// ─── AFTER ─────────────────────────────────────────────
// Remove #[deprecated], simplify:
pub const fn from_attributes(attrs: u32) -> Self {
    Self { flags: attrs, ..Self::DEFAULT }
}
```

### 6. Accessor methods — NO CHANGE NEEDED

All 17 accessor methods (`is_hidden()`, `is_directory()`, etc.) use
`self.flags & Self::IS_*` internally. Since the constants change but
the methods still reference them by name, **all 17 accessors auto-fix**.

### 7. `set_directory()` — NO CHANGE NEEDED

Uses `Self::IS_DIRECTORY` constant. Auto-fixes.

---

## v8 → v9 Deserializer Compatibility

**File**: `crates/uffs-mft/src/index/storage/deserialize.rs`

When loading an old v8 `.uffs` cache, the `flags` field contains remapped
bits. We need to convert them to raw NTFS on the fly:

```rust
// In the record reading loop, after reading stdinfo.flags:
let flags = read_u32!();

// v8 compatibility: convert remapped flags to raw NTFS
let flags = if version <= 8 {
    v8_flags_to_ntfs(flags)
} else {
    flags
};

/// Convert v8 remapped StandardInfo flags to raw NTFS FILE_ATTRIBUTE_*.
///
/// Input: OLD v8 bit positions (frozen format values — raw hex acceptable).
/// Output: named constants (StandardInfo::IS_*) — NO raw hex.
const fn v8_flags_to_ntfs(old: u32) -> u32 {
    use StandardInfo as S;
    let mut ntfs = 0_u32;
    // v8 bit → named NTFS constant
    if old & (1 << 0)  != 0 { ntfs |= S::IS_READONLY; }
    if old & (1 << 1)  != 0 { ntfs |= S::IS_ARCHIVE; }
    if old & (1 << 2)  != 0 { ntfs |= S::IS_SYSTEM; }
    if old & (1 << 3)  != 0 { ntfs |= S::IS_HIDDEN; }
    if old & (1 << 4)  != 0 { ntfs |= S::IS_OFFLINE; }
    if old & (1 << 5)  != 0 { ntfs |= S::IS_NOT_INDEXED; }
    if old & (1 << 6)  != 0 { ntfs |= S::IS_NO_SCRUB_DATA; }
    if old & (1 << 7)  != 0 { ntfs |= S::IS_INTEGRITY_STREAM; }
    if old & (1 << 8)  != 0 { ntfs |= S::IS_PINNED; }
    if old & (1 << 9)  != 0 { ntfs |= S::IS_UNPINNED; }
    if old & (1 << 10) != 0 { ntfs |= S::IS_DIRECTORY; }
    if old & (1 << 11) != 0 { ntfs |= S::IS_COMPRESSED; }
    if old & (1 << 12) != 0 { ntfs |= S::IS_ENCRYPTED; }
    if old & (1 << 13) != 0 { ntfs |= S::IS_SPARSE; }
    if old & (1 << 14) != 0 { ntfs |= S::IS_REPARSE; }
    if old & (1 << 15) != 0 { ntfs |= S::IS_TEMPORARY; }
    if old & (1 << 16) != 0 { ntfs |= S::IS_VIRTUAL; }
    // Preserve DELETED_FLAG (bit 31) — internal USN marker
    if old & 0x8000_0000 != 0 { ntfs |= 0x8000_0000; }
    ntfs
}
```

**File**: `crates/uffs-mft/src/index/storage/header.rs`

```rust
// Bump version:
const INDEX_VERSION: u32 = 9;
```

---

## Direct Bit Manipulation Audit

Files that directly manipulate `stdinfo.flags` (not through accessors):

| File | Line | Code | Affected? |
|------|------|------|-----------|
| `standard_info.rs` | 451 | `self.flags \|= Self::IS_DIRECTORY` | ✅ Auto-fixed (uses constant) |
| `standard_info.rs` | 453 | `self.flags &= !Self::IS_DIRECTORY` | ✅ Auto-fixed (uses constant) |
| `tests_extensions.rs` | 478 | `rec.stdinfo.flags \|= StandardInfo::IS_HIDDEN` | ✅ Auto-fixed (uses constant) |
| `tests_extensions.rs` | 481 | `rec.stdinfo.flags \|= StandardInfo::IS_SYSTEM` | ✅ Auto-fixed (uses constant) |
| `usn.rs` | 59 | `record.stdinfo.flags \|= DELETED_FLAG` | ✅ Unaffected (bit 31, custom) |
| `usn.rs` | 125 | `record.stdinfo.flags &= !DELETED_FLAG` | ✅ Unaffected (bit 31, custom) |

**Other `.flags` references** (NOT `stdinfo.flags` — different structs):

| File | Struct | Purpose | Affected? |
|------|--------|---------|-----------|
| `builder.rs:164,168` | `first_stream.flags` | Stream flags (sparse/resident) | ❌ Not `StandardInfo` |
| `types.rs:232,238` | `IndexStreamInfo.flags` | Stream flags | ❌ Not `StandardInfo` |
| `io/parser/index.rs:274` | `attr_header.flags` | NTFS attribute header | ❌ Not `StandardInfo` |
| `io/parser/index_extension.rs:170` | `attr_header.flags` | NTFS attribute header | ❌ Not `StandardInfo` |
| `ntfs/metadata.rs:223` | `IndexRoot.flags` | Index root flags | ❌ Not `StandardInfo` |
| `ntfs/records.rs:401,407` | `FileRecordSegmentHeader.flags` | MFT record header | ❌ Not `StandardInfo` |
| `parse/attribute_helpers.rs:188` | `header.flags` | Attribute header | ❌ Not `StandardInfo` |
| `raw/mod.rs:89` | `RawMftHeader.flags` | Raw MFT header | ❌ Not `StandardInfo` |

**Result: ZERO manual fixes needed** outside `standard_info.rs`. All
`stdinfo.flags` manipulation uses `IS_*` constants or accessor methods.

---

## `FileFlags` Alignment Verification

`FileFlags` in `flags.rs` already uses raw NTFS layout:

| Flag | `FileFlags` | `StandardInfo` (after) | Match? |
|------|-------------|----------------------|--------|
| READONLY | 0x0001 | 0x0001 | ✅ |
| HIDDEN | 0x0002 | 0x0002 | ✅ |
| SYSTEM | 0x0004 | 0x0004 | ✅ |
| DIRECTORY | 0x0010 | 0x0010 | ✅ |
| ARCHIVE | 0x0020 | 0x0020 | ✅ |
| SPARSE | 0x0200 | 0x0200 | ✅ |
| REPARSE | 0x0400 | 0x0400 | ✅ |
| COMPRESSED | 0x0800 | 0x0800 | ✅ |
| OFFLINE | 0x1000 | 0x1000 | ✅ |
| NOT_INDEXED | 0x2000 | 0x2000 | ✅ |
| ENCRYPTED | 0x4000 | 0x4000 | ✅ |

After the refactor, `FileFlags` and `StandardInfo` will be **perfectly
aligned** — a `StandardInfo.flags` value can be directly cast to `FileFlags`
without conversion.

---

## Key Insight: Most Changes Are Automatic

Since all consumers use accessor methods (`is_hidden()`, `is_directory()`,
`set_directory()`, etc.) and these methods use the `IS_*` constants
internally, **changing the constants propagates automatically**.

The real work is:
1. Update 17 constant values in `standard_info.rs` (~17 lines)
2. Simplify `from_raw_ntfs_flags()` → `Self { flags: attrs, .. }` (~1 line)
3. Simplify `to_attributes()` → `self.flags` (~1 line)
4. Simplify `from_attributes()` → `Self { flags: attrs, .. }` (~1 line)
5. Bump `INDEX_VERSION` to 9 (~1 line)
6. Add `v8_flags_to_ntfs()` in `deserialize.rs` (~20 lines)
7. TUI: `compact.rs` change `to_attributes()` → `flags` directly (~1 line)
8. Verify with `cargo test --workspace` + `cargo clippy --workspace`

**Estimated effort**: 1-2 hours. Zero manual fixes outside `standard_info.rs`
and `deserialize.rs`.

---

## Migration Checklist

- [ ] Phase 1: Change 17 constants in `standard_info.rs` to raw NTFS values
- [ ] Phase 1: Simplify `from_raw_ntfs_flags()` → store attrs directly
- [ ] Phase 1: Simplify `to_attributes()` → return `self.flags`
- [ ] Phase 1: Simplify/undeprecate `from_attributes()` → store directly
- [ ] Phase 2: Verify parsers compile (auto-fixed via constants)
- [ ] Phase 3: Bump `INDEX_VERSION = 9` in `header.rs`
- [ ] Phase 3: Add `v8_flags_to_ntfs()` conversion in `deserialize.rs`
- [ ] Phase 4: Verify all consumers compile (auto-fixed via accessors)
- [ ] Phase 5: Verify CLI compiles + tests pass
- [ ] Phase 6: TUI `compact.rs` — use `stdinfo.flags` directly, remove `to_attributes()`
- [ ] Phase 7: `cargo test --workspace` — all tests pass
- [ ] Phase 7: `cargo clippy --workspace` — zero warnings
- [ ] Phase 7: Parity check — CLI CSV output matches before/after
- [ ] Phase 7: TUI manual test — `is_directory()`, F3 filter, `\documents\*` tree search
