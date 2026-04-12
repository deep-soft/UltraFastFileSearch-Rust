# Performance

UFFS is designed to search millions of files in milliseconds.  This page
documents measured performance across seven NTFS drives totalling
**25.9 million records**, captured on real hardware with the standard
benchmark and profiling scripts.

> **See also:** [Advanced Diagnostics](advanced-diagnostics.md) ·
> [Daemon](daemon.md) · [Cache & Data Sources](cache-and-data.md) ·
> [Concepts](concepts.md)

---

## 1  Test System

| Component | Specification |
|-----------|--------------|
| OS | Windows 11 Pro 64-bit (24H2 / Build 26100) |
| CPU | AMD Ryzen 9 3900XT — 12 cores / 24 threads (Matisse, 7 nm) |
| RAM | 64 GB Dual-Channel DDR4 @ 1312 MHz |
| Motherboard | ASUS ProArt B550-CREATOR (AM4) |
| NVMe SSD | Samsung SSD 990 PRO 2 TB (C:, F:) |
| SATA SSD | Samsung SSD 980 PRO 1 TB (D:, E:) |
| SATA HDD | WD 8 TB × 2 (WDC WD82PURZ, M: and S:) |
| USB storage | SanDisk Extreme 58 GB USB stick (G:) |
| Power profile | AMD Ryzen High Performance |
| UFFS version | 0.4.106 |

### Drives Under Test

| Drive | Type | Records | Description |
|-------|------|--------:|-------------|
| C: | NVMe SSD | 3,510,866 | Windows system drive |
| D: | SATA SSD | 7,066,019 | Data drive (Dropbox, projects) |
| E: | SATA SSD | 2,929,519 | Media / archive |
| F: | NVMe SSD | 2,221,343 | Secondary Windows install |
| G: | USB stick | 15,090 | SanDisk Extreme 58 GB |
| M: | SATA HDD | 1,908,805 | WD 8 TB spinning disk (WDC WD82PURZ) |
| S: | SATA HDD | 8,278,102 | WD 8 TB spinning disk (WDC WD82PURZ) |
| **ALL** | **Mixed** | **25,929,744** | **All 7 drives in parallel** |

---

## 2  The Three-Phase Model

Every UFFS search goes through a startup phase before the first query
can be answered.  The cost depends on how "warm" the system is:

```
Phase 1: COLD                 Phase 2: WARM CACHE           Phase 3: HOT
─────────────────────         ─────────────────────         ──────────────
Kill daemon                   Kill daemon                   Daemon running
Delete cache files            Cache files stay on disk      Index in memory
                              ↓                             ↓
Read raw MFT from disk        Deserialize .iocp cache       Query directly
Parse → build index           → build index                 ↓
Write .iocp cache             ↓                             Results
↓                             Results                       (~200 ms)
Results
(seconds to minutes)          (~1–5 s)
```

| Phase | What happens | When it occurs |
|-------|-------------|----------------|
| **COLD** | Daemon reads raw NTFS MFT, parses every record, builds in-memory index, writes cache | First run ever, or after `daemon kill` + cache deletion |
| **WARM CACHE** | Daemon loads serialized `.iocp` cache from disk — skips expensive MFT parse | Daemon restart (reboot, manual kill) with cache intact |
| **HOT** | Daemon already running, index in memory — pure query execution | Every search after the first one |

---

## 3  Per-Drive Results

All timings are wall-clock, end-to-end (process spawn → exit), averaged
over 3 rounds.  Pattern: `*` (full scan), limit: 100 rows.

### Cold Start (Raw MFT Read)

| Drive | Records | Avg | Min | Max | Records/sec |
|-------|--------:|----:|----:|----:|------------:|
| G: | 15,090 | 1.4 s | 1.0 s | 1.5 s | 10,900 |
| F: | 2,221,343 | 4.6 s | 4.6 s | 4.6 s | 485,500 |
| C: | 3,510,866 | 7.5 s | 7.3 s | 7.8 s | 470,900 |
| M: | 1,908,805 | 26.7 s | 26.7 s | 26.7 s | 71,500 |
| D: | 7,066,019 | 28.8 s | 28.7 s | 28.8 s | 245,800 |
| E: | 2,929,519 | 41.5 s | 40.8 s | 42.8 s | 70,600 |
| S: | 8,278,102 | 67.0 s | 67.0 s | 67.1 s | 123,600 |
| **ALL** | **25,929,744** | **66.5 s** | **66.4 s** | **66.6 s** | **389,900** |

