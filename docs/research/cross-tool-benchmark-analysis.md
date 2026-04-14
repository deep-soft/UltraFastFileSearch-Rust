# Cross-Tool Benchmark Analysis

**Date:** 2026-04-14
**Version:** UFFS v0.5.4 (Rust), UFFS v0.4.x (C++), Everything 1.4 (es.exe)
**System:** AMD Ryzen 9 3900XT (12c/24t), 64 GB DDR4, Windows 11 Pro 24H2

## 1  Initial Results (unfair — output format asymmetry)

The first benchmark run revealed a critical methodological flaw:
all three tools wrote different amounts of data per row.

| Tool | Columns per row | Est. bytes/row | ext_dll (165K rows) |
|------|----------------|----------------|---------------------|
| Everything (es.exe) | 1 (Filename) | ~80 B | ~13 MB |
| UFFS C++ (uffs.com) | 25 (all) | ~300 B | ~50 MB |
| UFFS Rust (uffs.exe) | 34 (all) | ~400 B | ~66 MB |

UFFS Rust was writing **5× more data** than Everything per row.
This inflated timings by 3–5× on I/O-bound bulk exports.

### Raw results (unfair, 10 rounds, all columns)

| Drive | Pattern | UFFS HOT p50 | C++ p50 | Everything p50 | UFFS vs ES |
|-------|---------|-------------|---------|----------------|------------|
| C: | exact (26 rows) | 170 ms | 4.0 s | 79 ms | 2.2× slower |
| C: | prefix (37K rows) | 610 ms | 2.9 s | 111 ms | 5.5× slower |
| C: | ext_rare (1 row) | 169 ms | 11.8 s | 62 ms | 2.7× slower |
| C: | ext_dll (166K rows) | 793 ms | 12.2 s | 265 ms | 3.0× slower |
| C: | substring (28K rows) | 479 ms | 3.6 s | 118 ms | 4.1× slower |
| C: | full_scan (3.5M rows) | 11.3 s | 12.1 s | SKIP | — |
| D: | exact (3 rows) | 173 ms | 23.1 s | 64 ms | 2.7× slower |
| D: | prefix (15K rows) | 341 ms | 21.9 s | 73 ms | 4.7× slower |
| D: | ext_rare (11 rows) | 174 ms | 44.4 s | 60 ms | 2.9× slower |
| D: | ext_dll (45K rows) | 664 ms | 44.3 s | 120 ms | 5.5× slower |
| D: | substring (21K rows) | 407 ms | 22.3 s | 87 ms | 4.7× slower |
| D: | full_scan (7.1M rows) | 21.6 s | 40.3 s | SKIP | — |

### Key findings from unfair run

1. **Everything is 2–6× faster than UFFS HOT** on targeted queries —
   but this includes a ~5× I/O advantage (1 column vs 34 columns).
2. **UFFS HOT is 8–134× faster than C++** on targeted queries.
3. **UFFS HOT beats C++ on full_scan** (11.3 s vs 12.1 s on C:,
   21.6 s vs 40.3 s on D:) — despite outputting more columns.
4. **Everything cannot do full_scan** — 2 GB IPC memory limit.

## 2  Row Count Discrepancies

All three tools find different numbers of files on the same drive:

| Pattern | UFFS Rust | C++ | Everything | Notes |
|---------|-----------|-----|------------|-------|
| full_scan C: | 3,532,559 | 3,372,892 | SKIP | C++ missing ~160K |
| exact C: | 26 | 20 | 26 | C++ missing 6 |
| prefix C: | 36,803 | **0** | 36,637 | C++ bug? |
| ext_dll C: | 165,911 | 144,261 | 165,911 | C++ missing ~22K |
| substring C: | 27,527 | 25,983 | 26,942 | all differ |
| full_scan D: | 7,065,992 | 7,065,756 | SKIP | close match |

**Likely causes:**
- C++ skips MFT extension records (hardlinks, ADS) → fewer files
- C++ `win*` prefix returns 0 — possible glob handling bug
- Everything doesn't index `$`-prefixed NTFS metafiles → slightly fewer
- UFFS Rust is the most complete (indexes all MFT records)

## 3  Fairness Fix: Path-Only Output

Fixed the benchmark to use path-only output for all three tools:

- `uffs.exe --columns Path` → 1 column
- `uffs.com --columns=path` → 1 column
- `es.exe -export-csv` → already 1 column (Filename)

This eliminates the I/O asymmetry. **Re-run needed** to get fair numbers.

## 4  Architectural Differences Affecting Timing

| Factor | Everything | UFFS Rust | Impact |
|--------|-----------|-----------|--------|
| Daemon model | Windows service (always hot) | Auto-start daemon | ES: 0 ms startup; UFFS: ~170ms IPC overhead |
| IPC mechanism | Named pipe (lightweight) | Named pipe + serde | ES: raw bytes; UFFS: serialized SearchRow |
| Index format | Proprietary B-tree | Polars DataFrame + trigram | ES optimized for lookup; UFFS for analytics |
| File write | Direct write | BufWriter + CSV formatting | ES: simpler I/O path |
| Result building | Minimal (path only) | Full DisplayRow (30+ fields) | UFFS builds more per result even with --columns Path |
| Process model | es.exe = thin client | uffs.exe = thick client | es.exe likely faster process spawn |

## 5  Optimization Opportunities

### 5.1 Quick wins (expected 2–3× improvement)

1. **Lazy field resolution** — only resolve fields requested by `--columns`.
   Currently UFFS builds ALL 30+ fields in DisplayRow, then throws
   away the ones not requested. For path-only, skip size/dates/flags.
2. **Streaming file write** — write each row directly to BufWriter
   as it's matched, instead of collecting all DisplayRows in memory
   first and then writing. Eliminates the 2× memory overhead.
3. **Skip SearchRow serialization** — for `--out=file.csv`, write
   directly from daemon index to file, bypassing IPC entirely.

### 5.2 Medium-term (expected 5–10× for targeted queries)

4. **In-daemon file export** — the daemon already has the index in
   memory. Export directly from the daemon process to the output file,
   eliminating IPC transfer entirely for bulk exports.
5. **SIMD path assembly** — the path resolution (parent chain walk)
   is the dominant CPU cost for large result sets. SIMD memcpy of
   path segments could improve throughput.

### 5.3 Stretch goals

6. **Memory-mapped result passing** — shared memory for large result
   sets instead of pipe-based IPC.
7. **Columnar export** — write Parquet instead of CSV for downstream
   analytics workflows.

## 6  Next Steps

- [ ] Re-run benchmark with path-only output (fair comparison)
- [ ] Profile UFFS HOT to identify where time is spent per-query
- [ ] Implement lazy field resolution (5.1.1)
- [ ] Implement streaming file write (5.1.2)
- [ ] Re-benchmark after optimizations
