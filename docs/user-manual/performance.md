# Performance

UFFS is designed to search millions of files in milliseconds.  This page
documents measured performance across seven NTFS drives totalling
**25.9 million records**, captured on real hardware with the standard
benchmark and profiling scripts.

> **See also:** [Advanced Diagnostics](advanced-diagnostics.md) ·
> [Daemon](daemon.md) · [Cache & Data Sources](cache-and-data.md) ·
> [Concepts](concepts.md)

---

## What these numbers mean

This page intentionally separates **readiness** from **interactive query latency**.

- **COLD** measures raw MFT read, parse, compact index build, and cache write.
- **WARM CACHE** measures daemon restart from the serialized cache.
- **HOT** measures interactive search against a running in-memory daemon.

Unless explicitly noted otherwise, the headline tables on this page measure **end-to-end CLI latency** for the query `*` with `--limit 100`. That means the HOT numbers are **interactive top-N timings**, not full-result export timings. Daemon-side search time is reported separately from process spawn, IPC, and stdout formatting.

This separation is deliberate. Different tools and interfaces optimize different workloads. UFFS therefore treats readiness, interactive search, bulk retrieval, and scale ceiling as separate benchmark classes instead of forcing them into one number.

---

## Benchmark classes and fairness rules

When UFFS publishes cross-tool benchmarks, the rules are:

1. Compare like-for-like workloads only.
2. Report exact hardware, OS build, tool versions, settings, and query shape.
3. Keep interactive top-N and bulk export benchmarks separate.
4. Report daemon-side timings and end-to-end client timings separately.
5. Record crashes, timeouts, OOMs, interface limits, and incomplete results as **DNF**, not as missing data.

A benchmark is only useful if readers can see both the fastest successful run and the point where a tool stops being operational.

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
| UFFS version | 0.5.4 |

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

> **Corpus note:** The 25.9M-record benchmark corpus is a live working Windows machine, so per-drive record counts can drift slightly between runs as files are created, deleted, or updated. Minor differences of a few thousand records across tables reflect different benchmark passes on the same system, not different methodology.
>
> **Scale-ceiling note:** The 42.5M–100.4M tiers are constructed by adding offline MFT clones to the live 7-drive corpus on the same machine. They are scale-ceiling workloads, not additional live volumes.

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
↓                             Results                       (6–163 ms)
Results
(seconds to minutes)          (~0.6–6.9 s)
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

| Drive | Records | Total | Startup | Search |
|-------|--------:|------:|--------:|-------:|
| G: | 15,094 | 1.3 s | 540 ms | <1 ms |
| F: | 2,221,347 | 4.3 s | 3.6 s | 13 ms |
| C: | 3,512,541 | 7.7 s | 6.8 s | 30 ms |
| M: | 1,908,809 | 26.4 s | 24.9 s | 11 ms |
| D: | 7,066,020 | 28.6 s | 27.1 s | 94 ms |
| E: | 2,929,523 | 42.5 s | 41.7 s | 18 ms |
| S: | 8,278,106 | 67 s | 65 s | 99 ms |
| **ALL** | **25,931,436** | **66 s** | **65 s** | **235 ms** |

> **Note:** M: and S: are SATA spinning disks where I/O is the
> bottleneck — raw MFT reads are bound by HDD seek time, not CPU.
> NVMe drives (C:, F:) achieve 470K+ records/sec.
> The ALL-drives cold start runs all drives in parallel, so total time
> ≈ slowest individual drive.

### Warm Cache (Serialized .iocp Load)

| Drive | Records | Total | Speedup vs Cold |
|-------|--------:|------:|----------------:|
| G: | 15,094 | 572 ms | 2.3× |
| F: | 2,221,347 | 1.4 s | 3.1× |
| M: | 1,908,809 | 1.4 s | 19.5× |
| E: | 2,929,523 | 2.4 s | 18.0× |
| C: | 3,512,541 | 6.4 s | 1.2× |
| D: | 7,066,020 | 6.4 s | 4.4× |
| S: | 8,278,106 | 4.8 s | 14.0× |
| **ALL** | **25,931,436** | **6.9 s** | **9.6×** |

### Hot (In-Memory Query)

