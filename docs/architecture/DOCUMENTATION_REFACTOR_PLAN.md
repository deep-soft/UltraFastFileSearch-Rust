# UFFS Documentation Refactor Plan

> **Goal:** Bring the user-manual documentation to world-class level — accurate,
> complete, well-organized, and suitable for open-source publication.
>
> **Scope (Phase 1):** CLI capabilities only.
> **Future phases:** MCP server (STDIO / HTTP), TUI, GUI.
>
> **Date:** 2026-04-11 (plan) · 2026-04-12 (Phase 1 complete)
> **Status:** ✅ **PHASE 1 COMPLETE** — 18/18 steps executed.  All 9 new pages
> created, 7 existing pages updated, cross-links verified, README updated.

---

## Table of Contents

1. [Current State Assessment](#1--current-state-assessment)
2. [Gap Analysis](#2--gap-analysis)
3. [Target Documentation Structure](#3--target-documentation-structure)
4. [Per-File Refactor Actions](#4--per-file-refactor-actions)
5. [New Pages to Create](#5--new-pages-to-create)
6. [Cross-Cutting Concerns](#6--cross-cutting-concerns)
7. [Validation Checklist](#7--validation-checklist)
8. [Execution Order](#8--execution-order)

---

## 1  Current State Assessment

### 1.1  What Exists (docs/user-manual/) — Post-Refactor

| File | Lines | Status | Changes Made |
|------|------:|--------|-------------|
| `index.md` | 112 | ✅ **NEW** | Landing page with reading order diagram, page table, "I want to…" quick links |
| `installation.md` | 135 | ✅ **NEW** | Build from source, prerequisites, PATH setup, cross-compilation |
| `getting-started.md` | 199 | ✅ **NEW** | 5-minute tutorial, output explanation, daemon intro, 50+ recipes |
| `cli-overview.md` | ~320 | ✅ **Refactored** | Trimmed to hub; removed examples gallery; added missing flags, `uffs agg`/`mcp`/`status` subcommands, inline aggregation section |
| `search-modes.md` | 443 | ✅ **Polished** | Updated cross-links |
| `filters.md` | ~720 | ✅ **Updated** | Added `--between`, `--exact-size`, `--exact-size-on-disk`, `--exact-descendants`, `--hide-ads`, named time specs table |
| `sorting.md` | 322 | ✅ **Polished** | Updated cross-links |
| `output-formats.md` | 184 | ✅ **NEW** | CSV/JSON/table format details, column reference, scripting patterns |
| `aggregation.md` | ~520 | ✅ **Updated** | Fixed `uffs search` syntax, added inline aggregation flags section |
| `concepts.md` | ~375 | ✅ **Updated** | Added §10 Alternate Data Streams, §11 MFT explanation, glossary cross-link |
| `daemon.md` | ~260 | ✅ **Expanded** | Added auto-start, idle retirement, logging, platform differences, IPC paths, troubleshooting |
| `cache-and-data.md` | 148 | ✅ **NEW** | Data pipeline, .iocp format, data-dir layout, macOS/Linux setup |
| `advanced-diagnostics.md` | 125 | ✅ **NEW** | --profile, --benchmark, env vars, query modes, parity testing |
| `troubleshooting.md` | 135 | ✅ **NEW** | 9 common issues with causes and fixes |
| `faq.md` | 143 | ✅ **NEW** | 13 frequently asked questions |
| `glossary.md` | 46 | ✅ **NEW** | 30 terms from ADS to Treesize |
| `mcp.md` | 502 | — | Out of scope (Phase 2) |
| `tui-search-box.md` | 409 | — | Out of scope (Phase 3) |

### 1.2  Overall Quality Rating — Post-Refactor

**Resolved (was weakness, now addressed):**
- ✅ Landing page (`index.md`) with reading order and quick links.
- ✅ Installation + getting-started guides.
- ✅ All CLI flags from `args.rs` documented (verified against source).
- ✅ Troubleshooting, FAQ, and glossary pages.
- ✅ Output format deep dive with CSV/JSON/table samples.
- ✅ Daemon page expanded: auto-start, retirement, logging, platforms, troubleshooting.
- ✅ Aggregation examples use correct `uffs` syntax (not `uffs search`).
- ✅ Cross-links verified (all 17 .md targets resolve).
- ✅ Information architecture with progressive disclosure (Getting Started → Core → Advanced → Reference).
- ✅ README.md links to user manual.

**Remaining (minor, future work):**
- Version badges not added to individual pages (low priority — the index.md notes the version).
- Some example output is representative rather than captured from a live run (macOS build cannot run live NTFS).
- Phase 2 (MCP deep dives) and Phase 3 (TUI/GUI) not yet started.

---

## 2  Gap Analysis

### 2.1  Undocumented CLI Flags (present in `args.rs`, missing from docs)

| Flag | Category | Current Doc Status |
|------|----------|-------------------|
| `--between START,END` | Date filter | **Missing entirely** — shorthand for `--newer START --older END` |
| `--exact-size <SIZE>` | Size filter | Listed in `cli-overview.md` but **not in `filters.md`** |
| `--exact-size-on-disk <SIZE>` | Size filter | Listed in `cli-overview.md` §3 but **not detailed in `filters.md`** with its own section |
| `--exact-descendants <N>` | Descendant filter | **Missing entirely** |
| `--hide-ads` | Scope filter | Mentioned once in `cli-overview.md` but **not in `filters.md`** |
| `--no-bitmap` | Diagnostic | Listed in `cli-overview.md` §7 but no explanation of what it does |
| `--query-mode` | Diagnostic | Listed in `cli-overview.md` §7 but no real explanation |
| `--no-cache` | Data source | Mentioned but not explained (what is the cache? where? when to use?) |
| `--benchmark` | Diagnostic | Listed but not explained |
| `--profile` | Diagnostic | Listed but not explained |
| `--parity-compat` | Diagnostic | Listed but not explained |
| `--count` | Aggregation shorthand | Exists in `args.rs`, partially documented in `aggregation.md` |
| `--facet` | Aggregation shorthand | Exists in `args.rs`, partially documented in `aggregation.md` |
| `--stats` | Aggregation shorthand | Exists in `args.rs`, partially documented in `aggregation.md` |
| `--histogram` | Aggregation shorthand | Exists in `args.rs`, partially documented in `aggregation.md` |
| `--rows` | Aggregation modifier | Exists in `args.rs`, documented in `aggregation.md` |
| `--agg-cursor` | Aggregation | Documented in `aggregation.md` |
| `--agg-page-size` | Aggregation | Documented in `aggregation.md` |
| `--path-contains` | Path filter | Referenced in `aggregation.md` recipe but **not in args.rs** — verify if this is a real flag or daemon-only |
| `uffs status` | Subcommand | `SystemStatus` in args.rs — **not documented anywhere** |
| `uffs agg` alias | Subcommand | `aggregate` has `alias = "agg"` — not mentioned in docs |

### 2.2  Missing Documentation Pages

| Page | Why Needed |
|------|-----------|
| **`index.md` (landing page)** | Every well-structured manual needs a table of contents / entry point |
| **`installation.md`** | Build from source, platform requirements, binary locations, PATH setup |
| **`getting-started.md`** | First 5 minutes: install → first search → understanding output |
| **`output-formats.md`** | Deep dive: CSV, JSON, table, custom; column selection; output-to-file; examples of actual output |
| **`cache-and-data.md`** | Explain .uffs cache files, --no-cache, --data-dir, --mft-file, platform-specific paths |
| **`troubleshooting.md`** | Common errors, permission issues, daemon won't start, stale PID files, Windows elevation |
| **`faq.md`** | "Why is my first search slow?", "Why are sizes different from Explorer?", "Does UFFS modify files?", etc. |
| **`glossary.md`** | MFT, NTFS, cluster, FRS, allocated size, treesize, bulkiness, daemon, compact index |
| **`advanced-diagnostics.md`** | --profile, --benchmark, --query-mode, --parity-compat, --no-bitmap, logging/env vars |

### 2.3  Structural / IA Issues

| Issue | Impact | Fix |
|-------|--------|-----|
| No navigation hierarchy | New users don't know where to start | Add `index.md` with ordered reading path |
| Flat page structure | All pages are peers; no beginner/advanced split | Group into Getting Started → Core Usage → Advanced → Reference |
| `cli-overview.md` duplicates content from `filters.md`, `sorting.md`, `search-modes.md` | Maintenance burden; risk of drift | Make `cli-overview.md` a concise hub that links to detail pages; remove duplicated tables |
| Aggregation docs mix CLI syntax (`uffs agg`) with inline flags (`--agg`, `--facet`) | Confusing two invocation styles | Separate into: §A = `uffs agg <preset>` subcommand; §B = inline `--agg` flag on search |
| Daemon page doesn't explain auto-start | Users confused why daemon starts on its own | Add "Auto-Start Behavior" section |
| No explicit "Windows vs macOS/Linux" differences section | Cross-platform users confused by --data-dir requirement | Add platform notes to installation and daemon pages |

---

## 3  Target Documentation Structure

```
docs/user-manual/
├── index.md                    # Landing page: what UFFS is, reading order, quick links
├── installation.md             # Build from source, platforms, PATH, prerequisites
├── getting-started.md          # First search, understanding output, 5-minute tutorial
├── cli-overview.md             # Hub: pattern syntax summary, flag overview (links out)
├── search-modes.md             # Glob, literal, regex, path-aware, scope prefixes
├── filters.md                  # All filters with examples
├── sorting.md                  # All sort columns, multi-tier, deterministic ordering
├── output-formats.md           # CSV, JSON, table, custom; columns; output-to-file
├── aggregation.md              # Presets, custom specs, inline --agg, pagination
├── concepts.md                 # Size vs SizeOnDisk, treesize, bulkiness, timestamps
├── daemon.md                   # Lifecycle, auto-start, config, management, platforms
├── cache-and-data.md           # .uffs cache, --data-dir, --mft-file, platform paths
├── advanced-diagnostics.md     # --profile, --benchmark, --query-mode, logging, env vars
├── troubleshooting.md          # Common errors, permissions, platform-specific issues
├── faq.md                      # Frequently asked questions
├── glossary.md                 # Terminology reference
├── mcp.md                      # (Phase 2 — MCP server docs)
├── tui-search-box.md           # (Phase 3 — TUI docs)
└── DOCUMENTATION_REFACTOR_PLAN.md  # This file
```

### Reading Path for New Users

```
index.md → installation.md → getting-started.md → cli-overview.md
                                                       ↓
                              search-modes.md ← filters.md → sorting.md
                                                       ↓
                              output-formats.md → aggregation.md → daemon.md
                                                       ↓
                              concepts.md → cache-and-data.md → advanced-diagnostics.md
                                                       ↓
                              troubleshooting.md → faq.md → glossary.md
```

---

## 4  Per-File Refactor Actions

### 4.1  `cli-overview.md` — Refactor to Hub Page

**Current:** 347 lines, mixes summary tables with full examples gallery.  
**Target:** ~200 lines, concise hub linking to detail pages.

| Action | Detail |
|--------|--------|
| **Remove duplicated filter/sort tables** | §3 (Filters summary) and §4 (Sorting summary) duplicate content from `filters.md` and `sorting.md`. Replace with 3-line summary + link. |
| **Move Examples Gallery to `getting-started.md`** | §8 (Examples Gallery, ~90 lines) is excellent content but belongs in a tutorial context, not an overview page. |
| **Add missing flags to summary** | `--between`, `--exact-descendants`, `--hide-ads`, `--path-contains` (if real) |
| **Add `uffs status` subcommand** | Missing from §6 |
| **Add `uffs agg` alias note** | Mention that `aggregate` can be shortened to `agg` |
| **Update §7 (Advanced/Diagnostic)** | Link to new `advanced-diagnostics.md` instead of inline table |
| **Add version badge** | State which UFFS version the docs describe |

### 4.2  `filters.md` — Fill Gaps

**Current:** 684 lines, very thorough.  
**Target:** ~750 lines with additions.

| Action | Detail |
|--------|--------|
| **Add `--between` to §3** | New subsection: "Time Range Shorthand" — `--between 2026-01-01,2026-03-31` |
| **Add `--exact-size` to §2** | Mention as shortcut for `--min-size N --max-size N` |
| **Add `--exact-descendants` to §5** | Mention as shortcut for `--min-descendants N --max-descendants N` |
| **Add `--hide-ads` to §1** | Add to scope filters table: "Hide NTFS Alternate Data Stream entries" |
| **Add `--exact-size-on-disk` to §11** | Already partially there; make explicit |
| **Validate named time specs** | Quick Ref §17 mentions `today`, `yesterday`, `this_week`, `last_7d`, etc. — verify these actually work in the parser |
| **Add `--path-contains` if it exists** | Verify in daemon IPC; if real, add to §8 or create new section |

### 4.3  `sorting.md` — Minor Polish

**Current:** 322 lines, complete.  
**Target:** ~330 lines.

| Action | Detail |
|--------|--------|
| **Verify all 36+ column names** | Cross-check against `uffs --help` output and daemon sort handler |
| **Add note about `--sort` with aggregation** | Aggregation results have their own sort; clarify interaction |

### 4.4  `search-modes.md` — Minor Polish

**Current:** 443 lines, complete.  
**Target:** ~450 lines.

| Action | Detail |
|--------|--------|
| **Add OR operator (`\|`) documentation** | `tui-search-box.md` mentions `*.txt\|*.log` but `search-modes.md` doesn't cover the OR operator at all — verify if CLI supports it |
| **Verify `**` (double-star) support in CLI** | Currently documented but verify: does CLI glob support `**` or is it TUI-only? |
| **Add extension-index optimization note** | Mention that `--ext` combined with regex avoids full scan |

### 4.5  `concepts.md` — Minor Additions

**Current:** 317 lines, good.  
**Target:** ~350 lines.

| Action | Detail |
|--------|--------|
| **Add "Alternate Data Streams" concept** | Explain what ADS are, why `--hide-ads` exists |
| **Add "The Daemon" concept** | Brief explanation of why a daemon, what it holds in memory |
| **Add "Cache Files (.uffs)" concept** | What they are, where stored, why they exist |
| **Link to glossary** | Once glossary exists, cross-link key terms |

### 4.6  `daemon.md` — Major Expansion

**Current:** 148 lines, thin.  
**Target:** ~400 lines.

| Action | Detail |
|--------|--------|
| **Add "Auto-Start Behavior" section** | Explain that `uffs "*.txt"` auto-starts daemon if not running |
| **Add "Idle Retirement" section** | Daemon auto-exits after idle timeout; configurable |
| **Add "Platform Differences" section** | Windows (live NTFS, elevation needed) vs macOS/Linux (offline MFT files) |
| **Add "Logging" section** | `--log-level`, `--log-file` on `daemon start`; env vars |
| **Add "Socket / PID File Locations" section** | `~/.local/share/uffs/daemon.{pid,sock}` on Mac |
| **Add "Cache Interaction" section** | `--no-cache` on daemon start; when cache is used/skipped |
| **Add "Troubleshooting" subsection** | Stale PID file, port in use, daemon won't start |
| **Add `uffs status` documentation** | Combined daemon + MCP status view |
| **Update performance table** | Verify numbers are current |

### 4.7  `aggregation.md` — Consistency Pass

**Current:** 488 lines, thorough.  
**Target:** ~520 lines.

| Action | Detail |
|--------|--------|
| **Clarify two invocation styles** | §A: `uffs agg <preset>` subcommand vs §B: `uffs "*.rs" --agg "stats:size"` inline |
| **Validate all examples** | Some use `uffs search "*"` which is not the actual CLI syntax (no `search` subcommand) — should be `uffs "*"` |
| **Add `--count`, `--facet`, `--stats`, `--histogram` shorthand docs** | These are in args.rs but only partially shown in aggregation.md §9 |
| **Verify `--path-contains`** | Used in recipe §11 — is this a real flag? Not in args.rs. May be daemon-only. |
| **Add pagination example with actual cursor** | Show what a cursor looks like and how to pass it |

---

## 5  New Pages to Create

### 5.1  `index.md` — Landing Page

**Purpose:** Entry point for the entire user manual.  
**Content:**
- One-paragraph "What is UFFS?"
- Feature highlights (speed, NTFS-native, 40+ filters, daemon architecture)
- Supported platforms with brief notes
- Table of contents linking all manual pages in reading order
- "Quick links" for common tasks: "I want to find a file", "I want to clean up disk space", "I want to set up the MCP server"
- Version the docs describe

**Estimated length:** ~80 lines.

### 5.2  `installation.md` — Installation Guide

**Purpose:** Get UFFS installed and on PATH.  
**Content:**
- Prerequisites (Rust 1.85+, platform notes)
- Build from source (`cargo build --release`)
- Binary location per platform
- Adding to PATH
- Windows: elevation requirements for live MFT access
- macOS/Linux: obtaining MFT captures (offline workflow)
- Verifying installation (`uffs --version`, `uffs --help`)
- (Future) Pre-built binaries / package managers

**Estimated length:** ~120 lines.

### 5.3  `getting-started.md` — First 5 Minutes

**Purpose:** Guided tutorial from zero to productive.  
**Content:**
- **Step 1:** First search (`uffs "*.txt"` on Windows, or `uffs "*.txt" --data-dir ~/uffs_data` on Mac)
- **Step 2:** Understanding the output (columns, CSV format, what each field means)
- **Step 3:** Narrowing results (add `--files-only`, `--newer 7d`, `--sort size`)
- **Step 4:** The daemon (explain it started automatically, show `uffs daemon status`)
- **Step 5:** Examples Gallery (migrate from cli-overview.md §8)
  - Quick Find recipes
  - Filter by Type/Date/Size recipes
  - Triage & Cleanup recipes
  - Developer/Admin recipes
- "Next steps" links to search-modes, filters, sorting

**Estimated length:** ~250 lines.

### 5.4  `output-formats.md` — Output Control Deep Dive

**Purpose:** Show exactly what output looks like in each format.  
**Content:**
- Default CSV output with header (show actual sample)
- JSON output (`--format json`) with sample NDJSON
- Table output (`--format table`) with sample
- Custom format options
- Column selection (`--columns Name,Size,Path`)
- Column names reference (all available columns)
- Output to file (`--out results.csv`)
- Separator and quote customization (`--sep`, `--quotes`)
- Header suppression (`--header false`)
- Boolean representation (`--pos`, `--neg`)
- Timezone override (`--tz-offset`)
- Piping and scripting integration (`uffs "*.log" | wc -l`)

**Estimated length:** ~200 lines.

### 5.5  `cache-and-data.md` — Data Sources & Cache

**Purpose:** Explain the data pipeline: live NTFS → MFT capture → .uffs cache → daemon.  
**Content:**
- How UFFS reads NTFS (direct MFT access on Windows)
- MFT capture files (.iocp, .bin, .mft) for offline/cross-platform use
- The .uffs cache format (what it stores, where, auto-generation)
- `--data-dir` directory structure (`drive_c/`, `drive_d/`, etc.)
- `--mft-file` for individual files
- `--no-cache` to force re-parse
- Cache invalidation and freshness
- Platform-specific paths (`~/.local/share/uffs/` on Mac, `%LOCALAPPDATA%\uffs\` on Windows)

**Estimated length:** ~180 lines.

### 5.6  `advanced-diagnostics.md` — Power User & Developer Flags

**Purpose:** Document diagnostic/profiling flags used for development and troubleshooting.  
**Content:**
- `--profile` — timing breakdown
- `--benchmark` — MFT-only measurement (no output I/O)
- `--query-mode auto|index|dataframe` — force query path
- `--no-bitmap` — disable MFT bitmap optimization
- `--parity-compat` — C++ parity-compatible output
- Logging configuration (RUST_LOG, RUST_LOG_FILE, UFFS_LOG_DIR)
- `-v` / `--verbose` — info-level terminal output
- Log file location and rotation

**Estimated length:** ~150 lines.

### 5.7  `troubleshooting.md` — Common Issues & Solutions

**Purpose:** Self-service problem resolution.  
**Content:**
- "Daemon won't start" (stale PID file, port conflict, missing data source)
- "Access denied / permission error" (Windows elevation, macOS file permissions)
- "No results returned" (wrong drive, pattern escaping in shell, case sensitivity)
- "Results differ from Windows Explorer" (size differences, hidden files, ADS)
- "Search is slow" (first search loads MFT, use daemon for instant repeat queries)
- "Cache seems stale" (--no-cache, when to rebuild)
- "Long paths cause issues" (MAX_PATH, `--min-path-length 260`)
- How to report bugs (log files, `--profile` output, version info)

**Estimated length:** ~200 lines.

### 5.8  `faq.md` — Frequently Asked Questions

**Purpose:** Quick answers to common questions.  
**Content:**
- "Does UFFS modify my files?" → No, read-only MFT access.
- "Why is the first search slow but subsequent ones instant?" → Daemon loads MFT once.
- "Why are file sizes different from Explorer?" → Size vs SizeOnDisk.
- "Can UFFS search inside files (content search)?" → Not yet, NTFS metadata only.
- "Does UFFS work on Linux/macOS?" → Yes, with offline MFT captures.
- "What's the difference between `--ext` and `--type`?" → Extension list vs semantic category.
- "How do I search for files with spaces in their names?" → Quote the pattern.
- "Is UFFS like Everything?" → Similar speed, more filters, daemon architecture, cross-platform.
- "Can I use UFFS with AI agents?" → Yes, via MCP server (Phase 2 docs).

**Estimated length:** ~150 lines.

### 5.9  `glossary.md` — Terminology

**Purpose:** Single reference for UFFS-specific and NTFS terminology.  
**Content:** Alphabetical definitions for:
- ADS (Alternate Data Stream), Allocated Size, Bulkiness, Cache (.uffs), Cluster,
  Compact Index, Daemon, Descendants, Extension Index, FRS (File Record Segment),
  Glob, IPC, MFT (Master File Table), MFT Bitmap, NTFS, Regex, Reparse Point,
  Treesize, Tree Allocated, Trigram Index

**Estimated length:** ~100 lines.

---

## 6  Cross-Cutting Concerns

### 6.1  Style Guide (enforce across all pages)

- **Header:** Every page starts with `# Title` followed by a one-line description.
- **Navigation:** Every page has a `> See also:` block after the title linking related pages.
- **Sections:** Numbered with `## N  Title` (two spaces before title, matching existing style).
- **Code blocks:** Use `bash` fence for CLI examples; `text` fence for output samples; `json` for JSON.
- **Tables:** Use pipes. Keep column widths reasonable.
- **Cross-links:** Use relative paths (`[Filters](filters.md)`). Always include section anchors where relevant.
- **Version:** Add a front-matter note: `> Describes UFFS v0.X.Y` on each page.
- **No emoji in body text.** (Matching existing docs style; README can keep emoji.)

### 6.2  Correctness Validation

Every documented flag and example must be validated against the actual CLI:

1. **Parse `args.rs`** — every `#[arg]` field should appear in docs.
2. **Run `uffs --help`** — verify flag names, defaults, and descriptions match docs.
3. **Run sample commands** — verify examples produce the described output.
4. **Check daemon commands** — `uffs daemon start/status/stats/stop/kill/restart` all work as documented.
5. **Check `uffs agg` presets** — verify each preset name is recognized.
6. **Check named time specs** — verify `today`, `yesterday`, `this_week`, etc. are actually parsed.

### 6.3  Cross-Link Audit

After refactoring, run a sweep to ensure:
- All `[text](page.md#anchor)` links resolve.
- No broken anchors from renumbered sections.
- `cli-overview.md` links to every detail page.
- `index.md` links to every page.
- Back-links ("See also") are bidirectional.

### 6.4  README.md Update

The repo-root `README.md` needs updates to support the user manual:

| Issue | Fix |
|-------|-----|
| Benchmarks are from v0.2.208 | Update to latest numbers or remove specific version |
| "Quick Start" doesn't mention daemon | Add daemon quick-start |
| No link to user manual | Add: "📖 Full documentation: [User Manual](docs/user-manual/index.md)" |
| Installation section is minimal | Link to `installation.md` |
| `--index` flag mentioned but may be stale | Verify or remove |

---

## 7  Validation Checklist

Use this checklist after completing each page refactor:

- [x] Every `#[arg]` in `args.rs` is documented somewhere in the manual
- [x] Every subcommand (`index`, `info`, `stats`, `aggregate`/`agg`, `daemon`, `mcp`, `status`) is documented
- [x] Every example command is valid and runnable (verified flag names against source; live output not possible on macOS)
- [x] Every cross-link resolves (no broken anchors) — verified via grep sweep
- [x] Every page has a "See also" navigation block
- [ ] Every page has a version note — **deferred** (index.md has version; individual pages do not)
- [x] Filter, sort, and output column names exactly match the CLI's accepted values
- [x] Named time specs (`today`, `last_7d`, etc.) documented in filters.md
- [x] Extension collection aliases (`pictures`, `documents`, etc.) are verified
- [x] Type categories are verified and complete
- [x] NTFS attribute names and aliases are verified
- [x] No duplicated content between hub (cli-overview) and detail pages
- [x] Aggregation examples use correct CLI syntax (not `uffs search`)
- [ ] Output format examples show real output (not fabricated) — **partial**: representative output, not captured live

---

## 8  Execution Order

### Phase 1A — Foundation ✅

| Step | Action | Effort | Status |
|------|--------|--------|--------|
| 1 | **Create `index.md`** — landing page with ToC and reading path | S | ✅ Done |
| 2 | **Create `installation.md`** — build, platforms, prerequisites | S | ✅ Done |
| 3 | **Create `getting-started.md`** — migrate examples gallery from cli-overview, add tutorial | M | ✅ Done |
| 4 | **Refactor `cli-overview.md`** — trim to hub, remove duplicated tables, add missing flags | M | ✅ Done |

### Phase 1B — Fill Gaps ✅

| Step | Action | Effort | Status |
|------|--------|--------|--------|
| 5 | **Update `filters.md`** — add `--between`, `--exact-*`, `--hide-ads`, named time specs | S | ✅ Done |
| 6 | **Update `daemon.md`** — major expansion: auto-start, retirement, platforms, logging | M | ✅ Done |
| 7 | **Create `output-formats.md`** — CSV/JSON/table samples, column reference, scripting | M | ✅ Done |
| 8 | **Create `cache-and-data.md`** — data pipeline, cache format, platform paths | M | ✅ Done |
| 9 | **Update `aggregation.md`** — fix `uffs search` → `uffs`, document shorthand flags | S | ✅ Done |

### Phase 1C — Polish & Reference ✅

| Step | Action | Effort | Status |
|------|--------|--------|--------|
| 10 | **Create `advanced-diagnostics.md`** — profiling, logging, diagnostic flags | S | ✅ Done |
| 11 | **Create `troubleshooting.md`** — common issues and solutions | S | ✅ Done |
| 12 | **Create `faq.md`** — frequently asked questions | S | ✅ Done |
| 13 | **Create `glossary.md`** — terminology reference | S | ✅ Done |
| 14 | **Update `concepts.md`** — add ADS, daemon, cache concepts, glossary link | S | ✅ Done |
| 15 | **Polish `search-modes.md` and `sorting.md`** — updated cross-links | S | ✅ Done |

### Phase 1D — Validation ✅

| Step | Action | Effort | Status |
|------|--------|--------|--------|
| 16 | **Cross-link audit** — all 17 linked .md files resolve | S | ✅ Done |
| 17 | **Correctness pass** — all flag names verified against `args.rs` source | M | ✅ Done |
| 18 | **Update repo README.md** — manual link, architecture table, daemon quick-start | S | ✅ Done |

### Phase 2 — MCP Server (future)

- Expand `mcp.md` with per-tool deep dives
- Add MCP troubleshooting
- Add MCP integration recipes per host (Claude, Cursor, Windsurf, etc.)

### Phase 3 — TUI & GUI (future)

- Expand `tui-search-box.md`
- Create TUI getting-started guide
- Create GUI docs when available

---

## Effort Key

| Symbol | Meaning |
|--------|---------|
| **S** | Small — 1-2 hours, mostly writing |
| **M** | Medium — 2-4 hours, research + writing + validation |
| **L** | Large — 4+ hours, significant new content or restructuring |

**Total estimated effort for Phase 1:** ~30-40 hours across 18 steps.
**Actual:** Completed 2026-04-12.  9 new files (1,227 lines), 7 updated files (~500 lines added/changed).

---

## Appendix: Source of Truth Files

These are the authoritative source files for validating documentation accuracy:

| What | File |
|------|------|
| CLI flags & subcommands | `crates/uffs-cli/src/args.rs` |
| CLI dispatch logic | `crates/uffs-cli/src/main.rs` |
| Search filter parsing | `crates/uffs-core/src/search/filters.rs` |
| Extension collections | `crates/uffs-core/src/extensions.rs` |
| Sort column definitions | `crates/uffs-core/src/search/backend.rs` |
| Type categories | `crates/uffs-core/src/search/` (type mapping) |
| Daemon IPC methods | `crates/uffs-daemon/src/` |
| Aggregation presets | `crates/uffs-core/src/` (aggregation module) |
| Named time specs | `crates/uffs-core/src/search/filters.rs` (time parsing) |
| NTFS attribute bits | `crates/uffs-mft/src/` (StandardInfo constants) |
| MCP tools | `crates/uffs-mcp/src/` |
| Competitor research | `docs/mft_competitor_landscape_deep_research.md` |
| Search defaults research | `docs/mft_search_defaults_report.md` |
