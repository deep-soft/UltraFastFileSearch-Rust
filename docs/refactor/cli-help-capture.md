ok ... do T# CLI Help Text Capture — 2026-04-16

Captured from `uffs v0.5.12` (clap-generated) before T5 replaces clap
with a hand-written parser. This is the reference for the static help
text that will be hardcoded.

## `uffs --help`

```
Fast NTFS search via direct Master File Table reads.

Search is the default action: pass a pattern with no subcommand to search
a live volume, a saved index, or a raw MFT file. Use subcommands for index
creation and offline inspection.

Usage: uffs [OPTIONS] [PATTERN]
       uffs <COMMAND>

Commands:
  stats      Show statistics about files in an index
  aggregate  Run aggregate analytics on the filesystem index
  daemon     Manage the UFFS background daemon
  mcp        Manage the UFFS MCP server (AI agent integration)
  status     Show combined system status (daemon + MCP HTTP server)
  help       Print this message or the help of the given subcommand(s)

Arguments:
  [PATTERN]  Search pattern (glob, regex with `>`, or literal) - default action

Options:
  -v, --verbose           Enable verbose output
  -d, --drive <DRIVE>     Drive letter to search (e.g., C or C:)
      --drives <DRIVES>   Multiple drive letters (e.g., C,D,E)
      --mft-file <PATH>   Use raw MFT file(s) instead of live MFT
      --data-dir <PATH>   Data directory with drive_* subdirectories
      --files-only        Show only files (exclude directories)
      --dirs-only         Show only directories
      --hide-system       Hide system files (starting with $)
      --hide-ads          Hide NTFS Alternate Data Streams
      --profile           Show detailed timing breakdown
      --benchmark         Skip output, only measure performance
      --no-cache          Bypass cache, read MFT fresh
```


### Search options (continued)

```
      --agg <SPEC>              Run aggregate analytics alongside search
      --count                   Show only total matching count (no rows)
      --facet <FIELD[:TOP]>     Facet breakdown by field
      --stats <FIELD>           Scalar statistics for numeric field
      --histogram <FIELD[:INT]> Histogram for a field
      --rows                    Include rows alongside aggregates
      --agg-cursor <CURSOR>     Continue from previous page
      --agg-page-size <N>       Max buckets per page

      --min-size <SIZE>         Minimum file size (100KB, 10MB, 1GB)
      --max-size <SIZE>         Maximum file size
      --exact-size <SIZE>       Exact file size
      --min-descendants <N>     Min child entries (dirs only)
      --max-descendants <N>     Max child entries
      --exact-descendants <N>   Exact descendant count
      --min-name-length <N>     Min filename length
      --max-name-length <N>     Max filename length
      --min-path-length <N>     Min full-path length
      --max-path-length <N>     Max full-path length
      --min-size-on-disk <SIZE> Min allocated size
      --max-size-on-disk <SIZE> Max allocated size
      --exact-size-on-disk <S>  Exact on-disk size
      --min-treesize <SIZE>     Min subtree logical size
      --max-treesize <SIZE>     Max subtree logical size
      --min-tree-allocated <S>  Min subtree on-disk size
      --max-tree-allocated <S>  Max subtree on-disk size
      --min-bulkiness <N>       Min allocated-to-size ratio (%)
      --max-bulkiness <N>       Max bulkiness
      --month <MONTH>           Filter by month (jan, Q1, etc.)
      --between <START,END>     Time range (--newer + --older)

  -n, --limit <LIMIT>          Max results (0=unlimited) [default: 0]
  -f, --format <FORMAT>        Output: table, json, csv, custom [default: csv]
      --case                   Case-sensitive matching
      --smart-case             Auto case-sensitive if uppercase present
      --attr <ATTR>            NTFS attributes (hidden, !system, etc.)
      --newer <SPEC>           Modified after (7d, 2026-01-15, etc.)
      --older <SPEC>           Modified before
      --newer-created <SPEC>   Created after
      --older-created <SPEC>   Created before
      --newer-accessed <SPEC>  Accessed after
      --older-accessed <SPEC>  Accessed before
      --exclude <PATTERN>      Exclude matching files
      --in-path <GLOB>         Filter by directory path
      --type <CATEGORY>        Filter by type (code, picture, etc.)
      --begins-with <PREFIX>   Name begins with (sugar for PREFIX*)
      --ends-with <SUFFIX>     Name ends with (sugar for *SUFFIX)
      --contains <NEEDLE>      Name contains (sugar for *NEEDLE*)
      --not-contains <NEEDLE>  Exclude names containing NEEDLE
      --word                   Whole word matching
      --name-only              Match filename only, not full path
      --sort <COLS>            Sort by column(s), prefix - for desc
      --sort-desc              Reverse sort order
      --ext <EXT>              Filter by extension(s)
      --out <OUT>              Output destination [default: console]
      --columns <COLS>         Columns to output [default: all]
      --sep <SEP>              Column separator [default: ,]
      --quotes <CHAR>          Quote character [default: "]
      --header <BOOL>          Include header row [default: true]
      --pos <VAL>              True boolean representation [default: 1]
      --neg <VAL>              False boolean representation [default: 0]
      --tz-offset <HOURS>      Timezone offset for timestamps
      --parity-compat          Parity-compatible 25-column output
  -h, --help                   Print help
  -V, --version                Print version

Examples:
  uffs '*.txt'
  uffs '>.*\.log$' --drive C
  uffs '*' --mft-file G_mft.bin --drive G
```

## `uffs daemon --help`

```
Manage the UFFS background daemon

Usage: uffs daemon <COMMAND>

Commands:
  start    Start the daemon with specified data sources
  status   Show daemon status (running, loading, drives loaded, PID)
  stats    Show performance statistics
  stop     Gracefully stop the running daemon
  kill     Hard kill + remove PID/socket files
  restart  Stop then restart (re-loads all indices)
```

## `uffs mcp --help`

```
Manage the UFFS MCP server (AI agent integration)

Usage: uffs mcp <COMMAND>

Commands:
  start    Start MCP HTTP server as background service
  status   Show MCP HTTP server status
  stats    Show MCP server performance statistics
  stop     Gracefully stop MCP HTTP server
  kill     Force kill MCP server + remove PID file
  restart  Stop then restart MCP HTTP server
  reload   Reload all MCP servers to pick up current binary
```

## `uffs stats --help`

```
Show statistics about files in an index

Usage: uffs stats [OPTIONS] [PATH]

Arguments:
  [PATH]  Index file path (optional; omit to use daemon)

Options:
  --top <TOP>            Show top N largest files [default: 10]
  --data-dir <PATH>      Data directory
  --mft-file <PATH>      Raw MFT file(s)
```

## `uffs aggregate --help`

```
Run aggregate analytics on the filesystem index

Usage: uffs aggregate [OPTIONS] <PRESET>

Arguments:
  <PRESET>  Preset: overview, by_type, by_extension, by_drive,
            by_size, by_age, count

Options:
  --format <FORMAT>      Output format [default: table]
  --data-dir <PATH>      Data directory
  --mft-file <PATH>      Raw MFT file(s)
  --agg-cursor <CURSOR>  Continue from previous page
  --agg-page-size <N>    Max buckets per page
```

## `uffs status --help`

```
Show combined system status (daemon + MCP HTTP server)

Usage: uffs status
```