> **Note:** M: and S: are SATA spinning disks where I/O is the
> bottleneck — raw MFT reads are bound by HDD seek time, not CPU.
> NVMe drives (C:, F:) achieve 470K+ records/sec.
> The ALL-drives cold start runs all drives in parallel, so total time
> ≈ slowest individual drive.

### Warm Cache (Serialized .iocp Load)

| Drive | Records | Avg | Speedup vs Cold |
|-------|--------:|----:|----------------:|
| G: | 15,090 | 779 ms | 1.8× |
| M: | 1,908,805 | 1.6 s | 17.1× |
| F: | 2,221,343 | 2.2 s | 2.1× |
| C: | 3,510,866 | 2.6 s | 2.9× |
| E: | 2,929,519 | 2.6 s | 16.1× |
| D: | 7,066,019 | 4.9 s | 5.8× |
| S: | 8,278,102 | 4.7 s | 14.3× |
| **ALL** | **25,929,744** | **7.3 s** | **9.1×** |

### Hot (In-Memory Query)

| Drive | Records | Avg | Speedup vs Cold |
|-------|--------:|----:|----------------:|
| G: | 15,090 | 211 ms | 6.5× |
| M: | 1,908,805 | 224 ms | 119.2× |
| F: | 2,221,343 | 226 ms | 20.3× |
| C: | 3,510,866 | 229 ms | 32.6× |
| E: | 2,929,519 | 230 ms | 180.4× |
| D: | 7,066,019 | 253 ms | 113.7× |
| S: | 8,278,102 | 259 ms | 258.7× |
| **ALL** | **25,929,744** | **381 ms** | **174.5×** |

> **Note:** HOT timings include ~200 ms of process startup overhead
> (spawning `uffs.exe`, connecting to daemon via IPC, formatting output).
> The actual daemon-side search takes **~16–150 ms** depending on drive
> count (see §6 Profile Internals).

---


## 4  Speedup Summary

The table below shows end-to-end speedup from cold start to hot query
for each drive.  The Cold→Hot ratio is the primary performance metric.

| Drive | Cold | Warm | Hot | Cold→Hot | Cold→Warm |
|-------|-----:|-----:|----:|---------:|----------:|
| C: | 7.5 s | 2.6 s | 229 ms | **32.6×** | 2.9× |
| D: | 28.8 s | 4.9 s | 253 ms | **113.7×** | 5.8× |
| E: | 41.5 s | 2.6 s | 230 ms | **180.4×** | 16.1× |
| F: | 4.6 s | 2.2 s | 226 ms | **20.3×** | 2.1× |
| G: | 1.4 s | 779 ms | 211 ms | **6.5×** | 1.8× |
| M: | 26.7 s | 1.6 s | 224 ms | **119.2×** | 17.1× |
| S: | 67.0 s | 4.7 s | 259 ms | **258.7×** | 14.3× |
| **ALL** | **66.5 s** | **7.3 s** | **381 ms** | **174.5×** | **9.1×** |

> On spinning disks (M:, S:) the cold-start penalty is extreme —
> reading raw MFT from a HDD is 10–60× slower than NVMe.
> The daemon eliminates this entirely: once loaded, every drive
> responds in 200–260 ms regardless of media type.

---

## 5  HOT Query Patterns

Different search patterns exercise different code paths in the query
engine.  The benchmark runs three representative patterns against a
hot daemon:

| Pattern | Query path | What it tests |
|---------|-----------|---------------|
| `*` | Full scan — DataFrame pass-through | Baseline: no filtering |
| `*.txt` | Extension filter — Polars column predicate | Indexed column filter |
| `test` | Substring search — contains match | String scanning |

### Results (HOT, per drive, avg of 3 rounds)

