# UFFS Daemon

The UFFS daemon (`uffs-daemon`) is a long-running background process that holds
MFT indices in memory and serves search queries over a local IPC socket.
Searches that would normally take 10+ seconds to load data complete in
**~1 ms** because the daemon keeps everything hot.

## How It Works

```
┌─────────┐        IPC socket        ┌─────────────┐
│ uffs CLI ├──────────────────────────┤ uffs-daemon  │
│ uffs_tui │   JSON-RPC over Unix     │  (in-memory  │
└─────────┘   domain socket (Mac)     │   MFT index) │
              named pipe (Windows)    └─────────────┘
```

The daemon loads MFT data once at startup, then serves any number of search
queries without re-reading disk.  Multiple CLI / TUI clients share the same
daemon instance.

## Quick Start

### macOS / Linux (Offline MFT Files)

On non-Windows platforms, the daemon works with MFT capture files (`.iocp`,
`.bin`, `.mft`) exported from Windows NTFS volumes.

```bash
# Start the daemon with a data directory
uffs daemon start --data-dir ~/uffs_data

# Or with individual MFT files
uffs daemon start --mft-file /path/to/C_mft.iocp --mft-file /path/to/D_mft.iocp

# Search (daemon is already running — instant results)
uffs "*.rs" --data-dir ~/uffs_data

# Auto-start: if no daemon is running, search starts one automatically
uffs "*.dll" --data-dir ~/uffs_data
```

The `--data-dir` flag points to a directory with `drive_c/`, `drive_d/`, etc.
subdirectories, each containing an MFT capture file.

### Windows (Live NTFS Drives)

On Windows, the daemon auto-discovers all NTFS drives and reads their MFT
directly.  No `--data-dir` or `--mft-file` needed.

```powershell
# Start the daemon (auto-discovers C:, D:, E:, ...)
uffs daemon start

# Search — daemon auto-starts if not running
uffs "*.exe"

# Force specific drives only
uffs daemon start --drive C --drive D
```

> **Note:** Live MFT access requires **Administrator privileges**.

## Daemon Management

| Command               | Description                                          |
|-----------------------|------------------------------------------------------|
| `uffs daemon start`   | Start the daemon (with data sources)                 |
| `uffs daemon status`  | Show PID, uptime, loaded drives, record counts       |
| `uffs daemon stats`   | Show performance metrics (queries, timing, startup)  |
| `uffs daemon stop`    | Graceful shutdown via RPC                            |
| `uffs daemon kill`    | Hard kill (SIGKILL / taskkill) — always works        |
| `uffs daemon restart` | Stop → re-start with same data sources               |

### Status

```
$ uffs daemon status
Daemon PID:    72558
Uptime:        145s
Status:        Ready
Connections:   1
  C: —  3,428,455 records (file:/Users/rnio/uffs_data/drive_c/C_mft.iocp)
  D: —  7,065,539 records (file:/Users/rnio/uffs_data/drive_d/D_mft.iocp)
  E: —  2,929,519 records (file:/Users/rnio/uffs_data/drive_e/E_mft.iocp)
  ...
```

### Stats

```
$ uffs daemon stats
═══ Daemon Performance Stats ═══
Uptime:            591s
Startup duration:  10871ms
Total records:     25,846,853
Queries served:    2
Avg query time:    1190.5µs (1.19ms)
Total query time:  2381µs (2.38ms)
Queries/second:    0.00
```

## Performance (macOS — Offline, 7 Drives, 25.8M Records)

Measured on Apple Silicon, loading 7 MFT capture files totalling 25,846,853
NTFS records:

| Operation                   | Time       |
|-----------------------------|------------|
| Daemon startup (cold)       | ~11.8 s    |
| Search query (warm)         | ~1.2 ms    |
| Search end-to-end (CLI)     | ~16 ms     |
| Graceful stop               | ~15 ms     |
| Hard kill                   | ~25 ms     |
| Restart (stop + reload)     | ~12.8 s    |

Startup is dominated by deserializing the `.iocp` cache files.  Once loaded,
queries are sub-millisecond server-side; the ~16 ms CLI time includes process
spawn, IPC round-trip, and stdout formatting.

## Readiness Verification

A comprehensive test script exercises all daemon lifecycle combinations
(10 scenarios, 68 steps):

```bash
# With a data directory
rust-script scripts/dev/daemon-readiness.rs ~/uffs_data

# With a single MFT file
rust-script scripts/dev/daemon-readiness.rs /path/to/C_mft.iocp

# With custom search pattern
rust-script scripts/dev/daemon-readiness.rs ~/uffs_data --pattern "*.dll"
```

Scenarios tested: clean lifecycle, idempotent ops on stopped daemon, double
start, hard kill recovery, graceful stop→start cycle, restart data
preservation, double restart, stats accumulation, kill→status, and search
auto-start.

