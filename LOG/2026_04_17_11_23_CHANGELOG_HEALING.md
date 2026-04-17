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

(pending — in flight)

## Status

| Attempt | Command        | Outcome | Notes |
|---------|----------------|---------|-------|
| 1       | `just ship-fresh -v` | TBD     | Baseline fresh start |
