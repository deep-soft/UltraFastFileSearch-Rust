# Performance Deep Dive

## Introduction

This document explains why UFFS is a high-performance MFT search engine, the engineering decisions behind it, and real-world benchmark data from a 7-drive, 25.9-million-record production system tested up to **100M records**.

> **See also:** [Performance](../../user-manual/performance.md) for the
> full benchmark reference with per-drive tables and validation throughput.

---

## Architecture: Three Caching Levels

UFFS operates in three performance tiers, each with dramatically different latency:

| Level | What Happens | Typical Latency (25.9M records) |
|-------|-------------|-------------------------------|
| **COLD** | No daemon, no cache. Raw MFT read from disk, full parse, compact index build, trigram index build, path resolution tree. | 66 s (v0.5.4) → 68.5 s (v0.5.62) — 7 drives parallel |
| **WARM CACHE** | No daemon, but serialized compact index exists on disk. Daemon starts and deserializes cached index — no MFT read. | 6.9 s (v0.5.4) → **5.7 s (v0.5.62, −17 %)** |
| **HOT** | Daemon running with in-memory index. Pure search — no I/O, no startup. | **163 ms** e2e (`*` top-N, v0.5.4); on v0.5.62 targeted queries vs Everything range **29–100 ms** (C+D bench) with **0–3 ms daemon-side** |

The HOT path delivers **407× speedup** over COLD.  Targeted queries (exact name, prefix, extension, substring, combined) return in **9–13 ms** end-to-end with **0–3 ms daemon-side** — even at **100M records**.

---

## Real-World Benchmarks (v0.5.62 / v0.5.64)

> **Latest numbers:** cross-tool head-to-head vs Everything, 7-drive full
> scan at 13.5 s (25.9 M records), aggregations at ~180 ms, 5.7 s WARM
> restart, 4.99 GB settled RSS.  Full current-state table in
> `@/Users/rnio/Private/Github/UltraFastFileSearch/docs/research/cross-tool-benchmark-analysis.md` §Current State.
>
> The v0.5.4 per-drive tables below are **retained for historical
> context** — they were captured before Phase 1, Phase 2, and Phase 3
> shipped.  The HOT interactive-search shape (`*` with `--limit 100`,
> 8 patterns) has not been re-run on v0.5.62 yet; that re-bench is
> listed in the "Benchmarks still worth running" table of the cross-tool
> analysis doc.

### Test Environment

**System**: AMD Ryzen 9 3900XT — 12 cores / 24 threads, 64 GB DDR4
**Drives**: 7 NTFS volumes (2× NVMe Samsung 990 PRO, 2× SATA Samsung 980 PRO, 2× SATA WD 8 TB HDD, 1× USB stick)
**Total records**: 25,931,436 across all drives (live), scaled up to 100.4M with offline MFT clones
**Binary**: v0.5.4 release build (LTO=fat, codegen-units=1, cross-compiled from macOS via `cargo xwin`)
**Latest bench binary**: v0.5.62 (774 KB, Phase 2+3 closed, Run 12 shipped in v0.5.64)
**Protocol**: Per-drive profile (COLD → WARM → HOT) + interactive search (30 rounds, 8 patterns) + bulk retrieval

### Per-Drive 3-Phase Results (`*` pattern)

| Drive | Type | Records | COLD | WARM | HOT | Cold→Hot |
|-------|------|--------:|-----:|-----:|----:|---------:|
| C: | NVMe | 3,512,541 | 7.7 s | 6.4 s | **27 ms** | **284×** |
| D: | SATA SSD | 7,066,020 | 28.6 s | 6.4 s | **49 ms** | **584×** |
| E: | SATA SSD | 2,929,523 | 42.5 s | 2.4 s | **24 ms** | **1771×** |
| F: | NVMe | 2,221,347 | 4.3 s | 1.4 s | **19 ms** | **229×** |
| G: | USB stick | 15,094 | 1.3 s | 572 ms | **6 ms** | **219×** |
| M: | SATA HDD | 1,908,809 | 26.4 s | 1.4 s | **18 ms** | **1469×** |
| S: | SATA HDD | 8,278,106 | 67 s | 4.8 s | **54 ms** | **1236×** |
| **ALL** | **Mixed** | **25,931,436** | **66 s** | **6.9 s** | **163 ms** | **407×** |

### HOT Interactive Search Percentile Latency (ALL drives, 25.9M records, 30 rounds)