| Drive | `*` | `*.txt` | `test` |
|-------|----:|--------:|-------:|
| C: | 229 ms | 211 ms | 212 ms |
| D: | 253 ms | 211 ms | 215 ms |
| E: | 230 ms | 213 ms | 212 ms |
| F: | 226 ms | 217 ms | 213 ms |
| G: | 211 ms | 210 ms | 210 ms |
| M: | 224 ms | 209 ms | 210 ms |
| S: | 259 ms | 210 ms | 214 ms |
| **ALL** | **381 ms** | **216 ms** | **217 ms** |

> **Observations:**
> - Extension filters (`*.txt`) and substring searches (`test`) are
>   marginally faster than `*` because fewer rows pass through to output
>   formatting.
> - The `*` scan on ALL drives (381 ms) is the only case where search
>   time scales with drive count — it must touch every record across
>   25.9M rows.  Filtered queries stay flat at ~215 ms.

---

## 6  Profile Internals

The `--profile` flag breaks down where time is spent inside the daemon.
This data is from a hot daemon with all 7 drives loaded (25.9M records):

```
=== PROFILE: Client → Daemon ===
  Connect:              3 ms
  Await ready:          0 ms
  Search (IPC):       152 ms  (daemon: 151 ms, transfer: 1 ms)
  Convert rows:         0 ms  (10 rows)

=== PROFILE: Daemon Internals ===
  Startup:           3861 ms  (all drives loaded)
  Lock acquire:         0 ms
  Search:             151 ms  (25,929,744 records scanned)
  Row build:            0 ms  (10 → SearchRow)

=== PROFILE: Per-Drive ===
  Drive       Records   Matches     Cache   MFT ms
     C:     3,510,866         0      2798        0
     D:     7,066,019         5      3789        0
     E:     2,929,519         2      2113        0
     F:     2,221,343         0      1764        0
     G:        15,090         0        11        0
     M:     1,908,805         0      1442        0
     S:     8,278,102         3      5088        0
    SUM    25,929,744               17005      686

=== TOTAL: 182 ms ===
```

### Where the 200 ms Goes

| Component | Time | Notes |
|-----------|-----:|-------|
| Process spawn | ~25 ms | OS creates `uffs.exe` process |
| IPC connect | 3 ms | Named pipe handshake |
| Daemon search | 151 ms | Scan 25.9M records across 7 drives |
| IPC transfer | 1 ms | Send result rows back |
| Row conversion | <1 ms | Deserialize 10 rows |
| Output formatting | ~20 ms | Format and write to stdout |
| **Total** | **~200 ms** | |

> The daemon-side search (151 ms for 25.9M records) translates to
> **172 million records/second** scan throughput.

### Per-Drive Profile (Cold Start)

From the `profile.rs` 3-phase profiler with `--profile` enabled:

| Drive | Records | Cold Total | Connect | Await Ready | Startup | Search |
|-------|--------:|-----------:|--------:|------------:|--------:|-------:|
| C: | 3,503,124 | 7.0 s | 1.2 s | 5.8 s | 5.9 s | 48 ms |
| D: | 7,066,019 | 28.5 s | 556 ms | 27.9 s | 26.5 s | 52 ms |
| E: | 2,929,519 | 42.7 s | 601 ms | 42.0 s | 41.5 s | 53 ms |
| F: | 2,221,343 | 4.4 s | 568 ms | 3.8 s | 3.7 s | 20 ms |
| G: | 15,090 | 1.4 s | 598 ms | 790 ms | 516 ms | <1 ms |
| M: | 1,908,805 | 26.5 s | 574 ms | 25.9 s | 25.7 s | 12 ms |
| S: | 8,278,102 | 67 s | 583 ms | 66 s | 65 s | 97 ms |

> Cold-start time is dominated by **MFT read + parse** (the "Startup"
> column).  Search itself is always <100 ms even on 8M records.

---

## 7  Validation Suite Throughput

UFFS ships three validation suites that double as performance
benchmarks for the query engine under realistic workloads.  All suites
run against a hot daemon loaded with 25.9M records across 7 drives.

### CLI Validation (240 tests, parallel)

| Metric | Value |
|--------|------:|
| Parallelism | 24 concurrent |
| Wall time | 65.7 s |
| Sum CPU time | 818.2 s |
| Avg per test | 3,409 ms |
| Slowest | 8,803 ms (duplicates verify=hash) |
| Fastest | 47 ms (simple search) |