| Drive | Records | Total | Cold→Hot |
|-------|--------:|------:|---------:|
| G: | 15,094 | 6 ms | **219×** |
| M: | 1,908,809 | 18 ms | **1469×** |
| F: | 2,221,347 | 19 ms | **229×** |
| E: | 2,929,523 | 24 ms | **1771×** |
| C: | 3,512,541 | 27 ms | **284×** |
| D: | 7,066,020 | 49 ms | **584×** |
| S: | 8,278,106 | 54 ms | **1236×** |
| **ALL** | **25,931,436** | **163 ms** | **407×** |

> **Note:** HOT timings include process startup overhead
> (spawning `uffs.exe`, connecting to daemon via IPC, formatting output).
> The actual daemon-side search takes **0–155 ms** depending on drive
> count and pattern (see §6 Profile Internals).

---


## 4  Speedup Summary

The table below shows end-to-end speedup from cold start to hot query
for each drive.  The Cold→Hot ratio is the primary performance metric.

| Drive | Cold | Warm | Hot | Cold→Hot | Cold→Warm |
|-------|-----:|-----:|----:|---------:|----------:|
| C: | 7.7 s | 6.4 s | 27 ms | **284×** | 1.2× |
| D: | 28.6 s | 6.4 s | 49 ms | **584×** | 4.4× |
| E: | 42.5 s | 2.4 s | 24 ms | **1771×** | 18.0× |
| F: | 4.3 s | 1.4 s | 19 ms | **229×** | 3.1× |
| G: | 1.3 s | 572 ms | 6 ms | **219×** | 2.3× |
| M: | 26.4 s | 1.4 s | 18 ms | **1469×** | 19.5× |
| S: | 67 s | 4.8 s | 54 ms | **1236×** | 14.0× |
| **ALL** | **66 s** | **6.9 s** | **163 ms** | **407×** | **9.6×** |

> On spinning disks (M:, S:) the cold-start penalty is extreme —
> reading raw MFT from a HDD is 10–60× slower than NVMe.
> The daemon eliminates this entirely: once loaded, every drive
> responds in **6–54 ms** regardless of media type.

---

## 5  HOT Query Patterns

Different search patterns exercise different code paths in the query
engine.  The benchmark tests eight representative patterns against a
hot daemon across all drives (25.9M records, 30 rounds):

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

> **Observations:**
> - Targeted patterns (exact name, prefix, extension, substring, combined)
>   return in **9–11 ms e2e** — all daemon-side work completes in **0–1 ms**.
> - Only unfiltered `*` scans and date/size filters touch the full DataFrame;
>   these scale linearly with record count.
> - Filtered queries stay flat at ~10 ms regardless of corpus size.

---

## 6  Profile Internals

The `--profile` flag breaks down where time is spent inside the daemon.
This data is from a hot daemon with all 7 drives loaded (25.9M records):

| Component | Time | Notes |
|-----------|-----:|-------|
| Process spawn | ~8 ms | OS creates `uffs.exe` process |
| IPC connect | 1 ms | Named pipe handshake |
| Daemon search | 155 ms | Scan 25.9M records across 7 drives |
| IPC transfer | <1 ms | Send result rows back |
| Row conversion | <1 ms | Deserialize rows |
| Output formatting | ~5 ms | Format and write to stdout |
| **Total** | **~163 ms** | |

> The daemon-side search (155 ms for 25.9M records) translates to
> **167 million records/second** scan throughput.  Targeted queries
> skip the full scan entirely and return in **0–1 ms daemon-side**.

### Per-Drive Profile (Cold Start)

| Drive | Records | Cold Total | Startup | Search |
|-------|--------:|-----------:|--------:|-------:|
| C: | 3,512,541 | 7.7 s | 6.8 s | 30 ms |
| D: | 7,066,020 | 28.6 s | 27.1 s | 94 ms |
| E: | 2,929,523 | 42.5 s | 41.7 s | 18 ms |
| F: | 2,221,347 | 4.3 s | 3.6 s | 13 ms |
| G: | 15,094 | 1.3 s | 540 ms | <1 ms |
| M: | 1,908,809 | 26.4 s | 24.9 s | 11 ms |
| S: | 8,278,106 | 67 s | 65 s | 99 ms |

> Cold-start time is dominated by **MFT read + parse** (the "Startup"
> column).  Search itself is always <100 ms even on 8M records.

---

## 7  Bulk Retrieval Throughput

