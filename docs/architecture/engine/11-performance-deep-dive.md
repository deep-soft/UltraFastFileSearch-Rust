# Performance Deep Dive

## Introduction

This document explains why UFFS is a high-performance MFT search engine, the engineering decisions behind it, and real-world benchmark data from a 7-drive, 26-million-record production system.

---

## Architecture: Three Caching Levels

UFFS operates in three performance tiers, each with dramatically different latency:

| Level | What Happens | Typical Latency (26M records) |
|-------|-------------|-------------------------------|
| **COLD** | No daemon, no cache. Raw MFT read from disk, full parse, compact index build, trigram index build, path resolution tree. | 66s (7 drives parallel) |
| **WARM CACHE** | No daemon, but serialized compact index exists on disk. Daemon starts and deserializes cached index — no MFT read. | 7s |
| **HOT** | Daemon running with in-memory index. Pure search — no I/O, no startup. | **157ms** (all 7 drives, 26M records) |

The HOT path delivers **420× speedup** over COLD, and single-drive queries return in **6–100ms**.

---

## Real-World Benchmarks (v0.4.69)

### Test Environment

**System**: MASTER-PC — 24 CPU cores
**Drives**: 7 NTFS volumes (2× NVMe, 5× HDD/USB)
**Total records**: 25,842,119 across all drives
**Binary**: v0.4.69 release build (LTO=fat, codegen-units=1, cross-compiled from macOS via `cargo xwin`)
**Protocol**: 3-phase per drive — COLD → WARM CACHE → HOT, `--profile --limit 100`

### Per-Drive 3-Phase Profile (`*` pattern)

| Drive | Records | COLD | WARM CACHE | HOT | COLD→HOT Speedup |
|-------|---------|------|------------|-----|-------------------|
| C: (NVMe) | 3,423,716 | 7,717ms | 2,417ms | **24ms** | **322×** |
| D: (HDD) | 7,065,539 | 26,568ms | 4,511ms | **101ms** | **263×** |
| E: (HDD/USB) | 2,929,519 | 42,609ms | 1,419ms | **21ms** | **2,029×** |
| F: (NVMe) | 2,221,343 | 4,796ms | 1,742ms | **19ms** | **252×** |
| G: (USB) | 15,090 | 1,416ms | 660ms | **6ms** | **236×** |
| M: (HDD/NAS) | 1,908,805 | 26,493ms | 1,414ms | **17ms** | **1,558×** |
| S: (HDD) | 8,278,102 | 66,828ms | 6,841ms | **79ms** | **846×** |
| **ALL** | **25,842,119** | **66,074ms** | **7,041ms** | **157ms** | **421×** |

### HOT Path Timing Breakdown (ALL drives, 157ms)

```
Client → Daemon
  Connect:           4 ms    (named pipe)
  Await ready:       0 ms    (daemon already warm)
  Search (IPC):    149 ms    (daemon: 137ms search + 12ms transfer)
  Convert rows:      0 ms    (100 rows)
```

At 25.8M records searched in 137ms, the HOT path sustains **188 million records/second**.

---

## Why UFFS Is Fast

### 1. Direct MFT Reading (15× vs Standard APIs)

Windows file enumeration (`FindFirstFile`/`FindNextFile`) requires ~2 syscalls per file. UFFS reads the MFT as a raw byte stream via a single volume handle — one `ReadFile` call processes ~1,000 files (1 MB ÷ 1 KB records), reducing syscall overhead by ~2,000× on a 2M-file drive.

### 2. Bitmap Skip (40–55% I/O Reduction)

The MFT contains records for deleted files. Typical utilization is 40–60%. UFFS reads `$MFT::$BITMAP` first (~250 KB), then trims read ranges to skip contiguous unused regions. On the S: drive (11.5 GB MFT, 45% utilization), this saves ~6.3 GB of disk reads.

### 3. IOCP Sliding Window (I/O + CPU Overlap)

I/O Completion Ports with a sliding window of concurrent reads. While buffer N is parsed, buffers N+1..N+K are already in flight. Window size is auto-tuned per drive type: NVMe=32 (deep queue), SSD=8 (NCQ), HDD=2–6 (minimize seeks).

### 4. Inline Parsing (Zero Intermediate Copies)

`SlidingIocpInline` parses each completed I/O buffer directly into the `MftIndex` as the IOCP completion arrives. No intermediate `Vec<ParsedRecord>`, no second pass, no double-buffering of record data.

### 5. Compact Memory Layout (224 Bytes/Record)

