# CLI Overview

`uffs` is the command-line interface for Ultra Fast File Search.  It reads
NTFS Master File Tables directly and searches millions of files in
milliseconds.

**Search is the default action** — just type `uffs <pattern>`.  No
subcommand required.

> **Detailed guides:**
> [Concepts](concepts.md) ·
> [Search Modes](search-modes.md) ·
> [Filters](filters.md) ·
> [Sorting](sorting.md)

---

## 1  Quick Start

```bash
# ── Known-item lookup (the #1 reason people use file search) ──
uffs invoice                                    # Paths containing "invoice"
uffs '*.pdf'                                    # All PDFs across every drive
uffs 'dir:node_modules'                         # Directories named node_modules

# ── Narrow by type / date / size (the next-most-common workflow) ──
uffs '*.pdf' --newer 7d --sort size --limit 20  # Recent large PDFs
uffs '*' --ext pictures --min-size 1MB          # Images over 1 MB
uffs '*' --files-only --min-size 1GB --sort size # Giant files (storage triage)

# ── Cleanup & audit (the hidden killer feature) ──────────────
uffs '*' --dirs-only --max-descendants 0        # Empty directories
uffs '*' --files-only --sort pathlength --limit 20 # Longest paths (MAX_PATH risk)
uffs '*' --files-only --min-bulkiness 500 --sort bulkiness # Wasteful allocations

# ── Developer / admin ─────────────────────────────────────────
uffs '>.*\.log$' --newer 24h                    # Logs from last 24h (regex)
uffs '*.toml' --in-path '*projects*'            # Config files in project trees
uffs --contains setup --ext executables         # Installers by name fragment
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

### Scope Prefixes (Everything-compatible)

| Prefix | Effect | Detail |
|--------|--------|--------|
| `path:` | Match against the **full path**, not just filename | [Search Modes §10](search-modes.md#10--scope-prefixes) |
| `dir:` | Only search **directories** | Same as `--dirs-only` |
| `file:` | Only search **files** | Same as `--files-only` |

### Pattern Sugar

| Flag | Effect | Detail |
|------|--------|--------|
| `--begins-with <PREFIX>` | Sugar for `PREFIX*` | [Search Modes §11](search-modes.md#11--pattern-sugar-flags) |
| `--ends-with <SUFFIX>` | Sugar for `*SUFFIX` | Same |
| `--contains <NEEDLE>` | Sugar for `*NEEDLE*` | Same |
| `--not-contains <NEEDLE>` | Exclude names containing NEEDLE | Maps to `--exclude` |

### Search Modifiers

| Flag | Effect | Detail |
|------|--------|--------|
| `--case` | Case-sensitive matching | [Search Modes §6](search-modes.md#6--case-sensitivity) |
| `--smart-case` | Auto case-sensitive if pattern has uppercase | [Search Modes §6](search-modes.md#6--case-sensitivity) |
| `--word` | Whole-word boundaries (`\b…\b`) | [Search Modes §7](search-modes.md#7--whole-word-matching---word) |
| `--name-only` | Match filename only, not full path | [Search Modes §8](search-modes.md#8--name-only-matching---name-only) |
| `--in-path <GLOB>` | Filter by directory path (not filename) | [Search Modes §12](search-modes.md#12--path-directory-filter) |

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
| `--hide-system` | Scope | Hide `$`-prefixed NTFS metadata files |
| `--hide-ads` | Scope | Hide Alternate Data Stream entries |
| `--min-size <SIZE>` | Size | Minimum logical size (e.g. `100MB`, `1GB`) |
| `--max-size <SIZE>` | Size | Maximum logical size |
| `--exact-size <SIZE>` | Size | Exactly this size |
| `--min-size-on-disk <SIZE>` | Size | Minimum allocated size ([concept](concepts.md#1--size-vs-size-on-disk)) |
| `--max-size-on-disk <SIZE>` | Size | Maximum allocated size |
| `--newer <SPEC>` | Date | Modified within / after |
| `--older <SPEC>` | Date | Modified before |
| `--newer-created <SPEC>` | Date | Created within / after |
| `--older-created <SPEC>` | Date | Created before |
| `--newer-accessed <SPEC>` | Date | Accessed within / after |
| `--older-accessed <SPEC>` | Date | Accessed before |
| `--month <SPEC>` | Date | Month-of-year filter (jan, Q1, etc.) |
| `--attr <LIST>` | Attribute | Require/exclude NTFS attributes |
| `--type <CATEGORY>` | Type | Semantic type: code, picture, video, … |
| `--ext <LIST>` | Extension | Filter by extension or collection |
| `--min-descendants <N>` | Tree | Min child count (dirs) |
| `--max-descendants <N>` | Tree | Max child count (dirs) |
| `--min-treesize <SIZE>` | Tree | Min subtree logical size ([concept](concepts.md#2--tree-size--tree-allocated)) |
| `--max-treesize <SIZE>` | Tree | Max subtree logical size |
| `--min-tree-allocated <SIZE>` | Tree | Min subtree allocated size |
| `--max-tree-allocated <SIZE>` | Tree | Max subtree allocated size |
| `--min-bulkiness <N>` | Derived | Min waste ratio ([concept](concepts.md#3--bulkiness-waste-ratio)) |
| `--max-bulkiness <N>` | Derived | Max waste ratio |
| `--min-name-length <N>` | Derived | Min filename character count |
| `--max-name-length <N>` | Derived | Max filename character count |
| `--min-path-length <N>` | Derived | Min full-path character count |
| `--max-path-length <N>` | Derived | Max full-path character count |
| `--in-path <GLOB>` | Path | Directory path must match glob |
| `--exclude <GLOB>` | Exclude | Exclude matching filenames |
| `-n, --limit <N>` | Limit | Max results (0 = unlimited) |

---

## 4  Sorting

All 36+ sortable columns are detailed in the [Sorting guide](sorting.md).
Summary:

| Flag | Example | Effect |
|------|---------|--------|
| `--sort <SPEC>` | `--sort size` | Sort by column (smart default direction) |
| `--sort <SPEC>` | `--sort size:asc,name` | Multi-tier with explicit direction |
| `--sort-desc` | | Flip primary sort direction |

**Popular sort columns:** `size`, `modified`, `created`, `name`, `ext`,
`path`, `treesize`, `bulkiness`, `pathlength`, `namelength`,
`descendants`, `type`, `hidden`, `compressed`, `directory_flag`.
See [Sorting §2](sorting.md) for the full list.

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
`modified`, `accessed`, `ext`, `type`, `descendants`, `treesize`,
`treeallocated`, `bulkiness`, `namelength`, `pathlength`, and all 19
NTFS boolean attribute flags.  Use `--columns all` for everything.

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

These recipes are organized by the workflows people use file search
tools for most often (see [research report](../mft_search_defaults_report.md)
§5-6).

### Quick Find — known-item retrieval

The #1 reason people use file search: "I know roughly what it's called."

```bash
uffs invoice                                    # Paths containing "invoice"
uffs '*.pdf'                                    # All PDFs
uffs 'dir:node_modules'                         # Directories named node_modules
uffs --contains report                          # Names containing "report"
uffs 'c:/Users/*.docx'                          # Word docs on C: under Users
uffs --begins-with IMG --ext pictures           # Photos starting with IMG
```

### Filter by Type / Date / Size

The next-most-common workflow: "I know what kind of file it is."

```bash
uffs '*' --ext pictures --min-size 1MB          # Images over 1 MB
uffs '*' --ext executables --sort size          # Executables ranked by size
uffs '*' --type code --newer 7d                 # Source code modified this week
uffs '*' --type video --min-size 100MB          # Large videos
uffs '*.pdf' --newer 7d --files-only            # Recent PDFs
uffs '*' --newer-accessed 24h --files-only      # Files opened today
uffs '*' --month jan,feb --ext documents        # Docs from January & February
uffs '*' --ext config --in-path '*projects*'    # Config files in project trees
```

### Triage & Cleanup

Users repeatedly use search tools for storage management.

```bash
# ── Storage hogs ──────────────────────────────────────────
uffs '*' --files-only --sort size --limit 20    # Top 20 largest files
uffs '*' --files-only --min-size 1GB            # Files over 1 GB
uffs '*' --dirs-only --sort treesize --limit 20 # Biggest directory subtrees
uffs '*' --files-only --min-bulkiness 500 --sort bulkiness # Wasteful allocations