Bulk retrieval measures how fast UFFS can export large result sets.
Two output modes are tested: shell pipe (stdout) and direct file write (`--out-dir`).

### CSV Export — Live Drives (7 drives, 25.9M records, `--out-dir`)

| Tier | Rows | Avg Time | Rows/sec |
|------|-----:|---------:|---------:|
| 100 | 101 | 213 ms | 474/s |
| 1k | 1,001 | 202 ms | 5.0k/s |
| 10k | 10,001 | 323 ms | 31k/s |
| 100k | 100,001 | 1.4 s | 73k/s |
| 1M | 1,000,001 | 3.4 s | 292k/s |
| ALL (per-drive) | 8.3M | 25.6 s | **326k/s** |

### Pipe vs Direct File Write

| Mode | 8.3M rows | Rows/sec | Relative |
|------|----------:|---------:|---------:|
| Pipe (stdout) | 68 s | 122k/s | 1.0× |
| `--out-dir` | 25.6 s | 323k/s | **2.6×** |

> **Recommendation:** For exports exceeding ~100k rows, use `--out-dir`
> to bypass the shell pipe bottleneck.

### CSV vs JSON

Format makes no material difference to throughput.  Both CSV and JSON
achieve comparable rows/sec at each tier — the bottleneck is query
evaluation and IPC, not serialization.

---

## 8  Scale Ceiling

The scale ceiling test loads progressively larger MFT collections
(cloned offline drives + live drives) and measures interactive
search latency at each tier.

### Results (interactive search, `--limit 100`, 30 rounds per tier)

| Total Records | Drives | `*` e2e p50 | `*` e2e p95 | targeted p50 | Status |
|--------------:|-------:|------------:|------------:|-------------:|--------|
| 25.9M | 7 | 161 ms | 183 ms | 9–10 ms | ✅ |
| 42.5M | 9 | 259 ms | 312 ms | 9–10 ms | ✅ |
| 59.0M | 11 | 471 ms | 502 ms | 10–12 ms | ✅ |
| 75.6M | 13 | 600 ms | 626 ms | 10–12 ms | ✅ |
| 92.2M | 15 | 670 ms | 731 ms | 11–14 ms | ✅ |
| **100.4M** | **16** | **808 ms** | **855 ms** | **11–13 ms** | **✅** |
| >100M | 17+ | — | — | — | ❌ OOM |

> **Key insight:** Targeted queries (exact name, prefix, extension,
> substring, combined) stay at **0–3 ms daemon-side** regardless of
> corpus size.  Only unfiltered `*` scans and temporal/size filters
> scale linearly with total records.
>
> The OOM at >100M records is a memory ceiling on this test machine
> (64 GB DDR4).  Each MFT record occupies ~640 bytes in the in-memory
> DataFrame.

---

## 9  Validation Suite Throughput

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

## 10  Daemon Runtime Statistics

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

## 11  C++ vs Rust Parity Comparison

UFFS was rewritten from C++ to Rust.  The parity test runs both
implementations on the same drives and compares output.  The Rust
implementation reads raw MFT cold (no cache, no daemon), while the
C++ baseline runs warm:

| Drive | C++ (warm) | Rust (cold) | Ratio | Rust records/sec |
|-------|----------:|------------:|------:|-----------------:|
| C: | 12.4 s | 17.4 s | 1.40× slower | 201,658 |
| D: | 39.8 s | 47.1 s | 1.18× slower | 150,015 |
| E: | 43.6 s | 48.8 s | 1.12× slower | 59,998 |
| F: | 7.0 s | 11.0 s | 1.57× slower | 202,343 |
| M: | 24.1 s | 31.7 s | 1.31× slower | 60,160 |
| S: | 1m 1.6 s | 1m 26.8 s | 1.41× slower | 95,326 |
| **TOTAL** | **3m 8.6 s** | **4m 2.9 s** | **1.29× slower** | **106,695** |

> **Context:** The C++ times are warm (OS has cached MFT pages); the
> Rust times are cold (MFT read from disk + full parse + cache write).
> With the daemon running (HOT), Rust answers the same queries in
> **163 ms end-to-end** (all 7 drives) — a **407× speedup** over the cold Rust path.
> The C++ tool re-reads the MFT on every invocation; the Rust daemon
> never needs to re-read after the initial cold build.

---

## 12  Running Your Own Benchmarks

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