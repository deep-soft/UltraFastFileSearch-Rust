# Filters

UFFS provides a rich set of filters that narrow search results **after**
pattern matching.  Filters are applied server-side inside the daemon — only
matching rows are returned to the CLI, so adding filters never makes a
search slower; it makes it faster by reducing output.

> **See also:** [CLI Overview](cli-overview.md) · [Search Modes](search-modes.md) ·
> [Sorting](sorting.md)

---

## 1  Scope Filters

Scope filters control whether files, directories, or both appear in
results.

| Flag | Effect |
|------|--------|
| `--files-only` | Show only files (exclude directories) |
| `--dirs-only` | Show only directories (exclude files) |
| `--hide-system` | Hide NTFS system files (names starting with `$`) |

```bash
# All PDF files — no directory entries
uffs '*.pdf' --files-only

# All directories named "backup"
uffs backup --dirs-only

# Everything, but suppress $MFT, $Bitmap, $LogFile, etc.
uffs '*' --hide-system
```

> `--files-only` and `--dirs-only` are mutually exclusive in practice —
> combining them would return zero results.

---

## 2  Size Filters

Size filters work on the file's **logical size**.  Values accept
human-readable suffixes or plain byte counts.

| Flag | Meaning |
|------|---------|
| `--min-size <SIZE>` | Only files ≥ this size |
| `--max-size <SIZE>` | Only files ≤ this size |

### Size Suffixes

| Suffix | Multiplier | Example |
|--------|-----------|---------|
| *(none)* | 1 (bytes) | `1024` → 1 024 bytes |
| `B` | 1 | `512B` → 512 bytes |
| `KB` | 1 024 | `100KB` → 102 400 bytes |
| `MB` | 1 048 576 | `10MB` → 10 485 760 bytes |
| `GB` | 1 073 741 824 | `1GB` → 1 073 741 824 bytes |
| `TB` | 1 099 511 627 776 | `2TB` → 2 199 023 255 552 bytes |

Suffixes are **case-insensitive** (`kb`, `KB`, `Kb` all work).

### Examples

```bash
# Large files: at least 100 MB
uffs '*' --files-only --min-size 100MB

# Tiny files: at most 1 KB
uffs '*' --files-only --max-size 1KB

# Size range: 1 MB to 10 MB
uffs '*.pdf' --min-size 1MB --max-size 10MB

# Combine with sort to find the biggest PDFs
uffs '*.pdf' --min-size 1MB --sort size --sort-desc --limit 20

# Raw bytes still work
uffs '*.log' --min-size 4096
```

### Best Practice

- Size filters are most useful with `--files-only` — directories have
  a size of 0 in the MFT.
- Combine `--min-size` with `--sort size` to build a "top largest files"
  workflow.

---

## 3  Date / Time Filters

UFFS can filter on three NTFS timestamps: **modified**, **created**, and
**accessed**.  Each timestamp has a **newer** (after) and **older** (before)
bound.

| Flag | Timestamp | Direction |
|------|-----------|-----------|
| `--newer <SPEC>` | Modified | Files modified **within** / **after** |
| `--older <SPEC>` | Modified | Files modified **before** |
| `--newer-created <SPEC>` | Created | Files created **within** / **after** |
| `--older-created <SPEC>` | Created | Files created **before** |
| `--newer-accessed <SPEC>` | Accessed | Files accessed **within** / **after** |
| `--older-accessed <SPEC>` | Accessed | Files accessed **before** |

### Time Spec Formats

You can specify time bounds using **relative durations** or **absolute
dates**:

#### Duration Suffixes

| Suffix | Meaning | Example | Interpretation |
|--------|---------|---------|----------------|
| `s` | Seconds | `90s` | Last 90 seconds |
| `m` | Minutes | `30m` | Last 30 minutes |
| `h` | Hours | `24h` | Last 24 hours |
| `d` | Days | `7d` | Last 7 days |
| `w` | Weeks | `2w` | Last 2 weeks (14 days) |

#### ISO Date

Use `YYYY-MM-DD` format for absolute dates:

```bash
--newer 2026-01-15       # Modified on or after 15 January 2026
--older 2025-06-01       # Modified before 1 June 2025
```

### Examples

```bash
# Files modified in the last 7 days
uffs '*.log' --newer 7d

# Files NOT modified in over a year
uffs '*.doc' --older 365d

# Files created in the last month
uffs '*' --newer-created 30d --files-only

# Files modified between two dates (combine newer + older)
uffs '*' --newer 2026-01-01 --older 2026-03-31

# Recently accessed executables
uffs '*.exe' --newer-accessed 1d

# Old archives untouched for 2+ years
uffs '*.zip' --older 730d --files-only
```