Hand-tuned `FileRecord` with bit-packed flags (17 booleans in one `u32`), inline first name/stream (no heap allocation for 95%+ of files), sentinel values instead of `Option<>`, contiguous names buffer with `(offset, length)` references, and 16-bit interned extension IDs.

### 6. mimalloc Global Allocator

Purpose-built for the millions-of-small-allocations workload. ~10–15% throughput improvement on NVMe where parsing is the bottleneck.

### 7. Extension Index (50–200× for `*.ext` Queries)

Interned 16-bit extension IDs during parsing, with an inverted index `ext_id → Vec<record_index>`. A `*.rs` query on 5K results takes 0.5ms instead of 100ms full-scan.

### 8. Zero-Allocation Case-Insensitive Matching

Byte-level inline comparison without allocating a lowercase copy — eliminates 2–8M heap allocations per search query across 26M records.

### 9. Leaf-Peeling Tree Metrics (O(n), No Recursion)

Array-based Kahn-style topological sort for treesize/descendants. O(n) time, O(n) space, no recursion, cache-friendly sequential access. Guaranteed stack safety on any tree depth.

### 10. LCN-Ordered Reads (HDD Only)

Read chunks sorted by physical disk offset (LCN order) to minimize head movement on fragmented HDDs. 20–30% improvement on HDDs; no effect on NVMe/SSD.

### 11. Daemon Architecture with Compact Cache

The daemon holds the full index in memory. First search auto-starts the daemon, which persists a serialized compact cache to disk. Subsequent daemon starts deserialize the cache (~3–7s for 26M records) instead of re-reading the MFT (~66s). Once hot, searches are pure in-memory scans — **6–157ms** depending on drive count.

### 12. Trigram Index for Substring Queries

Three-character trigram index built during startup. Substring queries intersect trigram posting lists before scanning records, dramatically reducing the search space for patterns like `*config*`.

---

## Note on C++ Comparison

UFFS includes a C++ reference implementation for parity verification. When comparing COLD timings, the comparison is **not apples-to-apples**:

| | UFFS (Rust) | C++ Reference |
|-|-------------|---------------|
| MFT read | ✅ | ✅ |
| Full path resolution (parent chain walk) | ✅ | ✅ |
| Compact index build (224 B/record) | ✅ | ❌ |
| Trigram index build | ✅ | ❌ |
| Compact cache serialization to disk | ✅ | ❌ |
| Daemon startup + IPC | ✅ | ❌ (direct) |
| Tree metrics (descendants, treesize) | ✅ | ❌ |
| Extension interning + inverted index | ✅ | ❌ |

UFFS does **significantly more work** during COLD startup (~1.22× slower than C++) because it builds persistent data structures that make every subsequent search instant. The C++ tool re-reads the MFT on every invocation.

### Parity Comparison (v0.4.69, COLD, 7 drives)

| Drive | C++ (warm disk) | Rust (cold) | Ratio | Files/sec (Rust) |
|-------|-----------------|-------------|-------|------------------|
| C: | 12.1s | 14.2s | 1.17× | 241,821/s |
| D: | 40.7s | 43.8s | 1.08× | 161,177/s |
| E: | 43.5s | 48.9s | 1.12× | 59,929/s |
| F: | 7.1s | 10.5s | 1.48× | 211,822/s |
| G: | 279ms | 938ms | 3.35× | 16,059/s |
| M: | 24.1s | 29.4s | 1.22× | 64,882/s |
| S: | 1m 1.4s | 1m 23.5s | 1.36× | 99,187/s |
| **TOTAL** | **3m 9.3s** | **3m 51.2s** | **1.22×** | **111,782/s** |

After COLD, UFFS never needs to re-read the MFT — the daemon serves all subsequent queries from memory in **6–157ms**.

---

## Benchmark Methodology

### 3-Phase Protocol

Every benchmark runs three caching levels per drive:

1. **COLD** — Kill daemon, delete all cache files, run `uffs "*" --profile --drive X --limit 100`
2. **WARM CACHE** — Kill daemon (cache files remain), run same command
3. **HOT** — Daemon still running, run same command

This isolates: (1) raw MFT read + full index build, (2) cache deserialization, (3) pure in-memory search.

### Profiling

Use `--profile` for full per-phase timing breakdown (client connect, daemon startup, search, IPC, per-drive cache/MFT/compact/trigram timing). Use `rust-script scripts\windows\profile.rs` for automated 3-phase profiling across all drives.

---

*Last Updated: 2026-04-03*
*UFFS Version: 0.4.69*
