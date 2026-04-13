# Performance & Benchmarking

## Introduction

This document describes the performance characteristics of UFFS, the optimization techniques employed, and how to benchmark and profile the engine. After reading this document, you should be able to:

1. Understand why UFFS is fast and where time is spent
2. Profile and benchmark specific code paths
3. Identify optimization opportunities

---

## Benchmark Results (v0.4.106)

### Three-Phase Results — 7 Drives, 25.9M Records

Tested on AMD Ryzen 9 3900XT (12c/24t), 64 GB DDR4.  Pattern: `*`,
limit: 100, averaged over 3 rounds per phase.

| Drive | Type | Records | COLD | WARM | HOT | Cold→Hot |
|-------|------|--------:|-----:|-----:|----:|---------:|
| C: | NVMe | 3.5M | 7.5 s | 2.6 s | 229 ms | **33×** |
| D: | SATA SSD | 7.1M | 28.8 s | 4.9 s | 253 ms | **114×** |
| E: | SATA SSD | 2.9M | 41.5 s | 2.6 s | 230 ms | **180×** |
| F: | NVMe | 2.2M | 4.6 s | 2.2 s | 226 ms | **20×** |
| G: | USB stick | 15K | 1.4 s | 779 ms | 211 ms | **7×** |
| M: | SATA HDD | 1.9M | 26.7 s | 1.6 s | 224 ms | **119×** |
| S: | SATA HDD | 8.3M | 67.0 s | 4.7 s | 259 ms | **259×** |
| **ALL** | **Mixed** | **25.9M** | **66.5 s** | **7.3 s** | **381 ms** | **175×** |

### What the benchmark shows

- **Scale is the headline** — UFFS keeps **25.9M records across 7 drives** searchable from one daemon.
- **Cold-start time is storage-bound** — NVMe is parse-bound, while HDD cold runs are dominated by seek time and raw MFT I/O.
- **Warm restart is the operator win** — the full 25.9M-record searchable state returns in **7.3 s** from serialized cache.
- **Hot queries are media-independent** — once the daemon is warm, single-drive end-to-end queries stay in the **211–259 ms** range regardless of whether the underlying volume is NVMe, SSD, HDD, or USB.
- **Daemon throughput is higher than CLI wall time** — the **381 ms** all-drive hot number includes process spawn, IPC, and formatting; the actual daemon-side scan is **151 ms**.

> 📖 **Full benchmark data:** [Performance](../../user-manual/performance.md)

---

## Optimization Layers

### Layer 1: I/O Optimization

| Technique | Impact | Description |
|-----------|--------|-------------|
| **Direct MFT reading** | ~15× vs FindFirstFile | Bypass file system APIs entirely |
| **Bitmap skip** | 50-80% I/O reduction | Skip deleted records using $MFT::$BITMAP |
| **IOCP async I/O** | Overlaps I/O with CPU | Multiple reads in flight simultaneously |
| **LCN-ordered reads** | 20-30% HDD improvement | Minimize disk seeks by reading in physical order |
| **Drive-type tuning** | 10-50% per drive | NVMe: 32 concurrent reads, 4MB chunks; HDD: 2-6, 1MB |
| **Aligned buffers** | Required for NO_BUFFERING | Sector-aligned allocation avoids extra copies |

### Layer 2: Memory Optimization

| Technique | Impact | Description |
|-----------|--------|-------------|
| **Compact FileRecord** | 224 bytes/record | Bit-packed flags, inline first_name/first_stream |
| **Contiguous names buffer** | Cache-friendly | All names in one `String`, no per-name allocation |
| **Pre-allocated vectors** | Eliminates resizing | Sized from bitmap popcount before parsing starts |
| **Extension interning** | 8 bytes per name ref | 16-bit extension ID instead of string per record |
| **mimalloc** | ~10-15% throughput | Reduces fragmentation for millions of small allocs |
| **NO_ENTRY sentinel** | No Option overhead | `u32::MAX` instead of `Option<u32>` saves 4 bytes |

### Layer 3: Algorithm Optimization

| Technique | Impact | Description |
|-----------|--------|-------------|
| **Extension index** | 50× for *.ext queries | O(matches) instead of O(all) for extension patterns |
| **Inline parsing** | No intermediate copies | Records parsed directly into MftIndex during I/O |
| **Zero-alloc case compare** | Eliminates 8M allocs | Byte-level ASCII comparison instead of `.to_lowercase()` |
| **Leaf-peeling tree metrics** | O(n) no recursion | Array-based Kahn sort instead of recursive DFS |
| **Lazy path resolution** | Only for matched records | Paths computed after all filters applied |
| **Pattern classification** | Optimal matcher per type | `*.txt` → suffix check; `*foo*` → substring; etc. |

### Layer 4: Concurrency Optimization

| Technique | Impact | Description |
|-----------|--------|-------------|
| **IOCP sliding window** | Saturates I/O device | N reads always in flight (N tuned per drive type) |
| **Lock-free hot path** | Zero contention | Single-owner MftIndex during build, no mutexes |
| **Multi-drive parallelism** | Near-linear scaling | Bounded tokio tasks, independent IOCP per drive |
| **Rayon parallel parsing** | NVMe: overlaps CPU/IO | Parse completed buffers on worker threads |
| **Buffer recycling** | Zero allocs after warmup | Completed buffers returned to pool, not freed |

---

## Where Time Is Spent

### NVMe Drive (C:, 3.5M files, 7.5 s cold)

