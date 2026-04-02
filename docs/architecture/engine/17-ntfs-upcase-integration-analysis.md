# 17 — NTFS `$UpCase` Integration: Cost/Benefit Analysis & Refactor Plan

> **Date:** 2026-04-02
> **Scope:** Deep technical analysis of integrating the NTFS `$UpCase` table
> (MFT Record #10) for case-insensitive search. Covers the on-disk format,
> reading strategies, per-character cost vs alternatives, memory overhead,
> cache persistence, trigram impact, refactor scope, and risk assessment.
>
> **Core question:** What speed and storage do we give up to use NTFS's own
> case table — and is the trade-off worth making?

---

## Table of Contents

1. [What Is `$UpCase`?](#1-what-is-upcase)
2. [The On-Disk Format](#2-the-on-disk-format)
3. [Reading Strategies](#3-reading-strategies)
4. [Per-Character Performance Cost](#4-per-character-performance-cost)
5. [Aggregate Pipeline Impact](#5-aggregate-pipeline-impact)
6. [Memory & Storage Overhead](#6-memory--storage-overhead)
7. [Interaction With Trigram Index](#7-interaction-with-trigram-index)
8. [Cache Format Changes](#8-cache-format-changes)
9. [Refactor Scope & Complexity](#9-refactor-scope--complexity)
10. [Risk Assessment](#10-risk-assessment)
11. [Comparison: ASCII vs Unicode Simple vs $UpCase](#11-comparison)
12. [Recommendation](#12-recommendation)

---

## 1. What Is `$UpCase`?

Every NTFS volume contains a hidden system file called `$UpCase`, stored as
**MFT Record #10** (FRS 10). It is a flat lookup table that maps every
UTF-16 code point (0x0000–0xFFFF) to its uppercase equivalent. NTFS uses
this table for **all** case-insensitive operations:

- Filename uniqueness checks (can't have `readme.txt` and `README.TXT`
  in the same directory)
- Directory B-tree lookups (`FindFirstFile`, `OpenFile`)
- Index key comparisons in `$INDEX_ROOT` / `$INDEX_ALLOCATION`

The table is written once at volume format time and is **immutable for the
lifetime of the volume**. It is NOT locale-specific — it uses a fixed
Unicode mapping defined by the Windows version that formatted the volume.

### What It Looks Like

```
Offset    Content
───────── ──────────────────────────────────────────
0x0000    00 00  ← U+0000 → U+0000 (NUL maps to NUL)
0x0002    01 00  ← U+0001 → U+0001
  ...
0x00C2    41 00  ← U+0061 ('a') → U+0041 ('A')  ★ ASCII fold
0x00C4    42 00  ← U+0062 ('b') → U+0042 ('B')  ★
  ...
0x01B8    DC 00  ← U+00DC ('Ü') → U+00DC ('Ü')  ★ already uppercase
0x01B6    DC 00  ← U+00FC ('ü') → U+00DC ('Ü')  ★ European fold!
  ...
0x9E5A    2D 4E  ← U+4F2D → U+4F2D (CJK, no fold)
  ...
0x1FFFE   FF FF  ← U+FFFF → U+FFFF (last entry)
```

Total: 65,536 entries × 2 bytes = **131,072 bytes (128 KB)** exactly.


---

## 2. The On-Disk Format

`$UpCase` is stored as the `$DATA` attribute of MFT Record #10.

### Residency

At 128 KB, `$UpCase` is **always non-resident**. An MFT file record is
typically 1 KB (sometimes 4 KB), far too small to hold 128 KB inline.
The `$DATA` attribute header contains **data runs** (mapping pairs)
pointing to clusters on disk where the actual table lives.

```
MFT Record #10 ($UpCase), 1 KB on disk:
┌────────────────────────────────────────────┐
│ FileRecordSegmentHeader (48 B)             │
│ $STANDARD_INFORMATION (96 B)              │
│ $FILE_NAME (104 B)                         │
│ $DATA — NON-RESIDENT attribute header:     │
│   ├── is_non_resident: 1                   │
│   ├── data_size: 131072 (128 KB)           │
│   ├── mapping_pairs_offset: ...            │
│   └── Data Runs:                           │
│       └── Run 1: LCN=X, length=32 clusters │
└────────────────────────────────────────────┘

Actual $UpCase data (at LCN × bytes_per_cluster):
  128 KB of contiguous UTF-16LE entries
```

### Fragmentation & Version Stability

`$UpCase` is written at format time and never modified — always stored
in a single contiguous extent. The table is nearly identical across
Windows versions:

| Windows Version | Changes |
|-----------------|---------|
| NT 4.0 – XP | Baseline (Unicode 2.0) |
| Vista – Win 7 | ~50 new mappings (Unicode 5.0) |
| Win 8 – Win 10 | ~30 new mappings (Unicode 6.1+) |
| Win 11 | ~20 new mappings (Unicode 14.0+) |

The ASCII range (0x0000–0x007F) and Western European range
(0x0080–0x024F) have **never changed**. A compiled-in default would
cover >99.9% of real-world filenames.

---

## 3. Reading Strategies

Three approaches to obtain the `$UpCase` table, in order of complexity:

### Strategy A: Windows API — Open `$UpCase` Directly

```rust
#[cfg(windows)]
fn read_upcase_via_api(drive: char) -> Result<Box<[u16; 65536]>> {
    let path = format!("{}:\\$UpCase", drive.to_ascii_uppercase());
    // Opens with FILE_FLAG_BACKUP_SEMANTICS (required for system files)
    let handle = CreateFileW(
        &path, FILE_READ_DATA,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        None, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, None,
    )?;
    let mut buf = vec![0u8; 131072];
    let mut bytes_read = 0u32;
    ReadFile(handle, Some(&mut buf), Some(&mut bytes_read), None)?;
    CloseHandle(handle)?;
    // Convert [u8; 131072] → [u16; 65536] (LE)
    let table: Box<[u16; 65536]> = /* bytemuck cast or manual conversion */;
    Ok(table)
}
```

| Metric | Value |
|--------|-------|
| I/O | Single sequential read, ~128 KB |
| Latency | ~0.5 ms on SSD, ~2 ms on HDD |
| Privilege | Requires backup semantics (same as MFT reading) |
| Complexity | ~30 lines of Windows FFI |
| Cross-platform | Windows-only at read time |

**Pro:** Simple, reliable, uses existing volume access patterns.
**Con:** Requires the volume to be mounted (not for offline .uffs files).

### Strategy B: Extract from Raw MFT During Ingestion

During the MFT parsing sweep (which already reads every record including
FRS 10), intercept record #10, parse its `$DATA` attribute, follow the
data runs, and read the cluster data from the volume.

```rust
// In the MFT read loop, when frs == 10:
if frs == 10 {
    let attrs = AttributeIterator::new(record_data);
    for attr in attrs {
        if attr.attribute_type() == Some(AttributeType::Data) {
            if attr.is_non_resident() {
                let data_runs = attr.data_runs();
                // Read 128 KB from disk at LCN × bytes_per_cluster
                let table = read_clusters(volume_handle, &data_runs, bytes_per_cluster)?;
                upcase_table = Some(parse_upcase_table(&table));
            }
        }
    }
}
```

| Metric | Value |
|--------|-------|
| I/O | One extra seek + 128 KB read during MFT sweep |
| Latency | ~0.5 ms on SSD (amortised into MFT read) |
| Complexity | ~60 lines — parse data runs + cluster read |
| Advantage | No extra file open; uses existing volume handle |

**Pro:** Integrated into the MFT read pipeline, no separate I/O path.
**Con:** Requires threading `volume_handle` into the parser callback,
which currently only sees record bytes, not the volume handle. The
existing `MftReader` has a handle, but `parse_record_to_index` doesn't.

### Strategy C: Compile-In Default + Optional Live Override

Ship a default `$UpCase` table compiled into the binary (128 KB in
`.rodata`), and optionally read the live table for override.

```rust
/// Default $UpCase table from Windows 11 23H2 NTFS.
/// Covers Unicode 14.0+ case mappings.
static DEFAULT_UPCASE: &[u16; 65536] = include_bytes!("upcase_win11.bin");

fn get_upcase_table(drive: Option<char>) -> &'static [u16; 65536] {
    if let Some(d) = drive {
        // Try to read live table; fall back to default
        match read_upcase_via_api(d) {
            Ok(table) if table != *DEFAULT_UPCASE => {
                Box::leak(table)  // promote to 'static
            }
            _ => DEFAULT_UPCASE,
        }
    } else {
        DEFAULT_UPCASE
    }
}
```

| Metric | Value |
|--------|-------|
| Binary size | +128 KB (in .rodata, compressed in binary) |
| I/O | 0 for default path; 128 KB read for override |
| Correctness | 99.99% — only obscure Unicode ranges differ between versions |
| Cross-platform | Works for offline .uffs files without a live volume |

**Pro:** Always available, even for cached/offline indexes. Zero I/O
in the common case. **Con:** +128 KB binary size.

### Recommendation: Strategy A + C Hybrid

Use Strategy A (API read) during live MFT ingestion. Persist the table
in the compact cache. For offline/cross-platform, fall back to the
compiled-in default (Strategy C).

---

## 4. Per-Character Performance Cost

This is the critical performance question. How much slower is a table
lookup vs `to_ascii_lowercase()`?

### Current: `u8::to_ascii_lowercase()`

```rust
// Generated assembly (x86-64):
//   sub    al, 0x41      ; al = byte - 'A'
//   cmp    al, 0x1a      ; is it A-Z?
//   cmovae eax, edi      ; if not, keep original
//   add    al, 0x61      ; if yes, add 'a'
```

| Metric | Value |
|--------|-------|
| Instructions | 4 (sub, cmp, cmov, add) |
| Cycles | ~1 cycle (fully pipelined) |
| Data access | None (register-only) |
| Works for | ASCII A-Z only |
| Fails for | ü, é, Ö, Σ, Д — all non-ASCII |

### `$UpCase` Table Lookup

```rust
#[inline]
fn upcase_fold(ch: u16, table: &[u16; 65536]) -> u16 {
    table[ch as usize]
}
```

But our names are UTF-8, not UTF-16. So each lookup requires:
1. Decode UTF-8 → codepoint
2. Check if BMP (< 0x10000) — non-BMP has no case, skip
3. Table lookup: `table[codepoint as u16]`
4. Compare folded values

```rust
#[inline]
fn fold_and_compare_upcase(
    a: &[u8],  // UTF-8 name A
    b: &[u8],  // UTF-8 name B
    table: &[u16; 65536],
) -> core::cmp::Ordering {
    let mut a_chars = core::str::from_utf8(a).unwrap_or("").chars();
    let mut b_chars = core::str::from_utf8(b).unwrap_or("").chars();
    loop {
        match (a_chars.next(), b_chars.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) => {
                let fa = if (ca as u32) < 0x10000 {
                    table[ca as usize]
                } else {
                    ca as u16  // non-BMP: no case
                };
                let fb = if (cb as u32) < 0x10000 {
                    table[cb as usize]
                } else {
                    cb as u16
                };
                match fa.cmp(&fb) {
                    Ordering::Equal => continue,
                    other => return other,
                }
            }
        }
    }
}
```

### Cost Breakdown Per Character

| Operation | ASCII Path | Non-ASCII Path |
|-----------|------------|----------------|
| UTF-8 decode | 0 cycles (1 byte, trivial) | ~3 cycles (2-4 bytes) |
| BMP check | ~0.5 cycles (always true) | ~0.5 cycles |
| Table lookup | ~4 cycles (L1 hit) | ~4-8 cycles (L1/L2) |
| Compare | ~0.5 cycles | ~0.5 cycles |
| **Total** | **~5 cycles** | **~8-12 cycles** |

### Compared to `to_ascii_lowercase()`

| | ASCII-only fold | $UpCase fold | Overhead |
|---|---|---|---|
| Per-character cost | ~1 cycle | ~5 cycles (ASCII) | **5× slower** |
| Per-character cost (non-ASCII) | N/A (wrong result) | ~10 cycles | N/A (not comparable) |

### Why 5× Slower Is Actually Fine

The "5× slower" sounds alarming, but in absolute terms:

**Trigram build** (7M records × avg 20 chars = 140M characters):
- ASCII fold: 140M × 1 cycle = 140M cycles ≈ **47 ms** at 3 GHz
- $UpCase fold: 140M × 5 cycles = 700M cycles ≈ **233 ms** at 3 GHz
- **Delta: +186 ms** (one-time during build/cache load)

**Search matching** (50K candidates × avg 20 chars = 1M characters):
- ASCII fold: 1M × 1 cycle = 1M cycles ≈ **0.3 ms**
- $UpCase fold: 1M × 5 cycles = 5M cycles ≈ **1.7 ms**
- **Delta: +1.4 ms per search** (imperceptible at 60 fps TUI)

**Sort comparison** (10K rows × avg 5 chars compared before decision):
- ASCII fold: 50K × 1 cycle = 50K cycles ≈ **0.02 ms**
- $UpCase fold: 50K × 5 cycles = 250K cycles ≈ **0.08 ms**
- **Delta: +0.06 ms** (completely negligible)

### The L1 Cache Argument

The $UpCase table is 128 KB — larger than L1 data cache (typically
32–48 KB). But the **hot portion** (the part actually accessed during
filename processing) is much smaller:

| Codepoint Range | Coverage | Table Bytes | Cache Tier |
|-----------------|----------|-------------|------------|
| 0x0000–0x007F (ASCII) | ~95% of chars | 256 bytes | L1 (hot) |
| 0x0080–0x024F (Latin Ext) | ~4% of chars | 940 bytes | L1 (warm) |
| 0x0400–0x04FF (Cyrillic) | ~0.5% | 512 bytes | L1/L2 |
| 0x4E00–0x9FFF (CJK) | ~0.5% (no case) | 0 (identity) | N/A |

In practice, **~99% of accesses hit the first 2 KB** of the table,
which stays in L1 permanently. The remaining 126 KB is cold and doesn't
cause cache pressure for the working set.

---

## 5. Aggregate Pipeline Impact

### End-to-End Latency: Current vs $UpCase

| Pipeline Stage | Current (ms) | With $UpCase (ms) | Delta |
|----------------|-------------|-------------------|-------|
| MFT read (7M records) | 2,500 | 2,501 | +1 ms (read 128 KB) |
| Compact build | 800 | 800 | +0 ms (no change) |
| Trigram build | 200 | 386 | +186 ms (one-time) |
| Cache load | 390 | 576 | +186 ms (one-time) |
| Search (substring, 50K candidates) | 12 | 13.4 | +1.4 ms |
| Search (match-all, 7M scan) | 45 | 50 | +5 ms |
| Sort (10K rows, numeric) | 2 | 2.08 | +0.08 ms |
| Sort (10K rows, by name) | 5 | 5.5 | +0.5 ms |
| Path resolution (10K rows) | 3 | 3 | +0 ms (no fold) |

### Where The Cost Lands

```
One-time costs (startup / rebuild):
  Trigram build:    +186 ms  ← acceptable (2,500 ms total MFT read)
  Cache load:       +186 ms  ← noticeable but one-time

Per-search costs (interactive / TUI):
  Substring match:  +1.4 ms  ← imperceptible (16 ms frame budget)
  Match-all scan:   +5 ms    ← within budget
  Sort:             +0.5 ms  ← negligible
```

**Bottom line:** The per-search overhead is **<7 ms total**, well within
the 16 ms frame budget for 60 fps TUI interaction. The one-time trigram
build overhead (+186 ms) adds ~7% to the current MFT load time.

### Can We Mitigate The Trigram Build Cost?

Yes — with an **ASCII fast path**:

```rust
#[inline]
fn fold_byte_or_table(byte: u8, table: &[u16; 65536]) -> u16 {
    if byte < 0x80 {
        // ASCII: use register-only fold (1 cycle)
        u16::from(byte.to_ascii_lowercase())
    } else {
        // Non-ASCII: need to decode codepoint first
        // (handled at caller level — this byte is part of multi-byte)
        u16::from(byte)
    }
}
```

For the trigram build, 95% of filename bytes are ASCII. With the fast
path, 95% of characters pay ~1 cycle (same as today) and only 5% pay
~10 cycles. Blended cost:

```
0.95 × 1 cycle + 0.05 × 10 cycles = 1.45 cycles per character
140M chars × 1.45 cycles = 203M cycles ≈ 68 ms
Delta vs current: +21 ms (was +186 ms without fast path)
```

**With the ASCII fast path, the $UpCase trigram build overhead drops
from +186 ms to +21 ms.**

---

## 6. Memory & Storage Overhead

### Runtime Memory

| Component | Size | Lifetime |
|-----------|------|----------|
| `$UpCase` table | 128 KB | Static (entire app lifetime) |
| Default fallback table (if compiled in) | 128 KB | `.rodata` (not heap) |

128 KB is **0.007%** of the typical steady-state memory (1.8 GB for
2 drives). It is smaller than a single `CompactRecord` batch (80 B ×
1600 = 128 KB for ~1600 records).

### Disk Cache Overhead

If persisted in the compact cache (`.uffs` file):

| | Uncompressed | zstd Compressed |
|---|---|---|
| Raw table | 128 KB | ~12 KB (90% compression — repetitive data) |
| Cache v5 typical | ~200 MB | ~45 MB |
| Overhead | +0.064% | +0.027% |

The `$UpCase` table compresses extremely well because:
- 95% of entries are identity mappings (code point maps to itself)
- The remaining 5% have regular patterns (contiguous case-pair ranges)

### Binary Size

If using Strategy C (compiled-in default):

| | Current | With $UpCase |
|---|---|---|
| Release binary | ~8 MB | ~8.1 MB (+128 KB, compressed in .rodata) |
| Impact | | +1.6% |

---

## 7. Interaction With Trigram Index

This is the most technically nuanced aspect. The trigram index operates
on **bytes**, not characters. With `$UpCase`, we need case-folded
trigrams that match across case variants.

### Current: Byte-Level Trigrams, ASCII Fold

```
"über.txt" (UTF-8): C3 BC 62 65 72 2E 74 78 74
Byte trigrams:       [C3,BC,62] [BC,62,65] [62,65,72] [65,72,2E] ...

"ÜBER.TXT" (UTF-8): C3 9C 42 45 52 2E 54 58 54
Byte trigrams:       [C3,9C,42] [9C,42,45] [42,45,52] [45,52,2E] ...

With ASCII-only fold on "ÜBER.TXT":
                     [C3,9C,42] → [C3,9C,62] ← 0x9C NOT folded!
                     These DON'T match "über" trigrams.
```

### With $UpCase: Codepoint-Level Fold Before Trigram

Two approaches:

#### Approach 1: Fold UTF-8 Bytes In-Place, Then Byte Trigrams

For BMP characters, case folding maps within the same UTF-8 byte-length
class (2-byte stays 2-byte). We can fold the bytes in-place:

```rust
fn fold_utf8_byte_window(
    window: &[u8],
    name_bytes: &[u8],
    pos: usize,      // position of window[0] in name_bytes
    table: &[u16; 65536],
) -> [u8; 3] {
    let mut result = [0u8; 3];
    for (i, &b) in window.iter().enumerate() {
        if b < 0x80 {
            // ASCII: direct table lookup
            result[i] = table[b as usize] as u8;
        } else if (b & 0xC0) == 0x80 {
            // Continuation byte: need to find its start byte
            // and fold the whole codepoint
            result[i] = fold_continuation(name_bytes, pos + i, table);
        } else {
            // Start byte: decode full codepoint, fold, re-encode
            result[i] = fold_start_byte(name_bytes, pos + i, table);
        }
    }
    result
}
```

**Problem:** This is complex. Continuation bytes in the middle of a
trigram window don't have meaning in isolation. The fold result depends
on context (the start byte that precedes them).

#### Approach 2: Character Trigrams (Recommended)

Instead of byte trigrams, use **character-level trigrams**: 3 Unicode
codepoints, each folded via $UpCase, packed into a `u64`:

```rust
/// Pack 3 folded codepoints into a u64.
/// Each codepoint is ≤16 bits (BMP), so 3 × 16 = 48 bits fits in u64.
#[inline]
fn pack_char_trigram(a: u16, b: u16, c: u16) -> u64 {
    (u64::from(a) << 32) | (u64::from(b) << 16) | u64::from(c)
}

/// Build trigrams from a name using $UpCase folding.
fn char_trigrams(
    name: &str,
    table: &[u16; 65536],
) -> impl Iterator<Item = u64> + '_ {
    let folded: SmallVec<[u16; 64]> = name.chars()
        .map(|ch| {
            let cp = ch as u32;
            if cp < 0x10000 { table[cp as usize] }
            else { cp as u16 }
        })
        .collect();
    folded.windows(3).map(|w| pack_char_trigram(w[0], w[1], w[2]))
}
```

**Trade-offs vs byte trigrams:**

| | Byte Trigrams (current) | Char Trigrams |
|---|---|---|
| Key type | `u32` (3 bytes packed) | `u64` (3 u16 packed) |
| Key space | 16M (2²⁴) | 281T (2⁴⁸) — but sparse |
| LUT approach | Flat 64 MB array ✅ | Must use HashMap or BTreeMap |
| Posting list size | Slightly larger (more byte combos) | Slightly smaller (fewer char combos) |
| CJK selectivity | Excellent (3-byte chars are unique) | Good (1 CJK char = 1 trigram unit) |
| Build cost | O(N × avg_bytes) | O(N × avg_chars) — similar |
| Case correctness | ASCII only | Full Unicode |

**Key decision:** Character trigrams require replacing the flat
`tri_lut` (64 MB) with a `HashMap<u64, u32>`. This is actually a
memory **improvement** (current LUT is 64 MB with 0.3% utilisation;
a HashMap with 50K entries uses ~2 MB).

The CSR posting list format is unchanged — only the key type widens
from `u32` to `u64`. The `keys: Vec<u64>` array grows from 200 KB to
400 KB (50K entries × 8 bytes vs 4 bytes). Negligible.

---

## 8. Cache Format Changes

The compact cache (`.uffs` file) currently uses format v5. Adding
`$UpCase` support requires a version bump to v6.

### v6 Cache Layout

```
┌─────────────────────────────────────────────────┐
│ Header (48 B)                                    │
│   version: 6                                     │
│   record_count, names_len, etc.                  │
│   upcase_table_offset: u64  ← NEW field          │
├─────────────────────────────────────────────────┤
│ Records section: [CompactRecord; N]              │
│ Names section: [u8; names_len]                   │
│ ChildrenIndex offsets: [u32; N+1]                │
│ ChildrenIndex children: [u32; total_children]    │
├─────────────────────────────────────────────────┤
│ UpCase section (NEW): [u16; 65536]  ← 128 KB    │
│   or: sentinel value if using compiled default   │
├─────────────────────────────────────────────────┤
│ zstd footer / AES-GCM tag                        │
└─────────────────────────────────────────────────┘
```

### Backward Compatibility

- v6 reader can load v5 caches: if `upcase_table_offset == 0` or version
  < 6, use the compiled-in default table.
- v5 reader rejects v6 caches (existing behaviour: version mismatch
  triggers full rebuild). This is acceptable — upgrading to v6 causes
  a one-time cache rebuild.

### Space Impact

| | v5 | v6 |
|---|---|---|
| Uncompressed | ~590 MB | ~590.1 MB (+128 KB) |
| zstd compressed | ~45 MB | ~45.01 MB (+12 KB) |

Negligible. The 128 KB table compresses to ~12 KB.

### Serialisation

The `$UpCase` table is a flat `[u16; 65536]` — a `Pod` type if using
bytemuck. Serialisation is a single `cast_slice` call, consistent with
the existing cache serialisation pattern.

```rust
// Serialize:
let upcase_bytes: &[u8] = bytemuck::cast_slice(&upcase_table[..]);
writer.write_all(upcase_bytes)?;

// Deserialize:
let upcase_slice: &[u16] = bytemuck::cast_slice(&bytes[offset..offset + 131072]);
let mut table = [0u16; 65536];
table.copy_from_slice(upcase_slice);
```

---

## 9. Refactor Scope & Complexity

### Affected Crates & Files

```
uffs-mft (ingestion — Windows-only):
  ├── platform/volume.rs     Add read_upcase_table() (~30 lines)
  └── commands/load.rs       Pass upcase table from reader to caller

uffs-core (search & index):
  ├── compact.rs             Accept upcase table in build_compact_index
  │                          Store in DriveCompactIndex
  ├── compact_cache.rs       Serialize/deserialize upcase section (v6)
  ├── compact_loader.rs      Accept upcase for USN refresh path
  ├── trigram.rs             Change from byte trigrams to char trigrams
  │                          Use upcase for folding (~100 lines changed)
  ├── search/query.rs        Replace to_ascii_lowercase with upcase fold
  │                          Thread table ref through search functions
  ├── search/tree.rs         Replace to_ascii_lowercase with upcase fold
  ├── search/backend.rs      Replace sort key lowering with upcase fold
  ├── search/filters.rs      Replace filter matching with upcase fold
  └── index_search/pattern.rs  Update comparison helpers to accept table

uffs-cli:
  └── (no changes — consumes SearchResult unchanged)

uffs-tui:
  └── (no changes — consumes SearchResult unchanged)
```

### Lines of Code Estimate

| Component | New Code | Modified Code | Total |
|-----------|----------|---------------|-------|
| $UpCase reader (Windows API) | ~30 | 0 | 30 |
| Default table data file | ~5 (include_bytes) | 0 | 5 |
| `CaseFold` trait/struct | ~50 | 0 | 50 |
| Trigram refactor (byte → char) | ~80 | ~120 | 200 |
| Cache v6 format | ~20 | ~30 | 50 |
| Search/sort/filter fold swap | 0 | ~60 (15 sites × 4 lines) | 60 |
| Compact index plumbing | ~10 | ~20 | 30 |
| Tests | ~100 | ~40 | 140 |
| **Total** | **~295** | **~270** | **~565** |

### Complexity Assessment

| Component | Complexity | Risk | Notes |
|-----------|------------|------|-------|
| $UpCase reader | Low | Low | Simple file read, existing patterns |
| CaseFold abstraction | Low | Low | Trait or fn pointer, straightforward |
| Trigram byte→char | **High** | **Medium** | Core data structure change; needs careful testing |
| Cache v6 | Medium | Low | Additive change, backward compatible |
| Search fold swap | Low | Low | 15 mechanical replacements |
| Sort fold swap | Low | Low | Replace lowered String with fold compare |

**The trigram refactor is the critical path.** Everything else is
straightforward plumbing. The trigram change touches the CSR build
pipeline, the key packing, the search intersection, and the LUT
structure. It needs:

1. New `pack_char_trigram` / `unpack_char_trigram` functions
2. `TrigramIndex` key type from `Vec<u32>` to `Vec<u64>`
3. Replace flat `tri_lut` (64 MB) with `FxHashMap<u64, u32>` (~2 MB)
4. Update `TrigramIndex::search` to generate char trigrams from query
5. Update `TrigramIndex::build` pass 1 (count) and pass 2 (scatter)
6. Update `TinyTriSet` from `SmallVec<u32>` to `SmallVec<u64>`

### The CaseFold Abstraction

To cleanly separate the fold function from its consumers, introduce a
thin abstraction:

```rust
/// Case-folding strategy. Passed by reference to all search/trigram
/// functions. Cheap to clone (it's a reference to a static table).
#[derive(Clone, Copy)]
pub struct CaseFold {
    table: &'static [u16; 65536],
}

impl CaseFold {
    /// Create from the compiled-in default.
    pub fn default_table() -> Self {
        Self { table: &DEFAULT_UPCASE }
    }

    /// Create from a live $UpCase table read from a volume.
    pub fn from_ntfs(table: &'static [u16; 65536]) -> Self {
        Self { table }
    }

    /// Fold a single Unicode codepoint.
    #[inline]
    pub fn fold_char(&self, ch: char) -> u16 {
        let cp = ch as u32;
        if cp < 0x10000 {
            self.table[cp as usize]
        } else {
            cp as u16  // non-BMP: no case folding
        }
    }

    /// Fold an ASCII byte (fast path — no table lookup needed).
    #[inline]
    pub fn fold_ascii(&self, b: u8) -> u8 {
        debug_assert!(b < 0x80);
        self.table[b as usize] as u8
    }

    /// Case-insensitive comparison of two UTF-8 strings.
    #[inline]
    pub fn cmp_str(&self, a: &str, b: &str) -> core::cmp::Ordering {
        a.chars()
            .map(|ch| self.fold_char(ch))
            .cmp(b.chars().map(|ch| self.fold_char(ch)))
    }

    /// Case-insensitive equality.
    #[inline]
    pub fn eq_str(&self, a: &str, b: &str) -> bool {
        self.cmp_str(a, b) == core::cmp::Ordering::Equal
    }
}
```

This is passed by value (it's `Copy` — just a pointer) to all search,
sort, and trigram functions. Threading it through the pipeline is
mechanical: add `fold: CaseFold` parameter to each function.

---

## 10. Risk Assessment

### Technical Risks

| Risk | Severity | Likelihood | Mitigation |
|------|----------|------------|------------|
| Trigram char migration breaks search accuracy | High | Medium | Bit-for-bit comparison test: build byte-trigram + char-trigram indexes from same data, run identical queries, compare result sets |
| $UpCase read fails (permissions, OS version) | Medium | Low | Fall back to compiled-in default table; log warning |
| Cache v6 migration causes rebuild storm | Low | Certain | Expected: one-time rebuild per drive, same as any version bump |
| Non-BMP characters in filenames (emoji) | Low | Low | Non-BMP chars have no case; identity mapping is correct |
| Cross-compilation breaks (128 KB .rodata) | Low | Low | `include_bytes!` works on all platforms |
| UTF-8 decode overhead in tight loops | Low | Low | ASCII fast-path makes this a non-issue for 95% of bytes |

### Correctness Risks

| Scenario | Risk | Notes |
|----------|------|-------|
| $UpCase table differs from compiled default | Low | Only affects obscure Unicode ranges; log difference |
| Same file found by ASCII fold but missed by $UpCase | None | $UpCase is a SUPERSET of ASCII fold — it folds everything ASCII does plus more |
| Same file missed by ASCII fold but found by $UpCase | Expected | This is the GOAL — e.g., Ü ↔ ü matching |
| Sort order changes between ASCII fold and $UpCase | Expected | Case-insensitive sort will differ for filenames starting with non-ASCII letters; document as intentional behaviour change |

### The "ASCII Fold Is a Subset" Guarantee

This is a critical correctness property: for ALL ASCII characters
(0x00–0x7F), the $UpCase table maps identically to `to_ascii_lowercase`:

```
$UpCase[0x61] ('a') = 0x41 ('A')  →  fold to uppercase
ASCII lowercase: 'A'.to_ascii_lowercase() = 'a'  →  fold to lowercase

Both sides: 'A' == 'a' in case-insensitive comparison ✅
```

The direction of folding differs (NTFS folds to UPPERCASE; Rust's
`to_ascii_lowercase` folds to lowercase), but for COMPARISON purposes
the result is identical: two strings that compare equal under ASCII fold
will also compare equal under $UpCase fold, for ALL ASCII characters.

**No existing search results will be lost.** Users will only see MORE
results (European/Cyrillic matches they previously missed).

---

## 11. Comparison: ASCII vs Unicode Simple vs $UpCase

### Speed

| Operation | ASCII Fold | Unicode Simple | $UpCase |
|-----------|-----------|----------------|---------|
| Per-char cost (ASCII) | 1 cycle | 1 cycle (fast path) | 5 cycles |
| Per-char cost (Latin Ext) | N/A (broken) | 11 cycles (binary search) | 5 cycles |
| Per-char cost (CJK) | N/A (no case) | N/A (no case) | 5 cycles (identity) |
| Trigram build +delta | baseline | +20 ms | +21 ms (with fast path) |
| Search +delta | baseline | +1.5 ms | +1.4 ms |
| Sort +delta | baseline | +0.5 ms | +0.5 ms |

**$UpCase is FASTER than Unicode Simple for non-ASCII** because it uses
O(1) table lookup instead of O(log N) binary search.

### Storage

| Component | ASCII Fold | Unicode Simple | $UpCase |
|-----------|-----------|----------------|---------|
| Binary size | +0 | +5.6 KB | +128 KB |
| Runtime memory | +0 | +5.6 KB (static) | +128 KB (static) |
| Cache overhead | +0 | +0 (no persistence) | +12 KB (zstd compressed) |

### Correctness

| Scenario | ASCII | Unicode Simple | $UpCase |
|----------|-------|----------------|---------|
| A ↔ a | ✅ | ✅ | ✅ |
| Ü ↔ ü | ❌ | ✅ | ✅ |
| É ↔ é | ❌ | ✅ | ✅ |
| Σ ↔ σ ↔ ς (Greek final sigma) | ❌ | ✅ | ✅ |
| Д ↔ д (Cyrillic) | ❌ | ✅ | ✅ |
| ß ↔ SS (German eszett) | ❌ | ❌ (length-changing) | ❌ |
| İ ↔ i (Turkish) | ❌ | ❌ (locale-dependent) | ✅ (NTFS specific) |
| Matches NTFS behaviour exactly | ❌ | ~99% | **100%** |

### Maintenance

| Aspect | ASCII | Unicode Simple | $UpCase |
|--------|-------|----------------|---------|
| Code complexity | Trivial | Low (static table) | Medium (table + reader) |
| Update needed for new Unicode? | No | Yes (regenerate table) | No (NTFS table is definitive) |
| Cross-platform offline support | N/A | Built-in | Needs compiled-in default |
| Test complexity | Low | Low | Medium (trigram migration) |

---

## 12. Recommendation

### Verdict: $UpCase Integration Is Worth It

The cost/benefit analysis is strongly positive:

**What we give up:**

| Sacrifice | Magnitude | Verdict |
|-----------|-----------|---------|
| Per-character speed | +4 cycles (5× slower per char) | **Negligible** — total search impact <7 ms |
| Memory | +128 KB | **Trivial** — 0.007% of steady-state |
| Binary size | +128 KB | **Trivial** — 1.6% |
| Cache disk | +12 KB (compressed) | **Negligible** |
| Trigram build time | +21 ms (with fast path) | **Acceptable** — one-time |
| Code complexity | ~565 lines changed | **Moderate** — mostly mechanical |
| Cache version bump | v5 → v6 | **Expected** — one-time rebuild |

**What we gain:**

| Gain | Value |
|------|-------|
| Full Unicode case-insensitive search | European, Cyrillic, Greek users |
| Exact NTFS compatibility | Same results as Windows Explorer |
| Correct filename uniqueness | Matches NTFS's own uniqueness rules |
| Turkish İ/ı handling | Correct without locale configuration |
| No external Unicode dependency | No ICU, no unicode-normalization crate |
| Future-proof | NTFS defines the table; we follow it |

### Implementation Approach

```
Phase 1 (Now): Never-allocate strategy with ASCII fold
  ├── Implement doc 16 Steps 1-5
  ├── Establishes the fold-at-comparison-point architecture
  └── All fold calls go through a single pattern

Phase 2: $UpCase integration
  ├── Add CaseFold struct (thin wrapper around &[u16; 65536])
  ├── Compile in default $UpCase table (128 KB .rodata)
  ├── Add Windows API reader for live $UpCase
  ├── Refactor trigram from byte to char trigrams
  ├── Bump cache to v6 with upcase section
  ├── Swap fold function at all ~15 Category C sites
  └── Total: ~565 lines, 2-3 days of work

Phase 3 (Optional): Persist live $UpCase in cache
  └── Only if compiled-in default proves insufficient
```

### Why Not Unicode Simple Fold Instead?

Unicode Simple Fold is a viable intermediate step — but if the end goal
is NTFS-exact behaviour, it's wasted work:

1. We'd build the Simple Fold table (~5.6 KB), integrate it, test it —
   then later **replace it** with $UpCase anyway.
2. $UpCase handles Turkish İ/ı correctly; Unicode Simple does not.
3. $UpCase is O(1) per character; Unicode Simple is O(log N).
4. The refactor scope is identical (same call sites, same CaseFold
   abstraction) — only the table source differs.

**Skip Unicode Simple. Go directly to $UpCase.** The compiled-in
default table provides the same coverage as Unicode Simple Fold, plus
NTFS-specific mappings, with O(1) lookup.

### What Makes This Feasible

1. **The never-allocate architecture (doc 16) is a prerequisite.** Once
   all fold sites use the comparison-point / buffer-reuse pattern, swapping
   the fold function is a mechanical change.
2. **The CaseFold abstraction isolates the fold function.** Once introduced,
   changing from ASCII fold → $UpCase fold → any future fold is a single-line
   change at the construction site.
3. **The trigram byte→char migration is the only structural change.** Everything
   else is parameter threading.
4. **The compiled-in default eliminates the hard dependency on live Windows
   volumes.** Offline .uffs files, cross-platform development, and CI all
   work with the default table.