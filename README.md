# UFFS — Ultra Fast File Search

[![License: MPL 2.0](https://img.shields.io/badge/License-MPL%202.0-brightgreen.svg)](https://opensource.org/licenses/MPL-2.0)
[![Rust](https://img.shields.io/badge/rust-1.91%2B-orange.svg)](https://www.rust-lang.org)

**The fastest open-source NTFS file search engine.** Reads the Master File Table directly, indexes 25.9 million files across 7 drives, and answers every query in ~200 ms.

> An open-source alternative to [Everything (voidtools)](https://www.voidtools.com/), [WizFile](https://antibody-software.com/wizfile/), [UltraSearch](https://www.jam-software.com/ultrasearch), and other NTFS search tools — built in Rust with Polars DataFrames, a background daemon, 40+ filters, and an MCP server for AI agents.

📖 **[Full User Manual](docs/user-manual/index.md)** — installation, tutorials, filters, daemon, TUI, MCP integration, and more.

> **Open source, forever.** The UFFS platform — engine, daemon, CLI, and MCP server — is licensed under the [Mozilla Public License 2.0](LICENSE). Code released as part of UFFS Core will never be made less open. Commercial products and enterprise offerings are built on top of the open platform, not by restricting it.

---

## Why UFFS?

Most file search tools ask the OS to enumerate files one at a time (`FindFirstFile`, `os.walk`).
UFFS **reads the NTFS Master File Table directly** — once — and holds it in memory using Polars DataFrames.

- ⚡ **172 million records/second** scan throughput (HOT daemon)
- 🔍 **40+ filters** — size, date, extension, type, attributes, path length, tree size, regex
- 🖥️ **CLI + TUI + MCP** — terminal, interactive UI, and AI-agent integration
- 🔄 **Background daemon** — loads once, answers every query in ~200 ms
- 🧩 **Cross-platform** — native NTFS on Windows; offline MFT analysis on macOS and Linux

---

## Benchmark (v0.4.107)

Measured on AMD Ryzen 9 3900XT, 64 GB RAM, 7 NTFS volumes (NVMe + SATA SSD + SATA HDD), 25.9M total records:

| Phase | What happens | ALL 7 drives | Single NVMe |
|-------|-------------|-------------:|------------:|
| **COLD** | Raw MFT read + index build | 66.5 s | 7.5 s |
| **WARM CACHE** | Load serialized `.iocp` cache | 7.3 s | 2.6 s |
| **HOT** | In-memory query (daemon running) | **381 ms** | **229 ms** |

Cold→Hot speedup: **175×** (all drives) · **259×** (8.3M-record HDD).

> 📖 **[Full benchmark data](docs/user-manual/performance.md)** — per-drive tables, profile internals, C++ parity comparison.

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
| `uffs-tui` | Terminal UI ([📖](docs/user-manual/tui-search-box.md)) |
| `uffs-mcp` | MCP server for AI agents ([📖](docs/user-manual/mcp.md)) |
| `uffs-polars` | Polars compilation-isolation facade |
| `uffs-client` | IPC client library |

---

## Alternatives & Landscape

UFFS was built after the author wrote [an earlier C++ MFT search tool](https://github.com/githubrobbi/Ultra-Fast-File-Search) and then rebuilt it from scratch in Rust for safety, performance, and maintainability.

### Measured speed vs alternatives

The [C++ predecessor](https://github.com/githubrobbi/Ultra-Fast-File-Search) was benchmarked head-to-head against Everything and WizFile on the same hardware (19M records, 1 SSD + 4 HDDs). The Rust rewrite then measured against the C++ version on 7 drives (25.9M records):

| Tool | Cold index (19M records) | Warm query | Source |
|------|-------------------------:|------------|--------|
| **WizFile** | 299 s (1 HDD, 6.5M) | — | [C++ benchmark](https://github.com/githubrobbi/Ultra-Fast-File-Search#benchmark) |
| **Everything** | 178 s | service keeps index hot | [C++ benchmark](https://github.com/githubrobbi/Ultra-Fast-File-Search#benchmark) |
| **UFFS C++** | 121 s | — | [C++ benchmark](https://github.com/githubrobbi/Ultra-Fast-File-Search#benchmark) |
| **UFFS Rust (cold)** | 66.5 s (25.9M, 7 drives) | — | [Parity test](docs/user-manual/performance.md#9--c-vs-rust-parity-comparison) |
| **UFFS Rust (HOT)** | — | **200 ms** (25.9M records) | [Benchmark](docs/user-manual/performance.md) |

Summary: **68% faster** than Everything at cold indexing · **47× faster** than C++ warm path on HOT queries · **172M records/sec** daemon-side scan throughput.

### How UFFS compares to other file search tools

| Category | Tools | How UFFS differs |
|----------|-------|-----------------|
| **Instant NTFS filename search** | [Everything (voidtools)](https://www.voidtools.com/), [WizFile](https://antibody-software.com/wizfile/), [WizTree](https://www.diskanalyzer.com/), [UltraSearch (JAM Software)](https://www.jam-software.com/ultrasearch), [SwiftSearch](https://sourceforge.net/projects/swiftsearch/), [Locate32](https://locate32.cogit.net/) | Open-source Rust engine; 68% faster cold indexing than Everything; Polars DataFrames; daemon + CLI + TUI + MCP; 40+ filters; forensic mode; cross-platform offline analysis |
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
