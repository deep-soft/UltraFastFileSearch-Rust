# CHANGELOG — Healing Run 2026-04-17 11:23 PDT

## Goal

Ship **v0.5.36** via the `just ship -v` CI pipeline.

The release bundles two independently-committed changes that currently
sit on top of `main` (one commit ahead of the v0.5.35 tag):

1. **`a29ca08c1` feat(uac):** default `asInvoker` spawn with actionable
   elevation error.  Replaces the old unconditional
   `ShellExecuteW("runas")` behaviour in `spawn_daemon` with a
   policy-driven variant (`ElevationPolicy::RequireExistingElevation`
   by default, `AllowUacPrompt` when the user passes `--elevate` or
   sets `UFFS_ELEVATE=1`).  CLI `main` walks the `anyhow` error chain
   for `ClientError::DaemonNeedsElevation` and renders a three-option
   help message (elevated shell / `--elevate` / `uffs-broker --install`).
2. **`0ca8e4019` feat(just):** `bench-cross` and `bench-verify` recipes,
   plus a Windows variant of `just pgo` (the bash version failed under
   PowerShell with a path-translation error).

Both commits compiled clean locally (`cargo check --workspace` and
`cargo test -p uffs-client --lib` all green) before this healing run
started.  This document exists so that any pipeline-only failures —
formatting, advisory lints that only trip on the CI toolchain, or
cross-crate drift surfaced by `--all-features` — are recorded with
root cause and fix as we heal them.

## Operating rules for this run

*Baseline and final validation: `just ship-fresh -v` once, then
`just ship -v` for every subsequent iteration.*  Local `cargo` checks
are advisory only.

* No suppression hacks (`#[allow]`, disabled lints, skipped tests).
* Surgical, idiomatic fixes targeted at root cause.
* Preserve public API and observable behaviour unless the pipeline
  proves otherwise.
* Strengthen tests, do not dodge them.
* One atomic commit per healed issue with `fix:` prefix and root-cause
  summary.  This document stays current throughout and is part of the
  final commit.

## Run log

### Run 1 — `just ship-fresh -v`

**Result:** ❌ Phase 1 step `03-format-code` + `File size policy` failed.

* `cargo fmt --all` rewrote a handful of `anyhow::Error::from(...)`
  chains in `crates/uffs-cli/src/main.rs` and tweaked
  `connect_sync.rs` — these were formatting drift from the previous
  session and apply cleanly.
* `scripts/ci/check_file_size_policy.sh` rejected
  `crates/uffs-client/src/connect.rs` at **899 LOC** (ceiling: 800,
  not in `scripts/ci/file_size_exceptions.txt`).

**Root cause of the policy failure:**  The v0.5.36 UAC work added
~115 LOC to `connect.rs` (`connect_with_elevation`,
`connect_with_args_inner`, `elevation_policy_tests` module, plus
doc blocks), pushing a previously-compliant 784 LOC file over the
800 ceiling.

**Fix (surgical, no suppression hack):**

1. Extracted three tracing helpers (`log_spawn_details`,
   `log_connect_attempt`, `log_connect_error`) into a new sibling
   module `crates/uffs-client/src/connect_logging.rs` (54 LOC, cfg-gated
   behind the same `async` feature as `connect`).
2. Moved the `elevation_policy_tests` test module (~75 LOC) from
   `connect.rs` to `daemon_ctl.rs`.  The tests exercise
   `elevation_policy_from`, which lives in `daemon_ctl.rs` — so this
   also improves colocation.
3. Added `use crate::connect_logging::{...};` to `connect.rs` and
   declared `mod connect_logging;` in `lib.rs` (also `async`-gated).

**Verification (local):**

* `wc -l crates/uffs-client/src/connect.rs` → **791 LOC** (under
  800).
* `bash scripts/ci/check_file_size_policy.sh` → **`File size policy
  OK`** (no `MISSING_EXCEPTION` output).
* `cargo test -p uffs-client --lib` → **79 passed** (including all
  five `elevation_policy_tests` in their new home).

## Status

| Attempt | Command              | Outcome    | Notes |
|---------|----------------------|------------|-------|
| 1       | `just ship-fresh -v` | ❌ fail    | File size policy on `connect.rs` |
| 2       | `just ship -v`       | ⏳ pending | After extract of logging + test modules |
