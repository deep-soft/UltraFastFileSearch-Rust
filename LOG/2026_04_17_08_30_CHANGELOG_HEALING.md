# CHANGELOG_HEALING — 2026-04-17 08:30

## Context

Running `just ship -v` after thin-client output fixes (column resolution,
NDJSON, shmem, `--out`, aggregation format defaults).

## What Failed

`05-parallel-validation` — Production linting (`cargo clippy`) rejected 6 errors
in `uffs-cli`:

| # | Lint | File | Line | Root Cause |
|---|------|------|------|------------|
| 1 | `shadow_reuse` | `commands/output/mod.rs:330` | `let lower = lower.trim()` shadows `lower` | Variable re-bound to trimmed version of itself |
| 2 | `manual_checked_ops` | `commands/output/mod.rs:457` | `if logical == 0 { … } else { alloc * 100 / logical }` | Manual zero-check before division instead of `checked_div` |
| 3 | `min_ident_chars` | `main.rs:149` | `\|a\| a == "--out"` | Single-char closure param |
| 4 | `unnecessary_map_or` | `main.rs:150` | `rows.map_or(true, …)` | Should use `is_none_or` |
| 5 | `collapsible_if` | `main.rs:152` | Nested `if !daemon_wrote_file { if let Some(…) }` | Collapse into `if … && let Some(…)` |
| 6 | `min_ident_chars` | `main.rs:286` | `\|a\| a == "--format"` | Single-char closure param |

## Fixes Applied

1. **`shadow_reuse`**: Renamed second binding to `trimmed`.
2. **`manual_checked_ops`**: Replaced manual zero-check with `checked_mul(100).and_then(|n| n.checked_div(logical)).unwrap_or(0)`.
3. **`min_ident_chars` (×2)**: Renamed closure params from `a` to `arg`.
4. **`unnecessary_map_or`**: Changed `map_or(true, …)` → `is_none_or(…)`.
5. **`collapsible_if`**: Collapsed nested `if` into `if !daemon_wrote_file && let Some(row_slice) = rows { … }`.

## Verification

- Local `cargo clippy -p uffs-cli --bins --all-features --no-deps -- -D warnings` passes clean.
- Full `just ship -v` pipeline passed — v0.5.15 deployed.

---

## Round 2: MCP readiness script fails at A3 (uffsmcp exits code 1)

### What Failed

`scripts/dev/mcp-readiness.rs` scenario A3 `mcp_start()`:
- Spawned `uffs mcp start` with piped stdout/stderr (`.spawn()`)
- Independently polled `/health` for 3.5 minutes
- Never got a response → timed out, killed child, reported failure
- stderr showed `uffsmcp` exited immediately with code 1 — but the actual
  MCP server works fine when run normally

### Root Cause

The script duplicated `mcp start`'s own health-polling logic. On Windows,
piping stdout/stderr via `.spawn()` causes handle inheritance — the
grandchild (`mcp serve`) holds the pipe handles open, causing `.output()`
to block forever. The workaround (`.spawn()` + independent health polling)
was fragile and masked real startup errors.

Meanwhile `uffs mcp start` already:
1. Auto-starts the daemon if needed
2. Spawns `uffs mcp serve` as a background process
3. Polls `/health` until ready
4. Exits 0 on success, non-zero on failure

### Fix

Replaced `mcp_start()` to use `.status()` (inherits console stdio — no
pipe handles, no Windows inheritance issue). The script now:
1. Runs `uffs mcp start --port X --bind Y` via `.status()`
2. Checks exit code
3. Does one `/health` sanity check

Also added `health_check_detail()` and diagnostic logging to
`wait_for_health()` so future failures show what's actually happening
instead of silently timing out.
