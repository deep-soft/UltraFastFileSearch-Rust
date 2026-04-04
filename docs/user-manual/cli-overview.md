# CLI Overview

`uffs` is the command-line interface for Ultra Fast File Search.  It reads
NTFS Master File Tables directly and searches millions of files in
milliseconds.

**Search is the default action** — just type `uffs <pattern>`.  No
subcommand required.

> **Detailed guides:**
> [Search Modes](search-modes.md) ·
> [Filters](filters.md) ·
> [Sorting](sorting.md)

---

## 1  Quick Start

```bash
# Find all .txt files
uffs '*.txt'

# Find files containing "invoice" in the path
uffs invoice

# Regex search for .log files
uffs '>.*\.log$'

# Find large PDFs modified recently
uffs '*.pdf' --min-size 1MB --newer 7d --sort size --limit 20

# Find empty directories on C: drive
uffs '*' --dirs-only --max-descendants 0 --drive C
```

---

## 2  Search (Default Action)

When no subcommand is specified, `uffs` performs a search.  The first
positional argument is the pattern.

```
uffs [OPTIONS] <PATTERN>
```

### Pattern Syntax

| Syntax | Mode | Detail |
|--------|------|--------|
| `*.ext` | Glob | Wildcards `*`, `?`, `[…]` → [Search Modes §2](search-modes.md#2--glob-patterns-default) |
| `word` | Literal | Substring match on full path → [Search Modes §3](search-modes.md#3--literal-search) |
| `>regex` | Regex | `>` prefix activates regex → [Search Modes §4](search-modes.md#4--regex-search) |
| `c:/path*` | Path-aware | Contains separators → [Search Modes §5](search-modes.md#5--path-aware-patterns) |

### Search Modifiers

| Flag | Effect | Detail |
|------|--------|--------|
| `--case` | Case-sensitive matching | [Search Modes §6](search-modes.md#6--case-sensitivity) |
| `--smart-case` | Auto case-sensitive if pattern has uppercase | [Search Modes §6](search-modes.md#6--case-sensitivity) |
| `--word` | Whole-word boundaries (`\b…\b`) | [Search Modes §7](search-modes.md#7--whole-word-matching---word) |
| `--name-only` | Match filename only, not full path | [Search Modes §8](search-modes.md#8--name-only-matching---name-only) |

### Drive Selection

| Flag | Example | Effect |
|------|---------|--------|
| `--drive <X>` | `--drive C` | Search single drive |
| `--drives <X,Y>` | `--drives C,D,E` | Search multiple drives concurrently |
| Pattern prefix | `c:/*.dll` | Infer drive from pattern |

### Data Sources

| Flag | Description |
|------|-------------|
| `--mft-file <PATH>` | Use offline raw MFT file(s) instead of live volume |
| `--data-dir <DIR>` | Auto-discover MFT files in `drive_*` subdirectories |
| `--no-cache` | Bypass cache; re-read MFT fresh |

---

## 3  Filters

All filters are detailed in the [Filters guide](filters.md).  Summary:

| Flag | Category | Quick Description |
|------|----------|-------------------|
| `--files-only` | Scope | Files only |
| `--dirs-only` | Scope | Directories only |
| `--hide-system` | Scope | Hide `$`-prefixed NTFS files |
| `--min-size <SIZE>` | Size | Minimum size (e.g. `100MB`, `1GB`) |
| `--max-size <SIZE>` | Size | Maximum size (e.g. `1KB`, `10MB`) |
| `--newer <SPEC>` | Date | Modified within / after |
| `--older <SPEC>` | Date | Modified before |
| `--newer-created <SPEC>` | Date | Created within / after |
| `--older-created <SPEC>` | Date | Created before |
| `--newer-accessed <SPEC>` | Date | Accessed within / after |
| `--older-accessed <SPEC>` | Date | Accessed before |
| `--attr <LIST>` | Attribute | Require/exclude NTFS attributes |
| `--min-descendants <N>` | Descendants | Min child count (dirs) |
| `--max-descendants <N>` | Descendants | Max child count (dirs) |
| `--ext <LIST>` | Extension | Filter by extension or collection |
| `--exclude <GLOB>` | Exclude | Exclude matching filenames |
| `-n, --limit <N>` | Limit | Max results (0 = unlimited) |

---

## 4  Sorting

All sorting options are detailed in the [Sorting guide](sorting.md).
Summary:

| Flag | Example | Effect |
|------|---------|--------|
| `--sort <SPEC>` | `--sort size` | Sort by column (smart default direction) |
| `--sort <SPEC>` | `--sort size:asc,name` | Multi-tier with explicit direction |
| `--sort-desc` | | Flip primary sort direction |

---

## 5  Output Control

| Flag | Default | Description |
|------|---------|-------------|
| `--format <FMT>` | `csv` | Output format: `csv`, `json`, `table`, `custom` |
| `--columns <LIST>` | `all` | Columns to output (comma-separated or `all`) |
| `--out <DEST>` | `console` | Output destination: `console` or a filename |
| `--sep <CHAR>` | `,` | Column separator (CSV mode) |
| `--quotes <CHAR>` | `"` | Quote character for string fields |
| `--header <BOOL>` | `true` | Include header row (`--header false` to suppress) |
| `--pos <STR>` | `1` | Representation for true/active boolean attributes |
| `--neg <STR>` | `0` | Representation for false/inactive boolean attributes |

### Available Output Columns

`drive`, `name`, `path`, `pathonly`, `size`, `sizeondisk`, `created`,
`modified`, `accessed`, `ext`, `type`, `descendants`, and all NTFS
attribute flags.

---

## 6  Subcommands

### `uffs index`

Build a Parquet index from one or more NTFS drives.

```bash
uffs index output.parquet              # Index ALL drives
uffs index -d C output.parquet         # Index C: only
uffs index --drives C,D,E out.parquet  # Index specific drives
```

### `uffs info`

Show metadata about a saved index file.

```bash
uffs info index.parquet
```

### `uffs stats`

Show file statistics from an index (e.g. top N largest files).

```bash
uffs stats index.parquet --top 20
```

### `uffs daemon`

Manage the UFFS background daemon.  The daemon starts automatically on
first search — these commands give explicit control.

```bash
uffs daemon start --data-dir ~/uffs_data   # Start with specific data
uffs daemon status                          # Check status
uffs daemon stats                           # Performance statistics
uffs daemon stop                            # Graceful shutdown
uffs daemon kill                            # Force kill + cleanup
uffs daemon restart                         # Stop then restart
```

---

## 7  Advanced / Diagnostic Flags

These flags are for power users, profiling, and parity testing:

| Flag | Description |
|------|-------------|
| `-v, --verbose` | Enable verbose output (global) |
| `--profile` | Show detailed timing breakdown |
| `--benchmark` | Skip output; measure only MFT reading + filtering |
| `--no-bitmap` | Disable MFT bitmap optimisation (read ALL records) |
| `--query-mode <MODE>` | Force query path: `auto`, `index`, `dataframe` |
| `--tz-offset <HOURS>` | Override timezone offset for timestamps |
| `--parity-compat` | C++ parity-compatible output (25 baseline columns) |

---

## 8  Examples Gallery

```bash
# ── Quick Find ─────────────────────────────────────────────
uffs '*.txt'                                    # All text files
uffs invoice                                    # Paths containing "invoice"
uffs 'c:/Users/*.docx'                          # Word docs on C: under Users

# ── Filtered Search ────────────────────────────────────────
uffs '*.pdf' --newer 7d --files-only            # Recent PDFs
uffs '*' --ext pictures --min-size 1MB           # Images over 1 MB
uffs '*' --attr hidden,encrypted --files-only   # Hidden + encrypted files

# ── Sorted Results ─────────────────────────────────────────
uffs '*' --files-only --sort size --limit 20    # Top 20 largest files
uffs '*.log' --sort modified --limit 50         # Most recent logs

# ── Cleanup Workflows ──────────────────────────────────────
uffs '*' --dirs-only --max-descendants 0        # Empty directories
uffs '*' --ext archives --older 730d            # Old archives
uffs '*' --files-only --min-size 1GB             # Files over 1 GB

# ── Regex Power ────────────────────────────────────────────
uffs '>.*\.log$' --newer 24h                    # Logs from last 24h
uffs '>[0-9]{4}-[0-9]{2}-[0-9]{2}' --ext csv   # Date-stamped CSVs

# ── Multi-Drive ───────────────────────────────────────────
uffs '*.exe' --drives C,D,E --sort size --limit 10

# ── Output Control ────────────────────────────────────────
uffs '*.rs' --format json --limit 5             # NDJSON output
uffs '*.dll' --columns Name,Size,Path Only      # Selective columns
uffs '*.txt' --out results.csv                  # Write to file
uffs '*.log' --sep '|' --quotes "'"             # Pipe-separated
```
