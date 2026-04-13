# UFFS — Ultra Fast File Search

[![CI](https://github.com/githubrobbi/UltraFastFileSearch/actions/workflows/ci.yml/badge.svg)](https://github.com/githubrobbi/UltraFastFileSearch/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/githubrobbi/UltraFastFileSearch?label=release)](https://github.com/githubrobbi/UltraFastFileSearch/releases/latest)
[![License: MPL 2.0](https://img.shields.io/badge/License-MPL%202.0-brightgreen.svg)](https://opensource.org/licenses/MPL-2.0)
[![Platform: Windows](https://img.shields.io/badge/platform-Windows-blue.svg)](https://github.com/githubrobbi/UltraFastFileSearch/releases/latest)

**A benchmark-driven NTFS search engine for Windows.** UFFS reads the Master File Table directly, builds a compact persisted index, and keeps large NTFS estates searchable through a background daemon.

> Proven on a real 7-drive, 25.9M-record Windows system:
> - **66.5 s COLD** — raw MFT read + compact index build
> - **7.3 s WARM CACHE** — restart from serialized cache
> - **381 ms HOT** — end-to-end query with a running daemon
> - **151 ms daemon-side scan** across all 25.9M records

UFFS is built for **exact filename, path, and metadata search** at scales where directory walking, shell search, and some automation surfaces become the bottleneck. It is open source, written in Rust, and designed first for deterministic local search; CLI, TUI, API, and MCP are all interfaces on top of the same engine.

> An open-source NTFS search engine for Windows power users, developers, IT teams, and investigations-style workflows.

📖 **[Full User Manual](docs/user-manual/index.md)** — installation, tutorials, filters, daemon, TUI, MCP integration, and more.

> **Open source, forever.** The UFFS platform — engine, daemon, CLI, and MCP server — is licensed under the [Mozilla Public License 2.0](LICENSE). Code released as part of UFFS Core will never be made less open. Commercial products and enterprise offerings are built on top of the open platform, not by restricting it.

---

## Why UFFS?

- ⚡ **25.9M-record proven scale** — measured across 7 NTFS volumes on real hardware
- 🚀 **Cold / warm / hot architecture** — build once from raw MFT, restart fast from cache, answer hot queries from memory
- 🔍 **40+ filters** — size, date, extension, type, attributes, path length, tree size, regex
- 🧩 **One engine, multiple interfaces** — CLI, TUI, daemon, API, and MCP share the same index
- 🧭 **Deterministic local scope** — built for exact NTFS filename/path/metadata search, not fuzzy ranking
- 🖥️ **Cross-platform offline analysis** — live NTFS on Windows; offline MFT analysis on macOS and Linux

---

## Benchmark snapshot (v0.4.106)

Measured on AMD Ryzen 9 3900XT, 64 GB RAM, Windows 11 Pro 24H2, 7 NTFS volumes totaling 25,929,744 records. Query: `*`, limit: 100, averages over 3 rounds.

| Phase | What happens | ALL 7 drives | Single NVMe |
|-------|--------------|-------------:|------------:|
| **COLD** | Raw MFT read, parse, compact index build, cache write | 66.5 s | 7.5 s |
| **WARM CACHE** | Daemon restart + serialized cache load | 7.3 s | 2.6 s |
| **HOT** | Query a running daemon with the index already in memory | **381 ms end-to-end** | **229 ms end-to-end** |

Hot-path context:
- **151 ms daemon-side search** across all 25.9M records
- **211–259 ms** end-to-end hot queries on single drives
- **174.5×** cold→hot speedup across all 7 drives

> 📖 **[Full benchmark data](docs/user-manual/performance.md)** — methodology, per-drive tables, profile internals, validation throughput, and caveats.

---

## Quick Start

```bash
# Build from source (requires Rust nightly — see rust-toolchain.toml)
cargo build --release

# Search all drives (daemon starts automatically)
uffs "*.rs"

# Search a specific drive
uffs "*.txt" --drive C

# Filter by size, date, type
uffs "*.log" --min-size 100MB --newer 7d --files-only

# macOS/Linux: search offline MFT captures
uffs "*.txt" --data-dir ~/uffs_data

# Daemon management
uffs daemon status
uffs daemon restart
```

> 📖 **[Installation](docs/user-manual/installation.md)** · **[5-minute tutorial](docs/user-manual/getting-started.md)** · **[CLI reference](docs/user-manual/cli-overview.md)** · **[40+ filters](docs/user-manual/filters.md)**

---

## How It Works

1. **Read** — Opens the raw NTFS volume and reads the MFT sequentially using IOCP with a sliding window. Bitmap skip eliminates 40–55% of I/O by skipping deleted records.
2. **Parse** — Each I/O buffer is parsed inline into a compact 224-byte `FileRecord` — zero intermediate copies, zero per-record heap allocations. On NVMe, Rayon parallelizes parsing across all CPU cores.
3. **Index** — Records are loaded into Polars DataFrames with an inverted extension index for 50–200× faster `*.ext` queries.
4. **Serve** — A background daemon holds the index in memory and answers queries via IPC. CLI, TUI, and MCP clients all share the same daemon.

> 📖 **[Architecture deep-dive](docs/architecture/engine/01-overview.md)** — 11 documents covering every subsystem.

---

## Architecture

| Crate | Role |
|-------|------|
| `uffs-mft` | Direct MFT reading → Polars DataFrame ([📖](crates/uffs-mft/README.md)) |
| `uffs-core` | Query engine (Polars lazy API) |
| `uffs-daemon` | Background index server ([📖](docs/user-manual/daemon.md)) |
| `uffs-cli` | Command-line interface ([📖](docs/user-manual/cli-overview.md)) |
| `uffs-mcp` | MCP server for AI agents ([📖](docs/user-manual/mcp.md)) |
| `uffs-polars` | Polars compilation-isolation facade |
| `uffs-client` | IPC client library |

---

## Alternatives & Landscape

UFFS was built after the author wrote [an earlier C++ MFT search tool](https://github.com/githubrobbi/Ultra-Fast-File-Search) and then rebuilt it from scratch in Rust for safety, performance, and maintainability.

### Comparison scope

UFFS competes first in the **local NTFS filename/path/metadata** lane: exact search across large Windows filesystems with deterministic scope and a reusable in-memory daemon.

We do **not** collapse all search products into one "fastest search tool" claim. The following are different benchmark classes and should be compared separately:

1. **Readiness** — cold build, warm restart, and hot query
2. **Interactive search** — end-to-end top-N query latency
3. **Bulk retrieval** — time to stream or export large result sets
4. **Scale ceiling** — largest corpus completed without timeout, crash, or incorrect results

That distinction matters because a tool can be excellent at interactive top-N search and still hit a wall during full-result export or very large automation workloads.

The older C++ implementation remains useful as a parity and regression baseline, but it is **not** the headline market benchmark for the Rust engine. Public cross-tool comparisons should be run against the current Rust engine with exact versions, settings, workloads, and raw results published alongside the charts.

### How UFFS compares to other file search tools

| Category | Tools | How UFFS differs |
|----------|-------|-----------------|
| **Instant NTFS filename search** | [Everything (voidtools)](https://www.voidtools.com/), [WizFile](https://antibody-software.com/wizfile/), [WizTree](https://www.diskanalyzer.com/), [UltraSearch (JAM Software)](https://www.jam-software.com/ultrasearch), [SwiftSearch](https://sourceforge.net/projects/swiftsearch/), [Locate32](https://locate32.cogit.net/) | Open-source Rust engine; 25.9M-record proven scale; Polars DataFrames; daemon + CLI + TUI + MCP; 40+ filters; forensic mode; cross-platform offline analysis |
| **Content / regex search** | [FileLocator Pro / Agent Ransack](https://www.mythicsoft.com/filelocatorpro/), [grepWin](https://tools.stefankueng.com/grepWin.html), [AstroGrep](http://astrogrep.sourceforge.net/), [dnGrep](https://dngrep.github.io/), [SearchMyFiles (NirSoft)](https://www.nirsoft.net/utils/search_my_files.html) | UFFS focuses on MFT-level metadata speed; pairs well with `ripgrep` for content |
| **Enterprise / eDiscovery** | [X1 Search](https://www.x1.com/), [dtSearch](https://www.dtsearch.com/), [Copernic](https://copernic.com/) | UFFS is a specialist local-NTFS tool, not a multi-repository governance platform |
| **Developer CLI** | [fd](https://github.com/sharkdp/fd), [ripgrep](https://github.com/BurntSushi/ripgrep), [fzf](https://github.com/junegunn/fzf), [GNU find](https://www.gnu.org/software/findutils/) | UFFS reads the MFT instead of walking directories — orders of magnitude faster for whole-drive search |
| **Forensic MFT tools** | [MFTECmd (Eric Zimmerman)](https://ericzimmerman.github.io/), [analyzeMFT](https://github.com/dkovar/analyzeMFT) | UFFS is an interactive search engine, not a one-shot parser; includes daemon, TUI, and live queries |
| **Linux / macOS** | [FSearch](https://github.com/cboxdoerfer/fsearch), [Recoll](https://www.recoll.org/), [DocFetcher](https://docfetcher.sourceforge.net/), [Catfish](https://docs.xfce.org/apps/catfish/start), [Find Any File](https://findanyfile.app/), [HoudahSpot](https://www.houdah.com/houdahSpot/) | UFFS supports offline MFT analysis on macOS/Linux via cached index files |

> 📖 **[Full competitor landscape analysis](docs/mft_competitor_landscape_deep_research.md)** — 12 tools, corporate adoption data, market positioning.

---

## Requirements

- **Windows** for live NTFS MFT reading (Administrator privileges required)
- **macOS / Linux** for offline MFT analysis (no admin needed)
- **Rust 1.91+** (Edition 2024, nightly required) to build from source

---

## Documentation

| Topic | Link |
|-------|------|
| Installation | [docs/user-manual/installation.md](docs/user-manual/installation.md) |
| Getting started (5 min) | [docs/user-manual/getting-started.md](docs/user-manual/getting-started.md) |
| CLI overview & examples | [docs/user-manual/cli-overview.md](docs/user-manual/cli-overview.md) |
| 40+ search filters | [docs/user-manual/filters.md](docs/user-manual/filters.md) |
| Daemon management | [docs/user-manual/daemon.md](docs/user-manual/daemon.md) |
| TUI interactive search | [docs/user-manual/tui-search-box.md](docs/user-manual/tui-search-box.md) |
| MCP server (AI agents) | [docs/user-manual/mcp.md](docs/user-manual/mcp.md) |
| Performance & benchmarks | [docs/user-manual/performance.md](docs/user-manual/performance.md) |
| Cache & data sources | [docs/user-manual/cache-and-data.md](docs/user-manual/cache-and-data.md) |
| Architecture (11 docs) | [docs/architecture/engine/](docs/architecture/engine/) |
| FAQ | [docs/user-manual/faq.md](docs/user-manual/faq.md) |
| Troubleshooting | [docs/user-manual/troubleshooting.md](docs/user-manual/troubleshooting.md) |

---

## Contributing

Start with [CONTRIBUTING.md](CONTRIBUTING.md) for the pinned toolchain, `just`/`cargo` workflows, and Windows/Admin caveats. For the broader docs map, see [docs/README.md](docs/README.md) and [docs/dev/README.md](docs/dev/README.md).

---

## License

UFFS is licensed under the [Mozilla Public License 2.0 (MPL-2.0)](LICENSE).

You can use, modify, and distribute UFFS freely. If you modify MPL-covered source files and distribute the result, those file-level changes must remain under MPL-2.0. Building proprietary applications on top of UFFS does not require opening your application.

See [LICENSES/MPL-2.0.txt](LICENSES/MPL-2.0.txt) for the full license text and [Mozilla's MPL FAQ](https://www.mozilla.org/en-US/MPL/2.0/FAQ/) for plain-language guidance.

---

## Acknowledgments

UFFS benefits from the broader NTFS tooling ecosystem, including [SwiftSearch](https://sourceforge.net/projects/swiftsearch/) by wfunction.

**Author:** Robert Nio · [github.com/githubrobbi/UltraFastFileSearch](https://github.com/githubrobbi/UltraFastFileSearch)