```
Phase                Time     %    Notes
───────────────────────────────────────────────
Volume open          <1ms   <1%   CreateFile + FSCTL
Metadata collection    5ms   <1%   Volume data + retrieval pointers
Bitmap read           10ms   <1%   ~250KB bitmap
Chunk planning         1ms   <1%   In-memory calculation
IOCP read + parse   5.9s    79%   ★ DOMINANT — parsing is bottleneck
Tree metrics        0.8s    10%   Leaf-peeling O(n)
Extension index     0.3s     4%   Build interned lookup
Stats + finalize    0.5s     6%   Recompute, cleanup
```

On NVMe, **parsing is the bottleneck** (not I/O). The disk can deliver data faster than the CPU can parse it. This is why parallel parsing helps on NVMe.

### SATA HDD Drive (S:, 8.3M files, 67 s cold)

```
Phase                Time     %    Notes
───────────────────────────────────────────────
Volume open          <1ms   <1%
Metadata collection    8ms   <1%
Bitmap read           20ms   <1%
Chunk planning         2ms   <1%
IOCP read + parse  65.0s    97%   ★ DOMINANT — I/O is bottleneck
Tree metrics        1.0s     1%
Extension index     0.5s     1%
Stats + finalize    0.5s     1%
```

On HDD, **I/O is the bottleneck**. The disk head seek time dominates. Bitmap skip and LCN-ordered reads are critical here.

---

## Profiling

### Built-in Profiling (`--profile`)

```bash
uffs * --drive C --profile
```

Outputs detailed timing for each phase:
```
=== PROFILE: Client → Daemon ===
  Connect:              3 ms
  Await ready:          0 ms
  Search (IPC):       152 ms  (daemon: 151 ms, transfer: 1 ms)
  Convert rows:         0 ms  (10 rows)

=== PROFILE: Daemon Internals ===
  Startup:           3861 ms
  Search:             151 ms  (25,929,744 records scanned)
  Row build:            0 ms  (10 → SearchRow)

=== TOTAL: 182 ms ===
```

### Benchmark Mode (`--benchmark`)

Skips output formatting to isolate MFT reading performance:

```bash
uffs * --drive C --benchmark
# Only measures: volume open → index built
# No path resolution, no formatting, no stdout I/O
```

### Flamegraph Profiling

```bash
# Build with profiling profile (debug symbols, no LTO)
cargo build --profile profiling

# Use cargo-flamegraph or perf
cargo flamegraph --profile profiling -- uffs * --drive C --benchmark
```

The `profiling` Cargo profile enables debug symbols while keeping optimizations:

```toml
[profile.profiling]
inherits = "release"
debug = true
strip = false
lto = false
codegen-units = 16
```

### Tracing

UFFS uses `tracing` for structured logging. Enable with environment variables:

```bash
RUST_LOG=debug uffs * --drive C    # Info + debug messages
RUST_LOG=trace uffs * --drive C    # Maximum verbosity (very noisy)
```

Key trace points:
- `[TRIP]` markers at function entry/exit for trip-wire debugging
- I/O chunk progress (bytes read, records parsed)
- Drive detection and tuning decisions
- Extension record processing

---

## Memory Usage

### Typical Memory Footprint (3.5M files, single NVMe drive)

| Component | Size | Notes |
|-----------|------|-------|
| `records: Vec<FileRecord>` | 448 MB | 2M × 224 bytes |
| `frs_to_idx: Vec<u32>` | 20 MB | 5M × 4 bytes (sparse) |
| `names: String` | 46 MB | 2M × 23 bytes avg |
| `links: Vec<LinkInfo>` | 3 MB | ~125K hardlinks |
| `streams: Vec<IndexStreamInfo>` | 15 MB | ~500K ADS |
| `children: Vec<ChildInfo>` | 42 MB | 3M entries |
| I/O buffers (peak) | 32 MB | 32 × 1MB (NVMe) |
| **Total** | **~600 MB** | |

### Memory Reduction Techniques

1. **Compact types**: 224 bytes per record vs ~400+ bytes with naive layout
2. **Inline first_name/first_stream**: Avoids heap allocation for 95%+ of records
3. **Shared names buffer**: One allocation instead of 2M `String` objects
4. **Extension interning**: 2 bytes per name ref instead of string per extension
5. **NO_ENTRY sentinel**: `u32::MAX` instead of `Option<u32>` (saves 4 bytes × millions)

---

## Why UFFS Is Fast on NVMe

The dominant performance advantages come from architectural decisions in the Rust engine:

1. **Inline parsing**: Records parsed directly into `MftIndex` during IOCP completion — no intermediate copies or staging buffers.
2. **mimalloc**: Purpose-built allocator reduces fragmentation for millions of small objects.
3. **Compact `FileRecord`**: 224 bytes per record with bit-packed flags and inline first-name/first-stream.
4. **Zero-copy NTFS parsing**: `zerocopy` crate reads NTFS headers directly from I/O buffers without memcpy.
5. **Contiguous names buffer**: Single `String` allocation instead of per-name heap objects.

## Known Optimization Targets

### Multi-Drive Filtered Scan (`*.ext`)

Filtered multi-drive parallel scans are marginally faster than full scans
for selective patterns (`*.txt`: 216 ms vs `*`: 381 ms on all 7 drives).
Fewer matching rows means less output formatting overhead.

---

## Cargo Build Profiles

| Profile | Use Case | LTO | Debug | Opt |
|---------|----------|-----|-------|-----|
| `dev` | Development | No | Full | 0 |
| `debug-optimized` | Dev with speed | No | Full | 2 |
| `release` | Production | Fat | No | 3 |
| `profiling` | Flamegraphs | No | Full | 3 |
| `bench` | Benchmarks | Thin | No | 3 |
| `dist` | Distribution | Thin | No | 3 |
| `xwin-dev` | Cross-compile dev | No | Full | 0 |

---

*Document Version: 2.0*
*Last Updated: 2026-04-12*
*UFFS Version: 0.4.106*