### API Validation (225 tests, parallel)

| Metric | Value |
|--------|------:|
| Parallelism | 8 concurrent |
| Wall time | 48.7 s |
| Sum CPU time | 177.9 s |
| Avg per test | 790 ms |
| Slowest | 4,844 ms (duplicates JSON) |
| Fastest | <1 ms (status RPC) |

### MCP Validation (253 tests, sequential)

| Metric | Value |
|--------|------:|
| Parallelism | Sequential (MCP session) |
| Wall time | 96.2 s |
| Avg per test | 379 ms |
| Slowest | 5,244 ms (agent flow: overview → facet → drill-down) |
| Fastest | <1 ms (drive selection) |

> **Total:** 718 tests across CLI, API, and MCP — all pass, all
> exercising the same hot daemon with 25.9M records.

---

## 8  Daemon Runtime Statistics

After a full session (43 minutes uptime, validation + profiling + benchmark):

| Metric | Value |
|--------|------:|
| Uptime | 43 min 31 s |
| Startup duration | 3.7 s (warm cache, all 7 drives) |
| Total records | 25,922,252 |
| Queries served | 922 |
| Avg query time | 180 ms |
| Total query time | 2 min 46 s |
| Queries/second | 0.35 (reflects pauses between test runs) |

> Startup of 3.7 s for 25.9M records (warm cache) translates to
> **7.0 million records/second** cache deserialization throughput.

---

## 9  C++ vs Rust Parity Comparison

UFFS was rewritten from C++ to Rust.  The parity test runs both
implementations on the same drives and compares output.  The Rust
implementation reads raw MFT cold (no cache, no daemon), while the
C++ baseline runs warm:

| Drive | C++ (warm) | Rust (cold) | Ratio | Rust records/sec |
|-------|----------:|------------:|------:|-----------------:|
| C: | 12.4 s | 17.4 s | 1.40× slower | 201,658 |
| D: | 47.1 s | 47.1 s | 1.00× | — |
| E: | 43.6 s | 48.8 s | 1.12× slower | — |
| F: | 7.0 s | 11.0 s | 1.57× slower | — |
| M: | 24.1 s | 31.7 s | 1.32× slower | — |
| S: | 61.6 s | 86.8 s | 1.41× slower | — |
| **TOTAL** | **3m 9s** | **4m 3s** | **1.29× slower** | **106,695** |

> **Context:** The C++ times are warm (OS has cached MFT pages); the
> Rust times are cold (MFT read from disk + full parse + cache write).
> With the daemon running (HOT), Rust answers the same queries in
> **200 ms** — a **900× improvement** over the cold Rust path and
> **47× faster** than the warm C++ path.

---

## 10  Running Your Own Benchmarks

UFFS includes two profiling scripts in `scripts/windows/`:

### Full Benchmark (`benchmark.rs`)

Three-phase benchmark with multi-round statistics, per-drive isolation,
and multi-pattern HOT testing:

```bash
# Default: all drives, 3 rounds, patterns: *, *.txt, test
rust-script scripts/windows/benchmark.rs

# Specific drives, more rounds
rust-script scripts/windows/benchmark.rs --drives C,D --rounds 5

# HOT phase only with custom patterns
rust-script scripts/windows/benchmark.rs --phase hot --pattern "*.dll" --pattern "config"

# Non-Windows with offline MFT data
rust-script scripts/windows/benchmark.rs --data-dir ~/uffs_data
```

### Profile Script (`profile.rs`)

Detailed `--profile` output for each phase, showing daemon internals:

```bash
# Profile all drives
rust-script scripts/windows/profile.rs --drives C,D,E,F,G,M,S

# Single drive
rust-script scripts/windows/profile.rs --drives C
```

### Quick One-Off Profile

```bash
# Profile a single search
uffs "*.dll" --profile

# Benchmark mode (suppress output, measure engine only)
uffs "*.dll" --benchmark --limit 5
```

> **See also:** [Advanced Diagnostics](advanced-diagnostics.md) for
> `--profile`, `--benchmark`, and `--verbose` flag details.