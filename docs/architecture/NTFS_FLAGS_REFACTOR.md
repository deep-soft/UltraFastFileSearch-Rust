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

## Key Insight: Most Changes Are Automatic

Since all consumers use accessor methods (`is_hidden()`, `is_directory()`,
`set_directory()`, etc.) and these methods use the `IS_*` constants
internally, **changing the constants propagates automatically**.

The real work is:
1. Update 17 constant values (~17 lines)
2. Simplify `from_extended()` (~30 lines)
3. Simplify `to_attributes()` (~1 line: return `self.flags`)
4. Simplify `from_attributes()` (~1 line: store directly)
5. Add v8→v9 compat in `deserialize()` (~20 lines)
6. Update the TUI `compact.rs` to remove `to_attributes()` call (~1 line)
7. Verify everything

**Estimated effort**: 1-2 hours. Most files need zero manual changes.

---

## Migration Checklist

- [ ] Phase 1: Change constants + simplify conversion methods
- [ ] Phase 2: Verify parsers (should auto-fix via constants)
- [ ] Phase 3: Bump `.uffs` version, add v8 compat in deserializer
- [ ] Phase 4: Verify all consumers (should auto-fix via accessors)
- [ ] Phase 5: Verify CLI (should auto-fix via accessors)
- [ ] Phase 6: Simplify TUI `compact.rs`
- [ ] Phase 7: Full test suite + parity check