### Best Practice

- Duration specs (`7d`, `24h`) are the most common and intuitive for
  everyday use.
- Combine `--newer` and `--older` to define a time window.
- The `--newer-created` filter is useful for finding newly downloaded or
  installed files that may have old modification dates.
- All times are resolved relative to **now** at query execution time.

---

## 4  NTFS Attribute Filters

The `--attr` flag filters by NTFS file-system attributes.  You can
**require** attributes (they must be set) or **exclude** them (they must
not be set) by prefixing with `!`.

### Syntax

```bash
--attr hidden              # Must have Hidden attribute
--attr !hidden             # Must NOT have Hidden attribute
--attr hidden,system       # Must have BOTH Hidden AND System
--attr !system,!hidden     # Must have NEITHER System NOR Hidden
--attr compressed,!hidden  # Must be Compressed AND NOT Hidden
```

### Available Attributes

| Name | Aliases | Hex Bit | Description |
|------|---------|---------|-------------|
| `readonly` | `read-only`, `r` | `0x0001` | Read-only file |
| `hidden` | `h` | `0x0002` | Hidden file |
| `system` | `s` | `0x0004` | System file |
| `directory` | `dir`, `d` | `0x0010` | Directory entry |
| `archive` | `a` | `0x0020` | Archive (modified since backup) |
| `temporary` | `temp`, `t` | `0x0100` | Temporary file |
| `sparse` | — | `0x0200` | Sparse file |
| `reparse` | — | `0x0400` | Reparse point (symlink, junction) |
| `compressed` | `c` | `0x0800` | NTFS-compressed |
| `offline` | `o` | `0x1000` | Offline / tiered storage |
| `notindexed` | `notcontent`, `n` | `0x2000` | Not indexed by content indexer |
| `encrypted` | `e` | `0x4000` | EFS-encrypted |
| `integrity` | `i` | `0x8000` | Integrity stream (ReFS) |
| `virtual` | `v` | `0x10000` | Virtual file |
| `noscrub` | `no_scrub_data`, `x` | `0x20000` | No scrub data |
| `pinned` | `p` | `0x80000` | Pinned to local storage |
| `unpinned` | `u` | `0x100000` | Not pinned |

### Examples

```bash
# Find all hidden files
uffs '*' --attr hidden --files-only

# Find encrypted files
uffs '*' --attr encrypted --files-only

# Find compressed but not hidden files
uffs '*' --attr compressed,!hidden --files-only

# Find reparse points (symlinks, junctions)
uffs '*' --attr reparse

# Find sparse files (often VM disks, database files)
uffs '*' --attr sparse --files-only
```

### Best Practice

- Use short aliases for quick filtering: `--attr h` instead of
  `--attr hidden`.
- Combine `--attr !hidden,!system` with `--hide-system` for the cleanest
  "user files only" view.
- The `archive` attribute is useful for backup workflows — it marks files
  modified since the last backup.

---

## 5  Descendant Filters

Descendant filters operate on directories, filtering by the number of
direct children (files and subdirectories) they contain.

| Flag | Meaning |
|------|---------|
| `--min-descendants <N>` | Directories with at least N children |
| `--max-descendants <N>` | Directories with at most N children |

### Examples

```bash
# Find empty directories (zero children)
uffs '*' --dirs-only --max-descendants 0

# Find large directories with 1000+ items
uffs '*' --dirs-only --min-descendants 1000

# Directories with exactly 0 children, sorted by path
uffs '*' --dirs-only --max-descendants 0 --sort path
```

### Best Practice

- Empty-directory detection (`--max-descendants 0`) is one of the most
  popular cleanup workflows.
- Combine with `--sort descendants` to rank directories by child count.

---

## 6  Extension Filters

The `--ext` flag filters files by extension.  It accepts individual
extensions and **collection aliases** that expand to predefined groups.

### Syntax

```bash
--ext rs                   # Single extension
--ext jpg,png,gif          # Multiple extensions
--ext documents            # Collection alias (expands to many extensions)
--ext documents,mp4,heic   # Mix collections and individual extensions
```

Extensions are case-insensitive.  A leading dot is stripped automatically
(`.txt` and `txt` are equivalent).

### Collection Aliases

