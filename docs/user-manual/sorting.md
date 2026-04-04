# Sorting

UFFS supports single- and multi-tier sorting of search results.  Sort
specifications are applied after all filters, so you are always sorting
the final result set.

> **See also:** [CLI Overview](cli-overview.md) · [Search Modes](search-modes.md) ·
> [Filters](filters.md)

---

## 1  Basic Usage

```bash
# Sort by file size (largest first — size defaults to descending)
uffs '*.pdf' --sort size

# Sort by name (A → Z — name defaults to ascending)
uffs '*.txt' --sort name

# Sort by modification date (newest first — date defaults to descending)
uffs '*.log' --sort modified
```

Without `--sort`, results are returned in the natural order produced by
the search engine (typically by MFT record number).

---

## 2  Sort Columns

| Column Name | Aliases | Default Direction | Description |
|-------------|---------|-------------------|-------------|
| `name` | — | Ascending (A → Z) | Filename (case-folded comparison) |
| `size` | — | Descending (largest first) | Logical file size in bytes |
| `sizeondisk` | `allocated` | Descending | Allocated size on disk |
| `modified` | `date`, `written` | Descending (newest first) | Last modification timestamp |
| `created` | — | Descending (newest first) | Creation timestamp |
| `accessed` | — | Descending (newest first) | Last access timestamp |
| `path` | — | Ascending (A → Z) | Full path (case-folded) |
| `drive` | — | Ascending (A → Z) | Drive letter |
| `ext` | `extension` | Ascending (A → Z) | File extension (case-folded) |
| `type` | — | Ascending | File type (devicon category) |
| `descendants` | — | Descending (most first) | Child count (directories) |

### Smart Defaults

Every column has a **sensible default direction** so you rarely need to
specify `:asc` or `:desc` explicitly:

- **Numeric / date columns** (size, sizeondisk, created, modified,
  accessed, descendants) default to **descending** — you usually want the
  biggest / newest / most-populated items first.
- **Text columns** (name, path, drive, ext, type) default to
  **ascending** — alphabetical order is the natural expectation.

---

## 3  Explicit Direction

You can override the default direction with a `:asc` or `:desc` suffix:

```bash
# Size ascending (smallest first)
uffs '*.dll' --sort size:asc

# Name descending (Z → A)
uffs '*.txt' --sort name:desc

# Modified ascending (oldest first)
uffs '*.log' --sort modified:asc
```

### `--sort-desc` Flag

The `--sort-desc` flag reverses the direction of the **primary** sort
column (useful when you want the opposite of the smart default):

```bash
# Normally size defaults to descending — this flips it to ascending
uffs '*.iso' --sort size --sort-desc
```

> When using `:asc` / `:desc` suffixes, the `--sort-desc` flag is
> ignored for columns that already have an explicit direction.

---

## 4  Multi-Tier Sorting

Separate multiple sort columns with commas.  The first column is the
primary sort; subsequent columns break ties.

```bash
# Primary: size descending, tiebreaker: name ascending
uffs '*.dll' --sort size,name

# Primary: modified descending, then size descending, then name ascending
uffs '*.log' --sort modified,size,name

# Explicit directions on each tier
uffs '*.pdf' --sort modified:desc,size:asc,name
```

### Automatic Name Tiebreaker

If `name` does not appear anywhere in your sort specification, UFFS
automatically appends a **name-ascending** tiebreaker.  This ensures
deterministic, reproducible ordering even when the primary column has
many identical values (e.g. thousands of files with the same size).

```bash
# You type:
uffs '*.exe' --sort size

# UFFS internally sorts by:  size:desc, name:asc
```

---

## 5  Case-Folded String Sorting

String-based columns (`name`, `path`, `ext`) are sorted using
**case-folded comparison** — uppercase and lowercase letters are treated
identically.  This means `readme.txt` sorts next to `README.txt`, which
matches standard Windows Explorer behaviour.

The case-folding uses a Schwartzian transform (pre-computed sort keys) to
avoid per-comparison allocation overhead, so even millions of rows sort
efficiently.

---

## 6  Examples and Recipes

### Top 20 Largest Files

```bash
uffs '*' --files-only --sort size --limit 20
```

### Most Recently Modified Logs

```bash
uffs '*.log' --sort modified --limit 50
```

### Directories Ranked by Child Count

```bash
uffs '*' --dirs-only --sort descendants --limit 30
```

### Files Sorted by Extension, Then Name

```bash
uffs '*' --files-only --sort ext,name --limit 100
```

### Oldest Files on C: Drive

```bash
uffs '*' --drive C --files-only --sort created:asc --limit 20
```

### Multi-Drive Sort by Path

```bash
uffs '*.pdf' --drives C,D --sort drive,path
```

---

## 7  Quick Reference

```text
SORT SYNTAX
  --sort <COLUMN>                    Single column (smart default direction)
  --sort <COLUMN>:asc               Explicit ascending
  --sort <COLUMN>:desc              Explicit descending
  --sort <COL1>,<COL2>,<COL3>       Multi-tier (comma-separated)
  --sort-desc                        Flip primary column direction

COLUMNS AND DEFAULTS
  name          asc       Filename (case-folded)
  size          desc      Logical file size
  sizeondisk    desc      Allocated size on disk
  modified      desc      Last modification time
  created       desc      Creation time
  accessed      desc      Last access time
  path          asc       Full path (case-folded)
  drive         asc       Drive letter
  ext           asc       File extension (case-folded)
  type          asc       File type category
  descendants   desc      Child count (directories)

NOTES
  • Unknown column names are silently ignored.
  • A name-ascending tiebreaker is appended automatically if name
    is not already in the sort spec.
  • String columns use case-folded comparison (Windows-style).
```