# ── Stale content ─────────────────────────────────────────
uffs '*' --ext archives --older 730d            # Archives over 2 years old
uffs '*' --files-only --older 365d --min-size 100MB # Old large files
uffs '*.tmp' --older 30d --sort size            # Old temp files

# ── Empty / broken structures ─────────────────────────────
uffs '*' --dirs-only --max-descendants 0        # Empty directories
uffs '*' --files-only --max-size 0              # Zero-byte files

# ── Path & name problems ─────────────────────────────────
uffs '*' --files-only --min-path-length 250 --sort pathlength # MAX_PATH risk
uffs '*' --files-only --min-name-length 100 --sort namelength # Absurdly long names
```

### Power Search — hidden / system / attribute files

Finding what the built-in index misses.

```bash
uffs '*' --attr hidden --files-only             # Hidden files
uffs '*' --attr hidden,encrypted --files-only   # Hidden + encrypted
uffs '*' --attr compressed --sort size          # NTFS-compressed files by size
uffs '*' --attr system --sort sizeondisk        # System files by disk usage
uffs '*' --sort hidden:desc,name                # Group hidden files first
uffs '*' --sort compressed:desc,size            # Compressed first, then by size
```

### Developer / Admin Workflows

```bash
# ── Regex power ───────────────────────────────────────────
uffs '>.*\.log$' --newer 24h                    # Logs from last 24h
uffs '>[0-9]{4}-[0-9]{2}-[0-9]{2}' --ext csv   # Date-stamped CSVs
uffs '>^[A-Z]{2,4}_[0-9]+' --files-only        # Coded file naming patterns

# ── Project / folder scoping ─────────────────────────────
uffs '*.rs' --in-path '*projects*'              # Rust files in project dirs
uffs 'path:*node_modules*package.json'          # package.json in node_modules
uffs '*.toml' --in-path '*github*'              # Config files in repos

# ── Executables & manifests ───────────────────────────────
uffs --ext executables --sort size --limit 20   # Largest executables
uffs '*.json' --contains package --files-only   # Package manifests
uffs --ext config --newer 7d                    # Recently changed configs
uffs '*.dll' --attr system --sort sizeondisk    # System DLLs by disk usage

# ── Multi-drive & output ─────────────────────────────────
uffs '*.exe' --drives C,D,E --sort size --limit 10
uffs '*.rs' --format json --limit 5             # NDJSON output
uffs '*.dll' --columns Name,Size,Path Only      # Selective columns
uffs '*.txt' --out results.csv                  # Write to file
```