| Alias | Also Accepted | Extensions |
|-------|---------------|------------|
| `pictures` | `images` | jpg, jpeg, png, gif, bmp, tiff, tif, webp, svg, ico, raw, heic |
| `documents` | `docs` | doc, docx, pdf, txt, rtf, odt, xls, xlsx, ppt, pptx, csv, md |
| `videos` | `video` | mp4, avi, mkv, mov, wmv, flv, webm, mpeg, mpg, m4v, 3gp |
| `music` | `audio` | mp3, wav, flac, aac, ogg, wma, m4a, opus, aiff |
| `archives` | `compressed` | zip, rar, 7z, tar, gz, bz2, xz, iso |
| `code` | `source` | rs, py, js, ts, java, c, cpp, h, hpp, go, rb, php, swift, kt |

### Examples

```bash
# All image files across all drives
uffs '*' --ext pictures

# All source code files
uffs '*' --ext code --files-only

# Documents and spreadsheets modified this week
uffs '*' --ext documents --newer 7d

# Archives larger than 100 MB
uffs '*' --ext archives --min-size 100MB

# Mix: documents plus MP4 and HEIC
uffs '*' --ext documents,mp4,heic
```

### Best Practice

- Collections are the fastest way to search for "all images" or "all
  documents" without remembering every extension.
- Use `--ext` instead of a complex glob like `>.*\.(jpg|png|gif)$` — it
  is both simpler and faster.

---

## 7  Exclude Filter

The `--exclude` flag removes files matching a glob pattern **after** the
main pattern match.

```bash
# All .txt files, but skip anything starting with "backup"
uffs '*.txt' --exclude 'backup*'

# All files, but exclude temp files
uffs '*' --exclude '*.tmp' --files-only

# Rust files, but skip test files
uffs '*.rs' --exclude '*test*'
```

The exclude pattern is matched against the **filename only** (not the full
path) and is always case-insensitive.

---

## 8  Result Limit

The `--limit` (or `-n`) flag caps the number of results returned.

```bash
# Top 20 largest files
uffs '*' --files-only --sort size --sort-desc --limit 20

# Just check if any .exe exists on D:
uffs '*.exe' --drive D --limit 1
```

A limit of `0` (the default) means unlimited.

---

## 9  Combining Filters — Recipes

Filters are **ANDed together** — every filter must pass for a row to
appear.  This makes it easy to build precise queries by stacking filters.

### Find the Top 10 Largest PDFs Modified This Month

```bash
uffs '*.pdf' --files-only --newer 30d --sort size --sort-desc --limit 10
```

### Find Empty Directories on C: Drive

```bash
uffs '*' --dirs-only --max-descendants 0 --drive C
```

### Find Hidden Encrypted Files Over 1 MB

```bash
uffs '*' --attr hidden,encrypted --min-size 1MB --files-only
```

### Find Old Archives Untouched for 2+ Years

```bash
uffs '*' --ext archives --older 730d --files-only --sort size --sort-desc
```

### Find Recently Created Source Code

```bash
uffs '*' --ext code --newer-created 7d --files-only
```

### Find Large Directories with Many Children

```bash
uffs '*' --dirs-only --min-descendants 500 --sort descendants --sort-desc --limit 20
```

---

## 10  Quick Reference

```text
SCOPE
  --files-only             Files only (no directories)
  --dirs-only              Directories only (no files)
  --hide-system            Hide $-prefixed NTFS system files

SIZE
  --min-size <SIZE>        Minimum file size (e.g. 100MB, 1GB, or raw bytes)
  --max-size <SIZE>        Maximum file size (e.g. 100KB, 10MB, or raw bytes)

DATE / TIME
  --newer <SPEC>           Modified within / after  (7d, 24h, 2026-01-15)
  --older <SPEC>           Modified before
  --newer-created <SPEC>   Created within / after
  --older-created <SPEC>   Created before
  --newer-accessed <SPEC>  Accessed within / after
  --older-accessed <SPEC>  Accessed before

ATTRIBUTES
  --attr <LIST>            Require/exclude NTFS attrs (hidden, !system, …)

DESCENDANTS
  --min-descendants <N>    Minimum child count (dirs)
  --max-descendants <N>    Maximum child count (dirs)

EXTENSIONS
  --ext <LIST>             Filter by extension or collection alias

EXCLUDE
  --exclude <GLOB>         Exclude files matching glob

LIMIT
  -n, --limit <N>          Maximum result count (0 = unlimited)

TIME SPEC FORMATS
  90s / 30m / 24h / 7d / 2w     Relative durations
  2026-01-15                     ISO date (YYYY-MM-DD)
```