| Pattern | e2e p50 | e2e p95 | daemon p50 | daemon p95 |
|---------|--------:|--------:|-----------:|-----------:|
| `*` (full scan) | 161 ms | 183 ms | 152 ms | 172 ms |
| `notepad.exe` (exact) | 9 ms | 9 ms | 0 ms | 0 ms |
| `win*` (prefix) | 10 ms | 10 ms | 1 ms | 1 ms |
| `*.dll` (extension) | 9 ms | 10 ms | 1 ms | 1 ms |
| `config` (substring) | 10 ms | 11 ms | 1 ms | 1 ms |
| date filter | 152 ms | 156 ms | 143 ms | 147 ms |
| size filter | 153 ms | 160 ms | 144 ms | 150 ms |
| combined | 9 ms | 10 ms | 0 ms | 0 ms |

At 25.9M records, targeted queries complete in **0–1 ms daemon-side**.
Full scans sustain **167 million records/second**.

### Bulk Retrieval (CSV, `--out-dir`, per-drive)

| Tier | Rows | Time | Rows/sec |
|------|-----:|-----:|---------:|
| 100 | 101 | 213 ms | 474/s |
| 1k | 1,001 | 202 ms | 5.0k/s |
| 10k | 10,001 | 323 ms | 31k/s |
| 100k | 100,001 | 1.4 s | 73k/s |
| 1M | 1,000,001 | 3.4 s | 292k/s |
| ALL (8.3M) | 8,278,106 | 25.6 s | **326k/s** |

Pipe-based output peaks at ~122k rows/s.  Direct file write (`--out-dir`)
reaches **323k rows/s** — a **2.6× speedup**.

### Scale Ceiling (100M records, 16 drives, 30 rounds)

| Total Records | `*` e2e p50 | targeted p50 | Status |
|--------------:|------------:|-------------:|--------|
| 25.9M | 161 ms | 9–10 ms | ✅ |
| 42.5M | 259 ms | 9–10 ms | ✅ |
| 59.0M | 471 ms | 10–12 ms | ✅ |
| 75.6M | 600 ms | 10–12 ms | ✅ |
| 92.2M | 670 ms | 11–14 ms | ✅ |
| **100.4M** | **808 ms** | **11–13 ms** | **✅** |
| >100M | — | — | ❌ OOM |

Targeted queries stay at **0–3 ms daemon-side** regardless of corpus size.

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

The daemon holds the full index in memory. First search auto-starts the daemon, which persists a serialized compact cache to disk. Subsequent daemon starts deserialize the cache (~6.9 s for 25.9M records) instead of re-reading the MFT (~66 s). Once hot, targeted searches return in **9–13 ms** end-to-end; full scans in **163 ms** across all 7 drives.

### 12. Trigram Index for Substring Queries

Three-character trigram index built during startup. Substring queries intersect trigram posting lists before scanning records, dramatically reducing the search space for patterns like `*config*`.

---

## C++ Reference Baseline (engineering validation, not public market benchmark)

UFFS keeps the earlier C++ implementation as a parity and regression baseline. This comparison is useful for validating parser correctness and understanding cold-path trade-offs, but it is not the headline market benchmark for the Rust engine.

The Rust engine intentionally does more work during COLD startup: compact index build, cache serialization, extension interning, tree metrics, and daemon-ready data structures. The relevant buyer-facing payoff is not the raw COLD number alone, but the combination of:

- full cold build from raw MFT
- warm restart from serialized cache
- hot in-memory queries once the daemon is ready

Public external comparisons should therefore use the current Rust engine and separate readiness, interactive top-N, bulk retrieval, and scale-ceiling workloads.

When comparing COLD timings, the comparison is **not apples-to-apples**:

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

UFFS does **significantly more work** during COLD startup (~1.29× slower than C++) because it builds persistent data structures that make every subsequent search instant. The C++ tool re-reads the MFT on every invocation.

### Parity Comparison (v0.4.106, COLD, 6 drives)

| Drive | C++ (warm disk) | Rust (cold) | Ratio | Files/sec (Rust) |
|-------|-----------------|-------------|-------|------------------|
| C: | 12.4 s | 17.4 s | 1.40× | 201,658/s |
| D: | 39.8 s | 47.1 s | 1.18× | 150,015/s |
| E: | 43.6 s | 48.8 s | 1.12× | 59,998/s |
| F: | 7.0 s | 11.0 s | 1.57× | 202,343/s |
| M: | 24.1 s | 31.7 s | 1.31× | 60,160/s |
| S: | 1m 1.6 s | 1m 26.8 s | 1.41× | 95,326/s |
| **TOTAL** | **3m 8.6 s** | **4m 2.9 s** | **1.29×** | **106,695/s** |

After COLD, UFFS never needs to re-read the MFT — the daemon serves all subsequent queries from memory in **9–163 ms** end-to-end.

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

*Last Updated: 2026-04-14*
*UFFS Version: 0.5.4*
